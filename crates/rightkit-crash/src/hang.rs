//! Hang watchdog: notices an event loop or UI thread that stops pumping while the process stays
//! alive (every window "Responding", logs still being written) and records evidence of what was
//! stuck: a `hang` report, an all-threads native capture (Windows minidump, macOS `sample`) and
//! the scrubbed tail of the app's own log files. Local only, like every artifact of this crate.
//!
//! Each watched loop owns a [`Heartbeat`] and calls [`Heartbeat::beat`] as it turns. One watchdog
//! thread compares the last beat with the clock; a beat is one atomic store, so beating threads
//! never wait for the watchdog and the watchdog never takes a lock a beating thread holds.

use crate::hook::{
    next_file_seq, record_report, recording_enabled, resolve_crash_dir, CrashConfig,
};
use crate::native::capture_native_with_secrets;
use crate::report::redact_known;
use crate::NativeCapture;
use std::collections::HashMap;
use std::io::Write;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

/// A loop must be silent at least this long before it counts as hung.
const MIN_THRESHOLD: Duration = Duration::from_secs(2);
const DEFAULT_THRESHOLD: Duration = Duration::from_secs(10);
const DEFAULT_COOLDOWN: Duration = Duration::from_secs(10 * 60);
const DEFAULT_TAIL_BYTES: u64 = 64 * 1024;
const MIN_INTERVAL: Duration = Duration::from_millis(10);
/// Slack added to one interval before a check counts as late (see `is_late`).
const LATE_SLACK_MS: u64 = 250;
/// Content of the log-tail file when the tail could not be scrubbed.
const LOG_WITHHELD_MARKER: &str = "log tail withheld: scrub failed\n";

/// What [`HangOptions::on_hang`] is told: a loop went silent, or recovered. Carries no secrets.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct HangEvent {
    /// The loop's (sanitised) name.
    pub name: String,
    /// How long the loop was silent when reported; for a recovery, the length of the stall.
    pub stalled: Duration,
    /// The `hang` report file, when one was written (always `None` for a recovery).
    pub report: Option<PathBuf>,
    /// `true` when the loop beat again after a reported stall.
    pub recovered: bool,
}

#[derive(Clone)]
struct HangCallback(Arc<dyn Fn(&HangEvent) + Send + Sync>);

impl std::fmt::Debug for HangCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HangCallback")
    }
}

/// Settings for [`HangWatchdog::start`].
#[derive(Debug, Clone)]
pub struct HangOptions {
    threshold: Duration,
    floor: Duration,
    interval: Option<Duration>,
    cooldown: Duration,
    log_files: Vec<PathBuf>,
    tail_bytes: u64,
    native: bool,
    on_hang: Option<HangCallback>,
}

impl Default for HangOptions {
    fn default() -> Self {
        Self {
            threshold: DEFAULT_THRESHOLD,
            floor: MIN_THRESHOLD,
            interval: None,
            cooldown: DEFAULT_COOLDOWN,
            log_files: Vec::new(),
            tail_bytes: DEFAULT_TAIL_BYTES,
            native: true,
            on_hang: None,
        }
    }
}

impl HangOptions {
    /// Defaults: 10 s threshold, check every quarter of it, 10 minutes between captures per loop,
    /// no log files, 64 KiB of each, native capture on.
    pub fn new() -> Self {
        Self::default()
    }

    /// How long a loop may go without a beat before it is reported (default 10 s, at least 2 s).
    pub fn threshold(mut self, threshold: Duration) -> Self {
        self.threshold = threshold;
        self
    }

    /// How often the watchdog checks (default a quarter of the threshold, at least 10 ms). A hang
    /// is noticed within one interval after the threshold passes.
    pub fn check_interval(mut self, interval: Duration) -> Self {
        self.interval = Some(interval);
        self
    }

    /// Minimum time between two captures (native capture and log tail) for the same loop (default
    /// 10 minutes). The `hang` report itself is still recorded once per episode.
    pub fn cooldown(mut self, cooldown: Duration) -> Self {
        self.cooldown = cooldown;
        self
    }

