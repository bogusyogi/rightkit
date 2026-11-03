//! Main-thread hang watchdog.
//!
//! Base: HeardRight `hang_watchdog` (a dedicated OS thread watches a heartbeat that a recurring
//! `run_on_main_thread` ping refreshes; the report fires once per stall episode and re-arms after a
//! fresh beat; neither thread takes an app lock, so the watchdog can observe a deadlock without
//! joining it). Changes: the report goes to a caller callback and a `rightkit-shell://stall`
//! event instead of HeardRight's crash-dump module, and the threshold/interval are configurable.
//! The watchdog thread touches only atomics, its wake-up `Bell` and the callback in callback-only
//! mode ([`WatchdogConfig::emit_event`] `false`), HeardRight's `hang_watchdog` rule.
//!
//! # Episodes (PK11-4, K11)
//!
//! One loop, one detector. HeardRight's `ui_stall_watchdog` (a second loop with its own timing,
//! ping and capture rules) is NOT a second watchdog here: its mechanics are fields of
//! [`WatchdogConfig`], and its timings are inputs (threshold 200 ms, heartbeat and poll 50 ms,
//! observer gap 1 s, capture cooldown 60 s), never defaults.
//!
//! * **Monotonic time.** Beats and the stall test use a monotonic clock, so a wall-clock change
//!   neither raises nor hides a stall.
//! * **One outstanding ping.** The heartbeat thread does not queue another main-thread callback
//!   while the last one is unanswered, so a hung UI never accumulates a backlog that all runs at
//!   once when it frees.
//! * **Observer-gap suppression** ([`WatchdogConfig::observer_gap`]). When the watchdog thread
//!   itself wakes later than it asked (the machine slept, the process was suspended), the age of
//!   the beat proves nothing about the UI. The episode is dropped, the gap is counted
//!   ([`WatchdogStatus::observer_gaps`]) and reported ([`WatchdogEvent::ObserverGap`]), and nothing is
//!   reported until the main thread answers a fresh ping.
//! * **One bounded episode per stall.** [`WatchdogConfig::on_stall`] and the event run once per
//!   episode, then the watchdog waits for the next beat. A beat ends the episode and cancels its
//!   capture ([`CaptureRequest::is_recovered`]); [`WatchdogEvent::Recovered`] says so.
//! * **Capture** ([`CaptureConfig`]). After the report the watchdog may run one capture on its own
//!   thread (the watchdog thread never waits for it): skipped when the UI already recovered, inside
//!   the cooldown after the last capture, or while another capture is in flight; a capture that
//!   fails or panics is counted ([`WatchdogStatus::capture_failures`]) and reported
//!   ([`WatchdogEvent::CaptureFailed`]), never silent. With the `crash` feature,
//!   `crash_capture` adapts `rightkit-crash` (a redacted report, optionally a native sample or
//!   minidump, retention) to it.
//! * **Bounded shutdown.** [`WatchdogHandle::stop`] (and [`shutdown`], which the shell's ordered exit
//!   calls) ends both threads and waits for them and any capture for at most
//!   [`WatchdogConfig::shutdown_bound`]; the [`StopReport`] says whether everything ended.
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
//! These estimates count waits in these two worker threads only; they exclude main-thread
//! dispatch and native runtime activity. They do not bound process_usage() wake-up counters.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

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
/// Default for [`WatchdogConfig::observer_gap`].
pub const DEFAULT_OBSERVER_GAP: Duration = Duration::from_secs(1);
/// Default for [`CaptureConfig::cooldown`].
pub const DEFAULT_CAPTURE_COOLDOWN: Duration = Duration::from_secs(60);
/// Default for [`WatchdogConfig::shutdown_bound`].
pub const DEFAULT_SHUTDOWN_BOUND: Duration = Duration::from_secs(2);

/// One stall episode as a capture sees it. Cheap to clone; `is_recovered` flips when the main thread
/// answers a ping after the stall was reported.
#[derive(Debug, Clone)]
pub struct CaptureRequest {
    /// Episode number, 1-based, in this process.
    pub episode: u64,
    pub stalled_ms: u64,
    pub threshold_ms: u64,
    recovered: Arc<AtomicBool>,
}

