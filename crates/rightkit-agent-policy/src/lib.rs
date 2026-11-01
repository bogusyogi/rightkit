//! `rightkit-agent-policy` — pure permission decision engine (spec §13).
//!
//! No I/O, no async, no network. Given a tool call description, returns one of:
//! - [`PermissionDecision::Allow`] — proceed without asking the user
//! - [`PermissionDecision::Deny`]  — block the call, return error to agent
//! - [`PermissionDecision::Ask`]   — pause and ask the user for approval
//!
//! # Quick start
//! ```rust
//! use rightkit_agent_policy::{PermissionEngine, CapabilityClass, PermissionMode, WorkspaceTrust, PermissionDecision};
//!
//! let engine = PermissionEngine::new();
//! let verdict = engine.evaluate(
//!     CapabilityClass::WriteFile,
//!     PermissionMode::Ask,
//!     WorkspaceTrust::Trusted,
//!     Some("src/main.rs"),
//!     false,
//! );
//! assert_eq!(verdict.decision, PermissionDecision::Ask);
//! ```

pub mod approval_binding;
pub mod capabilities;
pub mod decision_log;
pub mod denial_tracking;
pub mod deny_globs;
pub mod destructive;
pub mod engine;
pub mod net_guard;
pub mod owning_process;
pub mod pdp;
pub mod provenance;
pub mod role_pack_resolver;
pub mod sandbox_escalation;
pub mod sandbox_policy;
pub mod shell_command;
pub(crate) mod shell_stages;
pub mod threat_patterns;
pub mod trust;
pub mod types;
pub mod workspace_trust_gate;

// Flat re-exports for the most common types.
pub use capabilities::{
    effect_class_for_tool, is_file_write, is_read_only, parse_capability, ApprovalClass,
    CapabilityClass, EffectClass,
};
pub use decision_log::{DecisionLog, DecisionRecord};
pub use denial_tracking::{
    DenialRecord, DenialTracker, Rule, RuleError, RuleSource, RuleStore, ToolRuleTarget,
};
pub use deny_globs::{DenyGlobError, DenyGlobSet, DEFAULT_READ_DENY, DEFAULT_WRITE_DENY};
pub use destructive::catastrophic_reason;
pub use engine::{is_test_file, PermissionEngine};
pub use net_guard::{
    is_private_ip, resolve_provider_endpoint, resolve_then_reject_private,
    resolve_then_reject_private_allow_loopback, NetGuardError,
};
pub use owning_process::{
    owning_process_kill_reason, protected_process_name, register_daemon_lock_path,
    set_protected_process_name, OwningProcess, DEFAULT_PROTECTED_PROCESS_NAME,
};
pub use pdp::{Attribute, CondOp, Condition, PolicyDecisionPoint, PolicyRequest, PolicyRule};
pub use provenance::{
    durable_write_is_authorized, durable_write_requires_approval, enforce_durable_write_provenance,
    memory_write_is_authorized, memory_write_requires_approval, ContentOrigin, ContentProvenance,
    DurableWriteKind,
};
pub use sandbox_escalation::{evaluate_escalation, EscalationOutcome, RecognizedDenialCategory};
pub use sandbox_policy::{
    resolve_sandbox_policy, SandboxPolicyResolution, SandboxPolicySource, SandboxTier,
};
pub use shell_command::{classify_shell_command, ShellCommandClassification, ShellDialect};
pub use threat_patterns::{
    prompt_injection_placeholder, scan_prompt_injection, ThreatPatternScope, ThreatScanFinding,
    ThreatScanResult, ThreatScanScope,
};
pub use trust::TrustPolicy;
pub use types::{
    parse_permission_mode, parse_permission_mode_strict, parse_workspace_trust, PermissionDecision,
    PermissionMode, PermissionVerdict, WorkspaceTrust,
};
pub use workspace_trust_gate::{GateOutcome, TrustGateState, WorkspaceTrustGate};

#[cfg(test)]
mod injection_harness_tests;
#[cfg(feature = "legion")]
pub mod legion;

pub use approval_binding::{ApprovalBinding, ApprovalBindingError, PayloadDigest, SessionGrants};
