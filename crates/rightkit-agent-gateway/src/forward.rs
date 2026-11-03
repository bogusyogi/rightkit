//! The `forward` method: input validation, resolving the original message from
//! the local database, and composing the forward draft's subject and plain-text
//! body. Pure apart from the lookups; the creator builds the Gmail draft (with
//! the original attachments) through [`super::draft::DraftCreator::forward`].
//!
//! A forward discloses existing mail to new recipients, so it never skips the
//! user's card: the request only creates a draft and a pending approval.
//! Nothing here records recipients, subject or body anywhere; [`counts`] is the
//! only text derived from a request that reaches the audit log.

use super::attachment::{self, AttachmentRequest};
use super::draft::{address_list, invisible_or_control, MAX_RECIPIENTS, SUBJECT_MAX};
use super::outcome::AgentError;
use rightkit_db::rusqlite::{params, Connection, OptionalExtension};
use rightkit_fs::digest::Sha256Hasher;
use serde_json::Value;

/// Longest note the agent may put above the forwarded message.
pub const NOTE_MAX: usize = 10_000;
/// Longest original body a forward quotes. A longer one is refused rather than
/// cut, so a forwarded message is never silently shortened.
pub const ORIGINAL_BODY_MAX: usize = 400_000;
/// Most attachments one forward carries.
pub const MAX_ATTACHMENTS: usize = 20;
/// Largest total attachment size a forward carries, before encoding.
pub const ATTACHMENT_TOTAL_MAX: i64 = 18 * 1024 * 1024;
const ID_MAX: usize = 200;
const ALLOWED_KEYS: [&str; 6] = ["account", "message_id", "to", "cc", "bcc", "note"];

/// A validated request, before the original message is looked up.
#[derive(Clone, PartialEq, Eq)]
pub struct ForwardQuery {
    pub account: String,
    pub message_id: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub note: String,
}

/// A forward ready to be built in Gmail: final subject and body, and the
/// original message's attachments to include.
#[derive(Clone, PartialEq, Eq)]
pub struct ForwardRequest {
    pub account: String,
    pub message_id: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub body: String,
    pub note_chars: usize,
    pub attachments: Vec<AttachmentRequest>,
}

/// Content must never reach a log through `{:?}`.
impl std::fmt::Debug for ForwardRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForwardRequest")
            .field("counts", &counts(self))
            .finish_non_exhaustive()
    }
}

/// The draft the creator made, and the hash of its content as Gmail holds it.
/// The hash is what the card later compares to tell whether the draft was
/// edited after the app built it. `attachment_digest` is one SHA-256 over the
/// bytes of the attachments the app put in the draft (see [`attachments_digest`])
/// and `attachment_bytes` their total size; both are stored with the draft
/// record and never shown to the agent. The digest is not compared at approve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardCreated {
    pub draft_id: String,
    pub hash: String,
    pub attachment_digest: String,
    pub attachment_bytes: i64,
}

fn feed(h: &mut Sha256Hasher, s: &str) {
    h.update(&(s.len() as u64).to_be_bytes());
    h.update(s.as_bytes());
}

/// What a forward is about, for the decline cooldown: a hash of the original
/// message id and the set of recipients (to, cc and bcc together, lower-cased,
/// order and repeats ignored). No address is stored, only the hash.
pub fn target_key(r: &ForwardRequest) -> String {
    let mut all: Vec<String> =
        r.to.iter()
            .chain(&r.cc)
            .chain(&r.bcc)
            .map(|a| a.trim().to_ascii_lowercase())
            .collect();
    all.sort();
    all.dedup();
    let mut h = Sha256Hasher::new();
    feed(&mut h, "forward");
    feed(&mut h, &r.message_id);
    for a in &all {
        feed(&mut h, a);
    }
    h.finalize()
}

/// One digest over the attachments' bytes, from each file's own SHA-256, in
/// the order they were attached. Needs nothing beyond the bytes already fetched.
pub fn attachments_digest(per_file: &[[u8; 32]]) -> String {
    let mut h = Sha256Hasher::new();
    h.update(&(per_file.len() as u64).to_be_bytes());
    for d in per_file {
        h.update(d);
    }
    h.finalize()
}

/// A MIME type safe to put in a header: a plain `type/subtype` token (ASCII
/// letters, digits and `!#$&^_.+-`, exactly one `/`, at most 127 characters).
/// Anything else becomes `application/octet-stream`.
pub fn safe_mime(m: &str) -> String {
    let token = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "!#$&^_.+-".contains(c))
    };
    if m.len() <= 127 && m.matches('/').count() == 1 && m.split('/').all(token) {
        m.to_string()
    } else {
        "application/octet-stream".to_string()
    }
}

