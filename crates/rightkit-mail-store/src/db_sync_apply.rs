#[path = "db_sync_apply/chunked.rs"]
mod chunked;
#[path = "db_sync_apply/health.rs"]
mod health;
#[cfg(test)]
#[path = "db_sync_apply/repair_tests.rs"]
mod repair_tests;
use super::*;
pub use chunked::*;
pub use health::*;
fn collect_thread_ids_for_messages(
    conn: &Connection,
    account_id: &str,
    ids: &[String],
) -> Result<HashSet<String>, String> {
    let mut out = HashSet::new();
    for id in ids {
        let thread_id: Option<String> = conn
            .query_row(
                "SELECT thread_id FROM messages WHERE account_id = ?1 AND id = ?2",
                params![account_id, id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| format!("collect thread id: {e}"))?;
        if let Some(thread_id) = thread_id {
            out.insert(thread_id);
        }
    }
    Ok(out)
}
fn rebuild_threads_for_ids(
    conn: &Connection,
    account_id: &str,
    thread_ids: &HashSet<String>,
) -> Result<(), String> {
    for thread_id in thread_ids {
        conn.execute(
            "DELETE FROM threads WHERE account_id = ?1 AND id = ?2",
            params![account_id, thread_id],
        )
        .map_err(|e| format!("clear thread: {e}"))?;
        conn.execute(
            "INSERT INTO threads
               (account_id, id, subject, snippet, unread, starred,
                last_message_at, message_count)
             SELECT m.account_id, m.thread_id,
               (SELECT subject FROM messages
                  WHERE account_id = m.account_id AND thread_id = m.thread_id
                  ORDER BY internal_date DESC LIMIT 1),
               (SELECT snippet FROM messages
                  WHERE account_id = m.account_id AND thread_id = m.thread_id
                  ORDER BY internal_date DESC LIMIT 1),
               MAX(m.unread), 0, MAX(m.internal_date), COUNT(*)
             FROM messages m
             WHERE m.account_id = ?1 AND m.thread_id = ?2
             GROUP BY m.thread_id",
            params![account_id, thread_id],
        )
        .map_err(|e| format!("insert thread: {e}"))?;
        conn.execute(
            "DELETE FROM thread_labels WHERE account_id = ?1 AND thread_id = ?2",
            params![account_id, thread_id],
        )
        .map_err(|e| format!("clear thread labels: {e}"))?;
        conn.execute(
            "INSERT OR IGNORE INTO thread_labels(account_id, thread_id, label)
             SELECT m.account_id, m.thread_id, je.value
             FROM messages m, json_each(m.label_ids) je
             WHERE m.account_id = ?1 AND m.thread_id = ?2 AND m.label_ids IS NOT NULL",
            params![account_id, thread_id],
        )
        .map_err(|e| format!("insert thread labels: {e}"))?;
        // Starred + important flags are derived from thread_labels, so they
        // must be re-evaluated after the label rebuild (L15: important was
        // previously missing here — `apply_messages` used the full
        // `rebuild_rollups` which updated it, so the incremental path needs to
        // match or the `important` column drifts stale).
        conn.execute(
            "UPDATE threads SET starred = EXISTS(
               SELECT 1 FROM thread_labels tl
               WHERE tl.account_id = threads.account_id AND tl.thread_id = threads.id
                 AND tl.label = 'STARRED')
             WHERE account_id = ?1 AND id = ?2",
            params![account_id, thread_id],
        )
        .map_err(|e| format!("update starred: {e}"))?;
        conn.execute(
            "UPDATE threads SET important = EXISTS(
               SELECT 1 FROM thread_labels tl
               WHERE tl.account_id = threads.account_id AND tl.thread_id = threads.id
                 AND tl.label = 'IMPORTANT')
             WHERE account_id = ?1 AND id = ?2",
            params![account_id, thread_id],
        )
        .map_err(|e| format!("update important: {e}"))?;
    }
    Ok(())
}
// INVARIANT (schema v9, kept by v15): every fts_messages row is keyed `rowid = messages.rowid`
// for its message. This is what makes the per-message delete below an O(log n)
// rowid lookup. FTS5 cannot index WHERE-equality on its own columns (message_id/
// account_id are UNINDEXED and even indexed columns only accelerate MATCH), so
// the old `DELETE ... WHERE account_id=? AND message_id=?` form was a full scan
// of every email body per message — ~60 s per 25-message sync batch on a 2 GB
// mailbox, all inside the global connection lock (the 2026-07-03 "app hangs
// during download-all" bug). Safe because `upsert_messages` uses ON CONFLICT DO
// UPDATE (rowid preserved) and nothing in the codebase runs VACUUM (which could
// renumber messages.rowid — if VACUUM is ever introduced, rebuild fts_messages
// with the SCHEMA_V9 SQL afterwards).
fn rebuild_fts_for_message_ids(
    conn: &Connection,
    account_id: &str,
    message_ids: &[String],
) -> Result<(), String> {
    use crate::db::semantic::invalidate;
    let before = invalidate::text_fingerprints(conn, account_id, message_ids)?;
    crate::db::fts_index::index_messages(conn, account_id, message_ids)?;
    // Only a changed subject/body (or a new message) drops its vector; a
    // re-fetch that changed labels, read or star state keeps it.
    invalidate::invalidate_changed(conn, account_id, message_ids, &before)
}
/// Rebuild threads, thread_labels, and FTS for one account from the CURRENT
/// messages table (not a passed slice), so it is correct after an incremental
/// delta as well as a full sync. Caller must hold a txn.
///
/// Segment-level timings are emitted to `diagnostics.log` so we can prove
/// where time goes on the 200k+ mailboxes that motivated the move to
/// incremental apply (L15). Format: `ROLLUP seg=<label> account=<id> ms=<n>`.
#[cfg(test)]
pub(crate) fn rebuild_rollups(conn: &Connection, account_id: &str) -> Result<(), String> {
    let total_started = std::time::Instant::now();

    macro_rules! seg {
        ($label:expr, $started:expr) => {{
            let ms = $started.elapsed().as_millis();
            crate::host::host().perf_diag(&format!(
                "ROLLUP seg={} account={} ms={}",
                $label, account_id, ms
            ));
        }};
    }

    let t = std::time::Instant::now();
    conn.execute("DELETE FROM threads WHERE account_id = ?1", [account_id])
        .map_err(|e| format!("clear threads: {e}"))?;
    seg!("clear_threads", t);

    let t = std::time::Instant::now();
    conn.execute(
        "INSERT INTO threads
           (account_id, id, subject, snippet, unread, starred,
            last_message_at, message_count)
         SELECT m.account_id, m.thread_id,
           (SELECT subject FROM messages
              WHERE account_id = m.account_id AND thread_id = m.thread_id
              ORDER BY internal_date DESC LIMIT 1),
           (SELECT snippet FROM messages
              WHERE account_id = m.account_id AND thread_id = m.thread_id
              ORDER BY internal_date DESC LIMIT 1),
           MAX(m.unread), 0, MAX(m.internal_date), COUNT(*)
         FROM messages m
         WHERE m.account_id = ?1
         GROUP BY m.thread_id",
        [account_id],
    )
    .map_err(|e| format!("rebuild threads: {e}"))?;
    seg!("rebuild_threads", t);

    let t = std::time::Instant::now();
    conn.execute(
        "DELETE FROM thread_labels WHERE account_id = ?1",
        [account_id],
    )
    .map_err(|e| format!("clear thread_labels: {e}"))?;
    seg!("clear_thread_labels", t);

    let t = std::time::Instant::now();
    conn.execute(
        "INSERT OR IGNORE INTO thread_labels(account_id, thread_id, label)
         SELECT m.account_id, m.thread_id, je.value
         FROM messages m, json_each(m.label_ids) je
         WHERE m.account_id = ?1 AND m.label_ids IS NOT NULL",
        [account_id],
    )
    .map_err(|e| format!("rebuild thread_labels: {e}"))?;
    seg!("rebuild_thread_labels", t);

    let t = std::time::Instant::now();
    conn.execute(
        "UPDATE threads SET starred = EXISTS(
           SELECT 1 FROM thread_labels tl
           WHERE tl.account_id = threads.account_id AND tl.thread_id = threads.id
             AND tl.label = 'STARRED')
         WHERE account_id = ?1",
        [account_id],
    )
    .map_err(|e| format!("rebuild starred: {e}"))?;
    seg!("rebuild_starred", t);

    let t = std::time::Instant::now();
    conn.execute(
        "UPDATE threads SET important = EXISTS(
           SELECT 1 FROM thread_labels tl
           WHERE tl.account_id = threads.account_id AND tl.thread_id = threads.id
             AND tl.label = 'IMPORTANT')
         WHERE account_id = ?1",
        [account_id],
    )
    .map_err(|e| format!("rebuild important: {e}"))?;
    seg!("rebuild_important", t);

    let t = std::time::Instant::now();
    crate::db::fts_index::remove_account(conn, account_id)?;
    crate::db::semantic::invalidate::clear_account(conn, account_id)?;
    seg!("clear_fts", t);

    let t = std::time::Instant::now();
    crate::db::fts_index::index_account(conn, account_id)?;
    seg!("rebuild_fts", t);

    seg!("total", total_started);
    Ok(())
}
/// Rebuild all rollups (threads, thread_labels, FTS) for one account from the
/// current messages table. Use after a download-all so previously snippet-only
/// rows pick up their full bodies.
#[cfg(test)]
pub fn rebuild_all_rollups(conn: &Connection, account_id: &str) -> Result<(), String> {
    in_txn(conn, || rebuild_rollups(conn, account_id))
}
/// Local archive progress: (total messages mirrored, messages that have a body).
pub fn archive_counts(conn: &Connection, account_id: &str) -> Result<(i64, i64), String> {
    let total: i64 = conn
        .query_row(
            "SELECT count(*) FROM messages WHERE account_id = ?1",
            [account_id],
            |r| r.get(0),
        )
        .map_err(|e| format!("count messages: {e}"))?;
    let with_body: i64 = conn
        .query_row(
            "SELECT count(DISTINCT message_id) FROM message_bodies WHERE account_id = ?1",
            [account_id],
            |r| r.get(0),
        )
        .map_err(|e| format!("count bodies: {e}"))?;
    Ok((total, with_body))
}
pub fn apply_messages(
    conn: &Connection,
    account_id: &str,
    msgs: &[FetchedMessage],
) -> Result<usize, String> {
    if msgs.is_empty() {
        return Ok(0);
    }
    // L15: incremental rebuild. The old `rebuild_rollups` walked EVERY thread
    // and EVERY FTS row for the account — O(mailbox) inside the writer mutex,
    // which is the path the 2026-07-03 "Not Responding" audit flagged as
    // secondary suspect #1. Sync delivers 25-message batches; we only need to
    // touch the threads those messages belong to. This mirrors
    // `apply_search_messages` and `apply_delta`, both of which already use
    // the per-thread / per-message incremental helpers.
    //
    // Legitimate full-rebuild callers (post download-all body refresh, FTS
    // repair) still go through `rebuild_all_rollups` → `rebuild_rollups`,
    // which is now instrumented with per-segment timings.
    in_txn(conn, || {
        upsert_messages(conn, account_id, msgs)?;
        let mut thread_ids: HashSet<String> = HashSet::with_capacity(msgs.len());
        let mut message_ids: Vec<String> = Vec::with_capacity(msgs.len());
        for m in msgs {
            thread_ids.insert(m.thread_id.clone());
            message_ids.push(m.id.clone());
        }
        rebuild_threads_for_ids(conn, account_id, &thread_ids)?;
        rebuild_fts_for_message_ids(conn, account_id, &message_ids)
    })?;
    Ok(msgs.len())
}
pub fn apply_search_messages(
    conn: &Connection,
    account_id: &str,
    msgs: &[FetchedMessage],
) -> Result<usize, String> {
    if msgs.is_empty() {
        return Ok(0);
    }
    in_txn(conn, || {
        upsert_messages(conn, account_id, msgs)?;
        let mut thread_ids: HashSet<String> = HashSet::new();
        let mut message_ids: Vec<String> = Vec::new();
        for m in msgs {
            thread_ids.insert(m.thread_id.clone());
            message_ids.push(m.id.clone());
        }
        rebuild_threads_for_ids(conn, account_id, &thread_ids)?;
        rebuild_fts_for_message_ids(conn, account_id, &message_ids)?;
        Ok(())
    })?;
    Ok(msgs.len())
}
pub fn apply_delta(
    conn: &Connection,
    account_id: &str,
    changed: &[FetchedMessage],
    deleted: &[String],
) -> Result<(usize, usize), String> {
    in_txn(conn, || {
        let mut thread_ids = collect_thread_ids_for_messages(conn, account_id, deleted)?;
        let changed_message_ids: Vec<String> = changed.iter().map(|m| m.id.clone()).collect();
        for m in changed {
            thread_ids.insert(m.thread_id.clone());
        }
        delete_message_rows(conn, account_id, deleted)?;
        upsert_messages(conn, account_id, changed)?;
        rebuild_threads_for_ids(conn, account_id, &thread_ids)?;
        rebuild_fts_for_message_ids(conn, account_id, &changed_message_ids)
    })?;
    Ok((changed.len(), deleted.len()))
}
fn delta_failures_key(account_id: &str) -> String {
    format!("delta_failed_ids:{account_id}")
}
fn read_delta_failures(
    conn: &Connection,
    account_id: &str,
) -> Result<HashMap<String, i64>, String> {
    let raw = get_setting(conn, &delta_failures_key(account_id))?.unwrap_or_default();
    if raw.trim().is_empty() {
        return Ok(HashMap::new());
    }
    Ok(serde_json::from_str(&raw).unwrap_or_default())
}
/// Maximum number of message_ids kept in the delta_failures blob (a2-09).
const DELTA_FAILURE_MAX_ENTRIES: usize = 500;
fn write_delta_failures(
    conn: &Connection,
    account_id: &str,
    failures: &HashMap<String, i64>,
) -> Result<(), String> {
    let pruned: HashMap<&String, &i64> = if failures.len() > DELTA_FAILURE_MAX_ENTRIES {
        let mut sorted: Vec<(&String, &i64)> = failures.iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(a.1));
        sorted.into_iter().take(DELTA_FAILURE_MAX_ENTRIES).collect()
    } else {
        failures.iter().collect()
    };
    let raw = serde_json::to_string(&pruned).unwrap_or_else(|_| "{}".to_string());
    set_setting(conn, &delta_failures_key(account_id), &raw)
}
fn update_delta_failure_counts(
    conn: &Connection,
    account_id: &str,
    successful_ids: &[String],
    failed_ids: &[String],
) -> Result<bool, String> {
    let mut failures = read_delta_failures(conn, account_id)?;
    for id in successful_ids {
        failures.remove(id);
    }
    if failed_ids.is_empty() {
        write_delta_failures(conn, account_id, &failures)?;
        return Ok(false);
    }
    for id in failed_ids {
        *failures.entry(id.clone()).or_insert(0) += 1;
    }
    let exhausted = failed_ids
        .iter()
        .all(|id| failures.get(id).copied().unwrap_or_default() >= DELTA_FAILURE_ADVANCE_THRESHOLD);
    if exhausted {
        for id in failed_ids {
            failures.remove(id);
        }
    }
    write_delta_failures(conn, account_id, &failures)?;
    Ok(exhausted)
}
pub fn apply_sync_work(
    conn: &Connection,
    account_id: &str,
    work: rightkit_google_apis::gmail::SyncWork,
) -> Result<usize, String> {
    match work {
        rightkit_google_apis::gmail::SyncWork::Full(r) => {
            let _ = apply_labels(conn, account_id, &r.labels);
            let n = apply_messages(conn, account_id, &r.outcome.messages)?;
            if let Some(h) = &r.history_id {
                let _ = set_history_id(conn, account_id, h);
            }
            audit(
                conn,
                "sync",
                Some(account_id),
                &format!("full sync: {n} messages{}", r.outcome.skip_summary()),
            );
            Ok(n)
        }
        rightkit_google_apis::gmail::SyncWork::Delta(d) => {
            let successful_ids: Vec<String> = d.changed.iter().map(|m| m.id.clone()).collect();
            let exhausted_failures =
                update_delta_failure_counts(conn, account_id, &successful_ids, &d.failed_ids)?;
            if !d.changed.is_empty() || !d.deleted.is_empty() {
                let (changed, deleted) = apply_delta(conn, account_id, &d.changed, &d.deleted)?;
                let note = if d.failed > 0 {
                    if exhausted_failures {
                        format!(", {} unreconciled skipped after retries", d.failed)
                    } else {
                        format!(", {} unreconciled (cursor held)", d.failed)
                    }
                } else {
                    String::new()
                };
                audit(
                    conn,
                    "sync",
                    Some(account_id),
                    &format!("delta sync: +{changed} -{deleted}{note}"),
                );
            } else if d.failed > 0 {
                let note = if exhausted_failures {
                    format!("{} unreconciled skipped after retries", d.failed)
                } else {
                    format!("{} unreconciled (cursor held)", d.failed)
                };
                audit(
                    conn,
                    "sync",
                    Some(account_id),
                    &format!("delta sync: {note}"),
                );
            }
            let cursor = d.new_history_id.as_ref().or_else(|| {
                exhausted_failures
                    .then_some(())
                    .and(d.latest_history_id.as_ref())
            });
            if let Some(h) = cursor {
                let _ = set_history_id(conn, account_id, h);
            }
            Ok(d.changed.len())
        }
    }
}
pub fn get_history_id(conn: &Connection, account_id: &str) -> Result<Option<String>, String> {
    match conn.query_row(
        "SELECT last_history_id FROM sync_state WHERE account_id = ?1",
        [account_id],
        |r| r.get::<_, Option<String>>(0),
    ) {
        Ok(v) => Ok(v),
        Err(rightkit_db::rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(format!("get history_id: {e}")),
    }
}
