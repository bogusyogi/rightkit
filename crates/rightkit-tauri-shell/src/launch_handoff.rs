//! One launch retirement followed by repeat reveals when an engine restarts.

use std::sync::atomic::{AtomicU64, Ordering::SeqCst};
use std::sync::Mutex;

use tauri::{AppHandle, Runtime};

#[cfg(not(target_os = "macos"))]
use std::sync::Arc;
#[cfg(not(target_os = "macos"))]
use tauri::{Manager, WindowEvent};

use crate::window_motion::MotionTiming;

/// Host configuration and surface actions. Geometry, appearance and copy belong to the host.
pub struct LaunchHost<R: Runtime> {
    pub splash_label: &'static str,
    pub splash_target: fn() -> Option<[f64; 4]>,
    pub motion: MotionTiming,
    pub ease: fn(f64) -> f64,
    /// Starts the native macOS flight. The host calls `splash_retired` after its actual close.
    pub native_flight: fn(&AppHandle<R>),
    pub reveal: fn(&AppHandle<R>),
    pub reveal_with_intro: fn(&AppHandle<R>),
    pub settle_activation: fn(&AppHandle<R>),
    pub show_deferred_hub: fn(&AppHandle<R>),
    pub each_finish: fn(&AppHandle<R>),
    pub sink: fn(&str, &str),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Pending,
    Retiring,
    Closed,
    Revealing,
    Ready,
}

enum Action {
    Retire,
    RevealFirst,
    RevealAgain,
}

struct State {
    phase: Phase,
    repeat_pending: bool,
}

/// Coordinates launch against actual splash retirement. Create one static value per host.
pub struct LaunchHandoff {
    state: Mutex<State>,
}

impl LaunchHandoff {
    pub const fn new() -> Self {
        Self {
            state: Mutex::new(State {
                phase: Phase::Pending,
                repeat_pending: false,
            }),
        }
    }

    /// True once the first scheduled handoff begins.
    pub fn is_done(&self) -> bool {
        self.lock().phase != Phase::Pending
    }

