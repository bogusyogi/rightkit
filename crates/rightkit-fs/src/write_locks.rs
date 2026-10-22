//! [`KeyedWriteLocks`]: serialize the writers of one destination file inside a process, and
//! optionally across processes, over the crate's own [`FileLock`] and [`AtomicWriter`].
//!
//! An app that saves user documents needs "one writer per destination at a time" without a
//! permanent map entry for every path it ever wrote. This is that: a registry of in-process slots
//! keyed by the destination's directory entry, created on demand and removed when the last holder
//! or waiter leaves, plus an optional cross-process lock file per destination for cooperating
//! processes.
//!
//! ```no_run
//! use rightkit_fs::{AtomicWriteOptions, KeyedWriteLocks};
//! use std::path::Path;
//!
//! let locks = KeyedWriteLocks::new();
//! let destination = Path::new("report.txt");
//! let saved = locks.with_destination(destination, |held| -> Result<(), rightkit_fs::FsError> {
//!     let mut writer = held.atomic_writer(AtomicWriteOptions::default())?;
//!     writer.write_all(b"contents")?;
//!     // Re-check the caller's own conflict marker here, immediately before replacement.
//!     writer.commit_checked(|_temp| Ok(()))
//! })?;
//! saved?;
//! # Ok::<(), rightkit_fs::FsError>(())
//! ```
//!
//! # The key
//!
//! A destination is keyed by `(identity of its parent directory, its file name)`, never by its
//! full path and never by the file's own inode. The parent is resolved through links and
//! identified by [`FileKey`] (device and inode, volume serial and file index on Windows), so
//! `a/./x`, `a/../a/x` and a path through a symlinked directory are one key. The file name is
//! the key's other half because an atomic replace gives the file a new inode on every write: a
//! key built from the file's own identity would change under a holder. The leaf need not exist
//! (a first save of a new file is serialized like any other), and it is never followed if it is
//! a symlink: a replace replaces the link itself.
//!
//! Stated limits:
//!
//! * Two different names for one file (a hard link, or a case alias on a case-sensitive volume
//!   that Windows or macOS folding does not know about) are two keys. On Windows and macOS file
//!   names are compared case-folded even on a case-sensitive volume, which only ever serializes
//!   more than needed.
//! * Unicode normalization differences (NFC against NFD) are two keys, as in `crate::contain`.
//! * The parent must exist and be a directory; it is verified, not created.
//!
//! # What it does not promise
//!
//! The lock serializes **cooperating** writers that use the same registry (in one process) or the
//! same process-lock directory (across processes). It does not stop an editor, a sync client or a
//! shell command from writing the file, and it gives no compare-and-swap against such a writer: a
//! check made inside [`AtomicWriter::commit_checked`] narrows the window between the check and the
//! rename, it does not close it, because no platform rename compares the destination's content.
//! The caller keeps its own source fingerprint and conflict behaviour; this type removes only the
//! duplicated lock map.
//!
//! # Lock order
//!
//! Acquisition order is fixed: the in-process slot first, then the process lock file, and release
//! is the reverse. A transaction ([`crate::transaction`], feature `transaction`) takes neither: its
//! own operation lock is the third and last lock in the order, and a transaction never acquires a
//! keyed write lock, so the order has no cycle. Do not call a `with_destination` for the same
//! destination from inside its own operation (the slot is not re-entrant; the inner call waits for
//! the bounded timeout and returns [`FsError::LockTimeout`]).
//!
//! # Boundedness
//!
//! The registry holds one entry per destination that currently has a holder or a waiter, and the
//! entry is removed when the last of them leaves, on every path (success, timeout, error, panic).
//! [`KeyedWriteLocks::active_destinations`] reports the count. Every wait is bounded by
//! [`WriteLockOptions::timeout`] (default 30 seconds); a timeout the clock cannot add, such as
//! `Duration::MAX`, is the caller asking for no deadline and then a wait lasts as long as the
//! holder does. No thread or timer is created.

use crate::atomic::{AtomicWriteOptions, AtomicWriter, TempWriter, WriteReport};
use crate::error::{FsError, Result};
use crate::file_key::FileKey;
use crate::lock::{FileLock, LockOptions};
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// How long a writer waits for a destination, and where cross-process lock files live.
#[derive(Clone, Debug)]
pub struct WriteLockOptions {
    timeout: Duration,
    process_lock_dir: Option<PathBuf>,
}

