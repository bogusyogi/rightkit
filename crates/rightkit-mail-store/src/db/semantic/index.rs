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
