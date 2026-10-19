//! [`run_streaming`]: the cancel-aware streaming runner that apps used to hand-roll around
//! [`OwnedCommand`](crate::OwnedCommand). Output is shown while the child runs, the caller can
//! cancel, and nothing is retained except bounded tails.

use super::{StdinWriter, DRAIN_WAIT};
use crate::{NativeOutputCapture, OwnedCommand, ReapOutcome};
use std::{
    io,
    process::ExitStatus,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError, SyncSender, TrySendError},
    },
    time::{Duration, Instant},
};

/// How long the killed tree's leader gets to be reaped before the call fails with `TimedOut`
/// (a process in an uninterruptible state cannot be reaped by any means).
const KILL_REAP_WAIT: Duration = Duration::from_secs(5);

/// The longest a loop iteration waits for output before it looks at the child, the cancel
/// predicate and the deadline again. `StreamOptions::poll` shortens it, never lengthens it.
const MAX_STEP: Duration = Duration::from_millis(25);

/// Chunks that may be queued between the reader threads and the calling thread. A caller whose
/// callbacks are slower than the child makes the child wait on the pipe (back-pressure), not
/// the process grow.
const QUEUE_CHUNKS: usize = 64;

/// A stderr line longer than this is not delivered: one marker line,
/// `[stderr line over N bytes dropped]`, takes its place (the rule of
/// [`StderrTail`](crate::StderrTail), with a fixed limit of four times its default 64 KiB tail).
const LINE_CAP: usize = 256 * 1024;

/// Limits and options for [`run_streaming`]. Build one with [`StreamOptions::new`] (or
/// `Default`) and the builder methods; the struct is `#[non_exhaustive]`, so later options do
/// not break callers.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct StreamOptions {
    /// How long the program may run before its tree is terminated. `None` (the default) is no
    /// deadline: only the child's exit or the cancel predicate ends the call.
    pub timeout: Option<Duration>,
    /// The longest the call goes without evaluating the cancel predicate and the deadline
    /// (default 100 ms). The predicate is evaluated at least this often, however much output
    /// is flowing. Values under 1 ms behave as 1 ms.
    pub poll: Duration,
    /// Most bytes of stdout kept in [`StreamOutput::stdout_tail`]: the last ones (default
    /// 64 KiB).
    pub stdout_tail_cap: usize,
    /// Most bytes of stderr kept in [`StreamOutput::stderr_tail`]: the last ones (default
    /// 64 KiB).
    pub stderr_tail_cap: usize,
    /// Hide the console window on Windows. The default. `false` leaves it visible.
    pub no_window: bool,
    /// Bytes written to the child's stdin, then EOF. `None` (the default) gives it a null
    /// stdin.
    pub input: Option<Vec<u8>>,
}

impl Default for StreamOptions {
    fn default() -> Self {
        Self {
            timeout: None,
            poll: Duration::from_millis(100),
            stdout_tail_cap: 64 * 1024,
            stderr_tail_cap: 64 * 1024,
            no_window: true,
            input: None,
        }
    }
}

impl StreamOptions {
    /// No deadline, a 100 ms poll, 64 KiB tails, no window, null stdin.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn poll(mut self, poll: Duration) -> Self {
        self.poll = poll;
        self
    }

    pub fn stdout_tail_cap(mut self, bytes: usize) -> Self {
        self.stdout_tail_cap = bytes;
        self
    }

    pub fn stderr_tail_cap(mut self, bytes: usize) -> Self {
        self.stderr_tail_cap = bytes;
        self
    }

    pub fn no_window(mut self, hidden: bool) -> Self {
        self.no_window = hidden;
        self
    }

    pub fn input(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.input = Some(bytes.into());
        self
    }
}

/// What [`run_streaming`] observed.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct StreamOutput {
    /// The program's exit status. When `cancelled` or `timed_out` is true, the status of the
    /// terminated tree's leader (on Unix, usually a signal).
    pub status: ExitStatus,
    /// True when the cancel predicate returned true and the tree was terminated.
    pub cancelled: bool,
    /// True when the deadline passed and the tree was terminated. Never set together with
    /// `cancelled`: the predicate is evaluated first.
    pub timed_out: bool,
    /// The last `stdout_tail_cap` bytes of stdout, exactly as the child wrote them.
    pub stdout_tail: Vec<u8>,
    /// The last lines of stderr, joined by `\n` (no trailing newline), at most
    /// `stderr_tail_cap` bytes; invalid UTF-8 is replaced. A cut that falls inside a
    /// multi-byte character drops the partial character.
    pub stderr_tail: String,
}

