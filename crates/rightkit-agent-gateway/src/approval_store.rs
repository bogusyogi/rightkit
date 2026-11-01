//! Database side of send approvals: the agent-created draft record, the
//! approval rows and every status move. A move is one UPDATE guarded by the
//! status it expects, so two deciders can never both win. Rows hold ids, times
//! and reason codes only. The only audit rows written here come from `sweep`, through the gate
//! module's `record_decision`; every other move is audited by its caller.

use super::gate::{record_decision, Decision};
use rightkit_db::rusqlite::{params, Connection, OptionalExtension};

/// An approval waits this long, then it is denied with reason `timeout`.
pub const TIMEOUT_MS: i64 = 15 * 60 * 1000;
/// A send that was claimed but never finished is failed after this long.
pub const STUCK_MS: i64 = 2 * 60 * 1000;
/// Most pending approvals one agent may have.
pub const PENDING_CAP: i64 = 5;
/// Send limits, per agent and per account. Constants: no setting raises them.
pub const HOURLY_LIMIT: i64 = 10;
pub const DAILY_LIMIT: i64 = 50;
pub const HOUR_MS: i64 = 60 * 60 * 1000;
pub const DAY_MS: i64 = 24 * HOUR_MS;
/// Delete limits (threads moved to Trash), per agent and per account.
pub const DELETE_HOURLY_LIMIT: i64 = 20;
pub const DELETE_DAILY_LIMIT: i64 = 100;
/// After the user denies a request, the same agent may not ask for the same
/// target again for this long. A timeout does not count.
pub const DECLINE_COOLDOWN_MS: i64 = 10 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approval {
    pub id: String,
    pub agent_id: String,
    pub account_id: String,
    pub draft_id: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub boot_id: String,
    /// pending | approved | sent | denied | failed
    pub status: String,
    pub reason: Option<String>,
    pub decided_at: Option<i64>,
    pub result_id: Option<String>,
    /// send | forward | delete (see `approval_kind`). For a delete, `draft_id`
    /// holds the thread id.
    pub kind: String,
}

const COLS: &str = "id, agent_id, account_id, draft_id, created_at, expires_at, boot_id, status, reason, decided_at, result_id, kind";

fn row(r: &rightkit_db::rusqlite::Row<'_>) -> rightkit_db::rusqlite::Result<Approval> {
    Ok(Approval {
        id: r.get(0)?,
        agent_id: r.get(1)?,
        account_id: r.get(2)?,
        draft_id: r.get(3)?,
        created_at: r.get(4)?,
        expires_at: r.get(5)?,
        boot_id: r.get(6)?,
        status: r.get(7)?,
        reason: r.get(8)?,
        decided_at: r.get(9)?,
        result_id: r.get(10)?,
        kind: r.get(11)?,
    })
}

fn db(what: &str, e: rightkit_db::rusqlite::Error) -> String {
    format!("approval {what}: {e}")
}

/// Remember a draft the agent created, with the hash of what was validated.
pub fn record_draft(
    conn: &Connection,
    agent_id: &str,
    account: &str,
    draft_id: &str,
    hash: &str,
    now: i64,
) -> Result<(), String> {
    conn.execute(
        "INSERT OR REPLACE INTO agent_drafts(agent_id, account_id, draft_id, created_at, content_hash)
         VALUES(?1, ?2, ?3, ?4, ?5)",
        params![agent_id, account, draft_id, now, hash],
    )
    .map(|_| ())
    .map_err(|e| db("record draft", e))
}

/// [`record_draft`] for a draft that carries attachments: also stores the
/// digest over their names, types, sizes and bytes. The digest is stored only;
/// approve does not compare it.
pub fn record_draft_with_digest(
    conn: &Connection,
    agent_id: &str,
    account: &str,
    draft_id: &str,
    hash: &str,
    attachment_digest: &str,
    now: i64,
) -> Result<(), String> {
    conn.execute(
        "INSERT OR REPLACE INTO agent_drafts(agent_id, account_id, draft_id, created_at, content_hash, attachment_digest)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![agent_id, account, draft_id, now, hash, attachment_digest],
    )
    .map(|_| ())
    .map_err(|e| db("record draft", e))
}

/// Remember a forward draft the app built for this agent. Only a record made
/// here has kind `forward`; `send` accepts `draft` records alone, so a forward
/// draft can never be approved under the plain Send heading.
pub fn record_forward_draft(
    conn: &Connection,
    agent_id: &str,
    account: &str,
    draft_id: &str,
    hash: &str,
    attachment_digest: &str,
    now: i64,
) -> Result<(), String> {
    conn.execute(
        "INSERT OR REPLACE INTO agent_drafts(agent_id, account_id, draft_id, created_at, content_hash, kind, attachment_digest)
         VALUES(?1, ?2, ?3, ?4, ?5, 'forward', ?6)",
        params![agent_id, account, draft_id, now, hash, attachment_digest],
    )
    .map(|_| ())
    .map_err(|e| db("record forward draft", e))
}

