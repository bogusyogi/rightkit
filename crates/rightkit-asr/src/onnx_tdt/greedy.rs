//! Greedy TDT decoding over [`OnnxAsr`]: one hypothesis, argmax token and duration per joint step.
//!
//! This is the policy-free-of-search path for hosts that want a plain greedy pass (no beam, no
//! phrase bias). The sessions, mel, encoder and joint calls are [`OnnxAsr`]'s; this module only
//! walks the frames. Search-based decoding lives in `crate::parakeet_core`.

use super::{Error, OnnxAsr, Result};
use crate::parakeet_core::DecodedPiece;

/// Index of the largest value (the first one on a tie); 0 for an empty slice.
fn argmax(values: &[f32]) -> usize {
    let mut best = 0;
    let mut best_value = f32::NEG_INFINITY;
    for (index, value) in values.iter().enumerate() {
        if *value > best_value {
            best = index;
            best_value = *value;
        }
    }
    best
}

/// Greedy-decode `audio` (mono f32 PCM at `sample_rate`) into timed pieces.
///
/// A static-window graph is fed in chunks that fit its window (`hop_length` samples per mel
/// frame, one frame of margin); a dynamic graph takes the audio whole. Per encoder frame the loop
/// takes the argmax token and, from the duration logits, the argmax duration: a non-blank token
/// is emitted with its start and end in seconds (offset by the audio already consumed) and
/// advances the predictor state; a positive duration skips that many frames; a blank, or
/// `max_symbols_per_frame` symbols on one frame, advances one frame. The duration table is taken
/// as the identity `[0, 1, 2, ..]`, as the supported exports use it.
///
/// `sample_rate` and `hop_length` convert frames to seconds and chunk sizes; pass the model's
/// feature-extractor values (16000 and 160 for Parakeet TDT). Returns [`Error::Shape`] when the
/// joint logits are shorter than the vocabulary, and the model's own errors otherwise.
pub fn decode_greedy(
    model: &mut OnnxAsr,
    audio: &[f32],
    sample_rate: u32,
    hop_length: usize,
    max_symbols_per_frame: usize,
) -> Result<Vec<DecodedPiece>> {
    let vocab = model.vocab_pieces().to_vec();
    let blank = model.blank_id();
    let frame_s = model.frame_seconds(sample_rate as usize, hop_length);

    // Static-window graphs reject longer inputs: chunk to fit. Mel frames are about
    // samples / hop + 1, so leave a frame of margin.
    let chunk_samples = match model.static_window() {
        Some(win) => win.saturating_sub(2).max(1) * hop_length,
        None => audio.len().max(1),
    };

    let mut pieces = Vec::new();
    let mut time_offset = 0.0f32;
    for chunk in audio.chunks(chunk_samples.max(1)) {
        let mel = model.mel(chunk)?;
        let enc = model.encode(&mel)?;
        let (mut h, mut c) = model.initial_state();
        let mut last = blank as i32;
        let mut t = 0usize;
        while t < enc.used {
            let frame = enc.frame(t);
            let mut symbols = 0usize;
            loop {
                let step = model.joint_step(&frame, last, &h, &c)?;
                if step.logits.len() < vocab.len() {
                    return Err(Error::Shape("joint logits shorter than vocabulary".into()));
                }
                let (tok_logits, dur_logits) = step.logits.split_at(vocab.len());
                let tok = argmax(tok_logits);
                let dur = if dur_logits.is_empty() {
                    0
                } else {
                    argmax(dur_logits)
                };
                if tok != blank {
                    let piece = vocab
                        .get(tok)
                        .ok_or_else(|| Error::Shape("token id outside vocabulary".into()))?;
                    pieces.push(DecodedPiece {
                        token_id: tok,
                        piece: piece.clone(),
                        start: time_offset + t as f32 * frame_s,
                        end: time_offset + (t + dur.max(1)) as f32 * frame_s,
                    });
                    h = step.state_h;
                    c = step.state_c;
                    last = tok as i32;
                }
                symbols += 1;
                if dur > 0 {
                    t += dur;
                    break;
                }
                if tok == blank || symbols >= max_symbols_per_frame {
                    t += 1;
                    break;
                }
            }
        }
        time_offset += chunk.len() as f32 / sample_rate as f32;
    }
    Ok(pieces)
}
