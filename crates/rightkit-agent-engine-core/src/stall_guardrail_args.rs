// Sibling fragment of stall_guardrail.rs — included via `include!()`. Pure
// helpers: canonicalize the JSON arguments (sort object keys recursively),
// then hash the canonical form so re-ordered fields hash identically.

/// Canonicalize the JSON arguments and hash the resulting canonical form.
/// Two argument-JSONs that differ ONLY by key order — which LLMs frequently
/// emit — produce the same signature hash.
pub fn normalize_args_hash(arguments_json: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::Hasher;
    let canonical = canonicalize(arguments_json);
    let mut hasher = DefaultHasher::new();
    hasher.write(canonical.as_bytes());
    hasher.finish()
}

fn canonicalize(s: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(s) {
        Ok(value) => canonicalize_value(&value),
        Err(_) => s.to_string(),
    }
}

fn canonicalize_value(v: &serde_json::Value) -> String {
    use serde_json::Value;
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => {
            format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
        }
        Value::Array(arr) => {
            let parts: Vec<String> = arr.iter().map(canonicalize_value).collect();
            format!("[{}]", parts.join(","))
        }
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            let parts: Vec<String> = entries
                .iter()
                .map(|(k, v)| {
                    format!(
                        "\"{}\":{}",
                        k.replace('\\', "\\\\").replace('"', "\\\""),
                        canonicalize_value(v)
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
    }
}
