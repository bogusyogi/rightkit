//! Stage a catalog-verified binary into a Tauri `externalBin` layout.
//!
//! Tauri resolves `"externalBin": ["binaries/ffmpeg"]` to
//! `src-tauri/binaries/ffmpeg-<target-triple>[.exe]` at bundle time. This module
//! writes exactly that file from a [`VerifiedFfmpeg`] or [`VerifiedFfprobe`]:
//! the source digest is re-checked against the catalog immediately before the copy,
//! the copy lands through a temporary file and an atomic rename, the result is
//! re-hashed, and Unix permissions are set to `0755`. Re-staging identical bytes is a
//! no-op apart from repairing permissions, so a `beforeBuildCommand` can call it on
//! every build.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use crate::{Artifact, MediaError, VerifiedFfmpeg, VerifiedFfprobe};

/// A binary that passed catalog verification and may be staged.
pub trait VerifiedArtifact {
    fn verified_path(&self) -> &Path;
    fn verified_artifact(&self) -> &Artifact;
}

impl VerifiedArtifact for VerifiedFfmpeg {
    fn verified_path(&self) -> &Path {
        self.path()
    }
    fn verified_artifact(&self) -> &Artifact {
        self.artifact()
    }
}

impl VerifiedArtifact for VerifiedFfprobe {
    fn verified_path(&self) -> &Path {
        self.path()
    }
    fn verified_artifact(&self) -> &Artifact {
        self.artifact()
    }
}

/// Rust target triple Tauri uses for a catalog `os`/`arch` pair.
pub fn tauri_target_triple(os: &str, arch: &str) -> Option<&'static str> {
    match (
        os.to_ascii_lowercase().as_str(),
        arch.to_ascii_lowercase().as_str(),
    ) {
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        ("windows", "aarch64") => Some("aarch64-pc-windows-msvc"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-gnu"),
        _ => None,
    }
}

/// `<binaries_dir>/<stem>-<triple>` plus `.exe` for Windows triples.
pub fn external_bin_path(binaries_dir: &Path, stem: &str, target_triple: &str) -> PathBuf {
    let exe = if target_triple.contains("windows") {
        ".exe"
    } else {
        ""
    };
    binaries_dir.join(format!("{stem}-{target_triple}{exe}"))
}

/// Result of [`stage_external_bin`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedBinary {
    pub path: PathBuf,
    /// The artifact's content digest (signature-independent), not a whole-file hash.
    pub sha256: String,
    pub artifact_id: String,
    /// `false` when a destination with the same content identity was already staged.
    pub changed: bool,
}

/// Copy a verified binary to `<binaries_dir>/<tool>-<triple>[.exe]`.
///
/// `target_triple` defaults to the triple of the artifact's own platform; a different
/// triple is refused so an arm64 binary can never be staged under an x86_64 name.
pub fn stage_external_bin(
    verified: &dyn VerifiedArtifact,
    binaries_dir: &Path,
    target_triple: Option<&str>,
) -> Result<StagedBinary, MediaError> {
    let artifact = verified.verified_artifact();
    let expected = artifact.approved_content_sha256()?.to_ascii_lowercase();
    let own_triple = tauri_target_triple(&artifact.os, &artifact.arch).ok_or_else(|| {
        MediaError::StagingFailed(format!(
            "no Tauri target triple for {}/{}",
            artifact.os, artifact.arch
        ))
    })?;
    let triple = target_triple.unwrap_or(own_triple);
    if triple != own_triple {
        return Err(MediaError::StagingFailed(format!(
            "{} is built for {own_triple}, not {triple}",
            artifact.id
        )));
    }
    let source = verified.verified_path();
    // The file may have changed since verification; never stage unverified bytes.
    crate::verify_content(artifact, source)?;
    let dest = external_bin_path(binaries_dir, artifact.tool.stem(), triple);
    fs::create_dir_all(binaries_dir)?;
    let already = dest.is_file() && crate::verify_content(artifact, &dest).is_ok();
    if !already {
        // Temp file in the destination directory, fsync, atomic rename (rightkit-fs); the
        // mode is set before the rename so `dest` is never visible half-staged.
        let copied =
            rightkit_fs::atomic_write_with(&dest, rightkit_fs::AtomicOptions::default(), |out| {
                let mut input = fs::File::open(source)?;
                io::copy(&mut input, out)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    out.set_permissions(fs::Permissions::from_mode(0o755))?;
                }
                Ok(())
            });
        if let Err(error) = copied {
            return Err(MediaError::StagingFailed(format!(
                "{}: {error}",
                dest.display()
            )));
        }
        if let Err(error) = crate::verify_content(artifact, &dest) {
            let _ = fs::remove_file(&dest);
            return Err(error);
        }
    }
    set_executable(&dest)?;
    Ok(StagedBinary {
        path: dest,
        sha256: expected,
        artifact_id: artifact.id.clone(),
        changed: !already,
    })
}

#[cfg(unix)]
fn set_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(path)?.permissions().mode() & 0o7777;
    if mode != 0o755 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(path: &Path) -> io::Result<()> {
    // Windows executability comes from the `.exe` name; only clear read-only so the
    // next staging run can replace the file.
    let mut permissions = fs::metadata(path)?.permissions();
    if permissions.readonly() {
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}
