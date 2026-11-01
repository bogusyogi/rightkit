//! Small shared helpers: hex decoding, time, randomness.

use std::fmt::Display;
use std::time::{SystemTime, UNIX_EPOCH};

use ring::rand::{SecureRandom, SystemRandom};

use crate::error::MeshError;

/// True when `text` is exactly `len` lowercase hex characters. Used before any id becomes a
/// file name, so a hostile id cannot name a path.
pub(crate) fn is_hex(text: &str, len: usize) -> bool {
    text.len() == len && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

pub(crate) fn unhex<const N: usize>(text: &str) -> Result<[u8; N], MeshError> {
    if !is_hex(text, N * 2) {
        return Err(MeshError::Malformed("hex"));
    }
    rightkit_fs::digest::unhex(text)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(MeshError::Malformed("hex"))
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub(crate) fn random<const N: usize>() -> Result<[u8; N], MeshError> {
    let mut out = [0u8; N];
    SystemRandom::new()
        .fill(&mut out)
        .map_err(|_| MeshError::Crypto("randomness unavailable"))?;
    Ok(out)
}

/// A stream, socket or iroh error as a [`MeshError`]. Only the message is kept.
pub(crate) fn stream_err(error: impl Display) -> MeshError {
    MeshError::Stream(error.to_string())
}
