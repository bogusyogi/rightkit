//! The `draft` method: input validation, the typed effect the gate authorises
//! and the [`DraftCreator`] seam the service layer performs it through.
//!
//! The gate validates and authorises under its short database closures, then
//! hands a [`DraftRequest`] to the creator with no database lock held. The
//! creator creates a draft in the user's Gmail Drafts; it has no send path.
//! Nothing here records recipients, subject or body anywhere: [`counts`] is the
//! only text derived from a request that reaches the audit log.

use super::attachment::AttachmentRequest;
use super::draft_files::{self, DraftAttachment};
use super::export::ScanVerdict;
use super::forward::{ForwardCreated, ForwardRequest};
use super::mutate::{ModifyFailure, ModifyRequest};
use super::outcome::AgentError;
use rightkit_db::rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

/// Recipients across `to`, `cc` and `bcc`.
pub const MAX_RECIPIENTS: usize = 50;
pub const SUBJECT_MAX: usize = 998;
pub const BODY_MAX: usize = 200_000;
const ADDRESS_MAX: usize = 254;
const THREAD_ID_MAX: usize = 200;
/// Gmail label put on agent drafts when the Settings option is on.
pub const LABEL_NAME: &str = "Agent draft";
/// Settings key of the "Label drafts made by agents" option ("true" = on).
pub const LABEL_OPTION_KEY: &str = "agent_label_drafts";

const ALLOWED_KEYS: [&str; 8] = [
    "account",
    "to",
    "cc",
    "bcc",
    "subject",
    "body",
    "reply_to_thread",
    "attachments",
];

/// A validated draft. Every header-bound field is free of control characters.
#[derive(Clone, PartialEq, Eq)]
pub struct DraftRequest {
    pub account: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub body: String,
    /// Thread the draft replies into; must exist locally for `account`.
    pub reply_thread: Option<String>,
    /// Message-ID of the newest message in that thread (header safe), if known.
    pub in_reply_to: Option<String>,
    /// Validated names of files in the agent's export folder (see
    /// `draft_files`); the effect step reads them into `attachments`.
    pub attachment_names: Vec<String>,
    /// The files read for this draft, empty until the effect step fills them.
    pub attachments: Vec<DraftAttachment>,
}

/// Content must never reach a log through `{:?}`.
impl std::fmt::Debug for DraftRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DraftRequest")
            .field("counts", &counts(self))
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftCreated {
    pub draft_id: String,
    /// True only when the "Agent draft" label was applied.
    pub labelled: bool,
}

/// Why no draft was created. The reason class is audited; nothing else is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftFailure {
    NoCredentials,
    Timeout,
    Gmail,
}

impl DraftFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            DraftFailure::NoCredentials => "no_credentials",
            DraftFailure::Timeout => "timeout",
            DraftFailure::Gmail => "gmail_error",
        }
    }
}

/// Performs the effect the gate authorised. Called with no database lock held,
/// so it may block on the network, but must be bounded by its own deadline.
/// Production: the app's Gmail draft-create path. Tests: a recording fake.
pub trait DraftCreator: Send + Sync {
    fn create(&self, req: &DraftRequest, label: bool) -> Result<DraftCreated, DraftFailure>;

    /// `forward`: build the forward draft in Gmail (the original message
    /// quoted as plain text, its attachments included) and read it back for
    /// its content hash. Creates a draft only: no path to a send. Same
    /// contract as `create`: no database lock held, bounded by a deadline.
    fn forward(&self, _req: &ForwardRequest) -> Result<ForwardCreated, DraftFailure> {
        Err(DraftFailure::NoCredentials)
    }

    /// `label`, `archive` and `move`: change label ids on messages in Gmail.
    /// Same contract as `create`: no database lock held, bounded by a
    /// deadline, no path to Trash, Spam or any delete. A failure says how many
    /// whole batches Gmail had already taken.
    fn modify(&self, _req: &ModifyRequest) -> Result<(), ModifyFailure> {
        Err(ModifyFailure {
            why: DraftFailure::NoCredentials,
            applied_chunks: 0,
        })
    }

