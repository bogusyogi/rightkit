//! The `label`, `archive` and `move` methods: input validation and the typed
//! effect the gate authorises. Same seam as `draft`: the gate validates under
//! short database closures, then hands a [`ModifyRequest`] to the injected
//! [`super::draft::DraftCreator`] with no database lock held.
//!
//! These methods only add or remove label ids on threads. They cannot reach
//! Trash, Spam, Sent or Draft (the forbidden set is checked here and again
//! against the local catalog), and nothing deletes a thread. [`counts`] is the
//! only text derived from a request that reaches the audit log.

use super::draft::DraftFailure;
use super::outcome::AgentError;
use serde_json::Value;
use std::collections::HashSet;

/// Threads one request may change.
pub const MAX_THREADS: usize = 50;
/// Thread changes one agent may make per rolling hour.
/// (Changes are serialised per process; see `gate_effects::hold_mutation`.)
pub const HOURLY_LIMIT: i64 = 200;
const ID_MAX: usize = 200;
const LABELS_MAX: usize = 10;
/// System labels an agent may add or remove.
pub const SAFE_SYSTEM: [&str; 4] = ["STARRED", "IMPORTANT", "UNREAD", "INBOX"];
/// System labels no agent request may touch.
const FORBIDDEN: [&str; 4] = ["TRASH", "SPAM", "SENT", "DRAFT"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Label,
    Archive,
    Move,
}

impl Op {
    pub fn from_method(m: &str) -> Option<Op> {
        match m {
            "label" => Some(Op::Label),
            "archive" => Some(Op::Archive),
            "move" => Some(Op::Move),
            _ => None,
        }
    }
    fn allowed_keys(self) -> &'static [&'static str] {
        match self {
            Op::Label => &["account", "thread_ids", "add", "remove"],
            Op::Archive => &["account", "thread_ids"],
            Op::Move => &["account", "thread_ids", "target"],
        }
    }
}

/// A validated request. Label ids are syntactically safe; whether they exist
/// in the account's catalog is checked later, against the database.
#[derive(Clone, PartialEq, Eq)]
pub struct MutateRequest {
    pub op: Op,
    pub account: String,
    pub threads: Vec<String>,
    pub add: Vec<String>,
    pub remove: Vec<String>,
}

/// Thread ids are mail identifiers; keep them out of any `{:?}` output.
impl std::fmt::Debug for MutateRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MutateRequest")
            .field("counts", &counts(self))
            .finish_non_exhaustive()
    }
}

/// What the creator performs in Gmail: one batch change over message ids.
#[derive(Clone, PartialEq, Eq)]
pub struct ModifyRequest {
    pub account: String,
    pub message_ids: Vec<String>,
    pub add: Vec<String>,
    pub remove: Vec<String>,
    pub threads: usize,
}

impl std::fmt::Debug for ModifyRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModifyRequest")
            .field("threads", &self.threads)
            .field("messages", &self.message_ids.len())
            .finish_non_exhaustive()
    }
}

/// Gmail's batchModify takes at most 1000 ids per call.
pub const BATCH: usize = 1000;

/// Gmail calls needed for `messages` message ids.
pub fn chunk_count(messages: usize) -> usize {
    messages.div_ceil(BATCH)
}

/// A Gmail change that did not finish: why, and how many whole batches Gmail
/// had taken before it stopped (the batch in flight may also have landed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModifyFailure {
    pub why: DraftFailure,
    pub applied_chunks: usize,
}

/// Counts only, e.g. `threads=3 add=1 remove=1`.
pub fn counts(r: &MutateRequest) -> String {
    format!(
        "threads={} add={} remove={}",
        r.threads.len(),
        r.add.len(),
        r.remove.len()
    )
}

fn bad(msg: &str) -> AgentError {
    AgentError::BadRequest(msg.into())
}

