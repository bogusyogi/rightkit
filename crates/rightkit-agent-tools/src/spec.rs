//! The typed tool contract: a [`ToolSpec`] (name, description, JSON schema,
//! effect class) and a [`Tool`] that runs it.

use serde::Serialize;
use serde_json::{json, Value};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// What a tool does to the world, for admission and journaling. Content-free.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// Reads state; no side effect on the target.
    Observe,
    /// Input or a command that changes a target app.
    Act,
    /// Starts or stops a process / session.
    Launch,
    /// Captures pixels (screenshot).
    Capture,
    /// Reaches the network (browser navigation).
    Network,
}

impl Effect {
    pub fn label(self) -> &'static str {
        match self {
            Effect::Observe => "observe",
            Effect::Act => "act",
            Effect::Launch => "launch",
            Effect::Capture => "capture",
            Effect::Network => "network",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolSpec {
    /// `<family>.<verb>`, e.g. `genright.click`, `desktop.snapshot`, `browser.open`.
    pub name: String,
    pub description: String,
    /// JSON Schema (subset: object, properties, required, type, enum).
    pub input_schema: Value,
    pub effect: Effect,
}

impl ToolSpec {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        effect: Effect,
        input_schema: Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
            effect,
        }
    }
}

/// Stable machine-readable failure.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl ToolError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
        }
    }
    pub fn retryable(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: true,
        }
    }
    pub fn to_value(&self) -> Value {
        json!({"code": self.code, "message": self.message, "retryable": self.retryable})
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for ToolError {}

pub const ERR_UNKNOWN_TOOL: &str = "unknown_tool";
pub const ERR_NOT_EXPOSED: &str = "not_exposed";
pub const ERR_NOT_ALLOWED: &str = "tool_not_allowed";
pub const ERR_DENIED: &str = "denied_by_admission";
pub const ERR_INVALID_ARGS: &str = "invalid_arguments";
pub const ERR_CANCELLED: &str = "cancelled";

/// Who is calling (the allowlist key).
#[derive(Debug, Clone)]
pub struct Caller {
    pub app: String,
}

impl Caller {
    pub fn new(app: impl Into<String>) -> Self {
        Self { app: app.into() }
    }
}

/// Cooperative cancellation a tool may poll.
#[derive(Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);
impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

pub struct CallCtx<'a> {
    pub caller: &'a Caller,
    pub cancel: &'a CancelToken,
}

pub trait Tool: Send + Sync {
    fn spec(&self) -> &ToolSpec;
    /// The inner verb for tools that carry one (e.g. the app command name of
    /// `<app>.command`). The exposure rules and allowlist see it as
    /// `<tool>:<subject>`.
    fn subject(&self, _args: &Value) -> Option<String> {
        None
    }
    fn call(&self, ctx: &CallCtx, args: Value) -> Result<Value, ToolError>;
}

/// Strict schema check: everything [`validate_args`] checks, plus unknown-field rejection.
///
/// An argument is unknown when the schema does not list it under `properties`. The check is
/// skipped only when the schema explicitly sets `additionalProperties` to `true` or to a schema
/// object. A missing `additionalProperties` is treated as `false` (strict), so a typo such as
/// `{"nmae": ..}` fails closed instead of being silently ignored.
pub fn validate_args_strict(schema: &Value, args: &Value) -> Result<(), String> {
    validate_args(schema, args)?;
    let open = match &schema["additionalProperties"] {
        Value::Bool(b) => *b,
        Value::Object(_) => true,
        _ => false,
    };
    if open {
        return Ok(());
    }
    if let Value::Object(obj) = args {
        let props = schema["properties"].as_object();
        let mut unknown: Vec<&str> = obj
            .keys()
            .map(String::as_str)
            .filter(|k| !props.is_some_and(|p| p.contains_key(*k)))
            .collect();
        unknown.sort_unstable();
        if let Some(first) = unknown.first() {
            return Err(format!("unknown argument `{first}`"));
        }
    }
    Ok(())
}

/// Minimal schema check: `required` present, declared primitive types and enums match.
pub fn validate_args(schema: &Value, args: &Value) -> Result<(), String> {
    let obj = match args {
        Value::Object(o) => o,
        Value::Null => return validate_args(schema, &json!({})),
        _ => return Err("arguments must be a JSON object".into()),
    };
    if let Some(req) = schema["required"].as_array() {
        for r in req.iter().filter_map(Value::as_str) {
            if !obj.contains_key(r) {
                return Err(format!("missing required argument `{r}`"));
            }
        }
    }
    if let Some(props) = schema["properties"].as_object() {
        for (k, v) in obj {
            let Some(p) = props.get(k) else { continue };
            if let Some(t) = p["type"].as_str() {
                let ok = match t {
                    "string" => v.is_string(),
                    "number" => v.is_number(),
                    "integer" => v.is_i64() || v.is_u64(),
                    "boolean" => v.is_boolean(),
                    "object" => v.is_object(),
                    "array" => v.is_array(),
                    _ => true,
                };
                if !ok {
                    return Err(format!("argument `{k}` must be {t}"));
                }
            }
            if let Some(e) = p["enum"].as_array() {
                if !e.contains(v) {
                    return Err(format!("argument `{k}` is not one of the allowed values"));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod strict_tests {
    use super::*;

    fn schema() -> Value {
        json!({"type": "object", "required": ["a"], "properties": {"a": {"type": "string"}}})
    }

    #[test]
    fn strict_rejects_unknown_field() {
        assert!(validate_args_strict(&schema(), &json!({"a": "x"})).is_ok());
        let err = validate_args_strict(&schema(), &json!({"a": "x", "b": 1})).unwrap_err();
        assert!(err.contains("`b`"));
        // The lenient check still accepts it.
        assert!(validate_args(&schema(), &json!({"a": "x", "b": 1})).is_ok());
    }

    #[test]
    fn strict_still_enforces_base_rules() {
        assert!(validate_args_strict(&schema(), &json!({})).is_err());
        assert!(validate_args_strict(&schema(), &json!({"a": 1})).is_err());
        assert!(validate_args_strict(&schema(), &json!("x")).is_err());
    }

    #[test]
    fn strict_honors_explicit_additional_properties() {
        let mut open = schema();
        open["additionalProperties"] = json!(true);
        assert!(validate_args_strict(&open, &json!({"a": "x", "b": 1})).is_ok());
        let mut closed = schema();
        closed["additionalProperties"] = json!(false);
        assert!(validate_args_strict(&closed, &json!({"a": "x", "b": 1})).is_err());
    }

    #[test]
    fn strict_with_no_properties_rejects_any_field() {
        assert!(validate_args_strict(&json!({"type": "object"}), &json!({"z": 1})).is_err());
        assert!(validate_args_strict(&json!({"type": "object"}), &Value::Null).is_ok());
    }
}
