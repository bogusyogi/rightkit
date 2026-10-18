//! One handle for "catalog-verified, else a development binary", and the one-call resolver.
//!
//! An app that ships its own ffmpeg wants one value to pass to every call: the verified handle
//! when the bundled file is an approved build, and otherwise a development binary, but only in a
//! development build. [`ToolHandle`] is that value. It implements [`FfmpegBinary`], so every run,
//! probe, decode and encoder call takes it directly, and [`ToolHandle::is_verified`] lets the app
//! say which one it has.
//!
//! The fallback obeys the crate's policy (see [`MediaError::is_not_in_catalog`]). A development
//! handle is produced only when the verification error means "not in the catalog" (a digest that
//! is not the row's, or no approved row for this platform), the file exists, and the build allows
//! unverified binaries (`dev-overrides`). Every other error, and every release build, returns the
//! catalog error unchanged. A release build never runs an unverified binary through this module.

use std::{
    env,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{
    bundled::{verify_bundled_ffmpeg, verify_bundled_ffprobe},
    external_bin_path, resolve_unverified, tauri_target_triple, BuildMode, DevFfmpeg, FfmpegBinary,
    MediaError, Tool, UnverifiedRequest, VerifiedFfmpeg, VerifiedFfprobe,
};

/// A binary the app may run: catalog-verified, or a development binary the catalog does not list.
// Resolved once per app session and passed by reference; boxing the verified handle buys nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum ToolHandle<V> {
    /// Matched its approved catalog row (content digest and LGPL build probe).
    Verified(V),
    /// Not in the catalog. Produced only in a development build; labelled by its variant name.
    Development(DevFfmpeg),
}

/// [`ToolHandle`] for `ffmpeg`.
pub type FfmpegHandle = ToolHandle<VerifiedFfmpeg>;

/// [`ToolHandle`] for `ffprobe`.
pub type FfprobeHandle = ToolHandle<VerifiedFfprobe>;

impl<V> ToolHandle<V> {
    /// `true` only for a catalog-verified binary.
    pub fn is_verified(&self) -> bool {
        matches!(self, Self::Verified(_))
    }
}

impl<V: FfmpegBinary> FfmpegBinary for ToolHandle<V> {
    fn ffmpeg_path(&self) -> &Path {
        match self {
            Self::Verified(verified) => verified.ffmpeg_path(),
            Self::Development(dev) => dev.ffmpeg_path(),
        }
    }
}

/// The handle decision for one binary from its catalog verification result, under `mode`.
///
/// `verified` is the outcome of verifying `path` against the catalog (for example
/// [`verify_bundled_ffmpeg`]). Returns `Verified` on success. On failure it returns a
/// `Development` handle for `path` only when the error is not-in-catalog, `path` is a file, and
/// `mode` is [`BuildMode::Development`]; otherwise it returns the error unchanged.
pub fn verified_or_development_in<V>(
    verified: Result<V, MediaError>,
    path: &Path,
    mode: BuildMode,
) -> Result<ToolHandle<V>, MediaError> {
    match verified {
        Ok(handle) => Ok(ToolHandle::Verified(handle)),
        Err(error)
            if error.is_not_in_catalog() && mode == BuildMode::Development && path.is_file() =>
        {
            Ok(ToolHandle::Development(DevFfmpeg::unverified(path)))
        }
        Err(error) => Err(error),
    }
}

/// [`verified_or_development_in`] under the build's own mode ([`BuildMode::from_build_features`]).
pub fn verified_or_development<V>(
    verified: Result<V, MediaError>,
    path: &Path,
) -> Result<ToolHandle<V>, MediaError> {
    verified_or_development_in(verified, path, BuildMode::from_build_features())
}

/// Verify the `ffmpeg` at `path` against the approved `variant` row for this host, and return
/// its handle, with the development fallback described in the module docs.
pub fn verify_ffmpeg_or_development(
    path: &Path,
    variant: &str,
    timeout: Duration,
) -> Result<FfmpegHandle, MediaError> {
    verified_or_development(verify_bundled_ffmpeg(path, variant, timeout), path)
}

