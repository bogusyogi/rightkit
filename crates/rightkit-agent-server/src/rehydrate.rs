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

/// Visible-transcript fold. One copy of the logic serves `rehydrate_transcript`
/// and `rehydrate_for_resume`; `push` takes each decoded event by value.
pub(crate) struct TranscriptFold {
    out: Vec<TranscriptMessage>,
    rewound_tail: Option<Vec<TranscriptMessage>>,
}

impl TranscriptFold {
    pub(crate) fn new() -> Self {
        Self {
            out: Vec::new(),
            rewound_tail: None,
        }
    }

    /// Whether the visible fold reads this event at all. Call-start and
    /// approval events carry the large call arguments and are ignored by it.
    fn uses(event: &ProtocolEvent) -> bool {
        matches!(
            event,
            ProtocolEvent::UserMessageSubmitted { .. }
                | ProtocolEvent::AssistantMessageCompleted { .. }
                | ProtocolEvent::ToolResultAdded { .. }
                | ProtocolEvent::SessionCompacted { .. }
                | ProtocolEvent::SessionContextFolded { .. }
                | ProtocolEvent::HumanSteeringInjected { .. }
        )
    }

    pub(crate) fn push(&mut self, seq: i64, event: ProtocolEvent) {
        match event {
            ProtocolEvent::UserMessageSubmitted {
                text,
                created_at,
                trust,
                attachments,
                ..
            } => self.out.push(TranscriptMessage::user_with_attachments(
                format!("user-{}", seq),
                created_at,
                text,
                trust,
                false,
                attachments,
            )),
            ProtocolEvent::AssistantMessageCompleted { message, .. } => self.out.push(message),
            ProtocolEvent::ToolResultAdded { result, .. } => self.out.push(result),
            ProtocolEvent::SessionCompacted {
                summary,
                dropped_count,
                created_at,
                ..
            } => {
                self.out.push(TranscriptMessage::Compact {
                    id: format!("compact-{}", seq),
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
                apply_context_fold(&mut self.out, &start_marker, &end_marker, fold);
            }
            ProtocolEvent::HumanSteeringInjected {
                injected_by, text, ..
            } => apply_rewind_marker(
                &mut self.out,
                &mut self.rewound_tail,
                &injected_by,
                text.as_deref(),
            ),
            _ => {}
        }
    }

    pub(crate) fn finish(self) -> Vec<TranscriptMessage> {
        self.out
    }
}

/// Fold persisted events (in seq order) into a transcript.
pub fn rehydrate_transcript(events: &[EventRow]) -> Vec<TranscriptMessage> {
    let mut fold = TranscriptFold::new();
    for row in events {
        let Some(ev) = decode_row_durable_event(row) else {
            continue;
        };
        fold.push(row.seq, ev);
    }
    fold.finish()
}

/// Replay a persisted rewind or redo marker so a rebuilt transcript matches the
/// live one: a rewind drops the chosen prompt and everything after it (kept for
/// a redo), a redo puts that tail back. A marker whose prompt is gone changes
/// nothing, so corrupt events fail closed.
fn apply_rewind_marker(
    messages: &mut Vec<TranscriptMessage>,
    stash: &mut Option<Vec<TranscriptMessage>>,
    injected_by: &str,
    text: Option<&str>,
) {
    match injected_by {
        "rewind" => {
            let Some(target) = text
                .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
                .and_then(|marker| {
                    marker["rewind"]["messageCreatedAt"]
                        .as_str()
                        .map(str::to_string)
                })
            else {
                return;
            };
            let Some(position) = messages.iter().rposition(|message| {
                matches!(
                    message,
                    TranscriptMessage::User { created_at, pinned: false, .. } if *created_at == target
                )
            }) else {
                return;
            };
            *stash = Some(messages.split_off(position));
        }
        "redo" => {
            if let Some(tail) = stash.take() {
                messages.extend(tail);
            }
        }
        _ => {}
    }
}

/// Result-backed calls can be grouped ahead of their retained outputs. Ids only;
/// no payload is kept.
fn answered_call_ids(events: &[EventRow]) -> std::collections::HashSet<String> {
    let mut answered = std::collections::HashSet::new();
    for row in events {
        if let Some(ProtocolEvent::ToolResultAdded {
            result: TranscriptMessage::Tool { tool_call_id, .. },
            ..
        }) = decode_row_durable_event(row)
        {
            answered.insert(tool_call_id);
        }
    }
    answered
}

/// Rebuild the visible transcript and the model context for a resume in one
/// pass that consumes the rows, so each payload is freed as soon as it is
/// folded. Folds and their inputs are the same as `rehydrate_transcript` and
/// `rehydrate_engine_context`; a row that does not decode is skipped by both.
pub(crate) fn rehydrate_for_resume(
    events: Vec<EventRow>,
    session_id: &str,
    workspace_root: &std::path::Path,
    memory_block_provider: Option<std::sync::Arc<dyn MemoryBlockProvider>>,
) -> (Vec<TranscriptMessage>, Vec<TranscriptMessage>) {
    let mut context = EngineContextFold::new(
        answered_call_ids(&events),
        session_id,
        workspace_root,
        memory_block_provider,
    );
    let mut transcript = TranscriptFold::new();
    for row in events {
        context.note_row(row.seq);
        let Some(event) = decode_row_durable_event(&row) else {
            continue;
        };
        if TranscriptFold::uses(&event) {
            transcript.push(row.seq, event.clone());
        }
        context.push(row.seq, event);
    }
    (transcript.finish(), context.finish())
}

/// Model-context fold behind `rehydrate_engine_context` and `rehydrate_for_resume`.
pub(crate) struct EngineContextFold<'a> {
    answered_call_ids: std::collections::HashSet<String>,
    session_id: &'a str,
    workspace_root: &'a std::path::Path,
    memory_block_provider: Option<std::sync::Arc<dyn MemoryBlockProvider>>,
    messages: Vec<TranscriptMessage>,
    rewound_tail: Option<Vec<TranscriptMessage>>,
    // Durable call facts (`ToolCallStarted` / `ToolApprovalRequested`) whose
    // retained results have not yet replayed. The native engine publishes
    // only a final tool-free `AssistantMessageCompleted`, so these facts are
    // the sole source of the call records the provider needs ahead of each
    // `function_call_output`. Unanswered real call facts remain pending for
    // interrupted-call repair; replay never invents a successful result.
    pending_calls: Vec<rightkit_agent_engine_protocol::ToolCall>,
    pending_ids: std::collections::HashSet<String>,
    emitted_ids: std::collections::HashSet<String>,
    last_seq: Option<i64>,
}

impl<'a> EngineContextFold<'a> {
    pub(crate) fn new(
        answered_call_ids: std::collections::HashSet<String>,
        session_id: &'a str,
        workspace_root: &'a std::path::Path,
        memory_block_provider: Option<std::sync::Arc<dyn MemoryBlockProvider>>,
    ) -> Self {
        Self {
            answered_call_ids,
            session_id,
            workspace_root,
            memory_block_provider,
            messages: Vec::new(),
            rewound_tail: None,
            pending_calls: Vec::new(),
            pending_ids: std::collections::HashSet::new(),
            emitted_ids: std::collections::HashSet::new(),
            last_seq: None,
        }
    }