    /// Add a log file whose last lines are kept with a hang. Read-only here; the app writes it.
    pub fn log_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.log_files.push(path.into());
        self
    }

    /// Add several log files (see [`HangOptions::log_file`]).
    pub fn log_files<I, P>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.log_files.extend(paths.into_iter().map(Into::into));
        self
    }

    /// How many bytes to keep from the end of each log file (default 64 KiB). A first partial line
    /// is dropped.
    pub fn log_tail_bytes(mut self, bytes: u64) -> Self {
        self.tail_bytes = bytes;
        self
    }

    /// Whether a hang also takes the all-threads native capture (default `true`). Independent of
    /// [`CrashConfig::native_capture`], which governs the panic hook.
    pub fn native_capture(mut self, enabled: bool) -> Self {
        self.native = enabled;
        self
    }

    /// Call `callback` for each reported hang and each recovery, synchronously on the watchdog
    /// thread, after the `hang` report file is written and before any capture. The
    /// [`CrashConfig`] sink is not used for hangs; this is the hook for the app (for example to
    /// show a message or write to its own log). The callback must return promptly: the watchdog
    /// does no other work while it runs, and `stop()` waits for it. No callback runs once `stop()`
    /// has returned. Stopping or dropping the watchdog from inside the callback does not wait (a
    /// thread cannot join itself); the thread exits at its next check.
    pub fn on_hang(mut self, callback: impl Fn(&HangEvent) + Send + Sync + 'static) -> Self {
        self.on_hang = Some(HangCallback(Arc::new(callback)));
        self
    }

    fn resolve(&self) -> Resolved {
        let threshold = self.threshold.max(self.floor);
        let interval = self.interval.unwrap_or(threshold / 4).max(MIN_INTERVAL);
        Resolved {
            threshold_ms: u64::try_from(threshold.as_millis()).unwrap_or(u64::MAX),
            threshold,
            interval,
            cooldown: self.cooldown,
            log_files: self.log_files.clone(),
            tail_bytes: self.tail_bytes,
            native: self.native,
            on_hang: self.on_hang.clone(),
        }
    }
}

struct Resolved {
    threshold: Duration,
    threshold_ms: u64,
    interval: Duration,
    cooldown: Duration,
    log_files: Vec<PathBuf>,
    tail_bytes: u64,
    native: bool,
    on_hang: Option<HangCallback>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Stop signal that wakes sleepers at once.
struct Stop {
    stopped: Mutex<bool>,
    wake: Condvar,
}

impl Stop {
    fn new() -> Self {
        Self {
            stopped: Mutex::new(false),
            wake: Condvar::new(),
        }
    }

    fn trigger(&self) {
        *lock(&self.stopped) = true;
        self.wake.notify_all();
    }

    fn is_stopped(&self) -> bool {
        *lock(&self.stopped)
    }

    /// Sleep up to `duration`; true when stopped (before or during the sleep).
    fn wait(&self, duration: Duration) -> bool {
        let guard = lock(&self.stopped);
        let (guard, _) = self
            .wake
            .wait_timeout_while(guard, duration, |stopped| !*stopped)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard
    }
}

/// One watched loop. `last` is milliseconds since `epoch` at the last beat.
struct LoopState {
    id: u64,
    name: String,
    epoch: Instant,
    last: AtomicU64,
    /// Cleared by the last [`Heartbeat`] drop (never blocks) and checked before each write step.
    /// A report already past its last check may still complete.
    alive: AtomicBool,
}

impl LoopState {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

struct Shared {
    epoch: Instant,
    loops: Mutex<Vec<Arc<LoopState>>>,
    next_id: AtomicU64,
    stop: Stop,
    /// The config's known secrets, removed from loop names before they are filtered or shown.
    secrets: Vec<String>,
}

/// Removes the loop from the registry when the last [`Heartbeat`] clone is dropped.
struct Registration {
    state: Arc<LoopState>,
    shared: Weak<Shared>,
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.state.alive.store(false, Ordering::Release);
        if let Some(shared) = self.shared.upgrade() {
            lock(&shared.loops).retain(|l| l.id != self.state.id);
        }
    }
}

/// A watched loop's pulse. Cheap to clone and to call from any thread, including a UI thread.
/// When the last clone is dropped the loop is no longer watched. The drop never waits for a
/// report; it briefly takes the registry lock. A report already in progress at that moment may
/// still complete: the loop is checked right before each write step, not across the whole report.
#[derive(Clone)]
pub struct Heartbeat {
    registration: Arc<Registration>,
}

impl std::fmt::Debug for Heartbeat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Heartbeat")
            .field("name", &self.registration.state.name)
            .finish()
    }
}

