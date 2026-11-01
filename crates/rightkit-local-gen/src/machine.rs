// Machine-wide single local-image model server.
//
// Why this exists: one sd-server holds a whole image model in memory (the default qwen
// profile is about 10 GB). Two of them on one Mac exhausted memory, and the two belonged to
// engines with DIFFERENT data folders, so the per-data-folder `sd-server.lock` in runtime.rs
// could not see the other one. This lock therefore lives outside every data folder, in a
// per-user, per-machine directory, and is the single admission gate for starting a server.
//
// Mechanism: an advisory file lock (rightkit-job-queue `JobLocks`, which wraps rightkit-fs
// `FileLock`: flock on Unix, LockFileEx on Windows). The OS releases it when the holder dies,
// SIGKILL included, so a stale lock cannot wedge the machine. A JSON record next to it names
// the holder (pid, model, port, start time, data folder, job) for the busy message, `doctor`
// and orphan reaping; the lock, not the record, decides who holds.
//
// Yield protocol (the holder yields; nobody waits "longer" and nothing unloads "sooner" by a
// timer): a job that cannot get the lock REGISTERS as a waiter (one single-writer file per
// waiter in `sd-server.waiters/`). The holder folds the live waiters into its record
// (`waiters`) and keeps `state` ("busy" while loading or running a job, "idle" while the
// server is warm and unused). An IDLE holder that sees a live waiter unloads at once and
// releases; a BUSY holder keeps the lock and unloads the moment its job ends. With no waiter
// the normal idle-unload timer (300 s) is untouched. Waiters are ordered by registration time
// so the holder's own next job cannot barge ahead of a job that waited for it.
//
// See docs/rightkit-exceptions/M7.md item 2.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use rightkit_job_queue::{JobLock, JobLocks};
use serde::{Deserialize, Serialize};
use serde_json::json;

use rightkit_child_process::AdoptedIdentity;

use crate::runtime::{executable_of, kill_verified_pid, process_alive, rfc3339_now};

/// Typed error code of a job that could not get the model server in time. Not retryable by
/// default: retrying a busy machine in a loop is what exhausted memory.
pub const BUSY_CODE: &str = "local_model_busy";
/// Overrides the lock directory. Tests point it at a scratch folder so they never see (or
/// create) the real machine lock; the real-model tier leaves it unset on purpose.
pub const DIR_ENV: &str = "MACHINE_LOCK_DIR";
/// Seconds a job waits for the lock before failing with [`BUSY_CODE`].
pub const WAIT_ENV: &str = "LOCAL_MODEL_WAIT_SECS";
/// Default wait. A busy holder hands the server over the moment its job ends (see the yield
/// protocol above), so this only has to cover one generation, not a warm-idle period.
pub const DEFAULT_WAIT_SECS: u64 = 120;
const NAME: &str = "sd-server";
const RECORD_FILE: &str = "sd-server.holder.json";
const WAITERS_DIR: &str = "sd-server.waiters";
/// Waiter cadence: starts fast so a freed lock is taken within ~50 ms, backs off to the cap so a
/// long wait costs a handful of syscalls per second. When it is this waiter's turn the wait is
/// `rightkit_job_queue::JobLocks::acquire` for one slice (the published blocking acquire); the
/// slice bound is what lets the loop re-check the cross-process waiter order and the cancel flag.
const POLL_MIN: Duration = Duration::from_millis(50);
const POLL_MAX: Duration = Duration::from_millis(250);
const ANNOUNCE_EVERY: Duration = Duration::from_secs(10);
/// A recorded process start time matches the live one within this window. macOS and Windows
/// report microseconds / 100 ns; Linux derives it from boot time (1 s) plus clock ticks (10 ms).
/// Two seconds covers all three; a recycled pid starts at a different, later time.
const START_TOLERANCE: Duration = Duration::from_secs(2);

