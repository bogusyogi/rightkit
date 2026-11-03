//! WP-10 — Bounded A2A remote executor boundary.
//!
//! ADR-0021: remote executors run *inside* one bounded workflow node.
//! The descriptor is the trust + capability contract; the validator
//! rejects descriptors that cannot be admitted.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::control_plane::domain::CapabilitySet;

/// One remote executor descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct A2aExecutorDescriptor {
    /// Stable identifier of the executor.
    pub executor_id: String,
    /// Decentralized identifier of the executor identity.
    pub identity_did: String,
    /// Capabilities the executor is allowed to use (upper bound).
    pub capabilities: CapabilitySet,
    /// Maximum tokens the executor can spend per call.
    pub max_budget_tokens: u64,
    /// Maximum wall-clock time per call (ms).
    pub max_wall_clock_ms: u64,
    /// Workspace root the executor is confined to.
    pub workspace_root: String,
    /// Kind of cancellation token (cooperative / cooperative+forced).
    pub cancellation_token_kind: String,
    /// Result schema version this executor emits.
    pub result_schema_version: u32,
    /// SHA-256 of the trusted public key bundle.
    pub trust_root_pubkey_hash: String,
}

/// Cryptographic identity proof carried with an A2A admission request.
/// Verification stays at the daemon trust boundary; this pure crate only
/// defines its required input & fail-closed contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct A2aIdentityProof {
    pub key_id: String,
    pub algorithm: String,
    pub signed_payload_hash: String,
    pub signature: Vec<u8>,
}

/// Daemon-provided identity verifier. A transport adapter never substitutes
/// its own success: callers must supply a trust-root-backed verifier.
pub trait A2aIdentityVerifier {
    fn verify(
        &self,
        descriptor: &A2aExecutorDescriptor,
        proof: &A2aIdentityProof,
    ) -> Result<(), String>;
}

/// Explicit placeholder used until a cryptographic verifier is wired.
/// It rejects every remote executor rather than granting placeholder trust.
pub struct RejectUnverifiedIdentity;

impl A2aIdentityVerifier for RejectUnverifiedIdentity {
    fn verify(&self, _: &A2aExecutorDescriptor, _: &A2aIdentityProof) -> Result<(), String> {
        Err("A2A cryptographic identity verification is not configured".into())
    }
}

/// Classification of a remote output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum A2aResult {
    /// Output matches the expected schema and is trusted.
    Validated { output: Value },
    /// Output is well-formed but fails the schema.
    Invalid { reason: String },
    /// Output is malformed or from an untrusted source.
    Untrusted { reason: String },
    /// Output is retained only as untrusted evidence; it cannot drive an
    /// authority, lifecycle, approval, or effect transition.
    Quarantined { reason: String, output: Value },
}

/// Errors raised by [`validate_descriptor`].
#[derive(Debug, Error)]
pub enum A2aError {
    /// Executor id empty.
    #[error("executor_id is empty")]
    EmptyExecutorId,
    /// Identity DID missing.
    #[error("identity_did is empty")]
    EmptyIdentity,
    /// Workspace root must be non-empty.
    #[error("workspace_root is empty")]
    EmptyWorkspace,
    /// Budget exceeds the hard V1 ceiling.
    #[error("budget {0} exceeds V1 ceiling 1_000_000 tokens")]
    BudgetTooLarge(u64),
    /// A bounded executor must have a non-zero token budget.
    #[error("budget must be greater than zero")]
    ZeroBudget,
    /// Wall clock exceeds the hard V1 ceiling.
    #[error("wall_clock_ms {0} exceeds V1 ceiling 600_000 ms (10 minutes)")]
    WallClockTooLarge(u64),
    /// A bounded executor must have a non-zero wall-clock budget.
    #[error("wall_clock_ms must be greater than zero")]
    ZeroWallClock,
    /// Capability ceiling empty.
    #[error("capability ceiling is empty; at least one capability must be declared")]
    EmptyCapabilities,
    /// Trust root missing.
    #[error(
        "trust_root_pubkey_hash is empty; remote executors cannot be admitted without a trust root"
    )]
    NoTrustRoot,
    /// Cancellation token kind is unsupported.
    #[error("unsupported cancellation_token_kind `{0}`")]
    UnsupportedCancellation(String),
    /// Result schema versions are closed in V1.
    #[error("unsupported result_schema_version {0}; V1 admits only 1")]
    UnsupportedResultSchema(u32),
    /// Identity proof is structurally absent or unsupported.
    #[error("invalid A2A identity proof: {0}")]
    InvalidIdentityProof(String),
    /// Identity proof did not verify against the configured trust root.
    #[error("A2A identity proof verification failed: {0}")]
    IdentityVerificationFailed(String),
    /// Remote descriptor attempts to exceed local authority.
    #[error("remote capability `{0}` is not in local capability ceiling")]
    CapabilityEscalation(String),
    /// Remote descriptor escapes local workspace containment.
    #[error("remote workspace `{remote}` is outside local workspace `{local}")]
    WorkspaceEscape { remote: String, local: String },
}

const MAX_BUDGET_TOKENS: u64 = 1_000_000;
const MAX_WALL_CLOCK_MS: u64 = 600_000;