/// The recorded hash when this agent created this draft with `draft` (not a
/// forward draft) in this account.
pub fn draft_hash(
    conn: &Connection,
    agent_id: &str,
    account: &str,
    draft_id: &str,
) -> Result<Option<String>, String> {
    conn.query_row(
        "SELECT content_hash FROM agent_drafts
         WHERE agent_id = ?1 AND account_id = ?2 AND draft_id = ?3 AND kind = 'draft'",
        params![agent_id, account, draft_id],
        |r| r.get(0),
    )
    .optional()
    .map_err(|e| db("draft lookup", e))
}

/// The recorded hash for a draft of either kind (the approval card compares it).
pub fn any_draft_hash(
    conn: &Connection,
    agent_id: &str,
    account: &str,
    draft_id: &str,
) -> Result<Option<String>, String> {
    conn.query_row(
        "SELECT content_hash FROM agent_drafts WHERE agent_id = ?1 AND account_id = ?2 AND draft_id = ?3",
        params![agent_id, account, draft_id],
        |r| r.get(0),
    )
    .optional()
    .map_err(|e| db("draft lookup", e))
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<Approval>, String> {
    conn.query_row(
        &format!("SELECT {COLS} FROM agent_approvals WHERE id = ?1"),
        [id],
        row,
    )
    .optional()
    .map_err(|e| db("read", e))
}

pub fn insert_pending(conn: &Connection, a: &Approval, target_key: &str) -> Result<(), String> {
    conn.execute(
        "INSERT INTO agent_approvals(id, agent_id, account_id, draft_id, created_at, expires_at, boot_id, status, kind, target_key)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8, ?9)",
        params![
            a.id,
            a.agent_id,
            a.account_id,
            a.draft_id,
            a.created_at,
            a.expires_at,
            a.boot_id,
            a.kind,
            target_key
        ],
    )
    .map(|_| ())
    .map_err(|e| db("insert", e))
}

/// True when the user denied (reason `user`, not a timeout) a request of this
/// kind by this agent for this target after `since` (exclusive).
pub fn declined_since(
    conn: &Connection,
    agent_id: &str,
    account: &str,
    kind: &str,
    target_key: &str,
    since: i64,
) -> Result<bool, String> {
    conn.query_row(
        "SELECT 1 FROM agent_approvals
         WHERE agent_id = ?1 AND account_id = ?2 AND kind = ?3 AND target_key = ?4
           AND status = 'denied' AND reason = 'user' AND decided_at > ?5
         LIMIT 1",
        params![agent_id, account, kind, target_key, since],
        |r| r.get::<_, i64>(0),
    )
    .optional()
    .map(|v| v.is_some())
    .map_err(|e| db("decline lookup", e))
}

/// The newest approval of `kind` for this draft (a delete: this thread) that is
/// pending or in flight or, for the mail kinds, already sent. A thread that was
/// trashed (and perhaps restored since) can be asked about again.
pub fn live_for_draft(
    conn: &Connection,
    agent_id: &str,
    account: &str,
    draft_id: &str,
    kind: &str,
) -> Result<Option<Approval>, String> {
    let states = if kind == super::approval_kind::DELETE || kind == super::approval_kind::ATTACH {
        "('pending','approved')"
    } else {
        "('pending','approved','sent')"
    };
    conn.query_row(
        &format!(
            "SELECT {COLS} FROM agent_approvals
             WHERE agent_id = ?1 AND account_id = ?2 AND draft_id = ?3 AND kind = ?4
               AND status IN {states}
             ORDER BY created_at DESC LIMIT 1"
        ),
        params![agent_id, account, draft_id, kind],
        row,
    )
    .optional()
    .map_err(|e| db("lookup", e))
}

/// Remember the exact path of a new `attach` approval.
pub fn record_attach_path(conn: &Connection, approval_id: &str, path: &str) -> Result<(), String> {
    conn.execute(
        "INSERT OR REPLACE INTO agent_attach_requests(approval_id, path) VALUES(?1, ?2)",
        params![approval_id, path],
    )
    .map(|_| ())
    .map_err(|e| db("record attach path", e))
}

/// The path an `attach` approval asks about.
pub fn attach_path(conn: &Connection, approval_id: &str) -> Result<Option<String>, String> {
    conn.query_row(
        "SELECT path FROM agent_attach_requests WHERE approval_id = ?1",
        [approval_id],
        |r| r.get(0),
    )
    .optional()
    .map_err(|e| db("attach path", e))
}

pub fn count_pending(conn: &Connection, agent_id: &str) -> Result<i64, String> {
    conn.query_row(
        "SELECT COUNT(*) FROM agent_approvals WHERE agent_id = ?1 AND status = 'pending'",
        [agent_id],
        |r| r.get(0),
    )
    .map_err(|e| db("count", e))
}

/// Requests created since `since` (exclusive) by one agent (`by_agent`) or
/// into one account, whatever became of them: each one interrupted the user.
/// `deletes` counts the delete kind; otherwise sends and forwards together.
pub fn count_sends(
    conn: &Connection,
    by_agent: bool,
    key: &str,
    since: i64,
    deletes: bool,
) -> Result<i64, String> {
    let col = if by_agent { "agent_id" } else { "account_id" };
    conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM agent_approvals
             WHERE {col} = ?1 AND created_at > ?2 AND (kind = 'delete') = ?3"
        ),
        params![key, since, deletes],
        |r| r.get(0),
    )
    .map_err(|e| db("count", e))
}

