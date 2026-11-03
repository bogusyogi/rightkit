//! Canonical JSON for hashing and signing contract documents.
//!
//! The output follows the RFC 8785 (JCS) shape: no insignificant whitespace, object members
//! sorted by their UTF-16 code units, strings escaped as `serde_json` escapes them, and numbers
//! written in their shortest form. Two inputs that differ only in member order or whitespace
//! produce the same bytes, so a digest or signature over the output is stable across producers.
//!
//! Numbers are restricted to the forms where this writer is provably identical to ECMAScript
//! `Number` serialisation: integers, and finite floats whose shortest form has no exponent. Any
//! other number is refused with [`CanonicalError::UnsupportedNumber`] instead of being written in
//! a form a verifier might not reproduce.

use serde::Serialize;
use serde_json::Value;

/// Largest integer that an IEEE-754 double represents exactly (2^53).
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_992.0;

/// Why a value could not be written in canonical form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalError {
    /// The value could not be converted to a JSON value first.
    Serialize(String),
    /// A number has no canonical form this writer guarantees, such as an exponent form or a non-finite float.
    UnsupportedNumber(String),
}

impl std::fmt::Display for CanonicalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Serialize(message) => {
                write!(f, "canonical JSON: value does not serialise: {message}")
            }
            Self::UnsupportedNumber(number) => {
                write!(
                    f,
                    "canonical JSON: number {number} has no guaranteed canonical form"
                )
            }
        }
    }
}

impl std::error::Error for CanonicalError {}

/// Canonical JSON text for any serialisable value.
pub fn to_canonical_string<T: Serialize + ?Sized>(value: &T) -> Result<String, CanonicalError> {
    let value = serde_json::to_value(value)
        .map_err(|error| CanonicalError::Serialize(error.to_string()))?;
    canonical_value_string(&value)
}

/// Canonical JSON text for an existing [`serde_json::Value`].
pub fn canonical_value_string(value: &Value) -> Result<String, CanonicalError> {
    let mut out = String::new();
    write_value(value, &mut out)?;
    Ok(out)
}

fn write_value(value: &Value, out: &mut String) -> Result<(), CanonicalError> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => write_number(number.as_f64(), number, out)?,
        Value::String(text) => write_string(text, out)?,
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_value(item, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut members: Vec<(&String, &Value)> = map.iter().collect();
            members.sort_by(|left, right| left.0.encode_utf16().cmp(right.0.encode_utf16()));
            out.push('{');
            for (index, (key, member)) in members.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_string(key, out)?;
                out.push(':');
                write_value(member, out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

fn write_number(
    float: Option<f64>,
    number: &serde_json::Number,
    out: &mut String,
) -> Result<(), CanonicalError> {
    if let Some(integer) = number.as_i64() {
        out.push_str(&integer.to_string());
        return Ok(());
    }
    if let Some(unsigned) = number.as_u64() {
        out.push_str(&unsigned.to_string());
        return Ok(());
    }
    let Some(float) = float.filter(|value| value.is_finite()) else {
        return Err(CanonicalError::UnsupportedNumber(number.to_string()));
    };
    if float.fract() == 0.0 && float.abs() < MAX_SAFE_INTEGER {
        // Covers -0.0 as well: ECMAScript writes both zeros as "0".
        out.push_str(&(float as i64).to_string());
        return Ok(());
    }
    let shortest = serde_json::to_string(number)
        .map_err(|error| CanonicalError::Serialize(error.to_string()))?;
    if shortest.contains(['e', 'E']) {
        return Err(CanonicalError::UnsupportedNumber(shortest));
    }
    out.push_str(&shortest);
    Ok(())
}

fn write_string(text: &str, out: &mut String) -> Result<(), CanonicalError> {
    let quoted = serde_json::to_string(text)
        .map_err(|error| CanonicalError::Serialize(error.to_string()))?;
    out.push_str(&quoted);
    Ok(())
}
