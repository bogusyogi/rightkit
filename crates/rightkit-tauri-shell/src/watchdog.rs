//! Main-thread hang watchdog.
//!
//! Base: HeardRight `hang_watchdog` (a dedicated OS thread watches a heartbeat that a recurring
//! `run_on_main_thread` ping refreshes; the report fires once per stall episode and re-arms after a
//! fresh beat; neither thread takes an app lock, so the watchdog can observe a deadlock without
//! joining it). Changes: the report goes to a caller callback and a `rightkit-shell://stall`
//! event instead of HeardRight's crash-dump module, and the threshold/interval are configurable.
//! The watchdog thread touches only atomics, its wake-up `Bell` and the callback in callback-only
//! mode ([`WatchdogConfig::emit_event`] `false`), HeardRight's `hang_watchdog` rule.
//! HeardRight's opt-in macOS stack-sampling variant (`ui_stall_watchdog`) is not generalised.
//!
//! Wake-ups. Neither thread polls. The heartbeat thread sleeps one heartbeat period on the bell and
//! pings; the watchdog thread sleeps until the earliest moment a stall could be reported (the last
//! beat plus the threshold), looks at the beat again then, and sleeps to the next such moment. Idle,
//! that is one heartbeat wake-up per period plus one watchdog wake-up per threshold (about 1.2 a
//! second with the defaults, where the old 250 ms poll plus the heartbeat made about 5 a second
//! from these two threads). While a stall episode is open the watchdog thread has nothing to time,
//! so it waits for the next beat, which wakes it, or for the stop, which also does. Both threads
//! wait on one `Bell`: a mutex and two condition variables, the same on every platform. The two
//! threads are kept apart on purpose: the heartbeat thread holds the `AppHandle`, the watchdog
//! thread never does in callback-only mode, so a call that blocks on app state cannot delay a report.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Emitter, Wry};

use crate::EVENT_STALL;

#[derive(Debug, Clone, Copy)]
pub struct StallReport {
    pub stalled_ms: u64,
    pub threshold_ms: u64,
}

pub type StallCallback = Arc<dyn Fn(&StallReport) + Send + Sync>;

/// Default for [`WatchdogConfig::poll_interval`] (HeardRight's cadence): the lateness allowed past
/// the threshold, not a polling period.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone)]
pub struct WatchdogConfig {
    /// A heartbeat older than this is a stall.
    pub threshold: Duration,
    /// How often the main thread is pinged. The floor is 10 ms.
    pub heartbeat_interval: Duration,
    /// The shortest the watchdog thread waits between two looks at the heartbeat, which bounds how
    /// late a stall is reported past the threshold. Default 250 ms; the floor is 10 ms. The thread
    /// no longer polls at this rate: idle it sleeps until the last beat plus the threshold can have
    /// passed, so a larger value only widens the allowed lateness and a smaller one only shrinks it.
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
/// `rightkit_child_process::ProcessUsage`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShellRuntimeStatus {
    pub watchdog: WatchdogStatus,
    pub process: Option<rightkit_child_process::ProcessUsage>,
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
        process: rightkit_child_process::process_usage().ok(),
    }
}

/// The shortest wait of either thread. Also the floor of the configured intervals.
const MIN_PERIOD: Duration = Duration::from_millis(10);

/// How long a watchdog thread in an open stall episode waits for a beat before it looks again, when
/// the threshold is shorter. A beat wakes it at once; this only bounds a missed wake-up.
const EPISODE_RECHECK: Duration = Duration::from_secs(1);

pub fn install(app: AppHandle<Wry>, cfg: WatchdogConfig) {
    let last = Arc::new(AtomicU64::new(now_ms()));
    LAST_BEAT_MS.store(now_ms(), Ordering::Relaxed);
    let bell = Arc::new(Bell::new());
    {
        let (app, last, bell, interval) = (
            app.clone(),
            Arc::clone(&last),
            Arc::clone(&bell),
            cfg.heartbeat_interval.max(MIN_PERIOD),
        );
        std::thread::Builder::new()
            .name("rk-shell-heartbeat".into())
            .spawn(move || {
                heartbeat_loop(&bell, interval, || {
                    let last = Arc::clone(&last);
                    let bell = Arc::clone(&bell);
                    // Err means the event loop is gone (app exiting): stop.
                    app.run_on_main_thread(move || {
                        let now = now_ms();
                        last.store(now, Ordering::SeqCst);
                        LAST_BEAT_MS.store(now, Ordering::Relaxed);
                        // After the time is stored: the watchdog reads the count, then the time.
                        bell.beat();
                    })
                    .is_ok()
                })
            })
            .ok();
    }
    let poll = cfg.poll_interval.max(MIN_PERIOD);
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
                &bell,
            )
        })
        .ok();
}

