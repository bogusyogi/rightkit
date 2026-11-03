//! Typed FFmpeg argument vectors and the binary handle the runner executes.
//!
//! Donor: CutRight `cutright-timeline/src/ffmpeg.rs` (`FfmpegCommand`/`FfmpegArg`).
//! Arguments stay `OsString`-faithful (paths are never lossy-joined into text) and
//! are never passed through a shell.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::VerifiedFfmpeg;

/// One FFmpeg argument: plain text or a filesystem path (kept as `OsString`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FfmpegArg {
    Text(String),
    Path(PathBuf),
}

/// Video filter chain from [`FfmpegCommand::h264_compatible_input`].
pub const H264_COMPATIBLE_VF: &str = "scale=trunc(iw/2)*2:trunc(ih/2)*2,format=yuv420p";

/// An ordered FFmpeg argument vector.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FfmpegCommand {
    args: Vec<FfmpegArg>,
    /// The runner connects the child's stdin to a writer the caller supplies.
    stdin_piped: bool,
}

impl FfmpegCommand {
    pub fn new() -> Self {
        Self::default()
    }

    /// Migration adapter for callers that still build untyped argument vectors. The
    /// arguments are taken verbatim (no validation, no `-nostdin` handling beyond
    /// [`FfmpegCommand::stdin_piped`]); prefer the typed builders for new code.
    pub fn raw_args(args: Vec<String>) -> Self {
        let mut c = Self::new();
        c.texts(args);
        c
    }

    /// `-hide_banner -nostdin -y -loglevel <level> -nostats`: the baseline every
    /// non-interactive Right Suite invocation starts from. It adds `-y`, so an existing
    /// output file is overwritten without a prompt.
    pub fn base(loglevel: &str) -> Self {
        let mut c = Self::new();
        c.texts([
            "-hide_banner",
            "-nostdin",
            "-y",
            "-loglevel",
            loglevel,
            "-nostats",
        ]);
        c
    }

    pub fn text(&mut self, value: impl Into<String>) -> &mut Self {
        self.args.push(FfmpegArg::Text(value.into()));
        self
    }

    pub fn texts<I, S>(&mut self, values: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for v in values {
            self.text(v);
        }
        self
    }

    pub fn path(&mut self, value: impl AsRef<Path>) -> &mut Self {
        self.args
            .push(FfmpegArg::Path(value.as_ref().to_path_buf()));
        self
    }

    /// `-i <path>`.
    pub fn input(&mut self, value: impl AsRef<Path>) -> &mut Self {
        self.text("-i").path(value)
    }

    /// Machine-readable progress on stdout (`-progress pipe:1`, one block per 250 ms).
    pub fn progress_pipe(&mut self) -> &mut Self {
        self.texts(["-progress", "pipe:1", "-stats_period", "0.25"])
    }

    /// Connect the child's stdin to a caller-supplied writer (see
    /// `runner::run_with_stdin`). Removes every `-nostdin` token, because that flag
    /// stops FFmpeg from reading stdin at all. `false` only clears the piped flag; it
    /// does not re-add `-nostdin` (use [`FfmpegCommand::base`] for that).
    pub fn stdin_piped(&mut self, piped: bool) -> &mut Self {
        self.stdin_piped = piped;
        if piped {
            self.args
                .retain(|arg| !matches!(arg, FfmpegArg::Text(t) if t == "-nostdin"));
        }
        self
    }

    pub fn stdin_is_piped(&self) -> bool {
        self.stdin_piped
    }

    /// Append the H.264-safe video filter: width and height rounded down to even numbers and
    /// pixel format `yuv420p`. Call it after the input, before the output and codec options.
    /// It drops at most one pixel per odd dimension.
    pub fn h264_compatible_input(&mut self) -> &mut Self {
        self.text("-vf").text(H264_COMPATIBLE_VF)
    }

    pub fn args(&self) -> &[FfmpegArg] {
        &self.args
    }

    pub fn to_os_args(&self) -> Vec<OsString> {
        self.args
            .iter()
            .map(|a| match a {
                FfmpegArg::Text(t) => OsString::from(t),
                FfmpegArg::Path(p) => p.as_os_str().to_os_string(),
            })
            .collect()
    }

