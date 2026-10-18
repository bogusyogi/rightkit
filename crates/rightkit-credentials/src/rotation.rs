//! Pending-rotation recovery for a credential that other state depends on (for example a
//! database key: the database is re-keyed with the new value, then the credential is updated).
//! A crash between the two steps must leave the new value recoverable.
//!
//! The marker is a second item in the same namespace named `<account>.rotation-pending` that
//! holds the NEW value. Sequence: [`Store::begin_rotation`] (marker written create-only), the
//! caller applies the new value to whatever depends on it, then [`Store::commit_rotation_of`]
//! (primary set, marker removed). After a crash the caller finds the marker with
//! [`Store::pending_rotation`] and either commits or aborts. Commit is idempotent: if it dies
//! after the primary write, running it again sets the same value and clears the marker.
//!
//! ## Exclusion
//!
//! `begin`, `commit` and `abort` all run under one whole-rotation lock for the account in its
//! bound store. Each invocation pins one store before taking its lock, then uses that store for
//! every read and mutation. Retain [`Store::pinned`] across begin, external rekeying, and commit
//! or abort to keep separate invocations on that same store. Ordinary handles resolve the default
//! again; after a restart reselect the original default keychain before committing recovery
//! (production explicit-file macOS backends are read/delete-only).
//!
//! The lock has two layers, both taken before the first read and held until the last write of
//! the invocation:
//!
//! * a process-local mutex, shared by every `Store` that reaches the same native credential;
//! * a lock file, one per (store, credential) in the directory given by
//!   [`crate::Config::rotation_lock_dir`], that excludes every other PROCESS of the user. A store
//!   whose backend is durable (an OS store) refuses to rotate without that directory, because a
//!   per-process lock cannot stop an abort/rebegin in another process from landing between
//!   commit's pending read and its primary write (a native per-operation lock cannot either:
//!   commit is three native calls). Non-durable (memory, test) backends need no directory.
//!
//! Waiting for either layer is bounded by the store timeout and ends in [`Error::InProgress`].
//!
//! ## Generation check
//!
//! The lock makes each invocation atomic; it cannot tell that the rotation a caller began was
//! replaced between two invocations (another process aborts it and begins its own while the
//! first rekeys). A rotation's generation is its new value, which is a fresh random key.
//! [`Store::commit_rotation_of`] and [`Store::abort_rotation_of`] name the value the caller
//! began and act only while that value is still the pending one: a stale commit is refused with
//! `Error::Invalid { what: "pending rotation", .. }` and cannot overwrite a newer primary or
//! remove a newer marker, even for a caller that skipped the lock directory on a non-durable store.
//! [`Store::commit_rotation`] and [`Store::abort_rotation`] act on whatever is pending and are
//! meant for recovery after a restart, when the caller has no value of its own.

use crate::{ConditionalDelete, Error, Result, Secret, Store};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

/// Suffix of the marker item's account name.
pub const ROTATION_PENDING_SUFFIX: &str = ".rotation-pending";

/// How often a contended rotation lock is retried. The wait ends at the store timeout.
const POLL: Duration = Duration::from_millis(5);

fn marker(account: &str) -> String {
    format!("{account}{ROTATION_PENDING_SUFFIX}")
}

fn superseded() -> Error {
    Error::Invalid {
        what: "pending rotation",
        reason: "superseded: the rotation this caller began is no longer the pending one \
                 (it was aborted, replaced or committed by another caller)"
            .into(),
    }
}

fn unavailable(reason: &str) -> Error {
    Error::Unavailable(reason.into())
}

/// Retry `attempt` until it yields a value, fails, or `deadline` passes (`Error::InProgress`).
/// One attempt is always made, however short the deadline.
fn poll<T>(deadline: Instant, mut attempt: impl FnMut() -> Result<Option<T>>) -> Result<T> {
    loop {
        if let Some(value) = attempt()? {
            return Ok(value);
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(Error::InProgress { op: "rotation" });
        }
        std::thread::sleep(POLL.min(deadline - now));
    }
}