/// Whether the holder's server is in use.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum HolderState {
    /// Server warm, no job running: yields at once to a live waiter.
    Idle,
    /// Loading the model or running a job: keeps the lock until the job ends. The safe default
    /// for a record that does not say.
    #[default]
    Busy,
}

impl HolderState {
    pub fn as_str(self) -> &'static str {
        match self {
            HolderState::Idle => "idle",
            HolderState::Busy => "busy",
        }
    }
}

/// A job (or probe) waiting for the lock, as the holder reports it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Waiter {
    pub pid: u32,
    #[serde(default)]
    pub job_id: Option<String>,
    /// Registration time (fixed-width RFC 3339, millisecond UTC: sorts chronologically).
    pub since: String,
    /// The waiter process's own start time; with `pid` it proves the waiter is still the same
    /// live process (a recycled pid is a dead waiter).
    #[serde(default)]
    pub started: Option<String>,
}

/// What the lock holder recorded about itself.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Holder {
    /// The process that holds the lock: the detached runner (headless) or the desktop app.
    pub pid: u32,
    /// The holder process's start time (fixed-width RFC 3339, millisecond UTC). Addition to
    /// the shape: with `pid` it stops a recycled pid from making a dead holder look alive.
    #[serde(default)]
    pub started: Option<String>,
    pub model: String,
    #[serde(default)]
    pub port: Option<u16>,
    /// RFC 3339 start of the hold.
    pub since: String,
    /// `idle` | `busy`. Kept accurate at every transition by the holder.
    #[serde(default)]
    pub state: HolderState,
    /// Jobs waiting for the lock, oldest first; refreshed by the holder (see `MachineLock::refresh`).
    #[serde(default)]
    pub waiters: Vec<Waiter>,
    /// Data folder of the holder, so a job that is "not found" can be traced to its folder.
    pub data_dir: String,
    /// Local-image job id the holder is serving, when known.
    #[serde(default)]
    pub job_id: Option<String>,
    /// The kill target of the server (watchdog/group leader on macOS, sd-server elsewhere).
    #[serde(default)]
    pub server_pid: Option<u32>,
    /// Start time of `server_pid`, written when the server is spawned. Takeover kills a
    /// recorded server only when the pid is alive and this start time still matches (`server_binary`
    /// is informational).
    #[serde(default)]
    pub server_started: Option<String>,
    /// The executable OBSERVED for `server_pid` at spawn (on macOS the watchdog, which is the
    /// engine binary; elsewhere sd-server).
    #[serde(default)]
    pub server_binary: Option<String>,
    #[serde(default)]
    pub holder_binary: Option<String>,
}

impl Holder {
    fn unknown() -> Self {
        Holder {
            pid: 0,
            started: None,
            model: "unknown".into(),
            port: None,
            since: "unknown".into(),
            state: HolderState::Busy,
            waiters: Vec::new(),
            data_dir: String::new(),
            job_id: None,
            server_pid: None,
            server_started: None,
            server_binary: None,
            holder_binary: None,
        }
    }

    /// `pid 123, model qwen21-base-q6, since 2026-10-11T10:00:00Z, state busy, data folder /x, job j`.
    pub fn describe(&self) -> String {
        let mut text = format!(
            "pid {}, model {}, since {}, state {}",
            self.pid,
            self.model,
            self.since,
            self.state.as_str()
        );
        if !self.data_dir.is_empty() {
            text.push_str(&format!(", data folder {}", self.data_dir));
        }
        if let Some(job) = &self.job_id {
            text.push_str(&format!(", job {job}"));
        }
        if !self.waiters.is_empty() {
            text.push_str(&format!(", {} waiting", self.waiters.len()));
        }
        text
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| json!({}))
    }
}

/// What takeover found of a dead holder's server.
#[derive(Debug, Clone, PartialEq)]
pub struct StaleServer {
    pub pid: u32,
    /// True when pid and start time matched and the process was killed.
    pub killed: bool,
    /// Why it was killed or left alone.
    pub reason: String,
}

