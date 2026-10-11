//! A [`ClockStore`] that is atomic across processes.
//!
//! [`ClockStore::compare_and_save`] has no default because a load followed by a save is
//! not atomic against another process. [`FileClockStore`] keeps the ledger in one file and
//! serialises every read-compare-write-rename on an OS advisory lock of a sidecar file, so
//! two processes (an app's helper and its main process, or two launches of the same app)
//! can never interleave and move the trusted mark backwards. [`CrossProcessLock`] is the
//! same lock as a guard, for hosts that keep the ledger somewhere else (a keychain item, a
//! credential entry) and want to serialise their own `compare_and_save` with it.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::clock::{ClockDecodeError, ClockState, ClockStore};

/// An exclusive OS advisory lock on a file, shared by every process that opens the same
/// path. Released when dropped (or when the process exits).
///
/// Use it inside a [`ClockStore::compare_and_save`] whose backing store has no atomic
/// compare-and-swap of its own:
///
/// ```no_run
/// use rightkit_license::CrossProcessLock;
///
/// # fn demo(lock_path: &std::path::Path) -> std::io::Result<()> {
/// let _held = CrossProcessLock::acquire(lock_path)?;
/// // read the ledger, compare it with `expected`, write `new`: no other process can
/// // be doing the same thing until `_held` is dropped.
/// # Ok(())
/// # }
/// ```
///
/// Every process must use the same lock path and must take the lock around the whole
/// read-compare-write. The lock is advisory: it orders cooperating writers, and does not
/// stop a process that ignores it.
#[derive(Debug)]
pub struct CrossProcessLock {
    file: File,
}

impl CrossProcessLock {
    /// Block until this process holds the exclusive lock on `path`, creating the file (and
    /// its parent directories) if needed. The file's contents are never read or changed.
    pub fn acquire(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::open(path.as_ref(), true)
    }

    /// Shared lock: many readers at once, excluded while a writer holds the exclusive lock.
    fn acquire_shared(path: &Path) -> io::Result<Self> {
        Self::open(path, false)
    }

    fn open(path: &Path, exclusive: bool) -> io::Result<Self> {
        create_parent(path)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        if exclusive {
            file.lock()?;
        } else {
            file.lock_shared()?;
        }
        Ok(Self { file })
    }
}

impl Drop for CrossProcessLock {
    fn drop(&mut self) {
        // Closing the handle releases the lock anyway; unlock first so the release does not
        // wait for the handle to be closed.
        let _ = self.file.unlock();
    }
}

/// Failure of a [`FileClockStore`] operation.
#[derive(Debug)]
pub enum FileClockStoreError {
    /// Locking, reading, writing or renaming the ledger file failed.
    Io(io::Error),
    /// The ledger file exists but is not a valid clock record. This is an error and never
    /// "no ledger": a damaged ledger must not reset rollback protection.
    Corrupt(ClockDecodeError),
}

impl fmt::Display for FileClockStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "license clock file failed: {error}"),
            Self::Corrupt(error) => write!(f, "license clock file is damaged: {error}"),
        }
    }
}

impl std::error::Error for FileClockStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Corrupt(error) => Some(error),
        }
    }
}

impl From<io::Error> for FileClockStoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// File-backed [`ClockStore`], atomic across processes.
///
/// The ledger is the JSON record of [`ClockState::encode`]. Every write goes to a sibling
/// temporary file that is flushed and renamed over the ledger, so a reader sees the old or
/// the new record and never a torn one. Every operation takes the OS lock of
/// [`lock_path`](Self::lock_path) (shared for [`load`](ClockStore::load), exclusive for
/// writes), so [`compare_and_save`](ClockStore::compare_and_save) is one atomic step against
/// every other `FileClockStore` on the same path in any process.
///
/// The ledger file should live where the user cannot casually delete or rewrite it (a
/// per-user application-data directory); a file store is only as tamper-resistant as its
/// directory. Hosts that need keychain-grade protection implement [`ClockStore`] over the
/// keychain and serialise it with [`CrossProcessLock`].
#[derive(Debug, Clone)]
pub struct FileClockStore {
    path: PathBuf,
}

impl FileClockStore {
    /// A store over the ledger file at `path`. Nothing is created until the first write.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The ledger file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The sidecar file whose OS lock serialises every operation on this ledger.
    pub fn lock_path(&self) -> PathBuf {
        sidecar(&self.path, "lock")
    }

    fn read_unlocked(&self) -> Result<Option<ClockState>, FileClockStoreError> {
        match fs::read_to_string(&self.path) {
            Ok(raw) => ClockState::decode(&raw)
                .map(Some)
                .map_err(FileClockStoreError::Corrupt),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(FileClockStoreError::Io(error)),
        }
    }

    /// Write `state` through a temporary file and an atomic rename. The caller holds the
    /// exclusive lock.
    fn write_unlocked(&self, state: &ClockState) -> Result<(), FileClockStoreError> {
        create_parent(&self.path)?;
        let temporary = sidecar(&self.path, "tmp");
        let written = (|| -> io::Result<()> {
            let mut file = File::create(&temporary)?;
            file.write_all(state.encode().as_bytes())?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, &self.path)
        })();
        if written.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        written?;
        sync_parent(&self.path);
        Ok(())
    }
}

impl ClockStore for FileClockStore {
    type Error = FileClockStoreError;

    fn load(&self) -> Result<Option<ClockState>, Self::Error> {
        let _lock = CrossProcessLock::acquire_shared(&self.lock_path())?;
        self.read_unlocked()
    }

    fn save(&self, state: &ClockState) -> Result<(), Self::Error> {
        let _lock = CrossProcessLock::acquire(self.lock_path())?;
        self.write_unlocked(state)
    }

    fn compare_and_save(
        &self,
        expected: Option<&ClockState>,
        new: &ClockState,
    ) -> Result<bool, Self::Error> {
        let _lock = CrossProcessLock::acquire(self.lock_path())?;
        if self.read_unlocked()?.as_ref() != expected {
            return Ok(false);
        }
        self.write_unlocked(new)?;
        Ok(true)
    }
}

/// `path` with `.suffix` appended to its file name.
fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".");
    name.push(suffix);
    PathBuf::from(name)
}

fn create_parent(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => fs::create_dir_all(parent),
        _ => Ok(()),
    }
}

/// Make the rename durable. Best effort: the data file itself was already flushed.
#[cfg(unix)]
fn sync_parent(path: &Path) {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    if let Ok(directory) = File::open(parent) {
        let _ = directory.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_parent(_: &Path) {}
