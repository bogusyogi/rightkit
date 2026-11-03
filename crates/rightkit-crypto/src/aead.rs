//! Authenticated encryption: AES-256-GCM and ChaCha20-Poly1305.
//!
//! Wire form of [`seal`]: `nonce (12 bytes) || ciphertext || tag (16 bytes)`. The nonce is drawn
//! fresh from the system random generator on every call (random 96-bit nonces: stay below 2^32
//! messages per key). [`seal_with_nonce`] and [`open_with_nonce`] take a caller nonce for
//! protocols that fix its construction; reusing a nonce under one key breaks the cipher.

use ring::aead::{self, Aad, LessSafeKey, Nonce, UnboundKey};

use crate::CryptoError;

/// An AEAD cipher with a 256-bit key, a 96-bit nonce and a 128-bit tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cipher {
    Aes256Gcm,
    ChaCha20Poly1305,
}

/// Key length in bytes for both ciphers.
pub const KEY_LEN: usize = 32;
/// Nonce length in bytes for both ciphers.
pub const NONCE_LEN: usize = 12;
/// Tag length in bytes for both ciphers.
pub const TAG_LEN: usize = 16;

impl Cipher {
    fn key(self, key: &[u8]) -> Result<LessSafeKey, CryptoError> {
        let algorithm = match self {
            Self::Aes256Gcm => &aead::AES_256_GCM,
            Self::ChaCha20Poly1305 => &aead::CHACHA20_POLY1305,
        };
        UnboundKey::new(algorithm, key)
            .map(LessSafeKey::new)
            .map_err(|_| CryptoError)
    }
}

/// Encrypt `plaintext` with a random nonce. Returns `nonce || ciphertext || tag`.
pub fn seal(
    cipher: Cipher,
    key: &[u8],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let mut nonce = [0u8; NONCE_LEN];
    crate::random_fill(&mut nonce)?;
    let sealed = seal_with_nonce(cipher, key, &nonce, aad, plaintext)?;
    let mut out = Vec::with_capacity(NONCE_LEN + sealed.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&sealed);
    Ok(out)
}

/// Decrypt the output of [`seal`].
pub fn open(cipher: Cipher, key: &[u8], aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if sealed.len() < NONCE_LEN + TAG_LEN {
        return Err(CryptoError);
    }
    let (nonce, body) = sealed.split_at(NONCE_LEN);
    open_with_nonce(cipher, key, nonce, aad, body)
}

/// Encrypt under a caller-chosen 12-byte `nonce`. Returns `ciphertext || tag`. Never reuse a
/// nonce under one key.
pub fn seal_with_nonce(
    cipher: Cipher,
    key: &[u8],
    nonce: &[u8],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let nonce: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| CryptoError)?;
    let key = cipher.key(key)?;
    let mut buffer = plaintext.to_vec();
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce),
        Aad::from(aad),
        &mut buffer,
    )
    .map_err(|_| CryptoError)?;
    Ok(buffer)
}

/// Decrypt `ciphertext || tag` under a caller-supplied 12-byte `nonce`.
pub fn open_with_nonce(
    cipher: Cipher,
    key: &[u8],
    nonce: &[u8],
    aad: &[u8],
    ciphertext_and_tag: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let nonce: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| CryptoError)?;
    let key = cipher.key(key)?;
    let mut buffer = ciphertext_and_tag.to_vec();
    let plain_len = key
        .open_in_place(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(aad),
            &mut buffer,
        )
        .map_err(|_| CryptoError)?
        .len();
    buffer.truncate(plain_len);
    Ok(buffer)
}
