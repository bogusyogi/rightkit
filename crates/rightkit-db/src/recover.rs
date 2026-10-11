//! Open-time recovery: key rotation interrupted before the credential store was updated, and
//! corrupt databases.
//!
//! - **Key candidates**: the caller lists keys in preference order (current first, then a
//!   pending/previous key from a staged rotation). The first that decrypts wins and its index
//!   is reported so the app can promote it in its credential store (MailRight `open_at`).
//! - **Quarantine**: a database that is not a database (garbage, truncated header) is renamed
//!   aside with its sidecars and a fresh file is created, instead of crash-looping on every
//!   start. A *wrong key* is indistinguishable from corruption to SQLCipher, so a keyed
//!   database is only ever quarantined when the policy says so explicitly.

use crate::{DbError, DbOptions, Result};
use rusqlite::{Connection, ErrorCode};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RecoveryPolicy {
    /// Quarantine-and-recreate an unreadable *plain* database.
    pub quarantine_corrupt: bool,
    /// Also allow it for a keyed database (destroys access to data a wrong key could have read).
    pub quarantine_keyed: bool,
    /// Suffix format of the quarantined file. Default `.corrupt-<unix-ms>`.
    pub quarantine_stamp: QuarantineStamp,
    /// What happens to the `-wal`, `-shm` and `-journal` sidecars. Default renames them alongside
    /// the quarantined file.
    pub sidecars: SidecarPolicy,
}

/// Suffix format of a quarantined database, appended to its file name.
#[derive(Clone, Copy, Debug, Default)]
pub enum QuarantineStamp {
    /// `.corrupt-<unix-ms>` (default).
    #[default]
    Millis,
    /// `.corrupt-<unix-secs>`.
    Seconds,
    /// The formatter returns the whole suffix, for example `.bad-<n>`. The result must be
    /// non-empty and contain no path separator or NUL, or quarantine fails with
    /// [`DbError::InvalidQuarantineSuffix`].
    Custom(fn(SystemTime) -> String),
}

// Function pointers are compared by address with `fn_addr_eq`, which is the sanctioned form:
// a derived `==` on `fn` pointers triggers `unpredictable_function_pointer_comparisons`.
// Equal addresses compare equal, so `Eq` holds.
impl PartialEq for QuarantineStamp {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Millis, Self::Millis) | (Self::Seconds, Self::Seconds) => true,
            (Self::Custom(a), Self::Custom(b)) => std::ptr::fn_addr_eq(*a, *b),
            _ => false,
        }
    }
}

impl Eq for QuarantineStamp {}

impl QuarantineStamp {
    fn suffix(self, now: SystemTime) -> String {
        let since_epoch = now.duration_since(UNIX_EPOCH).unwrap_or_default();
        match self {
            Self::Millis => format!(".corrupt-{}", since_epoch.as_millis()),
            Self::Seconds => format!(".corrupt-{}", since_epoch.as_secs()),
            Self::Custom(format) => format(now),
        }
    }
}

/// What happens to the `-wal`, `-shm` and `-journal` sidecars of a quarantined database.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SidecarPolicy {
    /// Move each sidecar beside the quarantined file, under the same suffix. Nothing is deleted
    /// (default).
    #[default]
    RenameSidecars,
    /// Delete the sidecars. Their bytes are not kept for diagnosis, and no stale WAL is left at
    /// the original path.
    DeleteSidecars,
}

#[derive(Debug)]
pub struct OpenOutcome {
    pub conn: Connection,
    /// Index into the candidate list that opened the database (0 for a plain open).
    pub key_index: usize,
    /// Where the unreadable database was moved, when recovery quarantined it.
    pub quarantined: Option<PathBuf>,
}

