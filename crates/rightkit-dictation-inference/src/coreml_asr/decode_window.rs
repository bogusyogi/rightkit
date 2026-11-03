// macOS CoreML decode loop. The beam search itself is first-party and
// platform-neutral in `rightkit_asr::parakeet_core::tdt_beam`; what remains here is
// the CoreML execution that drives it.

use objc2::rc::autoreleasepool;
use rightkit_asr::parakeet_core::tdt_beam::{
    force_time_advance, rank_top_k, rnnt_step, tdt_step, BeamHyp,
};

use crate::coreml::{ml_f16, ml_i32, read_f16};

use super::frontend_lookahead_probe::EncodedWindow;
use super::fused_probe::FusedProbeRow;
use super::hypothesis_batch::{batch_chunk_size, resolve_hypothesis_batch_config};
use super::model::CoreMlParakeet;
use super::stats::{argmax, stats};
use super::window_health::{self, AsrWindowStats, ASR_WINDOW_STATS_UPSTREAM_INPUTS_NEEDED};

type PredictorOutputs = (Vec<f32>, Vec<f32>, Vec<f32>);
thread_local! { static PROBE_COUNTS: std::cell::Cell<[usize;4]> = const { std::cell::Cell::new([0;4]) }; }
thread_local! { static FUSED_COUNTS: std::cell::Cell<[usize;2]> = const { std::cell::Cell::new([0;2]) }; }
/// Fused API invocations & logical fused rows; shared calls count only once.
pub fn take_fused_probe_counts() -> [usize; 2] {
    FUSED_COUNTS.with(|v| v.replace([0; 2]))
}
/// Logical predictor/joint evaluations followed by their runtime API invocations.
pub fn take_decoder_probe_counts() -> [usize; 4] {
    PROBE_COUNTS.with(|v| v.replace([0; 4]))
}

fn carry_predictor_cache(
    enabled: bool,
    parent_hit_count: usize,
    child_hit_count: usize,
    predictor: &PredictorOutputs,
) -> Option<PredictorOutputs> {
    // Blank descendants retain the exact predictor state and projection. Any
    // emission changes last_tok/state, so its cached projection is invalid.
    (enabled && child_hit_count == parent_hit_count).then(|| predictor.clone())
}

impl CoreMlParakeet {
    /// One window: mel → encoder → beam-search TDT/RNN-T decode, one LSTM
    /// state per hypothesis. Pushes (piece_id, absolute_start_sec,
    /// duration_frames) per emitted token onto `hits`, taken from the single
    /// highest-scoring COMPLETE hypothesis. TDT predicts the duration; `0`
    /// frames is legal and means "another token is emitted at this same
    /// frame" -- the last token of such a chain carries the real advance.
    /// RNNT has no duration head, so its tokens are 1 frame.
    ///
    /// Beam width is `self.beam_width` (default 2; see `set_beam_width` /
    /// `RIGHTKIT_ASR_BEAM_WIDTH`). AT WIDTH 1 THIS IS BIT-IDENTICAL TO THE
    /// PRE-BEAM GREEDY DECODE: `rank_top_k` ranks on the raw
    /// `logit + bias-bonus` sum — the exact arithmetic the retired
    /// `argmax`/`biased_argmax` used — never on the log-softmax `score`
    /// field, which only ranks ALTERNATIVE hypotheses against each other
    /// and is a complete no-op when there is only one hypothesis in the
    /// beam (nothing to compare it to). Every branch below (dur==0 chaining,
    /// the max_symbols cap, blank vs non-blank state advance) mirrors the
    /// old single-hypothesis control flow step for step; see the
    /// `beam_width_one_matches_greedy_*` tests in this file for the proof —
    /// this fn calls real CoreML models, so it can't be unit-tested
    /// directly, and per the task's acceptance bar the "scoring core"
    /// (`tdt_step`/`rnnt_step`/`rank_top_k`/`log_softmax`) is factored out
    /// pure and exercised there with synthetic logits instead.
    ///
    /// Context bias applies at the HYPOTHESIS level: `bonuses` is
    /// recomputed per hypothesis from ITS OWN emitted-token history each
    /// step, so a hypothesis that has partially matched a multi-token bias
    /// phrase keeps earning bonuses as it extends the match — letting a
    /// beam width > 1 keep a complete two-token phrase alive long enough to
    /// outscore a locally-better single-token guess, which a per-token
    /// nudge on a single greedy path can never do.
    pub(crate) fn decode_window(
        &self,
        seg: &[f32],
        max_symbols: usize,
        win_start_sec: f32,
        hits: &mut Vec<(usize, f32, usize)>,
    ) -> Result<(), String> {
        self.decode_window_with_frontend(seg, max_symbols, win_start_sec, hits, None)
    }

