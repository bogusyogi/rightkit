//! Signatures: Ed25519 sign and verify, ECDSA P-256 verify, RSA PKCS#1 v1.5 SHA-256 verify.
//!
//! Backed by `ring`. Every verify takes raw bytes and returns a plain `bool`; a malformed key and a
//! bad signature are the same answer.

use ring::signature::{
    Ed25519KeyPair, KeyPair, UnparsedPublicKey, ECDSA_P256_SHA256_ASN1, ECDSA_P256_SHA256_FIXED,
    ED25519, RSA_PKCS1_2048_8192_SHA256,
};

use crate::{CryptoError, Secret};

/// An Ed25519 signing key.
pub struct Ed25519Signer {
    pair: Ed25519KeyPair,
}

impl Ed25519Signer {
    /// A new key from the system random generator. Returns the signer and its PKCS#8 v2 document
    /// (the form [`Ed25519Signer::from_pkcs8`] reads), held as a wiped [`Secret`].
    pub fn generate() -> Result<(Self, Secret), CryptoError> {
        let rng = ring::rand::SystemRandom::new();
        let doc = Ed25519KeyPair::generate_pkcs8(&rng).map_err(|_| CryptoError)?;
        let secret = Secret::new(doc.as_ref().to_vec());
        let pair = Ed25519KeyPair::from_pkcs8(secret.expose()).map_err(|_| CryptoError)?;
        Ok((Self { pair }, secret))
    }

    /// Load a key from a PKCS#8 v2 document (the form `generate` returns).
    pub fn from_pkcs8(pkcs8: &[u8]) -> Result<Self, CryptoError> {
        Ed25519KeyPair::from_pkcs8(pkcs8)
            .map(|pair| Self { pair })
            .map_err(|_| CryptoError)
    }

    /// Load a key from a 32-byte seed (RFC 8032 private key).
    pub fn from_seed(seed: &[u8]) -> Result<Self, CryptoError> {
        Ed25519KeyPair::from_seed_unchecked(seed)
            .map(|pair| Self { pair })
            .map_err(|_| CryptoError)
    }

    /// The 32-byte public key.
    pub fn public_key(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        out.copy_from_slice(self.pair.public_key().as_ref());
        out
    }

    /// The 64-byte signature of `message`.
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        let mut out = [0u8; 64];
        out.copy_from_slice(self.pair.sign(message).as_ref());
        out
    }
}

impl std::fmt::Debug for Ed25519Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Ed25519Signer(<redacted>)")
    }
}

/// Verify an Ed25519 `signature` (64 bytes) over `message` under the 32-byte `public_key`.
pub fn ed25519_verify(public_key: &[u8], message: &[u8], signature: &[u8]) -> bool {
    UnparsedPublicKey::new(&ED25519, public_key)
        .verify(message, signature)
        .is_ok()
}

/// Encoding of an ECDSA P-256 signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EcdsaEncoding {
    /// ASN.1 DER `SEQUENCE { r, s }`.
    Asn1,
    /// Fixed 64 bytes, `r || s`.
    Fixed,
}

/// Verify an ECDSA P-256 / SHA-256 `signature` over `message` under `public_key` (65-byte
/// uncompressed SEC1 point, leading `0x04`).
pub fn ecdsa_p256_verify(
    public_key: &[u8],
    message: &[u8],
    signature: &[u8],
    encoding: EcdsaEncoding,
) -> bool {
    match encoding {
        EcdsaEncoding::Asn1 => UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, public_key)
            .verify(message, signature)
            .is_ok(),
        EcdsaEncoding::Fixed => UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, public_key)
            .verify(message, signature)
            .is_ok(),
    }
}

/// Verify an RSASSA-PKCS1-v1_5 / SHA-256 `signature` over `message` under `public_pkcs1`, a DER
/// PKCS#1 `RSAPublicKey` of 2048 to 8192 bits.
pub fn rsa_pkcs1_sha256_verify(public_pkcs1: &[u8], message: &[u8], signature: &[u8]) -> bool {
    UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, public_pkcs1)
        .verify(message, signature)
        .is_ok()
}