impl DbOptions {
    /// Try each passphrase in order; the first that decrypts opens the writer.
    pub fn open_writer_with_key_candidates(
        &self,
        candidates: &[Zeroizing<String>],
    ) -> Result<(Connection, usize)> {
        let mut first_error = None;
        for (index, candidate) in candidates.iter().enumerate() {
            let mut attempt = self.clone();
            attempt.key = Some(candidate.clone());
            attempt.raw_key = None;
            match attempt.open_writer() {
                Ok(conn) => return Ok((conn, index)),
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        Err(first_error.unwrap_or(DbError::NoKeyOpened))
    }

    /// Open the writer; if the file is unreadable and the policy allows it, quarantine it and
    /// open a fresh database at the same path.
    pub fn open_writer_recovering(&self, policy: RecoveryPolicy) -> Result<OpenOutcome> {
        match self.open_writer() {
            Ok(conn) => Ok(OpenOutcome {
                conn,
                key_index: 0,
                quarantined: None,
            }),
            Err(error) if is_unreadable(&error) && allowed(self, policy) => {
                let moved =
                    quarantine_database_with(&self.path, policy.quarantine_stamp, policy.sidecars)?;
                let conn = self.open_writer()?;
                Ok(OpenOutcome {
                    conn,
                    key_index: 0,
                    quarantined: Some(moved),
                })
            }
            Err(error) => Err(error),
        }
    }
}

fn allowed(options: &DbOptions, policy: RecoveryPolicy) -> bool {
    if options.is_keyed() {
        policy.quarantine_keyed
    } else {
        policy.quarantine_corrupt
    }
}

/// True when the file is not a usable SQLite database: SQLite reports it as not-a-database or
/// corrupt, or an open-time integrity check failed ([`DbError::Integrity`], from `Quick` or `Full`).
/// Busy, locked, permission and other I/O failures are not unreadable. They are transient or
/// environmental, and quarantining on them would move a healthy file aside.
fn is_unreadable(error: &DbError) -> bool {
    match error {
        DbError::Integrity(_) => true,
        DbError::Sqlite(rusqlite::Error::SqliteFailure(failure, _)) => matches!(
            failure.code,
            ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt
        ),
        _ => false,
    }
}

/// Move `db` and its `-wal` / `-shm` / `-journal` sidecars to `<db>.corrupt-<unix-ms>[...]`.
/// Returns the new main-file path. Nothing is deleted.
pub fn quarantine_database(db: &Path) -> Result<PathBuf> {
    quarantine_database_with(db, QuarantineStamp::Millis, SidecarPolicy::RenameSidecars)
}

/// [`quarantine_database`] with an explicit suffix format and sidecar policy. Refuses to overwrite
/// an existing file at the destination. Sidecar failures are errors, not ignored: a `-wal` left
/// at the original path would be replayed into the replacement database.
pub fn quarantine_database_with(
    db: &Path,
    stamp: QuarantineStamp,
    sidecars: SidecarPolicy,
) -> Result<PathBuf> {
    quarantine_database_opts(
        db,
        &QuarantineOptions {
            name: stamp,
            sidecars,
            ..QuarantineOptions::default()
        },
    )
}

/// How a quarantined file leaves its original path.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MoveMode {
    /// `rename(2)`: one step, within the directory (default).
    #[default]
    Rename,
    /// Hard-link the file to its destination, then remove the original, so the destination
    /// exists before the original name goes away. Fails, with the original still in place, where
    /// hard links are not supported.
    HardLinkThenRemove,
}

/// Options for [`quarantine_database_opts`]. `Default` matches [`quarantine_database`].
#[derive(Clone, Debug, Default)]
pub struct QuarantineOptions {
    /// Suffix format appended to the database file name.
    pub name: QuarantineStamp,
    /// What happens to the `-wal`, `-shm` and `-journal` sidecars.
    pub sidecars: SidecarPolicy,
    /// How the main file and the sidecars leave their original names.
    pub move_mode: MoveMode,
    /// Recovery marker written atomically (owner-only) before any file moves. It is JSON:
    /// `{"schema", "original", "quarantined", "unixMs"}`. It stays in place after success; the app
    /// removes it when recovery is complete.
    pub marker: Option<PathBuf>,
}

/// [`quarantine_database`] with full [`QuarantineOptions`]. The same refusal rules apply: an
/// existing destination is an error, and sidecar failures are errors.
pub fn quarantine_database_opts(db: &Path, options: &QuarantineOptions) -> Result<PathBuf> {
    let name = db
        .file_name()
        .ok_or(DbError::InvalidDatabasePath)?
        .to_string_lossy()
        .into_owned();
    let suffix = options.name.suffix(SystemTime::now());
    if suffix.is_empty() || suffix.contains(['/', '\\', '\0']) {
        return Err(DbError::InvalidQuarantineSuffix(suffix));
    }
    let dest = db.with_file_name(format!("{name}{suffix}"));
    if dest.exists() {
        return Err(DbError::Fs(rightkit_fs::FsError::io(
            &dest,
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "quarantine destination already exists",
            ),
        )));
    }
    if let Some(marker) = &options.marker {
        write_marker(marker, db, &dest)?;
    }
    move_file(db, &dest, options.move_mode)
        .map_err(|e| DbError::Fs(rightkit_fs::FsError::io(db, e)))?;
    for sidecar in ["-wal", "-shm", "-journal"] {
        let from = with_suffix(db, sidecar);
        match options.sidecars {
            SidecarPolicy::RenameSidecars => {
                move_if_present(&from, &with_suffix(&dest, sidecar), options.move_mode)?
            }
            SidecarPolicy::DeleteSidecars => remove_if_present(&from)?,
        }
    }
    Ok(dest)
}

fn move_file(from: &Path, to: &Path, mode: MoveMode) -> std::io::Result<()> {
    match mode {
        MoveMode::Rename => std::fs::rename(from, to),
        MoveMode::HardLinkThenRemove => {
            std::fs::hard_link(from, to)?;
            std::fs::remove_file(from)
        }
    }
}

fn move_if_present(from: &Path, to: &Path, mode: MoveMode) -> Result<()> {
    match move_file(from, to, mode) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(DbError::Fs(rightkit_fs::FsError::io(from, error))),
    }
}

fn write_marker(marker: &Path, original: &Path, quarantined: &Path) -> Result<()> {
    let unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let body = format!(
        "{{\"schema\":\"rightkit-db-quarantine.v1\",\"original\":{},\"quarantined\":{},\"unixMs\":{}}}\n",
        json_string(&original.to_string_lossy()),
        json_string(&quarantined.to_string_lossy()),
        unix_ms
    );
    rightkit_fs::atomic_write_private(marker, body.as_bytes())?;
    Ok(())
}

/// A JSON string literal for `value`, escaping quotes, backslashes and control characters.
fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(DbError::Fs(rightkit_fs::FsError::io(path, error))),
    }
}

/// Rotate the SQLCipher key in place (`PRAGMA rekey`, atomic inside SQLCipher) and verify the
/// connection still reads. The caller stages the new key in its credential store *before* calling
/// this and promotes it after; an interruption between is recovered with
/// [`DbOptions::open_writer_with_key_candidates`].
#[cfg(feature = "cipher")]
pub fn rekey(conn: &Connection, new_key: &str) -> Result<()> {
    crate::validate_key(Some(new_key))?;
    let statement = Zeroizing::new(format!("PRAGMA rekey = '{new_key}';"));
    conn.execute_batch(statement.as_str())?;
    crate::verify_key(conn)
}
