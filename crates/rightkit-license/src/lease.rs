//! Device-lease envelope signature verification.
//!
//! A lease exchange returns an *envelope*: lease metadata plus a nested entitlement
//! token and an Ed25519 `signature` over the metadata only. This module verifies that
//! envelope signature. It reproduces, byte for byte, the verifier ScrapeRight
//! (`pro_bridge.rs::verify_lease_envelope_signature`) and HeardRight (`license/lease.rs`)
//! ship today, which in turn mirrors the backend's `canonicalLeaseEnvelope`.
//!
//! ## Signing input
//! Compact JSON (no whitespace) of exactly seven camelCase fields in this order:
//! `leaseId, deviceIdHash, pool, issuedAt, expiresAt, graceSeconds, generation`.
//! `entitlement` and `signature` are **not** part of the signed bytes. The signature is
//! base64url without padding and is checked with plain (non-strict) Ed25519 `verify`,
//! against the same deployment key as entitlement tokens.
//!
//! ## Caller responsibilities (what this does not do)
//! * It does **not** authenticate the nested `entitlement`: the envelope signature does
//!   not cover it. The field is therefore exposed as
//!   [`LeaseEnvelope::unverified_entitlement`]; run it through
//!   [`crate::verify_token`] / [`crate::evaluate_lifecycle`] before trusting it.
//! * It does **not** check expiry, device binding or generation ordering of the lease.
//! * Legacy v1 token verification is intentionally not provided here. It remains
//!   app-side migration code and is not part of this crate's contract.

use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

/// The signed lease metadata, as returned by [`verify_lease_envelope`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseEnvelope {
    pub lease_id: String,
    pub device_id_hash: String,
    pub pool: String,
    pub issued_at: String,
    pub expires_at: String,
    pub grace_seconds: i64,
    pub generation: u64,
    /// The nested entitlement token exactly as received. **Not covered by the envelope
    /// signature** and not verified here.
    pub unverified_entitlement: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LeaseWire {
    lease_id: String,
    device_id_hash: String,
    pool: String,
    #[serde(default)]
    entitlement: Option<String>,
    issued_at: String,
    expires_at: String,
    grace_seconds: i64,
    generation: u64,
    #[serde(default)]
    signature: Option<String>,
}

// Field order here IS the wire contract: serde_json preserves declaration order and
// emits no whitespace, matching Node's `JSON.stringify` for these value types.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LeaseCanonical<'a> {
    lease_id: &'a str,
    device_id_hash: &'a str,
    pool: &'a str,
    issued_at: &'a str,
    expires_at: &'a str,
    grace_seconds: i64,
    generation: u64,
}

/// Why a lease envelope was rejected. Every variant means "do not trust this envelope".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseError {
    /// The envelope is not JSON of the expected shape.
    Parse(String),
    /// `signature` is absent or blank.
    MissingSignature,
    /// `signature` is not base64url.
    BadEncoding,
    /// `signature` is not 64 bytes.
    MalformedSignature,
    /// The supplied public key is not a valid Ed25519 key.
    BadKey,
    /// The signature does not verify over the canonical bytes.
    BadSignature,
    /// The canonical bytes could not be produced.
    Canonicalize(String),
}

impl std::fmt::Display for LeaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(error) => write!(f, "device lease envelope is malformed: {error}"),
            Self::MissingSignature => {
                write!(
                    f,
                    "Device lease response is missing its envelope signature."
                )
            }
            Self::BadEncoding => {
                write!(f, "Device lease envelope signature is not valid base64url.")
            }
            Self::MalformedSignature => {
                write!(f, "Device lease envelope signature is malformed.")
            }
            Self::BadKey => write!(f, "Embedded license public key is invalid."),
            Self::BadSignature => {
                write!(f, "Device lease envelope signature does not verify.")
            }
            Self::Canonicalize(error) => write!(
                f,
                "could not encode lease envelope for verification: {error}"
            ),
        }
    }
}
impl std::error::Error for LeaseError {}

/// The exact bytes the backend signs for `envelope` (see the module docs). Useful to
/// signers and tests; verification uses the same function.
pub fn canonical_lease_envelope(envelope: &LeaseEnvelope) -> Result<String, LeaseError> {
    canonical(
        &envelope.lease_id,
        &envelope.device_id_hash,
        &envelope.pool,
        &envelope.issued_at,
        &envelope.expires_at,
        envelope.grace_seconds,
        envelope.generation,
    )
}

