//! The full-screen watcher of [`OverlayService`]: keeps every overlay's full-screen verdict current
//! without the app polling.
//!
//! One worker thread re-evaluates ([`OverlayService::refresh_full_screen_with`]) when "the foreground
//! window or its full-screen state may have changed". On Windows the trigger is
//! `rightkit_overlay::windows::run_foreground_event_pump` (WinEvent hooks, no timer): a burst of
//! focus events becomes one evaluation 40 ms after the first, one follow-up pass 500 ms after the
//! last (a full-screen switch settles after its event), and a resize of the foreground window at
//! most every 150 ms. Whatever the platform, a periodic pass every
//! [`FullScreenWatch::fallback_interval`] covers a missed event, or all of them when the hooks
//! could not be installed.
//!
//! macOS has no event source here: the verdict there comes from the app (see the `rightkit-overlay`
//! CHANGELOG), so the watcher starts nothing and the app calls
//! [`OverlayService::refresh_full_screen_with`] from its own notifications.

use std::time::Duration;

use rightkit_overlay::FullScreenProbe;
use tauri::{AppHandle, Wry};

use super::{OverlayError, OverlayService};

/// The shortest fallback interval; a smaller one is raised to it.
pub const MIN_FALLBACK_INTERVAL: Duration = Duration::from_millis(500);

/// How [`OverlayService::watch_full_screen`] decides and how often it checks without an event.
#[derive(Clone, Debug)]
pub struct FullScreenWatch {
    /// A pass runs at least this often even when no event arrives (raised to
    /// [`MIN_FALLBACK_INTERVAL`] when shorter). Events keep it from being the main path: an app
    /// that trusts them passes a long one (30 s), an app that also wants a cheap re-check of its
    /// own state passes a short one (2.5 s).
    pub fallback_interval: Duration,
    /// What counts as full screen. The default ignores this process's own windows.
    pub probe: FullScreenProbe,
}

impl FullScreenWatch {
    pub fn new(fallback_interval: Duration) -> Self {
        Self {
            fallback_interval,
            probe: FullScreenProbe {
                own_pids: vec![std::process::id()],
                ..Default::default()
            },
        }
    }

    pub fn probe(mut self, probe: FullScreenProbe) -> Self {
        self.probe = probe;
        self
    }
}

/// A running full-screen watcher. Dropping it stops the worker and the event source.
pub struct FullScreenWatcher {
    #[cfg(windows)]
    inner: Option<windows::Running>,
}

impl OverlayService {
    /// Start keeping every overlay's full-screen verdict current (see the module notes). Only one
    /// watcher can run per process. Returns an inert watcher off Windows.
    pub fn watch_full_screen(
        &self,
        app: &AppHandle<Wry>,
        config: FullScreenWatch,
    ) -> Result<FullScreenWatcher, OverlayError> {
        #[cfg(windows)]
        {
            let host = std::sync::Arc::clone(&self.host);
            let inner = windows::start(host, app.clone(), config)?;
            Ok(FullScreenWatcher { inner: Some(inner) })
        }
        #[cfg(not(windows))]
        {
            let _ = (app, config);
            Ok(FullScreenWatcher {})
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use rightkit_overlay::{ForegroundEvent, ForegroundPumpStop};
    use tauri::{AppHandle, Wry};

    use super::{FullScreenWatch, FullScreenWatcher, MIN_FALLBACK_INTERVAL};
    use crate::overlay_host::{refresh_full_screen_of, OverlayError, OverlayHost, TauriSurface};

    /// Trailing window that merges the burst one focus change produces into one pass.
    const FOCUS_COALESCE: Duration = Duration::from_millis(40);
    /// A move or resize streams events; evaluate at most this often.
    const GEOMETRY_COALESCE: Duration = Duration::from_millis(150);
    /// One follow-up pass after a focus event: a full-screen switch settles after it fires.
    const SETTLE_PASS: Duration = Duration::from_millis(500);

    enum Message {
        Event(ForegroundEvent),
        Stop,
    }

    pub(super) struct Running {
        tx: Sender<Message>,
        pump: Option<ForegroundPumpStop>,
    }

    pub(super) fn start(
        host: Arc<OverlayHost<TauriSurface>>,
        app: AppHandle<Wry>,
        config: FullScreenWatch,
    ) -> Result<Running, OverlayError> {
        let (tx, rx) = channel::<Message>();
        let (ready_tx, ready_rx) = channel::<Option<ForegroundPumpStop>>();
        let event_tx = tx.clone();
        // The pump owns this thread until it is stopped. A failed install leaves the periodic
        // pass as the only trigger.
        std::thread::Builder::new()
            .name("rk-overlay-winevents".to_string())
            .spawn(move || {
                let announce = ready_tx.clone();
                let started = rightkit_overlay::windows::run_foreground_event_pump(
                    move |stop| {
                        let _ = announce.send(Some(stop));
                    },
                    move |event| {
                        let _ = event_tx.send(Message::Event(event));
                    },
                );
                if !started {
                    let _ = ready_tx.send(None);
                }
            })
            .map_err(|_| OverlayError::Invalid("could not start the full-screen event source"))?;
        let pump = ready_rx.recv_timeout(Duration::from_secs(3)).ok().flatten();

        let interval = config.fallback_interval.max(MIN_FALLBACK_INTERVAL);
        let probe = config.probe;
        std::thread::Builder::new()
            .name("rk-overlay-fullscreen".to_string())
            .spawn(move || {
                let mut due: Option<Instant> = None;
                let mut settle: Option<Instant> = None;
                let mut fallback = Instant::now() + interval;
                loop {
                    let next = [due, settle, Some(fallback)]
                        .into_iter()
                        .flatten()
                        .min()
                        .unwrap_or(fallback);
                    let wait = next.saturating_duration_since(Instant::now());
                    match rx.recv_timeout(wait) {
                        Ok(Message::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                        Ok(Message::Event(event)) => {
                            let now = Instant::now();
                            let window = match event {
                                ForegroundEvent::Focus => {
                                    settle = Some(now + SETTLE_PASS);
                                    FOCUS_COALESCE
                                }
                                ForegroundEvent::Geometry => GEOMETRY_COALESCE,
                            };
                            // Fixed from the first event of a burst, so a stream still evaluates.
                            let at = now + window;
                            due = Some(due.map_or(at, |existing| existing.min(at)));
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                    }
                    let now = Instant::now();
                    let mut run = false;
                    if due.is_some_and(|at| at <= now) {
                        due = None;
                        run = true;
                    }
                    if settle.is_some_and(|at| at <= now) {
                        settle = None;
                        run = true;
                    }
                    if fallback <= now {
                        run = true;
                    }
                    if run {
                        refresh_full_screen_of(&host, &app, |monitor| {
                            rightkit_overlay::windows::foreground_fullscreen_on(
                                monitor.bounds,
                                &probe,
                            )
                        });
                        fallback = Instant::now() + interval;
                    }
                }
            })
            .map_err(|_| OverlayError::Invalid("could not start the full-screen watcher"))?;
        Ok(Running { tx, pump })
    }

    impl Drop for FullScreenWatcher {
        fn drop(&mut self) {
            if let Some(running) = self.inner.take() {
                let _ = running.tx.send(Message::Stop);
                if let Some(pump) = running.pump {
                    pump.stop();
                }
            }
        }
    }
}