impl StaleServer {
    pub fn to_json(&self) -> serde_json::Value {
        json!({"pid": self.pid, "killed": self.killed, "reason": self.reason})
    }
}

/// Why the lock was not obtained.
#[derive(Debug)]
pub enum AcquireError {
    /// Another job still holds the server after the wait budget.
    Busy {
        holder: Box<Holder>,
        waited: Duration,
    },
    /// The caller's cancel flag fired while waiting.
    Cancelled,
    Io(String),
}

impl AcquireError {
    /// `local_model_busy: …` / `lock_error: …`, the `code: message` shape the service maps to a job error code.
    pub fn message(&self) -> String {
        match self {
            AcquireError::Busy { holder, waited } => format!(
                "{BUSY_CODE}: another job holds the local image model server ({}); waited {}s; retry when it finishes. One server runs per machine because each loads the whole model into memory",
                holder.describe(),
                waited.as_secs()
            ),
            AcquireError::Cancelled => "cancelled: cancelled while waiting for the local image model server".into(),
            AcquireError::Io(error) => format!("local_model_lock_error: {error}"),
        }
    }
}

/// The per-user, per-machine directory that holds the lock. NOT under any data folder.
///
/// Unix: `/tmp/<machine dir name>-<uid>` (deliberately not `$TMPDIR`/`$HOME`: QA runners and isolated
/// callers remap both, which would hide one engine from another). Windows:
/// `%PROGRAMDATA%\<machine dir name>\runtime` (not remapped by the QA runners, which rewrite
/// `APPDATA`/`LOCALAPPDATA`/`TEMP`).
pub fn machine_dir() -> PathBuf {
    if let Some(dir) = crate::host::env_var(DIR_ENV).filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn getuid() -> u32;
        }
        let uid = unsafe { getuid() };
        PathBuf::from(format!(
            "/tmp/{}-{uid}",
            crate::host::config().machine_dir_name
        ))
    }
    #[cfg(windows)]
    {
        std::env::var_os("PROGRAMDATA")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
            .join(&crate::host::config().machine_dir_name)
            .join("runtime")
    }
}

/// Wait budget: [`WAIT_ENV`] seconds, else [`DEFAULT_WAIT_SECS`]. (The job request has no
/// per-job option for this today; the environment is the caller-settable place.)
pub fn wait_budget() -> Duration {
    let seconds = crate::host::env_var(WAIT_ENV)
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_WAIT_SECS);
    Duration::from_secs(seconds)
}

fn ensure_dir(dir: &Path) -> Result<(), String> {
    rightkit_fs::ensure_private_dir(dir).map_err(|e| format!("create {}: {e}", dir.display()))
}

// ---------------------------------------------------------------- process identity
//
// A pid is only a name until it is paired with the time the process started: a recycled pid has
// the old number and a new start time. The start time comes from rightkit-child-process
// (`AdoptedIdentity`: macOS `proc_pidinfo`, Linux `/proc`, Windows `GetProcessTimes`). Times are
// unix milliseconds internally and fixed-width RFC 3339 UTC (`2026-10-11T10:00:00.123Z`) in
// records, so they read like `since` and sort as text.

/// `2026-10-11T10:00:00.123Z` (fixed width: text order is time order). Civil conversion is rightkit-time's.
fn fmt_ms(ms: i64) -> String {
    match rightkit_time::DateTime::from_unix(ms.div_euclid(1000), 0) {
        Some(t) => format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
            t.year,
            t.month,
            t.day,
            t.hour,
            t.minute,
            t.second,
            ms.rem_euclid(1000)
        ),
        None => "1970-01-01T00:00:00.000Z".to_string(),
    }
}