impl Heartbeat {
    /// Record that the loop is alive: one atomic store, no lock, no allocation.
    pub fn beat(&self) {
        let state = &self.registration.state;
        state.last.store(state.now_ms(), Ordering::Release);
    }
}

/// Handle of the running watchdog thread. Dropping it, or [`HangWatchdog::stop`], stops the thread
/// and waits for it to finish, including any [`HangOptions::on_hang`] callback and native capture
/// in progress; once the call returns, the thread has exited, no callback runs and nothing more is
/// written. The heartbeats stay valid but nothing watches them.
pub struct HangWatchdog {
    shared: Arc<Shared>,
    interval: Duration,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for HangWatchdog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HangWatchdog").finish_non_exhaustive()
    }
}

impl HangWatchdog {
    /// Spawn the watchdog thread (`rightkit-hang-watchdog`). It records the `hang` report through
    /// `config` as [`record_report`] does: nothing is written while recording is off, and the file
    /// goes to the config's crash folder. The config's sink is not called for hangs; use
    /// [`HangOptions::on_hang`].
    pub fn start(config: CrashConfig, options: HangOptions) -> std::io::Result<Self> {
        let resolved = options.resolve();
        let interval = resolved.interval;
        let shared = Arc::new(Shared {
            epoch: Instant::now(),
            loops: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
            stop: Stop::new(),
            secrets: config.secrets().to_vec(),
        });
        let monitor = Monitor {
            config,
            options: resolved,
            shared: Arc::clone(&shared),
            episodes: HashMap::new(),
            captured: HashMap::new(),
            // The first check is measured from the start, so it is protected like the others.
            last_tick_ms: Some(0),
        };
        let thread = std::thread::Builder::new()
            .name("rightkit-hang-watchdog".into())
            .spawn(move || monitor.run())?;
        Ok(Self {
            shared,
            interval,
            thread: Some(thread),
        })
    }

    /// Start watching a loop named `name`. Configured secrets and token patterns are removed first, then letters,
    /// digits, `-`, `_` and `.` are kept, at most 48 characters. It counts as beating now. Drop
    /// every clone of the returned [`Heartbeat`] when the loop ends.
    pub fn heartbeat(&self, name: &str) -> Heartbeat {
        // Known secrets, then the shared pattern scrub (tokens, keys); a scrubber panic yields
        // the fallback name, never the input.
        let redacted = catch_unwind(AssertUnwindSafe(|| {
            rightkit_logs::scrub_text(&crate::report::redact_known(name, &self.shared.secrets))
        }))
        .unwrap_or_default();
        let mut name: String = redacted
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            .take(48)
            .collect();
        if name.is_empty() {
            name = "loop".to_string();
        }
        let state = Arc::new(LoopState {
            id: self.shared.next_id.fetch_add(1, Ordering::Relaxed),
            name,
            epoch: self.shared.epoch,
            last: AtomicU64::new(0),
            alive: AtomicBool::new(true),
        });
        state.last.store(state.now_ms(), Ordering::Release);
        lock(&self.shared.loops).push(Arc::clone(&state));
        Heartbeat {
            registration: Arc::new(Registration {
                state,
                shared: Arc::downgrade(&self.shared),
            }),
        }
    }

