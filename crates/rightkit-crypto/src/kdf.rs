//! Key derivation (HKDF-SHA256, PBKDF2), the system random generator, HMAC for SHA-1/256/512 and
//! TOTP.

use std::num::NonZeroU32;

use ring::{hkdf, hmac as ring_hmac, pbkdf2};

use crate::CryptoError;

/// Fill `out` from the operating system's secure random generator.
pub fn random_fill(out: &mut [u8]) -> Result<(), CryptoError> {
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), out).map_err(|_| CryptoError)
}

/// `len` random bytes.
pub fn random_bytes(len: usize) -> Result<Vec<u8>, CryptoError> {
    let mut out = vec![0u8; len];
    random_fill(&mut out)?;
    Ok(out)
}

struct OutLen(usize);

impl hkdf::KeyType for OutLen {
    fn len(&self) -> usize {
        self.0
    }
}

/// HKDF-SHA256 (RFC 5869) extract-then-expand: `len` bytes of output keying material from `ikm`,
/// `salt` (may be empty) and `info`. `len` is at most 255 * 32 = 8160.
pub fn hkdf_sha256(
    ikm: &[u8],
    salt: &[u8],
    info: &[u8],
    len: usize,
) -> Result<Vec<u8>, CryptoError> {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(ikm);
    let info = [info];
    let okm = prk.expand(&info, OutLen(len)).map_err(|_| CryptoError)?;
    let mut out = vec![0u8; len];
    okm.fill(&mut out).map_err(|_| CryptoError)?;
    Ok(out)
}

/// PBKDF2 pseudo-random function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pbkdf2Hash {
    Sha256,
    Sha512,
}

impl Pbkdf2Hash {
    fn algorithm(self) -> pbkdf2::Algorithm {
        match self {
            Self::Sha256 => pbkdf2::PBKDF2_HMAC_SHA256,
            Self::Sha512 => pbkdf2::PBKDF2_HMAC_SHA512,
        }
    }
}

/// PBKDF2 (RFC 8018): `len` bytes derived from `password` and `salt`. `iterations` must be
/// non-zero.
pub fn pbkdf2_derive(
    hash: Pbkdf2Hash,
    iterations: u32,
    salt: &[u8],
    password: &[u8],
    len: usize,
) -> Result<Vec<u8>, CryptoError> {
    let iterations = NonZeroU32::new(iterations).ok_or(CryptoError)?;
    let mut out = vec![0u8; len];
    pbkdf2::derive(hash.algorithm(), iterations, salt, password, &mut out);
    Ok(out)
}

/// Check `expected` against the PBKDF2 output for `password` and `salt`, in constant time.
pub fn pbkdf2_verify(
    hash: Pbkdf2Hash,
    iterations: u32,
    salt: &[u8],
    password: &[u8],
    expected: &[u8],
) -> bool {
    match NonZeroU32::new(iterations) {
        Some(iterations) => {
            pbkdf2::verify(hash.algorithm(), iterations, salt, password, expected).is_ok()
        }
        None => false,
    }
}

/// HMAC hash. SHA-1 exists only for interoperability (TOTP secrets issued by sites); do not use it
/// for new designs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HmacAlgorithm {
    Sha1,
    Sha256,
    Sha512,
}

impl HmacAlgorithm {
    fn algorithm(self) -> ring_hmac::Algorithm {
        match self {
            Self::Sha1 => ring_hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY,
            Self::Sha256 => ring_hmac::HMAC_SHA256,
            Self::Sha512 => ring_hmac::HMAC_SHA512,
        }
    }
}

/// HMAC of `msg` under `key` (RFC 2104): 20, 32 or 64 bytes by algorithm.
pub fn hmac(algorithm: HmacAlgorithm, key: &[u8], msg: &[u8]) -> Vec<u8> {
    ring_hmac::sign(&ring_hmac::Key::new(algorithm.algorithm(), key), msg)
        .as_ref()
        .to_vec()
}

/// One-time-password digit counts accepted by [`totp`].
pub const TOTP_DIGITS: std::ops::RangeInclusive<u32> = 6..=8;

/// RFC 6238 time-based one-time password (RFC 4226 truncation) for the raw `secret` bytes at Unix
/// time `unix`. Returns the zero-padded code and the seconds it remains valid. `digits` is 6 to 8
/// and `period` is non-zero; anything else returns `CryptoError`.
pub fn totp(
    secret: &[u8],
    algorithm: HmacAlgorithm,
    digits: u32,
    period: u64,
    unix: u64,
) -> Result<(String, u64), CryptoError> {
    if !TOTP_DIGITS.contains(&digits) || period == 0 {
        return Err(CryptoError);
    }
    let tag = hmac(algorithm, secret, &(unix / period).to_be_bytes());
    let offset = (tag.last().copied().ok_or(CryptoError)? & 0x0f) as usize;
    let word: [u8; 4] = tag
        .get(offset..offset + 4)
        .and_then(|s| s.try_into().ok())
        .ok_or(CryptoError)?;
    let code = (u32::from_be_bytes(word) & 0x7fff_ffff) % 10u32.pow(digits);
    Ok((
        format!("{code:0width$}", width = digits as usize),
        period - unix % period,
    ))
}
