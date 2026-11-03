use std::cell::RefCell;
use std::sync::{Mutex, OnceLock};

use serde::Serialize;

/// Content-free per-window CoreML decoder health for collapse detection.
///
/// This deliberately excludes raw PCM, VAD decisions, token IDs, transcripts,
/// and bias phrases. `conditioned_audio_rms_quarters` is calculated from the
/// already-conditioned ASR input owned by this decoder boundary.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct TensorHealth {
    pub rms: f32,
    pub min: f32,
    pub max: f32,
    pub nonfinite: usize,
}

/// Content-free silent span in decoder-frame coordinates (80 ms per frame).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct NoEmissionGap {
    pub frame_start: usize,
    pub frame_len: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct AsrWindowStats {
    pub frames_total: usize,
    pub joint_calls: usize,
    pub emitted_tokens: usize,
    pub blank_steps: usize,
    pub forced_advances: usize,
    pub duration_mean_frames: f32,
    pub duration_max_frames: usize,
    pub duration_frames_skipped: usize,
    /// Positional >2 s gaps, kept in decoder-frame coordinates for quarter alignment.
    pub no_emission_gaps_over_2s: Vec<NoEmissionGap>,
    pub top1_top2_margin_mean: f32,
    pub near_tie_count: usize,
    pub bias_installed_count: usize,
    pub bias_applied_count: usize,
    pub conditioned_audio_rms_quarters: [f32; 4],
    pub mel_rms: f32,
    pub mel_min: f32,
    pub mel_max: f32,
    pub mel_nonfinite: usize,
    pub mel_quarters: [TensorHealth; 4],
    pub encoder_rms: f32,
    pub encoder_min: f32,
    pub encoder_max: f32,
    pub encoder_nonfinite: usize,
    pub encoder_quarters: [TensorHealth; 4],
}

/// Capture-side facts scoped to one CoreML decoder call. No samples, VAD
/// scores, tokens, or transcript text escape this context.
#[derive(Clone, Debug)]
pub struct AsrWindowInput {
    pub raw_pcm_rms_quarters: Option<[f32; 4]>,
    pub recording_id: Option<String>,
    pub window_index: Option<u64>,
    pub window_type: Option<&'static str>,
    pub window_start_offset_samples: Option<usize>,
    pub vad_speech_state_quarters: [Option<&'static str>; 4],
    pub vad_speech_ratio_quarters: [Option<f32>; 4],
    pub vad_speech_frame_count: Option<usize>,
}

impl AsrWindowInput {
    pub fn from_raw_pcm(raw_pcm: &[f32], window_type: Option<&'static str>) -> Self {
        Self {
            raw_pcm_rms_quarters: Some(quarter_rms(raw_pcm)),
            recording_id: None,
            window_index: None,
            window_type,
            window_start_offset_samples: None,
            vad_speech_state_quarters: [None; 4],
            vad_speech_ratio_quarters: [None; 4],
            vad_speech_frame_count: None,
        }
    }