/// The wait point of the watchdog's two threads, and the channel the main thread's beat uses to
/// wake the watchdog thread during a stall episode. One mutex and two condition variables (one per
/// waiter, so a beat never wakes the heartbeat thread); `std` only, with no platform-specific wait.
/// No lock is held while the watchdog thread runs the stall callback or the emitter.
pub(crate) struct Bell {
    state: Mutex<BellState>,
    /// The watchdog thread waits here.
    watch: Condvar,
    /// The heartbeat thread waits here.
    pulse: Condvar,
    /// Returns from either wait, for the quiet-window test.
    #[cfg(test)]
    wakes: AtomicU64,
}

#[derive(Default)]
struct BellState {
    stop: bool,
    /// Heartbeats the main thread has answered.
    beats: u64,
    /// The watchdog thread is in a stall episode and wants to hear the next beat.
    watching: bool,
}

impl Bell {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(BellState::default()),
            watch: Condvar::new(),
            pulse: Condvar::new(),
            #[cfg(test)]
            wakes: AtomicU64::new(0),
        }
    }

    fn state(&self) -> MutexGuard<'_, BellState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// End both threads' waits for good. Idempotent. Test-only: the installed watchdog has no
    /// shutdown handle and runs until the process exits, so production code never stops the bell.
    #[cfg(test)]
    pub(crate) fn stop(&self) {
        self.state().stop = true;
        self.watch.notify_all();
        self.pulse.notify_all();
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.state().stop
    }

    /// The main thread answered a heartbeat. Call it after the beat time is stored. It wakes the
    /// watchdog thread only while a stall episode is open; a healthy watchdog is never woken by it.
    pub(crate) fn beat(&self) {
        let mut state = self.state();
        state.beats += 1;
        if state.watching {
            self.watch.notify_all();
        }
    }

    /// Heartbeats answered so far. The watchdog thread reads it before it reads the beat time.
    pub(crate) fn beats(&self) -> u64 {
        self.state().beats
    }

    /// Heartbeat thread: sleep for `period`, or until stopped. `true` means stopped.
    pub(crate) fn sleep_unless_stopped(&self, period: Duration) -> bool {
        let state = self.state();
        let (state, _) = self
            .pulse
            .wait_timeout_while(state, period, |s| !s.stop)
            .unwrap_or_else(|e| e.into_inner());
        #[cfg(test)]
        self.wakes.fetch_add(1, Ordering::SeqCst);
        state.stop
    }

    /// Watchdog thread: wait for at most `timeout`. With `episode` set to the beat count already
    /// seen, a newer beat also ends the wait. Stop always does. `true` means stopped.
    pub(crate) fn wait_watch(&self, timeout: Duration, episode: Option<u64>) -> bool {
        let mut state = self.state();
        state.watching = episode.is_some();
        let (mut state, _) = self
            .watch
            .wait_timeout_while(state, timeout, |s| {
                !s.stop && episode.is_none_or(|seen| s.beats == seen)
            })
            .unwrap_or_else(|e| e.into_inner());
        state.watching = false;
        #[cfg(test)]
        self.wakes.fetch_add(1, Ordering::SeqCst);
        state.stop
    }

    #[cfg(test)]
    pub(crate) fn wakes(&self) -> u64 {
        self.wakes.load(Ordering::SeqCst)
    }
}

