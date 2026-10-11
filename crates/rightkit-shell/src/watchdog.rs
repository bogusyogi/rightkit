//! Main-thread hang watchdog.
//!
//! Base: HeardRight `hang_watchdog` (a dedicated OS thread polls a heartbeat that a recurring
//! `run_on_main_thread` ping refreshes; the report fires once per stall episode and re-arms after a
//! fresh beat; neither thread takes an app lock, so the watchdog can observe a deadlock without
//! joining it). Changes: the report goes to a caller callback and a `rightkit-shell://stall`
//! event instead of HeardRight's crash-dump module, and the threshold/interval are configurable.
//! The poll thread touches only atomics and the callback in callback-only mode
//! ([`WatchdogConfig::emit_event`] `false`), HeardRight's `hang_watchdog` rule.
//! HeardRight's opt-in macOS stack-sampling variant (`ui_stall_watchdog`) is not generalised.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Emitter, Wry};

use crate::EVENT_STALL;

#[derive(Debug, Clone, Copy)]
pub struct StallReport {
    pub stalled_ms: u64,
    pub threshold_ms: u64,
}

pub type StallCallback = Arc<dyn Fn(&StallReport) + Send + Sync>;

/// How often the watchdog thread checks the heartbeat by default (HeardRight's cadence).
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone)]
pub struct WatchdogConfig {
    /// A heartbeat older than this is a stall.
    pub threshold: Duration,
    /// How often the main thread is pinged.
    pub heartbeat_interval: Duration,
    /// How often the watchdog thread looks at the heartbeat. Default 250 ms; the floor is 10 ms.
    pub poll_interval: Duration,
    /// Called once per stall episode with the stall duration (see [`StallReport::stalled_ms`]), on the
    /// watchdog thread. A host can use it to write a hang dump. It runs before the `rightkit-shell://stall`
    /// event.
    pub on_stall: Option<StallCallback>,
    /// Also emit the `rightkit-shell://stall` event to the webview. Default `true`.
    ///
    /// Hazard: the event is emitted from the watchdog thread through the `AppHandle`, and that
    /// thread exists to observe an app that may be deadlocked. Emitting takes app-side locks, so a
    /// deadlock in the app can stop the watchdog thread too, at the moment its report matters most.
    /// Set `false` for callback-only mode: the poll thread then touches only `std` atomics and
    /// `on_stall` (it is not even given an `AppHandle`). A failed emit never ends the loop in either
    /// mode; it is counted in [`WatchdogStatus::emit_failures`].
    pub emit_event: bool,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            threshold: Duration::from_secs(5),
            heartbeat_interval: Duration::from_secs(1),
            poll_interval: DEFAULT_POLL_INTERVAL,
            on_stall: None,
            emit_event: true,
        }
    }
}

/// The edge-triggered stall decision, separated from the thread so it can be tested. Feed it each
/// observation; it reports a stall once per episode and re-arms after a fresh beat.
#[derive(Debug, Clone)]
pub struct StallDetector {
    threshold_ms: u64,
    fired: bool,
}

impl StallDetector {
    pub fn new(threshold: Duration) -> Self {
        Self {
            threshold_ms: threshold.as_millis() as u64,
            fired: false,
        }
    }

