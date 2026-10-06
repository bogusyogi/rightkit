//! Native WhisperKit-format Whisper on CoreML (macOS; ANE/GPU/CPU).
//!
//! Runs the precompiled `AudioEncoder.mlmodelc` + `TextDecoder.mlmodelc`
//! in-process. The mel front-end is computed in Rust (canonical openai/whisper
//! log-mel), not by the WhisperKit MelSpectrogram stage.
//!
//! Donor: HeardRight `heardright-inference/src/whisper_coreml/engine.rs`, a
//! port of WhisperKit's `TextDecoder.decodeText` greedy KV-cache loop. Per step:
//! force the prompt token while inside the prompt, else feed the previously
//! sampled token; predict; apply the logits filters; argmax; write the
//! `[1,5120,1,1]` KV update into `cache[:,:,:,i]`; advance the two masks.
pub(crate) mod bridge;
mod logits;
mod mel;

pub use bridge::ComputeUnits;
use bridge::{f16_set, i32_set, ml_f16, ml_i32, read_f16, Stage};
use logits::{apply_filters, argmax};
use mel::{log_mel, MEL_N_MELS};

use super::{Segment, WindowDecoder};
use crate::error::{Error, Result};
use objc2::rc::Retained;
use objc2_core_ml::MLMultiArray;
use std::path::Path;
use tokenizers::Tokenizer;

const WINDOW_SAMPLES: usize = super::WHISPER_WINDOW_SAMPLES;
const KV_DIM: usize = 5120; // 1280 d_model x 4 turbo decoder layers
const KV_MAX: usize = 448;
pub(crate) const VOCAB: usize = 51866;

const SOT: i32 = 50258;
pub(crate) const EOT: i32 = 50257;
const LANG_EN: i32 = 50259;
const LANG_LAST: i32 = 50358;
const TRANSCRIBE: i32 = 50360;
pub(crate) const NO_TS: i32 = 50364;
pub(crate) const TS_BEGIN: i32 = 50365;
/// `begin_suppress_tokens`: whitespace and EOT at the first sampled token.
/// WhisperKit's 88-id non-speech suppress list is deliberately NOT applied: it
/// corrupted real words in HeardRight's measurements.
pub(crate) const BEGIN_SUPPRESS: &[usize] = &[220, 50257];
const AUTO_LANG: i32 = -1;
const TS_STEP_S: f32 = 0.02;

pub struct WhisperCoreMl {
    encoder: Stage,
    decoder: Stage,
    tokenizer: Tokenizer,
}

impl WhisperCoreMl {
    /// `dir` holds `AudioEncoder.mlmodelc`, `TextDecoder.mlmodelc`, `tokenizer.json`.
    pub fn load(dir: &Path, units: ComputeUnits) -> Result<Self> {
        let m = |e: String| Error::Model(e);
        let encoder = Stage::load(&dir.join("AudioEncoder.mlmodelc"), units).map_err(m)?;
        let decoder = Stage::load(&dir.join("TextDecoder.mlmodelc"), units).map_err(m)?;
        let tok = dir.join("tokenizer.json");
        let tokenizer = Tokenizer::from_file(&tok)
            .map_err(|e| Error::Tokenizer(format!("load {}: {e}", tok.display())))?;
        Ok(Self { encoder, decoder, tokenizer })
    }

    fn lang_token(&self, code: &str) -> Result<i32> {
        self.tokenizer
            .token_to_id(&format!("<|{code}|>"))
            .filter(|id| (LANG_EN as u32..=LANG_LAST as u32).contains(id))
            .map(|id| id as i32)
            .ok_or_else(|| Error::Config(format!("unknown Whisper language code `{code}`")))
    }

    fn detok(&self, toks: &[u32]) -> Result<String> {
        self.tokenizer
            .decode(toks, true)
            .map(|s| s.trim().to_string())
            .map_err(|e| Error::Tokenizer(format!("detokenize: {e}")))
    }

    fn encode_audio(&self, seg: &[f32]) -> Result<Retained<MLMultiArray>> {
        let mut padded = vec![0f32; WINDOW_SAMPLES];
        padded[..seg.len()].copy_from_slice(seg);
        let mel = log_mel(&padded); // [128 x 3000] C-order
        let arr = ml_f16(&[1, MEL_N_MELS, 1, mel.len() / MEL_N_MELS], &mel).map_err(Error::Model)?;
        let mut outs = self
            .encoder
            .predict_multi(&[("melspectrogram_features", &arr)], &["encoder_output_embeds"])
            .map_err(Error::Model)?;
        Ok(outs.remove(0))
    }

