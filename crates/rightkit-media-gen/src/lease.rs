//! Caller leases for local-image jobs.
//!
//! A local image job loads a ~10 GB model server inside a detached `job run` runner. The runner
//! used to be tied to nothing: when the process that asked for the picture (`generate --wait`,
//! `brief run`, the desktop sidecar) died, the runner and its model server kept running as
//! orphans. A lease ties them to the asker.
//!
//! - A **waiting caller** (`generate --wait`, `brief run`, `job wait`) takes the `rightkit_fs::Lease`
//!   at `<data>/locks/leases/<job_id>.lease` (30 s TTL) and renews it every 5 s while it waits.
//!   If another live holder already has the lease (the resident sidecar, or an earlier waiter),
//!   the caller does not displace it: that holder keeps the job guarded. RightKit's lease has no
//!   "newest caller takes over" step; a stale holder (pid confirmed dead, or heartbeat past the
//!   TTL) is taken over by the next `try_acquire`.
//! - The **resident sidecar** (the desktop app's engine) holds a lease for every local job it
//!   starts or resumes, so quitting the app releases the model. Its keeper re-acquires a lease
//!   that was released or went stale while the job still runs.
//! - The **runner** (`job run`) watches the lease of its own local-image job every 2 s with
//!   `Lease::peek` + `LeaseInfo::staleness`. A holder is dead when RightKit confirms its pid
//!   dead or its heartbeat is older than the TTL. If no live holder re-attaches within 60 s of
//!   death, the runner cancels the job (`job cancel`, the supported path that stops the model
//!   server) and, if it is still alive 60 s later, exits hard. The server then dies with the
//!   runner (watchdog / kill-on-parent-exit / Windows Job Object).
//!
//! Scope: only `local-image` jobs. Remote (paid) jobs keep the "start, quit, resume later"
//! behaviour untouched. A job with no lease file (plain `generate` without `--wait`, whose caller
//! is gone by design) is not watched; a waiting caller that stops waiting normally (finished, or
//! its `--wait-timeout` expired) removes the lease and leaves the job running as before.
//!
//! Mechanism: `rightkit_fs::Lease` (rightkit-fs 0.4.5, feature `fs`): pid + token + heartbeat TTL.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::{store, Context};
use rightkit_fs::{Acquire, FsError, Lease as FsLease, LeaseInfo, LeaseOptions};
use serde_json::json;

const HEARTBEAT_SECS: u64 = 5;
/// A holder whose heartbeat is older than this is dead even if its pid answers (pid reuse).
const TTL_SECS: u64 = 30;
const WATCH_POLL: Duration = Duration::from_secs(2);
/// Seconds a dead lease may wait for a new holder before the runner cancels the job.
pub const GRACE_ENV: &str = "RIGHTKIT_LOCAL_LEASE_GRACE_SECS";
const DEFAULT_GRACE_SECS: u64 = 60;
/// How long a cancelled runner may take to exit before it exits hard.
const CANCEL_BUDGET: Duration = Duration::from_secs(60);

static RESIDENT: AtomicBool = AtomicBool::new(false);

