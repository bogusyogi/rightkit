// Preservation helpers used by `Compactor::compact()` to decide which
// messages survive a compaction boundary verbatim.
//
// Sibling include! module: reduces the parent `compactor.rs` from ~393
// lines to under 400 by moving the partition predicate helpers here.

use std::collections::HashSet;

/// `true` for messages marked `pinned` -- always preserved verbatim.
pub(crate) fn is_pinned(m: &TranscriptMessage) -> bool {
    match m {
        TranscriptMessage::System { pinned, .. }
        | TranscriptMessage::User { pinned, .. }
        | TranscriptMessage::Assistant { pinned, .. }
        | TranscriptMessage::Tool { pinned, .. }
        | TranscriptMessage::Compact { pinned, .. }
        | TranscriptMessage::Fold { pinned, .. } => *pinned,
    }
}

/// Returns all prior summaries in order so the next model-authored summary can
/// merge legacy multi-summary transcripts without losing information. New live
/// compactions retain exactly one Compact message, so this is normally one item.
pub(crate) fn extract_prior_summary(transcript: &Transcript) -> Option<String> {
    let summaries = transcript
        .messages()
        .iter()
        .filter_map(|message| match message {
            TranscriptMessage::Compact { summary, .. } => Some(summary.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    (!summaries.is_empty()).then(|| summaries.join("\n\n"))
}

/// Return the original tagged obligation messages before any observation
/// masking or pruning can replace/elide their content.  The full message is
/// retained because the structured id/content record alone is not enough to
/// independently inject the obligation back into a rebuilt context.
pub(crate) fn collect_open_obligation_messages(
    messages: &[TranscriptMessage],
) -> Vec<TranscriptMessage> {
    messages
        .iter()
        .filter(|message| is_open_obligation_message(message))
        .cloned()
        .collect()
}

/// Preserve every tagged obligation as a pinned, independently injectable
/// message.  An obligation may also be present in the summarizer input; this
/// copy is the canonical safety net that prevents masking/pruning/rebuild from
/// making the obligation prose depend on model output.
pub(crate) fn protect_open_obligation_messages(
    mut kept: Vec<TranscriptMessage>,
    obligations: &[TranscriptMessage],
) -> Vec<TranscriptMessage> {
    for obligation in obligations {
        let TranscriptMessage::Tool { id, .. } = obligation else {
            continue;
        };

        if let Some(index) = kept.iter().position(|message| {
            matches!(message, TranscriptMessage::Tool { id: existing_id, .. } if existing_id == id)
        }) {
            // Replace any masked copy with the original full record, then
            // pin it so subsequent rebuild partitioning cannot elide it.
            kept[index] = bound_obligation_message(obligation.clone());
            if let TranscriptMessage::Tool { pinned, .. } = &mut kept[index] {
                *pinned = true;
            }
            continue;
        }

        let mut retained = bound_obligation_message(obligation.clone());
        if let TranscriptMessage::Tool { pinned, .. } = &mut retained {
            *pinned = true;
        }
        kept.push(retained);
    }
    kept
}

/// Header on an obligation-tagged tool result whose bulk was reclaimed.
const BOUNDED_OBLIGATION_HEADER: &str =
    "[tool output bounded for compaction; open obligation kept]";

/// With the host option `bound_obligation_results` on, keep an obligation-tagged tool result
/// reclaimable (audit 2026-11-05 compaction M1). The marker otherwise pins the whole output
/// verbatim through every pass, so one large log mentioning "open obligation" could hold the
/// context above the trigger forever. Above the recent-output cap, keep only the obligation
/// itself: the text from the marker on, bounded to that cap. Off, the result is kept whole.
fn bound_obligation_message(mut message: TranscriptMessage) -> TranscriptMessage {
    if !crate::host_options::bound_obligation_results() {
        return message;
    }
    if let TranscriptMessage::Tool { summary, .. } = &mut message {
        if summary.len() > MAX_RECENT_TOOL_SUMMARY_BYTES {
            // ASCII uppercasing keeps byte offsets, so `marker` is a char
            // boundary in the original text.
            let marker = summary
                .to_ascii_uppercase()
                .find("OPEN OBLIGATION")
                .unwrap_or(0);
            let excerpt = truncate_utf8(&summary[marker..], MAX_RECENT_TOOL_SUMMARY_BYTES);
            *summary = format!("{BOUNDED_OBLIGATION_HEADER}\n{excerpt}");
        }
    }
    message
}

fn is_open_obligation_message(message: &TranscriptMessage) -> bool {
    matches!(
        message,
        TranscriptMessage::Tool { summary, .. }
            if summary.to_ascii_uppercase().contains("OPEN OBLIGATION")
    )
}

/// Normalize a compact boundary immediately before it is written by any
/// rebuild path, including replay/prefire callers that provide a persisted or
/// externally constructed summary.
pub(crate) fn normalize_compact_message(message: TranscriptMessage) -> TranscriptMessage {
    match message {
        TranscriptMessage::Compact {
            id,
            created_at,
            summary,
            dropped_count,
            pinned,
        } => TranscriptMessage::Compact {
            id,
            created_at,
            summary: core_safety_pass(&summary),
            dropped_count,
            pinned,
        },
        other => other,
    }
}

pub(crate) fn partition_authoritative_messages(
    messages: Vec<TranscriptMessage>,
    keep_recent: usize,
) -> (Vec<TranscriptMessage>, Vec<TranscriptMessage>) {
    let total = messages.len();
    let keep_from = total.saturating_sub(keep_recent);
    let preserved_skill_reads = preserved_skill_read_indices(&messages, keep_from);
    let preserved_tool_blocks = preserved_tool_block_indices(&messages, keep_from);
    let preserved_latest_user = latest_user_index_before(&messages, keep_from);

    let mut summarized = Vec::new();
    let mut kept = Vec::new();
    for (index, message) in messages.into_iter().enumerate() {
        // Prior summaries are supplied separately to the summarizer and replaced
        // by the single newly merged Compact message.
        if matches!(message, TranscriptMessage::Compact { .. }) {
            continue;
        }
        if is_pinned(&message)
            || index >= keep_from
            || preserved_skill_reads.contains(&index)
            || preserved_tool_blocks.contains(&index)
            || preserved_latest_user == Some(index)
        {
            kept.push(message);
        } else {
            summarized.push(message);
        }
    }
    (summarized, kept)
}

pub(crate) fn rebuild_authoritative_messages(
    kept: Vec<TranscriptMessage>,
    compact_message: TranscriptMessage,
    session_id: &str,
    workspace_root: &std::path::Path,
    memory_block_provider: Option<&dyn MemoryBlockProvider>,
) -> Vec<TranscriptMessage> {
    let compact_message = normalize_compact_message(compact_message);
    let (pinned_kept, recent_kept): (Vec<_>, Vec<_>) = kept.into_iter().partition(is_pinned);
    let has_continuation = pinned_kept.iter().any(|message| {
        matches!(
            message,
            TranscriptMessage::System { id, .. } if id == POST_COMPACTION_CONTINUATION_ID
        )
    });
    let has_prior_memory = has_memory_block(&pinned_kept, session_id);
    let pinned_kept = if has_prior_memory || memory_block_provider.is_some() {
        strip_prior_memory_block(pinned_kept, session_id)
    } else {
        pinned_kept
    };
    let memory_block = match memory_block_provider {
        Some(provider) => {
            let session = SessionRef {
                session_id,
                workspace_root,
                task_id: None,
                request_id: None,
                generation: None,
                capacity: Default::default(),
            };
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                provider.memory_block(session)
            }));
            match outcome {
                Ok(Some(block)) if !block.trim().is_empty() => Some(block),
                Ok(_) => None,
                Err(_) => {
                    eprintln!(
                        "[compactor] memory_block_provider panicked; \
                         proceeding without memory re-injection"
                    );
                    None
                }
            }
        }
        _ => None,
    };
    let capacity = pinned_kept.len() + 3 + recent_kept.len();
    let mut rebuilt = Vec::with_capacity(capacity);
    rebuilt.extend(pinned_kept);
    if !has_continuation {
        rebuilt.push(post_compaction_continuation_message());
    }
    if let Some(block) = memory_block {
        rebuilt.push(memory_block_message(session_id, &block));
    }
    rebuilt.push(compact_message);
    rebuilt.extend(recent_kept);
    rebuilt
}

