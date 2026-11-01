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

#[cfg(test)]
mod tests {
    use super::*;

    fn deltas(sub: &mut crate::conduit::ConduitSubscription) -> Vec<(String, String)> {
        std::iter::from_fn(|| sub.try_recv())
            .filter_map(|event| match event {
                ProtocolEvent::ToolOutputDelta {
                    stream,
                    text,
                    call_id,
                    ..
                } => {
                    assert_eq!(call_id, "call-1");
                    Some((stream, text))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn chunks_inside_one_window_coalesce_and_finish_flushes_the_tail() {
        let conduit = Arc::new(Conduit::new(64));
        let mut sub = conduit.subscribe();
        let c = ToolOutputCoalescer::with_interval(
            conduit.clone(),
            "s",
            "call-1",
            Duration::from_secs(3600),
        );
        let n = c.notifier();
        n.notify(ToolOutputStream::Stdout, "a\n"); // first chunk is immediate
        n.notify(ToolOutputStream::Stdout, "b\n");
        n.notify(ToolOutputStream::Stdout, "c\n");
        n.notify(ToolOutputStream::Stderr, "e1");
        c.finish();
        assert_eq!(
            deltas(&mut sub),
            vec![
                ("stdout".to_string(), "a\n".to_string()),
                ("stderr".to_string(), "e1".to_string()),
                ("stdout".to_string(), "b\nc\n".to_string()),
            ]
        );
    }

    #[test]
    fn zero_interval_emits_every_chunk_in_order() {
        let conduit = Arc::new(Conduit::new(64));
        let mut sub = conduit.subscribe();
        let c = ToolOutputCoalescer::with_interval(conduit.clone(), "s", "call-1", Duration::ZERO);
        c.push(ToolOutputStream::Stdout, "1");
        c.push(ToolOutputStream::Stdout, "2");
        c.finish();
        assert_eq!(
            deltas(&mut sub),
            vec![
                ("stdout".to_string(), "1".to_string()),
                ("stdout".to_string(), "2".to_string())
            ]
        );
    }

    #[test]
    fn oversized_burst_keeps_the_newest_bytes_on_a_char_boundary() {
        let conduit = Arc::new(Conduit::new(64));
        let mut sub = conduit.subscribe();
        let c = ToolOutputCoalescer::with_interval(
            conduit.clone(),
            "s",
            "call-1",
            Duration::from_secs(3600),
        );
        c.push(ToolOutputStream::Stdout, "first"); // immediate
        c.push(ToolOutputStream::Stdout, &"é".repeat(MAX_DELTA_BYTES)); // 2 bytes each
        c.finish();
        let out = deltas(&mut sub);
        let tail = &out[1].1;
        assert!(tail.len() <= MAX_DELTA_BYTES);
        assert!(tail.chars().all(|ch| ch == 'é'));
    }

    #[test]
    fn empty_chunks_emit_nothing() {
        let conduit = Arc::new(Conduit::new(64));
        let mut sub = conduit.subscribe();
        let c = ToolOutputCoalescer::new(conduit.clone(), "s", "call-1");
        c.push(ToolOutputStream::Stdout, "");
        c.finish();
        assert!(deltas(&mut sub).is_empty());
    }
}
