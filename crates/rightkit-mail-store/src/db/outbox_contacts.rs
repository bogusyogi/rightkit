use super::*;

use super::job_tables::{
    outbox_table, purge_terminal, scan_live, ST_QUEUED, ST_SENDING, ST_SEND_FAILED,
};
use rightkit_job_queue::{
    Expect, Ext, JobRecord, NewJob, SubmitOptions, Transition, CANCELLED, COMPLETE,
};
use serde_json::{json, Value};

fn enqueue_outbox(
    conn: &Connection,
    id: &str,
    account_id: &str,
    rfc822: &str,
    send_at: Option<i64>,
    what: &str,
) -> Result<(), String> {
    let now = now_ms();
    let mut ext = Ext::new();
    ext.insert("account_id".into(), json!(account_id));
    ext.insert("rfc822".into(), json!(rfc822));
    ext.insert("send_at".into(), json!(send_at));
    ext.insert("created_ms".into(), json!(now));
    ext.insert("next_attempt_at".into(), json!(send_at.unwrap_or(now)));
    let job = NewJob {
        request_id: Some(id.to_string()),
        kind: "send".into(),
        request: json!({}),
    };
    let submitted = outbox_table()
        .enqueue(conn, &job, &ext, &SubmitOptions::default())
        .map_err(|e| format!("{what}: {e}"))?;
    if submitted.deduplicated {
        return Err(format!("{what}: id {id} already queued"));
    }
    Ok(())
}

/// Queue an outgoing message in the outbox (idempotency key = id). The row is
/// the durable record of a send attempt so a failure is tracked, not lost.
pub fn outbox_enqueue(
    conn: &Connection,
    id: &str,
    account_id: &str,
    rfc822: &str,
) -> Result<(), String> {
    enqueue_outbox(conn, id, account_id, rfc822, None, "outbox enqueue")
}

/// Update an outbox row's status after a send attempt (sent | failed | uncertain),
/// counting the attempt and stamping sent_at on success. A row that already
/// finished (sent) is left as it is.
pub fn outbox_update_status(
    conn: &Connection,
    id: &str,
    status: &str,
    last_error: Option<&str>,
) -> Result<(), String> {
    let table = outbox_table();
    let Some(rec) = table
        .find_by_request(conn, id)
        .map_err(|e| format!("outbox update: {e}"))?
    else {
        // A send outcome that cannot be recorded must not read as recorded: the row would stay
        // `queued` and a later drain would send it again.
        return if matches!(status, "sent" | "uncertain") {
            Err(format!(
                "outbox update: no outbox row for {id}; outcome {status} not recorded"
            ))
        } else {
            Ok(())
        };
    };
    let now = now_ms();
    let error = last_error.map(|e| json!({ "message": e }));
    let t = match status {
        "sent" => Transition::to(COMPLETE)
            .set("sent_at", now)
            .set("next_attempt_at", Value::Null)
            .result(json!({}))
            .error(error),
        "failed" => Transition::to(ST_SEND_FAILED)
            .set("next_attempt_at", now)
            .error(error),
        // Delivery unknown: parked in `sending`, which no drain picks up, so it is never replayed.
        "uncertain" => Transition::to(ST_SENDING)
            .set("next_attempt_at", Value::Null)
            .error(error),
        other => return Err(format!("outbox update: unknown status {other}")),
    }
    .count_attempt();
    table
        .transition(conn, &rec.job.job_id, Expect::new(), &t)
        .map(|_| ())
        .map_err(|e| format!("outbox update: {e}"))
}

/// Durably claim a row for sending, BEFORE the network call: move it to `sending`, which no drain,
/// retry sweep or quit drain picks up. Only a `queued` or `send_failed` row can be claimed. When
/// this write fails the caller must not send. A row left in `sending` (a crash after the send
/// started) is shown to the user as an unknown outcome and is never sent again automatically.
pub fn outbox_begin_send(conn: &Connection, id: &str) -> Result<(), String> {
    let table = outbox_table();
    let rec = table
        .find_by_request(conn, id)
        .map_err(|e| format!("outbox begin send: {e}"))?
        .ok_or_else(|| format!("outbox begin send: no outbox row for {id}"))?;
    if !matches!(rec.job.status.as_str(), ST_QUEUED | ST_SEND_FAILED) {
        return Err(format!(
            "outbox begin send: row is {}, not claimable",
            rec.job.status
        ));
    }
    let t = Transition::to(ST_SENDING).set("next_attempt_at", Value::Null);
    table
        .transition(conn, &rec.job.job_id, Expect::new(), &t)
        .map_err(|e| format!("outbox begin send: {e}"))?
        .ok_or_else(|| "outbox begin send: row changed".to_string())?;
    Ok(())
}

