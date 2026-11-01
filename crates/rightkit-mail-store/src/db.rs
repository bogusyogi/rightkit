pub(crate) mod fts_index;
pub(crate) mod fts_text;
mod migrations;
pub(crate) use migrations::ensure_runtime_schema;
// The schema migration runs only through `migration_gate::gate_and_migrate`:
// outside tests `migrate` is not visible beyond this module.
#[cfg(test)]
pub(crate) use migrations::migrate;
#[cfg(not(test))]
use migrations::migrate;
pub(crate) mod data_dir;
pub use data_dir::data_dir;
mod migration_backup;
mod migration_gate;
pub(crate) mod store_lock;
/// The database file's name inside the data folder.
/// The database file name inside the data folder: `<app slug>.sqlite`.
pub fn db_file() -> String {
    format!("{}.sqlite", crate::host::host().app_slug())
}
mod pragma_optimize;
pub(crate) use pragma_optimize::pragma_optimize;
mod util;
pub(crate) use util::query_map_collect;
// SQLCipher mail store; the schema is created up front so sync and compose have a stable shape.
mod outbox_contacts;
pub use outbox_contacts::*;
mod accounts_labels;
pub use accounts_labels::*;
mod threads;
pub use threads::*;
mod search;
pub use search::*;
mod job_tables;
mod pending_ops;
pub use pending_ops::*;
mod recipients;
// Local semantic search (C5): embedder + sqlite-vec storage + hybrid query +
// resumable index job. `pub` because commands/semantic.rs and the sync
// service call into it directly (mirrors `pub use` of the other db
// sub-modules above).
#[path = "db_sync_apply.rs"]
mod db_sync_apply;
pub mod semantic;
pub use db_sync_apply::*;
use rightkit_db::rusqlite::{params, Connection, OptionalExtension};
use rightkit_db::{CacheProfile, DbOptions, DurabilityProfile, ReadPool, ReadPoolOptions};
use rightkit_google_apis::gmail::{FetchedMessage, LabelInfo};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
const DELTA_FAILURE_ADVANCE_THRESHOLD: i64 = 3;
pub struct OpenResult {
    pub conn: Connection,
    pub path: String,
}
pub fn open_default() -> Result<OpenResult, String> {
    // The data folder comes from `data_dir::data_dir_checked`: a test-mode build
    // run against a bound test root uses that root's folder, a refused test selection is an error, and every
    // other build uses the real per-user folder and reads no environment.
    let (base, _) = data_dir::data_dir_checked()?;
    rightkit_fs::ensure_private_dir(&base).map_err(|e| format!("create data dir failed: {e}"))?;
    let db_path = base.join(db_file());
    open_at(db_path)
}
pub fn open_at(db_path: PathBuf) -> Result<OpenResult, String> {
    let key = crate::host::host()
        .get_or_create_db_key()
        .map_err(|e| format!("credential store: {e}"))?;
    // Try the primary key first. If it fails AND a key rotation was interrupted
    // (a pending key exists), try the pending key and promote it — this
    // recovers a DB that was re-encrypted before the credential update landed.
    let mut candidates = vec![rightkit_db::Zeroizing::new(key)];
    let pending = crate::host::host().read_pending_db_key();
    if let Some(pending) = &pending {
        candidates.push(rightkit_db::Zeroizing::new(pending.clone()));
    }
    // Cooperating lifetime lock: shared for as long as this process keeps connections, so
    // a replace in another process waits for (or refuses) a live store.
    let took_lock = store_lock::hold_shared(&db_path)?;
    let opened = open_candidates(&db_path, &candidates);
    if opened.is_err() && took_lock {
        store_lock::release();
    }
    let (conn, used) = opened?;
    if used > 0 {
        // Recovered with the pending key — make it primary, clear pending.
        if let Err(e) = crate::host::host().promote_pending_db_key(&candidates[used]) {
            drop(conn);
            if took_lock {
                store_lock::release();
            }
            return Err(format!("promote pending db key: {e}"));
        }
    }
    let finished = finish_open(conn, &db_path, &candidates[used]);
    if finished.is_err() && took_lock {
        store_lock::release();
    }
    finished
}