/// FNV-1a, 64 bit, from `seed`. Hand-rolled because the lock file name must be identical in
/// every process and every build (`DefaultHasher` promises neither). A collision only makes two
/// credentials share a lock; it never lets two holders in.
fn fnv1a(seed: u64, parts: &[&str]) -> u64 {
    let mut hash = seed;
    for part in parts {
        // Length-prefixed, so ("ab", "c") and ("a", "bc") differ.
        for byte in (part.len() as u64)
            .to_le_bytes()
            .into_iter()
            .chain(part.bytes())
        {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

/// The lock file name for one credential of one store.
pub(crate) fn lock_file_name(backend: &str, store: &str, credential: &str) -> String {
    let parts = [backend, store, credential];
    format!(
        "rotation-{:016x}{:016x}.lock",
        fnv1a(0xcbf2_9ce4_8422_2325, &parts),
        fnv1a(0x9e37_79b9_7f4a_7c15, &parts)
    )
}

/// The whole-rotation lock of one credential: the process-local mutex and, when the store has a
/// lock directory, the cross-process lock file.
pub(crate) struct RotationLock {
    local: Arc<Mutex<()>>,
    file: Option<PathBuf>,
    timeout: Duration,
}

/// Held for one begin, commit or abort; both layers release on drop.
pub(crate) struct RotationHeld<'a> {
    _file: Option<File>,
    _local: MutexGuard<'a, ()>,
}

impl RotationLock {
    pub(crate) fn new(local: Arc<Mutex<()>>, file: Option<PathBuf>, timeout: Duration) -> Self {
        Self {
            local,
            file,
            timeout,
        }
    }

    /// Take the process-local mutex, then the lock file. Both waits share one deadline of the
    /// store timeout. The mutex is always taken first, so two threads of one process queue on it
    /// and only one of them contends for the file.
    pub(crate) fn acquire(&self) -> Result<RotationHeld<'_>> {
        let now = Instant::now();
        let deadline = now
            .checked_add(self.timeout)
            .unwrap_or_else(|| now + Duration::from_secs(365 * 24 * 60 * 60));
        let local = poll(deadline, || match self.local.try_lock() {
            Ok(guard) => Ok(Some(guard)),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Poisoned(_)) => Err(unavailable("rotation lock poisoned")),
        })?;
        let file = match &self.file {
            Some(path) => Some(lock_file(path, deadline)?),
            None => None,
        };
        Ok(RotationHeld {
            _file: file,
            _local: local,
        })
    }
}

/// Create the lock directory if needed and require that it is closed to group and others (Unix;
/// on Windows the caller supplies a per-user directory).
fn prepare_dir(dir: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .map_err(|_| unavailable("cannot create the rotation lock directory"))?;
    let meta = std::fs::metadata(dir)
        .map_err(|_| unavailable("cannot inspect the rotation lock directory"))?;
    if !meta.is_dir() {
        return Err(unavailable(
            "the rotation lock directory is not a directory",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(unavailable(
                "the rotation lock directory is not private to this user",
            ));
        }
    }
    Ok(())
}

fn open_lock_file(path: &Path) -> Result<File> {
    if let Some(dir) = path.parent() {
        prepare_dir(dir)?;
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|_| unavailable("cannot open the rotation lock file"))
}

/// Exclusive advisory lock on `path` (`flock` on Unix, `LockFileEx` on Windows through std),
/// released when the returned file is dropped or the process dies. Every open of the file is its
/// own lock holder, so two stores of one process exclude each other here as well.
fn lock_file(path: &Path, deadline: Instant) -> Result<File> {
    let file = open_lock_file(path)?;
    poll(deadline, || match file.try_lock() {
        Ok(()) => Ok(Some(())),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) if e.kind() == std::io::ErrorKind::Unsupported => {
            Err(unavailable("this platform has no file locks for rotation"))
        }
        Err(std::fs::TryLockError::Error(e)) => Err(Error::Backend {
            op: "rotation lock",
            code: e.raw_os_error().map_or(-1, i64::from),
        }),
    })?;
    Ok(file)
}

/// Remove `value`'s marker. `Ok(true)` when this call removed it. Runs under the rotation lock.
/// Remove only the marker that was read: `Changed` means another writer put a different marker
/// there, which stays. Backends without an atomic conditional delete report `Unsupported`; the
/// plain delete is then still serialised against every rotation call by the lock.
fn remove_marker(store: &Store, account: &str, value: &Secret) -> Result<bool> {
    match store.delete_if_equals(&marker(account), value)? {
        ConditionalDelete::Deleted => Ok(true),
        ConditionalDelete::Absent | ConditionalDelete::Changed => Ok(false),
        ConditionalDelete::Unsupported => store.delete(&marker(account)),
    }
}

/// Commit body; the caller holds the rotation lock. `expected` is the value the caller began,
/// or `None` to commit whatever is pending (recovery).
fn commit_locked(store: &Store, account: &str, expected: Option<&Secret>) -> Result<bool> {
    store.rotation_ready(account)?;
    let Some(new) = store.pending_rotation(account)? else {
        let Some(expected) = expected else {
            return Ok(false);
        };
        // Nothing pending. Either this caller's own commit already finished (the primary holds
        // its value: idempotent re-run), or its rotation was aborted or replaced and committed.
        return match store.get(account)? {
            Some(current) if current.ct_eq(expected) => Ok(false),
            _ => Err(superseded()),
        };
    };
    if expected.is_some_and(|expected| !new.ct_eq(expected)) {
        return Err(superseded());
    }
    store.set(account, &new)?;
    remove_marker(store, account, &new)?;
    Ok(true)
}

impl Store {
    /// Record `new` as the pending value for `account`. `Ok(false)` when a rotation is already
    /// pending (nothing was changed; resolve it first). An empty value is rejected. On a durable
    /// store this needs [`crate::Config::rotation_lock_dir`].
    pub fn begin_rotation(&self, account: &str, new: &Secret) -> Result<bool> {
        if account.ends_with(ROTATION_PENDING_SUFFIX) {
            return Err(Error::Invalid {
                what: "account",
                reason: "names a rotation marker".into(),
            });
        }
        let (store, lock) = self.rotation_store(account)?;
        let _held = lock.acquire()?;
        store.rotation_ready(account)?;
        store.add_if_absent(&marker(account), new)
    }

