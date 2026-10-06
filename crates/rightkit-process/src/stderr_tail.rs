//! Bounded capture of a child's stderr so a crash is diagnosable without a
//! debugger (ScrapeRight keeps a 16 KiB tail; HeardRight redacts each line
//! before it reaches telemetry). Draining stderr is also what keeps a chatty
//! child from blocking on a full pipe.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

/// A background drain of one stderr stream. Keeps the most recent lines whose
/// total size is at most `max_bytes`; older lines fall off the front.
pub struct StderrTail {
    lines: Arc<Mutex<(VecDeque<String>, usize)>>,
    handle: Option<JoinHandle<()>>,
}

impl StderrTail {
    /// Drain `stderr` on a thread. `map` sees every line first and returns the
    /// text to keep (redaction), or `None` to drop it. `on_line` receives the
    /// kept text as it arrives (forward to logs/telemetry).
    pub fn capture<R: Read + Send + 'static>(
        stderr: R,
        max_bytes: usize,
        map: impl Fn(&str) -> Option<String> + Send + 'static,
        on_line: impl Fn(&str) + Send + 'static,
    ) -> Self {
        let lines: Arc<Mutex<(VecDeque<String>, usize)>> = Arc::default();
        let shared = Arc::clone(&lines);
        let handle = thread::Builder::new().name("rightkit-stderr-tail".into()).spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut raw = Vec::new();
            loop {
                raw.clear();
                // A pathological unterminated line must not grow without bound.
                match reader.by_ref().take(max_bytes.max(1) as u64 * 4).read_until(b'\n', &mut raw) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {}
                }
                let text = String::from_utf8_lossy(&raw);
                let Some(kept) = map(text.trim_end_matches(['\r', '\n'])) else { continue };
                on_line(&kept);
                let mut g = shared.lock().unwrap_or_else(|e| e.into_inner());
                g.1 += kept.len() + 1;
                g.0.push_back(kept);
                while g.1 > max_bytes && g.0.len() > 1 {
                    if let Some(old) = g.0.pop_front() {
                        g.1 -= old.len() + 1;
                    }
                }
            }
        });
        Self { lines, handle: handle.ok() }
    }

    /// Capture with no redaction and no forwarding.
    pub fn plain<R: Read + Send + 'static>(stderr: R, max_bytes: usize) -> Self {
        Self::capture(stderr, max_bytes, |l| Some(l.to_string()), |_| {})
    }

    /// The retained lines, oldest first, joined by newlines.
    pub fn snapshot(&self) -> String {
        let g = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        g.0.iter().map(String::as_str).collect::<Vec<_>>().join("\n")
    }

    /// Like [`Self::finish`] but gives up after `timeout` (a grandchild that
    /// inherited stderr can keep the stream open after the child is gone) and
    /// returns what was captured so far.
    pub fn finish_within(&mut self, timeout: std::time::Duration) -> String {
        let deadline = std::time::Instant::now() + timeout;
        while self.handle.as_ref().is_some_and(|h| !h.is_finished()) && std::time::Instant::now() < deadline {
            thread::sleep(std::time::Duration::from_millis(5));
        }
        if self.handle.as_ref().is_some_and(|h| h.is_finished()) {
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
        self.snapshot()
    }

    /// Wait for the stream to end (the child exited) so the tail is complete.
    pub fn finish(&mut self) -> String {
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        self.snapshot()
    }
}
