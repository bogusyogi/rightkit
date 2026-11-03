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

use crate::{BuildMode, MediaError, VerifiedFfmpeg};

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
///
/// Policy by build mode (see [`BuildMode`]): [`DevFfmpeg::try_unverified`] refuses with
/// [`MediaError::UnverifiedRefusedInRelease`] in any build without the `dev-overrides` feature.
/// `DevFfmpeg::unverified`, which has no mode check, is compiled only in this crate's own test
/// builds and with `dev-overrides`.
///
/// What is closed: the two public constructors that turned a bare path into a `DevFfmpeg` with
/// no mode check. `DevFfmpeg::allow_in_release_builds` is removed in every build and
/// `DevFfmpeg::unverified` does not exist without the feature (both are proved by the doctests
/// below). This is not a guarantee that a release build runs only catalog-verified binaries.
/// What remains open, by design or as a follow-up:
///
/// - [`crate::dev_resolve::resolve_for_development`] is a public function with no mode check:
///   it returns a `DevFfmpeg` for the first existing candidate (including `RIGHTKIT_FFMPEG`)
///   when none verifies against the catalog. Its callers (`rightkit-audio`, `rightkit-motion`
///   and this crate's end-to-end tests) depend on that, so its behaviour is unchanged here.
///   Gating it behind `dev-overrides` is a follow-up, not done in this change.
/// - [`FfmpegBinary`] is a public, unsealed trait: any type can implement it and be passed to
///   the runner.
/// - [`BuildMode::Development`] is a public variant: a caller that passes it to
///   [`crate::handle::verified_or_development_in`] or [`crate::unverified::resolve_unverified`]
///   gets the development fallback in any build.
///
/// The crate's own fallbacks ([`crate::verified_or_development`],
/// [`crate::resolve_ffmpeg_handle`]) check the mode before they build a `DevFfmpeg` through a
/// crate-private constructor.
///
/// ```
/// use rightkit_ffmpeg::DevFfmpeg;
///
/// // The control for the proofs below: the mode-checked constructor is always there.
/// let _ = DevFfmpeg::try_unverified("ffmpeg");
/// ```
///
/// `DevFfmpeg::allow_in_release_builds`, the explicit release escape of earlier versions, is
/// removed in every build:
///
/// ```compile_fail,E0599
/// let _ = rightkit_ffmpeg::DevFfmpeg::allow_in_release_builds("ffmpeg");
/// ```
#[cfg_attr(
    not(feature = "dev-overrides"),
    doc = "
Without the `dev-overrides` feature `DevFfmpeg::unverified` does not exist either, so no code
outside a test build of this crate can turn a bare path into a `DevFfmpeg` without the mode check:

```compile_fail,E0599
let _ = rightkit_ffmpeg::DevFfmpeg::unverified(\"ffmpeg\");
```
"
)]
#[derive(Clone, Debug)]
pub struct DevFfmpeg(PathBuf);

impl DevFfmpeg {
    /// A development binary the caller has already decided may run: the callers in this crate
    /// check [`BuildMode::Development`] first.
    pub(crate) fn mode_gated(path: impl Into<PathBuf>) -> Self {
        Self(path.into())
    }

    /// A `DevFfmpeg` for `path`, refused with [`MediaError::UnverifiedRefusedInRelease`]
    /// in any build without the `dev-overrides` feature.
    pub fn try_unverified(path: impl Into<PathBuf>) -> Result<Self, MediaError> {
        Self::try_unverified_in(path, BuildMode::from_build_features())
    }

    fn try_unverified_in(path: impl Into<PathBuf>, mode: BuildMode) -> Result<Self, MediaError> {
        match mode {
            BuildMode::Development => Ok(Self::mode_gated(path)),
            BuildMode::Release => Err(MediaError::UnverifiedRefusedInRelease),
        }
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
