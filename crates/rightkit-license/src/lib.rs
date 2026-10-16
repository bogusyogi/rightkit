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
//!
//! ## Lifecycle, clock and refresh (0.2.0)
//! - [`Lifecycle`] / [`evaluate_lifecycle`] classify an authenticated entitlement
//!   (trial, expired trial, active, grace, lifetime, ...) against a trusted clock.
//! - [`AntiRollbackClock`] persists a high-water mark of observed time and refuses to
//!   extend validity on a rolled-back clock; [`TrustedClock`] is the hook that
//!   authorization layers consult instead of the raw wall clock.
//! - [`refresh_and_reanchor`] refreshes a license from the server and re-anchors the
//!   clock on the server-signed issue time, returning a typed [`RefreshOutcome`].
//! - [`verify_lease_envelope`] verifies a device-lease envelope signature;
//!   [`verify_lease_envelope_fields`] does the same for already-parsed fields.
//!
//! ## Engine-side verification
//! [`EngineEntitlements`] lets an engine process verify the stored licence itself and
//! expose `require(feature)`; it has no input for a pushed flag. See the `engine` module
//! docs for the window-to-engine pattern.
//!
//! ## Google-subject entitlement (feature `subject-entitlement`, 0.3.0)
//! `SubjectEntitlementSource` asks the shared entitlement service whether the signed-in
//! Google account holds Pro (Play Billing, Windows licence, or a linked StoreKit purchase),
//! with a daily re-check, strict `expires_at` and a 7-day offline grace. It is off by default
//! and independent of the signed-token path above. See the `subject` module docs.
//!
//! ## Caller responsibilities
//! Verification here is deliberately narrow. The caller still owns:
//! - **The nested lease entitlement.** The envelope signature covers seven metadata
//!   fields only, not the `entitlement` token inside the envelope. It is returned as
//!   [`LeaseEnvelope::unverified_entitlement`] and must go through [`verify_token`] or
//!   [`evaluate_lifecycle`] before it is trusted.
//! - **Lease expiry, device binding and generation ordering.** A lease whose signature
//!   verifies may be expired, issued for another device, or older than a lease already
//!   held; checking `expiresAt`/`graceSeconds`, `deviceIdHash` and `generation` (and
//!   rejecting a lower generation than the one stored) is the caller's job.

mod clock;
mod engine;
mod file_store;
mod lease;
mod lifecycle;
mod refresh;
#[cfg(feature = "engine-gate")]
mod gate;
#[cfg(feature = "subject-entitlement")]
mod subject;

#[cfg(feature = "engine-gate")]
pub use gate::{
    DenyReason, EngineGate, Enforcement, GateOutcome, Verdict, EXIT_ALLOWED, EXIT_CONFIG,
    EXIT_DENIED, EXIT_UNAVAILABLE,
};

#[cfg(feature = "subject-entitlement")]
pub use subject::{
    BearerTokenProvider, Freshness, MemorySubjectCache, PlayPurchase, SubjectCacheStore,
    SubjectDecision, SubjectEntitlement, SubjectEntitlementConfig, SubjectEntitlementError,
    SubjectEntitlementSource, SubjectSource, TokenError, SUBJECT_OFFLINE_GRACE_SECS,
    SUBJECT_RECHECK_SECS,
};

pub use clock::reconcile_clock;
pub use clock::{
    plan_reanchor, system_now_unix, AnchorRefusal, AntiRollbackClock, ClockDecision,
    ClockDecodeError, ClockError, ClockReading, ClockRefusal, ClockState, ClockStore,
    MemoryClockStore, ProcessAnchor, ReanchorPlan, Reanchored, SystemClock, TrustedClock,
    CLOCK_ROLLBACK_TOLERANCE_SECS, CLOCK_WRITE_ATTEMPTS, CLOCK_WRITE_INTERVAL_SECS,
};
pub use engine::{
    EngineEntitlementError, EngineEntitlements, EngineEntitlementsConfig, DEFAULT_CACHE_TTL_SECS,
    DEFAULT_TOKEN_FILE, MAX_CACHE_TTL_SECS, MAX_TOKEN_BYTES,
};
pub use file_store::{
    ClosureClockStore, ClosureClockStoreError, CrossProcessLock, FileClockStore,
    FileClockStoreError, LockFailure, LockWait,
};
pub use lease::{
    canonical_lease_envelope, verify_lease_envelope, verify_lease_envelope_fields, LeaseEnvelope,
    LeaseError,
};
pub use lifecycle::{
    evaluate_lifecycle, Lifecycle, LifecycleContext, LifecycleMessageContext,
    LifecycleMessageFormatter, LifecycleMessageKind, LifecycleMessages, LifecyclePolicy,
    LifecycleReport, StoredCredential,
};
pub use refresh::{
    judge_refresh_response, refresh_and_reanchor, RefreshJudgement, RefreshOutcome, RefreshParams,
    RefreshRejection, RefreshRequest, RefreshResponse, RefreshTransport, RejectedToken,
    TokenDropReason, TokenKeptReason, TransportError,
};

