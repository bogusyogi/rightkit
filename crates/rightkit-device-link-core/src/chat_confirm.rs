//! Phone confirmation of a reserved action a server or a guest asked for through chat (#503), and
//! structured operation approvals (#479).
//!
//! A cloud guest and a server are full chat members, but destruction, spend, publication,
//! production changes, settings and device management they ask for need the owner's phone first. The
//! receiving agent asks the engine to call [`ChatConfirms::submit`], which holds one request for ten
//! minutes. A paired phone the owner switched on for `admin` lists it, asks for a challenge, and
//! answers with a Secure Enclave signature over `admin_sig::confirm_message`, or denies. The agent
//! polls [`ChatConfirms::status`] and receives approval once, then performs the described action
//! through its own tools.
//!
//! The sender's role is read from the host's registry (`PairedDevice::member_role`), never from
//! the message. Requests live in memory: a restart voids them and the poll then answers `unknown`,
//! which the agent treats as a refusal. The audit line (`audit_file` beside the registry) and any
//! event carry ids and decision codes, never the summary.
//!
//! The digest domain tags and the audit file name are product values given in [`ConfirmConfig`]; the
//! phone recomputes the digests with the same tags.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use rightkit_owner_approvals::PhoneError;
use rightkit_owner_approvals::admin_sig::{self, Nonces};

use crate::registry::MemberRole;

/// How long a request waits for the phone before it expires (the phone is pull-only).
pub const CONFIRM_WAIT_MS: u64 = 10 * 60 * 1000;
/// The longest summary kept, in characters; the phone shows it whole.
pub const SUMMARY_MAX: usize = 16_384;
const NAME_MAX: usize = 80;
const CHAT_MAX: usize = 120;
const PENDING_MAX: usize = 8;
/// A settled request stays readable this long (the agent's poll, the audit).
const KEEP_MS: u64 = 10 * 60 * 1000;

/// Product values a host passes in: the domain tags the phone also uses and the audit file name.
#[derive(Clone, Copy, Debug)]
pub struct ConfirmConfig {
    /// First field of the payload hash, so it is a digest of this and nothing else.
    pub payload_tag: &'static str,
    /// First field of the operation hash.
    pub operation_tag: &'static str,
    /// File name of the audit lines, created in the root given to [`ChatConfirms::new`].
    pub audit_file: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Pending,
    Approved,
    Denied,
    Expired,
    /// An approval consumed by the receiving agent: it cannot be used again.
    Consumed,
}

struct Entry {
    id: String,
    message_id: String,
    sender_device_id: String,
    sender_pairing: String,
    sender_name: String,
    sender_role: MemberRole,
    chat: String,
    summary: String,
    requested_ms: u64,
    expires_ms: u64,
    payload_hash: String,
    state: State,
    settled_ms: Option<u64>,
    approved_by: Option<(String, String, String)>,
    /// An `approvals.*` request (the structured operation is `summary`); false for a chat confirm.
    approval: bool,
    /// [`operation_hash`] of `summary` for an approval; empty for a chat confirm.
    op_hash: String,
}

/// What a caller asks the phone to confirm. Names and ids come from this computer's own records.
pub struct Submission {
    pub message_id: String,
    pub sender_device_id: String,
    pub sender_pairing: String,
    pub sender_name: String,
    pub sender_role: MemberRole,
    pub chat: String,
    pub summary: String,
}

pub struct ChatConfirms {
    entries: Mutex<Vec<Entry>>,
    audit_path: PathBuf,
    config: ConfirmConfig,
}

