//! ENG-86 — deferred tool-call resolution.
//!
//! A tool call may need to report its result *after* the streaming loop that
//! issued it has already moved on: a long-running external job, a human
//! approval that lands out of band, or a callback from another process.
//! [`DeferredToolCallRegistry`] gives such a tool a place to park a
//! `tool_call_id` and a single-use resolver, following the exact pattern
//! `engine_tool_dispatch_approval.rs` already uses for `pending_approvals`
//! (a `tokio::sync::oneshot` keyed by call id, guarded by a `Mutex` over the
//! map). That file is read-only reference here — this module is the
//! tools-crate-side primitive a `Tool` implementation calls into; the engine
//! decides how a deferred call is awaited/timed out on its side, exactly as
//! it already owns `APPROVAL_WAIT_TIMEOUT` for approvals.
//!
//! ## Mechanism decision (ENG-86 vs ENG-99)
//!
//! ENG-86 (defer *this* call's result) and ENG-99 (an async inbox decoupled
//! from the turn) are adjacent — both detach a result from the turn that
//! requested it — but they are not the same shape and are kept as two
//! distinct types rather than forced into one generic mechanism:
//!
//! - A deferred call is **single-use**: exactly one `tool_call_id` gets
//!   exactly one resolution, and the resolver is consumed on first use (the
//!   oneshot pattern). It answers "what did *this specific call* return."
//! - An inbox entry (`async_inbox.rs`) is **queue-shaped**: a session can
//!   accumulate zero or many messages between turns, and the `async_inbox`
//!   tool drains them. It answers "what messages/events have arrived since
//!   I last checked," with no 1:1 call correspondence.
//!
//! They do compose: a slow tool can register a [`DeferredToolCall`] here for
//! its own eventual return value, *and* have its completion callback also
//! post a summary into the session's [`crate::coding_tools::async_inbox::AsyncInbox`] so
//! the agent notices the resolution even if it never re-checks the specific
//! `tool_call_id`. `async_inbox.rs` depends on this module's `ResolutionKind`
//! only for that composition; it does not depend on the registry itself.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::Value;
use tokio::sync::oneshot;

/// The outcome a deferred call resolves to. Mirrors the shape of
/// [`crate::coding_tools::tool_trait::ToolResult`] closely enough to build one from, but
/// stays serialize-friendly (`ToolResult` is not `Serialize`) since a
/// resolution may cross a process boundary (external job callback, IPC from
/// the daemon) before it reaches this in-process registry.
#[derive(Debug, Clone)]
pub struct DeferredResolution {
    pub content: String,
    pub is_error: bool,
    /// Optional structured payload alongside `content`, for a resolver that
    /// wants to hand back JSON rather than a pre-rendered string.
    pub data: Option<Value>,
}

impl DeferredResolution {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            data: None,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            data: None,
        }
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

/// A single pending deferred tool call: the receiving half plus a flag the
/// registry uses to reject a second resolution attempt cheaply, without
/// waiting for `oneshot::Sender::send` to fail on a dropped receiver.
struct PendingEntry {
    sender: oneshot::Sender<DeferredResolution>,
}

/// Handle returned to whoever registered a deferred call. Holding it does
/// not by itself guarantee single delivery — that guarantee comes from the
/// registry consuming the map entry's `oneshot::Sender` on first
/// `resolve()`, exactly like `pending_approvals` in
/// `engine_tool_dispatch_approval.rs` consumes its `tx` on
/// `.remove(&call.id)`. A second `resolve()` for the same id, or a
/// `resolve()` after the receiver already timed out/was dropped, both
/// return `Err` and land no effect.
pub struct DeferredToolCall {
    pub tool_call_id: String,
    pub receiver: oneshot::Receiver<DeferredResolution>,
}

/// Error returned when a `tool_call_id` cannot be resolved: either nothing
/// was ever registered under that id, or it was already resolved once
/// (single-use consumed) or the waiting side went away (receiver dropped,
/// e.g. after a timeout on the engine's side).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    NotFound,
    AlreadyResolvedOrAbandoned,
}

/// A second registration cannot replace a live call's waiter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterError {
    AlreadyPending,
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::NotFound => write!(f, "no pending deferred call with this id"),
            ResolveError::AlreadyResolvedOrAbandoned => {
                write!(f, "deferred call already resolved or its waiter is gone")
            }
        }
    }
}

/// Per-session (or per-engine, callers choose the scope by which instance
/// they hold) registry of in-flight deferred tool calls, keyed by
/// `tool_call_id`. Single-use per id: `try_register` inserts once, `resolve`
/// removes-and-consumes once. A second registration for a pending id fails,
/// so two calls never share one id's fate or lose an existing waiter.
#[derive(Default)]
pub struct DeferredToolCallRegistry {
    pending: Mutex<HashMap<String, PendingEntry>>,
}

