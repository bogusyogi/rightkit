//! Client-side signing helpers for the device and webhook schemes. The daemon
//! uses the same functions to verify, so a client built on this module cannot
//! drift from the server.

use ring::hmac;

use crate::util::{hex, sha256_hex};

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
