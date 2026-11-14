//! Session rehydration — rebuild a transcript from the persisted event log so a
//! session's history survives a daemon restart (session-lifecycle P2).
//!
//! The event log (written by `spawn_event_persister`) is the source of truth.
//! This fold handles a superset of events compared to the live conduit consumer
//! in `api.rs`:
//!
//! - `UserMessageSubmitted` is handled here but not in the live fold, because
//!   the live session records user messages directly in `post_message()` before
//!   the engine runs.
//!
//! - `SessionCompacted` adds the same inline marker used by the live fold. It
//!   never clears the human-facing transcript: context compaction and visible
//!   conversation history are separate concerns.
//!
//! These are not unintentional duplications: the two paths serve genuinely
//! different purposes (incremental live update vs. full reconstruction).

use rightkit_agent_engine_core::compactor::MemoryBlockProvider;
use rightkit_agent_engine_protocol::{ProtocolEvent, TranscriptMessage};
use rightkit_agent_engine_storage::EventRow;

/// Decode one persisted row using the row's session and sequence as the
/// authoritative identity. Rehydration is a trust boundary: a payload from a
/// different session must not enter the requested transcript, and constant
/// message ids must not be allowed to collide after restart.
fn decode_row_event(row: &EventRow) -> Option<ProtocolEvent> {
    let mut event = serde_json::from_str::<ProtocolEvent>(&row.payload_json).ok()?;
    if event.session_id() != row.session_id {
        return None;
    }
    event.canonicalize_identity(row.seq);
    Some(event)
}

/// Decode only canonical semantic replay truth. Live projection events may be
/// persisted for diagnostics/catch-up compatibility, but they are never inputs
/// to transcript, provider-context, or task reconstruction.
fn decode_row_durable_event(row: &EventRow) -> Option<ProtocolEvent> {
    decode_row_event(row).filter(ProtocolEvent::is_durable_fact)
}

/// Fold persisted events (in seq order) into a transcript.
pub fn rehydrate_transcript(events: &[EventRow]) -> Vec<TranscriptMessage> {
    let mut out = Vec::new();
    for row in events {
        let Some(ev) = decode_row_durable_event(row) else {
            continue;
        };
        match ev {
            ProtocolEvent::UserMessageSubmitted {
                text,
                created_at,
                trust,
                attachments,
                ..
            } => out.push(TranscriptMessage::user_with_attachments(
                format!("user-{}", row.seq),
                created_at,
                text,
                trust,
                false,
                attachments,
            )),
            ProtocolEvent::AssistantMessageCompleted { message, .. } => out.push(message),
            ProtocolEvent::ToolResultAdded { result, .. } => out.push(result),
            ProtocolEvent::SessionCompacted {
                summary,
                dropped_count,
                created_at,
                ..
            } => {
                out.push(TranscriptMessage::Compact {
                    id: format!("compact-{}", row.seq),
                    created_at,
                    summary,
                    dropped_count,
                    pinned: false,
                });
            }
            ProtocolEvent::SessionContextFolded {
                start_marker,
                end_marker,
                fold,
                ..
            } => {
                apply_context_fold(&mut out, &start_marker, &end_marker, fold);
            }
            _ => {}
        }
    }
    out
}

