//! The `rk-pair/1` short-authenticated-string functions (#965): the commit-reveal commitment and
//! the six-digit code both devices display. This is the one owner; `rightkit-sync-keys` consumes
//! it, and `rightkit-daemon`'s remote pairing is to delegate to it (same domain strings, same
//! length-prefixed framing, same SHA-256, same "first four bytes, big-endian, modulo 10^6").
//!
//! Roles: device D (`public_key` = its ephemeral public key, `label` = its device name) sends
//! the commitment first; host H picks `nonce_h`; `host_binding` carries H's public key and the
//! account context. The code therefore commits to both keys, both nonces, the label and the
//! context, with commit-reveal ordering.

use crate::util::{hex, sha256};

const COMMIT_DOMAIN: &[u8] = b"rk-pair/1 commit";
const SAS_DOMAIN: &[u8] = b"rk-pair/1 sas";

/// Length of both nonces, in bytes.
pub const NONCE_BYTES: usize = 16;
/// Longest accepted device label, in characters. Over this the host refuses: a label is bound
/// into the commitment, so it is never truncated.
pub const MAX_LABEL_CHARS: usize = 64;
/// Digits in the code.
pub const CODE_DIGITS: usize = 6;

/// Length-prefixed concatenation, so no two different field lists share an encoding.
fn framed(parts: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for part in parts {
        out.extend_from_slice(&(part.len() as u32).to_be_bytes());
        out.extend_from_slice(part);
    }
    out
}

/// A present empty label differs from an absent one.
pub fn label_bytes(label: Option<&str>) -> Vec<u8> {
    match label {
        Some(l) => [&[1u8][..], l.as_bytes()].concat(),
        None => vec![0u8],
    }
}

/// Hex commitment D sends first.
pub fn pairing_commit(public_key: &[u8], nonce_d: &[u8], label: Option<&str>) -> String {
    let lb = label_bytes(label);
    hex(&sha256(&framed(&[COMMIT_DOMAIN, public_key, nonce_d, &lb])))
}

/// The six-digit code both sides display, from the whole transcript.
pub fn pairing_code(
    public_key: &[u8],
    nonce_d: &[u8],
    nonce_h: &[u8],
    label: Option<&str>,
    host_binding: &[u8],
) -> String {
    let lb = label_bytes(label);
    let d = sha256(&framed(&[
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
