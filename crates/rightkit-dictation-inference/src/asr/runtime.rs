// ASR runtime for the sidecar.
//
// The sidecar loads the ASR model once and runs authoritative decode on every
// dictation stop. Long static Parakeet clips use padded-window decode so 15 s
// model limits do not become hard text boundaries.

use std::path::Path;

#[cfg(not(target_os = "macos"))]
use rightkit_asr::onnx_tdt::{ExecutionProvider, OnnxAsr, SessionOptions};
use rightkit_asr::parakeet_core::{TimedToken, TranscriptionResult};
use rightkit_dictation_core::engine::FileTranscript;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use rightkit_dictation_core::engine::TimedWord;

use super::models::configured_backend;
#[cfg(target_os = "windows")]
use super::models::{
    initial_context_bias, model_subdir_for, probe_directml_embedded, try_load_whisper_win,
};
#[cfg(target_os = "macos")]
use super::models::{try_load_coreml_parakeet, try_load_coreml_whisper};
use super::windowing::{
    append_with_overlap, ms_to_samples, padded_window_samples, quiet_cut_in, suffix_prefix_overlap,
    PADDED_WINDOW_OVERLAP_CHARS, PADDED_WINDOW_PADDING_MS, SAMPLE_RATE,
};

/// N0 for the text-only backends. Whisper returns a bare string with no token
/// stream, so the piece-level strip cannot reach it — but it must not hand
/// `uh`/`um` to L1 either. Same word list as the Parakeet path.
fn n0(text: String) -> String {
    rightkit_asr::parakeet_core::detok::strip_mandatory_fillers_text(&text)
}

#[cfg(not(target_os = "macos"))]
/// Decode one window on the ONNX path with the SHARED first-party beam, then
/// reconstruct the transcript with the shared detokenizer.
///
/// The vendored crate now does ORT execution only — mel, encoder, one
/// decoder/joint step per hypothesis. Search, scoring, the emission reward,
/// context bias and text assembly are all first-party and identical to macOS.
/// Before this, Windows decoded greedily, so a multi-token vocabulary phrase
/// could not survive a locally-worse step and custom vocabulary did materially
/// less here than on macOS or iOS.
fn onnx_beam_result(
    model: &mut OnnxAsr,
    bias: Option<&rightkit_asr::parakeet_core::bias::ContextBias>,
    window: &[f32],
) -> Result<TranscriptionResult, String> {
    onnx_beam_result_with_policy(
        model,
        bias,
        window,
        rightkit_asr::parakeet_core::DetokPolicy::canonical_rust(),
    )
}

#[cfg(not(target_os = "macos"))]
fn onnx_beam_result_with_policy(
    model: &mut OnnxAsr,
    bias: Option<&rightkit_asr::parakeet_core::bias::ContextBias>,
    window: &[f32],
    policy: rightkit_asr::parakeet_core::DetokPolicy,
) -> Result<TranscriptionResult, String> {
    let beam_width = rightkit_dictation_core::dev_var!("RIGHTKIT_ASR_BEAM_WIDTH")
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .map(|parsed| parsed.clamp(1, 8))
        .unwrap_or(2);
    let pass_started = std::time::Instant::now();
    let hits = crate::onnx_beam_decode::decode_segment(model, window, 0.0, bias, beam_width)?;
    // Timeline telemetry: samples + total here; the mel/encode/search split is
    // attached when `decode_segment` calls `timeline_recorder::note_stage_detail`.
    crate::timeline_recorder::record_pass(
        window.len(),
        pass_started.elapsed().as_secs_f64() * 1_000.0,
    );
    let pieces = model.vocab_pieces();
    let decoded: Vec<rightkit_asr::parakeet_core::DecodedPiece> = hits
        .into_iter()
        .map(
            |(token_id, start, dur_frames)| rightkit_asr::parakeet_core::DecodedPiece {
                token_id,
                piece: pieces.get(token_id).cloned().unwrap_or_default(),
                start,
                end: start + dur_frames as f32 * 0.08,
            },
        )
        .collect();
    Ok(rightkit_asr::parakeet_core::assemble_timed_pieces(
        &decoded, policy,
    ))
}

#[cfg(target_os = "windows")]
fn onnx_alignment_bounds(earliest_sec: f32, frames: usize) -> (usize, usize) {
    const FRAME_SEC: f32 = 0.08;
    const SEARCH_WINDOW_SEC: f32 = 1.4;
    let first =
        ((earliest_sec.max(0.0) / FRAME_SEC).floor() as usize).min(frames.saturating_sub(1));
    let last = (((earliest_sec.max(0.0) + SEARCH_WINDOW_SEC) / FRAME_SEC).floor() as usize)
        .min(frames)
        .max(first + 1);
    (first, last)
}

#[cfg(target_os = "windows")]
fn onnx_exact_suffix_score(
    prefix_score: f32,
    lp_blank: &[Vec<f32>],
    lp_token: &[Vec<f32>],
    token_count: usize,
) -> Option<(f32, f32)> {
    let span = lp_blank.first()?.len();
    if span == 0 || lp_blank.len() != token_count + 1 || lp_token.len() != token_count {
        return None;
    }
    let neg = f32::NEG_INFINITY;
    let mut alpha = vec![vec![neg; span]; token_count + 1];
    let mut token_mass = vec![vec![0.0f32; span]; token_count + 1];
    alpha[0][0] = prefix_score;
    for i in 0..span {
        for k in 0..=token_count {
            let mut value = alpha[k][i];
            let mut mass = token_mass[k][i];
            if i > 0 && alpha[k][i - 1] > neg && alpha[k][i - 1] + lp_blank[k][i - 1] > value {
                value = alpha[k][i - 1] + lp_blank[k][i - 1];
                mass = token_mass[k][i - 1];
            }
            if k > 0 && alpha[k - 1][i] > neg && alpha[k - 1][i] + lp_token[k - 1][i] > value {
                value = alpha[k - 1][i] + lp_token[k - 1][i];
                mass = token_mass[k - 1][i] + lp_token[k - 1][i];
            }
            alpha[k][i] = value;
            token_mass[k][i] = mass;
        }
    }
    let total = alpha[token_count][span - 1] + lp_blank[token_count][span - 1];
    (total > neg).then(|| {
        (
            total,
            token_mass[token_count][span - 1] / token_count as f32,
        )
    })
}

#[cfg(target_os = "windows")]
fn onnx_best_exact_candidate(candidates: &[(usize, f32, f32)]) -> Option<(usize, f32)> {
    candidates
        .iter()
        .reduce(|best, candidate| {
            if candidate.1 > best.1 {
                candidate
            } else {
                best
            }
        })
        .map(|(boundary, _total, mean)| (*boundary, *mean))
}

#[cfg(target_os = "windows")]
fn onnx_alignment_accepted(mean: f32) -> bool {
    mean.is_finite() && mean >= -6.8
}

