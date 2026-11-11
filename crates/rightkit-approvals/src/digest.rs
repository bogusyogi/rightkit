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
