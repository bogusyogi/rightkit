//! First-party beam decode for the ONNX transducer (Windows/DirectML, Linux,
//! and macOS-without-CoreML). One window: encoder, then the shared
//! [`crate::parakeet_core::tdt_beam`] search with one decoder_joint call per
//! active hypothesis per round. Donor: HeardRight
//! `heardright-inference/src/onnx_beam_decode.rs` (the production `Fused`
//! predictor experiment, i.e. the stateful `decoder_joint` graph this crate's
//! greedy loop already drives; the borrowed/preallocated/batched/split-cached
//! experiments are benchmark variants that do not change decoded text).
//!
//! Platform policy values are explicit arguments, never environment reads:
//! width 2, emission reward 0.10 (Windows/ONNX value; macOS CoreML uses 0.15),
//! duration cap INDEX 2 (the duration-logit index is clamped and used as the
//! frame advance, measured better than 4 on WER and RTF), 10 symbols/frame.
//!
//! STATUS: compiled and covered by the gated e2e test
//! `onnx_transducer_decodes_the_fixtures_when_the_export_is_present`; no ONNX
//! Parakeet TDT export exists on the macOS dev machine, so the model-backed
//! gate has not run here (see `final.md` for the exact files it needs).
use rightkit_ort::ort;
use crate::error::{Error, Result};
use crate::model_transducer::TransducerModel;
use crate::parakeet_core::policy::{DecodePolicy, PruningMode};
use crate::parakeet_core::tdt_beam::{force_time_advance, rnnt_step, tdt_step, BeamHyp};
use ndarray::{Array1, Array2, Array3};
use std::collections::{BTreeMap, HashMap};

/// `(token_id, absolute_start_sec, duration_frames)` per emitted token.
pub type Hit = (usize, f32, usize);

/// Windows/ONNX production policy.
pub fn onnx_decode_policy() -> DecodePolicy {
    DecodePolicy {
        beam_width: 2,
        emit_reward: crate::parakeet_core::policy::WINDOWS_EMIT_REWARD,
        duration_cap_index: Some(crate::parakeet_core::policy::WINDOWS_VENDOR_DURATION_CAP_INDEX),
        pruning_mode: PruningMode::LegacyGlobal,
        max_search_steps: 100_000,
    }
}

const PRED_LAYERS: usize = 2;
const PRED_HIDDEN: usize = 640;

fn prune(pool: &mut Vec<BeamHyp>, width: usize) {
    pool.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    pool.truncate(width);
}

