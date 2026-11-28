//! Daemon-side executor adapters. Runtime ports are injected by daemon startup;
//! missing ports fail closed.

#![forbid(unsafe_code)]

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;
use tracing::{debug, info, warn};

use rightkit_workflow::control_plane::a2a::{
    classify_output, validate_admission, validate_descriptor, workspace_contains,
    A2aExecutorDescriptor, A2aIdentityProof, A2aIdentityVerifier, A2aResult,
};
use rightkit_workflow::control_plane::context::ContextManifest;
use rightkit_workflow::control_plane::domain::{Capability, FailureClass, NodeKind, NodeSpec};
use rightkit_workflow::control_plane::executors::{ExecutorAdapter, ExecutorError, NodeOutput};

// ─── Errors ──────────────────────────────────────────────────────────────────

/// Typed failures specific to the native / conductor adapters. These are
/// *adapter-side* errors distinct from
/// [`ExecutorError`](rightkit_workflow::control_plane::executors::ExecutorError) which is
/// the cross-adapter contract error. Daemon-side errors are richer because
/// they have access to runtime state (storage, providers, telemetry).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AdapterError {
    /// The adapter needed a runtime handle that was not supplied (e.g. the
    /// `SessionEngine` was not built because the feature gate is off).
    #[error("adapter `{0}` missing runtime handle: {1}")]
    MissingRuntime(&'static str, String),
    /// The adapter failed to translate the manifest into a runtime
    /// request shape. The reducer maps this to
    /// [`FailureClass::InvalidBinding`](rightkit_workflow::control_plane::domain::FailureClass::InvalidBinding).
    #[error("adapter `{adapter}` could not project manifest: {reason}")]
    ProjectionFailed {
        /// Name of the adapter that failed.
        adapter: &'static str,
        /// Why projection failed.
        reason: String,
    },
}

/// Identity & local cancellation state for one A2A attempt. Retry attempts
/// receive distinct ids, so timeout cancellation cannot target later work.
#[derive(Debug, Clone)]
pub struct A2aAttempt {
    id: Arc<str>,
    cancelled: Arc<AtomicBool>,
}