/// The heartbeat thread's loop: sleep one `interval` on the bell, then `ping` the main thread.
/// `ping` returns `false` when the event loop is gone; the loop then ends. It also ends, at once,
/// when the bell is stopped (the installed watchdog never stops it).
pub(crate) fn heartbeat_loop(bell: &Bell, interval: Duration, mut ping: impl FnMut() -> bool) {
    while !bell.sleep_unless_stopped(interval) {
        if !ping() {
            break;
        }
    }
}

/// How long until a stall can first be reported against the beat at `beat_ms`: the instant
/// `now - beat` exceeds the threshold ([`should_fire`] is strict, hence the extra millisecond). Never
/// more than `threshold + 1 ms`, which a fresh beat would give: a clock set back would otherwise
/// leave the watchdog asleep far past the real deadline.
fn until_stall(beat_ms: u64, threshold_ms: u64, now_ms: u64) -> Duration {
    let wait = beat_ms
        .saturating_add(threshold_ms)
        .saturating_add(1)
        .saturating_sub(now_ms);
    Duration::from_millis(wait.min(threshold_ms.saturating_add(1)))
}

/// Delivers a stall to the webview; the error text is only counted.
pub(crate) type EmitDyn = dyn Fn(&StallReport) -> Result<(), String> + Send;
pub(crate) type EmitFn = Box<EmitDyn>;

/// The emitter for a config, built only when events are on. `make` is where an `AppHandle` is
/// captured, so callback-only mode never calls it.
pub(crate) fn emitter_for(emit_event: bool, make: impl FnOnce() -> EmitFn) -> Option<EmitFn> {
    emit_event.then(make)
}

