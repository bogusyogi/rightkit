//! First-party beam decode for the ONNX (Windows / non-macOS unix) path.
//!
//! Windows and Apple platforms share the first-party beam policy. The Windows
//! width-2 path was qualified against the canonical corpus on 2026-09-04; see
//! docs/experiments/2026-09-04-onnx-beam-qualification.md.
//!
//! The search is `rightkit_asr::parakeet_core::tdt_beam` — the same code macOS
//! drives. The vendored crate is reduced to ORT execution: mel, encoder, and one
//! decoder/joint step per hypothesis. Token selection, scoring, the emission
//! reward, context bias and advancement policy are all first-party.
//!
//! Platform policy still differs deliberately and is passed in explicitly:
//! Windows cap 2 is the historically qualified policy from the canonical
//! corpus run on 2026-09-04; cap 4 was the then-current comparator. See
//! docs/experiments/2026-09-04-onnx-beam-qualification.md section 7.

use rightkit_asr::onnx_tdt::OnnxAsr;
use rightkit_asr::onnx_tdt::PredictorOutput;
use rightkit_asr::parakeet_core::bias::ContextBias;
use rightkit_asr::parakeet_core::tdt_beam::{force_time_advance, rnnt_step, tdt_step, BeamHyp};
use rightkit_asr::parakeet_core::PruningMode;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PredictorExperiment {
    Fused,
    FusedBatched,
    FusedBorrowed,
    FusedBorrowedPreallocated,
    FusedBorrowedSharedState,
    SplitUncached,
    SplitUncachedBorrowed,
    SplitCached,
    SplitCachedBorrowed,
}

impl PredictorExperiment {
    fn from_env() -> Result<Self, String> {
        match rightkit_dictation_core::dev_var!("RIGHTKIT_ONNX_PREDICTOR_EXPERIMENT")
            .unwrap_or_else(|_| "fused".to_owned())
            .as_str()
        {
            "fused" => Ok(Self::Fused),
            "fused_batched" => Ok(Self::FusedBatched),
            "fused_borrowed" => Ok(Self::FusedBorrowed),
            "fused_borrowed_preallocated" => Ok(Self::FusedBorrowedPreallocated),
            "fused_borrowed_shared_state" => Ok(Self::FusedBorrowedSharedState),
            "split_uncached" => Ok(Self::SplitUncached),
            "split_uncached_borrowed" => Ok(Self::SplitUncachedBorrowed),
            "split_cached" => Ok(Self::SplitCached),
            "split_cached_borrowed" => Ok(Self::SplitCachedBorrowed),
            other => Err(format!("unknown ONNX predictor experiment mode: {other}")),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Fused => "fused",
            Self::FusedBatched => "fused_batched",
            Self::FusedBorrowed => "fused_borrowed",
            Self::FusedBorrowedPreallocated => "fused_borrowed_preallocated",
            Self::FusedBorrowedSharedState => "fused_borrowed_shared_state",
            Self::SplitUncached => "split_uncached",
            Self::SplitUncachedBorrowed => "split_uncached_borrowed",
            Self::SplitCached => "split_cached",
            Self::SplitCachedBorrowed => "split_cached_borrowed",
        }
    }

    fn caches_predictor(self) -> bool {
        matches!(self, Self::SplitCached | Self::SplitCachedBorrowed)
    }

    fn borrows_inputs(self) -> bool {
        matches!(
            self,
            Self::SplitCachedBorrowed | Self::SplitUncachedBorrowed
        )
    }

    fn shares_beam_state(self) -> bool {
        self == Self::FusedBorrowedSharedState
    }
}

/// TDT safety cap on symbols emitted at one frame. 10 to match the CoreML path
/// (`coreml_asr/decode_window.rs`) — this was 5, which force-advanced the
/// frame on Windows for anything needing 6-10 symbols (dense numerals,
/// hyphenated compounds) while macOS emitted them.
fn max_tokens_per_step() -> usize {
    rightkit_dictation_core::dev_var!("RIGHTKIT_RNNT_MAX_TOKENS")
        .ok()
        .and_then(|raw| raw.trim().parse().ok())
        .unwrap_or(10)
}

