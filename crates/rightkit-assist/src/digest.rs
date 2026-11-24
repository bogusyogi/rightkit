//! Digests over `rightkit-fs`'s SHA-256, with every part length-prefixed so two different part lists can
//! never feed the same bytes.

use rightkit_fs::digest::Sha256Hasher;

/// Lowercase hex SHA-256 of the length-prefixed `parts`.
pub fn digest(parts: &[&str]) -> String {
    let mut hasher = Sha256Hasher::new();
    for part in parts {
        hasher.update(&(part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    hasher.finalize()
}

/// The first 16 hex characters of [`digest`], for readable ids. 64 bits is ample for the few thousand
/// rows one owner keeps; keys that must be collision-proof use the full digest.
pub fn short(parts: &[&str]) -> String {
    let full = digest(parts);
    full.chars().take(16).collect()
}