impl TransducerModel {
    /// Decode ONE window of mel `features` ([time, feat]) with beam search and
    /// fresh predictor state. Static-shape encoders accept at most their fixed
    /// window and are padded with silence while the REAL frame count is passed
    /// as `length`; dynamic encoders take the whole sequence.
    pub(crate) fn decode_window_beam(
        &mut self,
        features: Array2<f32>,
        policy: &DecodePolicy,
        max_symbols: usize,
        win_start_sec: f32,
        bonuses_for: &dyn Fn(&[usize]) -> HashMap<usize, f32>,
    ) -> Result<Vec<Hit>> {
        // A segment of exactly `win * 160` samples yields `win + 1` mel frames, one
        // more than the static encoder accepts; the extra frame is the centre-pad
        // tail and is dropped.
        let features = match self.static_window {
            Some(win) if features.shape()[0] == win + 1 => {
                features.slice(ndarray::s![..win, ..]).to_owned()
            }
            _ => features,
        };
        let real = features.shape()[0];
        let feat = features.shape()[1];
        let (enc, enc_len) = match self.static_window {
            Some(win) => {
                if real > win {
                    return Err(Error::Audio(format!(
                        "static encoder window is {win} mel frames but {real} were supplied; \
                         use the long-form route"
                    )));
                }
                let mut window = Array2::<f32>::zeros((win, feat));
                if real > 0 {
                    window.slice_mut(ndarray::s![..real, ..]).assign(&features);
                }
                self.run_encoder_len(&window, real.max(1) as i64)?
            }
            None => self.run_encoder_len(&features, real as i64)?,
        };
        let enc_dim = enc.shape()[1];
        let time_steps = enc.shape()[2];
        // DML rounds encoded_lengths up 1-2 frames; never read past T-1.
        let used = (enc_len.max(0) as usize).min(time_steps.saturating_sub(1)).min(time_steps);
        if used == 0 {
            return Ok(Vec::new());
        }
        let vocab_size = self.config.vocab_size;
        let blank = (vocab_size - 1) as i32;
        let has_duration = self.config.has_duration;
        let width = policy.beam_width.max(1);
        let max_symbols = max_symbols.max(1);
        let init = vec![0f32; PRED_LAYERS * PRED_HIDDEN];
        let root = BeamHyp {
            h: init.clone(),
            c: init,
            last_tok: blank,
            t: 0,
            sym: 0,
            score: 0.0,
            hits: Vec::new(),
        };

        let expand = |model: &mut TransducerModel, hyp: &BeamHyp| -> Result<Vec<BeamHyp>> {
            let frame = enc
                .slice(ndarray::s![0, .., hyp.t])
                .to_owned()
                .to_shape((1, enc_dim, 1))
                .map_err(|e| Error::Model(format!("frame: {e}")))?
                .to_owned();
            let targets = Array2::from_shape_vec((1, 1), vec![hyp.last_tok])
                .map_err(|e| Error::Model(format!("targets: {e}")))?;
            let h = Array3::from_shape_vec((PRED_LAYERS, 1, PRED_HIDDEN), hyp.h.clone())
                .map_err(|e| Error::Model(format!("state_h: {e}")))?;
            let c = Array3::from_shape_vec((PRED_LAYERS, 1, PRED_HIDDEN), hyp.c.clone())
                .map_err(|e| Error::Model(format!("state_c: {e}")))?;
            let out = model.decoder_joint.run(ort::inputs!(
                "encoder_outputs" => ort::value::Value::from_array(frame)?,
                "targets" => ort::value::Value::from_array(targets)?,
                "target_length" => ort::value::Value::from_array(Array1::from_vec(vec![1i32]))?,
                "input_states_1" => ort::value::Value::from_array(h)?,
                "input_states_2" => ort::value::Value::from_array(c)?
            ))?;
            let (_, logits) = out["outputs"]
                .try_extract_tensor::<f32>()
                .map_err(|e| Error::Model(format!("logits: {e}")))?;
            let logits = logits.to_vec();
            let (_, new_h) = out["output_states_1"]
                .try_extract_tensor::<f32>()
                .map_err(|e| Error::Model(format!("state_h out: {e}")))?;
            let (_, new_c) = out["output_states_2"]
                .try_extract_tensor::<f32>()
                .map_err(|e| Error::Model(format!("state_c out: {e}")))?;
            // Bias is per-hypothesis, from ITS OWN emitted history: that is what
            // keeps a multi-token phrase alive across steps.
            let emitted: Vec<usize> = hyp.hits.iter().map(|e| e.0).collect();
            let bonuses = bonuses_for(&emitted);
            Ok(if has_duration {
                let nd = logits.len().saturating_sub(vocab_size);
                // Index-as-value table spanning EVERY duration logit, with the VALUE
                // clamped by the cap (a truncated table panics on index 3/4).
                let cap = policy.duration_cap_index.unwrap_or(usize::MAX);
                let durations: Vec<i64> = (0..nd).map(|i| i.min(cap) as i64).collect();
                tdt_step(
                    hyp, &logits, nd, &durations, blank, max_symbols, width, policy.emit_reward,
                    win_start_sec, &bonuses, new_h, new_c,
                )
            } else {
                rnnt_step(
                    hyp, &logits, blank, width, policy.emit_reward, win_start_sec, &bonuses, new_h,
                    new_c,
                )
            })
        };

        // Defensive round cap: every round advances some hypothesis's `t` or `sym`.
        let max_rounds = used.saturating_add(1) * max_symbols + 4;
        if policy.pruning_mode == PruningMode::LegacyGlobal {
            let mut beam = vec![root];
            let mut rounds = 0usize;
            while beam.iter().any(|h| h.t < used) {
                rounds += 1;
                if rounds > max_rounds {
                    break;
                }
                let mut next: Vec<BeamHyp> = Vec::with_capacity(beam.len() * width);
                for hyp in std::mem::take(&mut beam) {
                    if hyp.t >= used {
                        next.push(hyp);
                    } else if hyp.sym >= max_symbols {
                        next.push(force_time_advance(&hyp));
                    } else {
                        next.extend(expand(self, &hyp)?);
                    }
                }
                prune(&mut next, width);
                beam = next;
            }
            prune(&mut beam, width);
            return Ok(beam.into_iter().next().map(|h| h.hits).unwrap_or_default());
        }

        // Time-synchronous: only hypotheses reading the same next frame compete;
        // finished ones wait and never compete with ones still transcribing.
        let mut buckets: BTreeMap<usize, Vec<BeamHyp>> = BTreeMap::new();
        buckets.insert(0, vec![root]);
        let mut finished: Vec<BeamHyp> = Vec::new();
        let mut rounds = 0usize;
        while let Some((&t, _)) = buckets.iter().next() {
            let mut active = buckets.remove(&t).unwrap_or_default();
            if t >= used {
                finished.append(&mut active);
                continue;
            }
            loop {
                rounds += 1;
                if rounds > max_rounds || active.is_empty() {
                    break;
                }
                prune(&mut active, width);
                let mut same_frame = Vec::new();
                for hyp in active.drain(..) {
                    if hyp.sym >= max_symbols {
                        let moved = force_time_advance(&hyp);
                        buckets.entry(moved.t).or_default().push(moved);
                        continue;
                    }
                    for child in expand(self, &hyp)? {
                        if child.t == t {
                            same_frame.push(child);
                        } else if child.t >= used {
                            finished.push(child);
                        } else {
                            buckets.entry(child.t).or_default().push(child);
                        }
                    }
                }
                active = same_frame;
            }
            if rounds > max_rounds {
                for (_, mut pool) in std::mem::take(&mut buckets) {
                    finished.append(&mut pool);
                }
                finished.append(&mut active);
            }
        }
        prune(&mut finished, 1);
        Ok(finished.into_iter().next().map(|h| h.hits).unwrap_or_default())
    }
}
