//! Process-lifetime cache for catalog verification: the whole-file digest and the
//! `-version` build probe that [`crate::resolve_ffmpeg`] and [`crate::verify_ffmpeg`]
//! run on every call.
//!
//! An entry is keyed by the canonical path, size, modification time, file identity
//! (device and inode on Unix), the artifact id, and the expected digest. Any change to
//! any of those misses the cache and re-verifies. On Windows the identity is `None`
//! because the file index is not available through stable `std`, so a file replaced in
//! place with identical size and mtime is not detected there; call
//! [`verify_cache_clear`] after such a replacement.
//!
//! The cache only remembers successes. A failed digest or probe is recomputed next time.

use std::{
    cell::Cell,
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex, MutexGuard},
    time::SystemTime,
};

use crate::{Artifact, MediaError};

thread_local! {
    static DIGEST_COMPUTATIONS: Cell<u64> = const { Cell::new(0) };
}

/// Whole-file digests that catalog verification has computed on the calling thread.
/// Diagnostics and tests use it to prove a cache hit skipped the hash. Counts are per
/// thread, so concurrent verifications on other threads do not affect the result.
pub fn digest_computations() -> u64 {
    DIGEST_COMPUTATIONS.with(Cell::get)
}

pub(crate) fn record_digest_computation() {
    DIGEST_COMPUTATIONS.with(|count| count.set(count.get() + 1));
}

/// Forget every cached verification. The next resolve or verify of any binary runs the
/// digest and the build probe again. Call it after replacing a binary in a way the key
/// cannot see (see the module docs for the Windows case).
pub fn verify_cache_clear() {
    lock().clear();
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct VerifyKey {
    path: PathBuf,
    size: u64,
    modified: Option<SystemTime>,
    identity: Option<(u64, u64)>,
    artifact_id: String,
    expected_sha256: String,
}

/// Which checks have passed for one key. Both default to "not yet".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct VerifyEntry {
    pub(crate) digest_ok: bool,
    pub(crate) probe_ok: bool,
}

static CACHE: LazyLock<Mutex<HashMap<VerifyKey, VerifyEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn lock() -> MutexGuard<'static, HashMap<VerifyKey, VerifyEntry>> {
    // A panic while holding the lock cannot leave the map inconsistent, so recover it.
    CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Build the key from the file's current metadata. Call it before hashing: a write that
/// lands during the hash then changes the key, so the stale result cannot be reused.
pub(crate) fn key_for(
    path: &Path,
    artifact: &Artifact,
    expected_sha256: &str,
) -> Result<VerifyKey, MediaError> {
    let metadata = fs::metadata(path)?;
    Ok(VerifyKey {
        path: fs::canonicalize(path)?,
        size: metadata.len(),
        modified: metadata.modified().ok(),
        identity: file_identity(&metadata),
        artifact_id: artifact.id.clone(),
        expected_sha256: expected_sha256.to_ascii_lowercase(),
    })
}

