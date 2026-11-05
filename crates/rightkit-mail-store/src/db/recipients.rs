// L10: normalized recipient metadata (message_recipients) — kills the
// leading-wildcard `LIKE '%term%'` scan class over the raw to_addrs/cc_addrs/
// bcc_addrs header-string columns. See docs/LOCAL_SEARCH_ARCHITECTURE.md
// Phase 2 ("Normalize Search Metadata") — this module is that phase's ADD-only
// slice: message_recipients is derived from the existing header-string
// columns/FetchedMessage fields, never the only copy of the data.
use super::*;

/// One normalized recipient row waiting to be written. `addr_norm` is always
/// lowercased + trimmed; `name_norm` is lowercased + trimmed (may be empty).
pub(crate) struct RecipientRow {
    pub(crate) kind: &'static str, // "to" | "cc" | "bcc" | "from"
    pub(crate) addr_norm: String,
    pub(crate) name_norm: String,
}

/// Split a raw RFC822-ish header value ("Alice <a@x.com>, Bob <b@x.com>" or a
/// bare "a@x.com, b@x.com", or — for older test fixtures / any future caller
/// that stuffs one in directly — a JSON array of strings) into normalized
/// (name, addr) pairs. Best-effort: malformed fragments are skipped rather
/// than failing the whole parse, since this only ever feeds a derived,
/// rebuildable search index (never a source of truth for what was actually
/// sent).
pub(crate) fn parse_address_list(raw: &str) -> Vec<(String, String)> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    // A handful of call sites (tests, and any future importer) hand this
    // function a JSON array of address strings instead of a comma-joined
    // header value. Detect and unwrap that shape first.
    if raw.starts_with('[') {
        if let Ok(items) = serde_json::from_str::<Vec<String>>(raw) {
            return items.iter().flat_map(|s| parse_address_list(s)).collect();
        }
    }
    split_addr_list(raw)
        .into_iter()
        .filter_map(|part| parse_one_address(part.trim()))
        .collect()
}

