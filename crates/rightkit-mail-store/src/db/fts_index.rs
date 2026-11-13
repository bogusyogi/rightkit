//! Full-text index maintenance (schema v15).
//!
//! `message_search` holds the extracted plain text exactly once (rowid =
//! `messages.rowid`); `fts_messages` is an external-content FTS5 table over it
//! (`prefix='2 3'`), so there is no second copy of the body and `snippet()` /
//! `highlight()` keep working. Triggers on `message_search` keep the index in
//! step, so every writer only touches `message_search`.
//!
//! Upgrade: a populated mailbox keeps its OLD `fts_messages` serving searches
//! while `fts_messages_next` is filled by [`rebuild_step`] in rowid order
//! (cursor in `fts_rebuild_state`, one short transaction per batch, so an
//! interrupted run resumes at the cursor). Writers during that window feed
//! only `message_search` and so the new index (via its triggers); the old
//! index is read-only and stale until the last step swaps the new table in.

use super::fts_text::html_to_text;
use rightkit_db::rusqlite::{params, Connection, OptionalExtension};

const NEXT: &str = "fts_messages_next";
/// The replaced index, emptied in small steps after the swap.
const OLD: &str = "fts_messages_old";
/// `fts_rebuild_state.last_rowid` once the swap is done and only the old
/// index remains to be emptied and dropped.
const CLEANUP: i64 = -1;
/// Rows removed from one of the old index's tables per cleanup step.
const CLEANUP_ROWS: i64 = 400;
/// Rows re-indexed per rebuild transaction. Each batch holds the writer:
/// 500 rows held it about 0.7 s on a 164k-message mailbox and other
/// commands were seen waiting up to 1.3 s; 50 rows is about 70 ms.
pub const REBUILD_BATCH: usize = 50;

const BASE_SQL: &str = "
CREATE TABLE IF NOT EXISTS message_search (
  -- Explicit key: a plain rowid may be renumbered by VACUUM, which would
  -- detach every row from the index and from messages.rowid.
  id         INTEGER PRIMARY KEY,
  message_id TEXT NOT NULL DEFAULT '',
  account_id TEXT NOT NULL DEFAULT '',
  subject    TEXT NOT NULL DEFAULT '',
  from_text  TEXT NOT NULL DEFAULT '',
  body       TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS fts_rebuild_state (
  id         INTEGER PRIMARY KEY CHECK (id = 1),
  last_rowid INTEGER NOT NULL,
  indexed    INTEGER NOT NULL
);";

fn create_fts_sql(name: &str) -> String {
    format!(
        "CREATE VIRTUAL TABLE {name} USING fts5(
           message_id UNINDEXED, account_id UNINDEXED, subject, from_text, body,
           content='message_search', content_rowid='id',
           prefix='2 3', tokenize='porter unicode61');"
    )
}

const TRIGGER_NAMES: [&str; 3] = [
    "message_search_ai",
    "message_search_ad",
    "message_search_au",
];

fn create_triggers_sql(fts: &str) -> String {
    let cols = "message_id, account_id, subject, from_text, body";
    let new_vals = "new.message_id, new.account_id, new.subject, new.from_text, new.body";
    let old_vals = "old.message_id, old.account_id, old.subject, old.from_text, old.body";
    let del =
        format!("INSERT INTO {fts}({fts}, rowid, {cols}) VALUES('delete', old.rowid, {old_vals});");
    let ins = format!("INSERT INTO {fts}(rowid, {cols}) VALUES(new.rowid, {new_vals});");
    format!(
        "CREATE TRIGGER IF NOT EXISTS message_search_ai AFTER INSERT ON message_search BEGIN {ins} END;
         CREATE TRIGGER IF NOT EXISTS message_search_ad AFTER DELETE ON message_search BEGIN {del} END;
         CREATE TRIGGER IF NOT EXISTS message_search_au AFTER UPDATE ON message_search BEGIN {del} {ins} END;"
    )
}

