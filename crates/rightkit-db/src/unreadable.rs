//! Opt-in recovery for a database that cannot be *read*, as opposed to one that is corrupt.
//!
//! A permission-denied or I/O read failure is not evidence of corruption. A transient lock, a
//! network share that dropped, a full disk or the wrong mount would all look the same, and
//! setting the file aside would replace a healthy database with an empty one. So the default is
//! [`UnreadablePolicy::Fail`]: a typed [`DbError::Unreadable`] naming the cause, and the file is
//! never touched. An application that wants to get unstuck chooses
//! [`UnreadablePolicy::QuarantineAfter`]:
//!
//! - only failures whose cause is a *read failure of this file* ([`UnreadableCause::PermissionDenied`],
//!   [`UnreadableCause::IoReadFailure`]) count;
//! - failures that say the *storage* is unavailable ([`UnreadableCause::StorageUnavailable`]: disk
//!   full, read-only filesystem, device or share not ready, sharing or lock violations, busy or
//!   locked, file missing) never count and never lead to quarantine, however often they repeat;
//! - the count is persisted beside the database (`<db>.unreadable-opens`) and is *consecutive*:
//!   any successful open removes it;
//! - at the configured count the file is renamed aside with its sidecars (never deleted, never
//!   overwritten) and a fresh database is created.
//!
//! The corruption path ([`RecoveryPolicy`]) is unchanged and still decides what happens to a
//! database that SQLite reports as not-a-database or corrupt.

use crate::recover::{allowed, is_unreadable, quarantine_database_with};
use crate::recover::{OpenOutcome, RecoveryPolicy, SidecarPolicy};
use crate::{DbError, DbOptions, Result};
use rusqlite::{Connection, ErrorCode};
use std::fmt;
use std::path::{Path, PathBuf};

/// What to do with a database that cannot be read (not corrupt: see the module docs).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum UnreadablePolicy {
    /// Return [`DbError::Unreadable`]; never touch the file. The default.
    #[default]
    Fail,
    /// Quarantine the file once reading it has failed `attempts` consecutive opens (counting the
    /// current one; a value of 0 is treated as 1). Three or more is sensible: it means the
    /// failure survived at least two restarts.
    QuarantineAfter { attempts: u32 },
}

/// Why a database could not be read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum UnreadableCause {
    /// The operating system refused to let this process read the file.
    PermissionDenied,
    /// The operating system or SQLite reported a read error (`EIO`, a short read, a bad sector).
    IoReadFailure,
    /// The storage itself is unavailable; the file may be perfectly healthy.
    StorageUnavailable(StorageProblem),
}

impl UnreadableCause {
    /// True for the causes that count toward [`UnreadablePolicy::QuarantineAfter`].
    pub fn counts_toward_quarantine(self) -> bool {
        matches!(self, Self::PermissionDenied | Self::IoReadFailure)
    }
}

/// Why storage is unavailable. None of these ever leads to quarantine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum StorageProblem {
    /// `ENOSPC`, `EDQUOT`, `SQLITE_FULL`, Windows `ERROR_DISK_FULL`/`ERROR_HANDLE_DISK_FULL`.
    DiskFull,
    /// `EROFS`, `SQLITE_READONLY`, Windows `ERROR_WRITE_PROTECT`.
    ReadOnlyFilesystem,
    /// `ENXIO`, `ENODEV`, `ENOMEDIUM`, `ESTALE`, `ENOTCONN`, `ENETDOWN`, `ENETUNREACH`,
    /// `EHOSTUNREACH`, `ETIMEDOUT`; Windows `ERROR_NOT_READY`, `ERROR_DEV_NOT_EXIST`,
    /// `ERROR_DEVICE_NOT_CONNECTED`, `ERROR_BAD_NETPATH`, `ERROR_NETNAME_DELETED`,
    /// `ERROR_UNEXP_NET_ERR`, `ERROR_NETWORK_UNREACHABLE`.
    DeviceNotReady,
    /// Windows `ERROR_SHARING_VIOLATION` and `ERROR_LOCK_VIOLATION`.
    SharingViolation,
    /// `EBUSY`, `ETXTBSY`, `EAGAIN`, `EDEADLK`; `SQLITE_BUSY`, `SQLITE_LOCKED`, the
    /// `SQLITE_IOERR_*LOCK` family and `SQLITE_PROTOCOL`.
    LockedOrBusy,
    /// The file is not there; there is nothing to set aside.
    FileMissing,
    /// Anything else that is not a read failure of this file: a write-side I/O error, `EMFILE`,
    /// out of memory, a directory where the file should be, or a file this process can read
    /// but SQLite could not open.
    Other,
}

