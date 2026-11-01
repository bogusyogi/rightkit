#![forbid(unsafe_code)]

/// Legion contract source qualified by this adapter.
pub const LEGION_UPSTREAM_REVISION: &str = "6fec4bb02224f1db5e876603dfa6c5e43848be62";

pub mod effect_adapter;
pub mod error;
pub mod policy_adapter;
pub mod receipt;

pub use effect_adapter::{
    EffectExecutor, EffectExecutorAdapter, EffectRequest, HostEffectExecutor,
};
pub use error::AdapterError;
pub use policy_adapter::{
    ApprovalRequirement, AuthorizationRequest, CanonicalPathInput, CapabilityCeiling,
    ContractVersion, EffectClass, EnforcementLevel, HostEnforcement, LeasePolicy, LegionPolicyPack,
    Operation, PolicyAuthorizer, PolicyAuthorizerAdapter, PolicyDecision, PolicyOutcome,
    PolicyPack, PolicyRule, ReceiptRequirements, RuleDecision, RulePredicate, SymlinkState,
    TrustLevel, TrustMinima, POLICY_SCHEMA_VERSION,
};
pub use receipt::{EffectReceipt, ExecutionEvidence};
pub use rightkit_sandbox::execution::{
    EffectExecutionError, EffectExecutionEvidence, EffectExecutionOutcome, EffectExecutionRequest,
    EffectExecutionStatus, EffectPerformer,
};