#[cfg(unix)]
fn file_identity(metadata: &fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn file_identity(_metadata: &fs::Metadata) -> Option<(u64, u64)> {
    None
}

pub(crate) fn lookup(key: &VerifyKey) -> VerifyEntry {
    lock().get(key).copied().unwrap_or_default()
}

pub(crate) fn remember(key: VerifyKey, update: impl FnOnce(&mut VerifyEntry)) {
    update(lock().entry(key).or_default());
}

#[cfg(all(test, unix))]
mod tests {
    use std::{
        fs,
        io::Write,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        time::{Duration, SystemTime},
    };

    use crate::{
        digest_computations, resolve_ffmpeg, sha256_file, verify_cache_clear, verify_ffmpeg,
        Artifact, ArtifactStatus, Attribution, MediaError, ResolutionRequest, Tool, VerifiedFfmpeg,
        DECODE_VARIANT,
    };

    const T: Duration = Duration::from_secs(20);

    /// The cache is process-wide, so these tests run one at a time.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A stand-in `ffmpeg` that passes the build probe and records each run in `marker`.
    fn fake_ffmpeg(dir: &Path, marker: &Path) -> PathBuf {
        let path = dir.join("fake-ffmpeg");
        write_script(&path, marker, "fixture-one");
        path
    }

    fn write_script(path: &Path, marker: &Path, tag: &str) {
        let body = format!(
            "#!/bin/sh\necho probe >> '{}'\nprintf 'ffmpeg version 7.1.5 {tag}\\nconfiguration: --disable-gpl --disable-nonfree --disable-autodetect\\n'\n",
            marker.display()
        );
        let mut file = fs::File::create(path).unwrap();
        file.write_all(body.as_bytes()).unwrap();
        file.sync_all().unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn artifact_for(digest: &str) -> Artifact {
        Artifact {
            id: "ffmpeg-cache-fixture".into(),
            tool: Tool::Ffmpeg,
            variant: DECODE_VARIANT.into(),
            os: "macos".into(),
            arch: "aarch64".into(),
            version: "7.1.5".into(),
            status: ArtifactStatus::Approved,
            sha256: Some(digest.into()),
            content_sha256: Some(digest.into()),
            size: None,
            source_sha256: "de668509caf9e35e3cd162473441fdb29538c6d96ed080292b3cf9e6fc5d558f"
                .into(),
            attribution: Attribution {
                license_spdx: "LGPL-2.1-or-later".into(),
                source_url: "https://ffmpeg.org/releases/ffmpeg-7.1.5.tar.xz".into(),
                build_provenance: "BUILD-NOTES.txt".into(),
                notice_files: vec!["COPYING.LGPLv2.1".into(), "BUILD-NOTES.txt".into()],
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

    fn resolve_and_verify(artifact: &Artifact, path: &Path) -> Result<VerifiedFfmpeg, MediaError> {
        let resolved = resolve_ffmpeg(
            artifact,
            &ResolutionRequest {
                override_path: Some(path.to_owned()),
                bundled_paths: vec![],
                development_path: None,
                allow_development_path: false,
            },
        )?;
        verify_ffmpeg(resolved, T)
    }

    fn marker_runs(marker: &Path) -> usize {
        fs::read_to_string(marker).map_or(0, |text| text.lines().count())
    }

    #[test]
    fn a_second_verification_of_the_same_binary_hashes_and_probes_nothing() {
        let _serial = SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("probe-runs");
        let bin = fake_ffmpeg(dir.path(), &marker);
        let artifact = artifact_for(&sha256_file(&bin).unwrap());
        verify_cache_clear();

        let before = digest_computations();
        let first = resolve_and_verify(&artifact, &bin).unwrap();
        assert_eq!(digest_computations() - before, 1);
        assert_eq!(marker_runs(&marker), 1);

        let before = digest_computations();
        let second = resolve_and_verify(&artifact, &bin).unwrap();
        assert_eq!(digest_computations() - before, 0, "cache hit must not hash");
        assert_eq!(marker_runs(&marker), 1, "cache hit must not run -version");
        assert_eq!(first.path(), second.path());
    }

    #[test]
    fn a_touched_file_is_verified_again_and_a_rewritten_file_is_rejected() {
        let _serial = SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("probe-runs");
        let bin = fake_ffmpeg(dir.path(), &marker);
        let artifact = artifact_for(&sha256_file(&bin).unwrap());
        verify_cache_clear();
        resolve_and_verify(&artifact, &bin).unwrap();

        // Same bytes, new mtime: the key changes, so the digest and probe run again.
        let later = SystemTime::now() + Duration::from_secs(86_400);
        fs::File::options()
            .write(true)
            .open(&bin)
            .unwrap()
            .set_modified(later)
            .unwrap();
        let before = digest_computations();
        resolve_and_verify(&artifact, &bin).unwrap();
        assert_eq!(digest_computations() - before, 1, "touch must re-hash");
        assert_eq!(marker_runs(&marker), 2, "touch must re-probe");

        // Different bytes under the same path: the digest no longer matches.
        let _ = fs::remove_file(&bin);
        write_script(&bin, &marker, "tampered-build");
        assert!(matches!(
            resolve_and_verify(&artifact, &bin),
            Err(MediaError::DigestMismatch { .. })
        ));
    }

    #[test]
    fn clearing_the_cache_forces_a_fresh_verification() {
        let _serial = SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("probe-runs");
        let bin = fake_ffmpeg(dir.path(), &marker);
        let artifact = artifact_for(&sha256_file(&bin).unwrap());
        verify_cache_clear();
        resolve_and_verify(&artifact, &bin).unwrap();

        verify_cache_clear();
        let before = digest_computations();
        resolve_and_verify(&artifact, &bin).unwrap();
        assert_eq!(digest_computations() - before, 1);
        assert_eq!(marker_runs(&marker), 2);
    }
}