impl CaptureRequest {
    /// True once the UI answered a ping after this stall. A capture that has not started its
    /// expensive step yet should stop: a stack taken after recovery cannot show the blocking frame.
    pub fn is_recovered(&self) -> bool {
        self.recovered.load(Ordering::Acquire)
    }
}

/// What one capture did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureOutcome {
    /// Done. `artifact` is a file NAME (never a path) for the host's log.
    Captured { artifact: Option<String> },
    /// Nothing to do, for a fixed reason (`"recovered"`, `"unsupported"`).
    Skipped(&'static str),
    /// It failed. The text must not hold a secret; it reaches [`WatchdogEvent::CaptureFailed`].
    Failed(String),
}

/// A capture the watchdog runs on its own thread.
pub type CaptureFn = Arc<dyn Fn(&CaptureRequest) -> CaptureOutcome + Send + Sync>;

/// Capture on a stall: the function and its cooldown.
#[derive(Clone)]
pub struct CaptureConfig {
    pub run: CaptureFn,
    /// The least time between the starts of two captures. Default [`DEFAULT_CAPTURE_COOLDOWN`].
    pub cooldown: Duration,
}

impl CaptureConfig {
    pub fn new(run: impl Fn(&CaptureRequest) -> CaptureOutcome + Send + Sync + 'static) -> Self {
        Self {
            run: Arc::new(run),
            cooldown: DEFAULT_CAPTURE_COOLDOWN,
        }
    }

    pub fn cooldown(mut self, cooldown: Duration) -> Self {
        self.cooldown = cooldown;
        self
    }
}

/// Structured watchdog events for the host's log. None carries a path, a value or a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchdogEvent {
    Started {
        threshold_ms: u64,
        heartbeat_ms: u64,
    },
    Stall {
        episode: u64,
        stalled_ms: u64,
        threshold_ms: u64,
    },
    Recovered {
        episode: u64,
        stalled_ms: u64,
    },
    /// The watchdog thread woke `gap_ms` later than it asked: nothing was concluded about the UI.
    ObserverGap {
        gap_ms: u64,
    },
    CaptureSkipped {
        episode: u64,
        reason: &'static str,
    },
    CaptureStarted {
        episode: u64,
    },
    CaptureFinished {
        episode: u64,
        elapsed_ms: u64,
        artifact: Option<String>,
    },
    CaptureFailed {
        episode: u64,
        reason: String,
    },
    Stopped {
        reason: &'static str,
    },
}

/// Receives [`WatchdogEvent`]s on the watchdog (or capture) thread. It must not block.
pub type WatchdogEventSink = Arc<dyn Fn(&WatchdogEvent) + Send + Sync>;

#[derive(Clone)]
pub struct WatchdogConfig {
    /// A heartbeat older than this is a stall.
    pub threshold: Duration,
    /// How often the main thread is pinged. The floor is 10 ms. One ping is outstanding at a time.
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
    /// When the watchdog thread wakes later than it asked by more than this, the stall test is
    /// skipped until the next fresh beat (the machine slept or the process was suspended).
    /// Default [`DEFAULT_OBSERVER_GAP`]; `Duration::ZERO` turns the check off.
    pub observer_gap: Duration,
    /// Capture on a stall (see [`CaptureConfig`]). `None` (the default): no capture.
    pub capture: Option<CaptureConfig>,
    /// Structured events (start, stall, recovery, observer gap, capture, stop). `None`: none.
    pub on_event: Option<WatchdogEventSink>,
    /// The longest [`WatchdogHandle::stop`] waits for the threads and a running capture.
    /// Default [`DEFAULT_SHUTDOWN_BOUND`].
    pub shutdown_bound: Duration,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            threshold: Duration::from_secs(5),
            heartbeat_interval: Duration::from_secs(1),
            poll_interval: DEFAULT_POLL_INTERVAL,
            on_stall: None,
            emit_event: true,
            observer_gap: DEFAULT_OBSERVER_GAP,
            capture: None,
            on_event: None,
            shutdown_bound: DEFAULT_SHUTDOWN_BOUND,
        }
    }
}

impl WatchdogConfig {
    /// Capture on a stall.
    pub fn with_capture(mut self, capture: CaptureConfig) -> Self {
        self.capture = Some(capture);
        self
    }

    /// Receive structured events.
    pub fn with_events(mut self, sink: impl Fn(&WatchdogEvent) + Send + Sync + 'static) -> Self {
        self.on_event = Some(Arc::new(sink));
        self
    }