impl Default for WriteLockOptions {
    /// Wait up to 30 seconds, in-process locking only.
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            process_lock_dir: None,
        }
    }
}

impl WriteLockOptions {
    /// Wait at most `timeout` for a destination that is in use. `Duration::ZERO` tries once. A
    /// timeout too large to add to the clock (such as `Duration::MAX`) means no deadline.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Also take an exclusive [`FileLock`] per destination, in `dir`, so cooperating processes
    /// that use the same directory exclude each other too. The directory is created owner-only
    /// when it is missing. The lock file for a destination has a stable name derived from its
    /// key ([`KeyedWriteLocks::lock_file_for`]); lock files are never deleted.
    pub fn process_lock_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.process_lock_dir = Some(dir.into());
        self
    }
}

/// The identity of one destination: its directory and its (case-folded where the platform folds)
/// name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct DestinationKey {
    parent: FileKey,
    name: OsString,
}

#[derive(Debug)]
struct Slot {
    held: Mutex<bool>,
    ready: Condvar,
}

#[derive(Debug, Default)]
struct Registry {
    /// Live slots and how many holders plus waiters reference each.
    slots: Mutex<HashMap<DestinationKey, (Arc<Slot>, usize)>>,
}

fn lock_ignoring_poison<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A reference to a slot that releases it (and removes the registry entry with the last
/// reference) on every exit path.
struct SlotGuard {
    registry: Arc<Registry>,
    key: DestinationKey,
    slot: Arc<Slot>,
    held: bool,
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        if self.held {
            *lock_ignoring_poison(&self.slot.held) = false;
            self.slot.ready.notify_one();
        }
        let mut slots = lock_ignoring_poison(&self.registry.slots);
        let remove = match slots.get_mut(&self.key) {
            Some(entry) => {
                entry.1 = entry.1.saturating_sub(1);
                entry.1 == 0
            }
            None => false,
        };
        if remove {
            slots.remove(&self.key);
        }
    }
}

/// In-process write locks keyed by destination, with an optional cross-process lock file. Clones
/// share one registry.
#[derive(Clone, Debug, Default)]
pub struct KeyedWriteLocks {
    registry: Arc<Registry>,
    options: WriteLockOptions,
}

/// The destination a [`KeyedWriteLocks::with_destination`] operation holds.
#[derive(Debug)]
pub struct LockedDestination {
    path: PathBuf,
}

impl LockedDestination {
    /// The destination path exactly as the caller gave it.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A writer borrowing this locked destination. It must commit before the operation returns.
    ///
    /// ```compile_fail
    /// use rightkit_fs::{AtomicWriteOptions, KeyedWriteLocks};
    /// use std::path::Path;
    /// let locks = KeyedWriteLocks::new();
    /// let escaped = locks.with_destination(Path::new("report.txt"), |held| {
    ///     held.atomic_writer(AtomicWriteOptions::default()).unwrap()
    /// }).unwrap();
    /// escaped.commit().unwrap();
    /// ```
    pub fn atomic_writer(&self, options: AtomicWriteOptions) -> Result<LockedAtomicWriter<'_>> {
        Ok(LockedAtomicWriter {
            writer: AtomicWriter::create_with(&self.path, options)?,
            _held: self,
        })
    }
}

/// An atomic writer whose publication cannot outlive its destination lock.
///
/// Created by [`LockedDestination::atomic_writer`]. It exposes no owned inner writer or alternate
/// destination commit: every publication uses the destination protected by the borrowed lock.
pub struct LockedAtomicWriter<'held> {
    writer: AtomicWriter,
    _held: &'held LockedDestination,
}

