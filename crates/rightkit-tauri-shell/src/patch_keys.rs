//! Helpers for a settings-patch command that runs side effects per touched key.
//!
//! The revision handshake (`expectedRevision`) is the shell's host settings service; what stays
//! with the host is the list of keys that need an effect after a patch (refresh a surface, restart
//! an engine). These helpers answer "which keys did this patch name" without each effect
//! re-parsing the JSON object.

use serde_json::Value;

/// The patch's keys joined with `,` for a log line; `<non-object>` when the patch is not a JSON
/// object.
pub fn summary(patch: &Value) -> String {
    patch
        .as_object()
        .map(|object| object.keys().cloned().collect::<Vec<_>>().join(","))
        .unwrap_or_else(|| "<non-object>".to_string())
}

/// Whether the patch names at least one of `keys`. A non-object patch names none.
pub fn touches_any(patch: &Value, keys: &[&str]) -> bool {
    patch
        .as_object()
        .is_some_and(|object| keys.iter().any(|key| object.contains_key(*key)))
}

/// Whether the patch names `key`.
pub fn touches(patch: &Value, key: &str) -> bool {
    touches_any(patch, &[key])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_keys() {
        let patch = json!({"a": 1, "b": 2});
        assert!(touches(&patch, "a"));
        assert!(touches_any(&patch, &["x", "b"]));
        assert!(!touches_any(&patch, &["x"]));
        assert_eq!(summary(&json!(3)), "<non-object>");
        assert!(!touches(&json!([1]), "a"));
    }
}
