//! Provider, transport, and identity identifiers for the CLI runtime.
//!
//! PR 1 of the architecture plan. These types replace the closed
//! `DriverKind` and `DriverTransport` unions with open string-typed
//! identifiers that the registry and adapters operate over.
//!
//! The hard ceilings in this module:
//! - Native CLI account is the only launchable authenticated state.
//! - `CliAuthMode::ByokOrExternalApi` is never a launchable state for
//!   this feature. Adapters that observe it must return
//!   `CliProviderEligibility::Unqualified` and refuse to start.
//! - `CliAuthMode::Unknown` is a launch refusal with a clear repair
//!   action. There is no path from `Unknown` to `NativeCliAccount`
//!   that does not pass through an explicit user login or operator
//!   probe.
//! - `HarnessIsolationMode::ExclusiveCodeRightGateway` is the target
//!   isolation for every new adapter. Compatibility mode is a
//!   temporary transition state, not a steady-state design.
//! - `ResumeStrategy::ProcessRestartWithResumeFlag` is the only
//!   disposable-process variant. "Replay all messages into a new
//!   session" is not a resume strategy and must not appear in any
//!   adapter implementation.

use serde::{Deserialize, Serialize};

/// A vendor CLI identifier. Persisted as a string so future providers
/// (for example GitHub Copilot CLI) can be added without a schema
/// migration.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CliProviderId(String);

impl CliProviderId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for CliProviderId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl From<String> for CliProviderId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl AsRef<str> for CliProviderId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CliProviderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// An account-profile identifier scoped to a single provider. Profiles
/// are vendor-CLI-owned login locations; the host app stores only the
/// identifier and a one-way account fingerprint.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CliAccountProfileId(String);

impl CliAccountProfileId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for CliAccountProfileId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl std::fmt::Display for CliAccountProfileId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Allowed authentication modes for a CLI provider. The variants are
/// deliberately a closed enum so adapters cannot grow a fourth
/// "ambiguous" case. See `docs/architecture/CLI-RUNTIME-SCOPE.md` for
/// the normative definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliAuthMode {
    /// The user completed the vendor CLI's own browser/device/account
    /// flow.
    NativeCliAccount,
    /// The CLI is configured from a user-supplied API key, endpoint,
    /// cloud credential, or BYOK provider. Forbidden for this feature.
    ByokOrExternalApi,
    /// the host app cannot prove the source. Forbidden until resolved.
    Unknown,
}

impl CliAuthMode {
    /// Whether this mode allows a provider launch under this feature.
    /// Only `NativeCliAccount` is launchable; the other variants are
    /// tripwires that the registry must surface as
    /// `CliProviderEligibility::Unqualified`.
    pub fn is_launchable(self) -> bool {
        matches!(self, CliAuthMode::NativeCliAccount)
    }

    /// Human-readable label suitable for UI and structured logs.
    pub fn label(self) -> &'static str {
        match self {
            CliAuthMode::NativeCliAccount => "native_cli_account",
            CliAuthMode::ByokOrExternalApi => "byok_or_external_api",
            CliAuthMode::Unknown => "unknown",
        }
    }
}

/// Transport kind used by an adapter. The transport is not the
/// provider: Grok, Cursor, and Cline can share ACP without sharing
/// auth, launch, session, or qualification policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliTransportKind {
    ClaudeStreamJson,
    CodexAppServer,
    Acp,
    ResumableNdjson,
}

impl CliTransportKind {
    pub fn label(self) -> &'static str {
        match self {
            CliTransportKind::ClaudeStreamJson => "claude_stream_json",
            CliTransportKind::CodexAppServer => "codex_app_server",
            CliTransportKind::Acp => "acp",
            CliTransportKind::ResumableNdjson => "resumable_ndjson",
        }
    }
}

/// How a binding isolates the provider process from the surrounding
/// trust domain. `ExclusiveCodeRightGateway` is the target state.
/// `NativeReadOnlyCompatibility` is a documented transition mode and
/// must be visible in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessIsolationMode {
    /// No vendor-native file, shell, browser, network, plugin, memory,
    /// or delegation route. All capabilities are the host app MCP tools.
    ExclusiveHostGateway,
    /// Temporary compatibility: provider may inspect workspace
    /// read-only, but every stateful effect, approval, memory, network,
    /// and delegation is the host app-owned.
    NativeReadOnlyCompatibility,
}

impl HarnessIsolationMode {
    pub fn label(self) -> &'static str {
        match self {
            HarnessIsolationMode::ExclusiveHostGateway => "exclusive_rightkit_gateway",
            HarnessIsolationMode::NativeReadOnlyCompatibility => "native_read_only_compatibility",
        }
    }
}

/// How a binding resumes the provider's vendor session across process
/// restarts. "Replay visible transcript into a new session" is not a
/// permitted variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeStrategy {
    InProcessSession,
    ProtocolLoadById,
    ProcessRestartWithResumeFlag,
}

impl ResumeStrategy {
    pub fn label(self) -> &'static str {
        match self {
            ResumeStrategy::InProcessSession => "in_process_session",
            ResumeStrategy::ProtocolLoadById => "protocol_load_by_id",
            ResumeStrategy::ProcessRestartWithResumeFlag => "process_restart_with_resume_flag",
        }
    }
}

