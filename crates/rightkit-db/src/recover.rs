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
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RecoveryPolicy {
    /// Quarantine-and-recreate an unreadable *plain* database.
    pub quarantine_corrupt: bool,
    /// Also allow it for a keyed database (destroys access to data a wrong key could have read).
    pub quarantine_keyed: bool,
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
                let moved = quarantine_database(&self.path)?;
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

fn is_unreadable(error: &DbError) -> bool {
    matches!(
        error,
        DbError::Sqlite(rusqlite::Error::SqliteFailure(failure, _))
            if matches!(failure.code, ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt)
    )
}

/// Move `db` and its `-wal` / `-shm` / `-journal` sidecars to `<db>.corrupt-<unix-ms>[...]`.
/// Returns the new main-file path. Nothing is deleted.
pub fn quarantine_database(db: &Path) -> Result<PathBuf> {
    let name = db
        .file_name()
        .ok_or(DbError::InvalidDatabasePath)?
        .to_string_lossy()
        .into_owned();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let dest = db.with_file_name(format!("{name}.corrupt-{stamp}"));
    std::fs::rename(db, &dest).map_err(|e| DbError::Fs(rightkit_fs::FsError::io(db, e)))?;
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut from = db.as_os_str().to_os_string();
        from.push(suffix);
        let mut to = dest.as_os_str().to_os_string();
        to.push(suffix);
        let _ = std::fs::rename(PathBuf::from(from), PathBuf::from(to));
    }
    Ok(dest)
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
