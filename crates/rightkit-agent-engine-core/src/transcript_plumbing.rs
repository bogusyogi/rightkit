//! Transcript-side helpers used by the main session loop.
//!
//! These helpers keep view-image payload injection, interrupted tool-call repair,
//! and worker-spawning classification out of the core `SessionEngine` loop.

use crate::transcript::{tool_result_trust, Transcript};
use rightkit_agent_engine_protocol::{ToolCall, TranscriptMessage};
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingViewImage {
    tool_call_id: String,
    data_url: String,
}

pub(crate) fn extract_view_images(tool_call_id: &str, content: &str) -> Vec<PendingViewImage> {
    content
        .lines()
        .filter_map(|line| {
            let data_url = line.trim().strip_prefix("data_url: ")?.trim();
            if !is_supported_image_data_url(data_url) {
                return None;
            }
            Some(PendingViewImage {
                tool_call_id: tool_call_id.to_string(),
                data_url: data_url.to_string(),
            })
        })
        .collect()
}

fn is_supported_image_data_url(data_url: &str) -> bool {
    let Some(rest) = data_url.strip_prefix("data:image/") else {
        return false;
    };
    rest.contains(";base64,")
}

pub(crate) fn inject_pending_view_images_with_state(
    transcript: &mut Transcript,
    pending: &mut Vec<PendingViewImage>,
) -> bool {
    let Some((assistant_index, tool_call_ids)) = latest_assistant_tool_call_block(transcript)
    else {
        return false;
    };
    let tool_call_id_set = tool_call_ids.iter().cloned().collect::<HashSet<_>>();
    let following = &transcript.messages()[assistant_index + 1..];

    if following.iter().any(is_view_image_clear_message) {
        pending.clear();
        return false;
    }
    if following.iter().any(is_structured_view_image_message) {
        pending.retain(|image| !tool_call_id_set.contains(&image.tool_call_id));
        return false;
    }

    let paired_tool_results = following
        .iter()
        .filter_map(|message| match message {
            TranscriptMessage::Tool { tool_call_id, .. }
                if tool_call_id_set.contains(tool_call_id) =>
            {
                Some(tool_call_id.clone())
            }
            _ => None,
        })
        .collect::<HashSet<_>>();
    if !tool_call_ids
        .iter()
        .all(|tool_call_id| paired_tool_results.contains(tool_call_id))
    {
        return false;
    }

    let images = tool_call_ids
        .iter()
        .flat_map(|tool_call_id| {
            pending
                .iter()
                .filter(move |image| &image.tool_call_id == tool_call_id)
                .cloned()
        })
        .collect::<Vec<_>>();
    if images.is_empty() {
        return false;
    }

    transcript.push_pinned_user(view_image_user_content(&images));
    pending.retain(|image| !tool_call_id_set.contains(&image.tool_call_id));
    true
}

fn latest_assistant_tool_call_block(transcript: &Transcript) -> Option<(usize, Vec<String>)> {
    let messages = transcript.messages();
    let assistant_index = messages.iter().rposition(
        |message| matches!(message, TranscriptMessage::Assistant { tool_calls, .. } if !tool_calls.is_empty()),
    )?;
    let TranscriptMessage::Assistant { tool_calls, .. } = &messages[assistant_index] else {
        return None;
    };
    Some((
        assistant_index,
        tool_calls.iter().map(|call| call.id.clone()).collect(),
    ))
}

fn is_view_image_clear_message(message: &TranscriptMessage) -> bool {
    let TranscriptMessage::User { content, .. } = message else {
        return false;
    };
    let normalized = content.trim().to_ascii_lowercase();
    normalized == "[view_image:clear]"
        || (normalized.contains("<view_image") && normalized.contains("clear"))
}