/// Inverse of [`fmt_ms`]; also reads any RFC 3339 UTC stamp (`Z` or `+00:00`, any fraction), through
/// rightkit-time's parser. A stamp with a non-zero offset is refused.
fn parse_ms(text: &str) -> Option<i64> {
    let t = rightkit_time::DateTime::parse_rfc3339(text)?;
    if t.offset_secs != 0 {
        return None;
    }
    Some(t.unix_seconds() * 1000 + i64::from(t.nanos / 1_000_000))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// The start time of a live process in record form (`None` when dead or unreadable).
pub fn process_started(pid: u32) -> Option<String> {
    // A zombie, or a process that has exited but whose object lingers, is dead (`is_running`),
    // not "live with a start time".
    let identity = AdoptedIdentity::capture(std::num::NonZeroU32::new(pid)?).ok()??;
    if !identity.is_running().ok()? {
        return None;
    }
    let since_epoch = identity
        .start_time()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    Some(fmt_ms(i64::try_from(since_epoch.as_millis()).ok()?))
}

/// The executable of a live process (`None` when dead or unreadable). Informational only.
pub fn process_executable(pid: u32) -> Option<PathBuf> {
    executable_of(pid)
}

/// The live process `pid` when it started within [`START_TOLERANCE`] of `recorded` (record form).
fn recorded_identity(pid: u32, recorded: &str) -> Option<AdoptedIdentity> {
    let pid = std::num::NonZeroU32::new(pid)?;
    let recorded_ms = u64::try_from(parse_ms(recorded)?).ok()?;
    AdoptedIdentity::from_recorded_unix_ms(pid, recorded_ms, START_TOLERANCE).ok()?
}

/// Alive AND, when a start time was recorded, still the same process. Without a recorded start
/// the pid alone is all there is (an old or foreign record).
pub(crate) fn same_process_alive(pid: u32, started: Option<&str>) -> bool {
    if pid == 0 || !process_alive(pid) {
        return false;
    }
    let Some(recorded) = started else { return true };
    recorded_identity(pid, recorded).is_some_and(|identity| identity.is_running().unwrap_or(false))
}

/// The kill rule: a recorded server process is the one we mean only when the pid is alive AND
/// its start time equals the recorded `server_started` (within `START_TOLERANCE`). The start
/// time closes pid reuse. The executable path is not compared: a process that execs keeps its pid
/// and start time (rightkit-child-process `AdoptedIdentity`: pid plus start time IS the identity),
/// so the path is informational only. `Ok` carries the proven identity for the kill; `Err` says
/// which leg failed.
pub fn identity_check(pid: u32, started: Option<&str>) -> Result<AdoptedIdentity, String> {
    if pid == 0 {
        return Err("no pid recorded".into());
    }
    if !process_alive(pid) {
        return Err("not running".into());
    }
    let recorded = started.ok_or_else(|| "record has no server_started".to_string())?;
    let recorded_ms =
        parse_ms(recorded).ok_or_else(|| format!("unreadable recorded start time {recorded:?}"))?;
    if u64::try_from(recorded_ms).is_err() {
        return Err(format!("unreadable recorded start time {recorded:?}"));
    }
    match recorded_identity(pid, recorded) {
        Some(identity) if identity.is_running().unwrap_or(false) => Ok(identity),
        Some(_) => Err("not running".into()),
        None => Err(format!(
            "start time differs from the recorded {recorded} (or is unreadable)"
        )),
    }
}

// ---------------------------------------------------------------- record and waiter files

fn read_record(dir: &Path) -> Option<Holder> {
    serde_json::from_slice(&fs::read(dir.join(RECORD_FILE)).ok()?).ok()
}

fn write_atomic(target: &Path, text: &str) {
    let _ = rightkit_fs::atomic_write(target, text.as_bytes());
}

fn write_record(dir: &Path, holder: &Holder) {
    let text = serde_json::to_string_pretty(holder).unwrap_or_default();
    write_atomic(&dir.join(RECORD_FILE), &text);
}

// RightKit ruling 12b1a381: #11 pending RightKit fix (waiter order)
/// Live waiters, oldest first. `prune` also deletes the entries of waiters that are gone
/// (dead pid, or a live pid with a different start time), which the holder and waiters do; the
/// read-only `doctor` view never deletes.
fn read_waiters(dir: &Path, prune: bool) -> Vec<Waiter> {
    let mut live = Vec::new();
    let Ok(entries) = fs::read_dir(dir.join(WAITERS_DIR)) else {
        return live;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue; // `.tmp` of a write in flight
        }
        let Some(waiter) = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Waiter>(&bytes).ok())
        else {
            continue;
        };
        if same_process_alive(waiter.pid, waiter.started.as_deref()) {
            live.push(waiter);
        } else if prune {
            let _ = fs::remove_file(&path);
        }
    }
    live.sort_by(|a, b| (&a.since, a.pid, &a.job_id).cmp(&(&b.since, b.pid, &b.job_id)));
    live
}

