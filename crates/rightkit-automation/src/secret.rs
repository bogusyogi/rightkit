//! A secret the caller wants written into a password or card field ([`Action::FillSecret`]).
//!
//! The engine never types credentials: a `SetValue` or `TypeText` payload that looks like one,
//! and any secure or credential-labelled target, is refused. `FillSecret` is the one owned
//! route around that, and it keeps the reason for the refusal intact:
//!
//! * the secret travels only inside a [`Secret`], whose `Debug` prints nothing of the value,
//!   which is zeroed in memory when dropped, and which the receipt, the journal, the
//!   admission request and every error message never see (`Action::payload` is `None`);
//! * the write is tier 1 only (a semantic value write on the element): never a keystroke, never
//!   the clipboard, never a pointer event, so nothing else in the input stream or on the
//!   pasteboard can observe it;
//! * the field must belong to the page or app the caller named: the engine reads where the
//!   field lives ([`FieldOrigin`]) and refuses on any mismatch, and also when the origin cannot
//!   be read (fail closed);
//! * the value is not read back (a secure field does not return it, and reading it would put it
//!   in another process's answer): the receipt says the write was acknowledged, nothing more.

use crate::model::{Error, Result};

/// A secret string. Not `Display`; `Debug` is fixed text. The bytes are overwritten when the
/// value is dropped (best effort: copies made by the OS or the target app are out of reach).
pub struct Secret(Vec<u8>);

impl Secret {
    /// Take ownership of `value`. Empty and over-long (more than 4096 bytes) values are refused
    /// so a bug cannot clear a field or push an unbounded blob.
    pub fn new(value: String) -> Result<Secret> {
        if value.is_empty() {
            return Err(Error::Invalid("secret is empty".into()));
        }
        if value.len() > 4096 {
            return Err(Error::Invalid("secret is longer than 4096 bytes".into()));
        }
        Ok(Secret(value.into_bytes()))
    }
    /// The value, for the platform write only. Never log it.
    pub fn expose(&self) -> &str {
        // Built from a `String` and never mutated except by the zeroing drop.
        std::str::from_utf8(&self.0).unwrap_or("")
    }
}

impl Clone for Secret {
    fn clone(&self) -> Self {
        Secret(self.0.clone())
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(..)")
    }
}

impl PartialEq for Secret {
    fn eq(&self, other: &Self) -> bool {
        // Whole-length fold, so the comparison time does not depend on where they differ.
        self.0.len() == other.0.len()
            && self
                .0
                .iter()
                .zip(&other.0)
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        for b in self.0.iter_mut() {
            // SAFETY: `b` is a valid, aligned, exclusive reference; the volatile write keeps the
            // compiler from removing the zeroing as a dead store.
            unsafe { std::ptr::write_volatile(b, 0) };
        }
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }
}

/// Where a field lives, as far as the platform can read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldOrigin {
    /// Inside a web document: the origin (`scheme://host[:port]`, lowercase) of the nearest
    /// document that contains the field, so a field in a frame answers for that frame.
    Web(String),
    /// A native control, not web content: it belongs to the app that owns it.
    Native,
    /// The platform could not tell, or has no way to. Always refused.
    Unknown,
}

/// Reduce a URL to its origin: lowercase `scheme://host[:port]`, userinfo, path, query and
/// fragment dropped, default ports dropped. Only `http` and `https` yield one; `None` for
/// anything else (a `file:` page, `about:blank`, a missing host).
pub fn origin_of_url(url: &str) -> Option<String> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    if host_port.is_empty() || host_port.contains(char::is_whitespace) {
        return None;
    }
    let host_port = host_port.to_ascii_lowercase();
    let (host, port) = match host_port.rsplit_once(':') {
        // An IPv6 literal ends with `]`; its colons are not a port separator.
        Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => (h, Some(p)),
        _ => (host_port.as_str(), None),
    };
    if host.is_empty() {
        return None;
    }
    let default = if scheme == "https" { "443" } else { "80" };
    Some(match port {
        Some(p) if p != default => format!("{scheme}://{host}:{p}"),
        _ => format!("{scheme}://{host}"),
    })
}

/// Whether a field at `observed` may take a secret meant for `expected`. `expected` is a URL or
/// origin for a web field and the app id for a native one.
pub(crate) fn origin_matches(observed: &FieldOrigin, expected: &str, app_id: &str) -> bool {
    match observed {
        FieldOrigin::Web(o) => origin_of_url(expected).is_some_and(|e| e == *o),
        FieldOrigin::Native => expected.eq_ignore_ascii_case(app_id),
        FieldOrigin::Unknown => false,
    }
}
