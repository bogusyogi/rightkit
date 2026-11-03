//! Bounded capture of a child's stderr so a crash is diagnosable without a
//! debugger (ScrapeRight keeps a 16 KiB tail; HeardRight redacts each line
//! before it reaches telemetry). Draining stderr is also what keeps a chatty
//! child from blocking on a full pipe.

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Read};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

/// The retained lines and their cost: each line counts its bytes plus one for the newline
/// that joins it to the next.
type Retained = (VecDeque<String>, usize);

/// A background drain of one stderr stream. Keeps the most recent lines whose
/// total size is at most `max_bytes`; older lines fall off the front. A single line that
/// alone exceeds the budget is cut to its last bytes, so [`snapshot`](Self::snapshot) never
/// returns more than `max_bytes` bytes.
pub struct StderrTail {
    lines: Arc<Mutex<Retained>>,
    handle: Option<JoinHandle<()>>,
    /// Set (and signalled) when the drain thread is over, so a bounded wait sleeps on the
    /// condition instead of polling `is_finished`.
    done: Arc<Done>,
}

type Done = (Mutex<bool>, Condvar);

/// Marks the drain over when the thread's body ends, however it ends.
struct DoneGuard(Arc<Done>);

impl Drop for DoneGuard {
    fn drop(&mut self) {
        *self.0 .0.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.0 .1.notify_all();
    }
}

