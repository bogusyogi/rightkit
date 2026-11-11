//! ENG-99 — an agent-facing asynchronous message inbox decoupled from the
//! live turn.
//!
//! `send_message.rs` (`SendMessageTool`) is turn-synchronous in the
//! direction that matters here: it *writes out* to another session, and its
//! delivery is either a live in-process dispatch or a drop-file another
//! daemon instance must be running to pick up. Nothing in this codebase
//! lets *this* agent later ask "what arrived for me while I wasn't looking,"
//! independent of whichever turn happens to be running when it arrives.
//! The async inbox (this module: [`push`] in, [`AsyncInboxTool`] out) is that receiving side: a bounded, per-session FIFO queue
//! that anything in-process can push into (a completed background shell, a
//! webhook callback, a deferred-tool-call resolution — see
//! `deferred_resolution.rs`'s module doc for how the two connect) and that
//! the `async_inbox` tool drains on request, with no requirement that the
//! draining turn be the same turn — or even the same conversation branch —
//! that was live when a message arrived.
//!
//! ## Session ownership
//!
//! The registry is process-wide only as storage: every queue is keyed by its
//! session id, engine construction activates that id, and session deletion
//! retires it. A late producer cannot recreate a deleted session's inbox.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};

/// Maximum messages retained per session. Past this the oldest message is
/// dropped to admit the newest — "never lost on turn end" bounds turn
/// boundaries, not total volume; an inbox nobody ever drains must still not
/// grow without limit.
pub const MAX_QUEUE_LEN: usize = 256;

/// One message sitting in a session's inbox.
#[derive(Debug, Clone)]
pub struct AsyncMessage {
    pub id: u64,
    pub channel: String,
    pub body: String,
    /// Optional structured payload alongside `body`.
    pub data: Option<Value>,
    pub enqueued_at_ms: u128,
}

type SessionQueue = VecDeque<AsyncMessage>;

/// Shared handle to one session's bounded queue plus its id counter, so
/// `id` stays monotonic even as old entries are evicted.
struct InboxState {
    queue: SessionQueue,
    next_id: u64,
    /// Count of messages evicted for exceeding `MAX_QUEUE_LEN`, surfaced to
    /// the draining tool call so a caller can tell "empty because nothing
    /// arrived" apart from "empty because I fell behind and lost some."
    dropped_total: u64,
}

impl Default for InboxState {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            next_id: 1,
            dropped_total: 0,
        }
    }
}

#[derive(Default)]
struct InboxRegistry {
    states: HashMap<String, InboxState>,
    /// A monotonically changing lease prevents a producer retained from a
    /// retired session from delivering into a later session that reuses its id.
    active: HashMap<String, u64>,
    next_generation: u64,
}

fn registry() -> &'static Mutex<InboxRegistry> {
    static INBOX_REGISTRY: OnceLock<Mutex<InboxRegistry>> = OnceLock::new();
    INBOX_REGISTRY.get_or_init(|| Mutex::new(InboxRegistry::default()))
}

/// Session-bound capability a background producer retains. Its generation is
/// checked at enqueue time, so retirement is terminal for producers from that
/// session lifetime even when an operator later reuses its visible id.
#[derive(Debug, Clone)]
pub struct AsyncInboxProducer {
    session_id: String,
    generation: u64,
}

impl AsyncInboxProducer {
    pub fn push(
        &self,
        channel: impl Into<String>,
        body: impl Into<String>,
        data: Option<Value>,
    ) -> Option<u64> {
        push_for_generation(
            &self.session_id,
            self.generation,
            channel.into(),
            body.into(),
            data,
        )
    }
}

/// Mark a session id live before it can receive asynchronous completions &
/// return producer capability scoped to its current lifetime.
pub fn activate_session(session_id: &str) -> AsyncInboxProducer {
    let mut guard = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let generation = match guard.active.get(session_id) {
        Some(generation) => *generation,
        None => {
            guard.next_generation = guard.next_generation.saturating_add(1).max(1);
            let generation = guard.next_generation;
            guard.active.insert(session_id.to_string(), generation);
            guard.states.remove(session_id);
            generation
        }
    };
    AsyncInboxProducer {
        session_id: session_id.to_string(),
        generation,
    }
}

