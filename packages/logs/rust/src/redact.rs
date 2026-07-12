use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

const REDACTION_MARKER: &str = "[redacted:diagnostics]";
const DEFAULT_EXACT_KEYS: &[&str] = &[
    "api_key",
    "authorization",
    "body",
    "clipboard",
    "completion",
    "content",
    "cookie",
    "last_transcript",
    "message",
    "password",
    "prompt",
    "recognized_text",
    "refresh_token",
    "transcript",
    "window_title",
];
const DEFAULT_KEY_FRAGMENTS: &[&str] = &["secret", "password", "api_key", "token"];

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactionPolicy {
    #[serde(default)]
    pub additional_exact_keys: BTreeSet<String>,
    #[serde(default)]
    pub additional_key_fragments: BTreeSet<String>,
}

impl RedactionPolicy {
    /// Redact sensitive fields in-place before an event is persisted or exported.
    pub fn redact_value(&self, value: &mut Value) {
        self.redact(value);
    }

    pub(crate) fn redact(&self, value: &mut Value) {
        match value {
            Value::Object(object) => {
                for (key, child) in object.iter_mut() {
                    if self.is_sensitive_key(key) {
                        *child = Value::String(REDACTION_MARKER.to_string());
                    } else {
                        self.redact(child);
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    self.redact(item);
                }
            }
            _ => {}
        }
    }

    fn is_sensitive_key(&self, key: &str) -> bool {
        let normalized = key.to_ascii_lowercase();
        DEFAULT_EXACT_KEYS.contains(&normalized.as_str())
            || DEFAULT_KEY_FRAGMENTS
                .iter()
                .any(|fragment| normalized.contains(fragment))
            || self
                .additional_exact_keys
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(key))
            || self.additional_key_fragments.iter().any(|fragment| {
                let fragment = fragment.to_ascii_lowercase();
                !fragment.is_empty() && normalized.contains(&fragment)
            })
    }
}