use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LicenseKind {
    Trial,
    Lifetime,
    Term,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntitlementStage {
    TimeBound,
    Provisional,
    Final,
}

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
    /// Additive lifecycle claims. All are absent on legacy v2 tokens.
    #[serde(default)]
    pub license_kind: Option<LicenseKind>,
    #[serde(default)]
    pub entitlement_stage: Option<EntitlementStage>,
    #[serde(default)]
    pub license_issued_at: Option<i64>,
    #[serde(default)]
    pub confirmation_due_at: Option<i64>,
}

/// Native authorization policy.
///
/// A valid requirement has at least one effective constraint: a recognized
/// `free`, `plus`, or `pro` tier, or one or more non-empty feature names.
/// Unknown tiers, blank feature names, and a completely empty requirement are
/// invalid and always fail closed in [`Entitlement::satisfies`].
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

    fn is_valid(&self) -> bool {
        let has_tier = match self.tier.as_deref() {
            Some(tier) if tier_rank(tier).is_some() => true,
            Some(_) => return false,
            None => false,
        };
        let has_feature = match self.feature.as_deref() {
            Some(feature) if valid_feature(feature) => true,
            Some(_) => return false,
            None => false,
        };
        if self
            .all_features
            .iter()
            .any(|feature| !valid_feature(feature))
            || self
                .any_features
                .iter()
                .any(|feature| !valid_feature(feature))
        {
            return false;
        }
        has_tier || has_feature || !self.all_features.is_empty() || !self.any_features.is_empty()
    }
}

impl Entitlement {
    pub fn is_final_lifetime(&self) -> bool {
        self.license_kind == Some(LicenseKind::Lifetime)
            && self.entitlement_stage == Some(EntitlementStage::Final)
    }

    /// True when the signed lifecycle claims are complete and internally consistent.
    ///
    /// Legacy v2 tokens (no lifecycle claims at all) are valid. Partial claims, or claims
    /// whose `license_issued_at` / `iat` / `exp` / `confirmation_due_at` ordering is
    /// impossible for the declared kind and stage, are not. [`verify_token`] already
    /// rejects such tokens with [`VerifyError::InvalidLifecycle`]; this accessor lets
    /// callers that hold an [`authenticate_token`] result apply the same rule.
    pub fn has_valid_lifecycle(&self) -> bool {
        match (
            self.license_kind,
            self.entitlement_stage,
            self.license_issued_at,
            self.confirmation_due_at,
        ) {
            // Legacy v2 tokens remain valid until their existing exp boundary.
            (None, None, None, None) => true,
            (
                Some(LicenseKind::Trial | LicenseKind::Term),
                Some(EntitlementStage::TimeBound),
                Some(issued),
                None,
            ) => issued <= self.iat && self.iat <= self.exp,
            (
                Some(LicenseKind::Lifetime),
                Some(EntitlementStage::Provisional),
                Some(issued),
                Some(due),
            ) => issued <= self.iat && self.iat < due && self.exp == due,
            (
                Some(LicenseKind::Lifetime),
                Some(EntitlementStage::Final),
                Some(issued),
                Some(due),
            ) => issued <= due && due <= self.iat && self.iat <= self.exp,
            _ => false,
        }
    }