/// [`verify_ffmpeg_or_development`] for an `ffprobe`.
pub fn verify_ffprobe_or_development(
    path: &Path,
    variant: &str,
    timeout: Duration,
) -> Result<FfprobeHandle, MediaError> {
    verified_or_development(verify_bundled_ffprobe(path, variant, timeout), path)
}

/// Where a bundled tool may sit, in lookup order, without touching the filesystem: first beside
/// the application executable (`<tool>` or `<tool>.exe`), then in `src-tauri/binaries` as
/// `<tool>-<target-triple>[.exe]`, under the executable's directory and each of its ancestors
/// (the development checkout layout). The triple list is empty for a host RightKit does not
/// name, which leaves only the beside-the-executable candidate.
pub fn bundled_tool_candidates(tool: Tool, exe: &Path, os: &str, arch: &str) -> Vec<PathBuf> {
    let Some(dir) = exe.parent() else {
        return Vec::new();
    };
    let mut candidates = vec![dir.join(tool.file_name(os))];
    if let Some(triple) = tauri_target_triple(os, arch) {
        candidates.extend(dir.ancestors().map(|ancestor| {
            external_bin_path(
                &ancestor.join("src-tauri").join("binaries"),
                tool.stem(),
                triple,
            )
        }));
    }
    candidates
}

/// Options for the one-call resolver. The default keeps the documented order (bundled first).
///
/// The crate reads no application environment variable itself: an app that wants a development
/// override (for example its own `GENRIGHT_FFMPEG`) reads the variable and passes the path in
/// through [`ResolveOptions::override_first`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResolveOptions {
    override_first: Option<PathBuf>,
}

impl ResolveOptions {
    /// The default options: no override, bundled candidates first.
    pub fn new() -> Self {
        Self::default()
    }

    /// Try `path` before any bundled candidate. It must be a file and pass the same verification
    /// as a bundled one (catalog digest and build probe, with the development fallback of
    /// [`verified_or_development_in`]). When it does not, it is skipped, the reason is recorded in
    /// [`HandleResolution::skipped`], and resolution continues in the normal order.
    #[must_use]
    pub fn override_first(mut self, path: impl Into<PathBuf>) -> Self {
        self.override_first = Some(path.into());
        self
    }

    /// The configured override path, if any.
    pub fn override_path(&self) -> Option<&Path> {
        self.override_first.as_deref()
    }
}

/// A candidate the resolver looked at and did not use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedCandidate {
    /// The candidate path.
    pub path: PathBuf,
    /// Why it was not used.
    pub reason: String,
}

/// A resolved handle plus the candidates that were skipped on the way.
#[derive(Clone, Debug)]
pub struct HandleResolution<V> {
    /// The handle the app should use.
    pub handle: ToolHandle<V>,
    /// Candidates tried and rejected before `handle` (today: an invalid `override_first`).
    pub skipped: Vec<SkippedCandidate>,
}