static WAITER_SEQ: AtomicU64 = AtomicU64::new(0);

/// One registered waiter. Single writer by construction: only the waiter writes (and removes)
/// its file, so the holder can read it without any lock. Removed on drop, so a waiter that
/// gives up, is cancelled or panics leaves nothing behind; a waiter that is killed leaves a
/// file that fails the liveness check and is pruned.
struct WaiterGuard {
    path: PathBuf,
    entry: Waiter,
}

impl WaiterGuard {
    fn register(dir: &Path, job_id: Option<&str>) -> Result<Self, String> {
        let waiters = dir.join(WAITERS_DIR);
        ensure_dir(&waiters)?;
        let pid = std::process::id();
        let entry = Waiter {
            pid,
            job_id: job_id.map(str::to_string),
            since: fmt_ms(now_ms()),
            started: process_started(pid),
        };
        let path = waiters.join(format!(
            "{pid}-{}.json",
            WAITER_SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let guard = WaiterGuard { path, entry };
        guard.write();
        if !guard.path.is_file() {
            return Err(format!("register as a waiter in {}", waiters.display()));
        }
        Ok(guard)
    }

    fn write(&self) {
        let text = serde_json::to_string(&self.entry).unwrap_or_default();
        write_atomic(&self.path, &text);
    }

    /// Re-create the entry if something pruned it (a pruner that judged a previous owner of a
    /// recycled pid dead can race our fresh write; this closes that window on the next poll).
    fn ensure_present(&self) {
        if !self.path.is_file() {
            self.write();
        }
    }

    fn is_me(&self, other: &Waiter) -> bool {
        other == &self.entry
    }
}

impl Drop for WaiterGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

// ---------------------------------------------------------------- the lock

/// The held machine-wide lock. Dropping it removes the record and releases the lock.
pub struct MachineLock {
    // Declared first is irrelevant: `Drop::drop` removes the record, then the field drops release the lock.
    _lock: JobLock,
    dir: PathBuf,
    holder: Mutex<Holder>,
    /// The record that a dead previous holder left behind, if any (for diagnostics).
    pub took_over: Option<Holder>,
    /// What takeover did about the dead holder's server, when it recorded one that was still running.
    pub stale_server: Option<StaleServer>,
}

impl MachineLock {
    /// Take the lock, waiting up to `wait` while another live process holds it.
    ///
    /// The caller registers as a waiter for the whole wait, so the holder can see it and yield
    /// (see the protocol above) and so waiters are served oldest first. `on_wait` is called
    /// with the current holder once, then every 10 s, so the caller can publish job progress
    /// ("waiting for the local image model server (in use by another job)"). `cancelled` is
    /// polled on every poll (50 to 250 ms).
    pub fn acquire(
        model: &str,
        data_dir: &Path,
        job_id: Option<&str>,
        wait: Duration,
        mut on_wait: impl FnMut(&Holder),
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<MachineLock, AcquireError> {
        let dir = machine_dir();
        ensure_dir(&dir).map_err(AcquireError::Io)?;
        let locks = JobLocks::new(&dir);
        let ticket = WaiterGuard::register(&dir, job_id).map_err(AcquireError::Io)?;
        let started = Instant::now();
        let mut last_announce: Option<Instant> = None;
        let mut poll = POLL_MIN;
        loop {
            ticket.ensure_present();
            // Oldest live ticket goes first, so the holder's own next job cannot take the lock
            // it just yielded ahead of the job that waited for it.
            let my_turn = read_waiters(&dir, true)
                .first()
                .is_none_or(|oldest| ticket.is_me(oldest));
            let mut blocked_in_lock = false;
            if my_turn {
                let slice = poll.min(wait.saturating_sub(started.elapsed()));
                blocked_in_lock = !slice.is_zero();
                let attempt = if slice.is_zero() {
                    locks.try_lock(NAME)
                } else {
                    locks.acquire(NAME, slice)
                };
                match attempt {
                    Ok(Some(lock)) => {
                        // We hold the lock, so any recorded holder is dead. Its server can still be
                        // alive (an orphan): reap it, but only a process that is provably that server.
                        let stale = read_record(&dir);
                        let stale_server = stale.as_ref().and_then(reap_stale_server);
                        drop(ticket);
                        let pid = std::process::id();
                        let holder = Holder {
                            pid,
                            started: process_started(pid),
                            model: model.to_string(),
                            port: None,
                            since: rfc3339_now(),
                            state: HolderState::Busy, // about to load the model
                            waiters: read_waiters(&dir, true),
                            data_dir: data_dir.to_string_lossy().into_owned(),
                            job_id: job_id.map(str::to_string),
                            server_pid: None,
                            server_started: None,
                            server_binary: None,
                            holder_binary: std::env::current_exe()
                                .ok()
                                .map(|p| p.to_string_lossy().into_owned()),
                        };
                        write_record(&dir, &holder);
                        return Ok(MachineLock {
                            _lock: lock,
                            dir,
                            holder: Mutex::new(holder),
                            took_over: stale,
                            stale_server,
                        });
                    }
                    Ok(None) => {}
                    Err(error) => return Err(AcquireError::Io(error.to_string())),
                }
            }
            if cancelled() {
                return Err(AcquireError::Cancelled);
            }
            let remaining = wait.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(AcquireError::Busy {
                    holder: Box::new(observed_holder(&dir)),
                    waited: started.elapsed(),
                });
            }
            if last_announce.is_none_or(|at| at.elapsed() >= ANNOUNCE_EVERY) {
                on_wait(&observed_holder(&dir));
                last_announce = Some(Instant::now());
            }
            if !blocked_in_lock {
                thread::sleep(poll.min(remaining));
            }
            poll = (poll * 2).min(POLL_MAX);
        }
    }

    /// Record the server this hold now runs: its pid, its start time (read from the OS now, at
    /// spawn) and the executable observed for that pid. The kill rule needs all three.
    pub fn set_server(&self, pid: u32, port: Option<u16>, binary: &Path) {
        let mut holder = self.holder.lock().unwrap();
        holder.server_pid = Some(pid);
        holder.server_started = process_started(pid);
        holder.port = port;
        holder.server_binary = Some(binary.to_string_lossy().into_owned());
        holder.waiters = read_waiters(&self.dir, true);
        write_record(&self.dir, &holder);
    }

    /// Record a state transition: `Busy` when a job starts (or the model loads), `Idle` when
    /// the job is done and the server stays warm.
    pub fn set_state(&self, state: HolderState) {
        let mut holder = self.holder.lock().unwrap();
        if holder.state != state {
            holder.state = state;
            holder.waiters = read_waiters(&self.dir, true);
            write_record(&self.dir, &holder);
        }
    }

    /// Live waiter count. Also folds the live waiters into the on-disk record (rewriting it
    /// only when they changed), so a reader of the record sees at most one call-interval old
    /// data. The holder calls this on its worker tick (500 ms) and on every job poll (750 ms).
    pub fn refresh(&self) -> usize {
        let live = read_waiters(&self.dir, true);
        let mut holder = self.holder.lock().unwrap();
        if holder.waiters != live {
            holder.waiters = live;
            write_record(&self.dir, &holder);
        }
        holder.waiters.len()
    }

    /// The yield rule: the server is idle and a live waiter exists. The caller then unloads
    /// the server at once and drops this lock. A busy holder never yields mid-job; it asks
    /// again when the job ends. A dead or departed waiter never counts (see `read_waiters`).
    pub fn yield_due(&self) -> bool {
        let waiting = self.refresh();
        waiting > 0 && self.holder.lock().unwrap().state == HolderState::Idle
    }

    pub fn holder(&self) -> Holder {
        self.holder.lock().unwrap().clone()
    }
}

impl Drop for MachineLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.dir.join(RECORD_FILE));
    }
}