    /// The observer-gap bound (`Duration::ZERO`: off).
    pub fn observer_gap(mut self, gap: Duration) -> Self {
        self.observer_gap = gap;
        self
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

    /// `last_beat_ms` is the last heartbeat and `now_ms` the current time, both on one clock in
    /// milliseconds. Returns the report on the first observation of a stall episode.
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

/// Added to the monotonic clock so a beat of 0 ("never beaten") reads as long ago even right after
/// the process started.
const CLOCK_BASE_MS: u64 = 3_600_000;

fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

/// Milliseconds on the watchdog's monotonic clock (an arbitrary origin; only differences mean
/// anything). Never goes backwards and ignores wall-clock changes.
pub(crate) fn now_ms() -> u64 {
    CLOCK_BASE_MS.saturating_add(epoch().elapsed().as_millis() as u64)
}

type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

fn monotonic_clock() -> Clock {
    Arc::new(now_ms)
}

/// Edge-triggered decision, separated from the loop.
pub fn should_fire(last_beat_ms: u64, now_ms: u64, already_fired: bool, threshold_ms: u64) -> bool {
    !already_fired && now_ms.saturating_sub(last_beat_ms) > threshold_ms
}

/// Stall, recovery, gap and capture counts of one watchdog.
#[derive(Default)]
pub(crate) struct PollCounters {
    stalls: AtomicU64,
    emit_failures: AtomicU64,
    recoveries: AtomicU64,
    observer_gaps: AtomicU64,
    captures_started: AtomicU64,
    captures_skipped: AtomicU64,
    capture_failures: AtomicU64,
}

impl PollCounters {
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

/// Runtime figures of the hang watchdog, computed on call by [`status`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchdogStatus {
    /// A watchdog has been installed in this process (and not stopped).
    pub installed: bool,
    /// Milliseconds since the main thread last answered a heartbeat ping (0 when not installed).
    pub last_beat_age_ms: u64,
    /// Stall episodes reported since install.
    pub stalls_total: u64,
    /// `rightkit-shell://stall` emits that failed. A failure is counted and the watchdog keeps
    /// running.
    pub emit_failures: u64,
    /// Stall episodes that ended with a fresh beat.
    pub recoveries: u64,
    /// Times the watchdog thread itself woke late and the stall test was skipped.
    pub observer_gaps: u64,
    /// Captures that started.
    pub captures_started: u64,
    /// Captures not started or ended without an artifact for a fixed reason (cooldown, recovered,
    /// already in flight).
    pub captures_skipped: u64,
    /// Captures that failed or panicked.
    pub capture_failures: u64,
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

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The installed watchdog, one per process.
static INSTALLED: Mutex<Option<WatchdogHandle>> = Mutex::new(None);

/// Watchdog state and process resource usage, read now.
pub fn status() -> ShellRuntimeStatus {
    let watchdog = lock(&INSTALLED)
        .as_ref()
        .map(WatchdogHandle::status)
        .unwrap_or(WatchdogStatus {
            installed: false,
            last_beat_age_ms: 0,
            stalls_total: 0,
            emit_failures: 0,
            recoveries: 0,
            observer_gaps: 0,
            captures_started: 0,
            captures_skipped: 0,
            capture_failures: 0,
        });
    ShellRuntimeStatus {
        watchdog,
        process: rightkit_child_process::process_usage().ok(),
    }
}

/// The shortest wait of either thread. Also the floor of the configured intervals.
const MIN_PERIOD: Duration = Duration::from_millis(10);

/// How long a watchdog thread in an open stall episode waits for a beat before it looks again, when
/// the threshold is shorter. A beat wakes it at once; this only bounds a missed wake-up.
const EPISODE_RECHECK: Duration = Duration::from_secs(1);

/// Runs a closure on the main thread; `false` when the event loop is gone.
pub(crate) type Dispatch = Arc<dyn Fn(Box<dyn FnOnce() + Send>) -> bool + Send + Sync>;

/// Count of live worker threads (heartbeat, watchdog, captures), so a stop can wait for them.
#[derive(Default)]
pub(crate) struct Alive {
    live: Mutex<usize>,
    cv: Condvar,
}

pub(crate) struct AliveGuard(Arc<Alive>);

impl Alive {
    fn enter(self: &Arc<Self>) -> AliveGuard {
        *lock(&self.live) += 1;
        AliveGuard(Arc::clone(self))
    }

    /// Wait up to `bound` for every worker to end; true when none is left.
    fn wait_none(&self, bound: Duration) -> bool {
        let guard = lock(&self.live);
        let (guard, _) = self
            .cv
            .wait_timeout_while(guard, bound, |live| *live > 0)
            .unwrap_or_else(|e| e.into_inner());
        *guard == 0
    }

    fn count(&self) -> usize {
        *lock(&self.live)
    }
}

impl Drop for AliveGuard {
    fn drop(&mut self) {
        let mut live = lock(&self.0.live);
        *live = live.saturating_sub(1);
        self.0.cv.notify_all();
    }
}

/// What the loop reports to: events and the capture runtime.
pub(crate) struct Episodes {
    events: Option<WatchdogEventSink>,
    capture: Option<CaptureRuntime>,
    alive: Arc<Alive>,
}

impl Episodes {
    fn event(&self, event: WatchdogEvent) {
        if let Some(sink) = &self.events {
            // A sink that panics must not end the watchdog.
            let _ = catch_unwind(AssertUnwindSafe(|| sink(&event)));
        }
    }
}

struct CaptureRuntime {
    run: CaptureFn,
    cooldown_ms: u64,
    last_started: Mutex<Option<u64>>,
    in_flight: Arc<AtomicBool>,
}

/// Clears the in-flight flag when the capture thread ends, however it ends.
struct Flight(Arc<AtomicBool>);

impl Drop for Flight {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn start_capture(
    episodes: &Arc<Episodes>,
    counters: &Arc<PollCounters>,
    request: CaptureRequest,
    now: u64,
    already_recovered: bool,
) {
    let Some(rt) = &episodes.capture else {
        return;
    };
    let skip = |reason: &'static str| {
        counters.captures_skipped.fetch_add(1, Ordering::Relaxed);
        episodes.event(WatchdogEvent::CaptureSkipped {
            episode: request.episode,
            reason,
        });
    };
    if already_recovered {
        return skip("already_recovered");
    }
    {
        let mut last = lock(&rt.last_started);
        if last.is_some_and(|t| now.saturating_sub(t) < rt.cooldown_ms) {
            drop(last);
            return skip("cooldown");
        }
        if rt.in_flight.swap(true, Ordering::AcqRel) {
            drop(last);
            return skip("in_flight");
        }
        *last = Some(now);
    }
    let (run, flight) = (Arc::clone(&rt.run), Flight(Arc::clone(&rt.in_flight)));
    let (episodes_for_thread, counters_for_thread) = (Arc::clone(episodes), Arc::clone(counters));
    let alive = episodes.alive.enter();
    let spawned = std::thread::Builder::new()
        .name("rk-shell-stall-capture".into())
        .spawn({
            let request = request.clone();
            move || {
                let _alive = alive;
                let _flight = flight;
                let (episodes, counters) = (episodes_for_thread, counters_for_thread);
                let episode = request.episode;
                if request.is_recovered() {
                    counters.captures_skipped.fetch_add(1, Ordering::Relaxed);
                    episodes.event(WatchdogEvent::CaptureSkipped {
                        episode,
                        reason: "recovered_before_launch",
                    });
                    return;
                }
                counters.captures_started.fetch_add(1, Ordering::Relaxed);
                episodes.event(WatchdogEvent::CaptureStarted { episode });
                let started = Instant::now();
                match catch_unwind(AssertUnwindSafe(|| run(&request))) {
                    Ok(CaptureOutcome::Captured { artifact }) => {
                        episodes.event(WatchdogEvent::CaptureFinished {
                            episode,
                            elapsed_ms: started.elapsed().as_millis() as u64,
                            artifact,
                        });
                    }
                    Ok(CaptureOutcome::Skipped(reason)) => {
                        counters.captures_skipped.fetch_add(1, Ordering::Relaxed);
                        episodes.event(WatchdogEvent::CaptureSkipped { episode, reason });
                    }
                    Ok(CaptureOutcome::Failed(reason)) => {
                        counters.capture_failures.fetch_add(1, Ordering::Relaxed);
                        episodes.event(WatchdogEvent::CaptureFailed { episode, reason });
                    }
                    Err(_) => {
                        counters.capture_failures.fetch_add(1, Ordering::Relaxed);
                        episodes.event(WatchdogEvent::CaptureFailed {
                            episode,
                            reason: "the capture panicked".into(),
                        });
                    }
                }
            }
        });
    if spawned.is_err() {
        // The closure (and with it the alive and flight guards) was dropped by the failed spawn.
        counters.capture_failures.fetch_add(1, Ordering::Relaxed);
        episodes.event(WatchdogEvent::CaptureFailed {
            episode: request.episode,
            reason: "the capture thread could not be started".into(),
        });
    }
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
        }
    }

    fn state(&self) -> MutexGuard<'_, BellState> {
        lock(&self.state)
    }

