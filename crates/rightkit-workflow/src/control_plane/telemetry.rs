//! WP-10 — Redacted local-first telemetry.
//!
//! All telemetry produced by the workflow control plane is local-first.
//! Strings are the only projection — structured values are flattened,
//! and any field matching a `SecretPattern` is replaced with
//! `[REDACTED]`.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::control_plane::privacy::{SecretKind, SecretPatterns};

/// What kind of event this telemetry row records.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TelemetryKind {
    /// Run was admitted.
    RunStarted,
    /// Node finished (success or terminal failure).
    NodeCompleted,
    /// Effect intent was dispatched to its adapter.
    EffectDispatched,
    /// Approval was decided.
    ApprovalDecided,
    /// Inbox item was delivered.
    InboxDelivered,
}

/// One redaction-safe telemetry event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelemetryEvent {
    /// Stable id of the event.
    pub id: String,
    /// Kind.
    pub kind: TelemetryKind,
    /// Run id.
    pub run_id: String,
    /// Node id, if applicable.
    pub node_id: String,
    /// Correlation id — same across all events of one logical task.
    pub correlation_id: String,
    /// When the event occurred (Unix ms).
    pub occurred_at_unix_ms: i64,
    /// Flattened, redacted string-only payload.
    pub redacted_payload: BTreeMap<String, String>,
}

/// Errors raised by [`redact`].
#[derive(Debug, Error)]
pub enum TelemetryError {
    /// The input JSON could not be serialized to a string.
    #[error("failed to flatten value: {0}")]
    Flatten(String),
}

/// Flatten a [`Value`] into a sorted string map and scrub each string
/// against the given [`SecretPatterns`].
pub fn redact(value: &Value, patterns: &SecretPatterns) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    flatten_into(value, "", &mut out);
    for v in out.values_mut() {
        if let Some(kind) = match_kind(v, patterns) {
            *v = format!("[REDACTED:{}]", kind.as_str());
        }
    }
    out
}

/// Return the correlation scope string (run + correlation id).
pub fn correlation_scope(event: &TelemetryEvent) -> String {
    format!("{}|{}", event.run_id, event.correlation_id)
}

fn flatten_into(value: &Value, prefix: &str, out: &mut BTreeMap<String, String>) {
    match value {
        Value::Null => {
            out.insert(prefix.to_string(), "null".into());
        }
        Value::Bool(b) => {
            out.insert(prefix.to_string(), b.to_string());
        }
        Value::Number(n) => {
            out.insert(prefix.to_string(), n.to_string());
        }
        Value::String(s) => {
            out.insert(prefix.to_string(), s.clone());
        }
        Value::Array(arr) => {
            for (i, item) in arr.iter().enumerate() {
                let key = if prefix.is_empty() {
                    format!("[{i}]")
                } else {
                    format!("{prefix}[{i}]")
                };
                flatten_into(item, &key, out);
            }
        }
        Value::Object(map) => {
            for (k, v) in map {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten_into(v, &key, out);
            }
        }
    }
}

/// Match a string against the registered patterns. Returns the
/// [`SecretKind`] that matched, or `None`. Iterates over the public
/// `as_slice()` accessor — `SecretPatterns::patterns` is private.
fn match_kind(s: &str, patterns: &SecretPatterns) -> Option<SecretKind> {
    for p in patterns.as_slice() {
        if (p.matcher)(s) {
            return match p.kind {
                "api_key" => Some(SecretKind::ApiKey),
                "email" => Some(SecretKind::Email),
                "ipv4" => Some(SecretKind::Ipv4),
                "private_key_pem" => Some(SecretKind::PrivateKeyPem),
                _ => None,
            };
        }
    }
    None
}