/// Monotonic counter incremented whenever a change can alter the
/// provider's prompt prefix (model swap, tool schema swap, system
/// policy change, provider upgrade, account switch). A context-epoch
/// change does not require a new conversation, but it makes cache
/// continuity non-comparable and must be visible in diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextEpoch {
    pub value: u64,
    pub reason: ContextEpochReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextEpochReason {
    NewSession,
    ProviderCompaction,
    ModelChanged,
    ToolSchemaChanged,
    SystemPolicyChanged,
    ProviderVersionChanged,
    AccountChanged,
}

/// UI- and dispatch-facing eligibility state for a provider.
///
/// Only `Ready` is launchable as a the host app harness root or child.
/// `CompatibilityReadOnly` is launchable as a separately labeled
/// transition state. Every other variant produces a disabled row with
/// the concrete reason. There is no warning-then-launch path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CliProviderEligibility {
    Ready,
    CompatibilityReadOnly,
    LoginRequired,
    HarnessVerificationRequired,
    VersionUnsupported,
    Unqualified { reasons: Vec<String> },
}

impl CliProviderEligibility {
    /// Whether this eligibility allows a launch under this feature.
    pub fn is_launchable(self) -> bool {
        matches!(
            self,
            CliProviderEligibility::Ready | CliProviderEligibility::CompatibilityReadOnly
        )
    }
}

/// Lifecycle state for a `CliAgentBinding`. Mirrors the state machine
/// in section 18 of the architecture plan, but is intentionally
/// flat (no nested substates) so persistence and JSON transport stay
/// straightforward.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliBindingStatus {
    Unconfigured,
    LoginRequired,
    AccountReady,
    HarnessVerificationRequired,
    Ready,
    Starting,
    Connected,
    Running,
    WaitingForApproval,
    WaitingForChild,
    Resuming,
    RecoveryRequired,
    Interrupted,
    Completed,
    Failed,
}

impl CliBindingStatus {
    /// Whether this status represents an active binding holding
    /// a process instance. Used to enforce "one active binding per
    /// the host app agent" in the storage layer.
    pub fn is_active(self) -> bool {
        matches!(
            self,
            CliBindingStatus::Starting
                | CliBindingStatus::Connected
                | CliBindingStatus::Running
                | CliBindingStatus::WaitingForApproval
                | CliBindingStatus::WaitingForChild
                | CliBindingStatus::Resuming
        )
    }
}

/// Static capability description declared by an adapter. These are
/// what the adapter claims; live attestation is still required to
/// confirm them on the current binary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliProviderCapabilities {
    pub supports_session_resume: bool,
    pub supports_model_discovery: bool,
    pub supports_native_account_login: bool,
    pub supports_child_delegation: bool,
    pub isolation_mode: HarnessIsolationMode,
}

impl Default for CliProviderCapabilities {
    fn default() -> Self {
        Self {
            supports_session_resume: true,
            supports_model_discovery: true,
            supports_native_account_login: true,
            supports_child_delegation: false,
            isolation_mode: HarnessIsolationMode::ExclusiveHostGateway,
        }
    }
}

/// What a binding requires from the harness before it can be
/// launched. Each field is a precondition, not a negotiable option.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HarnessRequirements {
    pub requires_managed_profile: bool,
    pub requires_one_mcp_server: bool,
    pub requires_scoped_lease_token: bool,
    pub requires_driver_sandbox: bool,
    pub forbids_native_subagent: bool,
    pub forbids_native_shell: bool,
    pub forbids_native_browser: bool,
    pub forbids_native_memory_writes: bool,
}

impl HarnessRequirements {
    /// `Some(false)` proves the driver sandbox is required. Without that
    /// requirement the caller cannot distinguish compatibility mode from an
    /// incomplete declaration, so retain `None` for conservative review.
    pub fn isolation_mode_for_compatibility_check(&self) -> Option<bool> {
        self.requires_driver_sandbox.then_some(false)
    }
}

/// One qualification probe used to attest a binary. Negative probes
/// must pass (the bad thing must not happen); positive probes must
/// pass (the good thing must happen). A failure makes the provider
/// `Unqualified`, never a warning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualificationProbe {
    pub id: String,
    pub description: String,
    pub polarity: ProbePolarity,
    pub mandatory: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbePolarity {
    Negative,
    Positive,
}

/// Evidence recorded at every launch describing how the auth mode was
/// determined. If `mode` is anything other than `NativeCliAccount`,
/// the launch is refused before the first prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthEvidence {
    pub mode: CliAuthMode,
    pub source_label: String,
    pub account_fingerprint: Option<String>,
    pub observed_at: String,
}

/// Snapshot of a CLI account profile. the host app stores only
/// non-secret metadata; credentials live inside the vendor-managed
/// profile directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliAccountProfile {
    pub id: CliAccountProfileId,
    pub provider_id: CliProviderId,
    pub label: String,
    pub managed_home: std::path::PathBuf,
    pub auth_mode: CliAuthMode,
    pub signed_in: bool,
    pub account_fingerprint: Option<String>,
    pub last_verified_at: Option<String>,
}

/// Result of running a qualification suite against a real installed
/// binary. Attestations are content-addressed by every input that can
/// affect provider behavior; changing any input invalidates them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessAttestation {
    pub id: String,
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
    pub passed_probes: Vec<String>,
    pub failed_probes: Vec<String>,
    pub verified_at: String,
}

impl HarnessAttestation {
    /// Whether this attestation has any failing probes. A failing
    /// mandatory probe disqualifies the provider outright.
    pub fn passed(&self) -> bool {
        self.failed_probes.is_empty()
    }
}
