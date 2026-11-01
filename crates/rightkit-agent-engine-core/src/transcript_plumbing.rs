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

#[cfg(test)]
pub(crate) fn inject_pending_view_images(transcript: &mut Transcript) -> bool {
    let mut pending = transcript
        .messages()
        .iter()
        .flat_map(|message| match message {
            TranscriptMessage::Tool {
                tool_call_id,
                summary,
                is_error,
                ..
            } if !is_error => extract_view_images(tool_call_id, summary),
            _ => Vec::new(),
        })
        .collect::<Vec<_>>();
    inject_pending_view_images_with_state(transcript, &mut pending)
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

/// Every tool-call id already carried by an assistant message in the
/// transcript, excluding the assistant message currently being streamed.
pub(crate) fn prior_tool_call_ids(
    transcript: &Transcript,
    current_assistant_id: Option<&str>,
) -> HashSet<String> {
    transcript
        .messages()
        .iter()
        .filter_map(|message| match message {
            TranscriptMessage::Assistant { id, tool_calls, .. }
                if Some(id.as_str()) != current_assistant_id =>
            {
                Some(tool_calls)
            }
            _ => None,
        })
        .flat_map(|calls| calls.iter().map(|call| call.id.clone()))
        .collect()
}

/// Cross-turn dedupe (audit C11). A provider that reuses a tool-call id from
/// an earlier turn would map one id to two invocations and corrupt tool-result
/// pairing, so such calls are dropped before dispatch and the transcript.
/// Returns the dropped calls.
pub(crate) fn drop_cross_turn_duplicate_tool_calls(
    calls: &mut Vec<ToolCall>,
    prior_ids: &HashSet<String>,
) -> Vec<ToolCall> {
    if prior_ids.is_empty() {
        return Vec::new();
    }
    let mut dropped = Vec::new();
    let mut kept = Vec::with_capacity(calls.len());
    for call in calls.drain(..) {
        if prior_ids.contains(&call.id) {
            dropped.push(call);
        } else {
            kept.push(call);
        }
    }
    *calls = kept;
    dropped
}

/// Note appended to the assistant text for a dropped duplicate-id call.
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

#[cfg(test)]
mod cross_turn_dedupe_tests {
    use super::*;

    fn call(id: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: "read_file".into(),
            arguments_json: "{}".into(),
        }
    }

    #[test]
    fn cross_turn_duplicate_tool_call_ids_are_rejected() {
        let mut transcript = Transcript::new();
        transcript.push_assistant("", None, vec![call("call_1")]);
        transcript.push_tool_result("call_1", "ok", false);
        let prior = prior_tool_call_ids(&transcript, None);
        let mut next = vec![call("call_1"), call("call_2")];
        let dropped = drop_cross_turn_duplicate_tool_calls(&mut next, &prior);
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].id, "call_1");
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].id, "call_2");
    }

    #[test]
    fn current_streaming_assistant_is_not_its_own_prior_turn() {
        let mut transcript = Transcript::new();
        let id = match transcript.push_assistant("", None, vec![call("call_9")]) {
            TranscriptMessage::Assistant { id, .. } => id.clone(),
            _ => unreachable!(),
        };
        assert!(prior_tool_call_ids(&transcript, Some(&id)).is_empty());
        assert!(prior_tool_call_ids(&transcript, None).contains("call_9"));
    }
}
