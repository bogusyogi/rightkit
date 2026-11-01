// Mechanisms 3 + 5 (book §P2-3, 3-3 and 3-5): cheap-first escalation and
// tag-based retention.
//
// Sibling include! module of compactor.rs -- shares its module scope, no
// mod/use headers of its own. `repos/cli/swe-agent/.../history_processors.py`
// (the book's cited prior art for mechanism 5) does not exist in this
// workspace, so this is implemented from the book's prose description, not
// read from source.

/// Non-model pruning strategy tried before summarization. Implementors
/// operate on a transcript snapshot and return the pruned messages; they
/// MUST NOT call a model -- that is what makes this "cheap".
pub trait CheapPruner: Send + Sync {
    fn prune(&self, messages: &[TranscriptMessage]) -> Vec<TranscriptMessage>;
}

/// Estimate transcript size through the SAME meter mechanism 3 requires be
/// used both before and after pruning, so the escalation decision is an
/// apples-to-apples remeasurement rather than two different estimators.
/// Reuses TokenBudgetSummarizer's deterministic chars/4 estimator over each
/// message's formatted line (mechanism 4's existing meter).
pub fn estimate_transcript_tokens(messages: &[TranscriptMessage]) -> u32 {
    messages
        .iter()
        .map(|m| TokenBudgetSummarizer::estimate_tokens(&format_message_for_summarizer(m)))
        .fold(0u32, |acc, tokens| acc.saturating_add(tokens))
}

/// Outcome of a cheap-first escalation decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EscalationOutcome {
    /// Pruning alone relieved pressure below the trigger; no model call was
    /// made.
    PrunedNoEscalation {
        tokens_before: u32,
        tokens_after: u32,
    },
    /// Pruning did not relieve pressure; the caller must summarize.
    EscalateToSummarizer { tokens_after: u32 },
}

impl Compactor {
    /// Mechanism 3 (cheap-first escalation). Runs pruner first, remeasures
    /// through estimate_transcript_tokens (the same meter used before
    /// pruning), and only signals escalation to a summarization call if
    /// pruning did not bring the transcript back under trigger_tokens.
    pub fn cheap_first_escalate(
        &self,
        messages: &[TranscriptMessage],
        pruner: &dyn CheapPruner,
        trigger_tokens: u32,
    ) -> (Vec<TranscriptMessage>, EscalationOutcome) {
        let tokens_before = estimate_transcript_tokens(messages);
        let pruned = pruner.prune(messages);
        let tokens_after = estimate_transcript_tokens(&pruned);
        let outcome = if tokens_after <= trigger_tokens {
            EscalationOutcome::PrunedNoEscalation {
                tokens_before,
                tokens_after,
            }
        } else {
            EscalationOutcome::EscalateToSummarizer { tokens_after }
        };
        (pruned, outcome)
    }
}

/// Mechanism 5 (tag-based retention). TranscriptMessage carries no first-
/// class tag field (that would be a rightkit_agent_engine_protocol change, outside
/// this lane's owned files), so retention here is keyed by message id: any
/// id present in always_keep_ids is protected from cheap pruning / elision
/// regardless of age, mirroring swe-agent's always_keep_output_for_tags.
/// Callers populate always_keep_ids from whatever tags they assign
/// (task-spec turn, the latest user-intent turn, open-obligation markers).
///
/// A message already marked pinned is always-keep independent of this set
/// -- this function is strictly additive to the existing pinned invariant.
pub fn is_always_keep(
    message: &TranscriptMessage,
    always_keep_ids: &std::collections::HashSet<String>,
) -> bool {
    if is_pinned(message) {
        return true;
    }
    let (id, _len) = message_identity(message);
    always_keep_ids.contains(id)
}

/// Reference CheapPruner for mechanism 5: drops non-protected Tool
/// messages (the "ls/grep chatter" category) while never eliding anything
/// whose id is in always_keep_ids or that is already pinned -- the task
/// spec, user intent, and open obligations this lane must never compact away.
pub struct TagAwarePruner {
    pub always_keep_ids: std::collections::HashSet<String>,
}

impl CheapPruner for TagAwarePruner {
    fn prune(&self, messages: &[TranscriptMessage]) -> Vec<TranscriptMessage> {
        messages
            .iter()
            .filter(|message| {
                is_always_keep(message, &self.always_keep_ids)
                    || !matches!(message, TranscriptMessage::Tool { .. })
            })
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod cheap_prune_tests {
    use super::*;

    fn tool_message(id: &str, summary: &str) -> TranscriptMessage {
        TranscriptMessage::Tool {
            id: id.to_string(),
            created_at: "t".into(),
            tool_call_id: format!("call-{id}"),
            summary: summary.to_string(),
            output: None,
            trust: None,
            is_error: false,
            pinned: false,
            process_id: None,
        }
    }

    #[test]
    fn tagged_obligations_are_never_elided_by_the_cheap_pruner() {
        let mut transcript = Transcript::new();
        transcript.push_user("task spec: do the thing");
        transcript.replace({
            let mut messages = transcript.messages().to_vec();
            messages.push(tool_message(
                "obligation-tool-1",
                "OPEN OBLIGATION: must finish X",
            ));
            messages.push(tool_message("chatter-tool-2", "ls output: a.txt b.txt"));
            messages
        });

        let always_keep: std::collections::HashSet<String> =
            ["obligation-tool-1".to_string()].into_iter().collect();
        let pruner = TagAwarePruner {
            always_keep_ids: always_keep,
        };
        let compactor = Compactor::new(usize::MAX, 0);
        let (pruned, _outcome) = compactor.cheap_first_escalate(transcript.messages(), &pruner, 0);

        assert!(
            pruned.iter().any(
                |m| matches!(m, TranscriptMessage::Tool { id, .. } if id == "obligation-tool-1")
            ),
            "tagged obligation must survive pruning"
        );
        assert!(
            !pruned
                .iter()
                .any(|m| matches!(m, TranscriptMessage::Tool { id, .. } if id == "chatter-tool-2")),
            "untagged tool chatter is eligible for elision"
        );
    }

    #[test]
    fn cheap_first_escalation_skips_the_summarizer_when_pruning_relieves_pressure() {
        let mut transcript = Transcript::new();
        let messages: Vec<TranscriptMessage> = (0..5)
            .map(|i| tool_message(&format!("chatter-{i}"), "ls output: a.txt b.txt"))
            .collect();
        transcript.replace(messages);

        let compactor = Compactor::new(usize::MAX, 0);
        let pruner = TagAwarePruner {
            always_keep_ids: std::collections::HashSet::new(),
        };
        let (pruned, outcome) =
            compactor.cheap_first_escalate(transcript.messages(), &pruner, 1_000_000);
        assert!(pruned.is_empty());
        assert!(matches!(
            outcome,
            EscalationOutcome::PrunedNoEscalation { .. }
        ));
    }
}
