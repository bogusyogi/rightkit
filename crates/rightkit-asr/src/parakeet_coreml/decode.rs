//! One window: mel, encoder, then a beam-search TDT/RNN-T decode with one LSTM
//! state per hypothesis. The search itself is the shared
//! [`crate::parakeet_core::tdt_beam`]; this file is the CoreML execution that
//! drives it. Donor: HeardRight `coreml_asr/decode_window.rs` (sequential split
//! decoder path, which is the path production runs without the optional fused
//! graph; fusion/batching do not change decoded text).
//!
//! AT WIDTH 1 THIS IS BIT-IDENTICAL TO THE PRE-BEAM GREEDY DECODE:
//! `rank_top_k` ranks on `logit + bias bonus`, the exact arithmetic the retired
//! greedy `argmax` used, never on the log-softmax `score`, which only orders
//! ALTERNATIVE hypotheses and is a no-op with a single hypothesis.

use super::{CoreMlParakeet, ENCODER_DIM};
use crate::error::{Error, Result};
use crate::parakeet_core::policy::PruningMode;
use crate::parakeet_core::tdt_beam::{force_time_advance, rank_top_k, rnnt_step, tdt_step, BeamHyp};
use crate::whisper::coreml::bridge::{ml_f16, ml_i32, read_f16};
use objc2::rc::autoreleasepool;
use std::collections::{BTreeMap, HashMap};

/// `(token_id, absolute_start_sec, duration_frames)` per emitted token.
pub type Hit = (usize, f32, usize);

type Predictor = (Vec<f32>, Vec<f32>, Vec<f32>);

/// Counters for one decode (summed across windows by `transcribe_long`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DecodeStats {
    pub windows: usize,
    /// Encoder frames covering real audio (the length clamp).
    pub frames: usize,
    pub emitted_tokens: usize,
    /// Logical TextDecoder evaluations (a cache hit does not count).
    pub predictor_calls: usize,
    /// Joint evaluations (one per active hypothesis per round).
    pub joint_calls: usize,
    /// Rounds whose best candidate was blank.
    pub blank_steps: usize,
    /// Hypotheses forced forward by the `max_symbols` cap.
    pub forced_advances: usize,
    /// Rounds in which a hypothesis consumed a cached predictor output.
    pub predictor_cache_hits: usize,
}

impl DecodeStats {
    pub fn add(&mut self, other: &DecodeStats) {
        self.windows += other.windows;
        self.frames += other.frames;
        self.emitted_tokens += other.emitted_tokens;
        self.predictor_calls += other.predictor_calls;
        self.joint_calls += other.joint_calls;
        self.blank_steps += other.blank_steps;
        self.forced_advances += other.forced_advances;
        self.predictor_cache_hits += other.predictor_cache_hits;
    }
}

/// Blank descendants retain the exact predictor output; any emission changes
/// `last_tok`/state, so its cached projection is invalid.
fn carry_predictor_cache(
    enabled: bool,
    parent_hits: usize,
    child_hits: usize,
    predictor: &Predictor,
) -> Option<Predictor> {
    (enabled && child_hits == parent_hits).then(|| predictor.clone())
}

fn nz(e: String) -> Error {
    Error::Model(e)
}

fn score_desc(a: &(BeamHyp, Option<Predictor>), b: &(BeamHyp, Option<Predictor>)) -> std::cmp::Ordering {
    b.0.score.partial_cmp(&a.0.score).unwrap_or(std::cmp::Ordering::Equal)
}

impl CoreMlParakeet {
    fn run_decoder(&self, last_tok: i32, h: &[f32], c: &[f32]) -> Result<Predictor> {
        let (pl, ph) = (self.meta.pred_layers, self.meta.pred_hidden);
        autoreleasepool(|_| -> Result<Predictor> {
            let ids = ml_i32(&[1, 1], &[last_tok]).map_err(nz)?;
            let hin = ml_f16(&[pl, 1, ph], h).map_err(nz)?;
            let cin = ml_f16(&[pl, 1, ph], c).map_err(nz)?;
            let outs = self
                .dec
                .predict_multi(
                    &[("decoder_input_ids", &ids), ("state_h", &hin), ("state_c", &cin)],
                    &[&self.g_name, &self.state_h_out, &self.state_c_out],
                )
                .map_err(nz)?;
            Ok((read_f16(&outs[0]), read_f16(&outs[1]), read_f16(&outs[2])))
        })
    }

