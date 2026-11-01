//! Local execution backend registry data and doctor checks.
//!
//! the host app's lean-launch execution path is local shell execution. The registry
//! still exists so daemon/UI code can make that path explicit without carrying
//! disabled remote backend templates.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionBackendRegistry {
    pub default_backend: String,
    #[serde(default)]
    pub backends: Vec<ExecutionBackendConfig>,
    #[serde(default)]
    pub network_profiles: Vec<NetworkProfile>,
}

impl Default for ExecutionBackendRegistry {
    fn default() -> Self {
        Self {
            default_backend: "local".to_string(),
            backends: vec![ExecutionBackendConfig::local()],
            network_profiles: vec![NetworkProfile {
                id: "local-host".to_string(),
                name: "Local host".to_string(),
                default: NetworkDefault::Allow,
                allow_hosts: Vec::new(),
                allow_private_networks: true,
                deny_private_networks: false,
            }],
        }
    }
}

impl ExecutionBackendRegistry {
    pub fn get(&self, id: &str) -> Option<&ExecutionBackendConfig> {
        self.backends.iter().find(|backend| backend.id == id)
    }

    pub fn doctor(
        &self,
        backend_id: &str,
        workspace_root: Option<&Path>,
    ) -> Option<BackendDoctorReport> {
        let backend = self.get(backend_id)?;
        Some(doctor_backend(backend, workspace_root))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionBackendConfig {
    pub id: String,
    pub name: String,
    pub kind: BackendKind,
    pub enabled: bool,
    pub persistent: bool,
    pub workspace_layout: WorkspaceLayout,
    pub network_profile: Option<String>,
    pub resource_limits: ResourceLimits,
    #[serde(default)]
    pub supports: BackendCapabilities,
}

impl ExecutionBackendConfig {
    pub fn local() -> Self {
        Self {
            id: "local".to_string(),
            name: "Local machine".to_string(),
            kind: BackendKind::Local,
            enabled: true,
            persistent: true,
            workspace_layout: WorkspaceLayout::default(),
            network_profile: Some("local-host".to_string()),
            resource_limits: ResourceLimits::host_default(),
            supports: BackendCapabilities {
                run: true,
                spawn: true,
                poll: true,
                stdin: true,
                kill: true,
                network_isolation: false,
                persistent_workspace: true,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BackendKind {
    Local,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceLayout {
    pub root: String,
    pub uploads: String,
    pub workspace: String,
    pub outputs: String,
    pub logs: String,
    pub skills: String,
    pub metadata: String,
}

impl Default for WorkspaceLayout {
    fn default() -> Self {
        Self {
            root: ".rightkit/workspaces/{workspaceId}".to_string(),
            uploads: "uploads".to_string(),
            workspace: "workspace".to_string(),
            outputs: "outputs".to_string(),
            logs: "logs".to_string(),
            skills: "skills".to_string(),
            metadata: "metadata.json".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceLimits {
    pub cpu: Option<u32>,
    pub memory_mb: Option<u32>,
    pub disk_mb: Option<u32>,
}

impl ResourceLimits {
    pub fn host_default() -> Self {
        Self {
            cpu: None,
            memory_mb: None,
            disk_mb: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BackendCapabilities {
    pub run: bool,
    pub spawn: bool,
    pub poll: bool,
    pub stdin: bool,
    pub kill: bool,
    pub network_isolation: bool,
    pub persistent_workspace: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NetworkProfile {
    pub id: String,
    pub name: String,
    pub default: NetworkDefault,
    #[serde(default)]
    pub allow_hosts: Vec<String>,
    pub allow_private_networks: bool,
    pub deny_private_networks: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum NetworkDefault {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BackendDoctorReport {
    pub backend_id: String,
    pub verdict: DoctorVerdict,
    #[serde(default)]
    pub checks: Vec<BackendDoctorCheck>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum DoctorVerdict {
    Pass,
    Warn,
    Block,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BackendDoctorCheck {
    pub id: String,
    pub label: String,
    pub verdict: DoctorVerdict,
    pub detail: String,
    pub fix: String,
}

pub fn doctor_backend(
    backend: &ExecutionBackendConfig,
    workspace_root: Option<&Path>,
) -> BackendDoctorReport {
    let mut checks = Vec::new();
    match backend.kind {
        BackendKind::Local => doctor_local(backend, workspace_root, &mut checks),
    }

    if !backend.enabled {
        checks.push(check(
            "backend-enabled",
            "Backend enabled",
            DoctorVerdict::Warn,
            "Backend is registered but disabled.",
            "Enable this backend explicitly before routing sessions to it.",
        ));
    }

    let verdict = checks
        .iter()
        .map(|check| check.verdict.clone())
        .max()
        .unwrap_or(DoctorVerdict::Pass);

    BackendDoctorReport {
        backend_id: backend.id.clone(),
        verdict,
        checks,
    }
}

fn doctor_local(
    backend: &ExecutionBackendConfig,
    workspace_root: Option<&Path>,
    checks: &mut Vec<BackendDoctorCheck>,
) {
    if let Some(root) = workspace_root {
        if root.exists() {
            checks.push(check(
                "local-workspace-root",
                "Workspace root",
                DoctorVerdict::Pass,
                format!("Workspace root exists: {}", root.display()),
                "No action needed.",
            ));
        } else {
            checks.push(check(
                "local-workspace-root",
                "Workspace root",
                DoctorVerdict::Block,
                format!("Workspace root does not exist: {}", root.display()),
                "Choose an existing workspace root before using the local backend.",
            ));
        }
    }

    if !backend.supports.network_isolation {
        checks.push(check(
            "local-network-isolation",
            "Network isolation",
            DoctorVerdict::Warn,
            "The local backend cannot enforce host-level network profiles.",
            "Use OS-level controls when network egress isolation is required.",
        ));
    }

    if checks.is_empty() {
        checks.push(check(
            "local-ready",
            "Local backend",
            DoctorVerdict::Pass,
            "Local backend is available.",
            "No action needed.",
        ));
    }
}

fn check(
    id: impl Into<String>,
    label: impl Into<String>,
    verdict: DoctorVerdict,
    detail: impl Into<String>,
    fix: impl Into<String>,
) -> BackendDoctorCheck {
    BackendDoctorCheck {
        id: id.into(),
        label: label.into(),
        verdict,
        detail: detail.into(),
        fix: fix.into(),
    }
}

/// A process-effect request with an exact executable and argument vector.
///
/// This is intentionally a data boundary: it has no shell command field and
/// no policy decision method. A performer is injected by the owning runtime.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct EffectExecutionRequest {
    pub invocation_id: String,
    pub task_id: String,
    pub plan_id: String,
    pub policy_id: String,
    pub capability_id: String,
    pub executable: String,
    #[serde(default)]
    pub argv: Vec<String>,
    pub cwd: String,
    #[serde(default)]
    pub environment: Vec<EnvironmentVariable>,
    pub sandbox: SandboxRequest,
    pub network: NetworkRequest,
    pub resource_limits: EffectResourceLimits,
    pub output_limits: OutputLimits,
    pub deadline: AbsoluteDeadline,
    /// The cancellation signal is runtime-only and is never serialized.
    #[serde(skip, default)]
    pub cancellation: CancellationToken,
}

impl EffectExecutionRequest {
    pub fn new(
        invocation_id: impl Into<String>,
        executable: impl Into<String>,
        argv: Vec<String>,
        cwd: impl Into<String>,
    ) -> Self {
        Self {
            invocation_id: invocation_id.into(),
            task_id: String::new(),
            plan_id: String::new(),
            policy_id: String::new(),
            capability_id: String::new(),
            executable: executable.into(),
            argv,
            cwd: cwd.into(),
            environment: Vec::new(),
            sandbox: SandboxRequest::default(),
            network: NetworkRequest::default(),
            resource_limits: EffectResourceLimits::default(),
            output_limits: OutputLimits::default(),
            deadline: AbsoluteDeadline::from_now(Duration::from_secs(300)),
            cancellation: CancellationToken::new(),
        }
    }

    pub fn exact_argv(&self) -> Vec<&str> {
        std::iter::once(self.executable.as_str())
            .chain(self.argv.iter().map(String::as_str))
            .collect()
    }

    pub fn with_context(
        mut self,
        task_id: impl Into<String>,
        plan_id: impl Into<String>,
        policy_id: impl Into<String>,
        capability_id: impl Into<String>,
    ) -> Self {
        self.task_id = task_id.into();
        self.plan_id = plan_id.into();
        self.policy_id = policy_id.into();
        self.capability_id = capability_id.into();
        self
    }

    pub fn validate(&self) -> Result<(), EffectExecutionError> {
        for (label, value) in [
            ("invocation_id", &self.invocation_id),
            ("task_id", &self.task_id),
            ("plan_id", &self.plan_id),
            ("policy_id", &self.policy_id),
            ("capability_id", &self.capability_id),
            ("executable", &self.executable),
            ("cwd", &self.cwd),
        ] {
            if value.is_empty() {
                return Err(EffectExecutionError::InvalidRequest {
                    field: label.to_string(),
                    reason: "must not be empty".to_string(),
                });
            }
            if value.contains('\0') {
                return Err(EffectExecutionError::InvalidRequest {
                    field: label.to_string(),
                    reason: "contains NUL".to_string(),
                });
            }
        }
        if self.argv.iter().any(|arg| arg.contains('\0')) {
            return Err(EffectExecutionError::InvalidRequest {
                field: "argv".to_string(),
                reason: "contains NUL".to_string(),
            });
        }
        for variable in &self.environment {
            variable.validate()?;
        }
        Ok(())
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }
}

impl PartialEq for EffectExecutionRequest {
    fn eq(&self, other: &Self) -> bool {
        self.invocation_id == other.invocation_id
            && self.task_id == other.task_id
            && self.plan_id == other.plan_id
            && self.policy_id == other.policy_id
            && self.capability_id == other.capability_id
            && self.executable == other.executable
            && self.argv == other.argv
            && self.cwd == other.cwd
            && self.environment == other.environment
            && self.sandbox == other.sandbox
            && self.network == other.network
            && self.resource_limits == other.resource_limits
            && self.output_limits == other.output_limits
            && self.deadline == other.deadline
    }
}

impl Eq for EffectExecutionRequest {}

/// A secret environment value which serializes and debugs as a redaction.
#[derive(Clone, Eq, PartialEq)]
pub struct RedactedEnvironmentValue(String);

impl RedactedEnvironmentValue {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for RedactedEnvironmentValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RedactedEnvironmentValue([REDACTED])")
    }
}

impl Serialize for RedactedEnvironmentValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str("[REDACTED]")
    }
}

impl<'de> Deserialize<'de> for RedactedEnvironmentValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        String::deserialize(deserializer).map(Self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentVariable {
    pub name: String,
    pub value: RedactedEnvironmentValue,
}

impl EnvironmentVariable {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: RedactedEnvironmentValue::new(value),
        }
    }

    fn validate(&self) -> Result<(), EffectExecutionError> {
        if self.name.is_empty() || self.name.contains('=') || self.name.contains('\0') {
            return Err(EffectExecutionError::InvalidRequest {
                field: "environment.name".to_string(),
                reason: "must be a non-empty variable name".to_string(),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct SandboxRequest {
    pub profile: Option<String>,
    #[serde(default)]
    pub required: bool,
}

/// Backend-neutral launch contract shared by daemon process producers.
/// Implementations must either return a fully prepared, confined argv or a
/// typed error; callers must never fall back to raw process creation when a
/// policy is requested.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessLaunchContext {
    pub workspace_root: String,
    pub sandbox_profile: Option<String>,
    #[serde(default)]
    pub sandbox_required: bool,
    #[serde(default)]
    pub network_required: bool,
}

/// Evidence returned by a managed child proving which native confinement
/// backend owns its process boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SandboxProcessProof {
    pub backend: String,
    pub profile: String,
    pub enforced: bool,
    pub network_enforced: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedProcessLaunch {
    pub program: String,
    pub argv: Vec<String>,
    pub backend: String,
}

pub trait CommandSandboxAdapter: Send + Sync {
    fn prepare(
        &self,
        program: &str,
        argv: &[String],
        context: &ProcessLaunchContext,
    ) -> Result<PreparedProcessLaunch, EffectExecutionError>;
}

/// Sandbox profile name of plugin subprocesses. A child launched under it starts from an empty
/// environment: `CommandLaunchSpec::env` is the whole environment, not an overlay on the daemon's.
pub const PLUGIN_SANDBOX_PROFILE: &str = "plugin";

/// Complete child-launch input used by daemon-owned process producers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandLaunchSpec {
    pub program: String,
    pub argv: Vec<String>,
    pub cwd: String,
    pub context: ProcessLaunchContext,
    /// Environment passed to child process, kept in launch contract so
    /// policy adapters remain the sole process-creation boundary.
    pub env: Vec<(String, String)>,
}

impl CommandLaunchSpec {
    /// True when the child must start from an empty environment and see only `env`.
    pub fn starts_from_empty_env(&self) -> bool {
        self.context.sandbox_profile.as_deref() == Some(PLUGIN_SANDBOX_PROFILE)
    }
}

/// Initial/current PTY dimensions. This is deliberately only kernel PTY state:
/// the host app does not own a terminal cell grid, scrollback, fonts, or rendering.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct PtySize {
    pub rows: u16,
    pub cols: u16,
    #[serde(default)]
    pub pixel_width: u16,
    #[serde(default)]
    pub pixel_height: u16,
}

impl Default for PtySize {
    fn default() -> Self {
        Self {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

/// Managed child I/O channel. `Pipe` remains the default for ordinary
/// non-interactive work; `Pty` is an explicit interactive execution request.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ExecutionChannel {
    #[default]
    Pipe,
    Pty(PtySize),
}

/// Opaque managed child returned by a native adapter. Concrete hosts retain
/// ownership of platform handles and implement cancellation/reaping without
/// exposing OS-specific process types to portable crates.
#[async_trait]
pub trait ManagedChild: Send {
    fn pid(&self) -> Option<u32>;
    fn sandbox_proof(&self) -> Option<SandboxProcessProof> {
        None
    }
    fn execution_channel(&self) -> ExecutionChannel {
        ExecutionChannel::Pipe
    }
    fn start_kill(&mut self) -> bool {
        false
    }
    /// Returned input is raw bytes. `AsyncWrite::shutdown` is the managed EOF operation.
    fn take_stdin(&mut self) -> Option<Pin<Box<dyn tokio::io::AsyncWrite + Send>>>;
    /// PTY output is a raw byte stream; stderr is multiplexed into stdout and
    /// `take_stderr` therefore returns `None` for PTY children.
    fn take_stdout(&mut self) -> Option<Pin<Box<dyn tokio::io::AsyncRead + Send>>>;
    fn take_stderr(&mut self) -> Option<Pin<Box<dyn tokio::io::AsyncRead + Send>>>;
    fn resize_pty(&mut self, _size: PtySize) -> Result<(), EffectExecutionError> {
        Err(EffectExecutionError::InvalidRequest {
            field: "execution_channel".to_string(),
            reason: "managed child is not a PTY".to_string(),
        })
    }
    async fn wait(&mut self) -> Result<i32, EffectExecutionError>;
    async fn cancel(&mut self) -> Result<(), EffectExecutionError>;
}

#[async_trait]
pub trait ProcessLaunchAdapter: Send + Sync {
    async fn spawn(
        &self,
        spec: CommandLaunchSpec,
    ) -> Result<Box<dyn ManagedChild>, EffectExecutionError>;

    async fn spawn_channel(
        &self,
        spec: CommandLaunchSpec,
        channel: ExecutionChannel,
    ) -> Result<Box<dyn ManagedChild>, EffectExecutionError> {
        match channel {
            ExecutionChannel::Pipe => self.spawn(spec).await,
            ExecutionChannel::Pty(_) => Err(EffectExecutionError::InvalidRequest {
                field: "execution_channel".to_string(),
                reason: "process adapter does not support managed PTY execution".to_string(),
            }),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct NetworkRequest {
    pub profile: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub allow_hosts: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct EffectResourceLimits {
    pub cpu_millis: Option<u64>,
    pub memory_bytes: Option<u64>,
    pub disk_bytes: Option<u64>,
    pub process_count: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct OutputLimits {
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
}

impl Default for OutputLimits {
    fn default() -> Self {
        Self {
            stdout_bytes: 1024 * 1024,
            stderr_bytes: 1024 * 1024,
        }
    }
}

/// An absolute wall-clock deadline represented as Unix milliseconds.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AbsoluteDeadline(pub u64);

impl AbsoluteDeadline {
    pub fn from_unix_millis(unix_millis: u64) -> Self {
        Self(unix_millis)
    }

    pub fn from_now(duration: Duration) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Self(now.saturating_add(duration).as_millis() as u64)
    }

    pub fn unix_millis(self) -> u64 {
        self.0
    }

    pub fn is_expired(self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        now.as_millis() as u64 >= self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct EffectExecutionOutcome {
    pub status: EffectExecutionStatus,
    pub evidence: EffectExecutionEvidence,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EffectExecutionStatus {
    Succeeded,
    Failed,
    TimedOut,
    Cancelled,
    OutputLimited,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct EffectExecutionEvidence {
    pub exit: ExitEvidence,
    pub stdout: OutputEvidence,
    pub stderr: OutputEvidence,
    pub process_tree: ProcessTreeEvidence,
    pub sandbox: SandboxEvidence,
    pub network: NetworkEvidence,
    pub artifacts: ArtifactEvidence,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct ExitEvidence {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct OutputEvidence {
    pub digest: Option<String>,
    pub bytes_seen: u64,
    pub bytes_captured: u64,
    pub limit_bytes: u64,
    pub truncated: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct ProcessTreeEvidence {
    pub cleanup_attempted: bool,
    pub cleanup_complete: bool,
    pub descendants_seen: Option<u32>,
    pub descendants_remaining: Option<u32>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct SandboxEvidence {
    pub enforced: bool,
    pub profile: Option<String>,
    pub backend: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct NetworkEvidence {
    pub enforced: bool,
    pub profile: Option<String>,
    pub egress_observed: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactEvidence {
    pub committed: bool,
    pub sink: Option<String>,
    #[serde(default)]
    pub digests: Vec<String>,
}

/// Stable, machine-readable failure classes for the effect boundary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "code", content = "detail")]
pub enum EffectExecutionError {
    InvalidRequest { field: String, reason: String },
    MissingExecutable,
    UnsealedExecutable,
    UnauthorizedEffect,
    SandboxMissing,
    ResourceLimitsMissing,
    SpawnFailed,
    Timeout,
    Cancelled,
    OutputLimited,
    KillFailed,
    ArtifactFailed,
    Internal,
}

impl EffectExecutionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidRequest { .. } => "invalid_request",
            Self::MissingExecutable => "missing_executable",
            Self::UnsealedExecutable => "unsealed_executable",
            Self::UnauthorizedEffect => "unauthorized_effect",
            Self::SandboxMissing => "sandbox_missing",
            Self::ResourceLimitsMissing => "resource_limits_missing",
            Self::SpawnFailed => "spawn_failed",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::OutputLimited => "output_limited",
            Self::KillFailed => "kill_failed",
            Self::ArtifactFailed => "artifact_failed",
            Self::Internal => "internal",
        }
    }
}

impl fmt::Display for EffectExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl std::error::Error for EffectExecutionError {}

/// Injected asynchronous effect boundary. Implementations own process APIs,
/// sandbox setup, policy integration, and process-tree cleanup.
#[async_trait]
pub trait EffectPerformer: Send + Sync {
    async fn perform(
        &self,
        request: EffectExecutionRequest,
    ) -> Result<EffectExecutionOutcome, EffectExecutionError>;

    async fn execute(
        &self,
        request: EffectExecutionRequest,
    ) -> Result<EffectExecutionOutcome, EffectExecutionError> {
        self.perform(request).await
    }
}

/// Production local performer for the host app's effect boundary.
///
/// It launches an exact executable/argv vector through `tokio::process` with
/// no shell, carries request limits into the process boundary, and returns
/// bounded output plus cleanup/sandbox/network evidence. Policy remains owned
/// by `host_legion-policy`; this type only performs an already-authorized
/// request.
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalEffectPerformer;

/// Stable name for the performer injected by embedded Legion composition.
pub type HostEffectPerformer = LocalEffectPerformer;

#[async_trait]
impl EffectPerformer for LocalEffectPerformer {
    async fn perform(
        &self,
        request: EffectExecutionRequest,
    ) -> Result<EffectExecutionOutcome, EffectExecutionError> {
        request.validate()?;
        if !Path::new(&request.cwd).is_absolute() {
            return Err(EffectExecutionError::InvalidRequest {
                field: "cwd".into(),
                reason: "must be absolute".into(),
            });
        }
        if !Path::new(&request.executable).is_absolute() {
            return Err(EffectExecutionError::InvalidRequest {
                field: "executable".into(),
                reason: "must be absolute".into(),
            });
        }
        if request.output_limits.stdout_bytes == 0 || request.output_limits.stderr_bytes == 0 {
            return Err(EffectExecutionError::InvalidRequest {
                field: "output_limits".into(),
                reason: "limits must be positive".into(),
            });
        }
        if request.sandbox.required || request.sandbox.profile.is_some() {
            return Err(EffectExecutionError::SandboxMissing);
        }
        if request.network.required
            || request.network.profile.is_some()
            || !request.network.allow_hosts.is_empty()
        {
            return Err(EffectExecutionError::SandboxMissing);
        }
        if request.resource_limits != EffectResourceLimits::default() {
            return Err(EffectExecutionError::ResourceLimitsMissing);
        }
        if request.is_cancelled() {
            return Err(EffectExecutionError::Cancelled);
        }
        if request.deadline.is_expired() {
            return Err(EffectExecutionError::Timeout);
        }

        // Crate sits below host_tools, so the no-window constructor is not
        // reachable here; RightKit's `NoWindow` is applied directly.
        #[allow(clippy::disallowed_methods)]
        let mut command = tokio::process::Command::new(&request.executable);
        rightkit_child_process::NoWindow::no_window(&mut command);
        command
            .args(&request.argv)
            .current_dir(&request.cwd)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if !configure_process_tree(&mut command) {
            return Err(EffectExecutionError::Internal);
        }
        for variable in &request.environment {
            command.env(&variable.name, variable.value.expose());
        }
        let child = command
            .spawn()
            .map_err(|_| EffectExecutionError::SpawnFailed)?;
        let output = match wait_for_bounded_output(
            child,
            request.output_limits.stdout_bytes,
            request.output_limits.stderr_bytes,
            request.cancellation.clone(),
            request.deadline,
        )
        .await
        .map_err(|_| EffectExecutionError::Internal)?
        {
            WaitResult::Completed(output) => output,
            WaitResult::Cancelled(cleanup_complete) => {
                return Ok(interrupted_outcome(
                    EffectExecutionStatus::Cancelled,
                    cleanup_complete,
                ))
            }
            WaitResult::TimedOut(cleanup_complete) => {
                return Ok(interrupted_outcome(
                    EffectExecutionStatus::TimedOut,
                    cleanup_complete,
                ))
            }
        };

        let (stdout, stderr, exit_status, cleanup_attempted, cleanup_complete) = output;
        let output_limited = stdout.truncated || stderr.truncated;
        let status = if output_limited {
            EffectExecutionStatus::OutputLimited
        } else if exit_status.success() {
            EffectExecutionStatus::Succeeded
        } else {
            EffectExecutionStatus::Failed
        };
        Ok(EffectExecutionOutcome {
            status,
            evidence: EffectExecutionEvidence {
                exit: ExitEvidence {
                    exit_code: exit_status.code(),
                    signal: exit_signal(&exit_status),
                },
                stdout: stdout.evidence,
                stderr: stderr.evidence,
                process_tree: ProcessTreeEvidence {
                    cleanup_attempted,
                    cleanup_complete,
                    descendants_seen: None,
                    descendants_remaining: None,
                },
                sandbox: SandboxEvidence::default(),
                network: NetworkEvidence::default(),
                artifacts: ArtifactEvidence {
                    committed: false,
                    sink: None,
                    digests: Vec::new(),
                },
            },
        })
    }
}

fn interrupted_outcome(
    status: EffectExecutionStatus,
    cleanup_complete: bool,
) -> EffectExecutionOutcome {
    EffectExecutionOutcome {
        status,
        evidence: EffectExecutionEvidence {
            process_tree: ProcessTreeEvidence {
                cleanup_attempted: true,
                cleanup_complete,
                descendants_seen: None,
                descendants_remaining: None,
            },
            sandbox: SandboxEvidence::default(),
            network: NetworkEvidence::default(),
            ..EffectExecutionEvidence::default()
        },
    }
}

struct BoundedOutput {
    evidence: OutputEvidence,
    truncated: bool,
}

enum WaitResult {
    Completed(
        (
            BoundedOutput,
            BoundedOutput,
            std::process::ExitStatus,
            bool,
            bool,
        ),
    ),
    Cancelled(bool),
    TimedOut(bool),
}

async fn wait_for_bounded_output(
    mut child: tokio::process::Child,
    stdout_limit: u64,
    stderr_limit: u64,
    cancellation: CancellationToken,
    deadline: AbsoluteDeadline,
) -> std::io::Result<WaitResult> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("stdout pipe missing"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("stderr pipe missing"))?;
    let exceeded = Arc::new(AtomicBool::new(false));
    let notify = Arc::new(tokio::sync::Notify::new());
    let mut stdout_read = tokio::spawn(read_bounded(
        stdout,
        stdout_limit,
        exceeded.clone(),
        notify.clone(),
    ));
    let mut stderr_read = tokio::spawn(read_bounded(
        stderr,
        stderr_limit,
        exceeded.clone(),
        notify.clone(),
    ));
    let cancellation_wait = cancellation.cancelled();
    tokio::pin!(cancellation_wait);
    let deadline_wait = sleep_until(deadline);
    tokio::pin!(deadline_wait);
    let child_pid = child.id();
    let (status, cleanup_attempted, cleanup_complete) = loop {
        if let Some(status) = child.try_wait()? {
            let cleanup_attempted = !process_tree_quiescent(child_pid);
            if cleanup_attempted {
                terminate_process_tree(&mut child, child_pid).await;
            }
            let cleanup_complete = wait_for_process_tree_quiescence(child_pid).await;
            break (status, cleanup_attempted, cleanup_complete);
        }
        tokio::select! {
            _ = notify.notified() => {
                terminate_process_tree(&mut child, child_pid).await;
                let cleanup_complete = wait_for_process_tree_quiescence(child_pid).await;
                break (child.wait().await?, true, cleanup_complete);
            }
            _ = &mut cancellation_wait => {
                terminate_process_tree(&mut child, child_pid).await;
                stdout_read.abort();
                stderr_read.abort();
                return Ok(WaitResult::Cancelled(wait_for_process_tree_quiescence(child_pid).await));
            }
            _ = &mut deadline_wait => {
                terminate_process_tree(&mut child, child_pid).await;
                stdout_read.abort();
                stderr_read.abort();
                return Ok(WaitResult::TimedOut(wait_for_process_tree_quiescence(child_pid).await));
            }
            _ = tokio::time::sleep(Duration::from_millis(10)) => {}
        }
    };
    let stdout = match tokio::time::timeout(Duration::from_secs(1), &mut stdout_read).await {
        Ok(result) => result.map_err(|_| std::io::Error::other("stdout reader cancelled"))??,
        Err(_) => {
            stdout_read.abort();
            stderr_read.abort();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "stdout pipe remained open after process cleanup",
            ));
        }
    };
    let stderr = match tokio::time::timeout(Duration::from_secs(1), &mut stderr_read).await {
        Ok(result) => result.map_err(|_| std::io::Error::other("stderr reader cancelled"))??,
        Err(_) => {
            stderr_read.abort();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "stderr pipe remained open after process cleanup",
            ));
        }
    };
    Ok(WaitResult::Completed((
        stdout,
        stderr,
        status,
        cleanup_attempted,
        cleanup_complete,
    )))
}

/// Bounded stream capture for callers that need the bytes themselves
/// (blocking hooks, adapters): drains the stream to EOF so a blocked writer
/// never stalls the child, retains only the first `limit` bytes, and reports
/// whether truncation occurred. Shares `read_bounded`'s drain-and-truncate
/// contract without its evidence-only output shape.
pub async fn capture_stream_bounded<R: AsyncRead + Unpin>(
    mut reader: R,
    limit: u64,
) -> std::io::Result<(Vec<u8>, bool)> {
    let mut captured = Vec::new();
    let mut bytes_seen = 0u64;
    let mut buffer = [0u8; 8192];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        bytes_seen = bytes_seen.saturating_add(count as u64);
        let remaining = limit.saturating_sub(captured.len() as u64) as usize;
        captured.extend_from_slice(&buffer[..count.min(remaining)]);
    }
    let truncated = bytes_seen > captured.len() as u64;
    Ok((captured, truncated))
}

async fn read_bounded<R: AsyncRead + Unpin>(
    mut reader: R,
    limit: u64,
    exceeded: Arc<AtomicBool>,
    notify: Arc<tokio::sync::Notify>,
) -> std::io::Result<BoundedOutput> {
    let mut captured = Vec::new();
    let mut bytes_seen = 0u64;
    let mut buffer = [0u8; 8192];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        bytes_seen = bytes_seen.saturating_add(count as u64);
        let remaining = limit.saturating_sub(captured.len() as u64) as usize;
        captured.extend_from_slice(&buffer[..count.min(remaining)]);
        if bytes_seen > limit && !exceeded.swap(true, Ordering::SeqCst) {
            notify.notify_one();
        }
    }
    let truncated = bytes_seen > captured.len() as u64;
    let digest = rightkit_crypto::sha256(&captured)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(BoundedOutput {
        truncated,
        evidence: OutputEvidence {
            digest: Some(format!("sha256:{digest}")),
            bytes_seen,
            bytes_captured: captured.len() as u64,
            limit_bytes: limit,
            truncated,
        },
    })
}

fn configure_process_tree(command: &mut tokio::process::Command) -> bool {
    #[cfg(unix)]
    {
        // The child leads its own process group, which is what
        // `rightkit_child_process::tokio_support::terminate_tree` signals.
        command.process_group(0);
        true
    }
    #[cfg(not(unix))]
    {
        let _ = command;
        false
    }
}

fn process_tree_quiescent(pid: Option<u32>) -> bool {
    #[cfg(unix)]
    {
        let Some(pgid) = pid.and_then(std::num::NonZeroU32::new) else {
            return false;
        };
        // Observation only (signal 0); an error or a refused group id is "not quiescent".
        return matches!(
            rightkit_child_process::tokio_support::process_group_exists(pgid),
            Ok(false)
        );
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

async fn wait_for_process_tree_quiescence(pid: Option<u32>) -> bool {
    for _ in 0..40 {
        if process_tree_quiescent(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    process_tree_quiescent(pid)
}

async fn terminate_process_tree(child: &mut tokio::process::Child, _pid: Option<u32>) {
    let options = rightkit_child_process::TerminationOptions::default()
        .with_grace(Duration::from_millis(500));
    let _ = rightkit_child_process::tokio_support::terminate_tree(child, options).await;
}

async fn sleep_until(deadline: AbsoluteDeadline) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    tokio::time::sleep(Duration::from_millis(
        deadline.unix_millis().saturating_sub(now),
    ))
    .await;
}

fn exit_signal(status: &std::process::ExitStatus) -> Option<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_registry_contains_only_local_backend() {
        let registry = ExecutionBackendRegistry::default();
        assert_eq!(registry.default_backend, "local");
        let backend_ids: Vec<&str> = registry
            .backends
            .iter()
            .map(|backend| backend.id.as_str())
            .collect();
        assert_eq!(backend_ids, vec!["local"]);
        assert!(registry
            .network_profiles
            .iter()
            .any(|profile| profile.id == "local-host"));
    }

    #[test]
    fn local_backend_doctor_preserves_existing_workspace() {
        let registry = ExecutionBackendRegistry::default();
        let cwd = std::env::current_dir().unwrap();
        let report = registry.doctor("local", Some(&cwd)).unwrap();
        assert!(report.checks.iter().any(
            |check| check.id == "local-workspace-root" && check.verdict == DoctorVerdict::Pass
        ));
    }

    #[cfg(unix)]
    fn shell_request(script: &str) -> EffectExecutionRequest {
        let cwd = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        EffectExecutionRequest::new(
            "fault-test",
            "/bin/sh",
            vec!["-c".into(), script.into()],
            cwd,
        )
        .with_context("task", "plan", "policy", "capability")
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn local_performer_terminates_and_measures_output_cap_cleanup() {
        let mut request = shell_request("printf 0123456789; sleep 5");
        request.output_limits.stdout_bytes = 2;
        let outcome = LocalEffectPerformer.perform(request).await.unwrap();
        assert_eq!(outcome.status, EffectExecutionStatus::OutputLimited);
        assert!(outcome.evidence.stdout.bytes_seen > outcome.evidence.stdout.limit_bytes);
        assert!(outcome.evidence.process_tree.cleanup_attempted);
        assert!(outcome.evidence.process_tree.cleanup_complete);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn local_performer_timeout_reports_measured_tree_cleanup() {
        let mut request = shell_request("sleep 5");
        request.deadline = AbsoluteDeadline::from_now(Duration::from_millis(25));
        let outcome = LocalEffectPerformer.perform(request).await.unwrap();
        assert_eq!(outcome.status, EffectExecutionStatus::TimedOut);
        assert!(outcome.evidence.process_tree.cleanup_attempted);
        assert!(outcome.evidence.process_tree.cleanup_complete);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn local_performer_cancellation_kills_process_group() {
        let request = shell_request("sleep 5 & wait");
        let cancellation = request.cancellation.clone();
        let task = tokio::spawn(async move { LocalEffectPerformer.perform(request).await });
        tokio::time::sleep(Duration::from_millis(25)).await;
        cancellation.cancel();
        let outcome = task.await.unwrap().unwrap();
        assert_eq!(outcome.status, EffectExecutionStatus::Cancelled);
        assert!(outcome.evidence.process_tree.cleanup_attempted);
        assert!(outcome.evidence.process_tree.cleanup_complete);
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn local_performer_fails_closed_without_sandbox_or_network_enforcement() {
        let mut sandbox = shell_request("true");
        sandbox.sandbox.required = true;
        assert!(matches!(
            LocalEffectPerformer.perform(sandbox).await,
            Err(EffectExecutionError::SandboxMissing)
        ));
        let mut network = shell_request("true");
        network.network.required = true;
        assert!(matches!(
            LocalEffectPerformer.perform(network).await,
            Err(EffectExecutionError::SandboxMissing)
        ));

        let mut hosts = shell_request("true");
        hosts.network.allow_hosts.push("example.com".into());
        assert!(matches!(
            LocalEffectPerformer.perform(hosts).await,
            Err(EffectExecutionError::SandboxMissing)
        ));

        let mut limits = shell_request("true");
        limits.resource_limits.process_count = Some(1);
        assert!(matches!(
            LocalEffectPerformer.perform(limits).await,
            Err(EffectExecutionError::ResourceLimitsMissing)
        ));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn local_performer_rejects_path_resolved_executable() {
        let mut request = shell_request("true");
        request.executable = "sh".into();
        assert!(matches!(
            LocalEffectPerformer.perform(request).await,
            Err(EffectExecutionError::InvalidRequest { field, .. }) if field == "executable"
        ));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn local_performer_cleans_descendants_after_parent_exits() {
        let request = shell_request("sleep 5 & exit 0");
        let outcome = tokio::time::timeout(
            Duration::from_secs(2),
            LocalEffectPerformer.perform(request),
        )
        .await
        .expect("performer must not wait for inherited descendant pipes")
        .unwrap();
        assert_eq!(outcome.status, EffectExecutionStatus::Succeeded);
        assert!(outcome.evidence.process_tree.cleanup_attempted);
        assert!(outcome.evidence.process_tree.cleanup_complete);
    }
}