    /// Watch an event loop the app does not beat itself. A poster thread (`rightkit-hang-pump`)
    /// calls `post` every check interval with a closure that beats; `post` must hand it to the
    /// loop and return `true`, without blocking. While the loop is blocked the closure never runs
    /// and the hang is detected. At most one closure is outstanding: no new one is posted until
    /// the previous one has run. When `post` returns `false` (the loop is gone, as at shutdown) the
    /// poster stops and the loop is no longer watched. Tauri:
    /// `wd.pump_via("main", move |f| handle.run_on_main_thread(f).is_ok())`.
    pub fn pump_via<F>(&self, name: &str, post: F) -> std::io::Result<()>
    where
        F: Fn(Box<dyn FnOnce() + Send>) -> bool + Send + 'static,
    {
        let heartbeat = self.heartbeat(name);
        let shared = Arc::clone(&self.shared);
        let interval = self.interval;
        let in_flight = Arc::new(AtomicBool::new(false));
        // Not joined: `post` is the app's and might block; the thread ends on stop or at its
        // next wake.
        std::thread::Builder::new()
            .name("rightkit-hang-pump".into())
            .spawn(move || loop {
                if in_flight
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    let beat = heartbeat.clone();
                    let flag = Arc::clone(&in_flight);
                    let beat_on_loop: Box<dyn FnOnce() + Send> = Box::new(move || {
                        beat.beat();
                        flag.store(false, Ordering::Release);
                    });
                    let posted =
                        catch_unwind(AssertUnwindSafe(|| post(beat_on_loop))).unwrap_or(false);
                    if !posted {
                        break;
                    }
                }
                if shared.stop.wait(interval) {
                    break;
                }
            })?;
        Ok(())
    }

    /// Stop the watchdog and wait for its thread to exit. This waits for any `on_hang` callback
    /// and native capture in progress (callbacks must return promptly; a capture cannot be
    /// cancelled). Once this returns, no write step or callback runs; one already past its check
    /// when stop is signalled may still finish before the thread exits.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.shared.stop.trigger();
        if let Some(thread) = self.thread.take() {
            // Stopped from inside an `on_hang` callback the watchdog thread cannot join itself;
            // it exits at its next check instead.
            if thread.thread().id() != std::thread::current().id() {
                let _ = thread.join();
            }
        }
    }
}

impl Drop for HangWatchdog {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// An open stall of one loop. `beat_ms` is the last beat before it.
struct Episode {
    beat_ms: u64,
}

/// State owned by the watchdog thread alone.
struct Monitor {
    config: CrashConfig,
    options: Resolved,
    shared: Arc<Shared>,
    episodes: HashMap<u64, Episode>,
    captured: HashMap<u64, Instant>,
    /// When the previous check ran, to tell a system sleep from a hang.
    last_tick_ms: Option<u64>,
}

impl Monitor {
    fn run(mut self) {
        while !self.shared.stop.wait(self.options.interval) {
            if catch_unwind(AssertUnwindSafe(|| self.tick())).is_err() {
                let _ = writeln!(
                    std::io::stderr(),
                    "rightkit-crash: hang watchdog check failed; continuing"
                );
            }
        }
    }

