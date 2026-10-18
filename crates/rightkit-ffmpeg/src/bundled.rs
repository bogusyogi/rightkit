//! Verify a bundled binary once, keep the handle, pass it everywhere.
//!
//! An app that ships `ffmpeg` and `ffprobe` inside its bundle knows the file path, the tool,
//! and the catalog variant it expects. These calls turn that into a [`VerifiedFfmpeg`] or
//! [`VerifiedFfprobe`] in one step: select the single approved catalog row for the tool,
//! variant, and this host's OS and architecture, check the file's content digest against the
//! row, then run the `-version` build probe (LGPL banner, required configure flags). The
//! handles are `Clone`, implement [`crate::FfmpegBinary`], and are accepted by every run,
//! probe, decode, loudness, and staging call, so verification happens at start-up and nowhere
//! else.
//!
//! Verification goes through the process-wide verify cache ([`crate::verify_cache_clear`]):
//! asking again for the same unchanged file neither re-hashes nor re-runs `-version`, so a
//! caller that cannot keep the handle still pays the cost once.
//!
//! The path is the only candidate. There is no development fallback and no skip: a missing
//! file is [`MediaError::ArtifactNotFound`], a file that is not the approved build is
//! [`MediaError::DigestMismatch`], and a variant with no approved row for this platform is
//! [`MediaError::NoApprovedArtifact`] (or [`MediaError::ArtifactPending`] when its row exists
//! but is not approved yet). Use [`MediaError::is_not_in_catalog`] to decide whether a
//! development fallback is even permitted.

use std::{env, path::Path, time::Duration};

use crate::{
    resolve_ffmpeg, verify_ffmpeg, verify_ffprobe, ArtifactCatalog, MediaError, ResolutionRequest,
    Tool, VerifiedFfmpeg, VerifiedFfprobe,
};

fn only(path: &Path) -> ResolutionRequest {
    ResolutionRequest {
        override_path: Some(path.to_owned()),
        bundled_paths: vec![],
        development_path: None,
        allow_development_path: false,
    }
}

/// Verify the bundled `ffmpeg` at `path` against the embedded catalog's approved `variant`
/// row for this host, and return the handle. `timeout` bounds the `-version` probe.
///
/// ```no_run
/// use rightkit_ffmpeg::{
///     verify_bundled_ffmpeg, verify_bundled_ffprobe,
///     DECODE_APPLE_VARIANT, EXPORT_ASS_ZIMG_VARIANT,
/// };
/// use std::time::Duration;
/// # let bundle = std::path::Path::new("/path/to/app/binaries");
/// let ffmpeg = verify_bundled_ffmpeg(&bundle.join("ffmpeg"), EXPORT_ASS_ZIMG_VARIANT, Duration::from_secs(10))?;
/// let ffprobe = verify_bundled_ffprobe(&bundle.join("ffprobe"), DECODE_APPLE_VARIANT, Duration::from_secs(10))?;
/// // pass `&ffmpeg` / `&ffprobe` to run_checked, probe_media, decode_pcm_f32, ...
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn verify_bundled_ffmpeg(
    path: &Path,
    variant: &str,
    timeout: Duration,
) -> Result<VerifiedFfmpeg, MediaError> {
    verify_bundled_ffmpeg_in(
        &ArtifactCatalog::embedded()?,
        env::consts::OS,
        env::consts::ARCH,
        path,
        variant,
        timeout,
    )
}

/// [`verify_bundled_ffmpeg`] for an `ffprobe`.
pub fn verify_bundled_ffprobe(
    path: &Path,
    variant: &str,
    timeout: Duration,
) -> Result<VerifiedFfprobe, MediaError> {
    verify_bundled_ffprobe_in(
        &ArtifactCatalog::embedded()?,
        env::consts::OS,
        env::consts::ARCH,
        path,
        variant,
        timeout,
    )
}

/// [`verify_bundled_ffmpeg`] against an explicit catalog and platform (`os` is `macos` or
/// `windows`; `arch` is `aarch64` or `x86_64`), for tests and for tools that stage another
/// platform's binary. The `-version` probe still executes the file, so a foreign-architecture
/// binary fails the probe here.
pub fn verify_bundled_ffmpeg_in(
    catalog: &ArtifactCatalog,
    os: &str,
    arch: &str,
    path: &Path,
    variant: &str,
    timeout: Duration,
) -> Result<VerifiedFfmpeg, MediaError> {
    let artifact = catalog.approved_for(Tool::Ffmpeg, variant, os, arch)?;
    verify_ffmpeg(resolve_ffmpeg(artifact, &only(path))?, timeout)
}

/// [`verify_bundled_ffprobe`] against an explicit catalog and platform; see
/// [`verify_bundled_ffmpeg_in`].
pub fn verify_bundled_ffprobe_in(
    catalog: &ArtifactCatalog,
    os: &str,
    arch: &str,
    path: &Path,
    variant: &str,
    timeout: Duration,
) -> Result<VerifiedFfprobe, MediaError> {
    let artifact = catalog.approved_for(Tool::Ffprobe, variant, os, arch)?;
    verify_ffprobe(resolve_ffmpeg(artifact, &only(path))?, timeout)
}
