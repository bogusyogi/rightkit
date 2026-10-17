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
