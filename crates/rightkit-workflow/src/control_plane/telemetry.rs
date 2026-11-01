//! WP-10 — Redacted local-first telemetry.
//!
//! All telemetry produced by the workflow control plane is local-first.
//! Strings are the only projection — structured values are flattened,
//! and any field matching a [`SecretPattern`] is replaced with
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_plane::privacy::matches_api_key;

    #[test]
    fn redact_flattens_object_into_sorted_keys() {
        let v = serde_json::json!({"b": 2, "a": 1});
        let patterns = SecretPatterns::defaults();
        let r = redact(&v, &patterns);
        let keys: Vec<&String> = r.keys().collect();
        assert_eq!(keys, vec!["a", "b"]);
    }

    #[test]
    fn redact_scrubs_email() {
        let v = serde_json::json!({"contact": "alice@example.com"});
        let patterns = SecretPatterns::defaults();
        let r = redact(&v, &patterns);
        assert!(r.get("contact").unwrap().starts_with("[REDACTED:"));
    }

    #[test]
    fn redact_scrubs_api_key() {
        let v = serde_json::json!({"token": concat!("sk", "-1234567890abcdef")});
        let patterns = SecretPatterns::defaults();
        let r = redact(&v, &patterns);
        assert!(r.get("token").unwrap().starts_with("[REDACTED:"));
    }

    #[test]
    fn redact_scrubs_ipv4() {
        let v = serde_json::json!({"peer": "10.0.0.1"});
        let patterns = SecretPatterns::defaults();
        let r = redact(&v, &patterns);
        assert!(r.get("peer").unwrap().starts_with("[REDACTED:"));
    }

    #[test]
    fn redact_passes_through_safe_string() {
        let v = serde_json::json!({"name": "Morning Briefing"});
        let patterns = SecretPatterns::defaults();
        let r = redact(&v, &patterns);
        assert_eq!(r.get("name").unwrap(), "Morning Briefing");
    }

    #[test]
    fn correlation_scope_combines_run_and_correlation() {
        let ev = TelemetryEvent {
            id: "t1".into(),
            kind: TelemetryKind::RunStarted,
            run_id: "wfr_1".into(),
            node_id: "n1".into(),
            correlation_id: "c-7".into(),
            occurred_at_unix_ms: 0,
            redacted_payload: BTreeMap::new(),
        };
        assert_eq!(correlation_scope(&ev), "wfr_1|c-7");
    }

    #[test]
    fn redact_handles_nested_arrays() {
        let v = serde_json::json!({"items": [{"x": 1}, {"x": 2}]});
        let patterns = SecretPatterns::defaults();
        let r = redact(&v, &patterns);
        assert_eq!(r.get("items[0].x").unwrap(), "1");
        assert_eq!(r.get("items[1].x").unwrap(), "2");
    }

    #[test]
    fn redact_returns_null_for_null() {
        let v = serde_json::json!(null);
        let patterns = SecretPatterns::defaults();
        let r = redact(&v, &patterns);
        assert_eq!(r.get("").unwrap(), "null");
    }

    #[test]
    fn redact_scrubs_private_key_pem() {
        let v = serde_json::json!({"material": concat!("--", "---BEGIN PRIVATE KEY-----\nABC")});
        let patterns = SecretPatterns::defaults();
        let r = redact(&v, &patterns);
        assert!(r.get("material").unwrap().starts_with("[REDACTED:"));
    }

    #[test]
    fn redact_does_not_mutate_input() {
        let v = serde_json::json!({"name": "alice@example.com"});
        let patterns = SecretPatterns::defaults();
        let _ = redact(&v, &patterns);
        assert_eq!(v.to_string(), "{\"name\":\"alice@example.com\"}");
    }

    #[test]
    fn redact_works_with_custom_pattern_set() {
        let v = serde_json::json!({"k": "anything-with-bearer-in-it"});
        let patterns = SecretPatterns::with_kind(SecretKind::ApiKey, matches_api_key);
        let r = redact(&v, &patterns);
        assert!(r.get("k").unwrap().starts_with("[REDACTED:"));
    }
}
