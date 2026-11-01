// Native WhisperKit engine (Track W-C) — runs the 4 precompiled `.mlmodelc`
// stages in-process on the shared CoreML bridge, dropping the `whisperkit-cli`
// subprocess. macOS / ANE only.
//
// Faithful port of WhisperKit's `TextDecoder.decodeText` greedy KV-cache loop.
// Per step:
//   1. force the prompt token while `tokenIndex < prompt.len()`, else use the
//      previously-sampled token as `input_ids`;
//   2. predict → `logits`, `key/value_cache_updates`;
//   3. suppress specials → argmax (greedy);
//   4. write the `[1,5120,1,1]` KV slice into `cache[:,:,:,tokenIndex]`;
//   5. advance the two masks one position.
// The TextDecoderContextPrefill stage (a 3-token KV precompute optimization) is
// intentionally skipped — we run the prompt tokens through the decoder instead.
use std::cell::RefCell;
use std::path::Path;
use std::time::{Duration, Instant};

use half::f16;
use objc2::rc::Retained;
use objc2_core_ml::MLMultiArray;

use crate::coreml::{f16_set, i32_set, ml_f16, ml_i32, read_f16, Stage};

use super::logits::{apply_filters, argmax};
use super::mel::{log_mel, tokenizer_path, MEL_N_MELS};
use super::vocab::WhisperVocab;

// 30 s window @ 16 kHz; model fixed shapes.
const WINDOW_SAMPLES: usize = 480_000;
const KV_DIM: usize = 5120; // 1280 d_model × 4 turbo decoder layers
const KV_MAX: usize = 448; // max decoded positions (key_cache seq dim) — matches whisper-multi/626 + fp16 large-v3-turbo
pub(crate) const VOCAB: usize = 51866;

// Special token ids (Whisper large-v3, from generation_config.json).
const SOT: i32 = 50258; // <|startoftranscript|>
pub(crate) const EOT: i32 = 50257; // <|endoftext|>  (also pad)
const LANG_EN: i32 = 50259; // <|en|>
const LANG_LAST: i32 = 50358; // <|yue|> — last of the 100 large-v3 language tokens
const TRANSCRIBE: i32 = 50360; // <|transcribe|>
pub(crate) const NO_TS: i32 = 50364; // <|notimestamps|>
pub(crate) const TS_BEGIN: i32 = 50365; // <|0.00|> — first timestamp token (NO_TS + 1)

// `begin_suppress_tokens` ([whitespace, eot]) applied at the first sampled token.
// NOTE: WhisperKit's `suppressTokens` (the 88-id non-speech list from
// generation_config) defaults to EMPTY — it is an unimplemented item in
// DecodingOptions, and the cli ships []. Suppressing those 88 ids corrupted real
// words (e.g. "metro" → "mê trông"), so we match WhisperKit and suppress nothing
// beyond this begin-suppress + the timestamp rules. Decode runs in TIMESTAMP mode
// (initial <|0.00|> forced via the prompt) so the model's trained timestamp
// structure terminates segments — without it greedy loops/truncates.
pub(crate) const BEGIN_SUPPRESS: &[usize] = &[220, 50257];

// Sentinel for "detect the language" — Whisper predicts the language token at
// the SOT step; passing this instead of a real `<|xx|>` reads that prediction.
const AUTO_LANG: i32 = -1;

/// The six decoder inputs, allocated once per engine and reset per window
/// (ORT-14). Fresh per-window allocation built two 9.2 MB f32 temporaries and
/// converted 4.6 M elements just to produce zeroed KV caches.
struct DecoderInputs {
    ids: Retained<MLMultiArray>,
    clen: Retained<MLMultiArray>,
    kc: Retained<MLMultiArray>,
    vc: Retained<MLMultiArray>,
    kpad: Retained<MLMultiArray>,
    kupd: Retained<MLMultiArray>,
}

// SAFETY: same contract as `Stage`'s owned output-backing arrays: these
// arrays are owned solely by one engine, touched only synchronously inside
// `decode_from_enc` (behind a non-Sync `RefCell`), and never shared. Moving
// that sole ownership to another thread is what `WhisperCoreMl: Send` (the
// engine is loaded and then handed to the ASR worker) already relied on
// before the buffers were cached.
unsafe impl Send for DecoderInputs {}

