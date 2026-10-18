//! Stop fan-in for a daemon host (issue #375): one [`StopSource`] that several inputs may
//! trigger (an OS stop signal, the parent closing this process's stdin, or the program itself),
//! and [`StopToken`]s that the host's threads wait on. The first cause wins and later triggers
//! are ignored.
//!
//! Signals: [`stop_on_signals`] installs handlers for SIGINT and SIGTERM on Unix and the console
//! control handler on Windows (Ctrl+C, Ctrl+Break, close). The handler only records the event in a
//! static atomic (the one thing a signal handler may do here); a watcher thread turns that into a
//! trigger, polling every 50 ms, and exits once the source is stopped. Other platforms return
//! `Unsupported`.
//!
//! Stdin: [`stop_on_stdin_close`] triggers the source when stdin reaches end of file (see
//! [`crate::private::watch_stdin_eof`]).

use std::io;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Why the stop was triggered. The first trigger's cause is the one reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopCause {
    /// SIGINT, Ctrl+C or Ctrl+Break.
    Interrupt,
    /// SIGTERM or the console close event.
    Terminate,
    /// This process's stdin reached end of file: the parent went away.
    StdinClosed,
    /// The parent process this one was fenced to (pid + start marker) is gone.
    ParentGone,
    /// The program asked for the stop.
    Requested,
}

struct Inner {
    state: Mutex<Option<StopCause>>,
    changed: Condvar,
}

/// The trigger side. Clone it to give inputs their own handle; every clone triggers the same stop.
#[derive(Clone)]
pub struct StopSource {
    inner: Arc<Inner>,
}

/// The waiting side. Cheap to clone; it sees the stop that its source triggers.
#[derive(Clone)]
pub struct StopToken {
    inner: Arc<Inner>,
}

impl Default for StopSource {
    fn default() -> Self {
        Self::new()
    }
}

impl StopSource {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(None),
                changed: Condvar::new(),
            }),
        }
    }

    /// A token that observes this source.
    pub fn token(&self) -> StopToken {
        StopToken {
            inner: self.inner.clone(),
        }
    }

    /// Trigger the stop. Returns `true` for the call that set it; later calls change nothing.
    pub fn trigger(&self, cause: StopCause) -> bool {
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.is_some() {
            return false;
        }
        *state = Some(cause);
        self.inner.changed.notify_all();
        true
    }
}

