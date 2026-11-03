//! The signed webhook inbox over a host-implemented [`InboxStore`]. The daemon receives a platform's
//! webhook POST and hands the exact raw body, the `X-Hub-Signature-256` header value and the app secret to
//! `inbox.receive`. This module makes NO network call, listens on nothing, replies to nobody and automates
//! nothing: it verifies, stores and lists.
//!
//! Trust. A body is verified FIRST, over the raw bytes exactly as received and before any JSON parsing:
//! the header must equal `sha256=` plus the lowercase hex HMAC-SHA256 of the body keyed with the app
//! secret, compared in constant time. A missing header, a wrong prefix, non-hex, a wrong length and a
//! mismatch are all the one refusal `signature_invalid`, and nothing says which check failed. A refused
//! body is never stored; only a counter by platform and UTC day moves. Everything taken from a verified
//! payload (ids, text, times, the raw item) is untrusted data: stored verbatim and bounded, never
//! interpreted, never put in a path or a query string, never logged.
//!
//! The secret. `app_secret` is a field of the call input. It is removed from the input before anything else
//! is parsed, held for the call, never written to the store or the log, never in a result, and scrubbed from
//! any error text.
//!
//! Idempotency. A delivery is keyed by the SHA-256 of its raw body. The same bytes again return the stored
//! delivery and events with `duplicate: true` and write nothing.
//!
//! Events. Each `changes[]` item and each `messaging[]` item of each payload entry is one event row (an
//! entry with neither is one `other` row), so a batched entry loses nothing. The row keeps the raw JSON of
//! that item. The recipient is matched to the accounts store by platform and exact handle; no match, or
//! more than one scope, stores the event with `scope: null` and `unmatched_account: true`. A verified body
//! that is not the expected shape is one `unparseable` delivery with no events, never an error (an error
//! would make the platform retry forever).

use std::collections::HashMap;

use rightkit_social_publish::calendar_time as time;
use rightkit_social_publish::util::{bad, in_txn, parse_input, WriteTxn};
use serde_json::Value;

use crate::protocol::{
    InboxDelivery, InboxEvent, InboxGetInput, InboxGetOutput, InboxListInput, InboxListOutput,
    InboxReceiveInput, InboxReceiveOutput, InboxRefusal, InboxScopeFilter, InboxSetStateInput,
    InboxSetStateOutput, InboxStatsGroup, InboxStatsOutput, Scope, INBOX_KINDS, INBOX_PLATFORMS,
    INBOX_STATES,
};
use crate::webhook::{
    decode_body, parse_delivery, scrub, signature_invalid, take_secret, verify_signature, Parsed,
    MAX_BODY_BYTES,
};
use crate::Fail;

/// Methods that write. The host's dry-run branch reports these with [`crate::webhook::dry_run_input`],
/// never the raw input.
pub const WRITE_METHODS: &[&str] = &["inbox.receive", "inbox.set_state"];

/// Every method this module routes.
pub const METHODS: &[&str] = &[
    "inbox.receive",
    "inbox.list",
    "inbox.get",
    "inbox.set_state",
    "inbox.stats",
];

/// The longest note on an event state.
const MAX_NOTE_CHARS: usize = 500;
const DEFAULT_LIMIT: u32 = 50;
const MAX_LIMIT: u32 = 200;

/// A delivery row to store.
#[derive(Debug, Clone)]
pub struct NewDelivery<'a> {
    pub body_sha256: &'a str,
    pub platform: &'a str,
    pub received_at: &'a str,
    pub body_bytes: i64,
    /// `parsed` or `unparseable`.
    pub status: &'a str,
    pub reason: Option<&'a str>,
    pub object: Option<&'a str>,
    pub event_count: i64,
}

/// An event row to store. A matched event carries the scope; an unmatched one carries the reason.
#[derive(Debug, Clone)]
pub struct NewStoredEvent<'a> {
    pub event_id: &'a str,
    pub body_sha256: &'a str,
    pub seq: i64,
    pub platform: &'a str,
    pub scope: Option<&'a Scope>,
    pub unmatched_account: bool,
    pub unmatched_reason: Option<&'a str>,
    pub recipient_id: Option<&'a str>,
    pub kind: &'a str,
    pub platform_event_id: Option<&'a str>,
    pub sender_id: Option<&'a str>,
    pub text: Option<&'a str>,
    pub event_time: Option<&'a str>,
    pub oversize: bool,
    pub raw_json: &'a str,
    pub received_at: &'a str,
}

