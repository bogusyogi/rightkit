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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_plane::domain::Capability;

    fn good_manifest() -> PackageManifest {
        PackageManifest {
            name: "morning-brief".into(),
            version: "1.0.0".into(),
            content_hash: "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
                .into(),
            intended_state: PublicationState::Draft,
            declared_capabilities: CapabilitySet::from_iter([Capability::new("fs.read")]),
            schema_version: 1,
            declared_effects: Vec::new(),
            signing_key_id: "test-key".into(),
        }
    }

    #[test]
    fn build_computes_sha256_checksum() {
        let pkg = build(good_manifest(), b"hello world".to_vec(), None).expect("ok");
        assert_eq!(
            pkg.checksum_sha256,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn verify_checksum_succeeds_for_intact_package() {
        let pkg = build(good_manifest(), b"abc".to_vec(), None).expect("ok");
        verify_checksum(&pkg).expect("ok");
    }

    #[test]
    fn verify_checksum_detects_tampering() {
        let mut pkg = build(good_manifest(), b"abc".to_vec(), None).expect("ok");
        pkg.payload = b"abcd".to_vec();
        let err = verify_checksum(&pkg).expect_err("mismatch");
        assert!(matches!(err, ImportError::ChecksumMismatch { .. }));
    }

    #[test]
    fn build_rejects_empty_payload() {
        let err = build(good_manifest(), Vec::new(), None).expect_err("empty");
        assert!(matches!(err, ImportError::EmptyPayload));
    }

    #[test]
    fn build_rejects_empty_name() {
        let mut m = good_manifest();
        m.name = String::new();
        assert!(matches!(
            build(m, b"x".to_vec(), None),
            Err(ImportError::EmptyName)
        ));
    }

    #[test]
    fn build_rejects_empty_version() {
        let mut m = good_manifest();
        m.version = String::new();
        assert!(matches!(
            build(m, b"x".to_vec(), None),
            Err(ImportError::EmptyVersion)
        ));
    }

    #[test]
    fn build_rejects_non_draft_intended_state() {
        let mut m = good_manifest();
        m.intended_state = PublicationState::Published;
        let err = build(m, b"x".to_vec(), None).expect_err("non-draft");
        assert!(matches!(err, ImportError::ForbiddenState(_)));
    }

    #[test]
    fn quarantine_starts_at_pending_review() {
        let pkg = build(good_manifest(), b"abc".to_vec(), None).expect("ok");
        assert_eq!(quarantine(&pkg), QuarantineStatus::PendingReview);
    }

    #[test]
    fn quarantine_never_auto_promotes() {
        // Even a "perfect" package must wait for human review.
        let pkg = build(good_manifest(), b"abc".to_vec(), Some(b"sig".to_vec())).expect("ok");
        assert_eq!(quarantine(&pkg), QuarantineStatus::PendingReview);
    }

    #[test]
    fn rejected_status_carries_reason() {
        let s = QuarantineStatus::Rejected {
            reason: "unsafe capability".into(),
        };
        assert!(matches!(s, QuarantineStatus::Rejected { .. }));
    }

    struct AcceptSignature;
    impl PackageSignatureVerifier for AcceptSignature {
        fn verify(&self, _: &PackageManifest, _: &[u8], _: &[u8]) -> Result<(), String> {
            Ok(())
        }
    }

    fn authority_free_package() -> AuthorityFreeBotPackage {
        AuthorityFreeBotPackage {
            schema_version: 1,
            id: "portable-reviewer".into(),
            display_name: "Portable Review Bot".into(),
            instructions: "Review changed files and summarize risks.".into(),
            task_templates: vec!["review changes".into(), "summarize risks".into()],
            capability_labels: vec!["fs.read".into(), "search".into()],
            schedules: Vec::new(),
            provenance: "fixture:authority-free-v1".into(),
            content_hash: String::new(),
        }
    }

    #[test]
    fn import_requires_hash_schema_capability_and_signature() {
        let pkg = build(good_manifest(), b"abc".to_vec(), Some(vec![7])).expect("package");
        let allowed = CapabilitySet::from_iter([Capability::new("fs.read")]);
        verify_import(&pkg, &allowed, &AcceptSignature).expect("verified quarantine input");
        assert!(matches!(
            verify_import(&pkg, &allowed, &RejectUnsignedPackage),
            Err(ImportError::InvalidSignature(_))
        ));
    }

    #[test]
    fn authority_free_bot_import_rejects_schedules_and_secrets() {
        let raw = br#"{"schemaVersion":1,"id":"b","displayName":"B","provenance":"test","contentHash":""}"#;
        let package = import_authority_free_bot_package(raw).expect("portable package");
        assert!(package.schedules.is_empty());
        let with_schedule = br#"{"schemaVersion":1,"id":"b","displayName":"B","schedules":[{"id":"s","kind":"cron"}],"provenance":"test","contentHash":""}"#;
        assert!(import_authority_free_bot_package(with_schedule).is_err());
        let rejected = import_authority_free_bot_package(
            br#"{"schemaVersion":1,"id":"b","displayName":"B","token":"secret"}"#,
        );
        assert!(rejected.unwrap_err().contains("forbidden"));
    }

    #[test]
    fn authority_free_bot_fixture_has_no_import_authority() {
        let package = import_authority_free_bot_package(include_bytes!(
            "../../../../fixtures/bots/authority-free-import.json"
        ))
        .expect("authority-free fixture");

        assert_eq!(package.schema_version, 1);
        assert_eq!(package.id, "portable-reviewer");
        assert_eq!(package.display_name, "Portable Review Bot");
        assert!(package.schedules.is_empty());
        assert!(package.content_hash.starts_with("sha256:"));
    }

    #[test]
    fn authority_free_bot_export_import_round_trip_is_deterministic() {
        let package = authority_free_package();
        let exported = export_authority_free_bot_package(&package).expect("export");
        let imported = import_authority_free_bot_package(&exported).expect("import");
        let reexported = export_authority_free_bot_package(&imported).expect("re-export");

        assert_eq!(exported, reexported);
        assert_eq!(imported.id, package.id);
        assert_eq!(imported.display_name, package.display_name);
        assert_eq!(imported.instructions, package.instructions);
        assert_eq!(imported.task_templates, package.task_templates);
        assert_eq!(imported.capability_labels, package.capability_labels);
        assert_eq!(imported.provenance, package.provenance);
        assert!(imported.schedules.is_empty());
        assert!(imported.content_hash.starts_with("sha256:"));
    }

    #[test]
    fn authority_free_bot_import_rejects_nested_forbidden_keys() {
        for forbidden in ["credentials", "path", "permissionGrant"] {
            let raw = format!(
                r#"{{"schemaVersion":1,"id":"b","displayName":"B","taskTemplates":[{{"nested":{{"{forbidden}":"blocked"}}}}],"provenance":"test","contentHash":""}}"#
            );
            let error = import_authority_free_bot_package(raw.as_bytes())
                .expect_err("nested authority must be rejected");
            assert!(error.contains("forbidden"), "{error}");
            assert!(error.contains(forbidden), "{error}");
        }
    }

    #[test]
    fn authority_free_bot_import_rejects_content_hash_tampering() {
        let exported =
            export_authority_free_bot_package(&authority_free_package()).expect("export");
        let mut value: Value = serde_json::from_slice(&exported).expect("json");
        value["instructions"] = Value::String("tampered instructions".into());
        let tampered_payload = serde_json::to_vec(&value).expect("json bytes");
        let error = import_authority_free_bot_package(&tampered_payload)
            .expect_err("content tamper must be rejected");
        assert!(error.contains("content hash mismatch"), "{error}");

        value["instructions"] = Value::String("Review changed files and summarize risks.".into());
        value["contentHash"] = Value::String("sha256:tampered".into());
        let tampered_hash = serde_json::to_vec(&value).expect("json bytes");
        let error = import_authority_free_bot_package(&tampered_hash)
            .expect_err("hash tamper must be rejected");
        assert!(error.contains("content hash mismatch"), "{error}");
    }

    #[test]
    fn authority_free_bot_import_rejects_unsupported_schema_and_preserves_references() {
        let mut unsupported = serde_json::to_value(authority_free_package()).expect("package json");
        unsupported["schemaVersion"] = Value::from(2);
        let raw = serde_json::to_vec(&unsupported).expect("json bytes");
        let error = import_authority_free_bot_package(&raw).expect_err("schema must be rejected");
        assert!(error.contains("unsupported bot package schema"), "{error}");

        let exported =
            export_authority_free_bot_package(&authority_free_package()).expect("export");
        let imported = import_authority_free_bot_package(&exported).expect("import");
        assert_eq!(imported.id, "portable-reviewer");
        assert_eq!(
            imported.task_templates,
            vec!["review changes".to_string(), "summarize risks".to_string()]
        );
        assert_eq!(
            imported.capability_labels,
            vec!["fs.read".to_string(), "search".to_string()]
        );
    }

    #[test]
    fn authority_free_bot_import_rejects_every_forbidden_key_at_any_depth() {
        // Exhaustive coverage of the FORBIDDEN_BOT_IMPORT_KEYS wall: every
        // listed key must be rejected at the top level, inside a nested
        // object, inside an array element, and under an ASCII case variant.
        for forbidden in FORBIDDEN_BOT_IMPORT_KEYS {
            let variants = [
                (
                    "top-level",
                    format!(
                        r#"{{"schemaVersion":1,"id":"b","displayName":"B","{forbidden}":"x","provenance":"t","contentHash":""}}"#
                    ),
                ),
                (
                    "nested-object",
                    format!(
                        r#"{{"schemaVersion":1,"id":"b","displayName":"B","taskTemplates":[{{"nested":{{"{forbidden}":"x"}}}}],"provenance":"t","contentHash":""}}"#
                    ),
                ),
                (
                    "array-element",
                    format!(
                        r#"{{"schemaVersion":1,"id":"b","displayName":"B","capabilityLabels":[{{"{forbidden}":"x"}}],"provenance":"t","contentHash":""}}"#
                    ),
                ),
                (
                    "case-variant",
                    format!(
                        r#"{{"schemaVersion":1,"id":"b","displayName":"B","{}":"x","provenance":"t","contentHash":""}}"#,
                        forbidden.to_ascii_uppercase()
                    ),
                ),
            ];
            for (shape, raw) in variants {
                let error = import_authority_free_bot_package(raw.as_bytes())
                    .expect_err("forbidden authority must be rejected");
                assert!(
                    error.to_ascii_lowercase().contains("forbidden"),
                    "{forbidden}/{shape}: {error}"
                );
            }
        }
    }

    #[test]
    fn authority_free_bot_import_rejects_checksum_drift_but_accepts_exact_hash() {
        let package = authority_free_package();
        let mut clean = package.clone();
        clean.content_hash.clear();
        let canonical = serde_json::to_vec(&clean).expect("canonical json");
        let exact = rightkit_fs::digest::sha256_bytes(&canonical);
        clean.content_hash = format!("sha256:{exact}");
        let raw = serde_json::to_vec(&clean).expect("json bytes");
        let imported = import_authority_free_bot_package(&raw).expect("exact checksum is accepted");
        assert_eq!(imported.content_hash, format!("sha256:{exact}"));

        clean.content_hash = format!("sha256:{}", "0".repeat(64));
        let drifted = serde_json::to_vec(&clean).expect("json bytes");
        let error = import_authority_free_bot_package(&drifted)
            .expect_err("checksum drift must be rejected");
        assert!(error.contains("content hash mismatch"), "{error}");
    }

    #[test]
    fn authority_free_bot_import_never_activates_schedules_or_runtime_authority() {
        // The disabled-import invariant: a successfully imported package
        // carries no schedule authority, no provenance rewriting beyond
        // defaulting, and no execution-capable field — the daemon import
        // handler additionally persists it with enabled=false.
        let exported =
            export_authority_free_bot_package(&authority_free_package()).expect("export");
        let imported = import_authority_free_bot_package(&exported).expect("import");
        assert!(imported.schedules.is_empty());
        assert_eq!(imported.schema_version, 1);
        let serialized = serde_json::to_string(&imported).expect("json");
        for forbidden in FORBIDDEN_BOT_IMPORT_KEYS {
            let needle = format!("\"{forbidden}\":");
            assert!(
                !serialized
                    .to_ascii_lowercase()
                    .contains(&needle.to_ascii_lowercase()),
                "imported package must not carry key {forbidden}"
            );
        }
    }

    #[test]
    fn authority_free_bot_export_forces_schedules_paused_and_keeps_secrets_out() {
        let mut package = authority_free_package();
        package.schedules.push(PausedBotSchedule {
            id: "daily-review".into(),
            kind: "cron".into(),
            expression: Some("0 9 * * *".into()),
            at_unix_ms: None,
            paused: false,
        });
        let exported = export_authority_free_bot_package(&package).expect("export");
        let value: Value = serde_json::from_slice(&exported).expect("json");
        assert_eq!(value["schedules"][0]["paused"], Value::Bool(true));
        let serialized = String::from_utf8(exported).expect("utf8");
        for forbidden in ["credential", "password", "token", "apiKey", "path"] {
            assert!(!serialized.contains(forbidden), "{forbidden}: {serialized}");
        }
    }
}