fn open_candidates(
    db_path: &Path,
    candidates: &[rightkit_db::Zeroizing<String>],
) -> Result<(Connection, usize), String> {
    db_options(db_path, &candidates[0])
        .open_writer_with_key_candidates(candidates)
        .map_err(|e| format!("open database: {e}"))
}
/// Post-key steps of `open_at`: foreign keys, gated migration, runtime
/// schema. No checkpoint runs here. `key` is the cipher key that opened `conn`
/// (the pre-migration backup is encrypted with it).
fn finish_open(conn: Connection, db_path: &Path, key: &str) -> Result<OpenResult, String> {
    // A refused folder check (test-job process) is treated as the real folder.
    let kind = data_dir::data_dir_checked().map_or(data_dir::DirKind::Default, |(_, k)| k);
    let policy = migration_gate::GatePolicy::for_this_build(kind);
    finish_open_with(conn, db_path, key, &policy)
}

/// [`finish_open`] with the channel and folder kind injected (tests).
fn finish_open_with(
    conn: Connection,
    db_path: &Path,
    key: &str,
    policy: &migration_gate::GatePolicy,
) -> Result<OpenResult, String> {
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .map_err(|e| format!("enable fk: {e}"))?;
    migration_gate::gate_and_migrate(&conn, db_path, key, policy)?;
    ensure_runtime_schema(&conn)?;
    let _ = rightkit_db::optimize_on_open(&conn);
    Ok(OpenResult {
        conn,
        path: db_path.to_string_lossy().to_string(),
    })
}
/// Connections per read pool: two general readers plus one so a search does
/// not queue behind list refreshes. The single place to change the size.
pub(crate) const READER_POOL_SIZE: usize = 3;

/// Page cache per pooled reader, KiB (32 MiB). The writer keeps 64 MiB
/// (`CacheProfile::LargeWorkingSet`); readers are halved so three of them stay
/// at ~96 MiB of cache in total.
pub(crate) const POOLED_READER_CACHE_KIB: u32 = 32 * 1024;

/// The shared open contract for the writer and the pool: SQLCipher key,
/// large-working-set cache, NORMAL durability, `analysis_limit = 400` and the
/// sqlite-vec registration through the connection-init hook. The planner
/// refresh is not run by the open itself: `finish_open_with` runs it after the
/// migration gate, and the checkpoint connection never runs it.
pub(crate) fn db_options(db_path: &Path, key: &str) -> DbOptions {
    // `sqlite3_auto_extension` only affects connections opened after it runs,
    // so register before the first open as well as from the init hook.
    semantic::vec_ext::ensure_registered();
    let mut opts = DbOptions::new(db_path)
        .without_optimize_on_open()
        .with_passphrase(key);
    opts.profile = CacheProfile::LargeWorkingSet;
    opts.durability = DurabilityProfile::Normal;
    opts.init = Some(std::sync::Arc::new(|conn| {
        semantic::vec_ext::ensure_registered();
        conn.execute_batch("PRAGMA analysis_limit = 400;")
    }));
    opts
}

/// Build the UI read pool: read-only readers with the pooled cache size. The
/// first connection opens now so an unavailable DB/key is reported here; the
/// rest open lazily. Returns None when the database/key isn't available
/// (callers fall back to the writer). The pool carries the key current at
/// creation; key rotation closes the pool and builds a new one.
pub fn open_read_pool(db_path: &str) -> Option<ReadPool> {
    let key = crate::host::host().get_or_create_db_key().ok()?;
    db_options(Path::new(db_path), &key)
        .open_read_pool_with(
            READER_POOL_SIZE,
            ReadPoolOptions::new()
                .with_read_only(true)
                .with_reader_cache_kib(POOLED_READER_CACHE_KIB),
        )
        .ok()
}

