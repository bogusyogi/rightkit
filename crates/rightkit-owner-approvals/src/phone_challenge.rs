//! What the owner's phone signs, and the checks on what it sends.
//!
//! The phone answers an owner's approval by signing the exact bytes of a digest it rebuilt from what it
//! shows (`admin_sig`, one tag for each kind so a signature for one cannot answer another). This module
//! holds the digests the host computes again from its own stored item, so a request that changed since
//! the phone looked produces another digest and the signature fails, together with the request-id names
//! the challenges are kept under, the idempotency key of a phone's acting request, and the strict
//! parameter checks. It signs and verifies nothing itself: the signature check, the single-use nonce and
//! the key lookup are in [`crate::admin_sig`]; the host calls them with these digests.
//!
//! Parameter errors are returned as the plain message of a bad-input refusal.

use serde_json::{Value, json};

use crate::admin_sig::RELAY_TTL_MS;

/// Longest request typed in the thread, in bytes (what `agent.act` accepts).
pub const REQUEST_MAX: usize = 4096;

/// Longest plain id, in bytes.
pub const ID_MAX: usize = 128;

/// Longest client message id, in bytes.
pub const CLIENT_ID_MAX: usize = 64;

fn sha_hex(text: &str) -> String {
    rightkit_crypto::hex_encode(&rightkit_crypto::sha256(text.as_bytes()))
}

/// A plain id: 1 to 128 characters of letters, digits and `. _ : @ # = -` (the Inbox's rule).
pub fn id_param(params: &Value, key: &str) -> Result<String, String> {
    match params.get(key).and_then(Value::as_str) {
        Some(id)
            if !id.is_empty()
                && id.len() <= ID_MAX
                && id.chars().all(|c| {
                    c.is_ascii_alphanumeric()
                        || matches!(c, '.' | '_' | ':' | '@' | '#' | '=' | '-')
                }) =>
        {
            Ok(id.to_string())
        }
        Some(_) => Err(format!("param `{key}` is not a plain id")),
        None => Err(format!("missing param `{key}`")),
    }
}

/// `clientMsgId`: 1 to 64 letters, digits, `-` or `_`.
pub fn client_param(params: &Value) -> Result<&str, String> {
    params
        .get("clientMsgId")
        .and_then(Value::as_str)
        .filter(|id| {
            !id.is_empty()
                && id.len() <= CLIENT_ID_MAX
                && id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
        .ok_or_else(|| "`clientMsgId` must be 1 to 64 letters, digits, - or _".to_string())
}

/// `request`: the text typed, trimmed, 1 to [`REQUEST_MAX`] bytes.
pub fn request_param(params: &Value) -> Result<&str, String> {
    params
        .get("request")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|r| !r.is_empty() && r.len() <= REQUEST_MAX)
        .ok_or_else(|| format!("`request` must be 1 to {REQUEST_MAX} bytes"))
}

// ---- the names a challenge is kept under ------------------------------------------------------------------

/// The request id of a held request's challenge.
pub fn held_request_id(id: &str) -> String {
    format!("held:{id}")
}

/// The request id of a schedule confirmation's challenge.
pub fn schedule_request_id(id: &str) -> String {
    format!("schedule:{id}")
}

/// The request id of a standing instruction's approval challenge.
pub fn rule_request_id(id: &str) -> String {
    format!("rule:{id}")
}

/// The request id of an acting request typed in the phone's own thread.
pub fn act_request_id(client_id: &str) -> String {
    format!("phone-act:{client_id}")
}

/// The request id of an acting request relayed to another computer (the same on the computer that
/// accepts the phone's request and on the target).
pub fn relay_request_id(client_id: &str) -> String {
    format!("phone-relay:{client_id}")
}

/// The idempotency key of a phone's acting request; `own_act_key` tells a device's own tasks by its prefix.
pub fn act_key(device_id: &str, client_id: &str) -> String {
    format!("phone-act:{device_id}:{client_id}")
}

/// Whether `key` is an idempotency key this device's acting requests carry.
pub fn own_act_key(key: &str, device_id: &str) -> bool {
    key.starts_with(&act_key(device_id, ""))
}

// ---- the digests ------------------------------------------------------------------------------------------

/// What the phone's signature covers for a schedule: the id, the label, the misfire policy, the run times the
/// engine will use (Unix seconds, comma-separated) and the engine's own digest of the schedule. The phone
/// rebuilds it from what it shows.
pub fn schedule_digest(
    id: &str,
    label: &str,
    misfire: &str,
    runs: &[i64],
    engine_digest: &str,
) -> String {
    let runs: Vec<String> = runs.iter().map(|t| t.to_string()).collect();
    sha_hex(&format!(
        "pulse.schedule-confirm.v1\nid:{id}\nlabel:{label}\nmisfire:{misfire}\nruns:{}\nengine:{engine_digest}",
        runs.join(","),
    ))
}

/// The rule as the owner reads it: the engine's own sentence when the record has one, else the typed parts as
/// compact text (a record from an older engine). The digest covers exactly this text, so what the owner reads
/// is what the signature binds.
pub fn rule_summary(
    sentence: Option<&str>,
    trigger: &Value,
    condition: &Value,
    action: &Value,
) -> String {
    if let Some(sentence) = sentence {
        return sentence.to_string();
    }
    json!({
        "trigger": trigger,
        "condition": condition,
        "action": action,
    })
    .to_string()
}

/// What the phone's signature covers for a standing instruction: the id, the revision and the exact summary text.
pub fn rule_digest(id: &str, revision: u64, summary: &str) -> String {
    sha_hex(&format!(
        "pulse.rule-approve.v1\nid:{id}\nrevision:{revision}\nsummary:{summary}",
    ))
}

/// What the signature covers for an acting request typed in the thread: the client id and the exact text typed.
pub fn act_digest(client_id: &str, request: &str) -> String {
    sha_hex(&format!(
        "pulse.phone-act.v1\nclient:{client_id}\nrequest:{request}"
    ))
}

// ---- relay ------------------------------------------------------------------------------------------------

/// What the phone signed for a relayed request (nonce, key id, signature) and the challenge's expiry, cut to
/// what a challenge can last (`RELAY_TTL_MS` from `now_ms`), as the task body carries it to the target.
pub fn relay_signed(params: &Value, now_ms: u64) -> Value {
    json!({
        "nonce": params.get("nonce").cloned().unwrap_or(Value::Null),
        "keyId": params.get("keyId").cloned().unwrap_or(Value::Null),
        "sig": params.get("sig").cloned().unwrap_or(Value::Null),
        "expiresMs": params
            .get("expiresMs")
            .and_then(Value::as_u64)
            .map_or(0, |at| at.min(now_ms.saturating_add(RELAY_TTL_MS))),
    })
}

/// Whether a computer's reported capability words include one that needs a desktop (`screen_observe`,
/// `screen_act`, `browser`, `typed_fill`): a computer with none of them is not offered as a place to run a
/// request from the phone.
pub fn offers_desktop(words: &Value) -> bool {
    const DESKTOP_WORDS: [&str; 4] = ["screen_observe", "screen_act", "browser", "typed_fill"];
    words.as_array().is_some_and(|words| {
        words
            .iter()
            .filter_map(Value::as_str)
            .any(|w| DESKTOP_WORDS.contains(&w))
    })
}