/// Rebuild the model-facing context from the same event log while replaying
/// authoritative compaction boundaries. The visible transcript intentionally
/// keeps full history; this seed must not, or a resumed near-full session can
/// exceed the model window before the next automatic compaction can run.
///
/// `session_id` and `workspace_root` identify the rebuilt session for the
/// memory-block provider; `memory_block_provider` is the optional seam
/// that re-injects a fresh durable-memory block at every replayed
/// boundary, mirroring the live `compact_authoritative` path. When the
/// provider is `None` the rebuild behaves exactly as before — no
/// sentinel block is injected.
pub fn rehydrate_engine_context(
    events: &[EventRow],
    session_id: &str,
    workspace_root: &std::path::Path,
    memory_block_provider: Option<std::sync::Arc<dyn MemoryBlockProvider>>,
) -> Vec<TranscriptMessage> {
    use std::collections::HashSet;

    // Result-backed calls can be grouped ahead of their retained outputs.
    // Unanswered real call facts remain pending for interrupted-call repair;
    // replay never invents a successful result for them.
    let mut answered_call_ids = HashSet::new();
    for row in events {
        if let Some(ProtocolEvent::ToolResultAdded {
            result: TranscriptMessage::Tool { tool_call_id, .. },
            ..
        }) = decode_row_durable_event(row).as_ref()
        {
            answered_call_ids.insert(tool_call_id.clone());
        }
    }

    let mut messages = Vec::new();
    // Durable call facts (`ToolCallStarted` / `ToolApprovalRequested`) whose
    // retained results have not yet replayed. The native engine publishes
    // only a final tool-free `AssistantMessageCompleted`, so these facts are
    // the sole source of the call records the provider needs ahead of each
    // `function_call_output`.
    let mut pending_calls: Vec<rightkit_agent_engine_protocol::ToolCall> = Vec::new();
    let mut pending_ids = HashSet::new();
    let mut emitted_ids = HashSet::new();
    for row in events {
        let Some(event) = decode_row_durable_event(row) else {
            continue;
        };
        match event {
            ProtocolEvent::UserMessageSubmitted {
                text,
                created_at,
                trust,
                attachments,
                ..
            } => messages.push(TranscriptMessage::user_with_attachments(
                format!("user-{}", row.seq),
                created_at,
                text,
                trust,
                false,
                attachments,
            )),
            ProtocolEvent::ToolCallStarted { call, .. }
            | ProtocolEvent::ToolApprovalRequested { call, .. } => {
                // An approval request and the subsequent dispatch start both
                // carry the same call; record it once.
                if !pending_ids.contains(&call.id) && !emitted_ids.contains(&call.id) {
                    pending_ids.insert(call.id.clone());
                    pending_calls.push(call);
                }
            }
            ProtocolEvent::AssistantMessageCompleted { message, .. } => messages.push(message),
            ProtocolEvent::ToolResultAdded { result, .. } => {
                // Declare every recorded, result-backed call ahead of the
                // first retained output. Parallel calls share one assistant
                // envelope; serial calls each get their own. An orphan output
                // with no durable call fact stays an orphan — nothing is
                // invented for it.
                let (declared, deferred): (Vec<_>, Vec<_>) = std::mem::take(&mut pending_calls)
                    .into_iter()
                    .partition(|call| answered_call_ids.contains(&call.id));
                pending_calls = deferred;
                pending_ids = pending_calls.iter().map(|call| call.id.clone()).collect();
                if !declared.is_empty() {
                    for call in &declared {
                        emitted_ids.insert(call.id.clone());
                    }
                    messages.push(TranscriptMessage::Assistant {
                        id: rightkit_agent_engine_protocol::ids::assistant_message_id(row.seq),
                        created_at: result.created_at().to_string(),
                        content: String::new(),
                        reasoning_content: None,
                        tool_calls: declared,
                        pinned: false,
                        origin: None,
                    });
                }
                messages.push(result);
            }
            ProtocolEvent::SessionCompacted {
                summary,
                dropped_count,
                created_at,
                retained_turns,
                ..
            } => {
                let mut transcript = rightkit_agent_engine_core::transcript::Transcript::new();
                transcript.replace(messages);
                let keep_recent = retained_turns as usize;
                let compactor = rightkit_agent_engine_core::Compactor::new(0, keep_recent);
                let compactor = match memory_block_provider.clone() {
                    Some(provider) => compactor.with_memory_block_provider(provider),
                    None => compactor,
                };
                compactor.replay_authoritative_boundary(
                    &mut transcript,
                    format!("compact-{}", row.seq),
                    created_at,
                    summary,
                    dropped_count,
                    session_id,
                    workspace_root,
                );
                messages = transcript.messages().to_vec();
                continue;
            }
            ProtocolEvent::SessionContextFolded {
                start_marker,
                end_marker,
                fold,
                ..
            } => {
                apply_context_fold(&mut messages, &start_marker, &end_marker, fold);
            }
            _ => {}
        }
    }
    if !pending_calls.is_empty() {
        // A started/requested call without a durable result remains uncertain.
        // Retain its real call fact so the engine's dangling-call repair emits
        // an interrupted error before the resumed provider request.
        messages.push(TranscriptMessage::Assistant {
            id: format!("replay-pending-{}", events.last().map_or(0, |row| row.seq)),
            created_at: rightkit_agent_engine_protocol::now_iso(),
            content: String::new(),
            reasoning_content: None,
            tool_calls: pending_calls,
            pinned: false,
            origin: None,
        });
    }
    messages
}