    fn unavailable() -> Self {
        Self {
            raw_pcm_rms_quarters: None,
            recording_id: None,
            window_index: None,
            window_type: None,
            window_start_offset_samples: None,
            vad_speech_state_quarters: [None; 4],
            vad_speech_ratio_quarters: [None; 4],
            vad_speech_frame_count: None,
        }
    }
}

thread_local! {
    static WINDOW_INPUT: RefCell<Option<AsrWindowInput>> = const { RefCell::new(None) };
}

pub struct AsrWindowInputScope(Option<AsrWindowInput>);

impl Drop for AsrWindowInputScope {
    fn drop(&mut self) {
        WINDOW_INPUT.with(|slot| *slot.borrow_mut() = self.0.take());
    }
}

pub fn install_asr_window_input(input: AsrWindowInput) -> AsrWindowInputScope {
    let previous = WINDOW_INPUT.with(|slot| slot.borrow_mut().replace(input));
    AsrWindowInputScope(previous)
}

fn current_asr_window_input() -> AsrWindowInput {
    WINDOW_INPUT.with(|slot| {
        slot.borrow()
            .clone()
            .unwrap_or_else(AsrWindowInput::unavailable)
    })
}

/// Versioned outbox payload for a content-free detailed-telemetry window.
#[derive(Serialize)]
pub struct AsrWindowStatsEnvelope<'a> {
    pub schema_version: u8,
    pub event: &'static str,
    pub model_fingerprint: &'a str,
    pub raw_pcm_rms_quarters: Option<[f32; 4]>,
    pub recording_id: Option<String>,
    pub window_index: Option<u64>,
    pub window_type: Option<&'a str>,
    pub window_start_offset_samples: Option<usize>,
    pub vad_speech_state_quarters: [Option<&'static str>; 4],
    pub vad_speech_ratio_quarters: [Option<f32>; 4],
    pub vad_speech_frame_count: Option<usize>,
    pub coreml_compile_cache: &'a str,
    pub thermal_state: Option<&'static str>,
    pub low_power_mode: Option<bool>,
    pub model_dir_sha256: Option<&'a str>,
    pub mel_compute: &'a str,
    pub joint_compute: &'a str,
    pub stats: &'a AsrWindowStats,
    pub encoder_compute: &'a str,
    pub decoder_compute: &'a str,
    pub elapsed_ms: u64,
}

impl<'a> AsrWindowStatsEnvelope<'a> {
    pub(crate) fn new(
        stats: &'a AsrWindowStats,
        model_fingerprint: &'a str,
        model_dir_sha256: Option<&'a str>,
        coreml_compile_cache: &'a str,
        mel_compute: &'a str,
        joint_compute: &'a str,
        encoder_compute: &'a str,
        decoder_compute: &'a str,
        elapsed_ms: u64,
    ) -> Self {
        let input = current_asr_window_input();
        Self {
            schema_version: 1,
            event: "asr_window_stats",
            model_fingerprint,
            raw_pcm_rms_quarters: input.raw_pcm_rms_quarters,
            recording_id: input.recording_id,
            window_index: input.window_index,
            window_type: input.window_type,
            window_start_offset_samples: input.window_start_offset_samples,
            vad_speech_state_quarters: input.vad_speech_state_quarters,
            vad_speech_ratio_quarters: input.vad_speech_ratio_quarters,
            vad_speech_frame_count: input.vad_speech_frame_count,
            coreml_compile_cache,
            thermal_state: None,
            low_power_mode: None,
            model_dir_sha256,
            mel_compute,
            joint_compute,
            stats,
            encoder_compute,
            decoder_compute,
            elapsed_ms,
        }
    }
}

/// Inputs intentionally unavailable at `decode_window` boundary. Upstream
/// telemetry must provide these if a watchdog needs them; decoder never
/// reconstructs them from conditioned audio.
pub const ASR_WINDOW_STATS_UPSTREAM_INPUTS_NEEDED: &[&str] = &[
    "vad_speech_state_quarters",
    "vad_speech_ratio_quarters",
    "vad_speech_frame_count",
    "recording_id",
    "window_index",
    "window_start_offset_samples",
];

static LATEST_ASR_WINDOW_STATS: OnceLock<Mutex<Option<AsrWindowStats>>> = OnceLock::new();
#[derive(Clone, Debug, Serialize)]
pub struct CalibrationWindowStats {
    pub window_index: usize,
    pub window_start_sec: f32,
    pub window_start_offset_samples: usize,
    pub window_type: &'static str,
    pub stats: AsrWindowStats,
}

static CALIBRATION_WINDOW_STATS: OnceLock<Mutex<Vec<CalibrationWindowStats>>> = OnceLock::new();

fn latest_slot() -> &'static Mutex<Option<AsrWindowStats>> {
    LATEST_ASR_WINDOW_STATS.get_or_init(|| Mutex::new(None))
}

pub(crate) fn publish_asr_window_stats(stats: AsrWindowStats) {
    *latest_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(stats);
}

pub(crate) fn publish_calibration_window_stats(stats: AsrWindowStats, window_start_sec: f32) {
    if !crate::settings::onboarding_calibration_active() {
        return;
    }
    let mut collected = CALIBRATION_WINDOW_STATS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if collected.len() < 8 {
        let index = collected.len();
        collected.push(CalibrationWindowStats {
            window_index: index,
            window_start_sec,
            window_start_offset_samples: (window_start_sec * 16_000.0) as usize,
            window_type: "final",
            stats,
        });
    }
}

pub fn take_calibration_window_stats() -> Vec<CalibrationWindowStats> {
    std::mem::take(
        &mut *CALIBRATION_WINDOW_STATS
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    )
}

