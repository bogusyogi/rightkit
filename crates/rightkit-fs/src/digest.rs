//! SHA-256 digests and lowercase hex, behind the `digest` feature.
//!
//! Consolidates the file-digest and hex helpers that Right Suite crates each copied. File
//! digests stream in 64 KiB chunks, so large artifacts never load into memory.

use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

const CHUNK: usize = 64 * 1024;

/// Lowercase hex of `bytes`: two characters per byte, `0-9a-f` only.
pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// True when every byte of `text` is an ASCII hex digit (`0-9`, `a-f`, `A-F`). The empty string is
/// hex (zero bytes). Pure, no allocation. Callers that name a file after the text check the length
/// too, so a hostile id cannot name a path.
pub fn is_hex(text: &str) -> bool {
    text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Decode hex text (either case) into bytes. `None` when the length is odd or a character is not
/// an ASCII hex digit. The inverse of [`hex`] for lowercase input.
pub fn unhex(text: &str) -> Option<Vec<u8>> {
    if !is_hex(text) || !text.len().is_multiple_of(2) {
        return None;
    }
    text.as_bytes()
        .chunks(2)
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16)?;
            let low = (pair[1] as char).to_digit(16)?;
            Some(((high << 4) | low) as u8)
        })
        .collect()
}

/// Incremental SHA-256 over this crate's own `sha2`, so a consumer that hashes while it writes needs no
/// second `sha2` major. Feed it with [`Sha256Hasher::update`] (or as an [`io::Write`]), then take the
/// lowercase hex with [`Sha256Hasher::finalize`]. The result equals [`sha256_bytes`] of the
/// concatenated input.
#[derive(Clone, Default)]
pub struct Sha256Hasher(Sha256);

impl Sha256Hasher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Hash more input.
    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    /// The lowercase hex digest of everything fed so far.
    pub fn finalize(self) -> String {
        hex(self.0.finalize().as_slice())
    }

    /// The raw 32-byte digest.
    pub fn finalize_bytes(self) -> [u8; 32] {
        self.0.finalize().into()
    }
}

impl io::Write for Sha256Hasher {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Lowercase hex SHA-256 of `bytes`.
pub fn sha256_bytes(bytes: &[u8]) -> String {
    hex(Sha256::digest(bytes).as_slice())
}

/// Lowercase hex SHA-256 of everything `reader` yields, read to end-of-file.
pub fn sha256_reader<R: Read>(reader: &mut R) -> io::Result<String> {
    sha256_reader_counted(reader).map(|(digest, _)| digest)
}

/// Lowercase hex SHA-256 of the file at `path`, streamed.
pub fn sha256_file(path: &Path) -> io::Result<String> {
    sha256_file_with_len(path).map(|(digest, _)| digest)
}

/// As [`sha256_file`], also returning the number of bytes that were hashed.
pub fn sha256_file_with_len(path: &Path) -> io::Result<(String, u64)> {
    let mut file = File::open(path)?;
    sha256_reader_counted(&mut file)
}

fn sha256_reader_counted<R: Read>(reader: &mut R) -> io::Result<(String, u64)> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; CHUNK];
    let mut total = 0u64;
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        total += n as u64;
    }
    Ok((hex(hasher.finalize().as_slice()), total))
}

#[cfg(test)]
mod tests {
    use super::*;

    // FIPS 180-2 test vector for "abc".
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn hex_is_lowercase_two_chars_per_byte() {
        assert_eq!(hex(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
        assert_eq!(hex(&[]), "");
    }

    #[test]
    fn unhex_inverts_hex_and_accepts_either_case() {
        let bytes = [0x00u8, 0x0f, 0xa0, 0xff];
        assert_eq!(unhex(&hex(&bytes)), Some(bytes.to_vec()));
        assert_eq!(unhex("0FA0Ff"), Some(vec![0x0f, 0xa0, 0xff]));
        assert_eq!(unhex(""), Some(Vec::new()));
    }

    #[test]
    fn unhex_and_is_hex_refuse_odd_length_and_non_hex() {
        assert_eq!(unhex("abc"), None);
        assert_eq!(unhex("zz"), None);
        assert_eq!(unhex("0g"), None);
        assert_eq!(unhex("é0"), None);
        assert!(is_hex("0123456789abcdefABCDEF"));
        assert!(!is_hex("0x12"));
        assert!(!is_hex("../x"));
    }

    #[test]
    fn bytes_digest_matches_known_vector() {
        assert_eq!(sha256_bytes(b"abc"), ABC);
    }

    #[test]
    fn streamed_file_digest_matches_bytes_digest_across_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big");
        // Larger than one chunk so the loop runs more than once.
        let body: Vec<u8> = (0..(CHUNK * 2 + 17)).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &body).unwrap();
        let (digest, len) = sha256_file_with_len(&path).unwrap();
        assert_eq!(digest, sha256_bytes(&body));
        assert_eq!(len, body.len() as u64);
        assert_eq!(sha256_file(&path).unwrap(), digest);
    }

    #[test]
    fn missing_file_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = sha256_file(&dir.path().join("absent")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
