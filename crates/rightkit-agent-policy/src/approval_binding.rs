//! Approval binding (EFF-002): an approval binds the exact post-transform
//! dispatch payload. The digest is taken at approval-request time over the
//! final bytes (after normalization, middleware and hook transforms) and is
//! re-checked at dispatch; any difference is a typed rejection.

use serde::{Deserialize, Serialize};

/// Hex SHA-256 digest of a final dispatch payload.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PayloadDigest(String);

impl PayloadDigest {
    /// Digest of the tool name and the exact final payload bytes. Fields are
    /// length-prefixed so (tool, payload) pairs cannot collide by shifting bytes.
    pub fn of(tool: &str, payload: &[u8]) -> Self {
        let mut buf = Vec::with_capacity(tool.len() + payload.len() + 16);
        buf.extend_from_slice(&(tool.len() as u64).to_be_bytes());
        buf.extend_from_slice(tool.as_bytes());
        buf.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        buf.extend_from_slice(payload);
        Self(sha256_hex(&buf))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Typed rejection for a dispatch whose payload differs from the approved one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApprovalBindingError {
    #[error("approval payload mismatch: approved {approved}, dispatching {actual}")]
    PayloadMismatch { approved: String, actual: String },
}

/// An approval bound to one final payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalBinding {
    digest: PayloadDigest,
}

impl ApprovalBinding {
    /// Bind at approval request time, over the payload that will be shown.
    pub fn bind(tool: &str, final_payload: &[u8]) -> Self {
        Self {
            digest: PayloadDigest::of(tool, final_payload),
        }
    }

    pub fn digest(&self) -> &PayloadDigest {
        &self.digest
    }

    /// Re-check at dispatch against the bytes about to execute.
    pub fn verify(&self, tool: &str, dispatch_payload: &[u8]) -> Result<(), ApprovalBindingError> {
        let actual = PayloadDigest::of(tool, dispatch_payload);
        if actual == self.digest {
            Ok(())
        } else {
            Err(ApprovalBindingError::PayloadMismatch {
                approved: self.digest.0.clone(),
                actual: actual.0,
            })
        }
    }
}

/// Session grants keyed on the final payload digest: a grant matches only
/// calls whose final payload digest equals the approved one.
#[derive(Debug, Clone, Default)]
pub struct SessionGrants {
    granted: std::collections::HashSet<String>,
}

impl SessionGrants {
    pub fn grant(&mut self, binding: &ApprovalBinding) {
        self.granted.insert(binding.digest.0.clone());
    }

    /// Grant by an already-computed digest string (the engine's
    /// `effect_payload_digest` of the post-transform payload).
    pub fn grant_digest(&mut self, digest: &str) {
        self.granted.insert(digest.to_string());
    }

    pub fn matches(&self, tool: &str, final_payload: &[u8]) -> bool {
        self.granted
            .contains(&PayloadDigest::of(tool, final_payload).0)
    }

    pub fn matches_digest(&self, digest: &str) -> bool {
        self.granted.contains(digest)
    }
}

fn sha256_hex(data: &[u8]) -> String {
    rightkit_crypto::sha256(data)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
