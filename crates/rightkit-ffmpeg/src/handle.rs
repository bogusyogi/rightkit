//! One handle for a catalog-verified binary, and the one-call resolver.
//!
//! An app that ships its own ffmpeg wants one value to pass to every call. [`ToolHandle`] is that
//! value: it holds a binary that matched its approved catalog row (content digest and LGPL build
//! probe). It implements [`FfmpegBinary`], so every run, probe, decode and encoder call takes it
//! directly. There is no development fallback and no unverified binary in any build: a file that
//! does not verify returns the catalog error unchanged (Adrian, 2026-11-03: "dev mode, is mine ...
//! real data.").

use std::{
    env,
    path::{Component, Path, PathBuf},
    time::Duration,
};

use crate::{
    bundled::{verify_bundled_ffmpeg, verify_bundled_ffprobe},
    external_bin_path, tauri_target_triple, FfmpegBinary, MediaError, Tool, VerifiedFfmpeg,
    VerifiedFfprobe,
};

/// A binary the app may run: one that matched its approved catalog row.
#[derive(Clone, Debug)]
pub enum ToolHandle<V> {
    /// Matched its approved catalog row (content digest and LGPL build probe).
    Verified(V),
}

/// [`ToolHandle`] for `ffmpeg`.
pub type FfmpegHandle = ToolHandle<VerifiedFfmpeg>;

/// [`ToolHandle`] for `ffprobe`.
pub type FfprobeHandle = ToolHandle<VerifiedFfprobe>;

impl<V> ToolHandle<V> {
    /// Always `true`: a handle exists only for a catalog-verified binary.
    pub fn is_verified(&self) -> bool {
        matches!(self, Self::Verified(_))
    }
}

impl<V: FfmpegBinary> FfmpegBinary for ToolHandle<V> {
    fn ffmpeg_path(&self) -> &Path {
        match self {
            Self::Verified(verified) => verified.ffmpeg_path(),
        }
    }
}

/// The handle for one binary from its catalog verification result: `Verified` on success, the
/// error unchanged on failure.
fn verified_handle<V>(verified: Result<V, MediaError>) -> Result<ToolHandle<V>, MediaError> {
    verified.map(ToolHandle::Verified)
}

/// Verify the `ffmpeg` at `path` against the approved `variant` row for this host, and return
/// its handle.
pub fn verify_ffmpeg_handle(
    path: &Path,
    variant: &str,
    timeout: Duration,
) -> Result<FfmpegHandle, MediaError> {
    verified_handle(verify_bundled_ffmpeg(path, variant, timeout))
}

/// [`verify_ffmpeg_handle`] for an `ffprobe`.
pub fn verify_ffprobe_handle(
    path: &Path,
    variant: &str,
    timeout: Duration,
) -> Result<FfprobeHandle, MediaError> {
    verified_handle(verify_bundled_ffprobe(path, variant, timeout))
}

/// Where a bundled tool may sit, in lookup order, without touching the filesystem: first beside
/// the application executable (`<tool>` or `<tool>.exe`), then in `src-tauri/binaries` as
/// `<tool>-<target-triple>[.exe]`, under the executable's directory and each of its ancestors
/// (the checkout layout). The triple list is empty for a host RightKit does not
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
/// The crate reads no application environment variable itself.
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
    /// as a bundled one (catalog digest and build probe). When it does not, it is skipped, the reason is recorded in
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
/// so a pending row is reported as pending. An empty `variants` list is [`MediaError::MissingMetadata`].
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
///    order when that list is not empty ([`verify_first_of`]).
/// 2. With no bundled file, [`MediaError::ArtifactNotFound`].
fn resolve_handle<V>(
    tool: Tool,
    exe: &Path,
    variant: &str,
    options: &ResolveOptions,
    verify: impl Fn(&Path, &str, Duration) -> Result<V, MediaError>,
    timeout: Duration,
) -> Result<HandleResolution<V>, MediaError> {
    let variants: Vec<&str> = match options.variant_list() {
        [] => vec![variant],
        listed => listed.iter().map(String::as_str).collect(),
    };
    let mut skipped = Vec::new();
    if let Some(path) = options.override_path() {
        let outcome = if path.is_file() {
            verified_handle(verify_first_of(path, &variants, timeout, &verify))
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
        let handle = verified_handle(verify_first_of(&path, &variants, timeout, &verify))?;
        return Ok(HandleResolution { handle, skipped });
    }
    Err(MediaError::ArtifactNotFound)
}

/// `resolve_handle` for `ffmpeg`: bundled beside `exe` (or in `src-tauri/binaries`), verified
/// against the `variant` row.
pub fn resolve_ffmpeg_handle(
    exe: &Path,
    variant: &str,
    timeout: Duration,
) -> Result<FfmpegHandle, MediaError> {
    resolve_ffmpeg_handle_with(exe, variant, &ResolveOptions::default(), timeout)
        .map(|resolution| resolution.handle)
}

/// [`resolve_ffmpeg_handle`] with [`ResolveOptions`] (for example `override_first`), returning the
/// skipped candidates alongside the handle.
pub fn resolve_ffmpeg_handle_with(
    exe: &Path,
    variant: &str,
    options: &ResolveOptions,
    timeout: Duration,
) -> Result<HandleResolution<VerifiedFfmpeg>, MediaError> {
    resolve_handle(
        Tool::Ffmpeg,
        exe,
        variant,
        options,
        verify_bundled_ffmpeg,
        timeout,
    )
}

/// [`resolve_ffmpeg_handle`] for `ffprobe`.
pub fn resolve_ffprobe_handle(
    exe: &Path,
    variant: &str,
    timeout: Duration,
) -> Result<FfprobeHandle, MediaError> {
    resolve_ffprobe_handle_with(exe, variant, &ResolveOptions::default(), timeout)
        .map(|resolution| resolution.handle)
}

/// [`resolve_ffmpeg_handle_with`] for `ffprobe`.
pub fn resolve_ffprobe_handle_with(
    exe: &Path,
    variant: &str,
    options: &ResolveOptions,
    timeout: Duration,
) -> Result<HandleResolution<VerifiedFfprobe>, MediaError> {
    resolve_handle(
        Tool::Ffprobe,
        exe,
        variant,
        options,
        verify_bundled_ffprobe,
        timeout,
    )
}