    /// `attachment`: scan a written file, waiting at most `wait`.
    fn scan(&self, path: &std::path::Path, wait: std::time::Duration) -> ScanVerdict {
        super::export::defender_within(path, wait)
    }

    /// `attachment`: the bytes of one attachment (at most the file cap).
    fn fetch_attachment(&self, _req: &AttachmentRequest) -> Result<Vec<u8>, DraftFailure> {
        Err(DraftFailure::NoCredentials)
    }

    /// The app-owned root exported attachments are written under, if this
    /// context hosts one.
    fn export_root(&self) -> Option<std::path::PathBuf> {
        None
    }
}

/// Creator for contexts that host no Gmail path: every call fails.
pub struct NoDrafts;

impl DraftCreator for NoDrafts {
    fn create(&self, _req: &DraftRequest, _label: bool) -> Result<DraftCreated, DraftFailure> {
        Err(DraftFailure::NoCredentials)
    }
}

/// Counts only, e.g. `to=2 cc=0 bcc=0 body_chars=153`; with attachments also
/// `attachments=<n> attachment_bytes=<total>` (bytes once the files are read).
pub fn counts(r: &DraftRequest) -> String {
    let files = if r.attachment_names.is_empty() {
        String::new()
    } else {
        format!(
            " attachments={} attachment_bytes={}",
            r.attachment_names.len(),
            draft_files::total_bytes(&r.attachments)
        )
    };
    format!(
        "to={} cc={} bcc={} body_chars={}{}{}",
        r.to.len(),
        r.cc.len(),
        r.bcc.len(),
        r.body.chars().count(),
        if r.reply_thread.is_some() {
            " reply=true"
        } else {
            ""
        },
        files
    )
}

fn bad(msg: &str) -> AgentError {
    AgentError::BadRequest(msg.into())
}

/// Control characters, bidi controls and invisible format characters: text
/// that can make what a person reads differ from what is there.
pub fn invisible_or_control(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{2069}'
                | '\u{FEFF}'
                | '\u{00AD}'
                | '\u{061C}'
                | '\u{2028}'
                | '\u{2029}'
        )
}

fn header_unsafe(c: char) -> bool {
    c.is_control() || c == '\u{2028}' || c == '\u{2029}'
}