impl DecoderInputs {
    /// Today's per-window construction, value for value.
    fn fresh(first_token: i32) -> Result<Self, String> {
        let ids = ml_i32(&[1], &[first_token])?;
        let clen = ml_i32(&[1], &[0])?;
        let kc = ml_f16(&[1, KV_DIM, 1, KV_MAX], &vec![0f32; KV_DIM * KV_MAX])?;
        let vc = ml_f16(&[1, KV_DIM, 1, KV_MAX], &vec![0f32; KV_DIM * KV_MAX])?;
        let kpad = ml_f16(&[1, KV_MAX], &vec![-10_000f32; KV_MAX])?;
        f16_set(&kpad, 0, 0.0);
        let kupd = ml_i32(&[1, KV_MAX], &vec![0i32; KV_MAX])?;
        i32_set(&kupd, 0, 1);
        Ok(Self {
            ids,
            clen,
            kc,
            vc,
            kpad,
            kupd,
        })
    }

    /// Reuse is only sound when every buffer is plain C-contiguous: the step
    /// loop (and `f16_set`/`i32_set`) index linearly. The opt-in aligned
    /// allocation probe can hand back padded arrays; those stay per-window.
    fn reusable(&self) -> bool {
        [
            &self.ids, &self.clen, &self.kc, &self.vc, &self.kpad, &self.kupd,
        ]
        .into_iter()
        .all(|array| is_c_contiguous(array))
    }

    /// Restore exactly the state `fresh` builds: the f16 bit patterns of 0.0
    /// and -10000.0 and the i32 masks, element for element.
    // `dataPointer` is the same raw access `crate::coreml` uses (deprecated in
    // favour of block-based accessors that cannot express in-place reuse).
    #[allow(deprecated)]
    fn reset(&self, first_token: i32) {
        i32_set(&self.ids, 0, first_token);
        i32_set(&self.clen, 0, 0);
        let zero = f16::from_f32(0.0).to_bits();
        fill_u16(&self.kc, KV_DIM * KV_MAX, zero);
        fill_u16(&self.vc, KV_DIM * KV_MAX, zero);
        fill_u16(&self.kpad, KV_MAX, f16::from_f32(-10_000.0).to_bits());
        f16_set(&self.kpad, 0, 0.0);
        // SAFETY: C-contiguous Int32 [1, KV_MAX] (checked by `reusable`).
        unsafe {
            let ptr = self.kupd.dataPointer().as_ptr() as *mut i32;
            std::slice::from_raw_parts_mut(ptr, KV_MAX).fill(0);
        }
        i32_set(&self.kupd, 0, 1);
    }
}

fn is_c_contiguous(array: &MLMultiArray) -> bool {
    // SAFETY: shape/strides are plain NSNumber arrays on a live MLMultiArray.
    let (shape, strides): (Vec<isize>, Vec<isize>) = unsafe {
        (
            array.shape().iter().map(|n| n.integerValue()).collect(),
            array.strides().iter().map(|n| n.integerValue()).collect(),
        )
    };
    if shape.len() != strides.len() {
        return false;
    }
    let mut expected = 1isize;
    for (dim, stride) in shape.iter().zip(strides.iter()).rev() {
        if *stride != expected {
            return false;
        }
        expected *= *dim;
    }
    true
}

/// Fill the first `count` 16-bit elements of a C-contiguous f16 array.
// `dataPointer` is the same raw access `crate::coreml` uses (deprecated in
// favour of block-based accessors that cannot express in-place reuse).
#[allow(deprecated)]
fn fill_u16(array: &MLMultiArray, count: usize, bits: u16) {
    // SAFETY: callers pass C-contiguous Float16 arrays of at least `count`
    // elements (checked by `DecoderInputs::reusable`).
    unsafe {
        let ptr = array.dataPointer().as_ptr() as *mut u16;
        std::slice::from_raw_parts_mut(ptr, count).fill(bits);
    }
}

