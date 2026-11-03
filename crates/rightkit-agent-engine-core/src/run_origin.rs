//! Pre-effect durable origin seam (Lane C3 / C3 task 1).
//!
//! The engine must persist a typed `ToolCallStarted` origin record BEFORE
//! executing the tool side effect. This guarantees that a tool never makes
//! a workspace or network mutation without an originating durable artifact
//! — replay/recovery can therefore find the call by stable id, and the
//! storage failure modes surface to the engine instead of being silently
//! swallowed.
//!
//! The engine crate does NOT depend on `rightkit-agent-engine-storage`; instead it
//! speaks to a [`RunOriginSink`] trait, and the daemon binary wires a
//! concrete implementation in (see `crates/host-app/src/api_state_*`).
//! This file is the abstraction, the invariants, and the test scaffolding.
//!
//! ## Failure model
//!
//! `record_run_origin` returns an [`OriginWriteResult`]:
//!
//! - `Persisted(seq)` — durable write committed; engine may proceed to the
//!   tool side effect.
//! - `Deferred(reason)` — sink asked the engine to retry later (typical for
//!   a queued replay during a daemon restart). Engine records the deferral
//!   and does NOT execute yet.
//! - `Failed(reason)` — write did not commit; the engine aborts dispatch
//!   and emits a `RunGovernanceRejected` event with `kind = OriginMissing`.
//!
//! The contract: a tool's side effect ONLY runs after a `Persisted` result
//! (or an explicit `Skipped` by the sink), and the pre-effect dispatch in
//! `engine_tool_dispatch_single.rs` enforces it.

use rightkit_agent_engine_protocol::ToolCall;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use std::time::Duration;

/// What the durable origin seam returned for one tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OriginWriteResult {
    /// The persistence seam committed the record. `seq` is the durable event
    /// sequence number assigned by storage; pass it through to the
    /// post-effect code so the matching `ToolResultAdded` can correlate by
    /// the same origin.
    Persisted(i64),
    /// The seam explicitly asked the engine not to execute yet (e.g. a
    /// transient retry). Engine keeps the call pending and the dispatch
    /// loop retries on the next turn slot.
    Deferred(String),
    /// The persistence call failed; engine MUST refuse the side effect.
    Failed(String),
    /// The sink explicitly opted this call OUT of the durable origin seam
    /// (e.g. the call is the in-band `noop` probe used to test the loop).
    /// Equivalent to `Persisted(0)` for engine purposes.
    Skipped,
}

impl OriginWriteResult {
    pub fn is_safe_to_proceed(&self) -> bool {
        matches!(
            self,
            OriginWriteResult::Persisted(_) | OriginWriteResult::Skipped
        )
    }

    pub fn origin_seq(&self) -> Option<i64> {
        match self {
            OriginWriteResult::Persisted(seq) => Some(*seq),
            _ => None,
        }
    }
}

impl fmt::Display for OriginWriteResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OriginWriteResult::Persisted(seq) => write!(f, "Persisted(seq={seq})"),
            OriginWriteResult::Deferred(r) => write!(f, "Deferred({r})"),
            OriginWriteResult::Failed(r) => write!(f, "Failed({r})"),
            OriginWriteResult::Skipped => write!(f, "Skipped"),
        }
    }
}

/// Storage-side hook the engine uses to durably record a tool call before
/// its side effect. The bin wires a SQLite-backed implementation; tests
/// provide an in-memory mock.
pub trait RunOriginSink: Send + Sync {
    /// Persist the `ToolCallStarted` origin record for `call` and return
    /// the durable outcome. The implementation MUST block until storage
    /// has acknowledged the write (or returned an error) — asynchronous
    /// best-effort sinks violate the contract.
    fn record_origin(&self, call: &ToolCall) -> OriginWriteResult;

    /// Persist the exact canonical event identity that will be published.
    /// Legacy/test sinks may keep implementing `record_origin`; durable sinks
    /// override this method so the pre-effect row and live event share one seq
    /// and one payload instead of racing two independent appends.
    fn record_event(
        &self,
        event: &rightkit_agent_engine_protocol::ProtocolEvent,
    ) -> OriginWriteResult {
        match event {
            rightkit_agent_engine_protocol::ProtocolEvent::ToolCallStarted { call, .. } => {
                self.record_origin(call)
            }
            _ => OriginWriteResult::Failed("run origin event was not ToolCallStarted".into()),
        }
    }

    /// Async canonical-event persistence used by the live dispatch path.
    ///
    /// The compatibility default preserves existing lightweight test sinks.
    /// Durable sinks that need an async mutex or retry delay override this
    /// method; dispatch always awaits it before allowing the side effect.
    fn record_event_async<'a>(
        &'a self,
        event: &'a rightkit_agent_engine_protocol::ProtocolEvent,
    ) -> Pin<Box<dyn Future<Output = OriginWriteResult> + Send + 'a>> {
        Box::pin(async move { self.record_event(event) })
    }

    /// Return `true` when the sink has been wired and is reachable
    /// (typically a heartbeat/ping). On `false`, the engine returns an
    /// `OriginWriteResult::Deferred` to drive an explicit retry.
    fn is_ready(&self) -> bool;
}