fn is_structured_view_image_message(message: &TranscriptMessage) -> bool {
    let TranscriptMessage::User { content, .. } = message else {
        return false;
    };
    let Ok(parts) = serde_json::from_str::<serde_json::Value>(content) else {
        return false;
    };
    let Some(parts) = parts.as_array() else {
        return false;
    };
    parts.iter().any(|part| {
        part.get("type").and_then(|value| value.as_str()) == Some("image_url")
            && part
                .get("image_url")
                .and_then(|image| image.get("url"))
                .and_then(|url| url.as_str())
                .is_some_and(is_supported_image_data_url)
    })
}

fn view_image_user_content(images: &[PendingViewImage]) -> String {
    let mut parts = vec![serde_json::json!({
        "type": "text",
        "text": format!(
            "[view_image] {} image(s) from completed tool results are attached for visual inspection.",
            images.len()
        ),
    })];
    parts.extend(images.iter().map(|image| {
        serde_json::json!({
            "type": "image_url",
            "image_url": {
                "url": image.data_url,
            },
        })
    }));
    serde_json::to_string(&parts).expect("view_image parts serialize")
}

pub(crate) fn repair_dangling_tool_calls(transcript: &mut Transcript) -> usize {
    let mut repaired = Vec::with_capacity(transcript.len());
    let mut inserted = 0usize;
    let mut i = 0usize;
    let messages = transcript.messages().to_vec();

    while i < messages.len() {
        let message = messages[i].clone();
        let tool_calls = match &message {
            TranscriptMessage::Assistant { tool_calls, .. } if !tool_calls.is_empty() => {
                tool_calls.clone()
            }
            _ => {
                repaired.push(message);
                i += 1;
                continue;
            }
        };

        repaired.push(message);
        i += 1;

        let mut tool_block = Vec::new();
        while i < messages.len() && matches!(messages[i], TranscriptMessage::Tool { .. }) {
            tool_block.push(messages[i].clone());
            i += 1;
        }

        let mut consumed = HashSet::new();
        for call in &tool_calls {
            let existing = tool_block.iter().enumerate().find(|(index, candidate)| {
                !consumed.contains(index)
                    && matches!(
                        candidate,
                        TranscriptMessage::Tool { tool_call_id, .. } if tool_call_id == &call.id
                    )
            });
            if let Some((index, tool_result)) = existing {
                consumed.insert(index);
                repaired.push(tool_result.clone());
            } else {
                inserted += 1;
                repaired.push(TranscriptMessage::Tool {
                    id: format!("synthetic-missing-tool-{}", call.id),
                    created_at: rightkit_agent_engine_protocol::now_iso(),
                    tool_call_id: call.id.clone(),
                    summary: format!(
                        "Tool call '{}' ({}) did not complete before the session was interrupted or resumed. Treat this as a failed tool result and decide the next safe step.",
                        call.id, call.name
                    ),
                    output: None,
                    trust: Some(tool_result_trust(&call.name, &call.id)),
                    is_error: true,
                    process_id: None,
                    pinned: false,
                });
            }
        }

        for (index, tool_result) in tool_block.into_iter().enumerate() {
            if !consumed.contains(&index) {
                repaired.push(tool_result);
            }
        }
    }

    if inserted > 0 {
        transcript.replace(repaired);
    }
    inserted
}

/// Tool calls already carried by an assistant message in the transcript, excluding the
/// assistant message currently being streamed.
fn prior_tool_calls<'a>(
    transcript: &'a Transcript,
    current_assistant_id: Option<&'a str>,
) -> impl Iterator<Item = &'a ToolCall> {
    transcript
        .messages()
        .iter()
        .filter_map(move |message| match message {
            TranscriptMessage::Assistant { id, tool_calls, .. }
                if Some(id.as_str()) != current_assistant_id =>
            {
                Some(tool_calls)
            }
            _ => None,
        })
        .flat_map(|calls| calls.iter())
}

/// What to do with a call whose id an earlier turn already used.
pub(crate) enum ReusedId {
    /// The id is new: run the call as it is.
    Fresh,
    /// Same id and same tool and arguments as an earlier call: a replay, never run twice.
    Replay,
    /// Same id, different call (a provider that numbers ids per response): the call was given a
    /// new id and runs.
    Rekeyed,
}

