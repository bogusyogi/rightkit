//! Fast alignment of a spotter-confirmed terminal command.
//!
//! This is intentionally separate from ordinary transcription. It aligns the
//! supplied command against the final CoreML window so callers can remove the
//! command from their already decoded transcript. The fast locator is the
//! validated hybrid: a +10 first-token locator, followed by the exact lattice
//! when that token is not emitted.

use std::collections::HashMap;

use objc2::rc::{autoreleasepool, Retained};
use objc2_core_ml::MLMultiArray;
use rightkit_asr::parakeet_core::tdt_beam::log_softmax;

use crate::coreml::{ml_f16, ml_i32, read_f16};

use super::model::CoreMlParakeet;
use super::stats::argmax;

const LATTICE_FRAME_SEC: f32 = 0.08;
const LATEST_EXTRA_SEC: f32 = 1.4;
const FIRST_TOKEN_BIAS: f32 = 10.0;
pub const MIN_COMMAND_LOGPROB_MEAN: f32 = -6.8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandAlignmentMethod {
    Fast,
    Full,
}

impl CommandAlignmentMethod {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Full => "full",
        }
    }
}

impl std::fmt::Display for CommandAlignmentMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct CommandAlignment {
    pub boundary_sec: f32,
    pub accepted: bool,
    /// Stable string form consumed by cross-platform control-cleanup code.
    pub method: &'static str,
    pub method_kind: CommandAlignmentMethod,
    /// Fast results contain the first-token locator score. Full results
    /// contain mean forced-command log probability (the acceptance score).
    pub score: f32,
    pub joint_calls: usize,
}

impl CommandAlignment {
    pub const fn method_string(&self) -> &'static str {
        self.method
    }
}

#[derive(Clone)]
struct State {
    h: Vec<f32>,
    c: Vec<f32>,
    last_tok: i32,
    t: usize,
    sym: usize,
    score: f32,
    ids: Vec<usize>,
}

type MemoValue = (Vec<f32>, usize, Vec<f32>, Vec<f32>);
type Memo = HashMap<(i32, u64, usize), MemoValue>;
type PredictorValue = (Vec<f32>, Vec<f32>, Vec<f32>, Retained<MLMultiArray>);

impl CoreMlParakeet {
    /// Whether this loaded model has the production 15-second TDT shape used
    /// by command alignment. Other CoreML models keep ordinary decode paths.
    pub fn supports_control_alignment(&self) -> bool {
        self.meta.is_tdt
            && self.win_samples == 15 * 16_000
            && (self.meta.window_sec - 15.0).abs() < f32::EPSILON
    }

    pub fn supports_control_cleanup(&self) -> bool {
        self.supports_control_alignment()
    }

