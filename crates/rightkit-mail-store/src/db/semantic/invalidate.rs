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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::semantic::embedder::STORED_DIM;

    fn unit(seed: f32) -> [f32; STORED_DIM] {
        let mut v = [0f32; STORED_DIM];
        v[0] = seed;
        v[1] = (1.0 - seed * seed).max(0.0).sqrt();
        v
    }

    fn add_message(conn: &Connection, account: &str, id: &str) -> i64 {
        conn.execute(
            "INSERT OR IGNORE INTO accounts(id, created_at) VALUES (?1, 0)",
            [account],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages(account_id, id, thread_id, internal_date) VALUES (?1, ?2, 't', 0)",
            params![account, id],
        )
        .unwrap();
        conn.query_row(
            "SELECT rowid FROM messages WHERE account_id = ?1 AND id = ?2",
            params![account, id],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn vec_count(conn: &Connection, rowid: i64) -> i64 {
        conn.query_row(
            "SELECT count(*) FROM message_vec WHERE rowid = ?1",
            [rowid],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn deleted_message_vector_is_not_inherited_by_a_reused_rowid() {
        let conn = crate::db::test_conn();
        let keep = add_message(&conn, "a1", "m0");
        let gone = add_message(&conn, "a1", "m1");
        storage::upsert_vector(&conn, keep, &unit(0.2)).unwrap();
        storage::upsert_vector(&conn, gone, &unit(0.8)).unwrap();

        crate::db::delete_message_rows(&conn, "a1", &["m1".to_string()]).unwrap();
        assert_eq!(vec_count(&conn, gone), 0, "vector removed with its message");

        // The highest rowid was freed, so SQLite reuses it.
        let reused = add_message(&conn, "a1", "m2");
        assert_eq!(reused, gone, "test premise: rowid is reused");
        assert_eq!(vec_count(&conn, reused), 0, "new message has no vector");
        assert_eq!(vec_count(&conn, keep), 1, "other message untouched");
    }

    #[test]
    fn invalidate_drops_vector_and_rewinds_a_finished_index() {
        let conn = crate::db::test_conn();
        let r0 = add_message(&conn, "a1", "m0");
        let r1 = add_message(&conn, "a1", "m1");
        storage::upsert_vector(&conn, r0, &unit(0.2)).unwrap();
        storage::upsert_vector(&conn, r1, &unit(0.8)).unwrap();
        let mut st = storage::get_index_state(&conn, "a1").unwrap();
        st.embed_model_id = "fake".to_string();
        st.cursor_rowid = r1;
        st.indexed_count = 2;
        st.done = true;
        storage::upsert_index_state(&conn, &st).unwrap();

        invalidate_messages(&conn, "a1", &["m1".to_string()]).unwrap();

        assert_eq!(vec_count(&conn, r1), 0);
        assert_eq!(vec_count(&conn, r0), 1);
        let st = storage::get_index_state(&conn, "a1").unwrap();
        assert!(!st.done);
        assert_eq!(st.cursor_rowid, r1 - 1);
        assert_eq!(st.indexed_count, 1);
    }

    #[test]
    fn account_removal_clears_only_that_accounts_vectors() {
        let mut conn = crate::db::test_conn();
        let a = add_message(&conn, "a1", "m0");
        let b = add_message(&conn, "a2", "m0");
        storage::upsert_vector(&conn, a, &unit(0.3)).unwrap();
        storage::upsert_vector(&conn, b, &unit(0.6)).unwrap();

        crate::db::remove_account_tx(&mut conn, "a1").unwrap();

        assert_eq!(vec_count(&conn, a), 0);
        assert_eq!(vec_count(&conn, b), 1, "other account keeps its vector");
    }

    #[test]
    fn hooks_tolerate_a_database_without_the_vec_table() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::db_schema::SCHEMA_V1).unwrap();
        remove_message_vector(&conn, "a1", "m0").unwrap();
        invalidate_messages(&conn, "a1", &["m0".to_string()]).unwrap();
        clear_account(&conn, "a1").unwrap();
    }
}