/// Runs `cmd` and shows its output while it runs.
///
/// The caller configures `cmd` (program, arguments, environment, working directory); the crate
/// sets stdin, stdout and stderr (pipes; stdin is null unless `options.input` is set) and the
/// window mode from `options.no_window`, as [`run_command`](crate::run_command) does (this
/// includes the stdio of a Windows `WindowsSpawnConfig` on an `inherit_handles_only` command). The child
/// is owned: its whole tree is terminated on cancel or timeout, not only the leader.
///
/// Reader threads read the child's stdout and stderr and queue what they read; the callbacks
/// run on the calling thread, in arrival order:
/// - `on_stdout` receives raw chunks, byte-exact (a `\r` progress rewrite arrives as the
///   `\r` it is). Chunk boundaries are arbitrary: do not assume a chunk is a line or a
///   character.
/// - `on_stderr_line` receives stderr split into lines the way [`StderrTail`](crate::StderrTail)
///   splits them: a line ends at `\n`, at `\r`, or at `\r\n` as one terminator, so each
///   progress rewrite is a line; the terminator is not included; empty lines are delivered; an
///   unterminated last line is delivered at the end of the stream. Invalid UTF-8 is replaced.
///   A line over 256 KiB is replaced by one `[stderr line over 262144 bytes dropped]` line.
///
/// `cancel` is evaluated on the calling thread at least every `options.poll` and before the
/// first output is waited for; callbacks and `cancel` may share state through `Cell`s or
/// atomics. When it returns true, or when `options.timeout` passes, the tree is terminated
/// ([`OwnedChild::terminate_tree_bounded`](crate::OwnedChild::terminate_tree_bounded)), the
/// leader is reaped, and the output still queued is delivered; the call then returns `Ok` with
/// `cancelled` or `timed_out` set, not an error. A child that exits on its own is not signalled
/// (its leftover tree is, as for [`run`](crate::run)).
///
/// A slow callback slows the child down (back-pressure on its pipe; at most 64 chunks are
/// queued) and delays the next `cancel` evaluation by as long as it runs.
///
/// Nothing waits without a bound. After the child is gone the readers get 2 s to reach the end
/// of their streams (a grandchild that escaped the tree and kept a pipe open cannot extend it);
/// they are then stopped and joined, and what they had queued is still delivered. Tails keep
/// only the last bytes, so memory stays bounded however much the child writes.
///
/// Stdin input uses the same bounded writer as [`run_with_stdin`](crate::run_with_stdin): it is
/// written on its own thread while output is drained, EOF follows, and a child that exits
/// without reading it is a normal early exit. Any other stdin write failure is returned as
/// `Err` after the tree is down. A process that outlived the tree and still holds stdin without
/// reading it cannot hold the call past its deadline.
///
/// What a return guarantees about the writer:
/// - Every `Ok` return: the writer thread has been joined and the write end is closed.
/// - A writer that cannot be stopped within its bound makes the call fail with
///   `ErrorKind::TimedOut`.
/// - Any other `Err` (an early failure, such as starting a reader or waiting on the child)
///   still cancels the writer with the same bound before returning, but reports that original
///   error, not the cleanup outcome.
/// - Unix, every return: the parent has closed the write end, so a writer thread that is
///   still alive can deliver nothing more and ends on its next step.
/// - Windows, after `TimedOut` or an early `Err`: a writer that did not stop within the bound
///   may still be alive and blocked in a write, owning the write end, with its cancel flag set.
///   This needs a pending `CancelIoEx` that fails to abort the write, which is not expected.
///
/// Fails when the program, a reader or the writer cannot start, when waiting on the child
/// fails, or with `ErrorKind::TimedOut` when the stdin writer cannot be stopped or the
/// terminated leader is not reaped within 5 s (a callback panic unwinds with the same cleanup).
/// A pipe read failure on either stream is also an `Err` (with the failing stream named), never
/// an `Ok` with truncated tails. A non-zero exit is a normal `Ok` with `status` set.
pub fn run_streaming(
    mut cmd: OwnedCommand,
    options: &StreamOptions,
    cancel: &dyn Fn() -> bool,
    on_stdout: &mut dyn FnMut(&[u8]),
    on_stderr_line: &mut dyn FnMut(&str),
) -> io::Result<StreamOutput> {
    // Recorded, not applied to `Command` here: `spawn` applies it to the std `Command` and to a
    // native Windows spawn config alike, after every caller edit.
    cmd.pipe_stdio(options.input.is_some());
    if options.no_window {
        cmd.windows_hide();
    } else {
        cmd.windows_visible();
    }
    let mut child = cmd.spawn()?;
    // Declared before the readers so an early `?` return drops (and so stops) them first; the
    // writer's `Drop` cancels it with its bound, and the child's `Drop` kills the tree.
    let writer = match (options.input.as_deref(), child.take_stdin()) {
        (Some(bytes), Some(stdin)) => Some(StdinWriter::spawn(stdin, bytes.to_vec())?),
        _ => None,
    };
    let (sender, receiver) = mpsc::sync_channel::<Message>(QUEUE_CHUNKS);
    let mut stdout = child
        .take_stdout()
        .map(|pipe| {
            NativeOutputCapture::stdout_streaming(pipe, forward(sender.clone(), Message::Stdout))
        })
        .transpose()?;
    let mut stderr = child
        .take_stderr()
        .map(|pipe| {
            NativeOutputCapture::stderr_streaming(pipe, forward(sender.clone(), Message::Stderr))
        })
        .transpose()?;
    // Only the readers' sinks keep the channel open: it disconnects when both are gone.
    drop(sender);

    let mut sinks = Sinks {
        on_stdout,
        on_stderr_line,
        stdout_tail: Tail::new(options.stdout_tail_cap),
        stderr_tail: Tail::new(options.stderr_tail_cap),
        splitter: LineSplitter::default(),
    };
    let poll = options.poll.max(Duration::from_millis(1));
    let step = poll.min(MAX_STEP);
    let started = Instant::now();
    let deadline = options.timeout.and_then(|limit| started.checked_add(limit));
    let mut last_cancel: Option<Instant> = None;
    let mut last_exit_look = started;
    let mut streams_done = false;
    let ended = loop {
        let now = Instant::now();
        if last_cancel.is_none_or(|at| now.duration_since(at) >= poll) {
            last_cancel = Some(now);
            if cancel() {
                break Ended::Cancelled;
            }
        }
        if deadline.is_some_and(|at| now >= at) {
            break Ended::TimedOut;
        }
        if streams_done {
            // Nothing more to read, so the wait for the exit is the pause.
            if let Some(status) = child.wait_exit_then_sweep(step)? {
                break Ended::Exited(status);
            }
            continue;
        }
        match receiver.recv_timeout(step) {
            Ok(message) => sinks.handle(message),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                streams_done = true;
                continue;
            }
        }
        if last_exit_look.elapsed() >= step {
            last_exit_look = Instant::now();
            if let Some(status) = child.wait_exit_then_sweep(Duration::ZERO)? {
                break Ended::Exited(status);
            }
        }
    };

    let (status, cancelled, timed_out) = match ended {
        Ended::Exited(status) => (Some(status), false, false),
        Ended::Cancelled | Ended::TimedOut => {
            let reaped = match child.terminate_tree_bounded(KILL_REAP_WAIT)? {
                ReapOutcome::Exited(status) => Some(status),
                ReapOutcome::TimedOut => None,
            };
            (
                reaped,
                matches!(ended, Ended::Cancelled),
                matches!(ended, Ended::TimedOut),
            )
        }
    };

    // The child is gone. Let the readers reach the end of their streams, but never wait longer
    // than DRAIN_WAIT: a process that escaped the tree can keep a pipe open.
    let drain_end = Instant::now() + DRAIN_WAIT;
    while !streams_done {
        let left = drain_end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match receiver.recv_timeout(left) {
            Ok(message) => sinks.handle(message),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => streams_done = true,
        }
    }
    // Stop and join both readers (each wakes at once and gives up a pending send), then deliver
    // what they had queued.
    if let Some(capture) = stdout.as_mut() {
        let _ = capture.cancel();
    }
    if let Some(capture) = stderr.as_mut() {
        let _ = capture.cancel();
    }
    for message in receiver.try_iter() {
        sinks.handle(message);
    }
    sinks.finish_stream();
    let writer_result = writer.map(StdinWriter::finish).transpose();
    // Both readers are joined, so a read or shutdown failure they stored is final. Taken for
    // both streams before any return: the delivered output is incomplete, so it is an error.
    let stdout_failure = super::capture_failure("stdout", stdout.as_ref());
    let stderr_failure = super::capture_failure("stderr", stderr.as_ref());
    stdout_failure?;
    stderr_failure?;
    writer_result?;
    let Some(status) = status else {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "the terminated process was not reaped within its bound",
        ));
    };
    Ok(StreamOutput {
        status,
        cancelled,
        timed_out,
        stdout_tail: sinks.stdout_tail.into_bytes(),
        stderr_tail: sinks.stderr_tail.into_text(),
    })
}