    /// End both threads' waits for good. Idempotent.
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
        state.stop
    }
}

/// The heartbeat thread's loop: sleep one `interval` on the bell, then `ping` the main thread.
/// `ping` returns `false` when the event loop is gone; the loop then ends. It also ends, at once,
/// when the bell is stopped.
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

/// How much later than asked a wait ended, in milliseconds: `elapsed` is what the clock says passed
/// across the wait, `asked` what the wait was asked to last. Pure.
pub(crate) fn lateness_ms(elapsed_ms: u64, asked: Duration) -> u64 {
    elapsed_ms.saturating_sub(asked.as_millis() as u64)
}

/// Delivers a stall to the webview; the error text is only counted.
pub(crate) type EmitDyn = dyn Fn(&StallReport) -> Result<(), String> + Send + Sync;
pub(crate) type EmitFn = Box<EmitDyn>;

/// The emitter for a config, built only when events are on. `make` is where an `AppHandle` is
/// captured, so callback-only mode never calls it.
pub(crate) fn emitter_for(emit_event: bool, make: impl FnOnce() -> EmitFn) -> Option<EmitFn> {
    emit_event.then(make)
}

/// Everything the watchdog thread needs.
pub(crate) struct LoopDeps {
    pub last: Arc<AtomicU64>,
    pub bell: Arc<Bell>,
    pub counters: Arc<PollCounters>,
    pub poll: Duration,
    pub on_stall: Option<StallCallback>,
    pub emit: Option<EmitFn>,
    /// 0: no observer-gap check.
    pub observer_gap_ms: u64,
    pub clock: Clock,
    pub episodes: Arc<Episodes>,
}

