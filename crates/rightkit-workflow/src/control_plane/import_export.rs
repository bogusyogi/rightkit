//! WP-10 — Signed/checksummed workflow import / export quarantine.
//!
//! Every imported package starts in `PendingReview`. This module never
//! auto-promotes to `Approved` — that decision is reserved for a human
//! reviewer in WP-09 (Review Inbox) or later.

#![forbid(unsafe_code)]

use serde_json::Value;
use thiserror::Error;

use crate::control_plane::domain::{CapabilitySet, PublicationState};

/// One workflow package: manifest + payload + optional signature.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkflowPackage {
    /// Manifest describing the package.
    pub manifest: PackageManifest,
    /// Raw payload (typically a serialized WorkflowVersion).
    pub payload: Vec<u8>,
    /// Optional detached signature.
    pub signature: Option<Vec<u8>>,
    /// SHA-256 of `payload` as a lowercase hex string.
    pub checksum_sha256: String,
}

/// Package manifest.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PackageManifest {
    /// Human-friendly name.
    pub name: String,
    /// Version string.
    pub version: String,
    /// Content hash of the canonical payload.
    pub content_hash: String,
    /// Intended publication state — always `Draft` on import.
    pub intended_state: PublicationState,
    /// Capabilities the package declares.
    pub declared_capabilities: CapabilitySet,
    /// Versioned package schema. V1 accepts exactly `1`.
    pub schema_version: u32,
    /// Declared effect kinds. Imports never activate them directly.
    pub declared_effects: Vec<String>,
    /// Configured trust-root key id used for detached signature verification.
    pub signing_key_id: String,
}

/// Daemon-provided detached-signature verifier. Import stays fail-closed when
/// no verifier or no signature is supplied.
pub trait PackageSignatureVerifier: Send + Sync {
    fn verify(
        &self,
        manifest: &PackageManifest,
        payload: &[u8],
        signature: &[u8],
    ) -> Result<(), String>;
}

/// Explicit no-crypto placeholder. It prevents accidental unsigned import.
pub struct RejectUnsignedPackage;

impl PackageSignatureVerifier for RejectUnsignedPackage {
    fn verify(&self, _: &PackageManifest, _: &[u8], _: &[u8]) -> Result<(), String> {
        Err("workflow package signature verification is not configured".into())
    }
}

/// Quarantine status. Always starts at `PendingReview`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuarantineStatus {
    /// Awaiting human review.
    PendingReview,
    /// Approved by a reviewer.
    Approved,
    /// Rejected with reason.
    Rejected { reason: String },
    /// Import policy disabled this package; bytes may be retained for audit
    /// but never become a runnable workflow.
    Disabled { reason: String },
}

/// Errors raised by [`verify_checksum`] and [`quarantine`].
#[derive(Debug, Error)]
pub enum ImportError {
    /// Checksum mismatch.
    #[error("checksum mismatch: expected {expected}, computed {computed}")]
    ChecksumMismatch { expected: String, computed: String },
    /// Empty manifest name.
    #[error("manifest name is empty")]
    EmptyName,
    /// Empty manifest version.
    #[error("manifest version is empty")]
    EmptyVersion,
    /// Empty payload.
    #[error("payload is empty")]
    EmptyPayload,
    /// Forbidden publication state.
    #[error("imported package cannot request {0:?} as intended_state; only Draft is accepted")]
    ForbiddenState(PublicationState),
    /// Manifest content hash must bind exactly to payload bytes.
    #[error("manifest content hash mismatch: expected {expected}, computed {computed}")]
    ContentHashMismatch { expected: String, computed: String },
    /// V1 accepts only its closed schema.
    #[error("unsupported package schema version {0}")]
    UnsupportedSchema(u32),
    /// Detached signature is mandatory for imports.
    #[error("package signature is required")]
    MissingSignature,
    /// Detached signature did not verify.
    #[error("package signature verification failed: {0}")]
    InvalidSignature(String),
    /// Package asks for capability outside local import policy.
    #[error("package capability `{0}` is not allowed")]
    ForbiddenCapability(String),
    /// Effects must be explicitly reviewed outside import admission.
    #[error("package declares effect `{0}`; imports cannot activate effects")]
    DeclaredEffect(String),
}