impl fmt::Display for StorageProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::DiskFull => "the disk is full",
            Self::ReadOnlyFilesystem => "the filesystem is read-only",
            Self::DeviceNotReady => "the device or share is not ready",
            Self::SharingViolation => "a sharing or lock violation",
            Self::LockedOrBusy => "the database is busy or locked",
            Self::FileMissing => "the file is missing",
            Self::Other => "the storage reported an unclassified error",
        })
    }
}

impl fmt::Display for UnreadableCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PermissionDenied => f.write_str("permission denied reading the file"),
            Self::IoReadFailure => f.write_str("I/O error reading the file"),
            Self::StorageUnavailable(p) => write!(f, "storage unavailable: {p}"),
        }
    }
}

/// What [`DbOptions::open_writer_recovering_unreadable`] did.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RecoveryAction {
    /// The database opened; nothing was moved. Any failure counter was reset.
    OpenedNormally,
    /// The database was corrupt and the [`RecoveryPolicy`] allowed quarantine.
    QuarantinedCorrupt,
    /// The database could not be read for `consecutive_failures` consecutive opens and the
    /// [`UnreadablePolicy`] allowed quarantine. The old file is preserved at `quarantined`.
    QuarantinedUnreadable {
        cause: UnreadableCause,
        consecutive_failures: u32,
    },
}

/// Result of an open that may have set an unreadable or corrupt database aside.
#[derive(Debug)]
pub struct UnreadableRecovery {
    pub conn: Connection,
    /// Index of the key candidate that opened (always 0 here).
    pub key_index: usize,
    /// Where the old file went, when it was set aside.
    pub quarantined: Option<PathBuf>,
    pub action: RecoveryAction,
}

impl From<UnreadableRecovery> for OpenOutcome {
    fn from(r: UnreadableRecovery) -> Self {
        OpenOutcome {
            conn: r.conn,
            key_index: r.key_index,
            quarantined: r.quarantined,
        }
    }
}

