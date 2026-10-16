//! LOCAL COPY, TO BE REPLACED by a shared extraction.
//!
//! These are the transport-free functions of `rk-pair/1` from
//! `rightkit-daemon/src/remote/pairing.rs` (`framed`, `label_bytes`, `pairing_commit`,
//! `pairing_code`, and the constants), copied byte for byte in behaviour: same domain strings,
//! same length-prefixed framing, same SHA-256, same "first four bytes, big-endian, modulo 10^6".
//! The only difference is the hash implementation (`sha2` here, `ring` there); the outputs are
//! identical, and `tests::rk_pair_1_vectors` pins them.
//!
//! Why a copy: the daemon's pairing module is behind its `remote` feature (rustls, tokio, ...)
//! and is not published yet, so this crate cannot depend on it. Minimal extraction that retires
//! this file: move those four items into a tiny dependency-light crate (for example
//! `rightkit-pairing`, dependencies: `sha2` or `ring`), have `rightkit-daemon::remote::pairing`
//! re-export them, and have this crate depend on it. No behaviour change on either side.
//!
//! How this crate uses rk-pair/1 (see [`crate::transfer`]): the joining device plays the daemon's
//! device D (`public_key` = its ephemeral X25519 public key, `label` = its device name), the
//! approving device plays the host H (picks `nonce_h`), and `host_binding` carries the approver's
//! X25519 public key and the account context. The code therefore commits to both X25519 public
//! keys, both nonces, the label and the context, with the same commit-reveal ordering.

use crate::util::{hex_encode, push_lp};
use sha2::{Digest, Sha256};

const COMMIT_DOMAIN: &[u8] = b"rk-pair/1 commit";
const SAS_DOMAIN: &[u8] = b"rk-pair/1 sas";

/// Length of both nonces, in bytes.
pub(crate) const NONCE_BYTES: usize = 16;
/// Longest accepted device label, in characters.
pub(crate) const MAX_LABEL_CHARS: usize = 64;
/// Digits in the code.
pub(crate) const CODE_DIGITS: usize = 6;

fn framed(parts: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for part in parts {
        push_lp(&mut out, part);
    }
    out
}

pub(crate) fn label_bytes(label: Option<&str>) -> Vec<u8> {
    // A present empty label differs from an absent one.
    match label {
        Some(l) => [&[1u8][..], l.as_bytes()].concat(),
        None => vec![0u8],
    }
}

/// Hex commitment D sends first.
pub(crate) fn pairing_commit(public_key: &[u8], nonce_d: &[u8], label: Option<&str>) -> String {
    let lb = label_bytes(label);
    hex_encode(&Sha256::digest(framed(&[
        COMMIT_DOMAIN,
        public_key,
        nonce_d,
        &lb,
    ])))
}

/// The six-digit code both sides display, from the whole transcript.
pub(crate) fn pairing_code(
    public_key: &[u8],
    nonce_d: &[u8],
    nonce_h: &[u8],
    label: Option<&str>,
    host_binding: &[u8],
) -> String {
    let lb = label_bytes(label);
    let d = Sha256::digest(framed(&[
        SAS_DOMAIN,
        public_key,
        nonce_d,
        nonce_h,
        &lb,
        host_binding,
    ]));
    let n = u32::from_be_bytes([d[0], d[1], d[2], d[3]]) % 1_000_000;
    format!("{n:06}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_changes_with_every_transcript_field() {
        let (k, nd, nh) = ([1u8; 32], [2u8; 16], [3u8; 16]);
        let base = pairing_code(&k, &nd, &nh, Some("a"), b"b");
        assert_eq!(base.len(), CODE_DIGITS);
        assert_eq!(base, pairing_code(&k, &nd, &nh, Some("a"), b"b"));
        assert_ne!(base, pairing_code(&[9u8; 32], &nd, &nh, Some("a"), b"b"));
        assert_ne!(base, pairing_code(&k, &[9u8; 16], &nh, Some("a"), b"b"));
        assert_ne!(base, pairing_code(&k, &nd, &[9u8; 16], Some("a"), b"b"));
        assert_ne!(base, pairing_code(&k, &nd, &nh, Some("z"), b"b"));
        assert_ne!(base, pairing_code(&k, &nd, &nh, None, b"b"));
        assert_ne!(base, pairing_code(&k, &nd, &nh, Some("a"), b"c"));
    }

    #[test]
    fn framing_keeps_field_boundaries() {
        assert_ne!(
            pairing_commit(&[1, 2], &[3], None),
            pairing_commit(&[1], &[2, 3], None)
        );
        assert_ne!(
            pairing_commit(&[1], &[2], Some("")),
            pairing_commit(&[1], &[2], None)
        );
    }

    /// Pins the encoding to the daemon's definition: SHA-256 over u32-BE length-prefixed fields,
    /// label as a presence byte plus bytes. Recomputed here by hand, not through `framed`.
    #[test]
    fn rk_pair_1_vectors() {
        let mut m = Vec::new();
        for f in [&b"rk-pair/1 commit"[..], &[7u8; 32], &[8u8; 16], &[1, b'x']] {
            m.extend_from_slice(&(f.len() as u32).to_be_bytes());
            m.extend_from_slice(f);
        }
        assert_eq!(
            pairing_commit(&[7u8; 32], &[8u8; 16], Some("x")),
            hex_encode(&Sha256::digest(&m))
        );
    }
}