    /// Greedy KV-cache decode. Returns sampled ids (text and timestamp tokens,
    /// EOT excluded) and the resolved language token.
    fn decode_tokens(&self, enc: &Retained<MLMultiArray>, lang_tok: i32) -> Result<(Vec<i32>, i32)> {
        let m = Error::Model;
        let mut resolved = lang_tok;
        let prompt_lang = if lang_tok == AUTO_LANG { LANG_EN } else { lang_tok };
        // Timestamp mode: the initial <|0.00|> is forced via the prompt.
        let mut prompt = vec![SOT, prompt_lang, TRANSCRIBE, TS_BEGIN];
        let init_prompt = prompt.len();
        let ids = ml_i32(&[1], &[prompt[0]]).map_err(m)?;
        let clen = ml_i32(&[1], &[0]).map_err(m)?;
        let kc = ml_f16(&[1, KV_DIM, 1, KV_MAX], &vec![0f32; KV_DIM * KV_MAX]).map_err(m)?;
        let vc = ml_f16(&[1, KV_DIM, 1, KV_MAX], &vec![0f32; KV_DIM * KV_MAX]).map_err(m)?;
        let kpad = ml_f16(&[1, KV_MAX], &vec![-10_000f32; KV_MAX]).map_err(m)?;
        f16_set(&kpad, 0, 0.0);
        let kupd = ml_i32(&[1, KV_MAX], &vec![0i32; KV_MAX]).map_err(m)?;
        i32_set(&kupd, 0, 1);

        let mut current = prompt.clone();
        let mut next = prompt[0];
        for ti in 0..KV_MAX - 1 {
            if ti < init_prompt {
                next = prompt[ti];
            }
            i32_set(&ids, 0, next);
            i32_set(&clen, 0, ti as i32);
            let outs = self
                .decoder
                .predict_multi(
                    &[
                        ("input_ids", &ids),
                        ("cache_length", &clen),
                        ("key_cache", &kc),
                        ("value_cache", &vc),
                        ("decoder_key_padding_mask", &kpad),
                        ("kv_cache_update_mask", &kupd),
                        ("encoder_output_embeds", enc),
                    ],
                    &["logits", "key_cache_updates", "value_cache_updates"],
                )
                .map_err(Error::Model)?;
            // The SOT step predicts the language; lock it into the forced prompt.
            if resolved == AUTO_LANG && ti == 0 {
                let logits = read_f16(&outs[0]);
                let best = (LANG_EN..=LANG_LAST)
                    .max_by(|a, b| {
                        logits[*a as usize]
                            .partial_cmp(&logits[*b as usize])
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .unwrap_or(LANG_EN);
                resolved = best;
                prompt[1] = best;
                current[1] = best;
            }
            // Sampling begins at the last prompt token; earlier steps prefill the KV cache.
            if ti >= init_prompt - 1 {
                let mut logits = read_f16(&outs[0]);
                apply_filters(&mut logits, &current, init_prompt, true);
                let sampled = argmax(&logits) as i32;
                if sampled == EOT {
                    break;
                }
                current.push(sampled);
                next = sampled;
            }
            let ku = read_f16(&outs[1]);
            let vu = read_f16(&outs[2]);
            for c in 0..KV_DIM {
                f16_set(&kc, c * KV_MAX + ti, ku[c]);
                f16_set(&vc, c * KV_MAX + ti, vu[c]);
            }
            f16_set(&kpad, ti + 1, 0.0);
            i32_set(&kupd, ti, 0);
            i32_set(&kupd, ti + 1, 1);
        }
        Ok((current[init_prompt..].to_vec(), resolved))
    }

    /// Split sampled tokens at timestamp tokens into segments.
    fn segments(&self, toks: &[i32], window_s: f32) -> Result<Vec<Segment>> {
        let mut out = Vec::new();
        let (mut start, mut buf): (f32, Vec<u32>) = (0.0, Vec::new());
        for &t in toks {
            if t >= TS_BEGIN {
                let ts = ((t - TS_BEGIN) as f32 * TS_STEP_S).min(window_s);
                if !buf.is_empty() {
                    let text = self.detok(&buf)?;
                    if !text.is_empty() {
                        out.push(Segment { text, start, end: ts });
                    }
                    buf.clear();
                }
                start = ts;
            } else if t < EOT {
                buf.push(t as u32);
            }
        }
        if !buf.is_empty() {
            let text = self.detok(&buf)?;
            if !text.is_empty() {
                out.push(Segment { text, start, end: window_s });
            }
        }
        Ok(out)
    }
}

impl WindowDecoder for WhisperCoreMl {
    fn decode_window(&mut self, samples: &[f32], lang: Option<&str>) -> Result<(Vec<Segment>, String)> {
        let lang_tok = match lang {
            Some(code) => self.lang_token(code)?,
            None => AUTO_LANG,
        };
        let enc = self.encode_audio(samples)?;
        let (toks, resolved) = self.decode_tokens(&enc, lang_tok)?;
        let code = if resolved == AUTO_LANG {
            "en".to_string()
        } else {
            self.tokenizer
                .id_to_token(resolved as u32)
                .map(|t| t.trim_start_matches("<|").trim_end_matches("|>").to_string())
                .unwrap_or_else(|| "en".into())
        };
        Ok((self.segments(&toks, samples.len() as f32 / 16_000.0)?, code))
    }
}
