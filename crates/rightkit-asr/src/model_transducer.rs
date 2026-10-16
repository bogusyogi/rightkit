//! Unified offline NeMo transducer — handles BOTH conformer-RNN-T and conformer-TDT
//! with one decode path. RNN-T and TDT share the exact same encoder I/O and stateful
//! decoder_joint (encoder_outputs/targets/target_length/input_states_1,2 → outputs/
//! output_states_1,2, 2x1x640 LSTM states). The ONLY difference: TDT's joint output
//! carries `n_durations` extra logits after the vocab (frame-skip); RNN-T does not.
//! `has_duration` is set by the loader from config.json (`model_type`). This is why a
//! customer can drop in either downloaded model and it just plugs in — the engine
//! self-configures from its own config.
use rightkit_onnx::ort;
use crate::error::{Error, Result};
use crate::execution::ModelConfig as ExecutionConfig;
#[cfg(feature = "directml")]
use crate::execution::ExecutionProvider;
use ndarray::{Array1, Array2, Array3};
use rightkit_onnx::ort::session::Session;
use rightkit_onnx::ort::value::{TensorElementType, ValueType};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct TransducerConfig {
    pub vocab_size: usize,          // incl. blank (blank = vocab_size - 1)
    pub max_tokens_per_step: usize, // RNN-T cap; also a TDT safety cap
    pub has_duration: bool,         // true = TDT (frame-skip), false = RNN-T
    pub duration_cap: usize,        // TDT only: clamp frame-skip
    pub window_overlap_mel: usize,
    pub debug_diagnostics: bool,
}

/// One context-bias implementation for every runtime (shared parakeet_core).
pub use crate::parakeet_core::bias::ContextBias;

pub struct TransducerModel {
    pub(crate) encoder: Session,
    pub(crate) decoder_joint: Session,
    pub(crate) config: TransducerConfig,
    pub(crate) context_bias: Option<ContextBias>,
    pub(crate) encoder_audio_signal_type: TensorElementType,
    /// `Some(W)` when the encoder's `audio_signal` time axis is a FIXED length `W`
    /// (the static-shape export, e.g. 1500 mel frames = 15 s). Drives windowed
    /// decode: the encoder can only ingest exactly `W` frames, so longer audio is
    /// split into `W`-frame windows, each encoded + decoded with a fresh decoder
    /// state. `None` = dynamic encoder, full-sequence encode in one pass.
    pub static_window: Option<usize>,
}

impl TransducerModel {
    /// True for TDT models (durations predicted per token), false for RNN-T.
    pub fn has_duration(&self) -> bool {
        self.config.has_duration
    }

