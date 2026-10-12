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
                loop {
                    raw.clear();
                    // One byte more than the cap tells a line of exactly the cap from a longer
                    // one. A pathological unterminated line must not grow without bound.
                    match reader
                        .by_ref()
                        .take(line_cap as u64 + 1)
                        .read_until(b'\n', &mut raw)
                    {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }
                    let oversized = raw.last() != Some(&b'\n') && raw.len() > line_cap;
                    let (kept, ended) = if oversized {
                        let ended = discard_rest_of_line(&mut reader);
                        (
                            Some(format!("[stderr line over {line_cap} bytes dropped]")),
                            ended,
                        )
                    } else {
                        let text = String::from_utf8_lossy(&raw);
                        (map(text.trim_end_matches(['\r', '\n'])), false)
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

/// Reads and drops bytes up to and including the next newline. True when the stream ended
/// first.
fn discard_rest_of_line(reader: &mut impl BufRead) -> bool {
    loop {
        let (used, found) = match reader.fill_buf() {
            Ok([]) => return true,
            Ok(buffer) => match buffer.iter().position(|&byte| byte == b'\n') {
                Some(index) => (index + 1, true),
                None => (buffer.len(), false),
            },
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return true,
        };
        reader.consume(used);
        if found {
            return false;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A redactor like the line-based ones the apps pass: everything from `key=` on goes.
    fn redact(line: &str) -> Option<String> {
        Some(match line.split_once("key=") {
            Some((head, _)) => format!("{head}key=[redacted]"),
            None => line.to_string(),
        })
    }

    fn capture_all(input: Vec<u8>, max_bytes: usize) -> (String, Vec<String>, Vec<String>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let forwarded = Arc::new(Mutex::new(Vec::new()));
        let (map_seen, on_line_seen) = (Arc::clone(&seen), Arc::clone(&forwarded));
        let mut tail = StderrTail::capture(
            Cursor::new(input),
            max_bytes,
            move |line| {
                map_seen.lock().unwrap().push(line.to_string());
                redact(line)
            },
            move |line| on_line_seen.lock().unwrap().push(line.to_string()),
        );
        let snapshot = tail.finish();
        let seen = seen.lock().unwrap().clone();
        let forwarded = forwarded.lock().unwrap().clone();
        (snapshot, seen, forwarded)
    }

    /// Comparative review, 2026-10-12: the bounded finish joined as soon as the drain body was
    /// over, but the callback is dropped after that, and its destructor here waits on a lock
    /// the caller holds. 0.3.3 returned at the deadline; so must this.
    #[test]
    fn finish_within_returns_while_the_callback_is_still_being_dropped() {
        struct WaitsOnDrop(Arc<Mutex<()>>);
        impl Drop for WaitsOnDrop {
            fn drop(&mut self) {
                drop(self.0.lock().unwrap_or_else(|e| e.into_inner()));
            }
        }
        let gate = Arc::new(Mutex::new(()));
        let held = gate.lock().unwrap();
        let captured = WaitsOnDrop(Arc::clone(&gate));
        let mut tail = StderrTail::capture(
            Cursor::new(b"kept\n".to_vec()),
            64,
            |l| Some(l.to_string()),
            move |_| {
                let _ = &captured;
            },
        );
        let started = std::time::Instant::now();
        let snapshot = tail.finish_within(std::time::Duration::from_millis(100));
        assert_eq!(snapshot, "kept");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "finish_within waited for the callback's destructor"
        );
        // Released: the thread ends and an unbounded finish joins it.
        drop(held);
        assert_eq!(tail.finish(), "kept");
    }

    #[test]
    fn try_capture_surfaces_a_spawn_failure_and_capture_degrades_to_an_empty_tail() {
        let failed = StderrTail::try_capture_with(
            |_, _| Err(io::Error::new(io::ErrorKind::WouldBlock, "no threads")),
            Cursor::new(b"lost\n".to_vec()),
            64,
            |l| Some(l.to_string()),
            |_| {},
        );
        let error = failed.err().expect("the spawn error must reach the caller");
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        // The public constructor reports success as an `io::Result` and still captures.
        let mut tail = StderrTail::try_capture(
            Cursor::new(b"one\ntwo\n".to_vec()),
            64,
            |l| Some(l.to_string()),
            |_| {},
        )
        .expect("spawn succeeds");
        assert_eq!(tail.finish(), "one\ntwo");
    }

    /// `max_bytes = 8` gives a 32-byte line cap. The secret line is 64 bytes, so the old code
    /// cut it after 32 bytes and presented both halves to the redactor as separate lines: the
    /// second half, `SSSS...`, holds no `key=` and went through unredacted.
    #[test]
    fn the_continuation_of_an_oversized_line_never_reaches_the_redactor_or_the_sinks() {
        let secret = "S".repeat(60);
        let input = format!("key={secret}\nafter\n").into_bytes();
        let (snapshot, seen, forwarded) = capture_all(input, 8);
        assert_eq!(
            seen,
            vec!["after".to_string()],
            "the redactor saw a fragment"
        );
        assert!(
            !forwarded.iter().any(|line| line.contains('S')),
            "forwarded: {forwarded:?}"
        );
        assert!(!snapshot.contains('S'), "retained: {snapshot:?}");
        assert!(
            snapshot.len() <= 8,
            "{} bytes retained: {snapshot:?}",
            snapshot.len()
        );
        // The marker stands in for the dropped line, and the line after it is intact.
        assert_eq!(forwarded.len(), 2, "{forwarded:?}");
        assert!(forwarded[0].starts_with("[stderr line over 32 bytes dropped]"));
        assert_eq!(forwarded[1], "after");
    }

    #[test]
    fn an_oversized_line_is_drained_across_many_reads_and_the_next_line_survives() {
        // Far longer than the reader's buffer, so the discard loop runs many times.
        let mut input = vec![b'x'; 200_000];
        input.extend_from_slice(b"\nfinal\n");
        let (snapshot, seen, _) = capture_all(input, 2048);
        assert_eq!(seen, vec!["final".to_string()]);
        assert!(snapshot.ends_with("final"), "{snapshot:?}");
        assert!(snapshot.len() <= 2048);
    }

    #[test]
    fn an_oversized_line_that_ends_the_stream_is_dropped_without_hanging() {
        let (snapshot, seen, forwarded) = capture_all(vec![b'x'; 5000], 100);
        assert!(seen.is_empty(), "{seen:?}");
        assert_eq!(forwarded.len(), 1);
        assert!(snapshot.len() <= 100);
    }

    #[test]
    fn a_line_of_exactly_the_cap_is_inspected_and_one_byte_more_is_not() {
        // max_bytes = 8: the cap is 32.
        let exact = "a".repeat(32);
        let over = "b".repeat(33);
        let input = format!("{exact}\n{over}\n").into_bytes();
        let (_, seen, forwarded) = capture_all(input, 8);
        assert_eq!(seen, vec![exact]);
        assert_eq!(forwarded.len(), 2);
        assert!(forwarded[1].starts_with("[stderr line over"));
        // A final line with no newline that fits the cap is still a line.
        let (_, seen, _) = capture_all(b"tail with no newline".to_vec(), 8);
        assert_eq!(seen, vec!["tail with no newline".to_string()]);
    }

    /// One line, within the cap but larger than the budget: the old code kept it whole.
    #[test]
    fn one_entry_larger_than_the_budget_is_cut_to_its_last_bytes() {
        let (snapshot, _, _) = capture_all(format!("{}END\n", "a".repeat(50)).into_bytes(), 16);
        assert!(
            snapshot.len() <= 16,
            "{} bytes: {snapshot:?}",
            snapshot.len()
        );
        assert!(
            snapshot.ends_with("END"),
            "the newest bytes are the ones kept: {snapshot:?}"
        );
    }

    #[test]
    fn a_cut_never_splits_a_character() {
        // Each `é` is two bytes; 20 of them are 40 bytes. A 16-byte budget leaves 15 for text.
        let (snapshot, _, _) = capture_all(format!("{}\n", "é".repeat(20)).into_bytes(), 16);
        assert!(snapshot.len() <= 16);
        assert!(!snapshot.is_empty());
        assert!(snapshot.chars().all(|c| c == 'é'), "{snapshot:?}");
    }

    #[test]
    fn the_total_stays_within_the_budget_over_many_lines() {
        let input: String = (0..500).map(|n| format!("line-{n}\n")).collect();
        let (snapshot, _, _) = capture_all(input.into_bytes(), 40);
        assert!(snapshot.len() <= 40, "{} bytes", snapshot.len());
        assert!(snapshot.ends_with("line-499"), "{snapshot:?}");
    }

    #[test]
    fn a_zero_budget_retains_nothing_and_still_drains() {
        let (snapshot, seen, _) = capture_all(b"one\ntwo\n".to_vec(), 0);
        assert_eq!(snapshot, "");
        assert_eq!(seen, vec!["one".to_string(), "two".to_string()]);
    }
}