/// Cross-turn dedupe (audit C11). A provider that reuses a tool-call id from an earlier turn
/// would map one id to two invocations and corrupt tool-result pairing. A call that repeats an
/// earlier call exactly is a replay and is dropped. A call with new arguments under a reused id
/// is a new call: it takes a fresh id (`<id>~<n>`) so its result pairs with it alone.
/// `taken` holds ids already used in the response being checked.
pub(crate) fn resolve_reused_tool_call_id(
    call: &mut ToolCall,
    transcript: &Transcript,
    current_assistant_id: Option<&str>,
    taken: &HashSet<String>,
) -> ReusedId {
    let mut reused = false;
    let mut shape: Option<String> = None;
    for earlier in prior_tool_calls(transcript, current_assistant_id).filter(|c| c.id == call.id) {
        reused = true;
        let wanted = shape.get_or_insert_with(|| crate::mop::tool_call_shape(call));
        if crate::mop::tool_call_shape(earlier) == *wanted {
            return ReusedId::Replay;
        }
    }
    if !reused {
        return ReusedId::Fresh;
    }
    let mut n = 1usize;
    let new_id = loop {
        let candidate = format!("{}~{n}", call.id);
        if !taken.contains(&candidate)
            && !prior_tool_calls(transcript, current_assistant_id).any(|c| c.id == candidate)
        {
            break candidate;
        }
        n += 1;
    };
    call.id = new_id;
    ReusedId::Rekeyed
}

/// Cross-turn dedupe for a finished response: drops replays, re-keys the rest. Returns the
/// dropped calls.
pub(crate) fn drop_cross_turn_duplicate_tool_calls(
    calls: &mut Vec<ToolCall>,
    transcript: &Transcript,
) -> Vec<ToolCall> {
    let mut dropped = Vec::new();
    let mut kept: Vec<ToolCall> = Vec::with_capacity(calls.len());
    let mut taken: HashSet<String> = HashSet::new();
    for mut call in calls.drain(..) {
        match resolve_reused_tool_call_id(&mut call, transcript, None, &taken) {
            ReusedId::Replay => dropped.push(call),
            ReusedId::Fresh | ReusedId::Rekeyed => {
                taken.insert(call.id.clone());
                kept.push(call);
            }
        }
    }
    *calls = kept;
    dropped
}

/// Note appended to the assistant text for a dropped replayed call.
pub(crate) fn duplicate_tool_call_note(call: &ToolCall) -> String {
    format!(
        "[Rejected tool call `{}`: its id `{}` was already used in an earlier turn. The call was not run; reissue it if still needed.]",
        call.name, call.id
    )
}

pub(crate) fn is_worker_spawning_tool_call(call: &ToolCall) -> bool {
    call.name == "spawn_subagent" || is_local_agent_task_create(call)
}

fn is_local_agent_task_create(call: &ToolCall) -> bool {
    if call.name != "task_create" {
        return false;
    }

    serde_json::from_str::<serde_json::Value>(&call.arguments_json)
        .ok()
        .and_then(|args| {
            args.get("kind")
                .and_then(|kind| kind.as_str())
                .map(|kind| kind.eq_ignore_ascii_case("local_agent"))
        })
        .unwrap_or(false)
}

/// The session so far as plain text for the `advisor` tool: each user and assistant message and a
/// one-line summary per tool result, oldest first. Reasoning and raw tool payloads are left out.
pub(crate) fn advisor_transcript_text(messages: &[TranscriptMessage]) -> String {
    let mut out = String::new();
    for message in messages {
        let (label, text) = match message {
            TranscriptMessage::User { content, .. } => ("User", content.as_str()),
            TranscriptMessage::Assistant { content, .. } => ("Assistant", content.as_str()),
            TranscriptMessage::Tool { summary, .. } => ("Tool result", summary.as_str()),
            _ => continue,
        };
        if text.trim().is_empty() {
            continue;
        }
        out.push_str(label);
        out.push_str(": ");
        out.push_str(text.trim());
        out.push_str("\n\n");
    }
    out
}
