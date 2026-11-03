//! Phase (c) of the index step: the short write that stores embedded vectors.
//!
//! The batch was read in phase (a) and embedded with no lock held, so the
//! message may have been deleted or its text changed in between. Each vector
//! is stored only when its message still exists and the text it is made from
//! still hashes the same; otherwise it is dropped and the cursor is not
//! advanced past it, so the row is re-read and re-embedded by a later step.

use rightkit_db::rusqlite::types::Value;
use rightkit_db::rusqlite::{params_from_iter, Connection};
use std::collections::HashMap;

use super::embedder::STORED_DIM;
use super::index::{hash_text, max_message_rowid, EmbeddedBatch, TEXT_EXPR};
use super::storage::{self, VecRow};

/// Current text hash for each of `rowids` that still exists in `account_id`.
fn current_hashes(
    conn: &Connection,
    account_id: &str,
    rowids: &[i64],
) -> Result<HashMap<i64, u64>, String> {
    if rowids.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = vec!["?"; rowids.len()].join(",");
    let sql = format!(
        "SELECT m.rowid, {TEXT_EXPR}
         FROM messages m
         WHERE m.account_id = ?1 AND m.rowid IN ({placeholders})"
    );
    let mut values: Vec<Value> = Vec::with_capacity(rowids.len() + 1);
    values.push(Value::Text(account_id.to_string()));
    values.extend(rowids.iter().map(|r| Value::Integer(*r)));
    let rows: Vec<(i64, String)> = crate::db::query_map_collect(
        conn,
        &sql,
        params_from_iter(values.iter()),
        "semantic_index_commit_guard",
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(rows
        .into_iter()
        .map(|(rowid, text)| (rowid, hash_text(&text)))
        .collect())
}

/// Store the still-valid vectors and advance the cursor. The caller holds the
/// writer for this one short call (no embedding happens here).
pub(super) fn commit_step(
    conn: &Connection,
    account_id: &str,
    model_id: &str,
    pipeline_key: &str,
    batch: EmbeddedBatch,
) -> Result<usize, String> {
    let rowids: Vec<i64> = batch.rows.iter().map(|r| r.rowid).collect();
    let current = current_hashes(conn, account_id, &rowids)?;

    let mut keep: Vec<VecRow> = Vec::with_capacity(batch.rows.len());
    let mut advance_to = batch.state.cursor_rowid;
    let mut blocked = false;
    for row in &batch.rows {
        match current.get(&row.rowid) {
            // Deleted since phase (a): nothing to store, safe to pass.
            None => {
                if !blocked {
                    advance_to = advance_to.max(row.rowid);
                }
            }
            Some(h) if *h == row.hash => {
                keep.push(VecRow {
                    rowid: row.rowid,
                    embedding: row.embedding,
                });
                if !blocked {
                    advance_to = advance_to.max(row.rowid);
                }
            }
            // Changed since phase (a): drop the vector, stop advancing so
            // the next step re-reads this row.
            Some(_) => blocked = true,
        }
    }

    // Vectors, the cursor and the pipeline key that describes them commit
    // together in ONE immediate transaction: a failure leaves none of them.
    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(|e| format!("begin vector commit: {e}"))?;
    let written = write_vectors_and_state(
        conn,
        account_id,
        model_id,
        pipeline_key,
        &batch.state,
        &keep,
        advance_to,
    );
    match written {
        Ok(stored) => {
            conn.execute_batch("COMMIT")
                .map_err(|e| format!("commit vector commit: {e}"))?;
            Ok(if stored { keep.len() } else { 0 })
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// The writes of one commit, run inside the caller's transaction. Returns
/// `false` (writing nothing) when the batch is stale: the stored model, dim
/// or pipeline key no longer match those the batch was read under (a reset or
/// a newer worker changed them), so its vectors would be in the wrong space.
fn write_vectors_and_state(
    conn: &Connection,
    account_id: &str,
    model_id: &str,
    pipeline_key: &str,
    read_state: &storage::SemanticIndexState,
    keep: &[VecRow],
    advance_to: i64,
) -> Result<bool, String> {
    let batch_cursor = read_state.cursor_rowid;
    // Re-read the state under the write lock (compare-and-set): a concurrent
    // change may have reset the index or rewound the cursor (invalidate.rs).
    let mut state = storage::get_index_state(conn, account_id)?;
    if state.embed_model_id != read_state.embed_model_id
        || state.embed_dim != read_state.embed_dim
        || state.pipeline_key != read_state.pipeline_key
    {
        return Ok(false);
    }
    for row in keep {
        storage::upsert_vector(conn, row.rowid, &row.embedding)?;
    }

    // Never move the cursor forward over a rewind, never move it backward.
    if state.cursor_rowid >= batch_cursor {
        state.cursor_rowid = state.cursor_rowid.max(advance_to);
    }
    state.embed_model_id = model_id.to_string();
    state.embed_dim = STORED_DIM as i64;
    state.pipeline_key = Some(pipeline_key.to_string());
    state.indexed_count += keep.len() as i64;
    let max_rowid = max_message_rowid(conn, account_id)?;
    state.done = state.cursor_rowid >= max_rowid;
    storage::upsert_index_state(conn, &state)?;
    Ok(true)
}