/// A label id: short, ASCII letters, digits, `_` and `-` only.
fn label_id_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= ID_MAX
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub fn is_forbidden(id: &str) -> bool {
    FORBIDDEN.iter().any(|f| f.eq_ignore_ascii_case(id))
}

pub fn is_safe_system(id: &str) -> bool {
    SAFE_SYSTEM.contains(&id)
}

fn thread_ids(params: &Value) -> Result<Vec<String>, AgentError> {
    let arr = params
        .get("thread_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("`thread_ids` is required (an array of thread ids)"))?;
    if arr.is_empty() {
        return Err(bad("`thread_ids` needs at least one thread id"));
    }
    if arr.len() > MAX_THREADS {
        return Err(bad("too many threads (50 at most per request)"));
    }
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        let id = v
            .as_str()
            .filter(|s| {
                !s.is_empty()
                    && s.chars().count() <= ID_MAX
                    && !s.chars().any(|c| c.is_control() || c.is_whitespace())
            })
            .ok_or_else(|| bad("each thread id must be a string of 1 to 200 characters"))?;
        if seen.insert(id) {
            out.push(id.to_string());
        }
    }
    Ok(out)
}

fn label_list(params: &Value, name: &str) -> Result<Vec<String>, AgentError> {
    let arr = match params.get(name) {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(a)) => a,
        Some(_) => {
            return Err(AgentError::BadRequest(format!(
                "`{name}` must be an array of label ids"
            )))
        }
    };
    if arr.len() > LABELS_MAX {
        return Err(AgentError::BadRequest(format!(
            "`{name}` has too many labels (10 at most)"
        )));
    }
    let mut out: Vec<String> = Vec::new();
    for v in arr {
        match v.as_str() {
            Some(s) if label_id_ok(s) => {
                if is_forbidden(s) {
                    return Err(bad("that system label cannot be changed by an agent"));
                }
                if !out.iter().any(|x| x == s) {
                    out.push(s.to_string());
                }
            }
            _ => {
                return Err(AgentError::BadRequest(format!(
                    "`{name}` entries must be label ids (letters, digits, _ and -)"
                )))
            }
        }
    }
    Ok(out)
}

/// Validate the params of a `label`, `archive` or `move` request for
/// `account` (already checked against the agent's allow list). Pure.
pub fn validate(op: Op, account: &str, params: &Value) -> Result<MutateRequest, AgentError> {
    let obj = params
        .as_object()
        .ok_or_else(|| bad("params must be an object"))?;
    if let Some(k) = obj
        .keys()
        .find(|k| !op.allowed_keys().contains(&k.as_str()))
    {
        let k: String = k.chars().take(40).filter(|c| !c.is_control()).collect();
        return Err(AgentError::BadRequest(format!("unknown parameter `{k}`")));
    }
    let threads = thread_ids(params)?;
    let (add, remove) = match op {
        Op::Archive => (Vec::new(), vec!["INBOX".to_string()]),
        Op::Label => {
            let (add, remove) = (label_list(params, "add")?, label_list(params, "remove")?);
            if add.is_empty() && remove.is_empty() {
                return Err(bad("`label` needs `add` or `remove` with at least one id"));
            }
            if add.iter().any(|a| remove.contains(a)) {
                return Err(bad("a label cannot be in both `add` and `remove`"));
            }
            (add, remove)
        }
        Op::Move => {
            let target = params
                .get("target")
                .and_then(Value::as_str)
                .filter(|t| label_id_ok(t))
                .ok_or_else(|| bad("`target` is required (a label id or INBOX)"))?;
            if is_forbidden(target) {
                return Err(bad("that system label cannot be changed by an agent"));
            }
            if target == "INBOX" {
                (vec!["INBOX".to_string()], Vec::new())
            } else if is_safe_system(target) {
                return Err(bad("`target` must be a user label or INBOX"));
            } else {
                (vec![target.to_string()], vec!["INBOX".to_string()])
            }
        }
    };
    Ok(MutateRequest {
        op,
        account: account.to_string(),
        threads,
        add,
        remove,
    })
}
