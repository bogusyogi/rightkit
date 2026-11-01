//! Resumable, throttled background embedding index job (C5 item 3). Mirrors
//! `sync::backfill::run_backfill_step`'s shape: batch N rows, do the
//! expensive work OUTSIDE any DB lock, then one short write transaction.
//!
//! LOCK DISCIPLINE (non-negotiable per the C5 spec): embedding inference is
//! pure CPU on text already read from the DB — it never runs while a DB
//! mutex is held. Three phases: (a) [`prepare_step`] brief read of a batch of
//! (rowid, text, hash); (b) [`embed_batch`] with no lock held; (c)
//! [`commit_step`] brief write that stores only vectors whose row still
//! exists with an unchanged text hash (`index_commit.rs`). The embedder
//! itself holds its own internal `Mutex` (see embedder.rs) which is
//! independent of and never nested inside the DB locks.

use rightkit_db::rusqlite::{params, Connection};
use std::hash::{Hash, Hasher};

use super::embedder::{Embedder, STORED_DIM};
use super::index_commit;
use super::storage::{self, SemanticIndexState};

/// One batch step's worth of work (design: 16).
const INDEX_BATCH: usize = 16;

/// SQL expression for the text that gets embedded (subject + body, the same
/// source FTS indexes — no refetch from Gmail, purely local). Shared by the
/// batch read and by the commit-time guard that re-reads the row, so both
/// hash exactly the same bytes.
pub(super) const TEXT_EXPR: &str = "COALESCE(m.subject, '') || ' ' || COALESCE(
                  (SELECT body FROM message_bodies b
                     WHERE b.account_id = m.account_id AND b.message_id = m.id AND b.mime_type = 'text/plain'),
                  (SELECT body FROM message_bodies b
                     WHERE b.account_id = m.account_id AND b.message_id = m.id AND b.mime_type = 'text/html'),
                  m.snippet, '')";