    /// `last_beat_ms` is the last heartbeat and `now_ms` the current time, both in Unix milliseconds.
    /// Returns the report on the first observation of a stall episode.
    pub fn observe(&mut self, last_beat_ms: u64, now_ms: u64) -> Option<StallReport> {
        if should_fire(last_beat_ms, now_ms, self.fired, self.threshold_ms) {
            self.fired = true;
            return Some(StallReport {
                stalled_ms: now_ms.saturating_sub(last_beat_ms),
                threshold_ms: self.threshold_ms,
            });
        }
        if self.fired && now_ms.saturating_sub(last_beat_ms) <= self.threshold_ms {
            self.fired = false;
        }
        None
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Edge-triggered decision, separated from the loop.
pub fn should_fire(last_beat_ms: u64, now_ms: u64, already_fired: bool, threshold_ms: u64) -> bool {
    !already_fired && now_ms.saturating_sub(last_beat_ms) > threshold_ms
}

/// Heartbeat of the installed watchdog (Unix ms), 0 when none is installed. One watchdog per process.
static LAST_BEAT_MS: AtomicU64 = AtomicU64::new(0);
/// Counters of the installed watchdog.
static COUNTERS: PollCounters = PollCounters::new();

/// Stall and emit-failure counts of one poll loop.
pub(crate) struct PollCounters {
    stalls: AtomicU64,
    emit_failures: AtomicU64,
}

impl PollCounters {
    pub(crate) const fn new() -> Self {
        Self {
            stalls: AtomicU64::new(0),
            emit_failures: AtomicU64::new(0),
        }
    }
}

/// Runtime figures of the hang watchdog, computed on call by [`status`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchdogStatus {
    /// A watchdog has been installed in this process.
    pub installed: bool,
    /// Milliseconds since the main thread last answered a heartbeat ping (0 when not installed).
    pub last_beat_age_ms: u64,
    /// Stall episodes reported since install.
    pub stalls_total: u64,
    /// `rightkit-shell://stall` emits that failed. A failure is counted and the watchdog keeps
    /// running.
    pub emit_failures: u64,
}

/// Runtime figures of the shell: the watchdog, plus the resident memory, CPU time and I/O counters
/// of the whole process (the watchdog's two threads and the shell run in it), or `None` when the
/// host cannot report them. Computed on call; no polling thread runs for this. See
/// `rightkit_process::ProcessUsage`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShellRuntimeStatus {
    pub watchdog: WatchdogStatus,
    pub process: Option<rightkit_process::ProcessUsage>,
}

/// Watchdog state and process resource usage, read now.
pub fn status() -> ShellRuntimeStatus {
    let beat = LAST_BEAT_MS.load(Ordering::Relaxed);
    ShellRuntimeStatus {
        watchdog: WatchdogStatus {
            installed: beat != 0,
            last_beat_age_ms: if beat == 0 {
                0
            } else {
                now_ms().saturating_sub(beat)
            },
            stalls_total: COUNTERS.stalls.load(Ordering::Relaxed),
            emit_failures: COUNTERS.emit_failures.load(Ordering::Relaxed),
        },
        process: rightkit_process::process_usage().ok(),
    }
}

pub fn install(app: AppHandle<Wry>, cfg: WatchdogConfig) {
    let last = Arc::new(AtomicU64::new(now_ms()));
    LAST_BEAT_MS.store(now_ms(), Ordering::Relaxed);
    {
        let (app, last, interval) = (app.clone(), Arc::clone(&last), cfg.heartbeat_interval);
        std::thread::Builder::new()
            .name("rk-shell-heartbeat".into())
            .spawn(move || loop {
                std::thread::sleep(interval);
                let last = Arc::clone(&last);
                // Err means the event loop is gone (app exiting): stop.
                if app
                    .run_on_main_thread(move || {
                        let now = now_ms();
                        last.store(now, Ordering::SeqCst);
                        LAST_BEAT_MS.store(now, Ordering::Relaxed);
                    })
                    .is_err()
                {
                    break;
                }
            })
            .ok();
    }
    let poll = cfg.poll_interval.max(Duration::from_millis(10));
    // Callback-only mode never hands the poll thread an AppHandle: `make` is not called.
    let emit = emitter_for(cfg.emit_event, || {
        let app = app.clone();
        Box::new(move |report: &StallReport| {
            app.emit(
                EVENT_STALL,
                serde_json::json!({"stalledMs": report.stalled_ms, "thresholdMs": report.threshold_ms}),
            )
            .map_err(|e| e.to_string())
        })
    });
    std::thread::Builder::new()
        .name("rk-shell-watchdog".into())
        .spawn(move || {
            poll_loop(
                &last,
                StallDetector::new(cfg.threshold),
                poll,
                cfg.on_stall.as_ref(),
                emit.as_deref(),
                &COUNTERS,
                &AtomicBool::new(false),
            )
        })
        .ok();
}

/// Delivers a stall to the webview; the error text is only counted.
pub(crate) type EmitDyn = dyn Fn(&StallReport) -> Result<(), String> + Send;
pub(crate) type EmitFn = Box<EmitDyn>;

/// The emitter for a config, built only when events are on. `make` is where an `AppHandle` is
/// captured, so callback-only mode never calls it.
pub(crate) fn emitter_for(emit_event: bool, make: impl FnOnce() -> EmitFn) -> Option<EmitFn> {
    emit_event.then(make)
}