/// Default no-op sink (used when the daemon has not yet wired storage).
/// Records nothing; the engine raises a deterministic
/// `OriginWriteResult::Failed` so C3 tests can prove the dispatch was
/// refused on a missing sink.
#[derive(Debug, Default, Clone)]
pub struct NoopOriginSink;

impl RunOriginSink for NoopOriginSink {
    fn record_origin(&self, _call: &ToolCall) -> OriginWriteResult {
        OriginWriteResult::Failed("no origin sink wired".into())
    }

    fn is_ready(&self) -> bool {
        false
    }
}

/// In-memory sink for tests. Records every accepted `(call_id, call_name)`
/// in the order they were written, plus an optional fault to inject.
#[derive(Debug, Default)]
pub struct InMemoryOriginSink {
    inner: Mutex<InMemoryOriginSinkInner>,
}

#[derive(Debug, Default)]
struct InMemoryOriginSinkInner {
    writes: Vec<(String, String)>,
    failures: Vec<OriginWriteResult>,
    force_outcome: Option<OriginWriteResult>,
    force_outcome_calls: usize,
    ready: bool,
    last_seq: i64,
}

impl InMemoryOriginSink {
    /// Build a sink that commits every record, never injects failures,
    /// and assigns monotonically increasing `seq` from 1.
    pub fn always_ready() -> Self {
        let me = Self::default();
        me.inner.lock().unwrap().ready = true;
        me
    }

    /// Build a sink that injects the given sequence of failures on the first
    /// N calls, then settles back into always-ready mode. Tests use this to
    /// model transient storage outages.
    pub fn with_failure_sequence(failures: Vec<OriginWriteResult>) -> Self {
        let me = Self::default();
        {
            let mut inner = me.inner.lock().unwrap();
            inner.ready = true;
            inner.failures = failures;
        }
        me
    }

    /// Force every call to return the same outcome, useful for deterministic
    /// persistence-failure tests.
    pub fn force_outcome(outcome: OriginWriteResult) -> Self {
        let me = Self::default();
        {
            let mut inner = me.inner.lock().unwrap();
            inner.ready = true;
            inner.force_outcome = Some(outcome);
            inner.force_outcome_calls = 0;
        }
        me
    }

    /// Marks the sink as not-ready (so `is_ready` returns `false`).
    pub fn with_not_ready() -> Self {
        let me = Self::default();
        me.inner.lock().unwrap().ready = false;
        me
    }

    /// Returns the recorded writes in order.
    pub fn recorded(&self) -> Vec<(String, String)> {
        self.inner.lock().unwrap().writes.clone()
    }

    /// Number of durable writes this sink committed.
    pub fn write_count(&self) -> usize {
        self.inner.lock().unwrap().writes.len()
    }
}

impl RunOriginSink for InMemoryOriginSink {
    fn record_origin(&self, call: &ToolCall) -> OriginWriteResult {
        let mut inner = self.inner.lock().unwrap();
        if !inner.ready {
            return OriginWriteResult::Deferred("sink not ready".into());
        }
        if let Some(outcome) = inner.force_outcome.clone() {
            inner.force_outcome_calls = inner.force_outcome_calls.saturating_add(1);
            return outcome;
        }
        if !inner.failures.is_empty() {
            return inner.failures.remove(0);
        }
        inner.last_seq += 1;
        let seq = inner.last_seq;
        inner.writes.push((call.id.clone(), call.name.clone()));
        OriginWriteResult::Persisted(seq)
    }

    fn is_ready(&self) -> bool {
        self.inner.lock().unwrap().ready
    }
}

/// Stable hash of the tool call arguments. The origin record carries this so
/// the post-edit diagnostic step can correlate "intent shown in
/// `ToolCallStarted`" with "actual mutation reported in `ToolResultAdded`"
/// without trusting either side.
pub fn stable_call_fingerprint(call: &ToolCall) -> String {
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    hasher.update(call.name.as_bytes());
    hasher.update(b"\0");
    hasher.update(call.arguments_json.as_bytes());
    let digest = hasher.finalize_bytes();
    let mut out = String::with_capacity(16);
    for byte in &digest[..8] {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// What the engine should do next. Encoded so the dispatch site can switch
/// on it instead of inspecting the underlying `OriginWriteResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreEffectDecision {
    Execute,
    DeferUntilReady,
    Abort,
}

pub fn decide_after_origin(result: &OriginWriteResult) -> PreEffectDecision {
    match result {
        OriginWriteResult::Persisted(_) | OriginWriteResult::Skipped => PreEffectDecision::Execute,
        OriginWriteResult::Deferred(_) => PreEffectDecision::DeferUntilReady,
        OriginWriteResult::Failed(_) => PreEffectDecision::Abort,
    }
}

/// Convenience: returns true when the call is the in-band `noop` probe and
/// the engine should bypass the origin seam.
pub fn is_noop_probe(call: &ToolCall) -> bool {
    call.name == "noop" || call.name.is_empty()
}

/// Latency budget used when the bin asks the storage seam to record an
/// origin. The default is generous — the call is a single SQLite INSERT
/// inside a daemon mutex.
pub const ORIGIN_WRITE_BUDGET: Duration = Duration::from_millis(2_500);
