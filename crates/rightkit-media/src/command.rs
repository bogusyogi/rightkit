//! Typed FFmpeg argument vectors and the binary handle the runner executes.
//!
//! Donor: CutRight `cutright-timeline/src/ffmpeg.rs` (`FfmpegCommand`/`FfmpegArg`).
//! Arguments stay `OsString`-faithful (paths are never lossy-joined into text) and
//! are never passed through a shell.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use crate::VerifiedFfmpeg;

/// One FFmpeg argument: plain text or a filesystem path (kept as `OsString`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FfmpegArg {
    Text(String),
    Path(PathBuf),
}

/// An ordered FFmpeg argument vector.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FfmpegCommand {
    args: Vec<FfmpegArg>,
}

impl FfmpegCommand {
    pub fn new() -> Self {
        Self::default()
    }

    /// `-hide_banner -nostdin -y -loglevel <level> -nostats`: the baseline every
    /// non-interactive Right Suite invocation starts from.
    pub fn base(loglevel: &str) -> Self {
        let mut c = Self::new();
        c.texts(["-hide_banner", "-nostdin", "-y", "-loglevel", loglevel, "-nostats"]);
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
        self.args.push(FfmpegArg::Path(value.as_ref().to_path_buf()));
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
/// [`DevFfmpeg`] exists for development hosts and tests that have an FFmpeg that is
/// not in the approved catalog (for example Homebrew's); it is deliberately named
/// so it cannot be mistaken for the verified path.
pub trait FfmpegBinary {
    fn ffmpeg_path(&self) -> &Path;
}

impl FfmpegBinary for VerifiedFfmpeg {
    fn ffmpeg_path(&self) -> &Path {
        self.path()
    }
}

/// An FFmpeg path that has NOT been checked against the artifact catalog.
#[derive(Clone, Debug)]
pub struct DevFfmpeg(PathBuf);

impl DevFfmpeg {
    pub fn unverified(path: impl Into<PathBuf>) -> Self {
        Self(path.into())
    }
}

impl FfmpegBinary for DevFfmpeg {
    fn ffmpeg_path(&self) -> &Path {
        &self.0
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
/// `-progress pipe:1`.
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
