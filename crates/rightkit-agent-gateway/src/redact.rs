//! Redaction check: walks any response JSON and reports a string that looks
//! like a token or secret. Reused as the release gate: seed fake secrets, run
//! every method, and fail if this finds one in an output or the audit log.

use serde_json::Value;

/// Markers that must never appear in a response or audit row.
const SECRET_MARKERS: [&str; 5] = ["ya29.", "1//", "Bearer ", "refresh_token", "client_secret"];

/// First leaked string found in `v` (keys and values), if any. `extra` adds
/// exact secrets to look for, such as a pairing key or its hash.
pub fn find_secret(v: &Value, extra: &[&str]) -> Option<String> {
    let bad = |s: &str| -> bool {
        SECRET_MARKERS.iter().any(|m| s.contains(m))
            || extra.iter().any(|e| !e.is_empty() && s.contains(e))
    };
    match v {
        Value::String(s) => bad(s).then(|| s.clone()),
        Value::Array(a) => a.iter().find_map(|x| find_secret(x, extra)),
        Value::Object(o) => o.iter().find_map(|(k, x)| {
            if bad(k) {
                Some(k.clone())
            } else {
                find_secret(x, extra)
            }
        }),
        _ => None,
    }
}

/// Same check for a plain string such as an audit column.
pub fn text_has_secret(s: &str, extra: &[&str]) -> bool {
    find_secret(&Value::String(s.to_string()), extra).is_some()
}

/// Panics (without echoing the match) when `v` contains a secret-looking string.
pub fn assert_clean(v: &Value, extra: &[&str]) {
    assert!(
        find_secret(v, extra).is_none(),
        "response contains a token or secret-looking string"
    );
}