/// The watchdog thread's loop. It touches only atomics, the bell, `on_stall` and the optional
/// emitter, and it never ends because an emit failed: it runs until the bell is stopped (the
/// installed watchdog never stops it, so it re-arms for the life of the process).
///
/// It does not poll. Healthy, it sleeps until the last beat plus the threshold can have passed
/// (never less than `poll`, which is how late a report may be past the threshold) and looks again
/// then. After a report it sleeps until the main thread answers a heartbeat, which wakes it, and
/// re-arms on any beat newer than the one it reported against, even one that has gone stale again.
pub(crate) fn poll_loop(
    last: &AtomicU64,
    mut detector: StallDetector,
    poll: Duration,
    on_stall: Option<&StallCallback>,
    emit: Option<&EmitDyn>,
    counters: &PollCounters,
    bell: &Bell,
) {
    let threshold_ms = detector.threshold_ms;
    let episode_wait = Duration::from_millis(threshold_ms).max(EPISODE_RECHECK);
    // The beat the open stall episode was reported against.
    let mut reported: Option<u64> = None;
    while !bell.is_stopped() {
        // The count first, then the time: a beat that lands between the two is a newer count than
        // `seen`, so the episode wait below returns at once instead of sleeping through it.
        let seen = bell.beats();
        let beat = last.load(Ordering::SeqCst);
        if detector.fired && reported != Some(beat) {
            detector.fired = false;
        }
        if let Some(report) = detector.observe(beat, now_ms()) {
            reported = Some(beat);
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
        let stopped = if detector.fired {
            bell.wait_watch(episode_wait, Some(seen))
        } else {
            bell.wait_watch(until_stall(beat, threshold_ms, now_ms()).max(poll), None)
        };
        if stopped {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    const POLL: Duration = Duration::from_millis(10);

    fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ok() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_failing_emit_is_counted_and_the_next_stall_still_fires() {
        let last = Arc::new(AtomicU64::new(0)); // stale: stalled from the start
        let counters = Arc::new(PollCounters::new());
        let bell = Arc::new(Bell::new());
        let fired = Arc::new(AtomicUsize::new(0));
        let emits = Arc::new(AtomicUsize::new(0));
        let (l, c, b, f, e) = (
            last.clone(),
            counters.clone(),
            bell.clone(),
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
                &b,
            );
        });

        wait_for("the first stall", || fired.load(Ordering::SeqCst) == 1);
        wait_for("its failed emit to be counted", || {
            counters.emit_failures.load(Ordering::SeqCst) == 1
        });
        // A fresh beat ends the episode. It goes stale again after the threshold, and that next
        // stall must fire even though every emit has failed.
        last.store(now_ms(), Ordering::SeqCst);
        bell.beat();
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
        bell.stop();
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
        let bell = Arc::new(Bell::new());
        let fired = Arc::new(AtomicUsize::new(0));
        let (l, c, b, f) = (last.clone(), counters.clone(), bell.clone(), fired.clone());
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
                &b,
            );
        });
        wait_for("the callback", || fired.load(Ordering::SeqCst) == 1);
        assert_eq!(counters.emit_failures.load(Ordering::SeqCst), 0);
        bell.stop();
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

    /// Wait for `rx` to report `n` finished threads, each within a generous bound.
    fn finished(rx: &std::sync::mpsc::Receiver<&'static str>, n: usize) -> Vec<&'static str> {
        let mut done: Vec<&'static str> = (0..n)
            .map(|_| {
                rx.recv_timeout(Duration::from_secs(10))
                    .expect("a thread did not end after the stop")
            })
            .collect();
        done.sort_unstable();
        done
    }

    #[test]
    fn stop_wakes_both_threads_out_of_hour_long_waits() {
        let last = Arc::new(AtomicU64::new(now_ms()));
        let counters = Arc::new(PollCounters::new());
        let bell = Arc::new(Bell::new());
        let (tx, rx) = std::sync::mpsc::channel();
        let (l, c, b, t) = (last.clone(), counters.clone(), bell.clone(), tx.clone());
        let watchdog = std::thread::spawn(move || {
            poll_loop(
                &l,
                StallDetector::new(Duration::from_secs(3600)),
                POLL,
                None,
                None,
                &c,
                &b,
            );
            let _ = t.send("watchdog");
        });
        let (b, t) = (bell.clone(), tx);
        let heartbeat = std::thread::spawn(move || {
            heartbeat_loop(&b, Duration::from_secs(3600), || true);
            let _ = t.send("heartbeat");
        });
        // Give both time to reach their waits; stopping before they do ends them at once too.
        std::thread::sleep(Duration::from_millis(100));
        bell.stop();
        assert_eq!(finished(&rx, 2), ["heartbeat", "watchdog"]);
        watchdog.join().unwrap();
        heartbeat.join().unwrap();
        assert_eq!(counters.stalls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_heartbeat_wakes_a_stalled_watchdog_at_once() {
        // A threshold of ten minutes: the episode wait is ten minutes, so only the beat can end it.
        let last = Arc::new(AtomicU64::new(0)); // stale: stalled from the start
        let counters = Arc::new(PollCounters::new());
        let bell = Arc::new(Bell::new());
        let (l, c, b) = (last.clone(), counters.clone(), bell.clone());
        let thread = std::thread::spawn(move || {
            poll_loop(
                &l,
                StallDetector::new(Duration::from_secs(600)),
                POLL,
                None,
                None,
                &c,
                &b,
            );
        });
        wait_for("the stall", || counters.stalls.load(Ordering::SeqCst) == 1);
        let woke = bell.wakes();
        last.store(now_ms(), Ordering::SeqCst);
        bell.beat();
        wait_for("the beat to wake the watchdog", || bell.wakes() > woke);
        // The beat is fresh, so the episode is over and nothing more is reported.
        assert_eq!(counters.stalls.load(Ordering::SeqCst), 1);
        bell.stop();
        thread.join().unwrap();
    }

    #[test]
    fn a_healthy_watchdog_is_not_woken_by_beats() {
        let last = Arc::new(AtomicU64::new(now_ms()));
        let counters = Arc::new(PollCounters::new());
        let bell = Arc::new(Bell::new());
        let (l, c, b) = (last.clone(), counters.clone(), bell.clone());
        let thread = std::thread::spawn(move || {
            poll_loop(
                &l,
                StallDetector::new(Duration::from_secs(600)),
                POLL,
                None,
                None,
                &c,
                &b,
            );
        });
        std::thread::sleep(Duration::from_millis(100));
        for _ in 0..50 {
            last.store(now_ms(), Ordering::SeqCst);
            bell.beat();
        }
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            bell.wakes(),
            0,
            "a beat must not wake a watchdog with no stall open"
        );
        bell.stop();
        thread.join().unwrap();
    }

    #[test]
    fn a_quiet_watchdog_wakes_about_once_per_heartbeat() {
        let last = Arc::new(AtomicU64::new(now_ms()));
        let counters = Arc::new(PollCounters::new());
        let bell = Arc::new(Bell::new());
        let (tx, rx) = std::sync::mpsc::channel();
        let (l, c, b, t) = (last.clone(), counters.clone(), bell.clone(), tx.clone());
        let watchdog = std::thread::spawn(move || {
            poll_loop(
                &l,
                StallDetector::new(Duration::from_secs(5)),
                POLL,
                None,
                None,
                &c,
                &b,
            );
            let _ = t.send("watchdog");
        });
        let (l, b, t) = (last.clone(), bell.clone(), tx);
        let heartbeat = std::thread::spawn(move || {
            // What the installed ping does on the main thread, once a second.
            heartbeat_loop(&b, Duration::from_secs(1), || {
                l.store(now_ms(), Ordering::SeqCst);
                b.beat();
                true
            });
            let _ = t.send("heartbeat");
        });
        let (start, woke_before) = (Instant::now(), bell.wakes());
        std::thread::sleep(Duration::from_secs(2));
        let (secs, woke) = (start.elapsed().as_secs_f64(), bell.wakes() - woke_before);
        // The window can stretch on a loaded machine and the allowance stretches with it. The
        // 250 ms poll plus the heartbeat woke about ten times in these two seconds.
        let allowed = 3u64.max(secs.ceil() as u64 + 1);
        assert!(woke <= allowed, "woke {woke} times in {secs:.1} s");
        assert_eq!(counters.stalls.load(Ordering::SeqCst), 0);
        bell.stop();
        finished(&rx, 2);
        watchdog.join().unwrap();
        heartbeat.join().unwrap();
    }

    #[test]
    fn a_beat_that_goes_silent_is_reported_at_the_threshold() {
        // Beat now, then never again: the report comes one threshold later, not a poll period or
        // a second threshold later.
        let last = Arc::new(AtomicU64::new(now_ms()));
        let counters = Arc::new(PollCounters::new());
        let bell = Arc::new(Bell::new());
        let reports = Arc::new(Mutex::new(Vec::<StallReport>::new()));
        let (l, c, b, r) = (
            last.clone(),
            counters.clone(),
            bell.clone(),
            reports.clone(),
        );
        let thread = std::thread::spawn(move || {
            let on_stall: StallCallback = Arc::new(move |report: &StallReport| {
                r.lock().unwrap().push(*report);
            });
            poll_loop(
                &l,
                StallDetector::new(Duration::from_secs(1)),
                DEFAULT_POLL_INTERVAL,
                Some(&on_stall),
                None,
                &c,
                &b,
            );
        });
        wait_for("the stall", || !reports.lock().unwrap().is_empty());
        let report = reports.lock().unwrap()[0];
        assert_eq!(report.threshold_ms, 1000);
        assert!(report.stalled_ms > 1000, "{report:?}");
        // One second of margin over the threshold plus the 250 ms period.
        assert!(report.stalled_ms < 2000, "reported late: {report:?}");
        bell.stop();
        thread.join().unwrap();
    }

    #[test]
    fn the_wait_to_the_next_possible_stall_follows_the_beat_and_never_exceeds_the_threshold() {
        // Beat 1 s ago with a 5 s threshold: a stall can first be reported 4 s from now (+1 ms).
        assert_eq!(
            until_stall(9_000, 5_000, 10_000),
            Duration::from_millis(4_001)
        );
        // A beat as old as the threshold or older: already reportable, no wait.
        assert_eq!(until_stall(1_000, 5_000, 6_001), Duration::ZERO);
        assert_eq!(until_stall(1_000, 5_000, 99_000), Duration::ZERO);
        // A beat stamped in the future (the clock was set back) never sleeps past one threshold.
        assert_eq!(
            until_stall(1_000_000, 5_000, 10_000),
            Duration::from_millis(5_001)
        );
        // A fresh beat waits the threshold plus the millisecond `should_fire` needs.
        assert_eq!(
            until_stall(10_000, 5_000, 10_000),
            Duration::from_millis(5_001)
        );
    }
}