/// Classify an open-time failure that is neither corruption nor an unrelated error. `None` means
/// "not in this family": the caller returns the original error. `path` is the database file; it
/// is probed (opened for reading) only to tell a permission failure from other `SQLITE_CANTOPEN`
/// causes, because SQLite reports `EACCES` as `SQLITE_CANTOPEN`.
pub fn classify_open_failure(error: &DbError, path: &Path) -> Option<UnreadableCause> {
    match error {
        DbError::Sqlite(rusqlite::Error::SqliteFailure(failure, _)) => {
            classify_sqlite(failure.code, failure.extended_code, path)
        }
        other => {
            let mut source: Option<&(dyn std::error::Error + 'static)> =
                std::error::Error::source(other);
            while let Some(e) = source {
                if let Some(io) = e.downcast_ref::<std::io::Error>() {
                    return Some(classify_io(io));
                }
                source = e.source();
            }
            None
        }
    }
}

// Extended result codes (sqlite3.h); named here so the table does not depend on which
// constants the bindings export.
const IOERR_READ: i32 = 266;
const IOERR_SHORT_READ: i32 = 522;
const IOERR_UNLOCK: i32 = 2058;
const IOERR_RDLOCK: i32 = 2314;
const IOERR_BLOCKED: i32 = 2826;
const IOERR_CHECKRESERVEDLOCK: i32 = 3594;
const IOERR_LOCK: i32 = 3850;
const IOERR_SHMLOCK: i32 = 5130;
const IOERR_DATA: i32 = 8202;
const IOERR_CORRUPTFS: i32 = 8458;
const CANTOPEN_PLAIN: i32 = 14;

fn storage(p: StorageProblem) -> Option<UnreadableCause> {
    Some(UnreadableCause::StorageUnavailable(p))
}

fn classify_sqlite(code: ErrorCode, extended: i32, path: &Path) -> Option<UnreadableCause> {
    match code {
        ErrorCode::PermissionDenied => Some(UnreadableCause::PermissionDenied),
        ErrorCode::DiskFull => storage(StorageProblem::DiskFull),
        ErrorCode::ReadOnly => storage(StorageProblem::ReadOnlyFilesystem),
        ErrorCode::DatabaseBusy
        | ErrorCode::DatabaseLocked
        | ErrorCode::FileLockingProtocolFailed => storage(StorageProblem::LockedOrBusy),
        ErrorCode::OutOfMemory => storage(StorageProblem::Other),
        ErrorCode::SystemIoFailure => match extended {
            IOERR_READ | IOERR_SHORT_READ => Some(UnreadableCause::IoReadFailure),
            IOERR_UNLOCK
            | IOERR_RDLOCK
            | IOERR_BLOCKED
            | IOERR_CHECKRESERVEDLOCK
            | IOERR_LOCK
            | IOERR_SHMLOCK => storage(StorageProblem::LockedOrBusy),
            // File-system level corruption is not a read failure of the device: leave it to
            // the corruption policy, which does not claim it either.
            IOERR_DATA | IOERR_CORRUPTFS => None,
            _ => storage(StorageProblem::Other),
        },
        ErrorCode::CannotOpen if extended == CANTOPEN_PLAIN => match std::fs::File::open(path) {
            // This process can read the file: SQLite's failure was about writing or locking,
            // not about reading. Not ours to set aside.
            Ok(_) => storage(StorageProblem::Other),
            Err(e) => Some(classify_io(&e)),
        },
        ErrorCode::CannotOpen => storage(StorageProblem::Other),
        _ => None,
    }
}

fn classify_io(e: &std::io::Error) -> UnreadableCause {
    use std::io::ErrorKind as K;
    match e.kind() {
        K::PermissionDenied => return UnreadableCause::PermissionDenied,
        K::NotFound => return UnreadableCause::StorageUnavailable(StorageProblem::FileMissing),
        K::StorageFull | K::QuotaExceeded => {
            return UnreadableCause::StorageUnavailable(StorageProblem::DiskFull)
        }
        K::ReadOnlyFilesystem => {
            return UnreadableCause::StorageUnavailable(StorageProblem::ReadOnlyFilesystem)
        }
        K::ResourceBusy | K::Deadlock | K::WouldBlock => {
            return UnreadableCause::StorageUnavailable(StorageProblem::LockedOrBusy)
        }
        K::StaleNetworkFileHandle
        | K::NotConnected
        | K::NetworkDown
        | K::NetworkUnreachable
        | K::HostUnreachable
        | K::TimedOut => {
            return UnreadableCause::StorageUnavailable(StorageProblem::DeviceNotReady)
        }
        _ => {}
    }
    match e.raw_os_error() {
        Some(code) => classify_os_error(code),
        None => UnreadableCause::StorageUnavailable(StorageProblem::Other),
    }
}

#[cfg(unix)]
fn classify_os_error(code: i32) -> UnreadableCause {
    use StorageProblem as P;
    let problem = match code {
        libc::EIO => return UnreadableCause::IoReadFailure,
        libc::ENXIO | libc::ENODEV => P::DeviceNotReady,
        libc::EBUSY | libc::ETXTBSY | libc::EAGAIN | libc::EDEADLK => P::LockedOrBusy,
        libc::ENOSPC | libc::EDQUOT => P::DiskFull,
        libc::EROFS => P::ReadOnlyFilesystem,
        libc::ESTALE
        | libc::ENOTCONN
        | libc::ENETDOWN
        | libc::ENETUNREACH
        | libc::EHOSTUNREACH
        | libc::ETIMEDOUT => P::DeviceNotReady,
        #[cfg(target_os = "linux")]
        libc::ENOMEDIUM => P::DeviceNotReady,
        _ => P::Other,
    };
    UnreadableCause::StorageUnavailable(problem)
}

#[cfg(windows)]
fn classify_os_error(code: i32) -> UnreadableCause {
    use StorageProblem as P;
    let problem = match code {
        // ERROR_IO_DEVICE, ERROR_CRC, ERROR_SECTOR_NOT_FOUND, ERROR_READ_FAULT
        1117 | 23 | 27 | 30 => return UnreadableCause::IoReadFailure,
        // ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION
        32 | 33 => P::SharingViolation,
        // ERROR_NOT_READY, ERROR_DEV_NOT_EXIST, ERROR_DEVICE_NOT_CONNECTED, ERROR_BAD_NETPATH,
        // ERROR_NETNAME_DELETED, ERROR_UNEXP_NET_ERR, ERROR_NETWORK_UNREACHABLE
        21 | 55 | 1167 | 53 | 64 | 59 | 1231 => P::DeviceNotReady,
        // ERROR_DISK_FULL, ERROR_HANDLE_DISK_FULL
        112 | 39 => P::DiskFull,
        // ERROR_WRITE_PROTECT
        19 => P::ReadOnlyFilesystem,
        _ => P::Other,
    };
    UnreadableCause::StorageUnavailable(problem)
}

#[cfg(not(any(unix, windows)))]
fn classify_os_error(_code: i32) -> UnreadableCause {
    UnreadableCause::StorageUnavailable(StorageProblem::Other)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Where the consecutive-failure count is kept: beside the database.
pub fn unreadable_counter_path(db: &Path) -> PathBuf {
    with_suffix(db, ".unreadable-opens")
}

fn read_counter(path: &Path) -> u32 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.lines().nth(1).and_then(|l| l.trim().parse().ok()))
        .unwrap_or(0)
}

