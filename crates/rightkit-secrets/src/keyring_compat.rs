//! Blob codec for [`WindowsTarget::KeyringCompat`](crate::WindowsTarget::KeyringCompat).
//!
//! The `keyring` crate's windows-native store writes the credential blob as UTF-16LE with no
//! terminator and reads it back with `String::from_utf16`. These pure functions keep
//! `rightkit-secrets` byte-compatible with it. They compile on every platform; only the
//! Windows backend calls them.

use zeroize::Zeroizing;

/// UTF-8 text becomes UTF-16LE (no terminator); anything else is written as raw bytes.
pub(crate) fn encode(secret: &[u8]) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(secret) else {
        return secret.to_vec();
    };
    let units: Zeroizing<Vec<u16>> = Zeroizing::new(text.encode_utf16().collect());
    let mut out = Vec::with_capacity(units.len() * 2);
    for unit in units.iter() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

/// A blob that is even-length, non-empty, valid UTF-16LE, and not plain UTF-8 text (UTF-8 text
/// never contains NUL; UTF-16 ASCII always does) is decoded to UTF-8 bytes. Everything else
/// (0.1.x UTF-8 writes, binary, odd length) passes through unchanged.
pub(crate) fn decode(blob: &[u8]) -> Vec<u8> {
    if blob.is_empty() || !blob.len().is_multiple_of(2) {
        return blob.to_vec();
    }
    let plain_utf8 = !blob.contains(&0) && std::str::from_utf8(blob).is_ok();
    if plain_utf8 {
        return blob.to_vec();
    }
    let units: Zeroizing<Vec<u16>> = Zeroizing::new(
        blob.as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect(),
    );
    match String::from_utf16(&units) {
        Ok(text) => text.into_bytes(),
        Err(_) => blob.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16le(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
    }

    #[test]
    fn ascii_is_utf16le_without_terminator() {
        assert_eq!(encode(b"abc"), vec![b'a', 0, b'b', 0, b'c', 0]);
        assert_eq!(decode(&utf16le("abc")), b"abc");
    }

    #[test]
    fn round_trip_non_ascii_and_emoji() {
        for s in [
            "caf\u{e9}",
            "\u{65e5}\u{672c}\u{8a9e}",
            "key \u{1f511} ok",
            "a\u{1f600}",
        ] {
            let blob = encode(s.as_bytes());
            assert_eq!(blob, utf16le(s));
            assert_eq!(decode(&blob), s.as_bytes());
        }
    }

    #[test]
    fn empty_string() {
        assert!(encode(b"").is_empty());
        assert!(decode(b"").is_empty());
    }

    #[test]
    fn odd_length_passes_through() {
        let blob = [b'a', 0, b'b'];
        assert_eq!(decode(&blob), blob);
    }

    #[test]
    fn legacy_utf8_blob_passes_through() {
        assert_eq!(decode(b"abcd"), b"abcd");
        let s = "caf\u{e9}!"; // even-length UTF-8 with non-ASCII
        assert_eq!(s.len() % 2, 0);
        assert_eq!(decode(s.as_bytes()), s.as_bytes());
    }

    #[test]
    fn binary_passes_through() {
        // Odd-length non-UTF-8 bytes cannot be UTF-16, so they pass through.
        // An even-length binary blob that happens to be valid UTF-16 is
        // indistinguishable from keyring text; KeyringCompat stores text.
        let bin = [0xff, 0xfe, 0xfd];
        assert_eq!(encode(&bin), bin);
        assert_eq!(decode(&bin), bin);
        // Lone surrogate (not valid UTF-16) with a NUL byte.
        let bad = [0x00, 0xd8, 0x41, 0x00];
        assert_eq!(decode(&bad), bad);
    }
}