/// Portable bot package surface. The type is intentionally narrower than a
/// runtime profile: no credentials, paths, cookies, executable code, or active
/// schedules can be represented here.
///
/// Provenance: adapted from OpenMausBot `bot-package.ts` authority rules
/// (near-verbatim, made stricter — reject-vs-strip and schedules banned).
/// See `docs/research/canon-provenance/reconciled-spec.md` domain N.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorityFreeBotPackage {
    pub schema_version: u32,
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub task_templates: Vec<String>,
    #[serde(default)]
    pub capability_labels: Vec<String>,
    #[serde(default)]
    pub schedules: Vec<PausedBotSchedule>,
    pub provenance: String,
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PausedBotSchedule {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub expression: Option<String>,
    #[serde(default)]
    pub at_unix_ms: Option<i64>,
    /// Always true on import; active schedules are never portable authority.
    #[serde(default = "always_true")]
    pub paused: bool,
}

fn always_true() -> bool {
    true
}

const FORBIDDEN_BOT_IMPORT_KEYS: &[&str] = &[
    "credential",
    "credentials",
    "secret",
    "secrets",
    "token",
    "apiKey",
    "password",
    "cookie",
    "cookies",
    "browserProfile",
    "profilePath",
    "path",
    "paths",
    "absolutePath",
    "workdir",
    "workingDirectory",
    "executable",
    "command",
    "code",
    "activeSchedule",
    "scheduleEnabled",
    "permissionGrant",
];

fn contains_forbidden_bot_key(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if FORBIDDEN_BOT_IMPORT_KEYS
                    .iter()
                    .any(|forbidden| key.eq_ignore_ascii_case(forbidden))
                {
                    return Some(key.clone());
                }
                if let Some(found) = contains_forbidden_bot_key(child) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(values) => values.iter().find_map(contains_forbidden_bot_key),
        _ => None,
    }
}

/// Parse an authority-free package. Import carries no schedule authority:
/// callers must bind triggers locally, after review, in a paused state.
pub fn import_authority_free_bot_package(raw: &[u8]) -> Result<AuthorityFreeBotPackage, String> {
    let value: Value =
        serde_json::from_slice(raw).map_err(|e| format!("invalid bot package: {e}"))?;
    if let Some(key) = contains_forbidden_bot_key(&value) {
        return Err(format!("bot package contains forbidden field `{key}`"));
    }
    let mut package: AuthorityFreeBotPackage = serde_json::from_value(value)
        .map_err(|e| format!("invalid authority-free bot package: {e}"))?;
    if package.schema_version != 1 {
        return Err(format!(
            "unsupported bot package schema {}",
            package.schema_version
        ));
    }
    if package.id.trim().is_empty() || package.display_name.trim().is_empty() {
        return Err("bot package identity is empty".into());
    }
    if !package.schedules.is_empty() {
        return Err("authority-free bot imports cannot carry schedules".into());
    }
    package.provenance = if package.provenance.trim().is_empty() {
        "rightkit-agent:authority-free-import".into()
    } else {
        package.provenance.trim().to_string()
    };
    let supplied_hash = package.content_hash.trim().to_string();
    package.content_hash.clear();
    let canonical = serde_json::to_vec(&package).map_err(|e| e.to_string())?;
    let computed_hash = format!("sha256:{}", checksum_hex(&canonical));
    if !supplied_hash.is_empty() && supplied_hash != computed_hash {
        return Err("authority-free bot content hash mismatch".into());
    }
    package.content_hash = computed_hash;
    Ok(package)
}