/// The `--stdio` sidecar calls this: it outlives CLI calls, so it holds leases for the jobs it starts.
pub fn mark_resident() {
    RESIDENT.store(true, Ordering::SeqCst);
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn safe_id(job_id: &str) -> bool {
    !job_id.is_empty() && !job_id.contains(['/', '\\', '.'])
}

fn lease_path(ctx: &Context, job_id: &str) -> PathBuf {
    ctx.locks_dir()
        .join("leases")
        .join(format!("{job_id}.lease"))
}

fn runner_path(ctx: &Context, job_id: &str) -> PathBuf {
    ctx.locks_dir()
        .join("runners")
        .join(format!("{job_id}.json"))
}

/// Take the lease at `path`, or `None` when another live holder has it (it keeps guarding the
/// job). A stale holder is taken over inside `try_acquire`. A contended attempt (`WouldBlock`)
/// is retried once; a caller that still loses simply holds nothing and tries again later.
fn acquire_lease(path: &Path, kind: &str) -> Option<FsLease> {
    let options = LeaseOptions::new(Duration::from_secs(TTL_SECS)).label(kind);
    for _ in 0..2 {
        match FsLease::try_acquire(path, &options) {
            Ok(Acquire::Acquired(lease)) => return Some(lease),
            Ok(Acquire::Held(_)) => return None,
            Err(_) => {}
        }
    }
    None
}

/// A renew that failed because the lease is lost (record gone or taken over), as opposed to a
/// retryable `WouldBlock` while another process changes the record.
fn renew_lost(error: &FsError) -> bool {
    error.kind() != Some(std::io::ErrorKind::WouldBlock)
}

fn holder_alive(info: &LeaseInfo) -> bool {
    !info.staleness().is_stale()
}

/// The job exists, runs on the local image provider, and is not finished.
fn live_local_image_job(ctx: &Context, job_id: &str) -> bool {
    store::open(ctx).ok().is_some_and(|db| live_in(&db, job_id))
}

fn live_in(db: &store::Db, job_id: &str) -> bool {
    store::get_job(db, job_id).ok().is_some_and(|job| {
        job.provider == "local-image" && !job.is_terminal() && !job.awaits_resolution()
    })
}

/// A lease held by this process while it waits for a job. Dropping it releases the lease.
pub struct Lease {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Lease {
    /// Become the job's lease holder. `None` for jobs that need no lease (not a live local-image
    /// job) and when another live holder already guards the job.
    pub fn attach(ctx: &Context, job_id: &str, kind: &'static str) -> Option<Lease> {
        if !safe_id(job_id) || !live_local_image_job(ctx, job_id) {
            return None;
        }
        let mut lease = acquire_lease(&lease_path(ctx, job_id), kind)?;
        let stop = Arc::new(AtomicBool::new(false));
        let beat_stop = stop.clone();
        let thread = thread::Builder::new()
            .name("rightkit-lease".into())
            .spawn(move || {
                let mut slept = 0u64;
                while !beat_stop.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_secs(1));
                    slept += 1;
                    if slept >= HEARTBEAT_SECS {
                        slept = 0;
                        // `renew` refuses a record that is no longer ours: stop beating a lost lease.
                        if lease.renew().is_err_and(|e| renew_lost(&e)) {
                            break;
                        }
                    }
                }
                // Dropping the lease removes the record when it is still this holder's.
                drop(lease);
            })
            .ok();
        Some(Lease { stop, thread })
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Kept {
    ctx: Context,
    job_id: String,
    /// `None` while another live holder has the lease; the keeper retries every beat.
    lease: Option<FsLease>,
}

fn kept() -> &'static Mutex<Vec<Kept>> {
    static KEPT: OnceLock<Mutex<Vec<Kept>>> = OnceLock::new();
    KEPT.get_or_init(|| Mutex::new(Vec::new()))
}

/// Resident sidecar only: hold a lease for `job_id` until the job finishes.
pub fn keep(ctx: &Context, job_id: &str) {
    if !RESIDENT.load(Ordering::SeqCst) || !safe_id(job_id) || !live_local_image_job(ctx, job_id) {
        return;
    }
    {
        let mut list = kept().lock().unwrap();
        if list
            .iter()
            .any(|k| k.job_id == job_id && k.ctx.data_dir == ctx.data_dir)
        {
            return;
        }
        let lease = acquire_lease(&lease_path(ctx, job_id), "app");
        list.push(Kept {
            ctx: ctx.clone(),
            job_id: job_id.to_string(),
            lease,
        });
    }
    static KEEPER: OnceLock<()> = OnceLock::new();
    KEEPER.get_or_init(|| {
        let _ = thread::Builder::new()
            .name("rightkit-lease-keeper".into())
            .spawn(|| loop {
                thread::sleep(Duration::from_secs(HEARTBEAT_SECS));
                let mut list = kept().lock().unwrap();
                // A finished job drops its entry, and with it the lease (released on drop).
                list.retain_mut(|k| {
                    if !live_local_image_job(&k.ctx, &k.job_id) {
                        return false;
                    }
                    match k.lease.as_mut() {
                        Some(lease) => {
                            if lease.renew().is_err_and(|e| renew_lost(&e)) {
                                k.lease = None;
                            }
                        }
                        // Do not fight a different live holder (a `job wait` that attached); take the
                        // lease back once that holder released it or went stale.
                        None => k.lease = acquire_lease(&lease_path(&k.ctx, &k.job_id), "app"),
                    }
                    true
                });
            });
    });
}

/// Registered by a running `job run`: records the runner pid (so a test harness can end a stuck
/// runner by pid) and, for a local-image job with a caller lease, watches that lease.
pub struct RunnerGuard {
    pid_file: Option<PathBuf>,
}

impl Drop for RunnerGuard {
    fn drop(&mut self) {
        if let Some(path) = self.pid_file.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub fn watch_runner(ctx: &Context, job_id: &str) -> RunnerGuard {
    let mut guard = RunnerGuard { pid_file: None };
    if !safe_id(job_id) {
        return guard;
    }
    let pid_file = runner_path(ctx, job_id);
    if let Some(dir) = pid_file.parent() {
        let _ = rightkit_fs::ensure_private_dir(dir);
    }
    let record = json!({"pid": std::process::id(), "started_unix": now_unix()});
    if rightkit_fs::atomic_write(&pid_file, record.to_string().as_bytes()).is_ok() {
        guard.pid_file = Some(pid_file);
    }
    if live_local_image_job(ctx, job_id) {
        let (ctx, job_id) = (ctx.clone(), job_id.to_string());
        let _ = thread::Builder::new()
            .name("rightkit-lease-watch".into())
            .spawn(move || watch(&ctx, &job_id));
    }
    guard
}

fn grace() -> Duration {
    let seconds = std::env::var(GRACE_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_GRACE_SECS);
    Duration::from_secs(seconds)
}

fn event(name: &str, job_id: &str, extra: serde_json::Value) {
    let mut fields: Vec<(&str, serde_json::Value)> = vec![
        ("job_id", json!(job_id)),
        ("pid", json!(std::process::id())),
    ];
    if let Some(source) = extra.as_object() {
        fields.extend(
            source
                .iter()
                .map(|(key, value)| (key.as_str(), value.clone())),
        );
    }
    crate::host::diag::info(name, &fields);
}

fn watch(ctx: &Context, job_id: &str) {
    let path = lease_path(ctx, job_id);
    let mut dead_since: Option<Instant> = None;
    // One connection for the whole watch: the job store is opened (and its schema checked) once.
    let Ok(db) = store::open(ctx) else { return };
    loop {
        thread::sleep(WATCH_POLL);
        if !live_in(&db, job_id) {
            return;
        }
        match FsLease::peek(&path) {
            // No lease: the job was started without a waiting caller, or its caller finished waiting.
            // An unreadable record directory is no evidence of a dead holder either.
            Ok(None) | Err(_) => dead_since = None,
            Ok(Some(lease)) if holder_alive(&lease) => dead_since = None,
            Ok(Some(lease)) => {
                let since = *dead_since.get_or_insert_with(|| {
                    event(
                        "lease.holder_gone",
                        job_id,
                        json!({"holder_pid": lease.pid, "kind": lease.label}),
                    );
                    Instant::now()
                });
                if since.elapsed() >= grace() {
                    expire(job_id, &lease);
                }
            }
        }
    }
}

/// The holder is gone and nobody re-attached: cancel the job (stops the model server) and make
/// sure this runner does not outlive it.
fn expire(job_id: &str, lease: &LeaseInfo) -> ! {
    event(
        "lease.expired",
        job_id,
        json!({"holder_pid": lease.pid, "action": "cancel job, stop model server, exit runner"}),
    );
    if let Ok(exe) = std::env::current_exe() {
        // Owned, hidden and bounded: a cancel that outlives the budget is killed as a tree.
        let _ = rightkit_child_process::run(
            exe,
            ["job", "cancel", job_id],
            &rightkit_child_process::RunOptions::new(CANCEL_BUDGET),
        );
    }
    // The main thread normally ends the process once the cancelled job settles. If it has not
    // within the budget, exit hard: the server's watchdog / Job Object takes it down with us.
    let deadline = Instant::now() + CANCEL_BUDGET;
    while Instant::now() < deadline {
        thread::sleep(Duration::from_secs(1));
    }
    event(
        "lease.forced_exit",
        job_id,
        json!({"reason": "runner did not exit after cancel"}),
    );
    std::process::exit(75);
}