struct OpenEpisode {
    episode: u64,
    recovered: Arc<AtomicBool>,
    stalled_ms: u64,
}

/// The watchdog thread's loop. It touches only atomics, the bell, `on_stall`, the optional
/// emitter and the capture it spawns, and it never ends because an emit failed: it runs until the
/// bell is stopped.
///
/// It does not poll. Healthy, it sleeps until the last beat plus the threshold can have passed
/// (never less than `poll`, which is how late a report may be past the threshold) and looks again
/// then. After a report it sleeps until the main thread answers a heartbeat, which wakes it, and
/// re-arms on any beat newer than the one it reported against, even one that has gone stale again.
///
/// The clock is read exactly twice per iteration (before the wait, after it), which keeps a fake
/// clock deterministic in tests.
pub(crate) fn poll_loop(mut detector: StallDetector, deps: &LoopDeps) {
    let threshold_ms = detector.threshold_ms;
    let episode_wait = Duration::from_millis(threshold_ms).max(EPISODE_RECHECK);
    // The beat the open stall episode was reported against.
    let mut reported: Option<u64> = None;
    let mut open: Option<OpenEpisode> = None;
    let mut episodes_seen = 0_u64;
    // Beat count at which an observer gap was noted; nothing is concluded until a newer beat.
    let mut suppress_from: Option<u64> = None;
    let close_episode = |open: &mut Option<OpenEpisode>| {
        if let Some(ep) = open.take() {
            ep.recovered.store(true, Ordering::Release);
            deps.counters.recoveries.fetch_add(1, Ordering::Relaxed);
            deps.episodes.event(WatchdogEvent::Recovered {
                episode: ep.episode,
                stalled_ms: ep.stalled_ms,
            });
        }
    };
    while !deps.bell.is_stopped() {
        // The count first, then the time: a beat that lands between the two is a newer count than
        // `seen`, so the episode wait below returns at once instead of sleeping through it.
        let seen = deps.bell.beats();
        if let Some(from) = suppress_from {
            if seen == from {
                if deps.bell.wait_watch(episode_wait, Some(seen)) {
                    break;
                }
                continue;
            }
            suppress_from = None;
        }
        let beat = deps.last.load(Ordering::SeqCst);
        let now = (deps.clock)();
        if detector.fired && reported != Some(beat) {
            detector.fired = false;
            close_episode(&mut open);
        }
        if let Some(report) = detector.observe(beat, now) {
            reported = Some(beat);
            episodes_seen += 1;
            let recovered = Arc::new(AtomicBool::new(false));
            open = Some(OpenEpisode {
                episode: episodes_seen,
                recovered: Arc::clone(&recovered),
                stalled_ms: report.stalled_ms,
            });
            deps.counters.stalls.fetch_add(1, Ordering::Relaxed);
            deps.episodes.event(WatchdogEvent::Stall {
                episode: episodes_seen,
                stalled_ms: report.stalled_ms,
                threshold_ms: report.threshold_ms,
            });
            if let Some(cb) = &deps.on_stall {
                cb(&report);
            }
            if let Some(emit) = &deps.emit {
                if emit(&report).is_err() {
                    deps.counters.emit_failures.fetch_add(1, Ordering::Relaxed);
                }
            }
            start_capture(
                &deps.episodes,
                &deps.counters,
                CaptureRequest {
                    episode: episodes_seen,
                    stalled_ms: report.stalled_ms,
                    threshold_ms: report.threshold_ms,
                    recovered,
                },
                now,
                deps.bell.beats() != seen,
            );
        }
        let asked;
        let stopped = if detector.fired {
            asked = episode_wait;
            deps.bell.wait_watch(episode_wait, Some(seen))
        } else {
            asked = until_stall(beat, threshold_ms, now).max(deps.poll);
            deps.bell.wait_watch(asked, None)
        };
        if stopped {
            break;
        }
        if deps.observer_gap_ms > 0 {
            let after = (deps.clock)();
            let late = lateness_ms(after.saturating_sub(now), asked);
            if late > deps.observer_gap_ms {
                deps.counters.observer_gaps.fetch_add(1, Ordering::Relaxed);
                deps.episodes
                    .event(WatchdogEvent::ObserverGap { gap_ms: late });
                // What the beat's age says about the UI is unknowable now: end any open episode and
                // ignore the stale beat until the main thread answers a fresh ping.
                detector.fired = false;
                reported = None;
                close_episode(&mut open);
                let beats_now = deps.bell.beats();
                if beats_now == seen {
                    suppress_from = Some(seen);
                }
            }
        }
    }
}

