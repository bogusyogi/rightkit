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

/// The one-call resolver shared by both tools.
///
/// 1. The first existing bundled candidate for `tool` on this host (see
///    [`bundled_tool_candidates`]) is verified against the approved `variant` row, with the
///    development fallback of [`verified_or_development_in`].
/// 2. With no bundled file, a development build (`mode == Development`) resolves `dev` (its
///    configured path, environment variable, or `PATH` when enabled) and returns it as a
///    `Development` handle. Every other build returns [`MediaError::ArtifactNotFound`].
///
/// `dev.tool` must be `tool`, or the call returns [`MediaError::WrongTool`].
fn resolve_handle<V>(
    tool: Tool,
    exe: &Path,
    variant: &str,
    dev: &UnverifiedRequest,
    mode: BuildMode,
    verify: impl Fn(&Path, &str, Duration) -> Result<V, MediaError>,
    timeout: Duration,
) -> Result<ToolHandle<V>, MediaError> {
    if dev.tool != tool {
        return Err(MediaError::WrongTool {
            expected: tool,
            found: dev.tool,
        });
    }
    let os = env::consts::OS;
    let arch = env::consts::ARCH;
    if let Some(path) = bundled_tool_candidates(tool, exe, os, arch)
        .into_iter()
        .find(|candidate| candidate.is_file())
    {
        return verified_or_development_in(verify(&path, variant, timeout), &path, mode);
    }
    if mode == BuildMode::Development {
        let found = resolve_unverified(dev, mode)?;
        return Ok(ToolHandle::Development(DevFfmpeg::unverified(found.path())));
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
    resolve_handle(
        Tool::Ffmpeg,
        exe,
        variant,
        dev,
        mode,
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
    resolve_handle(
        Tool::Ffprobe,
        exe,
        variant,
        dev,
        mode,
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
}
