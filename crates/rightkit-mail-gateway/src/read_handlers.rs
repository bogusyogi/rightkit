//! Read handlers behind the gate: `accounts`, `search`, `thread`, `labels`.
//! They read through the host ([`GatewayHost`](super::host::GatewayHost)), and
//! propagate errors (no `unwrap_or_default`) and wrap every sender-written
//! field per the untrusted content contract. Handlers never write the audit
//! log and never check policy; the gate does both.

use super::content::{untrusted, untrusted_opt, BodyBudget};
use super::host::{host, SearchCursor, ThreadHit};
use super::outcome::{from_db_error, AgentError};
use super::registry::Agent;
use rightkit_db::rusqlite::Connection;
use serde_json::{json, Value};

/// Threads returned per `search` call.
pub const SEARCH_LIMIT: usize = 50;
/// Most messages one `thread` response carries (the newest ones).
pub const THREAD_MESSAGE_LIMIT: usize = 100;
const QUERY_MAX: usize = 500;
const ID_MAX: usize = 200;

/// A handler result: the bare response object plus a non-content audit detail.
pub struct Out {
    pub value: Value,
    pub detail: String,
}

pub fn str_param<'a>(params: &'a Value, name: &str, max: usize) -> Result<&'a str, AgentError> {
    match params.get(name).and_then(Value::as_str) {
        Some(s) if !s.is_empty() && s.chars().count() <= max => Ok(s),
        _ => Err(AgentError::BadRequest(format!(
            "`{name}` is required (a string of 1 to {max} characters)"
        ))),
    }
}

/// Run a read method. The gate has already checked key, lock, profile and
/// account; `method` is one of the read methods in the gate's table.
pub fn dispatch(
    conn: &Connection,
    agent: &Agent,
    method: &str,
    params: &Value,
) -> Result<Out, AgentError> {
    match method {
        "accounts" => accounts(conn, agent),
        "search" => search(conn, params),
        "thread" => thread(conn, agent, params),
        "labels" => labels(conn, params),
        _ => Err(AgentError::Internal),
    }
}

fn accounts(conn: &Connection, agent: &Agent) -> Result<Out, AgentError> {
    let rows = host().list_accounts(conn).map_err(|e| from_db_error(&e))?;
    let list: Vec<Value> = rows
        .into_iter()
        .filter(|r| agent.scopes.iter().any(|a| a == &r.id))
        .map(|r| json!({"id": r.id, "display_name": r.display_name}))
        .collect();
    let detail = format!("accounts={}", list.len());
    Ok(Out {
        value: json!({"accounts": list}),
        detail,
    })
}

fn encode_cursor(c: &SearchCursor) -> String {
    rightkit_crypto::base64url_encode(format!("{}|{}", c.last_message_at, c.thread_id).as_bytes())
}

fn decode_cursor(token: &str, account: &str) -> Result<SearchCursor, AgentError> {
    let bad = || AgentError::BadRequest("`cursor` is not a valid cursor".into());
    let raw = rightkit_crypto::base64url_decode(token).map_err(|_| bad())?;
    let raw = String::from_utf8(raw).map_err(|_| bad())?;
    let (ms, thread) = raw.split_once('|').ok_or_else(bad)?;
    Ok(SearchCursor {
        last_message_at: ms.parse().map_err(|_| bad())?,
        account_id: account.to_string(),
        thread_id: thread.to_string(),
    })
}

fn thread_summary(t: &ThreadHit) -> Value {
    let snippet = t.matching_message_excerpt.as_deref().unwrap_or(&t.snippet);
    json!({
        "account_id": t.account_id,
        "thread_id": t.thread_id,
        "subject": untrusted(&t.subject),
        "from_name": untrusted(&t.from_name),
        "snippet": untrusted(snippet),
        "participants": t.participants.iter().take(20).map(|p| untrusted(p)).collect::<Vec<_>>(),
        "attachment_names": t.attachment_names.iter().take(10).map(|n| untrusted(n)).collect::<Vec<_>>(),
        "labels": t.labels,
        "unread": t.unread,
        "starred": t.starred,
        "has_attachments": t.has_attachments,
        "last_message_at": t.last_message_at,
        "message_count": t.message_count,
        "match_count": t.match_count,
    })
}

