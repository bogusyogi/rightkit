//! SQLCipher key application and post-key connection PRAGMA setup.
//!
//! Split out of `sqlcipher_store.rs`: opening a connection, applying the
//! SQLCipher key, and enforcing the PRAGMA contract are one concern that
//! `open_at` used to inline before it got to schema init and the legacy JSON
//! migration.
//!
//! This is also where lost-key / migrated-machine recovery is decided
//! (see `super::recovery`): a database file that predates this install but has
//! no readable credential is quarantined and rebuilt rather than wedging
//! history forever, while a transient credential-store failure (locked,
//! denied) surfaces unchanged and never touches the file.

use super::recovery::{db_files_have_content, quarantine_db_files, RecoveryReason};
use crate::history::HistoryError;
use rightkit_db::{rusqlite, DbError, DbOptions, DurabilityProfile, JournalSelect};
use std::path::Path;

/// Failure of `open_connection_keyed`. `unreadable_db` is true ONLY when the
/// database file itself failed to decipher/parse (SQLITE_NOTADB /
/// SQLITE_CORRUPT): the applied key cannot read it. Every other failure —
/// credential store, I/O, locks, malformed key string — propagates without
/// touching the file, because transient errors never justify quarantining
/// user data.
pub(super) struct OpenDbFailure {
    pub error: HistoryError,
    pub unreadable_db: bool,
}

impl OpenDbFailure {
    fn sqlite(context: &str, err: rusqlite::Error) -> Self {
        let unreadable_db = matches!(
            &err,
            rusqlite::Error::SqliteFailure(inner, _)
                if matches!(
                    inner.code,
                    rusqlite::ErrorCode::NotADatabase | rusqlite::ErrorCode::DatabaseCorrupt
                )
        );
        Self {
            error: HistoryError::new(format!("{context}: {err}")),
            unreadable_db,
        }
    }

    /// Map a `rightkit_db` open failure. Only a genuine SQLite error can be
    /// classified as an unreadable (wrong-key / corrupt) file; every other
    /// `DbError` (invalid key text, filesystem, feature) is "other" and never
    /// triggers quarantine.
    fn db(context: &str, err: DbError) -> Self {
        match err {
            DbError::Sqlite(inner) => Self::sqlite(context, inner),
            other => Self::other(HistoryError::new(format!("{context}: {other}"))),
        }
    }

    fn other(error: HistoryError) -> Self {
        Self {
            error,
            unreadable_db: false,
        }
    }
}

impl From<HistoryError> for OpenDbFailure {
    fn from(error: HistoryError) -> Self {
        Self::other(error)
    }
}

/// Open `db_path` and apply `key`, enforcing the post-key PRAGMA contract.
///
/// `rightkit_db` applies `PRAGMA key = '<key>'` as the first statement on the
/// connection (only the `sqlite3_busy_timeout` C call precedes it), then
/// deciphers page 1 with a `SELECT` on `sqlite_master` — a wrong key surfaces
/// there as SQLITE_NOTADB, which `OpenDbFailure` classifies for the recovery
/// decision in `open_and_key`. The key stays a passphrase-style key (SQLCipher
/// runs its KDF over it), exactly as before, so existing databases open
/// unchanged. The remaining PRAGMA contract is re-asserted by
/// `configure_connection_after_key`.
fn open_connection_keyed(db_path: &Path, key: &str) -> Result<rusqlite::Connection, OpenDbFailure> {
    // Validated as base64 (a stricter set than rightkit-db's own check).
    let key = validate_sqlcipher_key_for_pragma(key).map_err(OpenDbFailure::from)?;
    let mut options = DbOptions::new(db_path).with_passphrase(key);
    // WAL or fail: never silently downgrade to a rollback journal; the
    // contract check below rejects anything else.
    options.journal = JournalSelect::WalStrict;
    // `PRAGMA synchronous=NORMAL`, as before.
    options.durability = DurabilityProfile::Normal;
    // Keep file ACLs exactly as before; this port does not harden them.
    options.owner_only = false;
    // Deleted history is overwritten on disk (the owner applies it on every connection).
    options.secure_delete = Some(true);
    let conn = options
        .open_writer()
        .map_err(|err| OpenDbFailure::db("open keyed sqlite", err))?;
    configure_connection_after_key(&conn)?;
    Ok(conn)
}

/// Open the SQLite file at `db_path`, apply the SQLCipher key from the
/// credential store, and enforce the post-key PRAGMA contract. Returns a
/// connection ready for `initialize_schema`.
///
/// Recovery contract:
/// - credential store `Err` (locked/denied/unavailable): surface it, change
///   nothing — the real key may return.
/// - credential absent + existing DB content: the DB outlived its key
///   (migration). Quarantine, mint a fresh key, rebuild empty.
/// - stored key + DB that fails decipher: key doesn't match this file
///   (foreign/migrated DB or corruption). Quarantine, rebuild with the same
///   stored key.
/// - credential absent + no DB: plain first run — mint and open.
pub(super) fn open_and_key(db_path: &Path) -> Result<rusqlite::Connection, HistoryError> {
    // Serialize the whole complete-pending → classify → mint → quarantine →
    // open transition so concurrent opens (foreground runtime vs. background
    // CloudKit merge) cannot interleave: without it a second opener could
    // mint a competing key while the first is mid-mint, or quarantine the
    // fresh replacement another opener just created.
    let _transition = super::recovery::transition_guard()?;
    // Finish or roll back any recorded-but-incomplete quarantine moves for
    // this DB set BEFORE touching the files — a missing main DB must never
    // be opened alongside a still-live predecessor WAL.
    super::recovery::complete_pending_quarantine(db_path)?;
    let db_preexisting = db_files_have_content(db_path)?;
    match crate::host::host().db_key_state() {
        Err(err) => Err(HistoryError::new(format!("credential store: {err}"))),
        Ok(stored_key) => open_and_key_resolved(db_path, db_preexisting, stored_key),
    }
}