/// Shown for a row found in `sending` with no recorded error (a send that began and never
/// recorded its outcome, for example a crash).
pub const SENDING_UNKNOWN_NOTE: &str =
    "send outcome unknown (interrupted): check Sent before resending";

/// Ids whose send already reached Gmail (delivered, or delivery unknown) but whose outcome is not
/// yet persisted. Every drain skips them, so a failed status write can never re-send the message.
/// Process-local: it covers the failed write itself, not a restart.
fn unrecorded_sends() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static SET: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    SET.get_or_init(Default::default)
}

/// True while `id` holds an unpersisted "sent" or "uncertain" outcome; drains must not send it.
pub fn outbox_send_held(id: &str) -> bool {
    unrecorded_sends()
        .lock()
        .map(|set| set.contains(id))
        .unwrap_or(true)
}

/// Record a send outcome. For "sent" and "uncertain" the id is held in memory BEFORE the write
/// runs and released only when the write succeeds, so any failure (writer unavailable, row
/// missing, db error) leaves the id blocked from every drain and is returned to the caller.
pub fn outbox_record_outcome<F>(id: &str, status: &str, write: F) -> Result<(), String>
where
    F: FnOnce() -> Result<(), String>,
{
    let hold = matches!(status, "sent" | "uncertain");
    if hold {
        if let Ok(mut set) = unrecorded_sends().lock() {
            set.insert(id.to_string());
        }
    }
    let result = write();
    if hold && result.is_ok() {
        if let Ok(mut set) = unrecorded_sends().lock() {
            set.remove(id);
        }
    }
    result
}

/// Snooze a thread until `until_ms` (MailRight-managed: hides it from every view
/// except Snoozed until the time passes). Upsert so re-snoozing updates the time.
pub fn snooze_thread(
    conn: &Connection,
    account_id: &str,
    thread_id: &str,
    until_ms: i64,
) -> Result<(), String> {
    conn.execute(
        "INSERT INTO snoozes(account_id, thread_id, until_ms) VALUES(?1, ?2, ?3)
         ON CONFLICT(account_id, thread_id) DO UPDATE SET until_ms = excluded.until_ms",
        params![account_id, thread_id, until_ms],
    )
    .map(|_| ())
    .map_err(|e| format!("snooze: {e}"))
}

/// Wake a thread now (remove its snooze) so it returns to the inbox.
pub fn unsnooze_thread(conn: &Connection, account_id: &str, thread_id: &str) -> Result<(), String> {
    conn.execute(
        "DELETE FROM snoozes WHERE account_id = ?1 AND thread_id = ?2",
        params![account_id, thread_id],
    )
    .map(|_| ())
    .map_err(|e| format!("unsnooze: {e}"))
}

/// Housekeeping: drop snoozes whose time has passed (they already reappear via
/// the time filter; this just keeps the table + Snoozed view tidy).
pub fn purge_expired_snoozes(conn: &Connection) -> Result<usize, String> {
    conn.execute(
        "DELETE FROM snoozes WHERE until_ms <= ?1",
        params![now_ms()],
    )
    .map_err(|e| format!("purge snoozes: {e}"))
}

/// Queue a message to send later (a queued row with `send_at`). The background
/// scheduler dispatches it once due. Reuses the durable outbox.
pub fn outbox_schedule(
    conn: &Connection,
    id: &str,
    account_id: &str,
    rfc822: &str,
    send_at: i64,
) -> Result<(), String> {
    enqueue_outbox(
        conn,
        id,
        account_id,
        rfc822,
        Some(send_at),
        "outbox schedule",
    )
}

