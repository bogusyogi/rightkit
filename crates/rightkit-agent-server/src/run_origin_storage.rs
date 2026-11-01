//! SQLite-backed `RunOriginSink` (Lane C3 task 1, additive wire-up).
//!
//! The engine speaks only to the [`RunOriginSink`] trait — it knows nothing
//! about SQLite. This module is the daemon-side adapter that turns a
//! `record_origin` call into a durable `events` row whose `kind` is
//! `"toolCallStarted"`. The sink blocks until the row is committed, then
//! returns `OriginWriteResult::Persisted(seq)` carrying the freshly assigned
//! storage sequence so the post-effect code can correlate by the same id.
//!
//! Failure handling:
//!
//! - Lock contention or any storage error → `OriginWriteResult::Failed` so
//!   the dispatch path refuses the side effect.
//! - Empty session id or sink not yet wired → `OriginWriteResult::Deferred`
//!   so the engine knows to retry on the next turn slot.
//!
//! The sink is owned by the daemon at session-creation time; the engine
//! only ever sees the trait object installed via `set_run_origin_sink`.

use rightkit_agent_engine_core::run_origin::{OriginWriteResult, RunOriginSink};
use rightkit_agent_engine_protocol::ToolCall;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::api::SharedStorage;

/// SQLite-backed origin sink. Persists every accepted tool call as a
/// `kind = "toolCallStarted"` event row, blocking until the row commits.
pub struct StorageRunOriginSink {
    storage: Option<SharedStorage>,
    session_id: String,
}

impl StorageRunOriginSink {
    /// Construct a sink bound to a session's storage handle. `storage` may
    /// be `None` (a dev/test daemon without persistence); the sink then
    /// reports `Deferred` until storage is wired up.
    pub fn new(storage: Option<SharedStorage>, session_id: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            storage,
            session_id: session_id.into(),
        })
    }
}

impl RunOriginSink for StorageRunOriginSink {
    fn record_origin(&self, call: &ToolCall) -> OriginWriteResult {
        let Some(storage) = self.storage.as_ref() else {
            return OriginWriteResult::Deferred("storage not wired on this session".into());
        };
        if self.session_id.is_empty() {
            return OriginWriteResult::Deferred("empty session id".into());
        }
        // This legacy hook is deliberately nonblocking. Live tool dispatch
        // uses `record_event_async`, which preserves the canonical event seq.
        let mut guard = match storage.try_lock() {
            Ok(guard) => guard,
            Err(_) => return OriginWriteResult::Deferred("storage writer busy".into()),
        };
        let payload = match serde_json::to_string(&serde_json::json!({
            "toolCallId": call.id,
            "toolName": call.name,
            "call": call,
        })) {
            Ok(payload) => payload,
            Err(error) => return OriginWriteResult::Failed(format!("serialize call: {error}")),
        };
        match guard.append_event(&self.session_id, "toolCallStarted", &payload) {
            Ok(seq) => OriginWriteResult::Persisted(seq),
            Err(error) => OriginWriteResult::Failed(format!("append_event: {error}")),
        }
    }

    fn is_ready(&self) -> bool {
        self.storage.is_some() && !self.session_id.is_empty()
    }

    fn record_event_async<'a>(
        &'a self,
        event: &'a rightkit_agent_engine_protocol::ProtocolEvent,
    ) -> Pin<Box<dyn Future<Output = OriginWriteResult> + Send + 'a>> {
        Box::pin(async move {
            let rightkit_agent_engine_protocol::ProtocolEvent::ToolCallStarted {
                session_id,
                seq,
                call,
                ..
            } = event
            else {
                return OriginWriteResult::Failed("origin event was not ToolCallStarted".into());
            };
            if session_id != &self.session_id {
                return OriginWriteResult::Failed("origin event session binding mismatch".into());
            }
            let Some(storage) = self.storage.as_ref() else {
                return OriginWriteResult::Deferred("storage not wired on this session".into());
            };
            let payload = match serde_json::to_string(event) {
                Ok(payload) => payload,
                Err(error) => {
                    return OriginWriteResult::Failed(format!("serialize call: {error}"));
                }
            };
            let kind = "toolCallStarted";
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
            loop {
                let outcome = storage
                    .lock()
                    .await
                    .append_event_at_seq(session_id, *seq, kind, &payload);
                match outcome {
                    Ok(()) => return OriginWriteResult::Persisted(*seq),
                    Err(
                        rightkit_agent_engine_storage::PersistenceGapError::SeqAboveHighWater {
                            ..
                        },
                    ) if tokio::time::Instant::now() < deadline => {
                        // Earlier Conduit events are persisted by the background writer. Release
                        // the SQLite mutex while it catches up, then reserve this exact sequence
                        // before allowing the effect. A failed/stalled writer reaches the deadline
                        // and the tool call fails closed.
                        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
                    }
                    Err(error) => {
                        return OriginWriteResult::Failed(format!(
                            "append_event_at_seq for {}: {error}",
                            call.id
                        ));
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_engine_protocol::{ProtocolEvent, ToolCall};
    use rightkit_agent_engine_storage::{SessionRow, Storage};

    #[tokio::test(flavor = "current_thread")]
    async fn canonical_origin_write_is_safe_on_current_thread_runtime() {
        let storage = SharedStorage::new(Storage::open_in_memory().unwrap());
        {
            let guard = storage.lock().await;
            guard
                .create_session(&SessionRow {
                    id: "origin-current-thread".into(),
                    title: "Origin test".into(),
                    workspace_root: "/tmp".into(),
                    created_at: "2026-07-22T00:00:00Z".into(),
                    updated_at: "2026-07-22T00:00:00Z".into(),
                    parent_session_id: None,
                    role: None,
                    status: "active".into(),
                    config_json: "{}".into(),
                    tags: String::new(),
                    recovery_marker: None,
                })
                .unwrap();
        }
        let sink = StorageRunOriginSink::new(Some(storage), "origin-current-thread");
        let event = ProtocolEvent::ToolCallStarted {
            session_id: "origin-current-thread".into(),
            seq: 1,
            created_at: "2026-07-22T00:00:00Z".into(),
            call: ToolCall {
                id: "call-current-thread".into(),
                name: "read_file".into(),
                arguments_json: "{}".into(),
            },
        };

        assert_eq!(
            sink.record_event_async(&event).await,
            OriginWriteResult::Persisted(1)
        );
    }
}