/// Kill the server a dead holder recorded, but only a process that is provably that server
/// (see [`identity_check`]). Anything else that is still running under the recorded pid is
/// left alone and reported, never signalled.
fn reap_stale_server(old: &Holder) -> Option<StaleServer> {
    let pid = old.server_pid?;
    if pid == 0 || pid == std::process::id() || !process_alive(pid) {
        return None;
    }
    Some(match identity_check(pid, old.server_started.as_deref()) {
        Ok(_) => {
            // Re-verify immediately before the kill so the pid-reuse window is as small as it can
            // be, then report `killed` only once the process is confirmed gone (the kill waits
            // up to 10 s). Otherwise say so: the lock is still taken over.
            match identity_check(pid, old.server_started.as_deref()) {
                Ok(identity) if kill_verified_pid(&identity) => StaleServer {
                    pid,
                    killed: true,
                    reason: "pid and start time match the recorded server".into(),
                },
                Ok(_) => StaleServer {
                    pid,
                    killed: false,
                    reason: "pid and start time match the recorded server; kill requested but the process is still running".into(),
                },
                Err(reason) => StaleServer { pid, killed: false, reason: format!("left running, not the recorded server: {reason}") },
            }
        }
        Err(reason) => StaleServer {
            pid,
            killed: false,
            reason: format!("left running, not the recorded server: {reason}"),
        },
    })
}