impl LockedAtomicWriter<'_> {
    /// The staged temporary file's path, as in [`AtomicWriter::temp_path`].
    pub fn temp_path(&self) -> &Path {
        self.writer.temp_path()
    }

    /// Access the temporary file, as in [`AtomicWriter::file_mut`].
    pub fn file_mut(&mut self) -> &mut std::fs::File {
        self.writer.file_mut()
    }

    /// Write all bytes to the staged file, as in [`AtomicWriter::write_all`].
    pub fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer.write_all(bytes)
    }

    /// Stream into the staged file, as in [`AtomicWriter::writer`].
    pub fn writer(&mut self) -> TempWriter<'_> {
        self.writer.writer()
    }

    /// Publish while the destination lock is held, as in [`AtomicWriter::commit`].
    pub fn commit(self) -> Result<()> {
        self.writer.commit()
    }

    /// Publish with achieved durability reporting, as in [`AtomicWriter::commit_reporting`].
    pub fn commit_reporting(self) -> Result<WriteReport> {
        self.writer.commit_reporting()
    }

    /// Recheck before publishing, as in [`AtomicWriter::commit_checked`].
    pub fn commit_checked(self, check: impl FnOnce(&Path) -> std::io::Result<()>) -> Result<()> {
        self.writer.commit_checked(check)
    }

    /// Publish and return the staged bytes' SHA-256, as in [`AtomicWriter::commit_with_digest`].
    #[cfg(feature = "digest")]
    pub fn commit_with_digest(self) -> Result<String> {
        self.writer.commit_with_digest()
    }
}

impl KeyedWriteLocks {
    /// A registry with the default options (30 second wait, in-process only).
    pub fn new() -> Self {
        Self::default()
    }

    /// A registry with explicit options.
    pub fn with_options(options: WriteLockOptions) -> Self {
        Self {
            registry: Arc::new(Registry::default()),
            options,
        }
    }

    /// Run `operation` while holding the exclusive write lock of `path`, waiting up to the
    /// configured timeout.
    ///
    /// Returns the operation's value. Errors are those of taking the lock only: the destination
    /// has no usable parent ([`FsError::Io`], [`FsError::NotADirectory`]) or a reserved name
    /// ([`FsError::BadName`]); the slot stayed busy ([`FsError::LockTimeout`]); or the process
    /// lock was refused ([`FsError::Locked`], [`FsError::LockTimeout`]). The lock is released on
    /// every path, including a panic in `operation`.
    pub fn with_destination<T>(
        &self,
        path: &Path,
        operation: impl FnOnce(&LockedDestination) -> T,
    ) -> Result<T> {
        self.run(path, self.options.timeout, operation)
    }

    /// [`KeyedWriteLocks::with_destination`] without waiting: a busy destination is
    /// [`FsError::Locked`] at once.
    pub fn try_with_destination<T>(
        &self,
        path: &Path,
        operation: impl FnOnce(&LockedDestination) -> T,
    ) -> Result<T> {
        self.run(path, Duration::ZERO, operation)
    }

    /// The cross-process lock file this registry uses for `path`, or `None` when no process lock
    /// directory is configured. The name is stable across runs and processes (FNV-1a of the key),
    /// so a collision only ever serializes two destinations that did not need it.
    pub fn lock_file_for(&self, path: &Path) -> Result<Option<PathBuf>> {
        let (key, _) = destination_key(path)?;
        Ok(self
            .options
            .process_lock_dir
            .as_deref()
            .map(|dir| process_lock_path(dir, &key)))
    }

    /// How many destinations currently have a holder or a waiter. Zero when idle: the registry
    /// keeps no entry for a destination nobody is using.
    pub fn active_destinations(&self) -> usize {
        lock_ignoring_poison(&self.registry.slots).len()
    }

    fn run<T>(
        &self,
        path: &Path,
        timeout: Duration,
        operation: impl FnOnce(&LockedDestination) -> T,
    ) -> Result<T> {
        let (key, _parent) = destination_key(path)?;
        let started = Instant::now();
        // `Duration::MAX` (or any wait the clock cannot add) means no deadline, not a panic.
        let deadline = started.checked_add(timeout);
        // Drop order below is the reverse of declaration: the operation's view first, then the
        // process lock, then the in-process slot.
        let slot = self.acquire_slot(&key, deadline, path, timeout)?;
        let process_lock = match &self.options.process_lock_dir {
            Some(dir) => {
                crate::private::ensure_private_dir(dir)?;
                let remaining =
                    deadline.map_or(timeout, |at| at.saturating_duration_since(Instant::now()));
                let options = LockOptions::wait(remaining).labeled("rightkit-fs keyed write");
                Some(FileLock::acquire(&process_lock_path(dir, &key), &options)?)
            }
            None => None,
        };
        let destination = LockedDestination {
            path: path.to_path_buf(),
        };
        let value = operation(&destination);
        drop(destination);
        drop(process_lock);
        drop(slot);
        Ok(value)
    }

