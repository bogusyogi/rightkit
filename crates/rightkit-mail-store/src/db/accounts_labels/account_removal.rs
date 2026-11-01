use super::*;

// Split out of `accounts_labels.rs` to keep that module under the 400-line
// cap (L7 added the transactional variant here alongside the original).

/// Transactional account removal (L7). Runs all the DELETEs plus the
/// removal audit-log row in ONE transaction and propagates every error
/// (`map_err` + `?`) instead of swallowing the four follow-up DELETEs. If
/// any statement after the first fails, the whole transaction rolls back —
/// the account row itself is NOT removed and no partial/orphaned rows are
/// left behind. `conn` needs to be `&mut` because an IMMEDIATE transaction
/// borrows it mutably.
///
/// Order matters: the FTS DELETE has to run BEFORE the `accounts` DELETE
/// because the rowid-join query reads from `messages`, and the FK cascade
/// on `accounts` would delete those rows first, leaving the FTS DELETE
/// running against an empty `messages` set and silently no-op'ing
/// (L15 regression caught by `account_lifecycle_tests::remove_account_tx_*`).
pub fn remove_account_tx(conn: &mut Connection, account_id: &str) -> Result<(), String> {
    let tx = rightkit_db::write_txn(conn).map_err(|e| format!("remove_account tx begin: {e}"))?;
    // 1. fts_messages FIRST (rowid-joined to messages — see comment above).
    crate::db::fts_index::remove_account(&tx, account_id)
        .map_err(|e| format!("delete fts_messages: {e}"))?;
    // 1b. vectors, same reason: keyed by messages.rowid.
    crate::db::semantic::invalidate::clear_account(&tx, account_id)
        .map_err(|e| format!("delete message_vec: {e}"))?;
    // 2. accounts — its FK cascade now clears messages / threads / labels /
    //    sync_state / aliases / drafts in one shot.
    tx.execute("DELETE FROM accounts WHERE id = ?1", [account_id])
        .map_err(|e| format!("delete account: {e}"))?;
    // 2b. the outbox and pending_ops job tables carry no foreign key, so the
    //     account's queued rows go in this same transaction.
    crate::db::job_tables::delete_account_rows(&tx, account_id)?;
    tx.execute(
        "DELETE FROM message_bodies WHERE account_id = ?1",
        [account_id],
    )
    .map_err(|e| format!("delete message_bodies: {e}"))?;
    tx.execute(
        "DELETE FROM attachments WHERE account_id = ?1",
        [account_id],
    )
    .map_err(|e| format!("delete attachments: {e}"))?;
    tx.execute(
        "DELETE FROM ai_summaries WHERE account_id = ?1",
        [account_id],
    )
    .map_err(|e| format!("delete ai_summaries: {e}"))?;
    tx.execute(
        "INSERT INTO audit_log(at, kind, account_id, detail) VALUES(?1, 'account', ?2, 'account removed')",
        rightkit_db::rusqlite::params![now_ms(), account_id],
    )
    .map_err(|e| format!("insert removal audit: {e}"))?;
    tx.commit()
        .map_err(|e| format!("remove_account tx commit: {e}"))
}