/// The record with its waiters re-read from the live waiter files.
fn observed_holder(dir: &Path) -> Holder {
    let mut holder = read_record(dir).unwrap_or_else(Holder::unknown);
    holder.waiters = read_waiters(dir, false);
    holder
}

/// Who holds the machine-wide lock right now (for `doctor`). `None` when it is free. A
/// leftover record of a dead holder is not reported as a holder. Read-only: creates nothing.
pub fn current_holder() -> Option<Holder> {
    let dir = machine_dir();
    if !dir.is_dir() {
        return None;
    }
    match JobLocks::new(&dir).try_lock(NAME) {
        Ok(Some(_free)) => None,
        Ok(None) => Some(observed_holder(&dir)),
        Err(_) => None,
    }
}

/// `doctor` section: lock directory, state, holder and whether the holder's processes are alive.
/// `state` is the lock (`free` | `held`); `holder_state` is the holder's `idle` | `busy`.
pub fn report() -> serde_json::Value {
    let dir = machine_dir();
    match current_holder() {
        None => json!({"lock_dir": dir.to_string_lossy(), "state": "free", "holder": null,
            "holder_state": null, "waiters": 0, "server_started": null,
            "wait_secs": wait_budget().as_secs()}),
        Some(holder) => json!({
            "lock_dir": dir.to_string_lossy(),
            "state": "held",
            "holder_state": holder.state.as_str(),
            "waiters": holder.waiters.len(),
            "server_started": holder.server_started,
            "holder": holder.to_json(),
            "holder_alive": same_process_alive(holder.pid, holder.started.as_deref()),
            "server_alive": holder.server_pid.map(|pid| match holder.server_started.as_deref() {
                Some(started) => same_process_alive(pid, Some(started)),
                None => process_alive(pid),
            }),
            "wait_secs": wait_budget().as_secs(),
        }),
    }
}