fn search(conn: &Connection, params: &Value) -> Result<Out, AgentError> {
    let account = str_param(params, "account", ID_MAX)?;
    let query = str_param(params, "query", QUERY_MAX)?;
    let cursor = match params.get("cursor") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(decode_cursor(s, account)?),
        Some(_) => return Err(AgentError::BadRequest("`cursor` must be a string".into())),
    };
    // One extra row tells us whether another page exists.
    let mut rows = host()
        .search_threads(conn, query, account, cursor.as_ref(), SEARCH_LIMIT + 1)
        .map_err(|e| from_db_error(&e))?;
    let more = rows.len() > SEARCH_LIMIT;
    rows.truncate(SEARCH_LIMIT);
    let next = match (more, rows.last()) {
        (true, Some(last)) => Value::String(encode_cursor(&SearchCursor {
            last_message_at: last.last_message_at,
            account_id: last.account_id.clone(),
            thread_id: last.thread_id.clone(),
        })),
        _ => Value::Null,
    };
    let detail = format!("threads={}", rows.len());
    Ok(Out {
        value: json!({
            "threads": rows.iter().map(thread_summary).collect::<Vec<_>>(),
            "next_cursor": next,
        }),
        detail,
    })
}

/// The user's own account addresses stay plain; anyone else's is wrapped.
fn address(addr: &str, own: &[String]) -> Value {
    if own.iter().any(|a| a.eq_ignore_ascii_case(addr)) {
        Value::String(addr.to_string())
    } else {
        untrusted(addr)
    }
}

fn thread(conn: &Connection, agent: &Agent, params: &Value) -> Result<Out, AgentError> {
    let account = str_param(params, "account", ID_MAX)?;
    let thread_id = str_param(params, "thread_id", ID_MAX)?;
    let msgs = host()
        .thread_messages(conn, account, thread_id)
        .map_err(|e| from_db_error(&e))?;
    // Long threads return only the newest messages and say so.
    let omitted = msgs.len().saturating_sub(THREAD_MESSAGE_LIMIT);
    let mut budget = BodyBudget::new();
    let mut list: Vec<Value> = Vec::new();
    for m in msgs.iter().skip(omitted) {
        let files = if m.has_attachments {
            attachment_list(conn, account, &m.message_id)?
        } else {
            Vec::new()
        };
        list.push(json!({
            "id": m.message_id,
            "from_name": untrusted(&m.from_name),
            "from_addr": address(&m.from_addr, &agent.scopes),
            "to": untrusted(&m.to_addrs),
            "cc": untrusted(&m.cc_addrs),
            "reply_to": untrusted_opt(m.reply_to.as_deref()),
            "subject": untrusted(&m.subject),
            "date": m.internal_date,
            "unread": m.unread,
            "has_attachments": m.has_attachments,
            "attachments": files,
            "body": budget.body(&m.body, m.is_html),
        }));
    }
    let detail = format!("messages={} omitted={omitted}", list.len());
    Ok(Out {
        value: json!({
            "thread_id": thread_id,
            "messages": list,
            "truncated": omitted > 0,
            "omitted_older_messages": omitted,
        }),
        detail,
    })
}

/// The attachments of one message for the `attachment` method: a zero-based
/// `index`, the id, the size, and the sender-written name and type wrapped.
fn attachment_list(
    conn: &Connection,
    account: &str,
    message_id: &str,
) -> Result<Vec<Value>, AgentError> {
    let rows = super::attachment::list_for_message(conn, account, message_id)
        .map_err(|e| from_db_error(&e))?;
    Ok(rows
        .iter()
        .take(50)
        .enumerate()
        .map(|(i, a)| {
            json!({"index": i, "attachment_id": a.attachment_id, "size": a.size,
                   "name": untrusted(&a.name), "mime": untrusted(&a.mime)})
        })
        .collect())
}

fn labels(conn: &Connection, params: &Value) -> Result<Out, AgentError> {
    let account = str_param(params, "account", ID_MAX)?;
    let rows = host()
        .label_catalog(conn, account)
        .map_err(|e| from_db_error(&e))?;
    let list: Vec<Value> = rows
        .iter()
        .map(|l| {
            json!({"id": l.id, "name": l.name, "type": l.label_type,
                   "total": l.total, "unread": l.unread})
        })
        .collect();
    let detail = format!("labels={}", list.len());
    Ok(Out {
        value: json!({"labels": list}),
        detail,
    })
}