    fn acquire_slot(
        &self,
        key: &DestinationKey,
        deadline: Option<Instant>,
        path: &Path,
        timeout: Duration,
    ) -> Result<SlotGuard> {
        let slot = {
            let mut slots = lock_ignoring_poison(&self.registry.slots);
            let entry = slots.entry(key.clone()).or_insert_with(|| {
                (
                    Arc::new(Slot {
                        held: Mutex::new(false),
                        ready: Condvar::new(),
                    }),
                    0,
                )
            });
            entry.1 += 1;
            Arc::clone(&entry.0)
        };
        // The reference count above entitles the entry to exist; this guard gives it back on
        // every exit, including a timeout.
        let mut guard = SlotGuard {
            registry: Arc::clone(&self.registry),
            key: key.clone(),
            slot: Arc::clone(&slot),
            held: false,
        };
        let mut held = lock_ignoring_poison(&slot.held);
        while *held {
            let now = Instant::now();
            if deadline.is_some_and(|at| now >= at) {
                drop(held);
                return Err(if timeout.is_zero() {
                    FsError::Locked {
                        path: path.to_path_buf(),
                        owner: "another writer of this destination in this process".into(),
                    }
                } else {
                    FsError::LockTimeout {
                        path: path.to_path_buf(),
                        waited_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
                    }
                });
            }
            held = match deadline {
                Some(at) => {
                    slot.ready
                        .wait_timeout(held, at - now)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                None => slot
                    .ready
                    .wait(held)
                    .unwrap_or_else(PoisonError::into_inner),
            };
        }
        *held = true;
        drop(held);
        guard.held = true;
        Ok(guard)
    }
}

/// The key of `path` and its parent directory, verified to exist and be a directory.
fn destination_key(path: &Path) -> Result<(DestinationKey, PathBuf)> {
    let name = path
        .file_name()
        .ok_or_else(|| FsError::BadName(path.display().to_string()))?;
    crate::atomic::reject_reserved_name(&name.to_string_lossy())?;
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    // Follows links: a directory reached through a symlink is the same directory.
    let meta = std::fs::metadata(&parent).map_err(|e| FsError::io(&parent, e))?;
    if !meta.is_dir() {
        return Err(FsError::NotADirectory(parent));
    }
    let key = DestinationKey {
        parent: FileKey::of_path(&parent)?,
        name: fold_name(name),
    };
    Ok((key, parent))
}

/// Windows and macOS volumes are case-insensitive by default, so their names are compared
/// lowercased; a name that is not valid Unicode is kept exactly.
fn fold_name(name: &OsStr) -> OsString {
    if cfg!(any(windows, target_vendor = "apple")) {
        if let Some(text) = name.to_str() {
            return OsString::from(text.to_lowercase());
        }
    }
    name.to_os_string()
}

fn fnv1a(bytes: &[u8], mut hash: u64) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn process_lock_path(dir: &Path, key: &DestinationKey) -> PathBuf {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    hash = fnv1a(&key.parent.volume.to_le_bytes(), hash);
    hash = fnv1a(&key.parent.index.to_le_bytes(), hash);
    hash = fnv1a(key.name.as_encoded_bytes(), hash);
    dir.join(format!("write-{hash:016x}.lock"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_file_names_are_stable_and_distinguish_destinations() {
        let dir = tempfile::tempdir().unwrap();
        let locks = KeyedWriteLocks::with_options(
            WriteLockOptions::default().process_lock_dir(dir.path().join("locks")),
        );
        let a = locks.lock_file_for(&dir.path().join("a.txt")).unwrap();
        let again = locks
            .lock_file_for(&dir.path().join(".").join("a.txt"))
            .unwrap();
        let b = locks.lock_file_for(&dir.path().join("b.txt")).unwrap();
        assert!(a.is_some());
        assert_eq!(
            a, again,
            "a spelling of the same destination is one lock file"
        );
        assert_ne!(a, b);
    }

    #[test]
    fn fnv1a_matches_the_published_vector() {
        // FNV-1a 64 of "a" is af63dc4c8601ec8c.
        assert_eq!(fnv1a(b"a", 0xcbf2_9ce4_8422_2325), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn no_process_lock_dir_means_no_lock_file() {
        let dir = tempfile::tempdir().unwrap();
        let locks = KeyedWriteLocks::new();
        assert_eq!(locks.lock_file_for(&dir.path().join("a")).unwrap(), None);
    }
}