fn drop_triggers_sql() -> String {
    TRIGGER_NAMES
        .iter()
        .map(|t| format!("DROP TRIGGER IF EXISTS {t};"))
        .collect()
}

/// v15 DDL. Empty mailbox: build the final shape now. Populated mailbox: leave
/// the old index in service and register the background rebuild (driven by [`rebuild_step`]).
pub fn migrate_v15(conn: &Connection) -> Result<(), String> {
    let e = |what: &'static str| move |err: rightkit_db::rusqlite::Error| format!("{what}: {err}");
    conn.execute_batch(BASE_SQL)
        .map_err(e("create message_search"))?;
    // An external-content serving table must be emptied together with its
    // content table, or its index would describe rows that no longer exist
    // (a later delete would then read the wrong text and corrupt it).
    if serving_is_external(conn) && table_exists(conn, "fts_messages") {
        conn.execute(
            "INSERT INTO fts_messages(fts_messages) VALUES('delete-all')",
            [],
        )
        .map_err(e("empty serving index"))?;
    }
    conn.execute_batch(&format!(
        "{} DROP TABLE IF EXISTS {NEXT}; DROP TABLE IF EXISTS {OLD}; DELETE FROM fts_rebuild_state; DELETE FROM message_search;",
        drop_triggers_sql()
    ))
    .map_err(e("reset"))?;
    let has_messages: bool = conn
        .query_row("SELECT EXISTS(SELECT 1 FROM messages)", [], |r| r.get(0))
        .map_err(e("count messages"))?;
    if has_messages {
        conn.execute_batch(&format!(
            "{} {}",
            create_fts_sql(NEXT),
            create_triggers_sql(NEXT)
        ))
        .map_err(e("create next index"))?;
        conn.execute("INSERT INTO fts_rebuild_state VALUES (1, 0, 0)", [])
            .map_err(e("register rebuild"))?;
    } else {
        conn.execute_batch(&format!(
            "DROP TABLE IF EXISTS fts_messages; {} {}",
            create_fts_sql("fts_messages"),
            create_triggers_sql("fts_messages")
        ))
        .map_err(e("create final index"))?;
    }
    Ok(())
}

fn table_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |_| Ok(()),
    )
    .optional()
    .map(|r| r.is_some())
    .unwrap_or(false)
}

/// True when the serving `fts_messages` is the external-content table over
/// `message_search` (or absent / unreadable: then nothing may be written to
/// it). Read from the stored CREATE statement, not from rebuild state.
pub(crate) fn serving_is_external(conn: &Connection) -> bool {
    let sql: Result<Option<Option<String>>, _> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'fts_messages'",
            [],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional();
    match sql {
        Ok(Some(Some(sql))) => {
            let flat: String = sql
                .chars()
                .filter(|c| !c.is_whitespace() && *c != '\'' && *c != '"')
                .collect::<String>()
                .to_ascii_lowercase();
            flat.contains("content=message_search")
        }
        Ok(Some(None)) | Ok(None) | Err(_) => true,
    }
}

/// Outcome of one [`rebuild_step`].
#[derive(Debug, PartialEq, Eq)]
pub enum RebuildStep {
    /// Nothing to do (no rebuild registered).
    Idle,
    /// A batch was indexed; call again. Carries the running total.
    Progress(i64),
    /// The new index was complete and has replaced the old one.
    Swapped,
}