/// Split on top-level commas only — a display name may itself contain a
/// comma inside a quoted string ("Doe, Jane" <jane@x.com>), so a naive
/// `split(',')` would cut it in half. Track quote/angle-bracket depth.
fn split_addr_list(raw: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut in_quotes = false;
    let mut angle_depth = 0i32;
    for (i, c) in raw.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            '<' if !in_quotes => angle_depth += 1,
            '>' if !in_quotes => angle_depth -= 1,
            ',' if !in_quotes && angle_depth <= 0 => {
                out.push(&raw[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&raw[start..]);
    out
}

/// Parse one "Name <addr@x.com>" or bare "addr@x.com" fragment into
/// normalized (name_norm, addr_norm). Returns None if no address-shaped
/// token (containing '@') is found.
fn parse_one_address(part: &str) -> Option<(String, String)> {
    let part = part.trim();
    if part.is_empty() {
        return None;
    }
    let (name, addr) = if let (Some(lt), Some(gt)) = (part.find('<'), part.rfind('>')) {
        if lt < gt {
            let name = part[..lt].trim().trim_matches('"').to_string();
            let addr = part[lt + 1..gt].trim().to_string();
            (name, addr)
        } else {
            (String::new(), part.to_string())
        }
    } else {
        (String::new(), part.to_string())
    };
    let addr_norm = addr.trim().trim_matches('"').to_lowercase();
    if !addr_norm.contains('@') {
        return None;
    }
    Some((name.to_lowercase(), addr_norm))
}

/// Build the full recipient-row set for one message from its FetchedMessage-
/// shaped fields (used by both `upsert_messages` and the backfill migration,
/// which reads the same columns back out of `messages`).
pub(crate) fn recipient_rows_for(
    from_addr: &str,
    from_name: &str,
    to_addrs: &str,
    cc_addrs: &str,
    bcc_addrs: &str,
) -> Vec<RecipientRow> {
    let mut out = Vec::new();
    if !from_addr.trim().is_empty() {
        out.push(RecipientRow {
            kind: "from",
            addr_norm: from_addr.trim().to_lowercase(),
            name_norm: from_name.trim().to_lowercase(),
        });
    }
    for (kind, raw) in [("to", to_addrs), ("cc", cc_addrs), ("bcc", bcc_addrs)] {
        for (name_norm, addr_norm) in parse_address_list(raw) {
            out.push(RecipientRow {
                kind,
                addr_norm,
                name_norm,
            });
        }
    }
    out
}

/// Replace every recipient row for one message (delete-then-insert; cheap at
/// per-message granularity and keeps the invariant simple — no diffing).
/// Caller must hold a txn (mirrors the fts/rollup maintenance helpers).
pub(crate) fn replace_message_recipients(
    conn: &Connection,
    account_id: &str,
    message_id: &str,
    rows: &[RecipientRow],
) -> Result<(), String> {
    conn.execute(
        "DELETE FROM message_recipients WHERE account_id = ?1 AND message_id = ?2",
        params![account_id, message_id],
    )
    .map_err(|e| format!("clear recipients: {e}"))?;
    if rows.is_empty() {
        return Ok(());
    }
    let mut stmt = conn
        .prepare(
            "INSERT INTO message_recipients(account_id, message_id, kind, addr_norm, name_norm)
             VALUES(?1, ?2, ?3, ?4, ?5)",
        )
        .map_err(|e| format!("prepare insert recipient: {e}"))?;
    for r in rows {
        stmt.execute(params![
            account_id,
            message_id,
            r.kind,
            r.addr_norm,
            r.name_norm
        ])
        .map_err(|e| format!("insert recipient: {e}"))?;
    }
    Ok(())
}

/// Delete every recipient row for the given message ids (mirrors
/// `delete_message_rows`'s per-id cleanup for the other derived tables).
pub(crate) fn delete_message_recipients(
    conn: &Connection,
    account_id: &str,
    message_ids: &[String],
) -> Result<(), String> {
    for id in message_ids {
        conn.execute(
            "DELETE FROM message_recipients WHERE account_id = ?1 AND message_id = ?2",
            params![account_id, id],
        )
        .map_err(|e| format!("delete recipients: {e}"))?;
    }
    Ok(())
}

/// One backfill scan row: rowid, account, message id, then the header columns.
type BackfillRow = (
    i64,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// One-time batched backfill from the existing header-string columns into
/// `message_recipients`, run inside the v11 migration txn. Batches by rowid
/// range so a large mailbox doesn't build one giant prepared-statement list;
/// each batch is still inside the SAME outer migration transaction (atomic
/// with the schema change — a crash mid-backfill rolls back to v10, not a
/// half-populated v11).
pub(crate) fn backfill_message_recipients(conn: &Connection) -> Result<usize, String> {
    const BATCH: i64 = 2000;
    let mut inserted = 0usize;
    let mut last_rowid = 0i64;
    loop {
        let rows: Vec<BackfillRow> = crate::db::query_map_collect(
            conn,
            "SELECT rowid, account_id, id, from_addr, from_name, to_addrs, cc_addrs, bcc_addrs
                 FROM messages WHERE rowid > ?1 ORDER BY rowid ASC LIMIT ?2",
            params![last_rowid, BATCH],
            "backfill scan",
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )?;
        if rows.is_empty() {
            break;
        }
        for (rowid, account_id, message_id, from_addr, from_name, to_addrs, cc_addrs, bcc_addrs) in
            &rows
        {
            let recipient_rows = recipient_rows_for(
                from_addr.as_deref().unwrap_or(""),
                from_name.as_deref().unwrap_or(""),
                to_addrs.as_deref().unwrap_or(""),
                cc_addrs.as_deref().unwrap_or(""),
                bcc_addrs.as_deref().unwrap_or(""),
            );
            inserted += recipient_rows.len();
            replace_message_recipients(conn, account_id, message_id, &recipient_rows)?;
            last_rowid = *rowid;
        }
        if (rows.len() as i64) < BATCH {
            break;
        }
    }
    Ok(inserted)
}
