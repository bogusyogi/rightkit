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

use crate::{sha256_file, Artifact, MediaError, VerifiedFfmpeg, VerifiedFfprobe};

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
    pub sha256: String,
    pub artifact_id: String,
    /// `false` when identical bytes were already staged.
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
    let expected = artifact.approved_sha256()?.to_ascii_lowercase();
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
    if !sha256_file(source)?.eq_ignore_ascii_case(&expected) {
        return Err(MediaError::DigestMismatch {
            path: source.to_owned(),
        });
    }
    let dest = external_bin_path(binaries_dir, artifact.tool.stem(), triple);
    fs::create_dir_all(binaries_dir)?;
    let already = dest.is_file() && sha256_file(&dest)?.eq_ignore_ascii_case(&expected);
    if !already {
        let tmp = binaries_dir.join(format!(
            ".{}.staging-{}",
            dest.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            std::process::id()
        ));
        let copied = fs::copy(source, &tmp).and_then(|_| {
            set_executable(&tmp)?;
            replace_file(&tmp, &dest)
        });
        if let Err(error) = copied {
            let _ = fs::remove_file(&tmp);
            return Err(error.into());
        }
        let staged = sha256_file(&dest)?;
        if !staged.eq_ignore_ascii_case(&expected) {
            let _ = fs::remove_file(&dest);
            return Err(MediaError::DigestMismatch { path: dest });
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

fn replace_file(tmp: &Path, dest: &Path) -> io::Result<()> {
    #[cfg(windows)]
    if dest.exists() {
        // rename does not replace an existing file on every Windows filesystem.
        fs::remove_file(dest)?;
    }
    fs::rename(tmp, dest)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::{
        resolve_ffmpeg, ArtifactStatus, Attribution, ResolutionRequest, ResolvedFfmpeg, Tool,
        DECODE_VARIANT,
    };

    fn script_artifact(digest: &str, os: &str, arch: &str, tool: Tool) -> Artifact {
        Artifact {
            id: format!("{tool}-test-{os}-{arch}"),
            tool,
            variant: DECODE_VARIANT.into(),
            os: os.into(),
            arch: arch.into(),
            version: "7.1.5".into(),
            status: ArtifactStatus::Approved,
            sha256: Some(digest.into()),
            source_sha256: "de668509caf9e35e3cd162473441fdb29538c6d96ed080292b3cf9e6fc5d558f"
                .into(),
            attribution: Attribution {
                license_spdx: "LGPL-2.1-or-later".into(),
                source_url: "https://ffmpeg.org/releases/ffmpeg-7.1.5.tar.xz".into(),
                build_provenance: "BUILD-NOTES.txt".into(),
                notice_files: vec!["LGPL-2.1.txt".into(), "BUILD-NOTES.txt".into()],
                components: vec![],
            },
            required_configure_flags: vec![
                "--disable-gpl".into(),
                "--disable-nonfree".into(),
                "--disable-autodetect".into(),
            ],
            pending: None,
        }
    }

    /// A fake tool that answers `-version` like an approved build, so the real
    /// verification path (digest + build probe) runs end to end.
    #[cfg(unix)]
    fn fake_verified(dir: &Path, tool: Tool) -> (PathBuf, Artifact) {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(format!("{tool}-src"));
        fs::write(
            &path,
            format!(
                "#!/bin/sh\necho '{tool} version 7.1.5'\necho 'configuration: --disable-gpl --disable-nonfree --disable-autodetect'\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let digest = sha256_file(&path).unwrap();
        let (os, arch) = match std::env::consts::ARCH {
            "aarch64" => ("macos", "aarch64"),
            _ => ("macos", "x86_64"),
        };
        (path, script_artifact(&digest, os, arch, tool))
    }

    fn resolve(path: &Path, artifact: &Artifact) -> ResolvedFfmpeg {
        resolve_ffmpeg(
            artifact,
            &ResolutionRequest {
                override_path: Some(path.to_owned()),
                bundled_paths: vec![],
                development_path: None,
                allow_development_path: false,
            },
        )
        .unwrap()
    }

    #[test]
    fn external_bin_names_follow_the_tauri_convention() {
        let dir = Path::new("src-tauri/binaries");
        assert_eq!(
            external_bin_path(dir, "ffmpeg", "aarch64-apple-darwin"),
            dir.join("ffmpeg-aarch64-apple-darwin")
        );
        assert_eq!(
            external_bin_path(dir, "ffprobe", "x86_64-pc-windows-msvc"),
            dir.join("ffprobe-x86_64-pc-windows-msvc.exe")
        );
        assert_eq!(
            tauri_target_triple("macos", "x86_64"),
            Some("x86_64-apple-darwin")
        );
        assert_eq!(
            tauri_target_triple("windows", "x86_64"),
            Some("x86_64-pc-windows-msvc")
        );
        assert_eq!(tauri_target_triple("plan9", "mips"), None);
    }

    #[cfg(unix)]
    #[test]
    fn staging_copies_verified_bytes_with_0755_and_is_idempotent() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        for tool in [Tool::Ffmpeg, Tool::Ffprobe] {
            let (src, artifact) = fake_verified(dir.path(), tool);
            let resolved = resolve(&src, &artifact);
            let binaries = dir.path().join("src-tauri/binaries");
            let first = match tool {
                Tool::Ffmpeg => {
                    let v = crate::verify_ffmpeg(resolved, Duration::from_secs(5)).unwrap();
                    let staged = stage_external_bin(&v, &binaries, None).unwrap();
                    // Second run: identical bytes, nothing rewritten.
                    let again = stage_external_bin(&v, &binaries, None).unwrap();
                    assert!(!again.changed);
                    // Permissions drift is repaired without a copy.
                    fs::set_permissions(&staged.path, fs::Permissions::from_mode(0o600)).unwrap();
                    let repaired = stage_external_bin(&v, &binaries, None).unwrap();
                    assert!(!repaired.changed);
                    // Tampered destination is replaced.
                    fs::write(&staged.path, b"tampered").unwrap();
                    let replaced = stage_external_bin(&v, &binaries, None).unwrap();
                    assert!(replaced.changed);
                    staged
                }
                Tool::Ffprobe => {
                    let v = crate::verify_ffprobe(resolved, Duration::from_secs(5)).unwrap();
                    stage_external_bin(&v, &binaries, None).unwrap()
                }
            };
            assert!(first.changed);
            let triple = tauri_target_triple(&artifact.os, &artifact.arch).unwrap();
            assert_eq!(first.path, binaries.join(format!("{tool}-{triple}")));
            assert_eq!(fs::read(&first.path).unwrap(), fs::read(&src).unwrap());
            let mode = fs::metadata(&first.path).unwrap().permissions().mode() & 0o7777;
            assert_eq!(mode, 0o755);
            assert_eq!(first.sha256, artifact.sha256.clone().unwrap());
            // No temporary files are left behind.
            let leftovers: Vec<_> = fs::read_dir(&binaries)
                .unwrap()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().contains(".staging-"))
                .collect();
            assert!(leftovers.is_empty());
        }
    }

    #[cfg(unix)]
    #[test]
    fn staging_refuses_a_foreign_triple_and_a_source_changed_after_verification() {
        let dir = tempfile::tempdir().unwrap();
        let (src, artifact) = fake_verified(dir.path(), Tool::Ffmpeg);
        let verified =
            crate::verify_ffmpeg(resolve(&src, &artifact), Duration::from_secs(5)).unwrap();
        let binaries = dir.path().join("binaries");
        let foreign = if artifact.arch == "aarch64" {
            "x86_64-apple-darwin"
        } else {
            "aarch64-apple-darwin"
        };
        assert!(matches!(
            stage_external_bin(&verified, &binaries, Some(foreign)),
            Err(MediaError::StagingFailed(_))
        ));
        fs::write(&src, b"#!/bin/sh\necho swapped\n").unwrap();
        assert!(matches!(
            stage_external_bin(&verified, &binaries, None),
            Err(MediaError::DigestMismatch { .. })
        ));
        assert!(!binaries
            .join(format!(
                "ffmpeg-{}",
                tauri_target_triple(&artifact.os, &artifact.arch).unwrap()
            ))
            .exists());
    }
}
