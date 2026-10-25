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
    path::{Component, Path, PathBuf},
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
            Ok(ToolHandle::Development(DevFfmpeg::mode_gated(path)))
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
    bundled_tool_candidates_with_dirs(tool, exe, os, arch, &[])
}

/// [`bundled_tool_candidates`] with bundle directories: relative sub-directories such as
/// `runtime` and `resources/runtime`, resolved against the executable's directory. The order is
/// beside the executable, then each of `bundle_dirs` in the order given, then the
/// `src-tauri/binaries` development layout. For `os` `macos` and an executable in
/// `Contents/MacOS`, each bundle directory is tried a second time under `Contents/Resources`,
/// right after its beside-the-executable form (so `runtime` gives `Contents/MacOS/runtime`, then
/// `Contents/Resources/runtime`). An entry that is empty, absolute or has a component other than
/// a plain name (`..`, `.`) is skipped: a bundle directory cannot leave the bundle.
pub fn bundled_tool_candidates_with_dirs(
    tool: Tool,
    exe: &Path,
    os: &str,
    arch: &str,
    bundle_dirs: &[PathBuf],
) -> Vec<PathBuf> {
    let Some(dir) = exe.parent() else {
        return Vec::new();
    };
    let name = tool.file_name(os);
    let mut candidates = vec![dir.join(&name)];
    let resources = macos_resources_dir(dir).filter(|_| os.eq_ignore_ascii_case("macos"));
    for sub in bundle_dirs.iter().filter(|sub| is_plain_relative(sub)) {
        candidates.push(dir.join(sub).join(&name));
        if let Some(resources) = &resources {
            candidates.push(resources.join(sub).join(&name));
        }
    }
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

/// `Contents/Resources` for an executable directory that is `Contents/MacOS`.
fn macos_resources_dir(exe_dir: &Path) -> Option<PathBuf> {
    let contents = exe_dir.parent()?;
    (exe_dir.file_name()? == "MacOS" && contents.file_name()? == "Contents")
        .then(|| contents.join("Resources"))
}

fn is_plain_relative(dir: &Path) -> bool {
    dir.components().next().is_some()
        && dir
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

/// Options for the one-call resolver. The default keeps the documented order (bundled first).
///
/// The crate reads no application environment variable itself: an app that wants a development
/// override (for example its own `GENRIGHT_FFMPEG`) reads the variable and passes the path in
/// through [`ResolveOptions::override_first`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResolveOptions {
    override_first: Option<PathBuf>,
    bundle_dirs: Vec<PathBuf>,
    variants: Vec<String>,
}

impl ResolveOptions {
    /// The default options: no override, no bundle directories, the resolver's own `variant`.
    pub fn new() -> Self {
        Self::default()
    }

    /// The common app layout: a `runtime` directory or a `resources/runtime` directory beside the
    /// executable (on macOS also under `Contents/Resources`), in that order. Same as
    /// `ResolveOptions::new().bundle_dirs(["runtime", "resources/runtime"])`.
    pub fn runtime_layout() -> Self {
        Self::new().bundle_dirs(["runtime", "resources/runtime"])
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

    /// Also look for the bundled tool in these relative sub-directories of the executable's
    /// directory, in the given order, after the file beside the executable and before the
    /// `src-tauri/binaries` development layout; see [`bundled_tool_candidates_with_dirs`] for the
    /// macOS `Contents/Resources` form and for the entries that are skipped. Replaces any earlier
    /// list.
    #[must_use]
    pub fn bundle_dirs<I, P>(mut self, dirs: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.bundle_dirs = dirs.into_iter().map(Into::into).collect();
        self
    }

    /// The catalog variant slugs to verify a candidate against, in order (for example
    /// `[EXPORT_ASS_ZIMG_VARIANT, EXPORT_ASS_VARIANT]`); see [`verify_first_of`]. A non-empty list
    /// replaces the resolver's own `variant` argument; the default empty list keeps it. Replaces
    /// any earlier list.
    #[must_use]
    pub fn variants<I, S>(mut self, variants: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.variants = variants.into_iter().map(Into::into).collect();
        self
    }

    /// The configured override path, if any.
    pub fn override_path(&self) -> Option<&Path> {
        self.override_first.as_deref()
    }

    /// The configured bundle directories, in lookup order.
    pub fn bundle_dir_list(&self) -> &[PathBuf] {
        &self.bundle_dirs
    }

    /// The configured variant slugs, in verification order.
    pub fn variant_list(&self) -> &[String] {
        &self.variants
    }
}

/// Verify the one file at `path` against each of `variants` in order (with `verify`, for example
/// [`verify_bundled_ffmpeg`]) and return the first success.
///
/// It goes on to the next variant when the error is not-in-catalog
/// ([`MediaError::is_not_in_catalog`]: the file's digest is not that variant's row, or the
/// variant has no approved row for this platform) and also when it is
/// [`MediaError::ArtifactPending`] (the variant's only row for this host is pending, so the
/// variant cannot be used here, and a later variant may be approved: `[export-ass-zimg, decode]`
/// on a host where only `decode` is approved). Any other error (the build probe failed, I/O,
/// the wrong tool, an ambiguous row) says something a different variant cannot fix, and is
/// returned at once. When every variant fails this way, the first variant's error is returned,
/// so a development fallback (which applies only to a not-in-catalog error) is not offered for a
/// pending row. An empty `variants` list is [`MediaError::MissingMetadata`].
pub fn verify_first_of<V, S: AsRef<str>>(
    path: &Path,
    variants: &[S],
    timeout: Duration,
    verify: impl Fn(&Path, &str, Duration) -> Result<V, MediaError>,
) -> Result<V, MediaError> {
    let mut first_error = None;
    for variant in variants {
        match verify(path, variant.as_ref(), timeout) {
            Ok(verified) => return Ok(verified),
            Err(error)
                if error.is_not_in_catalog()
                    || matches!(error, MediaError::ArtifactPending { .. }) =>
            {
                first_error.get_or_insert(error);
            }
            Err(error) => return Err(error),
        }
    }
    Err(first_error.unwrap_or(MediaError::MissingMetadata("variants")))
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
///    [`bundled_tool_candidates_with_dirs`], with [`ResolveOptions::bundle_dirs`]) is verified
///    against the approved `variant` row, or against each of [`ResolveOptions::variants`] in
///    order when that list is not empty ([`verify_first_of`]). The development fallback of
///    [`verified_or_development_in`] applies only after every variant failed not-in-catalog.
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
    let variants: Vec<&str> = match options.variant_list() {
        [] => vec![variant],
        listed => listed.iter().map(String::as_str).collect(),
    };
    let mut skipped = Vec::new();
    if let Some(path) = options.override_path() {
        let outcome = if path.is_file() {
            verified_or_development_in(
                verify_first_of(path, &variants, timeout, &verify),
                path,
                mode,
            )
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
    if let Some(path) =
        bundled_tool_candidates_with_dirs(tool, exe, os, arch, options.bundle_dir_list())
            .into_iter()
            .find(|candidate| candidate.is_file())
    {
        let handle = verified_or_development_in(
            verify_first_of(&path, &variants, timeout, &verify),
            &path,
            mode,
        )?;
        return Ok(HandleResolution { handle, skipped });
    }
    if mode == BuildMode::Development {
        let found = resolve_unverified(dev, mode)?;
        return Ok(HandleResolution {
            handle: ToolHandle::Development(DevFfmpeg::mode_gated(found.path())),
            skipped,
        });
    }
    Err(MediaError::ArtifactNotFound)
}

/// `resolve_handle` for `ffmpeg`: bundled beside `exe` (or in `src-tauri/binaries`), verified
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

    // Bundle directories and variants.

    fn dirs(list: &[&str]) -> Vec<PathBuf> {
        list.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn bundle_dirs_are_tried_in_order_between_the_executable_and_the_checkout_layout() {
        let exe = Path::new("/opt/app/app");
        let candidates = bundled_tool_candidates_with_dirs(
            Tool::Ffmpeg,
            exe,
            "linux",
            "x86_64",
            &dirs(&["runtime", "resources/runtime"]),
        );
        assert_eq!(
            candidates[..3],
            [
                Path::new("/opt/app/ffmpeg"),
                Path::new("/opt/app/runtime/ffmpeg"),
                Path::new("/opt/app/resources/runtime/ffmpeg"),
            ]
        );
        assert_eq!(
            candidates[3],
            Path::new("/opt/app/src-tauri/binaries/ffmpeg-x86_64-unknown-linux-gnu")
        );
        assert!(candidates[3..]
            .iter()
            .all(|path| { path.ends_with("src-tauri/binaries/ffmpeg-x86_64-unknown-linux-gnu") }));
    }

    #[test]
    fn the_macos_bundle_layout_also_tries_contents_resources_after_each_directory() {
        let exe = Path::new("/App.app/Contents/MacOS/app");
        let candidates = bundled_tool_candidates_with_dirs(
            Tool::Ffmpeg,
            exe,
            "macos",
            "aarch64",
            &dirs(&["runtime", "resources/runtime"]),
        );
        assert_eq!(
            candidates[..5],
            [
                Path::new("/App.app/Contents/MacOS/ffmpeg"),
                Path::new("/App.app/Contents/MacOS/runtime/ffmpeg"),
                Path::new("/App.app/Contents/Resources/runtime/ffmpeg"),
                Path::new("/App.app/Contents/MacOS/resources/runtime/ffmpeg"),
                Path::new("/App.app/Contents/Resources/resources/runtime/ffmpeg"),
            ]
        );
        assert_eq!(
            candidates[5],
            Path::new("/App.app/Contents/MacOS/src-tauri/binaries/ffmpeg-aarch64-apple-darwin")
        );

        // Neither another OS nor an executable outside Contents/MacOS gets the Resources form.
        for (exe, os) in [
            ("/App.app/Contents/MacOS/app", "linux"),
            ("/opt/app/app", "macos"),
            ("/App.app/Contents/Frameworks/app", "macos"),
            ("/Elsewhere/MacOS/app", "macos"),
        ] {
            let candidates = bundled_tool_candidates_with_dirs(
                Tool::Ffmpeg,
                Path::new(exe),
                os,
                "aarch64",
                &dirs(&["runtime"]),
            );
            assert!(
                candidates
                    .iter()
                    .all(|path| !path.to_string_lossy().contains("Resources")),
                "{exe} on {os}: {candidates:?}"
            );
        }
    }

    #[test]
    fn bundle_dir_candidates_use_windows_file_names() {
        let exe = Path::new("C:/app/app.exe");
        let candidates = bundled_tool_candidates_with_dirs(
            Tool::Ffprobe,
            exe,
            "windows",
            "x86_64",
            &dirs(&["runtime", "resources/runtime"]),
        );
        assert_eq!(
            candidates[..3],
            [
                Path::new("C:/app/ffprobe.exe"),
                Path::new("C:/app/runtime/ffprobe.exe"),
                Path::new("C:/app/resources/runtime/ffprobe.exe"),
            ]
        );
        assert!(candidates[3]
            .to_string_lossy()
            .ends_with("ffprobe-x86_64-pc-windows-msvc.exe"));
    }

    #[test]
    fn a_bundle_dir_that_could_leave_the_bundle_is_skipped() {
        let exe = Path::new("/opt/app/app");
        let candidates = bundled_tool_candidates_with_dirs(
            Tool::Ffmpeg,
            exe,
            "linux",
            "x86_64",
            &dirs(&["", "/abs", "../up", "ok", "a/../b", "./dot"]),
        );
        assert_eq!(candidates[0], Path::new("/opt/app/ffmpeg"));
        assert_eq!(candidates[1], Path::new("/opt/app/ok/ffmpeg"));
        assert!(candidates[2]
            .parent()
            .unwrap()
            .ends_with("src-tauri/binaries"));
    }

    #[test]
    fn no_bundle_dirs_gives_exactly_the_existing_candidates() {
        for (exe, os, arch) in [
            ("/app/Contents/MacOS/app", "macos", "aarch64"),
            ("C:/app/app.exe", "windows", "x86_64"),
            ("/opt/app/app", "plan9", "mips"),
        ] {
            for tool in [Tool::Ffmpeg, Tool::Ffprobe] {
                assert_eq!(
                    bundled_tool_candidates(tool, Path::new(exe), os, arch),
                    bundled_tool_candidates_with_dirs(tool, Path::new(exe), os, arch, &[]),
                );
            }
        }
        assert!(
            bundled_tool_candidates(Tool::Ffmpeg, Path::new("/"), "macos", "aarch64").is_empty()
        );
    }

    #[test]
    fn the_runtime_layout_is_runtime_then_resources_runtime() {
        let layout = ResolveOptions::runtime_layout();
        assert_eq!(
            layout.bundle_dir_list(),
            dirs(&["runtime", "resources/runtime"])
        );
        assert_eq!(
            layout,
            ResolveOptions::new().bundle_dirs(["runtime", "resources/runtime"])
        );
        assert!(layout.variant_list().is_empty());
        assert_eq!(layout.override_path(), None);
    }

    /// A verify closure that records the variants it was asked about and answers per variant.
    struct Variants {
        asked: std::cell::RefCell<Vec<String>>,
    }

    impl Variants {
        fn new() -> Self {
            Self {
                asked: Default::default(),
            }
        }

        fn verify<'a>(
            &'a self,
            answer: impl Fn(&str) -> Result<String, MediaError> + 'a,
        ) -> impl Fn(&Path, &str, Duration) -> Result<String, MediaError> + 'a {
            move |_path, variant, _timeout| {
                self.asked.borrow_mut().push(variant.to_owned());
                answer(variant)
            }
        }

        fn asked(&self) -> Vec<String> {
            self.asked.borrow().clone()
        }
    }

    fn digest_mismatch(variant: &str) -> MediaError {
        MediaError::DigestMismatch {
            path: PathBuf::from(variant),
        }
    }

    fn run_in<V>(
        exe: &Path,
        mode: BuildMode,
        options: &ResolveOptions,
        verify: impl Fn(&Path, &str, Duration) -> Result<V, MediaError>,
    ) -> Result<HandleResolution<V>, MediaError> {
        resolve_handle(
            Tool::Ffmpeg,
            exe,
            "decode",
            &UnverifiedRequest::new(Tool::Ffmpeg),
            mode,
            options,
            verify,
            Duration::from_secs(1),
        )
    }

    fn verified_value(resolution: &HandleResolution<String>) -> &str {
        match &resolution.handle {
            ToolHandle::Verified(value) => value,
            ToolHandle::Development(_) => panic!("expected a verified handle"),
        }
    }

    /// `<dir>/app` plus a tool file in each of the given sub-directories of `dir`.
    fn bundle_layout(subs: &[&str]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("app");
        std::fs::write(&exe, b"app").unwrap();
        for sub in subs {
            let at = dir.path().join(sub);
            std::fs::create_dir_all(&at).unwrap();
            std::fs::write(at.join(Tool::Ffmpeg.file_name(env::consts::OS)), sub).unwrap();
        }
        (dir, exe)
    }

    #[test]
    fn a_tool_in_a_bundle_dir_is_found_and_the_first_listed_dir_wins() {
        let (_dir, exe) = bundle_layout(&["resources/runtime"]);
        let found = run_in(
            &exe,
            BuildMode::Release,
            &ResolveOptions::runtime_layout(),
            |path, _, _| Ok(std::fs::read_to_string(path).unwrap()),
        )
        .unwrap();
        assert_eq!(verified_value(&found), "resources/runtime");

        // Without the option the same layout has no bundled file in a release build.
        let none = run_in(
            &exe,
            BuildMode::Release,
            &ResolveOptions::new(),
            |path, _, _| Ok(path.to_owned()),
        );
        assert!(matches!(none, Err(MediaError::ArtifactNotFound)));

        let (_dir, exe) = bundle_layout(&["runtime", "resources/runtime"]);
        let pick = |options: &ResolveOptions| {
            let resolution = run_in(&exe, BuildMode::Release, options, |path, _, _| {
                Ok(std::fs::read_to_string(path).unwrap())
            })
            .unwrap();
            verified_value(&resolution).to_owned()
        };
        assert_eq!(pick(&ResolveOptions::runtime_layout()), "runtime");
        let reversed = ResolveOptions::new().bundle_dirs(["resources/runtime", "runtime"]);
        assert_eq!(pick(&reversed), "resources/runtime");
        // A file beside the executable still comes before every bundle dir.
        std::fs::write(
            exe.with_file_name(Tool::Ffmpeg.file_name(env::consts::OS)),
            "beside",
        )
        .unwrap();
        assert_eq!(pick(&ResolveOptions::runtime_layout()), "beside");
    }

    #[test]
    fn the_default_options_verify_the_resolvers_own_variant_once() {
        let (_dir, exe, _bundled, _over) = layout();
        let asked = Variants::new();
        let resolution = run_in(
            &exe,
            BuildMode::Release,
            &ResolveOptions::default(),
            asked.verify(|variant| Ok(variant.to_owned())),
        )
        .unwrap();
        assert_eq!(verified_value(&resolution), "decode");
        assert_eq!(asked.asked(), ["decode"]);
        assert!(ResolveOptions::new().variant_list().is_empty());
        assert!(ResolveOptions::new().bundle_dir_list().is_empty());
    }

    #[test]
    fn a_later_variant_is_chosen_when_the_first_is_not_in_the_catalog() {
        let (_dir, exe, _bundled, _over) = layout();
        let options = ResolveOptions::new().variants(["export-ass-zimg", "export-ass", "decode"]);
        let asked = Variants::new();
        let resolution = run_in(
            &exe,
            BuildMode::Release,
            &options,
            asked.verify(|variant| match variant {
                "export-ass-zimg" => Err(MediaError::NoApprovedArtifact {
                    os: "x".into(),
                    arch: "y".into(),
                }),
                other => Ok(other.to_owned()),
            }),
        )
        .unwrap();
        assert_eq!(verified_value(&resolution), "export-ass");
        // The list replaces the resolver's `variant`, and a success stops the walk.
        assert_eq!(asked.asked(), ["export-ass-zimg", "export-ass"]);
    }

    #[test]
    fn a_probe_failure_or_io_error_does_not_fall_through_to_the_next_variant() {
        let (_dir, exe, _bundled, _over) = layout();
        let options = ResolveOptions::new().variants(["a", "b"]);
        for error in [
            (|| MediaError::BuildProbeFailed) as fn() -> MediaError,
            || MediaError::Io(std::io::Error::other("unreadable")),
            || MediaError::AmbiguousArtifact {
                os: "x".into(),
                arch: "y".into(),
            },
            || MediaError::WrongTool {
                expected: Tool::Ffmpeg,
                found: Tool::Ffprobe,
            },
        ] {
            let asked = Variants::new();
            let result = run_in(
                &exe,
                BuildMode::Release,
                &options,
                asked.verify(|variant| match variant {
                    "a" => Err(error()),
                    other => Ok(other.to_owned()),
                }),
            );
            assert!(
                matches!(&result, Err(e) if !e.is_not_in_catalog()),
                "{result:?}"
            );
            assert_eq!(asked.asked(), ["a"]);
        }
    }

    #[test]
    fn when_every_variant_is_not_in_the_catalog_the_first_variants_error_comes_back() {
        let (_dir, exe, _bundled, _over) = layout();
        let options = ResolveOptions::new().variants(["a", "b"]);
        let asked = Variants::new();
        let result = run_in(
            &exe,
            BuildMode::Release,
            &options,
            asked.verify(|variant| Err(digest_mismatch(variant))),
        );
        assert!(matches!(
            result,
            Err(MediaError::DigestMismatch { ref path }) if path == Path::new("a")
        ));
        assert_eq!(asked.asked(), ["a", "b"]);
    }

    #[test]
    fn a_release_build_never_produces_a_development_handle_from_the_variants() {
        let (_dir, exe, bundled, _over) = layout();
        assert!(bundled.is_file());
        let options = ResolveOptions::new().variants(["a", "b"]);
        let result = run_in(
            &exe,
            BuildMode::Release,
            &options,
            |_: &Path, variant: &str, _| Err::<(), _>(digest_mismatch(variant)),
        );
        assert!(matches!(result, Err(MediaError::DigestMismatch { .. })));
    }

    #[test]
    fn a_development_build_falls_back_only_after_every_variant_is_not_in_the_catalog() {
        let (_dir, exe, bundled, _over) = layout();
        let options = ResolveOptions::new().variants(["a", "b"]);

        // Every variant not in the catalog: the bundled file becomes a development handle.
        let asked = Variants::new();
        let resolution = run_in(
            &exe,
            BuildMode::Development,
            &options,
            asked.verify(|variant| Err(digest_mismatch(variant))),
        )
        .unwrap();
        match &resolution.handle {
            ToolHandle::Development(dev) => assert_eq!(dev.ffmpeg_path(), bundled.as_path()),
            ToolHandle::Verified(_) => panic!("nothing verified"),
        }
        assert_eq!(asked.asked(), ["a", "b"]);

        // A later variant that verifies beats the fallback.
        let resolution = run_in(
            &exe,
            BuildMode::Development,
            &options,
            |_: &Path, variant: &str, _| {
                if variant == "b" {
                    Ok(variant.to_owned())
                } else {
                    Err(digest_mismatch(variant))
                }
            },
        )
        .unwrap();
        assert_eq!(verified_value(&resolution), "b");

        // A probe failure on a later variant is shown, not worked around.
        let result = run_in(
            &exe,
            BuildMode::Development,
            &options,
            |_: &Path, variant: &str, _| {
                if variant == "b" {
                    Err::<String, _>(MediaError::BuildProbeFailed)
                } else {
                    Err(digest_mismatch(variant))
                }
            },
        );
        assert!(matches!(result, Err(MediaError::BuildProbeFailed)));
    }

    #[test]
    fn override_first_still_wins_over_a_bundle_dir_and_is_checked_against_the_variant_list() {
        let (dir, exe) = bundle_layout(&["runtime"]);
        let over = dir.path().join("override-ffmpeg");
        std::fs::write(&over, b"override").unwrap();
        let options = ResolveOptions::runtime_layout()
            .variants(["a", "b"])
            .override_first(&over);
        let asked = Variants::new();
        let resolution = run_in(
            &exe,
            BuildMode::Release,
            &options,
            asked.verify(|variant| {
                if variant == "b" {
                    Ok("override".into())
                } else {
                    Err(digest_mismatch(variant))
                }
            }),
        )
        .unwrap();
        assert_eq!(verified_value(&resolution), "override");
        assert!(resolution.skipped.is_empty());
        assert_eq!(asked.asked(), ["a", "b"]);

        // An override that is in no listed variant is skipped, then the bundle dir is used.
        let resolution = run_in(
            &exe,
            BuildMode::Release,
            &options,
            |path: &Path, variant: &str, _| {
                if variant == "b" && path.ends_with(Tool::Ffmpeg.file_name(env::consts::OS)) {
                    Ok("bundled".to_owned())
                } else {
                    Err(digest_mismatch(variant))
                }
            },
        )
        .unwrap();
        assert_eq!(verified_value(&resolution), "bundled");
        assert_eq!(resolution.skipped.len(), 1);
        assert_eq!(resolution.skipped[0].path, over);
    }

    #[test]
    fn verify_first_of_returns_the_first_success_and_refuses_an_empty_list() {
        let (_dir, _exe, bundled, _over) = layout();
        let asked = Variants::new();
        let first = verify_first_of(
            &bundled,
            &["a", "b", "c"],
            Duration::from_secs(1),
            asked.verify(|variant| Ok(variant.to_owned())),
        )
        .unwrap();
        assert_eq!(first, "a");
        assert_eq!(asked.asked(), ["a"]);

        let owned: Vec<String> = vec!["x".into(), "y".into()];
        let second = verify_first_of(
            &bundled,
            &owned,
            Duration::from_secs(1),
            |_: &Path, variant: &str, _| {
                if variant == "y" {
                    Ok(variant.to_owned())
                } else {
                    Err(digest_mismatch(variant))
                }
            },
        )
        .unwrap();
        assert_eq!(second, "y");

        let none: &[&str] = &[];
        let empty = verify_first_of(
            &bundled,
            none,
            Duration::from_secs(1),
            |_: &Path, v: &str, _| Ok(v.to_owned()),
        );
        assert!(matches!(
            empty,
            Err(MediaError::MissingMetadata("variants"))
        ));
        assert!(!MediaError::MissingMetadata("variants").is_not_in_catalog());
    }

    fn pending(id: &str) -> MediaError {
        MediaError::ArtifactPending { id: id.to_owned() }
    }

    #[test]
    fn verify_first_of_skips_a_pending_variant_and_uses_the_next_approved_one() {
        let (_dir, _exe, bundled, _over) = layout();
        let asked = Variants::new();
        // `[export-ass-zimg, decode]` on a host where export-ass-zimg is pending and decode is approved.
        let used = verify_first_of(
            &bundled,
            &["zimg", "decode"],
            Duration::from_secs(1),
            asked.verify(|variant| {
                if variant == "decode" {
                    Ok(variant.to_owned())
                } else {
                    Err(pending("zimg-row"))
                }
            }),
        )
        .unwrap();
        assert_eq!(used, "decode");
        assert_eq!(asked.asked(), ["zimg", "decode"]);
    }

    #[test]
    fn verify_first_of_returns_the_first_variants_error_when_every_variant_is_pending() {
        let (_dir, _exe, bundled, _over) = layout();
        let asked = Variants::new();
        let error = verify_first_of(
            &bundled,
            &["a", "b"],
            Duration::from_secs(1),
            asked.verify(|variant| Err(pending(&format!("row-{variant}")))),
        )
        .unwrap_err();
        assert!(
            matches!(&error, MediaError::ArtifactPending { id } if id == "row-a"),
            "{error:?}"
        );
        assert_eq!(asked.asked(), ["a", "b"]);
    }

    #[test]
    fn verify_first_of_still_returns_any_other_error_at_once_after_a_pending_variant() {
        let (_dir, _exe, bundled, _over) = layout();
        let asked = Variants::new();
        let error = verify_first_of(
            &bundled,
            &["a", "b", "c"],
            Duration::from_secs(1),
            asked.verify(|variant| match variant {
                "a" => Err(pending("row-a")),
                "b" => Err(MediaError::BuildProbeFailed),
                _ => Ok(variant.to_owned()),
            }),
        )
        .unwrap_err();
        assert!(matches!(error, MediaError::BuildProbeFailed), "{error:?}");
        assert_eq!(asked.asked(), ["a", "b"]);
    }

    #[test]
    fn a_pending_only_variant_list_gets_no_development_fallback() {
        let (_dir, exe, _bundled, _over) = layout();
        let options = ResolveOptions::new().variants(["a", "b"]);
        let error = run_in(
            &exe,
            BuildMode::Development,
            &options,
            |_: &Path, variant: &str, _| -> Result<String, MediaError> {
                Err(pending(&format!("row-{variant}")))
            },
        )
        .map(|_| ())
        .unwrap_err();
        assert!(
            matches!(error, MediaError::ArtifactPending { .. }),
            "{error:?}"
        );
    }
}