fn canonical(
    lease_id: &str,
    device_id_hash: &str,
    pool: &str,
    issued_at: &str,
    expires_at: &str,
    grace_seconds: i64,
    generation: u64,
) -> Result<String, LeaseError> {
    serde_json::to_string(&LeaseCanonical {
        lease_id,
        device_id_hash,
        pool,
        issued_at,
        expires_at,
        grace_seconds,
        generation,
    })
    .map_err(|error| LeaseError::Canonicalize(error.to_string()))
}

/// Verify a device-lease envelope's own signature and return its signed metadata.
///
/// `envelope_json` is the lease object from the exchange response (the JSON object with
/// `leaseId`, `deviceIdHash`, `pool`, `entitlement`, `issuedAt`, `expiresAt`,
/// `graceSeconds`, `generation`, `signature`). Unknown extra fields are ignored. Fails
/// closed: any missing, malformed or non-verifying signature is `Err`, and the caller
/// must not touch stored license state on `Err`.
///
/// A signature that verifies proves only that the server signed these seven fields. The
/// caller still owns the nested entitlement, lease expiry, device binding and generation
/// ordering (see the module docs).
pub fn verify_lease_envelope(
    envelope_json: &str,
    pubkey: &[u8; 32],
) -> Result<LeaseEnvelope, LeaseError> {
    let wire: LeaseWire = serde_json::from_str(envelope_json)
        .map_err(|error| LeaseError::Parse(error.to_string()))?;

    verify_fields(
        &wire.lease_id,
        &wire.device_id_hash,
        &wire.pool,
        &wire.issued_at,
        &wire.expires_at,
        wire.grace_seconds,
        wire.generation,
        wire.signature.as_deref(),
        pubkey,
    )?;

    Ok(LeaseEnvelope {
        lease_id: wire.lease_id,
        device_id_hash: wire.device_id_hash,
        pool: wire.pool,
        issued_at: wire.issued_at,
        expires_at: wire.expires_at,
        grace_seconds: wire.grace_seconds,
        generation: wire.generation,
        unverified_entitlement: wire.entitlement,
    })
}

/// Verify a lease envelope whose fields the caller has already parsed, against the
/// base64url (no padding) `signature` that came with it, without re-serialising to JSON
/// and parsing it again.
///
/// Only the seven signed fields of `envelope` are used;
/// [`LeaseEnvelope::unverified_entitlement`] is ignored. This shares its canonicalisation
/// and signature check with [`verify_lease_envelope`], so for the same field values the
/// two accept and reject identically. Same caller responsibilities as there: a
/// successful check says nothing about the nested entitlement, lease expiry, device
/// binding or generation ordering.
pub fn verify_lease_envelope_fields(
    envelope: &LeaseEnvelope,
    signature: &str,
    pubkey: &[u8; 32],
) -> Result<(), LeaseError> {
    verify_fields(
        &envelope.lease_id,
        &envelope.device_id_hash,
        &envelope.pool,
        &envelope.issued_at,
        &envelope.expires_at,
        envelope.grace_seconds,
        envelope.generation,
        Some(signature),
        pubkey,
    )
}

/// The one verification routine behind both entry points: canonicalise the seven fields,
/// then check `signature_b64` over those bytes.
#[allow(clippy::too_many_arguments)]
fn verify_fields(
    lease_id: &str,
    device_id_hash: &str,
    pool: &str,
    issued_at: &str,
    expires_at: &str,
    grace_seconds: i64,
    generation: u64,
    signature_b64: Option<&str>,
    pubkey: &[u8; 32],
) -> Result<(), LeaseError> {
    let signature_b64 = signature_b64
        .filter(|value| !value.trim().is_empty())
        .ok_or(LeaseError::MissingSignature)?;

    let canonical_json = canonical(
        lease_id,
        device_id_hash,
        pool,
        issued_at,
        expires_at,
        grace_seconds,
        generation,
    )?;

    let sig_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(signature_b64)
        .map_err(|_| LeaseError::BadEncoding)?;
    let signature =
        Signature::from_slice(&sig_bytes).map_err(|_| LeaseError::MalformedSignature)?;
    let verifying_key = VerifyingKey::from_bytes(pubkey).map_err(|_| LeaseError::BadKey)?;
    verifying_key
        .verify(canonical_json.as_bytes(), &signature)
        .map_err(|_| LeaseError::BadSignature)
}