impl A2aAttempt {
    pub fn from_manifest(manifest: &ContextManifest) -> Self {
        let provenance = manifest.provenance();
        Self {
            id: Arc::from(format!(
                "{}:{}:{}",
                provenance.run_id, provenance.node_id, provenance.reducer_revision
            )),
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Stable transport key for this attempt.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Transport implementations should stop before starting new work when
    /// this flag is set.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn request_cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

/// Bounded A2A lane. Construction requires a validated descriptor; execution
/// remains fail-closed until daemon supplies a trusted transport port.
pub struct A2aAdapter {
    descriptor: A2aExecutorDescriptor,
    verifier: Option<Arc<dyn A2aIdentityVerifier + Send + Sync>>,
    transport: Option<Arc<dyn A2aExecutionPort>>,
}

impl A2aAdapter {
    pub fn new(descriptor: A2aExecutorDescriptor) -> Result<Self, String> {
        validate_descriptor(&descriptor).map_err(|e| e.to_string())?;
        Ok(Self {
            descriptor,
            verifier: None,
            transport: None,
        })
    }

    /// Construct a live adapter only when both trust verification & transport
    /// are injected by daemon startup. Runtime execution remains fail-closed.
    pub fn with_transport(
        descriptor: A2aExecutorDescriptor,
        verifier: Arc<dyn A2aIdentityVerifier + Send + Sync>,
        transport: Arc<dyn A2aExecutionPort>,
    ) -> Result<Self, String> {
        validate_descriptor(&descriptor).map_err(|e| e.to_string())?;
        Ok(Self {
            descriptor,
            verifier: Some(verifier),
            transport: Some(transport),
        })
    }

    /// Descriptor workspace used by dispatcher-level workflow containment.
    pub fn workspace_root(&self) -> &str {
        &self.descriptor.workspace_root
    }

    /// Stable executor binding used by workflow admission to prevent a node
    /// from silently dispatching to a different injected remote executor.
    pub fn executor_id(&self) -> &str {
        &self.descriptor.executor_id
    }

    /// Request cancellation after dispatcher deadline expiry. An absent
    /// transport remains a typed failure rather than a best-effort success.
    pub fn cancel(&self, attempt: &A2aAttempt) -> Result<(), ExecutorError> {
        attempt.request_cancel();
        let transport = self
            .transport
            .as_ref()
            .ok_or_else(|| ExecutorError::internal("a2a", "remote transport is not configured"))?;
        transport.cancel(attempt).map_err(ExecutorError::from)
    }

    /// Validate all daemon/workflow admission inputs before starting remote
    /// work. The dispatcher calls this outside its blocking execution task so
    /// an untrusted descriptor cannot reach transport merely by being wired.
    pub fn validate_workflow_admission(
        &self,
        spec: &NodeSpec,
        manifest: &ContextManifest,
        workflow_workspace_root: &str,
    ) -> Result<(), ExecutorError> {
        let verifier = self
            .verifier
            .as_ref()
            .ok_or_else(|| ExecutorError::internal("a2a", "identity verifier unavailable"))?;
        let transport = self
            .transport
            .as_ref()
            .ok_or_else(|| ExecutorError::internal("a2a", "remote transport is not configured"))?;
        self.validate_manifest_budget(manifest)?;
        for required in self.required_capabilities(spec) {
            if !manifest.capabilities().contains(&required) {
                return Err(ExecutorError::CapabilityDenied {
                    adapter: "a2a",
                    required: required.to_string(),
                    kind: spec.kind,
                });
            }
        }
        if !workspace_contains(workflow_workspace_root, &self.descriptor.workspace_root) {
            return Err(ExecutorError::InvalidManifest {
                adapter: "a2a",
                reason: "descriptor workspace escapes workflow workspace".into(),
            });
        }
        validate_admission(
            &self.descriptor,
            &transport.identity_proof(),
            manifest.capabilities(),
            transport.local_workspace_root(),
            verifier.as_ref(),
        )
        .map_err(|error| ExecutorError::InvalidManifest {
            adapter: "a2a",
            reason: error.to_string(),
        })
    }
}

impl A2aAdapter {
    /// Execute one attempt with cancellation bound to its provenance.
    pub fn execute_attempt(
        &self,
        spec: &NodeSpec,
        manifest: &ContextManifest,
        attempt: &A2aAttempt,
    ) -> Result<NodeOutput, ExecutorError> {
        ensure_supported_kind(self.name(), spec)?;
        if attempt.is_cancelled() {
            return Ok(NodeOutput::Cancelled {
                reason: "A2A attempt was cancelled before dispatch".into(),
            });
        }
        let verifier = self
            .verifier
            .as_ref()
            .ok_or_else(|| ExecutorError::internal("a2a", "identity verifier unavailable"))?;
        let transport = self
            .transport
            .as_ref()
            .ok_or_else(|| ExecutorError::internal("a2a", "remote transport is not configured"))?;
        self.validate_manifest_budget(manifest)?;
        for required in self.required_capabilities(spec) {
            if !manifest.capabilities().contains(&required) {
                return Err(ExecutorError::CapabilityDenied {
                    adapter: "a2a",
                    required: required.to_string(),
                    kind: spec.kind,
                });
            }
        }
        validate_admission(
            &self.descriptor,
            &transport.identity_proof(),
            manifest.capabilities(),
            transport.local_workspace_root(),
            verifier.as_ref(),
        )
        .map_err(|e| ExecutorError::InvalidManifest {
            adapter: "a2a",
            reason: e.to_string(),
        })?;
        let output = transport
            .execute(spec, manifest, attempt)
            .map_err(ExecutorError::from)?;
        if attempt.is_cancelled() {
            return Ok(NodeOutput::Cancelled {
                reason: "A2A attempt was cancelled during dispatch".into(),
            });
        }
        match output {
            NodeOutput::Success {
                ref output_json,
                cost_usd_micros,
                tokens_used,
                ..
            } => {
                if let Some(max_tokens) = manifest.budget().max_tokens {
                    if tokens_used > max_tokens {
                        return Ok(NodeOutput::Failure {
                            class: FailureClass::BudgetExceeded,
                            message: "A2A executor exceeded manifest token budget".into(),
                            retryable: false,
                        });
                    }
                }
                if let Some(max_cost) = manifest.budget().max_cost_usd_micros {
                    if cost_usd_micros > max_cost {
                        return Ok(NodeOutput::Failure {
                            class: FailureClass::BudgetExceeded,
                            message: "A2A executor exceeded manifest cost budget".into(),
                            retryable: false,
                        });
                    }
                }
                match classify_output(output_json, self.descriptor.result_schema_version) {
                    A2aResult::Validated { .. } => Ok(output),
                    A2aResult::Quarantined { reason, .. }
                    | A2aResult::Invalid { reason }
                    | A2aResult::Untrusted { reason } => Ok(NodeOutput::Failure {
                        class: FailureClass::InvalidOutput,
                        message: bound_a2a_message(reason),
                        retryable: false,
                    }),
                }
            }
            other => Ok(other),
        }
    }

    fn validate_manifest_budget(&self, manifest: &ContextManifest) -> Result<(), ExecutorError> {
        let max_tokens =
            manifest
                .budget()
                .max_tokens
                .ok_or_else(|| ExecutorError::InvalidManifest {
                    adapter: "a2a",
                    reason: "manifest lacks bounded token budget".into(),
                })?;
        if max_tokens > self.descriptor.max_budget_tokens {
            return Err(ExecutorError::InvalidManifest {
                adapter: "a2a",
                reason: "manifest token budget exceeds A2A descriptor".into(),
            });
        }
        let deadline =
            manifest
                .budget()
                .deadline_unix_ms
                .ok_or_else(|| ExecutorError::InvalidManifest {
                    adapter: "a2a",
                    reason: "manifest lacks bounded wall-clock budget".into(),
                })?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| ExecutorError::internal("a2a", error.to_string()))?
            .as_millis() as i64;
        let remaining = deadline.saturating_sub(now);
        if remaining <= 0 {
            return Err(ExecutorError::InvalidManifest {
                adapter: "a2a",
                reason: "manifest deadline expired".into(),
            });
        }
        if remaining as u64 > self.descriptor.max_wall_clock_ms {
            return Err(ExecutorError::InvalidManifest {
                adapter: "a2a",
                reason: "manifest wall-clock budget exceeds A2A descriptor".into(),
            });
        }
        Ok(())
    }
}

fn bound_a2a_message(message: String) -> String {
    message.chars().take(1024).collect()
}

impl ExecutorAdapter for A2aAdapter {
    fn name(&self) -> &'static str {
        "a2a"
    }
    fn supports(&self, kind: NodeKind) -> bool {
        matches!(kind, NodeKind::Agent | NodeKind::Recipe)
    }
    fn check_capability(
        &self,
        spec: &NodeSpec,
        manifest: &ContextManifest,
    ) -> Result<(), ExecutorError> {
        ensure_supported_kind(self.name(), spec)?;
        for required in self.required_capabilities(spec) {
            if !manifest.capabilities().contains(&required) {
                return Err(ExecutorError::CapabilityDenied {
                    adapter: "a2a",
                    required: required.to_string(),
                    kind: spec.kind,
                });
            }
        }
        Ok(())
    }
    fn required_capabilities(&self, _spec: &NodeSpec) -> Vec<Capability> {
        self.descriptor.capabilities.as_slice().to_vec()
    }
    fn execute(
        &self,
        spec: &NodeSpec,
        manifest: &ContextManifest,
    ) -> Result<NodeOutput, ExecutorError> {
        self.execute_attempt(spec, manifest, &A2aAttempt::from_manifest(manifest))
    }
}

impl From<AdapterError> for ExecutorError {
    fn from(err: AdapterError) -> Self {
        // The dispatcher / reducer only sees [`ExecutorError`]. Map
        // adapter-side errors into the cross-adapter variants so a
        // reducer looking at the journal sees consistent shapes.
        match err {
            AdapterError::MissingRuntime(name, msg) => {
                ExecutorError::internal(name, format!("missing runtime: {msg}"))
            }
            AdapterError::ProjectionFailed { adapter, reason } => {
                ExecutorError::InvalidManifest { adapter, reason }
            }
        }
    }
}

// ─── Shared helpers ──────────────────────────────────────────────────────────

/// Capability strings the native / conductor adapters advertise they need
/// to run any `Agent` / `Recipe` node. The reducer uses these to verify
/// the manifest's [`CapabilitySet`](rightkit_workflow::control_plane::domain::CapabilitySet)
/// before dispatch.
const REQUIRED_CAPABILITIES_AGENT: &[&str] = &["model.call", "tool.read", "fs.read_workspace"];

fn ensure_supported_kind(adapter_name: &'static str, spec: &NodeSpec) -> Result<(), ExecutorError> {
    match spec.kind {
        NodeKind::Agent | NodeKind::Recipe => Ok(()),
        other => {
            warn!(
                target: "agent_server.workflow.executors",
                adapter = adapter_name,
                kind = ?other,
                "adapter received unsupported kind"
            );
            Err(ExecutorError::UnsupportedKind(adapter_name))
        }
    }
}

/// Runtime boundary for an inner execution lane.  It deliberately owns no
/// daemon state: daemon construction supplies a port backed by SessionEngine,
/// Conductor, or driver-host and this adapter remains deterministic to test.
pub trait WorkflowExecutionPort: Send + Sync {
    fn execute(
        &self,
        spec: &NodeSpec,
        manifest: &ContextManifest,
    ) -> Result<NodeOutput, AdapterError>;
}

/// Trusted A2A transport port. The proof is supplied by transport handshake;
/// adapter validates it before delegating each node attempt.
pub trait A2aExecutionPort: Send + Sync {
    /// Local workspace boundary supplied by daemon startup.
    fn local_workspace_root(&self) -> &str;
    fn identity_proof(&self) -> A2aIdentityProof;
    fn execute(
        &self,
        spec: &NodeSpec,
        manifest: &ContextManifest,
        attempt: &A2aAttempt,
    ) -> Result<NodeOutput, AdapterError>;
    /// Cooperatively stop active remote work after its admitted deadline.
    /// Implementations must fail when no cancellation channel exists.
    fn cancel(&self, attempt: &A2aAttempt) -> Result<(), AdapterError>;
}

impl<F> WorkflowExecutionPort for F
where
    F: Fn(&NodeSpec, &ContextManifest) -> Result<NodeOutput, AdapterError> + Send + Sync,
{
    fn execute(
        &self,
        spec: &NodeSpec,
        manifest: &ContextManifest,
    ) -> Result<NodeOutput, AdapterError> {
        self(spec, manifest)
    }
}

/// Named runtime ports keep construction type-safe without forcing workflow
/// code to own a mutable SessionEngine or a vendor subprocess handle.
pub trait NativeExecutionPort: WorkflowExecutionPort {}
impl<T: WorkflowExecutionPort + ?Sized> NativeExecutionPort for T {}

pub trait ConductorExecutionPort: WorkflowExecutionPort {}
impl<T: WorkflowExecutionPort + ?Sized> ConductorExecutionPort for T {}

// ─── NativeAdapter ───────────────────────────────────────────────────────────

/// Native session lane backed by an injected governed runtime port.
pub struct NativeAdapter {
    /// Session lane injected by daemon startup. A concrete port serializes
    /// `SessionEngine::submit_user_message` or driver-host lifecycle calls.
    session_engine: Option<Arc<dyn NativeExecutionPort>>,
}

impl NativeAdapter {
    /// Build an adapter with no runtime handle. Useful for tests and for
    /// daemons that want to construct the adapter up-front before the
    /// `SessionEngine` is built (e.g. when the feature gate is disabled).
    pub fn new() -> Self {
        Self {
            session_engine: None,
        }
    }
}

impl Default for NativeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl ExecutorAdapter for NativeAdapter {
    fn name(&self) -> &'static str {
        "native"
    }