#[derive(Clone, Copy)]
enum Ended {
    Exited(ExitStatus),
    Cancelled,
    TimedOut,
}

enum Message {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
}

/// A reader-thread sink that queues each chunk. A full queue is waited out in short sleeps and
/// given up once the capture is stopped, so stopping a reader never waits on the consumer.
fn forward(
    sender: SyncSender<Message>,
    wrap: fn(Vec<u8>) -> Message,
) -> crate::native_capture::ChunkSink {
    Box::new(move |chunk: &[u8], stop: &AtomicBool| {
        let mut message = wrap(chunk.to_vec());
        loop {
            match sender.try_send(message) {
                Ok(()) | Err(TrySendError::Disconnected(_)) => return,
                Err(TrySendError::Full(back)) => {
                    if stop.load(Ordering::Acquire) {
                        return;
                    }
                    message = back;
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
        }
    })
}

/// The calling thread's side: callbacks plus the bounded tails.
struct Sinks<'a> {
    on_stdout: &'a mut dyn FnMut(&[u8]),
    on_stderr_line: &'a mut dyn FnMut(&str),
    stdout_tail: Tail,
    stderr_tail: Tail,
    splitter: LineSplitter,
}

impl Sinks<'_> {
    fn handle(&mut self, message: Message) {
        match message {
            Message::Stdout(chunk) => {
                self.stdout_tail.push(&chunk);
                (self.on_stdout)(&chunk);
            }
            Message::Stderr(chunk) => {
                let Self {
                    on_stderr_line,
                    stderr_tail,
                    splitter,
                    ..
                } = self;
                splitter.feed(&chunk, &mut |line| {
                    stderr_tail.push_line(line);
                    (**on_stderr_line)(line);
                });
            }
        }
    }

    /// Delivers the unterminated last stderr line, if any.
    fn finish_stream(&mut self) {
        let Self {
            on_stderr_line,
            stderr_tail,
            splitter,
            ..
        } = self;
        splitter.finish(&mut |line| {
            stderr_tail.push_line(line);
            (**on_stderr_line)(line);
        });
    }
}