/// Bare `local@domain` only: ASCII, no display name, no quoting, no comments.
pub fn valid_address(s: &str) -> bool {
    if s.is_empty() || s.len() > ADDRESS_MAX {
        return false;
    }
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    let local_ok = !local.is_empty()
        && local.len() <= 64
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..")
        && local
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+/=?^_`{|}~.-".contains(c));
    let labels: Vec<&str> = domain.split('.').collect();
    let domain_ok = labels.len() >= 2
        && labels.iter().all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        });
    local_ok && domain_ok
}

pub fn address_list(params: &Value, name: &str, required: bool) -> Result<Vec<String>, AgentError> {
    let arr = match params.get(name) {
        None | Some(Value::Null) if !required => return Ok(Vec::new()),
        Some(Value::Array(a)) => a,
        _ => {
            return Err(AgentError::BadRequest(format!(
                "`{name}` must be an array of email addresses"
            )))
        }
    };
    if required && arr.is_empty() {
        return Err(AgentError::BadRequest(format!(
            "`{name}` needs at least one address"
        )));
    }
    if arr.len() > MAX_RECIPIENTS {
        return Err(bad("too many recipients (50 at most in total)"));
    }
    let mut out = Vec::with_capacity(arr.len());
    for (i, v) in arr.iter().enumerate() {
        match v.as_str() {
            Some(s) if valid_address(s) => out.push(s.to_string()),
            _ => {
                return Err(AgentError::BadRequest(format!(
                    "`{name}` entry {} is not a plain email address (name@domain)",
                    i + 1
                )))
            }
        }
    }
    Ok(out)
}

fn text_param<'a>(params: &'a Value, name: &str) -> Result<&'a str, AgentError> {
    params
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| AgentError::BadRequest(format!("`{name}` is required (a string)")))
}

/// Validate the params of a `draft` request for `account` (already checked
/// against the agent's allow list). Pure: no I/O, no lookups.
pub fn validate(account: &str, params: &Value) -> Result<DraftRequest, AgentError> {
    let obj = params
        .as_object()
        .ok_or_else(|| bad("params must be an object"))?;
    if let Some(k) = obj.keys().find(|k| !ALLOWED_KEYS.contains(&k.as_str())) {
        // The key name is the agent's own input, not mail content.
        let k: String = k.chars().take(40).filter(|c| !header_unsafe(*c)).collect();
        return Err(AgentError::BadRequest(format!("unknown parameter `{k}`")));
    }
    let to = address_list(params, "to", true)?;
    let cc = address_list(params, "cc", false)?;
    let bcc = address_list(params, "bcc", false)?;
    if to.len() + cc.len() + bcc.len() > MAX_RECIPIENTS {
        return Err(bad("too many recipients (50 at most in total)"));
    }
    let subject = text_param(params, "subject")?;
    if subject.chars().count() > SUBJECT_MAX {
        return Err(bad("`subject` is longer than 998 characters"));
    }
    if subject
        .chars()
        .any(|c| header_unsafe(c) || invisible_or_control(c))
    {
        return Err(bad("`subject` must be one line without control characters"));
    }
    let body = text_param(params, "body")?;
    if body.chars().count() > BODY_MAX {
        return Err(bad("`body` is longer than 200000 characters"));
    }
    if body.contains('\0') {
        return Err(bad("`body` must not contain NUL characters"));
    }
    let reply_thread = match params.get("reply_to_thread") {
        None | Some(Value::Null) => None,
        Some(Value::String(t))
            if !t.is_empty()
                && t.chars().count() <= THREAD_ID_MAX
                && !t.chars().any(|c| header_unsafe(c) || c.is_whitespace()) =>
        {
            Some(t.clone())
        }
        Some(_) => return Err(bad("`reply_to_thread` must be a thread id string")),
    };
    let attachment_names = draft_files::names(params)?;
    Ok(DraftRequest {
        account: account.to_string(),
        to,
        cc,
        bcc,
        subject: subject.to_string(),
        body: body.to_string(),
        reply_thread,
        in_reply_to: None,
        attachment_names,
        attachments: Vec::new(),
    })
}

/// A Message-ID taken from stored mail is third-party data: keep it only if it
/// is one short token with no whitespace or control characters.
fn header_safe_id(s: &str) -> Option<String> {
    let s = s.trim();
    let ok = !s.is_empty()
        && s.len() <= 998
        && s.is_ascii()
        && !s.chars().any(|c| c.is_control() || c.is_whitespace());
    ok.then(|| s.to_string())
}

/// Look up the thread a draft replies into. `Ok(None)`: no such thread for the
/// account. `Ok(Some(id))`: found; `id` is the newest message's Message-ID when
/// it is header safe.
pub fn reply_target(
    conn: &Connection,
    account: &str,
    thread: &str,
) -> Result<Option<Option<String>>, String> {
    let row: Option<Option<String>> = conn
        .query_row(
            "SELECT rfc822_message_id FROM messages
              WHERE account_id = ?1 AND thread_id = ?2
              ORDER BY internal_date DESC LIMIT 1",
            params![account, thread],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("reply target: {e}"))?;
    Ok(row.map(|id| id.as_deref().and_then(header_safe_id)))
}

/// The Settings option "Label drafts made by agents" (off by default).
pub fn label_option(conn: &Connection) -> Result<bool, String> {
    Ok(crate::host::host()
        .get_setting(conn, LABEL_OPTION_KEY)?
        .as_deref()
        == Some("true"))
}

pub fn set_label_option(conn: &Connection, on: bool) -> Result<(), String> {
    crate::host::host().set_setting(conn, LABEL_OPTION_KEY, if on { "true" } else { "false" })
}
