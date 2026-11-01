//! Content-addressed harness attestation.
//!
//! PR 1 of the architecture plan. PR 3 fills in the storage and
//! runners; PR 1 only lays down the address key and the lookup helpers
//! so adapters can produce an attestation identifier without a
//! storage schema in place yet.

use crate::driver_host::cli_runtime::ids::{
    CliProviderId, HarnessAttestation, HarnessIsolationMode,
};

/// Canonical inputs that determine whether an attestation is still
/// valid. Any change to a member invalidates the prior attestation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttestationInputs {
    pub provider_id: CliProviderId,
    pub executable_sha256: String,
    pub binary_version: String,
    pub protocol_version: String,
    pub profile_hash: String,
    pub policy_hash: String,
    pub tool_schema_hash: String,
    pub operating_system: String,
    pub architecture: String,
    pub isolation_mode: HarnessIsolationMode,
}

impl AttestationInputs {
    /// Produce the full content-addressed SHA-256 identifier for these inputs.
    /// Storage validates a 64-character lowercase digest, so truncation would
    /// make every qualification result impossible to persist.
    pub fn address(&self) -> String {
        let canonical = format!(
            "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
            self.provider_id.as_str(),
            self.executable_sha256,
            self.binary_version,
            self.protocol_version,
            self.profile_hash,
            self.policy_hash,
            self.tool_schema_hash,
            self.operating_system,
            self.architecture,
            self.isolation_mode.label(),
        );
        let mut h = rightkit_fs::digest::Sha256Hasher::new();
        h.update(canonical.as_bytes());
        let digest = h.finalize_bytes();
        rightkit_crypto::hex_encode(&digest)
    }

    /// Convenience: build a [`HarnessAttestation`] directly from
    /// inputs plus a probe outcome.
    pub fn build_attestation(
        &self,
        attestation_id: String,
        passed_probes: Vec<String>,
        failed_probes: Vec<String>,
        verified_at: String,
    ) -> HarnessAttestation {
        HarnessAttestation {
            id: attestation_id,
            provider_id: self.provider_id.clone(),
            executable_sha256: self.executable_sha256.clone(),
            binary_version: self.binary_version.clone(),
            protocol_version: self.protocol_version.clone(),
            profile_hash: self.profile_hash.clone(),
            policy_hash: self.policy_hash.clone(),
            tool_schema_hash: self.tool_schema_hash.clone(),
            operating_system: self.operating_system.clone(),
            architecture: self.architecture.clone(),
            isolation_mode: self.isolation_mode,
            passed_probes,
            failed_probes,
            verified_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_inputs() -> AttestationInputs {
        AttestationInputs {
            provider_id: CliProviderId::new("grok"),
            executable_sha256: "deadbeef".into(),
            binary_version: "0.1.0".into(),
            protocol_version: "acp/1".into(),
            profile_hash: "p-hash".into(),
            policy_hash: "x-hash".into(),
            tool_schema_hash: "t-hash".into(),
            operating_system: "macos".into(),
            architecture: "arm64".into(),
            isolation_mode: HarnessIsolationMode::ExclusiveHostGateway,
        }
    }

    #[test]
    fn address_is_deterministic_and_bounded() {
        let a = sample_inputs().address();
        let b = sample_inputs().address();
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn address_changes_when_any_input_changes() {
        let mut other = sample_inputs();
        other.binary_version = "0.2.0".into();
        assert_ne!(sample_inputs().address(), other.address());

        let mut other2 = sample_inputs();
        other2.isolation_mode = HarnessIsolationMode::NativeReadOnlyCompatibility;
        assert_ne!(sample_inputs().address(), other2.address());
    }
}
