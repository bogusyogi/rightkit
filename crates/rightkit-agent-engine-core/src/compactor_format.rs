// Constants, formatters, and ISO-timestamp helpers for the compactor.
//
// Sibling include! module: keeps the orchestrator file slim by hosting the
// leaf utilities and constants used across all sibling files.

/// Maximum number of assistant-initiated skill-read blocks preserved verbatim
/// across a compaction boundary. The newest N reads are pinned.
pub(crate) const MAX_PRESERVED_SKILL_READ_BLOCKS: usize = 4;

/// Identifier of the post-compaction continuation system message. Detected by
/// `pinned_kept.iter().any(|m| matches!(...))` to avoid duplicate injections
/// across successive compactions.
pub(crate) const POST_COMPACTION_CONTINUATION_ID: &str = "post-compact-continuation";

/// Strings joined with `\n` to form the continuation system message body.
pub(crate) const POST_COMPACTION_CONTINUATION_STRINGS: &[&str] = &[
    "The transcript above was compacted automatically to fit the context window.",
    "Continue exactly where the summary leaves off; do not ask the user to repeat context.",
];

/// Formats a transcript message for LLM consumption (or statistical digest).
pub(crate) fn format_message_for_summarizer(msg: &TranscriptMessage) -> String {
    match msg {
        TranscriptMessage::System { content, .. } => format!("[System] {}", content),
        TranscriptMessage::User { content, .. } => format!("[User] {}", content),
        TranscriptMessage::Assistant { content, .. } => format!("[Assistant] {}", content),
        TranscriptMessage::Tool {
            summary, is_error, ..
        } => {
            let error_flag = if *is_error { " (error)" } else { "" };
            format!("[Tool]{} {}", error_flag, summary)
        }
        TranscriptMessage::Compact { summary, .. } => format!("[Compact] {}", summary),
        TranscriptMessage::Fold {
            summary,
            subtask_id,
            ..
        } => format!("[Fold:subtask={}] {}", subtask_id, summary),
    }
}

/// Wall-clock ISO 8601 timestamp.
pub(crate) fn now_iso() -> String {
    rightkit_agent_engine_protocol::now_iso()
}