/// A file name safe inside a quoted header string: control, bidi and invisible
/// characters, quotes and backslashes become `_`, at most 200 characters; a
/// blank name becomes `attachment`.
pub fn safe_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if invisible_or_control(c) || c == '"' || c == '\\' {
                '_'
            } else {
                c
            }
        })
        .take(200)
        .collect();
    if cleaned.trim().is_empty() {
        "attachment".to_string()
    } else {
        cleaned
    }
}

/// Counts only, e.g. `to=2 cc=0 bcc=0 note_chars=40 attachments=1`.
pub fn counts(r: &ForwardRequest) -> String {
    format!(
        "to={} cc={} bcc={} note_chars={} attachments={}",
        r.to.len(),
        r.cc.len(),
        r.bcc.len(),
        r.note_chars,
        r.attachments.len()
    )
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

/// Validate the params of a `forward` request for `account` (already checked
/// against the agent's allow list). Pure: no I/O, no lookups.
pub fn validate(account: &str, params: &Value) -> Result<ForwardQuery, AgentError> {
    let obj = params
        .as_object()
        .ok_or_else(|| bad("params must be an object"))?;
    if let Some(k) = obj.keys().find(|k| !ALLOWED_KEYS.contains(&k.as_str())) {
        let k: String = k.chars().take(40).filter(|c| !c.is_control()).collect();
        return Err(AgentError::BadRequest(format!("unknown parameter `{k}`")));
    }
    let message_id = params
        .get("message_id")
        .and_then(Value::as_str)
        .filter(|s| id_ok(s))
        .ok_or_else(|| bad("`message_id` is required (a message id string)"))?;
    let to = address_list(params, "to", true)?;
    let cc = address_list(params, "cc", false)?;
    let bcc = address_list(params, "bcc", false)?;
    if to.len() + cc.len() + bcc.len() > MAX_RECIPIENTS {
        return Err(bad("too many recipients (50 at most in total)"));
    }
    let note = match params.get("note") {
        None | Some(Value::Null) => "",
        Some(Value::String(n)) => n.as_str(),
        Some(_) => return Err(bad("`note` must be a string")),
    };
    if note.chars().count() > NOTE_MAX {
        return Err(bad("`note` is longer than 10000 characters"));
    }
    if note.contains('\0') {
        return Err(bad("`note` must not contain NUL characters"));
    }
    Ok(ForwardQuery {
        account: account.to_string(),
        message_id: message_id.to_string(),
        to,
        cc,
        bcc,
        note: note.to_string(),
    })
}

/// One line of third-party text for the quote header block or the subject:
/// control, bidi and invisible characters become spaces, so nothing can break
/// the header block or hide inside it.
fn one_line(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if invisible_or_control(c) || c.is_whitespace() {
                ' '
            } else {
                c
            }
        })
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The stored recipient columns are either a JSON array of strings or plain text.
fn display_addrs(raw: &str) -> String {
    let t = raw.trim();
    if t.starts_with('[') {
        if let Ok(list) = serde_json::from_str::<Vec<String>>(t) {
            return one_line(&list.join(", "));
        }
    }
    one_line(t)
}

/// "Fwd: ..." unless the subject already starts with a forward prefix.
pub fn forward_subject(original: &str) -> String {
    let s = one_line(original);
    let lower = s.to_ascii_lowercase();
    let out = if lower.starts_with("fwd:") || lower.starts_with("fw:") {
        s
    } else if s.is_empty() {
        "Fwd:".to_string()
    } else {
        format!("Fwd: {s}")
    };
    out.chars().take(SUBJECT_MAX).collect()
}

/// The original message as the local database holds it.
struct Original {
    from: String,
    to: String,
    cc: String,
    subject: String,
    date_ms: i64,
    has_attachments: bool,
    body: String,
}

fn sender(name: &str, addr: &str) -> String {
    let (name, addr) = (one_line(name), one_line(addr));
    match (name.is_empty(), addr.is_empty()) {
        (false, false) => format!("{name} <{addr}>"),
        (false, true) => name,
        _ => addr,
    }
}

