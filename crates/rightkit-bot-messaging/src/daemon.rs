//! The local-daemon HTTP API client contract.
//!
//! Remote adapters talk to the running `host app serve` daemon over its local HTTP API —
//! never to the engine directly. This module defines the contract as a trait so adapters
//! can be tested against an in-memory `StubDaemon`.

use async_trait::async_trait;
use rightkit_agent_engine_protocol::ProtocolEvent;

#[derive(Debug, Clone, thiserror::Error)]
pub enum DaemonError {
    #[error("daemon http error: {0}")]
    Http(String),
    #[error("session not found: {0}")]
    SessionNotFound(String),
    #[error("not authorized")]
    Unauthorized,
}

/// The subset of the daemon API that remote adapters use.
#[async_trait]
pub trait DaemonClient: Send + Sync {
    /// Submit a user message to a session; returns the op id.
    async fn submit_message(&self, session_id: &str, text: &str) -> Result<String, DaemonError>;

    /// Submit a remote-channel message to a bound session; returns the op id.
    async fn submit_channel_message(
        &self,
        channel_id: &str,
        target_key: &str,
        session_id: &str,
        text: &str,
    ) -> Result<String, DaemonError>;

    /// Resolve a pending tool approval.
    async fn resolve_approval(
        &self,
        session_id: &str,
        tool_call_id: &str,
        approved: bool,
    ) -> Result<(), DaemonError>;

    /// Fetch events newer than `after_seq` for a session.
    async fn events_after(
        &self,
        session_id: &str,
        after_seq: i64,
    ) -> Result<Vec<ProtocolEvent>, DaemonError>;

    /// Record a chat-to-session binding for SQL dual-write.
    /// The `target_key` is an opaque/redacted identifier (e.g. redacted chat id).
    async fn record_binding(
        &self,
        channel_id: &str,
        target_key: &str,
        session_id: &str,
    ) -> Result<(), DaemonError>;
}

/// An in-memory daemon stub for tests. Records submitted messages and resolved approvals,
/// and replays a scripted set of events.
pub struct StubDaemon {
    pub submitted: std::sync::Mutex<Vec<(String, String)>>,
    pub channel_submitted: std::sync::Mutex<Vec<(String, String, String, String)>>,
    pub approvals: std::sync::Mutex<Vec<(String, String, bool)>>,
    pub bindings: std::sync::Mutex<Vec<(String, String, String)>>,
    /// Optional error to return from `record_binding` instead of the default
    /// success. Used to exercise the Telegram dual-write failure path.
    pub binding_error: std::sync::Mutex<Option<DaemonError>>,
    events: Vec<ProtocolEvent>,
    known_sessions: Vec<String>,
}

impl StubDaemon {
    pub fn new() -> Self {
        Self {
            submitted: std::sync::Mutex::new(Vec::new()),
            channel_submitted: std::sync::Mutex::new(Vec::new()),
            approvals: std::sync::Mutex::new(Vec::new()),
            bindings: std::sync::Mutex::new(Vec::new()),
            binding_error: std::sync::Mutex::new(None),
            events: Vec::new(),
            known_sessions: vec!["s1".to_string()],
        }
    }

    pub fn with_events(mut self, events: Vec<ProtocolEvent>) -> Self {
        self.events = events;
        self
    }

    pub fn with_session(mut self, id: &str) -> Self {
        self.known_sessions.push(id.to_string());
        self
    }

    /// Force `record_binding` to return the given error (test-only).
    pub fn with_binding_error(self, error: DaemonError) -> Self {
        *self.binding_error.lock().unwrap() = Some(error);
        self
    }

    pub fn submitted_count(&self) -> usize {
        self.submitted.lock().unwrap().len()
    }

    pub fn last_approval(&self) -> Option<(String, String, bool)> {
        self.approvals.lock().unwrap().last().cloned()
    }
}

impl Default for StubDaemon {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DaemonClient for StubDaemon {
    async fn submit_message(&self, session_id: &str, text: &str) -> Result<String, DaemonError> {
        if !self.known_sessions.contains(&session_id.to_string()) {
            return Err(DaemonError::SessionNotFound(session_id.to_string()));
        }
        self.submitted
            .lock()
            .unwrap()
            .push((session_id.to_string(), text.to_string()));
        Ok(format!("op-{}", self.submitted_count()))
    }

