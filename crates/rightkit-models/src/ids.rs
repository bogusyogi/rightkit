//! Validated identifiers. Every on-disk path the store builds comes from one
//! of these types, so a hostile manifest or CLI argument can never name a
//! location outside the store root.

use crate::error::{Error, ErrorCode, Result};
use std::fmt;

const MAX_ID: usize = 128;
const MAX_KEY: usize = 200;

/// A model identifier: one directory name under the store root.
///
/// Accepts ASCII letters, digits, `.`, `_`, `-`; 1..=128 bytes; no leading or
/// trailing `.`; not a Windows reserved device name; not ending in `.lock`
/// (reserved for the store's own lock files).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModelId(String);

impl ModelId {
    pub fn parse(id: &str) -> Result<Self> {
        let bad = |why: &str| {
            Error::new(
                ErrorCode::InvalidSpec,
                format!("invalid model id {id:?}: {why}"),
            )
        };
        if id.is_empty() || id.len() > MAX_ID {
            return Err(bad("length must be 1..=128"));
        }
        if !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return Err(bad(
                "only ASCII letters, digits, '.', '_' and '-' are allowed",
            ));
        }
        if id.starts_with('.') || id.ends_with('.') {
            return Err(bad("must not start or end with '.'"));
        }
        if id.to_ascii_lowercase().ends_with(".lock") {
            return Err(bad("'.lock' suffix is reserved"));
        }
        if is_reserved_device(id) {
            return Err(bad("reserved device name"));
        }
        if id == "refs" {
            return Err(bad("'refs' is the reference registry directory"));
        }
        Ok(Self(id.to_string()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A revision directory key: the injective encoding produced by
/// [`encode_revision_key`] (`<revision>` or `<revision>@<quant>`).
///
/// Alphabet is `a-z 0-9 . - _ @` only (lowercase, so case-insensitive file
/// systems cannot merge two keys); `_` always introduces a two-digit lowercase
/// hex escape; at most one `@`; no empty part; no leading/trailing `.`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RevisionKey(String);

impl RevisionKey {
    pub fn parse(key: &str) -> Result<Self> {
        let bad = |why: &str| {
            Error::new(
                ErrorCode::InvalidSpec,
                format!("invalid revision key {key:?}: {why}"),
            )
        };
        if key.is_empty() || key.len() > MAX_KEY {
            return Err(bad("length must be 1..=200"));
        }
        let mut parts = 0usize;
        for part in key.split('@') {
            parts += 1;
            if part.is_empty() {
                return Err(bad("empty revision or quant part"));
            }
            if part.starts_with('.') || part.ends_with('.') {
                return Err(bad("part must not start or end with '.'"));
            }
        }
        if parts > 2 {
            return Err(bad("at most one '@'"));
        }
        let b = key.as_bytes();
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'@' => i += 1,
                b'_' => {
                    let ok = b
                        .get(i + 1..i + 3)
                        .is_some_and(|h| h.iter().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')));
                    if !ok {
                        return Err(bad("'_' must be followed by two lowercase hex digits"));
                    }
                    i += 3;
                }
                _ => return Err(bad("character outside the key alphabet")),
            }
        }
        // Windows device names are matched on the stem before the first '.'.
        if is_reserved_device(key.split('@').next().unwrap_or("")) {
            return Err(bad("reserved device name"));
        }
        Ok(Self(key.to_string()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RevisionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Injective, case-insensitive-safe encoding of one key component. Anything
/// outside `[a-z0-9-]` (and interior `.`) becomes `_xx` (lowercase hex of each
/// UTF-8 byte), so distinct inputs never collide.
fn encode_part(s: &str) -> String {
    let last = s.len().saturating_sub(1);
    let mut out = String::with_capacity(s.len());
    for (i, b) in s.bytes().enumerate() {
        let keep =
            matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-') || (b == b'.' && i != 0 && i != last);
        if keep {
            out.push(b as char);
        } else {
            out.push_str(&format!("_{b:02x}"));
        }
    }
    out
}

/// Directory key for a (revision, quant) pair; validated before returning.
pub fn encode_revision_key(revision: &str, quant: Option<&str>) -> Result<RevisionKey> {
    let mut key = encode_part(revision);
    if let Some(q) = quant {
        key.push('@');
        key.push_str(&encode_part(q));
    }
    RevisionKey::parse(&key)
}

fn is_reserved_device(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit())
}
