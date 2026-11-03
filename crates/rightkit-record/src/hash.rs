//! Content addressing. Every hash in a bundle is `blake3:<64 lowercase hex>`.

use std::fs::File;
use std::io;
use std::path::Path;

pub const PREFIX: &str = "blake3:";
/// Prefix of a keyed digest: `hmac-sha256:<64 lowercase hex>`.
pub const HMAC_PREFIX: &str = "hmac-sha256:";

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

/// Length of a keyed digest, bytes.
const MAC_LEN: usize = 32;

/// HMAC-SHA256 of `message` under `key`.
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; MAC_LEN] {
    hmac_sha256::HMAC::mac(message, key)
}

/// Lowercase hex of `bytes`.
pub fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// The digest a `hash` redaction stores: HMAC-SHA256 over the field label and the
/// value, keyed by the per-installation secret, written `hmac-sha256:<64 hex>`
/// (record-event contract 1.2). The event's `saltId` is `hmac-sha256:<key id>`.
pub fn keyed_digest(key: &[u8], field: &str, value: &str) -> String {
    let mut message = Vec::with_capacity(field.len() + 1 + value.len());
    message.extend_from_slice(field.as_bytes());
    message.push(0);
    message.extend_from_slice(value.as_bytes());
    format!("{HMAC_PREFIX}{}", hex_lower(&hmac_sha256(key, &message)))
}