impl StopToken {
    /// The cause, once the stop has been triggered.
    pub fn cause(&self) -> Option<StopCause> {
        *self.inner.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn is_stopped(&self) -> bool {
        self.cause().is_some()
    }

    /// Wait up to `timeout` for the stop. Returns its cause as soon as it is triggered, or `None`
    /// when the timeout passes first. Returns at once when the stop is already set.
    pub fn wait(&self, timeout: Duration) -> Option<StopCause> {
        let deadline = Instant::now() + timeout;
        let mut state = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(cause) = *state {
                return Some(cause);
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let (next, _) = self
                .inner
                .changed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(|e| e.into_inner());
            state = next;
        }
    }
}

/// Static slot the signal handler writes; 0 means none pending.
#[cfg(unix)]
static PENDING_SIGNAL: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

#[cfg(unix)]
const SIGINT: i32 = 2;
#[cfg(unix)]
const SIGTERM: i32 = 15;

#[cfg(unix)]
extern "C" fn on_unix_signal(signal: i32) {
    // Only an atomic store here: the one operation a signal handler may safely do.
    PENDING_SIGNAL.store(signal, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(unix)]
extern "C" {
    fn signal(signum: i32, handler: usize) -> usize;
}

#[cfg(windows)]
static PENDING_CONSOLE_EVENT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

#[cfg(windows)]
unsafe extern "system" fn on_console_event(event: u32) -> i32 {
    PENDING_CONSOLE_EVENT.store(event + 1, std::sync::atomic::Ordering::SeqCst);
    1 // handled: the default handler (which ends the process) is not run
}

#[cfg(windows)]
extern "system" {
    fn SetConsoleCtrlHandler(
        handler: Option<unsafe extern "system" fn(u32) -> i32>,
        add: i32,
    ) -> i32;
}

/// Install OS stop-signal handlers that trigger `source` (see the module docs). The watcher thread
/// ends when the source is stopped by any input.
pub fn stop_on_signals(source: &StopSource) -> io::Result<JoinHandle<()>> {
    install_os_handlers()?;
    let source = source.clone();
    std::thread::Builder::new()
        .name("rk-stop-signals".into())
        .spawn(move || loop {
            if source.token().is_stopped() {
                break;
            }
            if let Some(cause) = take_pending_signal() {
                source.trigger(cause);
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        })
}

/// Trigger `source` when this process's stdin reaches end of file.
pub fn stop_on_stdin_close(source: &StopSource) -> JoinHandle<()> {
    let source = source.clone();
    crate::private::watch_stdin_eof(move || {
        source.trigger(StopCause::StdinClosed);
    })
}

/// A parent-process identity fence: the parent's pid pinned to its OS start marker, so a
/// different process that later reuses the pid is never mistaken for the parent. A child
/// whose parent cannot hold its stdin open (or must not rely on it alone) captures this at
/// start and stops itself when [`parent_gone`](Self::parent_gone) turns true; conversely a
/// reused pid alone can neither keep the child alive nor claim to be its parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParentFence {
    pid: u32,
    started: Option<(u64, u64)>,
}

impl ParentFence {
    /// Capture process `pid`'s identity now (its start marker as the OS reports it;
    /// `None` when unreadable, in which case the fence falls back to pid liveness and a
    /// reused pid cannot be told apart — see [`parent_gone`](Self::parent_gone)).
    pub fn capture(pid: u32) -> Self {
        Self {
            pid,
            started: crate::host_identity::process_start_marker(pid),
        }
    }

    /// A fence from a pid and a start marker captured elsewhere (for example handed to
    /// the child at spawn, before any reuse could happen).
    pub fn from_parts(pid: u32, started: Option<(u64, u64)>) -> Self {
        Self { pid, started }
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// The captured start marker, when the OS reported one.
    pub fn started(&self) -> Option<(u64, u64)> {
        self.started
    }

    /// True when the fenced parent is gone: with a captured start marker, when the pid's
    /// current marker differs (the process exited, or the pid was reused by another
    /// process); without one, when the pid is no longer alive.
    pub fn parent_gone(&self) -> bool {
        parent_gone(
            self.started,
            crate::host_identity::process_start_marker(self.pid),
            crate::transport::pid_alive(self.pid),
        )
    }
}

/// The pure fence decision, split out for tests.
fn parent_gone(captured: Option<(u64, u64)>, current: Option<(u64, u64)>, pid_alive: bool) -> bool {
    match captured {
        // The marker pins the pid to one process: any difference — gone, or a reused pid
        // with a different start — means the parent itself is gone.
        Some(marker) => current != Some(marker),
        None => !pid_alive,
    }
}

/// Trigger `source` with [`StopCause::ParentGone`] once `fence`'s parent is gone. Polls
/// every `poll` (raised to at least 10 ms); the watcher thread ends when the source is
/// stopped by any input.
pub fn stop_when_parent_gone(
    source: &StopSource,
    fence: ParentFence,
    poll: Duration,
) -> JoinHandle<()> {
    let source = source.clone();
    let poll = poll.max(Duration::from_millis(10));
    std::thread::spawn(move || loop {
        if source.token().is_stopped() {
            break;
        }
        if fence.parent_gone() {
            source.trigger(StopCause::ParentGone);
            break;
        }
        std::thread::sleep(poll);
    })
}

#[cfg(unix)]
fn install_os_handlers() -> io::Result<()> {
    for sig in [SIGINT, SIGTERM] {
        let handler = on_unix_signal as extern "C" fn(i32) as usize;
        // SAFETY: `signal` is the C library function; the handler is an `extern "C"` function
        // that only stores to an atomic. SIG_ERR is `usize::MAX` on both Linux and macOS.
        let previous = unsafe { signal(sig, handler) };
        if previous == usize::MAX {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(unix)]
fn take_pending_signal() -> Option<StopCause> {
    match PENDING_SIGNAL.swap(0, std::sync::atomic::Ordering::SeqCst) {
        0 => None,
        SIGINT => Some(StopCause::Interrupt),
        _ => Some(StopCause::Terminate),
    }
}

#[cfg(windows)]
fn install_os_handlers() -> io::Result<()> {
    // SAFETY: the handler is an `unsafe extern "system"` function that only stores to an atomic.
    let ok = unsafe { SetConsoleCtrlHandler(Some(on_console_event), 1) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
fn take_pending_signal() -> Option<StopCause> {
    // Console events: 0 Ctrl+C and 1 Ctrl+Break are interrupts; 2 (close) and the logoff and
    // shutdown events are terminations. The stored value is event + 1, so 0 is "none".
    match PENDING_CONSOLE_EVENT.swap(0, std::sync::atomic::Ordering::SeqCst) {
        0 => None,
        1 | 2 => Some(StopCause::Interrupt),
        _ => Some(StopCause::Terminate),
    }
}

#[cfg(not(any(unix, windows)))]
fn install_os_handlers() -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "stop signals are not implemented on this platform",
    ))
}

#[cfg(not(any(unix, windows)))]
fn take_pending_signal() -> Option<StopCause> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_trigger_wins() {
        let source = StopSource::new();
        assert!(source.trigger(StopCause::Terminate));
        assert!(!source.trigger(StopCause::StdinClosed));
        assert_eq!(source.token().cause(), Some(StopCause::Terminate));
    }

    #[test]
    fn a_reused_parent_pid_is_gone_but_the_same_process_is_not() {
        let mine = Some((10, 20));
        // Same pid, same start marker: the parent itself is alive.
        assert!(!parent_gone(mine, mine, true));
        // Same pid, different start marker: the pid was reused; the parent is gone even
        // though the pid is alive, so the reuser cannot stop (or spare) the child.
        assert!(parent_gone(mine, Some((99, 1)), true));
        // No process at the pid any more.
        assert!(parent_gone(mine, None, false));
        // Without a captured marker the fence is pid liveness only.
        assert!(!parent_gone(None, None, true));
        assert!(parent_gone(None, None, false));
    }

    #[test]
    fn a_fence_on_this_process_is_not_gone_and_a_watcher_fires_for_a_dead_parent() {
        let fence = ParentFence::capture(std::process::id());
        assert_eq!(fence.pid(), std::process::id());
        assert!(!fence.parent_gone());
        // A fence whose marker cannot match the (live) test process reads as gone: the
        // pid-reuse shape, driven through the watcher end to end.
        let reused = ParentFence::from_parts(std::process::id(), Some((u64::MAX, u64::MAX)));
        if fence.started().is_some() {
            assert!(reused.parent_gone());
            let source = StopSource::new();
            let token = source.token();
            let watcher = stop_when_parent_gone(&source, reused, Duration::from_millis(10));
            assert_eq!(
                token.wait(Duration::from_secs(5)),
                Some(StopCause::ParentGone)
            );
            watcher.join().unwrap();
        }
    }

    #[test]
    fn a_wait_times_out_until_triggered() {
        let source = StopSource::new();
        let token = source.token();
        assert_eq!(token.wait(Duration::from_millis(5)), None);
        assert!(!token.is_stopped());
        let clone = source.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            clone.trigger(StopCause::Requested);
        });
        assert_eq!(
            token.wait(Duration::from_secs(5)),
            Some(StopCause::Requested)
        );
        t.join().unwrap();
    }
}