fn bump_counter(path: &Path) -> Result<u32> {
    let next = read_counter(path).saturating_add(1);
    rightkit_fs::atomic_write_private(
        path,
        format!("rightkit-db-unreadable.v1\n{next}\n").as_bytes(),
    )?;
    Ok(next)
}

fn reset_counter(path: &Path) {
    // Best effort and only when present: the common open touches nothing.
    if std::fs::symlink_metadata(path).is_ok() {
        let _ = std::fs::remove_file(path);
    }
}

fn is_regular_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file())
}

impl DbOptions {
    /// [`DbOptions::open_writer_recovering`] plus a policy for a database that cannot be read.
    ///
    /// - Opens normally: returns [`RecoveryAction::OpenedNormally`] and clears the failure count.
    /// - Corrupt (not-a-database, corrupt, failed integrity check): as `open_writer_recovering`,
    ///   decided by `policy`; quarantined gives [`RecoveryAction::QuarantinedCorrupt`].
    /// - Unreadable or storage unavailable ([`UnreadableCause`]): decided by `unreadable`; the
    ///   default [`UnreadablePolicy::Fail`] returns [`DbError::Unreadable`] and touches nothing.
    /// - Any other error is returned unchanged.
    ///
    /// # Errors
    /// [`DbError::Unreadable`] as above; the original error otherwise; a quarantine or reopen
    /// failure after the file was set aside (the old file stays preserved).
    pub fn open_writer_recovering_unreadable(
        &self,
        policy: RecoveryPolicy,
        unreadable: UnreadablePolicy,
    ) -> Result<UnreadableRecovery> {
        self.open_writer_recovering_via(policy, unreadable, || self.open_writer())
    }

    /// As [`DbOptions::open_writer_recovering_unreadable`], with the open step supplied by the
    /// caller: it is called once for the first attempt and once more after a quarantine. This is
    /// the seam for tests that inject open errors and for apps with their own bootstrap.
    pub fn open_writer_recovering_via(
        &self,
        policy: RecoveryPolicy,
        unreadable: UnreadablePolicy,
        mut open: impl FnMut() -> Result<Connection>,
    ) -> Result<UnreadableRecovery> {
        let counter = unreadable_counter_path(&self.path);
        let error = match open() {
            Ok(conn) => {
                reset_counter(&counter);
                return Ok(UnreadableRecovery {
                    conn,
                    key_index: 0,
                    quarantined: None,
                    action: RecoveryAction::OpenedNormally,
                });
            }
            Err(error) => error,
        };
        if is_unreadable(&error) && allowed(self, policy) {
            let moved =
                quarantine_database_with(&self.path, policy.quarantine_stamp, policy.sidecars)?;
            let conn = open()?;
            reset_counter(&counter);
            return Ok(UnreadableRecovery {
                conn,
                key_index: 0,
                quarantined: Some(moved),
                action: RecoveryAction::QuarantinedCorrupt,
            });
        }
        let Some(cause) = classify_open_failure(&error, &self.path) else {
            return Err(error);
        };
        let threshold = match unreadable {
            UnreadablePolicy::QuarantineAfter { attempts } if cause.counts_toward_quarantine() => {
                Some(attempts.max(1))
            }
            _ => None,
        };
        let Some(threshold) = threshold else {
            // Fail, or a storage problem: never count, never move.
            return Err(self.unreadable_error(cause, read_counter(&counter), None, error));
        };
        let failures = match bump_counter(&counter) {
            Ok(n) => n,
            // Cannot record the count, so cannot honour "consecutive": stay safe and fail.
            Err(_) => {
                return Err(self.unreadable_error(
                    cause,
                    read_counter(&counter),
                    Some(threshold),
                    error,
                ))
            }
        };
        if failures < threshold || !is_regular_file(&self.path) {
            return Err(self.unreadable_error(cause, failures, Some(threshold), error));
        }
        // Rename only, and sidecars always travel with the file: nothing is deleted.
        let moved = quarantine_database_with(
            &self.path,
            policy.quarantine_stamp,
            SidecarPolicy::RenameSidecars,
        )?;
        reset_counter(&counter);
        let conn = open()?;
        Ok(UnreadableRecovery {
            conn,
            key_index: 0,
            quarantined: Some(moved),
            action: RecoveryAction::QuarantinedUnreadable {
                cause,
                consecutive_failures: failures,
            },
        })
    }

    fn unreadable_error(
        &self,
        cause: UnreadableCause,
        consecutive_failures: u32,
        quarantine_after: Option<u32>,
        source: DbError,
    ) -> DbError {
        DbError::Unreadable {
            path: self.path.clone(),
            cause,
            consecutive_failures,
            quarantine_after,
            source: Box::new(source),
        }
    }
}
