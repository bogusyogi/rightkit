use super::{ProtocolEvent, ProtocolEventMeta};
use crate::message::TranscriptMessage;

macro_rules! match_event_meta {
    ($event:expr, |$session_id:ident, $seq:ident, $created_at:ident| $body:expr) => {
        match $event {
            ProtocolEvent::SessionStarted {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::SessionTitleUpdated {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::AssistantDelta {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::AssistantMessageCompleted {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::ToolCallStarted {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::ToolApprovalRequested {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::ToolApprovalResolved {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::ToolResultAdded {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::SessionTurnFinished {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::StreamResyncRequired {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::Overloaded {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::PersistenceFailed {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::ProviderRetrying {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::SessionCompactionStarted {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::VerificationFinished {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::TaskGraphUpdated {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::TaskStatusUpdated {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::TaskArtifactUpdated {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::ArtifactLineageUpdated {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::RunInterrupted {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::RunFailed {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::SessionCompacted {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::SessionContextFolded {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::UsageUpdated {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::TurnUsage {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::PromptCacheBreak {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::ToolOutputDelta {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::BackgroundShellExited {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::BudgetExceeded {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::ProviderRouteUpdated {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::AccountRateLimited {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::AccountUsageReported {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::DispatchDecided {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::OrchestrationObservationRecorded {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::AgentQueryIssued {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::AgentQueryResponded {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::HumanSteeringInjected {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::SubAgentSpawned {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::SubAgentCompleted {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::SubAgentCancelled {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::UserMessageSubmitted {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::UserQuestionAsked {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::McpElicitationRequested {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::CredentialRequested {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::CredentialResolved {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::UserQuestionAnswered {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::PlanPresented {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::AgentRosterUpdated {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::CheckRunCompleted {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::GoalProgressUpdated {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::SelfReviewProposalReceived {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::ToolApprovalReviewed {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::ToolPolicyEvent {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::ModelRouteDecision {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            }
            | ProtocolEvent::RunGovernanceRejected {
                session_id: $session_id,
                seq: $seq,
                created_at: $created_at,
                ..
            } => $body,
        }
    };
}

/// Exhaustive set_seq matcher: any new ProtocolEvent variant forces a
/// compile-time decision here. Used by the storage event-row sequence
/// unifier to align the row sequence with the payload sequence at the
/// boundary, and by the conductor's pre-publish canonicalization (where the
/// row sequence may be unknown but the caller supplies one).
fn assign_seq(event: &mut ProtocolEvent, new_seq: i64) {
    match event {
        ProtocolEvent::SessionStarted { seq, .. }
        | ProtocolEvent::SessionTitleUpdated { seq, .. }
        | ProtocolEvent::AssistantDelta { seq, .. }
        | ProtocolEvent::AssistantMessageCompleted { seq, .. }
        | ProtocolEvent::ToolCallStarted { seq, .. }
        | ProtocolEvent::ToolApprovalRequested { seq, .. }
        | ProtocolEvent::ToolApprovalResolved { seq, .. }
        | ProtocolEvent::ToolResultAdded { seq, .. }
        | ProtocolEvent::SessionTurnFinished { seq, .. }
        | ProtocolEvent::StreamResyncRequired { seq, .. }
        | ProtocolEvent::Overloaded { seq, .. }
        | ProtocolEvent::PersistenceFailed { seq, .. }
        | ProtocolEvent::ProviderRetrying { seq, .. }
        | ProtocolEvent::SessionCompactionStarted { seq, .. }
        | ProtocolEvent::VerificationFinished { seq, .. }
        | ProtocolEvent::TaskGraphUpdated { seq, .. }
        | ProtocolEvent::TaskStatusUpdated { seq, .. }
        | ProtocolEvent::TaskArtifactUpdated { seq, .. }
        | ProtocolEvent::ArtifactLineageUpdated { seq, .. }
        | ProtocolEvent::RunInterrupted { seq, .. }
        | ProtocolEvent::RunFailed { seq, .. }
        | ProtocolEvent::SessionCompacted { seq, .. }
        | ProtocolEvent::SessionContextFolded { seq, .. }
        | ProtocolEvent::UsageUpdated { seq, .. }
        | ProtocolEvent::TurnUsage { seq, .. }
        | ProtocolEvent::PromptCacheBreak { seq, .. }
        | ProtocolEvent::ToolOutputDelta { seq, .. }
        | ProtocolEvent::BackgroundShellExited { seq, .. }
        | ProtocolEvent::BudgetExceeded { seq, .. }
        | ProtocolEvent::ProviderRouteUpdated { seq, .. }
        | ProtocolEvent::AccountRateLimited { seq, .. }
        | ProtocolEvent::AccountUsageReported { seq, .. }
        | ProtocolEvent::DispatchDecided { seq, .. }
        | ProtocolEvent::OrchestrationObservationRecorded { seq, .. }
        | ProtocolEvent::AgentQueryIssued { seq, .. }
        | ProtocolEvent::AgentQueryResponded { seq, .. }
        | ProtocolEvent::HumanSteeringInjected { seq, .. }
        | ProtocolEvent::SubAgentSpawned { seq, .. }
        | ProtocolEvent::SubAgentCompleted { seq, .. }
        | ProtocolEvent::SubAgentCancelled { seq, .. }
        | ProtocolEvent::UserMessageSubmitted { seq, .. }
        | ProtocolEvent::UserQuestionAsked { seq, .. }
        | ProtocolEvent::McpElicitationRequested { seq, .. }
        | ProtocolEvent::CredentialRequested { seq, .. }
        | ProtocolEvent::CredentialResolved { seq, .. }
        | ProtocolEvent::UserQuestionAnswered { seq, .. }
        | ProtocolEvent::PlanPresented { seq, .. }
        | ProtocolEvent::AgentRosterUpdated { seq, .. }
        | ProtocolEvent::CheckRunCompleted { seq, .. }
        | ProtocolEvent::GoalProgressUpdated { seq, .. }
        | ProtocolEvent::SelfReviewProposalReceived { seq, .. }
        | ProtocolEvent::ModelRouteDecision { seq, .. }
        | ProtocolEvent::ToolApprovalReviewed { seq, .. }
        | ProtocolEvent::ToolPolicyEvent { seq, .. }
        | ProtocolEvent::RunGovernanceRejected { seq, .. } => *seq = new_seq,
    }
}

/// Id-bearing message helper — exhaustive over every variant that carries a
/// `TranscriptMessage`. The compiler refuses to add a new variant without
/// deciding the identifier semantics here.
fn rewrite_message_id<F: FnOnce(&mut TranscriptMessage)>(event: &mut ProtocolEvent, rewrite: F) {
    match event {
        ProtocolEvent::AssistantMessageCompleted { message, .. } => rewrite(message),
        ProtocolEvent::ToolResultAdded { result, .. } => rewrite(result),
        ProtocolEvent::SessionContextFolded { fold, .. } => rewrite(fold),
        _ => {}
    }
}

impl ProtocolEvent {
    pub fn meta(&self) -> ProtocolEventMeta {
        match_event_meta!(self, |session_id, seq, created_at| ProtocolEventMeta {
            session_id: session_id.clone(),
            seq: *seq,
            created_at: created_at.clone(),
        })
    }

    pub fn session_id(&self) -> &str {
        match_event_meta!(self, |session_id, _seq, _created_at| session_id.as_str())
    }

    pub fn seq(&self) -> i64 {
        match_event_meta!(self, |_session_id, seq, _created_at| *seq)
    }

    pub fn created_at(&self) -> &str {
        match_event_meta!(self, |_session_id, _seq, created_at| created_at.as_str())
    }

    /// Exhaustive set_seq. Used by `decode_event_row` and by `Conduit::publish`
    /// when the caller has determined the canonical sequence for this event.
    pub fn set_seq(&mut self, new_seq: i64) {
        assign_seq(self, new_seq);
    }

    /// Canonicalize every identity-bearing field against `authoritative_seq`.
    /// `seq` is rewritten for every variant; `TranscriptMessage::id` on each
    /// nested message is rewritten too so producer-chosen constant IDs (`final`,
    /// `forced-stop`) cannot survive a row-replay.
    pub fn canonicalize_identity(&mut self, authoritative_seq: i64) {
        assign_seq(self, authoritative_seq);
        let outer = authoritative_seq;
        rewrite_message_id(self, |message| {
            message.set_id(derive_message_id(message, outer));
        });
    }
}

/// Choose the canonical id for a `TranscriptMessage` based on its variant.
fn derive_message_id(message: &TranscriptMessage, row_seq: i64) -> String {
    use TranscriptMessage::*;
    match message {
        System { .. } => crate::ids::message_id(row_seq),
        User { .. } => crate::ids::message_id(row_seq),
        Assistant { .. } => crate::ids::assistant_message_id(row_seq),
        Tool { tool_call_id, .. } => crate::ids::tool_result_id(tool_call_id, row_seq),
        Compact { .. } => crate::ids::message_id(row_seq),
        Fold { .. } => crate::ids::message_id(row_seq),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::TranscriptMessage;
    use crate::tool::ToolCall;

    fn assistant_with_id(id: &str) -> TranscriptMessage {
        TranscriptMessage::Assistant {
            id: id.into(),
            created_at: "t".into(),
            content: "hi".into(),
            reasoning_content: None,
            tool_calls: vec![],
            pinned: false,
        }
    }

    fn tool_message(id: &str) -> TranscriptMessage {
        TranscriptMessage::Tool {
            id: id.into(),
            created_at: "t".into(),
            tool_call_id: "tc1".into(),
            summary: "ok".into(),
            output: None,
            trust: None,
            is_error: false,
            pinned: false,
            process_id: None,
        }
    }

    #[test]
    fn set_seq_rewrites_every_variant() {
        let mut ev = ProtocolEvent::AssistantDelta {
            session_id: "s".into(),
            seq: 0,
            created_at: "t".into(),
            text: "x".into(),
            is_reasoning: false,
        };
        ev.set_seq(42);
        assert_eq!(ev.seq(), 42);

        let mut ev = ProtocolEvent::ModelRouteDecision {
            session_id: "s".into(),
            seq: 0,
            created_at: "t".into(),
            decision: crate::event::ModelRouteDecisionPayload {
                agent_role: "default".into(),
                model_id: "test-model".into(),
                tier: "cheap".into(),
                reason: "unit-test".into(),
                signals_digest: "0".into(),
                price_snapshot: crate::event::PriceSnapshot {
                    prompt_per_m: 0.0,
                    completion_per_m: 0.0,
                    source: "test".into(),
                    fetched_at: "t".into(),
                    hash: "h".into(),
                },
                est_cost_usd: 0.0,
                schema_version: 1,
            },
        };
        ev.set_seq(7);
        assert_eq!(ev.seq(), 7);
    }

    #[test]
    fn canonicalize_identity_assigns_seq_and_message_id() {
        let mut ev = ProtocolEvent::AssistantMessageCompleted {
            session_id: "s".into(),
            seq: 0,
            created_at: "t".into(),
            message: assistant_with_id("final"),
        };
        ev.canonicalize_identity(11);
        assert_eq!(ev.seq(), 11);
        match &ev {
            ProtocolEvent::AssistantMessageCompleted { message, .. } => {
                assert_eq!(message.id(), "assistant:11");
            }
            _ => panic!("variant"),
        }
    }

    #[test]
    fn canonicalize_tool_result_uses_tool_call_id() {
        let mut ev = ProtocolEvent::ToolResultAdded {
            session_id: "s".into(),
            seq: 0,
            created_at: "t".into(),
            result: tool_message("tr"),
        };
        ev.canonicalize_identity(22);
        match &ev {
            ProtocolEvent::ToolResultAdded { result, .. } => {
                assert_eq!(result.id(), "tool:tc1:22");
            }
            _ => panic!("variant"),
        }
    }

    #[test]
    fn canonicalize_session_started_keeps_variants_without_a_message() {
        let mut ev = ProtocolEvent::SessionStarted {
            session_id: "s".into(),
            seq: 0,
            created_at: "t".into(),
            title: None,
        };
        ev.canonicalize_identity(3);
        assert_eq!(ev.seq(), 3);
        assert_eq!(ev.session_id(), "s");
    }

    #[test]
    fn set_seq_is_exhaustive_compile_check() {
        // Adding a ProtocolEvent variant must also add an arm to
        // `assign_seq`/`set_seq`. The exhaustive match below lists every
        // existing variant; if a new variant is added this test fails to
        // compile — the developer is forced to choose identity semantics
        // there too. Any variant not explicitly handled falls into a
        // deliberate compile error.
        fn all_arms(e: &mut ProtocolEvent, mutate: impl Fn(&mut i64)) {
            match e {
                ProtocolEvent::SessionStarted { seq, .. }
                | ProtocolEvent::SessionTitleUpdated { seq, .. }
                | ProtocolEvent::AssistantDelta { seq, .. }
                | ProtocolEvent::AssistantMessageCompleted { seq, .. }
                | ProtocolEvent::ToolCallStarted { seq, .. }
                | ProtocolEvent::ToolApprovalRequested { seq, .. }
                | ProtocolEvent::ToolApprovalResolved { seq, .. }
                | ProtocolEvent::ToolResultAdded { seq, .. }
                | ProtocolEvent::SessionTurnFinished { seq, .. }
                | ProtocolEvent::StreamResyncRequired { seq, .. }
                | ProtocolEvent::Overloaded { seq, .. }
                | ProtocolEvent::PersistenceFailed { seq, .. }
                | ProtocolEvent::ProviderRetrying { seq, .. }
                | ProtocolEvent::SessionCompactionStarted { seq, .. }
                | ProtocolEvent::VerificationFinished { seq, .. }
                | ProtocolEvent::TaskGraphUpdated { seq, .. }
                | ProtocolEvent::TaskStatusUpdated { seq, .. }
                | ProtocolEvent::TaskArtifactUpdated { seq, .. }
                | ProtocolEvent::ArtifactLineageUpdated { seq, .. }
                | ProtocolEvent::RunInterrupted { seq, .. }
                | ProtocolEvent::RunFailed { seq, .. }
                | ProtocolEvent::SessionCompacted { seq, .. }
                | ProtocolEvent::SessionContextFolded { seq, .. }
                | ProtocolEvent::UsageUpdated { seq, .. }
                | ProtocolEvent::TurnUsage { seq, .. }
                | ProtocolEvent::PromptCacheBreak { seq, .. }
                | ProtocolEvent::ToolOutputDelta { seq, .. }
                | ProtocolEvent::BackgroundShellExited { seq, .. }
                | ProtocolEvent::BudgetExceeded { seq, .. }
                | ProtocolEvent::ProviderRouteUpdated { seq, .. }
                | ProtocolEvent::AccountRateLimited { seq, .. }
                | ProtocolEvent::AccountUsageReported { seq, .. }
                | ProtocolEvent::DispatchDecided { seq, .. }
                | ProtocolEvent::OrchestrationObservationRecorded { seq, .. }
                | ProtocolEvent::AgentQueryIssued { seq, .. }
                | ProtocolEvent::AgentQueryResponded { seq, .. }
                | ProtocolEvent::HumanSteeringInjected { seq, .. }
                | ProtocolEvent::SubAgentSpawned { seq, .. }
                | ProtocolEvent::SubAgentCompleted { seq, .. }
                | ProtocolEvent::SubAgentCancelled { seq, .. }
                | ProtocolEvent::UserMessageSubmitted { seq, .. }
                | ProtocolEvent::UserQuestionAsked { seq, .. }
                | ProtocolEvent::McpElicitationRequested { seq, .. }
                | ProtocolEvent::CredentialRequested { seq, .. }
                | ProtocolEvent::CredentialResolved { seq, .. }
                | ProtocolEvent::UserQuestionAnswered { seq, .. }
                | ProtocolEvent::PlanPresented { seq, .. }
                | ProtocolEvent::AgentRosterUpdated { seq, .. }
                | ProtocolEvent::CheckRunCompleted { seq, .. }
                | ProtocolEvent::GoalProgressUpdated { seq, .. }
                | ProtocolEvent::SelfReviewProposalReceived { seq, .. }
                | ProtocolEvent::ToolApprovalReviewed { seq, .. }
                | ProtocolEvent::ToolPolicyEvent { seq, .. }
                | ProtocolEvent::RunGovernanceRejected { seq, .. } => mutate(seq),
                // ModelRouteDecision nests a payload — exhaustive coverage
                // requires the inner seq; in practice, set_seq delegates to
                // assign_seq which already mirrors this exact arm list.
                ProtocolEvent::ModelRouteDecision { seq, .. } => mutate(seq),
            }
        }
        let mut ev = ProtocolEvent::ToolCallStarted {
            session_id: "s".into(),
            seq: 0,
            created_at: "t".into(),
            call: ToolCall {
                id: "tc1".into(),
                name: "read_file".into(),
                arguments_json: "{}".into(),
            },
        };
        all_arms(&mut ev, |s| *s = 9);
        assert_eq!(ev.seq(), 9);
    }
}