/// Builds the post-compaction continuation system message.
pub(crate) fn post_compaction_continuation_message() -> TranscriptMessage {
    TranscriptMessage::System {
        id: POST_COMPACTION_CONTINUATION_ID.into(),
        created_at: now_iso(),
        content: POST_COMPACTION_CONTINUATION_STRINGS.join("\n"),
        pinned: true,
    }
}

/// Indices of assistant-initiated skill/instruction read blocks (assistant
/// tool-call + any consecutive `Tool` results) that should be preserved
/// verbatim, even when they sit before the `keep_from` boundary. The newest
/// [`MAX_PRESERVED_SKILL_READ_BLOCKS`] such blocks survive.
pub(crate) fn preserved_skill_read_indices(
    messages: &[TranscriptMessage],
    keep_from: usize,
) -> HashSet<usize> {
    let mut blocks = Vec::new();
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

        if i < keep_from && assistant_reads_skill_or_instruction(tool_calls) {
            blocks.push((i..block_end).collect::<Vec<_>>());
        }

        i = block_end;
    }

    blocks
        .into_iter()
        .rev()
        .take(MAX_PRESERVED_SKILL_READ_BLOCKS)
        .flatten()
        .collect()
}

/// Indices of assistant->tool blocks that straddle the `keep_from` boundary.
/// Providers that validate tool-result pairing (e.g. the OpenAI/Anthropic
/// chat-completions schema) need both halves preserved.
pub(crate) fn preserved_tool_block_indices(
    messages: &[TranscriptMessage],
    keep_from: usize,
) -> HashSet<usize> {
    let mut preserved = HashSet::new();
    let mut i = 0usize;

    while i < messages.len() {
        let TranscriptMessage::Assistant { tool_calls, .. } = &messages[i] else {
            i += 1;
            continue;
        };
        if tool_calls.is_empty() {
            i += 1;
            continue;
        }

        let mut block_end = i + 1;
        while block_end < messages.len()
            && matches!(messages[block_end], TranscriptMessage::Tool { .. })
        {
            block_end += 1;
        }

        if i < keep_from && block_end > keep_from {
            preserved.extend(i..block_end);
        }

        i = block_end;
    }

    preserved
}