    fn run_joint(&self, g: &[f32], enc_step: &[f32]) -> Result<Vec<f32>> {
        let ph = self.meta.pred_hidden;
        autoreleasepool(|_| -> Result<Vec<f32>> {
            let enc_in = ml_f16(&[1, ENCODER_DIM], enc_step).map_err(nz)?;
            let dec_in = ml_f16(&[1, ph], g).map_err(nz)?;
            let out = self
                .joint
                .predict(&[("encoder_step", &enc_in), ("decoder_step", &dec_in)], &self.joint_out)
                .map_err(nz)?;
            Ok(read_f16(&out))
        })
    }

    /// Decode one window of at most `window_samples` samples. Hit times are
    /// `win_start_sec + frame * 80 ms`.
    pub fn decode_window(&self, seg: &[f32], win_start_sec: f32) -> Result<(Vec<Hit>, DecodeStats)> {
        let m = &self.meta;
        let o = &self.options;
        let (pl, ph) = (m.pred_layers, m.pred_hidden);
        let blank = m.blank_id;
        let nd = m.durations.len();
        let duration_head_len = if m.is_tdt { nd } else { 0 };
        let width = o.beam_width.max(1);
        let max_symbols = o.max_symbols.max(1);
        let mut stats = DecodeStats { windows: 1, ..Default::default() };

        // --- frontend: pad to the static window, mel, then encoder ---
        let mut padded = vec![0f32; self.win_samples];
        padded[..seg.len()].copy_from_slice(seg);
        let mel_full = autoreleasepool(|_| -> Result<Vec<f32>> {
            let audio_in = ml_f16(&[1, self.win_samples], &padded).map_err(nz)?;
            let len_in = ml_i32(&[1], &[seg.len() as i32]).map_err(nz)?;
            let out = self
                .mel
                .predict(&[("audio", &audio_in), ("audio_len", &len_in)], &self.mel_out)
                .map_err(nz)?;
            Ok(read_f16(&out))
        })?;
        // mel_full shape [1, n_mel, mel_frames+?]; slice to [n_mel, mel_frames].
        let mel_t_full = mel_full.len() / m.n_mel;
        if mel_t_full < m.mel_frames {
            return Err(Error::Model(format!(
                "MelSpectrogram produced {mel_t_full} frames, pipeline.json needs {}",
                m.mel_frames
            )));
        }
        let mut mel = vec![0f32; m.n_mel * m.mel_frames];
        for ch in 0..m.n_mel {
            mel[ch * m.mel_frames..(ch + 1) * m.mel_frames]
                .copy_from_slice(&mel_full[ch * mel_t_full..ch * mel_t_full + m.mel_frames]);
        }
        let enc_flat = autoreleasepool(|_| -> Result<Vec<f32>> {
            let mel_in = ml_f16(&[1, m.n_mel, m.mel_frames], &mel).map_err(nz)?;
            let len_in = ml_i32(&[1], &[m.mel_frames as i32]).map_err(nz)?;
            let out = self
                .enc
                .predict(&[("melspectrogram_features", &mel_in), ("mel_len", &len_in)], &self.enc_out)
                .map_err(nz)?;
            Ok(read_f16(&out))
        })?;
        if enc_flat.len() % ENCODER_DIM != 0 {
            return Err(Error::Model(format!(
                "encoder output length {} is not divisible by {ENCODER_DIM}",
                enc_flat.len()
            )));
        }
        // enc_flat is [enc_d, enc_T] row-major (batch 1 dropped).
        let enc_t = enc_flat.len() / ENCODER_DIM;
        let encoded = |t: usize| -> Vec<f32> { (0..ENCODER_DIM).map(|d| enc_flat[d * enc_t + t]).collect() };

        // Length clamp: only decode frames covering REAL audio, never the pad.
        let nframes = enc_t.min(((seg.len() as f32) / 160.0 / 8.0).ceil() as usize);
        stats.frames = nframes;

        // Fresh decoder state per window; one hypothesis to start.
        let root = BeamHyp {
            h: vec![0f32; pl * ph],
            c: vec![0f32; pl * ph],
            last_tok: blank,
            t: 0,
            sym: 0,
            score: 0.0,
            hits: Vec::new(),
        };
        let mut beam: Vec<(BeamHyp, Option<Predictor>)> = vec![(root, None)];
        let mut buckets: BTreeMap<usize, Vec<(BeamHyp, Option<Predictor>)>> = BTreeMap::new();
        let mut finished: Vec<(BeamHyp, Option<Predictor>)> = Vec::new();
        let timesync = o.pruning == PruningMode::TimeSynchronous;
        let no_bonus: HashMap<usize, f32> = HashMap::new();

        // Defensive round cap: every round advances some hypothesis's `t` or
        // `sym`, both bounded, so this never legitimately fires; a logic bug
        // degrades to a truncated transcript instead of an infinite loop.
        let max_rounds = nframes.saturating_add(1) * max_symbols + 4;
        let mut rounds = 0usize;
        while beam.iter().any(|(hyp, _)| hyp.t < nframes) {
            rounds += 1;
            if rounds > max_rounds {
                break;
            }
            let mut next: Vec<(BeamHyp, Option<Predictor>)> = Vec::with_capacity(beam.len() * width);
            for (hyp, cached) in beam.drain(..) {
                if hyp.t >= nframes {
                    next.push((hyp, cached));
                    continue;
                }
                if hyp.sym >= max_symbols {
                    stats.forced_advances += 1;
                    next.push((force_time_advance(&hyp), cached));
                    continue;
                }
                let predictor = match cached {
                    Some(p) if o.predictor_cache => {
                        stats.predictor_cache_hits += 1;
                        p
                    }
                    _ => {
                        stats.predictor_calls += 1;
                        self.run_decoder(hyp.last_tok, &hyp.h, &hyp.c)?
                    }
                };
                let logits = self.run_joint(&predictor.0, &encoded(hyp.t))?;
                stats.joint_calls += 1;
                let tok_len = logits.len().saturating_sub(duration_head_len);
                if tok_len <= blank as usize {
                    return Err(Error::Model(format!(
                        "Joint emitted {tok_len} token logits but blank_id is {blank}"
                    )));
                }
                let emitted: Vec<usize> = hyp.hits.iter().map(|e| e.0).collect();
                let bonuses = self
                    .bias
                    .as_ref()
                    .map(|b| b.next_token_bonuses(&emitted))
                    .unwrap_or_else(|| no_bonus.clone());
                if rank_top_k(&logits[..tok_len], &bonuses, 1).first() == Some(&(blank as usize)) {
                    stats.blank_steps += 1;
                }
                let (new_h, new_c) = (&predictor.1, &predictor.2);
                let children = if m.is_tdt {
                    let durations = &m.durations;
                    tdt_step(
                        &hyp, &logits, nd, durations, blank, max_symbols, width, o.emit_reward,
                        win_start_sec, &bonuses, new_h, new_c,
                    )
                } else {
                    rnnt_step(
                        &hyp, &logits, blank, width, o.emit_reward, win_start_sec, &bonuses, new_h, new_c,
                    )
                };
                let parent_hits = hyp.hits.len();
                next.extend(children.into_iter().map(|child| {
                    let cache = carry_predictor_cache(
                        o.predictor_cache,
                        parent_hits,
                        child.hits.len(),
                        &predictor,
                    );
                    (child, cache)
                }));
            }
            if timesync {
                // Only paths reading the same next frame compete; completed
                // paths wait in `finished`.
                for row in next {
                    if row.0.t >= nframes {
                        finished.push(row);
                    } else {
                        buckets.entry(row.0.t).or_default().push(row);
                    }
                }
                beam = buckets.pop_first().map(|(_, rows)| rows).unwrap_or_default();
            } else {
                beam = next;
            }
            beam.sort_by(score_desc);
            beam.truncate(width);
        }
        if timesync {
            finished.append(&mut beam);
            for (_, mut rows) in std::mem::take(&mut buckets) {
                finished.append(&mut rows);
            }
            beam = finished;
        }
        beam.sort_by(score_desc);
        let hits = beam.into_iter().next().map(|(best, _)| best.hits).unwrap_or_default();
        stats.emitted_tokens = hits.len();
        Ok((hits, stats))
    }
}
