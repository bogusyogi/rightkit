//! Typed external-microphone decode and waveform alignment.
//!
//! FFmpeg is used only as a typed PCM decoder. Alignment itself is deterministic
//! Rust math, with source/sample time converted to canonical timeline ticks.

use std::path::Path;
use std::time::Duration;

use rightkit_audio::{align_mic, AlignParams, AudioError};
use rightkit_ffmpeg as rk;
use rightkit_timeline::time::round_div_half_down;
use rightkit_timeline::{Ticks, TICKS_PER_SECOND};

use crate::error::ProjectError;
use crate::tools::MediaTools;

/// Bounds the PCM decode. The decode is already capped by `MAX_DECODE_SECONDS`, so this
/// only stops a hung decoder.
const DECODE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

pub const SAMPLE_RATE: u32 = 16_000;
const MAX_DECODE_SECONDS: u32 = 180;
const MIN_OVERLAP_SECONDS: u32 = 1;
/// A longer reference includes speech after leading camera silence.
const SEARCH_WINDOW_MS: f64 = 30_000.0;
/// Minimum normalized correlation accepted as an alignment (the former peak floor).
const MIN_CONFIDENCE: f64 = 0.30;

#[derive(Debug, Clone, PartialEq)]
pub struct AudioAlignment {
    /// Microphone source time minus camera source time for matching samples.
    pub offset: Ticks,
    pub lag_samples: i64,
    pub sample_rate: u32,
    pub confidence: f64,
    pub overlap_samples: usize,
}

/// Decode first audio stream as finite, mono, little-endian f32 PCM.
pub fn decode_pcm(tools: &MediaTools, file: &Path) -> Result<Vec<f32>, ProjectError> {
    let pcm = rk::decode::decode_pcm_f32(
        &rk::verify_ffmpeg_or_development(
            &tools.ffmpeg,
            rk::EXPORT_ASS_ZIMG_VARIANT,
            crate::tools::VERIFY_TIMEOUT,
        )
        .or_else(|_| {
            rk::verify_ffmpeg_or_development(
                &tools.ffmpeg,
                rk::EXPORT_ASS_VARIANT,
                crate::tools::VERIFY_TIMEOUT,
            )
        })
        .map_err(|e| ProjectError::MediaToolMissing(e.to_string()))?,
        file,
        &rk::decode::DecodeOptions {
            sample_rate: SAMPLE_RATE,
            channels: 1,
            max_duration: Some(Duration::from_secs(u64::from(MAX_DECODE_SECONDS))),
            audio_stream_index: Some(0),
            cancel: rk::CancellationToken::new(),
            timeout: Some(DECODE_TIMEOUT),
        },
    )
    .map_err(|e| match e {
        rk::RunError::Io(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            ProjectError::AudioDecodeFailed(e.to_string())
        }
        rk::RunError::Spawn(e) | rk::RunError::Io(e) => {
            ProjectError::AudioDecodeFailed(format!("cannot run {}: {e}", tools.ffmpeg.display()))
        }
        rk::RunError::Failed { stderr_tail, .. } => {
            ProjectError::AudioDecodeFailed(stderr_tail.trim().to_string())
        }
        other => ProjectError::AudioDecodeFailed(other.to_string()),
    })?;
    if pcm.iter().any(|sample| !sample.is_finite()) {
        return Err(ProjectError::AudioDecodeFailed(
            "decoder returned non-finite PCM".into(),
        ));
    }
    Ok(pcm)
}

/// Align decoded signals through RightKit, convert lag to canonical ticks, and map errors.
///
/// Positive lag means microphone time is later than camera time (the same convention as
/// `align_mic`).
pub fn align_pcm(
    camera: &[f32],
    microphone: &[f32],
    max_offset_ms: u64,
) -> Result<AudioAlignment, ProjectError> {
    if camera.len() < SAMPLE_RATE as usize * MIN_OVERLAP_SECONDS as usize
        || microphone.len() < SAMPLE_RATE as usize * MIN_OVERLAP_SECONDS as usize
    {
        return Err(ProjectError::AudioSignalUnavailable(
            "each source needs at least one second of audio".into(),
        ));
    }
    let params = AlignParams {
        max_lag_ms: max_offset_ms.min(MAX_DECODE_SECONDS as u64 * 1000) as f64,
        window_ms: SEARCH_WINDOW_MS,
        min_ncc: MIN_CONFIDENCE,
    };
    let found = align_mic(camera, microphone, SAMPLE_RATE, &params).map_err(|e| match e {
        AudioError::Invalid(message) => ProjectError::AudioSignalUnavailable(message),
        other => ProjectError::AudioSignalUnavailable(other.to_string()),
    })?;
    let lag = found.lag_samples;
    let window =
        ((params.window_ms * SAMPLE_RATE as f64 / 1000.0).round() as usize).min(camera.len());
    let camera_start = (-lag).max(0) as usize;
    let camera_end = (microphone.len() as i128 - lag as i128).clamp(0, window as i128) as usize;
    let overlap_samples = camera_end.saturating_sub(camera_start);
    let offset = Ticks(round_div_half_down(
        lag as i128 * TICKS_PER_SECOND as i128,
        SAMPLE_RATE as i128,
    ) as i64);
    Ok(AudioAlignment {
        offset,
        lag_samples: lag,
        sample_rate: SAMPLE_RATE,
        confidence: found.confidence,
        overlap_samples,
    })
}

pub fn align_files(
    tools: &MediaTools,
    camera: &Path,
    microphone: &Path,
    max_offset_ms: u64,
) -> Result<AudioAlignment, ProjectError> {
    let camera_pcm = decode_pcm(tools, camera)?;
    let microphone_pcm = decode_pcm(tools, microphone)?;
    align_pcm(&camera_pcm, &microphone_pcm, max_offset_ms)
}
