//! The binding digest and request ids.

use crate::error::{Refusal, Result};

/// Hex SHA-256 of `parts`, each prefixed with its length (8 bytes, big-endian), so two different
/// splits never hash alike. This is the digest every approval binds: the exact arguments of an
/// act, and (through [`crate::Request::display_digest`]) what the decider was shown.
pub fn bind_of(parts: &[&[u8]]) -> String {
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    for part in parts {
        hasher.update(&(part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    hasher.finalize()
}

/// Hex SHA-256 of `bytes`, for an app that hashes one canonical form (for example canonical
/// JSON) instead of parts.
pub fn sha256_hex(bytes: &[u8]) -> String {
    rightkit_fs::digest::sha256_bytes(bytes)
}

/// A new request id: 16 bytes from the operating system's random source, lowercase hex. If the
/// source fails there is no id (`approval_entropy`), so no request is filed.
pub fn new_id() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| {
        Refusal::new(
            "approval_entropy",
            format!("the system random source failed ({e}); no approval request was filed"),
        )
    })?;
    Ok(rightkit_fs::digest::hex(&bytes))
}

/// Domain tag of [`args_digest`].
const ARGS_DOMAIN: &[u8] = b"rightkit.approval.args.v1";

/// The canonical text of `value`: no whitespace, object keys sorted by their UTF-8 bytes at every
/// depth, strings escaped as `serde_json` escapes them, numbers as `serde_json` prints them. Two
/// values that differ in any key, element, string or number never share a canonical text, and the
/// text does not depend on the order the keys were inserted in (it does not rely on
/// `serde_json`'s `preserve_order` feature being off). `1` and `1.0` are different numbers here:
/// a caller that means them as equal normalises before it hashes.
pub fn canonical_json(value: &serde_json::Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &serde_json::Value, out: &mut String) {
    use serde_json::Value;
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => out.push_str(&value.to_string()),
        Value::String(text) => out.push_str(&Value::String(text.clone()).to_string()),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                if let Some(item) = map.get(key.as_str()) {
                    write_canonical(item, out);
                }
            }
            out.push('}');
        }
    }
}

/// The digest an approval binds for an act whose arguments are JSON (a message to send, a form to
/// submit): hex SHA-256, domain-tagged, of [`canonical_json`]. Put it in [`crate::Binding::args`].
/// Any change to the arguments, a draft's recipient, subject or body included, changes it, and an
/// approval bound to the old one is then void ([`crate::authority::recheck`]).
pub fn args_digest(value: &serde_json::Value) -> String {
    bind_of(&[ARGS_DOMAIN, canonical_json(value).as_bytes()])
}