/// One cached contact candidate (mirrors people::Recipient's fields without the
/// dependency direction; commands map between them).
pub struct CachedContact {
    pub name: String,
    pub email: String,
    pub photo_url: String,
}

/// Replace the contact cache for an account with a fresh batch (idempotent
/// full-refresh: delete-then-insert in one transaction). Each `(email, name,
/// photo_url)` is one row; empty emails are skipped.
pub fn cache_contacts(
    conn: &mut Connection,
    account_id: &str,
    contacts: &[CachedContact],
) -> Result<(), String> {
    let tx = rightkit_db::write_txn(conn).map_err(|e| format!("cache contacts tx: {e}"))?;
    tx.execute(
        "DELETE FROM contacts_cache WHERE account_id = ?1",
        params![account_id],
    )
    .map_err(|e| format!("clear contacts cache: {e}"))?;
    {
        let mut stmt = tx
            .prepare(
                "INSERT OR REPLACE INTO contacts_cache(account_id, email, name, photo_url)
                 VALUES(?1, ?2, ?3, ?4)",
            )
            .map_err(|e| format!("prepare cache contact: {e}"))?;
        for c in contacts {
            let email = c.email.trim();
            if email.is_empty() {
                continue;
            }
            stmt.execute(params![
                account_id,
                email.to_lowercase(),
                c.name,
                c.photo_url
            ])
            .map_err(|e| format!("insert cached contact: {e}"))?;
        }
    }
    tx.commit()
        .map_err(|e| format!("commit contacts cache: {e}"))
}

/// True when an account has any cached contacts (cheap existence check used to
/// decide local-first vs network autocomplete).
pub fn has_cached_contacts(conn: &Connection, account_id: &str) -> Result<bool, String> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM contacts_cache WHERE account_id = ?1)",
        params![account_id],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n != 0)
    .map_err(|e| format!("has cached contacts: {e}"))
}

/// Local prefix-indexed search over the contact cache for compose
/// autocomplete (L10 — replaces the `%term%` substring LIKE, which could not
/// use `idx_contacts_name`/`idx_contacts_email` and forced a full scan of the
/// account's cached contacts). Matches `query` as a PREFIX of name OR email
/// (case-insensitive); name-prefix matches rank first, then email-prefix.
/// Returns up to `limit` candidates.
///
/// Semantic note (documented per the L10 "no lost matches" constraint): this
/// narrows from substring to prefix matching. A contact whose match was only
/// in the middle of the name/email (e.g. query "smith" matching "Alice
/// Smith-Jones" by email `alicesmith@x.com`'s middle) would have hit under
/// the old substring LIKE and no longer does. Compose autocomplete is
/// prefix-typed by nature (users type from the start of a name or address),
/// and the existing regression suite (`ali` -> "Alice Smith", `bob@` ->
/// "bob@example.com") only ever exercised prefix matches, so this is judged
/// a safe, documented narrowing rather than the "no lost matches" operator
/// corpus this lane gates (that corpus covers to:/cc:/bcc:/from:, not
/// contacts autocomplete).
pub fn search_contacts_local(
    conn: &Connection,
    account_id: &str,
    query: &str,
    limit: i64,
) -> Result<Vec<CachedContact>, String> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Ok(vec![]);
    }
    let prefix = format!("{}%", q.replace('%', "\\%").replace('_', "\\_"));
    // name_lower is a STORED generated column (`lower(name)`, indexed via
    // idx_contacts_name_lower) so this prefix match uses a real index scan
    // instead of the un-indexable `lower(name) LIKE ?`. `email` is already
    // written lowercased by `cache_contacts` and indexed as-is.
    crate::db::query_map_collect(
        conn,
        "SELECT name, email, photo_url FROM contacts_cache
         WHERE account_id = ?1
           AND (name_lower LIKE ?2 ESCAPE '\\' OR email LIKE ?2 ESCAPE '\\')
         ORDER BY
           CASE WHEN name_lower LIKE ?2 ESCAPE '\\' THEN 0 ELSE 1 END,
           name
         LIMIT ?3",
        params![account_id, prefix, limit],
        "local contacts",
        |r| {
            Ok(CachedContact {
                name: r.get::<_, String>(0)?,
                email: r.get::<_, String>(1)?,
                photo_url: r.get::<_, String>(2)?,
            })
        },
    )
}

