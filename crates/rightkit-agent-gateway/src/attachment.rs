//! The `attachment` method: input validation, resolving an attachment against
//! the local attachment table, and the request the injected creator fetches.
//! The bytes are written by [`super::export`], never by the caller's choice of
//! path. The audit log gets a size, never a file name.

use super::export::FILE_CAP;
use super::outcome::AgentError;
use rightkit_db::rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

const ID_MAX: usize = 200;
/// Fetch and scan together stay inside this, under the engine's 20 second
/// request deadline.
pub const TOTAL_BUDGET: std::time::Duration = std::time::Duration::from_secs(18);

pub const TOO_LARGE_MESSAGE: &str =
    "That attachment is larger than 25 MiB, the most an agent may export in one file.";

/// How the caller picked an attachment of a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    Id(String),
    /// Zero-based position in the message's attachment list (the order the
    /// `thread` reply shows).
    Index(usize),
}

/// A validated request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentQuery {
    pub account: String,
    pub message_id: String,
    pub selector: Selector,
}

/// A resolved attachment. `name` and `mime` are third-party text.
#[derive(Clone, PartialEq, Eq)]
pub struct AttachmentRequest {
    pub account: String,
    pub message_id: String,
    pub attachment_id: String,
    pub name: String,
    pub mime: String,
    pub size: i64,
}

/// File names are mail content: keep them out of any `{:?}` output.
impl std::fmt::Debug for AttachmentRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AttachmentRequest")
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

fn bad(msg: &str) -> AgentError {
    AgentError::BadRequest(msg.into())
}

fn id_ok(s: &str) -> bool {
    !s.is_empty()
        && s.chars().count() <= ID_MAX
        && !s
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '/' || c == '\\')
}

/// Validate `{account, message_id, attachment_id | index}`. Pure.
pub fn validate(account: &str, params: &Value) -> Result<AttachmentQuery, AgentError> {
    let obj = params
        .as_object()
        .ok_or_else(|| bad("params must be an object"))?;
    if let Some(k) = obj
        .keys()
        .find(|k| !["account", "message_id", "attachment_id", "index"].contains(&k.as_str()))
    {
        let k: String = k.chars().take(40).filter(|c| !c.is_control()).collect();
        return Err(AgentError::BadRequest(format!("unknown parameter `{k}`")));
    }
    let message_id = params
        .get("message_id")
        .and_then(Value::as_str)
        .filter(|s| id_ok(s))
        .ok_or_else(|| bad("`message_id` is required (a message id string)"))?;
    let selector = match (params.get("attachment_id"), params.get("index")) {
        (Some(Value::String(id)), None) if id_ok(id) => Selector::Id(id.clone()),
        (None, Some(Value::Number(n))) => match n.as_u64() {
            Some(i) if i < 1000 => Selector::Index(i as usize),
            _ => return Err(bad("`index` must be a whole number from 0 to 999")),
        },
        _ => return Err(bad("give exactly one of `attachment_id` or `index`")),
    };
    Ok(AttachmentQuery {
        account: account.to_string(),
        message_id: message_id.to_string(),
        selector,
    })
}

/// One local attachment row of a message, in list order.
pub struct Listed {
    pub attachment_id: String,
    pub name: String,
    pub mime: String,
    pub size: i64,
}

/// The attachments the local database knows for a message, in a fixed order.
pub fn list_for_message(
    conn: &Connection,
    account: &str,
    message_id: &str,
) -> Result<Vec<Listed>, String> {
    query_map_collect(
        conn,
        "SELECT attachment_id, COALESCE(filename, ''), COALESCE(mime_type, ''), COALESCE(size, 0)
           FROM attachments WHERE account_id = ?1 AND message_id = ?2 ORDER BY rowid",
        params![account, message_id],
        "attachments",
        |r| {
            Ok(Listed {
                attachment_id: r.get(0)?,
                name: r.get(1)?,
                mime: r.get(2)?,
                size: r.get(3)?,
            })
        },
    )
}

/// Find the attachment the query names. The message must exist locally for the
/// account; an over-size file is refused here, before any download.
pub fn resolve(conn: &Connection, q: &AttachmentQuery) -> Result<AttachmentRequest, AgentError> {
    let db = |e: String| super::outcome::from_db_error(&e);
    let known: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM messages WHERE account_id = ?1 AND id = ?2",
            params![q.account, q.message_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| db(format!("message lookup: {e}")))?;
    if known.is_none() {
        return Err(bad("the message was not found in this account"));
    }
    let list = list_for_message(conn, &q.account, &q.message_id).map_err(db)?;
    let found = match &q.selector {
        Selector::Id(id) => list.into_iter().find(|a| &a.attachment_id == id),
        Selector::Index(i) => list.into_iter().nth(*i),
    };
    let a = found.ok_or_else(|| bad("the attachment was not found in that message"))?;
    if a.size > FILE_CAP as i64 {
        return Err(AgentError::Quota(TOO_LARGE_MESSAGE));
    }
    Ok(AttachmentRequest {
        account: q.account.clone(),
        message_id: q.message_id.clone(),
        attachment_id: a.attachment_id,
        name: a.name,
        mime: a.mime,
        size: a.size,
    })
}

/// Prepare `sql`, run it with `params` and collect every row through `row_fn`. `label` names the
/// operation in the three error texts (prepare, query, row).
fn query_map_collect<T, P, F>(
    conn: &Connection,
    sql: &str,
    params: P,
    label: &str,
    mut row_fn: F,
) -> Result<Vec<T>, String>
where
    P: rightkit_db::rusqlite::Params,
    F: FnMut(&rightkit_db::rusqlite::Row<'_>) -> rightkit_db::rusqlite::Result<T>,
{
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("prepare {label}: {e}"))?;
    let rows = stmt
        .query_map(params, |r| row_fn(r))
        .map_err(|e| format!("query {label}: {e}"))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| format!("row {label}: {e}"))?);
    }
    Ok(out)
}