    /// Human-readable rendering for logs only (lossy, not shell-safe).
    pub fn display_lossy(&self) -> String {
        self.args
            .iter()
            .map(|a| match a {
                FfmpegArg::Text(t) => t.clone(),
                FfmpegArg::Path(p) => p.to_string_lossy().into_owned(),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Anything the runner can execute as FFmpeg.
///
/// Production code passes a [`VerifiedFfmpeg`] (catalog digest + LGPL build probe).
/// The trait is public and unsealed, so a caller can implement it for its own type.
pub trait FfmpegBinary {
    fn ffmpeg_path(&self) -> &Path;
}

impl FfmpegBinary for VerifiedFfmpeg {
    fn ffmpeg_path(&self) -> &Path {
        self.path()
    }
}

/// Audio transcode request: the common "to this codec/rate/channels" shape.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioTranscode {
    /// FFmpeg audio encoder, e.g. `pcm_s16le`, `pcm_f32le`, `flac`.
    pub codec: String,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    /// Optional `-af` filter chain.
    pub filter: Option<String>,
}

impl AudioTranscode {
    pub fn pcm_s16le(sample_rate: u32, channels: u32) -> Self {
        Self {
            codec: "pcm_s16le".into(),
            sample_rate: Some(sample_rate),
            channels: Some(channels),
            filter: None,
        }
    }
}

/// `ffmpeg -i input -vn [-af ..] [-ar ..] [-ac ..] -c:a codec output` with
/// `-progress pipe:1`. The order is fixed: `-ar` (sample rate) is emitted before `-ac`
/// (channel count), and `-af` before both.
pub fn transcode_audio_command(
    input: &Path,
    output: &Path,
    spec: &AudioTranscode,
) -> FfmpegCommand {
    let mut c = FfmpegCommand::base("error");
    c.progress_pipe().input(input).text("-vn");
    if let Some(f) = &spec.filter {
        c.texts(["-af", f.as_str()]);
    }
    if let Some(r) = spec.sample_rate {
        c.texts(["-ar".to_string(), r.to_string()]);
    }
    if let Some(n) = spec.channels {
        c.texts(["-ac".to_string(), n.to_string()]);
    }
    c.texts(["-c:a", spec.codec.as_str()]).path(output);
    c
}

/// What a clip keeps from its input, for [`extract_args`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtractMode {
    /// `-c copy`: stream copy, no re-encode.
    Copy,
    /// `-vn`: drop video and keep the audio streams.
    AudioOnly,
}

/// Clip options for [`extract_args`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExtractOptions {
    /// Seek to this input time before decoding (`-ss`, placed before `-i`).
    pub start: Option<Duration>,
    /// Stop after this output duration (`-t`, placed after `-i`).
    pub duration: Option<Duration>,
    /// `None` leaves the streams to FFmpeg's defaults (no `-vn` and no `-c copy`).
    pub mode: Option<ExtractMode>,
    /// Extra tokens placed after the mode flags and before the output path.
    pub extra: Vec<OsString>,
}

/// Seconds for `-ss` and `-t`, with microsecond precision (`1.500000`). A sub-microsecond
/// remainder is dropped.
fn seconds_arg(duration: Duration) -> OsString {
    OsString::from(format!(
        "{}.{:06}",
        duration.as_secs(),
        duration.subsec_micros()
    ))
}

/// `-y [-ss S] -i input [-t D] [-vn | -c copy] extra... output`: a clip, audio-only extract or
/// stream copy from `input` into `output`. Overwrites `output` (`-y`). Paths and extra tokens
/// stay `OsString`s and are never passed through a shell. The binary is not part of the list:
/// the caller runs it with [`crate::runner::run_checked`] or a similar call.
pub fn extract_args(input: &Path, output: &Path, options: &ExtractOptions) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![OsString::from("-y")];
    if let Some(start) = options.start {
        args.push(OsString::from("-ss"));
        args.push(seconds_arg(start));
    }
    args.push(OsString::from("-i"));
    args.push(input.as_os_str().to_owned());
    if let Some(duration) = options.duration {
        args.push(OsString::from("-t"));
        args.push(seconds_arg(duration));
    }
    match options.mode {
        Some(ExtractMode::Copy) => args.extend(["-c", "copy"].map(OsString::from)),
        Some(ExtractMode::AudioOnly) => args.push(OsString::from("-vn")),
        None => {}
    }
    args.extend(options.extra.iter().cloned());
    args.push(output.as_os_str().to_owned());
    args
}
