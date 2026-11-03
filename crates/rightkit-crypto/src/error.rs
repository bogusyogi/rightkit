//! One opaque error for the primitives that wrap `ring`: no key, nonce or plaintext material is
//! ever carried or formatted.

/// A cryptographic operation failed. Deliberately carries no detail: ring reports none, and a
/// distinguishing message on an open or verify failure would help an attacker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CryptoError;

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cryptographic operation failed")
    }
}

impl std::error::Error for CryptoError {}