/// Copy a `[1, KV_DIM, 1, 1]` f16 update into column `ti` of a C-contiguous
/// `[1, KV_DIM, 1, KV_MAX]` cache as raw f16 bits, honouring the source's real
/// strides. Equivalent to the `read_f16` + `f16_set` round trip (f16 -> f32 ->
/// f16 is exact for every non-NaN value) without the per-token Vec and the
/// 2 x 5120 conversions. Returns false (caller uses the old path) when the
/// source is not the expected f16 shape.
// `dataPointer` is the same raw access `crate::coreml` uses (deprecated in
// favour of block-based accessors that cannot express in-place reuse).
#[allow(deprecated)]
fn copy_kv_column(update: &MLMultiArray, cache: &MLMultiArray, ti: usize) -> bool {
    // SAFETY: shape/strides/dataType reads on a live MLMultiArray; the
    // pointer walk stays inside the logical extent those describe, and the
    // destination index `c * KV_MAX + ti` is inside the KV_DIM x KV_MAX cache.
    unsafe {
        if update.dataType() != objc2_core_ml::MLMultiArrayDataType::Float16 {
            return false;
        }
        let shape: Vec<usize> = update
            .shape()
            .iter()
            .map(|n| n.integerValue() as usize)
            .collect();
        let strides: Vec<isize> = update.strides().iter().map(|n| n.integerValue()).collect();
        if shape.len() != strides.len() || shape.iter().product::<usize>() != KV_DIM {
            return false;
        }
        // The single non-unit dim carries the KV_DIM channels (C order).
        let Some(axis) = shape.iter().position(|&d| d == KV_DIM) else {
            return false;
        };
        let stride = strides[axis];
        let src = update.dataPointer().as_ptr() as *const u16;
        let dst = cache.dataPointer().as_ptr() as *mut u16;
        for c in 0..KV_DIM {
            *dst.add(c * KV_MAX + ti) = *src.offset(c as isize * stride);
        }
        true
    }
}

pub struct WhisperCoreMl {
    encoder: Stage,
    decoder: Stage,
    vocab: WhisperVocab,
    /// ORT-14 (output-identical): reuse decoder input buffers across windows,
    /// let CoreML write decoder outputs into reusable backings, and copy the
    /// per-token KV update as raw f16 bits. `RIGHTKIT_WHISPER_COREML_CACHE_REUSE=0`
    /// (test-mode) restores the per-window allocation path for A/B.
    cache_reuse: bool,
    decoder_inputs: RefCell<Option<DecoderInputs>>,
}

impl WhisperCoreMl {
    /// Load the WhisperKit `.mlmodelc` bundle dir + the Whisper tokenizer.
    /// `RIGHTKIT_WHISPER_TOKENIZER` overrides the bundled tokenizer path for QA only.
    pub fn load(model_dir: &Path) -> Result<Self, String> {
        // MelSpectrogram.mlmodelc is intentionally NOT loaded — the mel is computed in
        // Rust (canonical whisper log_mel), which beat the WK mel stage on the ANE.
        let encoder = Stage::load(&model_dir.join("AudioEncoder.mlmodelc"))?;
        let mut decoder = Stage::load(&model_dir.join("TextDecoder.mlmodelc"))?;
        let tok_path = tokenizer_path(model_dir);
        // DEP-09: a vocab table + GPT-2 byte decoder reproducing `tokenizers`
        // 0.23.2 exactly (parity test in vocab.rs), without the crate.
        let vocab = WhisperVocab::from_file(&tok_path)
            .map_err(|e| format!("load tokenizer {}: {e}", tok_path.display()))?;
        let cache_reuse = rightkit_dictation_core::dev_var!("RIGHTKIT_WHISPER_COREML_CACHE_REUSE")
            .as_deref()
            != Ok("0");
        if cache_reuse {
            // Logits + both KV updates land in per-stage reusable arrays
            // instead of fresh CoreML allocations every token. Each step reads
            // its outputs fully before the next prediction overwrites them.
            decoder.enable_output_backings();
        }
        tracing::info!(
            target: "rightkit_dictation_inference::whisper_coreml::timing",
            cache_reuse,
            "whisper decoder buffer policy"
        );
        Ok(Self {
            encoder,
            decoder,
            vocab,
            cache_reuse,
            decoder_inputs: RefCell::new(None),
        })
    }

    /// Look up a Whisper language token id, e.g. `lang_token("ja")` → `<|ja|>`.
    /// `"auto"` returns the AUTO sentinel (-1): Whisper detects the language
    /// itself from the SOT step instead of being forced to one.
    pub fn lang_token(&self, lang: &str) -> i32 {
        if lang == "auto" {
            return AUTO_LANG;
        }
        self.vocab
            .token_to_id(&format!("<|{lang}|>"))
            .map(|x| x as i32)
            .unwrap_or(LANG_EN)
    }

    fn detok(&self, toks: &[u32]) -> Result<String, String> {
        Ok(self.vocab.decode(toks, true).trim().to_string())
    }

    /// Transcribe 16 kHz mono f32 samples (English, no timestamps). Windows at 30 s.
    pub fn transcribe(&self, audio: &[f32]) -> Result<String, String> {
        self.transcribe_lang_windowed(audio, LANG_EN)
    }

