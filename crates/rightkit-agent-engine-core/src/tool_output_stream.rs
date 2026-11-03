//! Rate-bounded live tool output.
//!
//! A running shell/exec tool reports raw chunks through a
//! [`ToolOutputNotifier`]. This coalescer turns them into at most one
//! `toolOutputDelta` protocol event per stream per interval (default 100ms)
//! and caps each event at [`MAX_DELTA_BYTES`], keeping the NEWEST bytes (the
//! live tail). `toolResultAdded` remains the authoritative final output.

use crate::conduit::Conduit;
use rightkit_agent_engine_protocol::{
    now_iso, ProtocolEvent, ToolOutputNotifier, ToolOutputStream,
};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Default coalescing window per stream.
pub const DEFAULT_FLUSH_INTERVAL: Duration = Duration::from_millis(100);
/// Upper bound on one event's text; older bytes are dropped (tail kept).
pub const MAX_DELTA_BYTES: usize = 8 * 1024;

#[derive(Default)]
struct StreamState {
    pending: String,
    last_flush: Option<Instant>,
}

#[derive(Default)]
struct State {
    stdout: StreamState,
    stderr: StreamState,
}

pub struct ToolOutputCoalescer {
    conduit: Arc<Conduit>,
    session_id: String,
    call_id: String,
    interval: Duration,
    state: Mutex<State>,
}

impl ToolOutputCoalescer {
    pub fn new(
        conduit: Arc<Conduit>,
        session_id: impl Into<String>,
        call_id: impl Into<String>,
    ) -> Arc<Self> {
        Self::with_interval(conduit, session_id, call_id, DEFAULT_FLUSH_INTERVAL)
    }

    pub fn with_interval(
        conduit: Arc<Conduit>,
        session_id: impl Into<String>,
        call_id: impl Into<String>,
        interval: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            conduit,
            session_id: session_id.into(),
            call_id: call_id.into(),
            interval,
            state: Mutex::new(State::default()),
        })
    }

    /// Notifier to place on the tool context for this call.
    pub fn notifier(self: &Arc<Self>) -> ToolOutputNotifier {
        let this = Arc::clone(self);
        ToolOutputNotifier::new(move |stream, text| this.push(stream, text))
    }

    /// Buffer a chunk; emit when the stream's window has elapsed.
    pub fn push(&self, stream: ToolOutputStream, text: &str) {
        if text.is_empty() {
            return;
        }
        let ready = {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            let slot = match stream {
                ToolOutputStream::Stdout => &mut state.stdout,
                ToolOutputStream::Stderr => &mut state.stderr,
            };
            slot.pending.push_str(text);
            trim_to_tail(&mut slot.pending);
            let due = slot
                .last_flush
                .is_none_or(|last| last.elapsed() >= self.interval);
            if due {
                slot.last_flush = Some(Instant::now());
                Some(std::mem::take(&mut slot.pending))
            } else {
                None
            }
        };
        if let Some(chunk) = ready {
            self.emit(stream, chunk);
        }
    }

    /// Flush anything still buffered (call when the tool finishes).
    pub fn finish(&self) {
        let (out, err) = {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            (
                std::mem::take(&mut state.stdout.pending),
                std::mem::take(&mut state.stderr.pending),
            )
        };
        if !out.is_empty() {
            self.emit(ToolOutputStream::Stdout, out);
        }
        if !err.is_empty() {
            self.emit(ToolOutputStream::Stderr, err);
        }
    }

    fn emit(&self, stream: ToolOutputStream, text: String) {
        if text.is_empty() {
            return;
        }
        self.conduit.emit(|seq| ProtocolEvent::ToolOutputDelta {
            session_id: self.session_id.clone(),
            seq,
            created_at: now_iso(),
            call_id: self.call_id.clone(),
            stream: stream.as_str().to_string(),
            text,
        });
    }
}

/// Keep only the newest `MAX_DELTA_BYTES`, cutting on a char boundary.
fn trim_to_tail(text: &mut String) {
    if text.len() <= MAX_DELTA_BYTES {
        return;
    }
    let mut start = text.len() - MAX_DELTA_BYTES;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text.drain(..start);
}
