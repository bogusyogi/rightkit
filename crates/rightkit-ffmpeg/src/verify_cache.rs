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