/// The installed watchdog: stop it, or read it. Cheap to clone.
#[derive(Clone)]
pub struct WatchdogHandle {
    inner: Arc<HandleInner>,
}

struct HandleInner {
    bell: Arc<Bell>,
    alive: Arc<Alive>,
    counters: Arc<PollCounters>,
    last: Arc<AtomicU64>,
    clock: Clock,
    bound: Duration,
    episodes: Arc<Episodes>,
}

/// How a [`WatchdogHandle::stop`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopReport {
    /// Both threads and every capture had ended when the bound passed.
    pub stopped: bool,
    /// A capture was still running (a stop never waits past the bound for it).
    pub capture_in_flight: bool,
    /// Worker threads still alive at return.
    pub threads_left: usize,
    pub elapsed: Duration,
}

impl WatchdogHandle {
    pub fn status(&self) -> WatchdogStatus {
        let i = &self.inner;
        let c = &i.counters;
        WatchdogStatus {
            installed: !i.bell.is_stopped(),
            last_beat_age_ms: (i.clock)().saturating_sub(i.last.load(Ordering::Relaxed)),
            stalls_total: c.stalls.load(Ordering::Relaxed),
            emit_failures: c.emit_failures.load(Ordering::Relaxed),
            recoveries: c.recoveries.load(Ordering::Relaxed),
            observer_gaps: c.observer_gaps.load(Ordering::Relaxed),
            captures_started: c.captures_started.load(Ordering::Relaxed),
            captures_skipped: c.captures_skipped.load(Ordering::Relaxed),
            capture_failures: c.capture_failures.load(Ordering::Relaxed),
        }
    }

    /// True once the watchdog was stopped.
    pub fn is_stopped(&self) -> bool {
        self.inner.bell.is_stopped()
    }

