//! Client-side signing helpers for the device and webhook schemes. The daemon
//! uses the same functions to verify, so a client built on this module cannot
//! drift from the server.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use ring::hmac;
use ring::signature::{Ed25519KeyPair, UnparsedPublicKey, ED25519};

use crate::remote::util::{hex, sha256_hex, unhex};

pub const H_TIMESTAMP: &str = "x-rk-timestamp";
pub const H_NONCE: &str = "x-rk-nonce";
pub const H_SIGNATURE: &str = "x-rk-signature";
pub const H_PROVIDER_SIGNATURE: &str = "x-provider-signature";
pub const H_PROVIDER_TIMESTAMP: &str = "x-provider-timestamp";

/// Canonical string a device signs with its Ed25519 key:
/// `METHOD \n path-and-query \n timestamp \n nonce \n hex(sha256(body))`.
pub fn canonical_request(
    method: &str,
    path_and_query: &str,
    timestamp: u64,
    nonce: &str,
    body: &[u8],
) -> String {
    format!(
        "{}\n{}\n{}\n{}\n{}",
        method.to_ascii_uppercase(),
        path_and_query,
        timestamp,
        nonce,
        sha256_hex(body)
    )
}

/// Hex HMAC-SHA256 over `timestamp "." body`, the provider webhook signature.
pub fn webhook_signature(secret: &[u8], timestamp: u64, body: &[u8]) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
    let mut ctx = hmac::Context::with_key(&key);
    ctx.update(timestamp.to_string().as_bytes());
    ctx.update(b".");
    ctx.update(body);
    hex(ctx.sign().as_ref())
}

/// Sign a request as a device: unpadded base64url of the Ed25519 signature over
/// [`canonical_request`]. The value of the [`H_SIGNATURE`] header. The daemon
/// verifies with [`verify_request`], which builds the same canonical string.
pub fn sign_request(
    key: &Ed25519KeyPair,
    method: &str,
    path_and_query: &str,
    timestamp: u64,
    nonce: &str,
    body: &[u8],
) -> String {
    let msg = canonical_request(method, path_and_query, timestamp, nonce, body);
    URL_SAFE_NO_PAD.encode(key.sign(msg.as_bytes()).as_ref())
}

/// Verify a raw (already decoded) device signature over the canonical string
/// of a request against a raw 32-byte Ed25519 public key. The only place the
/// daemon checks a device signature.
pub fn verify_request(
    public_key: &[u8],
    method: &str,
    path_and_query: &str,
    timestamp: u64,
    nonce: &str,
    body: &[u8],
    signature: &[u8],
) -> bool {
    let msg = canonical_request(method, path_and_query, timestamp, nonce, body);
    UnparsedPublicKey::new(&ED25519, public_key)
        .verify(msg.as_bytes(), signature)
        .is_ok()
}

/// Per-session upload digest key from its hex form (`digestKey` in the init response).
pub fn parse_digest_key(key_hex: &str) -> Option<[u8; 32]> {
    unhex(key_hex).and_then(|k| k.try_into().ok())
}

/// Hex keyed blake3 of one upload chunk (the `X-Chunk-Digest` value).
pub fn chunk_digest(key: &[u8; 32], data: &[u8]) -> String {
    blake3::keyed_hash(key, data).to_hex().to_string()
}

/// Incremental keyed blake3 for the whole-object digest sent to `upload.complete`.
pub fn object_hasher(key: &[u8; 32]) -> blake3::Hasher {
    blake3::Hasher::new_keyed(key)
}

/// Hex form of a finished [`object_hasher`].
pub fn object_digest(hasher: &blake3::Hasher) -> String {
    hasher.finalize().to_hex().to_string()
}