    /// Called for EVERY row, decodable or not: the `replay-pending-<seq>` anchor
    /// is the last row of the whole log.
    pub(crate) fn note_row(&mut self, seq: i64) {
        self.last_seq = Some(seq);
    }

    pub(crate) fn push(&mut self, seq: i64, event: ProtocolEvent) {
        match event {
            ProtocolEvent::UserMessageSubmitted {
                text,
                created_at,
                trust,
                attachments,
                ..
            } => {
                // The engine sent this turn with a reminder keyed to the event's own
                // timestamp; rebuild that exact content so a resumed session keeps the
                // provider's cached prefix byte-identical.
                let reminder = crate::api::user_turn_reminder_at(&created_at, &[]);
                let wrapped = trust.as_ref().map_or_else(
                    || text.clone(),
                    |trust| trust.boundary_wrapped_content(&text),
                );
                let content =
                    crate::api::decorate_user_payload_with_system_reminder(&wrapped, &reminder);
                self.messages.push(TranscriptMessage::user_with_attachments(
                    format!("user-{}", seq),
                    created_at,
                    content,
                    trust,
                    false,
                    attachments,
                ))
            }
            ProtocolEvent::ToolCallStarted { call, .. }
            | ProtocolEvent::ToolApprovalRequested { call, .. } => {
                // An approval request and the subsequent dispatch start both
                // carry the same call; record it once.
                if !self.pending_ids.contains(&call.id) && !self.emitted_ids.contains(&call.id) {
                    self.pending_ids.insert(call.id.clone());
                    self.pending_calls.push(call);
                }
            }
            ProtocolEvent::AssistantMessageCompleted { message, .. } => self.messages.push(message),
            ProtocolEvent::ToolResultAdded { result, .. } => {
                // Declare every recorded, result-backed call ahead of the
                // first retained output. Parallel calls share one assistant
                // envelope; serial calls each get their own. An orphan output
                // with no durable call fact stays an orphan — nothing is
                // invented for it.
                let (declared, deferred): (Vec<_>, Vec<_>) =
                    std::mem::take(&mut self.pending_calls)
                        .into_iter()
                        .partition(|call| self.answered_call_ids.contains(&call.id));
                self.pending_calls = deferred;
                self.pending_ids = self
                    .pending_calls
                    .iter()
                    .map(|call| call.id.clone())
                    .collect();
                if !declared.is_empty() {
                    for call in &declared {
                        self.emitted_ids.insert(call.id.clone());
                    }
                    self.messages.push(TranscriptMessage::Assistant {
                        id: rightkit_agent_engine_protocol::ids::assistant_message_id(seq),
                        created_at: result.created_at().to_string(),
                        content: String::new(),
                        reasoning_content: None,
                        tool_calls: declared,
                        pinned: false,
                        origin: None,
                    });
                }
                self.messages.push(result);
            }
            ProtocolEvent::SessionCompacted {
                summary,
                dropped_count,
                created_at,
                retained_turns,
                ..
            } => {
                let mut transcript = rightkit_agent_engine_core::transcript::Transcript::new();
                transcript.replace(std::mem::take(&mut self.messages));
                let keep_recent = retained_turns as usize;
                let compactor = rightkit_agent_engine_core::Compactor::new(0, keep_recent);
                let compactor = match self.memory_block_provider.clone() {
                    Some(provider) => compactor.with_memory_block_provider(provider),
                    None => compactor,
                };
                compactor.replay_authoritative_boundary(
                    &mut transcript,
                    format!("compact-{}", seq),
                    created_at,
                    summary,
                    dropped_count,
                    self.session_id,
                    self.workspace_root,
                );
                self.messages = transcript.messages().to_vec();
            }
            ProtocolEvent::SessionContextFolded {
                start_marker,
                end_marker,
                fold,
                ..
            } => {
                apply_context_fold(&mut self.messages, &start_marker, &end_marker, fold);
            }
            ProtocolEvent::OrchestrationObservationRecorded { observation, .. } => {
                // A loop warning the model was sent as a message: put it back where the live
                // transcript had it (between the tool results that tripped it and the next call).
                if let Some(nudge) =
                    rightkit_agent_engine_core::transcript::loop_warning_nudge(&observation, seq)
                {
                    self.messages.push(nudge);
                }
            }
            ProtocolEvent::HumanSteeringInjected {
                injected_by, text, ..
            } => {
                apply_rewind_marker(
                    &mut self.messages,
                    &mut self.rewound_tail,
                    &injected_by,
                    text.as_deref(),
                );
                if injected_by == "rewind" {
                    // Calls started in the rewound turns must not resurface as pending.
                    self.pending_calls.clear();
                    self.pending_ids.clear();
                }
            }
            _ => {}
        }
    }

    pub(crate) fn finish(mut self) -> Vec<TranscriptMessage> {
        if !self.pending_calls.is_empty() {
            // A started/requested call without a durable result remains uncertain.
            // Retain its real call fact so the engine's dangling-call repair emits
            // an interrupted error before the resumed provider request.
            self.messages.push(TranscriptMessage::Assistant {
                id: format!("replay-pending-{}", self.last_seq.unwrap_or(0)),
                created_at: rightkit_agent_engine_protocol::now_iso(),
                content: String::new(),
                reasoning_content: None,
                tool_calls: self.pending_calls,
                pinned: false,
                origin: None,
            });
        }
        self.messages
    }
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
/// ever reported. Callers copy the reported rows to quarantine but keep them in
/// the live log, because a newer build may decode them (audit M1).
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
