//! WP-06 — Stable dedupe / event-id / content-hash helpers.
//!
//! This module is the single authority for deriving the *post-admission*
//! shape of a trigger event:
//!
//! - [`derive_event_id`] is the only path that produces a [`TriggerEventId`].
//!   It hashes `(source, external_id, time_unix_ms)` so the same upstream
//!   delivery redelivered at the same provider time always yields the same
//!   id across restarts and replays. The hash is `sha256:<hex>`.
//! - [`derive_dedupe_key`] is the *idempotency key* the storage layer uses
//!   to collapse redeliveries. It folds in the content hash and the
//!   trigger id (when known) so two deliveries with different content but
//!   the same external id do not silently merge.
//! - [`compute_content_hash`] wraps the canonical-JSON SHA-256 used for both
//!   the event id and the dedupe key, and exposes the result in the
//!   `sha256:<hex64>` form the schema mandates.
//! - [`build_normalized_canonical_json`] is the canonical-JSON serializer
//!   used for hashing. We avoid pulling in `serde_json::to_string` because
//!   the upstream caller controls the input shape; the canonical form
//!   pins object key order so two semantically equal envelopes hash to the
//!   same digest.
//!
//! ## Why a single module?
//!
//! Both the API delivery route (`api_triggers`) and the recovery worker
//! (`workflow::restart`) call into this module to (re)derive the event id
//! and dedupe key. Centralising the hash function guarantees the live
//! write path and the replay path agree, which is the only way
//! `WorkflowTriggerWriter::insert_trigger_event_idempotent` can be
//! expected to collapse redeliveries correctly.

#![forbid(unsafe_code)]

use serde_json::Value;

use rightkit_workflow::control_plane::domain::{trigger_event_id, ContentHash, TriggerEventId};

/// Maximum length, in bytes, of a derived dedupe key. The schema caps it
/// at 1024 bytes; we truncate defensively so a misbehaving caller cannot
/// crash admission by handing us an unbounded key.
pub const MAX_DEDUPE_KEY_BYTES: usize = 1024;

/// Maximum length, in bytes, of a derived dedupe key input component.
/// Anything longer is truncated before being folded into the hash so the
/// function stays O(component length) and the output stays bounded.
pub const MAX_COMPONENT_BYTES: usize = 256;

/// Derive the canonical [`TriggerEventId`] for an admitted event. The id is
/// `wf_evt:<22 hex>` — a 22-hex truncation of `sha256(source || 0x1f ||
/// external_id || 0x1f || time_unix_ms_be)`. The truncation keeps ids
/// short enough for log lines without exposing the full hash.
///
/// All three components are required; pass empty strings only when the
/// schema genuinely has no external id. Two events with the same three
/// inputs are guaranteed to hash to the same id across processes.
pub fn derive_event_id(source: &str, external_id: &str, time_unix_ms: i64) -> TriggerEventId {
    let raw = raw_event_id_hash(source, external_id, time_unix_ms);
    trigger_event_id(raw)
}

/// Internal helper that produces the 22-hex raw component of the event id
/// without wrapping it in a [`TriggerEventId`]. Exposed only so the
/// recovery worker can compute the *expected* id for a re-derived event
/// and compare it to the stored id without paying for a second allocation.
pub(crate) fn raw_event_id_hash(source: &str, external_id: &str, time_unix_ms: i64) -> String {
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    hasher.update(format!("{}:trigger-event-v1\x1f", crate::host_identity::app()).as_bytes());
    hasher.update(truncate_bytes(source, MAX_COMPONENT_BYTES).as_bytes());
    hasher.update(b"\x1f");
    hasher.update(truncate_bytes(external_id, MAX_COMPONENT_BYTES).as_bytes());
    hasher.update(b"\x1f");
    hasher.update(&time_unix_ms.to_be_bytes());
    let digest = hasher.finalize_bytes();
    rightkit_fs::digest::hex(&digest[..11])
}

/// Derive the *idempotency key* used by the storage layer to collapse
/// redeliveries. The key is
/// `source|external_id|content_hash|trigger_id`, where each component is
/// truncated to [`MAX_COMPONENT_BYTES`] bytes. The composite result is
/// itself truncated to [`MAX_DEDUPE_KEY_BYTES`] bytes.
///
/// Two deliveries with the same `(source, external_id)` will *only*
/// collide if they also share the content hash, so an upstream that
/// retries after a body change does not silently merge.
pub fn derive_dedupe_key(
    source: &str,
    external_id: &str,
    content_hash: &ContentHash,
    trigger_id: Option<&str>,
) -> String {
    let mut buf = String::with_capacity(128);
    buf.push_str(&truncate_bytes(source, MAX_COMPONENT_BYTES));
    buf.push('|');
    buf.push_str(&truncate_bytes(external_id, MAX_COMPONENT_BYTES));
    buf.push('|');
    buf.push_str(content_hash.as_str());
    if let Some(trigger_id) = trigger_id {
        buf.push('|');
        buf.push_str(&truncate_bytes(trigger_id, MAX_COMPONENT_BYTES));
    }
    truncate_bytes(&buf, MAX_DEDUPE_KEY_BYTES)
}

/// Compute the canonical SHA-256 of a canonical-JSON byte slice. The
/// result is rendered as `sha256:<hex64>` to match the schema and the
/// rest of the workflow crate.
pub fn compute_content_hash(canonical_bytes: &[u8]) -> ContentHash {
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    hasher.update(format!("{}:trigger-content-v1\x1f", crate::host_identity::app()).as_bytes());
    hasher.update(canonical_bytes);
    let digest = hasher.finalize_bytes();
    ContentHash::sha256(&digest)
}

/// Build the canonical-JSON form used for hashing. We do not pull in
/// `serde_json::to_vec` here because the input has already been
/// normalised into [`NormalizedData`](super::event::NormalizedData) by the
/// time we see it. The canonical form is:
///
/// 1. Re-serialise through [`serde_json::to_string`] — this is
///    deterministic for `BTreeMap`-backed JSON objects but not for
///    `serde_json::Map`. We therefore sort object keys at the top level
///    and within nested objects.
/// 2. Re-parse and re-serialise with `serde_json::Serializer::with_formatter`
///    so the byte order matches across processes.
///
/// For non-object data the canonical form is the JSON value itself.
pub fn build_normalized_canonical_json(
    data: &super::event::NormalizedData,
) -> Result<Vec<u8>, serde_json::Error> {
    let value = data.as_value();
    let sorted = sort_object_keys(value);
    serde_json::to_vec(&sorted)
}

/// Truncate `value` to at most `max_bytes` UTF-8 bytes. Operates on the
/// nearest valid UTF-8 boundary so we never split a code point in the
/// middle.
pub fn truncate_bytes(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = 0;
    for (offset, ch) in value.char_indices() {
        let next = offset + ch.len_utf8();
        if next >= max_bytes {
            break;
        }
        end = next;
    }
    value[..end].to_string()
}

/// Recursively sort object keys so two semantically equal JSON values
/// serialize to the same byte string. The schema is closed for objects
/// (`NormalizedData::Object` carries a `serde_json::Map`), but the
/// `BTreeMap` in `NormalizedTriggerEvent::extensions` and the inbound
/// `serde_json::Value::Object` form are not necessarily sorted.
fn sort_object_keys(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let mut sorted = serde_json::Map::with_capacity(entries.len());
            for (k, v) in entries {
                sorted.insert(k, sort_object_keys(v));
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sort_object_keys).collect()),
        other => other,
    }
}