/// Validate the descriptor. Pure.
pub fn validate_descriptor(d: &A2aExecutorDescriptor) -> Result<(), A2aError> {
    if d.executor_id.trim().is_empty() {
        return Err(A2aError::EmptyExecutorId);
    }
    if d.identity_did.trim().is_empty() {
        return Err(A2aError::EmptyIdentity);
    }
    if d.workspace_root.trim().is_empty() {
        return Err(A2aError::EmptyWorkspace);
    }
    if d.trust_root_pubkey_hash.trim().is_empty() {
        return Err(A2aError::NoTrustRoot);
    }
    if d.max_budget_tokens > MAX_BUDGET_TOKENS {
        return Err(A2aError::BudgetTooLarge(d.max_budget_tokens));
    }
    if d.max_budget_tokens == 0 {
        return Err(A2aError::ZeroBudget);
    }
    if d.max_wall_clock_ms > MAX_WALL_CLOCK_MS {
        return Err(A2aError::WallClockTooLarge(d.max_wall_clock_ms));
    }
    if d.max_wall_clock_ms == 0 {
        return Err(A2aError::ZeroWallClock);
    }
    if d.capabilities.as_slice().is_empty() {
        return Err(A2aError::EmptyCapabilities);
    }
    if !matches!(
        d.cancellation_token_kind.as_str(),
        "cooperative" | "cooperative+forced"
    ) {
        return Err(A2aError::UnsupportedCancellation(
            d.cancellation_token_kind.clone(),
        ));
    }
    if d.result_schema_version != 1 {
        return Err(A2aError::UnsupportedResultSchema(d.result_schema_version));
    }
    Ok(())
}

/// Admit a remote executor only after local containment & identity proof.
/// The descriptor is an upper bound, never an authority grant.
pub fn validate_admission(
    descriptor: &A2aExecutorDescriptor,
    proof: &A2aIdentityProof,
    local_capabilities: &CapabilitySet,
    local_workspace_root: &str,
    verifier: &dyn A2aIdentityVerifier,
) -> Result<(), A2aError> {
    validate_descriptor(descriptor)?;
    if proof.key_id.trim().is_empty()
        || proof.algorithm != "ed25519"
        || proof.signed_payload_hash.trim().is_empty()
        || proof.signature.is_empty()
    {
        return Err(A2aError::InvalidIdentityProof(
            "key_id, ed25519 algorithm, payload hash, & signature are required".into(),
        ));
    }
    for capability in descriptor.capabilities.as_slice() {
        if !local_capabilities.contains(capability) {
            return Err(A2aError::CapabilityEscalation(capability.to_string()));
        }
    }
    if !workspace_contains(local_workspace_root, &descriptor.workspace_root) {
        return Err(A2aError::WorkspaceEscape {
            remote: descriptor.workspace_root.clone(),
            local: local_workspace_root.to_string(),
        });
    }
    verifier
        .verify(descriptor, proof)
        .map_err(A2aError::IdentityVerificationFailed)
}

/// Check lexical containment for workspace roots without touching the
/// filesystem. Both slash styles are accepted, parent traversal is rejected,
/// & sibling prefixes never count as children.
pub fn workspace_contains(local_workspace_root: &str, remote_workspace_root: &str) -> bool {
    let Some(local) = normalize_workspace_root(local_workspace_root) else {
        return false;
    };
    let Some(remote) = normalize_workspace_root(remote_workspace_root) else {
        return false;
    };
    if local == "/" || local == "//" || local.ends_with(":/") {
        remote.starts_with(&local)
    } else {
        remote == local || remote.starts_with(&format!("{local}/"))
    }
}

fn normalize_workspace_root(raw: &str) -> Option<String> {
    let normalized = raw.trim().replace('\\', "/");
    let bytes = normalized.as_bytes();
    let drive_root = bytes.len() >= 3 && bytes[1] == b':' && bytes[2] == b'/';
    if normalized.is_empty() || (!normalized.starts_with('/') && !drive_root) {
        return None;
    }
    let prefix = if drive_root {
        format!("{}:/", normalized[..1].to_ascii_lowercase())
    } else if normalized.starts_with("//") {
        "//".to_string()
    } else {
        "/".to_string()
    };
    let mut result = prefix.clone();
    for component in normalized.split('/') {
        if component.is_empty()
            || component == "."
            || (drive_root && component.len() == 2 && component.ends_with(':'))
        {
            continue;
        }
        if component == ".." {
            return None;
        }
        if result != prefix {
            result.push('/');
        }
        if drive_root || normalized.starts_with("//") {
            result.push_str(&component.to_ascii_lowercase());
        } else {
            result.push_str(component);
        }
    }
    (result != "//").then_some(result)
}

/// Classify an output against the expected schema version. Pure.
///
/// V1 only accepts the literal version `1`. Any unknown top-level field
/// marks the output `Invalid`.
pub fn classify_output(output: &Value, expected_schema_version: u32) -> A2aResult {
    if expected_schema_version != 1 {
        return A2aResult::Quarantined {
            reason: format!(
                "unsupported schema_version {expected_schema_version}; V1 admits only 1"
            ),
            output: output.clone(),
        };
    }
    match output {
        Value::Object(map) => {
            if !map.contains_key("result") {
                return A2aResult::Invalid {
                    reason: "remote output is missing required `result` field".into(),
                };
            }
            const ALLOWED: &[&str] = &["result", "metadata", "trace_id"];
            for key in map.keys() {
                if !ALLOWED.contains(&key.as_str()) {
                    return A2aResult::Quarantined {
                        reason: format!("unknown field `{key}` in remote output"),
                        output: output.clone(),
                    };
                }
            }
            A2aResult::Validated {
                output: output.clone(),
            }
        }
        _ => A2aResult::Quarantined {
            reason: "remote output is not a JSON object".into(),
            output: output.clone(),
        },
    }
}