/// The digest the phone approves, bound to one request: the payload tag, the request id, the
/// message id, the sender's device id, name and role word, the chat, the summary and the request
/// time in ms (decimal text), each as an 8-byte little-endian length then its UTF-8 bytes. Lowercase
/// hex of SHA-256. The phone recomputes it from what it shows before it signs.
#[allow(clippy::too_many_arguments)]
pub fn payload_hash(
    tag: &str,
    id: &str,
    message_id: &str,
    sender_device_id: &str,
    sender_name: &str,
    role: MemberRole,
    chat: &str,
    summary: &str,
    requested_ms: u64,
) -> String {
    let requested = requested_ms.to_string();
    let mut ctx = rightkit_fs::digest::Sha256Hasher::new();
    for part in [
        tag,
        id,
        message_id,
        sender_device_id,
        sender_name,
        role.as_str(),
        chat,
        summary,
        requested.as_str(),
    ] {
        ctx.update(&(part.len() as u64).to_le_bytes());
        ctx.update(part.as_bytes());
    }
    ctx.finalize()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// `text` cut to `max` characters, control characters other than newline and tab replaced by a space
/// so a name cannot fake a line on the phone.
fn clipped(text: &str, max: usize, keep_lines: bool) -> String {
    let visible: String = text.chars().map(|c| {
        if matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
            format!("\\u{{{:04x}}}", c as u32)
        } else { c.to_string() }
    }).collect();
    visible
        .chars()
        .map(|c| {
            if c.is_control() && !(keep_lines && (c == '\n' || c == '\t')) {
                ' '
            } else {
                c
            }
        })
        .take(max)
        .collect()
}

/// Id prefix of an `approvals.*` request; chat confirmations use `cc-`.
pub const APPROVAL_ID_PREFIX: &str = "ap-";
const APPROVAL_CHAT: &str = "approvals";
/// Longest canonical operation text.
pub const OPERATION_MAX: usize = 16 * 1024;
/// An attachment is named, never carried: this is the largest one the phone is asked to fetch.
pub const ATTACHMENT_MAX: u64 = 64 * 1024 * 1024;
/// Parameter names that would carry a secret; an operation names where a secret is kept, never it.
const SECRET_WORDS: [&str; 11] = [
    "password",
    "passwd",
    "passphrase",
    "secret",
    "token",
    "apikey",
    "api_key",
    "credential",
    "cookie",
    "authorization",
    "privatekey",
];

/// SHA-256 over the operation tag and the canonical operation text, each as an 8-byte little-endian
/// length then its bytes. Lowercase hex. The requester's own digest of what it will run.
pub fn operation_hash(tag: &str, canonical: &str) -> String {
    let mut ctx = rightkit_fs::digest::Sha256Hasher::new();
    for part in [tag, canonical] {
        ctx.update(&(part.len() as u64).to_le_bytes());
        ctx.update(part.as_bytes());
    }
    ctx.finalize()
}

fn text_ok(text: &str, max: usize, lines: bool) -> bool {
    !text.trim().is_empty()
        && text.chars().count() <= max
        && !text.chars().any(|c| {
            (c.is_control() && !(lines && (c == '\n' || c == '\t')))
                || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
}

fn looks_secret(value: &str) -> bool {
    value.contains("-----BEGIN")
        || ["sk-", "ghp_", "github_pat_", "xoxb-", "AKIA"]
            .iter()
            .any(|p| value.starts_with(p) && value.len() > 20 && !value.contains(' '))
}

fn quoted(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_default()
}

fn bad(message: &str) -> PhoneError {
    PhoneError::new("invalid_operation", message)
}

fn str_field<'a>(
    obj: &'a serde_json::Map<String, Value>,
    key: &str,
    max: usize,
    required: bool,
) -> Result<Option<&'a str>, PhoneError> {
    match obj.get(key) {
        None if !required => Ok(None),
        Some(Value::String(s)) if text_ok(s, max, false) => Ok(Some(s)),
        _ => Err(bad(&format!(
            "`{key}` is {}text of at most {max} characters without control characters",
            if required { "" } else { "optional " }
        ))),
    }
}

fn only_keys(
    obj: &serde_json::Map<String, Value>,
    allowed: &[&str],
    what: &str,
) -> Result<(), PhoneError> {
    match obj.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => Err(bad(&format!("{what} has no field `{k}`"))),
        None => Ok(()),
    }
}