pub fn clear_calibration_window_stats() {
    CALIBRATION_WINDOW_STATS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

/// Stable recovery/watchdog API. Returns a clone so callers never hold decoder
/// state or a telemetry lock while processing a report.
pub fn latest_asr_window_stats() -> Option<AsrWindowStats> {
    latest_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Remove & return one decoder report. Recovery clears this before each route
/// so an error cannot inherit health from an earlier window.
pub fn take_latest_asr_window_stats() -> Option<AsrWindowStats> {
    latest_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
}

/// Clears prior health before a decoder attempt. A failed attempt can therefore
/// never leave a prior window's report available to a watchdog.
pub fn clear_latest_asr_window_stats() {
    *latest_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

pub(crate) fn quarter_rms(values: &[f32]) -> [f32; 4] {
    std::array::from_fn(|quarter| {
        let start = values.len() * quarter / 4;
        let end = values.len() * (quarter + 1) / 4;
        let mut sum_squares = 0.0f64;
        let mut finite = 0usize;
        for value in values[start..end]
            .iter()
            .copied()
            .filter(|value| value.is_finite())
        {
            sum_squares += f64::from(value) * f64::from(value);
            finite += 1;
        }
        if finite == 0 {
            0.0
        } else {
            (sum_squares / finite as f64).sqrt() as f32
        }
    })
}

pub(crate) fn tensor_health(values: &[f32]) -> TensorHealth {
    let mut finite = 0usize;
    let mut nonfinite = 0usize;
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    let mut sum_squares = 0.0f64;
    for value in values.iter().copied() {
        if !value.is_finite() {
            nonfinite += 1;
            continue;
        }
        finite += 1;
        min = min.min(value);
        max = max.max(value);
        sum_squares += f64::from(value) * f64::from(value);
    }
    if finite == 0 {
        return TensorHealth {
            nonfinite,
            ..TensorHealth::default()
        };
    }
    TensorHealth {
        rms: (sum_squares / finite as f64).sqrt() as f32,
        min,
        max,
        nonfinite,
    }
}

pub(crate) fn quartered_tensor_health(values: &[f32]) -> [TensorHealth; 4] {
    std::array::from_fn(|quarter| {
        let start = values.len() * quarter / 4;
        let end = values.len() * (quarter + 1) / 4;
        tensor_health(&values[start..end])
    })
}

pub(crate) fn no_emission_gaps_over_2s(
    hits: &[(usize, f32, usize)],
    win_start_sec: f32,
    frames_total: usize,
) -> Vec<NoEmissionGap> {
    const FRAME_SECONDS: f32 = 0.08;
    const GAP_THRESHOLD_SECONDS: f32 = 2.0;
    let mut gaps = Vec::new();
    let mut prior_end_frame = 0usize;
    for &(_, start_sec, duration_frames) in hits {
        let start_frame = ((start_sec - win_start_sec) / FRAME_SECONDS)
            .round()
            .max(0.0) as usize;
        record_gap(
            &mut gaps,
            prior_end_frame,
            start_frame.saturating_sub(prior_end_frame),
        );
        prior_end_frame = start_frame.saturating_add(duration_frames);
    }
    record_gap(
        &mut gaps,
        prior_end_frame,
        frames_total.saturating_sub(prior_end_frame),
    );
    gaps.retain(|gap| gap.frame_len as f32 * FRAME_SECONDS > GAP_THRESHOLD_SECONDS);
    gaps
}

fn record_gap(gaps: &mut Vec<NoEmissionGap>, frame_start: usize, frame_len: usize) {
    gaps.push(NoEmissionGap {
        frame_start,
        frame_len,
    });
}

pub(crate) fn top1_top2_margin(
    logits: &[f32],
    bonuses: &std::collections::HashMap<usize, f32>,
) -> Option<f32> {
    let mut top1 = f32::NEG_INFINITY;
    let mut top2 = f32::NEG_INFINITY;
    for (index, logit) in logits.iter().copied().enumerate() {
        let score = logit + bonuses.get(&index).copied().unwrap_or(0.0);
        if !score.is_finite() {
            continue;
        }
        if score > top1 {
            top2 = top1;
            top1 = score;
        } else if score > top2 {
            top2 = score;
        }
    }
    top2.is_finite().then_some(top1 - top2)
}
