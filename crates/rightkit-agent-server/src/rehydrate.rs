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
        if let Some(ProtocolEvent::ToolResultAdded { result, .. }) = decode_row_durable_event(row) {
            if let TranscriptMessage::Tool { tool_call_id, .. } = &result {
                answered_call_ids.insert(tool_call_id.clone());
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn row(seq: i64, ev: &ProtocolEvent) -> EventRow {
        EventRow {
            session_id: "s1".into(),
            seq,
            kind: String::new(),
            payload_json: serde_json::to_string(ev).unwrap(),
            created_at: "t".into(),
        }
    }

    #[test]
    fn folds_user_and_assistant_turns_in_order() {
        let user = ProtocolEvent::UserMessageSubmitted {
            session_id: "s1".into(),
            seq: 1,
            created_at: "t".into(),
            text: "hello".into(),
            nonce: None,
            trust: None,
            attachments: Vec::new(),
        };
        let assistant = ProtocolEvent::AssistantMessageCompleted {
            session_id: "s1".into(),
            seq: 2,
            created_at: "t".into(),
            message: TranscriptMessage::Assistant {
                id: "a1".into(),
                created_at: "t".into(),
                content: "hi there".into(),
                reasoning_content: None,
                tool_calls: vec![],
                pinned: false,
            },
        };
        let events = vec![row(1, &user), row(2, &assistant)];
        let t = rehydrate_transcript(&events);
        assert_eq!(t.len(), 2);
        match &t[0] {
            TranscriptMessage::User { content, .. } => assert_eq!(content, "hello"),
            _ => panic!("expected user first"),
        }
        match &t[1] {
            TranscriptMessage::Assistant { content, .. } => assert_eq!(content, "hi there"),
            _ => panic!("expected assistant second"),
        }
    }

    #[test]
    fn typed_user_references_survive_visible_and_model_rehydration() {
        let reference = rightkit_agent_engine_protocol::ReferenceMeta::empty(
            "ref-1",
            "repository",
            "https://github.com/stablyai/orca",
        );
        let event = row(
            1,
            &ProtocolEvent::UserMessageSubmitted {
                session_id: "s1".into(),
                seq: 1,
                created_at: "t".into(),
                text: "inspect this".into(),
                nonce: None,
                trust: None,
                attachments: vec![reference.clone()],
            },
        );
        let visible = rehydrate_transcript(std::slice::from_ref(&event));
        let model = rehydrate_engine_context(&[event], "s1", Path::new("."), None);
        assert_eq!(
            visible[0].user_attachments(),
            std::slice::from_ref(&reference)
        );
        assert_eq!(model[0].user_attachments(), &[reference]);
    }

    #[test]
    fn compaction_keeps_visible_history_and_inserts_an_inline_marker() {
        let mk_user = |seq: i64, t: &str| {
            row(
                seq,
                &ProtocolEvent::UserMessageSubmitted {
                    session_id: "s1".into(),
                    seq,
                    created_at: "t".into(),
                    text: t.into(),
                    nonce: None,
                    trust: None,
                    attachments: Vec::new(),
                },
            )
        };
        let compact = row(
            3,
            &ProtocolEvent::SessionCompacted {
                session_id: "s1".into(),
                seq: 3,
                created_at: "t".into(),
                summary: "summary of earlier turns".into(),
                dropped_count: 2,
                tier: String::new(),
                tokens_before: 0,
                tokens_after: 0,
                epoch: 0,
                command_id: String::new(),
                retained_turns: 8,
            },
        );
        // Two turns, then a compaction, then one more turn.
        let events = vec![
            mk_user(1, "old1"),
            mk_user(2, "old2"),
            compact,
            mk_user(4, "new"),
        ];
        let t = rehydrate_transcript(&events);
        // Context compaction must not erase the human-facing transcript.
        assert_eq!(t.len(), 4, "{t:?}");
        assert!(matches!(t[0], TranscriptMessage::User { .. }));
        assert!(matches!(t[1], TranscriptMessage::User { .. }));
        assert!(matches!(t[2], TranscriptMessage::Compact { .. }));
        match &t[3] {
            TranscriptMessage::User { content, .. } => assert_eq!(content, "new"),
            _ => panic!("expected the post-compaction user turn"),
        }
    }

    #[test]
    fn engine_context_replays_compaction_without_reseeding_dropped_history() {
        let mk_user = |seq: i64, text: &str| {
            row(
                seq,
                &ProtocolEvent::UserMessageSubmitted {
                    session_id: "s1".into(),
                    seq,
                    created_at: "t".into(),
                    text: text.into(),
                    nonce: None,
                    trust: None,
                    attachments: Vec::new(),
                },
            )
        };
        let mut events = (1..=12)
            .map(|seq| mk_user(seq, &format!("old-{seq}")))
            .collect::<Vec<_>>();
        events.push(row(
            13,
            &ProtocolEvent::SessionCompacted {
                session_id: "s1".into(),
                seq: 13,
                created_at: "t".into(),
                summary: "authoritative summary".into(),
                dropped_count: 3,
                tier: String::new(),
                tokens_before: 0,
                tokens_after: 0,
                epoch: 0,
                command_id: String::new(),
                retained_turns: 8,
            },
        ));
        events.push(mk_user(14, "after-boundary"));

        let visible = rehydrate_transcript(&events);
        let context = rehydrate_engine_context(&events, "s1", Path::new("/tmp"), None);

        assert_eq!(visible.len(), 14);
        assert!(context.len() < visible.len());
        assert!(!context.iter().any(|message| matches!(
            message,
            TranscriptMessage::User { content, .. } if content == "old-1"
        )));
        assert_eq!(
            context
                .iter()
                .filter(|message| matches!(message, TranscriptMessage::Compact { .. }))
                .count(),
            1
        );
        assert!(context.iter().any(|message| matches!(
            message,
            TranscriptMessage::User { content, .. } if content == "after-boundary"
        )));
    }

    fn retained_cut_events(retained_turns: u32) -> Vec<EventRow> {
        let mut events = (1..=12)
            .map(|seq| {
                row(
                    seq,
                    &ProtocolEvent::UserMessageSubmitted {
                        session_id: "s1".into(),
                        seq,
                        created_at: "t".into(),
                        text: format!("old-{seq}"),
                        nonce: None,
                        trust: None,
                        attachments: Vec::new(),
                    },
                )
            })
            .collect::<Vec<_>>();
        events.push(row(
            13,
            &ProtocolEvent::SessionCompacted {
                session_id: "s1".into(),
                seq: 13,
                created_at: "t".into(),
                summary: "authoritative summary".into(),
                dropped_count: 7,
                tier: String::new(),
                tokens_before: 0,
                tokens_after: 0,
                epoch: 0,
                command_id: String::new(),
                retained_turns,
            },
        ));
        events
    }

    fn retained_old_user_texts(events: &[EventRow]) -> Vec<String> {
        rehydrate_engine_context(events, "s1", Path::new("/tmp"), None)
            .into_iter()
            .filter_map(|message| match message {
                TranscriptMessage::User { content, .. } if content.starts_with("old-") => {
                    Some(content)
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn replay_uses_committed_retained_turns_not_the_default() {
        for turns in [3u32, 5] {
            let retained = retained_old_user_texts(&retained_cut_events(turns));
            // Live and replay share `partition_authoritative_messages`, which
            // keeps the N recent turns plus the latest user message before
            // the cut (compactor_preservation.rs:122,136): N + 1 here.
            let expected = (12 - turns..=12)
                .map(|n| format!("old-{n}"))
                .collect::<Vec<_>>();
            assert_eq!(retained, expected, "retained_turns={turns}");
        }
    }

    #[test]
    fn replay_of_zero_retained_turns_matches_pressure_pass() {
        // Zero recent turns still keeps the latest-user anchor, exactly like
        // the live under-pressure pass.
        assert_eq!(
            retained_old_user_texts(&retained_cut_events(0)),
            vec!["old-12".to_string()]
        );
    }

    #[test]
    fn context_fold_replays_identically_for_visible_and_engine_transcripts() {
        let mk_user = |seq: i64, text: &str| {
            row(
                seq,
                &ProtocolEvent::UserMessageSubmitted {
                    session_id: "s1".into(),
                    seq,
                    created_at: "t".into(),
                    text: text.into(),
                    nonce: None,
                    trust: None,
                    attachments: Vec::new(),
                },
            )
        };
        let fold = TranscriptMessage::Fold {
            id: "fold-task".into(),
            created_at: "t".into(),
            subtask_id: "task".into(),
            summary: "finished".into(),
            dropped_count: 2,
            archive_ref: "a".repeat(64),
            pinned: false,
        };
        let events = vec![
            mk_user(1, "[task:start]"),
            mk_user(2, "working"),
            mk_user(3, "[task:end]"),
            row(
                4,
                &ProtocolEvent::SessionContextFolded {
                    session_id: "s1".into(),
                    seq: 4,
                    created_at: "t".into(),
                    subtask_id: "task".into(),
                    start_marker: "[task:start]".into(),
                    end_marker: "[task:end]".into(),
                    fold,
                },
            ),
        ];
        let visible = rehydrate_transcript(&events);
        let engine = rehydrate_engine_context(&events, "s1", Path::new("/tmp"), None);
        assert_eq!(
            serde_json::to_value(&visible).unwrap(),
            serde_json::to_value(&engine).unwrap()
        );
        assert_eq!(visible.len(), 2);
        assert!(matches!(visible[0], TranscriptMessage::Fold { .. }));
    }

    #[test]
    fn rehydration_rejects_cross_session_payloads_and_canonicalizes_row_identity() {
        let event = ProtocolEvent::AssistantMessageCompleted {
            session_id: "other-session".into(),
            seq: 99,
            created_at: "t".into(),
            message: TranscriptMessage::Assistant {
                id: "final".into(),
                created_at: "t".into(),
                content: "must not leak".into(),
                reasoning_content: None,
                tool_calls: vec![],
                pinned: false,
            },
        };
        let mut foreign = row(7, &event);
        assert!(rehydrate_transcript(&[foreign.clone()]).is_empty());

        foreign.session_id = "other-session".into();
        let transcript = rehydrate_transcript(&[foreign]);
        match &transcript[0] {
            TranscriptMessage::Assistant { id, .. } => assert_eq!(id, "assistant:7"),
            other => panic!("expected assistant message, got {other:?}"),
        }
    }

    #[test]
    fn transient_projection_loss_duplication_and_reordering_do_not_change_semantic_replay() {
        let user = row(
            1,
            &ProtocolEvent::UserMessageSubmitted {
                session_id: "s1".into(),
                seq: 1,
                created_at: "t".into(),
                text: "hello".into(),
                nonce: None,
                trust: None,
                attachments: Vec::new(),
            },
        );
        let tool = row(
            2,
            &ProtocolEvent::ToolResultAdded {
                session_id: "s1".into(),
                seq: 2,
                created_at: "t".into(),
                result: TranscriptMessage::Tool {
                    id: "tool-result".into(),
                    created_at: "t".into(),
                    tool_call_id: "call-1".into(),
                    summary: "ok".into(),
                    output: None,
                    trust: None,
                    is_error: false,
                    pinned: false,
                    process_id: None,
                },
            },
        );
        let assistant = row(
            3,
            &ProtocolEvent::AssistantMessageCompleted {
                session_id: "s1".into(),
                seq: 3,
                created_at: "t".into(),
                message: TranscriptMessage::Assistant {
                    id: "assistant".into(),
                    created_at: "t".into(),
                    content: "done".into(),
                    reasoning_content: None,
                    tool_calls: vec![],
                    pinned: false,
                },
            },
        );
        let task = rightkit_agent_engine_protocol::TaskNode {
            id: "task-1".into(),
            title: "Task".into(),
            deps: vec![],
            done_when: "done".into(),
            done_when_predicates: vec![],
            state: rightkit_agent_engine_protocol::TaskState::Open,
            category: "dev".into(),
            estimated_duration: String::new(),
            assigned_role: String::new(),
            escalation_path: String::new(),
            open_questions: vec![],
            created_at: "t".into(),
            updated_at: "t".into(),
        };
        let task_graph = row(
            4,
            &ProtocolEvent::TaskGraphUpdated {
                session_id: "s1".into(),
                seq: 4,
                created_at: "t".into(),
                summary: "one".into(),
                tasks: vec![task],
            },
        );
        let delta = |seq: i64, text: &str| {
            row(
                seq,
                &ProtocolEvent::AssistantDelta {
                    session_id: "s1".into(),
                    seq,
                    created_at: "t".into(),
                    text: text.into(),
                    is_reasoning: false,
                },
            )
        };
        let baseline = vec![
            user.clone(),
            tool.clone(),
            assistant.clone(),
            task_graph.clone(),
        ];
        let noisy = vec![
            delta(91, "late"),
            user,
            delta(90, "early"),
            delta(90, "early"),
            tool,
            delta(92, "middle"),
            assistant,
            task_graph,
            delta(89, "reordered"),
        ];
        assert_eq!(
            serde_json::to_value(rehydrate_transcript(&baseline)).unwrap(),
            serde_json::to_value(rehydrate_transcript(&noisy)).unwrap(),
        );
        assert_eq!(
            serde_json::to_value(rehydrate_engine_context(
                &baseline,
                "s1",
                Path::new("."),
                None
            ))
            .unwrap(),
            serde_json::to_value(rehydrate_engine_context(&noisy, "s1", Path::new("."), None))
                .unwrap(),
        );
        assert_eq!(
            serde_json::to_value(rehydrate_tasks(&baseline)).unwrap(),
            serde_json::to_value(rehydrate_tasks(&noisy)).unwrap(),
        );
    }

    #[test]
    fn terminal_turn_reconstructs_after_midstream_disconnect_without_deltas() {
        let user = row(
            1,
            &ProtocolEvent::UserMessageSubmitted {
                session_id: "s1".into(),
                seq: 1,
                created_at: "t".into(),
                text: "hello".into(),
                nonce: None,
                trust: None,
                attachments: Vec::new(),
            },
        );
        let delta = row(
            2,
            &ProtocolEvent::AssistantDelta {
                session_id: "s1".into(),
                seq: 2,
                created_at: "t".into(),
                text: "par".into(),
                is_reasoning: false,
            },
        );
        let assistant = row(
            3,
            &ProtocolEvent::AssistantMessageCompleted {
                session_id: "s1".into(),
                seq: 3,
                created_at: "t".into(),
                message: TranscriptMessage::Assistant {
                    id: "final".into(),
                    created_at: "t".into(),
                    content: "complete".into(),
                    reasoning_content: None,
                    tool_calls: vec![],
                    pinned: false,
                },
            },
        );
        let terminal = row(
            4,
            &ProtocolEvent::SessionTurnFinished {
                session_id: "s1".into(),
                seq: 4,
                created_at: "t".into(),
                turn_id: "turn-1".into(),
                status: "completed".into(),
            },
        );
        let uninterrupted = vec![user.clone(), delta, assistant.clone(), terminal.clone()];
        let reconnected = vec![user, assistant, terminal];
        assert_eq!(
            serde_json::to_value(rehydrate_transcript(&uninterrupted)).unwrap(),
            serde_json::to_value(rehydrate_transcript(&reconnected)).unwrap(),
        );
        assert_eq!(
            rehydrate_terminal_turn(&uninterrupted),
            rehydrate_terminal_turn(&reconnected),
        );
        assert_eq!(
            rehydrate_terminal_turn(&reconnected),
            Some(RehydratedTerminalTurn {
                seq: 4,
                turn_id: "turn-1".into(),
                status: "completed".into()
            })
        );
    }

    #[test]
    #[ignore = "EVT-004 final qualification: run durable reconnect + tail/backward paging driver"]
    fn qualification_evt004_disconnect_reconnect_pages_to_same_terminal_truth() {
        let mut store = rightkit_agent_engine_storage::Storage::open_in_memory().unwrap();
        store
            .create_session(&rightkit_agent_engine_storage::SessionRow {
                id: "s1".into(),
                title: "reconnect".into(),
                workspace_root: "/tmp".into(),
                created_at: "t".into(),
                updated_at: "t".into(),
                parent_session_id: None,
                role: None,
                status: "running".into(),
                config_json: "{}".into(),
                tags: String::new(),
                recovery_marker: None,
            })
            .unwrap();
        let events = vec![
            (
                "userMessageSubmitted",
                ProtocolEvent::UserMessageSubmitted {
                    session_id: "s1".into(),
                    seq: 1,
                    created_at: "t1".into(),
                    text: "hello".into(),
                    nonce: Some("evt004-nonce".into()),
                    trust: None,
                    attachments: Vec::new(),
                },
            ),
            (
                "assistantDelta",
                ProtocolEvent::AssistantDelta {
                    session_id: "s1".into(),
                    seq: 2,
                    created_at: "t2".into(),
                    text: "par".into(),
                    is_reasoning: false,
                },
            ),
            (
                "assistantDelta",
                ProtocolEvent::AssistantDelta {
                    session_id: "s1".into(),
                    seq: 3,
                    created_at: "t3".into(),
                    text: "tial".into(),
                    is_reasoning: false,
                },
            ),
            (
                "assistantMessageCompleted",
                ProtocolEvent::AssistantMessageCompleted {
                    session_id: "s1".into(),
                    seq: 4,
                    created_at: "t4".into(),
                    message: TranscriptMessage::Assistant {
                        id: "final".into(),
                        created_at: "t4".into(),
                        content: "complete".into(),
                        reasoning_content: None,
                        tool_calls: vec![],
                        pinned: false,
                    },
                },
            ),
            (
                "sessionTurnFinished",
                ProtocolEvent::SessionTurnFinished {
                    session_id: "s1".into(),
                    seq: 5,
                    created_at: "t5".into(),
                    turn_id: "turn-1".into(),
                    status: "completed".into(),
                },
            ),
        ];
        for (kind, event) in &events {
            let seq = rightkit_agent_engine_core::conduit::seq_of(event);
            store
                .append_event_at_seq("s1", seq, kind, &serde_json::to_string(event).unwrap())
                .unwrap();
        }
        let uninterrupted = store.all_events("s1").unwrap();
        assert_eq!(
            uninterrupted.iter().map(|row| row.seq).collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5]
        );

        // A client disconnected after receiving streaming seq=2. On reconnect
        // it hydrates the durable tail first, then pages backwards until empty.
        let tail = store.events_before("s1", 6, 2).unwrap();
        let middle = store.events_before("s1", tail[0].seq, 2).unwrap();
        let head = store.events_before("s1", middle[0].seq, 2).unwrap();
        assert!(store
            .events_before("s1", head[0].seq, 2)
            .unwrap()
            .is_empty());
        let paged = [head.as_slice(), middle.as_slice(), tail.as_slice()].concat();
        assert_eq!(
            paged, uninterrupted,
            "tail-first hydration plus backward paging preserves durable order"
        );

        let durable_only = paged
            .iter()
            .filter(|row| {
                serde_json::from_str::<ProtocolEvent>(&row.payload_json)
                    .is_ok_and(|event| event.is_durable_fact())
            })
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            durable_only.iter().map(|row| row.seq).collect::<Vec<_>>(),
            vec![1, 4, 5]
        );
        assert_eq!(
            serde_json::to_value(rehydrate_transcript(&uninterrupted)).unwrap(),
            serde_json::to_value(rehydrate_transcript(&durable_only)).unwrap(),
        );
        assert_eq!(
            rehydrate_terminal_turn(&uninterrupted),
            rehydrate_terminal_turn(&durable_only)
        );
        assert_eq!(
            rehydrate_terminal_turn(&durable_only).unwrap().status,
            "completed"
        );
    }

    fn tool_call(id: &str, name: &str) -> rightkit_agent_engine_protocol::ToolCall {
        rightkit_agent_engine_protocol::ToolCall {
            id: id.into(),
            name: name.into(),
            arguments_json: "{}".into(),
        }
    }

    fn started_row(seq: i64, call: rightkit_agent_engine_protocol::ToolCall) -> EventRow {
        row(
            seq,
            &ProtocolEvent::ToolCallStarted {
                session_id: "s1".into(),
                seq,
                created_at: "t".into(),
                call,
            },
        )
    }

    fn approval_row(seq: i64, call: rightkit_agent_engine_protocol::ToolCall) -> EventRow {
        row(
            seq,
            &ProtocolEvent::ToolApprovalRequested {
                session_id: "s1".into(),
                seq,
                created_at: "t".into(),
                call,
                risk_label: "edit".into(),
                proposed_change: None,
                policy_source: None,
                middleware_decisions: Vec::new(),
            },
        )
    }

    fn result_row(seq: i64, call_id: &str) -> EventRow {
        row(
            seq,
            &ProtocolEvent::ToolResultAdded {
                session_id: "s1".into(),
                seq,
                created_at: "t".into(),
                result: TranscriptMessage::Tool {
                    id: "tr".into(),
                    created_at: "t".into(),
                    tool_call_id: call_id.into(),
                    summary: "ok".into(),
                    output: None,
                    trust: None,
                    is_error: false,
                    pinned: false,
                    process_id: None,
                },
            },
        )
    }

    fn assistant_completed_row(seq: i64, text: &str) -> EventRow {
        row(
            seq,
            &ProtocolEvent::AssistantMessageCompleted {
                session_id: "s1".into(),
                seq,
                created_at: "t".into(),
                message: TranscriptMessage::Assistant {
                    id: "final".into(),
                    created_at: "t".into(),
                    content: text.into(),
                    reasoning_content: None,
                    tool_calls: vec![],
                    pinned: false,
                },
            },
        )
    }

    fn declared_calls(message: &TranscriptMessage) -> &[rightkit_agent_engine_protocol::ToolCall] {
        match message {
            TranscriptMessage::Assistant { tool_calls, .. } => tool_calls,
            other => panic!("expected assistant tool-call envelope, got {other:?}"),
        }
    }

    #[test]
    fn engine_context_pairs_retained_result_with_its_recorded_call() {
        let events = vec![
            started_row(1, tool_call("call-1", "read")),
            result_row(2, "call-1"),
            assistant_completed_row(3, "done"),
        ];
        let context = rehydrate_engine_context(&events, "s1", Path::new("."), None);
        assert_eq!(context.len(), 3, "{context:?}");
        let calls = declared_calls(&context[0]);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call-1");
        assert_eq!(calls[0].name, "read");
        assert_eq!(calls[0].arguments_json, "{}");
        match &context[1] {
            TranscriptMessage::Tool { tool_call_id, .. } => {
                assert_eq!(tool_call_id, "call-1")
            }
            other => panic!("expected tool result after its call, got {other:?}"),
        }
        match &context[2] {
            TranscriptMessage::Assistant {
                content,
                tool_calls,
                ..
            } => {
                assert_eq!(content, "done");
                assert!(tool_calls.is_empty());
            }
            other => panic!("expected final text, got {other:?}"),
        }
    }

    #[test]
    fn denied_approval_call_is_paired_without_a_started_record() {
        // A denied call never reaches ToolCallStarted; the approval request
        // alone is the durable call fact.
        let events = vec![
            approval_row(1, tool_call("call-d", "write")),
            result_row(2, "call-d"),
        ];
        let context = rehydrate_engine_context(&events, "s1", Path::new("."), None);
        assert_eq!(context.len(), 2, "{context:?}");
        let calls = declared_calls(&context[0]);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call-d");
        assert!(matches!(context[1], TranscriptMessage::Tool { .. }));
    }

    #[test]
    fn approval_and_started_records_for_one_call_emit_a_single_call() {
        let events = vec![
            approval_row(1, tool_call("call-1", "write")),
            started_row(2, tool_call("call-1", "write")),
            result_row(3, "call-1"),
        ];
        let context = rehydrate_engine_context(&events, "s1", Path::new("."), None);
        assert_eq!(context.len(), 2, "{context:?}");
        assert_eq!(declared_calls(&context[0]).len(), 1);
    }

    #[test]
    fn serial_and_parallel_calls_replay_as_valid_call_result_pairs() {
        let serial = vec![
            started_row(1, tool_call("call-a", "read")),
            result_row(2, "call-a"),
            started_row(3, tool_call("call-b", "write")),
            result_row(4, "call-b"),
            assistant_completed_row(5, "done"),
        ];
        let context = rehydrate_engine_context(&serial, "s1", Path::new("."), None);
        assert_eq!(context.len(), 5, "{context:?}");
        assert_eq!(declared_calls(&context[0])[0].id, "call-a");
        assert_eq!(declared_calls(&context[2])[0].id, "call-b");
        assert!(matches!(context[1], TranscriptMessage::Tool { .. }));
        assert!(matches!(context[3], TranscriptMessage::Tool { .. }));

        let parallel = vec![
            started_row(1, tool_call("call-a", "read")),
            started_row(2, tool_call("call-b", "read")),
            result_row(3, "call-a"),
            result_row(4, "call-b"),
        ];
        let context = rehydrate_engine_context(&parallel, "s1", Path::new("."), None);
        assert_eq!(context.len(), 3, "{context:?}");
        let calls = declared_calls(&context[0]);
        assert_eq!(
            calls.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["call-a", "call-b"]
        );
        assert!(matches!(context[1], TranscriptMessage::Tool { .. }));
        assert!(matches!(context[2], TranscriptMessage::Tool { .. }));
    }

    #[test]
    fn cancelled_call_without_a_result_retains_its_uncertain_call() {
        let events = vec![
            started_row(1, tool_call("call-c", "write")),
            assistant_completed_row(2, "stopped"),
        ];
        let context = rehydrate_engine_context(&events, "s1", Path::new("."), None);
        assert_eq!(context.len(), 2, "{context:?}");
        assert!(declared_calls(&context[0]).is_empty());
        assert_eq!(declared_calls(&context[1])[0].id, "call-c");
        assert!(!context
            .iter()
            .any(|message| matches!(message, TranscriptMessage::Tool { .. })));
    }

    #[test]
    fn foreign_session_call_facts_do_not_repair_orphan_results() {
        let foreign = ProtocolEvent::ToolCallStarted {
            session_id: "other-session".into(),
            seq: 1,
            created_at: "t".into(),
            call: tool_call("call-x", "read"),
        };
        let events = vec![row(1, &foreign), result_row(2, "call-x")];
        let context = rehydrate_engine_context(&events, "s1", Path::new("."), None);
        // The foreign call fact is dropped at decode; the same-session
        // result is retained but no call is invented for it.
        assert_eq!(context.len(), 1, "{context:?}");
        assert!(matches!(context[0], TranscriptMessage::Tool { .. }));
    }

    #[test]
    fn todo_list_survives_compaction_from_durable_task_events() {
        use rightkit_agent_engine_protocol::TaskState::{Done, InProgress, Open};
        let node = |i: usize, title: &str, state: rightkit_agent_engine_protocol::TaskState| {
            rightkit_agent_engine_protocol::TaskNode {
                id: format!("todo-{i}"),
                title: title.into(),
                deps: vec![],
                done_when: String::new(),
                done_when_predicates: vec![],
                state,
                category: "todo".into(),
                estimated_duration: String::new(),
                assigned_role: String::new(),
                escalation_path: String::new(),
                open_questions: vec![],
                created_at: "t".into(),
                updated_at: "t".into(),
            }
        };
        let graph = |seq: i64, tasks: Vec<rightkit_agent_engine_protocol::TaskNode>| {
            row(
                seq,
                &ProtocolEvent::TaskGraphUpdated {
                    session_id: "s1".into(),
                    seq,
                    created_at: "t".into(),
                    summary: String::new(),
                    tasks,
                },
            )
        };
        // Transcript rows were compacted away; only durable task events remain.
        let events = vec![
            graph(1, vec![node(0, "Read", InProgress), node(1, "Edit", Open)]),
            graph(2, vec![node(0, "Read", Done), node(1, "Edit", InProgress)]),
        ];
        assert!(rehydrate_transcript(&events).is_empty());
        let tasks = rehydrate_tasks(&events);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].state, Done);
        assert_eq!(tasks[1].state, InProgress);
    }

    #[test]
    fn ignores_unrelated_events() {
        let delta = ProtocolEvent::AssistantDelta {
            session_id: "s1".into(),
            seq: 1,
            created_at: "t".into(),
            text: "partial".into(),
            is_reasoning: false,
        };
        assert!(rehydrate_transcript(&[row(1, &delta)]).is_empty());
    }
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

#[cfg(test)]
mod undecodable_tests {
    use super::*;

    fn row(seq: i64, kind: &str, payload: &str) -> EventRow {
        EventRow {
            session_id: "s".into(),
            seq,
            kind: kind.into(),
            payload_json: payload.into(),
            created_at: "t".into(),
        }
    }

    #[test]
    fn undecodable_rows_are_reported_but_projection_and_tail_are_not() {
        let good = r#"{"$kind":"userMessageSubmitted","sessionId":"s","seq":0,"createdAt":"t","text":"hi","attachments":[]}"#;
        let events = vec![
            row(1, "userMessageSubmitted", good),
            row(
                2,
                "userMessageSubmitted",
                r#"{"oversizedPayloadSpilled":true}"#,
            ),
            row(3, "compactionProjection", r#"{"not":"a protocol event"}"#),
            row(4, "userMessageSubmitted", "not json"),
            row(
                5,
                "userMessageSubmitted",
                "tail is kept as the high-water anchor",
            ),
        ];
        assert_eq!(undecodable_event_seqs(&events), vec![2, 4]);
    }
}