    fn tick(&mut self) {
        let now_ms = u64::try_from(self.shared.epoch.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.tick_at(now_ms);
        // Time spent reporting and capturing is not lateness: measure the next gap from here.
        self.last_tick_ms =
            Some(u64::try_from(self.shared.epoch.elapsed().as_millis()).unwrap_or(u64::MAX));
    }

    /// One check at `now_ms` (milliseconds since the registry epoch).
    fn tick_at(&mut self, now_ms: u64) {
        // A check that comes far later than scheduled means this thread was not running either
        // (system sleep or a suspended process): every loop looks silent for the same gap. Skip
        // this check; a loop that is really stuck is still silent at the next one.
        let interval_ms = u64::try_from(self.options.interval.as_millis()).unwrap_or(u64::MAX);
        let late = self
            .last_tick_ms
            .is_some_and(|prev| is_late(prev, now_ms, interval_ms));
        self.last_tick_ms = Some(now_ms);
        if late {
            return;
        }
        if !recording_enabled() {
            return;
        }
        // The registry lock is held only to copy the list; beating threads never take it.
        let loops: Vec<Arc<LoopState>> = lock(&self.shared.loops).clone();
        self.episodes
            .retain(|id, _| loops.iter().any(|l| l.id == *id));
        self.captured
            .retain(|id, _| loops.iter().any(|l| l.id == *id));
        for state in &loops {
            if !state.alive.load(Ordering::Acquire) {
                self.episodes.remove(&state.id);
                self.captured.remove(&state.id);
                continue;
            }
            if self.shared.stop.is_stopped() {
                return;
            }
            let last = state.last.load(Ordering::Acquire);
            let now = now_ms.max(last);
            match self.episodes.get(&state.id).map(|e| e.beat_ms) {
                Some(beat_ms) if beat_ms != last => {
                    self.episodes.remove(&state.id);
                    // From the last beat before the stall to the first one after it, or a little
                    // later when several beats came between two checks.
                    let stalled = Duration::from_millis(last.saturating_sub(beat_ms));
                    // An earlier callback may have turned recording off.
                    if self.may_write(state) {
                        event_line("hang-recovered", &state.name, stalled);
                        self.notify(&state.name, stalled, None, true);
                    }
                }
                Some(_) => {}
                None => {
                    let silent = now.saturating_sub(last);
                    // An episode is only committed once its report was written, so a hang
                    // skipped while recording was off is still reported once it is back on.
                    if silent >= self.options.threshold_ms
                        && self.report(state, Duration::from_millis(silent))
                    {
                        self.episodes.insert(state.id, Episode { beat_ms: last });
                    }
                }
            }
        }
    }

    /// Whether the report was written (consent, stop and registration allowed it).
    fn report(&mut self, state: &LoopState, stalled: Duration) -> bool {
        let message = format!(
            "loop {} stopped beating for {:.1} s (threshold {:.1} s)",
            state.name,
            stalled.as_secs_f64(),
            self.options.threshold.as_secs_f64()
        );
        // The native capture below is the hang's own, not the panic hook's.
        // The config's sink is not used here: the app hears about hangs through `on_hang`.
        let mut config = self.config.clone().without_sink();
        config.native_capture = NativeCapture::Off;
        if !self.may_write(state) {
            return false;
        }
        let path = record_report(&config, "hang", &message, None, &state.name, "");
        // The writer's own outcome decides: when the file was not written (consent turned off
        // around the write, or the write failed) the episode stays open and the hang is reported
        // at a later check, never silently dropped. Without a report file, stderr and `on_hang`
        // are the report.
        if (config.writes_files() && path.is_none()) || !self.may_write(state) {
            return false;
        }
        event_line("hang", &state.name, stalled);
        self.notify(&state.name, stalled, path, false);
        self.capture(state);
        true
    }

    /// The hang's log tail and native capture, once per cooldown. The callback may have turned
    /// recording off, the loop may have ended, or the watchdog may have been stopped: checked
    /// again before each write step.
    fn capture(&mut self, state: &LoopState) {
        if !self.may_write(state) {
            return;
        }
        let wants_capture = self.options.native || !self.options.log_files.is_empty();
        let cooled = self
            .captured
            .get(&state.id)
            .is_none_or(|at| at.elapsed() >= self.options.cooldown);
        if !wants_capture || !cooled || !self.config.writes_files() {
            return;
        }
        self.captured.insert(state.id, Instant::now());
        let Some(dir) = resolve_crash_dir(&self.config) else {
            return;
        };
        if !self.options.log_files.is_empty() {
            if !self.may_write(state) {
                return;
            }
            let _ = write_log_tail(
                &dir,
                &self.options.log_files,
                self.options.tail_bytes,
                self.config.secrets(),
            );
        }
        if self.options.native {
            if !self.may_write(state) {
                return;
            }
            let _ = capture_native_with_secrets(
                &dir,
                "hang",
                self.config.secrets(),
                self.config.sample_duration(),
            );
        }
        if self.may_write(state) {
            crate::retention::prune(&dir, self.config.retention);
        }
    }

    /// Tell `on_hang`, synchronously, unless the watchdog is stopping or recording is off.
    fn notify(&self, name: &str, stalled: Duration, report: Option<PathBuf>, recovered: bool) {
        let Some(callback) = &self.options.on_hang else {
            return;
        };
        if self.shared.stop.is_stopped() || !recording_enabled() {
            return;
        }
        let event = HangEvent {
            name: name.to_string(),
            stalled,
            report,
            recovered,
        };
        let _ = catch_unwind(AssertUnwindSafe(|| (callback.0)(&event)));
    }

    /// Consent, stop and registration, checked right before each write step.
    fn may_write(&self, state: &LoopState) -> bool {
        recording_enabled() && !self.shared.stop.is_stopped() && state.alive.load(Ordering::Acquire)
    }
}

/// Whether a check at `now_ms` came so much later than the previous one at `prev_ms` that this
/// thread cannot have been running (sleep, suspension): more than two intervals, or one interval
/// plus a small slack, whichever is larger. Independent of the hang threshold, so a suspension
/// near the threshold is classified correctly.
fn is_late(prev_ms: u64, now_ms: u64, interval_ms: u64) -> bool {
    let allowed = interval_ms
        .saturating_mul(2)
        .max(interval_ms.saturating_add(LATE_SLACK_MS));
    now_ms.saturating_sub(prev_ms) > allowed
}

/// One structured line on stderr, like the crate's other `rightkit-crash:` lines. `name` is
/// already reduced to a safe alphabet.
fn event_line(event: &str, name: &str, stalled: Duration) {
    let _ = writeln!(
        std::io::stderr(),
        "rightkit-crash: {event} loop={name} stalled_secs={:.1}",
        stalled.as_secs_f64()
    );
}

/// The last `max` bytes of `path` as text, without a first partial line (a secret could straddle
/// the cut, and half a line is no use to a reader). `None` when the file cannot be read.
fn tail_text(path: &Path, max: u64) -> Option<String> {
    let bytes = rightkit_logs::tail_bytes(path, max).ok()?;
    let cut = u64::try_from(bytes.len()).unwrap_or(u64::MAX) >= max;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if cut {
        match text.find('\n') {
            Some(i) => {
                text.drain(..=i);
            }
            None => text.clear(),
        }
    }
    Some(text)
}

/// Known secrets first, then the pattern scrub. A panic in the scrubber, or a known secret still
/// present afterwards, yields the marker: never the input.
fn scrub_tail(text: &str, secrets: &[String]) -> String {
    let scrubbed = catch_unwind(AssertUnwindSafe(|| {
        rightkit_logs::scrub_text(&redact_known(text, secrets))
    }));
    match scrubbed {
        Ok(out)
            if !secrets
                .iter()
                .any(|s| !s.is_empty() && out.contains(s.as_str())) =>
        {
            out
        }
        _ => LOG_WITHHELD_MARKER.to_string(),
    }
}

/// Write the scrubbed tails of `files` to one `sample_hang_logs_*.txt` in `dir`: owner-only,
/// written to a temp file and renamed into place (a symlink at the name is replaced, never
/// written through). Nothing unscrubbed touches the disk. The `sample_` prefix makes retention
/// prune it with the native captures and the support bundle scrub it again.
fn write_log_tail(dir: &Path, files: &[PathBuf], max: u64, secrets: &[String]) -> Option<PathBuf> {
    let mut text = String::new();
    for path in files {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "log".to_string());
        text.push_str(&format!("== {name} ==\n"));
        match tail_text(path, max) {
            Some(tail) => text.push_str(&tail),
            None => text.push_str("[unreadable]"),
        }
        if !text.ends_with('\n') {
            text.push('\n');
        }
    }
    let scrubbed = scrub_tail(&text, secrets);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!(
        "sample_hang_logs_{stamp}_{}_{}.txt",
        std::process::id(),
        next_file_seq()
    ));
    let options = rightkit_fs::AtomicWriteOptions {
        private: true,
        ..rightkit_fs::AtomicWriteOptions::default()
    };
    rightkit_fs::atomic_write_with_options(&path, options, |file| {
        file.write_all(scrubbed.as_bytes())
    })
    .ok()?;
    Some(path)
}