    /// Align a spotter-confirmed terminal command in one conditioned window.
    ///
    /// `earliest_sec` is the supplied spotter onset minus one second. Search
    /// uses the validated +1.4 second cap, stops the greedy pass at that cap,
    /// and gives the first command token a +10 locator bias. If the locator
    /// never emits that token, the exact unbiased lattice runs with the memo
    /// populated by the locator pass.
    pub fn align_control_suffix(
        &self,
        conditioned_audio: &[f32],
        command: &str,
        earliest_sec: f32,
    ) -> Result<CommandAlignment, String> {
        if !self.meta.is_tdt {
            return Err("command alignment requires a TDT model".into());
        }
        if conditioned_audio.is_empty() {
            return Err("command alignment requires non-empty audio".into());
        }
        if conditioned_audio.len() > self.win_samples {
            return Err("audio longer than one CoreML window".into());
        }
        if !conditioned_audio.iter().all(|sample| sample.is_finite()) {
            return Err("audio contains a non-finite sample".into());
        }
        if command.trim().is_empty() {
            return Err("command must be non-empty".into());
        }
        if !earliest_sec.is_finite() {
            return Err("earliest_sec must be finite".into());
        }
        if self.meta.n_mel == 0
            || self.meta.mel_frames == 0
            || self.meta.pred_layers == 0
            || self.meta.pred_hidden == 0
            || self.meta.durations.is_empty()
        {
            return Err("CoreML model metadata is incomplete".into());
        }

        let suffix = rightkit_asr::parakeet_core::bias::encode_phrase_greedy(&self.pieces, command)
            .ok_or("command has no token encoding")?;
        if suffix.is_empty() {
            return Err("command has no token encoding".into());
        }

        let m = &self.meta;
        let (pl, ph, blank, nd) = (m.pred_layers, m.pred_hidden, m.blank_id, m.durations.len());
        let max_symbols = 10usize;

        let mut padded = vec![0f32; self.win_samples];
        padded[..conditioned_audio.len()].copy_from_slice(conditioned_audio);
        let mel_full = autoreleasepool(|_| -> Result<Vec<f32>, String> {
            let audio_in = ml_f16(&[1, self.win_samples], &padded)?;
            let len_in = ml_i32(&[1], &[conditioned_audio.len() as i32])?;
            let out = self.mel.predict(
                &[("audio", &audio_in), ("audio_len", &len_in)],
                &self.mel_out,
            )?;
            Ok(read_f16(&out))
        })?;
        if mel_full.len() % m.n_mel != 0 {
            return Err("MelSpectrogram output has invalid shape".into());
        }
        let mel_t_full = mel_full.len() / m.n_mel;
        if mel_t_full < m.mel_frames {
            return Err("MelSpectrogram output is shorter than model metadata".into());
        }
        let mut mel = vec![0f32; m.n_mel * m.mel_frames];
        for c in 0..m.n_mel {
            for t in 0..m.mel_frames {
                mel[c * m.mel_frames + t] = mel_full[c * mel_t_full + t];
            }
        }

        let enc_flat = autoreleasepool(|_| -> Result<Vec<f32>, String> {
            let mel_in = ml_f16(&[1, m.n_mel, m.mel_frames], &mel)?;
            let len_in = ml_i32(&[1], &[m.mel_frames as i32])?;
            let out = self.enc.predict(
                &[("melspectrogram_features", &mel_in), ("mel_len", &len_in)],
                &self.enc_out,
            )?;
            Ok(read_f16(&out))
        })?;
        let enc_d = 1024usize;
        if enc_flat.len() % enc_d != 0 {
            return Err("AudioEncoder output has invalid shape".into());
        }
        let enc_t = enc_flat.len() / enc_d;
        let nframes = enc_t.min(((conditioned_audio.len() as f32) / 160.0 / 8.0).ceil() as usize);
        if nframes == 0 {
            return Err("audio contains no decodable CoreML frames".into());
        }
        let encoded =
            |t: usize| -> Vec<f32> { (0..enc_d).map(|d| enc_flat[d * enc_t + t]).collect() };
        let encoded_arrays: Vec<Retained<MLMultiArray>> = (0..nframes)
            .map(|t| ml_f16(&[1, enc_d], &encoded(t)))
            .collect::<Result<_, _>>()?;

        let mut memo = Memo::new();
        let state_key = |s: &State| -> (i32, u64, usize) {
            let mut hash = 0xcbf29ce484222325u64;
            for value in s.h.iter().chain(s.c.iter()) {
                hash ^= u64::from(value.to_bits());
                hash = hash.wrapping_mul(0x100000001b3);
            }
            (s.last_tok, hash, s.t)
        };
        let mut predictor_cache = HashMap::<(i32, Vec<u32>), PredictorValue>::new();
        let mut joint_calls = 0usize;

        let mut step = |s: &State, memo: &mut Memo, prefetch_end: Option<usize>| {
            let key = state_key(s);
            if let Some(hit) = memo.get(&key) {
                return Ok(hit.clone());
            }
            if s.t >= nframes {
                return Err("decoder step is beyond available frames".to_string());
            }

            let predictor_key = (
                s.last_tok,
                s.h.iter()
                    .chain(s.c.iter())
                    .map(|value| value.to_bits())
                    .collect::<Vec<_>>(),
            );
            let (_g, new_h, new_c, decoder_input) =
                if let Some(value) = predictor_cache.get(&predictor_key) {
                    value.clone()
                } else {
                    let value = autoreleasepool(|_| -> Result<_, String> {
                        let ids = ml_i32(&[1, 1], &[s.last_tok])?;
                        let hin = ml_f16(&[pl, 1, ph], &s.h)?;
                        let cin = ml_f16(&[pl, 1, ph], &s.c)?;
                        let outs = self.dec.predict_multi(
                            &[
                                ("decoder_input_ids", &ids),
                                ("state_h", &hin),
                                ("state_c", &cin),
                            ],
                            &[&self.g_name, &self.state_h_out, &self.state_c_out],
                        )?;
                        Ok((read_f16(&outs[0]), read_f16(&outs[1]), read_f16(&outs[2])))
                    })?;
                    let decoder_input = ml_f16(&[1, ph], &value.0)?;
                    let value = (value.0, value.1, value.2, decoder_input);
                    predictor_cache.insert(predictor_key, value.clone());
                    value
                };

            let end = prefetch_end.unwrap_or(s.t + 1).min(nframes);
            let times: Vec<usize> = (s.t..end)
                .filter(|t| !memo.contains_key(&(key.0, key.1, *t)))
                .collect();
            if times.is_empty() {
                return Ok(memo
                    .get(&key)
                    .ok_or_else(|| "decoder memo lookup failed".to_string())?
                    .clone());
            }
            let logits = autoreleasepool(|_| -> Result<Vec<Vec<f32>>, String> {
                if times.len() == 1 {
                    let out = self.joint.predict(
                        &[
                            ("encoder_step", &encoded_arrays[times[0]]),
                            ("decoder_step", &decoder_input),
                        ],
                        &self.joint_out,
                    )?;
                    Ok(vec![read_f16(&out)])
                } else {
                    let encoders: Vec<_> = times.iter().map(|t| &encoded_arrays[*t]).collect();
                    self.joint
                        .predict_joint_batch(&encoders, &decoder_input, &self.joint_out)
                        .map(|outs| outs.iter().map(|array| read_f16(array)).collect())
                }
            })?;
            joint_calls = joint_calls.saturating_add(times.len());
            if logits.len() != times.len() {
                return Err("CoreML joint batch size mismatch".into());
            }
            for (t, values) in times.into_iter().zip(logits) {
                if values.len() <= nd {
                    return Err("Joint output is shorter than TDT duration head".into());
                }
                let token_count = values.len() - nd;
                if blank < 0 || blank as usize >= token_count {
                    return Err("TDT blank token is outside Joint output".into());
                }
                if suffix.iter().any(|token| *token >= token_count) {
                    return Err("command token is outside Joint output".into());
                }
                let dur = m.durations[argmax(&values[token_count..])].max(0) as usize;
                memo.insert(
                    (key.0, key.1, t),
                    (
                        log_softmax(&values[..token_count]),
                        dur,
                        new_h.clone(),
                        new_c.clone(),
                    ),
                );
            }
            Ok(memo
                .get(&key)
                .ok_or_else(|| "CoreML joint batch did not fill memo".to_string())?
                .clone())
        };

        let emit = |s: &mut State, tok: usize, lp: f32, dur: usize, h: Vec<f32>, c: Vec<f32>| {
            s.score += lp;
            s.ids.push(tok);
            s.last_tok = tok as i32;
            s.h = h;
            s.c = c;
            s.sym += 1;
            if !(dur == 0 && s.sym < max_symbols) {
                s.t += dur.max(1);
                s.sym = 0;
            }
        };

        let earliest =
            ((earliest_sec.max(0.0) / LATTICE_FRAME_SEC) as usize).min(nframes.saturating_sub(1));
        let latest = (((earliest_sec.max(0.0) + LATEST_EXTRA_SEC) / LATTICE_FRAME_SEC) as usize)
            .min(nframes)
            .max(earliest + 1);
        let mut state;
        let snapshots;
        let mut locator_active = true;
        loop {
            let mut skipped_candidate: Option<(usize, f32)> = None;
            let greedy_end = latest;
            state = State {
                h: vec![0f32; pl * ph],
                c: vec![0f32; pl * ph],
                last_tok: blank,
                t: 0,
                sym: 0,
                score: 0.0,
                ids: Vec::new(),
            };
            let mut pass_snapshots: Vec<Option<State>> = vec![None; nframes + 1];
            pass_snapshots[0] = Some(state.clone());
            while state.t < greedy_end {
                let previous_frame = state.t;
                if state.sym >= max_symbols {
                    state.t += 1;
                    state.sym = 0;
                } else {
                    let (lp, dur, h, c) = step(&state, &mut memo, None)?;
                    let tok = if locator_active && state.t >= earliest {
                        let mut biased = lp.clone();
                        biased[suffix[0]] += FIRST_TOKEN_BIAS;
                        argmax(&biased)
                    } else {
                        argmax(&lp)
                    };
                    if locator_active && state.t >= earliest && tok == suffix[0] {
                        let (boundary_frame, boundary_score) =
                            skipped_candidate.unwrap_or((state.t, lp[tok]));
                        return Ok(CommandAlignment {
                            boundary_sec: boundary_frame as f32 * LATTICE_FRAME_SEC,
                            accepted: true,
                            method: CommandAlignmentMethod::Fast.as_str(),
                            method_kind: CommandAlignmentMethod::Fast,
                            score: boundary_score,
                            joint_calls,
                        });
                    }
                    if tok as i32 == blank {
                        state.score += lp[tok];
                        state.t += dur.max(1);
                        state.sym = 0;
                    } else {
                        emit(&mut state, tok, lp[tok], dur, h, c);
                    }
                }
                if locator_active && skipped_candidate.is_none() {
                    // Probe skipped candidate frames without changing greedy
                    // token history or committing a probe's recurrent state.
                    for candidate in
                        rightkit_asr::parakeet_core::command_alignment::skipped_locator_frames(
                            previous_frame,
                            state.t,
                            earliest,
                            latest,
                        )
                    {
                        let mut probe = state.clone();
                        probe.t = candidate;
                        let (lp, _, _, _) = step(&probe, &mut memo, None)?;
                        let best = lp.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                        if lp[suffix[0]] + FIRST_TOKEN_BIAS > best {
                            skipped_candidate = Some((candidate, lp[suffix[0]]));
                            break;
                        }
                    }
                }
                for slot in pass_snapshots
                    .iter_mut()
                    .take(state.t.min(nframes) + 1)
                    .skip(1)
                {
                    if slot.is_none() {
                        *slot = Some(state.clone());
                    }
                }
            }
            if locator_active {
                locator_active = false;
                continue;
            }
            snapshots = pass_snapshots;
            break;
        }
        let unconstrained = state;

        let k_total = suffix.len();
        let mut best: Option<(f32, usize, f32)> = None;
        let mut totals: Vec<(usize, f32)> = Vec::new();
        for (boundary, slot) in snapshots.iter().enumerate().take(latest).skip(earliest) {
            let Some(prefix) = slot.clone() else {
                continue;
            };
            if prefix.t != boundary {
                continue;
            }
            let mut predictors: Vec<(i32, Vec<f32>, Vec<f32>)> =
                vec![(prefix.last_tok, prefix.h.clone(), prefix.c.clone())];
            for k in 0..k_total {
                let (last_tok, h, c) = predictors[k].clone();
                let probe = State {
                    h,
                    c,
                    last_tok,
                    t: boundary,
                    sym: 0,
                    score: 0.0,
                    ids: Vec::new(),
                };
                let (_, _, new_h, new_c) = step(&probe, &mut memo, None)?;
                predictors.push((suffix[k] as i32, new_h, new_c));
            }
            let span = nframes - boundary;
            if span == 0 {
                continue;
            }
            let neg = f32::NEG_INFINITY;
            let mut lp_blank = vec![vec![neg; span]; k_total + 1];
            let mut lp_tok = vec![vec![neg; span]; k_total];
            for k in 0..=k_total {
                let (last_tok, h, c) = predictors[k].clone();
                for i in 0..span {
                    let probe = State {
                        h: h.clone(),
                        c: c.clone(),
                        last_tok,
                        t: boundary + i,
                        sym: 0,
                        score: 0.0,
                        ids: Vec::new(),
                    };
                    let (lp, _, _, _) = step(&probe, &mut memo, Some(nframes))?;
                    lp_blank[k][i] = lp[blank as usize];
                    if k < k_total {
                        lp_tok[k][i] = lp[suffix[k]];
                    }
                }
            }
            let mut alpha = vec![vec![neg; span]; k_total + 1];
            let mut token_mass = vec![vec![0f32; span]; k_total + 1];
            alpha[0][0] = prefix.score;
            for i in 0..span {
                for k in 0..=k_total {
                    let mut value = alpha[k][i];
                    let mut mass = token_mass[k][i];
                    if i > 0
                        && alpha[k][i - 1] > neg
                        && alpha[k][i - 1] + lp_blank[k][i - 1] > value
                    {
                        value = alpha[k][i - 1] + lp_blank[k][i - 1];
                        mass = token_mass[k][i - 1];
                    }
                    if k > 0 && alpha[k - 1][i] > neg && alpha[k - 1][i] + lp_tok[k - 1][i] > value
                    {
                        value = alpha[k - 1][i] + lp_tok[k - 1][i];
                        mass = token_mass[k - 1][i] + lp_tok[k - 1][i];
                    }
                    alpha[k][i] = value;
                    token_mass[k][i] = mass;
                }
            }
            let total = alpha[k_total][span - 1] + lp_blank[k_total][span - 1];
            if total > neg {
                totals.push((boundary, total));
                let command_score = token_mass[k_total][span - 1] / k_total as f32;
                if best.as_ref().is_none_or(|(score, _, _)| total > *score) {
                    best = Some((total, boundary, command_score));
                }
            }
        }
        let Some((_constrained_score, boundary, command_score)) = best else {
            return Err("the command could not be aligned in this window".into());
        };
        let _ = unconstrained;
        let accepted = command_score >= MIN_COMMAND_LOGPROB_MEAN;
        Ok(CommandAlignment {
            boundary_sec: boundary as f32 * LATTICE_FRAME_SEC,
            accepted,
            method: CommandAlignmentMethod::Full.as_str(),
            method_kind: CommandAlignmentMethod::Full,
            score: command_score,
            joint_calls,
        })
    }
}