/// Discard pending messages for a permanently deleted session. Reusing its id
/// must never expose completions from its former owner.
pub fn retire_session(session_id: &str) {
    let mut guard = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.active.remove(session_id);
    guard.states.remove(session_id);
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Push a message into `session_id`'s inbox. Not part of the `Tool` trait —
/// any in-process producer (a completed background job, a deferred-call
/// resolver, a webhook handler) calls this directly; it never goes through
/// the streaming loop, which is the whole point of decoupling delivery from
/// the live turn.
pub fn push(
    session_id: &str,
    channel: impl Into<String>,
    body: impl Into<String>,
    data: Option<Value>,
) -> Option<u64> {
    let generation = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .active
        .get(session_id)
        .copied()?;
    push_for_generation(session_id, generation, channel.into(), body.into(), data)
}

fn push_for_generation(
    session_id: &str,
    generation: u64,
    channel: String,
    body: String,
    data: Option<Value>,
) -> Option<u64> {
    let mut guard = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.active.get(session_id).copied() != Some(generation) {
        return None;
    }
    let state = guard.states.entry(session_id.to_string()).or_default();
    let id = state.next_id;
    state.next_id += 1;
    state.queue.push_back(AsyncMessage {
        id,
        channel,
        body,
        data,
        enqueued_at_ms: now_ms(),
    });
    while state.queue.len() > MAX_QUEUE_LEN {
        state.queue.pop_front();
        state.dropped_total += 1;
    }
    Some(id)
}

/// Read up to `limit` messages (oldest first), without acknowledging them.
pub fn read(session_id: &str, limit: usize) -> (Vec<AsyncMessage>, u64) {
    let guard = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(state) = guard.states.get(session_id) else {
        return (Vec::new(), 0);
    };
    let take = limit.min(state.queue.len());
    let messages = state.queue.iter().take(take).cloned().collect();
    (messages, state.dropped_total)
}

/// Acknowledge the contiguous prefix through `id`. Returns count removed;
/// unknown or stale ids are harmless, making retries idempotent.
pub fn acknowledge_through(session_id: &str, id: u64) -> usize {
    let mut guard = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(state) = guard.states.get_mut(session_id) else {
        return 0;
    };
    let mut removed = 0;
    while state.queue.front().is_some_and(|message| message.id <= id) {
        state.queue.pop_front();
        removed += 1;
    }
    removed
}

/// Compatibility helper for code which intentionally wants atomic
/// read-and-ack behavior.
pub fn drain(session_id: &str, limit: usize) -> (Vec<AsyncMessage>, u64) {
    let (messages, dropped_total) = read(session_id, limit);
    if let Some(last) = messages.last() {
        acknowledge_through(session_id, last.id);
    }
    (messages, dropped_total)
}

/// Number of messages currently queued for `session_id`, without draining.
/// For tests and diagnostics.
pub fn pending_count(session_id: &str) -> usize {
    registry()
        .lock()
        .unwrap()
        .states
        .get(session_id)
        .map(|s| s.queue.len())
        .unwrap_or(0)
}

/// `async_inbox` — read or acknowledge this session's asynchronous queue.
///
/// Read-only from the tool-call perspective (it mutates only the inbox's
/// own bookkeeping, never workspace state), so it declares
/// [`Tool::is_read_only`] `true` and emits no `mutations`.
pub struct AsyncInboxTool;

#[async_trait::async_trait]
impl Tool for AsyncInboxTool {
    fn name(&self) -> &str {
        "async_inbox"
    }

    fn description(&self) -> &str {
        "Read or acknowledge messages that arrived for this session outside the live turn — \
         completed background work, deferred tool-call resolutions, webhook or callback events. \
         Read returns the oldest pending messages first (bounded, default 20) without removing \
         them; acknowledge removes every pending message through one returned id."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["read", "acknowledge"],
                    "description": "read is default; acknowledge requires throughId."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum messages to read (default 20, max 256).",
                    "minimum": 1
                },
                "throughId": {
                    "type": "integer",
                    "description": "Last message id to acknowledge, inclusive."
                }
            }
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let limit = args["limit"]
            .as_u64()
            .map(|n| n.min(MAX_QUEUE_LEN as u64) as usize)
            .unwrap_or(20)
            .min(MAX_QUEUE_LEN);

        let action = args["action"].as_str().unwrap_or("read");
        if action == "acknowledge" {
            let Some(id) = args["throughId"].as_u64() else {
                return ToolResult::error("async_inbox acknowledge requires integer throughId.");
            };
            let acknowledged = acknowledge_through(&ctx.session_id, id);
            return ToolResult::ok(
                json!({
                    "acknowledged": acknowledged,
                    "throughId": id,
                    "pendingCount": pending_count(&ctx.session_id),
                })
                .to_string(),
            )
            .with_media_type("application/json");
        }
        if action != "read" {
            return ToolResult::error("async_inbox action must be read or acknowledge.");
        }

        let (messages, dropped_total) = read(&ctx.session_id, limit);

        if messages.is_empty() && dropped_total == 0 {
            return ToolResult::ok("Inbox empty: no messages pending.");
        }

        let payload = json!({
            "messages": messages.iter().map(|m| json!({
                "id": m.id,
                "channel": m.channel,
                "body": m.body,
                "data": m.data,
                "enqueuedAtMs": m.enqueued_at_ms,
            })).collect::<Vec<_>>(),
            "droppedTotal": dropped_total,
            "pendingCount": pending_count(&ctx.session_id),
        });

        ToolResult::ok(payload.to_string()).with_media_type("application/json")
    }
}
