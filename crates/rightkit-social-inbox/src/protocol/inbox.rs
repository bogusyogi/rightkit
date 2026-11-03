//! Payload shapes for the signed webhook inbox. A delivery is the raw body a
//! platform POSTed to the daemon, accepted only after its `X-Hub-Signature-256` HMAC verified. Each
//! verified delivery yields inbox events (a comment, a message, a mention, or `other`). Everything in an
//! event that came from the payload is untrusted data: it is stored as given, bounded, and never
//! interpreted. Nothing here replies to anyone.
//!
//! The app secret is never a field of these shapes. The daemon supplies it as `app_secret` in the call
//! input; the engine removes it before it parses the rest, never stores it and never returns it.
use serde::{Deserialize, Serialize};
use serde_json::Value;

use rightkit_social_publish::protocol::Scope;

/// Platforms whose signed webhooks the inbox accepts.
pub const INBOX_PLATFORMS: [&str; 3] = ["instagram", "facebook", "threads"];

/// Event kinds. Anything the inbox does not recognise is `other`.
pub const INBOX_KINDS: [&str; 4] = ["comment", "message", "mention", "other"];

/// Event states. A new event is `open`.
pub const INBOX_STATES: [&str; 3] = ["open", "handled", "ignored"];

/// `inbox.receive` input (besides the secret, which the engine takes out of the raw input first).
/// `raw_body_b64` is the exact request body, standard base64 with padding. `signature_header` is the
/// `X-Hub-Signature-256` header value; absent is refused as `signature_invalid`.
#[derive(Debug, Clone, Deserialize)]
pub struct InboxReceiveInput {
    pub platform: String,
    pub raw_body_b64: String,
    #[serde(default)]
    pub signature_header: Option<String>,
    pub received_at: String,
}

/// A scope filter. `account` is optional: omitted means every account of the channel.
#[derive(Debug, Clone, Deserialize)]
pub struct InboxScopeFilter {
    pub venture: String,
    pub channel: String,
    #[serde(default)]
    pub account: Option<String>,
}

/// `inbox.list` input. Exactly one of `scope` or `unmatched: true`. Newest first by arrival. `before` is
/// an event id: only events that arrived earlier are returned.
#[derive(Debug, Clone, Deserialize)]
pub struct InboxListInput {
    #[serde(default)]
    pub scope: Option<InboxScopeFilter>,
    #[serde(default)]
    pub unmatched: bool,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub before: Option<String>,
}

/// `inbox.get` input. `scope`, when given, must match the event's scope (`foreign_scope` otherwise).
#[derive(Debug, Clone, Deserialize)]
pub struct InboxGetInput {
    pub event_id: String,
    #[serde(default)]
    pub scope: Option<InboxScopeFilter>,
}

/// `inbox.set_state` input. `by` must be a person (`adrian`).
#[derive(Debug, Clone, Deserialize)]
pub struct InboxSetStateInput {
    pub event_id: String,
    pub state: String,
    #[serde(default)]
    pub note: String,
    pub by: String,
    #[serde(default)]
    pub now: Option<String>,
    #[serde(default)]
    pub scope: Option<InboxScopeFilter>,
}

/// One stored delivery. Holds no payload content: the hash, the size and the outcome of parsing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxDelivery {
    /// Lowercase hex SHA-256 of the raw body: the idempotency key.
    pub body_sha256: String,
    pub platform: String,
    /// UTC instant, `YYYY-MM-DDTHH:MM:SSZ`.
    pub received_at: String,
    pub body_bytes: i64,
    /// `parsed` or `unparseable`.
    pub status: String,
    /// Why a delivery is `unparseable`: `invalid_json`, `not_object`, `no_entry_array`,
    /// `too_many_events`. Null when parsed.
    pub reason: Option<String>,
    /// The payload's `object` string when it is at most 64 characters; null otherwise.
    pub object: Option<String>,
    pub event_count: i64,
}

/// One inbox event. `scope` is null with `unmatched_account` true when the recipient account id matched no
/// stored account (or matched more than one). `recipient_id`, `platform_event_id`, `sender_id`, `text`
/// and `event_time` are the payload's values, UNTRUSTED. `oversize` is true when any of them was cut to its
/// bound; the raw JSON keeps the whole item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxEvent {
    pub event_id: String,
    pub delivery: String,
    pub seq: i64,
    pub platform: String,
    pub scope: Option<Scope>,
    pub unmatched_account: bool,
    /// `no_account`, `ambiguous_account`, or null when matched.
    pub unmatched_reason: Option<String>,
    pub recipient_id: Option<String>,
    pub kind: String,
    pub platform_event_id: Option<String>,
    pub sender_id: Option<String>,
    pub text: Option<String>,
    pub event_time: Option<String>,
    pub oversize: bool,
    pub received_at: String,
    pub state: String,
    pub state_by: Option<String>,
    pub state_at: Option<String>,
    pub state_note: String,
}

/// `inbox.receive` output. `duplicate` is true when the same raw bytes were already stored; the stored
/// delivery and events are returned and nothing is written.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxReceiveOutput {
    pub duplicate: bool,
    pub delivery: InboxDelivery,
    pub events: Vec<InboxEvent>,
}

/// `inbox.list` output. `next_before` is the event id to pass as `before` for the next page, or null.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxListOutput {
    pub events: Vec<InboxEvent>,
    pub next_before: Option<String>,
}

/// `inbox.get` output: the event and the raw JSON of the item it came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxGetOutput {
    pub event: InboxEvent,
    pub raw: Value,
}

/// `inbox.set_state` output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxSetStateOutput {
    pub event: InboxEvent,
}

/// One row of the event counts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxStatsGroup {
    pub scope: Option<Scope>,
    pub kind: String,
    pub state: String,
    pub count: i64,
}

/// Deliveries refused for a bad signature, by platform and UTC day. No payload content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxRefusal {
    pub platform: String,
    pub day: String,
    pub count: i64,
}

/// `inbox.stats` output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxStatsOutput {
    pub total_events: i64,
    pub unmatched_events: i64,
    pub groups: Vec<InboxStatsGroup>,
    pub deliveries_parsed: i64,
    pub deliveries_unparseable: i64,
    pub refused_total: i64,
    pub refused: Vec<InboxRefusal>,
}
