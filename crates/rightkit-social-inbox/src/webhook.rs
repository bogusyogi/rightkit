//! Signed platform webhook deliveries (Meta: Instagram, Facebook, Threads): verify the
//! `X-Hub-Signature-256` header over the raw body, decode a base64 body under a size limit, take
//! the app secret out of a call input, and parse a verified body into bounded events.
//!
//! Trust. A body is verified FIRST, over the raw bytes exactly as received and before any JSON parsing:
//! the header must equal `sha256=` plus the lowercase hex HMAC-SHA256 of the body keyed with the app
//! secret, compared in constant time. Everything taken from a verified payload is untrusted data:
//! bounded here, never interpreted.
//!
//! Events. Each `changes[]` item and each `messaging[]` item of each payload entry is one event (an
//! entry with neither is one `other` event), so a batched entry loses nothing; each keeps its raw JSON.
//! A verified body that is not the expected shape is [`Parsed::Unparseable`] with a fixed reason.

use serde_json::Value;

use crate::Fail;

fn bad(msg: impl Into<String>) -> Fail {
    ("bad_input", msg.into())
}

/// Largest raw body accepted, decoded.
pub const MAX_BODY_BYTES: usize = 256 * 1024;
/// Longest app secret accepted, in bytes.
pub const MAX_SECRET_BYTES: usize = 256;
/// Most events one delivery may hold; a verified body with more is stored `unparseable`.
pub const MAX_EVENTS: usize = 2000;
/// Bounds on payload text, in characters. A longer field is cut and the event is `oversize`.
pub const MAX_ID_CHARS: usize = 256;
pub const MAX_TEXT_CHARS: usize = 4096;
pub const MAX_TIME_CHARS: usize = 64;
/// Longest payload `object` string kept on the delivery record.
pub const MAX_OBJECT_CHARS: usize = 64;
/// The prefix of a signature header value.
pub const SIGNATURE_PREFIX: &str = "sha256=";

/// The one refusal for every signature failure; it never says which check failed.
pub fn signature_invalid() -> Fail {
    (
        "signature_invalid",
        "the delivery's signature did not verify; nothing was stored".to_string(),
    )
}

fn body_too_large(max: usize) -> Fail {
    (
        "body_too_large",
        format!("the raw body is over the {max} byte limit; nothing was stored"),
    )
}

// ---------------------------------------------------------------------------------------------------
// Crypto: HMAC-SHA256, the constant-time compare, hex and strict base64 come from rightkit-crypto.
// ---------------------------------------------------------------------------------------------------

/// The header value a genuine delivery of `body` carries under `secret`.
pub fn signature_header_for(secret: &[u8], body: &[u8]) -> String {
    format!(
        "{SIGNATURE_PREFIX}{}",
        rightkit_crypto::hex_encode(&rightkit_crypto::hmac_sha256(secret, body))
    )
}

/// True only when `header` is exactly the genuine header for `body`. One constant-time comparison against
/// the well-formed expected value covers the prefix, the hex digits, the length and the digest at once, so
/// no malformed header can take a different path from a wrong digest. Fails closed.
pub fn verify_signature(secret: &[u8], body: &[u8], header: Option<&str>) -> bool {
    let Some(header) = header else {
        return false;
    };
    rightkit_crypto::ct_eq(
        header.as_bytes(),
        signature_header_for(secret, body).as_bytes(),
    )
}

/// The raw body from its base64, refused as `body_too_large` over `max_bytes` (checked on the encoded
/// length first, so an oversize string is never decoded) or `bad_base64`.
pub fn decode_body(text: &str, max_bytes: usize) -> Result<Vec<u8>, Fail> {
    if text.len() > max_bytes.div_ceil(3) * 4 {
        return Err(body_too_large(max_bytes));
    }
    let body = rightkit_crypto::base64_decode(text).map_err(|_| {
        (
            "bad_base64",
            "raw_body_b64 is not strict standard base64 with padding".to_string(),
        )
    })?;
    if body.len() > max_bytes {
        return Err(body_too_large(max_bytes));
    }
    Ok(body)
}

// ---------------------------------------------------------------------------------------------------
// The secret.
// ---------------------------------------------------------------------------------------------------