fn open_and_key_resolved(
    db_path: &Path,
    db_preexisting: bool,
    stored_key: Option<String>,
) -> Result<rusqlite::Connection, HistoryError> {
    let Some(key) = stored_key else {
        // No credential. Mint + persist the replacement key BEFORE touching
        // the unreadable DB: if persisting the key fails, nothing moved and
        // the next launch retries cleanly. If quarantine then fails, the
        // minted key is already stored, so the next open reaches the
        // StoredKeyRejected arm below and retries the move.
        let minted = crate::host::host()
            .get_or_create_db_key()
            .map_err(|err| HistoryError::new(format!("credential store: {err}")))?;
        if db_preexisting {
            quarantine_db_files(db_path, RecoveryReason::KeychainKeyAbsent)?;
        }
        return open_connection_keyed(db_path, &minted).map_err(|failure| failure.error);
    };
    match open_connection_keyed(db_path, &key) {
        Ok(conn) => Ok(conn),
        Err(failure) if db_preexisting && failure.unreadable_db => {
            // A rotation that rekeyed the file but crashed before promoting
            // the new key leaves that key in staging. Recover with it before
            // concluding the data is unreadable.
            if let Some(conn) = recover_with_staging_key(db_path)? {
                return Ok(conn);
            }
            // The stored key cannot decipher this file. Rebuild fresh with the
            // SAME stored key — the credential is still valid, just not for a
            // DB that came from a different install (or is corrupt on disk).
            // This says nothing about whether the ORIGINAL key is lost for
            // good; the quarantined file is preserved until the recovery
            // marker's cleanup gate decides (or keeps it).
            quarantine_db_files(db_path, RecoveryReason::StoredKeyRejected)?;
            open_connection_keyed(db_path, &key).map_err(|retry| retry.error)
        }
        Err(failure) => Err(failure.error),
    }
}

/// The owner (`rightkit_db::DbOptions`) applies WAL, busy timeout, synchronous, journal size
/// limit, temp store, optimize and `secure_delete`; this store only re-asserts the one policy it
/// must never lose: deleted history is overwritten on disk. A mismatch is a configuration
/// violation, not a lost key, so it never triggers quarantine.
pub(super) fn configure_connection_after_key(
    conn: &rusqlite::Connection,
) -> Result<(), OpenDbFailure> {
    let secure_delete: i64 = conn
        .query_row("PRAGMA secure_delete", [], |row| row.get(0))
        .map_err(|err| OpenDbFailure::sqlite("read secure_delete", err))?;
    if secure_delete != 1 {
        return Err(OpenDbFailure::other(HistoryError::new(format!(
            "sqlite pragma contract mismatch: secure_delete={secure_delete}"
        ))));
    }
    Ok(())
}

pub fn validate_sqlcipher_key_for_pragma(s: &str) -> Result<&str, HistoryError> {
    if s.is_empty() {
        return Err(HistoryError::new("SQLCipher key is empty"));
    }
    if !s
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
    {
        return Err(HistoryError::new(
            "SQLCipher key contains non-base64 characters",
        ));
    }
    Ok(s)
}

/// Test seam: open through the production keyed-open path and return the
/// connection (the `unreadable_db` classification is exposed as the bool).
#[doc(hidden)]
pub fn open_connection_keyed_for_test(
    db_path: &Path,
    key: &str,
) -> Result<rusqlite::Connection, (HistoryError, bool)> {
    open_connection_keyed(db_path, key).map_err(|f| (f.error, f.unreadable_db))
}

/// Try the staging key from an interrupted rotation. On success promote it to
/// the primary credential, then drop staging. Staging is kept on any failure
/// so it is never lost.
fn recover_with_staging_key(db_path: &Path) -> Result<Option<rusqlite::Connection>, HistoryError> {
    use crate::host::KeyVault;
    let vault = crate::host::host().key_vault();
    let staged = match vault.read_staging() {
        Ok(Some(key)) => key,
        Ok(None) => return Ok(None),
        Err(err) => return Err(HistoryError::new(format!("credential store: {err}"))),
    };
    let Ok(conn) = open_connection_keyed(db_path, &staged) else {
        return Ok(None);
    };
    crate::host::host()
        .persist_db_key(&staged)
        .map_err(|err| HistoryError::new(format!("promote recovered key: {err}")))?;
    let _ = vault.delete_staging();
    Ok(Some(conn))
}