impl StderrTail {
    /// Drain `stderr` on a thread. `map` sees every line first and returns the
    /// text to keep (redaction), or `None` to drop it. `on_line` receives the
    /// kept text as it arrives (forward to logs/telemetry).
    ///
    /// A line longer than four times `max_bytes` cannot be inspected whole, and `map` is a
    /// line-based redactor: handing it the first piece and then the rest as if each were a
    /// line would let a secret that spans the cut through unredacted. So such a line never
    /// reaches `map`. Its bytes are read and discarded up to its newline, and one marker line,
    /// `[stderr line over N bytes dropped]`, takes its place (forwarded to `on_line`, and
    /// retained, without passing through `map`: it holds nothing from the child).
    ///
    /// Best effort: if the drain thread cannot be spawned, this returns a tail that retains
    /// nothing (the error is discarded, and `stderr` is dropped undrained). Use
    /// [`try_capture`](Self::try_capture) to learn that the drain never started.
    pub fn capture<R: Read + Send + 'static>(
        stderr: R,
        max_bytes: usize,
        map: impl Fn(&str) -> Option<String> + Send + 'static,
        on_line: impl Fn(&str) + Send + 'static,
    ) -> Self {
        Self::try_capture(stderr, max_bytes, map, on_line).unwrap_or_else(|_| Self {
            lines: Arc::default(),
            handle: None,
            // No drain thread exists, so it is over from the start.
            done: Arc::new((Mutex::new(true), Condvar::new())),
        })
    }

    /// Like [`capture`](Self::capture), but returns the operating-system error when the drain
    /// thread cannot be spawned, so the caller can fail, retry, or log that stderr is not being
    /// captured. On success the behaviour is identical to `capture`.
    pub fn try_capture<R: Read + Send + 'static>(
        stderr: R,
        max_bytes: usize,
        map: impl Fn(&str) -> Option<String> + Send + 'static,
        on_line: impl Fn(&str) + Send + 'static,
    ) -> io::Result<Self> {
        Self::try_capture_with(
            |name, body| thread::Builder::new().name(name).spawn(body),
            stderr,
            max_bytes,
            map,
            on_line,
        )
    }

    /// `try_capture` with the thread spawner injected, so a test can make the spawn fail.
    fn try_capture_with<R: Read + Send + 'static>(
        spawn: impl FnOnce(String, Box<dyn FnOnce() + Send + 'static>) -> io::Result<JoinHandle<()>>,
        stderr: R,
        max_bytes: usize,
        map: impl Fn(&str) -> Option<String> + Send + 'static,
        on_line: impl Fn(&str) + Send + 'static,
    ) -> io::Result<Self> {
        let lines: Arc<Mutex<Retained>> = Arc::default();
        let shared = Arc::clone(&lines);
        let done: Arc<Done> = Arc::new((Mutex::new(false), Condvar::new()));
        let guard = DoneGuard(Arc::clone(&done));
        let handle = spawn(
            "rightkit-stderr-tail".into(),
            Box::new(move || {
                let _guard = guard;
                let line_cap = max_bytes.max(1).saturating_mul(4);
                let mut reader = BufReader::new(stderr);
                let mut raw = Vec::new();
                let mut skip_lf = false;
                loop {
                    raw.clear();
                    // One byte more than the cap tells a line of exactly the cap from a longer
                    // one. A pathological unterminated line must not grow without bound. A
                    // line ends at `\n`, at `\r`, or at the pair `\r\n` as one terminator:
                    // progress output rewrites its line with bare carriage returns, and each
                    // rewrite is a line the redactor must see whole.
                    let (terminated, stream_ended) =
                        read_line_bounded(&mut reader, &mut raw, line_cap, &mut skip_lf);
                    if raw.is_empty() && !terminated {
                        return; // clean end of stream (or an unreadable one)
                    }
                    let oversized = !terminated && !stream_ended && raw.len() > line_cap;
                    let (kept, ended) = if oversized {
                        let ended = discard_rest_of_line(&mut reader, &mut skip_lf);
                        (
                            Some(format!("[stderr line over {line_cap} bytes dropped]")),
                            ended,
                        )
                    } else {
                        let text = String::from_utf8_lossy(&raw);
                        (map(text.trim_end_matches(['\r', '\n'])), stream_ended)
                    };
                    if let Some(kept) = kept {
                        on_line(&kept);
                        let mut g = shared.lock().unwrap_or_else(|e| e.into_inner());
                        retain(&mut g, kept, max_bytes);
                    }
                    if ended {
                        return;
                    }
                }
            }),
        )?;
        Ok(Self {
            lines,
            handle: Some(handle),
            done,
        })
    }

    /// Capture with no redaction and no forwarding.
    pub fn plain<R: Read + Send + 'static>(stderr: R, max_bytes: usize) -> Self {
        Self::capture(stderr, max_bytes, |l| Some(l.to_string()), |_| {})
    }

    /// The retained lines, oldest first, joined by newlines.
    pub fn snapshot(&self) -> String {
        let g = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        g.0.iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Like [`Self::finish`] but gives up after `timeout` (a grandchild that
    /// inherited stderr can keep the stream open after the child is gone) and
    /// returns what was captured so far.
    pub fn finish_within(&mut self, timeout: std::time::Duration) -> String {
        // Sleeps on the condition the drain thread signals when its body ends: one wake-up,
        // however long the stream stays open.
        {
            let guard = self.done.0.lock().unwrap_or_else(|e| e.into_inner());
            let _ = self
                .done
                .1
                .wait_timeout_while(guard, timeout, |over| !*over)
                .unwrap_or_else(|e| e.into_inner());
        }
        // This call never joins the thread. After the signal the thread still runs the caller's
        // code: the captured `map` and `on_line` are dropped, and then any thread-local
        // destructors those callbacks registered; `JoinHandle::is_finished` can already be true
        // while they run, and a join would wait for them (0.3.3 joined here and could wait for
        // ever on a destructor blocked on a lock the caller holds). The tail is complete without
        // a join: every line was retained before the signal. The handle is kept, so
        // [`Self::finish`] can still join later; if it is never called the thread ends by itself.
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

/// Reads one line into `raw` (terminator included), stopping one byte past `line_cap` so the
/// caller can tell an exactly-cap line from a longer one. A line ends at `\n`, at `\r`, or at
/// `\r\n` consumed as one terminator. Returns `(terminated, stream_ended)`: an unterminated
/// final line at the end of the stream is still returned, with `stream_ended` set. A read
/// error other than `Interrupted` ends the stream like EOF does.
fn read_line_bounded(
    reader: &mut impl BufRead,
    raw: &mut Vec<u8>,
    line_cap: usize,
    skip_lf: &mut bool,
) -> (bool, bool) {
    loop {
        let buffer = match reader.fill_buf() {
            Ok([]) => return (false, true),
            Ok(buffer) => buffer,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return (false, true),
        };
        if std::mem::take(skip_lf) && buffer[0] == b'\n' {
            reader.consume(1);
            continue;
        }
        let room = line_cap.saturating_add(1).saturating_sub(raw.len());
        let scan = &buffer[..buffer.len().min(room)];
        match scan.iter().position(|&byte| byte == b'\n' || byte == b'\r') {
            Some(index) => {
                let carriage = scan[index] == b'\r';
                raw.extend_from_slice(&scan[..=index]);
                reader.consume(index + 1);
                *skip_lf = carriage;
                return (true, false);
            }
            None => {
                let used = scan.len();
                raw.extend_from_slice(scan);
                reader.consume(used);
                if raw.len() > line_cap {
                    return (false, false);
                }
            }
        }
    }
}

/// Reads and drops bytes up to and including the next line terminator (`\n`, `\r`, or the
/// pair `\r\n`). True when the stream ended first.
fn discard_rest_of_line(reader: &mut impl BufRead, skip_lf: &mut bool) -> bool {
    loop {
        let (used, terminator) = match reader.fill_buf() {
            Ok([]) => return true,
            Ok(buffer) => match buffer
                .iter()
                .position(|&byte| byte == b'\n' || byte == b'\r')
            {
                Some(index) => (index + 1, Some(buffer[index])),
                None => (buffer.len(), None),
            },
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return true,
        };
        reader.consume(used);
        match terminator {
            Some(b'\r') => {
                *skip_lf = true;
                return false;
            }
            Some(_) => return false,
            None => {}
        }
    }
}

/// Adds `kept` and drops the oldest lines until the total fits `max_bytes`. A line that alone
/// costs more than the budget is cut to its last bytes (at a character boundary) first, so the
/// bound holds for a single line too.
fn retain(state: &mut Retained, kept: String, max_bytes: usize) {
    let room = max_bytes.saturating_sub(1);
    let kept = if kept.len() > room {
        let mut start = kept.len() - room;
        while !kept.is_char_boundary(start) {
            start += 1;
        }
        kept[start..].to_string()
    } else {
        kept
    };
    let cost = kept.len() + 1;
    if cost > max_bytes {
        // Only a zero budget: nothing, not even an empty line, fits.
        return;
    }
    state.1 += cost;
    state.0.push_back(kept);
    while state.1 > max_bytes {
        match state.0.pop_front() {
            Some(old) => state.1 -= old.len() + 1,
            None => break,
        }
    }
}