    /// Stop both threads and wait for them and any running capture, at most
    /// [`WatchdogConfig::shutdown_bound`]. Idempotent. A stopped watchdog can be installed again.
    pub fn stop(&self) -> StopReport {
        let started = Instant::now();
        let i = &self.inner;
        let first = !i.bell.is_stopped();
        i.bell.stop();
        let stopped = i.alive.wait_none(i.bound);
        if first {
            i.episodes.event(WatchdogEvent::Stopped {
                reason: if stopped { "stopped" } else { "stop_timed_out" },
            });
        }
        let mut slot = lock(&INSTALLED);
        if slot
            .as_ref()
            .is_some_and(|h| Arc::ptr_eq(&h.inner, &self.inner))
        {
            *slot = None;
        }
        StopReport {
            stopped,
            capture_in_flight: i
                .episodes
                .capture
                .as_ref()
                .is_some_and(|c| c.in_flight.load(Ordering::Acquire)),
            threads_left: i.alive.count(),
            elapsed: started.elapsed(),
        }
    }
}

/// Stop the installed watchdog within its configured bound. `None` when none is installed. The
/// shell's ordered exit calls this.
pub fn shutdown() -> Option<StopReport> {
    let handle = lock(&INSTALLED).clone()?;
    Some(handle.stop())
}

/// Build the threads over `dispatch` (how a closure reaches the main thread). Separated from
/// [`install`] so the whole watchdog runs in tests without a Tauri app.
pub(crate) fn start_with(
    cfg: WatchdogConfig,
    dispatch: Dispatch,
    emit: Option<EmitFn>,
    clock: Clock,
) -> WatchdogHandle {
    let alive = Arc::new(Alive::default());
    let bell = Arc::new(Bell::new());
    let counters = Arc::new(PollCounters::new());
    let last = Arc::new(AtomicU64::new(clock()));
    let episodes = Arc::new(Episodes {
        events: cfg.on_event.clone(),
        capture: cfg.capture.as_ref().map(|c| CaptureRuntime {
            run: Arc::clone(&c.run),
            cooldown_ms: c.cooldown.as_millis() as u64,
            last_started: Mutex::new(None),
            in_flight: Arc::new(AtomicBool::new(false)),
        }),
        alive: Arc::clone(&alive),
    });
    let handle = WatchdogHandle {
        inner: Arc::new(HandleInner {
            bell: Arc::clone(&bell),
            alive: Arc::clone(&alive),
            counters: Arc::clone(&counters),
            last: Arc::clone(&last),
            clock: Arc::clone(&clock),
            bound: cfg.shutdown_bound,
            episodes: Arc::clone(&episodes),
        }),
    };
    let interval = cfg.heartbeat_interval.max(MIN_PERIOD);
    let poll = cfg.poll_interval.max(MIN_PERIOD);
    episodes.event(WatchdogEvent::Started {
        threshold_ms: cfg.threshold.as_millis() as u64,
        heartbeat_ms: interval.as_millis() as u64,
    });

    // The heartbeat thread: one ping outstanding at a time.
    let heartbeat_guard = alive.enter();
    let beat_spawn = std::thread::Builder::new()
        .name("rk-shell-heartbeat".into())
        .spawn({
            let (bell, last, clock, episodes) = (
                Arc::clone(&bell),
                Arc::clone(&last),
                Arc::clone(&clock),
                Arc::clone(&episodes),
            );
            move || {
                let _alive = heartbeat_guard;
                let pending = Arc::new(AtomicBool::new(false));
                heartbeat_loop(&bell.clone(), interval, || {
                    if pending.swap(true, Ordering::AcqRel) {
                        // The last ping is unanswered: the UI is busy or stuck. Do not queue another.
                        return true;
                    }
                    let (last, bell, clock, pending_ack) = (
                        Arc::clone(&last),
                        Arc::clone(&bell),
                        Arc::clone(&clock),
                        Arc::clone(&pending),
                    );
                    let sent = dispatch(Box::new(move || {
                        last.store(clock(), Ordering::SeqCst);
                        pending_ack.store(false, Ordering::Release);
                        // After the time is stored: the watchdog reads the count, then the time.
                        bell.beat();
                    }));
                    if !sent {
                        pending.store(false, Ordering::Release);
                    }
                    sent
                });
                if !bell.is_stopped() {
                    // The event loop is gone: with no heartbeat the beat age means nothing, so the
                    // watchdog thread ends too instead of reporting a stall for an exiting app.
                    episodes.event(WatchdogEvent::Stopped {
                        reason: "dispatch_failed",
                    });
                    bell.stop();
                }
            }
        });
    if beat_spawn.is_err() {
        episodes.event(WatchdogEvent::Stopped {
            reason: "heartbeat_spawn_failed",
        });
        bell.stop();
        return handle;
    }

    // The watchdog thread.
    let watch_guard = alive.enter();
    let deps = LoopDeps {
        last,
        bell: Arc::clone(&bell),
        counters,
        poll,
        on_stall: cfg.on_stall.clone(),
        emit,
        observer_gap_ms: cfg.observer_gap.as_millis() as u64,
        clock,
        episodes: Arc::clone(&episodes),
    };
    let detector = StallDetector::new(cfg.threshold);
    let watch_spawn = std::thread::Builder::new()
        .name("rk-shell-watchdog".into())
        .spawn(move || {
            let _alive = watch_guard;
            poll_loop(detector, &deps);
        });
    if watch_spawn.is_err() {
        episodes.event(WatchdogEvent::Stopped {
            reason: "watchdog_spawn_failed",
        });
        bell.stop();
    }
    handle
}