/// The last `cap` bytes written to it. Holds at most `2 * cap` bytes plus one chunk.
struct Tail {
    cap: usize,
    bytes: Vec<u8>,
}

impl Tail {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            bytes: Vec::new(),
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        if self.cap == 0 {
            return;
        }
        if chunk.len() >= self.cap {
            self.bytes.clear();
            self.bytes
                .extend_from_slice(&chunk[chunk.len() - self.cap..]);
            return;
        }
        self.bytes.extend_from_slice(chunk);
        if self.bytes.len() > self.cap.saturating_mul(2) {
            self.trim();
        }
    }

    /// One stderr line and the newline that separates it from the next.
    fn push_line(&mut self, line: &str) {
        self.push(line.as_bytes());
        self.push(b"\n");
    }

    fn trim(&mut self) {
        if self.bytes.len() > self.cap {
            let cut = self.bytes.len() - self.cap;
            self.bytes.drain(..cut);
        }
    }

    fn into_bytes(mut self) -> Vec<u8> {
        self.trim();
        self.bytes
    }

    /// The tail as the stderr text: lines joined by `\n`, without the final newline, and
    /// without a character the cut split.
    fn into_text(self) -> String {
        let bytes = self.into_bytes();
        let start = bytes
            .iter()
            .position(|&byte| byte & 0xC0 != 0x80)
            .unwrap_or(bytes.len());
        let mut text = String::from_utf8_lossy(&bytes[start..]).into_owned();
        if text.ends_with('\n') {
            text.pop();
        }
        text
    }
}

/// Splits stderr bytes into lines: `\n`, `\r` and `\r\n` end a line.
#[derive(Default)]
struct LineSplitter {
    line: Vec<u8>,
    /// The last byte was `\r`, so a `\n` next belongs to the same terminator.
    skip_lf: bool,
    /// The current line passed [`LINE_CAP`]; the rest of it is discarded.
    dropping: bool,
}

