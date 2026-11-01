//! The Activity record of remote calls: one JSON line per call in `remote-activity.jsonl` in
//! the state folder, which an Activity page reads.
//!
//! A line holds the time, the client (a label the caller gave), the origin (`local` or
//! `gateway`, set by the process that made the call: go by this, not the client, to tell the
//! gateway's calls apart), the tool, the SHAPE of
//! the arguments and the outcome:
//! the approved folder's index and the file's extension for a file call, the action kind, tier and
//! outcome of a screen call (never an app, window title, element text or typed value), the length of a search
//! (never the search text), the kind and first eight characters of a chat id, and the number of
//! bytes or items returned. Never a path, a file's content, a search term, a turn's text or
//! anything else the caller read. that is the rule.
//!
//! The file is shared by every process that makes a call (the CLI, the engine) and written with
//! one append per line (`rightkit_logs::JsonlStore`, owner-only), so a second process never
//! interleaves inside a line and none needs the engine's single-instance claim. The newest file
//! is kept under `MAX_BYTES`; when it is larger it becomes `remote-activity.jsonl.1` (the
//! previous one is dropped) and a new file starts, under the store's cross-process rotation lock.
//! A reader tolerates a torn final line.

use rightkit_logs::JsonlStore;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub const FILE: &str = "remote-activity.jsonl";
/// Size at which the log rolls over to `<file>.1`.
const MAX_BYTES: u64 = 1024 * 1024;

fn path() -> Option<PathBuf> {
    rightkit_app_host::state_directory()
        .ok()
        .map(|d| d.join(FILE))
}

fn store(path: PathBuf) -> JsonlStore {
    JsonlStore::new(path, MAX_BYTES)
        .with_owner_only(true)
        .with_cross_process_lock()
}

/// The security audit: approval decisions, PIN checks and owner-only settings changes, one JSON
/// line each, in `security-audit.jsonl` beside the Activity file. Unlike Activity (1 MiB, one
/// older file) these events are rare, so the cap is 8 MiB per file with one older file kept.
/// The engine's approval record lives only in memory, so this file is the only trace of who
/// approved what.
pub const SECURITY_FILE: &str = "security-audit.jsonl";
const SECURITY_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Appends one security event. `event` and every string in `fields` are fixed codes written at the
/// call site (kind, door, outcome, a refusal code, a protected setting's key): never a summary,
/// bind, value, PIN or anything the caller supplied. Characters outside `[A-Za-z0-9._:-]` are
/// dropped and each string is cut to 64 characters. Best effort.
pub fn record_security(event: &str, fields: Value) {
    let Some(path) = rightkit_app_host::state_directory()
        .ok()
        .map(|d| d.join(SECURITY_FILE))
    else {
        return;
    };
    let clean = |text: &str| -> String {
        text.chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
            .take(64)
            .collect()
    };
    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Refusals can repeat once per request from one peer; keep at most one line a minute per
    // event, device and reason, carrying how many were held back, so a flood cannot rotate the
    // rare approval, PIN and revoke lines out of the file.
    let held = if event.ends_with("refused") || event == "resident.peer_rejected" {
        let key = format!(
            "{event}|{}|{}",
            fields["device"].as_str().unwrap_or(""),
            fields["reason"].as_str().unwrap_or("")
        );
        match refusal_gate(&key, at) {
            Some(held) => held,
            None => return,
        }
    } else {
        0
    };
    let mut record = json!({ "v": 1, "at": at, "event": clean(event) });
    if held > 0
        && let Some(out) = record.as_object_mut()
    {
        out.insert("suppressed".to_string(), json!(held));
    }
    if let (Some(out), Some(extra)) = (record.as_object_mut(), fields.as_object()) {
        for (name, value) in extra {
            let value = match value {
                Value::String(text) => json!(clean(text)),
                Value::Bool(_) | Value::Number(_) => value.clone(),
                _ => continue,
            };
            out.insert(clean(name), value);
        }
    }
    let _ = JsonlStore::new(path, SECURITY_MAX_BYTES)
        .with_owner_only(true)
        .with_cross_process_lock()
        .append_value(&record);
}