/// Apply a persisted fold boundary. Returns false rather than mutating when
/// either marker is missing or out of order, so corrupt events fail closed.
pub(crate) fn apply_context_fold(
    messages: &mut Vec<TranscriptMessage>,
    start_marker: &str,
    end_marker: &str,
    fold: TranscriptMessage,
) -> bool {
    let contains = |message: &TranscriptMessage, marker: &str| match message {
        TranscriptMessage::User { content, .. } | TranscriptMessage::System { content, .. } => {
            content.contains(marker)
        }
        _ => false,
    };
    let Some(start) = messages
        .iter()
        .position(|message| contains(message, start_marker))
    else {
        return false;
    };
    let Some(end) = messages
        .iter()
        .enumerate()
        .skip(start + 1)
        .find_map(|(index, message)| contains(message, end_marker).then_some(index))
    else {
        return false;
    };
    messages.splice(start..end, [fold]);
    true
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RehydratedTerminalTurn {
    pub seq: i64,
    pub turn_id: String,
    pub status: String,
}

/// Latest durable terminal turn fact. Streaming deltas and transport
/// disconnects are intentionally irrelevant to this reconstruction.
pub fn rehydrate_terminal_turn(events: &[EventRow]) -> Option<RehydratedTerminalTurn> {
    events
        .iter()
        .filter_map(|row| match decode_row_durable_event(row)? {
            ProtocolEvent::SessionTurnFinished {
                seq,
                turn_id,
                status,
                ..
            } => Some(RehydratedTerminalTurn {
                seq,
                turn_id,
                status,
            }),
            _ => None,
        })
        .next_back()
}

/// Latest task-graph snapshot reconstructed from the event log (the last
/// `TaskGraphUpdated` wins, mirroring the live consumer).
pub fn rehydrate_tasks(events: &[EventRow]) -> Vec<rightkit_agent_engine_protocol::TaskNode> {
    let mut tasks = Vec::new();
    for row in events {
        if let Some(ProtocolEvent::TaskGraphUpdated { tasks: t, .. }) =
            decode_row_durable_event(row)
        {
            tasks = t;
        }
    }
    tasks
}

/// Seqs of persisted rows that cannot be decoded as protocol events for
/// `session_id` (corrupt JSON, a foreign session's payload, or an oversized
/// spill stub). The compaction projection record is a legitimate non-protocol
/// row and the final row anchors the persisted high-water mark, so neither is
/// ever reported. Callers move the reported rows to quarantine instead of
/// skipping them on every resume (audit M9).
pub(crate) fn undecodable_event_seqs(events: &[EventRow]) -> Vec<i64> {
    let last_seq = events.last().map(|row| row.seq);
    events
        .iter()
        .filter(|row| Some(row.seq) != last_seq)
        .filter(|row| {
            row.kind != rightkit_agent_engine_core::compaction_archive::PROJECTION_EVENT_KIND
        })
        .filter(|row| decode_row_event(row).is_none())
        .map(|row| row.seq)
        .collect()
}