/// The one-call resolver shared by both tools.
///
/// 0. With [`ResolveOptions::override_first`], that path is verified first (same checks as a
///    bundled candidate). If it is missing or fails, it is recorded as skipped and the order
///    below applies unchanged.
/// 1. The first existing bundled candidate for `tool` on this host (see
///    [`bundled_tool_candidates`]) is verified against the approved `variant` row, with the
///    development fallback of [`verified_or_development_in`].
/// 2. With no bundled file, a development build (`mode == Development`) resolves `dev` (its
///    configured path, environment variable, or `PATH` when enabled) and returns it as a
///    `Development` handle. Every other build returns [`MediaError::ArtifactNotFound`].
///
/// `dev.tool` must be `tool`, or the call returns [`MediaError::WrongTool`].
#[allow(clippy::too_many_arguments)]
fn resolve_handle<V>(
    tool: Tool,
    exe: &Path,
    variant: &str,
    dev: &UnverifiedRequest,
    mode: BuildMode,
    options: &ResolveOptions,
    verify: impl Fn(&Path, &str, Duration) -> Result<V, MediaError>,
    timeout: Duration,
) -> Result<HandleResolution<V>, MediaError> {
    if dev.tool != tool {
        return Err(MediaError::WrongTool {
            expected: tool,
            found: dev.tool,
        });
    }
    let mut skipped = Vec::new();
    if let Some(path) = options.override_path() {
        let outcome = if path.is_file() {
            verified_or_development_in(verify(path, variant, timeout), path, mode)
        } else {
            Err(MediaError::OverrideMissing {
                path: path.to_owned(),
            })
        };
        match outcome {
            Ok(handle) => return Ok(HandleResolution { handle, skipped }),
            Err(error) => skipped.push(SkippedCandidate {
                path: path.to_owned(),
                reason: error.to_string(),
            }),
        }
    }
    let os = env::consts::OS;
    let arch = env::consts::ARCH;
    if let Some(path) = bundled_tool_candidates(tool, exe, os, arch)
        .into_iter()
        .find(|candidate| candidate.is_file())
    {
        let handle = verified_or_development_in(verify(&path, variant, timeout), &path, mode)?;
        return Ok(HandleResolution { handle, skipped });
    }
    if mode == BuildMode::Development {
        let found = resolve_unverified(dev, mode)?;
        return Ok(HandleResolution {
            handle: ToolHandle::Development(DevFfmpeg::unverified(found.path())),
            skipped,
        });
    }
    Err(MediaError::ArtifactNotFound)
}

/// [`resolve_handle`] for `ffmpeg`: bundled beside `exe` (or in `src-tauri/binaries`), verified
/// against the `variant` row, else (development builds only) the `dev` request.
pub fn resolve_ffmpeg_handle(
    exe: &Path,
    variant: &str,
    dev: &UnverifiedRequest,
    mode: BuildMode,
    timeout: Duration,
) -> Result<FfmpegHandle, MediaError> {
    resolve_ffmpeg_handle_with(exe, variant, dev, mode, &ResolveOptions::default(), timeout)
        .map(|resolution| resolution.handle)
}

/// [`resolve_ffmpeg_handle`] with [`ResolveOptions`] (for example `override_first`), returning the
/// skipped candidates alongside the handle.
pub fn resolve_ffmpeg_handle_with(
    exe: &Path,
    variant: &str,
    dev: &UnverifiedRequest,
    mode: BuildMode,
    options: &ResolveOptions,
    timeout: Duration,
) -> Result<HandleResolution<VerifiedFfmpeg>, MediaError> {
    resolve_handle(
        Tool::Ffmpeg,
        exe,
        variant,
        dev,
        mode,
        options,
        verify_bundled_ffmpeg,
        timeout,
    )
}

/// [`resolve_ffmpeg_handle`] for `ffprobe`.
pub fn resolve_ffprobe_handle(
    exe: &Path,
    variant: &str,
    dev: &UnverifiedRequest,
    mode: BuildMode,
    timeout: Duration,
) -> Result<FfprobeHandle, MediaError> {
    resolve_ffprobe_handle_with(exe, variant, dev, mode, &ResolveOptions::default(), timeout)
        .map(|resolution| resolution.handle)
}

