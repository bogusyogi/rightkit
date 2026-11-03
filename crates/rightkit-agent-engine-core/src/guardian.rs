//! Optional per-tool guardian review layer (FM-031).
//!
//! Port lineage: san `internal/agent/permission.go` supplies fail-closed
//! review/request correlation; codex `core/src/guardian/` supplies lifecycle
//! shape. A second model reviews a proposed mutating tool call for
//! prompt-injection / adversarial intent before it executes.
//! Fail-closed by default: `SessionEngine.guardian` is `None` unless a caller
//! wires a reviewer via `set_guardian_reviewer`, and the check is skipped for
//! read-only tools. With no reviewer, each mutating call records an
//! `unavailable` verdict and proceeds to the normal human approval gate; the
//! reviewer can only block, never grant. Every verdict is emitted as a durable
//! `ToolApprovalReviewed` event carrying the call's correlation id.

use rightkit_agent_engine_protocol::ToolCall;

/// Lifecycle of one guardian decision. The correlation token is retained
/// across request, review, and terminal disposition so delayed reviewer
/// responses cannot authorize a different tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardianReviewState {
    Requested,
    Reviewing,
    Allowed,
    Blocked,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardianReviewLifecycle {
    pub correlation: String,
    pub state: GuardianReviewState,
}

impl GuardianReviewLifecycle {
    pub fn begin(request: &GuardianRequest) -> Self {
        Self {
            correlation: rightkit_agent_policy::denial_tracking::correlation_token(
                &request.session_id,
                &request.tool_call_id,
            ),
            state: GuardianReviewState::Requested,
        }
    }

    pub fn reviewing(&mut self) {
        self.state = GuardianReviewState::Reviewing;
    }
    pub fn allowed(&mut self) {
        self.state = GuardianReviewState::Allowed;
    }
    pub fn blocked(&mut self) {
        self.state = GuardianReviewState::Blocked;
    }
    pub fn failed(&mut self) {
        self.state = GuardianReviewState::Failed;
    }
}

/// Verdict returned by a `GuardianReviewer` for one proposed tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardianVerdict {
    /// The call may proceed to the normal permission/execution path.
    Allow,
    /// The call may proceed only when reviewer echoes exact request identity.
    AllowFor {
        session_id: String,
        active_run_id: String,
        tool_call_id: String,
    },
    /// The call must be denied. The string is a human-readable reason surfaced
    /// through the existing denial event path (same shape as a guardrail deny).
    Block(String),
}

/// What the guardian reviewer sees for one proposed mutating tool call.
#[derive(Debug, Clone)]
pub struct GuardianRequest {
    pub session_id: String,
    pub active_run_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub arguments_json: String,
    /// True when the recent transcript contains tool output flagged
    /// `ToolUntrusted` / `RemoteAgentUntrusted` — a signal (not a verdict) that
    /// untrusted external content is in play for this turn.
    pub recent_untrusted_context: bool,
}

impl GuardianRequest {
    pub fn from_call(session_id: &str, call: &ToolCall, recent_untrusted_context: bool) -> Self {
        Self::from_call_with_run(session_id, "", call, recent_untrusted_context)
    }

    pub fn from_call_with_run(
        session_id: &str,
        active_run_id: &str,
        call: &ToolCall,
        recent_untrusted_context: bool,
    ) -> Self {
        Self {
            session_id: session_id.to_string(),
            active_run_id: active_run_id.to_string(),
            tool_call_id: call.id.clone(),
            tool_name: call.name.clone(),
            arguments_json: call.arguments_json.clone(),
            recent_untrusted_context,
        }
    }

    pub fn allows(&self, verdict: &GuardianVerdict) -> bool {
        if self.session_id.trim().is_empty()
            || self.active_run_id.trim().is_empty()
            || self.tool_call_id.trim().is_empty()
        {
            return false;
        }
        matches!(
            verdict,
            GuardianVerdict::AllowFor {
                session_id,
                active_run_id,
                tool_call_id,
            } if session_id == &self.session_id
                && active_run_id == &self.active_run_id
                && tool_call_id == &self.tool_call_id
        )
    }

    pub fn correlated_allow(&self) -> GuardianVerdict {
        GuardianVerdict::AllowFor {
            session_id: self.session_id.clone(),
            active_run_id: self.active_run_id.clone(),
            tool_call_id: self.tool_call_id.clone(),
        }
    }
}

/// A second, BYO-config model that judges a proposed tool call before it
/// executes. Implementors are free to call any provider; the engine only
/// requires an async verdict. Reviewer errors and uncorrelated legacy `Allow`
/// verdicts fail closed; the guardian remains optional and off by default.
#[async_trait::async_trait]
pub trait GuardianReviewer: Send + Sync {
    async fn review(&self, request: &GuardianRequest) -> Result<GuardianVerdict, String>;
}
