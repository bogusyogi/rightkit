//! Right Suite license **enforcement** primitive (Rust) — the security half of the
//! licensing system; the JS `@rightkit/license` client is the convenience half.
//!
//! Verifies a server-issued, Ed25519-signed entitlement token **offline** against a
//! public key compiled into the app binary, then the app gates Pro features on the
//! returned [`Entitlement`] at its Rust command boundary (so a JS edit can't unlock
//! them). The token is minted by the backend signer (`rightapps`
//! `packages/api/src/licensing/sign.ts`); revocation propagates via the short `exp`
//! + the app re-fetching on refresh.
//!
//! ## Wire form (must match the signer byte-for-byte)
//! `base64url(canonical_payload_json) "." base64url(ed25519_signature)`, where the
//! canonical payload is compact JSON in this exact field order:
//! `v, id, app, tier, features, device_id, iat, exp`. base64url is URL-safe, **no
//! padding** (Node's `Buffer.toString("base64url")`).

use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;

/// The verified entitlement (the signed payload). Field order matches the signer's
/// canonical order, though serde does not require it for *parsing* — the signature
/// is what pins the bytes.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Entitlement {
    /// Schema version.
    pub v: u8,
    /// License id.
    pub id: String,
    /// appKey the token was minted for.
    pub app: String,
    /// `"plus"` | `"pro"`.
    pub tier: String,
    /// Entitlement features.
    pub features: Vec<String>,
    /// Device the token is bound to.
    pub device_id: String,
    /// Issued-at (unix seconds).
    pub iat: i64,
    /// Expiry (unix seconds).
    pub exp: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LicenseRequirement {
    pub tier: Option<String>,
    pub feature: Option<String>,
    pub all_features: Vec<String>,
    pub any_features: Vec<String>,
}

impl LicenseRequirement {
    pub fn pro() -> Self {
        Self {
            tier: Some("pro".to_string()),
            ..Self::default()
        }
    }

    pub fn feature(feature: impl Into<String>) -> Self {
        Self {
            feature: Some(feature.into()),
            ..Self::default()
        }
    }

    pub fn tier(tier: impl Into<String>) -> Self {
        Self {
            tier: Some(tier.into()),
            ..Self::default()
        }
    }

    pub fn all_features(features: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            all_features: features.into_iter().map(Into::into).collect(),
            ..Self::default()
        }
    }

    pub fn any_feature(features: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            any_features: features.into_iter().map(Into::into).collect(),
            ..Self::default()
        }
    }
}

impl Entitlement {
    pub fn has_feature(&self, f: &str) -> bool {
        self.features.iter().any(|x| x == f)
    }
    pub fn is_pro(&self) -> bool {
        self.tier == "pro"
    }
    /// Free < Plus < Pro.
    pub fn tier_at_least(&self, tier: &str) -> bool {
        tier_rank(&self.tier) >= tier_rank(tier)
    }
    pub fn has_all_features(&self, features: &[impl AsRef<str>]) -> bool {
        features
            .iter()
            .all(|feature| self.has_feature(feature.as_ref()))
    }
    pub fn has_any_feature(&self, features: &[impl AsRef<str>]) -> bool {
        features
            .iter()
            .any(|feature| self.has_feature(feature.as_ref()))
    }
    /// Shared native enforcement helper. Verify the signed token first with
    /// [`verify_token`], then apply the same requirement shape the JS UI uses.
    pub fn satisfies(&self, requirement: &LicenseRequirement) -> bool {
        if let Some(tier) = &requirement.tier {
            if !self.tier_at_least(tier) {
                return false;
            }
        }
        if let Some(feature) = &requirement.feature {
            if !self.has_feature(feature) {
                return false;
            }
        }
        if !requirement.all_features.is_empty() && !self.has_all_features(&requirement.all_features)
        {
            return false;
        }
        if !requirement.any_features.is_empty() && !self.has_any_feature(&requirement.any_features)
        {
            return false;
        }
        true
    }
}

fn tier_rank(t: &str) -> u8 {
    match t {
        "pro" => 2,
        "plus" => 1,
        _ => 0,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum VerifyError {
    /// Not a `payload.sig` token.
    Malformed,
    /// A part isn't valid base64url.
    BadEncoding,
    /// Signature does not verify against the public key.
    BadSignature,
    /// Embedded public key is not a valid Ed25519 key.
    BadKey,
    /// Token is for a different app.
    AppMismatch { expected: String, got: String },
    /// Token is bound to a different device.
    DeviceMismatch,
    /// `now > exp`.
    Expired { exp: i64, now: i64 },
    /// Payload JSON did not parse into the expected schema.
    Parse(String),
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerifyError::Malformed => write!(f, "license token is malformed"),
            VerifyError::BadEncoding => write!(f, "license token is not valid base64url"),
            VerifyError::BadSignature => write!(f, "license token signature does not verify"),
            VerifyError::BadKey => write!(f, "embedded public key is invalid"),
            VerifyError::AppMismatch { expected, got } => {
                write!(f, "license token is for app '{got}', not '{expected}'")
            }
            VerifyError::DeviceMismatch => {
                write!(f, "license token is bound to a different device")
            }
            VerifyError::Expired { exp, now } => write!(f, "license token expired ({now} > {exp})"),
            VerifyError::Parse(e) => write!(f, "license payload parse error: {e}"),
        }
    }
}
impl std::error::Error for VerifyError {}

