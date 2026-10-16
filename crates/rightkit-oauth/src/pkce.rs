//! Public PKCE (RFC 7636) and `state` helpers.
//!
//! Only the `S256` challenge method exists here; `plain` is deliberately not offered.
//! Verifiers and states come from the operating-system CSPRNG.

use std::fmt;

use sha2::{Digest, Sha256};

use crate::{base64url, random_urlsafe, OAuthError};

/// Shortest code verifier RFC 7636 section 4.1 allows.
pub const VERIFIER_MIN_LEN: usize = 43;
/// Longest code verifier RFC 7636 section 4.1 allows.
pub const VERIFIER_MAX_LEN: usize = 128;
/// The only challenge method this crate produces, for the `code_challenge_method` parameter.
pub const CHALLENGE_METHOD: &str = "S256";

/// Octets of entropy in a generated verifier (RFC 7636 section 4.1 recommends 32), which
/// encode to 43 base64url characters.
const VERIFIER_ENTROPY_BYTES: usize = 32;
/// Octets of entropy in a generated `state`.
const STATE_ENTROPY_BYTES: usize = 32;

/// A validated PKCE code verifier. `Debug` never prints it.
#[derive(Clone, PartialEq, Eq)]
pub struct PkceVerifier(String);

impl fmt::Debug for PkceVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PkceVerifier([redacted])")
    }
}

impl PkceVerifier {
    /// A fresh verifier: 32 octets from the OS CSPRNG, base64url without padding (43 chars).
    pub fn generate() -> Result<Self, OAuthError> {
        Ok(Self(random_urlsafe(VERIFIER_ENTROPY_BYTES)?))
    }

    /// Adopt an existing verifier after checking RFC 7636 section 4.1: 43 to 128 characters
    /// from the unreserved set `[A-Za-z0-9-._~]`.
    pub fn parse(value: impl Into<String>) -> Result<Self, OAuthError> {
        let value = value.into();
        validate_verifier(&value)?;
        Ok(Self(value))
    }

    /// The verifier text, for the token request. Call at the point of use.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// The `S256` challenge for this verifier.
    pub fn challenge(&self) -> String {
        s256(&self.0)
    }
}

/// A verifier together with its `S256` challenge.
#[derive(Clone)]
pub struct Pkce {
    pub verifier: PkceVerifier,
    pub challenge: String,
}

impl fmt::Debug for Pkce {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pkce")
            .field("verifier", &"[redacted]")
            .field("challenge", &self.challenge)
            .finish()
    }
}

impl Pkce {
    pub fn generate() -> Result<Self, OAuthError> {
        let verifier = PkceVerifier::generate()?;
        let challenge = verifier.challenge();
        Ok(Self {
            verifier,
            challenge,
        })
    }
}

/// Check a verifier against RFC 7636 section 4.1.
pub fn validate_verifier(verifier: &str) -> Result<(), OAuthError> {
    let len = verifier.len();
    if !(VERIFIER_MIN_LEN..=VERIFIER_MAX_LEN).contains(&len) {
        return Err(OAuthError::InvalidConfig(format!(
            "PKCE verifier must be {VERIFIER_MIN_LEN} to {VERIFIER_MAX_LEN} characters"
        )));
    }
    if !verifier
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
    {
        return Err(OAuthError::InvalidConfig(
            "PKCE verifier contains characters outside [A-Za-z0-9-._~]".into(),
        ));
    }
    Ok(())
}

/// `BASE64URL-ENCODE(SHA256(ASCII(verifier)))` without padding. The caller is responsible for
/// the verifier's validity; use [`challenge_s256`] to have it checked.
fn s256(verifier: &str) -> String {
    base64url(&Sha256::digest(verifier.as_bytes()))
}

/// The `S256` code challenge for `verifier`, which must satisfy RFC 7636 section 4.1.
pub fn challenge_s256(verifier: &str) -> Result<String, OAuthError> {
    validate_verifier(verifier)?;
    Ok(s256(verifier))
}

/// A fresh unguessable `state`: 32 octets from the OS CSPRNG, base64url (43 chars).
pub fn generate_state() -> Result<String, OAuthError> {
    random_urlsafe(STATE_ENTROPY_BYTES)
}

/// Base64url without padding (RFC 4648 section 5), the encoding PKCE and `state` use.
pub fn base64url_encode(bytes: &[u8]) -> String {
    base64url(bytes)
}

/// Compare two byte strings without an early exit on the first differing byte. The lengths
/// are not secret (a `state` has a public, fixed length) and are compared first.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    std::hint::black_box(diff) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    #[test]
    fn rfc7636_appendix_b_vector() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            challenge_s256(verifier).unwrap(),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(
            PkceVerifier::parse(verifier).unwrap().challenge(),
            challenge_s256(verifier).unwrap()
        );
    }

    #[test]
    fn generated_verifier_is_valid_and_distinct() {
        let a = PkceVerifier::generate().unwrap();
        let b = PkceVerifier::generate().unwrap();
        assert_eq!(a.expose().len(), 43);
        assert!(validate_verifier(a.expose()).is_ok());
        assert_ne!(a, b);
        // 32 random octets decode back from the verifier.
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(a.expose())
            .unwrap();
        assert_eq!(raw.len(), 32);
    }

    #[test]
    fn verifier_bounds_and_alphabet_are_enforced() {
        assert!(validate_verifier(&"a".repeat(42)).is_err());
        assert!(validate_verifier(&"a".repeat(43)).is_ok());
        assert!(validate_verifier(&"a".repeat(128)).is_ok());
        assert!(validate_verifier(&"a".repeat(129)).is_err());
        let mut bad = "a".repeat(50);
        bad.replace_range(10..11, "+");
        assert!(validate_verifier(&bad).is_err());
        assert!(challenge_s256(&bad).is_err());
        assert!(PkceVerifier::parse("").is_err());
    }

    #[test]
    fn pkce_pair_is_consistent_and_debug_redacts() {
        let p = Pkce::generate().unwrap();
        assert_eq!(p.challenge, challenge_s256(p.verifier.expose()).unwrap());
        assert_eq!(p.challenge.len(), 43);
        assert!(!format!("{p:?}").contains(p.verifier.expose()));
        assert!(!format!("{:?}", p.verifier).contains(p.verifier.expose()));
    }

    #[test]
    fn state_is_43_urlsafe_chars_and_unique() {
        let a = generate_state().unwrap();
        let b = generate_state().unwrap();
        assert_eq!(a.len(), 43);
        assert!(a
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'));
        assert_ne!(a, b);
    }

    #[test]
    fn constant_time_eq_matches_equality() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"", b"a"));
        assert!(constant_time_eq(b"", b""));
    }
}
