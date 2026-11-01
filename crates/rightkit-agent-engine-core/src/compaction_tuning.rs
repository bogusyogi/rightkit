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

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_msg(id: &str, summary: &str) -> TranscriptMessage {
        TranscriptMessage::Tool {
            id: id.into(),
            created_at: "t".into(),
            tool_call_id: "tc".into(),
            summary: summary.into(),
            output: None,
            trust: None,
            is_error: false,
            pinned: false,
            process_id: None,
        }
    }

    #[test]
    fn masks_old_tool_outputs_keeps_recent() {
        let mut t = Transcript::new();
        t.replace(vec![
            tool_msg("a", "old output AAAA"),
            tool_msg("b", "old output BBBB"),
            tool_msg("c", "recent output CCCC"),
        ]);
        let masker = ObservationMasker::new(1);
        let masked = masker.mask(&mut t);
        assert_eq!(masked, 2);
        let msgs = t.messages();
        if let TranscriptMessage::Tool { summary, .. } = &msgs[0] {
            assert!(summary.contains("masked"));
        } else {
            panic!("expected tool message");
        }
        if let TranscriptMessage::Tool { summary, .. } = &msgs[2] {
            assert_eq!(summary, "recent output CCCC");
        } else {
            panic!("expected tool message");
        }
    }

    #[test]
    fn failed_externalization_marker_survives_observation_masking() {
        let marker = "[Large tool output could not be archived; no retrieval reference was issued]\n--- head ---\nHEAD\n--- tail ---\nTAIL";
        let mut transcript = Transcript::new();
        let mut messages = vec![tool_msg("failed", marker)];
        for i in 0..8 {
            messages.push(tool_msg(&format!("filler-{i}"), "ordinary old output"));
        }
        messages.push(tool_msg("recent", "recent output"));
        transcript.replace(messages);

        let masked = ObservationMasker::new(1).mask(&mut transcript);

        assert_eq!(masked, 8, "only ordinary old outputs should be masked");
        match &transcript.messages()[0] {
            TranscriptMessage::Tool { summary, .. } => assert_eq!(summary, marker),
            other => panic!("expected failed storage marker, got {other:?}"),
        }
    }

    fn skill_assistant(id: &str) -> TranscriptMessage {
        TranscriptMessage::Assistant {
            id: id.into(),
            created_at: "t".into(),
            content: "".into(),
            reasoning_content: None,
            tool_calls: vec![rightkit_agent_engine_protocol::ToolCall {
                id: format!("tc-{id}"),
                name: "invoke_skill".into(),
                arguments_json: "{\"skill\":\"test\"}".into(),
            }],
            pinned: false,
        }
    }

    fn skill_tool(id: &str, summary: &str) -> TranscriptMessage {
        TranscriptMessage::Tool {
            id: id.into(),
            created_at: "t".into(),
            tool_call_id: format!("tc-{id}"),
            summary: summary.into(),
            output: None,
            trust: None,
            is_error: false,
            pinned: false,
            process_id: None,
        }
    }

    #[test]
    fn masking_leaves_four_trailing_skill_reads_verbatim() {
        // 6 skill-read blocks (assistant + tool each). With keep_recent=2, the
        // last block is recent and the 4 newest old blocks must survive verbatim;
        // only the oldest skill block's tool output should be masked.
        let mut t = Transcript::new();
        let mut msgs = Vec::new();
        for i in 0..6 {
            msgs.push(skill_assistant(&format!("a{i}")));
            msgs.push(skill_tool(&format!("t{i}"), &format!("skill output {i}")));
        }
        // also add a stale non-skill tool output that should be masked
        // insert at front to test mixed masking: add extra old tool before skill blocks
        // For this test we keep the 6 blocks as-is and verify counts.
        t.replace(msgs);
        let masker = ObservationMasker::new(2);
        let masked = masker.mask(&mut t);
        // invoke_skill is an exempt class (see `assistant_calls_exempt_tool`), so every skill
        // block stays verbatim and nothing is masked. The bounded skill-read budget applies only
        // to read_file instruction targets.
        assert_eq!(
            masked, 0,
            "exempt invoke_skill results must never be masked"
        );
        let out = t.messages();
        if let TranscriptMessage::Tool { summary, .. } = &out[1] {
            assert_eq!(summary, "skill output 0", "t0 must stay verbatim");
        } else {
            panic!("expected tool at index 1");
        }
        // t1..t5 verbatim
        for (idx, expected) in [
            (3, "skill output 1"),
            (5, "skill output 2"),
            (7, "skill output 3"),
            (9, "skill output 4"),
            (11, "skill output 5"),
        ] {
            if let TranscriptMessage::Tool { summary, .. } = &out[idx] {
                assert_eq!(
                    summary, expected,
                    "trailing skill read at {idx} must remain verbatim"
                );
            } else {
                panic!("expected tool at {idx}");
            }
        }
        // also verify exactly 4 trailing old skill reads preserved verbatim among old region:
        // indices 3,5,7,9 are the 4 trailing old blocks before the recent tail.
        let preserved_old = [3usize, 5, 7, 9];
        for i in preserved_old {
            if let TranscriptMessage::Tool { summary, .. } = &out[i] {
                assert!(
                    !summary.contains("masked"),
                    "preserved skill read at {i} must not be masked"
                );
            }
        }
    }

    #[test]
    fn non_skill_old_tools_are_masked_even_when_skill_reads_preserved() {
        let mut t = Transcript::new();
        // Old non-skill tool + 4 old skill blocks + 1 recent skill block
        let mut msgs = Vec::new();
        msgs.push(tool_msg(
            "old-generic",
            "generic old output that should be masked",
        ));
        for i in 0..5 {
            msgs.push(skill_assistant(&format!("a{i}")));
            msgs.push(skill_tool(&format!("t{i}"), &format!("skill output {i}")));
        }
        t.replace(msgs);
        let masker = ObservationMasker::new(2);
        let masked = masker.mask(&mut t);
        // old generic should be masked, oldest skill block among the 5 old blocks exceeds the 4-preserved limit, so oldest skill also masked? Let's trace:
        // total = 11, keep_from = 9, old = 0..8 inclusive. Old skill blocks fully before 9: blocks at (1,2),(3,4),(5,6),(7,8) -> 4 blocks. The generic at 0 is not a skill block, so it will be masked. None of the skill blocks before 9 exceed the 4 limit, so they all preserved.
        // So expect masked=1 (generic).
        assert_eq!(masked, 1);
        let out = t.messages();
        if let TranscriptMessage::Tool { summary, .. } = &out[0] {
            assert!(
                summary.contains("masked"),
                "generic old tool must be masked"
            );
        } else {
            panic!("expected tool at 0");
        }
        // skill tools at indices 2,4,6,8 must remain verbatim
        for idx in [2usize, 4, 6, 8] {
            if let TranscriptMessage::Tool { summary, .. } = &out[idx] {
                assert!(!summary.contains("masked"));
            } else {
                panic!("expected tool at {idx}");
            }
        }
    }
}