    async fn submit_channel_message(
        &self,
        channel_id: &str,
        target_key: &str,
        session_id: &str,
        text: &str,
    ) -> Result<String, DaemonError> {
        self.channel_submitted.lock().unwrap().push((
            channel_id.to_string(),
            target_key.to_string(),
            session_id.to_string(),
            text.to_string(),
        ));
        self.submit_message(session_id, text).await
    }

    async fn resolve_approval(
        &self,
        session_id: &str,
        tool_call_id: &str,
        approved: bool,
    ) -> Result<(), DaemonError> {
        self.approvals.lock().unwrap().push((
            session_id.to_string(),
            tool_call_id.to_string(),
            approved,
        ));
        Ok(())
    }

    async fn events_after(
        &self,
        _session_id: &str,
        after_seq: i64,
    ) -> Result<Vec<ProtocolEvent>, DaemonError> {
        // Filter scripted events by their seq field.
        let filtered = self
            .events
            .iter()
            .filter(|e| event_seq(e) > after_seq)
            .cloned()
            .collect();
        Ok(filtered)
    }

    async fn record_binding(
        &self,
        channel_id: &str,
        target_key: &str,
        session_id: &str,
    ) -> Result<(), DaemonError> {
        if let Some(err) = self.binding_error.lock().unwrap().clone() {
            return Err(err);
        }
        self.bindings.lock().unwrap().push((
            channel_id.to_string(),
            target_key.to_string(),
            session_id.to_string(),
        ));
        Ok(())
    }
}

/// Extract the monotonic seq from any ProtocolEvent variant.
pub fn event_seq(e: &ProtocolEvent) -> i64 {
    use ProtocolEvent::*;
    match e {
        SessionStarted { seq, .. }
        | SessionTitleUpdated { seq, .. }
        | AssistantDelta { seq, .. }
        | AssistantMessageCompleted { seq, .. }
        | ToolCallStarted { seq, .. }
        | ToolApprovalRequested { seq, .. }
        | ToolApprovalResolved { seq, .. }
        | ToolResultAdded { seq, .. }
        | SessionTurnFinished { seq, .. }
        | StreamResyncRequired { seq, .. }
        | Overloaded { seq, .. }
        | PersistenceFailed { seq, .. }
        | ProviderRetrying { seq, .. }
        | SessionCompactionStarted { seq, .. }
        | VerificationFinished { seq, .. }
        | TaskGraphUpdated { seq, .. }
        | TaskStatusUpdated { seq, .. }
        | TaskArtifactUpdated { seq, .. }
        | ArtifactLineageUpdated { seq, .. }
        | RunInterrupted { seq, .. }
        | RunFailed { seq, .. }
        | SessionCompacted { seq, .. }
        | SessionContextFolded { seq, .. }
        | UsageUpdated { seq, .. }
        | BudgetExceeded { seq, .. }
        | ProviderRouteUpdated { seq, .. }
        | AccountRateLimited { seq, .. }
        | AccountUsageReported { seq, .. }
        | DispatchDecided { seq, .. }
        | OrchestrationObservationRecorded { seq, .. }
        | AgentQueryIssued { seq, .. }
        | AgentQueryResponded { seq, .. }
        | HumanSteeringInjected { seq, .. }
        | SubAgentSpawned { seq, .. }
        | SubAgentCompleted { seq, .. }
        | SubAgentCancelled { seq, .. }
        | UserMessageSubmitted { seq, .. }
        | UserQuestionAsked { seq, .. }
        | CredentialRequested { seq, .. }
        | CredentialResolved { seq, .. }
        | McpElicitationRequested { seq, .. }
        | UserQuestionAnswered { seq, .. }
        | PlanPresented { seq, .. }
        | AgentRosterUpdated { seq, .. }
        | CheckRunCompleted { seq, .. }
        | GoalProgressUpdated { seq, .. }
        | SelfReviewProposalReceived { seq, .. }
        | ModelRouteDecision { seq, .. }
        | ToolApprovalReviewed { seq, .. }
        | RunGovernanceRejected { seq, .. }
        | TurnUsage { seq, .. }
        | PromptCacheBreak { seq, .. }
        | ToolOutputDelta { seq, .. }
        | BackgroundShellExited { seq, .. } => *seq,
    }
}
