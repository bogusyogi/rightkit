//! Creating a pending approval, for all three kinds (send, forward, delete):
//! the shared checks and the insert. The agent-facing methods call this and
//! nothing here can decide an approval, send mail or trash a thread: a request
//! only ever produces a row that waits for the user's click in the app window
//! (see `approval`).
//!
//! Order, never reordered: sweep the expired rows, answer a repeat of a
//! request that is still pending with the same approval, refuse a target the
//! user declined in the last 10 minutes, refuse over the pending cap (5 per agent, all kinds together), refuse over the hourly and
//! daily limits (send and forward share 10 and 50; delete has 20 and 100;
//! each per agent and per account), then insert.

use super::approval::Clock;
use super::approval_kind as kinds;
use super::approval_store::{self as store, Approval};
use super::outcome::{from_db_error, AgentError};
use super::registry::Agent;
use rightkit_db::rusqlite::Connection;
use serde_json::{json, Value};

fn db(e: String) -> AgentError {
    from_db_error(&e)
}

fn new_id() -> Result<String, String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(|e| format!("random: {e}"))?;
    Ok(rightkit_crypto::base64url_encode(&b))
}

/// The reply while a request waits for the user.
pub fn pending_reply(a: &Approval) -> Value {
    json!({
        "status": "approval_pending",
        "kind": a.kind,
        "approval_id": a.id,
        "expires_at": a.expires_at,
        "note": kinds::pending_note(&a.kind),
    })
}

/// The fixed reply while a user's denial blocks the same request.
pub const DECLINED_MESSAGE: &str = "The user declined this request. Do not ask again.";

/// Refuse when the user declined this very target recently, this agent or
/// account has used up its allowance for `kind`, or the agent already has the
/// most pending requests. Sweeps first. Reads and closes rows only; creates
/// nothing. `target_key` names what the request is about (see `create_pending`).
pub fn check_room(
    conn: &Connection,
    clock: &dyn Clock,
    agent: &Agent,
    account: &str,
    kind: &str,
    target_key: &str,
) -> Result<(), AgentError> {
    let (now, boot) = (clock.now_ms(), clock.boot_id());
    store::sweep(conn, now, &boot).map_err(db)?;
    let since = now - store::DECLINE_COOLDOWN_MS;
    if store::declined_since(conn, &agent.id, account, kind, target_key, since).map_err(db)? {
        return Err(AgentError::Quota(DECLINED_MESSAGE));
    }
    if store::count_pending(conn, &agent.id).map_err(db)? >= store::PENDING_CAP {
        return Err(AgentError::RateLimited);
    }
    let deletes = kind == kinds::DELETE;
    let (hourly, daily) = if deletes {
        (store::DELETE_HOURLY_LIMIT, store::DELETE_DAILY_LIMIT)
    } else {
        (store::HOURLY_LIMIT, store::DAILY_LIMIT)
    };
    for (by_agent, key) in [(true, agent.id.as_str()), (false, account)] {
        for (window, limit) in [(store::HOUR_MS, hourly), (store::DAY_MS, daily)] {
            if store::count_sends(conn, by_agent, key, now - window, deletes).map_err(db)? >= limit
            {
                return Err(AgentError::RateLimited);
            }
        }
    }
    Ok(())
}

/// Create the pending approval for `target` (a draft id, or for a delete a
/// thread id) of `kind`. `key` names the target for the decline cooldown; it
/// defaults to `target` (a forward passes a hash of the original message and
/// the recipient set, because each forward draft has a new id). Returns the
/// reply, and the id when a NEW approval was created (the caller tells the
/// user's window).
pub fn create_pending(
    conn: &Connection,
    clock: &dyn Clock,
    agent: &Agent,
    account: &str,
    kind: &str,
    target: &str,
    key: Option<&str>,
) -> Result<(Value, Option<String>), AgentError> {
    let (now, boot) = (clock.now_ms(), clock.boot_id());
    let key = key.unwrap_or(target);
    store::sweep(conn, now, &boot).map_err(db)?;
    if let Some(a) = store::live_for_draft(conn, &agent.id, account, target, kind).map_err(db)? {
        return match a.status.as_str() {
            "pending" => Ok((pending_reply(&a), None)),
            _ => Err(AgentError::BadRequest(
                "That request already has an action in progress or done.".into(),
            )),
        };
    }
    check_room(conn, clock, agent, account, kind, key)?;
    let a = Approval {
        id: new_id().map_err(db)?,
        agent_id: agent.id.clone(),
        account_id: account.to_string(),
        draft_id: target.to_string(),
        created_at: now,
        expires_at: now + store::TIMEOUT_MS,
        boot_id: boot,
        status: "pending".into(),
        reason: None,
        decided_at: None,
        result_id: None,
        kind: kind.to_string(),
    };
    store::insert_pending(conn, &a, key).map_err(db)?;
    Ok((pending_reply(&a), Some(a.id)))
}
