//! Decode an input's audio to interleaved `f32` or `i16` samples held in memory.
//!
//! The whole decoded output is buffered (`run_capture_with` reads stdout to the end), so
//! use it for clips and analysis windows, not for hours of audio in one call. Streaming
//! consumers should use [`crate::runner::spawn_streaming`] instead.

use std::{io, path::Path, time::Duration};

use crate::{
    runner::{run_checked_with, RunError, RunOptions},
    CancellationToken, FfmpegBinary, FfmpegCommand,
};

/// Output shape and run policy for [`decode_pcm_f32`].
#[derive(Clone, Debug)]
pub struct DecodeOptions {
    /// Output sample rate in Hz; must be non-zero.
    pub sample_rate: u32,
    /// Output channel count; must be non-zero. Samples are interleaved.
    pub channels: u32,
    pub cancel: CancellationToken,
    /// Deadline from spawn. `None` means no deadline; cancellation still applies.
    pub timeout: Option<Duration>,
}

/// Sample layout the decoder asks FFmpeg for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sample {
    /// `f32le`, the layout of [`decode_pcm_f32`].
    F32,
    /// `pcm_s16le`, the layout of [`decode_pcm_s16`].
    S16,
}

/// The FFmpeg argument vector for a decode. `F32` is unchanged from the original decoder;
/// `S16` differs only by the codec and the `s16le` container.
fn decode_command(input: &Path, options: &DecodeOptions, sample: Sample) -> FfmpegCommand {
    let mut cmd = FfmpegCommand::base("error");
    cmd.input(input).text("-vn").texts([
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
    let cmd = decode_command(input, options, sample);
    let run = RunOptions::unbounded(options.cancel.clone()).with_deadline(options.timeout);
    let captured = run_checked_with(binary, &cmd, run)?;
    Ok(captured.stdout)
}

/// Decode the first audio stream of `input` to `f32` little-endian PCM at
/// `options.sample_rate` and `options.channels`, returned as interleaved samples. Video is
/// dropped (`-vn`). Works with any [`FfmpegBinary`], catalog-verified or development.
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

#[cfg(test)]
mod argv_tests {
    use super::*;

    fn argv(cmd: &FfmpegCommand) -> Vec<String> {
        cmd.to_os_args()
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn options(sample_rate: u32, channels: u32) -> DecodeOptions {
        DecodeOptions {
            sample_rate,
            channels,
            cancel: CancellationToken::new(),
            timeout: None,
        }
    }

    const BASE: [&str; 6] = [
        "-hide_banner",
        "-nostdin",
        "-y",
        "-loglevel",
        "error",
        "-nostats",
    ];

    #[test]
    fn f32_argv_is_the_original_decode_argument_vector() {
        let cmd = decode_command(Path::new("in.wav"), &options(16_000, 1), Sample::F32);
        let expected: Vec<&str> = BASE
            .iter()
            .copied()
            .chain([
                "-i", "in.wav", "-vn", "-ac", "1", "-ar", "16000", "-f", "f32le", "pipe:1",
            ])
            .collect();
        assert_eq!(argv(&cmd), expected);
    }

    #[test]
    fn s16_argv_differs_from_f32_only_by_codec_and_container() {
        let cmd = decode_command(Path::new("in.wav"), &options(16_000, 1), Sample::S16);
        let expected: Vec<&str> = BASE
            .iter()
            .copied()
            .chain([
                "-i",
                "in.wav",
                "-vn",
                "-ac",
                "1",
                "-ar",
                "16000",
                "-c:a",
                "pcm_s16le",
                "-f",
                "s16le",
                "pipe:1",
            ])
            .collect();
        assert_eq!(argv(&cmd), expected);
    }

    #[test]
    fn the_s16_argv_carries_the_requested_rate_and_channels() {
        let cmd = decode_command(Path::new("a b.mp4"), &options(48_000, 2), Sample::S16);
        let args = argv(&cmd);
        let at = |flag: &str| {
            args.iter()
                .position(|a| a == flag)
                .map(|i| args[i + 1].clone())
        };
        assert_eq!(at("-ar").as_deref(), Some("48000"));
        assert_eq!(at("-ac").as_deref(), Some("2"));
        assert_eq!(at("-i").as_deref(), Some("a b.mp4"));
    }
}

#[cfg(test)]
mod s16_tests {
    use super::*;

    fn options(sample_rate: u32, channels: u32) -> DecodeOptions {
        DecodeOptions {
            sample_rate,
            channels,
            cancel: CancellationToken::new(),
            timeout: Some(Duration::from_secs(60)),
        }
    }

    /// A minimal 16-bit PCM WAV, so the round-trip test needs no fixture file.
    fn pcm_s16_wav(rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
        let data_len = (samples.len() * 2) as u32;
        let mut out = Vec::with_capacity(44 + data_len as usize);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
        out.extend_from_slice(&(channels * 2).to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        for s in samples {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }

    #[test]
    fn zero_formats_are_refused_before_ffmpeg_runs() {
        let missing = crate::DevFfmpeg::unverified("definitely-not-a-binary");
        let refused = decode_pcm_s16(&missing, Path::new("in.wav"), &options(0, 1));
        assert!(matches!(refused, Err(RunError::Io(e)) if e.kind() == io::ErrorKind::InvalidInput));
    }

    #[test]
    fn s16_decode_round_trips_pcm_when_an_ffmpeg_is_on_the_path() {
        let present = std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !present {
            crate::test_support::skip_missing(
                "FFmpeg on PATH for the s16 round-trip",
                "PATH (ffmpeg -version)",
            );
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("ramp.wav");
        let samples: [i16; 4] = [0, 1000, -1000, i16::MAX];
        std::fs::write(&wav, pcm_s16_wav(16_000, 1, &samples)).unwrap();
        let ffmpeg = crate::DevFfmpeg::unverified("ffmpeg");
        let decoded = decode_pcm_s16(&ffmpeg, &wav, &options(16_000, 1)).unwrap();
        assert_eq!(decoded, samples.to_vec());
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::DevFfmpeg;

    fn fake(dir: &std::path::Path, body: &str) -> DevFfmpeg {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-ffmpeg");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        DevFfmpeg::unverified(path)
    }

    fn options(sample_rate: u32, channels: u32) -> DecodeOptions {
        DecodeOptions {
            sample_rate,
            channels,
            cancel: CancellationToken::new(),
            timeout: Some(Duration::from_secs(20)),
        }
    }

    #[test]
    fn decoded_bytes_become_little_endian_f32_samples_and_the_arguments_request_them() {
        let dir = tempfile::tempdir().unwrap();
        // Refuses to answer unless the argument vector asks for raw f32 on stdout.
        let bin = fake(
            dir.path(),
            r#"case " $* " in *" f32le "*) ;; *) exit 3;; esac
case " $* " in *" pipe:1 "*) ;; *) exit 4;; esac
case " $* " in *" -vn "*) ;; *) exit 5;; esac
printf '\000\000\200\077\000\000\000\300'"#, // 1.0f32 then -2.0f32, little-endian
        );
        let samples = decode_pcm_f32(&bin, Path::new("in.wav"), &options(16_000, 1)).unwrap();
        assert_eq!(samples, vec![1.0, -2.0]);
    }

    #[test]
    fn a_partial_sample_is_an_error_and_zero_formats_are_refused_up_front() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake(dir.path(), "printf 'abc'");
        let result = decode_pcm_f32(&bin, Path::new("in.wav"), &options(16_000, 1));
        assert!(matches!(result, Err(RunError::Io(e)) if e.kind() == io::ErrorKind::InvalidData));
        let refused = decode_pcm_f32(&bin, Path::new("in.wav"), &options(0, 1));
        assert!(matches!(refused, Err(RunError::Io(e)) if e.kind() == io::ErrorKind::InvalidInput));
        assert!(decode_pcm_f32(&bin, Path::new("in.wav"), &options(16_000, 0)).is_err());
    }
}
