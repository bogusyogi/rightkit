//! Decode an input's audio to interleaved `f32` or `i16` samples held in memory.
//!
//! The whole decoded output is buffered (`run_capture_with` reads stdout to the end), so
//! set [`DecodeOptions::with_max_duration`] for bounded analysis windows. Streaming
//! consumers should use [`crate::runner::spawn_streaming`] instead.

use std::{io, path::Path, time::Duration};

use crate::{
    runner::{run_checked_with, RunError, RunOptions},
    CancellationToken, FfmpegBinary, FfmpegCommand,
};

/// Output shape and run policy for [`decode_pcm_f32`].
///
/// [`Default`] leaves `sample_rate` and `channels` at zero, which decoding refuses; set
/// both explicitly. Use [`DecodeOptions::with_max_duration`] and
/// [`DecodeOptions::with_audio_stream_index`] for the two optional selection bounds.
#[derive(Clone, Debug)]
pub struct DecodeOptions {
    /// Output sample rate in Hz; must be non-zero.
    pub sample_rate: u32,
    /// Output channel count; must be non-zero. Samples are interleaved.
    pub channels: u32,
    pub cancel: CancellationToken,
    /// Deadline from spawn. `None` means no deadline; cancellation still applies.
    pub timeout: Option<Duration>,
    /// Maximum output duration (`-t`, after input), independent of wall-clock `timeout`.
    /// Set with [`DecodeOptions::with_max_duration`].
    pub max_duration: Option<Duration>,
    /// Zero-based audio stream ordinal in input 0 (`-map 0:a:N`). Set with
    /// [`DecodeOptions::with_audio_stream_index`].
    pub audio_stream_index: Option<u32>,
}

impl Default for DecodeOptions {
    fn default() -> Self {
        Self {
            sample_rate: 0,
            channels: 0,
            cancel: CancellationToken::new(),
            timeout: None,
            max_duration: None,
            audio_stream_index: None,
        }
    }
}

impl DecodeOptions {
    /// Bound the output to `duration` from its start (`-t`); `None` decodes to EOF.
    /// Must be at least one microsecond; sub-microsecond fractions are truncated to
    /// FFmpeg's microsecond precision. Decoding refuses shorter durations.
    #[must_use]
    pub fn with_max_duration(mut self, duration: Option<Duration>) -> Self {
        self.max_duration = duration;
        self
    }

    /// Select the zero-based audio stream ordinal in input 0 (`-map 0:a:N`), not an
    /// absolute stream index. `None` keeps FFmpeg's automatic audio selection. An
    /// explicitly selected stream must exist or decoding fails.
    #[must_use]
    pub fn with_audio_stream_index(mut self, index: Option<u32>) -> Self {
        self.audio_stream_index = index;
        self
    }
}

/// Sample layout the decoder asks FFmpeg for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sample {
    /// `f32le`, the layout of [`decode_pcm_f32`].
    F32,
    /// `pcm_s16le`, the layout of [`decode_pcm_s16`].
    S16,
}

/// Shared input selection, output duration, rate & channel policy for both layouts.
fn decode_command(input: &Path, options: &DecodeOptions, sample: Sample) -> FfmpegCommand {
    let mut cmd = FfmpegCommand::base("error");
    cmd.input(input);
    if let Some(index) = options.audio_stream_index {
        cmd.texts(["-map".to_string(), format!("0:a:{index}")]);
    }
    if let Some(duration) = options.max_duration {
        cmd.texts([
            "-t".to_string(),
            format!("{}.{:06}", duration.as_secs(), duration.subsec_micros()),
        ]);
    }
    cmd.text("-vn").texts([
        "-ac".to_string(),
        options.channels.to_string(),
        "-ar".to_string(),
        options.sample_rate.to_string(),
    ]);
    match sample {
        Sample::F32 => cmd.texts(["-f", "f32le"]),
        Sample::S16 => cmd.texts(["-c:a", "pcm_s16le", "-f", "s16le"]),
    };
    cmd.text("pipe:1");
    cmd
}

/// Run the decode and return the raw stdout bytes, after the format checks.
fn decode_bytes(
    binary: &dyn FfmpegBinary,
    input: &Path,
    options: &DecodeOptions,
    sample: Sample,
) -> Result<Vec<u8>, RunError> {
    if options.sample_rate == 0 || options.channels == 0 {
        return Err(RunError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "sample rate and channel count must be non-zero",
        )));
    }
    if options
        .max_duration
        .is_some_and(|duration| duration < Duration::from_micros(1))
    {
        return Err(RunError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "maximum decode duration must be at least one microsecond",
        )));
    }
    let cmd = decode_command(input, options, sample);
    let run = RunOptions::unbounded(options.cancel.clone()).with_deadline(options.timeout);
    let captured = run_checked_with(binary, &cmd, run)?;
    Ok(captured.stdout)
}

/// Decode audio from `input` to `f32` little-endian PCM at `options.sample_rate`
/// & `options.channels`, returned as interleaved samples. The stream selection and
/// duration bound set through the builders apply; by default FFmpeg selects the audio
/// stream and decoding runs to EOF. Video is dropped (`-vn`). Works with any
/// [`FfmpegBinary`], catalog-verified or development.
pub fn decode_pcm_f32(
    binary: &dyn FfmpegBinary,
    input: &Path,
    options: &DecodeOptions,
) -> Result<Vec<f32>, RunError> {
    let bytes = decode_bytes(binary, input, options, Sample::F32)?;
    if bytes.len() % 4 != 0 {
        return Err(RunError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "decoded output is not a whole number of f32 samples",
        )));
    }
    Ok(bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| f32::from_le_bytes(*bytes))
        .collect())
}

/// [`decode_pcm_f32`] with signed 16-bit little-endian samples (`pcm_s16le`), returned as
/// interleaved `i16`. The format, rate, channel, and `-vn` handling are the same as
/// [`decode_pcm_f32`]; only the sample layout differs. It returns the samples alone, as
/// [`decode_pcm_f32`] does.
pub fn decode_pcm_s16(
    binary: &dyn FfmpegBinary,
    input: &Path,
    options: &DecodeOptions,
) -> Result<Vec<i16>, RunError> {
    let bytes = decode_bytes(binary, input, options, Sample::S16)?;
    if bytes.len() % 2 != 0 {
        return Err(RunError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "decoded output is not a whole number of 16-bit samples",
        )));
    }
    Ok(bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|bytes| i16::from_le_bytes(*bytes))
        .collect())
}