#[cfg(test)]
mod exempt_masking_tests {
    use super::*;
    use rightkit_agent_engine_protocol::TranscriptMessage;

    fn assistant_calling(name: &str) -> TranscriptMessage {
        TranscriptMessage::Assistant {
            id: format!("a-{name}"),
            created_at: "t".into(),
            content: String::new(),
            reasoning_content: None,
            tool_calls: vec![rightkit_agent_engine_protocol::ToolCall {
                id: format!("c-{name}"),
                name: name.into(),
                arguments_json: "{}".into(),
            }],
            pinned: false,
        }
    }

    fn tool_result(name: &str, body: &str) -> TranscriptMessage {
        TranscriptMessage::Tool {
            id: format!("t-{name}"),
            created_at: "t".into(),
            tool_call_id: format!("c-{name}"),
            summary: body.into(),
            output: None,
            trust: None,
            is_error: false,
            pinned: false,
            process_id: None,
        }
    }

    fn filler(id: &str) -> TranscriptMessage {
        TranscriptMessage::Assistant {
            id: id.into(),
            created_at: "t".into(),
            content: "filler".into(),
            reasoning_content: None,
            tool_calls: Vec::new(),
            pinned: false,
        }
    }

    /// A control-flow result must survive masking however old it is: without
    /// it the agent cannot see the decision it already made.
    #[test]
    fn an_exempt_tool_result_is_never_masked_however_old() {
        let mut transcript = Transcript::new();
        let mut messages = vec![
            assistant_calling("exit_plan_mode"),
            tool_result("exit_plan_mode", "plan accepted: do the thing"),
        ];
        for i in 0..20 {
            messages.push(filler(&format!("u{i}")));
        }
        transcript.replace(messages);

        ObservationMasker::new(4).mask(&mut transcript);

        let survived = transcript.messages().iter().any(|m| {
            matches!(m, TranscriptMessage::Tool { summary, .. }
                if summary.contains("plan accepted"))
        });
        assert!(survived, "an exit_plan_mode result was masked away");
    }

    /// The same guarantee for `ask_user`: its result is control flow — the
    /// user's answer — and must reach the model byte-for-byte however old it
    /// is and whatever compaction pressure exists.
    #[test]
    fn an_ask_user_result_is_never_masked_or_altered() {
        let answer = "USER ANSWER: ship option B, keep the old endpoint";
        let mut transcript = Transcript::new();
        let mut messages = vec![
            assistant_calling("ask_user"),
            tool_result("ask_user", answer),
        ];
        for i in 0..20 {
            messages.push(filler(&format!("u{i}")));
        }
        transcript.replace(messages);

        ObservationMasker::new(4).mask(&mut transcript);

        let intact = transcript
            .messages()
            .iter()
            .any(|m| matches!(m, TranscriptMessage::Tool { summary, .. } if summary == answer));
        assert!(
            intact,
            "an ask_user result was masked or altered; it must arrive verbatim"
        );
    }

    /// The exemption is by class, not a blanket amnesty: ordinary tool output
    /// of the same age is still masked, or the mechanism saves nothing.
    #[test]
    fn a_non_exempt_tool_result_of_the_same_age_is_still_masked() {
        let mut transcript = Transcript::new();
        let mut messages = vec![
            assistant_calling("search"),
            tool_result("search", "a very long search result body"),
        ];
        for i in 0..20 {
            messages.push(filler(&format!("u{i}")));
        }
        transcript.replace(messages);

        let masked = ObservationMasker::new(4).mask(&mut transcript);
        assert!(
            masked > 0,
            "an ordinary old tool result should still be masked"
        );

        let still_verbatim = transcript.messages().iter().any(|m| {
            matches!(m, TranscriptMessage::Tool { summary, .. }
                if summary.contains("a very long search result body"))
        });
        assert!(!still_verbatim, "a non-exempt result escaped masking");
    }
}