/// Frame-skip cap. The ONNX path now defaults to 4, matching macOS (it was 2
/// from the 2026-09-04 CPU qualification; see `ONNX_DURATION_CAP_INDEX`).
/// `RIGHTKIT_TDT_DURATION_CAP` still overrides; see the dated Windows bakeoff
/// records and ASR_DECODE_CONTRACT.md section 7.
fn duration_cap_index() -> usize {
    rightkit_dictation_core::dev_var!("RIGHTKIT_TDT_DURATION_CAP")
        .ok()
        .and_then(|raw| raw.trim().parse().ok())
        .unwrap_or(ONNX_DURATION_CAP_INDEX)
}

/// Duration-logit index cap for the ONNX path: 4, the macOS value. RightKit's
/// `WINDOWS_VENDOR_DURATION_CAP_INDEX` (2) is the 2026-09-04 qualification
/// value; on Ultra, FLEURS en_us 100 scored 5.35 % WER at cap 4 vs 5.53 % at
/// cap 2 with 24 % fewer decoder calls (paired AB/BA 6/6, 2026-10-23).
const ONNX_DURATION_CAP_INDEX: usize = 4;

/// Decode one already-windowed segment and return `(token_id, start_sec, dur_frames)`
/// for the best hypothesis, matching the macOS `decode_window` contract.
pub(crate) fn decode_segment(
    model: &mut OnnxAsr,
    audio: &[f32],
    win_start_sec: f32,
    bias: Option<&ContextBias>,
    beam_width: usize,
) -> Result<Vec<(usize, f32, usize)>, String> {
    let window_started = std::time::Instant::now();
    let buffer_reuse = decode_buffer_reuse();
    let mel_started = std::time::Instant::now();
    // ORT-16: the reuse path keeps mel in the graph's channel-major layout and
    // pads it into a reused encoder buffer: the same encoder input values
    // without two transposes. `RIGHTKIT_ONNX_DECODE_BUFFER_REUSE=0` restores the old path.
    let (encoded, mel_ms, encode_ms) = if buffer_reuse {
        let features = model.mel_channel_major(audio).map_err(|e| e.to_string())?;
        let mel_ms = mel_started.elapsed().as_secs_f64() * 1_000.0;
        let encode_started = std::time::Instant::now();
        let encoded = model
            .encode_channel_major(&features)
            .map_err(|e| e.to_string())?;
        (
            encoded,
            mel_ms,
            encode_started.elapsed().as_secs_f64() * 1_000.0,
        )
    } else {
        let features = model.mel(audio).map_err(|e| e.to_string())?;
        let mel_ms = mel_started.elapsed().as_secs_f64() * 1_000.0;
        let encode_started = std::time::Instant::now();
        let encoded = model.encode(&features).map_err(|e| e.to_string())?;
        (
            encoded,
            mel_ms,
            encode_started.elapsed().as_secs_f64() * 1_000.0,
        )
    };
    if encoded.time_steps == 0 || encoded.used == 0 {
        log_window_timing(
            win_start_sec,
            audio.len(),
            mel_ms,
            encode_ms,
            0.0,
            window_started,
            &encoded,
        );
        crate::timeline_recorder::note_stage_detail(mel_ms, encode_ms, 0.0);
        return Ok(Vec::new());
    }
    let used = encoded.used;
    let vocab_size = model.vocab_pieces().len();
    let blank = model.blank_id() as i32;
    let max_symbols = max_tokens_per_step();
    let duration_cap = duration_cap_index();
    let has_duration = model.config().model_type.contains("tdt");
    let width = beam_width.max(1);
    let selected_pruning = pruning_mode();
    // Batched fused decoder/joint is the shipping default on the legacy global
    // loop (batch-2 costs ~0.20 ms vs 2 x 0.17 ms); an explicit env value still
    // wins, and time-synchronous pruning keeps the per-hypothesis path.
    let predictor_experiment =
        if rightkit_dictation_core::dev_var_os!("RIGHTKIT_ONNX_PREDICTOR_EXPERIMENT").is_none()
            && selected_pruning == PruningMode::LegacyGlobal
        {
            PredictorExperiment::FusedBatched
        } else {
            PredictorExperiment::from_env()?
        };
    if predictor_experiment == PredictorExperiment::FusedBatched
        && selected_pruning != PruningMode::LegacyGlobal
    {
        return Err("fused_batched currently supports legacy global pruning only".into());
    }
    if !matches!(
        predictor_experiment,
        PredictorExperiment::Fused
            | PredictorExperiment::FusedBatched
            | PredictorExperiment::FusedBorrowed
            | PredictorExperiment::FusedBorrowedPreallocated
            | PredictorExperiment::FusedBorrowedSharedState
    ) && !model.has_split_predictor_experiment()
    {
        return Err("split predictor experiment directory was not loaded".into());
    }
    let predictor_bench =
        rightkit_dictation_core::dev_var_os!("RIGHTKIT_PREDICTOR_BENCH").is_some();
    let mut predictor_calls = 0usize;
    let mut joint_calls = 0usize;
    let mut predictor_elapsed = std::time::Duration::ZERO;
    let mut joint_elapsed = std::time::Duration::ZERO;

    // The duration table is the identity on this path: the ONNX side uses the
    // duration-logit INDEX as the frame advance, CLAMPED by `duration_cap`.
    // macOS looks the value up in the model's own table. The two agree only
    // while that table is identity-valued; see ASR_DECODE_CONTRACT.md section 7.
    //
    // The table must span every duration logit, not just the capped range: the
    // decoder argmaxes over all `nd` logits and indexes straight into this, so
    // a table truncated to `duration_cap + 1` panicked the moment a frame's
    // duration head picked index 3 or 4 — which is the common case over
    // silence. Clamp the VALUE, keep the table full length. `nd` is only known
    // once the joint has run, so the table is built there.

    type StatePair = (ndarray::Array3<f32>, ndarray::Array3<f32>);
    #[derive(Clone)]
    enum State {
        Owned(StatePair),
        Shared(Arc<StatePair>),
    }
    impl State {
        fn pair(&self) -> &StatePair {
            match self {
                Self::Owned(pair) => pair,
                Self::Shared(pair) => pair,
            }
        }

        fn child_copy(&self, share: bool) -> Self {
            if share {
                match self {
                    Self::Shared(pair) => Self::Shared(Arc::clone(pair)),
                    Self::Owned(pair) => Self::Shared(Arc::new((pair.0.clone(), pair.1.clone()))),
                }
            } else {
                let pair = self.pair();
                Self::Owned((pair.0.clone(), pair.1.clone()))
            }
        }
    }
    type CachedPredictor = Option<Arc<PredictorOutput>>;
    let initial_state: StatePair = model.initial_state();
    let root = BeamHyp {
        h: Vec::new(),
        c: Vec::new(),
        last_tok: blank,
        t: 0,
        sym: 0,
        score: 0.0,
        hits: Vec::new(),
    };

    // One decoder/joint call for one hypothesis; returns its children paired
    // with the predictor state each child carries forward. Shared by both
    // pruning modes so the only difference between them is WHICH hypotheses
    // compete for a beam slot.
    let root_state = if predictor_experiment.shares_beam_state() {
        State::Shared(Arc::new(initial_state))
    } else {
        State::Owned(initial_state)
    };

    let decoder_search_started = std::time::Instant::now();
    // ORT-16: the duration table depends only on the logit count, which is
    // fixed per model; build it once per window instead of once per expansion.
    let duration_table: std::cell::RefCell<Vec<i64>> = std::cell::RefCell::new(Vec::new());
    let skip_unused_frame =
        buffer_reuse && predictor_experiment == PredictorExperiment::FusedBatched;

    let expand = |model: &mut OnnxAsr,
                  predictor_calls: &mut usize,
                  joint_calls: &mut usize,
                  predictor_elapsed: &mut std::time::Duration,
                  joint_elapsed: &mut std::time::Duration,
                  hyp: &BeamHyp,
                  state: &State,
                  cache: &CachedPredictor,
                  batched_step: Option<(Vec<f32>, ndarray::Array3<f32>, ndarray::Array3<f32>)>|
     -> Result<Vec<(BeamHyp, State, CachedPredictor)>, String> {
        // The batched path already ran this row; its frame is unused here.
        let frame = if skip_unused_frame {
            Vec::new()
        } else {
            encoded.frame(hyp.t)
        };
        let (state_h, state_c) = state.pair();
        let mut prediction = None;
        let (logits, new_h, new_c) = match predictor_experiment {
            PredictorExperiment::Fused => {
                let joint_started = predictor_bench.then(std::time::Instant::now);
                let step = model
                    .joint_step(&frame, hyp.last_tok, state_h, state_c)
                    .map_err(|e| e.to_string())?;
                if let Some(started) = joint_started {
                    *predictor_calls += 1;
                    *joint_calls += 1;
                    *joint_elapsed += started.elapsed();
                }
                (step.logits, step.state_h, step.state_c)
            }
            PredictorExperiment::FusedBatched => batched_step
                .ok_or_else(|| "fused_batched requires batched round output".to_owned())?,
            PredictorExperiment::FusedBorrowed => {
                let joint_started = predictor_bench.then(std::time::Instant::now);
                let step = model
                    .joint_step_borrowed(&frame, hyp.last_tok, state_h, state_c)
                    .map_err(|e| e.to_string())?;
                if let Some(started) = joint_started {
                    *predictor_calls += 1;
                    *joint_calls += 1;
                    *joint_elapsed += started.elapsed();
                }
                (step.logits, step.state_h, step.state_c)
            }
            PredictorExperiment::FusedBorrowedPreallocated => {
                let joint_started = predictor_bench.then(std::time::Instant::now);
                let step = model
                    .joint_step_borrowed_preallocated(&frame, hyp.last_tok, state_h, state_c)
                    .map_err(|e| e.to_string())?;
                if let Some(started) = joint_started {
                    *predictor_calls += 1;
                    *joint_calls += 1;
                    *joint_elapsed += started.elapsed();
                }
                (step.logits, step.state_h, step.state_c)
            }
            PredictorExperiment::FusedBorrowedSharedState => {
                let joint_started = predictor_bench.then(std::time::Instant::now);
                let step = model
                    .joint_step_borrowed(&frame, hyp.last_tok, state_h, state_c)
                    .map_err(|e| e.to_string())?;
                if let Some(started) = joint_started {
                    *predictor_calls += 1;
                    *joint_calls += 1;
                    *joint_elapsed += started.elapsed();
                }
                (step.logits, step.state_h, step.state_c)
            }
            PredictorExperiment::SplitUncached
            | PredictorExperiment::SplitUncachedBorrowed
            | PredictorExperiment::SplitCached
            | PredictorExperiment::SplitCachedBorrowed => {
                let pred_started = predictor_bench.then(std::time::Instant::now);
                let pred = if predictor_experiment.caches_predictor() {
                    cache.as_ref().map(Arc::clone).map(Ok).unwrap_or_else(|| {
                        let result = if predictor_experiment.borrows_inputs() {
                            model.predictor_step_borrowed(hyp.last_tok, state_h, state_c)
                        } else {
                            model.predictor_step(hyp.last_tok, state_h, state_c)
                        };
                        result.map(Arc::new).map_err(|e| e.to_string())
                    })?
                } else {
                    let result = if predictor_experiment.borrows_inputs() {
                        model.predictor_step_borrowed(hyp.last_tok, state_h, state_c)
                    } else {
                        model.predictor_step(hyp.last_tok, state_h, state_c)
                    };
                    Arc::new(result.map_err(|e| e.to_string())?)
                };
                if let Some(started) = pred_started {
                    if !predictor_experiment.caches_predictor() || cache.is_none() {
                        *predictor_calls += 1;
                        *predictor_elapsed += started.elapsed();
                    }
                }
                let joint_started = predictor_bench.then(std::time::Instant::now);
                let joint_result = if predictor_experiment.borrows_inputs() {
                    model.joint_from_predictor_borrowed(&frame, &pred.projection)
                } else {
                    model.joint_from_predictor(&frame, &pred.projection)
                };
                let logits = joint_result.map_err(|e| e.to_string())?;
                if let Some(started) = joint_started {
                    *joint_calls += 1;
                    *joint_elapsed += started.elapsed();
                }
                prediction = Some(pred.clone());
                (logits, pred.state_h.clone(), pred.state_c.clone())
            }
        };
        // Bias is per-hypothesis, computed from ITS OWN emitted history —
        // that is what makes a multi-token phrase survivable across steps.
        let bonuses = bias
            .map(|b| {
                let w = b.history_window();
                let start = hyp.hits.len().saturating_sub(w);
                let emitted: Vec<usize> = hyp.hits[start..].iter().map(|(id, _, _)| *id).collect();
                b.next_token_bonuses(&emitted)
            })
            .unwrap_or_default();
        let children = if has_duration {
            let nd = logits.len().saturating_sub(vocab_size);
            let fresh;
            let mut cached;
            let durations: &[i64] = if buffer_reuse {
                cached = duration_table.borrow_mut();
                if cached.len() != nd {
                    *cached = duration_values(nd, duration_cap);
                }
                &cached
            } else {
                fresh = duration_values(nd, duration_cap);
                &fresh
            };
            tdt_step(
                hyp,
                &logits,
                nd,
                durations,
                blank,
                max_symbols,
                width,
                crate::beam_emit_reward(),
                win_start_sec,
                &bonuses,
                &[],
                &[],
            )
        } else {
            rnnt_step(
                hyp,
                &logits,
                blank,
                width,
                crate::beam_emit_reward(),
                win_start_sec,
                &bonuses,
                &[],
                &[],
            )
        };
        let has_advanced_child = children
            .iter()
            .any(|child| child.hits.len() > hyp.hits.len());
        let advanced_state = if has_advanced_child {
            let pair = (new_h, new_c);
            if predictor_experiment.shares_beam_state() {
                Some(State::Shared(Arc::new(pair)))
            } else {
                Some(State::Owned(pair))
            }
        } else {
            None
        };
        Ok(children
            .into_iter()
            .map(|child| {
                // Predictor state advances only on emission, exactly as the
                // greedy loop did: a blank leaves last_tok/h/c untouched.
                let advanced = child.hits.len() > hyp.hits.len();
                let (carried, carried_cache) = if advanced {
                    let carried = advanced_state
                        .as_ref()
                        .expect("advanced child requires predictor state")
                        .child_copy(predictor_experiment.shares_beam_state());
                    (carried, None)
                } else {
                    let next_cache = if predictor_experiment.caches_predictor() {
                        prediction.as_ref().map(Arc::clone)
                    } else {
                        None
                    };
                    (
                        state.child_copy(predictor_experiment.shares_beam_state()),
                        next_cache,
                    )
                };
                (child, carried, carried_cache)
            })
            .collect())
    };

    fn prune(pool: &mut Vec<(BeamHyp, State, CachedPredictor)>, width: usize) {
        pool.sort_by(|a, b| {
            b.0.score
                .partial_cmp(&a.0.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        pool.truncate(width);
    }

    // Same defensive round cap as macOS: every round advances some hypothesis's
    // `t` or `sym`, both bounded, so this can only trigger on a logic bug — and
    // then truncates rather than hanging.
    let max_rounds = used.saturating_add(1) * max_symbols.max(1) + 4;

    if selected_pruning == PruningMode::LegacyGlobal {
        // Shipping loop: every hypothesis, whatever frame it sits at and even
        // once it has finished, competes in ONE global sort. See `pruning_mode`
        // for the time-synchronous alternative and why it is opt-in.
        let mut beam: Vec<(BeamHyp, State, CachedPredictor)> = vec![(root, root_state, None)];
        let mut rounds = 0usize;
        while beam.iter().any(|(hyp, _, _)| hyp.t < used) {
            rounds += 1;
            if rounds > max_rounds {
                break;
            }
            let current = std::mem::take(&mut beam);
            let is_batched = predictor_experiment == PredictorExperiment::FusedBatched;
            let mut batched = if is_batched {
                vec![None; current.len()]
            } else {
                Vec::new()
            };
            if is_batched {
                let eligible: Vec<usize> = current
                    .iter()
                    .enumerate()
                    .filter_map(|(i, (hyp, _, _))| {
                        (hyp.t < used && hyp.sym < max_symbols).then_some(i)
                    })
                    .collect();
                if !eligible.is_empty() {
                    // Hypotheses with the same frame and emitted tokens share
                    // last token and predictor state, so one row serves both.
                    let keys: Vec<(usize, Vec<usize>)> = eligible
                        .iter()
                        .map(|&i| {
                            (
                                current[i].0.t,
                                current[i].0.hits.iter().map(|(id, _, _)| *id).collect(),
                            )
                        })
                        .collect();
                    let started = predictor_bench.then(std::time::Instant::now);
                    let steps = batch_unique_rows(&keys, |unique| {
                        let tokens: Vec<_> = unique
                            .iter()
                            .map(|&u| current[eligible[u]].0.last_tok)
                            .collect();
                        let states: Vec<(&ndarray::Array3<f32>, &ndarray::Array3<f32>)> = unique
                            .iter()
                            .map(|&u| {
                                let (h, c) = current[eligible[u]].1.pair();
                                (h, c)
                            })
                            .collect();
                        if buffer_reuse {
                            let frame_indices: Vec<usize> =
                                unique.iter().map(|&u| current[eligible[u]].0.t).collect();
                            return model
                                .joint_steps_batched_at(&encoded, &frame_indices, &tokens, &states)
                                .map_err(|e| e.to_string());
                        }
                        let frames: Vec<_> = unique
                            .iter()
                            .map(|&u| encoded.frame(current[eligible[u]].0.t))
                            .collect();
                        model
                            .joint_steps_batched(&frames, &tokens, &states)
                            .map_err(|e| e.to_string())
                    })?;
                    if let Some(started) = started {
                        predictor_calls += 1;
                        joint_calls += 1;
                        joint_elapsed += started.elapsed();
                    }
                    for (i, step) in eligible.into_iter().zip(steps) {
                        batched[i] = Some(step);
                    }
                }
            }
            let mut next: Vec<(BeamHyp, State, CachedPredictor)> =
                Vec::with_capacity(current.len() * width);
            for (index, (hyp, state, cache)) in current.into_iter().enumerate() {
                if hyp.t >= used {
                    next.push((hyp, state, cache));
                    continue;
                }
                if hyp.sym >= max_symbols {
                    next.push((force_time_advance(&hyp), state, cache));
                    continue;
                }
                let step = if is_batched {
                    batched[index].take()
                } else {
                    None
                };
                next.extend(expand(
                    model,
                    &mut predictor_calls,
                    &mut joint_calls,
                    &mut predictor_elapsed,
                    &mut joint_elapsed,
                    &hyp,
                    &state,
                    &cache,
                    step,
                )?);
            }
            prune(&mut next, width);
            beam = next;
        }
        prune(&mut beam, width);
        if predictor_bench {
            let (run_calls, run_elapsed) = rightkit_asr::onnx_tdt::take_batched_run_stats();
            eprintln!("onnx_predictor_experiment mode={} predictor_calls={predictor_calls} joint_calls={joint_calls} predictor_ms={:.3} joint_ms={:.3} run_calls={run_calls} run_ms={:.3} frames={used} width={width}", predictor_experiment.label(), predictor_elapsed.as_secs_f64() * 1_000.0, joint_elapsed.as_secs_f64() * 1_000.0, run_elapsed.as_secs_f64() * 1_000.0);
        }
        let hits = beam
            .into_iter()
            .next()
            .map(|(h, _, _)| h.hits)
            .unwrap_or_default();
        log_window_timing(
            win_start_sec,
            audio.len(),
            mel_ms,
            encode_ms,
            decoder_search_started.elapsed().as_secs_f64() * 1_000.0,
            window_started,
            &encoded,
        );
        crate::timeline_recorder::note_stage_detail(
            mel_ms,
            encode_ms,
            decoder_search_started.elapsed().as_secs_f64() * 1_000.0,
        );
        return Ok(hits);
    }

    // Time-synchronous search. Hypotheses are bucketed by the encoder frame
    // they will read next and only ever compete for a beam slot against
    // hypotheses at the SAME frame. A hypothesis that has consumed the whole
    // window moves to `finished` and never competes with one still
    // transcribing. TDT is not frame-by-frame: a duration prediction jumps a
    // hypothesis to `t + dur`, which simply files it in that later bucket, and
    // a `dur == 0` emission chains at the same frame, which keeps it in the
    // current bucket for another round (bounded by `max_symbols`, after which
    // `force_time_advance` files it one frame on).
    let mut buckets: std::collections::BTreeMap<usize, Vec<(BeamHyp, State, CachedPredictor)>> =
        std::collections::BTreeMap::new();
    buckets.insert(0, vec![(root, root_state, None)]);
    let mut finished: Vec<(BeamHyp, State, CachedPredictor)> = Vec::new();
    let mut rounds = 0usize;
    while let Some((&t, _)) = buckets.iter().next() {
        let mut active = buckets.remove(&t).unwrap_or_default();
        if t >= used {
            finished.append(&mut active);
            continue;
        }
        // Same-frame rounds: a bucket is re-entered while emissions with
        // `dur == 0` keep hypotheses at this frame.
        loop {
            rounds += 1;
            if rounds > max_rounds || active.is_empty() {
                break;
            }
            prune(&mut active, width);
            let mut same_frame: Vec<(BeamHyp, State, CachedPredictor)> = Vec::new();
            for (hyp, state, cache) in active.drain(..) {
                if hyp.sym >= max_symbols {
                    let moved = force_time_advance(&hyp);
                    buckets
                        .entry(moved.t)
                        .or_default()
                        .push((moved, state, cache));
                    continue;
                }
                for (child, carried, carried_cache) in expand(
                    model,
                    &mut predictor_calls,
                    &mut joint_calls,
                    &mut predictor_elapsed,
                    &mut joint_elapsed,
                    &hyp,
                    &state,
                    &cache,
                    None,
                )? {
                    if child.t == t {
                        same_frame.push((child, carried, carried_cache));
                    } else if child.t >= used {
                        finished.push((child, carried, carried_cache));
                    } else {
                        buckets
                            .entry(child.t)
                            .or_default()
                            .push((child, carried, carried_cache));
                    }
                }
            }
            active = same_frame;
        }
        if rounds > max_rounds {
            // Logic-bug guard: treat whatever is left as finished rather than hang.
            for (_, mut pool) in std::mem::take(&mut buckets) {
                finished.append(&mut pool);
            }
            finished.append(&mut active);
        }
    }
    if rightkit_dictation_core::dev_var!("RIGHTKIT_TDT_TRACE").is_ok() {
        let pieces = model.vocab_pieces();
        for (hyp, _, _) in &finished {
            let text: String = hyp
                .hits
                .iter()
                .map(|(id, _, _)| pieces.get(*id).cloned().unwrap_or_default())
                .collect::<Vec<_>>()
                .join("|");
            eprintln!(
                "  finished t={} sym={} tokens={} score={:.3} {}",
                hyp.t,
                hyp.sym,
                hyp.hits.len(),
                hyp.score,
                text
            );
        }
    }
    prune(&mut finished, 1);
    if predictor_bench {
        eprintln!("onnx_predictor_experiment mode={} predictor_calls={predictor_calls} joint_calls={joint_calls} predictor_ms={:.3} joint_ms={:.3} frames={used} width={width}", predictor_experiment.label(), predictor_elapsed.as_secs_f64() * 1_000.0, joint_elapsed.as_secs_f64() * 1_000.0);
    }
    let hits = finished
        .into_iter()
        .next()
        .map(|(h, _, _)| h.hits)
        .unwrap_or_default();
    log_window_timing(
        win_start_sec,
        audio.len(),
        mel_ms,
        encode_ms,
        decoder_search_started.elapsed().as_secs_f64() * 1_000.0,
        window_started,
        &encoded,
    );
    crate::timeline_recorder::note_stage_detail(
        mel_ms,
        encode_ms,
        decoder_search_started.elapsed().as_secs_f64() * 1_000.0,
    );
    Ok(hits)
}

/// Duration-logit index -> frame advance, clamped at `cap` (see the comment
/// at the top of `decode_segment`: the table spans every duration logit).
fn duration_values(nd: usize, cap: usize) -> Vec<i64> {
    (0..nd).map(|i| i.min(cap) as i64).collect()
}

/// ORT-16 output-identical buffer reuse in the ONNX decode loop: on by
/// default; `RIGHTKIT_ONNX_DECODE_BUFFER_REUSE=0` restores the old allocation
/// pattern for timing A/B.
fn decode_buffer_reuse() -> bool {
    rightkit_dictation_core::dev_var!("RIGHTKIT_ONNX_DECODE_BUFFER_REUSE").as_deref() != Ok("0")
}

/// The per-window `asr_window_stage_timing` line. Encoder fields (ORT-01 /
/// ENG-01): real mel frames, the `length` passed, decoded frames, and the age
/// of the previous encoder pass (the GPU pass on DirectML) at this pass's start.
fn log_window_timing(
    win_start_sec: f32,
    sample_count: usize,
    mel_ms: f64,
    encode_ms: f64,
    decoder_search_wall_ms: f64,
    window_started: std::time::Instant,
    encoded: &rightkit_asr::onnx_tdt::EncoderOutput,
) {
    tracing::info!(
        target: "rightkit_asr_stage_timing",
        backend = "onnx",
        window_start_sec = win_start_sec,
        sample_count,
        mel_ms,
        encode_ms,
        decoder_search_wall_ms,
        window_total_ms = window_started.elapsed().as_secs_f64() * 1_000.0,
        encoder_input_frames = encoded.input_frames,
        encoder_length_input = encoded.length_input,
        encoder_used_frames = encoded.used,
        prior_encoder_pass_age_ms = encoded
            .prior_pass_age
            .map(|age| age.as_secs_f64() * 1_000.0),
        "asr_window_stage_timing"
    );
}

/// Run `run` once over the distinct keys (given as indices of first
/// occurrences) and return one row per input key, cloning rows for duplicates.
fn batch_unique_rows<K: PartialEq, R: Clone>(
    keys: &[K],
    run: impl FnOnce(&[usize]) -> Result<Vec<R>, String>,
) -> Result<Vec<R>, String> {
    let mut unique: Vec<usize> = Vec::new();
    let mut slot: Vec<usize> = Vec::with_capacity(keys.len());
    for (i, key) in keys.iter().enumerate() {
        match unique.iter().position(|&u| keys[u] == *key) {
            Some(p) => slot.push(p),
            None => {
                slot.push(unique.len());
                unique.push(i);
            }
        }
    }
    let rows = run(&unique)?;
    if rows.len() != unique.len() {
        return Err(format!(
            "batched joint returned {} rows for {} hypotheses",
            rows.len(),
            unique.len()
        ));
    }
    Ok(slot.into_iter().map(|p| rows[p].clone()).collect())
}

/// Which hypotheses compete for a beam slot. Shipping default is the legacy
/// global sort; `RIGHTKIT_TDT_PRUNING=timesync` selects the bucketed
/// time-synchronous search above for corpus measurement.
///
/// Time-synchronous pruning is the fix `tdt_beam::emit_reward` names for the
/// cross-frame scoring bias, and it is implemented here so it can be measured.
/// It does NOT fix mid-word truncation producing a filler-word transcript:
/// that is the model's own likelihood winner once audio ends on a truncated
/// word, under either pruning rule. That case is fixed by the terminal
/// silence appended in `asr::runtime::with_terminal_silence`, which gives the
/// transcribing hypothesis frames to finish the word. Do not make
/// time-synchronous the default without a canonical-corpus run at beam width
/// 2 with vocabulary phrases — vocabulary gain is what this search exists for.
fn pruning_mode() -> PruningMode {
    match rightkit_dictation_core::dev_var!("RIGHTKIT_TDT_PRUNING")
        .ok()
        .as_deref()
    {
        Some("timesync") => PruningMode::TimeSynchronous,
        _ => PruningMode::LegacyGlobal,
    }
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    use rightkit_asr::parakeet_core::tdt_beam::{tdt_step, BeamHyp};

    fn hyp(t: usize, last: i32) -> BeamHyp {
        BeamHyp {
            h: vec![],
            c: vec![],
            last_tok: last,
            t,
            sym: 0,
            score: 0.0,
            hits: vec![],
        }
    }

    /// Deterministic synthetic joint: logits depend on (frame, token) only.
    fn synth(t: usize, tok: i32) -> Vec<f32> {
        (0..8)
            .map(|k| (((t * 7 + tok as usize * 13 + k * 5) % 11) as f32) * 0.3 - 1.0)
            .collect()
    }

    fn children(h: &BeamHyp, logits: &[f32]) -> Vec<(i32, usize, i64)> {
        tdt_step(
            h,
            logits,
            3,
            &[0, 1, 2],
            4,
            10,
            2,
            crate::beam_emit_reward(),
            0.0,
            &Default::default(),
            &[],
            &[],
        )
        .iter()
        .map(|c| (c.last_tok, c.t, (c.score * 1e4) as i64))
        .collect()
    }

    #[test]
    fn duration_table_clamps_values_and_spans_every_logit() {
        assert_eq!(duration_values(5, 2), [0, 1, 2, 2, 2]);
        assert_eq!(duration_values(5, 4), [0, 1, 2, 3, 4]);
        assert!(duration_values(0, 4).is_empty());
    }

    #[test]
    fn batched_rows_match_unbatched_hypotheses() {
        let hyps = [hyp(0, 4), hyp(3, 1), hyp(0, 4), hyp(5, 2)];
        let keys: Vec<(usize, i32)> = hyps.iter().map(|h| (h.t, h.last_tok)).collect();
        let mut calls = 0;
        let rows = batch_unique_rows(&keys, |u| {
            calls += 1;
            assert_eq!(u.len(), 3, "duplicate hypothesis must share a row");
            Ok(u.iter()
                .map(|&i| synth(hyps[i].t, hyps[i].last_tok))
                .collect())
        })
        .unwrap();
        assert_eq!(calls, 1);
        for (h, row) in hyps.iter().zip(&rows) {
            assert_eq!(children(h, row), children(h, &synth(h.t, h.last_tok)));
        }
    }
}
