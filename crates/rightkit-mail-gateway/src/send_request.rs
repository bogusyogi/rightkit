//! The agent-facing half of `send`, `forward` and `delete`: validate the
//! request, create a pending approval (the shared checks are in
//! `approval_request`), and answer `approval` status queries. Nothing here
//! sends mail, trashes a thread or can approve anything: a request only ever
//! produces a row that waits for the user's click in the app window (see
//! `approval`).

use super::approval::Clock;
use super::approval_kind as kinds;
use super::approval_request::create_pending;
use super::approval_store::{self as store, MailApproval, SqlStore};
use super::outcome::AgentError;
use super::read_handlers::str_param;
use super::registry::Agent;
use rightkit_approvals::Store;
use rightkit_db::rusqlite::Connection;
use serde_json::{json, Value};

const ID_MAX: usize = 200;

fn bad(msg: &str) -> AgentError {
    AgentError::BadRequest(msg.into())
}

fn db(e: String) -> AgentError {
    super::outcome::from_db_error(&e)
}

pub fn only_keys(params: &Value, allowed: &[&str]) -> Result<(), AgentError> {
    let obj = params
        .as_object()
        .ok_or_else(|| bad("params must be an object"))?;
    match obj.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => {
            let k: String = k.chars().take(40).filter(|c| !c.is_control()).collect();
            Err(AgentError::BadRequest(format!("unknown parameter `{k}`")))
        }
        None => Ok(()),
    }
}

/// Handle `send { account, draft_id }` for an agent whose profile and account
/// the gate already checked. Returns the pending reply, or why not. Also
/// returns the id when a NEW approval was created (the caller notifies the UI).
pub fn create(
    conn: &Connection,
    clock: &dyn Clock,
    agent: &Agent,
    account: &str,
    params: &Value,
) -> Result<(Value, Option<String>), AgentError> {
    only_keys(params, &["account", "draft_id"])?;
    let draft_id = str_param(params, "draft_id", ID_MAX)?;
    SqlStore::new(conn, &clock.boot_id())
        .sweep(clock.now_ms())
        .map_err(db)?;
    // Only a draft this agent made with `draft`, in this account, can be sent.
    // A forward draft is not one: it is approved as a forward, never as a send.
    if store::draft_hash(conn, &agent.id, account, draft_id)
        .map_err(db)?
        .is_none()
    {
        return Err(bad(
            "That draft was not created by this agent in this account.",
        ));
    }
    create_pending(conn, clock, agent, account, kinds::SEND, draft_id, None)
}

/// Handle `delete { account, thread_id }`: one thread per request. The thread
/// must exist in the account and not already be in Trash. The request only
/// creates a pending approval; the move to Trash happens on the user's click.
pub fn create_delete(
    conn: &Connection,
    clock: &dyn Clock,
    agent: &Agent,
    account: &str,
    params: &Value,
) -> Result<(Value, Option<String>), AgentError> {
    only_keys(params, &["account", "thread_id"])?;
    let thread_id = str_param(params, "thread_id", ID_MAX)?;
    match super::approval_trash::thread_state(conn, account, thread_id).map_err(db)? {
        None => return Err(bad("The thread was not found in this account.")),
        Some(s) if s.in_trash => return Err(bad("That thread is already in Trash.")),
        Some(_) => {}
    }
    create_pending(conn, clock, agent, account, kinds::DELETE, thread_id, None)
}

/// Handle `attach_file { account, path }`: one absolute path per request. The
/// path is checked as text only; no file is stat'ed or read here. The request
/// creates a pending approval; the user's card shows the path and the size, and
/// the file is read only after the click (see `attach_path`).
pub fn create_attach(
    conn: &Connection,
    clock: &dyn Clock,
    agent: &Agent,
    account: &str,
    params: &Value,
) -> Result<(Value, Option<String>), AgentError> {
    only_keys(params, &["account", "path"])?;
    let path = str_param(params, "path", super::attach_path::PATH_MAX)?;
    super::attach_path::validate_path(path)?;
    let target = super::attach_path::target(account, path);
    let (value, created) =
        create_pending(conn, clock, agent, account, kinds::ATTACH, &target, None)?;
    if let Some(id) = &created {
        store::record_attach_path(conn, id, path).map_err(db)?;
    }
    Ok((value, created))
}

/// Handle `approval { approval_id }`. An agent sees only approvals it created;
/// an unknown id and someone else's id get the same answer.
pub fn status(
    conn: &Connection,
    clock: &dyn Clock,
    agent: &Agent,
    params: &Value,
) -> Result<Value, AgentError> {
    only_keys(params, &["approval_id"])?;
    let id = str_param(params, "approval_id", ID_MAX)?;
    let approvals = SqlStore::new(conn, &clock.boot_id());
    approvals.sweep(clock.now_ms()).map_err(db)?;
    let a = approvals
        .get(id)
        .map_err(|e| db(e.message))?
        .filter(|a| a.agent_id() == agent.id)
        .ok_or_else(|| bad("No such approval."))?;
    let reason = a.reason.clone().unwrap_or_default();
    let kind = a.kind();
    match a.status() {
        "pending" => Ok(json!({
            "status": "pending",
            "kind": kind,
            "approval_id": a.id(),
            "expires_at": a.expires_at(),
        })),
        "approved" => Ok(json!({"status": "approved", "kind": kind, "approval_id": a.id()})),
        "sent" if kind == kinds::ATTACH => Ok(json!({
            "status": kinds::done_word(kind),
            "kind": kind,
            "approval_id": a.id(),
            "attachment": a.result,
        })),
        "sent" => Ok(json!({
            "status": kinds::done_word(kind),
            "kind": kind,
            "approval_id": a.id(),
        })),
        "denied" => Err(AgentError::ApprovalDenied(if reason == "user" {
            "user"
        } else {
            "timeout"
        })),
        _ if kind == kinds::ATTACH => Err(AgentError::AttachmentFailed(match reason.as_str() {
            "file_missing" => "file_missing",
            "file_not_plain" => "file_not_plain",
            "file_changed" => "file_changed",
            "file_too_large" => "file_too_large",
            "type_blocked" => "type_blocked",
            "not_permitted" => "not_permitted",
            "unknown_outcome" => "unknown_outcome",
            _ => "file_unreadable",
        })),
        _ if kind == kinds::DELETE => Err(AgentError::TrashFailed(match reason.as_str() {
            "unknown_outcome" => "unknown_outcome",
            "no_credentials" => "no_credentials",
            "not_permitted" => "not_permitted",
            "thread_missing" => "thread_missing",
            "draft_changed" => "draft_changed",
            _ => "gmail_error",
        })),
        _ if reason == "draft_changed" => Err(AgentError::DraftChanged),
        _ => Err(AgentError::SendFailed(match reason.as_str() {
            "unknown_outcome" => "unknown_outcome",
            "no_credentials" => "no_credentials",
            "not_permitted" => "not_permitted",
            _ => "fetch_failed",
        })),
    }
}