    /// Loads a model directory. With `allow_int8 = false` the int8 encoder and
    /// decoder variants are never selected (strict layout: no silent
    /// preference); a directory holding only int8 files is an error.
    pub(crate) fn from_pretrained_with<P: AsRef<Path>>(
        model_dir: P,
        exec_config: ExecutionConfig,
        config: TransducerConfig,
        allow_int8: bool,
    ) -> Result<Self> {
        let dir = model_dir.as_ref();
        let enc = Self::find(dir, &["encoder-model.int8.onnx", "encoder-model.onnx", "encoder.onnx"], "encoder", allow_int8)?;
        let dec = Self::find(dir, &["decoder_joint-model.int8.onnx", "decoder_joint-model.onnx", "decoder_joint.onnx"], "decoder_joint", allow_int8)?;

        // DirectML graph-fusion bug (onnxruntime-directml 1.24.4): the fused kernel for
        // the STATIC-shape conformer encoder silently produces near-zero output on DML
        // (CPU + dynamic-shape are both correct) -> the TDT joint emits blank at every
        // frame -> EMPTY TRANSCRIPT, no error/log. Symptom at smaller pins:
        // `RUNTIME_EXCEPTION ... DmlGraphFusionHelper.cpp(131)`. Confirmed precision-
        // independent (fp32/fp16/4-bit all collapse) and shape-triggered via a
        // same-bytes static-pin of a proven dynamic model (plan §9 Phase C + memory
        // hr_dml_static_graph_fusion_bug). Disabling ONLY the DML fusion pass restores
        // CPU-parity output, keeping every other optimization. SCOPED static + DML only
        // (dynamic models benefit from fusion). REMOVAL CONDITION: a future
        // onnxruntime-directml that fixes the static-conformer fusion (track ORT issue
        // DmlGraphFusionHelper) — drop the flag once the static DML decode passes without it.
        //
        // Static-ness is AUTHORITATIVELY read from the model's OWN GRAPH (a fixed
        // `audio_signal` time dim) below, NOT from a config sidecar that can drift out
        // of sync with the model it describes — shipping a static encoder whose
        // config forgot the key would silently re-trigger the collapse. config.json
        // `static_window` is only an OPTIONAL fast-path hint that lets us set the flag
        // on the first commit; if the graph says static and the hint didn't fire, we
        // re-commit with the workaround. The model can't lie about its own shape.
        // The DirectML graph-fusion workaround only applies when DirectML is
        // compiled in; on macOS/Linux (coreml/cpu) the variant doesn't exist, so
        // gate the reference to match the variant's `#[cfg(feature="directml")]`.
        #[cfg(feature = "directml")]
        let dml = matches!(exec_config.execution_provider, ExecutionProvider::DirectML);
        #[cfg(not(feature = "directml"))]
        let dml = false;
        let config_hint_static = std::fs::read_to_string(dir.join("config.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.get("static_window").and_then(|x| x.as_u64()))
            .map(|w| w > 0)
            .unwrap_or(false);
        let ec = exec_config.clone();
        let commit_encoder = move |disable_fusion: bool| -> Result<Session> {
            let mut b = Session::builder()?;
            if disable_fusion {
                b = b.with_config_entry("ep.dml.disable_graph_fusion", "1")?;
            }
            let mut b = ec.clone().apply_to_session_builder(b)?;
            Ok(b.commit_from_file(&enc)?)
        };
        let mut encoder = commit_encoder(config_hint_static && dml)?;

        // AUTHORITATIVE static-shape detection from the encoder's own graph: if
        // `audio_signal`'s time dim (axis 2 of [batch, n_mel, time]) is a fixed
        // positive length, this is the static-window export -> windowed decode. A
        // dynamic axis is a negative (symbolic) dim, so <= 0 means dynamic full-seq.
        let detect_static_window = |enc: &Session| -> Option<usize> {
            enc.inputs()
                .iter()
                .find(|input| input.name() == "audio_signal")
                .and_then(|input| match input.dtype() {
                    ValueType::Tensor { shape, .. } => shape.get(2).copied(),
                    _ => None,
                })
                .filter(|&d| d > 0)
                .map(|d| d as usize)
        };
        let static_window = detect_static_window(&encoder);
        // Graph says static + DML but the fast-path hint didn't fire (config missing /
        // drifted) -> the committed session is the broken (fused) one. Re-commit with
        // the workaround; correctness comes from the graph, not the sidecar.
        if dml && static_window.is_some() && !config_hint_static {
            eprintln!(
                "[rightkit-asr] encoder is static-shape (per graph) but config.json lacked \
                 static_window; applying DML graph-fusion workaround from the graph (re-commit)"
            );
            encoder = commit_encoder(true)?;
        }
        let encoder_audio_signal_type = encoder
            .inputs()
            .iter()
            .find(|input| input.name() == "audio_signal")
            .and_then(|input| match input.dtype() {
                ValueType::Tensor { ty, .. } => Some(*ty),
                _ => None,
            })
            .unwrap_or(TensorElementType::Float32);

        // Auxiliary graph placement is explicit in the shared execution config.
        let dec_config = exec_config.auxiliary_config();
        let b = Session::builder()?;
        let mut b = dec_config.apply_to_session_builder(b)?;
        let decoder_joint = b.commit_from_file(&dec)?;

        Ok(Self {
            encoder,
            decoder_joint,
            config,
            context_bias: None,
            encoder_audio_signal_type,
            static_window,
        })
    }

    fn find(dir: &Path, cands: &[&str], kind: &str, allow_int8: bool) -> Result<PathBuf> {
        let mut skipped_int8 = false;
        for c in cands {
            if !allow_int8 && c.contains("int8") {
                skipped_int8 |= dir.join(c).exists();
                continue;
            }
            let p = dir.join(c);
            if p.exists() { return Ok(p); }
        }
        if skipped_int8 {
            return Err(Error::Config(format!(
                "No fp {kind} model in {} (only an int8 variant is present; the strict layout refuses int8)",
                dir.display()
            )));
        }
        Err(Error::Config(format!("No {kind} model in {}", dir.display())))
    }

    /// full audio features [time, feat] -> (token_ids, frame_indices, durations)
    ///
    /// Static-window encoders (the locked TDT export) decode in `W`-frame windows
    /// with a fresh decoder state per window — the exact contract proven on macOS
    /// (`coreml_asr.rs`): a hard window boundary needs a state reset, else TDT emits
    /// dead all-blank windows. Dynamic encoders decode the whole sequence in one
    /// pass. The branch is invisible to callers.
    pub fn forward(&mut self, features: Array2<f32>) -> Result<(Vec<usize>, Vec<usize>, Vec<usize>)> {
        match self.static_window {
            Some(win) => self.forward_windowed(features, win),
            None => {
                let (enc_out, enc_len) = self.run_encoder(&features)?;
                self.greedy_decode(&enc_out, enc_len)
            }
        }
    }

    /// Windowed static-shape decode: split `features` into `win`-frame windows
    /// (the encoder's fixed input length), pad the trailing partial window with
    /// silence to `win`, and pass the REAL frame count as `length` so the encoder's
    /// `encoded_lengths` (→ `greedy_decode`'s length clamp) ignores the padding.
    /// Each window decodes with a fresh LSTM state (state never carries across a
    /// boundary). Token frame indices are offset to stay globally monotonic.
    fn forward_windowed(
        &mut self,
        features: Array2<f32>,
        win: usize,
    ) -> Result<(Vec<usize>, Vec<usize>, Vec<usize>)> {
        let total = features.shape()[0];
        let feat = features.shape()[1];
        let subsample = 8usize; // encoder subsamples mel frames 8x
        // Window stitching for >15 s audio. DEFAULT = hard, non-overlapping windows
        // (overlap=0): measured 3.02% WER on natural-pause >15 s clips (librispeech
        // narration a perfect 0.0%) vs single-window 1.93% — boundaries that land in
        // a pause stitch cleanly. The §2-locked OVERLAP-MERGE (overlap>0, frame-space
        // midpoint cut below) is RETAINED but OFF by default: as implemented it
        // regressed to 15% (drops/duplicates at the merge + a per-window all-blank
        // collapse when a window STARTS on a hard audio discontinuity — confirmed a
        // concat artifact, 0.5 s shift fixes it). Overlap-merge needs silence-aware
        // boundary placement (no `find_silence_cut` exists yet) + a correct merge
        // before it beats hard windows; tracked as a W-series follow-up. Tunable via
        let overlap = self.config.window_overlap_mel.min(win / 2);
        let stride = (win - overlap).max(1);
        let (mut tokens, mut frames, mut durations) = (Vec::new(), Vec::new(), Vec::new());
        let mut start = 0usize;
        // Lower bound (GLOBAL enc-frame) for tokens kept from the current window =
        // the midpoint cut with the PREVIOUS window. Starts at 0 for window 0.
        let mut cut_prev_enc = 0usize;
        loop {
            let end = (start + win).min(total.max(1));
            let real = end - start;
            // pad window to exactly `win` frames (static encoder requirement)
            let mut window = Array2::<f32>::zeros((win, feat));
            if real > 0 {
                window
                    .slice_mut(ndarray::s![..real, ..])
                    .assign(&features.slice(ndarray::s![start..end, ..]));
            }
            let (enc_out, enc_len) = self.run_encoder_len(&window, real.max(1) as i64)?;
            let (t, f, d) = self.greedy_decode(&enc_out, enc_len)?;
            let win_off_enc = start / subsample; // global enc-frame offset of this window
            // This window reaches the end -> last window. Stopping here (vs advancing
            // one more stride) is constraint 5: never decode a redundant tail whose
            // whole span is already covered by this window.
            let is_last = end >= total;
            // Cut with the NEXT window = midpoint of their overlap, IN ENC-FRAME SPACE
            // (constraint 1: midpoint after the 8x subsample, not in audio samples).
            let cut_next_enc = if is_last {
                usize::MAX
            } else {
                let next_start = start + stride;
                // overlap (mel) = [next_start, end); midpoint (mel) = (next_start+end)/2
                ((next_start + end) / 2) / subsample
            };
            // Keep tokens whose GLOBAL EMIT frame is in [cut_prev_enc, cut_next_enc).
            // Constraint 2: < midpoint -> this window; >= -> next window (next has the
            // fuller left context past the midpoint). Constraint 3: cut on the token's
            // EMIT frame (`f`), NOT its TDT duration-skip destination.
            for ((tok, fr), dur) in t.into_iter().zip(f).zip(d) {
                let g = fr + win_off_enc;
                if g >= cut_prev_enc && g < cut_next_enc {
                    tokens.push(tok);
                    frames.push(g);
                    durations.push(dur);
                }
            }
            if is_last {
                break;
            }
            cut_prev_enc = cut_next_enc;
            start += stride;
        }
        Ok((tokens, frames, durations))
    }

    pub fn set_context_bias(&mut self, bias: Option<ContextBias>) {
        self.context_bias = bias.filter(|b| !b.is_empty());
    }

    fn run_encoder(&mut self, features: &Array2<f32>) -> Result<(Array3<f32>, i64)> {
        let len = features.shape()[0] as i64;
        self.run_encoder_len(features, len)
    }

    /// Run the encoder over `features` ([time, feat]), telling it the real extent is
    /// `length` frames. For static-window calls `features` is padded to the fixed
    /// window length while `length` is the unpadded count, so `encoded_lengths`
    /// reflects only real audio.
    pub(crate) fn run_encoder_len(&mut self, features: &Array2<f32>, length: i64) -> Result<(Array3<f32>, i64)> {
        let time_steps = features.shape()[0];
        let feat = features.shape()[1];
        let input = features.t().to_shape((1, feat, time_steps))
            .map_err(|e| Error::Model(format!("reshape encoder input: {e}")))?.to_owned();
        let length = ort::value::Value::from_array(Array1::from_vec(vec![length]))?;
        let outputs = match self.encoder_audio_signal_type {
            TensorElementType::Float16 => {
                let input_f16 = input.mapv(half::f16::from_f32);
                self.encoder.run(ort::inputs!(
                    "audio_signal" => ort::value::Value::from_array(input_f16)?,
                    "length" => length
                ))?
            }
            TensorElementType::Float32 => self.encoder.run(ort::inputs!(
                "audio_signal" => ort::value::Value::from_array(input)?,
                "length" => length
            ))?,
            other => {
                return Err(Error::Model(format!(
                    "unsupported encoder audio_signal dtype: {other}"
                )));
            }
        };
        let outputs_value = &outputs["outputs"];
        let (shape, data) = match outputs_value.dtype().tensor_type() {
            Some(TensorElementType::Float16) => {
                let (shape, data) = outputs_value
                    .try_extract_tensor::<half::f16>()
                    .map_err(|e| Error::Model(format!("extract encoder output f16: {e}")))?;
                (
                    shape,
                    data.iter()
                        .map(|value| value.to_f32())
                        .collect::<Vec<f32>>(),
                )
            }
            Some(TensorElementType::Float32) => {
                let (shape, data) = outputs_value
                    .try_extract_tensor::<f32>()
                    .map_err(|e| Error::Model(format!("extract encoder output f32: {e}")))?;
                (shape, data.to_vec())
            }
            other => {
                return Err(Error::Model(format!(
                    "unsupported encoder output dtype: {other:?}"
                )));
            }
        };
        let (_, lens) = outputs["encoded_lengths"].try_extract_tensor::<i64>()
            .map_err(|e| Error::Model(format!("extract encoder lengths: {e}")))?;
        let d = shape.as_ref();
        if d.len() != 3 { return Err(Error::Model(format!("expected 3D encoder out, got {d:?}"))); }
        let arr = Array3::from_shape_vec((d[0] as usize, d[1] as usize, d[2] as usize), data)
            .map_err(|e| Error::Model(format!("encoder array: {e}")))?;
        Ok((arr, lens[0]))
    }

    fn greedy_decode(&mut self, enc: &Array3<f32>, enc_len: i64) -> Result<(Vec<usize>, Vec<usize>, Vec<usize>)> {
        let enc_dim = enc.shape()[1];
        let time_steps = enc.shape()[2];
        let vocab_size = self.config.vocab_size;
        let blank = vocab_size - 1;
        // LengthClamp: DML rounds encoded_lengths up 1-2 frames; never read past T-1.
        let used = (enc_len as usize).min(time_steps.saturating_sub(1)).min(time_steps);
        let dbg = self.config.debug_diagnostics;
        if dbg {
            let f: Vec<f32> = enc.iter().copied().collect();
            let (mn, mx, sm) = (f.iter().cloned().fold(f32::INFINITY, f32::min),
                                f.iter().cloned().fold(f32::NEG_INFINITY, f32::max),
                                f.iter().sum::<f32>());
            eprintln!("[dbg] greedy enc_len={enc_len} used={used} time_steps={time_steps} enc_dim={enc_dim} vocab_size={vocab_size} blank={blank} dur_cap={} | enc min={mn:.3} max={mx:.3} mean={:.4}",
                self.config.duration_cap, sm / f.len() as f32);
        }

        let mut h = Array3::<f32>::zeros((2, 1, 640));
        let mut c = Array3::<f32>::zeros((2, 1, 640));
        let (mut tokens, mut frames, mut durations) = (Vec::new(), Vec::new(), Vec::new());
        let (mut t, mut emitted, mut last) = (0usize, 0usize, blank as i32);

        while t < used {
            let frame = enc.slice(ndarray::s![0, .., t]).to_owned()
                .to_shape((1, enc_dim, 1)).map_err(|e| Error::Model(format!("frame: {e}")))?.to_owned();
            let targets = Array2::from_shape_vec((1, 1), vec![last]).map_err(|e| Error::Model(format!("targets: {e}")))?;
            let out = self.decoder_joint.run(ort::inputs!(
                "encoder_outputs" => ort::value::Value::from_array(frame)?,
                "targets" => ort::value::Value::from_array(targets)?,
                "target_length" => ort::value::Value::from_array(Array1::from_vec(vec![1i32]))?,
                "input_states_1" => ort::value::Value::from_array(h.clone())?,
                "input_states_2" => ort::value::Value::from_array(c.clone())?
            ))?;
            let (_, logits) = out["outputs"].try_extract_tensor::<f32>()
                .map_err(|e| Error::Model(format!("logits: {e}")))?;

            let token_id = argmax_token(
                logits.iter().take(vocab_size),
                self.context_bias.as_ref(),
                &tokens,
            );
            let dur = if self.config.has_duration {
                argmax(logits.iter().skip(vocab_size)).min(self.config.duration_cap)
            } else { 0 };
            if dbg && t < 4 {
                let blank_logit = logits.get(blank).copied().unwrap_or(f32::NAN);
                let tok_logit = logits.get(token_id).copied().unwrap_or(f32::NAN);
                let raw_dur = argmax(logits.iter().skip(vocab_size));
                eprintln!("[dbg] t={t} logits.len={} token_id={token_id} tok_logit={tok_logit:.3} blank_logit={blank_logit:.3} raw_dur_idx={raw_dur} dur={dur}", logits.len());
            }

            if token_id != blank {
                if let Ok((s, hd)) = out["output_states_1"].try_extract_tensor::<f32>() {
                    let s = s.as_ref();
                    h = Array3::from_shape_vec((s[0] as usize, s[1] as usize, s[2] as usize), hd.to_vec())
                        .map_err(|e| Error::Model(format!("state_h: {e}")))?;
                }
                if let Ok((s, cd)) = out["output_states_2"].try_extract_tensor::<f32>() {
                    let s = s.as_ref();
                    c = Array3::from_shape_vec((s[0] as usize, s[1] as usize, s[2] as usize), cd.to_vec())
                        .map_err(|e| Error::Model(format!("state_c: {e}")))?;
                }
                tokens.push(token_id); frames.push(t); durations.push(dur);
                last = token_id as i32; emitted += 1;
            }
            if dur > 0 { t += dur; emitted = 0; }
            else if token_id == blank || emitted >= self.config.max_tokens_per_step { t += 1; emitted = 0; }
        }
        if dbg { eprintln!("[dbg] greedy emitted {} tokens", tokens.len()); }
        Ok((tokens, frames, durations))
    }
}

// Tie rule (unchanged from 0.1.0, kept deliberately): `Iterator::max_by` returns
// the LAST maximal index on an exact float tie. The greedy path keeps it so no
// locked benchmark moves. The beam path's `rank_top_k` keeps the FIRST index.
// The two rules differ on exact ties only; see CHANGELOG 0.2.0.
fn argmax<'a, I: Iterator<Item = &'a f32>>(it: I) -> usize {
    it.enumerate()
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i).unwrap_or(0)
}

fn argmax_token<'a, I: Iterator<Item = &'a f32>>(
    it: I,
    bias: Option<&ContextBias>,
    emitted: &[usize],
) -> usize {
    let bonuses = bias.map(|b| b.next_token_bonuses(emitted));
    it.enumerate()
        .max_by(|(a_idx, a), (b_idx, b)| {
            let a_score = **a
                + bonuses
                    .as_ref()
                    .and_then(|b| b.get(a_idx))
                    .copied()
                    .unwrap_or(0.0);
            let b_score = **b
                + bonuses
                    .as_ref()
                    .and_then(|b| b.get(b_idx))
                    .copied()
                    .unwrap_or(0.0);
            a_score
                .partial_cmp(&b_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_bias_boosts_phrase_start_and_continuation() {
        let bias = ContextBias::new(vec![vec![10, 11, 12]], 1.0);
        assert_eq!(bias.next_token_bonuses(&[]).get(&10).copied(), Some(1.0));
        assert_eq!(
            bias.next_token_bonuses(&[10]).get(&11).copied(),
            Some(1.0)
        );
        assert_eq!(
            bias.next_token_bonuses(&[10, 11]).get(&12).copied(),
            Some(1.0)
        );
        assert_eq!(bias.next_token_bonuses(&[10]).get(&12).copied(), None);
    }

    #[test]
    fn biased_argmax_can_override_small_margin_only() {
        let bias = ContextBias::new(vec![vec![2]], 0.5);
        let logits = [0.0, 1.0, 0.75];
        assert_eq!(argmax_token(logits.iter(), Some(&bias), &[]), 2);

        let logits = [0.0, 1.0, 0.1];
        assert_eq!(argmax_token(logits.iter(), Some(&bias), &[]), 1);
    }
}
