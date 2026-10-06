//! FFmpeg resolution for development hosts and end-to-end tests.
//!
//! Tries the approved catalog first (explicit path or `RIGHTKIT_FFMPEG`, digest +
//! LGPL build probe via [`crate::verify_ffmpeg`]). When the host's FFmpeg is not the
//! approved artifact (an engineer's Homebrew build, CutRight's editing build) it
//! returns a clearly-labelled [`DevFfmpeg`] instead. Production code must never call
//! this: it resolves through [`crate::resolve_ffmpeg`] with bundled paths only.

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
            let request = ResolutionRequest {
                override_path: None,
                bundled_paths: candidates.clone(),
                development_path: None,
                allow_development_path: false,
            };
            // `resolve_ffmpeg` checks each existing candidate's digest; the first that
            // exists decides, so mismatches surface as DigestMismatch below.
            let attempt = crate::resolve_ffmpeg(&artifact, &request)
                .and_then(|r| verify_ffmpeg(r, Duration::from_secs(10)));
            match attempt {
                Ok(verified) => {
                    let id = verified.artifact_id().to_string();
                    return Some((
                        DevResolved::Verified(verified),
                        ResolutionKind::CatalogVerified { artifact_id: id },
                    ));
                }
                Err(e) => format!("not the approved catalog artifact: {e}"),
            }
        }
        Err(e) => format!("no approved catalog artifact for this host: {e}"),
    };
    Some((
        DevResolved::Dev(DevFfmpeg::unverified(first.clone())),
        ResolutionKind::DevUnverified {
            path: first,
            reason: catalog_reason,
        },
    ))
}