/// Open a connection, apply the SQLCipher key, and verify it decrypts the file
/// (or initializes a fresh one). Returns the keyed, verified writer connection.
pub(crate) fn open_keyed(db_path: &Path, key: &str) -> Result<Connection, String> {
    db_options(db_path, key)
        .open_writer()
        .map_err(|e| format!("open database: {e}"))
}

/// Truncate the WAL back into the main DB file. Called at sync-PASS boundaries
/// (after the writer lock/txn for the pass has been released) and at
/// quit/lock (NOT at open: see `open_at`), so the WAL can't grow unbounded: the app keeps a
/// long-lived reader connection whose snapshot can starve SQLite's automatic
/// checkpoint, and a multi-MB WAL is both slow and the condition under which a
/// reader once observed a transient SQLITE_CORRUPT mid-sync ("database disk
/// image is malformed" on search, 2026-05-25). TRUNCATE reclaims everything
/// past the oldest reader. Best-effort — never fails a sync. This can wait on
/// readers for up to the busy timeout, so callers are quiescent boundaries only.
///
/// NOTE (L4, 2026-07-04): this is intentionally NOT called after every small
/// write anymore — paying TRUNCATE inside a held writer lock on every batch
/// extended the critical section and was a contributor to the "Not
/// Responding" freeze. The WAL-bounded-after-every-sync-pass invariant is
/// unconditional; per-batch/per-small-write callers instead pay only a
/// size-gated PASSIVE checkpoint (`rightkit_db::checkpoint_if_wal_larger_than`).
pub fn checkpoint_wal(conn: &Connection) {
    let _ = rightkit_db::checkpoint_truncate(conn);
}
/// Rotate the SQLCipher database key in place via `PRAGMA rekey`, updating the
/// stored credential. Crash-safe: the new key is staged as the pending
/// credential before re-encryption, so an interruption is recovered on next open
/// (see `open_at`). On any failure the DB is left on its current key.
pub fn rekey(conn: &Connection) -> Result<(), String> {
    let new_key = crate::host::host().new_db_key()?;
    // 1. Stage the new key so an interrupted rekey is recoverable.
    let rotation = crate::host::host()
        .begin_key_rotation(&new_key)
        .map_err(|e| format!("stage pending key: {e}"))?;
    // 2. Re-encrypt every page with the new key (SQLCipher rekey is atomic)
    //    and verify the connection still reads.
    if let Err(e) = rightkit_db::rekey(conn, &new_key) {
        // Only a refused key proves the database was never touched. Any other
        // failure may follow a completed re-encryption (the verify read), so the
        // staged key stays for `open_at` to recover with.
        if matches!(e, rightkit_db::DbError::InvalidSqlcipherKey) {
            rotation.abort();
            return Err(format!("rekey failed (db unchanged): {e}"));
        }
        return Err(format!("rekey failed (pending key kept for recovery): {e}"));
    }
    // 3. Promote the new key to primary and drop the marker. On failure the marker stays,
    //    so the next open recovers with the pending key.
    rotation
        .commit()
        .map_err(|e| format!("store new key (pending key kept for recovery): {e}"))
}
/// True if `table` already has a column named `column`. Lets us make
/// `ALTER TABLE … ADD COLUMN` (which has no `IF NOT EXISTS`) idempotent. Table
/// and column are hardcoded literals at every call site — never user input.
pub fn count_tables(conn: &Connection) -> Result<i64, String> {
    conn.query_row(
        "
        SELECT count(*)
        FROM sqlite_master
        WHERE type IN ('table','view')
          AND name IN (
            'accounts',
            'labels',
            'threads',
            'messages',
            'message_bodies',
            'attachments',
            'sync_state',
            'aliases',
            'drafts',
            'outbox',
            'fts_messages',
            'ai_summaries',
            'settings',
            'audit_log',
            'thread_labels',
            'snoozes',
            'contacts_cache'
          )
        ",
        [],
        |r| r.get(0),
    )
    .map_err(|e| format!("count tables: {e}"))
}
pub fn count_accounts(conn: &Connection) -> Result<i64, String> {
    conn.query_row("SELECT count(*) FROM accounts", [], |r| r.get(0))
        .map_err(|e| format!("count accounts: {e}"))
}
/// Unix milliseconds now (`rightkit-search`'s `values::now_ms`).
pub(crate) use rightkit_search::values::now_ms;
/// Run `work` inside a write transaction that reserves the writer slot up front
/// (`rightkit_db::write_txn_unchecked`), rolling back on error.
fn in_txn(conn: &Connection, work: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    let tx = rightkit_db::write_txn_unchecked(conn).map_err(|e| format!("begin: {e}"))?;
    // An error returns before `commit`; dropping `tx` rolls the transaction back.
    work()?;
    tx.commit().map_err(|e| format!("commit: {e}"))
}
/// Upsert messages (+ their bodies) for one account. Caller must hold a txn.
fn upsert_messages(
    conn: &Connection,
    account_id: &str,
    msgs: &[FetchedMessage],
) -> Result<(), String> {
    for m in msgs {
        let labels = serde_json::to_string(&m.label_ids).unwrap_or_else(|_| "[]".into());
        conn.execute(
            "INSERT INTO messages
               (account_id, id, thread_id, from_addr, from_name, to_addrs, cc_addrs,
                reply_to, list_unsubscribe, subject, snippet, internal_date,
                label_ids, unread, has_attachments, auth_results,
                size_estimate, rfc822_message_id)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)
             ON CONFLICT(account_id, id) DO UPDATE SET
               to_addrs = excluded.to_addrs,
               cc_addrs = excluded.cc_addrs,
               reply_to = excluded.reply_to,
               list_unsubscribe = excluded.list_unsubscribe,
               snippet = excluded.snippet,
               label_ids = excluded.label_ids,
               unread = excluded.unread,
               has_attachments = CASE
                 WHEN excluded.has_attachments = 1 THEN 1
                 ELSE has_attachments
               END,
               auth_results = COALESCE(excluded.auth_results, auth_results),
               size_estimate = COALESCE(excluded.size_estimate, size_estimate),
               rfc822_message_id = COALESCE(excluded.rfc822_message_id, rfc822_message_id)",
            params![
                account_id,
                m.id,
                m.thread_id,
                m.from_addr,
                m.from_name,
                m.to_addrs,
                m.cc_addrs,
                m.reply_to,
                m.list_unsubscribe,
                m.subject,
                m.snippet,
                m.internal_date,
                labels,
                m.unread as i64,
                m.has_attachments as i64,
                m.auth_results,
                m.size_estimate,
                m.rfc822_message_id,
            ],
        )
        .map_err(|e| format!("insert message: {e}"))?;
        // L10: keep message_recipients in lock-step with every upsert (not
        // just the one-time migration backfill) — see db/recipients.rs.
        // FetchedMessage carries no bcc_addrs field (Gmail's metadata
        // fetch never surfaces Bcc for received/synced mail), so that slot
        // is always empty here; the messages.bcc_addrs column itself is
        // populated only by the send path, and this is additive metadata
        // derived from whatever the messages table already holds.
        let recipient_rows = recipients::recipient_rows_for(
            &m.from_addr,
            &m.from_name,
            &m.to_addrs,
            &m.cc_addrs,
            "",
        );
        recipients::replace_message_recipients(conn, account_id, &m.id, &recipient_rows)?;
        if let Some(text) = &m.body_plain {
            conn.execute(
                "INSERT OR REPLACE INTO message_bodies(account_id, message_id, mime_type, body)
                 VALUES(?1, ?2, 'text/plain', ?3)",
                params![account_id, m.id, text],
            )
            .map_err(|e| format!("insert body(text): {e}"))?;
        }
        if let Some(html) = &m.body_html {
            conn.execute(
                "INSERT OR REPLACE INTO message_bodies(account_id, message_id, mime_type, body)
                 VALUES(?1, ?2, 'text/html', ?3)",
                params![account_id, m.id, html],
            )
            .map_err(|e| format!("insert body(html): {e}"))?;
        }
        for a in &m.attachments {
            conn.execute(
                "INSERT OR REPLACE INTO attachments
                   (account_id, message_id, attachment_id, filename, mime_type, size)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    account_id,
                    m.id,
                    a.attachment_id,
                    a.filename,
                    a.mime_type,
                    a.size
                ],
            )
            .map_err(|e| format!("insert attachment: {e}"))?;
        }
    }
    Ok(())
}
/// Delete message rows (+ bodies) by id. Caller must hold a txn; rollups are
/// rebuilt afterward by rebuild_rollups.
fn delete_message_rows(conn: &Connection, account_id: &str, ids: &[String]) -> Result<(), String> {
    for id in ids {
        // Index maintenance by rowid; must run BEFORE the messages row is
        // deleted or the rowid can no longer be resolved (db/fts_index.rs).
        fts_index::remove_message(conn, account_id, id)?;
        // Same for the vector: a freed rowid may be reused by a later message.
        semantic::invalidate::remove_message_vector(conn, account_id, id)?;
        conn.execute(
            "DELETE FROM messages WHERE account_id = ?1 AND id = ?2",
            params![account_id, id],
        )
        .map_err(|e| format!("delete message: {e}"))?;
        conn.execute(
            "DELETE FROM attachments WHERE account_id = ?1 AND message_id = ?2",
            params![account_id, id],
        )
        .map_err(|e| format!("delete attachments: {e}"))?;
        conn.execute(
            "DELETE FROM message_bodies WHERE account_id = ?1 AND message_id = ?2",
            params![account_id, id],
        )
        .map_err(|e| format!("delete body: {e}"))?;
    }
    recipients::delete_message_recipients(conn, account_id, ids)
}
/// Ensure every connected account is on the local-indexing path. For each
/// account that does not yet have a `full_archive:<id>` flag set, arm
/// `download_all_active:<id> = "1"`. Idempotent: re-arming an active job or a
/// already-finished archive is a no-op. Called once on app startup so local
/// search becomes complete without a manual button click.
pub fn ensure_local_indexing(conn: &Connection) -> Result<(), String> {
    let accounts = crate::db::accounts_labels::list_accounts(conn)?;
    for acct in accounts {
        let id = acct.id;
        if get_setting(conn, &format!("full_archive:{id}"))?.as_deref() != Some("1") {
            set_setting(conn, &format!("download_all_active:{id}"), "1")?;
        }
    }
    Ok(())
}
/// Defense-in-depth on the SQLCipher key before it's interpolated into a
/// `PRAGMA key`/`PRAGMA rekey` statement. Our keys are always OS-CSPRNG base64
/// (standard alphabet + padding), so we can demand exactly that and reject
/// anything containing a quote or other unexpected byte outright — closing the
/// latent PRAGMA-injection path even if a future code path ever sourced a key
/// from elsewhere.
#[cfg(test)]
fn key_is_safe(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
}
// Schema SQL + version live in `db_schema` (T24 decomposition slice).
use crate::db_schema::{
    SCHEMA_V1, SCHEMA_V10, SCHEMA_V11, SCHEMA_V12, SCHEMA_V13, SCHEMA_V14, SCHEMA_V2, SCHEMA_V3,
    SCHEMA_V4, SCHEMA_V5, SCHEMA_V6, SCHEMA_V7, SCHEMA_V8, SCHEMA_V9, SCHEMA_VERSION,
};
#[cfg(test)]
mod test_conn;
#[cfg(test)]
pub use test_conn::test_conn;
#[cfg(test)]
mod tests;