    fn supports(&self, kind: NodeKind) -> bool {
        matches!(kind, NodeKind::Agent | NodeKind::Recipe)
    }

    fn check_capability(
        &self,
        spec: &NodeSpec,
        _manifest: &ContextManifest,
    ) -> Result<(), ExecutorError> {
        ensure_supported_kind(self.name(), spec)
    }

    fn required_capabilities(&self, _spec: &NodeSpec) -> Vec<Capability> {
        REQUIRED_CAPABILITIES_AGENT
            .iter()
            .map(|c| Capability::new(*c))
            .collect()
    }

    fn execute(
        &self,
        spec: &NodeSpec,
        manifest: &ContextManifest,
    ) -> Result<NodeOutput, ExecutorError> {
        debug!(
            target: "agent_server.workflow.executors",
            adapter = "native",
            node_id = %spec.id,
            manifest_hash = %manifest.provenance().content_hash,
            "NativeAdapter::execute called without runtime port"
        );
        let port = self.session_engine.as_ref().ok_or_else(|| {
            AdapterError::MissingRuntime(
                "native",
                "SessionEngine/driver-host port not wired".into(),
            )
        })?;
        port.execute(spec, manifest).map_err(Into::into)
    }
}

// ─── ConductorAdapter ────────────────────────────────────────────────────────

