//! iOS-facing ENG-86/99 runtime surface.
//!
//! This module keeps deferred receiver ownership inside the mobile runtime,
//! exposes bounded inbox read/ack snapshots for FFI, and explicitly retires
//! both pieces when Swift ends a local session. It has no host-process or
//! shell dependency.

use crate::{MobileRuntime, RuntimeError};
use rightkit_agent_tools::coding_tools::async_inbox::{self, AsyncMessage, MAX_QUEUE_LEN};
use rightkit_agent_tools::coding_tools::deferred_resolution::{DeferredResolution, ResolveError};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

type DeferredKey = (String, String);
type DeferredReceivers = HashMap<DeferredKey, tokio::sync::oneshot::Receiver<DeferredResolution>>;

fn deferred_receivers() -> &'static Mutex<DeferredReceivers> {
    static RECEIVERS: OnceLock<Mutex<DeferredReceivers>> = OnceLock::new();
    RECEIVERS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// FFI-safe projection of one session inbox message.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileInboxMessage {
    pub id: u64,
    pub channel: String,
    pub body: String,
    pub data: Option<Value>,
    pub enqueued_at_ms: u128,
}

impl From<AsyncMessage> for MobileInboxMessage {
    fn from(message: AsyncMessage) -> Self {
        Self {
            id: message.id,
            channel: message.channel,
            body: message.body,
            data: message.data,
            enqueued_at_ms: message.enqueued_at_ms,
        }
    }
}

/// Bounded snapshot returned to Swift before it acknowledges any messages.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileInboxSnapshot {
    pub messages: Vec<MobileInboxMessage>,
    pub dropped_total: u64,
    pub pending_count: usize,
}

impl MobileRuntime {
    /// Register work whose resolution will arrive after its originating turn.
    /// The receiver stays alive until resolution or explicit session retirement,
    /// so delayed resolution has a real, session-owned lifecycle on iOS.
    pub async fn defer_inbox_work(&self, tool_call_id: &str) -> Result<(), RuntimeError> {
        if tool_call_id.trim().is_empty() {
            return Err(RuntimeError::Config(
                "tool_call_id must not be empty".into(),
            ));
        }
        let key = (self.session_id.clone(), tool_call_id.to_string());
        if deferred_receivers()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains_key(&key)
        {
            return Err(RuntimeError::Engine(format!(
                "deferred tool call '{tool_call_id}' is already pending"
            )));
        }

        let engine = self.engine.lock().await;
        let deferred =
            engine
                .defer_tool_call(tool_call_id.to_string())
                .map_err(|error| match error {
                    rightkit_agent_tools::coding_tools::deferred_resolution::RegisterError::AlreadyPending => {
                        RuntimeError::Engine(format!(
                            "deferred tool call '{tool_call_id}' is already pending"
                        ))
                    }
                })?;
        deferred_receivers()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key, deferred.receiver);
        Ok(())
    }

    /// Resolve a deferred iOS work item, delivering its terminal payload to
    /// both its deferred waiter & this session's agent inbox.
    pub async fn resolve_inbox_work(
        &self,
        tool_call_id: &str,
        resolution: DeferredResolution,
    ) -> Result<(), RuntimeError> {
        let key = (self.session_id.clone(), tool_call_id.to_string());
        // Taking ownership before awaiting on `engine` makes resolution
        // genuinely single-use even when two iOS callbacks race. The first
        // caller consumes this session-owned capability; every later caller
        // gets the explicit "no pending" error below.
        let Some(_receiver) = deferred_receivers()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&key)
        else {
            return Err(RuntimeError::Engine(format!(
                "no pending deferred tool call '{tool_call_id}'"
            )));
        };

        let result = self
            .engine
            .lock()
            .await
            .resolve_deferred_tool_call(tool_call_id, resolution);
        match result {
            Ok(()) | Err(ResolveError::AlreadyResolvedOrAbandoned) => Ok(()),
            Err(error) => Err(RuntimeError::Engine(error.to_string())),
        }
    }

    /// Concrete iOS-local producer for completion callbacks that do not map
    /// to a tool call. This is intentionally separate from host shell events.
    pub async fn post_local_inbox_message(
        &self,
        channel: &str,
        body: &str,
        data: Option<Value>,
    ) -> Result<u64, RuntimeError> {
        if channel.trim().is_empty() || body.trim().is_empty() {
            return Err(RuntimeError::Config(
                "inbox channel and body must not be empty".into(),
            ));
        }
        self.engine
            .lock()
            .await
            .post_async_inbox_message(channel, body, data)
            .ok_or_else(|| RuntimeError::Engine("session inbox is retired".into()))
    }

    /// Read, but do not acknowledge, a bounded oldest-first inbox prefix.
    pub fn read_async_inbox(&self, limit: usize) -> MobileInboxSnapshot {
        let limit = limit.clamp(1, MAX_QUEUE_LEN);
        let (messages, dropped_total) = async_inbox::read(&self.session_id, limit);
        MobileInboxSnapshot {
            messages: messages.into_iter().map(Into::into).collect(),
            dropped_total,
            pending_count: async_inbox::pending_count(&self.session_id),
        }
    }

    /// Acknowledge every pending message through one read message id.
    pub fn acknowledge_async_inbox(&self, through_id: u64) -> usize {
        async_inbox::acknowledge_through(&self.session_id, through_id)
    }

    /// Session-end cleanup invoked by `host_mobile_runtime_free` before
    /// dropping its handle. It invalidates delayed producers & inbox state.
    pub async fn retire_deferred_inbox(&self) {
        let session_id = self.session_id.clone();
        deferred_receivers()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|(stored_session_id, _), _| stored_session_id != &session_id);
        self.engine.lock().await.retire_runtime_collaboration();
    }
}
