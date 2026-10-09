//! JSON helpers reproducing the JavaScript semantics the Node verdict relies on:
//! exact field sets, safe-integer and digest predicates, truthiness and canonical
//! JSON for digests.

use serde_json::Value;
use sha2::{Digest, Sha256};

/// `Number.MAX_SAFE_INTEGER`.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// Field predicate. Named functions and non-capturing closures coerce to this.
pub type Check = fn(&Value) -> bool;

static NULL: Value = Value::Null;

/// `value[key]`, or JSON null when absent (JavaScript `undefined ?? null`).
pub fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    value.get(key).unwrap_or(&NULL)
}

/// `Object.hasOwn(value, key)` for a JSON value.
pub fn has(value: &Value, key: &str) -> bool {
    value.get(key).is_some()
}

/// String content, or the empty string for anything else.
pub fn str_of(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}

/// Array items, or an empty slice for anything else.
pub fn items(value: &Value) -> &[Value] {
    match value.as_array() {
        Some(list) => list.as_slice(),
        None => &[],
    }
}

/// Mirrors `fields(value, required, optional)`: a plain object whose keys are all
/// declared, with every required key present and passing its check, and every
/// optional key that is present passing its check.
pub fn fields(value: &Value, required: &[(&str, Check)], optional: &[(&str, Check)]) -> bool {
    let Some(map) = value.as_object() else {
        return false;
    };
    let declared = |list: &[(&str, Check)], key: &str| list.iter().any(|(name, _)| *name == key);
    if !map
        .keys()
        .all(|key| declared(required, key) || declared(optional, key))
    {
        return false;
    }
    required
        .iter()
        .all(|(key, check)| map.get(*key).is_some_and(check))
        && optional
            .iter()
            .all(|(key, check)| map.get(*key).is_none_or(check))
}

pub fn text(value: &Value) -> bool {
    value.as_str().is_some_and(|s| !s.is_empty())
}

pub fn any_string(value: &Value) -> bool {
    value.is_string()
}

pub fn nullable_text(value: &Value) -> bool {
    value.is_null() || text(value)
}

pub fn boolean(value: &Value) -> bool {
    value.is_boolean()
}

pub fn nullable_boolean(value: &Value) -> bool {
    value.is_null() || value.is_boolean()
}

/// `Number.isSafeInteger(v) && v >= 0`. JSON `1.0` counts, as it does in JavaScript.
pub fn safe_ordinal(value: &Value) -> bool {
    value
        .as_f64()
        .is_some_and(|n| n.fract() == 0.0 && (0.0..=MAX_SAFE_INTEGER).contains(&n))
}

pub fn strings(value: &Value) -> bool {
    value.as_array().is_some_and(|list| list.iter().all(text))
}

/// Lowercase SHA-256 hex digest (`/^[a-f0-9]{64}$/`).
pub fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

pub fn digest(value: &Value) -> bool {
    value.as_str().is_some_and(is_hex64)
}

/// `^\d+(?:\.\d+)*$`.
pub fn is_dotted_version(s: &str) -> bool {
    !s.is_empty()
        && s.split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

pub fn version(value: &Value) -> bool {
    value.as_str().is_some_and(is_dotted_version)
}

/// JavaScript truthiness for JSON values.
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// Canonical JSON: object keys sorted, no insignificant whitespace.
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Array(list) => {
            out.push('[');
            for (index, item) in list.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String((*key).clone()).to_string());
                out.push(':');
                write_canonical(&map[*key], out);
            }
            out.push('}');
        }
        Value::Number(number) => out.push_str(&js_number(number)),
        scalar => out.push_str(&scalar.to_string()),
    }
}

/// `JSON.stringify` for a number: integral floats print without a fraction.
fn js_number(number: &serde_json::Number) -> String {
    if number.is_f64() {
        if let Some(n) = number.as_f64() {
            if n.fract() == 0.0 && n.abs() < 1e21 {
                return format!("{n:.0}");
            }
        }
    }
    number.to_string()
}

/// CLDR root collation rank of an ASCII punctuation or symbol character, which sort
/// before digits and letters in this order.
const PUNCTUATION_ORDER: &str = "_-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";

fn primary(c: char) -> (u32, u32) {
    if c.is_ascii_whitespace() {
        return (0, c as u32);
    }
    if let Some(rank) = PUNCTUATION_ORDER.find(c) {
        return (1, rank as u32);
    }
    if c.is_ascii_digit() {
        return (2, c as u32);
    }
    if c.is_ascii_alphabetic() {
        return (3, c.to_ascii_lowercase() as u32);
    }
    (4, c.to_lowercase().next().map_or(c as u32, |l| l as u32))
}

/// `String.prototype.localeCompare` under the default ICU root collation, as Node uses
/// it to order paths (CONTRACT A7). Exact for ASCII: primary weights are compared across
/// the whole string first, then case (lowercase before uppercase). Non-ASCII characters
/// order after ASCII letters by code point, which is an approximation.
pub fn locale_compare(a: &str, b: &str) -> std::cmp::Ordering {
    let key = |s: &str| s.chars().map(primary).collect::<Vec<_>>();
    key(a).cmp(&key(b)).then_with(|| {
        let case = |s: &str| {
            s.chars()
                .map(|c| u8::from(c.is_uppercase()))
                .collect::<Vec<_>>()
        };
        case(a).cmp(&case(b))
    })
}

/// Lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// SHA-256 over the canonical JSON of `value` (Node `sha256(value)`).
pub fn sha256_json(value: &Value) -> String {
    sha256_hex(canonical_json(value).as_bytes())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn locale_compare_matches_icu_for_the_fixture_paths() {
        let mut paths = vec![
            "Cargo.toml",
            "Cargo.lock",
            "src/main.rs",
            "A.rs",
            "a-b.rs",
            "a_b.rs",
            "B.rs",
            "c.rs",
            "Z.rs",
        ];
        paths.sort_by(|a, b| locale_compare(a, b));
        assert_eq!(
            paths,
            [
                "a_b.rs",
                "a-b.rs",
                "A.rs",
                "B.rs",
                "c.rs",
                "Cargo.lock",
                "Cargo.toml",
                "src/main.rs",
                "Z.rs"
            ]
        );
    }

    #[test]
    fn canonical_json_sorts_keys_and_is_stable() {
        let a = json!({"b": [1, {"y": null, "x": true}], "a": "s"});
        assert_eq!(
            canonical_json(&a),
            r#"{"a":"s","b":[1,{"x":true,"y":null}]}"#
        );
    }

    #[test]
    fn safe_ordinal_accepts_integral_floats_like_javascript() {
        assert!(safe_ordinal(&json!(0)));
        assert!(safe_ordinal(&json!(1.0)));
        assert!(!safe_ordinal(&json!(-1)));
        assert!(!safe_ordinal(&json!(1.5)));
        assert!(!safe_ordinal(&json!("1")));
    }
}