/// Conductor lane backed by an injected orchestration port.
pub struct ConductorAdapter {
    /// Conductor lane injected by daemon startup.
    conductor: Option<Arc<dyn ConductorExecutionPort>>,
}

impl ConductorAdapter {
    /// Build an adapter with no runtime handle.
    pub fn new() -> Self {
        Self { conductor: None }
    }
}

impl Default for ConductorAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl ExecutorAdapter for ConductorAdapter {
    fn name(&self) -> &'static str {
        "conductor"
    }

    fn supports(&self, kind: NodeKind) -> bool {
        matches!(
            kind,
            NodeKind::Agent | NodeKind::Recipe | NodeKind::Subworkflow
        )
    }

    fn check_capability(
        &self,
        spec: &NodeSpec,
        _manifest: &ContextManifest,
    ) -> Result<(), ExecutorError> {
        ensure_supported_kind(self.name(), spec)
    }

    fn required_capabilities(&self, _spec: &NodeSpec) -> Vec<Capability> {
        REQUIRED_CAPABILITIES_AGENT
            .iter()
            .map(|c| Capability::new(*c))
            .collect()
    }

    fn execute(
        &self,
        spec: &NodeSpec,
        manifest: &ContextManifest,
    ) -> Result<NodeOutput, ExecutorError> {
        debug!(
            target: "agent_server.workflow.executors",
            adapter = "conductor",
            node_id = %spec.id,
            manifest_hash = %manifest.provenance().content_hash,
            "ConductorAdapter::execute called without runtime port"
        );
        let port = self.conductor.as_ref().ok_or_else(|| {
            AdapterError::MissingRuntime("conductor", "Conductor port not wired".into())
        })?;
        port.execute(spec, manifest).map_err(Into::into)
    }
}

// ─── Daemon-startup A2A runtime ports (environment-driven; feature `workflow-a2a-env`) ───────

#[cfg(feature = "workflow-a2a-env")]
mod a2a_environment;
#[cfg(feature = "workflow-a2a-env")]
pub use a2a_environment::{
    try_build_daemon_a2a_adapter, Ed25519TrustRootVerifier, TrustedRemoteTransport,
};