#[cfg(target_os = "windows")]
fn align_onnx_control_suffix(
    model: &mut OnnxAsr,
    command: &str,
    conditioned_audio: &[f32],
    earliest_sec: f32,
    locator_enabled: bool,
) -> Result<crate::control_cleanup::Alignment, String> {
    use std::collections::HashMap;

    const FRAME_SEC: f32 = 0.08;
    const FIRST_TOKEN_BIAS: f32 = 10.0;
    const MAX_SYMBOLS: usize = 10;
    if !model
        .config()
        .model_type
        .to_ascii_lowercase()
        .contains("tdt")
        || model.static_window() != Some(1_500)
    {
        return Err("ONNX control alignment requires verified static-15 TDT metadata".to_string());
    }
    if conditioned_audio.is_empty()
        || conditioned_audio.len() > 15 * 16_000
        || !conditioned_audio.iter().all(|sample| sample.is_finite())
        || !earliest_sec.is_finite()
    {
        return Err("ONNX control alignment input is invalid".to_string());
    }
    if command.trim().is_empty() {
        return Err("command must be non-empty".to_string());
    }
    let ids =
        rightkit_asr::parakeet_core::bias::encode_phrase_greedy(model.vocab_pieces(), command)
            .ok_or_else(|| "command has no token encoding".to_string())?;
    if ids.is_empty() {
        return Err("command has no token encoding".to_string());
    }
    // ENG-08: the exact retry (locator off) runs right after a fast pass on
    // the same model and the same conditioned audio. With the test-mode switch
    // on it reuses that pass's encoder output instead of re-running mel + the
    // static-window encode; off (default) it re-encodes exactly as before.
    use rightkit_asr::parakeet_core::command_alignment::{
        align_encoder_output, AlignEncoderReuse, AlignEncoderSource,
    };
    let model_id = &*model as *const OnnxAsr as usize;
    let (encoded, encoder_source) = align_encoder_output(
        AlignEncoderReuse::from_env(),
        model_id,
        conditioned_audio,
        locator_enabled,
        || {
            let features = model.mel(conditioned_audio).map_err(|e| e.to_string())?;
            model.encode(&features).map_err(|e| e.to_string())
        },
        |previous, fresh| {
            previous.used == fresh.used
                && previous.time_steps == fresh.time_steps
                && previous.enc_dim == fresh.enc_dim
                && previous.data.len() == fresh.data.len()
                && previous
                    .data
                    .iter()
                    .zip(&fresh.data)
                    .all(|(a, b)| a.to_bits() == b.to_bits())
        },
    )?;
    match encoder_source {
        AlignEncoderSource::Encoded => {}
        AlignEncoderSource::Reused => {
            tracing::debug!("control alignment exact retry reused the fast pass encoder output")
        }
        AlignEncoderSource::Verified { identical } => tracing::info!(
            identical,
            "control alignment encoder reuse verify: exact retry re-encode {} the fast pass output",
            if identical { "matches" } else { "DIFFERS from" }
        ),
    }
    if encoded.used == 0 {
        return Err("ONNX control alignment has no encoder frames".to_string());
    }
    // Search bounds intentionally use the same floor/exclusive convention as CoreML.
    let (first, last) = onnx_alignment_bounds(earliest_sec, encoded.used);
    let blank_id = model.blank_id();
    let blank = blank_id as i32;
    let vocab_len = model.vocab_pieces().len();
    let first_token = ids[0];
    type State = (ndarray::Array3<f32>, ndarray::Array3<f32>);
    type Snapshot = (i32, State, usize, f32);
    type MemoValue = (Vec<f32>, usize, State);
    let state_key = |last: i32, state: &State, frame: usize| {
        let mut hash = 0xcbf29ce484222325u64;
        for value in state.0.iter().chain(state.1.iter()) {
            hash ^= u64::from(value.to_bits());
            hash = hash.wrapping_mul(0x100000001b3);
        }
        (last, hash, frame)
    };
    let mut memo = HashMap::<(i32, u64, usize), MemoValue>::new();
    let mut joint_calls = 0usize;
    let initial_state = model.initial_state();
    let mut step = |last: i32, state: &State, frame: usize| -> Result<MemoValue, String> {
        let key = state_key(last, state, frame);
        if let Some(value) = memo.get(&key) {
            return Ok(value.clone());
        }
        if frame >= encoded.used {
            return Err("decoder step is beyond available frames".to_string());
        }
        let out = model
            .joint_step(&encoded.frame(frame), last, &state.0, &state.1)
            .map_err(|e| e.to_string())?;
        joint_calls += 1;
        if out.logits.len() <= vocab_len {
            return Err("ONNX joint output has no duration head".to_string());
        }
        if blank < 0 || blank as usize >= vocab_len || ids.iter().any(|id| *id >= vocab_len) {
            return Err("ONNX token is outside Joint output".to_string());
        }
        let duration = out.logits[vocab_len..]
            .iter()
            .enumerate()
            .reduce(|best, candidate| {
                if candidate.1 > best.1 {
                    candidate
                } else {
                    best
                }
            })
            // Alignment follows the model duration table (identity 0..4 for
            // this ONNX export), like Mac's locator. The measured Windows
            // beam-search cap belongs only to normal transcription.
            .map(|(index, _)| index)
            .unwrap_or(1);
        let value = (
            rightkit_asr::parakeet_core::log_softmax(&out.logits[..vocab_len]),
            duration,
            (out.state_h, out.state_c),
        );
        memo.insert(key, value.clone());
        Ok(value)
    };

    let mut snapshots: Vec<Option<Snapshot>> = vec![None; encoded.used + 1];
    let mut locator_active = locator_enabled;
    loop {
        let mut skipped_candidate: Option<(usize, f32)> = None;
        let mut state = initial_state.clone();
        let mut last_token = blank;
        let mut frame = 0usize;
        let mut symbols = 0usize;
        let mut score = 0.0f32;
        let mut pass_snapshots: Vec<Option<Snapshot>> = vec![None; encoded.used + 1];
        pass_snapshots[0] = Some((last_token, state.clone(), 0, score));
        while frame < last {
            let previous_frame = frame;
            if symbols >= MAX_SYMBOLS {
                frame += 1;
                symbols = 0;
            } else {
                let (lp, duration, next_state) = step(last_token, &state, frame)?;
                let mut chosen = lp
                    .iter()
                    .enumerate()
                    .reduce(|best, candidate| {
                        if candidate.1 > best.1 {
                            candidate
                        } else {
                            best
                        }
                    })
                    .map(|(index, _)| index)
                    .unwrap_or(blank_id);
                if locator_active && frame >= first {
                    let biased = lp[first_token] + FIRST_TOKEN_BIAS;
                    if biased > lp[chosen] {
                        chosen = first_token;
                    }
                    if chosen == first_token {
                        let (boundary_frame, boundary_score) =
                            skipped_candidate.unwrap_or((frame, lp[first_token]));
                        return Ok(crate::control_cleanup::Alignment {
                            boundary_sec: boundary_frame as f32 * FRAME_SEC,
                            accepted: true,
                            method: "onnx_fast",
                            score: boundary_score,
                            joint_calls,
                        });
                    }
                }
                if chosen as i32 == blank {
                    score += lp[chosen];
                    frame += duration.max(1);
                    symbols = 0;
                } else {
                    score += lp[chosen];
                    last_token = chosen as i32;
                    state = next_state;
                    symbols += 1;
                    if duration != 0 || symbols >= MAX_SYMBOLS {
                        frame += duration.max(1);
                        symbols = 0;
                    }
                }
            }
            if locator_active && skipped_candidate.is_none() {
                // TDT durations skip frames, but command onset may lie in that
                // gap. Score candidates with the state from the original step;
                // never commit a probe's token, duration, or recurrent state.
                for candidate in
                    rightkit_asr::parakeet_core::command_alignment::skipped_locator_frames(
                        previous_frame,
                        frame,
                        first,
                        last,
                    )
                {
                    let (lp, _, _) = step(last_token, &state, candidate)?;
                    let best = lp.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                    if lp[first_token] + FIRST_TOKEN_BIAS > best {
                        // Refine an existing fast match only. If normal
                        // traversal finds none, preserve full suffix fallback.
                        skipped_candidate = Some((candidate, lp[first_token]));
                        break;
                    }
                }
            }
            for slot in pass_snapshots
                .iter_mut()
                .take(frame.min(encoded.used) + 1)
                .skip(1)
            {
                if slot.is_none() {
                    *slot = Some((last_token, state.clone(), frame, score));
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

    let k_total = ids.len();
    let mut candidates = Vec::<(usize, f32, f32)>::new();
    let neg = f32::NEG_INFINITY;
    for (boundary, snapshot) in snapshots.iter().enumerate().take(last).skip(first) {
        let Some((prefix_last, prefix_state, prefix_t, prefix_score)) = snapshot.clone() else {
            continue;
        };
        if prefix_t != boundary {
            continue;
        }
        let mut predictors: Vec<(i32, State)> = vec![(prefix_last, prefix_state)];
        for k in 0..k_total {
            let (last_token, state) = predictors[k].clone();
            let (_, _, next_state) = step(last_token, &state, boundary)?;
            predictors.push((ids[k] as i32, next_state));
        }
        let span = encoded.used - boundary;
        if span == 0 {
            continue;
        }
        let mut lp_blank = vec![vec![neg; span]; k_total + 1];
        let mut lp_tok = vec![vec![neg; span]; k_total];
        for k in 0..=k_total {
            let (last_token, state) = predictors[k].clone();
            for i in 0..span {
                let (lp, _, _) = step(last_token, &state, boundary + i)?;
                lp_blank[k][i] = lp[blank as usize];
                if k < k_total {
                    lp_tok[k][i] = lp[ids[k]];
                }
            }
        }
        if let Some((total, mean)) =
            onnx_exact_suffix_score(prefix_score, &lp_blank, &lp_tok, k_total)
        {
            candidates.push((boundary, total, mean));
        }
    }
    let Some((boundary, score)) = onnx_best_exact_candidate(&candidates) else {
        return Err("ONNX control alignment score rejected".to_string());
    };
    let accepted = onnx_alignment_accepted(score);
    Ok(crate::control_cleanup::Alignment {
        boundary_sec: boundary as f32 * FRAME_SEC,
        accepted,
        method: "onnx_forced_suffix",
        score,
        joint_calls,
    })
}

/// Silence appended to the LAST segment of a recording before decode, on every
/// platform. Both decoders stop at the frame count of the audio they are given
/// (`used` on ONNX, `nframes` on Core ML), so a recording that stops while the
/// final word is still sounding — a toggle released on the last syllable — gives
/// the transcribing hypothesis no frames in which to finish that word. Its last
/// tokens are then scored against a truncated onset, and at beam width 2 a
/// short filler path ("Yeah.") that blanks through the speech wins the
/// likelihood comparison outright. Applied only when the segment still ends in
/// speech (`ends_in_speech`), since padding every clip costs more WER than
/// gating it. 480 ms is six encoder frames: the measured minimum fix (400 ms)
/// plus one frame of margin, at no additional measured cost versus 400.
#[cfg(target_os = "windows")]
const FINAL_TAIL_SILENCE_MS: u64 = 480;
/// Mel hop (10 ms at 16 kHz). A segment of exactly one window yields one
/// frame more than the static encoder accepts, so the tail keeps this much
/// headroom below the window.
#[cfg(target_os = "windows")]
const MEL_HOP_SAMPLES: usize = 160;
/// Window inspected at the end of a segment to decide whether it was cut
/// mid-speech (`ends_in_speech`).
#[cfg(target_os = "windows")]
const FINAL_TAIL_PROBE_MS: u64 = 160;
/// Absolute RMS floor for that window; below this the tail is room tone
/// whatever the ratio says. Conditioned speech in the field captures sits at
/// 0.02-0.07 RMS; their silences at 0.0000-0.001.
#[cfg(target_os = "windows")]
const FINAL_TAIL_SPEECH_FLOOR: f32 = 0.004;
/// The tail window must carry at least this fraction of the whole segment's
/// RMS to count as speech.
#[cfg(target_os = "windows")]
const FINAL_TAIL_SPEECH_RATIO: f32 = 0.3;
pub const AUDIBLE_BLANK_TRANSCRIPTION_ERROR: &str =
    "transcription produced no text although the audio wasn't silent — recording kept, please retry";

/// Supported execution providers for the ONNX fallback path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsrEp {
    /// CUDA (NVIDIA GPU) — opt-in benchmark provider on Windows.
    #[cfg(all(target_os = "windows", feature = "cuda-bench"))]
    Cuda,
    /// DirectML (Windows GPU) — encoder EP on Windows.
    #[cfg(target_os = "windows")]
    Dml,
    /// CPU baseline. Default on Linux and the opt-in fallback on macOS.
    Cpu,
}

impl AsrEp {
    /// Probe-based default EP: DirectML if the device is available on
    /// Windows, otherwise CPU.
    pub fn resolve_default() -> Self {
        #[cfg(target_os = "windows")]
        {
            if let Some(route) = crate::settings::persisted_recognition_route() {
                return if route.provider == "dml" {
                    AsrEp::Dml
                } else {
                    AsrEp::Cpu
                };
            }
            if probe_directml_embedded() {
                AsrEp::Dml
            } else {
                AsrEp::Cpu
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            AsrEp::Cpu
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            #[cfg(all(target_os = "windows", feature = "cuda-bench"))]
            AsrEp::Cuda => "cuda",
            #[cfg(target_os = "windows")]
            AsrEp::Dml => "dml",
            AsrEp::Cpu => "cpu",
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn provider(self) -> ExecutionProvider {
        match self {
            #[cfg(all(target_os = "windows", feature = "cuda-bench"))]
            AsrEp::Cuda => ExecutionProvider::Cpu,
            #[cfg(target_os = "windows")]
            AsrEp::Dml => ExecutionProvider::DirectMl,
            AsrEp::Cpu => ExecutionProvider::Cpu,
        }
    }
}

pub enum AsrRuntime {
    /// ONNX Runtime path (Windows + non-macOS unix). macOS never constructs this —
    /// `OnnxAsr::load` is windows-only — so it is gated off macOS entirely,
    /// which is what lets macOS carry no ONNX dependency at all.
    #[cfg(not(target_os = "macos"))]
    Parakeet(
        OnnxAsr,
        Option<rightkit_asr::parakeet_core::bias::ContextBias>,
    ),
    #[cfg(target_os = "macos")]
    CoreMlParakeet(crate::coreml_asr::CoreMlParakeet),
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    Kws(crate::kws::Kws),
    /// Safety fallback selected when no KWS mechanism passes native acceptance.
    /// It keeps probe scheduling alive but can never emit text, timing, or control.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    ProbeDisabled,
    #[cfg(target_os = "macos")]
    WhisperCoreMl {
        engine: crate::whisper_coreml::WhisperCoreMl,
        lang_tok: i32,
    },
    /// Windows Whisper backend — shells out to the staged whisper.cpp Vulkan
    /// CLI per utterance. See `whisper_win.rs`. `lang` is the Whisper language
    /// code to pass (`"auto"` for detect, else a specific code).
    #[cfg(target_os = "windows")]
    WhisperWin {
        engine: crate::whisper_win::WhisperWin,
        lang: String,
    },
}

/// Compare a just-loaded execution provider/device against the persisted
/// recognition route. Returns `Some(persisted_route_id)` when the loaded
/// route diverges from what onboarding selected: loaded CPU while a
/// dml/coreml route is persisted, or loaded a different DML adapter index
/// than persisted. Pure and I/O-free so it is unit-testable without loading
/// a model. WARN-ONLY signal (I-1/EC-1-B) — never triggers a reload.
pub fn route_ep_mismatch(
    loaded_ep: &str,
    loaded_dml_device_id: Option<i32>,
    persisted: Option<&crate::settings::PersistedRecognitionRoute>,
) -> Option<String> {
    let route = persisted?;
    match (loaded_ep, route.provider.as_str()) {
        ("cpu", "dml") | ("cpu", "coreml") => Some(route.id.clone()),
        ("dml", "dml") if route.adapter_index != loaded_dml_device_id => Some(route.id.clone()),
        _ => None,
    }
}

/// Runtime kinds relevant to the background scheduled-static15 decode policy.
/// Pure classification so the policy is unit-testable without loading model
/// files (constructing a real `WhisperCoreMl`/`CoreMlParakeet` needs a bundle
/// on disk).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundDecodeKind {
    Parakeet,
    CoreMlParakeet,
    WhisperCoreMl,
    WhisperWin,
    Other,
}

/// Which runtime kinds get the background scheduled-static15 pipeline, given
/// the configured ASR backend id. Parakeet stays gated to the validated
/// `parakeet-tdt` alias; Whisper Core ML (macOS) and Whisper Win (Windows)
/// are both unconditional since neither has a TDT-only constraint — measured
/// checkpoint-15 vs. deferred30 on Windows: -3.32 WER points and -3070ms
/// median stop latency (5.50% vs 8.82% WER, 1521ms vs 4591ms stop p50).
/// Everything else (retired Unified/RNNT aliases, KWS/probe runtimes) stays
/// on deferred decode.
pub fn backend_supports_scheduled_static15(kind: BackgroundDecodeKind, backend: &str) -> bool {
    match kind {
        BackgroundDecodeKind::Parakeet | BackgroundDecodeKind::CoreMlParakeet => {
            backend == "parakeet-tdt"
        }
        BackgroundDecodeKind::WhisperCoreMl | BackgroundDecodeKind::WhisperWin => true,
        BackgroundDecodeKind::Other => false,
    }
}

impl AsrRuntime {
    /// Whether the loaded native model supports static-15 TDT suffix alignment.
    /// Other model shapes retain their existing finalisation path.
    pub fn supports_control_cleanup(&self) -> bool {
        #[cfg(target_os = "macos")]
        if let AsrRuntime::CoreMlParakeet(model) = self {
            return model.supports_control_alignment();
        }
        #[cfg(target_os = "windows")]
        if let AsrRuntime::Parakeet(model, _) = self {
            return model
                .config()
                .model_type
                .to_ascii_lowercase()
                .contains("tdt")
                && model.static_window() == Some(1_500);
        }
        false
    }

    /// Prepare candidate-only CoreML frontend lookahead before a measured
    /// decode. The environment switch is read by the resident model here,
    /// after model load, so ordinary runtime startup remains unchanged.
    #[cfg(target_os = "macos")]
    pub fn prepare_frontend_lookahead(
        &mut self,
        memory_gate: crate::coreml_asr::MemoryGate,
    ) -> Result<bool, String> {
        if let AsrRuntime::CoreMlParakeet(model) = self {
            return model.prepare_frontend_lookahead(memory_gate);
        }
        Err("frontend lookahead requires CoreML Parakeet runtime".into())
    }

    /// Snapshot candidate counters and residency for evaluator receipts.
    #[cfg(target_os = "macos")]
    pub fn frontend_lookahead_stats(&self) -> Option<crate::coreml_asr::FrontendLookaheadStats> {
        if let AsrRuntime::CoreMlParakeet(model) = self {
            return model.frontend_lookahead_stats();
        }
        None
    }

    /// Align controls with the resident native decoder. Caller owns the
    /// inference lease; worker code consumes one platform-independent result.
    pub fn align_control_suffix_under_lease(
        &mut self,
        conditioned_audio: &[f32],
        command: &str,
        earliest_sec: f32,
    ) -> Result<crate::control_cleanup::Alignment, String> {
        #[cfg(target_os = "macos")]
        if let AsrRuntime::CoreMlParakeet(model) = self {
            let found = model.align_control_suffix(conditioned_audio, command, earliest_sec)?;
            return Ok(crate::control_cleanup::Alignment {
                boundary_sec: found.boundary_sec,
                accepted: found.accepted,
                method: found.method,
                score: found.score,
                joint_calls: found.joint_calls,
            });
        }
        #[cfg(target_os = "windows")]
        if let AsrRuntime::Parakeet(model, bias) = self {
            let _ = bias;
            return align_onnx_control_suffix(
                model,
                command,
                conditioned_audio,
                earliest_sec,
                true,
            );
        }
        let _ = (conditioned_audio, command, earliest_sec);
        Err("control alignment is unavailable for this ASR runtime".to_string())
    }

    /// Exact suffix search used by Windows cleanup retry & diagnostics.
    #[cfg(target_os = "windows")]
    pub fn align_control_suffix_exact_under_lease(
        &mut self,
        conditioned_audio: &[f32],
        command: &str,
        earliest_sec: f32,
    ) -> Result<crate::control_cleanup::Alignment, String> {
        if let AsrRuntime::Parakeet(model, _) = self {
            return align_onnx_control_suffix(
                model,
                command,
                conditioned_audio,
                earliest_sec,
                false,
            );
        }
        Err("control alignment is unavailable for this ASR runtime".to_string())
    }

    /// Cross-platform isolated command/trigger runtime. Final dictation
    /// continues through `load`, using each platform's primary ASR unchanged.
    pub fn load_probe(models_dir: &Path, ep: AsrEp) -> Result<Self, String> {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            if rightkit_dictation_core::dev_var!("RIGHTKIT_DISABLE_KWS")
                .ok()
                .as_deref()
                == Some("1")
            {
                return Ok(AsrRuntime::ProbeDisabled);
            }
            let _ = (models_dir, ep);
            let dir = crate::engine_paths::kws_dir().ok_or_else(|| {
                "bundled KWS directory was not resolved at engine startup".to_string()
            })?;
            crate::kws::Kws::load(&dir).map(AsrRuntime::Kws)
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        Self::load(models_dir, ep)
    }

    /// The background scheduler is a validated TDT-only production policy for
    /// Parakeet, and unconditional for the macOS Whisper Core ML and Windows
    /// Whisper backends. Retired Unified/RNNT aliases and KWS/probe runtimes
    /// retain their existing deferred windowing behavior.
    pub fn uses_scheduled_static15(&self) -> bool {
        backend_supports_scheduled_static15(self.background_decode_kind(), &configured_backend())
    }

    fn background_decode_kind(&self) -> BackgroundDecodeKind {
        match self {
            #[cfg(not(target_os = "macos"))]
            AsrRuntime::Parakeet(..) => BackgroundDecodeKind::Parakeet,
            #[cfg(target_os = "macos")]
            AsrRuntime::CoreMlParakeet(_) => BackgroundDecodeKind::CoreMlParakeet,
            #[cfg(target_os = "macos")]
            AsrRuntime::WhisperCoreMl { .. } => BackgroundDecodeKind::WhisperCoreMl,
            #[cfg(target_os = "windows")]
            AsrRuntime::WhisperWin { .. } => BackgroundDecodeKind::WhisperWin,
            #[allow(unreachable_patterns)]
            _ => BackgroundDecodeKind::Other,
        }
    }

    /// Load the configured ASR model from `models_dir`.
    ///
    /// macOS prefers native CoreML bundles so the always-hot sidecar owns the
    /// ANE-resident production path. macOS uses the validated Unified CoreML
    /// bundle for the default alias and keeps TDT as an explicit rollback lane.
    pub fn load(models_dir: &Path, ep: AsrEp) -> Result<Self, String> {
        // DirectML adapter selection is persisted by onboarding. It must be
        // applied before `OnnxAsr::load`, where ORT builds its sessions.
        // Read only by the macOS CoreML route check below; the other targets
        // never consult it, so binding it there is an unused-variable warning.
        #[cfg(target_os = "macos")]
        let persisted_route = crate::settings::persisted_recognition_route();
        crate::calibration::apply_persisted_active_route();
        #[cfg(target_os = "macos")]
        if rightkit_dictation_core::dev_var!("RIGHTKIT_ASR_COREML")
            .ok()
            .as_deref()
            != Some("0")
        {
            let backend = configured_backend();
            let lang = crate::settings::dictation_language();
            // "auto"  = Parakeet auto-detect, 25 EU languages (free, bundled).
            // "multi" = Whisper auto-detect across 100+ languages (Pro).
            // <code>  = Whisper locked to that language (Pro) — Parakeet has no
            //           language input, so forcing a language (incl. an EU one,
            //           for short-utterance reliability) requires Whisper.
            let force_whisper =
                (lang != "auto" && !lang.is_empty()) || backend.starts_with("whisper");
            if force_whisper {
                // "multi" maps to Whisper's own "auto" detect token.
                let whisper_lang = if lang == "multi" { "auto" } else { &lang };
                return try_load_coreml_whisper(models_dir, whisper_lang);
            }
            if backend.starts_with("parakeet") || backend == "rnnt" {
                if let Some(runtime) = try_load_coreml_parakeet(models_dir)? {
                    return Ok(runtime);
                }
                if persisted_route
                    .as_ref()
                    .is_some_and(|route| route.provider == "coreml")
                {
                    return Err("selected CoreML recognition route is unavailable; complete onboarding setup again".to_string());
                }
            }
        }

        // Windows Whisper lane: Whisper multilingual is a cross-platform Pro
        // feature. The macOS engine is CoreML/ANE; Windows shells out to the
        // staged whisper.cpp Vulkan CLI per utterance (`whisper_win.rs`). Same
        // routing rule as macOS: a locked language, or an explicit whisper
        // backend selection, forces Whisper. "auto" stays on Parakeet (free,
        // bundled, on-device) — Whisper is Pro-gated at the shell's config
        // push, not here.
        #[cfg(target_os = "windows")]
        {
            let backend = configured_backend();
            let lang = crate::settings::dictation_language();
            let force_whisper =
                (lang != "auto" && !lang.is_empty()) || backend.starts_with("whisper");
            if force_whisper {
                let whisper_lang = if lang == "multi" {
                    "auto".to_string()
                } else {
                    lang.clone()
                };
                return try_load_whisper_win(models_dir, whisper_lang);
            }
        }

        // macOS has NO ONNX fallback. `rightkit-dictation-onnx-asr` is deliberately not a macOS
        // dependency: decoding, detokenization, result types and score policy are
        // first-party in heardright-parakeet-core, and execution is CoreML.
        // Reaching here means the CoreML route was declined (RIGHTKIT_ASR_COREML=0) or
        // unavailable, and on macOS there is no second engine to fall back to.
        #[cfg(target_os = "macos")]
        {
            let _ = ep;
            Err("CoreML recognition is unavailable and macOS has no ONNX fallback".to_string())
        }
        #[cfg(not(target_os = "macos"))]
        {
            let model_subdir = model_subdir_for(&configured_backend());
            let candidate = models_dir.join(&model_subdir);
            // The launch frame names the exact directory the shell resolved. When it is missing,
            // fail naming it: falling back to the models root hid the real path behind a
            // misleading "missing model artifact ...\models\config.json". The root itself is
            // used only when it holds a complete model (a models directory pointed straight at
            // one, as engine tests do).
            let model_dir = if candidate.is_dir() {
                candidate.as_path()
            } else if models_dir.join("config.json").is_file() {
                models_dir
            } else {
                return Err(format!(
                    "model directory not found: {} (models base {})",
                    candidate.display(),
                    models_dir.display()
                ));
            };
            #[cfg_attr(not(target_os = "windows"), allow(unused_mut))]
            let mut expected_route = crate::settings::persisted_recognition_route();
            #[cfg(target_os = "windows")]
            if matches!(ep, AsrEp::Dml) && !crate::settings::onboarding_calibration_active() {
                if let Some(route) = expected_route
                    .as_mut()
                    .filter(|route| route.provider == "dml")
                {
                    // A saved GPU that is gone, renamed or ambiguous never blocks dictation: load
                    // on the default DirectML adapter and keep the saved route for when it returns.
                    let resolved =
                        crate::calibration::enumerate_dml_adapters().and_then(|adapters| {
                            crate::calibration::resolve_calibrated_dml_adapter(route, &adapters)
                        });
                    match resolved {
                        Ok(index) => {
                            if route.adapter_index != Some(index) {
                                tracing::warn!(
                                    saved_index = route.adapter_index,
                                    resolved_index = index,
                                    adapter_name = route.adapter_name.as_deref(),
                                    "calibrated DirectML adapter index changed; retaining selected GPU"
                                );
                            }
                            crate::calibration::set_active_route(crate::calibration::ActiveRoute {
                                dml_device_id: Some(index),
                            });
                            route.adapter_index = Some(index);
                        }
                        Err(error) => {
                            tracing::warn!(
                                %error,
                                adapter_name = route.adapter_name.as_deref(),
                                "calibrated DirectML adapter unavailable; loading on the default adapter"
                            );
                            crate::calibration::set_active_route(crate::calibration::ActiveRoute {
                                dml_device_id: None,
                            });
                            route.adapter_index = None;
                        }
                    }
                }
            }
            let cfg = SessionOptions::new(ep.provider());
            #[cfg(target_os = "windows")]
            let cfg = if matches!(ep, AsrEp::Dml) {
                if let Some(adapter_index) = crate::calibration::active_route()
                    .dml_device_id
                    .filter(|index| *index >= 0)
                {
                    cfg.with_device_id(Some(adapter_index))
                } else {
                    cfg
                }
            } else {
                cfg
            };
            let model = OnnxAsr::load(model_dir, cfg).map_err(|e| {
                format!(
                    "model load ({}) on {}: {}",
                    model_dir.display(),
                    ep.as_str(),
                    e
                )
            })?;
            let bias = initial_context_bias(&model);
            let dml_device_id = crate::calibration::active_route().dml_device_id;
            #[cfg_attr(not(target_os = "windows"), allow(unused_mut))]
            let mut dml_adapter_name: Option<String> = None;
            #[cfg(target_os = "windows")]
            if matches!(ep, AsrEp::Dml) {
                if let Some(index) = dml_device_id {
                    if let Ok(adapters) = crate::calibration::enumerate_dml_adapters() {
                        dml_adapter_name = adapters
                            .into_iter()
                            .find(|adapter| adapter.index == index)
                            .map(|adapter| adapter.name);
                    }
                }
            }
            tracing::info!(
                ep = ep.as_str(),
                dml_device_id,
                dml_adapter_name = dml_adapter_name.as_deref(),
                model_dir = %model_dir.display(),
                "ASR model loaded"
            );
            if let Some(persisted_route_id) =
                route_ep_mismatch(ep.as_str(), dml_device_id, expected_route.as_ref())
            {
                tracing::error!(
                    event = "asr_route_mismatch",
                    loaded_ep = ep.as_str(),
                    persisted_route_id,
                );
            }
            Ok(AsrRuntime::Parakeet(model, bias))
        }
    }

    /// Run a mutation plus inference as one transaction. A shared runtime uses
    /// mutable decoder bias, so callers that change bias must hold this lease
    /// through the matching decode.
    pub fn with_inference_lease<T>(
        &mut self,
        owner: &'static str,
        work: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<T, String> {
        #[cfg(target_os = "macos")]
        if matches!(
            self,
            AsrRuntime::CoreMlParakeet(_) | AsrRuntime::WhisperCoreMl { .. }
        ) {
            let _lease = crate::coreml::inference_lease(owner);
            return work(self);
        }
        let _lease = crate::inference_gate::lease(owner);
        work(self)
    }

    pub fn transcribe(&mut self, samples: &[f32]) -> Result<String, String> {
        let owner = match self {
            #[cfg(not(target_os = "macos"))]
            AsrRuntime::Parakeet(..) => "parakeet_asr",
            #[cfg(target_os = "macos")]
            AsrRuntime::CoreMlParakeet(_) => "parakeet_asr",
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            AsrRuntime::Kws(_) => "kws",
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            AsrRuntime::ProbeDisabled => "disabled_probe",
            #[cfg(target_os = "macos")]
            AsrRuntime::WhisperCoreMl { .. } => "whisper_asr",
            #[cfg(target_os = "windows")]
            AsrRuntime::WhisperWin { .. } => "whisper_win_asr",
        };
        self.with_inference_lease(owner, |model| model.transcribe_under_lease(samples))
    }

    pub fn transcribe_under_lease(&mut self, samples: &[f32]) -> Result<String, String> {
        match self {
            #[cfg(not(target_os = "macos"))]
            AsrRuntime::Parakeet(m, bias) => transcribe_padded_window(samples, |window| {
                onnx_beam_result(m, bias.as_ref(), window)
            })
            .map(|result| result.text),
            #[cfg(target_os = "macos")]
            AsrRuntime::CoreMlParakeet(m) => {
                if samples.len() <= padded_window_samples() {
                    m.transcribe(samples)
                } else {
                    transcribe_padded_window(samples, |window| coreml_parakeet_result(m, window))
                        .map(|result| result.text)
                }
            }
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            AsrRuntime::Kws(m) => m.transcribe(samples),
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            AsrRuntime::ProbeDisabled => Ok(String::new()),
            #[cfg(target_os = "macos")]
            AsrRuntime::WhisperCoreMl {
                engine, lang_tok, ..
            } => engine.transcribe_lang_windowed(samples, *lang_tok).map(n0),
            #[cfg(target_os = "windows")]
            AsrRuntime::WhisperWin { engine, lang } => {
                engine.transcribe_lang(samples, lang).map(n0)
            }
        }
    }

    /// Timed counterpart to `transcribe_final_under_lease`: same audible-blank
    /// guard on CoreML, but retains tokens so the scheduled static-15 window
    /// and final-tail decode can carry timing through commit/finish.
    pub fn transcribe_final_result_under_lease(
        &mut self,
        samples: &[f32],
    ) -> Result<TranscriptionResult, String> {
        match self {
            #[cfg(target_os = "macos")]
            AsrRuntime::CoreMlParakeet(m) => {
                let result = if samples.len() <= padded_window_samples() {
                    coreml_parakeet_result(m, samples)
                } else if m.frontend_lookahead_enabled() {
                    coreml_parakeet_padded_result_with_lookahead(m, samples)
                } else {
                    transcribe_padded_window(samples, |window| coreml_parakeet_result(m, window))
                };
                match result {
                    Ok(result) if result.text.trim().is_empty() => {
                        Err(AUDIBLE_BLANK_TRANSCRIPTION_ERROR.to_string())
                    }
                    other => other,
                }
            }
            // Whisper has no timed-token decode; final dictation must still
            // succeed, so fall back to the text-only path with empty timing.
            #[cfg(target_os = "macos")]
            AsrRuntime::WhisperCoreMl {
                engine, lang_tok, ..
            } => engine
                .transcribe_lang_windowed(samples, *lang_tok)
                .map(|text| TranscriptionResult {
                    text: n0(text),
                    tokens: Vec::new(),
                }),
            #[cfg(target_os = "windows")]
            AsrRuntime::WhisperWin { engine, lang } => {
                engine
                    .transcribe_lang(samples, lang)
                    .map(|text| TranscriptionResult {
                        text: n0(text),
                        tokens: Vec::new(),
                    })
            }
            other => other.transcribe_result_under_lease(samples),
        }
    }

    pub fn transcribe_result(&mut self, samples: &[f32]) -> Result<TranscriptionResult, String> {
        self.with_inference_lease("parakeet_asr_result", |model| {
            model.transcribe_result_under_lease(samples)
        })
    }

    /// Timed CPU KWS decode. Probe runtime is worker-owned and independent
    /// from main ASR, so this path must never take the accelerator gate.
    pub fn transcribe_probe_result(
        &mut self,
        samples: &[f32],
    ) -> Result<TranscriptionResult, String> {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let AsrRuntime::Kws(model) = self {
            return model.transcribe_result(samples);
        }
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if matches!(self, AsrRuntime::ProbeDisabled) {
            return Ok(TranscriptionResult {
                text: String::new(),
                tokens: Vec::new(),
            });
        }
        Err("timed KWS decode requires KWS runtime".into())
    }

    /// Return cascade output plus an explicit absolute onset when the KWS cascade's
    /// acoustic wake candidate paired with a confirmed action. This keeps
    /// confirmation timestamps out of audio-cut ownership.
    pub fn transcribe_probe_cascade_result_with_wake_start(
        &mut self,
        samples: &[f32],
        snapshot_start_sample: usize,
        live_total_sample: usize,
        confirmation_armed: bool,
    ) -> Result<(TranscriptionResult, Option<usize>), String> {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let AsrRuntime::Kws(model) = self {
            return model.transcribe_cascade_result_with_wake_start(
                samples,
                snapshot_start_sample,
                live_total_sample,
                confirmation_armed,
            );
        }
        self.transcribe_probe_result(samples)
            .map(|result| (result, None))
    }

    pub fn transcribe_probe_fresh_confirmation_result_with_wake_start(
        &mut self,
        samples: &[f32],
    ) -> Result<(TranscriptionResult, Option<usize>), String> {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let AsrRuntime::Kws(model) = self {
            return model.transcribe_fresh_confirmation_result_with_wake_start(samples);
        }
        self.transcribe_probe_result(samples)
            .map(|result| (result, None))
    }

    pub fn transcribe_scheduled_probe_result_with_wake_start(
        &mut self,
        samples: &[f32],
        snapshot_start_sample: usize,
        live_total_sample: usize,
        confirmation_armed: bool,
        use_fresh_confirmation: bool,
    ) -> Result<(TranscriptionResult, Option<usize>, bool), String> {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let AsrRuntime::Kws(model) = self {
            return model.transcribe_scheduled_probe_result_with_wake_start(
                samples,
                snapshot_start_sample,
                live_total_sample,
                confirmation_armed,
                use_fresh_confirmation,
            );
        }
        self.transcribe_probe_result(samples)
            .map(|result| (result, None, false))
    }

    pub fn transcribe_result_under_lease(
        &mut self,
        samples: &[f32],
    ) -> Result<TranscriptionResult, String> {
        match self {
            #[cfg(not(target_os = "macos"))]
            AsrRuntime::Parakeet(m, bias) => transcribe_padded_window(samples, |window| {
                onnx_beam_result(m, bias.as_ref(), window)
            }),
            #[cfg(target_os = "macos")]
            AsrRuntime::CoreMlParakeet(m) => {
                transcribe_padded_window(samples, |window| coreml_parakeet_result(m, window))
            }
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            AsrRuntime::Kws(m) => m.transcribe_result(samples),
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            AsrRuntime::ProbeDisabled => Ok(TranscriptionResult {
                text: String::new(),
                tokens: Vec::new(),
            }),
            #[cfg(target_os = "macos")]
            AsrRuntime::WhisperCoreMl { .. } => {
                Err("streaming partial timestamps are unavailable for this ASR backend".into())
            }
            #[cfg(target_os = "windows")]
            AsrRuntime::WhisperWin { .. } => {
                Err("streaming partial timestamps are unavailable for this ASR backend".into())
            }
        }
    }

    /// Same contract as `transcribe_final_result_under_lease`, plus a
    /// per-recording language lock for Whisper Core ML. The background
    /// static-15 scheduler decodes one recording across many separate calls
    /// (each 15 s window, then the tail); without this, AUTO language
    /// detection would run fresh in every call and could flip languages
    /// mid-recording. `locked_lang` is owned by the caller (reset per
    /// recording generation, alongside `ScheduledStatic15::reset`) and is
    /// filled in from whatever the first call resolves — an explicit
    /// (non-AUTO) language just locks onto itself, a no-op.
    pub fn transcribe_final_result_under_lease_locked(
        &mut self,
        samples: &[f32],
        #[cfg_attr(not(target_os = "macos"), allow(unused_variables))] locked_lang: &mut Option<
            i32,
        >,
    ) -> Result<TranscriptionResult, String> {
        #[cfg(target_os = "macos")]
        if let AsrRuntime::WhisperCoreMl { engine, lang_tok } = self {
            let effective = locked_lang.unwrap_or(*lang_tok);
            let (text, resolved) = engine.transcribe_lang_windowed_resolved(samples, effective)?;
            if locked_lang.is_none() {
                *locked_lang = Some(resolved);
            }
            return Ok(TranscriptionResult {
                text: n0(text),
                tokens: Vec::new(),
            });
        }
        #[cfg(target_os = "windows")]
        if let AsrRuntime::WhisperWin { engine, lang } = self {
            let (text, resolved) = engine.transcribe_lang_locked(samples, lang, *locked_lang)?;
            if locked_lang.is_none() {
                if let Some(resolved) = resolved {
                    *locked_lang = Some(resolved);
                }
            }
            return Ok(TranscriptionResult {
                text: n0(text),
                tokens: Vec::new(),
            });
        }
        self.transcribe_final_result_under_lease(samples)
    }

    /// KWS control firing requires token-frame timing. If a timed decode
    /// fails, do not retry as text-only: that would reintroduce full-buffer
    /// final transcription of fired control audio.
    pub fn requires_timed_control_probe(&self) -> bool {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            matches!(self, AsrRuntime::Kws(_))
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            false
        }
    }

    pub fn reset_probe_stream(&mut self) -> Result<(), String> {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let AsrRuntime::Kws(model) = self {
            return model.reset_stream();
        }
        Ok(())
    }

    pub fn start_idle_wake(&mut self, threshold: f32) -> Result<(), String> {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let AsrRuntime::Kws(model) = self {
            return model.start_idle_wake(threshold);
        }
        Err("idle wake requires KWS".into())
    }

    pub fn stop_idle_wake(&mut self) {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let AsrRuntime::Kws(model) = self {
            model.stop_idle_wake();
        }
    }

    pub fn feed_idle_wake(
        &mut self,
        samples: &[f32],
        start_sample: usize,
        live_total_sample: usize,
    ) -> Result<Option<(usize, usize)>, String> {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let AsrRuntime::Kws(model) = self {
            return model.feed_idle_wake(samples, start_sample, live_total_sample);
        }
        Err("idle wake requires KWS".into())
    }

    pub fn transcribe_file(&mut self, samples: &[f32]) -> Result<FileTranscript, String> {
        self.with_inference_lease("parakeet_file_asr", |model| {
            model.transcribe_file_under_lease(samples)
        })
    }

    pub fn transcribe_file_under_lease(
        &mut self,
        samples: &[f32],
    ) -> Result<FileTranscript, String> {
        let mut job = FileJob::new(samples.to_vec());
        loop {
            if let FileStep::Done(result) = self.file_step(&mut job) {
                return result;
            }
        }
    }

    /// Decode at most ONE window of `job`, then return, so the caller can run
    /// dictation between windows. `Done` carries the final transcript.
    pub fn file_step(&mut self, job: &mut FileJob) -> FileStep {
        if job.finished {
            return FileStep::Done(Err("file job already finished".into()));
        }
        let FileJob {
            samples,
            cursor,
            finished,
            verbatim,
        } = job;
        let mut policy = rightkit_asr::parakeet_core::DetokPolicy::canonical_rust();
        policy.strip_fillers = !*verbatim;
        let outcome: Result<Option<FileTranscript>, String> = match self {
            #[cfg(target_os = "macos")]
            AsrRuntime::CoreMlParakeet(m) => {
                if samples.len() > padded_window_samples() {
                    cursor
                        .step(samples, |window| {
                            coreml_parakeet_file_result(m, window, policy)
                        })
                        .map(|done| {
                            done.map(|result| {
                                let words = timed_tokens_to_words(&result.tokens);
                                coreml_file_transcript(result.text, words)
                            })
                        })
                } else {
                    coreml_parakeet_file_result(m, samples, policy).map(|result| {
                        let words = timed_tokens_to_words(&result.tokens);
                        Some(coreml_file_transcript(result.text, words))
                    })
                }
            }
            #[cfg(target_os = "windows")]
            AsrRuntime::Parakeet(m, bias) => cursor
                .step(samples, |window| {
                    onnx_beam_result_with_policy(m, bias.as_ref(), window, policy)
                })
                .map(|done| {
                    done.map(|result| {
                        let words = timed_tokens_to_file_words(&result.tokens);
                        let (srt, vtt) = render_file_subtitles(&words);
                        FileTranscript {
                            text: result.text,
                            srt,
                            vtt,
                            words: words
                                .into_iter()
                                .map(|word| {
                                    TimedWord::new(
                                        word.text,
                                        (word.start.max(0.0) * 1_000.0).round() as u64,
                                        (word.end.max(word.start + 0.08) * 1_000.0).round() as u64,
                                    )
                                })
                                .collect(),
                        }
                    })
                }),
            #[cfg(target_os = "macos")]
            AsrRuntime::WhisperCoreMl { engine, lang_tok } => engine
                .transcribe_lang_windowed(samples, *lang_tok)
                .map(|text| {
                    Some(FileTranscript {
                        text: if *verbatim { text } else { n0(text) },
                        srt: String::new(),
                        vtt: String::new(),
                        words: Vec::new(),
                    })
                }),
            #[cfg(target_os = "windows")]
            AsrRuntime::WhisperWin { engine, lang } => {
                engine.transcribe_lang(samples, lang).map(|text| {
                    Some(FileTranscript {
                        text: if *verbatim { text } else { n0(text) },
                        srt: String::new(),
                        vtt: String::new(),
                        words: Vec::new(),
                    })
                })
            }
            other => other.transcribe_under_lease(samples).map(|text| {
                Some(FileTranscript {
                    text,
                    srt: String::new(),
                    vtt: String::new(),
                    words: Vec::new(),
                })
            }),
        };
        match outcome {
            Ok(Some(transcript)) => {
                *finished = true;
                FileStep::Done(Ok(transcript))
            }
            Ok(None) => FileStep::Pending,
            Err(err) => {
                *finished = true;
                FileStep::Done(Err(err))
            }
        }
    }
}

/// Resumable long-file transcription state. Owns the audio and the window
/// cursor; drive it with `AsrRuntime::file_step`.
#[derive(Debug)]
pub struct FileJob {
    samples: Vec<f32>,
    #[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
    cursor: PaddedWindowCursor,
    finished: bool,
    verbatim: bool,
}

/// Result of one `file_step`.
#[derive(Debug)]
pub enum FileStep {
    Pending,
    Done(Result<FileTranscript, String>),
}

impl FileJob {
    pub fn new(samples: Vec<f32>) -> Self {
        Self {
            samples,
            cursor: PaddedWindowCursor::new(),
            finished: false,
            verbatim: false,
        }
    }

    pub fn with_verbatim(mut self, verbatim: bool) -> Self {
        self.verbatim = verbatim;
        self
    }

    pub fn total_samples(&self) -> usize {
        self.samples.len()
    }

    /// Progress in samples; monotonic, reaches `total_samples` when finished.
    pub fn done_samples(&self) -> usize {
        if self.finished {
            self.samples.len()
        } else {
            self.cursor.done_samples(self.samples.len())
        }
    }
}

#[cfg(target_os = "macos")]
fn coreml_file_transcript(text: String, words: Vec<crate::coreml_asr::TimedTok>) -> FileTranscript {
    let (srt, vtt) = crate::coreml_asr::render_subtitles(&words);
    FileTranscript {
        text,
        srt,
        vtt,
        words: words
            .into_iter()
            .map(|word| {
                TimedWord::new(
                    word.text,
                    (word.start.max(0.0) * 1_000.0).round() as u64,
                    (word.end.max(word.start + 0.08) * 1_000.0).round() as u64,
                )
            })
            .collect(),
    }
}

#[derive(Debug)]
struct Static15Checkpoint {
    start_sample: usize,
    end_sample: usize,
    text: String,
    overlap_bytes: usize,
    inserted_separator: bool,
    /// Tokens from this window's decode, converted to recording-absolute
    /// seconds (`start_sample / SAMPLE_RATE + window_relative_time`).
    tokens: Vec<TimedToken>,
}

/// Per-recording static-15 scheduler shared by the Windows ONNX/DML and macOS
/// Core ML TDT runtimes. Audio remains owned by the worker's single, hard-capped
/// recording buffer; checkpoints retain enough provenance to roll back only
/// segments overlapping a later KWS cut.
#[derive(Debug, Default)]
pub struct ScheduledStatic15 {
    checkpoints: Vec<Static15Checkpoint>,
    committed_text: String,
    committed_samples: usize,
    /// Tokens from every retained checkpoint, in recording-absolute time.
    /// Rebuilt from `checkpoints` on rollback, exactly like `committed_text`.
    committed_tokens: Vec<TimedToken>,
}

impl ScheduledStatic15 {
    pub fn reset(&mut self) {
        self.checkpoints.clear();
        self.committed_text.clear();
        self.committed_samples = 0;
        self.committed_tokens.clear();
    }

    pub fn background_windows(&self) -> usize {
        self.checkpoints.len()
    }

    pub fn committed_samples(&self) -> usize {
        self.committed_samples
    }

    pub fn committed_text_counts(&self) -> (usize, usize) {
        (
            self.committed_text.chars().count(),
            self.committed_text.split_whitespace().count(),
        )
    }

    pub fn commit_window(
        &mut self,
        start_sample: usize,
        end_sample: usize,
        text: &str,
        tokens: Vec<TimedToken>,
    ) -> Result<(), String> {
        if start_sample != self.committed_samples {
            return Err(format!(
                "scheduled static-15 out of order: expected {}, got {}",
                self.committed_samples, start_sample
            ));
        }
        if end_sample <= start_sample {
            return Err("scheduled static-15 window must advance".to_string());
        }
        if text.trim().is_empty() {
            return Err(
                "scheduled static-15 window produced no text; defer full audio to stop".to_string(),
            );
        }
        let offset = start_sample as f32 / SAMPLE_RATE as f32;
        let token_start = self.committed_tokens.len();
        let (overlap_bytes, inserted_separator) = append_timed_result(
            &mut self.committed_text,
            &mut self.committed_tokens,
            TranscriptionResult {
                text: text.to_string(),
                tokens,
            },
            offset,
        );
        let absolute_tokens = self
            .committed_tokens
            .get(token_start..)
            .unwrap_or_default()
            .to_vec();
        self.checkpoints.push(Static15Checkpoint {
            start_sample,
            end_sample,
            text: text.to_string(),
            overlap_bytes,
            inserted_separator,
            tokens: absolute_tokens,
        });
        self.committed_samples = end_sample;
        Ok(())
    }

    /// Assemble final transcript plus recording-absolute timings. A backend
    /// without timing support returns an empty token list; never synthesize it.
    pub fn finish_result<F>(
        &mut self,
        samples: &[f32],
        mut decode: F,
    ) -> Result<TranscriptionResult, String>
    where
        F: FnMut(&[f32]) -> Result<TranscriptionResult, String>,
    {
        // A KWS command cut can land inside the latest padded segment. Retain
        // only checkpoints fully before the cut, then decode exactly from that
        // retained cursor to the cut.
        if samples.len() < self.committed_samples {
            self.checkpoints
                .retain(|checkpoint| checkpoint.end_sample <= samples.len());
            self.committed_samples = self
                .checkpoints
                .last()
                .map_or(0, |checkpoint| checkpoint.end_sample);
            self.committed_text.clear();
            // Timing MUST roll back with the text it belongs to. Tokens live in
            // the checkpoint, so `retain` above already dropped the discarded
            // ones; rebuild the flat accumulator from what survived so a late
            // KWS cut can never leave orphaned timings pointing past the cut.
            self.committed_tokens.clear();
            for checkpoint in &self.checkpoints {
                if checkpoint.inserted_separator {
                    self.committed_text.push(' ');
                }
                self.committed_text
                    .push_str(&checkpoint.text[checkpoint.overlap_bytes..]);
                self.committed_tokens
                    .extend(checkpoint.tokens.iter().map(|token| TimedToken {
                        text: token.text.clone(),
                        start: token.start,
                        end: token.end,
                    }));
            }
        }
        let mut combined = self.committed_text.clone();
        let mut expected_start = 0;
        for checkpoint in &self.checkpoints {
            expected_start = checkpoint.end_sample;
        }
        if self.committed_samples < samples.len() {
            let tail_samples = samples.len() - self.committed_samples;
            let tail_offset = self.committed_samples as f32 / SAMPLE_RATE as f32;
            match decode(&samples[self.committed_samples..]) {
                Ok(result) => {
                    let text = result.text.clone();
                    let tail_chars = text.chars().count();
                    let tail_words = text.split_whitespace().count();
                    let tail_empty = text.trim().is_empty();
                    let (overlap_bytes, _) = append_timed_result(
                        &mut combined,
                        &mut self.committed_tokens,
                        result,
                        tail_offset,
                    );
                    tracing::info!(
                        committed_samples = self.committed_samples,
                        tail_samples,
                        tail_chars,
                        tail_words,
                        tail_empty,
                        overlap_bytes,
                        assembled_chars = combined.chars().count(),
                        assembled_words = combined.split_whitespace().count(),
                        "scheduled static-15 final tail assembled"
                    );
                }
                Err(error) => {
                    // The audible-blank guard exists to catch silent model
                    // collapse. Non-empty committed text proves the model is
                    // decoding this very recording, so a blank tail (trailing
                    // breath/noise after a trigger cut) must keep the committed
                    // transcript instead of failing the whole assembly.
                    if error == AUDIBLE_BLANK_TRANSCRIPTION_ERROR && !combined.trim().is_empty() {
                        tracing::info!(
                            committed_samples = self.committed_samples,
                            tail_samples,
                            "scheduled static-15 final tail blank on audible audio; keeping committed transcript"
                        );
                    } else {
                        return Err(format!(
                            "scheduled static-15 partial transcript: committed through sample {}; \
                             final tail of {tail_samples} samples failed: {error}",
                            self.committed_samples
                        ));
                    }
                }
            }
        }
        tracing::info!(
            background_windows = self.background_windows(),
            committed_samples = self.committed_samples,
            total_samples = samples.len(),
            assembled_chars = combined.chars().count(),
            assembled_words = combined.split_whitespace().count(),
            assembled_empty = combined.trim().is_empty(),
            "scheduled static-15 assembly complete"
        );
        Ok(TranscriptionResult {
            text: combined.trim().to_string(),
            tokens: self
                .committed_tokens
                .iter()
                .map(|token| TimedToken {
                    text: token.text.clone(),
                    start: token.start,
                    end: token.end,
                })
                .collect(),
        })
    }
}

#[doc(hidden)]
pub fn scheduled_static15_ready_segment(
    samples: &[f32],
    start_sample: usize,
) -> Option<std::ops::Range<usize>> {
    let window = padded_window_samples();
    if start_sample + window >= samples.len() {
        return None;
    }
    let window_end = start_sample + window;
    let target_end = start_sample + window.saturating_sub(ms_to_samples(PADDED_WINDOW_PADDING_MS));
    let cut = quiet_cut_in(samples, target_end, window_end)
        .unwrap_or(window_end)
        .max(start_sample + 1);
    Some(start_sample..cut)
}

#[doc(hidden)]
pub fn append_scheduled_static15_text(buffer: &mut String, next: &str) {
    append_with_overlap(buffer, next);
}

/// True when the segment's last `FINAL_TAIL_PROBE_MS` still carry speech-level
/// energy relative to the segment as a whole — the recording was stopped
/// while the user was still talking. A segment that already ends in room
/// tone gets no tail: on the Windows canonical 50-clip set, padding every
/// clip cost 0.27 WER points (4.52% -> 4.79%) because digital zeros appended
/// after natural silence made the model drop a trailing word, while gating on
/// trailing energy leaves those clips byte-identical.
#[cfg(target_os = "windows")]
fn ends_in_speech(segment: &[f32]) -> bool {
    let probe = ms_to_samples(FINAL_TAIL_PROBE_MS).min(segment.len());
    if probe == 0 {
        return false;
    }
    let rms = |s: &[f32]| (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt();
    let tail_rms = rms(&segment[segment.len() - probe..]);
    let whole_rms = rms(segment);
    tail_rms > FINAL_TAIL_SPEECH_FLOOR && tail_rms > whole_rms * FINAL_TAIL_SPEECH_RATIO
}

/// See `FINAL_TAIL_SILENCE_MS`. Appends up to that much silence to a final
/// segment that still ends in speech, never growing it past the model window.
/// Timing is unaffected: silence is appended after every real sample.
#[cfg(target_os = "windows")]
fn with_terminal_silence(segment: &[f32], window: usize) -> std::borrow::Cow<'_, [f32]> {
    // One mel hop of headroom: a segment of exactly `window` samples produces
    // window / hop + 1 frames, one more than the static encoder accepts.
    let room = window
        .saturating_sub(MEL_HOP_SAMPLES)
        .saturating_sub(segment.len());
    let tail_ms = rightkit_dictation_core::dev_var!("RIGHTKIT_FINAL_TAIL_MS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(FINAL_TAIL_SILENCE_MS);
    let tail = ms_to_samples(tail_ms).min(room);
    if tail == 0 || segment.is_empty() || !ends_in_speech(segment) {
        return std::borrow::Cow::Borrowed(segment);
    }
    // Measurement knob: pad with low-level deterministic noise instead of
    // digital zeros (`RIGHTKIT_FINAL_TAIL_NOISE=<rms>`), to compare against the
    // corpus. Zeros are the shipping default.
    let noise_rms = rightkit_dictation_core::dev_var!("RIGHTKIT_FINAL_TAIL_NOISE")
        .ok()
        .and_then(|raw| raw.trim().parse::<f32>().ok())
        .unwrap_or(0.0);
    let mut padded = Vec::with_capacity(segment.len() + tail);
    padded.extend_from_slice(segment);
    if noise_rms > 0.0 {
        let mut state: u32 = 0x9e37_79b9;
        for _ in 0..tail {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let unit = (state >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0;
            padded.push(unit * noise_rms * 1.732);
        }
    } else {
        padded.resize(segment.len() + tail, 0.0);
    }
    std::borrow::Cow::Owned(padded)
}

/// Platform policy for the last segment of a recording. Windows (ONNX beam)
/// applies the terminal-silence flush. macOS is release-locked and
/// field-validated at beam 2 and is left byte-for-byte unchanged, and every
/// other target keeps the existing behaviour — the same mechanism-shared,
/// value-per-platform rule the duration cap follows. Do not enable it
/// elsewhere without that platform's corpus run.
#[cfg(target_os = "windows")]
pub(super) fn final_segment(segment: &[f32], window: usize) -> std::borrow::Cow<'_, [f32]> {
    with_terminal_silence(segment, window)
}

#[cfg(not(target_os = "windows"))]
pub(super) fn final_segment(segment: &[f32], _window: usize) -> std::borrow::Cow<'_, [f32]> {
    std::borrow::Cow::Borrowed(segment)
}

pub(crate) fn transcribe_padded_window<F>(
    samples: &[f32],
    mut decode: F,
) -> Result<TranscriptionResult, String>
where
    F: FnMut(&[f32]) -> Result<TranscriptionResult, String>,
{
    let mut cursor = PaddedWindowCursor::new();
    loop {
        if let Some(result) = cursor.step(samples, &mut decode)? {
            return Ok(result);
        }
    }
}

/// Resumable form of the padded-window loop: each `step` performs at most ONE
/// decode, so a caller can interleave other work (dictation) between windows.
/// `transcribe_padded_window` and `AsrRuntime::file_step` both drive this, so
/// there is a single production algorithm.
#[derive(Debug, Default)]
pub(crate) struct PaddedWindowCursor {
    start: usize,
    text: String,
    tokens: Vec<TimedToken>,
    finished: bool,
}

impl PaddedWindowCursor {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Samples consumed so far (monotonic). Equals `total` once finished.
    pub(crate) fn done_samples(&self, total: usize) -> usize {
        if self.finished {
            total
        } else {
            self.start.min(total)
        }
    }

    /// Decode one window. `Ok(None)` means more windows remain; `Ok(Some(_))`
    /// is the final transcript (the cursor is then finished).
    pub(crate) fn step<F>(
        &mut self,
        samples: &[f32],
        mut decode: F,
    ) -> Result<Option<TranscriptionResult>, String>
    where
        F: FnMut(&[f32]) -> Result<TranscriptionResult, String>,
    {
        if self.finished {
            return Err("padded window cursor already finished".into());
        }
        let window = padded_window_samples();
        if samples.len() <= window {
            self.finished = true;
            return decode(&final_segment(samples, window)).map(Some);
        }

        // Decode exactly to the quiet cut, matching the iOS runtime
        // (`ParakeetCoreMLRuntime.decodePaddedWindow`). The quiet-cut boundary is
        // unchanged — only the COMMIT rule differs from the previous form, which
        // decoded the full window and kept tokens whose 80 ms-quantised frame time
        // fell before the cut. That quantisation adjudicates every seam and gets
        // some wrong.
        //
        // REQUIRES the separator branch in `append_with_overlap`: these windows are
        // DISJOINT, so there is no character overlap to consume, and a raw
        // concatenation welds words across the seam. Porting this rule without that
        // fix measured 8.55% WER; with it, see the eval doc.
        if self.start < samples.len() && self.start + window < samples.len() {
            let padding = ms_to_samples(PADDED_WINDOW_PADDING_MS);
            let target = window.saturating_sub(padding);
            let start = self.start;
            let window_end = (start + window).min(samples.len());
            let target_end = (start + target).min(samples.len());
            let stable_end = quiet_cut_in(samples, target_end, window_end).unwrap_or(window_end);
            let segment: &[f32] = if start < stable_end {
                &samples[start..stable_end]
            } else {
                &[]
            };
            let result = match decode(segment) {
                Ok(result) => result,
                Err(err) => {
                    self.finished = true;
                    return Err(err);
                }
            };
            append_timed_result(
                &mut self.text,
                &mut self.tokens,
                result,
                start as f32 / SAMPLE_RATE as f32,
            );
            self.start = stable_end.max(start + 1);
            return Ok(None);
        }

        // Tail after the final quiet cut.
        self.finished = true;
        if self.start < samples.len() {
            let result = decode(&final_segment(&samples[self.start..], window))?;
            append_timed_result(
                &mut self.text,
                &mut self.tokens,
                result,
                self.start as f32 / SAMPLE_RATE as f32,
            );
        }
        Ok(Some(TranscriptionResult {
            text: self.text.trim().to_string(),
            tokens: std::mem::take(&mut self.tokens),
        }))
    }
}

/// Keep the timing stream at the same seam as rendered text. An overlap is
/// consumed from both; an unprovable token rendering drops timing only.
pub(super) fn append_timed_result(
    text: &mut String,
    tokens: &mut Vec<TimedToken>,
    result: TranscriptionResult,
    offset: f32,
) -> (usize, bool) {
    let overlap = if text.is_empty() {
        0
    } else {
        suffix_prefix_overlap(text, &result.text, PADDED_WINDOW_OVERLAP_CHARS)
    };
    let remainder = &result.text[overlap..];
    let separator = !text.is_empty()
        && !remainder.is_empty()
        && overlap == 0
        && !text.ends_with(char::is_whitespace)
        && !remainder.starts_with(char::is_whitespace);
    append_with_overlap(text, &result.text);
    let rendered = result
        .tokens
        .iter()
        .map(|token| token.text.as_str())
        .collect::<String>();
    if rendered.trim() != result.text.trim() {
        tokens.clear();
        return (overlap, separator);
    }
    let mut skip = if overlap == 0 {
        0
    } else {
        rendered.len() - rendered.trim_start().len() + overlap
    };
    let mut first = true;
    for mut token in result.tokens {
        if skip >= token.text.len() {
            skip -= token.text.len();
            continue;
        }
        if skip > 0 {
            if !token.text.is_char_boundary(skip) {
                tokens.clear();
                return (overlap, separator);
            }
            token.text = token.text[skip..].to_string();
            skip = 0;
        }
        if first && separator && !token.text.starts_with(char::is_whitespace) {
            token.text.insert(0, ' ');
        }
        first = false;
        token.start += offset;
        token.end += offset;
        tokens.push(token);
    }
    (overlap, separator)
}

#[cfg(target_os = "macos")]
fn coreml_parakeet_file_result(
    model: &crate::coreml_asr::CoreMlParakeet,
    samples: &[f32],
    policy: rightkit_asr::parakeet_core::DetokPolicy,
) -> Result<TranscriptionResult, String> {
    let pieces = model.transcribe_pieces_timed_with_policy(samples, policy)?;
    Ok(TranscriptionResult {
        text: pieces
            .iter()
            .map(|p| p.text.as_str())
            .collect::<String>()
            .trim()
            .to_string(),
        tokens: pieces
            .into_iter()
            .map(|p| TimedToken {
                text: p.text,
                start: p.start,
                end: p.start + p.duration,
            })
            .collect(),
    })
}

#[cfg(target_os = "macos")]
fn coreml_parakeet_result(
    model: &crate::coreml_asr::CoreMlParakeet,
    samples: &[f32],
) -> Result<TranscriptionResult, String> {
    coreml_parakeet_result_with_frontend(model, samples, None)
}

#[cfg(target_os = "macos")]
fn coreml_parakeet_result_with_frontend(
    model: &crate::coreml_asr::CoreMlParakeet,
    samples: &[f32],
    frontend: Option<crate::coreml_asr::EncodedWindow>,
) -> Result<TranscriptionResult, String> {
    let pieces = match frontend {
        Some(frontend) => model.transcribe_pieces_timed_with_frontend(samples, frontend)?,
        None => model.transcribe_pieces_timed(samples)?,
    };
    let text = pieces.iter().map(|p| p.text.as_str()).collect::<String>();
    let tokens = pieces
        .into_iter()
        .map(|piece| TimedToken {
            text: piece.text,
            start: piece.start,
            // Real TDT-predicted duration, not a fixed placeholder: a synthetic
            // end lets `timed_tokens_to_words` overwrite it with the NEXT
            // word's start, so a word's cue runs through any pause after it.
            end: piece.start + piece.duration,
        })
        .collect();
    Ok(TranscriptionResult {
        text: text.trim().to_string(),
        tokens,
    })
}

#[cfg(target_os = "macos")]
fn coreml_parakeet_padded_result_with_lookahead(
    model: &mut crate::coreml_asr::CoreMlParakeet,
    samples: &[f32],
) -> Result<TranscriptionResult, String> {
    let window = padded_window_samples();
    let padding = ms_to_samples(PADDED_WINDOW_PADDING_MS);
    let target = window.saturating_sub(padding);
    let mut text = String::new();
    let mut tokens = Vec::new();
    let mut start = 0usize;
    let mut sequence = 0u64;
    let mut pending_sequence = None;

    while start < samples.len() {
        let window_end = (start + window).min(samples.len());
        if window_end >= samples.len() {
            break;
        }
        let target_end = (start + target).min(samples.len());
        let stable_end = quiet_cut_in(samples, target_end, window_end).unwrap_or(window_end);
        let segment: &[f32] = if start < stable_end {
            &samples[start..stable_end]
        } else {
            &[]
        };

        // Previous request ran while this window decoded. Taking it now is
        // the only blocking point, after the overlap opportunity has elapsed.
        let frontend = if pending_sequence == Some(sequence) {
            pending_sequence = None;
            match model.take_frontend_window(sequence) {
                Ok(encoded) => Some(encoded),
                Err(error) => {
                    tracing::warn!(
                        error,
                        "frontend lookahead take failed; retrying window synchronously"
                    );
                    model.disable_frontend_lookahead();
                    None
                }
            }
        } else {
            None
        };

        let next_start = stable_end.max(start + 1);
        if model.frontend_lookahead_enabled() && next_start < samples.len() {
            let next_window_end = (next_start + window).min(samples.len());
            let next_segment: std::borrow::Cow<'_, [f32]> = if next_window_end >= samples.len() {
                final_segment(&samples[next_start..], window)
            } else {
                let next_target_end = (next_start + target).min(samples.len());
                let next_stable_end = quiet_cut_in(samples, next_target_end, next_window_end)
                    .unwrap_or(next_window_end);
                if next_start < next_stable_end {
                    std::borrow::Cow::Borrowed(&samples[next_start..next_stable_end])
                } else {
                    std::borrow::Cow::Borrowed(&[])
                }
            };
            let next_sequence = sequence + 1;
            if let Err(error) = model.enqueue_frontend_window(next_sequence, next_segment.as_ref())
            {
                tracing::warn!(
                    error,
                    "frontend lookahead enqueue failed; continuing synchronously"
                );
                model.disable_frontend_lookahead();
            } else {
                pending_sequence = Some(next_sequence);
            }
        }

        let had_frontend = frontend.is_some();
        let result = match coreml_parakeet_result_with_frontend(model, segment, frontend) {
            Ok(result) => result,
            Err(error) if had_frontend => {
                tracing::warn!(
                    error,
                    "frontend lookahead decode failed; retrying window synchronously"
                );
                model.disable_frontend_lookahead();
                coreml_parakeet_result_with_frontend(model, segment, None)?
            }
            Err(error) => {
                model.disable_frontend_lookahead();
                return Err(error);
            }
        };
        append_timed_result(
            &mut text,
            &mut tokens,
            result,
            start as f32 / SAMPLE_RATE as f32,
        );
        start = next_start;
        sequence += 1;
    }

    if start < samples.len() {
        let frontend = if pending_sequence == Some(sequence) {
            match model.take_frontend_window(sequence) {
                Ok(encoded) => Some(encoded),
                Err(error) => {
                    tracing::warn!(
                        error,
                        "frontend lookahead final take failed; retrying synchronously"
                    );
                    model.disable_frontend_lookahead();
                    None
                }
            }
        } else {
            None
        };
        let segment = final_segment(&samples[start..], window);
        let had_frontend = frontend.is_some();
        let result = match coreml_parakeet_result_with_frontend(model, segment.as_ref(), frontend) {
            Ok(result) => result,
            Err(error) if had_frontend => {
                tracing::warn!(
                    error,
                    "frontend lookahead final decode failed; retrying synchronously"
                );
                model.disable_frontend_lookahead();
                coreml_parakeet_result_with_frontend(model, segment.as_ref(), None)?
            }
            Err(error) => {
                model.disable_frontend_lookahead();
                return Err(error);
            }
        };
        append_timed_result(
            &mut text,
            &mut tokens,
            result,
            start as f32 / SAMPLE_RATE as f32,
        );
    }

    Ok(TranscriptionResult {
        text: text.trim().to_string(),
        tokens,
    })
}

#[cfg(target_os = "macos")]
pub(crate) fn timed_tokens_to_words(tokens: &[TimedToken]) -> Vec<crate::coreml_asr::TimedTok> {
    let mut words: Vec<crate::coreml_asr::TimedTok> = Vec::new();
    let mut pending_boundary = false;
    for token in tokens {
        let starts_word = pending_boundary
            || token
                .text
                .chars()
                .next()
                .map(|ch| ch.is_whitespace())
                .unwrap_or(false);
        let clean = token.text.trim().to_string();
        if clean.is_empty() {
            pending_boundary = starts_word;
            continue;
        }
        pending_boundary = false;
        if starts_word || words.is_empty() {
            words.push(crate::coreml_asr::TimedTok {
                text: clean,
                start: token.start,
                end: token.end,
            });
        } else if let Some(last) = words.last_mut() {
            last.text.push_str(&clean);
            last.end = token.end;
        }
    }
    // Word ends come from the model's own durations. Two clamps only:
    // never shorter than one 80 ms frame (zero-width cues are invalid in SRT,
    // and a piece that shares a frame with the next reports 0), and never past
    // the next word's start (durations are per-token, so a chain can otherwise
    // overhang). Deliberately does NOT stretch each cue to the next word's
    // start: that erased real pauses and made word-level highlighting run on
    // through silence.
    for i in 0..words.len() {
        let floor = words[i].start + 0.08;
        let mut end = words[i].end.max(floor);
        if i + 1 < words.len() {
            end = end.min(words[i + 1].start.max(floor));
        }
        words[i].end = end;
    }
    words
}

#[cfg(target_os = "windows")]
#[derive(Clone, Debug)]
pub(super) struct FileTimedWord {
    pub(super) text: String,
    pub(super) start: f32,
    pub(super) end: f32,
}

#[cfg(target_os = "windows")]
pub(super) fn timed_tokens_to_file_words(tokens: &[TimedToken]) -> Vec<FileTimedWord> {
    let mut words = Vec::new();
    let mut pending_boundary = false;
    for token in tokens {
        let starts_word =
            pending_boundary || token.text.chars().next().is_some_and(char::is_whitespace);
        let clean = token.text.trim();
        if clean.is_empty() {
            pending_boundary = starts_word;
            continue;
        }
        pending_boundary = false;
        if starts_word || words.is_empty() {
            words.push(FileTimedWord {
                text: clean.to_string(),
                start: token.start,
                end: token.end,
            });
        } else if let Some(last) = words.last_mut() {
            last.text.push_str(clean);
            last.end = token.end;
        }
    }
    for i in 0..words.len() {
        let floor = words[i].start + 0.08;
        let mut end = words[i].end.max(floor);
        if let Some(next) = words.get(i + 1) {
            end = end.min(next.start.max(floor));
        }
        words[i].end = end;
    }
    words
}

#[cfg(target_os = "windows")]
pub(super) fn render_file_subtitles(words: &[FileTimedWord]) -> (String, String) {
    crate::subtitle_export::render_srt_vtt(words.iter().map(|w| (w.text.clone(), w.start, w.end)))
}