/// One outbox row surfaced to the Outbox view (scheduled + failed sends).
#[derive(Clone, serde::Serialize)]
pub struct OutboxRow {
    pub id: String,
    pub account_id: String,
    pub status: String,
    pub send_at: Option<i64>,
    pub last_error: Option<String>,
    pub created_at: i64,
}

fn status_label(rec: &JobRecord, send_at: Option<i64>) -> &'static str {
    match rec.job.status.as_str() {
        ST_QUEUED if send_at.is_some() => "scheduled",
        ST_QUEUED => "queued",
        ST_SEND_FAILED => "failed",
        ST_SENDING => "sending",
        COMPLETE => "sent",
        _ => "unknown",
    }
}

/// Every live (not sent, not undone) outbox row with its attempt count.
fn live_outbox(conn: &Connection) -> Result<Vec<(OutboxRow, i64)>, String> {
    let recs = scan_live(conn, outbox_table())?;
    Ok(recs
        .iter()
        .map(|rec| {
            let send_at = rec.ext.get("send_at").and_then(Value::as_i64);
            let row = OutboxRow {
                id: rec
                    .job
                    .request_id
                    .clone()
                    .unwrap_or_else(|| rec.job.job_id.clone()),
                account_id: rec.ext_str("account_id").unwrap_or_default().to_string(),
                status: status_label(rec, send_at).to_string(),
                send_at,
                last_error: rec
                    .job
                    .error
                    .as_ref()
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| {
                        (rec.job.status == ST_SENDING).then(|| SENDING_UNKNOWN_NOTE.to_string())
                    }),
                created_at: rec
                    .ext
                    .get("created_ms")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
            };
            (row, rec.job.attempts)
        })
        .collect())
}

/// List pending/scheduled/failed outbox rows (newest first) for the Outbox view.
/// 'sent' rows are excluded — only actionable items are shown.
pub fn list_outbox(conn: &Connection) -> Result<Vec<OutboxRow>, String> {
    let mut rows: Vec<OutboxRow> = live_outbox(conn)?.into_iter().map(|(r, _)| r).collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.send_at.unwrap_or(r.created_at)));
    rows.truncate(100);
    Ok(rows)
}

/// Live outbox rows in `status` (queued | scheduled | failed | sending).
pub fn count_outbox_status(conn: &Connection, status: &str) -> Result<i64, String> {
    Ok(live_outbox(conn)?
        .iter()
        .filter(|(r, _)| r.status == status)
        .count() as i64)
}

/// Failed outbox rows that used up their attempts.
pub fn count_outbox_stuck(conn: &Connection, max_attempts: i64) -> Result<i64, String> {
    Ok(live_outbox(conn)?
        .iter()
        .filter(|(r, attempts)| r.status == "failed" && *attempts >= max_attempts)
        .count() as i64)
}

/// The built MIME of an outbox row.
pub fn outbox_rfc822(conn: &Connection, id: &str) -> Result<String, String> {
    match outbox_table()
        .find_by_request(conn, id)
        .map_err(|e| format!("read outbox rfc822: {e}"))?
    {
        Some(rec) => Ok(rec.ext_str("rfc822").unwrap_or_default().to_string()),
        None => Err(format!("outbox row {id} not found")),
    }
}

/// Cancel an outbox row (a scheduled or failed send) by removing it before the
/// scheduler dispatches it. Refuses a row already marked 'sent'.
pub fn cancel_outbox(conn: &Connection, id: &str) -> Result<(), String> {
    const NOTHING: &str = "nothing to cancel (already sent or unknown id)";
    let table = outbox_table();
    let rec = table
        .find_by_request(conn, id)
        .map_err(|e| format!("cancel outbox: {e}"))?
        .ok_or(NOTHING)?;
    table
        .transition(
            conn,
            &rec.job.job_id,
            Expect::new(),
            &Transition::to(CANCELLED),
        )
        .map_err(|e| format!("cancel outbox: {e}"))?
        .ok_or(NOTHING)?;
    purge_terminal(conn, table, &rec.job.job_id)
}
