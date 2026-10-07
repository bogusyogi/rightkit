//! Structured lifecycle events (start / stop / crash / denied) for the host's
//! journal. Content-free: never tool arguments, results, tokens, or session
//! headers. Nothing is uploaded; the host decides where events go.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum LifecycleKind {
    /// A client connection opened (stdio child spawned / HTTP initialized) or a server host began serving.
    Start,
    /// Intentional close or end of input.
    Stop,
    /// A stdio server process died, or a serving loop ended, without being asked to.
    Crash,
    /// A trust gate, admission hook, or HTTP gate refused something before any side effect.
    Denied,
}

impl LifecycleKind {
    pub fn label(self) -> &'static str {
        match self {
            LifecycleKind::Start => "start",
            LifecycleKind::Stop => "stop",
            LifecycleKind::Crash => "crash",
            LifecycleKind::Denied => "denied",
        }
    }
}

#[derive(Clone, Debug)]
pub struct McpEvent {
    pub kind: LifecycleKind,
    /// Local id of the client connection (`mcpc-N`), pool (`mcpp-N`), or serving run (`mcps-N`).
    /// Never the protocol `Mcp-Session-Id`.
    pub session_id: String,
    pub reason: String,
    pub at: SystemTime,
    /// Server name (client side) or serving host name (server side).
    pub server: Option<String>,
    /// OS process id of a stdio server child.
    pub pid: Option<u32>,
    /// Tool name for admission denials.
    pub tool: Option<String>,
    /// `stdio` or `http`.
    pub transport: Option<&'static str>,
}

impl McpEvent {
    pub fn new(kind: LifecycleKind, session_id: &str, reason: impl Into<String>) -> Self {
        Self {
            kind,
            session_id: session_id.to_string(),
            reason: reason.into(),
            at: SystemTime::now(),
            server: None,
            pid: None,
            tool: None,
            transport: None,
        }
    }

    pub fn timestamp_ms(&self) -> u64 {
        self.at
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "event": self.kind.label(),
            "session_id": self.session_id,
            "reason": self.reason,
            "ts_ms": self.timestamp_ms(),
            "server": self.server,
            "pid": self.pid,
            "tool": self.tool,
            "transport": self.transport,
        })
    }
}

pub type EventSink = Arc<dyn Fn(&McpEvent) + Send + Sync>;

static SEQ: AtomicU64 = AtomicU64::new(1);

pub(crate) fn next_id(prefix: &str) -> String {
    format!("{prefix}-{}", SEQ.fetch_add(1, Ordering::Relaxed))
}

/// Emit through an optional sink, building the event only when someone listens.
pub(crate) fn emit(sink: &Option<EventSink>, f: impl FnOnce() -> McpEvent) {
    if let Some(s) = sink {
        s(&f());
    }
}
