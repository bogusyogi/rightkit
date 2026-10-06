//! Cache keys. A [`Key`] identifies a *slot* (e.g. account + message + attachment)
//! with an unambiguous, length-prefixed SHA-256, and optionally belongs to a
//! group so a whole account / thread / URL family can be invalidated at once.

use sha2::{Digest, Sha256};

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn digest(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        // Length prefix: ("ab","c") and ("a","bc") must never collide.
        h.update((p.len() as u64).to_le_bytes());
        h.update(p.as_bytes());
    }
    hex(h.finalize().as_slice())
}

/// Group id: first 16 hex chars of the digest of the group parts. 16 hex = 64 bits, ample for namespacing.
pub(crate) const NO_GROUP: &str = "0000000000000000";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key {
    pub(crate) hash: String,
    pub(crate) group: String,
}

impl Key {
    /// Key over ordered parts, no group. `Key::new(&[account, message, attachment])`.
    pub fn new(parts: &[&str]) -> Self {
        Self {
            hash: digest(parts),
            group: NO_GROUP.to_string(),
        }
    }

    /// Key over `parts`, belonging to the group identified by `group_parts`.
    pub fn grouped(group_parts: &[&str], parts: &[&str]) -> Self {
        let mut all: Vec<&str> = Vec::with_capacity(group_parts.len() + parts.len());
        all.extend_from_slice(group_parts);
        all.extend_from_slice(parts);
        Self {
            hash: digest(&all),
            group: group_id(group_parts),
        }
    }

    /// Key for a URL (the webimg pattern): hash of the URL string.
    pub fn url(url: &str) -> Self {
        Self::new(&[url])
    }

    pub fn hash(&self) -> &str {
        &self.hash
    }
}

/// Group id for [`crate::DiskCache::clear_group`].
pub fn group_id(group_parts: &[&str]) -> String {
    digest(group_parts)[..16].to_string()
}