/// The exact operation an owner approves, checked and written as canonical JSON (fixed key order,
/// no spaces, integers only). Shape: `app`, `action`, `target {computer, deviceId?}`, optional
/// `destination`, `params` (name to text, integer or bool), optional `attachment {ref, name, mime,
/// size, sha256}`. No other field is accepted: no inline bytes, no secret-named parameter.
pub fn validate_operation(op: &Value) -> Result<String, PhoneError> {
    let obj = op
        .as_object()
        .ok_or_else(|| bad("the operation is an object"))?;
    only_keys(
        obj,
        &[
            "app",
            "action",
            "target",
            "destination",
            "params",
            "attachment",
        ],
        "the operation",
    )?;
    let app = str_field(obj, "app", 80, true)?.unwrap_or_default();
    let action = str_field(obj, "action", 120, true)?.unwrap_or_default();
    let target = obj
        .get("target")
        .and_then(Value::as_object)
        .ok_or_else(|| bad("`target` names the computer the action runs on"))?;
    only_keys(target, &["computer", "deviceId"], "`target`")?;
    let computer = str_field(target, "computer", 120, true)?.unwrap_or_default();
    let device = str_field(target, "deviceId", 128, false)?;
    let destination = str_field(obj, "destination", 500, false)?;
    let mut out = format!("{{\"action\":{},\"app\":{}", quoted(action), quoted(app));
    if let Some(att) = obj.get("attachment") {
        let att = att
            .as_object()
            .ok_or_else(|| bad("`attachment` is an object"))?;
        only_keys(
            att,
            &["ref", "name", "mime", "size", "sha256"],
            "`attachment`",
        )?;
        let reference = str_field(att, "ref", 256, true)?.unwrap_or_default();
        let name = str_field(att, "name", 120, true)?.unwrap_or_default();
        let mime = str_field(att, "mime", 100, true)?.unwrap_or_default();
        let size = att
            .get("size")
            .and_then(Value::as_u64)
            .filter(|n| (1..=ATTACHMENT_MAX).contains(n))
            .ok_or_else(|| bad("attachment `size` is 1 to 67108864 bytes"))?;
        let sha = att
            .get("sha256")
            .and_then(Value::as_str)
            .filter(|h| h.len() == 64 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
            .ok_or_else(|| bad("attachment `sha256` is 64 lowercase hex characters"))?;
        out.push_str(&format!(
            ",\"attachment\":{{\"mime\":{},\"name\":{},\"ref\":{},\"sha256\":\"{sha}\",\"size\":{size}}}",
            quoted(mime), quoted(name), quoted(reference)
        ));
    }
    if let Some(d) = destination {
        out.push_str(&format!(",\"destination\":{}", quoted(d)));
    }
    let mut params: Vec<(&String, &Value)> = match obj.get("params") {
        None => Vec::new(),
        Some(Value::Object(map)) => map.iter().collect(),
        Some(_) => return Err(bad("`params` is an object")),
    };
    if params.len() > 32 {
        return Err(bad("`params` has at most 32 entries"));
    }
    params.sort_by(|a, b| a.0.cmp(b.0));
    out.push_str(",\"params\":{");
    for (i, (key, value)) in params.iter().enumerate() {
        let lower = key.to_ascii_lowercase();
        if key.is_empty()
            || key.len() > 64
            || !key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
        {
            return Err(bad(
                "a parameter name is letters, digits, _ . - (at most 64)",
            ));
        }
        if SECRET_WORDS.iter().any(|w| lower.contains(w)) {
            return Err(bad(
                "an operation never carries a secret; name where it is kept instead",
            ));
        }
        let rendered = match value {
            Value::String(s) if text_ok(s, 2000, true) && !looks_secret(s) => quoted(s),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) if n.is_i64() || n.is_u64() => n.to_string(),
            _ => {
                return Err(bad(
                    "a parameter is text (at most 2000 characters, no secret), an integer or a bool",
                ));
            }
        };
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!("{}:{rendered}", quoted(key)));
    }
    out.push('}');
    out.push_str(&format!(",\"target\":{{\"computer\":{}", quoted(computer)));
    if let Some(d) = device {
        out.push_str(&format!(",\"deviceId\":{}", quoted(d)));
    }
    out.push_str("}}");
    if out.len() > OPERATION_MAX {
        return Err(bad("the operation is too large (16384 bytes at most)"));
    }
    Ok(out)
}