/// Install the watchdog on the running app. One per process: while one is installed (and not
/// stopped) a second call returns that one and ignores its configuration.
pub fn install(app: AppHandle<Wry>, cfg: WatchdogConfig) -> WatchdogHandle {
    let mut slot = lock(&INSTALLED);
    if let Some(existing) = slot.as_ref().filter(|h| !h.is_stopped()) {
        return existing.clone();
    }
    // Fix the clock's origin before the first beat.
    let _ = epoch();
    let dispatch: Dispatch = {
        let app = app.clone();
        Arc::new(move |job| app.run_on_main_thread(job).is_ok())
    };
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
    let handle = start_with(cfg, dispatch, emit, monotonic_clock());
    *slot = Some(handle.clone());
    handle
}

/// How [`crash_capture`] captures.
#[cfg(feature = "crash")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CrashCaptureOptions {
    /// Also take a native sample (macOS) or minidump (Windows) of this process. Off by default: a
    /// native artifact can hold memory, and sampling a process that is already struggling can
    /// lengthen a short stall, so a host opts in (HeardRight keeps it behind an environment switch).
    pub native: bool,
}

/// The crash adapter (feature `crash`): on a stall record a redacted report of kind `kind`
/// (`rightkit_crash::record_report`), and with [`CrashCaptureOptions::native`] also a native capture
/// of this process (`capture_native_for_pid`), both pruned under `config.retention`. A failure to
/// record anything is [`CaptureOutcome::Failed`], so it reaches [`WatchdogEvent::CaptureFailed`] and
/// [`WatchdogStatus::capture_failures`]. The report text is fixed and holds only the numbers.
#[cfg(feature = "crash")]
pub fn crash_capture(
    config: rightkit_crash::CrashConfig,
    kind: &str,
    options: CrashCaptureOptions,
) -> CaptureConfig {
    let kind = kind.to_owned();
    CaptureConfig::new(move |request| {
        if request.is_recovered() {
            return CaptureOutcome::Skipped("recovered");
        }
        let message = format!(
            "ui stalled {} ms (threshold {} ms), episode {}",
            request.stalled_ms, request.threshold_ms, request.episode
        );
        let report =
            rightkit_crash::record_report(&config, &kind, &message, None, "rk-shell-watchdog", "");
        let native = if options.native && !request.is_recovered() {
            rightkit_crash::capture_native_for_pid(&config, &kind, std::process::id())
        } else {
            None
        };
        let name = |p: &std::path::Path| p.file_name().and_then(|n| n.to_str()).map(str::to_owned);
        match (
            native.as_deref().and_then(name),
            report.as_deref().and_then(name),
        ) {
            (Some(file), _) | (None, Some(file)) => CaptureOutcome::Captured {
                artifact: Some(file),
            },
            (None, None) if report.is_some() || native.is_some() => {
                CaptureOutcome::Captured { artifact: None }
            }
            (None, None) => CaptureOutcome::Failed(
                "nothing was recorded (recording is off or the crash folder is unusable)".into(),
            ),
        }
    })
}