    /// Transcribe 16 kHz mono f32 samples with an explicit Whisper language token.
    ///
    /// Follows the padded-window contract (`docs/ASR_DECODE_CONTRACT.md`) with
    /// Whisper's native 30 s window. This previously advanced by hard
    /// `start += WINDOW_SAMPLES` cuts and joined the pieces with a blind space:
    /// a word straddling a 30 s boundary was split across two decodes with
    /// nothing to repair it. The quiet cut moves each boundary onto a low-energy
    /// span and the shared seam rule de-duplicates overlap.
    pub fn transcribe_lang_windowed(&self, audio: &[f32], lang_tok: i32) -> Result<String, String> {
        self.transcribe_lang_windowed_resolved(audio, lang_tok)
            .map(|(text, _resolved)| text)
    }

    /// Same as `transcribe_lang_windowed`, plus the language token that ended up
    /// active after the last window. For an explicit `lang_tok` this just echoes
    /// it back; for AUTO it is whatever got detected. Callers that decode a
    /// recording across several separate calls (the background static-15
    /// scheduler) use this to lock AUTO onto one language for the whole
    /// recording instead of letting each call detect independently.
    pub fn transcribe_lang_windowed_resolved(
        &self,
        audio: &[f32],
        lang_tok: i32,
    ) -> Result<(String, i32), String> {
        // For AUTO, detect on the first window then lock that language for the
        // rest so the transcript can't flip language mid-dictation.
        let mut active = lang_tok;
        let text = crate::asr::transcribe_padded_window_text(audio, WINDOW_SAMPLES, |seg| {
            let window_started = std::time::Instant::now();
            let enc = self.encode_audio(seg)?;
            let (toks, detected) = self.decode_from_enc(&enc, active)?;
            tracing::info!(
                "whisper window decoded: len_s={:.1} ms={} toks={} lang_tok={}",
                seg.len() as f32 / 16_000.0,
                window_started.elapsed().as_millis(),
                toks.len(),
                detected
            );
            active = detected;
            if toks.is_empty() {
                return Ok(String::new());
            }
            self.detok(&toks)
        })?;
        Ok((text, active))
    }

    /// Single-window transcribe with an explicit language token (clips ≤ 30 s).
    pub fn transcribe_lang(&self, audio: &[f32], lang_tok: i32) -> Result<String, String> {
        let seg = &audio[..audio.len().min(WINDOW_SAMPLES)];
        let enc = self.encode_audio(seg)?;
        let (toks, _) = self.decode_from_enc(&enc, lang_tok)?;
        self.detok(&toks)
    }

    /// CROSS-FEED: decode from an externally-computed log-mel (`128×3000`, C-order),
    /// bypassing the WhisperKit MelSpectrogram stage. Used to prove whether the
    /// WhisperKit-vs-whisper.cpp gap lives in the mel front-end.
    pub fn transcribe_mel_lang(&self, mel: &[f32], lang_tok: i32) -> Result<String, String> {
        if mel.len() != 128 * 3000 {
            return Err(format!("mel must be 128*3000, got {}", mel.len()));
        }
        let mel_arr = ml_f16(&[1, 128, 1, 3000], mel)?;
        let enc: Retained<MLMultiArray> = self.encoder.predict(
            &[("melspectrogram_features", &mel_arr)],
            "encoder_output_embeds",
        )?;
        let (toks, _) = self.decode_from_enc(&enc, lang_tok)?;
        self.detok(&toks)
    }

    /// 16 kHz f32 (≤30 s window) → encoder embeddings via the Rust whisper log-mel.
    fn encode_audio(&self, seg: &[f32]) -> Result<Retained<MLMultiArray>, String> {
        let mel_started = Instant::now();
        let mut padded = vec![0f32; WINDOW_SAMPLES];
        padded[..seg.len()].copy_from_slice(seg);
        let mel = log_mel(&padded); // [128 × 3000] C-order
        let mel_time = mel_started.elapsed();
        let input_started = Instant::now();
        let mel_arr = ml_f16(&[1, MEL_N_MELS, 1, mel.len() / MEL_N_MELS], &mel)?;
        let input_time = input_started.elapsed();
        let encoder_started = Instant::now();
        let encoded = self.encoder.predict(
            &[("melspectrogram_features", &mel_arr)],
            "encoder_output_embeds",
        )?;
        tracing::info!(
            target: "rightkit_dictation_inference::whisper_coreml::timing",
            mel_ms = mel_time.as_secs_f64() * 1000.0,
            mel_input_ms = input_time.as_secs_f64() * 1000.0,
            encoder_ms = encoder_started.elapsed().as_secs_f64() * 1000.0,
            "whisper encoder stages"
        );
        Ok(encoded)
    }