fn message_id_ok(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

impl ChatConfirms {
    /// `root` is the device-link folder; the audit file lives there.
    pub fn new(root: &Path, config: ConfirmConfig) -> ChatConfirms {
        ChatConfirms {
            entries: Mutex::new(Vec::new()),
            audit_path: root.join(config.audit_file),
            config,
        }
    }

    fn audit(&self, entry: &Entry, phone_id: &str, key: &str, outcome: &str, code: Option<&str>) {
        admin_sig::audit(
            &self.audit_path,
            &json!({
                "ts": now_ms(), "requestId": entry.id, "messageId": entry.message_id,
                "senderDeviceId": entry.sender_device_id, "role": entry.sender_role.as_str(),
                "phoneDeviceId": phone_id, "keyFingerprint": key,
                "payloadHash": entry.payload_hash, "outcome": outcome, "code": code,
                "kind": if entry.approval { "approval" } else { "chat" },
            }),
        );
    }

    /// Expire waiting requests and drop settled ones nobody read in time.
    fn sweep(&self, entries: &mut Vec<Entry>) {
        let now = now_ms();
        for entry in entries.iter_mut() {
            if matches!(entry.state, State::Pending | State::Approved) && now >= entry.expires_ms {
                entry.state = State::Expired;
                entry.settled_ms = Some(now);
                self.audit(entry, "", "", "expired", None);
            }
        }
        entries.retain(|e| {
            e.settled_ms
                .is_none_or(|at| now.saturating_sub(at) < KEEP_MS)
        });
    }

    /// Queue one request for the phone, or return the one already open for the same message from the
    /// same sender. Returns `{id, expiresMs}`.
    pub fn submit(&self, request: Submission) -> Result<Value, PhoneError> {
        self.submit_inner(request, false)
    }

    fn submit_inner(&self, request: Submission, approval: bool) -> Result<Value, PhoneError> {
        if !message_id_ok(&request.message_id) {
            return Err(PhoneError::new(
                "invalid_request",
                "the message id is letters, digits and - (at most 64)",
            ));
        }
        if request.sender_device_id.is_empty() || request.sender_device_id.len() > 128 {
            return Err(PhoneError::new(
                "invalid_request",
                "the sender's device id is missing",
            ));
        }
        if !request.sender_role.needs_phone() {
            return Err(PhoneError::new(
                "not_required",
                "the sender is one of the owner's computers",
            ));
        }
        if request.summary.trim().is_empty() || request.summary.chars().count() > SUMMARY_MAX {
            return Err(PhoneError::new(
                "invalid_request",
                "an exact action descriptor is required",
            ));
        }
        let summary = clipped(request.summary.trim(), SUMMARY_MAX, true);
        let sender_name = clipped(request.sender_name.trim(), NAME_MAX, false);
        let chat = clipped(request.chat.trim(), CHAT_MAX, false);
        let mut entries = lock(&self.entries);
        self.sweep(&mut entries);
        if let Some(open) = entries.iter().find(|e| {
            e.message_id == request.message_id
                && e.sender_device_id == request.sender_device_id
                && matches!(e.state, State::Pending | State::Approved)
        }) {
            if open.sender_pairing != request.sender_pairing
                || open.summary != summary
                || open.chat != chat
                || open.sender_role != request.sender_role
            {
                return Err(PhoneError::new(
                    "request_conflict",
                    "this message already has a different action waiting; finish or decline it first",
                ));
            }
            return Ok(json!({ "id": open.id, "expiresMs": open.expires_ms }));
        }
        let waiting = entries.iter().filter(|e| e.state == State::Pending).count();
        if waiting >= PENDING_MAX {
            return Err(PhoneError::retry(
                "busy",
                "too many confirmations are waiting for the phone",
            ));
        }
        let mut raw = [0u8; 8];
        rightkit_crypto::random_fill(&mut raw)
            .map_err(|_| PhoneError::new("rng_failed", "no random bytes for a request id"))?;
        let id = format!(
            "{}{}",
            if approval { APPROVAL_ID_PREFIX } else { "cc-" },
            rightkit_fs::digest::hex(&raw)
        );
        let requested_ms = now_ms();
        let entry = Entry {
            payload_hash: payload_hash(
                self.config.payload_tag,
                &id,
                &request.message_id,
                &request.sender_device_id,
                &sender_name,
                request.sender_role,
                &chat,
                &summary,
                requested_ms,
            ),
            id: id.clone(),
            message_id: request.message_id,
            sender_device_id: request.sender_device_id,
            sender_pairing: request.sender_pairing,
            sender_name,
            sender_role: request.sender_role,
            chat,
            requested_ms,
            expires_ms: requested_ms + CONFIRM_WAIT_MS,
            state: State::Pending,
            settled_ms: None,
            approved_by: None,
            op_hash: if approval {
                operation_hash(self.config.operation_tag, &summary)
            } else {
                String::new()
            },
            approval,
            summary,
        };
        let expires = entry.expires_ms;
        self.audit(&entry, "", "", "requested", None);
        entries.push(entry);
        Ok(json!({ "id": id, "expiresMs": expires }))
    }

    /// The agent's poll: `pending`, `approved` (once), `denied`, `expired`, or `unknown` (no such
    /// request, an approval already used, or an engine restart in between). Never carries content.
    pub fn status(
        &self,
        id: &str,
        active: &dyn Fn(&str, &str) -> bool,
        approval_valid: &dyn Fn(&str, &str, &str) -> bool,
    ) -> Value {
        self.status_for(id, false, None, active, approval_valid)
    }

    fn status_for(
        &self,
        id: &str,
        approval: bool,
        op_hash: Option<&str>,
        active: &dyn Fn(&str, &str) -> bool,
        approval_valid: &dyn Fn(&str, &str, &str) -> bool,
    ) -> Value {
        let mut entries = lock(&self.entries);
        self.sweep(&mut entries);
        let Some(entry) = entries
            .iter_mut()
            .find(|e| e.id == id && e.approval == approval)
        else {
            return json!({ "id": id, "state": "unknown" });
        };
        // An approval is spent only for the exact operation the requester is about to run: a
        // different operation voids it, so what the owner approved is never swapped afterwards.
        if approval
            && entry.state == State::Approved
            && !op_hash
                .is_some_and(|h| rightkit_crypto::ct_eq(h.as_bytes(), entry.op_hash.as_bytes()))
        {
            entry.state = State::Denied;
            entry.settled_ms = Some(now_ms());
            self.audit(entry, "", "", "voided", Some("operation_changed"));
            return json!({ "id": id, "state": "denied", "code": "operation_changed" });
        }
        // Resolve the stored sender, never a caller-selected identity. Check at consumption.
        if matches!(entry.state, State::Pending | State::Approved)
            && (!active(&entry.sender_device_id, &entry.sender_pairing)
                || entry
                    .approved_by
                    .as_ref()
                    .is_some_and(|(phone, pairing, key)| {
                        !active(phone, pairing) || !approval_valid(phone, pairing, key)
                    }))
        {
            entry.state = State::Denied;
            entry.settled_ms = Some(now_ms());
            self.audit(entry, "", "", "voided", Some("sender_inactive"));
        }
        let state = match entry.state {
            State::Pending => "pending",
            State::Approved => {
                entry.state = State::Consumed;
                self.audit(entry, "", "", "consumed", None);
                "approved"
            }
            State::Denied => "denied",
            State::Expired => "expired",
            State::Consumed => "unknown",
        };
        json!({ "id": id, "state": state, "messageId": entry.message_id })
    }

    /// Read-only reconciliation for a phone after an answer was lost. Never spends an approval.
    pub fn snapshot(&self, id: &str) -> Value {
        let mut entries = lock(&self.entries);
        self.sweep(&mut entries);
        let state = entries
            .iter()
            .find(|e| e.id == id)
            .map(|e| match e.state {
                State::Pending => "pending",
                State::Approved | State::Consumed => "approved",
                State::Denied => "denied",
                State::Expired => "expired",
            })
            .unwrap_or("unknown");
        json!({ "id": id, "state": state })
    }

    /// The waiting requests as the phone lists them (`admin.pending`, `confirms`).
    pub fn pending_rows(&self) -> Vec<Value> {
        let mut entries = lock(&self.entries);
        self.sweep(&mut entries);
        entries
            .iter()
            .filter(|e| e.state == State::Pending && !e.approval)
            .map(|e| {
                json!({
                    "id": e.id,
                    "messageId": e.message_id,
                    "senderName": e.sender_name,
                    "senderDeviceId": e.sender_device_id,
                    "senderRole": e.sender_role.as_str(),
                    "chat": e.chat,
                    "summary": e.summary,
                    "requestedMs": e.requested_ms,
                    "expiresMs": e.expires_ms,
                    "payloadHash": e.payload_hash,
                })
            })
            .collect()
    }

    /// The waiting `approvals.*` requests as the phone lists them (`approvals.pending`). `operation`
    /// is the canonical JSON text the digest binds; the phone shows its fields from that exact text.
    pub fn approval_rows(&self) -> Vec<Value> {
        let mut entries = lock(&self.entries);
        self.sweep(&mut entries);
        entries
            .iter()
            .filter(|e| e.state == State::Pending && e.approval)
            .map(|e| {
                json!({
                    "id": e.id,
                    "requestId": e.message_id,
                    "requester": e.sender_name,
                    "operation": e.summary,
                    "operationHash": e.op_hash,
                    "requestedMs": e.requested_ms,
                    "expiresMs": e.expires_ms,
                    "payloadHash": e.payload_hash,
                })
            })
            .collect()
    }

    /// Queue one structured operation for the phone ([`validate_operation`] first). `request_id`
    /// is the requester's idempotency key: the same key and operation return the open request.
    /// Returns `{id, expiresMs, operationHash, payloadHash}`.
    pub fn submit_approval(
        &self,
        requester_device_id: &str,
        requester_name: &str,
        request_id: &str,
        operation: &Value,
    ) -> Result<Value, PhoneError> {
        let canonical = validate_operation(operation)?;
        let queued = self.submit_inner(
            Submission {
                message_id: request_id.to_string(),
                sender_device_id: requester_device_id.to_string(),
                sender_pairing: "local".to_string(),
                sender_name: requester_name.to_string(),
                sender_role: MemberRole::Unclassified,
                chat: APPROVAL_CHAT.to_string(),
                summary: canonical.clone(),
            },
            true,
        )?;
        let id = queued["id"].as_str().unwrap_or_default().to_string();
        let payload = lock(&self.entries)
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.payload_hash.clone())
            .unwrap_or_default();
        Ok(json!({
            "id": id, "expiresMs": queued["expiresMs"],
            "operationHash": operation_hash(self.config.operation_tag, &canonical), "payloadHash": payload,
        }))
    }

    /// The requester's poll: `approved` is returned once and only when `operation` (what it is
    /// about to run) canonicalises to the operation the owner approved; any difference voids the
    /// approval (`denied`, code `operation_changed`). Never carries content.
    pub fn approval_status(
        &self,
        id: &str,
        operation: &Value,
        active: &dyn Fn(&str, &str) -> bool,
        approval_valid: &dyn Fn(&str, &str, &str) -> bool,
    ) -> Result<Value, PhoneError> {
        let hash = operation_hash(self.config.operation_tag, &validate_operation(operation)?);
        Ok(self.status_for(id, true, Some(&hash), active, approval_valid))
    }

    /// Whether `id` waits for a phone (so a challenge may be issued for it).
    pub fn is_pending(&self, id: &str) -> bool {
        let mut entries = lock(&self.entries);
        self.sweep(&mut entries);
        entries
            .iter()
            .any(|e| e.id == id && e.state == State::Pending)
    }

    /// The phone's signed approval. The request must still wait, `payload_hash` must be the digest
    /// this computer holds (the phone signed what it was shown), and the signature must verify
    /// (`admin_sig::check_confirm`, which spends the nonce whatever the outcome). One use.
    #[allow(clippy::too_many_arguments)]
    pub fn approve(
        &self,
        root: &Path,
        nonces: &Nonces,
        computer_id: &str,
        phone_id: &str,
        phone_pairing: &str,
        id: &str,
        shown_hash: &str,
        params: &Value,
    ) -> Result<(), PhoneError> {
        let mut entries = lock(&self.entries);
        self.sweep(&mut entries);
        let entry = entries
            .iter_mut()
            .find(|e| e.id == id && e.state == State::Pending)
            .ok_or_else(|| {
                PhoneError::new("not_found", "that request is gone, answered or expired")
            })?;
        if !rightkit_crypto::ct_eq(shown_hash.as_bytes(), entry.payload_hash.as_bytes()) {
            return Err(PhoneError::new(
                "hash_mismatch",
                "the request is not the one shown",
            ));
        }
        // The digest again from the fields held here, not the stored one.
        let again = payload_hash(
            self.config.payload_tag,
            &entry.id,
            &entry.message_id,
            &entry.sender_device_id,
            &entry.sender_name,
            entry.sender_role,
            &entry.chat,
            &entry.summary,
            entry.requested_ms,
        );
        if !rightkit_crypto::ct_eq(again.as_bytes(), entry.payload_hash.as_bytes()) {
            return Err(PhoneError::new(
                "hash_mismatch",
                "the request is not the one shown",
            ));
        }
        let key = match admin_sig::check_confirm(
            root,
            nonces,
            phone_id,
            computer_id,
            &entry.id,
            &entry.payload_hash,
            params,
            now_ms(),
        ) {
            Ok(key) => key,
            Err(error) => {
                let entry = &*entry;
                self.audit(entry, phone_id, "", "refused", Some(&error.code));
                return Err(error);
            }
        };
        entry.approved_by = Some((phone_id.to_string(), phone_pairing.to_string(), key.clone()));
        entry.state = State::Approved;
        entry.settled_ms = Some(now_ms());
        let entry = &*entry;
        self.audit(
            entry,
            phone_id,
            &admin_sig::fingerprint(&key),
            "approved",
            None,
        );
        Ok(())
    }

    /// The phone's decline: a waiting request ends as denied.
    pub fn deny(&self, phone_id: &str, id: &str) -> Result<(), PhoneError> {
        let mut entries = lock(&self.entries);
        self.sweep(&mut entries);
        let entry = entries
            .iter_mut()
            .find(|e| e.id == id && e.state == State::Pending)
            .ok_or_else(|| {
                PhoneError::new("not_found", "that request is gone, answered or expired")
            })?;
        entry.state = State::Denied;
        entry.settled_ms = Some(now_ms());
        let entry = &*entry;
        self.audit(entry, phone_id, "", "denied", None);
        Ok(())
    }

    /// Void every waiting request from `device_id` (its pairing was revoked).
    pub fn void_sender(&self, device_id: &str) {
        let mut entries = lock(&self.entries);
        let now = now_ms();
        for entry in entries.iter_mut() {
            if matches!(entry.state, State::Pending | State::Approved)
                && entry.sender_device_id == device_id
            {
                entry.state = State::Denied;
                entry.settled_ms = Some(now);
                self.audit(entry, "", "", "voided", Some("sender_revoked"));
            }
        }
    }
}
