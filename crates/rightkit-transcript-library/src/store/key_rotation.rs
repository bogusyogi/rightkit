//! Explicit, user-initiated rotation of the SQLCipher history key.
//!
//! Order: refuse unless the old key is readable AND is the key that opens this
//! connection's file -> write new key to a staging account -> `PRAGMA rekey`
//! -> only on success promote to the primary account and delete staging. Any
//! failure before promotion deletes staging and leaves the DB on the old key.
//! If promotion fails after a successful rekey, rekey back to the old key; if
//! even that fails the staging key is KEPT (it is the only copy of the live
//! key) and the error says so. Recovery (quarantine) covers a missing old key.

use super::sqlcipher_conn::validate_sqlcipher_key_for_pragma;
use super::HistoryError;
use crate::host::KeyVault;
use rightkit_db::rusqlite;

fn rekey(conn: &rusqlite::Connection, key: &str) -> Result<(), HistoryError> {
    // The stricter (base64) key policy first; the owner then rekeys and verifies the
    // connection still reads.
    let key = validate_sqlcipher_key_for_pragma(key)?;
    rightkit_db::rekey(conn, key).map_err(|e| HistoryError::new(format!("rekey: {e}")))
}

/// Rotate using `conn`, which must be the live connection opened with the
/// current primary key. `new_key` is injected so tests are deterministic.
pub(super) fn rotate_with(
    conn: &rusqlite::Connection,
    vault: &dyn KeyVault,
    new_key: &str,
) -> Result<(), HistoryError> {
    let old_key = match vault.read_primary() {
        Ok(Some(key)) if !key.is_empty() => key,
        Ok(_) => {
            return Err(HistoryError::new(
                "key rotation refused: current key is unavailable (use history recovery)",
            ))
        }
        Err(err) => {
            return Err(HistoryError::new(format!(
                "key rotation refused: credential store: {err}"
            )))
        }
    };
    validate_sqlcipher_key_for_pragma(new_key)?;
    // A leftover staging key is from a rotation interrupted before its rekey
    // landed: this connection opened with the primary key, and the open path
    // tries staging before it ever quarantines, so no file depends on it.
    // Confirm it is readable (an unreadable store must not be overwritten
    // blind), then replace it.
    vault
        .read_staging()
        .map_err(|e| HistoryError::new(format!("key rotation refused: credential store: {e}")))?;
    vault
        .write_staging(new_key)
        .map_err(|e| HistoryError::new(format!("stage new key: {e}")))?;
    if let Err(err) = rekey(conn, new_key) {
        let _ = vault.delete_staging();
        return Err(err);
    }
    if let Err(err) = vault.write_primary(new_key) {
        return match rekey(conn, &old_key) {
            Ok(()) => {
                let _ = vault.delete_staging();
                Err(HistoryError::new(format!(
                    "key rotation rolled back: promote failed: {err}"
                )))
            }
            Err(back) => Err(HistoryError::new(format!(
                "key rotation failed: promote failed ({err}) and rollback failed ({back}); \
                 the new key is kept in the staging credential"
            ))),
        };
    }
    let _ = vault.delete_staging();
    Ok(())
}

impl super::SQLCipherHistoryStore {
    /// Rotate the history key on this store's live connection.
    pub fn rotate_key(&self) -> Result<(), HistoryError> {
        // A dev or QA run opens with the fixed dev key and must never touch the OS credential
        // store, so it cannot rotate: refuse before any credential or file operation.
        if crate::host::host().qa_credentials_isolated() {
            return Err(HistoryError::new(
                "key rotation refused: a dev or QA run never writes the OS credential store",
            ));
        }
        // Serialize with history open transitions (foreground runtime and
        // background CloudKit merge) so no opener reads the primary key while
        // the file and the credential disagree mid-rotation.
        let _transition = super::recovery::transition_guard()?;
        let new_key = crate::host::host()
            .new_db_key()
            .map_err(HistoryError::new)?;
        rotate_with(&self.conn, crate::host::host().key_vault(), &new_key)
    }
}

#[doc(hidden)]
pub fn rotate_with_for_test(
    conn: &rusqlite::Connection,
    vault: &dyn KeyVault,
    new_key: &str,
) -> Result<(), HistoryError> {
    rotate_with(conn, vault, new_key)
}