/// Seconds between two written refusal lines with the same event, device and reason.
const REFUSAL_EVERY_SECS: u64 = 60;
/// Most distinct refusal keys remembered; the oldest is dropped past it.
const REFUSAL_KEYS: usize = 256;

/// `Some(held)` when a refusal line for `key` may be written at `now` (with the count held back
/// since the last one), `None` while it is held back.
fn refusal_gate(key: &str, now: u64) -> Option<u64> {
    use std::sync::Mutex;
    static GATE: Mutex<Vec<(String, u64, u64)>> = Mutex::new(Vec::new());
    let mut gate = GATE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(entry) = gate.iter_mut().find(|(k, _, _)| k == key) {
        if now.saturating_sub(entry.1) < REFUSAL_EVERY_SECS {
            entry.2 = entry.2.saturating_add(1);
            return None;
        }
        let held = entry.2;
        entry.1 = now;
        entry.2 = 0;
        return Some(held);
    }
    if gate.len() >= REFUSAL_KEYS {
        gate.remove(0);
    }
    gate.push((key.to_string(), now, 0));
    Some(0)
}

/// A device id cut to its first eight characters (the hub log's form) for a security event field.
/// Only characters `record_security` keeps survive, so nothing else can ride along.
pub fn short_id(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(8)
        .collect()
}

/// Appends one finished-call record to the Activity file, owner-only. Best effort: failures are
/// ignored. The gateway builds the record from its call; this module owns the file.
pub fn append_activity(record: &Value) {
    let Some(path) = path() else { return };
    let _ = store(path).append_value(record);
}

/// The record of one saved-login or saved-card step (`agent_secrets.rs`): the tool, the client's
/// short name, the site host the host app itself verified, the field's NAME, the outcome and, for a
/// payment, the amount in cents and the card's last four digits. the owner asked for amount and
/// merchant in the audit (docs/agent-credentials-payments.md); never a value, a label of the
/// saved item or anything the agent wrote. Best effort.
pub fn record_secret(
    client_key: &str,
    tool: &str,
    host: &str,
    field: &str,
    outcome: &str,
    amount_cents: Option<u64>,
    last4: Option<&str>,
) {
    let Some(path) = path() else { return };
    let kind: String = client_key
        .split(':')
        .next()
        .unwrap_or("")
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(16)
        .collect();
    let short: String = client_key
        .split_once(':')
        .map(|(_, id)| id)
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(8)
        .collect();
    let clean = |text: &str, max: usize| -> String {
        text.chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
            .take(max)
            .collect()
    };
    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut record = json!({
        "v": 1,
        "at": at,
        "client": "engine",
        "origin": "local",
        "tool": clean(tool, 32),
        "chatKind": kind,
        "chatId": short,
        "host": clean(host, 253),
        "field": clean(field, 16),
        "outcome": clean(outcome, 32),
    });
    if let Some(fields) = record.as_object_mut() {
        if let Some(cents) = amount_cents {
            fields.insert("amountCents".into(), json!(cents));
        }
        if let Some(last4) = last4 {
            let digits: String = last4.chars().filter(char::is_ascii_digit).take(4).collect();
            fields.insert("cardLast4".into(), json!(digits));
        }
    }
    let _ = store(path).append_value(&record);
}

/// A client name made safe: letters, digits, `.`, `_`, `-`, at most 32; `unknown` otherwise.
pub fn client(name: &str) -> String {
    let clean: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(32)
        .collect();
    if clean.is_empty() {
        "unknown".into()
    } else {
        clean
    }
}

/// The extension of a requested path: lower case, letters and digits, at most 16; empty when
/// there is none or it is odd.
pub fn extension(path: &str) -> String {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    if ext.len() <= 16 && ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        ext.to_ascii_lowercase()
    } else {
        String::new()
    }
}