/// The filter of one `inbox.list` page. With no `venture` the page is the unmatched events.
#[derive(Debug, Clone)]
pub struct EventQuery<'a> {
    pub venture: Option<&'a str>,
    pub channel: Option<&'a str>,
    pub account: Option<&'a str>,
    pub kind: Option<&'a str>,
    pub state: Option<&'a str>,
    /// Only events stored before this event (newest first order).
    pub before: Option<&'a str>,
    pub limit: i64,
}

/// What the host app provides: the delivery and event tables, the refusal counter and the accounts lookup.
/// Every method is one read or one write on the host's own database, on the caller's transaction.
pub trait InboxStore: WriteTxn {
    /// `role_forbidden` unless the host runs in the role that may write the inbox.
    fn require_role(&self) -> Result<(), Fail>;
    /// The stored delivery with this body hash, if any.
    fn load_delivery(&self, body_sha256: &str) -> Result<Option<InboxDelivery>, Fail>;
    /// The events of one delivery, in `seq` order.
    fn delivery_events(&self, body_sha256: &str) -> Result<Vec<InboxEvent>, Fail>;
    /// One event by id.
    fn load_event(&self, event_id: &str) -> Result<Option<InboxEvent>, Fail>;
    /// The raw JSON text of one event's item.
    fn event_raw_json(&self, event_id: &str) -> Result<String, Fail>;
    /// Stores one delivery row.
    fn insert_delivery(&self, delivery: &NewDelivery<'_>) -> Result<(), Fail>;
    /// Stores one event row (state `open`).
    fn insert_event(&self, event: &NewStoredEvent<'_>) -> Result<(), Fail>;
    /// Whether an event of this platform, kind, platform event id and recipient is already stored.
    fn platform_event_seen(
        &self,
        platform: &str,
        kind: &str,
        platform_event_id: &str,
        recipient: Option<&str>,
    ) -> Result<bool, Fail>;
    /// Up to two scopes of stored accounts with this platform and handle (two means ambiguous).
    fn accounts_with_handle(&self, platform: &str, handle: &str) -> Result<Vec<Scope>, Fail>;
    /// Adds one to the refusal counter of `(platform, day)`.
    fn bump_refusal(&self, platform: &str, day: &str) -> Result<(), Fail>;
    /// One page of events, newest arrival first.
    fn list_events(&self, query: &EventQuery<'_>) -> Result<Vec<InboxEvent>, Fail>;
    /// Sets an event's state, who set it, when and the note.
    fn set_event_state(
        &self,
        event_id: &str,
        state: &str,
        by: &str,
        stamp: &str,
        note: &str,
    ) -> Result<(), Fail>;
    /// `(total events, unmatched events)`.
    fn event_totals(&self) -> Result<(i64, i64), Fail>;
    /// Event counts by scope, kind and state, ordered by scope, kind, state.
    fn event_groups(&self) -> Result<Vec<InboxStatsGroup>, Fail>;
    /// Deliveries with this status (`parsed` or `unparseable`).
    fn delivery_count(&self, status: &str) -> Result<i64, Fail>;
    /// The refusal counters, ordered by day then platform.
    fn refusals(&self) -> Result<Vec<InboxRefusal>, Fail>;
}

fn encoded(value: serde_json::Result<Value>) -> Result<Value, Fail> {
    value.map_err(|e| ("internal", e.to_string()))
}

fn event_not_found() -> Fail {
    ("not_found", "no inbox event with that id".to_string())
}

/// `foreign_scope` unless the filter names a stored channel (and the account, when given).
fn require_channel(store: &dyn InboxStore, scope: &InboxScopeFilter) -> Result<(), Fail> {
    if !store.channel_known(&scope.venture, &scope.channel, scope.account.as_deref())? {
        return Err((
            "foreign_scope",
            format!(
                "no stored channel for venture={} channel={}{}",
                scope.venture,
                scope.channel,
                scope
                    .account
                    .as_deref()
                    .map(|a| format!(" account={a}"))
                    .unwrap_or_default()
            ),
        ));
    }
    Ok(())
}

/// `foreign_scope` unless the event sits in the scope the caller named (when it named one).
fn event_in_scope(event: &InboxEvent, scope: Option<&InboxScopeFilter>) -> Result<(), Fail> {
    let Some(filter) = scope else {
        return Ok(());
    };
    let inside = event.scope.as_ref().is_some_and(|s| {
        s.venture == filter.venture
            && s.channel == filter.channel
            && filter.account.as_ref().is_none_or(|a| *a == s.account)
    });
    if inside {
        Ok(())
    } else {
        Err((
            "foreign_scope",
            "the event is not in the scope named".to_string(),
        ))
    }
}

enum Matched {
    Scope(Scope),
    Unmatched(&'static str),
}

/// The scope that owns the recipient account id on this platform: exactly one stored account with that
/// platform and handle. Zero or several is unmatched, never a guess. Results are cached per delivery.
fn match_account(
    store: &dyn InboxStore,
    platform: &str,
    recipient: Option<&str>,
    recipient_cut: bool,
    cache: &mut HashMap<String, Option<Scope>>,
) -> Result<Matched, Fail> {
    let Some(handle) = recipient.filter(|_| !recipient_cut) else {
        return Ok(Matched::Unmatched("no_account"));
    };
    if let Some(known) = cache.get(handle) {
        return Ok(match known {
            Some(scope) => Matched::Scope(scope.clone()),
            None => Matched::Unmatched("no_account"),
        });
    }
    let mut found = store.accounts_with_handle(platform, handle)?;
    match found.len() {
        0 => {
            cache.insert(handle.to_string(), None);
            Ok(Matched::Unmatched("no_account"))
        }
        1 => {
            let scope = found.remove(0);
            cache.insert(handle.to_string(), Some(scope.clone()));
            Ok(Matched::Scope(scope))
        }
        _ => Ok(Matched::Unmatched("ambiguous_account")),
    }
}

// ---------------------------------------------------------------------------------------------------
// Commands.
// ---------------------------------------------------------------------------------------------------

/// Route one method of this module. Unknown methods are `unknown_method`.
pub fn dispatch(store: &dyn InboxStore, method: &str, input: Value) -> Result<Value, Fail> {
    match method {
        "inbox.receive" => {
            store.require_role()?;
            receive(store, input)
        }
        "inbox.list" => list(store, input),
        "inbox.get" => get(store, input),
        "inbox.set_state" => {
            store.require_role()?;
            set_state(store, input)
        }
        "inbox.stats" => stats(store),
        other => Err(("unknown_method", format!("unknown method: {other}"))),
    }
}

/// `inbox.receive`: verify, then store once. See the module comment.
pub fn receive(store: &dyn InboxStore, mut input: Value) -> Result<Value, Fail> {
    let secret = take_secret(&mut input)?;
    receive_verified(store, input, &secret)
        .map_err(|(code, message)| (code, scrub(message, &secret)))
}

fn receive_verified(store: &dyn InboxStore, input: Value, secret: &str) -> Result<Value, Fail> {
    let req: InboxReceiveInput = serde_json::from_value(input).map_err(|_| {
        bad("inbox.receive input needs platform, raw_body_b64, received_at and, optionally, signature_header")
    })?;
    if !INBOX_PLATFORMS.contains(&req.platform.as_str()) {
        return Err((
            "platform_unsupported",
            format!("platform must be one of {}", INBOX_PLATFORMS.join(", ")),
        ));
    }
    let received = time::format_instant(time::parse_instant(&req.received_at, "received_at")?);
    let body = decode_body(&req.raw_body_b64, MAX_BODY_BYTES)?;
    if !verify_signature(secret.as_bytes(), &body, req.signature_header.as_deref()) {
        // The counter holds a platform and a day, nothing from the body. A store failure here must not
        // change the refusal an unauthenticated caller sees, so it is dropped.
        let _ = count_refusal(store, &req.platform, &received);
        return Err(signature_invalid());
    }
    store_delivery(store, &req.platform, &received, &body)
}

/// Counts one delivery refused for its signature (platform and UTC day only), best effort: a store failure
/// never changes the refusal the caller sees.
pub fn note_refusal(store: &dyn InboxStore, platform: &str, received_secs: i64) {
    let _ = count_refusal(store, platform, &time::format_instant(received_secs));
}

fn count_refusal(store: &dyn InboxStore, platform: &str, received: &str) -> Result<(), Fail> {
    let day = received.get(..10).unwrap_or(received);
    store.write_txn(&mut || store.bump_refusal(platform, day))
}

/// The lowercase hex SHA-256 of the raw body: the delivery's idempotency key.
fn body_hash(body: &[u8]) -> String {
    rightkit_crypto::hex_encode(&rightkit_crypto::sha256(body))
}

fn store_delivery(
    store: &dyn InboxStore,
    platform: &str,
    received: &str,
    body: &[u8],
) -> Result<Value, Fail> {
    let sha = body_hash(body);
    let output = in_txn(store, || {
        if let Some(delivery) = store.load_delivery(&sha)? {
            let events = store.delivery_events(&sha)?;
            return Ok(InboxReceiveOutput {
                duplicate: true,
                delivery,
                events,
            });
        }
        let (status, reason, object, events) = match parse_delivery(body) {
            Parsed::Events { object, events } => ("parsed", None, object, events),
            Parsed::Unparseable(why) => ("unparseable", Some(why), None, Vec::new()),
        };
        let body_bytes = i64::try_from(body.len()).unwrap_or(i64::MAX);
        let event_count = i64::try_from(events.len()).unwrap_or(i64::MAX);
        store.insert_delivery(&NewDelivery {
            body_sha256: &sha,
            platform,
            received_at: received,
            body_bytes,
            status,
            reason,
            object: object.as_deref(),
            event_count,
        })?;
        let mut cache: HashMap<String, Option<Scope>> = HashMap::new();
        for (seq, event) in events.iter().enumerate() {
            let matched = match_account(
                store,
                platform,
                event.recipient.as_deref(),
                event.recipient_cut,
                &mut cache,
            )?;
            let (scope, unmatched, why) = match &matched {
                Matched::Scope(s) => (Some(s), false, None),
                Matched::Unmatched(why) => (None, true, Some(*why)),
            };
            // The same platform event arriving in a different body (a redelivery with other bytes, a
            // reordered batch) is stored once: skip an event whose platform id is already stored for the same
            // platform, kind and recipient. Only comment, mention and message events carry such an id; a cut
            // (oversize) id is never trusted for this.
            if !event.oversize && event.kind != "other" {
                if let Some(pid) = &event.platform_event_id {
                    if store.platform_event_seen(
                        platform,
                        event.kind,
                        pid,
                        event.recipient.as_deref(),
                    )? {
                        continue;
                    }
                }
            }
            let event_id = format!("ibx_{}_{seq:04}", &sha[..24]);
            store.insert_event(&NewStoredEvent {
                event_id: &event_id,
                body_sha256: &sha,
                seq: i64::try_from(seq).unwrap_or(i64::MAX),
                platform,
                scope,
                unmatched_account: unmatched,
                unmatched_reason: why,
                recipient_id: event.recipient.as_deref(),
                kind: event.kind,
                platform_event_id: event.platform_event_id.as_deref(),
                sender_id: event.sender_id.as_deref(),
                text: event.text.as_deref(),
                event_time: event.event_time.as_deref(),
                oversize: event.oversize,
                raw_json: &event.raw_json,
                received_at: received,
            })?;
        }
        let delivery = store.load_delivery(&sha)?.ok_or((
            "internal",
            "the delivery just written is missing".to_string(),
        ))?;
        let stored = store.delivery_events(&sha)?;
        Ok(InboxReceiveOutput {
            duplicate: false,
            delivery,
            events: stored,
        })
    })?;
    encoded(serde_json::to_value(output))
}

fn valid_kind(kind: &str) -> Result<(), Fail> {
    if INBOX_KINDS.contains(&kind) {
        Ok(())
    } else {
        Err(bad(format!(
            "kind must be one of {}",
            INBOX_KINDS.join(", ")
        )))
    }
}

/// `inbox.list`: newest arrival first, for one scope or for the unmatched events.
pub fn list(store: &dyn InboxStore, input: Value) -> Result<Value, Fail> {
    let req: InboxListInput = parse_input(input, "inbox.list")?;
    match (&req.scope, req.unmatched) {
        (Some(_), true) => return Err(bad("give scope or unmatched: true, not both")),
        (None, false) => return Err(bad("give scope or unmatched: true")),
        _ => {}
    }
    if let Some(kind) = req.kind.as_deref() {
        valid_kind(kind)?;
    }
    if let Some(state) = req.state.as_deref() {
        if !INBOX_STATES.contains(&state) {
            return Err(bad(format!(
                "state must be one of {}",
                INBOX_STATES.join(", ")
            )));
        }
    }
    let limit = req.limit.unwrap_or(DEFAULT_LIMIT);
    if limit == 0 || limit > MAX_LIMIT {
        return Err(bad(format!("limit must be 1 to {MAX_LIMIT}")));
    }
    if let Some(scope) = &req.scope {
        require_channel(store, scope)?;
    }
    if let Some(before) = req.before.as_deref() {
        if store.load_event(before)?.is_none() {
            return Err(bad("before must be the id of a stored event"));
        }
    }
    let mut events = store.list_events(&EventQuery {
        venture: req.scope.as_ref().map(|s| s.venture.as_str()),
        channel: req.scope.as_ref().map(|s| s.channel.as_str()),
        account: req.scope.as_ref().and_then(|s| s.account.as_deref()),
        kind: req.kind.as_deref(),
        state: req.state.as_deref(),
        before: req.before.as_deref(),
        limit: i64::from(limit) + 1,
    })?;
    let next_before = if events.len() > limit as usize {
        events.truncate(limit as usize);
        events.last().map(|e| e.event_id.clone())
    } else {
        None
    };
    encoded(serde_json::to_value(InboxListOutput {
        events,
        next_before,
    }))
}

/// `inbox.get`: one event with the raw JSON of its item.
pub fn get(store: &dyn InboxStore, input: Value) -> Result<Value, Fail> {
    let req: InboxGetInput = parse_input(input, "inbox.get")?;
    let event = store
        .load_event(&req.event_id)?
        .ok_or_else(event_not_found)?;
    event_in_scope(&event, req.scope.as_ref())?;
    let raw_text = store.event_raw_json(&req.event_id)?;
    let raw = serde_json::from_str(&raw_text).map_err(|e| ("internal", e.to_string()))?;
    encoded(serde_json::to_value(InboxGetOutput { event, raw }))
}

/// The author rule: `adrian`, `skill` or `daemon`, and only `adrian` is a person.
pub fn require_human(by: &str) -> Result<String, Fail> {
    let name = by.trim();
    if name.is_empty() {
        return Err(bad("by must not be empty"));
    }
    match name {
        "adrian" => Ok(name.to_string()),
        "skill" | "daemon" => Err((
            "author_forbidden",
            "by is an agent author: only a person (adrian) sets an inbox event's state".to_string(),
        )),
        _ => Err(bad("by must be adrian, skill or daemon")),
    }
}

/// `inbox.set_state`: a person marks an event `open`, `handled` or `ignored`. Records who and when.
/// Sends nothing.
pub fn set_state(store: &dyn InboxStore, input: Value) -> Result<Value, Fail> {
    let req: InboxSetStateInput = parse_input(input, "inbox.set_state")?;
    let by = require_human(&req.by)?;
    if !INBOX_STATES.contains(&req.state.as_str()) {
        return Err((
            "invalid_transition",
            format!("state must be one of {}", INBOX_STATES.join(", ")),
        ));
    }
    let note = req.note.trim().to_string();
    if note.chars().count() > MAX_NOTE_CHARS {
        return Err(bad(format!("note is over {MAX_NOTE_CHARS} characters")));
    }
    let stamp = time::format_instant(time::now_secs(req.now.as_deref())?);
    let event = in_txn(store, || {
        let current = store
            .load_event(&req.event_id)?
            .ok_or_else(event_not_found)?;
        event_in_scope(&current, req.scope.as_ref())?;
        store.set_event_state(&req.event_id, &req.state, &by, &stamp, &note)?;
        store.load_event(&req.event_id)?.ok_or_else(event_not_found)
    })?;
    encoded(serde_json::to_value(InboxSetStateOutput { event }))
}

/// `inbox.stats`: event counts by scope, kind and state; delivery counts; refusals by platform and day.
pub fn stats(store: &dyn InboxStore) -> Result<Value, Fail> {
    let (total_events, unmatched_events) = store.event_totals()?;
    let groups = store.event_groups()?;
    let deliveries_parsed = store.delivery_count("parsed")?;
    let deliveries_unparseable = store.delivery_count("unparseable")?;
    let refused = store.refusals()?;
    let refused_total: i64 = refused.iter().map(|r| r.count).sum();
    encoded(serde_json::to_value(InboxStatsOutput {
        total_events,
        unmatched_events,
        groups,
        deliveries_parsed,
        deliveries_unparseable,
        refused_total,
        refused,
    }))
}
