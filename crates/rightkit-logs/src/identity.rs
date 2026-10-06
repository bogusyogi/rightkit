//! Random identifiers. Install ids are random (never derived from hardware,
//! user name or account) and live in a file the user can delete to reset.

use std::fs;
use std::io;
use std::path::Path;

/// RFC 4122 version-4 UUID from the OS random source.
pub fn random_uuid() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        // OS RNG unavailable: fall back to time+pid entropy. Identifier only,
        // never a secret.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        bytes.copy_from_slice(&(nanos ^ ((std::process::id() as u128) << 64)).to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Load the persisted pseudonymous install id, creating it on first use.
pub fn install_id(path: &Path) -> io::Result<String> {
    if let Ok(existing) = fs::read_to_string(path) {
        let existing = existing.trim();
        if existing.len() == 36 && existing.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
            return Ok(existing.to_string());
        }
    }
    let id = random_uuid();
    crate::atomic::write_atomic(path, id.as_bytes())?;
    Ok(id)
}
