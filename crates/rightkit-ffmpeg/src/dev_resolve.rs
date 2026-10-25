//! FFmpeg resolution for development hosts and end-to-end tests.
//!
//! Every candidate (explicit `RIGHTKIT_FFMPEG`, the workspace's vendored catalog
//! artifact, Homebrew/system paths, CutRight's editing build) is checked against the
//! approved catalog (digest + LGPL build probe via [`crate::verify_ffmpeg`]); the first
//! one that verifies wins. When none is the approved artifact it returns the first
//! existing candidate as a clearly-labelled [`DevFfmpeg`]. Tests only: production code
//! resolves through [`crate::resolve_ffmpeg`] with bundled paths, or through
//! [`crate::unverified`] for an explicit, release-refused development fallback.

use std::{
    env,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{
    verify_ffmpeg, ArtifactCatalog, DevFfmpeg, FfmpegBinary, ResolutionRequest, VerifiedFfmpeg,
    APPROVED_CATALOG_JSON,
};

/// How the binary was obtained; tests and logs record it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolutionKind {
    /// Digest-pinned catalog artifact that passed the LGPL build probe.
    CatalogVerified { artifact_id: String },
    /// An FFmpeg that is not in the catalog (development only).
    DevUnverified { path: PathBuf, reason: String },
}

// Development-only resolution result, created once per test/tool run; boxing buys nothing.
#[allow(clippy::large_enum_variant)]
pub enum DevResolved {
    Verified(VerifiedFfmpeg),
    Dev(DevFfmpeg),
}

impl FfmpegBinary for DevResolved {
    fn ffmpeg_path(&self) -> &Path {
        match self {
            Self::Verified(v) => v.ffmpeg_path(),
            Self::Dev(d) => d.ffmpeg_path(),
        }
    }
}

fn host_os_arch() -> (&'static str, &'static str) {
    (env::consts::OS, env::consts::ARCH)
}

/// Candidate FFmpeg paths on a development host, most explicit first.
pub fn development_candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(p) = env::var("RIGHTKIT_FFMPEG") {
        v.push(PathBuf::from(p));
    }
    // The vendored approved artifact for this host, when the workspace is present.
    let vendored = match (env::consts::OS, env::consts::ARCH) {
        ("macos", "aarch64") => Some("vendor/ffmpeg-lgpl/ffmpeg-macos-arm64"),
        ("macos", "x86_64") => Some("vendor/ffmpeg-lgpl/ffmpeg-macos-x86_64"),
        ("windows", "x86_64") => Some("vendor/ffmpeg-lgpl/ffmpeg-win-x64.exe"),
        _ => None,
    };
    if let Some(rel) = vendored {
        let roots = env::var("RIGHTKIT_WORKSPACE_ROOT")
            .ok()
            .map(PathBuf::from)
            .into_iter()
            .chain([
                PathBuf::from("/Volumes/D/claude"),
                PathBuf::from("D:/Claude"),
            ]);
        v.extend(roots.map(|root| root.join(rel)));
    }
    for p in [
        "/opt/homebrew/bin/ffmpeg",
        "/usr/local/bin/ffmpeg",
        "/usr/bin/ffmpeg",
    ] {
        v.push(PathBuf::from(p));
    }
    // CutRight's editing build, relative to the workspace root when available.
    if let Ok(root) = env::var("RIGHTKIT_WORKSPACE_ROOT") {
        v.push(PathBuf::from(root).join(".cache/cutright-ffmpeg/mac/out/bin/ffmpeg"));
    }
    v.push(PathBuf::from(
        "/Volumes/D/claude/.cache/cutright-ffmpeg/mac/out/bin/ffmpeg",
    ));
    // CutRight's Windows build: `.cache/cutright-ffmpeg/win/<version>/bin/ffmpeg.exe`, newest first.
    if cfg!(windows) {
        let roots = env::var("RIGHTKIT_WORKSPACE_ROOT")
            .ok()
            .map(PathBuf::from)
            .into_iter()
            .chain([PathBuf::from("D:/Claude")]);
        for root in roots {
            let win = root.join(".cache/cutright-ffmpeg/win");
            let mut builds: Vec<PathBuf> = std::fs::read_dir(&win)
                .into_iter()
                .flatten()
                .flatten()
                .map(|entry| entry.path().join("bin").join("ffmpeg.exe"))
                .collect();
            builds.sort();
            v.extend(builds.into_iter().rev());
        }
    }
    v
}

/// Resolve FFmpeg: catalog-verified when a candidate matches the approved digest,
/// otherwise the first existing candidate as [`DevFfmpeg`]. `None` when no FFmpeg
/// exists on the host.
pub fn resolve_for_development() -> Option<(DevResolved, ResolutionKind)> {
    let candidates: Vec<PathBuf> = development_candidates()
        .into_iter()
        .filter(|p| p.is_file())
        .collect();
    let first = candidates.first()?.clone();
    let (os, arch) = host_os_arch();
    let catalog_reason = match ArtifactCatalog::parse(APPROVED_CATALOG_JSON)
        .and_then(|c| c.approved(os, arch).cloned())
    {
        Ok(artifact) => {
            let mut first_error = None;
            for candidate in &candidates {
                let request = ResolutionRequest {
                    override_path: None,
                    bundled_paths: vec![candidate.clone()],
                    development_path: None,
                    allow_development_path: false,
                };
                match crate::resolve_ffmpeg(&artifact, &request)
                    .and_then(|r| verify_ffmpeg(r, Duration::from_secs(10)))
                {
                    Ok(verified) => {
                        let id = verified.artifact_id().to_string();
                        return Some((
                            DevResolved::Verified(verified),
                            ResolutionKind::CatalogVerified { artifact_id: id },
                        ));
                    }
                    Err(e) => {
                        first_error.get_or_insert(e);
                    }
                }
            }
            match first_error {
                Some(e) => format!("not the approved catalog artifact: {e}"),
                None => "no candidate".to_owned(),
            }
        }
        Err(e) => format!("no approved catalog artifact for this host: {e}"),
    };
    Some((
        DevResolved::Dev(DevFfmpeg::mode_gated(first.clone())),
        ResolutionKind::DevUnverified {
            path: first,
            reason: catalog_reason,
        },
    ))
}
