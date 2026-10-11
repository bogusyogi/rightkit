//! Opt-in quit drain.
//!
//! By default the shell quits at once: Quit marks the app exiting, saves window state, stops the
//! engine and exits. A host that has work to flush first (a mail outbox, an unsaved draft) opts in
//! with [`crate::ShellBuilder::quit_drain`]. A quit request then does not exit; it emits
//! `rightkit-shell://quit-requested` to the renderer, prevents the exit, and waits for the renderer
//! to call `rk_shell_finish_quit`. The host never trusts the renderer to answer: after a bound
//! ([`DEFAULT_QUIT_DRAIN_BOUND`] unless the host sets one, never above [`MAX_QUIT_DRAIN_BOUND`]) the
//! app exits anyway.
//!
//! `@rightkit/shell` drains its handlers for 10 s by default, so the 15 s default host bound leaves
//! room for the renderer's own timeout to fire first and report.
//!
//! [`QuitGate`] is the whole decision and has no Tauri dependency, so the order of events is tested
//! without an event loop; `lifecycle` only maps `RunEvent::ExitRequested` and the tray Quit item
//! onto it.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// The host-side bound after which the app exits even if the renderer never calls finish.
pub const DEFAULT_QUIT_DRAIN_BOUND: Duration = Duration::from_secs(15);

/// The longest bound a host may configure; larger values are clamped. An unbounded drain would let
/// a hung renderer keep a quit-requested app alive forever.
pub const MAX_QUIT_DRAIN_BOUND: Duration = Duration::from_secs(120);

/// What a quit request should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitStep {
    /// Proceed with the exit now (drain not enabled, already exiting, or no renderer to drain).
    ExitNow,
    /// Emit `quit-requested`, prevent the exit and arm the bound.
    Drain,
    /// A drain is already running: prevent the exit, emit nothing.
    AlreadyDraining,
}

/// The quit decision. One per app, in `ShellState`.
#[derive(Debug)]
pub struct QuitGate {
    bound: Option<Duration>,
    draining: AtomicBool,
    /// Bumped by every armed bound and by `finish`, so a stale timer never fires.
    epoch: AtomicU64,
}

impl QuitGate {
    /// `None`: drain off (today's immediate quit). `Some(bound)`: drain on, clamped to
    /// [`MAX_QUIT_DRAIN_BOUND`].
    pub fn new(bound: Option<Duration>) -> Self {
        Self {
            bound: bound.map(|b| b.min(MAX_QUIT_DRAIN_BOUND)),
            draining: AtomicBool::new(false),
            epoch: AtomicU64::new(0),
        }
    }

    pub fn enabled(&self) -> bool {
        self.bound.is_some()
    }

    pub fn bound(&self) -> Option<Duration> {
        self.bound
    }

    /// True while a drain is in progress.
    pub fn draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }

    /// Decide a quit request. `exiting` is the shell's exiting flag: once set (by
    /// `rk_shell_finish_quit`, `rk_shell_quit` or the bound) the exit path never prompts again.
    /// `renderer_alive` is false when no webview window remains, so nothing can answer.
    pub fn request(&self, exiting: &AtomicBool, renderer_alive: bool) -> QuitStep {
        if exiting.load(Ordering::SeqCst) || !self.enabled() || !renderer_alive {
            return QuitStep::ExitNow;
        }
        if self.draining.swap(true, Ordering::SeqCst) {
            QuitStep::AlreadyDraining
        } else {
            QuitStep::Drain
        }
    }

    /// The renderer finished (or the host is exiting): cancel the bound.
    pub fn finish(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
        self.draining.store(false, Ordering::SeqCst);
    }

    /// Start the bound: after it elapses, `on_expiry` runs once unless [`QuitGate::finish`] ran
    /// first. The thread is short-lived and holds no `AppHandle` beyond what `on_expiry` captures.
    pub fn arm_bound(self: &Arc<Self>, on_expiry: impl FnOnce() + Send + 'static) {
        let Some(bound) = self.bound else {
            return;
        };
        let epoch = self.epoch.fetch_add(1, Ordering::SeqCst) + 1;
        let gate = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("rk-shell-quit-bound".into())
            .spawn(move || {
                std::thread::sleep(bound);
                if gate.epoch.load(Ordering::SeqCst) == epoch && gate.draining() {
                    gate.draining.store(false, Ordering::SeqCst);
                    on_expiry();
                }
            });
        if spawned.is_err() {
            // No thread, no bound: do not leave the app waiting on a renderer. The caller's
            // closure was moved into the failed spawn, so clear the drain and let a retry exit.
            self.draining.store(false, Ordering::SeqCst);
        }
    }
}
