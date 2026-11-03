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
use rightkit_asr::parakeet_core::bias::ContextBias;
use rightkit_asr::parakeet_core::tdt_beam::{force_time_advance, rnnt_step, tdt_step, BeamHyp};

/// TDT safety cap on symbols emitted at one frame. 10 to match the CoreML path
/// (`coreml_asr/decode_window.rs`) — this was 5, which force-advanced the
/// frame on Windows for anything needing 6-10 symbols (dense numerals,
/// hyphenated compounds) while macOS emitted them.
const MAX_TOKENS_PER_STEP: usize = 10;

/// Duration-logit index cap for the ONNX path: 4, the macOS value. RightKit's
/// `WINDOWS_VENDOR_DURATION_CAP_INDEX` (2) is the 2026-09-04 qualification
/// value; on Ultra, FLEURS en_us 100 scored 5.35 % WER at cap 4 vs 5.53 % at
/// cap 2 with 24 % fewer decoder calls (paired AB/BA 6/6, 2026-10-23). See the
/// dated Windows bakeoff records and ASR_DECODE_CONTRACT.md section 7.
const ONNX_DURATION_CAP_INDEX: usize = 4;

type State = (ndarray::Array3<f32>, ndarray::Array3<f32>);

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
    let mel_started = std::time::Instant::now();
    // ORT-16: mel stays in the graph's channel-major layout and is padded into a reused
    // encoder buffer: the same encoder input values without two transposes.
    let features = model.mel_channel_major(audio).map_err(|e| e.to_string())?;
    let mel_ms = mel_started.elapsed().as_secs_f64() * 1_000.0;
    let encode_started = std::time::Instant::now();
    let encoded = model
        .encode_channel_major(&features)
        .map_err(|e| e.to_string())?;
    let encode_ms = encode_started.elapsed().as_secs_f64() * 1_000.0;
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
    let max_symbols = MAX_TOKENS_PER_STEP;
    let duration_cap = ONNX_DURATION_CAP_INDEX;
    let has_duration = model.config().model_type.contains("tdt");
    let width = beam_width.max(1);

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

    let root = BeamHyp {
        h: Vec::new(),
        c: Vec::new(),
        last_tok: blank,
        t: 0,
        sym: 0,
        score: 0.0,
        hits: Vec::new(),
    };
    let root_state: State = model.initial_state();

    let decoder_search_started = std::time::Instant::now();
    // ORT-16: the duration table depends only on the logit count, which is
    // fixed per model; build it once per window instead of once per expansion.
    let duration_table: std::cell::RefCell<Vec<i64>> = std::cell::RefCell::new(Vec::new());

    // Expand one hypothesis from its batched decoder/joint row; returns its children paired
    // with the predictor state each child carries forward.
    let expand = |hyp: &BeamHyp,
                  state: &State,
                  batched_step: Option<(Vec<f32>, ndarray::Array3<f32>, ndarray::Array3<f32>)>|
     -> Result<Vec<(BeamHyp, State)>, String> {
        let (logits, new_h, new_c) =
            batched_step.ok_or_else(|| "fused_batched requires batched round output".to_owned())?;
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
            let mut cached = duration_table.borrow_mut();
            if cached.len() != nd {
                *cached = duration_values(nd, duration_cap);
            }
            let durations: &[i64] = &cached;
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
        let advanced_state = has_advanced_child.then_some((new_h, new_c));
        Ok(children
            .into_iter()
            .map(|child| {
                // Predictor state advances only on emission, exactly as the
                // greedy loop did: a blank leaves last_tok/h/c untouched.
                let advanced = child.hits.len() > hyp.hits.len();
                let carried = if advanced {
                    advanced_state
                        .as_ref()
                        .expect("advanced child requires predictor state")
                        .clone()
                } else {
                    state.clone()
                };
                (child, carried)
            })
            .collect())
    };

    fn prune(pool: &mut Vec<(BeamHyp, State)>, width: usize) {
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

    // Every hypothesis, whatever frame it sits at and even once it has finished, competes in
    // ONE global sort.
    let mut beam: Vec<(BeamHyp, State)> = vec![(root, root_state)];
    let mut rounds = 0usize;
    while beam.iter().any(|(hyp, _)| hyp.t < used) {
        rounds += 1;
        if rounds > max_rounds {
            break;
        }
        let current = std::mem::take(&mut beam);
        let mut batched = vec![None; current.len()];
        let eligible: Vec<usize> = current
            .iter()
            .enumerate()
            .filter_map(|(i, (hyp, _))| (hyp.t < used && hyp.sym < max_symbols).then_some(i))
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
            let steps = batch_unique_rows(&keys, |unique| {
                let tokens: Vec<_> = unique
                    .iter()
                    .map(|&u| current[eligible[u]].0.last_tok)
                    .collect();
                let states: Vec<(&ndarray::Array3<f32>, &ndarray::Array3<f32>)> = unique
                    .iter()
                    .map(|&u| {
                        let (h, c) = &current[eligible[u]].1;
                        (h, c)
                    })
                    .collect();
                let frame_indices: Vec<usize> =
                    unique.iter().map(|&u| current[eligible[u]].0.t).collect();
                model
                    .joint_steps_batched_at(&encoded, &frame_indices, &tokens, &states)
                    .map_err(|e| e.to_string())
            })?;
            for (i, step) in eligible.into_iter().zip(steps) {
                batched[i] = Some(step);
            }
        }
        let mut next: Vec<(BeamHyp, State)> = Vec::with_capacity(current.len() * width);
        for (index, (hyp, state)) in current.into_iter().enumerate() {
            if hyp.t >= used {
                next.push((hyp, state));
                continue;
            }
            if hyp.sym >= max_symbols {
                next.push((force_time_advance(&hyp), state));
                continue;
            }
            let step = batched[index].take();
            next.extend(expand(&hyp, &state, step)?);
        }
        prune(&mut next, width);
        beam = next;
    }
    prune(&mut beam, width);
    let hits = beam
        .into_iter()
        .next()
        .map(|(h, _)| h.hits)
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