fn load(conn: &Connection, q: &ForwardQuery) -> Result<Original, AgentError> {
    let db = |e: String| super::outcome::from_db_error(&e);
    type Row = (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
        i64,
        Option<String>,
    );
    let row: Option<Row> = conn
        .query_row(
            "SELECT from_name, from_addr, to_addrs, cc_addrs, subject, internal_date,
                    has_attachments, snippet
               FROM messages WHERE account_id = ?1 AND id = ?2",
            params![q.account, q.message_id],
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
        )
        .optional()
        .map_err(|e| db(format!("message lookup: {e}")))?;
    let Some((name, addr, to, cc, subject, date_ms, has, snippet)) = row else {
        return Err(bad("the message was not found in this account"));
    };
    let body_of = |mime: &str| -> Result<Option<String>, AgentError> {
        conn.query_row(
            "SELECT body FROM message_bodies
              WHERE account_id = ?1 AND message_id = ?2 AND mime_type = ?3",
            params![q.account, q.message_id, mime],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()
        .map(Option::flatten)
        .map_err(|e| db(format!("body lookup: {e}")))
    };
    let body = match body_of("text/plain")? {
        Some(t) if !t.trim().is_empty() => t,
        _ => match body_of("text/html")? {
            Some(h) => crate::host::host().html_to_text(&h),
            None => snippet.unwrap_or_default(),
        },
    };
    Ok(Original {
        from: sender(
            name.as_deref().unwrap_or_default(),
            addr.as_deref().unwrap_or_default(),
        ),
        to: display_addrs(to.as_deref().unwrap_or_default()),
        cc: display_addrs(cc.as_deref().unwrap_or_default()),
        subject: subject.unwrap_or_default(),
        date_ms,
        has_attachments: has != 0,
        body: body.replace('\0', "").replace("\r\n", "\n"),
    })
}

fn format_date(ms: i64) -> String {
    // The owner's IMF-fixdate is `Sun, 06 Nov 1994 08:49:37 GMT`; the first 22 bytes are
    // `Sun, 06 Nov 1994 08:49`, the shape this header line has always had.
    let http = rightkit_time::format_http_date(ms.div_euclid(1000));
    http.get(..22)
        .map(|head| format!("{head} UTC"))
        .unwrap_or_default()
}

/// The forward's plain-text body: the agent's note on top, then a standard
/// forwarded-message header block, then the original text.
pub fn compose_body(
    note: &str,
    o_from: &str,
    date: &str,
    subject: &str,
    to: &str,
    cc: &str,
    text: &str,
) -> String {
    let mut out = String::new();
    let note = note.replace("\r\n", "\n");
    if !note.trim().is_empty() {
        out.push_str(note.trim_end());
        out.push_str("\n\n");
    }
    out.push_str("---------- Forwarded message ---------\n");
    out.push_str(&format!("From: {o_from}\n"));
    out.push_str(&format!("Date: {date}\n"));
    out.push_str(&format!("Subject: {subject}\n"));
    out.push_str(&format!("To: {to}\n"));
    if !cc.is_empty() {
        out.push_str(&format!("Cc: {cc}\n"));
    }
    out.push('\n');
    out.push_str(text.trim_end());
    out.push('\n');
    out
}

/// Look up the original message and its attachments and compose the forward.
/// The message must exist locally for the account. Reads only.
pub fn resolve(conn: &Connection, q: ForwardQuery) -> Result<ForwardRequest, AgentError> {
    let o = load(conn, &q)?;
    if o.body.chars().count() > ORIGINAL_BODY_MAX {
        return Err(bad("that message is too long to forward"));
    }
    let db = |e: String| super::outcome::from_db_error(&e);
    let listed = attachment::list_for_message(conn, &q.account, &q.message_id).map_err(db)?;
    if o.has_attachments && listed.is_empty() {
        return Err(bad(
            "that message's attachments are not synced yet; try again after the next sync",
        ));
    }
    if listed.len() > MAX_ATTACHMENTS {
        return Err(bad("that message has too many attachments to forward"));
    }
    if listed.iter().map(|a| a.size.max(0)).sum::<i64>() > ATTACHMENT_TOTAL_MAX {
        return Err(bad("that message's attachments are too large to forward"));
    }
    let attachments = listed
        .into_iter()
        .map(|a| AttachmentRequest {
            account: q.account.clone(),
            message_id: q.message_id.clone(),
            attachment_id: a.attachment_id,
            name: a.name,
            mime: a.mime,
            size: a.size,
        })
        .collect();
    let body = compose_body(
        &q.note,
        &o.from,
        &format_date(o.date_ms),
        &one_line(&o.subject),
        &o.to,
        &o.cc,
        &o.body,
    );
    Ok(ForwardRequest {
        account: q.account,
        message_id: q.message_id,
        to: q.to,
        cc: q.cc,
        bcc: q.bcc,
        subject: forward_subject(&o.subject),
        body,
        note_chars: q.note.chars().count(),
        attachments,
    })
}