/// [`resolve_ffmpeg_handle_with`] for `ffprobe`.
pub fn resolve_ffprobe_handle_with(
    exe: &Path,
    variant: &str,
    dev: &UnverifiedRequest,
    mode: BuildMode,
    options: &ResolveOptions,
    timeout: Duration,
) -> Result<HandleResolution<VerifiedFfprobe>, MediaError> {
    resolve_handle(
        Tool::Ffprobe,
        exe,
        variant,
        dev,
        mode,
        options,
        verify_bundled_ffprobe,
        timeout,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn not_in_catalog() -> MediaError {
        MediaError::NoApprovedArtifact {
            os: "macos".into(),
            arch: "aarch64".into(),
        }
    }

    #[test]
    fn a_verified_binary_is_always_verified_in_either_mode() {
        for mode in [BuildMode::Release, BuildMode::Development] {
            let handle = verified_or_development_in(Ok(7u8), Path::new("missing"), mode).unwrap();
            assert!(handle.is_verified());
            assert!(matches!(handle, ToolHandle::Verified(7)));
        }
    }

    #[test]
    fn a_release_build_never_falls_back_to_a_development_handle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ffmpeg");
        std::fs::write(&path, b"stub").unwrap();
        let refused: Result<u8, MediaError> = Err(not_in_catalog());
        let error = verified_or_development_in(refused, &path, BuildMode::Release).unwrap_err();
        assert!(matches!(error, MediaError::NoApprovedArtifact { .. }));
    }

    #[test]
    fn a_development_build_falls_back_only_for_a_not_in_catalog_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ffmpeg");
        std::fs::write(&path, b"stub").unwrap();

        let fallback: Result<u8, MediaError> = Err(not_in_catalog());
        let handle = verified_or_development_in(fallback, &path, BuildMode::Development).unwrap();
        assert!(!handle.is_verified());
        match handle {
            ToolHandle::Development(dev) => assert_eq!(dev.ffmpeg_path(), path.as_path()),
            ToolHandle::Verified(_) => panic!("a not-in-catalog file must not verify"),
        }

        let digest: Result<u8, MediaError> = Err(MediaError::DigestMismatch { path: path.clone() });
        assert!(verified_or_development_in(digest, &path, BuildMode::Development).is_ok());

        let probe_failed: Result<u8, MediaError> = Err(MediaError::BuildProbeFailed);
        assert!(matches!(
            verified_or_development_in(probe_failed, &path, BuildMode::Development),
            Err(MediaError::BuildProbeFailed)
        ));
        let missing: Result<u8, MediaError> = Err(MediaError::ArtifactNotFound);
        assert!(matches!(
            verified_or_development_in(missing, &path, BuildMode::Development),
            Err(MediaError::ArtifactNotFound)
        ));
    }

    #[test]
    fn a_development_handle_needs_a_file_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("not-there");
        let refused: Result<u8, MediaError> = Err(not_in_catalog());
        assert!(matches!(
            verified_or_development_in(refused, &gone, BuildMode::Development),
            Err(MediaError::NoApprovedArtifact { .. })
        ));
    }

    #[test]
    fn bundled_candidates_try_beside_the_executable_then_the_checkout_layout() {
        let exe = Path::new("/app/Contents/MacOS/app");
        let candidates = bundled_tool_candidates(Tool::Ffmpeg, exe, "macos", "aarch64");
        assert_eq!(candidates[0], Path::new("/app/Contents/MacOS/ffmpeg"));
        assert!(candidates.len() > 1);
        assert!(candidates.iter().skip(1).all(|path| {
            path.file_name().and_then(|n| n.to_str()) == Some("ffmpeg-aarch64-apple-darwin")
        }));
        assert_eq!(
            candidates[1],
            Path::new("/app/Contents/MacOS/src-tauri/binaries/ffmpeg-aarch64-apple-darwin")
        );
    }

    #[test]
    fn bundled_candidates_name_windows_files_with_exe_and_skip_unknown_triples() {
        let exe = Path::new("C:/app/app.exe");
        let windows = bundled_tool_candidates(Tool::Ffprobe, exe, "windows", "x86_64");
        assert!(windows[0].ends_with("ffprobe.exe"));
        assert!(windows[1]
            .to_string_lossy()
            .ends_with("ffprobe-x86_64-pc-windows-msvc.exe"));

        let unknown = bundled_tool_candidates(Tool::Ffmpeg, exe, "plan9", "mips");
        assert_eq!(unknown.len(), 1);
    }

    /// A fake app layout: `<dir>/app` plus a bundled `ffmpeg` beside it, and a separate override file.
    fn layout() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("app");
        let bundled = dir.path().join(Tool::Ffmpeg.file_name(env::consts::OS));
        let over = dir.path().join("override-ffmpeg");
        std::fs::write(&exe, b"app").unwrap();
        std::fs::write(&bundled, b"bundled").unwrap();
        std::fs::write(&over, b"override").unwrap();
        (dir, exe, bundled, over)
    }

    /// Verification stub: accepts only files whose content is `ok`, returning the path.
    fn accept_named(
        ok: &'static [u8],
    ) -> impl Fn(&Path, &str, Duration) -> Result<PathBuf, MediaError> {
        move |path, _variant, _timeout| {
            if std::fs::read(path).map(|c| c == ok).unwrap_or(false) {
                Ok(path.to_owned())
            } else {
                Err(MediaError::DigestMismatch {
                    path: path.to_owned(),
                })
            }
        }
    }

    fn run(
        exe: &Path,
        options: &ResolveOptions,
        verify: impl Fn(&Path, &str, Duration) -> Result<PathBuf, MediaError>,
    ) -> Result<HandleResolution<PathBuf>, MediaError> {
        resolve_handle(
            Tool::Ffmpeg,
            exe,
            "decode",
            &UnverifiedRequest::new(Tool::Ffmpeg),
            BuildMode::Release,
            options,
            verify,
            Duration::from_secs(1),
        )
    }

    fn verified_path(resolution: &HandleResolution<PathBuf>) -> &Path {
        match &resolution.handle {
            ToolHandle::Verified(path) => path,
            ToolHandle::Development(_) => panic!("expected a verified handle"),
        }
    }

    #[test]
    fn a_valid_override_wins_over_the_bundled_binary() {
        let (_dir, exe, _bundled, over) = layout();
        let options = ResolveOptions::new().override_first(&over);
        // Both files verify; the override must still be chosen.
        let resolution = run(&exe, &options, |path, _, _| Ok(path.to_owned())).unwrap();
        assert_eq!(verified_path(&resolution), over.as_path());
        assert!(resolution.skipped.is_empty());
    }

    #[test]
    fn the_default_order_still_prefers_the_bundled_binary() {
        let (_dir, exe, bundled, _over) = layout();
        assert_eq!(ResolveOptions::new().override_path(), None);
        let resolution = run(&exe, &ResolveOptions::default(), |path, _, _| {
            Ok(path.to_owned())
        })
        .unwrap();
        assert_eq!(verified_path(&resolution), bundled.as_path());
        assert!(resolution.skipped.is_empty());
    }

    #[test]
    fn an_override_that_fails_verification_falls_through_with_a_reason() {
        let (_dir, exe, bundled, over) = layout();
        let options = ResolveOptions::new().override_first(&over);
        let resolution = run(&exe, &options, accept_named(b"bundled")).unwrap();
        assert_eq!(verified_path(&resolution), bundled.as_path());
        assert_eq!(resolution.skipped.len(), 1);
        assert_eq!(resolution.skipped[0].path, over);
        assert!(!resolution.skipped[0].reason.is_empty());
    }

    #[test]
    fn a_missing_override_falls_through_with_a_reason() {
        let (dir, exe, bundled, _over) = layout();
        let gone = dir.path().join("not-there");
        let options = ResolveOptions::new().override_first(&gone);
        let resolution = run(&exe, &options, |path, _, _| Ok(path.to_owned())).unwrap();
        assert_eq!(verified_path(&resolution), bundled.as_path());
        assert_eq!(resolution.skipped.len(), 1);
        assert_eq!(resolution.skipped[0].path, gone);
        assert!(!resolution.skipped[0].reason.is_empty());
    }

    #[test]
    fn an_invalid_override_with_nothing_else_reports_not_found() {
        let (_dir, exe, bundled, over) = layout();
        std::fs::remove_file(&bundled).unwrap();
        let options = ResolveOptions::new().override_first(&over);
        let result = run(&exe, &options, accept_named(b"nothing"));
        assert!(matches!(result, Err(MediaError::ArtifactNotFound)));
    }
}