/// Index one batch of messages into the new index, or swap it in when the
/// scan is complete. Own transaction: the caller must not hold one.
pub fn rebuild_step(conn: &Connection, batch: usize) -> Result<RebuildStep, String> {
    let cursor: Option<i64> = conn
        .query_row(
            "SELECT last_rowid FROM fts_rebuild_state WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("read rebuild state: {e}"))?;
    match cursor {
        None => return Ok(RebuildStep::Idle),
        Some(CLEANUP) => return cleanup_step(conn),
        Some(_) => {}
    }
    let tx = rightkit_db::write_txn_unchecked(conn)
        .map_err(|e| format!("begin fts rebuild batch: {e}"))?;
    // A failed batch drops `tx`, which rolls it back.
    let step = rebuild_batch(&tx, batch)?;
    tx.commit()
        .map_err(|e| format!("commit fts rebuild batch: {e}"))?;
    Ok(step)
}

fn rebuild_batch(conn: &Connection, batch: usize) -> Result<RebuildStep, String> {
    let (cursor, done): (i64, i64) = conn
        .query_row(
            "SELECT last_rowid, indexed FROM fts_rebuild_state WHERE id = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| format!("read rebuild cursor: {e}"))?;
    let rows: Vec<(i64, String, String)> = {
        let mut st = conn
            .prepare(
                "SELECT rowid, account_id, id FROM messages WHERE rowid > ?1 ORDER BY rowid LIMIT ?2",
            )
            .map_err(|e| format!("select rebuild batch: {e}"))?;
        let mapped = st
            .query_map(params![cursor, batch as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .map_err(|e| format!("select rebuild batch: {e}"))?;
        mapped
            .collect::<Result<_, _>>()
            .map_err(|e| format!("read rebuild batch: {e}"))?
    };
    let Some(last) = rows.last().map(|r| r.0) else {
        swap_in(conn)?;
        return Ok(RebuildStep::Progress(done));
    };
    for (_, account_id, id) in &rows {
        upsert_search_row(conn, account_id, id)?;
    }
    let total = done + rows.len() as i64;
    // Compare-and-set on the cursor this batch started from: a stale or
    // concurrent worker that lost the race changes nothing (the error rolls
    // the whole batch back, index rows included).
    let advanced = conn
        .execute(
            "UPDATE fts_rebuild_state SET last_rowid = ?1, indexed = ?2
             WHERE id = 1 AND last_rowid = ?3",
            params![last, total, cursor],
        )
        .map_err(|e| format!("advance rebuild cursor: {e}"))?;
    if advanced != 1 {
        return Err("fts rebuild cursor moved under this batch (stale worker)".to_string());
    }
    Ok(RebuildStep::Progress(total))
}

/// Put the new index in service. Renames only, so it is quick: the old
/// index is emptied afterwards by [`cleanup_step`], a little at a time.
/// Dropping it here freed several GB of pages in one transaction and held
/// the writer for about two minutes on a 164k-message mailbox.
fn swap_in(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(&format!(
        "{} DROP TABLE IF EXISTS {OLD};
         ALTER TABLE fts_messages RENAME TO {OLD};
         ALTER TABLE {NEXT} RENAME TO fts_messages; {}
         UPDATE fts_rebuild_state SET last_rowid = {CLEANUP} WHERE id = 1;",
        drop_triggers_sql(),
        create_triggers_sql("fts_messages")
    ))
    .map_err(|e| format!("swap in new index: {e}"))
}

/// Empty the replaced index in small transactions, then drop it.
fn cleanup_step(conn: &Connection) -> Result<RebuildStep, String> {
    for shadow in ["content", "data", "docsize"] {
        let table = format!("{OLD}_{shadow}");
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                [&table],
                |r| r.get(0),
            )
            .map_err(|e| format!("find {table}: {e}"))?;
        if !exists {
            continue;
        }
        let removed = conn
            .execute(
                &format!(
                    "DELETE FROM {table} WHERE id IN (SELECT id FROM {table} LIMIT {CLEANUP_ROWS})"
                ),
                [],
            )
            .map_err(|e| format!("empty {table}: {e}"))?;
        if removed > 0 {
            return Ok(RebuildStep::Progress(0));
        }
    }
    conn.execute_batch(&format!(
        "DROP TABLE IF EXISTS {OLD}; DELETE FROM fts_rebuild_state;"
    ))
    .map_err(|e| format!("drop old index: {e}"))?;
    Ok(RebuildStep::Swapped)
}

// ---------------------------------------------------------------------------
// Per-message maintenance. Callers hold their own transaction.
// ---------------------------------------------------------------------------

/// Re-index the given messages (insert or replace).
pub(crate) fn index_messages(
    conn: &Connection,
    account_id: &str,
    message_ids: &[String],
) -> Result<(), String> {
    for id in message_ids {
        upsert_search_row(conn, account_id, id)?;
    }
    Ok(())
}

/// Remove one message from the index. Run BEFORE its `messages` row is deleted.
pub(crate) fn remove_message(
    conn: &Connection,
    account_id: &str,
    message_id: &str,
) -> Result<(), String> {
    let rowid: Option<i64> = conn
        .query_row(
            "SELECT rowid FROM messages WHERE account_id = ?1 AND id = ?2",
            params![account_id, message_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("find fts row: {e}"))?;
    let Some(rowid) = rowid else { return Ok(()) };
    conn.execute("DELETE FROM message_search WHERE rowid = ?1", [rowid])
        .map_err(|e| format!("delete fts row: {e}"))?;
    Ok(())
}

/// Remove every message of an account from the index. Run BEFORE the
/// account's `messages` rows go away.
pub(crate) fn remove_account(conn: &Connection, account_id: &str) -> Result<(), String> {
    let scope = "rowid IN (SELECT rowid FROM messages WHERE account_id = ?1)";
    conn.execute(
        &format!("DELETE FROM message_search WHERE {scope}"),
        [account_id],
    )
    .map_err(|e| format!("clear fts: {e}"))?;
    Ok(())
}

/// Write the extracted text for one message (a message that no longer exists
/// is skipped).
fn upsert_search_row(conn: &Connection, account_id: &str, message_id: &str) -> Result<(), String> {
    let head: Option<(i64, String, String, String)> = conn
        .prepare(
            "SELECT rowid, COALESCE(subject, ''),
                    COALESCE(from_name, '') || ' ' || COALESCE(from_addr, ''),
                    COALESCE(snippet, '')
             FROM messages WHERE account_id = ?1 AND id = ?2",
        )
        .and_then(|mut st| {
            st.query_row(params![account_id, message_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .optional()
        })
        .map_err(|e| format!("read message for fts: {e}"))?;
    let Some((rowid, subject, from_text, snippet)) = head else {
        return Ok(());
    };
    let body = body_text(conn, account_id, message_id, snippet)?;
    conn.prepare(
        "INSERT INTO message_search(rowid, message_id, account_id, subject, from_text, body)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(rowid) DO UPDATE SET message_id = excluded.message_id,
           account_id = excluded.account_id, subject = excluded.subject,
           from_text = excluded.from_text, body = excluded.body",
    )
    .and_then(|mut st| {
        st.execute(params![
            rowid, message_id, account_id, subject, from_text, body
        ])
    })
    .map_err(|e| format!("write fts row: {e}"))?;
    Ok(())
}

/// Plain part if present, else the HTML part reduced to its visible text,
/// else the snippet.
fn body_text(
    conn: &Connection,
    account_id: &str,
    message_id: &str,
    snippet: String,
) -> Result<String, String> {
    let part = |mime: &str| -> Result<Option<String>, String> {
        conn.prepare(
            "SELECT body FROM message_bodies
             WHERE account_id = ?1 AND message_id = ?2 AND mime_type = ?3",
        )
        .and_then(|mut st| {
            st.query_row(params![account_id, message_id, mime], |r| {
                r.get::<_, Option<String>>(0)
            })
            .optional()
        })
        .map(Option::flatten)
        .map_err(|e| format!("read body for fts: {e}"))
    };
    if let Some(plain) = part("text/plain")?.filter(|b| !b.trim().is_empty()) {
        return Ok(plain);
    }
    if let Some(html) = part("text/html")? {
        let text = html_to_text(&html);
        if !text.is_empty() {
            return Ok(text);
        }
    }
    Ok(snippet)
}