impl DeferredToolCallRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of calls currently awaiting resolution. For tests and
    /// diagnostics only.
    pub fn pending_count(&self) -> usize {
        self.pending.lock().unwrap().len()
    }

    /// Park a new deferred call under `tool_call_id`. Returns the
    /// [`DeferredToolCall`] handle whose `receiver` completes exactly once,
    /// when (and if) `resolve` is called with the same id.
    ///
    /// Register one unique pending call. Duplicate ids fail rather than
    /// silently abandoning an existing receiver.
    pub fn try_register(
        &self,
        tool_call_id: impl Into<String>,
    ) -> Result<DeferredToolCall, RegisterError> {
        let tool_call_id = tool_call_id.into();
        let (sender, receiver) = oneshot::channel();
        let mut pending = self.pending.lock().unwrap();
        if pending.contains_key(&tool_call_id) {
            return Err(RegisterError::AlreadyPending);
        }
        pending.insert(tool_call_id.clone(), PendingEntry { sender });
        Ok(DeferredToolCall {
            tool_call_id,
            receiver,
        })
    }

    /// Compatibility helper for callers that already guarantee uniqueness.
    pub fn register(&self, tool_call_id: impl Into<String>) -> DeferredToolCall {
        self.try_register(tool_call_id)
            .expect("deferred tool call id must be unique while pending")
    }

    /// Resolve the deferred call registered under `tool_call_id`, consuming
    /// it. Single-use: the entry is removed from the map before the send is
    /// attempted, so a concurrent second `resolve()` for the same id always
    /// observes `NotFound`, never a race on "who gets to send."
    pub fn resolve(
        &self,
        tool_call_id: &str,
        resolution: DeferredResolution,
    ) -> Result<(), ResolveError> {
        let entry = {
            let mut guard = self.pending.lock().unwrap();
            guard.remove(tool_call_id)
        };
        match entry {
            None => Err(ResolveError::NotFound),
            Some(entry) => entry
                .sender
                .send(resolution)
                .map_err(|_| ResolveError::AlreadyResolvedOrAbandoned),
        }
    }

    /// Drop a pending entry without resolving it (e.g. the engine timed out
    /// waiting and wants the map entry gone). The receiver then observes a
    /// closed channel on await, same as a natural `resolve` race loss.
    pub fn abandon(&self, tool_call_id: &str) {
        self.pending.lock().unwrap().remove(tool_call_id);
    }

    /// Discard every unresolved call during session retirement.
    pub fn abandon_all(&self) {
        self.pending.lock().unwrap().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolve_delivers_to_the_registered_receiver() {
        let registry = DeferredToolCallRegistry::new();
        let deferred = registry.register("call-1");
        assert_eq!(registry.pending_count(), 1);

        registry
            .resolve("call-1", DeferredResolution::ok("done"))
            .expect("resolve should succeed for a pending id");

        let resolution = deferred.receiver.await.expect("receiver should complete");
        assert_eq!(resolution.content, "done");
        assert!(!resolution.is_error);
        assert_eq!(registry.pending_count(), 0);
    }

    #[tokio::test]
    async fn resolve_is_single_use_second_attempt_fails() {
        let registry = DeferredToolCallRegistry::new();
        let _deferred = registry.register("call-1");

        registry
            .resolve("call-1", DeferredResolution::ok("first"))
            .expect("first resolve should succeed");

        let second = registry.resolve("call-1", DeferredResolution::ok("second"));
        assert_eq!(second, Err(ResolveError::NotFound));
    }

    #[test]
    fn resolve_unknown_id_returns_not_found() {
        let registry = DeferredToolCallRegistry::new();
        let result = registry.resolve("never-registered", DeferredResolution::ok("x"));
        assert_eq!(result, Err(ResolveError::NotFound));
    }

    #[tokio::test]
    async fn dropped_receiver_makes_resolve_report_abandoned() {
        let registry = DeferredToolCallRegistry::new();
        let deferred = registry.register("call-1");
        drop(deferred.receiver);

        let result = registry.resolve("call-1", DeferredResolution::ok("late"));
        assert_eq!(result, Err(ResolveError::AlreadyResolvedOrAbandoned));
    }

    #[test]
    fn abandon_removes_pending_entry_without_resolving() {
        let registry = DeferredToolCallRegistry::new();
        let _deferred = registry.register("call-1");
        assert_eq!(registry.pending_count(), 1);

        registry.abandon("call-1");
        assert_eq!(registry.pending_count(), 0);
        assert_eq!(
            registry.resolve("call-1", DeferredResolution::ok("x")),
            Err(ResolveError::NotFound)
        );
    }

    #[tokio::test]
    async fn concurrent_resolve_attempts_only_one_wins() {
        use std::sync::Arc;

        let registry = Arc::new(DeferredToolCallRegistry::new());
        let deferred = registry.register("call-1");

        let r1 = Arc::clone(&registry);
        let r2 = Arc::clone(&registry);
        let h1 = tokio::spawn(async move { r1.resolve("call-1", DeferredResolution::ok("a")) });
        let h2 = tokio::spawn(async move { r2.resolve("call-1", DeferredResolution::ok("b")) });

        let (res1, res2) = tokio::join!(h1, h2);
        let outcomes = [res1.unwrap(), res2.unwrap()];
        let ok_count = outcomes.iter().filter(|r| r.is_ok()).count();
        let not_found_count = outcomes
            .iter()
            .filter(|r| *r == &Err(ResolveError::NotFound))
            .count();
        assert_eq!(ok_count, 1, "exactly one concurrent resolve should win");
        assert_eq!(not_found_count, 1);

        let resolution = deferred.receiver.await.expect("receiver should complete");
        assert!(resolution.content == "a" || resolution.content == "b");
    }
}
