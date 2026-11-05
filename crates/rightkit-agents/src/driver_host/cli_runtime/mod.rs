//! Multi-provider CLI runtime.
//!
//! PR 1 of the architecture plan. This module owns the open
//! provider/transport contracts that replace the closed
//! `DriverKind`/`DriverTransport` unions, plus the registry, profile,
//! attestation, policy, qualification, and executor pieces that
//! adapters compose against.
//!
//! Sub-module map:
//!
//! - [`ids`]: provider identifier, auth mode, transport kind,
//!   isolation mode, resume strategy, eligibility, binding status,
//!   capability and requirement types.
//! - [`adapter`]: `CliProviderAdapter`, `CliTransport`,
//!   `AgentRuntimeBackend`, `AgentRuntimeExecutor` traits plus the
//!   value types those traits exchange.
//! - [`auth`]: per-provider auth evidence gathering and forbidden
//!   environment-variable filtering.
//! - [`profile`]: managed account profile lifecycle and storage path
//!   conventions.
//! - [`login_routes`]: native login / status / logout routes that
//!   drive the vendor CLI under a managed home (PR 2).
//! - [`policy`]: session-scoped `CliDelegationPolicy` for cross-
//!   provider child routing.
//! - [`qualification`]: probe runner, negative/positive harness
//!   checks.
//! - [`attestation`]: content-addressed attestation of installed
//!   binaries.
//! - [`recovery`]: process-generation tracking, executable-digest
//!   re-validation, context-epoch tracking, native resume after app
//!   restart, deferred active-binding updates, and typed recovery
//!   errors.
//! - [`executor`]: shared `AgentRuntimeExecutor` infrastructure used
//!   by both the native and authenticated-CLI implementations.
//! - [`registry`]: built-in and dynamic adapter registry.
//! - [`built_in`]: the seed registry containing the six allowed CLIs.
//! - [`transports`]: per-transport implementations
//!   (claude_stream_json, codex_app_server, acp, resumable_ndjson).

pub mod account_identity;
pub mod adapter;
pub mod attestation;
pub mod auth;
pub mod built_in;
pub mod executor;
pub mod ids;
pub mod login_routes;
pub mod policy;
pub mod profile;
pub mod providers;
pub mod qualification;
pub mod recovery;
pub mod registry;
pub mod transports;

/// The fixed canonical list of CLI providers shipped in the
/// built-in registry. Order is the priority order used for picker
/// defaults and migration sequencing (see the architecture plan,
/// section 16).
pub const BUILT_IN_PROVIDER_IDS: &[&str] =
    &["claude", "codex", "grok", "cursor", "cline", "commandcode"];

/// Re-export the most-used types at the module root so callers can
/// write `driver_host::cli_runtime::CliProviderId` rather than the
/// deeper paths.
pub use adapter::{
    native_profile_hash, AgentKind, AgentRuntimeBackend, AgentRuntimeError, AgentRuntimeExecutor,
    AgentRuntimeHandle, AgentTurnRequest, AgentTurnResult, AgentTurnStatus, CliAdapterError,
    CliDiscovery, CliLaunchRequest, CliLaunchSpec, CliMcpServerSpec, CliModelCatalog, CliPrompt,
    CliProviderAdapter, CliRuntimeError, CliTransport, CreateVendorSession, ExecutorKind,
    LoadVendorSession, OpenAgentRequest, StartedCliProcess, VendorSessionHandle, VendorTurnHandle,
};
pub use attestation::AttestationInputs;
pub use executor::{
    fold_turn_result, AgentResult, AgentResultStatus, AuthenticatedCliExecutor, BridgeError,
    CliDriverHostBridge, CliExecutorLaunchContext, CliExecutorLaunchContextProvider,
    CliHandoffBrief, MutationPolicy, StartedBridgeSession,
};
pub use ids::{
    AuthEvidence, CliAccountProfile, CliAccountProfileId, CliAuthMode, CliBindingStatus,
    CliProviderCapabilities, CliProviderEligibility, CliProviderId, CliTransportKind, ContextEpoch,
    ContextEpochReason, HarnessAttestation, HarnessIsolationMode, HarnessRequirements,
    ProbePolarity, QualificationProbe, ResumeStrategy,
};
pub use login_routes::{
    ambient_forbidden_env, list as list_login_statuses, login, logout, redact_credential_bytes,
    status as login_status, LoginError, LoginReceipt, LoginStatus, LoginStatusKind,
};
pub use policy::{
    resolve_child_runtime, CliDelegationPolicy, CliRuntimeSelection, DelegateTaskRequest,
    DelegationError, RegisteredProviders, RejectedHintReason, RejectedRuntimeHint,
    RequestedRuntime, RuntimeResolution, RuntimeResolutionSource,
};
pub use profile::PROFILE_LAYOUTS;
pub use qualification::{
    extract_binary_metadata, run_qualification, ProbeResult, QualificationConfig,
    QualificationContext, QualificationEvidence, QualificationInputs, QualificationObserver,
    QualificationOutcome, DEFAULT_PROBE_TIMEOUT,
};
pub use recovery::{
    check_binding_for_resume, compute_executable_digest, validate_executable_digest,
    validate_protocol_version, validate_schema_version, walk_active_bindings_for_resume,
    BindingResumeGuard, BindingSnapshot, DeferredApplyReport, DeferredBindingUpdate,
    DeferredBindingUpdates, DeferredUpdateKind, DriverBindingRecoveryRow, LiveContextEpoch,
    LiveProcessGeneration, RecoveryError, ResumeDecision, ResumeWalkOutcome,
};
pub use registry::{
    AttestationStore, AttestationStoreError, CliAdapterRegistry, LatestAttestation, RegistryError,
    ATTESTATION_MAX_AGE,
};
