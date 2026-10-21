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

    /// Test seam: lets a test use a threshold below the 2 s floor.
    #[cfg(test)]
    fn floor(mut self, floor: Duration) -> Self {
        self.floor = floor;
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

    /// Start watching a loop named `name`. Configured secrets are removed first, then letters,
    /// digits, `-`, `_` and `.` are kept, at most 48 characters. It counts as beating now. Drop
    /// every clone of the returned [`Heartbeat`] when the loop ends.
    pub fn heartbeat(&self, name: &str) -> Heartbeat {
        let redacted = crate::report::redact_known(name, &self.shared.secrets);
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
            let _ = capture_native_with_secrets(&dir, "hang", self.config.secrets());
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook::{set_recording_enabled, TEST_SERIAL as SERIAL};
    use std::sync::atomic::AtomicUsize;

    const THRESHOLD: Duration = Duration::from_millis(500);
    const INTERVAL: Duration = Duration::from_millis(20);

    fn temp_dir(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "rightkit-crash-hang-{label}-{}-{nonce}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn fast_options() -> HangOptions {
        HangOptions::new()
            .floor(Duration::from_millis(1))
            .threshold(THRESHOLD)
            .check_interval(INTERVAL)
            .native_capture(false)
    }

    /// The crash folder a test watches; hangs are the `crash_hang_*.json` files in it.
    type Seen = PathBuf;

    fn config_for(dir: &Path) -> (CrashConfig, Seen) {
        (
            CrashConfig::new("test-app", "0.0.0", dir),
            dir.to_path_buf(),
        )
    }

    fn file_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn hangs(seen: &Seen) -> usize {
        file_names(seen)
            .iter()
            .filter(|n| n.starts_with("crash_hang_"))
            .count()
    }

    fn first_message(seen: &Seen) -> String {
        let name = file_names(seen)
            .into_iter()
            .find(|n| n.starts_with("crash_hang_"))
            .expect("a hang report file");
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(seen.join(name)).unwrap()).unwrap();
        value["message"].as_str().unwrap().to_string()
    }

    fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        condition()
    }

    /// Puts recording back on whatever the test did.
    struct RecordingOn;

    impl Drop for RecordingOn {
        fn drop(&mut self) {
            set_recording_enabled(true);
        }
    }

    #[test]
    fn a_callback_can_stop_its_own_watchdog() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let _on = RecordingOn;
        set_recording_enabled(true);
        let dir = temp_dir("self-stop");
        let (config, _seen) = config_for(&dir);
        let slot: Arc<Mutex<Option<HangWatchdog>>> = Arc::new(Mutex::new(None));
        let cb_slot = Arc::clone(&slot);
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let done_tx = Mutex::new(done_tx);
        let options = fast_options().on_hang(move |_| {
            let watchdog = cb_slot.lock().unwrap().take();
            if let Some(watchdog) = watchdog {
                watchdog.stop();
                let _ = done_tx.lock().unwrap().send(());
            }
        });
        let watchdog = HangWatchdog::start(config, options).expect("start");
        let _heartbeat = watchdog.heartbeat("ui");
        *slot.lock().unwrap() = Some(watchdog);
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("stop from the callback returned without joining itself");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_configured_secret_never_survives_in_a_loop_name() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let dir = temp_dir("secret-name");
        let config = CrashConfig::new("test-app", "0.0.0", &dir)
            .redact_values(vec!["zq-secret-value-7781".into()]);
        let watchdog = HangWatchdog::start(config, fast_options()).expect("start");
        let beat = watchdog.heartbeat("ui-zq-secret-value-7781-loop");
        let names: Vec<String> = lock(&watchdog.shared.loops)
            .iter()
            .map(|l| l.name.clone())
            .collect();
        drop(beat);
        watchdog.stop();
        assert_eq!(names.len(), 1);
        assert!(!names[0].contains("zq-secret"), "{}", names[0]);
        assert!(!names[0].contains("7781"), "{}", names[0]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stall_is_reported_exactly_once_per_episode() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let _on = RecordingOn;
        set_recording_enabled(true);
        let dir = temp_dir("once");
        let (config, seen) = config_for(&dir);
        let watchdog = HangWatchdog::start(config, fast_options()).expect("start");
        let _heartbeat = watchdog.heartbeat("ui");

        assert!(wait_until(|| hangs(&seen) == 1), "the stall is reported");
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(hangs(&seen), 1, "one report for the whole episode");
        let message = first_message(&seen);
        assert!(
            message.contains("loop ui"),
            "the report names the loop: {message}"
        );
        let wrote_report = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with("crash_hang_"));
        assert!(wrote_report, "a hang report file is in the crash folder");
        watchdog.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_beat_after_a_stall_re_arms_the_loop() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let _on = RecordingOn;
        set_recording_enabled(true);
        let dir = temp_dir("rearm");
        let (config, seen) = config_for(&dir);
        let watchdog = HangWatchdog::start(config, fast_options()).expect("start");
        let heartbeat = watchdog.heartbeat("ui");

        assert!(wait_until(|| hangs(&seen) == 1));
        heartbeat.beat();
        assert!(
            wait_until(|| hangs(&seen) == 2),
            "a second stall after recovery is a new episode"
        );
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(hangs(&seen), 2);
        watchdog.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_loop_that_keeps_beating_and_a_dropped_heartbeat_never_report() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let _on = RecordingOn;
        set_recording_enabled(true);
        let dir = temp_dir("dropped");
        let (config, seen) = config_for(&dir);
        let watchdog = HangWatchdog::start(config, fast_options()).expect("start");
        let finished = watchdog.heartbeat("finished");
        let finished_clone = finished.clone();
        let alive = watchdog.heartbeat("alive");
        drop(finished);
        drop(finished_clone);

        let until = Instant::now() + Duration::from_millis(600);
        while Instant::now() < until {
            alive.beat();
            std::thread::sleep(Duration::from_millis(30));
        }
        assert_eq!(hangs(&seen), 0, "neither loop is reported");
        assert!(
            lock(&watchdog.shared.loops).len() == 1,
            "only the live loop is watched"
        );
        watchdog.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_is_written_while_recording_is_off() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let _on = RecordingOn;
        let dir = temp_dir("off");
        let logs = temp_dir("off-logs");
        std::fs::create_dir_all(&logs).unwrap();
        let log = logs.join("app.log");
        std::fs::write(&log, "a line\n").unwrap();
        let (config, seen) = config_for(&dir);
        set_recording_enabled(false);
        let watchdog = HangWatchdog::start(config, fast_options().log_file(&log)).expect("start");
        let _heartbeat = watchdog.heartbeat("ui");

        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(hangs(&seen), 0);
        let written = std::fs::read_dir(&dir).map(|d| d.count()).unwrap_or(0);
        assert_eq!(written, 0, "no file while recording is off");

        set_recording_enabled(true);
        assert!(
            wait_until(|| hangs(&seen) == 1),
            "the still-stalled loop is reported once recording is back on"
        );
        watchdog.stop();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&logs);
    }

    #[test]
    fn a_secret_in_a_log_file_never_reaches_the_written_tail() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let _on = RecordingOn;
        set_recording_enabled(true);
        let dir = temp_dir("tail");
        let logs = temp_dir("tail-logs");
        std::fs::create_dir_all(&logs).unwrap();
        let log = logs.join("app.log");
        let secret = "zq-secret-value-7781";
        std::fs::write(
            &log,
            format!("first line\nconnected with {secret} ok\nlast line before the hang\n"),
        )
        .unwrap();
        let (config, _seen) = config_for(&dir);
        let config = config.redact_values(vec![secret.to_string()]);
        let watchdog = HangWatchdog::start(config, fast_options().log_file(&log)).expect("start");
        let _heartbeat = watchdog.heartbeat("ui");

        let find_tail = || {
            std::fs::read_dir(&dir)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .find(|p| {
                    p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with("sample_hang_logs_"))
                })
        };
        assert!(
            wait_until(|| find_tail().is_some()),
            "the log tail is written"
        );
        let written = std::fs::read_to_string(find_tail().unwrap()).unwrap();
        assert!(written.contains("last line before the hang"), "{written}");
        assert!(
            !written.contains(secret),
            "the secret is redacted: {written}"
        );
        watchdog.stop();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&logs);
    }

    #[test]
    fn a_tail_cut_mid_file_drops_its_first_partial_line() {
        let dir = temp_dir("cut");
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("app.log");
        std::fs::write(&log, "0123456789\nabcdef\n").unwrap();
        assert_eq!(tail_text(&log, 10).as_deref(), Some("abcdef\n"));
        assert_eq!(tail_text(&log, 3).as_deref(), Some(""));
        assert_eq!(
            tail_text(&log, 200).as_deref(),
            Some("0123456789\nabcdef\n")
        );
        assert_eq!(tail_text(&dir.join("missing.log"), 10).as_deref(), Some(""));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stop_returns_promptly_even_with_a_long_interval() {
        let dir = temp_dir("stop");
        let (config, _seen) = config_for(&dir);
        let options = fast_options().check_interval(Duration::from_secs(30));
        let watchdog = HangWatchdog::start(config, options).expect("start");
        std::thread::sleep(Duration::from_millis(50));
        let started = Instant::now();
        watchdog.stop();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "stop took {:?}",
            started.elapsed()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pump_via_keeps_at_most_one_closure_in_flight() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let _on = RecordingOn;
        set_recording_enabled(true);
        let dir = temp_dir("pump");
        let (config, seen) = config_for(&dir);
        let watchdog = HangWatchdog::start(config, fast_options()).expect("start");

        type Task = Box<dyn FnOnce() + Send>;
        let queued: Arc<Mutex<Vec<Task>>> = Arc::new(Mutex::new(Vec::new()));
        let posts = Arc::new(AtomicUsize::new(0));
        let (queue, count) = (Arc::clone(&queued), Arc::clone(&posts));
        watchdog
            .pump_via("blocked", move |f| {
                count.fetch_add(1, Ordering::SeqCst);
                queue.lock().unwrap().push(f);
                true
            })
            .expect("pump");

        assert!(wait_until(|| posts.load(Ordering::SeqCst) == 1));
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            posts.load(Ordering::SeqCst),
            1,
            "nothing stacks while blocked"
        );
        assert!(
            wait_until(|| hangs(&seen) == 1),
            "the blocked loop is a hang"
        );

        let first = queued.lock().unwrap().pop().expect("one queued closure");
        first();
        assert!(
            wait_until(|| posts.load(Ordering::SeqCst) == 2),
            "the next closure is posted once the first has run"
        );
        watchdog.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pump_via_on_a_running_loop_never_reports_and_a_closed_loop_is_unwatched() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let _on = RecordingOn;
        set_recording_enabled(true);
        let dir = temp_dir("pump-ok");
        let (config, seen) = config_for(&dir);
        let watchdog = HangWatchdog::start(config, fast_options()).expect("start");
        watchdog
            .pump_via("running", |f| {
                f();
                true
            })
            .expect("pump");
        watchdog.pump_via("closed", |_f| false).expect("pump");

        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(hangs(&seen), 0);
        assert!(wait_until(|| lock(&watchdog.shared.loops).len() == 1));
        watchdog.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn test_monitor(config: CrashConfig, options: HangOptions) -> (Monitor, Arc<Shared>) {
        let shared = Arc::new(Shared {
            epoch: Instant::now(),
            loops: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
            stop: Stop::new(),
            secrets: config.secrets().to_vec(),
        });
        let monitor = Monitor {
            config,
            options: options.resolve(),
            shared: Arc::clone(&shared),
            episodes: HashMap::new(),
            captured: HashMap::new(),
            last_tick_ms: Some(0),
        };
        (monitor, shared)
    }

    /// Registers a loop whose last beat is at the epoch (0 ms), without a watchdog thread.
    fn register(shared: &Arc<Shared>, name: &str) -> Heartbeat {
        let state = Arc::new(LoopState {
            id: shared.next_id.fetch_add(1, Ordering::Relaxed),
            name: name.to_string(),
            epoch: shared.epoch,
            last: AtomicU64::new(0),
            alive: AtomicBool::new(true),
        });
        lock(&shared.loops).push(Arc::clone(&state));
        Heartbeat {
            registration: Arc::new(Registration {
                state,
                shared: Arc::downgrade(shared),
            }),
        }
    }

    /// Checks every 20 ms from `from_ms` to `to_ms`, each within the allowed gap.
    fn drive(monitor: &mut Monitor, from_ms: u64, to_ms: u64) {
        let mut at = from_ms;
        while at <= to_ms {
            monitor.tick_at(at);
            at += 20;
        }
    }

    #[test]
    fn a_late_tick_just_over_the_threshold_is_skipped_not_reported() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let _on = RecordingOn;
        set_recording_enabled(true);
        // Defaults: a suspension of 11 s with a 2.5 s interval is late, though under 12.5 s.
        assert!(is_late(0, 11_000, 2_500));
        assert!(!is_late(0, 2_600, 2_500));

        let dir = temp_dir("late");
        let (config, seen) = config_for(&dir);
        let (mut monitor, shared) = test_monitor(config, fast_options());
        let _heartbeat = register(&shared, "ui");
        // 520 ms since the start: past the 500 ms threshold, but the check itself is 520 ms late
        // against a 20 ms interval, so the thread was not running either.
        monitor.tick_at(520);
        assert_eq!(hangs(&seen), 0, "the late check is skipped");
        // The next on-time check sees a loop that is still silent.
        monitor.tick_at(540);
        assert_eq!(
            hangs(&seen),
            1,
            "a stuck loop is reported at the next check"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_hang_whose_report_was_not_written_is_reported_later() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let _on = RecordingOn;
        set_recording_enabled(true);
        let dir = temp_dir("unwritten");
        let (config, seen) = config_for(&dir);
        let (mut monitor, shared) = test_monitor(config, fast_options());
        let _heartbeat = register(&shared, "ui");
        for now in (20..=500).step_by(20) {
            monitor.tick_at(now);
        }
        assert_eq!(hangs(&seen), 0, "under the threshold");
        // The report write fails at the first check past the threshold.
        crate::hook::FAIL_NEXT_WRITE.store(true, Ordering::SeqCst);
        let failed = catch_unwind(AssertUnwindSafe(|| monitor.tick_at(520)));
        assert!(failed.is_err(), "the injected write failure unwinds");
        assert!(
            monitor.episodes.is_empty(),
            "no episode is committed without its report"
        );
        monitor.tick_at(540);
        assert_eq!(hangs(&seen), 1, "the hang is reported at the next check");
        assert_eq!(monitor.episodes.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_heartbeat_dropped_during_an_earlier_report_is_never_reported() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let _on = RecordingOn;
        set_recording_enabled(true);
        let dir = temp_dir("dropped-mid");
        let (config, seen) = config_for(&dir);
        let slot: Arc<Mutex<Option<Heartbeat>>> = Arc::new(Mutex::new(None));
        let cb_slot = Arc::clone(&slot);
        let fired = Arc::new(AtomicUsize::new(0));
        let cb_fired = Arc::clone(&fired);
        let options = fast_options().on_hang(move |_| {
            cb_fired.fetch_add(1, Ordering::SeqCst);
            // The second loop ends while the first one is being reported.
            drop(cb_slot.lock().unwrap().take());
        });
        let (mut monitor, shared) = test_monitor(config, options);
        let _first = register(&shared, "first");
        *slot.lock().unwrap() = Some(register(&shared, "second"));

        // On-time checks until the threshold passes: the first loop is reported and its callback
        // ends the second loop's heartbeat while that report runs.
        drive(&mut monitor, 20, 600);
        assert!(
            slot.lock().unwrap().is_none(),
            "the callback ran and dropped the heartbeat"
        );
        assert_eq!(fired.load(Ordering::SeqCst), 1);
        assert_eq!(
            hangs(&seen),
            1,
            "only the loop still registered is reported"
        );
        assert!(first_message(&seen).contains("loop first"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_more_is_written_when_the_callback_turns_recording_off() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let _on = RecordingOn;
        set_recording_enabled(true);
        let dir = temp_dir("cb-off");
        let logs = temp_dir("cb-off-logs");
        std::fs::create_dir_all(&logs).unwrap();
        let log = logs.join("app.log");
        std::fs::write(&log, "a line\n").unwrap();
        let (config, _seen) = config_for(&dir);
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let tx = Mutex::new(tx);
        let options = fast_options().log_file(&log).on_hang(move |_| {
            set_recording_enabled(false);
            let _ = tx.lock().unwrap().send(());
        });
        let watchdog = HangWatchdog::start(config, options).expect("start");
        let _heartbeat = watchdog.heartbeat("ui");

        rx.recv_timeout(Duration::from_secs(2))
            .expect("the callback ran");
        // stop joins the thread, so every write it was going to make has happened or never will.
        watchdog.stop();
        let names = file_names(&dir);
        assert!(
            !names.iter().any(|n| n.starts_with("sample_")),
            "no log tail or native capture after consent ended: {names:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&logs);
    }

    #[test]
    fn nothing_is_written_after_stop_returns() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let _on = RecordingOn;
        set_recording_enabled(true);
        let dir = temp_dir("stop-busy");
        let logs = temp_dir("stop-busy-logs");
        std::fs::create_dir_all(&logs).unwrap();
        let log = logs.join("app.log");
        std::fs::write(&log, "a line\n").unwrap();
        let (config, _seen) = config_for(&dir);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
        let entered_tx = Mutex::new(entered_tx);
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release_rx = Mutex::new(release_rx);
        let options = fast_options().log_file(&log).on_hang(move |_| {
            let _ = entered_tx.lock().unwrap().send(());
            // Blocks until the test lets it go (bounded so a failure still ends).
            let _ = release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5));
        });
        let watchdog = HangWatchdog::start(config, options).expect("start");
        let shared = Arc::clone(&watchdog.shared);
        let _heartbeat = watchdog.heartbeat("ui");
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("the report reached its callback");
        // Stop from another thread while the callback is blocked.
        let stopper = std::thread::spawn(move || watchdog.stop());
        assert!(
            wait_until(|| shared.stop.is_stopped()),
            "stop was signalled"
        );
        let before = file_names(&dir);
        release_tx.send(()).unwrap();
        stopper.join().unwrap();
        let after = file_names(&dir);
        assert_eq!(before, after, "nothing was written once stop was signalled");
        assert!(
            !after.iter().any(|n| n.starts_with("sample_")),
            "no capture step ran: {after:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&logs);
    }

    #[test]
    fn a_heartbeat_dropped_inside_its_own_callback_does_not_deadlock() {
        let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let _on = RecordingOn;
        set_recording_enabled(true);
        let dir = temp_dir("own-callback");
        let (config, _seen) = config_for(&dir);
        let slot: Arc<Mutex<Option<Heartbeat>>> = Arc::new(Mutex::new(None));
        let cb_slot = Arc::clone(&slot);
        let (seen_tx, seen_rx) = std::sync::mpsc::channel::<()>();
        let seen_tx = Mutex::new(seen_tx);
        let options = fast_options().on_hang(move |_| {
            drop(cb_slot.lock().unwrap().take());
            let _ = seen_tx.lock().unwrap().send(());
        });
        let watchdog = HangWatchdog::start(config, options).expect("start");
        *slot.lock().unwrap() = Some(watchdog.heartbeat("ui"));
        seen_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("the callback dropped its own heartbeat");
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let stopper = std::thread::spawn(move || {
            watchdog.stop();
            let _ = done_tx.send(());
        });
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("stop returned");
        stopper.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