    /// Start the first handoff or request a repeat reveal. Requests during retirement coalesce
    /// behind actual completion. Scheduling failure leaves the request retryable.
    pub fn finish<R: Runtime>(&'static self, app: AppHandle<R>, host: LaunchHost<R>) {
        let sink = host.sink;
        #[cfg(target_os = "macos")]
        {
            let task_app = app.clone();
            if let Err(error) = app.run_on_main_thread(move || match self.admit() {
                Some(Action::Retire) => (host.native_flight)(&task_app),
                Some(Action::RevealFirst) => self.reveal(&task_app, &host, true),
                Some(Action::RevealAgain) => self.reveal(&task_app, &host, false),
                None => {}
            }) {
                sink("finish_launch_schedule_failed", &error.to_string());
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.spawn(sink, move || {
                let host = Arc::new(host);
                match self.admit() {
                    Some(Action::Retire) => self.retire_window(&app, &host),
                    Some(Action::RevealFirst) => self.reveal(&app, &host, true),
                    Some(Action::RevealAgain) => self.reveal(&app, &host, false),
                    None => {}
                }
            });
        }
    }

    /// Acknowledge the actual native splash close on macOS, including forced or already-absent
    /// completion. The native animation owns its initial surface reveal.
    #[cfg(target_os = "macos")]
    pub fn splash_retired<R: Runtime>(&'static self, app: AppHandle<R>, host: LaunchHost<R>) {
        if !self.note_retired() {
            return;
        }
        let sink = host.sink;
        let task_app = app.clone();
        if let Err(error) =
            app.run_on_main_thread(move || self.reveal_after_close(&task_app, &host))
        {
            sink("splash_retirement_schedule_failed", &error.to_string());
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn admit(&self) -> Option<Action> {
        let mut state = self.lock();
        let (phase, action) = match state.phase {
            Phase::Pending => (Phase::Retiring, Action::Retire),
            Phase::Closed => (Phase::Revealing, Action::RevealFirst),
            Phase::Ready => (Phase::Revealing, Action::RevealAgain),
            Phase::Retiring | Phase::Revealing => {
                state.repeat_pending = true;
                return None;
            }
        };
        state.phase = phase;
        Some(action)
    }

    fn note_retired(&self) -> bool {
        let mut state = self.lock();
        if state.phase != Phase::Retiring {
            return false;
        }
        state.phase = Phase::Closed;
        true
    }

    fn reveal_after_close<R: Runtime>(&self, app: &AppHandle<R>, host: &LaunchHost<R>) {
        {
            let mut state = self.lock();
            if state.phase != Phase::Closed {
                return;
            }
            state.phase = Phase::Revealing;
        }
        self.reveal(app, host, true);
    }

    fn reveal<R: Runtime>(&self, app: &AppHandle<R>, host: &LaunchHost<R>, mut first: bool) {
        loop {
            #[cfg(target_os = "macos")]
            {
                if !first {
                    (host.reveal)(app);
                }
                (host.settle_activation)(app);
                (host.show_deferred_hub)(app);
                (host.each_finish)(app);
            }
            #[cfg(target_os = "windows")]
            {
                (host.each_finish)(app);
                (host.reveal_with_intro)(app);
                (host.show_deferred_hub)(app);
            }
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            {
                (host.reveal)(app);
                (host.show_deferred_hub)(app);
            }
            if !first {
                (host.sink)("finish_launch_rereveal", "splash already gone");
            }

            let mut state = self.lock();
            if !std::mem::take(&mut state.repeat_pending) {
                state.phase = Phase::Ready;
                return;
            }
            first = false;
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn retire_window<R: Runtime>(&'static self, app: &AppHandle<R>, host: &Arc<LaunchHost<R>>) {
        let Some(splash) = app.get_webview_window(host.splash_label) else {
            if self.note_retired() {
                self.reveal_after_close(app, host);
            }
            return;
        };
        let event_app = app.clone();
        let event_host = Arc::clone(host);
        splash.on_window_event(move |event| {
            if matches!(event, WindowEvent::Destroyed) && self.note_retired() {
                let app = event_app.clone();
                let host = Arc::clone(&event_host);
                self.spawn(host.sink, move || self.reveal_after_close(&app, &host));
            }
        });

        #[cfg(target_os = "windows")]
        if let Some(target) = (host.splash_target)() {
            crate::window_motion::shrink_onto(&splash, target, host.motion, host.ease);
        }
        if let Err(error) = splash.destroy() {
            {
                let mut state = self.lock();
                if state.phase == Phase::Retiring {
                    state.phase = Phase::Pending;
                    state.repeat_pending = false;
                }
            }
            (host.sink)("splash_retirement_failed", &error.to_string());
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn spawn(&self, sink: fn(&str, &str), work: impl FnOnce() + Send + 'static) {
        if let Err(error) = std::thread::Builder::new()
            .name("launch-handoff".into())
            .spawn(work)
        {
            sink("finish_launch_schedule_failed", &error.to_string());
        }
    }
}

impl Default for LaunchHandoff {
    fn default() -> Self {
        Self::new()
    }
}

/// The splash tokens of one process. Every native show gets a fresh nonzero token (the generation
/// the view reports back on close): reserved as pending BEFORE the native call, promoted to
/// accepted only by a live accepted show, and snapshotted for the dismiss. A host with several
/// show paths needs this, since one fixed token would let a close of an older splash retire a
/// newer one. Create one static value per host; no lock is taken.
pub struct SplashTokens {
    next: AtomicU64,
    pending: AtomicU64,
    accepted: AtomicU64,
}

impl SplashTokens {
    /// A machine with no token reserved or accepted.
    pub const fn new() -> Self {
        Self {
            next: AtomicU64::new(0),
            pending: AtomicU64::new(0),
            accepted: AtomicU64::new(0),
        }
    }

    /// Mint a fresh nonzero token and reserve it as pending. No lock is taken.
    pub fn reserve(&self) -> u64 {
        let mut token = self.next.fetch_add(1, SeqCst).wrapping_add(1);
        if token == 0 {
            token = self.next.fetch_add(1, SeqCst).wrapping_add(1);
        }
        self.pending.store(token, SeqCst);
        token
    }

    /// The native show of `token` answered. Only an accepted show whose reservation is still the
    /// pending one becomes the accepted token; returns whether it did.
    pub fn settle(&self, token: u64, accepted: bool) -> bool {
        let live = self
            .pending
            .compare_exchange(token, 0, SeqCst, SeqCst)
            .is_ok();
        if accepted && live {
            self.accepted.store(token, SeqCst);
            return true;
        }
        false
    }

    /// The accepted token to dismiss (0 when none).
    pub fn snapshot_accepted(&self) -> u64 {
        self.accepted.load(SeqCst)
    }

    /// Consume the pending or accepted token for a close: true for exactly one close of it, so a
    /// stale or repeated close (and generation 0) retires nothing.
    pub fn consume_close(&self, generation: u64) -> bool {
        generation != 0
            && (self
                .accepted
                .compare_exchange(generation, 0, SeqCst, SeqCst)
                .is_ok()
                || self
                    .pending
                    .compare_exchange(generation, 0, SeqCst, SeqCst)
                    .is_ok())
    }
}

impl Default for SplashTokens {
    fn default() -> Self {
        Self::new()
    }
}