pub fn export_authority_free_bot_package(
    package: &AuthorityFreeBotPackage,
) -> Result<Vec<u8>, String> {
    if package.schema_version != 1 {
        return Err("unsupported bot package schema".into());
    }
    let mut clean = package.clone();
    for schedule in &mut clean.schedules {
        schedule.paused = true;
    }
    clean.provenance = if clean.provenance.trim().is_empty() {
        "rightkit-agent:authority-free-export".into()
    } else {
        clean.provenance.trim().to_string()
    };
    clean.content_hash.clear();
    let canonical = serde_json::to_vec(&clean).map_err(|e| e.to_string())?;
    clean.content_hash = format!("sha256:{}", checksum_hex(&canonical));
    serde_json::to_vec_pretty(&clean).map_err(|e| e.to_string())
}

/// Build the package from a manifest + payload. Computes the checksum
/// and rejects obvious mistakes up-front.
pub fn build(
    manifest: PackageManifest,
    payload: Vec<u8>,
    signature: Option<Vec<u8>>,
) -> Result<WorkflowPackage, ImportError> {
    if manifest.name.trim().is_empty() {
        return Err(ImportError::EmptyName);
    }
    if manifest.version.trim().is_empty() {
        return Err(ImportError::EmptyVersion);
    }
    if payload.is_empty() {
        return Err(ImportError::EmptyPayload);
    }
    if !matches!(manifest.intended_state, PublicationState::Draft) {
        return Err(ImportError::ForbiddenState(manifest.intended_state));
    }
    let checksum_sha256 = checksum_hex(&payload);
    Ok(WorkflowPackage {
        manifest,
        payload,
        signature,
        checksum_sha256,
    })
}

/// Verify the package checksum. Pure.
pub fn verify_checksum(pkg: &WorkflowPackage) -> Result<(), ImportError> {
    let computed = checksum_hex(&pkg.payload);
    if computed != pkg.checksum_sha256 {
        return Err(ImportError::ChecksumMismatch {
            expected: pkg.checksum_sha256.clone(),
            computed,
        });
    }
    Ok(())
}

/// Verify every security-relevant binding before persisting a disabled
/// quarantine row. A successful result does not approve or activate anything.
pub fn verify_import(
    pkg: &WorkflowPackage,
    allowed_capabilities: &CapabilitySet,
    verifier: &dyn PackageSignatureVerifier,
) -> Result<(), ImportError> {
    verify_checksum(pkg)?;
    let content_hash = format!("sha256:{}", checksum_hex(&pkg.payload));
    if pkg.manifest.content_hash != content_hash {
        return Err(ImportError::ContentHashMismatch {
            expected: pkg.manifest.content_hash.clone(),
            computed: content_hash,
        });
    }
    if pkg.manifest.schema_version != 1 {
        return Err(ImportError::UnsupportedSchema(pkg.manifest.schema_version));
    }
    for capability in pkg.manifest.declared_capabilities.as_slice() {
        if !allowed_capabilities.contains(capability) {
            return Err(ImportError::ForbiddenCapability(capability.to_string()));
        }
    }
    if let Some(effect) = pkg.manifest.declared_effects.first() {
        return Err(ImportError::DeclaredEffect(effect.clone()));
    }
    let signature = pkg
        .signature
        .as_deref()
        .ok_or(ImportError::MissingSignature)?;
    verifier
        .verify(&pkg.manifest, &pkg.payload, signature)
        .map_err(ImportError::InvalidSignature)
}

/// Place the package into quarantine. Always returns `PendingReview` —
/// this function never auto-promotes.
pub fn quarantine(pkg: &WorkflowPackage) -> QuarantineStatus {
    let _ = pkg;
    QuarantineStatus::PendingReview
}

fn checksum_hex(payload: &[u8]) -> String {
    rightkit_crypto::hex_encode(&rightkit_crypto::sha256(payload))
}
