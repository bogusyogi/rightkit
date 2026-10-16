//! Pre-serialised JSON that reaches the wire byte for byte.
//!
//! `serde_json::Value` cannot hold a `RawValue`, and re-serialising a parsed `Value`
//! reorders keys (no `preserve_order`) and rewrites numbers (`1.0` becomes `1.0`, `1e2`
//! becomes `100.0`, big integers lose precision). A host that already holds the exact bytes
//! it wants to return (a REST response, say) wraps them in [`RawJson`]. [`RawJson`] is
//! validated once, when it is built, and stands in the response tree as a private marker
//! object. The server writes every response through [`to_wire_string`], which swaps each
//! marker for its original text.

use std::sync::OnceLock;

use serde_json::value::RawValue;
use serde_json::{Map, Value};

/// A validated, pre-serialised JSON value whose bytes are emitted unchanged.
#[derive(Debug, Clone)]
pub struct RawJson(Box<RawValue>);

/// Equal when the JSON text is byte-for-byte equal (`RawValue` has no `PartialEq`).
impl PartialEq for RawJson {
    fn eq(&self, other: &Self) -> bool {
        self.0.get() == other.0.get()
    }
}

impl Eq for RawJson {}

impl RawJson {
    /// Validates `json` (one complete JSON value, nothing after it) and keeps the text as is.
    pub fn new(json: impl Into<String>) -> Result<Self, serde_json::Error> {
        RawValue::from_string(json.into()).map(Self)
    }

    /// Like [`RawJson::new`] from UTF-8 bytes.
    pub fn from_slice(json: &[u8]) -> Result<Self, serde_json::Error> {
        let text = std::str::from_utf8(json)
            .map_err(<serde_json::Error as serde::de::Error>::custom)?;
        Self::new(text)
    }

    /// Serialises `value` once and keeps those bytes.
    pub fn from_serialize<T: serde::Serialize + ?Sized>(
        value: &T,
    ) -> Result<Self, serde_json::Error> {
        serde_json::value::to_raw_value(value).map(Self)
    }

    /// The exact JSON text.
    pub fn get(&self) -> &str {
        self.0.get()
    }

    pub fn into_inner(self) -> Box<RawValue> {
        self.0
    }

    /// A `Value` that stands for this JSON inside a response tree (`structuredContent`, a
    /// `call_tool` / `AsyncRequestHandler` payload, ...). It is only meaningful to
    /// [`to_wire_string`], which the server host uses for every reply; do not inspect or
    /// serialise it any other way.
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert(
            marker_key().to_string(),
            Value::String(self.get().to_string()),
        );
        Value::Object(m)
    }
}

impl From<Box<RawValue>> for RawJson {
    fn from(raw: Box<RawValue>) -> Self {
        Self(raw)
    }
}

/// Per-process unguessable key, so data that a tool merely echoes cannot pose as a marker.
fn marker_key() -> &'static str {
    static KEY: OnceLock<String> = OnceLock::new();
    KEY.get_or_init(|| {
        let mut b = [0u8; 16];
        if getrandom::fill(&mut b).is_err() {
            let t = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            b.copy_from_slice(&(t ^ ((std::process::id() as u128) << 64)).to_le_bytes());
        }
        let mut s = String::from("$rightkit-raw-json-");
        for x in b {
            s.push_str(&format!("{x:02x}"));
        }
        s
    })
}

fn marker_text(m: &Map<String, Value>) -> Option<&str> {
    if m.len() != 1 {
        return None;
    }
    match m.get(marker_key()) {
        Some(Value::String(s)) => Some(s),
        _ => None,
    }
}

fn has_marker(v: &Value) -> bool {
    match v {
        Value::Object(m) => marker_text(m).is_some() || m.values().any(has_marker),
        Value::Array(a) => a.iter().any(has_marker),
        _ => false,
    }
}

fn write(v: &Value, out: &mut String) {
    match v {
        Value::Object(m) => {
            if let Some(raw) = marker_text(m) {
                out.push_str(raw);
                return;
            }
            out.push('{');
            for (i, (k, val)) in m.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).unwrap_or_default());
                out.push(':');
                write(val, out);
            }
            out.push('}');
        }
        Value::Array(a) => {
            out.push('[');
            for (i, val) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(val, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// Compact JSON text of `v`, with every [`RawJson`] marker replaced by its original bytes.
/// Identical to `v.to_string()` when the tree holds no marker.
pub fn to_wire_string(v: &Value) -> String {
    if !has_marker(v) {
        return v.to_string();
    }
    let mut out = String::new();
    write(v, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_malformed_and_trailing() {
        assert!(RawJson::new("{\"a\":").is_err());
        assert!(RawJson::new("{} {}").is_err());
        assert!(RawJson::new("").is_err());
        assert!(RawJson::from_slice(&[0xff]).is_err());
    }

    #[test]
    fn keeps_bytes_and_substitutes_in_tree() {
        let raw = r#"{"z":1.0,"a":1,"n":1E2,"u":"é😀\/"}"#;
        let r = RawJson::new(raw).unwrap();
        assert_eq!(r.get(), raw);
        let tree = serde_json::json!({"x":[1, r.to_value()], "y": r.to_value()});
        let s = to_wire_string(&tree);
        assert_eq!(s, format!(r#"{{"x":[1,{raw}],"y":{raw}}}"#));
        serde_json::from_str::<Value>(&s).unwrap();
    }

    #[test]
    fn plain_values_match_display() {
        let v = serde_json::json!({"b":[1,2.5,"x\n",null],"a":{"k":true}});
        assert_eq!(to_wire_string(&v), v.to_string());
    }

    #[test]
    fn echoed_lookalike_is_not_a_marker() {
        let v = serde_json::json!({"$rightkit-raw-json-00": "[1,2]"});
        assert_eq!(to_wire_string(&v), v.to_string());
    }
}