/// Pending approvals of this run that have not expired, oldest first.
pub fn list_pending(conn: &Connection, now: i64, boot: &str) -> Result<Vec<Approval>, String> {
    let mut st = conn
        .prepare(&format!(
            "SELECT {COLS} FROM agent_approvals
             WHERE status = 'pending' AND boot_id = ?1 AND expires_at > ?2
             ORDER BY created_at, rowid"
        ))
        .map_err(|e| db("list", e))?;
    let rows = st
        .query_map(params![boot, now], row)
        .map_err(|e| db("list", e))?;
    rows.map(|r| r.map_err(|e| db("list", e))).collect()
}

fn due(
    conn: &Connection,
    sql: &str,
    p: &[&dyn rightkit_db::rusqlite::ToSql],
) -> Result<Vec<Approval>, String> {
    let mut st = conn.prepare(sql).map_err(|e| db("sweep", e))?;
    let rows = st.query_map(p, row).map_err(|e| db("sweep", e))?;
    rows.map(|r| r.map_err(|e| db("sweep", e))).collect()
}

/// Close what can no longer be decided: pending rows past their expiry or left
/// by an earlier run become denied/timeout; in-flight sends that never finished
/// become failed/unknown_outcome. Each move is audited once by the gate.
pub fn sweep(conn: &Connection, now: i64, boot: &str) -> Result<(), String> {
    let pending = due(
        conn,
        &format!(
            "SELECT {COLS} FROM agent_approvals
             WHERE status = 'pending' AND (expires_at <= ?1 OR boot_id <> ?2)"
        ),
        &[&now, &boot],
    )?;
    for a in pending {
        if finish(conn, &a.id, "pending", "denied", Some("timeout"), now, None)? {
            record_decision(conn, &a, Decision::Denied, "reason=timeout")?;
        }
    }
    let stuck = due(
        conn,
        &format!(
            "SELECT {COLS} FROM agent_approvals
             WHERE status = 'approved' AND (decided_at <= ?1 OR boot_id <> ?2)"
        ),
        &[&(now - STUCK_MS), &boot],
    )?;
    for a in stuck {
        let reason = Some("unknown_outcome");
        if finish(conn, &a.id, "approved", "failed", reason, now, None)? {
            record_decision(conn, &a, Decision::SendFailed, "reason=unknown_outcome")?;
        }
    }
    Ok(())
}

/// Move `id` from `from` to `to`. True only for the caller that made the move.
pub fn finish(
    conn: &Connection,
    id: &str,
    from: &str,
    to: &str,
    reason: Option<&str>,
    now: i64,
    result_id: Option<&str>,
) -> Result<bool, String> {
    let n = conn
        .execute(
            "UPDATE agent_approvals SET status = ?3, reason = ?4, decided_at = ?5, result_id = ?6
             WHERE id = ?1 AND status = ?2",
            params![id, from, to, reason, now, result_id],
        )
        .map_err(|e| db("update", e))?;
    Ok(n == 1)
}

/// pending -> approved, only while unexpired and from this run. The one place
/// an approval is claimed for sending.
pub fn claim_approve(conn: &Connection, id: &str, now: i64, boot: &str) -> Result<bool, String> {
    let n = conn
        .execute(
            "UPDATE agent_approvals SET status = 'approved', decided_at = ?2
             WHERE id = ?1 AND status = 'pending' AND expires_at > ?2 AND boot_id = ?3",
            params![id, now, boot],
        )
        .map_err(|e| db("claim", e))?;
    Ok(n == 1)
}
