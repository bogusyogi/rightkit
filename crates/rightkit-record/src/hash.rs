//! Content addressing. Every hash in a bundle is `blake3:<64 lowercase hex>`.

use std::fs::File;
use std::io;
use std::path::Path;

pub const PREFIX: &str = "blake3:";

pub fn blake3_text(bytes: &[u8]) -> String {
    format!("{PREFIX}{}", blake3::hash(bytes).to_hex())
}

pub fn is_blake3_text(value: &str) -> bool {
    value.strip_prefix(PREFIX).is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// Streams a file through blake3, so a long media file never sits in memory.
pub fn file_hash(path: &Path) -> io::Result<(String, u64)> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let bytes = io::copy(&mut file, &mut hasher)?;
    Ok((format!("{PREFIX}{}", hasher.finalize().to_hex()), bytes))
}

/// The digest a `hash` redaction stores: label-salted, so equal values under
/// different fields do not collide.
pub fn salted_digest(salt_id: &str, value: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(salt_id.as_bytes());
    hasher.update(&[0]);
    hasher.update(value.as_bytes());
    format!("{PREFIX}{}", hasher.finalize().to_hex())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_canonical_blake3_text() {
        assert!(is_blake3_text(&blake3_text(b"frame")));
        assert!(!is_blake3_text("blake3:ABCD"));
        assert!(!is_blake3_text(&format!("sha256:{}", "a".repeat(64))));
        assert!(!is_blake3_text(&format!("blake3:{}", "A".repeat(64))));
    }

    #[test]
    fn salt_separates_labels() {
        assert_ne!(
            salted_digest("target.label", "x"),
            salted_digest("text", "x")
        );
    }
}