/// Stable-within-process hash of the embedded text; phase (c) compares it
/// against the row's current text to detect a change since phase (a).
pub(super) fn hash_text(text: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// A row of text ready to embed: the message rowid, the concatenated
/// subject+body text and its content hash.
pub(super) struct PendingRow {
    pub(super) rowid: i64,
    pub(super) text: String,
    pub(super) hash: u64,
}

/// Phase (a) output: the state read plus the rows to embed.
pub struct PendingBatch {
    pub(super) state: SemanticIndexState,
    pub(super) rows: Vec<PendingRow>,
}

/// Phase (a) result: nothing to do, or a batch to embed.
pub enum StepPlan {
    Idle,
    Embed(PendingBatch),
}

/// One embedded row, still carrying the hash of the text it was made from.
pub(super) struct EmbeddedRow {
    pub(super) rowid: i64,
    pub(super) hash: u64,
    pub(super) embedding: [f32; STORED_DIM],
}

/// Phase (b) output, consumed by [`commit_step`].
pub struct EmbeddedBatch {
    pub(super) state: SemanticIndexState,
    pub(super) rows: Vec<EmbeddedRow>,
}

/// Read up to `limit` messages for `account_id` with `rowid > cursor_rowid`
/// that don't yet have a vector, ordered by rowid so the cursor advances
/// monotonically and a resume never skips or repeats rows.
fn read_pending_batch(
    conn: &Connection,
    account_id: &str,
    cursor_rowid: i64,
    limit: usize,
) -> Result<Vec<PendingRow>, String> {
    let sql = format!(
        "SELECT m.rowid, {TEXT_EXPR}
         FROM messages m
         WHERE m.account_id = ?1
           AND m.rowid > ?2
           AND NOT EXISTS (SELECT 1 FROM message_vec v WHERE v.rowid = m.rowid)
         ORDER BY m.rowid ASC
         LIMIT ?3"
    );
    let rows: Vec<(i64, String)> = crate::db::query_map_collect(
        conn,
        &sql,
        params![account_id, cursor_rowid, limit as i64],
        "semantic_index_pending_batch",
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(rows
        .into_iter()
        .map(|(rowid, text)| PendingRow {
            rowid,
            hash: hash_text(&text),
            text,
        })
        .collect())
}

/// Highest message rowid for the account — lets the job detect "caught up"
/// (cursor has passed every currently-mirrored message) vs "more to do".
pub(super) fn max_message_rowid(conn: &Connection, account_id: &str) -> Result<i64, String> {
    conn.query_row(
        "SELECT COALESCE(MAX(rowid), 0) FROM messages WHERE account_id = ?1",
        params![account_id],
        |r| r.get(0),
    )
    .map_err(|e| format!("max message rowid: {e}"))
}

/// Phase (a): short database work that selects the next batch. Also does the
/// model-change reset and the "caught up" bookkeeping (both short writes) and
/// returns [`StepPlan::Idle`] when there is nothing to embed.
///
/// `read_conn`/`write_conn` are separate parameters so callers may pass the
/// reader for the read and the writer for the writes; this function takes no
/// app-level lock itself.
pub fn prepare_step(
    read_conn: &Connection,
    write_conn: &Connection,
    account_id: &str,
    model_id: &str,
    pipeline_key: &str,
) -> Result<StepPlan, String> {
    let mut state = storage::get_index_state(read_conn, account_id)?;

    // Model/dim change: reset progress so stale wrong-space vectors are never
    // mixed with the new model's output (C5 item 1).
    if storage::needs_reindex(&state, model_id, STORED_DIM, pipeline_key) {
        storage::reset_index_for_account(write_conn, account_id)?;
        state = storage::get_index_state(read_conn, account_id)?;
    }

    if state.done
        && state.embed_model_id == model_id
        && state.embed_dim == STORED_DIM as i64
        && state.pipeline_key.as_deref() == Some(pipeline_key)
    {
        return Ok(StepPlan::Idle);
    }

    let rows = read_pending_batch(read_conn, account_id, state.cursor_rowid, INDEX_BATCH)?;
    if rows.is_empty() {
        // Only mark "done" once the cursor has actually reached the current
        // max rowid — an empty batch at a stale cursor is "caught up for
        // now", not "done forever".
        let max_rowid = max_message_rowid(read_conn, account_id)?;
        state.embed_model_id = model_id.to_string();
        state.embed_dim = STORED_DIM as i64;
        state.pipeline_key = Some(pipeline_key.to_string());
        state.done = state.cursor_rowid >= max_rowid;
        storage::upsert_index_state(write_conn, &state)?;
        return Ok(StepPlan::Idle);
    }
    Ok(StepPlan::Embed(PendingBatch { state, rows }))
}

/// Phase (b): pure CPU inference on text already in memory. MUST be called
/// with no database lock held.
pub fn embed_batch(batch: PendingBatch, embedder: &dyn Embedder) -> Result<EmbeddedBatch, String> {
    let mut rows = Vec::with_capacity(batch.rows.len());
    for row in &batch.rows {
        let native = embedder.embed(&row.text)?;
        let mut arr = [0f32; STORED_DIM];
        arr.copy_from_slice(&native);
        rows.push(EmbeddedRow {
            rowid: row.rowid,
            hash: row.hash,
            embedding: arr,
        });
    }
    Ok(EmbeddedBatch {
        state: batch.state,
        rows,
    })
}

/// Phase (c): short write; see `index_commit::commit_step`.
pub fn commit_step(
    write_conn: &Connection,
    account_id: &str,
    model_id: &str,
    pipeline_key: &str,
    batch: EmbeddedBatch,
) -> Result<usize, String> {
    index_commit::commit_step(write_conn, account_id, model_id, pipeline_key, batch)
}

/// All three phases on the given connections (tests, probes). Production
/// callers run the phases separately so the embedding holds no lock
/// (commands/semantic.rs).
#[cfg(test)]
pub fn run_index_step(
    read_conn: &Connection,
    write_conn: &Connection,
    account_id: &str,
    embedder: &dyn Embedder,
) -> Result<usize, String> {
    let key = embedder.index_key();
    match prepare_step(read_conn, write_conn, account_id, embedder.model_id(), &key)? {
        StepPlan::Idle => Ok(0),
        StepPlan::Embed(batch) => {
            let embedded = embed_batch(batch, embedder)?;
            commit_step(write_conn, account_id, embedder.model_id(), &key, embedded)
        }
    }
}

/// Progress snapshot for the Settings/search-surface UI (mirrors
/// `ArchiveProgress`'s shape in commands/mail/archive_status.rs).
#[derive(serde::Serialize, Clone)]
pub struct SemanticIndexProgress {
    pub indexed_count: i64,
    pub total_messages: i64,
    pub done: bool,
    pub embed_model_id: String,
}

pub fn index_progress(
    conn: &Connection,
    account_id: &str,
) -> Result<SemanticIndexProgress, String> {
    let state: SemanticIndexState = storage::get_index_state(conn, account_id)?;
    let total_messages = max_message_rowid(conn, account_id).map(|_| {
        // total mirrored message COUNT (not max rowid, which can be sparse
        // after deletes) — a second cheap count query, acceptable since this
        // is a UI-poll path, not the hot index loop.
        conn.query_row(
            "SELECT count(*) FROM messages WHERE account_id = ?1",
            params![account_id],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
    })?;
    Ok(SemanticIndexProgress {
        indexed_count: state.indexed_count,
        total_messages,
        done: state.done,
        embed_model_id: state.embed_model_id,
    })
}

#[cfg(test)]
pub(super) mod test_support {
    use super::*;

    /// Deterministic fake embedder for tests — no ONNX runtime needed. Maps
    /// text to a vector via a stable hash so "same text -> same vector" and
    /// different texts differ, without pulling in fastembed for a unit test.
    pub struct FakeEmbedder {
        pub dim: usize,
        pub model_id: &'static str,
    }

    impl Embedder for FakeEmbedder {
        fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
            let mut v = vec![0f32; self.dim];
            let mut h: u64 = 1469598103934665603;
            for b in text.bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(1099511628211);
            }
            v[0] = ((h % 1000) as f32) / 1000.0;
            v[1] = 1.0 - v[0];
            Ok(v)
        }
        fn embed_query(&self, text: &str) -> Result<Vec<f32>, String> {
            self.embed(text)
        }
        fn model_id(&self) -> &'static str {
            self.model_id
        }
    }

    pub fn conn_with_schema() -> Connection {
        crate::db::semantic::vec_ext::ensure_registered();
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::migrate(&conn).unwrap();
        conn
    }

    pub fn seed_messages(conn: &Connection, account_id: &str, n: usize) {
        conn.execute(
            "INSERT OR IGNORE INTO accounts(id, created_at) VALUES (?1, 0)",
            params![account_id],
        )
        .unwrap();
        for i in 0..n {
            conn.execute(
                "INSERT INTO messages(account_id, id, thread_id, subject, snippet, internal_date)
                 VALUES (?1, ?2, ?3, ?4, '', ?5)",
                params![
                    account_id,
                    format!("m{i}"),
                    format!("t{i}"),
                    format!("subject about topic {i}"),
                    i as i64
                ],
            )
            .unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{conn_with_schema, seed_messages, FakeEmbedder};
    use super::*;

    #[test]
    fn index_step_embeds_a_batch_and_advances_cursor() {
        let conn = conn_with_schema();
        seed_messages(&conn, "a1", 5);
        let embedder = FakeEmbedder {
            dim: STORED_DIM,
            model_id: "fake-v1",
        };
        let n = run_index_step(&conn, &conn, "a1", &embedder).unwrap();
        assert_eq!(n, 5);
        let state = storage::get_index_state(&conn, "a1").unwrap();
        assert_eq!(state.indexed_count, 5);
        assert!(state.done, "5 messages in one batch of 16 should finish");
        let count: i64 = conn
            .query_row("SELECT count(*) FROM message_vec", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 5);
    }

    #[test]
    fn index_step_resumes_from_a_mid_point() {
        let conn = conn_with_schema();
        seed_messages(&conn, "a1", 60); // > INDEX_BATCH(16), forces multiple steps
        let embedder = FakeEmbedder {
            dim: STORED_DIM,
            model_id: "fake-v1",
        };
        let n1 = run_index_step(&conn, &conn, "a1", &embedder).unwrap();
        assert_eq!(n1, 16, "first step embeds exactly one batch");
        let state_after_1 = storage::get_index_state(&conn, "a1").unwrap();
        assert!(!state_after_1.done);
        assert_eq!(state_after_1.indexed_count, 16);

        let n2 = run_index_step(&conn, &conn, "a1", &embedder).unwrap();
        assert_eq!(n2, 16, "second step resumes past the first batch's cursor");
        let n3 = run_index_step(&conn, &conn, "a1", &embedder).unwrap();
        assert_eq!(n3, 16);
        let n4 = run_index_step(&conn, &conn, "a1", &embedder).unwrap();
        assert_eq!(n4, 12, "fourth step finishes the remaining 12");
        let final_state = storage::get_index_state(&conn, "a1").unwrap();
        assert!(final_state.done);
        assert_eq!(final_state.indexed_count, 60);

        // A step after done is a no-op (0), never re-embeds.
        let n5 = run_index_step(&conn, &conn, "a1", &embedder).unwrap();
        assert_eq!(n5, 0);
    }

    #[test]
    fn model_change_triggers_reindex_from_local_bodies() {
        let conn = conn_with_schema();
        seed_messages(&conn, "a1", 3);
        let embedder_v1 = FakeEmbedder {
            dim: STORED_DIM,
            model_id: "fake-v1",
        };
        run_index_step(&conn, &conn, "a1", &embedder_v1).unwrap();
        let state = storage::get_index_state(&conn, "a1").unwrap();
        assert!(state.done);
        assert_eq!(state.embed_model_id, "fake-v1");

        // Switching models mid-flight must wipe the old vectors + cursor
        // instead of mixing embedding spaces.
        let embedder_v2 = FakeEmbedder {
            dim: STORED_DIM,
            model_id: "fake-v2",
        };
        let n = run_index_step(&conn, &conn, "a1", &embedder_v2).unwrap();
        assert_eq!(n, 3, "reindex re-embeds all 3 messages under the new model");
        let state_v2 = storage::get_index_state(&conn, "a1").unwrap();
        assert_eq!(state_v2.embed_model_id, "fake-v2");
        assert!(state_v2.done);
    }

    #[test]
    fn index_progress_reports_counts() {
        let conn = conn_with_schema();
        seed_messages(&conn, "a1", 4);
        let embedder = FakeEmbedder {
            dim: STORED_DIM,
            model_id: "fake-v1",
        };
        run_index_step(&conn, &conn, "a1", &embedder).unwrap();
        let progress = index_progress(&conn, "a1").unwrap();
        assert_eq!(progress.indexed_count, 4);
        assert_eq!(progress.total_messages, 4);
        assert!(progress.done);
    }
}
