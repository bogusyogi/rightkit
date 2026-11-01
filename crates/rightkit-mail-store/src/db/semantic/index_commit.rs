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

#[cfg(test)]
mod tests {
    use super::super::embedder::Embedder;
    use super::super::index::test_support::{conn_with_schema, seed_messages, FakeEmbedder};
    use super::super::index::{embed_batch, prepare_step, run_index_step, StepPlan};
    use super::*;

    fn emb(model: &'static str) -> FakeEmbedder {
        FakeEmbedder {
            dim: STORED_DIM,
            model_id: model,
        }
    }

    fn plan(conn: &Connection, e: &FakeEmbedder) -> EmbeddedBatch {
        match prepare_step(conn, conn, "a1", e.model_id(), &e.index_key()).unwrap() {
            StepPlan::Embed(b) => embed_batch(b, e).unwrap(),
            StepPlan::Idle => panic!("expected a batch"),
        }
    }

    fn vec_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT count(*) FROM message_vec", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn crash_rollback_leaves_no_vectors_and_no_cursor_advance() {
        let conn = conn_with_schema();
        seed_messages(&conn, "a1", 60);
        let e = emb("fake-v1");
        run_index_step(&conn, &conn, "a1", &e).unwrap();
        let before = storage::get_index_state(&conn, "a1").unwrap();
        let vecs_before = vec_count(&conn);
        let batch = plan(&conn, &e);
        // The state write fails after the vectors were written in the batch.
        conn.execute_batch(
            "CREATE TRIGGER boom BEFORE INSERT ON semantic_index_state
             BEGIN SELECT RAISE(ABORT, 'boom'); END;",
        )
        .unwrap();
        assert!(commit_step(&conn, "a1", e.model_id(), &e.index_key(), batch).is_err());
        conn.execute_batch("DROP TRIGGER boom").unwrap();
        assert_eq!(vec_count(&conn), vecs_before, "vectors rolled back");
        assert_eq!(storage::get_index_state(&conn, "a1").unwrap(), before);
    }

    #[test]
    fn stale_worker_cannot_regress_or_overwrite_newer_state() {
        let conn = conn_with_schema();
        seed_messages(&conn, "a1", 60);
        let e = emb("fake-v1");
        let older = plan(&conn, &e);
        let newer = plan(&conn, &e);
        commit_step(&conn, "a1", e.model_id(), &e.index_key(), newer).unwrap();
        let after_newer = storage::get_index_state(&conn, "a1").unwrap();
        // The older worker's batch was read under a pre-index (empty) state.
        assert_eq!(
            commit_step(&conn, "a1", e.model_id(), &e.index_key(), older).unwrap(),
            0
        );
        assert_eq!(storage::get_index_state(&conn, "a1").unwrap(), after_newer);

        // Same-identity older worker: the cursor never moves backward.
        let b1 = plan(&conn, &e);
        let b2 = plan(&conn, &e);
        commit_step(&conn, "a1", e.model_id(), &e.index_key(), b2).unwrap();
        let cur = storage::get_index_state(&conn, "a1").unwrap().cursor_rowid;
        commit_step(&conn, "a1", e.model_id(), &e.index_key(), b1).unwrap();
        assert!(storage::get_index_state(&conn, "a1").unwrap().cursor_rowid >= cur);
    }

    #[test]
    fn model_change_invalidates_in_flight_batch_and_reindexes() {
        let conn = conn_with_schema();
        seed_messages(&conn, "a1", 40);
        let v1 = emb("fake-v1");
        let v2 = emb("fake-v2");
        run_index_step(&conn, &conn, "a1", &v1).unwrap();
        let in_flight = plan(&conn, &v1);
        // Model switch resets the cursor while the v1 batch is embedding.
        run_index_step(&conn, &conn, "a1", &v2).unwrap();
        let st = storage::get_index_state(&conn, "a1").unwrap();
        assert_eq!(st.embed_model_id, "fake-v2");
        let vecs = vec_count(&conn);
        assert_eq!(
            commit_step(&conn, "a1", v1.model_id(), &v1.index_key(), in_flight).unwrap(),
            0,
            "stale v1 batch is dropped"
        );
        assert_eq!(vec_count(&conn), vecs);
        assert_eq!(storage::get_index_state(&conn, "a1").unwrap(), st);
        while run_index_step(&conn, &conn, "a1", &v2).unwrap() > 0 {}
        assert_eq!(vec_count(&conn), 40);
        assert!(storage::get_index_state(&conn, "a1").unwrap().done);
    }

    #[test]
    fn content_change_resets_cursor_and_reindexes_row() {
        let conn = conn_with_schema();
        seed_messages(&conn, "a1", 5);
        let e = emb("fake-v1");
        let batch = plan(&conn, &e);
        conn.execute(
            "UPDATE messages SET subject = 'changed text' WHERE account_id='a1' AND id='m2'",
            [],
        )
        .unwrap();
        commit_step(&conn, "a1", e.model_id(), &e.index_key(), batch).unwrap();
        let st = storage::get_index_state(&conn, "a1").unwrap();
        assert!(!st.done, "cursor must not pass the changed row");
        while run_index_step(&conn, &conn, "a1", &e).unwrap() > 0 {}
        assert_eq!(vec_count(&conn), 5, "changed row re-embedded");
    }

    #[test]
    fn rewind_during_commit_skips_no_rows() {
        let conn = conn_with_schema();
        seed_messages(&conn, "a1", 60);
        let e = emb("fake-v1");
        run_index_step(&conn, &conn, "a1", &e).unwrap();
        let batch = plan(&conn, &e);
        conn.execute(
            "UPDATE semantic_index_state SET cursor_rowid = 0, done = 0 WHERE account_id='a1'",
            [],
        )
        .unwrap();
        commit_step(&conn, "a1", e.model_id(), &e.index_key(), batch).unwrap();
        assert_eq!(
            storage::get_index_state(&conn, "a1").unwrap().cursor_rowid,
            0,
            "commit must not advance over a rewind"
        );
        while run_index_step(&conn, &conn, "a1", &e).unwrap() > 0 {}
        assert_eq!(vec_count(&conn), 60, "every row indexed after the rewind");
        assert!(storage::get_index_state(&conn, "a1").unwrap().done);
    }
}
