//! Observation masking for compaction.
//!
//! [`ObservationMasker`] replaces stale tool outputs with short placeholders
//! while keeping recent ones verbatim — a cheap first layer before LLM
//! summarization.

use crate::transcript::Transcript;
use rightkit_agent_engine_protocol::{ToolCall, TranscriptMessage};
use std::collections::HashSet;

/// Maximum number of assistant-initiated skill-read blocks preserved verbatim
/// across a masking boundary. Mirrors `MAX_PRESERVED_SKILL_READ_BLOCKS` in
/// `compactor_format.rs`; the newest N reads are pinned.
const MAX_PRESERVED_SKILL_READ_BLOCKS: usize = 4;

/// Replaces stale tool outputs with placeholders while keeping recent ones verbatim.
#[derive(Debug, Clone)]
pub struct ObservationMasker {
    pub keep_recent: usize,
}

impl ObservationMasker {
    pub fn new(keep_recent: usize) -> Self {
        Self { keep_recent }
    }

    /// Mask non-pinned tool-result messages older than `keep_recent`. Returns the
    /// number of messages masked. The last `MAX_PRESERVED_SKILL_READ_BLOCKS`
    /// skill-read blocks (assistant `invoke_skill`/`read_file` skill.md + trailing
    /// Tool results) are preserved verbatim even when they fall before
    /// `keep_from`.
    pub fn mask(&self, transcript: &mut Transcript) -> usize {
        let messages = transcript.messages().to_vec();
        let total = messages.len();
        let keep_from = total.saturating_sub(self.keep_recent);
        let preserved = preserved_skill_read_indices(&messages, keep_from);

        let mut masked = 0;
        let mut rebuilt = Vec::with_capacity(total);
        for (i, msg) in messages.into_iter().enumerate() {
            match msg {
                TranscriptMessage::Tool {
                    id,
                    created_at,
                    tool_call_id,
                    pinned,
                    summary,
                    is_error,
                    output,
                    trust,
                    process_id: _,
                } if i < keep_from
                    && !pinned
                    && !preserved.contains(&i)
                    // A failed externalization is already bounded, but its
                    // marker is the only model-visible indication that the
                    // full result is unrecoverable. Preserve that typed
                    // failure diagnostic instead of replacing it with the
                    // generic successful-looking mask.
                    && !crate::tool_result_store::is_unrecoverable_preview(&summary) =>
                {
                    let masked_summary = format!("[tool output masked — {} bytes]", summary.len());
                    rebuilt.push(TranscriptMessage::Tool {
                        id,
                        created_at,
                        tool_call_id,
                        summary: masked_summary,
                        output,
                        trust,
                        is_error,
                        pinned: false,
                        process_id: None,
                    });
                    masked += 1;
                }
                other => rebuilt.push(other),
            }
        }
        if masked > 0 {
            transcript.replace(rebuilt);
        }
        masked
    }
}

fn preserved_skill_read_indices(
    messages: &[TranscriptMessage],
    keep_from: usize,
) -> HashSet<usize> {
    let mut blocks = Vec::new();
    let mut always: HashSet<usize> = HashSet::new();
    let mut i = 0usize;
    while i < messages.len() {
        let TranscriptMessage::Assistant { tool_calls, .. } = &messages[i] else {
            i += 1;
            continue;
        };
        let mut block_end = i + 1;
        while block_end < messages.len()
            && matches!(messages[block_end], TranscriptMessage::Tool { .. })
        {
            block_end += 1;
        }
        if i < keep_from && assistant_calls_exempt_tool(tool_calls) {
            // Exempt by class, so preserved outright rather than competing for
            // the bounded skill-read budget below — an old `exit_plan_mode`
            // result matters exactly as much as a recent one.
            always.extend(i..block_end);
        } else if i < keep_from && assistant_reads_skill_or_instruction(tool_calls) {
            blocks.push((i..block_end).collect::<Vec<_>>());
        }
        i = block_end;
    }
    always.extend(
        blocks
            .into_iter()
            .rev()
            .take(MAX_PRESERVED_SKILL_READ_BLOCKS)
            .flatten(),
    );
    always
}

/// Tool classes whose results must never be masked, whatever their age.
///
/// These are short, structural, control-flow calls whose whole value is being
/// visible: masking the result of `ask_user`, `exit_plan_mode`, or
/// `todo_write` leaves the agent unable to see the decision it just made or
/// the plan it just committed to, which is a correctness failure rather than a
/// context saving. They are cheap to keep — being brief is what makes them
/// structural.
///
/// CR-28/CR-32: this is a consumer of the canonical list in
/// `rightkit_agent_tools::coding_tools::output_compaction`, not a second definition — one
/// exempt class, one source of truth.
fn assistant_calls_exempt_tool(tool_calls: &[ToolCall]) -> bool {
    tool_calls
        .iter()
        .any(|call| crate::compaction_archive::is_exempt_tool_name(&call.name))
}

fn assistant_reads_skill_or_instruction(tool_calls: &[ToolCall]) -> bool {
    tool_calls
        .iter()
        .any(|call| call.name == "invoke_skill" || read_file_targets_instruction(call))
}

fn read_file_targets_instruction(call: &ToolCall) -> bool {
    if call.name != "read_file" {
        return false;
    }
    let Ok(args) = serde_json::from_str::<serde_json::Value>(&call.arguments_json) else {
        return false;
    };
    let Some(path) = args.get("path").and_then(|p| p.as_str()) else {
        return false;
    };
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    let file_name = normalized.rsplit('/').next().unwrap_or(normalized.as_str());
    matches!(
        file_name,
        "skill.md" | "agents.md" | "claude.md" | "instructions.md"
    ) || normalized.contains("/skills/")
        || normalized.contains("/.agents/skills/")
        || normalized.contains("/.codex/skills/")
}
