//! The transport-neutral half of the `apple` command: bounded JSON input, the request shape and
//! the result marking. The argv wiring (flags, defaults) stays with the app.

use std::{fs::File, io::Read, path::Path};

use serde_json::{json, Value};

/// Largest accepted JSON input, in bytes.
pub const INPUT_LIMIT: u64 = 1024 * 1024;

/// The operation arguments from `--input` text or an `--input-file` path (at most one),
/// bounded to [`INPUT_LIMIT`]. Neither means `{}`; `execute` sets `"execute": true`.
pub fn read_input(
    input: Option<&str>,
    input_file: Option<&Path>,
    execute: bool,
) -> Result<Value, String> {
    let raw = match (input, input_file) {
        (Some(raw), None) if raw.len() <= INPUT_LIMIT as usize => raw.to_owned(),
        (None, Some(path)) => {
            let file = File::open(path).map_err(|e| format!("Apple input: {e}"))?;
            if !file.metadata().map_err(|e| e.to_string())?.is_file() {
                return Err("Apple input must be a regular file".into());
            }
            let mut text = String::new();
            file.take(INPUT_LIMIT + 1)
                .read_to_string(&mut text)
                .map_err(|e| format!("Apple input: {e}"))?;
            if text.len() > INPUT_LIMIT as usize {
                return Err("Apple input exceeds 1 MiB".into());
            }
            text
        }
        (None, None) => "{}".into(),
        _ => return Err("provide at most 1 MiB through --input or --input-file".into()),
    };
    let mut value: Value =
        serde_json::from_str(&raw).map_err(|e| format!("invalid Apple JSON input: {e}"))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| "Apple input must be a JSON object".to_string())?;
    if execute {
        object.insert("execute".into(), Value::Bool(true));
    }
    Ok(value)
}

/// The request `crate::gate::invoke` takes. `policy_context` is the host's JSON text, if any.
pub fn build_request(
    operation: &str,
    arguments: Value,
    policy_context: Option<&str>,
) -> Result<Value, String> {
    let policy_context = policy_context
        .map(serde_json::from_str::<Value>)
        .transpose()
        .map_err(|e| format!("invalid Apple policy context: {e}"))?;
    Ok(json!({
        "operation": operation,
        "arguments": arguments,
        "policyContext": policy_context,
    }))
}

/// Mark a result that reports `success: false` or `ok: false` with `valid: false`.
pub fn mark_validity(mut value: Value) -> Value {
    if value.get("success").and_then(Value::as_bool) == Some(false)
        || value.get("ok").and_then(Value::as_bool) == Some(false)
    {
        value["valid"] = Value::Bool(false);
    }
    value
}
