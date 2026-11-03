//! Blob codec for [`WindowsTarget::KeyringCompat`](crate::WindowsTarget::KeyringCompat).
//!
//! The `keyring` crate's windows-native store writes the credential blob as UTF-16LE with no
//! terminator and reads it back with `String::from_utf16`. These pure functions keep
//! `rightkit-credentials` byte-compatible with it. They compile on every platform; only the
//! Windows backend calls them.
//!
//! The codec is selected by the namespace, never by looking at the bytes: a blob is UTF-16LE
//! text or it is an error. Guessing between UTF-16LE and UTF-8 cannot be made reliable, because
//! some text is the same bytes either way (U+4142 is `42 41`, which is also the UTF-8 text
//! "BA"). Blobs that 0.1.1 and 0.1.2 (published as rightkit-secrets) wrote as raw UTF-8 are read through the
//! separate [`WindowsTarget::KeyringCompatLegacyUtf8`](crate::WindowsTarget::KeyringCompatLegacyUtf8)
//! namespace, as an explicit migration step.

use crate::error::{Error, Result};
use zeroize::Zeroizing;

/// UTF-8 text becomes UTF-16LE with no terminator. A secret that is not valid UTF-8 text cannot be
/// represented in this layout and is `Error::NotUtf8`: writing raw bytes beside UTF-16LE text is
/// exactly what made reads ambiguous.
pub(crate) fn encode(secret: &[u8]) -> Result<Vec<u8>> {
    let text = std::str::from_utf8(secret).map_err(|_| Error::NotUtf8)?;
    let units: Zeroizing<Vec<u16>> = Zeroizing::new(text.encode_utf16().collect());
    let mut out = Vec::with_capacity(units.len() * 2);
    for unit in units.iter() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    Ok(out)
}

/// A UTF-16LE blob becomes UTF-8 bytes. An odd length or an unpaired surrogate is
/// `Error::NotUtf8`. The empty blob is the empty secret.
pub(crate) fn decode(blob: &[u8]) -> Result<Vec<u8>> {
    if blob.is_empty() {
        return Ok(Vec::new());
    }
    let (pairs, rest) = blob.as_chunks::<2>();
    if !rest.is_empty() {
        return Err(Error::NotUtf8);
    }
    let units: Zeroizing<Vec<u16>> =
        Zeroizing::new(pairs.iter().map(|c| u16::from_le_bytes(*c)).collect());
    // One allocation sized for the worst case (3 UTF-8 bytes per UTF-16 unit; a surrogate pair is
    // 4 bytes for 2 units), so the buffer never grows: a growing `String` frees its replaced
    // allocations unwiped, and on an error (an unpaired surrogate after a long valid prefix) the
    // decoded prefix would be left behind in ordinary freed memory. `Zeroizing` wipes the whole
    // buffer on the error path; the stack scratch is wiped too.
    let mut out: Zeroizing<Vec<u8>> = Zeroizing::new(Vec::with_capacity(units.len() * 3));
    let mut scratch = Zeroizing::new([0u8; 4]);
    for decoded in char::decode_utf16(units.iter().copied()) {
        let c = decoded.map_err(|_| Error::NotUtf8)?;
        out.extend_from_slice(c.encode_utf8(&mut scratch[..]).as_bytes());
    }
    Ok(std::mem::take(&mut *out))
}
