//! Database side of `label`, `archive` and `move`: resolving a validated
//! request against the local catalog and threads, the hourly limit, and the
//! optimistic local update after Gmail has accepted the change. Each function
//! runs inside one short gate closure; none touches the network.

use super::mutate::{is_forbidden, is_safe_system, ModifyRequest, MutateRequest, Op, HOURLY_LIMIT};
use super::outcome::AgentError;
use crate::host::{host, MutationClass};
use rightkit_db::rusqlite::{params, Connection, OptionalExtension};

const WINDOW_MS: i64 = 60 * 60 * 1000;
pub const LIMIT_MESSAGE: &str =
    "Hourly limit reached: an agent may change 200 threads per hour. Try again later.";

/// A request resolved against the database: what Gmail is asked to do, and
/// the guard class for the change.
pub struct Plan {
    pub req: MutateRequest,
    pub modify: ModifyRequest,
    pub class: MutationClass,
}

fn db(e: String) -> AgentError {
    super::outcome::from_db_error(&e)
}

/// Thread changes this agent made (or tried to make) in the last hour. Counted
/// from the audit rows the gate already writes: `threads=N` in the detail of
/// an `ok` or `mutation_failed` label, archive or move.
pub fn used_last_hour(conn: &Connection, agent_id: &str, now_ms: i64) -> Result<i64, String> {
    let mut st = conn
        .prepare(
            "SELECT detail FROM agent_audit
              WHERE agent_id = ?1 AND at >= ?2 AND at <= ?3
                AND method IN ('label','archive','move')
                AND outcome IN ('ok','mutation_failed')",
        )
        .map_err(|e| format!("limit read: {e}"))?;
    let rows = st
        .query_map(params![agent_id, now_ms - WINDOW_MS, now_ms], |r| {
            r.get::<_, Option<String>>(0)
        })
        .map_err(|e| format!("limit read: {e}"))?;
    let mut total = 0i64;
    for r in rows {
        let detail = r
            .map_err(|e| format!("limit read: {e}"))?
            .unwrap_or_default();
        total += threads_in(&detail);
    }
    Ok(total)
}

/// The `threads=N` figure of a mutation audit detail (0 when absent).
fn threads_in(detail: &str) -> i64 {
    detail
        .split_whitespace()
        .find_map(|w| w.strip_prefix("threads="))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// Is `id` allowed as a label to add or remove in `account`? A safe system
/// label always is; any other must be a user label in the local catalog.
fn label_allowed(conn: &Connection, account: &str, id: &str) -> Result<bool, AgentError> {
    if is_forbidden(id) {
        return Ok(false);
    }
    if is_safe_system(id) {
        return Ok(true);
    }
    let kind: Option<Option<String>> = conn
        .query_row(
            "SELECT type FROM labels WHERE account_id = ?1 AND id = ?2",
            params![account, id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| db(format!("label lookup: {e}")))?;
    Ok(matches!(kind, Some(Some(t)) if t == "user"))
}

/// Check the request against the account's labels and threads and the agent's
/// hourly allowance. Returns the plan to perform. Reads only.
pub fn resolve(
    conn: &Connection,
    agent_id: &str,
    req: MutateRequest,
    now_ms: i64,
) -> Result<Plan, AgentError> {
    for id in req.add.iter().chain(req.remove.iter()) {
        if !label_allowed(conn, &req.account, id)? {
            return Err(AgentError::BadRequest(
                "a label was not found in this account, or agents may not change it".into(),
            ));
        }
    }
    let mut message_ids: Vec<String> = Vec::new();
    for t in &req.threads {
        let ids = host()
            .thread_message_ids(conn, &req.account, t)
            .map_err(db)?;
        if ids.is_empty() {
            return Err(AgentError::BadRequest(
                "a thread was not found in this account".into(),
            ));
        }
        message_ids.extend(ids);
    }
    let used = used_last_hour(conn, agent_id, now_ms).map_err(db)?;
    if used + req.threads.len() as i64 > HOURLY_LIMIT {
        return Err(AgentError::Quota(LIMIT_MESSAGE));
    }
    let class = if req.remove.iter().any(|l| l == "INBOX") {
        MutationClass::ReversibleMutation
    } else {
        MutationClass::SafeMutation
    };
    let modify = ModifyRequest {
        account: req.account.clone(),
        message_ids,
        add: req.add.clone(),
        remove: req.remove.clone(),
        threads: req.threads.len(),
    };
    Ok(Plan { req, modify, class })
}

/// The guard action name recorded in the app's own mutation audit.
pub fn guard_action(op: Op) -> &'static str {
    match op {
        Op::Label => "agent_label",
        Op::Archive => "agent_archive",
        Op::Move => "agent_move",
    }
}

/// Update the local database the way the app's own actions do (see
/// the bulk-action engine group's `apply_local_action` and `relabel`), so the UI shows
/// the change before the next sync. UNREAD and STARRED also move the thread
/// flags the lists read.
pub fn apply_local(conn: &Connection, req: &MutateRequest) -> Result<(), String> {
    for thread in &req.threads {
        for id in &req.add {
            apply_one(conn, &req.account, thread, id, true)?;
        }
        for id in &req.remove {
            apply_one(conn, &req.account, thread, id, false)?;
        }
    }
    Ok(())
}

fn apply_one(
    conn: &Connection,
    account: &str,
    thread: &str,
    label: &str,
    add: bool,
) -> Result<(), String> {
    let h = host();
    match (label, add) {
        ("UNREAD", true) => h.set_thread_unread(conn, account, thread),
        ("UNREAD", false) => h.set_thread_read(conn, account, thread),
        ("STARRED", true) => {
            h.local_add_label(conn, account, thread, label)?;
            h.set_thread_starred(conn, account, thread, true)
        }
        ("STARRED", false) => {
            h.local_drop_label(conn, account, thread, label)?;
            h.set_thread_starred(conn, account, thread, false)
        }
        (_, true) => h.local_add_label(conn, account, thread, label),
        (_, false) => h.local_drop_label(conn, account, thread, label),
    }
}