    /// The pending new value, if a rotation was begun and not resolved.
    pub fn pending_rotation(&self, account: &str) -> Result<Option<Secret>> {
        Ok(self.get(&marker(account))?.filter(|s| !s.is_empty()))
    }

    /// Make whatever is pending the credential, then remove the marker. `Ok(false)` when nothing
    /// was pending. The marker is removed only after the primary write succeeded. The whole
    /// read → write → remove sequence runs under the account's rotation lock, shared with
    /// [`Store::begin_rotation`] and [`Store::abort_rotation`] and, with a lock directory, across
    /// processes. This commits the value that is pending at that moment, even one another
    /// caller began; a caller that began a rotation itself should use
    /// [`Store::commit_rotation_of`]. Use this one for recovery after a restart.
    pub fn commit_rotation(&self, account: &str) -> Result<bool> {
        let (store, lock) = self.rotation_store(account)?;
        let _held = lock.acquire()?;
        commit_locked(&store, account, None)
    }

    /// Commit the rotation this caller began with `begin_rotation(account, expected)`, and only
    /// that one. `Ok(true)`: the primary now holds `expected` and its marker was removed.
    /// `Ok(false)`: the primary already holds `expected` and nothing is pending (an earlier
    /// commit of this value finished). `Err(Error::Invalid { what: "pending rotation", .. })`:
    /// the rotation was aborted or replaced since it began; the primary and the newer marker are
    /// untouched. Same lock as [`Store::commit_rotation`].
    pub fn commit_rotation_of(&self, account: &str, expected: &Secret) -> Result<bool> {
        let (store, lock) = self.rotation_store(account)?;
        let _held = lock.acquire()?;
        commit_locked(&store, account, Some(expected))
    }

    /// Discard the pending value and leave the credential unchanged. `Ok(true)` if a marker was
    /// removed. Removes whatever marker is there; a caller that began the rotation itself should
    /// use [`Store::abort_rotation_of`].
    pub fn abort_rotation(&self, account: &str) -> Result<bool> {
        let (store, lock) = self.rotation_store(account)?;
        let _held = lock.acquire()?;
        store.rotation_ready(account)?;
        store.delete(&marker(account))
    }

    /// Discard the rotation this caller began with `begin_rotation(account, expected)`. A marker
    /// that holds a different value belongs to a newer rotation and stays: `Ok(false)`, like
    /// "nothing pending". `Ok(true)` when this call removed the marker.
    pub fn abort_rotation_of(&self, account: &str, expected: &Secret) -> Result<bool> {
        let (store, lock) = self.rotation_store(account)?;
        let _held = lock.acquire()?;
        store.rotation_ready(account)?;
        match store.pending_rotation(account)? {
            Some(pending) if pending.ct_eq(expected) => remove_marker(&store, account, &pending),
            _ => Ok(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock_over(path: &Path, timeout: Duration) -> RotationLock {
        // A fresh local mutex per lock: two of these are two processes as far as the file goes.
        RotationLock::new(Arc::new(Mutex::new(())), Some(path.to_path_buf()), timeout)
    }

    #[test]
    fn the_lock_file_excludes_a_holder_with_another_local_mutex_and_releases_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rotation-test.lock");
        let first = lock_over(&path, Duration::from_millis(100));
        let second = lock_over(&path, Duration::from_millis(100));

        let held = first.acquire().unwrap();
        let started = Instant::now();
        assert!(matches!(
            second.acquire(),
            Err(Error::InProgress { op: "rotation" })
        ));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the wait is bounded by the timeout"
        );
        drop(held);
        drop(second.acquire().unwrap());
        drop(first.acquire().unwrap());
    }

    #[test]
    fn the_local_mutex_wait_is_bounded_too() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rotation-test.lock");
        let lock = lock_over(&path, Duration::from_millis(50));
        let _held = lock.acquire().unwrap();
        assert!(matches!(
            lock.acquire(),
            Err(Error::InProgress { op: "rotation" })
        ));
    }

    #[test]
    fn lock_file_names_are_stable_distinct_per_credential_and_unambiguous() {
        let a = lock_file_name("keychain", "store-1", "svc/db");
        assert_eq!(a, lock_file_name("keychain", "store-1", "svc/db"));
        assert_ne!(a, lock_file_name("keychain", "store-2", "svc/db"));
        assert_ne!(a, lock_file_name("keychain", "store-1", "svc/other"));
        assert_ne!(
            lock_file_name("ab", "c", "d"),
            lock_file_name("a", "bc", "d")
        );
        assert!(a.starts_with("rotation-") && a.ends_with(".lock"));
        assert_eq!(a.len(), "rotation-".len() + 32 + ".lock".len());
    }

    #[test]
    fn a_missing_lock_directory_is_created_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("nested")
            .join("locks")
            .join("rotation-test.lock");
        let lock = lock_over(&path, Duration::from_millis(100));
        drop(lock.acquire().unwrap());
        assert!(path.is_file());
    }
}