/// Removes `app_secret` from the input and returns it. Every refusal here is worded without the value.
pub fn take_secret(input: &mut Value) -> Result<String, Fail> {
    let Some(map) = input.as_object_mut() else {
        return Err(bad("input must be a JSON object"));
    };
    let secret =
        match map.remove("app_secret") {
            Some(Value::String(s)) => s,
            Some(_) => return Err(bad("app_secret must be a string")),
            None => return Err(bad(
                "app_secret is required: the daemon supplies it for this call; it is never stored",
            )),
        };
    if secret.is_empty() || secret.len() > MAX_SECRET_BYTES || secret.chars().any(char::is_control)
    {
        return Err(bad(format!(
            "app_secret must be 1 to {MAX_SECRET_BYTES} bytes with no control characters"
        )));
    }
    Ok(secret)
}

/// Error text with every occurrence of the secret (and its encoded forms) replaced by the owner's known-secret
/// redaction. Belt and braces: no message here carries it.
pub fn scrub(message: String, secret: &str) -> String {
    rightkit_logs::KnownSecrets::from_values([secret]).redact(&message)
}

/// The input a dry run may report: the secret removed and the body replaced by its size.
pub fn dry_run_input(params: &Value) -> Value {
    let mut out = params.clone();
    if let Some(map) = out.as_object_mut() {
        map.remove("app_secret");
        map.remove("refresh_token");
        if let Some(Value::String(body)) = map.get_mut("raw_body_b64") {
            *body = format!("<{} base64 characters>", body.len());
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------------
// Parsing a verified body.
// ---------------------------------------------------------------------------------------------------

/// One event before it is stored.
pub struct NewEvent {
    pub recipient: Option<String>,
    /// The recipient was cut to its bound, so it is never matched to an account.
    pub recipient_cut: bool,
    pub kind: &'static str,
    pub platform_event_id: Option<String>,
    pub sender_id: Option<String>,
    pub text: Option<String>,
    pub event_time: Option<String>,
    pub oversize: bool,
    pub raw_json: String,
}

/// The payload fields of one item, before bounding.
struct Draft {
    id: Option<String>,
    sender: Option<String>,
    text: Option<String>,
    time: Option<String>,
}

/// The result of parsing a verified body.
pub enum Parsed {
    /// The payload's `object` (when short) and its events, in order.
    Events {
        /// The payload's `object` string, dropped when over the bound.
        object: Option<String>,
        /// One event per item.
        events: Vec<NewEvent>,
    },
    /// Not the expected shape: `invalid_json`, `not_object`, `no_entry_array` or `too_many_events`.
    Unparseable(&'static str),
}

/// `value` cut to `max` characters (at a character boundary). `cut` is set when it was cut.
fn bound(value: Option<String>, max: usize, cut: &mut bool) -> Option<String> {
    value.map(|v| match v.char_indices().nth(max) {
        Some((end, _)) => {
            *cut = true;
            v[..end].to_string()
        }
        None => v,
    })
}

/// A string or an integer as text; anything else is absent.
fn scalar(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

fn string(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

fn at<'a>(value: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    value.and_then(|v| v.get(key))
}

fn finish(recipient: &Option<String>, kind: &'static str, draft: Draft, raw: &Value) -> NewEvent {
    let mut recipient_cut = false;
    let mut cut = false;
    let recipient = bound(recipient.clone(), MAX_ID_CHARS, &mut recipient_cut);
    let platform_event_id = bound(draft.id, MAX_ID_CHARS, &mut cut);
    let sender_id = bound(draft.sender, MAX_ID_CHARS, &mut cut);
    let text = bound(draft.text, MAX_TEXT_CHARS, &mut cut);
    let event_time = bound(draft.time, MAX_TIME_CHARS, &mut cut);
    NewEvent {
        recipient,
        recipient_cut,
        kind,
        platform_event_id,
        sender_id,
        text,
        event_time,
        oversize: cut || recipient_cut,
        raw_json: raw.to_string(),
    }
}

/// A `changes[]` item: Instagram `comments`, `live_comments` and `mentions`, Facebook `feed` comments,
/// Threads `replies` and `mentions`. Anything else is `other`.
fn from_change(
    recipient: &Option<String>,
    entry_time: &Option<String>,
    change: &Value,
) -> NewEvent {
    let field = change.get("field").and_then(Value::as_str);
    let value = change.get("value");
    let item = at(value, "item").and_then(Value::as_str);
    let kind = match field {
        Some("comments" | "live_comments" | "replies") => "comment",
        Some("mentions" | "mention") => "mention",
        Some("feed") if item == Some("comment") => "comment",
        _ => "other",
    };
    let draft = Draft {
        id: scalar(at(value, "id")).or_else(|| scalar(at(value, "comment_id"))),
        sender: scalar(at(at(value, "from"), "id"))
            .or_else(|| scalar(at(at(value, "sender"), "id"))),
        text: string(at(value, "text")).or_else(|| string(at(value, "message"))),
        time: scalar(at(value, "created_time")).or_else(|| entry_time.clone()),
    };
    finish(recipient, kind, draft, change)
}

/// A `messaging[]` item. It is a `message` when it carries a `message` object; reads, deliveries,
/// reactions and postbacks are `other`.
fn from_message(recipient: &Option<String>, entry_time: &Option<String>, item: &Value) -> NewEvent {
    let message = item.get("message");
    let kind = if message.is_some_and(Value::is_object) {
        "message"
    } else {
        "other"
    };
    let draft = Draft {
        id: scalar(at(message, "mid")),
        sender: scalar(at(item.get("sender"), "id")),
        text: string(at(message, "text")),
        time: scalar(item.get("timestamp")).or_else(|| entry_time.clone()),
    };
    finish(recipient, kind, draft, item)
}

fn expand_entry(entry: &Value, out: &mut Vec<NewEvent>) {
    let recipient = scalar(entry.get("id"));
    let entry_time = scalar(entry.get("time"));
    let before = out.len();
    if let Some(changes) = entry.get("changes").and_then(Value::as_array) {
        for change in changes {
            out.push(from_change(&recipient, &entry_time, change));
        }
    }
    if let Some(items) = entry.get("messaging").and_then(Value::as_array) {
        for item in items {
            out.push(from_message(&recipient, &entry_time, item));
        }
    }
    if out.len() == before {
        let draft = Draft {
            id: None,
            sender: None,
            text: None,
            time: entry_time,
        };
        out.push(finish(&recipient, "other", draft, entry));
    }
}

/// Parses a VERIFIED body. A shape it does not recognise is `Unparseable` with a fixed reason.
pub fn parse_delivery(body: &[u8]) -> Parsed {
    let Ok(root) = serde_json::from_slice::<Value>(body) else {
        return Parsed::Unparseable("invalid_json");
    };
    let Some(map) = root.as_object() else {
        return Parsed::Unparseable("not_object");
    };
    let Some(entries) = map.get("entry").and_then(Value::as_array) else {
        return Parsed::Unparseable("no_entry_array");
    };
    let object = map
        .get("object")
        .and_then(Value::as_str)
        .filter(|s| s.chars().count() <= MAX_OBJECT_CHARS)
        .map(str::to_string);
    let mut events = Vec::new();
    for entry in entries {
        expand_entry(entry, &mut events);
        if events.len() > MAX_EVENTS {
            return Parsed::Unparseable("too_many_events");
        }
    }
    Parsed::Events { object, events }
}

/// Seconds since the epoch of a stored event time: epoch seconds (or milliseconds) as digits, a UTC instant, or a
/// Graph `created_time` with a zero offset. Anything else, including fractional seconds, is `None`: an unreadable
/// time is never guessed.
pub fn event_secs(raw: &str) -> Option<i64> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    if t.bytes().all(|b| b.is_ascii_digit()) {
        let n: i64 = t.parse().ok()?;
        return Some(if n > 100_000_000_000 { n / 1000 } else { n });
    }
    if let Some(secs) = parse_instant(t) {
        return Some(secs);
    }
    let base = t
        .strip_suffix("+0000")
        .or_else(|| t.strip_suffix("+00:00"))?;
    parse_instant(&format!("{base}Z"))
}

/// Parse a UTC instant `YYYY-MM-DDTHH:MM:SSZ` or `YYYY-MM-DDTHH:MMZ` into seconds since the epoch
/// (the calendar's rule, [`rightkit_time::zones::parse_instant`]).
fn parse_instant(raw: &str) -> Option<i64> {
    rightkit_time::zones::parse_instant(raw, "event_time").ok()
}