    /// Encoder embeddings → content token ids (greedy KV-cache decode), `lang_tok`
    /// selects the Whisper language in the forced prompt. Pass `AUTO_LANG` (-1) to
    /// let the model detect the language at the SOT step. Returns the decoded
    /// tokens plus the resolved language token (the detected one when AUTO).
    fn decode_from_enc(
        &self,
        enc_arr: &Retained<MLMultiArray>,
        lang_tok: i32,
    ) -> Result<(Vec<u32>, i32), String> {
        let decode_started = Instant::now();
        // Timestamp mode (WhisperKit default `withoutTimestamps=false`): the initial
        // timestamp <|0.00|> is FORCED via the prompt (not sampled — matches the
        // disabled in-filter init-ts path in WhisperKit's TimestampRulesFilter).
        let ts_mode = rightkit_dictation_core::dev_var!("RIGHTKIT_WHISPER_NOTS").is_err();
        // AUTO (-1): seed the prompt with English as a placeholder; the real
        // language is read from the SOT-step prediction at ti==0 below and
        // written back into prompt[1] before it is forced at ti==1.
        let mut resolved_lang = lang_tok;
        let prompt_lang = if lang_tok == AUTO_LANG {
            LANG_EN
        } else {
            lang_tok
        };
        let mut prompt: Vec<i32> = if ts_mode {
            vec![SOT, prompt_lang, TRANSCRIBE, TS_BEGIN]
        } else {
            vec![SOT, prompt_lang, TRANSCRIBE, NO_TS]
        };
        // 4 (SOT, lang, transcribe, <|0.00|> | <|notimestamps|>)
        let init_prompt = prompt.len();
        // Decoder inputs are mutated in place each step (CoreML reads at
        // predict time). `current` is the full token list (prompt + sampled),
        // exactly the `currentTokens` WhisperKit hands to every LogitsFilter.
        //
        // ORT-14: borrow the engine's reusable inputs (reset to the exact
        // fresh state) or, with reuse off / busy / non-contiguous, build them
        // per window as before.
        let mut reuse_slot = if self.cache_reuse {
            self.decoder_inputs.try_borrow_mut().ok()
        } else {
            None
        };
        let mut owned_inputs: Option<DecoderInputs> = None;
        if let Some(slot) = reuse_slot.as_deref_mut() {
            match slot {
                Some(existing) => existing.reset(prompt[0]),
                None => {
                    let built = DecoderInputs::fresh(prompt[0])?;
                    if built.reusable() {
                        *slot = Some(built);
                    } else {
                        owned_inputs = Some(built);
                    }
                }
            }
        } else {
            owned_inputs = Some(DecoderInputs::fresh(prompt[0])?);
        }
        let reused_inputs = owned_inputs.is_none();
        let inputs: &DecoderInputs = match (&owned_inputs, reuse_slot.as_deref()) {
            (Some(owned), _) => owned,
            (None, Some(Some(shared))) => shared,
            (None, _) => return Err("whisper decoder inputs unavailable".to_string()),
        };
        let DecoderInputs {
            ids,
            clen,
            kc,
            vc,
            kpad,
            kupd,
        } = inputs;

        let mut current: Vec<i32> = prompt.to_vec();
        let mut next = prompt[0];
        let cap = KV_MAX - 1;
        let mut decoder_time = Duration::ZERO;
        let mut logits_time = Duration::ZERO;
        let mut cache_time = Duration::ZERO;
        let mut decoder_steps = 0usize;
        let mut cache_updates = 0usize;
        let setup_time = decode_started.elapsed();
        for ti in 0..cap {
            if ti < init_prompt {
                next = prompt[ti]; // force the prompt prefix (incl. the <|0.00|>)
            }
            i32_set(ids, 0, next);
            i32_set(clen, 0, ti as i32);
            let decoder_started = Instant::now();
            let outs = self.decoder.predict_multi(
                &[
                    ("input_ids", ids),
                    ("cache_length", clen),
                    ("key_cache", kc),
                    ("value_cache", vc),
                    ("decoder_key_padding_mask", kpad),
                    ("kv_cache_update_mask", kupd),
                    ("encoder_output_embeds", enc_arr),
                ],
                &["logits", "key_cache_updates", "value_cache_updates"],
            )?;
            decoder_time += decoder_started.elapsed();
            decoder_steps += 1;
            let logits_started = Instant::now();
            // AUTO detection: the SOT step (ti==0) predicts the language token.
            // Argmax over the language-token range, lock it into the forced
            // prompt so ti==1 forces the detected language (not the placeholder).
            if resolved_lang == AUTO_LANG && ti == 0 {
                let logits = read_f16(&outs[0]);
                // Rank ALL language tokens so we can see the picked lang AND the
                // runners-up + their logits (diagnostic for mis-detection).
                let mut ranked: Vec<(i32, f32)> = (LANG_EN..=LANG_LAST)
                    .map(|t| (t, logits[t as usize]))
                    .collect();
                ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                let best = ranked[0].0;
                let top: Vec<String> = ranked
                    .iter()
                    .take(5)
                    .map(|(t, v)| {
                        format!(
                            "{}={:.2}",
                            self.vocab.id_to_token(*t as u32).unwrap_or_default(),
                            v
                        )
                    })
                    .collect();
                tracing::info!(
                    "whisper auto-detect: picked {} · top5: {}",
                    self.vocab.id_to_token(best as u32).unwrap_or_default(),
                    top.join("  ")
                );
                resolved_lang = best;
                prompt[1] = best;
                current[1] = best;
            }
            // Sampling begins at the last prompt token (ti == init_prompt-1); earlier
            // steps just prefill the KV cache with the forced prompt.
            if ti >= init_prompt - 1 {
                let mut logits = read_f16(&outs[0]); // [1,1,VOCAB]
                apply_filters(&mut logits, &current, init_prompt, ts_mode);
                let sampled = argmax(&logits) as i32;
                if sampled == EOT {
                    logits_time += logits_started.elapsed();
                    break;
                }
                current.push(sampled);
                next = sampled;
            }
            logits_time += logits_started.elapsed();
            // write the [1,5120,1,1] update slice into cache[:,:,:,ti] in place
            let cache_started = Instant::now();
            // Both columns or neither: the fallback rewrites both from scratch.
            let copied = reused_inputs
                && copy_kv_column(&outs[1], kc, ti)
                && copy_kv_column(&outs[2], vc, ti);
            if !copied {
                let ku = read_f16(&outs[1]);
                let vu = read_f16(&outs[2]);
                for c in 0..KV_DIM {
                    f16_set(kc, c * KV_MAX + ti, ku[c]);
                    f16_set(vc, c * KV_MAX + ti, vu[c]);
                }
            }
            cache_time += cache_started.elapsed();
            cache_updates += 2;
            f16_set(kpad, ti + 1, 0.0);
            i32_set(kupd, ti, 0);
            i32_set(kupd, ti + 1, 1);
        }
        tracing::info!(
            target: "rightkit_dictation_inference::whisper_coreml::timing",
            decoder_steps,
            cache_updates,
            reused_inputs,
            setup_ms = setup_time.as_secs_f64() * 1000.0,
            decoder_ms = decoder_time.as_secs_f64() * 1000.0,
            logits_ms = logits_time.as_secs_f64() * 1000.0,
            cache_copy_ms = cache_time.as_secs_f64() * 1000.0,
            total_ms = decode_started.elapsed().as_secs_f64() * 1000.0,
            "whisper decoder stages"
        );
        if rightkit_dictation_core::dev_var!("RIGHTKIT_WHISPER_DUMP").is_ok() {
            let s: String = current[init_prompt..]
                .iter()
                .map(|&t| {
                    if t >= TS_BEGIN {
                        format!("⟨{:.2}⟩", (t - TS_BEGIN) as f32 * 0.02)
                    } else {
                        self.vocab.id_to_token(t as u32).unwrap_or_default()
                    }
                })
                .collect();
            let n_ts = current[init_prompt..]
                .iter()
                .filter(|&&t| t >= TS_BEGIN)
                .count();
            eprintln!(
                "[dump] {} sampled, {} timestamps:\n{s}",
                current.len() - init_prompt,
                n_ts
            );
        }
        // keep only sampled text tokens (drop the prompt and all timestamps ≥ TS_BEGIN)
        let toks = current[init_prompt..]
            .iter()
            .filter(|&&t| t < EOT)
            .map(|&t| t as u32)
            .collect();
        Ok((toks, resolved_lang))
    }
}