    pub fn has_feature(&self, f: &str) -> bool {
        self.features.iter().any(|x| x == f)
    }
    pub fn is_pro(&self) -> bool {
        self.tier == "pro"
    }
    /// Free < Plus < Pro.
    pub fn tier_at_least(&self, tier: &str) -> bool {
        matches!(
            (tier_rank(&self.tier), tier_rank(tier)),
            (Some(actual), Some(required)) if actual >= required
        )
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
    /// Malformed entitlement tiers and invalid requirements fail closed.
    pub fn satisfies(&self, requirement: &LicenseRequirement) -> bool {
        if tier_rank(&self.tier).is_none() || !requirement.is_valid() {
            return false;
        }
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

fn tier_rank(t: &str) -> Option<u8> {
    match t {
        "pro" => Some(2),
        "plus" => Some(1),
        "free" => Some(0),
        _ => None,
    }
}

fn valid_feature(feature: &str) -> bool {
    !feature.trim().is_empty()
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// `now >= exp`.
    Expired { exp: i64, now: i64 },
    /// `iat` is implausibly later than the local clock.
    NotYetValid { iat: i64, now: i64 },
    /// Lifecycle fields are partial or internally inconsistent.
    InvalidLifecycle,
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
            VerifyError::Expired { exp, now } => {
                write!(f, "license token expired ({now} >= {exp})")
            }
            VerifyError::NotYetValid { iat, now } => {
                write!(f, "license token is not yet valid ({now} < {iat})")
            }
            VerifyError::InvalidLifecycle => write!(f, "license lifecycle claims are invalid"),
            VerifyError::Parse(e) => write!(f, "license payload parse error: {e}"),
        }
    }
}
impl std::error::Error for VerifyError {}

/// Authenticate a signed entitlement token: token shape, base64url decode, Ed25519
/// signature over the payload bytes (against `pubkey`), payload parse, `app ==
/// expected_app` and `device_id == device_id`.
///
/// This performs **no time check and no lifecycle-shape check**. An expired,
/// not-yet-valid or lifecycle-inconsistent token still returns `Ok`. It exists so that
/// lifecycle classification can read the *authenticated* claims of an expired token
/// (to tell an expired trial from an expired term) without trusting an unverified
/// decode. It must never be used on its own to grant access; use [`verify_token`] or
/// [`evaluate_lifecycle`] for that.
pub fn authenticate_token(
    token: &str,
    pubkey: &[u8; 32],
    expected_app: &str,
    device_id: &str,
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
    Ok(ent)
}

/// Verify a signed entitlement token offline.
///
/// Checks, in order: token shape, base64url decode, Ed25519 signature over the
/// payload bytes (against `pubkey`), `app == expected_app`, `device_id == device_id`,
/// `iat` not implausibly in the future, `now_unix < exp`, and lifecycle consistency.
/// Returns the verified [`Entitlement`] on success. The caller then gates features on
/// it (`is_pro()` / `has_feature()` / `tier_at_least()`).
///
/// `now_unix` should come from a [`TrustedClock`] (an [`AntiRollbackClock`]) rather
/// than the raw wall clock wherever the device clock is not trusted.
pub fn verify_token(
    token: &str,
    pubkey: &[u8; 32],
    expected_app: &str,
    device_id: &str,
    now_unix: i64,
) -> Result<Entitlement, VerifyError> {
    let ent = authenticate_token(token, pubkey, expected_app, device_id)?;
    // Tolerate ordinary clock drift but reject a token whose signed issue time is
    // far in the future. This blocks the simplest pre-issuance clock rollback.
    if now_unix.saturating_add(300) < ent.iat {
        return Err(VerifyError::NotYetValid {
            iat: ent.iat,
            now: now_unix,
        });
    }
    if now_unix >= ent.exp {
        return Err(VerifyError::Expired {
            exp: ent.exp,
            now: now_unix,
        });
    }
    if !ent.has_valid_lifecycle() {
        return Err(VerifyError::InvalidLifecycle);
    }
    Ok(ent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn sign_test_payload(payload: &str) -> (String, [u8; 32]) {
        let key = SigningKey::from_bytes(&[7_u8; 32]);
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let sig = key.sign(payload.as_bytes());
        (
            format!(
                "{}.{}",
                engine.encode(payload.as_bytes()),
                engine.encode(sig.to_bytes())
            ),
            key.verifying_key().to_bytes(),
        )
    }

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
    fn rejects_at_the_exact_expiry_boundary() {
        assert_eq!(
            verify_token(TOKEN, &PUBKEY, "heardright", "dev-1", 9_999_999_999),
            Err(VerifyError::Expired {
                exp: 9_999_999_999,
                now: 9_999_999_999,
            })
        );
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

    #[test]
    fn verifies_final_lifetime_lifecycle_claims() {
        let payload = r#"{"v":2,"id":"lic_life","app":"heardright","tier":"pro","features":[],"device_id":"dev-1","iat":2000,"exp":253402300799,"license_kind":"lifetime","entitlement_stage":"final","license_issued_at":1000,"confirmation_due_at":2000}"#;
        let (token, pubkey) = sign_test_payload(payload);
        let entitlement = verify_token(&token, &pubkey, "heardright", "dev-1", 2_000)
            .expect("final lifetime token should verify");
        assert_eq!(entitlement.license_kind, Some(LicenseKind::Lifetime));
        assert_eq!(entitlement.entitlement_stage, Some(EntitlementStage::Final));
        assert!(entitlement.is_final_lifetime());
    }

    #[test]
    fn rejects_partial_lifecycle_claims() {
        let payload = r#"{"v":2,"id":"lic_bad","app":"heardright","tier":"pro","features":[],"device_id":"dev-1","iat":1000,"exp":2000,"license_kind":"lifetime"}"#;
        let (token, pubkey) = sign_test_payload(payload);
        assert_eq!(
            verify_token(&token, &pubkey, "heardright", "dev-1", 1_500),
            Err(VerifyError::InvalidLifecycle)
        );
    }

    #[test]
    fn rejects_tokens_issued_in_the_future_beyond_clock_skew() {
        let payload = r#"{"v":2,"id":"lic_future","app":"heardright","tier":"pro","features":[],"device_id":"dev-1","iat":2000,"exp":3000}"#;
        let (token, pubkey) = sign_test_payload(payload);
        assert_eq!(
            verify_token(&token, &pubkey, "heardright", "dev-1", 1_000),
            Err(VerifyError::NotYetValid {
                iat: 2_000,
                now: 1_000
            })
        );
    }
}
