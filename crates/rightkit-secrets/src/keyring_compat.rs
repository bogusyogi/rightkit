//! Blob codec for [`WindowsTarget::KeyringCompat`](crate::WindowsTarget::KeyringCompat).
//!
//! The `keyring` crate's windows-native store writes the credential blob as UTF-16LE with no
//! terminator and reads it back with `String::from_utf16`. These pure functions keep
//! `rightkit-secrets` byte-compatible with it. They compile on every platform; only the
//! Windows backend calls them.
//!
//! The codec is selected by the namespace, never by looking at the bytes: a blob is UTF-16LE
//! text or it is an error. Guessing between UTF-16LE and UTF-8 cannot be made reliable, because
//! some text is the same bytes either way (U+4142 is `42 41`, which is also the UTF-8 text
//! "BA"). Blobs that rightkit-secrets 0.1.1 and 0.1.2 wrote as raw UTF-8 are read through the
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
    String::from_utf16(&units)
        .map(String::into_bytes)
        .map_err(|_| Error::NotUtf8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16le(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
    }

    #[test]
    fn ascii_is_utf16le_without_terminator() {
        assert_eq!(encode(b"abc").unwrap(), vec![b'a', 0, b'b', 0, b'c', 0]);
        assert_eq!(decode(&utf16le("abc")).unwrap(), b"abc");
    }

    #[test]
    fn round_trip_non_ascii_and_emoji() {
        for s in [
            "caf\u{e9}",
            "\u{65e5}\u{672c}\u{8a9e}",
            "key \u{1f511} ok",
            "a\u{1f600}",
        ] {
            let blob = encode(s.as_bytes()).unwrap();
            assert_eq!(blob, utf16le(s));
            assert_eq!(decode(&blob).unwrap(), s.as_bytes());
        }
    }

    #[test]
    fn utf16le_text_whose_bytes_are_also_valid_utf8_reads_back_as_the_original() {
        // Audit finding: U+4142 is written as `42 41`, which the old heuristic read as the UTF-8
        // text "BA". The encoding is selected by the namespace, so it must round-trip.
        let blob = encode("\u{4142}".as_bytes()).unwrap();
        assert_eq!(blob, [0x42, 0x41]);
        assert_eq!(decode(&blob).unwrap(), "\u{4142}".as_bytes());
        // The same holds for any text whose UTF-16LE bytes contain no NUL and form valid UTF-8:
        // BMP characters whose high and low bytes are both ASCII (0x01..=0x7f).
        for s in [
            "\u{4142}\u{4344}",
            "\u{5a5a}",
            "\u{3031}\u{3233}",
            "\u{7e7e}",
        ] {
            let blob = encode(s.as_bytes()).unwrap();
            assert!(!blob.contains(&0));
            assert_eq!(decode(&blob).unwrap(), s.as_bytes(), "{s:?}");
        }
    }

    #[test]
    fn empty_string() {
        assert!(encode(b"").unwrap().is_empty());
        assert!(decode(b"").unwrap().is_empty());
    }

    #[test]
    fn odd_length_is_not_utf16_and_is_an_error() {
        assert_eq!(decode(&[b'a', 0, b'b']), Err(Error::NotUtf8));
    }

    #[test]
    fn a_blob_that_is_not_valid_utf16le_is_an_error_and_never_guessed_as_utf8() {
        // An unpaired surrogate.
        assert_eq!(decode(&[0x00, 0xd8, 0x41, 0x00]), Err(Error::NotUtf8));
        // 0.1.x raw UTF-8 text of odd length is not read here (use KeyringCompatLegacyUtf8).
        assert_eq!(decode(b"abcde"), Err(Error::NotUtf8));
    }

    #[test]
    fn a_secret_that_is_not_utf8_text_cannot_be_written() {
        assert_eq!(encode(&[0xff, 0xfe, 0xfd]), Err(Error::NotUtf8));
        assert_eq!(encode(&[b'a', 0xc0]), Err(Error::NotUtf8));
    }

    #[test]
    fn encode_then_decode_is_the_identity_for_every_scalar_boundary() {
        for s in [
            "\u{0}",
            "\u{7f}",
            "\u{80}",
            "\u{7ff}",
            "\u{800}",
            "\u{d7ff}",
            "\u{e000}",
            "\u{ffff}",
            "\u{10000}",
            "\u{10ffff}",
        ] {
            assert_eq!(
                decode(&encode(s.as_bytes()).unwrap()).unwrap(),
                s.as_bytes()
            );
        }
    }
}