/// Verify a signed entitlement token offline.
///
/// Checks, in order: token shape, base64url decode, Ed25519 signature over the
/// payload bytes (against `pubkey`), `app == expected_app`, `device_id == device_id`,
/// and `now_unix <= exp`. Returns the verified [`Entitlement`] on success. The caller
/// then gates features on it (`is_pro()` / `has_feature()` / `tier_at_least()`).
pub fn verify_token(
    token: &str,
    pubkey: &[u8; 32],
    expected_app: &str,
    device_id: &str,
    now_unix: i64,
) -> Result<Entitlement, VerifyError> {
    let dot = token.find('.').ok_or(VerifyError::Malformed)?;
    let payload_b64 = &token[..dot];
    let sig_b64 = &token[dot + 1..];
    if payload_b64.is_empty() || sig_b64.is_empty() {
        return Err(VerifyError::Malformed);
    }

    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let payload_bytes = engine
        .decode(payload_b64)
        .map_err(|_| VerifyError::BadEncoding)?;
    let sig_bytes = engine
        .decode(sig_b64)
        .map_err(|_| VerifyError::BadEncoding)?;

    let vk = VerifyingKey::from_bytes(pubkey).map_err(|_| VerifyError::BadKey)?;
    let sig = Signature::from_slice(&sig_bytes).map_err(|_| VerifyError::BadEncoding)?;
    vk.verify(&payload_bytes, &sig)
        .map_err(|_| VerifyError::BadSignature)?;

    let ent: Entitlement =
        serde_json::from_slice(&payload_bytes).map_err(|e| VerifyError::Parse(e.to_string()))?;

    if ent.app != expected_app {
        return Err(VerifyError::AppMismatch {
            expected: expected_app.to_string(),
            got: ent.app.clone(),
        });
    }
    if ent.device_id != device_id {
        return Err(VerifyError::DeviceMismatch);
    }
    if now_unix > ent.exp {
        return Err(VerifyError::Expired {
            exp: ent.exp,
            now: now_unix,
        });
    }
    Ok(ent)
}

#[cfg(test)]
mod tests {
    use super::*;

    // CROSS-LANGUAGE VECTOR: produced by the TS signer (Node crypto Ed25519) over
    // payload {v:2,id:lic_1,app:heardright,tier:pro,features:[wake_word,voice_commands],
    // device_id:dev-1,iat:1000,exp:9999999999}. If the signer's canonical form ever
    // drifts from this crate's decode, this test breaks — which is the point.
    const PUBKEY: [u8; 32] = [
        108, 142, 220, 5, 154, 87, 124, 180, 244, 31, 25, 204, 0, 155, 84, 165, 16, 227, 134, 87,
        61, 66, 199, 238, 168, 219, 128, 145, 39, 235, 80, 211,
    ];
    const TOKEN: &str = "eyJ2IjoyLCJpZCI6ImxpY18xIiwiYXBwIjoiaGVhcmRyaWdodCIsInRpZXIiOiJwcm8iLCJmZWF0dXJlcyI6WyJ3YWtlX3dvcmQiLCJ2b2ljZV9jb21tYW5kcyJdLCJkZXZpY2VfaWQiOiJkZXYtMSIsImlhdCI6MTAwMCwiZXhwIjo5OTk5OTk5OTk5fQ.JNmWEwG16VehpwJz9SooQIia0k1Ht0c150jca86h7sAnFj9tKuPO8yZhfyR48vx3Yd_03RQyYYtbKm08TCgsBg";

    #[test]
    fn verifies_a_ts_signed_token() {
        let e = verify_token(TOKEN, &PUBKEY, "heardright", "dev-1", 2_000).expect("should verify");
        assert_eq!(e.v, 2);
        assert_eq!(e.tier, "pro");
        assert!(e.is_pro());
        assert!(e.has_feature("wake_word"));
        assert!(e.has_feature("voice_commands"));
        assert!(!e.has_feature("byok_ai"));
        assert!(e.tier_at_least("plus"));
        assert!(e.satisfies(&LicenseRequirement::pro()));
        assert!(e.satisfies(&LicenseRequirement::feature("wake_word")));
        assert!(e.satisfies(&LicenseRequirement::any_feature([
            "missing",
            "voice_commands"
        ])));
        assert!(e.satisfies(&LicenseRequirement::all_features([
            "wake_word",
            "voice_commands"
        ])));
        assert!(!e.satisfies(&LicenseRequirement::feature("byok_ai")));
    }

    #[test]
    fn rejects_wrong_app() {
        assert!(matches!(
            verify_token(TOKEN, &PUBKEY, "coderight", "dev-1", 2_000),
            Err(VerifyError::AppMismatch { .. })
        ));
    }

    #[test]
    fn rejects_wrong_device() {
        assert_eq!(
            verify_token(TOKEN, &PUBKEY, "heardright", "someone-else", 2_000),
            Err(VerifyError::DeviceMismatch)
        );
    }

    #[test]
    fn rejects_expired() {
        assert!(matches!(
            verify_token(TOKEN, &PUBKEY, "heardright", "dev-1", 10_000_000_000),
            Err(VerifyError::Expired { .. })
        ));
    }

    #[test]
    fn rejects_wrong_key() {
        let mut k = PUBKEY;
        k[0] ^= 1;
        assert!(verify_token(TOKEN, &k, "heardright", "dev-1", 2_000).is_err());
    }

    #[test]
    fn rejects_tampered_signature() {
        let mut t: Vec<char> = TOKEN.chars().collect();
        let last = t.len() - 1;
        t[last] = if t[last] == 'g' { 'h' } else { 'g' };
        let tampered: String = t.into_iter().collect();
        assert!(verify_token(&tampered, &PUBKEY, "heardright", "dev-1", 2_000).is_err());
    }

    #[test]
    fn rejects_malformed() {
        assert_eq!(
            verify_token("no-dot", &PUBKEY, "heardright", "dev-1", 0),
            Err(VerifyError::Malformed)
        );
        assert_eq!(
            verify_token("", &PUBKEY, "heardright", "dev-1", 0),
            Err(VerifyError::Malformed)
        );
    }
}