impl LineSplitter {
    fn feed(&mut self, bytes: &[u8], emit: &mut dyn FnMut(&str)) {
        for &byte in bytes {
            if std::mem::take(&mut self.skip_lf) && byte == b'\n' {
                continue;
            }
            match byte {
                b'\n' | b'\r' => {
                    self.skip_lf = byte == b'\r';
                    self.end_line(emit);
                }
                _ if self.dropping => {}
                _ => {
                    self.line.push(byte);
                    if self.line.len() > LINE_CAP {
                        self.line.clear();
                        self.dropping = true;
                    }
                }
            }
        }
    }

    /// The stream ended: an unterminated last line is still a line.
    fn finish(&mut self, emit: &mut dyn FnMut(&str)) {
        if !self.line.is_empty() || self.dropping {
            self.end_line(emit);
        }
    }

    fn end_line(&mut self, emit: &mut dyn FnMut(&str)) {
        if self.dropping {
            self.dropping = false;
            emit(&format!("[stderr line over {LINE_CAP} bytes dropped]"));
        } else {
            let text = String::from_utf8_lossy(&self.line).into_owned();
            emit(&text);
        }
        self.line.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(chunks: &[&[u8]]) -> Vec<String> {
        let mut splitter = LineSplitter::default();
        let mut out = Vec::new();
        for chunk in chunks {
            splitter.feed(chunk, &mut |line| out.push(line.to_owned()));
        }
        splitter.finish(&mut |line| out.push(line.to_owned()));
        out
    }

    #[test]
    fn lines_end_at_lf_cr_and_crlf_across_chunks() {
        assert_eq!(lines(&[b"a\rb\nc\r\nd"]), ["a", "b", "c", "d"]);
        // CRLF split across two reads is still one terminator.
        assert_eq!(lines(&[b"x\r", b"\ny\n\nz"]), ["x", "y", "", "z"]);
        assert_eq!(lines(&[b"par", b"tial\n"]), ["partial"]);
        assert!(lines(&[b""]).is_empty());
    }

    #[test]
    fn an_overlong_line_becomes_one_marker() {
        let long = vec![b'x'; LINE_CAP + 10];
        let got = lines(&[&long, b"\nnext\n"]);
        let marker = format!("[stderr line over {LINE_CAP} bytes dropped]");
        assert_eq!(got, [marker.as_str(), "next"]);
    }

    #[test]
    fn a_tail_keeps_the_last_bytes() {
        let mut tail = Tail::new(5);
        tail.push(b"ab");
        tail.push(b"cdefgh");
        tail.push(b"i");
        assert_eq!(tail.into_bytes(), b"efghi");
        let mut none = Tail::new(0);
        none.push(b"abc");
        assert!(none.into_bytes().is_empty());
        // Many small pushes stay bounded and keep the end.
        let mut many = Tail::new(4);
        for byte in 0..100u8 {
            many.push(&[byte]);
            assert!(many.bytes.len() <= 9);
        }
        assert_eq!(many.into_bytes(), [96, 97, 98, 99]);
    }

    #[test]
    fn the_stderr_tail_is_joined_lines_without_a_split_character() {
        let mut tail = Tail::new(6);
        for line in ["l1", "l2", "l3", "l4"] {
            tail.push_line(line);
        }
        assert_eq!(tail.into_text(), "l3\nl4");
        // A cut inside a two-byte character drops the rest of that character.
        let mut tail = Tail::new(4);
        tail.push_line("a\u{e9}\u{e9}");
        assert_eq!(tail.into_text(), "\u{e9}");
    }

    #[test]
    fn options_default_and_build() {
        let options = StreamOptions::new();
        assert_eq!(options.timeout, None);
        assert_eq!(options.poll, Duration::from_millis(100));
        assert_eq!(options.stdout_tail_cap, 64 * 1024);
        assert_eq!(options.stderr_tail_cap, 64 * 1024);
        assert!(options.no_window);
        assert!(options.input.is_none());
        let options = options
            .timeout(Duration::from_secs(3))
            .poll(Duration::from_millis(10))
            .stdout_tail_cap(1)
            .stderr_tail_cap(2)
            .no_window(false)
            .input(b"in".to_vec());
        assert_eq!(options.timeout, Some(Duration::from_secs(3)));
        assert_eq!(options.poll, Duration::from_millis(10));
        assert_eq!((options.stdout_tail_cap, options.stderr_tail_cap), (1, 2));
        assert!(!options.no_window);
        assert_eq!(options.input.as_deref(), Some(&b"in"[..]));
    }
}