/// Index of the latest user message before the `keep_from` boundary, so the
/// compaction keeps at least one user anchor.
pub(crate) fn latest_user_index_before(
    messages: &[TranscriptMessage],
    keep_from: usize,
) -> Option<usize> {
    messages[..keep_from.min(messages.len())]
        .iter()
        .rposition(|message| {
            // The quoted-tool-data user message is not the user's own intent.
            matches!(message, TranscriptMessage::User { id, .. } if !id.starts_with(PRESERVED_TOOL_DATA_ID_PREFIX))
        })
}

/// `true` if any of the assistant's tool calls read a SKILL.md / instruction
/// file (and should therefore be preserved across compaction).
pub(crate) fn assistant_reads_skill_or_instruction(tool_calls: &[ToolCall]) -> bool {
    tool_calls
        .iter()
        .any(|call| call.name == "invoke_skill" || read_file_targets_instruction(call))
}

/// `true` if a `read_file` tool call targets one of the canonical instruction
/// files (skill / agents / claude / instructions) or a known skills dir.
pub(crate) fn read_file_targets_instruction(call: &ToolCall) -> bool {
    if call.name != "read_file" {
        return false;
    }

    let Ok(args) = serde_json::from_str::<serde_json::Value>(&call.arguments_json) else {
        return false;
    };
    let Some(path) = args.get("path").and_then(|path| path.as_str()) else {
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
