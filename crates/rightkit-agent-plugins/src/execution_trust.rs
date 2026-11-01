//! EXT-002 extension execution/trust classification consumed by diagnostics.
//!
//! This is not an extension executor and deliberately does not resurrect the
//! deleted `ExtensionExecutionClass`. It describes the trust consequences of
//! the execution mechanisms the host app actually exposes. Capability/tool
//! registration is orthogonal to isolation: registering native arbitrary code
//! never turns that code into a sandbox.

use serde::{Deserialize, Serialize};

use crate::manifest::{PluginIsolationMode, PluginSandboxPolicy};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionTrustClass {
    DeclarativeBuiltIn,
    TrustedNativeInProcess,
    ExternalProcessMcp,
    FutureIsolated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionIsolationGuarantee {
    NotApplicable,
    NoneInProcess,
    ProcessPolicyDependent,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExtensionTrustDiagnostic {
    pub class: ExtensionTrustClass,
    pub arbitrary_code: bool,
    pub process_authority: bool,
    /// Capability registration is not an isolation boundary. This is false
    /// for every currently executable extension class.
    pub registration_is_sandbox: bool,
    pub isolation: ExtensionIsolationGuarantee,
    pub available: bool,
    pub detail: &'static str,
}

impl ExtensionTrustDiagnostic {
    pub const fn declarative_builtin() -> Self {
        Self {
            class: ExtensionTrustClass::DeclarativeBuiltIn,
            arbitrary_code: false,
            process_authority: false,
            registration_is_sandbox: false,
            isolation: ExtensionIsolationGuarantee::NotApplicable,
            available: true,
            detail: "declarative/built-in contribution; no arbitrary extension code implied",
        }
    }

    pub const fn trusted_native_in_process() -> Self {
        Self {
            class: ExtensionTrustClass::TrustedNativeInProcess,
            arbitrary_code: true,
            process_authority: true,
            registration_is_sandbox: false,
            isolation: ExtensionIsolationGuarantee::NoneInProcess,
            available: true,
            detail: "trusted native code runs in the Agent process with process authority; capability registration is not a sandbox",
        }
    }

    pub const fn external_process_mcp() -> Self {
        Self {
            class: ExtensionTrustClass::ExternalProcessMcp,
            arbitrary_code: true,
            process_authority: true,
            registration_is_sandbox: false,
            isolation: ExtensionIsolationGuarantee::ProcessPolicyDependent,
            available: true,
            detail: "external plugin/MCP code crosses the governed process boundary; sandbox claims require process-policy/backend evidence",
        }
    }

    pub const fn future_isolated() -> Self {
        Self {
            class: ExtensionTrustClass::FutureIsolated,
            arbitrary_code: true,
            process_authority: false,
            registration_is_sandbox: false,
            isolation: ExtensionIsolationGuarantee::Unavailable,
            available: false,
            detail: "reserved for a future explicit isolated runtime; no isolated extension runtime is implemented today",
        }
    }
}

/// Stable doctor/config diagnostic catalogue. CLI/API diagnostics and plugin
/// policy tests consume this same contract instead of duplicating prose.
pub const fn extension_trust_catalog() -> [ExtensionTrustDiagnostic; 4] {
    [
        ExtensionTrustDiagnostic::declarative_builtin(),
        ExtensionTrustDiagnostic::trusted_native_in_process(),
        ExtensionTrustDiagnostic::external_process_mcp(),
        ExtensionTrustDiagnostic::future_isolated(),
    ]
}

/// Map today's manifest modes to their actual execution boundary.
pub fn classify_plugin_policy(policy: &PluginSandboxPolicy) -> ExtensionTrustClass {
    match &policy.isolation {
        PluginIsolationMode::MetadataOnly => ExtensionTrustClass::DeclarativeBuiltIn,
        PluginIsolationMode::TrustedSubprocess | PluginIsolationMode::RestrictedSubprocess => {
            ExtensionTrustClass::ExternalProcessMcp
        }
    }
}

impl ExtensionTrustDiagnostic {
    /// Policy consumers must not accept a sandbox claim based on registration.
    /// This checks the execution class, not mutable descriptive output fields.
    pub fn validate_registration_sandbox_claim(
        &self,
        claims_sandbox: bool,
    ) -> Result<(), &'static str> {
        if claims_sandbox {
            return Err(match self.class {
                ExtensionTrustClass::TrustedNativeInProcess => {
                    "native code has process authority, not capability isolation"
                }
                ExtensionTrustClass::ExternalProcessMcp => {
                    "external isolation requires independent backend evidence"
                }
                ExtensionTrustClass::DeclarativeBuiltIn => {
                    "declarative registration does not establish an execution sandbox"
                }
                ExtensionTrustClass::FutureIsolated => "isolated extension runtime is unavailable",
            });
        }
        Ok(())
    }
}

impl PluginSandboxPolicy {
    /// Config-facing consumer: classify the executable mechanism, never infer
    /// isolation from a manifest's trust label or capability registrations.
    pub fn execution_trust(&self) -> ExtensionTrustDiagnostic {
        match classify_plugin_policy(self) {
            ExtensionTrustClass::DeclarativeBuiltIn => {
                ExtensionTrustDiagnostic::declarative_builtin()
            }
            _ => ExtensionTrustDiagnostic::external_process_mcp(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_registration_is_never_advertised_as_a_sandbox() {
        let native = ExtensionTrustDiagnostic::trusted_native_in_process();
        assert!(native.arbitrary_code);
        assert!(native.process_authority);
        assert!(!native.registration_is_sandbox);
        assert_eq!(native.isolation, ExtensionIsolationGuarantee::NoneInProcess);
    }

    #[test]
    fn capability_registration_cannot_upgrade_native_isolation() {
        let before = ExtensionTrustDiagnostic::trusted_native_in_process();
        let after_capability_registration = ExtensionTrustDiagnostic::trusted_native_in_process();
        assert_eq!(before, after_capability_registration);
        assert!(after_capability_registration
            .validate_registration_sandbox_claim(true)
            .is_err());
        assert!(after_capability_registration
            .validate_registration_sandbox_claim(false)
            .is_ok());
        let mut forged = after_capability_registration;
        forged.registration_is_sandbox = true;
        assert!(forged.validate_registration_sandbox_claim(true).is_err());
    }

    #[test]
    fn manifest_execution_modes_classify_by_real_execution_boundary() {
        let metadata = PluginSandboxPolicy {
            isolation: PluginIsolationMode::MetadataOnly,
            ..PluginSandboxPolicy::default()
        };
        let trusted = PluginSandboxPolicy {
            isolation: PluginIsolationMode::TrustedSubprocess,
            ..PluginSandboxPolicy::default()
        };
        let restricted = PluginSandboxPolicy::default();
        assert_eq!(
            restricted.execution_trust().class,
            ExtensionTrustClass::ExternalProcessMcp
        );
        assert!(!restricted.execution_trust().registration_is_sandbox);
        assert_eq!(
            classify_plugin_policy(&metadata),
            ExtensionTrustClass::DeclarativeBuiltIn
        );
        assert_eq!(
            classify_plugin_policy(&trusted),
            ExtensionTrustClass::ExternalProcessMcp
        );
        assert_eq!(
            classify_plugin_policy(&restricted),
            ExtensionTrustClass::ExternalProcessMcp
        );
    }

    #[test]
    fn future_isolated_class_is_explicitly_unavailable() {
        let isolated = ExtensionTrustDiagnostic::future_isolated();
        assert!(!isolated.available);
        assert!(!isolated.registration_is_sandbox);
        assert_eq!(isolated.isolation, ExtensionIsolationGuarantee::Unavailable);
    }
}
