//! Argument reading for typed QA commands.
//!
//! A QA command takes its arguments as one JSON object text and reads them with these helpers.
//! Empty or malformed text reads as an empty object, so every argument is absent. Nothing here
//! names a command, a key or a product.

use serde_json::{json, Value};

/// The command's arguments: one JSON object text, or nothing.
pub fn parse(raw: &str) -> Value {
    if raw.trim().is_empty() {
        return json!({});
    }
    serde_json::from_str(raw).unwrap_or_else(|_| json!({}))
}

/// A string argument, if present and a string.
pub fn text(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_string)
}

/// A non-blank string argument.
pub fn non_blank(args: &Value, key: &str) -> Option<String> {
    text(args, key).filter(|value| !value.trim().is_empty())
}

/// A boolean argument, as `true` or as the text `1`/`true`/`yes`.
pub fn flag(args: &Value, key: &str) -> bool {
    match args.get(key) {
        Some(Value::Bool(value)) => *value,
        Some(Value::String(value)) => {
            matches!(value.trim(), "1" | "true" | "TRUE" | "yes" | "YES")
        }
        _ => false,
    }
}