    pub(crate) fn decode_window_with_frontend(
        &self,
        seg: &[f32],
        max_symbols: usize,
        win_start_sec: f32,
        hits: &mut Vec<(usize, f32, usize)>,
        frontend: Option<EncodedWindow>,
    ) -> Result<(), String> {
        let window_started = std::time::Instant::now();
        let frontend_source = if frontend.is_some() {
            "lookahead_cache"
        } else {
            "synchronous"
        };
        let mut mel_predict_ms = 0.0f64;
        let mut encoder_predict_ms = 0.0f64;
        let _reuse_scope = super::input_reuse_probe::production_scope(
            self.fused_probe.is_some() && self.evaluator_fused_enabled.is_none(),
        );
        window_health::clear_latest_asr_window_stats();
        let m = &self.meta;
        // Explicit probes only; unset preserves shipping duration/symbol policy.
        let probe_cap = rightkit_dictation_core::dev_var!("RIGHTKIT_COREML_DURATION_CAP_PROBE")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|v| (0..=4).contains(v));
        let probe_durations: Vec<i64> = m
            .durations
            .iter()
            .map(|&d| probe_cap.map_or(d, |cap| d.min(cap)))
            .collect();
        let max_symbols = rightkit_dictation_core::dev_var!("RIGHTKIT_COREML_MAX_SYMBOLS_PROBE")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| (1..=64).contains(v))
            .unwrap_or(max_symbols);
        let timesync = rightkit_dictation_core::dev_var!("RIGHTKIT_COREML_PRUNING_PROBE")
            .as_deref()
            == Ok("timesync");
        let pl = m.pred_layers;
        let ph = m.pred_hidden;
        let blank = m.blank_id;
        let nd = m.durations.len();
        let duration_head_len = if m.is_tdt { nd } else { 0 };
        let width = self.beam_width.max(1);
        let detailed = crate::settings::asr_detailed_telemetry();
        let collect_diagnostics = detailed || crate::settings::onboarding_calibration_active();
        let decode_started = collect_diagnostics.then(std::time::Instant::now);
        let mut predictor_calls = 0usize;
        let mut joint_calls = 0usize;
        let mut predictor_invocations = 0usize;
        let mut joint_invocations = 0usize;
        let mut joint_batched_invocations = 0usize;
        let mut joint_batched_rows = 0usize;
        let mut fused_invocations = 0usize;
        let mut fused_rows = 0usize;
        let mut fused_enabled = self.fused_probe_enabled();
        // Production is fixed at B2; archived evaluators may request their
        // historical cap through the explicit model configuration API.
        let fused_max_rows = self.fused_max_rows;
        let hypothesis_batch = resolve_hypothesis_batch_config(
            rightkit_dictation_core::dev_var!("RIGHTKIT_COREML_HYP_BATCH")
                .ok()
                .as_deref(),
            rightkit_dictation_core::dev_var!("RIGHTKIT_COREML_HYP_BATCH_MAX_ROWS")
                .ok()
                .as_deref(),
        );
        let mut blank_steps = 0usize;
        let mut forced_advances = 0usize;
        let mut duration_sum_frames = 0usize;
        let mut duration_max_frames = 0usize;
        let mut duration_frames_skipped = 0usize;
        let mut duration_count = 0usize;
        let mut margin_sum = 0.0f64;
        let mut margin_count = 0usize;
        let mut near_tie_count = 0usize;
        let mut bias_applied_count = 0usize;
        let bias_installed_count = if collect_diagnostics {
            self.bias
                .as_ref()
                .filter(|bias| !bias.is_empty())
                .map(|bias| bias.phrase_token_ids.len())
                .unwrap_or(0)
        } else {
            0
        };

        // --- frontend: use one pre-encoded result or run mel → encoder ---
        let (mel_full, enc_flat) = if let Some(cached) = frontend {
            if cached.real_samples != seg.len()
                || cached.encoder_dim != 1024
                || cached.data.len() != cached.encoder_dim * cached.encoder_frames
            {
                return Err("frontend lookahead tensor does not match decode window".into());
            }
            (cached.mel_full, cached.data)
        } else {
            let mut padded = vec![0f32; self.win_samples];
            padded[..seg.len()].copy_from_slice(seg);
            let mel_full = autoreleasepool(|_| -> Result<Vec<f32>, String> {
                let audio_in = ml_f16(&[1, self.win_samples], &padded)?;
                let len_in = ml_i32(&[1], &[seg.len() as i32])?;
                let started = std::time::Instant::now();
                let out = self.mel.predict(
                    &[("audio", &audio_in), ("audio_len", &len_in)],
                    &self.mel_out,
                )?;
                mel_predict_ms = started.elapsed().as_secs_f64() * 1_000.0;
                Ok(read_f16(&out))
            })?;
            // mel_full shape [1, n_mel, mel_frames+?]; slice to [n_mel, mel_frames]
            let mel_t_full = mel_full.len() / m.n_mel;
            let mut mel = vec![0f32; m.n_mel * m.mel_frames];
            for c in 0..m.n_mel {
                for t in 0..m.mel_frames {
                    mel[c * m.mel_frames + t] = mel_full[c * mel_t_full + t];
                }
            }
            let enc_flat = autoreleasepool(|_| -> Result<Vec<f32>, String> {
                let mel_in = ml_f16(&[1, m.n_mel, m.mel_frames], &mel)?;
                let len_in = ml_i32(&[1], &[m.mel_frames as i32])?;
                let started = std::time::Instant::now();
                let out = self.enc.predict(
                    &[("melspectrogram_features", &mel_in), ("mel_len", &len_in)],
                    &self.enc_out,
                )?;
                encoder_predict_ms = started.elapsed().as_secs_f64() * 1_000.0;
                Ok(read_f16(&out))
            })?;
            (mel_full, enc_flat)
        };
        let dbg = rightkit_dictation_core::dev_var!("RIGHTKIT_COREML_DEBUG").is_ok();
        if dbg && !mel_full.is_empty() {
            let (mn, mx, sm) = stats(&mel_full);
            eprintln!(
                "[dbg] seg={} mel_full.len={} min={mn:.3} max={mx:.3} mean={:.3}",
                seg.len(),
                mel_full.len(),
                sm / mel_full.len() as f32
            );
        }
        if enc_flat.len() % 1024 != 0 {
            return Err(format!(
                "encoder output length {} is not divisible by 1024",
                enc_flat.len()
            ));
        }
        // enc_flat is [enc_d, enc_T] row-major (batch 1 dropped). Transpose to (T, enc_d).
        let enc_t = enc_flat.len() / 1024; // enc_d_model = 1024
        let enc_d = 1024usize;
        let encoded = |t: usize| -> Vec<f32> {
            let mut v = vec![0f32; enc_d];
            for d in 0..enc_d {
                v[d] = enc_flat[d * enc_t + t];
            }
            v
        };
        // Includes host-side beam/search work and decoder model calls; model
        // call components are separately reported below only where they are
        // explicitly timed. Frontend values are synchronous predict-call
        // durations, excluding tensor preparation/conversion.
        let decoder_search_started = std::time::Instant::now();
        let mut decoder_model_wall = std::time::Duration::ZERO;

        // length clamp: only decode frames covering REAL audio (not silence pad).
        let nframes = enc_t.min(((seg.len() as f32) / 160.0 / 8.0).ceil() as usize);
        if dbg {
            let (mn, mx, sm) = stats(&enc_flat);
            eprintln!(
                "[dbg] enc_flat.len={} enc_t={enc_t} nframes={nframes} min={mn:.3} max={mx:.3} mean={:.3}",
                enc_flat.len(),
                sm / enc_flat.len() as f32
            );
        }

        // fresh decoder state per window, one hypothesis to start. The cache
        // is deliberately scoped to this window: predictor state resets at
        // every production window boundary.
        let predictor_cache_enabled =
            rightkit_dictation_core::dev_var_os!("RIGHTKIT_ASR_DISABLE_PREDICTOR_CACHE").is_none();
        let root = BeamHyp {
            h: vec![0f32; pl * ph],
            c: vec![0f32; pl * ph],
            last_tok: blank,
            t: 0,
            sym: 0,
            score: 0.0,
            hits: Vec::new(),
        };
        let mut beam: Vec<(BeamHyp, Option<PredictorOutputs>)> = vec![(root, None)];
        let mut buckets: std::collections::BTreeMap<
            usize,
            Vec<(BeamHyp, Option<PredictorOutputs>)>,
        > = std::collections::BTreeMap::new();
        let mut finished = Vec::new();

        // Defensive round cap: every round advances some hypothesis's `t` or
        // `sym`, both bounded (`nframes`, `max_symbols`), so this can never
        // legitimately trigger — it exists so a logic bug degrades to a
        // truncated transcript instead of an infinite loop.
        let max_rounds = nframes.saturating_add(1) * max_symbols.max(1) + 4;
        let mut rounds = 0usize;
        while beam.iter().any(|(hyp, _)| hyp.t < nframes) {
            rounds += 1;
            if rounds > max_rounds {
                break;
            }
            let mut next: Vec<(BeamHyp, Option<PredictorOutputs>)> =
                Vec::with_capacity(beam.len() * width);
            // Evaluate independent eligible rows without changing beam order,
            // frame, state, cache inheritance, duration or emission policy.
            let mut prepared = if hypothesis_batch.mode.is_batched() || fused_enabled {
                vec![None; beam.len()]
            } else {
                Vec::new()
            };
            if fused_enabled {
                let eligible: Vec<_> = beam
                    .iter()
                    .enumerate()
                    .filter(|(_, (h, _))| h.t < nframes && h.sym < max_symbols)
                    .map(|(i, _)| i)
                    .collect();
                let missing: Vec<_> = eligible
                    .iter()
                    .copied()
                    .filter(|&i| !predictor_cache_enabled || beam[i].1.is_none())
                    .collect();
                for indices in missing.chunks(fused_max_rows) {
                    let frames: Vec<_> = indices.iter().map(|&i| encoded(beam[i].0.t)).collect();
                    let rows: Vec<_> = indices
                        .iter()
                        .zip(&frames)
                        .map(|(&i, frame)| {
                            let h = &beam[i].0;
                            FusedProbeRow {
                                last_tok: h.last_tok,
                                h: &h.h,
                                c: &h.c,
                                enc_step: frame,
                            }
                        })
                        .collect();
                    let model_call_started = std::time::Instant::now();
                    let outputs = match self.fused_probe_steps(&rows) {
                        Ok(outputs) => outputs,
                        Err(error) => {
                            decoder_model_wall += model_call_started.elapsed();
                            tracing::warn!(
                                error,
                                "fused decoder failed; retrying rows through split decoder"
                            );
                            fused_enabled = false;
                            // Rows from earlier successful fused chunks stay
                            // prepared; only failed chunk rows are replayed.
                            for &i in indices {
                                let h = &beam[i].0;
                                let model_call_started = std::time::Instant::now();
                                let (g, new_h, new_c, logits) = self.predict_decoder_joint(
                                    h.last_tok,
                                    &h.h,
                                    &h.c,
                                    &encoded(h.t),
                                    pl,
                                    ph,
                                    enc_d,
                                )?;
                                decoder_model_wall += model_call_started.elapsed();
                                prepared[i] = Some(((g, new_h, new_c), logits));
                            }
                            predictor_calls += indices.len();
                            joint_calls += indices.len();
                            predictor_invocations += indices.len();
                            joint_invocations += indices.len();
                            break;
                        }
                    };
                    decoder_model_wall += model_call_started.elapsed();
                    fused_invocations += 1;
                    fused_rows += rows.len();
                    predictor_invocations += 1;
                    joint_invocations += 1;
                    predictor_calls += rows.len();
                    for (&i, out) in indices.iter().zip(outputs) {
                        prepared[i] = Some(((out.g, out.h, out.c), out.logits));
                    }
                }
                // Preserve predictor reuse: only Joint executes for cached rows.
                let cached: Vec<_> = eligible
                    .iter()
                    .copied()
                    .filter(|&i| prepared[i].is_none())
                    .collect();
                let frames: Vec<_> = cached.iter().map(|&i| encoded(beam[i].0.t)).collect();
                let predictions: Vec<_> = cached
                    .iter()
                    .map(|&i| beam[i].1.take().expect("cached eligible row"))
                    .collect();
                let inputs: Vec<_> = predictions
                    .iter()
                    .zip(&frames)
                    .map(|(p, e)| (p.0.as_slice(), e.as_slice()))
                    .collect();
                let mut logits = Vec::with_capacity(inputs.len());
                let chunk = if hypothesis_batch.mode.batches_joint() {
                    batch_chunk_size(inputs.len(), hypothesis_batch.max_rows)
                } else {
                    1
                };
                for rows in inputs.chunks(chunk) {
                    joint_invocations += 1;
                    let model_call_started = std::time::Instant::now();
                    if rows.len() > 1 {
                        joint_batched_invocations += 1;
                        joint_batched_rows += rows.len();
                        logits.extend(self.predict_hypothesis_joints(rows)?);
                    } else {
                        logits.push(self.predict_joint(rows[0].0, rows[0].1, ph, enc_d)?);
                    }
                    decoder_model_wall += model_call_started.elapsed();
                }
                for ((i, p), logits) in cached.into_iter().zip(predictions).zip(logits) {
                    prepared[i] = Some((p, logits));
                }
            } else if hypothesis_batch.mode.is_batched() {
                let eligible: Vec<_> = beam
                    .iter()
                    .enumerate()
                    .filter(|(_, (h, _))| h.t < nframes && h.sym < max_symbols)
                    .map(|(i, _)| i)
                    .collect();
                let missing: Vec<_> = eligible
                    .iter()
                    .copied()
                    .filter(|&i| !predictor_cache_enabled || beam[i].1.is_none())
                    .collect();
                let states: Vec<_> = missing.iter().map(|&i| &beam[i].0).collect();
                let predicted = if hypothesis_batch.mode.batches_predictor() {
                    let chunk_size = batch_chunk_size(states.len(), hypothesis_batch.max_rows);
                    let mut predicted = Vec::with_capacity(states.len());
                    for (state_chunk, index_chunk) in
                        states.chunks(chunk_size).zip(missing.chunks(chunk_size))
                    {
                        let model_call_started = std::time::Instant::now();
                        if state_chunk.len() > 1 {
                            predictor_invocations += 1;
                            predicted.extend(self.predict_hypotheses(state_chunk)?);
                        } else {
                            predictor_invocations += state_chunk.len();
                            predicted.extend(
                                state_chunk
                                    .iter()
                                    .map(|h| self.predict_decoder(h.last_tok, &h.h, &h.c, pl, ph))
                                    .collect::<Result<Vec<_>, _>>()?,
                            );
                        }
                        decoder_model_wall += model_call_started.elapsed();
                    }
                    predicted
                } else {
                    predictor_invocations += states.len();
                    let model_call_started = std::time::Instant::now();
                    let predicted = states
                        .iter()
                        .map(|h| self.predict_decoder(h.last_tok, &h.h, &h.c, pl, ph))
                        .collect::<Result<Vec<_>, _>>()?;
                    decoder_model_wall += model_call_started.elapsed();
                    predicted
                };
                predictor_calls += states.len();
                let mut predictions: Vec<Option<PredictorOutputs>> = vec![None; beam.len()];
                for (&i, p) in missing.iter().zip(predicted) {
                    predictions[i] = Some(p);
                }
                for &i in &eligible {
                    if predictions[i].is_none() {
                        // Prepared rows consume predictor state below; move cached state
                        // instead of cloning it. Ineligible rows retain their cache.
                        predictions[i] = beam[i].1.take();
                    }
                }
                let encoders: Vec<_> = eligible.iter().map(|&i| encoded(beam[i].0.t)).collect();
                let inputs: Vec<_> = eligible
                    .iter()
                    .zip(&encoders)
                    .map(|(&i, e)| (predictions[i].as_ref().unwrap().0.as_slice(), e.as_slice()))
                    .collect();
                let logits = if hypothesis_batch.mode.batches_joint() {
                    let chunk_size = batch_chunk_size(inputs.len(), hypothesis_batch.max_rows);
                    let mut logits = Vec::with_capacity(inputs.len());
                    for input_chunk in inputs.chunks(chunk_size) {
                        let model_call_started = std::time::Instant::now();
                        if input_chunk.len() > 1 {
                            joint_batched_invocations += 1;
                            joint_batched_rows += input_chunk.len();
                            joint_invocations += 1;
                            logits.extend(self.predict_hypothesis_joints(input_chunk)?);
                        } else {
                            joint_invocations += input_chunk.len();
                            logits.extend(
                                input_chunk
                                    .iter()
                                    .map(|(g, e)| self.predict_joint(g, e, ph, enc_d))
                                    .collect::<Result<Vec<_>, _>>()?,
                            );
                        }
                        decoder_model_wall += model_call_started.elapsed();
                    }
                    logits
                } else {
                    joint_invocations += inputs.len();
                    let model_call_started = std::time::Instant::now();
                    let logits = inputs
                        .iter()
                        .map(|(g, e)| self.predict_joint(g, e, ph, enc_d))
                        .collect::<Result<Vec<_>, _>>()?;
                    decoder_model_wall += model_call_started.elapsed();
                    logits
                };
                for (&i, logits) in eligible.iter().zip(logits) {
                    prepared[i] = Some((predictions[i].take().unwrap(), logits));
                }
            }
            for (index, (hyp, cached_predictor)) in beam.drain(..).enumerate() {
                if hyp.t >= nframes {
                    next.push((hyp, cached_predictor));
                    continue;
                }
                if hyp.sym >= max_symbols {
                    if collect_diagnostics {
                        forced_advances += 1;
                    }
                    next.push((force_time_advance(&hyp), cached_predictor));
                    continue;
                }
                let (predictor, logits) = if let Some(prepared) =
                    prepared.get_mut(index).and_then(Option::take)
                {
                    prepared
                } else {
                    let predictor = match cached_predictor {
                        Some(cached) if predictor_cache_enabled => cached,
                        _ => {
                            predictor_calls += 1;
                            predictor_invocations += 1;
                            let model_call_started = std::time::Instant::now();
                            let prediction =
                                self.predict_decoder(hyp.last_tok, &hyp.h, &hyp.c, pl, ph)?;
                            decoder_model_wall += model_call_started.elapsed();
                            prediction
                        }
                    };
                    joint_invocations += 1;
                    let model_call_started = std::time::Instant::now();
                    let logits = self.predict_joint(&predictor.0, &encoded(hyp.t), ph, enc_d)?;
                    decoder_model_wall += model_call_started.elapsed();
                    (predictor, logits)
                };
                let (g, new_h, new_c) = (&predictor.0, &predictor.1, &predictor.2);
                joint_calls += 1;
                let bonuses = self
                    .bias
                    .as_ref()
                    .map(|b| {
                        let start = hyp.hits.len().saturating_sub(b.history_window());
                        let emitted: Vec<usize> =
                            hyp.hits[start..].iter().map(|entry| entry.0).collect();
                        b.next_token_bonuses(&emitted)
                    })
                    .unwrap_or_default();
                if collect_diagnostics {
                    bias_applied_count += usize::from(!bonuses.is_empty());
                }
                let token_logits = &logits[..logits.len().saturating_sub(duration_head_len)];
                if collect_diagnostics {
                    if rank_top_k(token_logits, &bonuses, 1).first() == Some(&(blank as usize)) {
                        blank_steps += 1;
                    }
                    if let Some(margin) = window_health::top1_top2_margin(token_logits, &bonuses) {
                        margin_sum += f64::from(margin);
                        margin_count += 1;
                        if margin <= 0.1 {
                            near_tie_count += 1;
                        }
                    }
                }
                if collect_diagnostics && m.is_tdt && nd > 0 {
                    let duration_index = argmax(&logits[logits.len() - nd..]);
                    let duration = probe_durations[duration_index].max(0) as usize;
                    duration_sum_frames = duration_sum_frames.saturating_add(duration);
                    duration_max_frames = duration_max_frames.max(duration);
                    duration_frames_skipped =
                        duration_frames_skipped.saturating_add(duration.saturating_sub(1));
                    duration_count += 1;
                }

                let children = if m.is_tdt {
                    tdt_step(
                        &hyp,
                        &logits,
                        nd,
                        &probe_durations,
                        blank,
                        max_symbols,
                        width,
                        crate::beam_emit_reward(),
                        win_start_sec,
                        &bonuses,
                        new_h,
                        new_c,
                    )
                } else {
                    rnnt_step(
                        &hyp,
                        &logits,
                        blank,
                        width,
                        crate::beam_emit_reward(),
                        win_start_sec,
                        &bonuses,
                        new_h,
                        new_c,
                    )
                };
                if dbg && hyp.t < 3 {
                    if let Some(top) = children.first() {
                        eprintln!(
                            "[dbg] t={} sym={} blank={blank} tok={:?} score={:.3} g0={:.3}",
                            hyp.t,
                            hyp.sym,
                            top.hits.last().map(|h| h.0),
                            top.score,
                            g.first().copied().unwrap_or(0.0)
                        );
                    }
                }
                let parent_hit_count = hyp.hits.len();
                next.extend(children.into_iter().map(|child| {
                    let child_cache = carry_predictor_cache(
                        predictor_cache_enabled,
                        parent_hit_count,
                        child.hits.len(),
                        &predictor,
                    );
                    (child, child_cache)
                }));
            }
            if timesync {
                // Same semantics as ONNX time-synchronous search: only paths
                // reading the same next frame compete; completed paths wait.
                for row in next {
                    if row.0.t >= nframes {
                        finished.push(row);
                    } else {
                        buckets.entry(row.0.t).or_default().push(row);
                    }
                }
                beam = buckets
                    .pop_first()
                    .map(|(_, rows)| rows)
                    .unwrap_or_default();
            } else {
                beam = next;
            }
            beam.sort_by(|a, b| {
                b.0.score
                    .partial_cmp(&a.0.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            beam.truncate(width);
        }

        if timesync {
            finished.append(&mut beam);
            for (_, mut rows) in buckets {
                finished.append(&mut rows);
            }
            beam = finished;
        }
        beam.sort_by(|a, b| {
            b.0.score
                .partial_cmp(&a.0.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        if rightkit_dictation_core::dev_var_os!("RIGHTKIT_ASR_PREDICTOR_DIAGNOSTICS").is_some() {
            FUSED_COUNTS.with(|v| {
                let mut c = v.get();
                c[0] += fused_invocations;
                c[1] += fused_rows;
                v.set(c);
            });
            PROBE_COUNTS.with(|v| {
                let mut c = v.get();
                for (i, n) in [
                    predictor_calls,
                    joint_calls,
                    predictor_invocations,
                    joint_invocations,
                ]
                .into_iter()
                .enumerate()
                {
                    c[i] += n;
                }
                v.set(c);
            });
        }
        if let Some((best, _)) = beam.into_iter().next() {
            if collect_diagnostics {
                let mel_health = window_health::tensor_health(&mel_full);
                let encoder_health = window_health::tensor_health(&enc_flat);
                let stats = AsrWindowStats {
                    frames_total: nframes,
                    joint_calls,
                    emitted_tokens: best.hits.len(),
                    blank_steps,
                    forced_advances,
                    duration_mean_frames: if duration_count == 0 {
                        0.0
                    } else {
                        duration_sum_frames as f32 / duration_count as f32
                    },
                    duration_max_frames,
                    duration_frames_skipped,
                    no_emission_gaps_over_2s: window_health::no_emission_gaps_over_2s(
                        &best.hits,
                        win_start_sec,
                        nframes,
                    ),
                    top1_top2_margin_mean: if margin_count == 0 {
                        0.0
                    } else {
                        (margin_sum / margin_count as f64) as f32
                    },
                    near_tie_count,
                    bias_installed_count,
                    bias_applied_count,
                    conditioned_audio_rms_quarters: window_health::quarter_rms(seg),
                    mel_rms: mel_health.rms,
                    mel_min: mel_health.min,
                    mel_max: mel_health.max,
                    mel_nonfinite: mel_health.nonfinite,
                    mel_quarters: window_health::quartered_tensor_health(&mel_full),
                    encoder_rms: encoder_health.rms,
                    encoder_min: encoder_health.min,
                    encoder_max: encoder_health.max,
                    encoder_nonfinite: encoder_health.nonfinite,
                    encoder_quarters: window_health::quartered_tensor_health(&enc_flat),
                };
                window_health::publish_asr_window_stats(stats.clone());
                window_health::publish_calibration_window_stats(stats.clone(), win_start_sec);
                if detailed {
                    let elapsed_ms = decode_started
                        .expect("diagnostic timer exists when detailed telemetry is enabled")
                        .elapsed()
                        .as_millis() as u64;
                    let envelope = window_health::AsrWindowStatsEnvelope::new(
                        &stats,
                        &self.model_fingerprint,
                        self.model_dir_sha256.as_deref(),
                        &self.coreml_compile_cache,
                        self.mel.compute_profile(),
                        self.joint.compute_profile(),
                        &self.encoder_compute,
                        &self.decoder_compute,
                        elapsed_ms,
                    );
                    if let Ok(stats_json) = serde_json::to_string(&envelope) {
                        eprintln!("RIGHTKIT_ASR_WINDOW_STATS_JSON={stats_json}");
                    }
                    tracing::info!(
                        event = "asr_window_stats",
                        stats = ?stats,
                        upstream_inputs_needed = ?ASR_WINDOW_STATS_UPSTREAM_INPUTS_NEEDED,
                        encoder_compute = self.encoder_compute,
                        decoder_compute = self.decoder_compute,
                        predictor_calls,
                        predictor_cache_enabled,
                        elapsed_ms,
                        "asr_window_stats"
                    );
                }
            }
            if detailed
                || rightkit_dictation_core::dev_var_os!("RIGHTKIT_ASR_PREDICTOR_DIAGNOSTICS")
                    .is_some()
            {
                eprintln!(
                    "RIGHTKIT_ASR_DECODER_CALLS predictor={predictor_calls} joint={joint_calls} cache={predictor_cache_enabled}"
                );
            }
            hits.extend(best.hits);
        }
        let decoder_search_wall_ms = decoder_search_started.elapsed().as_secs_f64() * 1_000.0;
        let decoder_model_wall_ms = decoder_model_wall.as_secs_f64() * 1_000.0;
        let beam_host_ms = (decoder_search_wall_ms - decoder_model_wall_ms).max(0.0);
        tracing::info!(
            target: "rightkit_asr_stage_timing",
            backend = "coreml",
            window_start_sec = win_start_sec,
            sample_count = seg.len(),
            frontend_source,
            mel_predict_ms,
            encoder_predict_ms,
            decoder_search_wall_ms,
            decoder_model_wall_ms,
            decoder_model_wall_scope =
                "predict-wrapper calls incl tensor prep/conversion/output read",
            beam_host_ms,
            beam_host_scope =
                "decoder_search_wall_ms minus wrapper-call wall; residual outside wrappers, not all host CPU",
            joint_batched_invocations,
            joint_batched_rows,
            joint_scalar_invocations =
                joint_invocations.saturating_sub(joint_batched_invocations),
            window_total_ms = window_started.elapsed().as_secs_f64() * 1_000.0,
            "asr_window_stage_timing"
        );
        crate::timeline_recorder::record_pass_with_detail(
            seg.len(),
            mel_predict_ms,
            encoder_predict_ms,
            decoder_search_wall_ms,
            window_started.elapsed().as_secs_f64() * 1_000.0,
        );
        Ok(())
    }
}
