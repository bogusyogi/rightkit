//! Keeps `message_vec` in step with the `messages` table.
//!
//! `message_vec.rowid = messages.rowid`, and `messages` has no AUTOINCREMENT,
//! so SQLite may hand a deleted message's rowid to a later message. A vector
//! left behind would then be inherited by the wrong message, and the indexer's
//! "already has a vector" check would skip it. These hooks run at exactly the
//! points where the full-text index is maintained (`db/fts_index.rs`):
//!
//! - [`remove_message_vector`]  <- `fts_index::remove_message`  (before the row is deleted)
//! - [`invalidate_messages`]    <- `fts_index::index_messages`  (indexed text may have changed)
//! - [`clear_account`]          <- `fts_index::remove_account`  (before the rows go away)
//!
//! Every function is a no-op on a database without the vec table. The index
//! cursor is rewound when a vector is dropped for a changed message so the
//! indexer re-embeds it (a finished index would otherwise never look again).

use rightkit_db::rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;

use super::index::hash_text;
use super::storage;

fn vec_table_exists(conn: &Connection) -> Result<bool, String> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'message_vec')",
        [],
        |r| r.get(0),
    )
    .map_err(|e| format!("find message_vec: {e}"))
}

/// Delete the vectors of `message_ids`; returns (vectors removed, lowest rowid seen).
fn drop_vectors(
    conn: &Connection,
    account_id: &str,
    message_ids: &[String],
) -> Result<(i64, Option<i64>), String> {
    let mut removed = 0i64;
    let mut min_rowid: Option<i64> = None;
    for id in message_ids {
        let rowid: Option<i64> = conn
            .query_row(
                "SELECT rowid FROM messages WHERE account_id = ?1 AND id = ?2",
                params![account_id, id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| format!("find vector row: {e}"))?;
        let Some(rowid) = rowid else { continue };
        min_rowid = Some(min_rowid.map_or(rowid, |m| m.min(rowid)));
        let had: Option<i64> = conn
            .query_row(
                "SELECT rowid FROM message_vec WHERE rowid = ?1",
                [rowid],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| format!("find vector: {e}"))?;
        if had.is_some() {
            storage::delete_vectors_by_rowids(conn, &[rowid])?;
            removed += 1;
        }
    }
    Ok((removed, min_rowid))
}

fn discount_indexed(conn: &Connection, account_id: &str, removed: i64) -> Result<(), String> {
    if removed == 0 {
        return Ok(());
    }
    conn.execute(
        "UPDATE semantic_index_state SET indexed_count = MAX(indexed_count - ?1, 0)
         WHERE account_id = ?2",
        params![removed, account_id],
    )
    .map_err(|e| format!("discount indexed count: {e}"))?;
    Ok(())
}

/// Drop one message's vector. Run BEFORE its `messages` row is deleted.
pub(crate) fn remove_message_vector(
    conn: &Connection,
    account_id: &str,
    message_id: &str,
) -> Result<(), String> {
    if !vec_table_exists(conn)? {
        return Ok(());
    }
    let (removed, _) = drop_vectors(conn, account_id, &[message_id.to_string()])?;
    discount_indexed(conn, account_id, removed)
}

/// The indexed text of these messages may have changed (or they are new):
/// drop their vectors and rewind the account's cursor so the indexer
/// re-embeds them. Called wherever the full-text rows are re-indexed.
pub(crate) fn invalidate_messages(
    conn: &Connection,
    account_id: &str,
    message_ids: &[String],
) -> Result<(), String> {
    if message_ids.is_empty() || !vec_table_exists(conn)? {
        return Ok(());
    }
    let (removed, min_rowid) = drop_vectors(conn, account_id, message_ids)?;
    discount_indexed(conn, account_id, removed)?;
    if let Some(min) = min_rowid {
        // No state row (never indexed) -> no-op.
        conn.execute(
            "UPDATE semantic_index_state
             SET cursor_rowid = MIN(cursor_rowid, ?1 - 1), done = 0
             WHERE account_id = ?2",
            params![min, account_id],
        )
        .map_err(|e| format!("rewind semantic cursor: {e}"))?;
    }
    Ok(())
}

/// Fingerprint of the indexed plain text (subject + body) of each message
/// that has a search row. Taken BEFORE the full-text rows are re-indexed and
/// compared after by [`invalidate_changed`], so a re-fetch that only changed
/// labels, read or star state never costs a vector. Empty (no work) for an
/// account that has no semantic progress row yet.
pub(crate) fn text_fingerprints(
    conn: &Connection,
    account_id: &str,
    message_ids: &[String],
) -> Result<HashMap<String, u64>, String> {
    let mut out = HashMap::new();
    if message_ids.is_empty() || !vec_table_exists(conn)? {
        return Ok(out);
    }
    let tracked: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM semantic_index_state WHERE account_id = ?1)",
            [account_id],
            |r| r.get(0),
        )
        .map_err(|e| format!("find semantic state: {e}"))?;
    if !tracked {
        return Ok(out);
    }
    for id in message_ids {
        let text: Option<String> = conn
            .query_row(
                "SELECT s.subject || ' ' || s.body
                 FROM messages m JOIN message_search s ON s.id = m.rowid
                 WHERE m.account_id = ?1 AND m.id = ?2",
                params![account_id, id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| format!("read indexed text: {e}"))?;
        if let Some(text) = text {
            out.insert(id.clone(), hash_text(&text));
        }
    }
    Ok(out)
}

fn has_vector(conn: &Connection, account_id: &str, message_id: &str) -> Result<bool, String> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM message_vec v JOIN messages m ON m.rowid = v.rowid
                       WHERE m.account_id = ?1 AND m.id = ?2)",
        params![account_id, message_id],
        |r| r.get(0),
    )
    .map_err(|e| format!("find vector: {e}"))
}

/// Like [`invalidate_messages`], but a message whose indexed text is the same
/// as in `before` (see [`text_fingerprints`]) and that already has a vector is
/// left alone: only a body or subject change, a new message, or a message
/// without a vector is re-queued. Deletions go through
/// [`remove_message_vector`].
pub(crate) fn invalidate_changed(
    conn: &Connection,
    account_id: &str,
    message_ids: &[String],
    before: &HashMap<String, u64>,
) -> Result<(), String> {
    if message_ids.is_empty() || !vec_table_exists(conn)? {
        return Ok(());
    }
    let after = text_fingerprints(conn, account_id, message_ids)?;
    let mut stale: Vec<String> = Vec::new();
    for id in message_ids {
        let same = matches!((before.get(id), after.get(id)), (Some(b), Some(a)) if b == a);
        if !same || !has_vector(conn, account_id, id)? {
            stale.push(id.clone());
        }
    }
    invalidate_messages(conn, account_id, &stale)
}

/// Drop every vector (and the progress row) of one account. Run BEFORE the
/// account's `messages` rows go away, and before a full re-index of its text.
pub(crate) fn clear_account(conn: &Connection, account_id: &str) -> Result<(), String> {
    if !vec_table_exists(conn)? {
        return Ok(());
    }
    storage::reset_index_for_account(conn, account_id)
}