/// The watchdog thread's loop. It touches only atomics, `on_stall` and the optional emitter, and it
/// never ends because an emit failed: it runs until `stop` is set (the installed watchdog never sets
/// it, so it re-arms for the life of the process).
pub(crate) fn poll_loop(
    last: &AtomicU64,
    mut detector: StallDetector,
    poll: Duration,
    on_stall: Option<&StallCallback>,
    emit: Option<&EmitDyn>,
    counters: &PollCounters,
    stop: &AtomicBool,
) {
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(poll);
        let beat = last.load(Ordering::SeqCst);
        if let Some(report) = detector.observe(beat, now_ms()) {
            counters.stalls.fetch_add(1, Ordering::Relaxed);
            if let Some(cb) = on_stall {
                cb(&report);
            }
            if let Some(emit) = emit {
                if emit(&report).is_err() {
                    counters.emit_failures.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    const POLL: Duration = Duration::from_millis(10);

    fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !ok() {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_failing_emit_is_counted_and_the_next_stall_still_fires() {
        let last = Arc::new(AtomicU64::new(0)); // stale: stalled from the start
        let counters = Arc::new(PollCounters::new());
        let stop = Arc::new(AtomicBool::new(false));
        let fired = Arc::new(AtomicUsize::new(0));
        let emits = Arc::new(AtomicUsize::new(0));
        let (l, c, s, f, e) = (
            last.clone(),
            counters.clone(),
            stop.clone(),
            fired.clone(),
            emits.clone(),
        );
        let thread = std::thread::spawn(move || {
            let on_stall: StallCallback = Arc::new(move |_| {
                f.fetch_add(1, Ordering::SeqCst);
            });
            let emit: EmitFn = Box::new(move |_| {
                e.fetch_add(1, Ordering::SeqCst);
                Err("webview gone".into())
            });
            poll_loop(
                &l,
                StallDetector::new(Duration::from_millis(50)),
                POLL,
                Some(&on_stall),
                Some(&*emit),
                &c,
                &s,
            );
        });

        wait_for("the first stall", || fired.load(Ordering::SeqCst) == 1);
        wait_for("its failed emit to be counted", || {
            counters.emit_failures.load(Ordering::SeqCst) == 1
        });
        // A fresh beat re-arms; the next stall must fire even though every emit has failed.
        last.store(now_ms(), Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(30));
        last.store(0, Ordering::SeqCst);
        wait_for("the second stall", || fired.load(Ordering::SeqCst) == 2);
        wait_for("its failed emit", || {
            counters.emit_failures.load(Ordering::SeqCst) == 2
        });
        assert!(
            !thread.is_finished(),
            "an emit failure must not end the watchdog"
        );
        assert_eq!(emits.load(Ordering::SeqCst), 2);
        assert_eq!(counters.stalls.load(Ordering::SeqCst), 2);
        stop.store(true, Ordering::SeqCst);
        thread.join().unwrap();
    }

    #[test]
    fn callback_only_mode_never_builds_or_uses_the_handle() {
        // `make` stands in for cloning the AppHandle; reaching it would panic.
        let emit = emitter_for(false, || {
            panic!("callback-only mode must not touch the app handle")
        });
        assert!(emit.is_none());

        let last = Arc::new(AtomicU64::new(0));
        let counters = Arc::new(PollCounters::new());
        let stop = Arc::new(AtomicBool::new(false));
        let fired = Arc::new(AtomicUsize::new(0));
        let (l, c, s, f) = (last.clone(), counters.clone(), stop.clone(), fired.clone());
        let thread = std::thread::spawn(move || {
            let on_stall: StallCallback = Arc::new(move |_| {
                f.fetch_add(1, Ordering::SeqCst);
            });
            poll_loop(
                &l,
                StallDetector::new(Duration::from_millis(50)),
                POLL,
                Some(&on_stall),
                emit.as_deref(),
                &c,
                &s,
            );
        });
        wait_for("the callback", || fired.load(Ordering::SeqCst) == 1);
        assert_eq!(counters.emit_failures.load(Ordering::SeqCst), 0);
        stop.store(true, Ordering::SeqCst);
        thread.join().unwrap();

        // With events on, the handle is requested exactly once.
        let built = std::cell::Cell::new(0);
        let emit = emitter_for(true, || {
            built.set(built.get() + 1);
            Box::new(|_| Ok(()))
        });
        assert!(emit.is_some());
        assert_eq!(built.get(), 1);
    }
}
