//! Owned processes: every child is spawned through `rightkit-child-process` (process
//! group / Job Object), registered with a [`Tracker`], and killed by recorded pid
//! only. Nothing here ever matches processes by name for killing.
use crate::util::{err, Result};
#[cfg(windows)]
use rightkit_child_process::WindowsJob;
use rightkit_child_process::{OwnedChild, OwnedCommand, ReapOutcome};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Longest a kill helper (`taskkill`, `kill`, `pgrep`, ...) may run before it is abandoned.
pub const KILL_HELPER_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest a process-tree termination may wait to reap the leader.
pub const TERMINATE_REAP_TIMEOUT: Duration = Duration::from_secs(5);
/// After a helper is killed for overrunning, how long to wait for it to be reaped.
const HELPER_REAP: Duration = Duration::from_secs(2);

fn poll_exit(
    child: &mut std::process::Child,
    deadline: Instant,
) -> std::io::Result<Option<ExitStatus>> {
    loop {
        if let Some(st) = child.try_wait()? {
            return Ok(Some(st));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

/// Kill an overrunning helper and reap it, bounded.
fn abandon(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = poll_exit(child, Instant::now() + HELPER_REAP);
}

/// Run a command to completion for its exit status, waiting at most `timeout`. The command
/// is killed when it overruns. `Ok(None)` means it timed out. The caller sets stdio.
pub fn status_bounded(cmd: &mut Command, timeout: Duration) -> std::io::Result<Option<ExitStatus>> {
    let mut child = cmd.spawn()?;
    match poll_exit(&mut child, Instant::now() + timeout)? {
        Some(st) => Ok(Some(st)),
        None => {
            abandon(&mut child);
            Ok(None)
        }
    }
}

/// Run a command for its captured output, waiting at most `timeout` (pipe draining is bounded
/// too, so a descendant holding a pipe open cannot hang the caller). `Ok(None)` means it
/// timed out and was killed. Stdin is null.
pub fn output_bounded(
    cmd: &mut Command,
    timeout: Duration,
) -> std::io::Result<Option<std::process::Output>> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let drain = |stream: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut s) = stream {
                let _ = s.read_to_end(&mut buf);
            }
            let _ = tx.send(buf);
        });
        rx
    };
    let out_rx = drain(
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let err_rx = drain(
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    match poll_exit(&mut child, Instant::now() + timeout)? {
        Some(status) => Ok(Some(std::process::Output {
            status,
            stdout: out_rx.recv_timeout(HELPER_REAP).unwrap_or_default(),
            stderr: err_rx.recv_timeout(HELPER_REAP).unwrap_or_default(),
        })),
        None => {
            abandon(&mut child);
            Ok(None)
        }
    }
}

/// Bounded `status().success()` with null stdio; a spawn failure or timeout is `false`.
#[cfg(unix)]
fn quiet_ok(cmd: &mut Command) -> bool {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    matches!(status_bounded(cmd, KILL_HELPER_TIMEOUT), Ok(Some(s)) if s.success())
}

/// Run a kill helper (e.g. `taskkill`) bounded by `timeout`. When it cannot finish (it
/// overran, or could not start) `fallback(pid)` runs instead (`TerminateProcess` on Windows)
/// and a note for the run log is returned; `None` means the helper finished. Never blocks
/// longer than `timeout` plus the helper reap.
pub fn kill_with_fallback(
    helper: &mut Command,
    pid: u32,
    timeout: Duration,
    fallback: impl FnOnce(u32) -> bool,
) -> Option<String> {
    helper
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let why = match status_bounded(helper, timeout) {
        Ok(Some(_)) => return None,
        Ok(None) => format!("timed out after {:.1}s", timeout.as_secs_f32()),
        Err(e) => format!("could not run ({e})"),
    };
    let ok = fallback(pid);
    Some(format!(
        "[rightkit-qa] kill helper for pid {pid} {why}; fell back to direct termination of the known pid ({})",
        if ok { "terminated" } else { "not terminated" }
    ))
}

/// `TerminateProcess` on one pid (no descendants; the owning Job Object takes those).
#[cfg(windows)]
fn terminate_pid(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    unsafe {
        let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if h.is_null() {
            return false;
        }
        let ok = TerminateProcess(h, 1) != 0;
        CloseHandle(h);
        ok
    }
}

#[cfg(windows)]
fn taskkill_tree(pid: u32) -> Option<String> {
    kill_with_fallback(
        hidden(Command::new("taskkill.exe").args(["/PID", &pid.to_string(), "/T", "/F"])),
        pid,
        KILL_HELPER_TIMEOUT,
        terminate_pid,
    )
}

/// How long a Job Object is given to empty after it was terminated, before its survivors are
/// handed to `taskkill`.
#[cfg(windows)]
const JOB_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a job is given to empty by itself after its leader exited, before what is left in it
/// counts as descendants that outlived the command. A console child's `conhost.exe` is a job
/// member that exits a moment after the leader; polling this long keeps it from being reported as
/// an orphan, while a real descendant (still running when the grace ends) is recorded as before.
#[cfg(windows)]
const JOB_EXIT_GRACE: Duration = Duration::from_millis(250);

/// Why a group is being settled: the leader exited on its own, or the whole tree was already
/// terminated (deadline). Descendants seen after a deliberate termination are only draining, so
/// they are not reported as having outlived the command.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GroupEnd {
    Exited,
    Terminated,
}

/// The kill-on-close Job Object `run_owned` places a child in (Windows), or nothing.
#[cfg(windows)]
type JobSlot = Option<WindowsJob>;
#[cfg(unix)]
type JobSlot = ();

/// What [`settle_group`] needs from the platform: the job's membership and termination, process
/// liveness, and the `taskkill` fallback. A seam so the decision logic runs against a fake.
#[cfg(windows)]
trait GroupOps {
    fn has_job(&self) -> bool;
    /// Pids currently in the job (leader included while it is still listed).
    fn members(&self) -> std::io::Result<Vec<u32>>;
    fn terminate(&self) -> std::io::Result<()>;
    fn alive(&self, pid: u32) -> bool;
    /// `taskkill /PID <pid> /T /F`, bounded, with direct termination as its own fallback.
    fn taskkill(&mut self, pid: u32) -> Option<String>;
}

#[cfg(windows)]
struct WinOps<'a> {
    job: Option<&'a WindowsJob>,
}

#[cfg(windows)]
impl GroupOps for WinOps<'_> {
    fn has_job(&self) -> bool {
        self.job.is_some()
    }
    fn members(&self) -> std::io::Result<Vec<u32>> {
        match self.job {
            Some(job) => job.process_ids(),
            None => Err(std::io::Error::other("no job object")),
        }
    }
    fn terminate(&self) -> std::io::Result<()> {
        match self.job {
            Some(job) => job.terminate(),
            None => Err(std::io::Error::other("no job object")),
        }
    }
    fn alive(&self, pid: u32) -> bool {
        is_alive(pid)
    }
    fn taskkill(&mut self, pid: u32) -> Option<String> {
        taskkill_tree(pid)
    }
}

/// Make sure nothing a finished (or terminated) child started is left running, without
/// spawning a helper when the Job Object proves there is nothing to do.
///
/// - No job: the leader is recorded as a survivor (the tree cannot be listed), then `taskkill`.
/// - Job query fails: the leader is recorded as a survivor with the error, then `taskkill`.
/// - Job holds nobody but the leader (the common case): nothing runs at all.
/// - Job holds others after [`GroupEnd::Exited`]: the job is polled for up to `grace` for them to
///   leave on their own (the console host of a console child does), and only members still there
///   are descendants. `grace` is not spent after a deliberate termination.
/// - Job holds descendants: for [`GroupEnd::Exited`] each is recorded in the tracker as an orphan
///   first, the job is terminated in-process (`TerminateJobObject`), and the job is given
///   `drain` to empty. A member that survives that is handed to `taskkill`, and stays
///   registered when even that did not stop it, so `assert_no_orphans` reports it.
///
/// The leader's own pid is ignored when it is still listed: the caller holds its process
/// handle, so the id cannot have been reused, and it has exited or been terminated.
#[cfg(windows)]
fn settle_group(
    ops: &mut dyn GroupOps,
    leader: u32,
    label: &str,
    tracker: &Tracker,
    end: GroupEnd,
    grace: Duration,
    drain: Duration,
) -> Vec<String> {
    let mut notes = Vec::new();
    if !ops.has_job() {
        // No job means nothing can list the tree, so the kill below cannot be proven to be a
        // no-op: record the leader first, so the scenario is errored rather than silently swept.
        let why = format!(
            "[rightkit-qa] {label} (pid {leader}) has no job object; its tree cannot be listed before taskkill, recorded as a survivor"
        );
        tracker.record_survivor(leader, &why);
        notes.push(why);
        notes.extend(ops.taskkill(leader));
        return notes;
    }
    let others = |ids: Vec<u32>| -> Vec<u32> { ids.into_iter().filter(|p| *p != leader).collect() };
    let mut members = match ops.members() {
        Ok(ids) => others(ids),
        Err(e) => {
            let why = format!(
                "[rightkit-qa] could not list the job object of pid {leader} ({e}); recorded as a survivor, then cleaning its tree with taskkill"
            );
            tracker.record_survivor(leader, &why);
            notes.push(why);
            notes.extend(ops.taskkill(leader));
            return notes;
        }
    };
    if !members.is_empty() && end == GroupEnd::Exited && !grace.is_zero() {
        let grace_end = Instant::now() + grace;
        while !members.is_empty() && Instant::now() < grace_end {
            std::thread::sleep(Duration::from_millis(10));
            // A failed re-query keeps the last snapshot: the checks below decide with it.
            if let Ok(ids) = ops.members() {
                members = others(ids);
            }
        }
    }
    if members.is_empty() {
        return notes;
    }
    let orphan_label = format!("descendant of {label} (pid {leader})");
    let mut recorded: Vec<u32> = Vec::new();
    if end == GroupEnd::Exited {
        for pid in &members {
            tracker.register(*pid, &orphan_label);
            // Permanent: the job is terminated below, and the scenario must still see these as
            // descendants that outlived the command (`errored`), not as nothing.
            tracker.record_survivor(*pid, &orphan_label);
        }
        recorded = members.clone();
    }
    if let Err(e) = ops.terminate() {
        notes.push(format!(
            "[rightkit-qa] terminating the job object of pid {leader} failed ({e})"
        ));
    }
    let deadline = Instant::now() + drain;
    loop {
        match ops.members() {
            Ok(ids) => members = others(ids),
            Err(_) => members.retain(|p| ops.alive(*p)),
        }
        if members.is_empty() || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    for pid in &recorded {
        if !members.contains(pid) {
            tracker.forget(*pid);
        }
    }
    if !recorded.is_empty() {
        let list = recorded
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        notes.push(format!(
            "[rightkit-qa] {} descendant process(es) of pid {leader} outlived it; {}: {list}",
            recorded.len(),
            if members.is_empty() {
                "terminated with its job"
            } else {
                "some survived job termination"
            }
        ));
    }
    for pid in &members {
        tracker.register(*pid, &orphan_label);
        notes.extend(ops.taskkill(*pid));
        if ops.alive(*pid) {
            notes.push(format!(
                "[rightkit-qa] pid {pid} is still running after job termination and taskkill; left registered as an orphan"
            ));
        } else {
            tracker.forget(*pid);
        }
    }
    notes
}

/// Spawn `owned` suspended, place it in a kill-on-close [`WindowsJob`] (nested inside the
/// child's own retained job), then release it, so no descendant can be created before the job
/// can see it. When the job cannot be created or joined the child runs without one and the
/// returned note says so (cleanup then uses `taskkill`).
#[cfg(windows)]
fn spawn_in_job(owned: OwnedCommand) -> std::io::Result<(OwnedChild, JobSlot, Option<String>)> {
    let child = owned.spawn_suspended()?;
    let pid = child.id();
    let (job, note) =
        match WindowsJob::new(None).and_then(|job| job.assign_process_id(pid).map(|()| job)) {
            Ok(job) => (Some(job), None),
            Err(e) => (
                None,
                Some(format!(
                    "[rightkit-qa] pid {pid} could not be placed in a job object ({e}); its tree is cleaned with taskkill"
                )),
            ),
        };
    // On failure the child is dropped here, which terminates it.
    child.resume()?;
    Ok((child, job, note))
}

/// Terminate an owned child's tree, waiting at most [`TERMINATE_REAP_TIMEOUT`] for the leader to
/// be reaped. Returns the exit status when reaped and a run-log note when it was not.
pub fn terminate_bounded(child: &mut OwnedChild) -> (Option<ExitStatus>, Option<String>) {
    match child.terminate_tree_bounded(TERMINATE_REAP_TIMEOUT) {
        Ok(ReapOutcome::Exited(st)) => (Some(st), None),
        Ok(ReapOutcome::TimedOut) => (
            None,
            Some(format!(
                "[rightkit-qa] pid {} was not reaped within {}s of tree termination; moving on",
                child.id(),
                TERMINATE_REAP_TIMEOUT.as_secs()
            )),
        ),
        Err(e) => (
            None,
            Some(format!(
                "[rightkit-qa] tree termination of pid {} failed: {e}",
                child.id()
            )),
        ),
    }
}

pub fn is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        if !quiet_ok(Command::new("kill").args(["-0", &pid.to_string()])) {
            return false;
        }
        // A zombie has exited and accepts signals, but is not running. It stays a zombie until
        // its parent reaps it, which may never happen (a parent that forgot its child), so it
        // must not count as a survivor.
        let stat = output_bounded(
            Command::new("ps").args(["-o", "stat=", "-p", &pid.to_string()]),
            KILL_HELPER_TIMEOUT,
        );
        !matches!(
            stat,
            Ok(Some(o)) if String::from_utf8_lossy(&o.stdout).trim_start().starts_with('Z')
        )
    }
    #[cfg(windows)]
    {
        // Native query: spawning `tasklist.exe` cost several hundred ms per call and was made a
        // dozen times per `stop` (2.8 s of a 3 s stop). Fail closed: a process that exists but cannot
        // be opened (access denied) counts as alive.
        use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ACCESS_DENIED};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        const STILL_ACTIVE: u32 = 259;
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return GetLastError() == ERROR_ACCESS_DENIED;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(h, &mut code) != 0;
            CloseHandle(h);
            !ok || code == STILL_ACTIVE
        }
    }
}

#[cfg(windows)]
fn hidden(cmd: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000)
}

#[cfg(unix)]
fn children_of(pid: u32) -> Vec<u32> {
    let out = output_bounded(
        Command::new("pgrep").args(["-P", &pid.to_string()]),
        KILL_HELPER_TIMEOUT,
    );
    match out {
        Ok(Some(o)) => String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect(),
        _ => Vec::new(),
    }
}

/// Terminate `pid` and its descendants. TERM first, KILL after a short grace.
pub fn kill_tree(pid: u32) {
    if !is_alive(pid) {
        return;
    }
    #[cfg(windows)]
    {
        if let Some(note) = taskkill_tree(pid) {
            eprintln!("{note}");
        }
    }
    #[cfg(unix)]
    {
        let mut all = vec![pid];
        let mut i = 0;
        while i < all.len() {
            let kids = children_of(all[i]);
            all.extend(kids);
            i += 1;
        }
        for sig in ["-TERM", "-KILL"] {
            for p in all.iter().rev() {
                let _ = quiet_ok(Command::new("kill").args([sig, &p.to_string()]));
            }
            let deadline = std::time::Instant::now() + Duration::from_millis(1500);
            while std::time::Instant::now() < deadline && all.iter().any(|p| is_alive(*p)) {
                crate::util::sleep_ms(25);
            }
            if !all.iter().any(|p| is_alive(*p)) {
                break;
            }
        }
    }
}

/// Pids of the process group `pgid` (zombies excluded), or `None` when it cannot be listed. An
/// empty group is `Some(vec![])`.
#[cfg(unix)]
fn group_members(pgid: u32) -> Option<Vec<u32>> {
    let out = output_bounded(
        Command::new("pgrep").args(["-g", &pgid.to_string()]),
        KILL_HELPER_TIMEOUT,
    )
    .ok()??;
    match out.status.code() {
        Some(0) => Some(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|l| l.trim().parse::<u32>().ok())
                .filter(|p| is_alive(*p))
                .collect(),
        ),
        // pgrep exits 1 when nothing matched.
        Some(1) => Some(Vec::new()),
        _ => None,
    }
}

/// Unix: record the members of the group led by `leader` (the leader excluded) as survivors BEFORE
/// the group is killed, polling up to `grace` for members that leave by themselves. A group that
/// cannot be listed fails closed (the leader's pid is recorded naming the failure). Returns notes
/// for the command's run log.
#[cfg(unix)]
fn record_group_survivors(
    leader: u32,
    label: &str,
    tracker: &Tracker,
    grace: Duration,
) -> Vec<String> {
    let list = || {
        group_members(leader).map(|all| {
            all.into_iter()
                .filter(|p| *p != leader)
                .collect::<Vec<u32>>()
        })
    };
    let Some(mut members) = list() else {
        let why = format!(
            "[rightkit-qa] the process group of {label} (pid {leader}) could not be listed before cleanup; recorded as a survivor"
        );
        tracker.record_survivor(leader, &why);
        return vec![why];
    };
    let end = Instant::now() + grace;
    while !members.is_empty() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
        if let Some(m) = list() {
            members = m;
        }
    }
    if members.is_empty() {
        return Vec::new();
    }
    let name = format!("descendant of {label} (pid {leader})");
    for pid in &members {
        tracker.record_survivor(*pid, &name);
    }
    let list = members
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    vec![format!(
        "[rightkit-qa] {} descendant process(es) of pid {leader} outlived it; terminated with its process group: {list}",
        members.len()
    )]
}

/// Wait up to `wait` for the leader of `child` to exit without reaping it or signalling anything;
/// when it did, record what it left running as survivors ([`record_survivors`]); then end the tree
/// and reap ([`OwnedChild::wait_then_kill`]: a leader that did not exit in time is killed with its
/// tree, which is a deliberate kill and records nothing). This is the one way a cleanup that gives
/// a process time to exit on its own (a daemon after its stdin closed, a plugin after `shutdown`)
/// makes sure the descendants it destroys were recorded first.
pub fn settle_exit(
    child: &mut OwnedChild,
    label: &str,
    tracker: &Tracker,
    wait: Duration,
) -> std::io::Result<rightkit_child_process::WaitOutcome> {
    let exited = child
        .process_handle()
        .and_then(|h| h.wait_exited_timeout(wait))
        .unwrap_or(false);
    if exited {
        record_survivors(child, label, tracker, SETTLE_GRACE);
        return child.wait_then_kill(TERMINATE_REAP_TIMEOUT);
    }
    child.wait_then_kill(Duration::ZERO)
}

/// Clean up whatever the owned child `pid` left behind. Unix kills the process group led by
/// the leader. Windows asks the child's Job Object (see [`settle_group`]), which needs no helper
/// process when the job is empty; `taskkill` is the fallback.
fn kill_group(
    pid: u32,
    job: &JobSlot,
    label: &str,
    tracker: &Tracker,
    end: GroupEnd,
) -> Vec<String> {
    #[cfg(unix)]
    {
        let _ = job;
        let mut notes = Vec::new();
        // Members are listed and recorded BEFORE the group is killed. After a deliberate
        // termination they are only the expected part of the kill; after the command exited on
        // its own they are descendants that outlived it.
        if end == GroupEnd::Exited {
            notes.extend(record_group_survivors(pid, label, tracker, SETTLE_GRACE));
        }
        let _ = quiet_ok(Command::new("kill").args(["-KILL", "--", &format!("-{pid}")]));
        notes
    }
    #[cfg(windows)]
    {
        settle_group(
            &mut WinOps { job: job.as_ref() },
            pid,
            label,
            tracker,
            end,
            JOB_EXIT_GRACE,
            JOB_DRAIN_TIMEOUT,
        )
    }
}

/// Registry of every pid a run started, so the run can assert nothing leaked.
#[derive(Clone, Default)]
pub struct Tracker {
    inner: Arc<Mutex<BTreeMap<u32, String>>>,
    /// Descendants seen running when their owner finished, recorded before any cleanup killed
    /// them (see [`Tracker::record_survivor`]). Never removed: the evidence outlives the process.
    recorded: Arc<Mutex<BTreeMap<u32, String>>>,
    registry: Arc<Mutex<Option<PathBuf>>>,
    /// Registered pids the runner itself started for this scenario and retires at teardown (see
    /// [`RunnerOwner`]). A subset of `inner`: [`Tracker::forget`] removes both.
    runner_owned: Arc<Mutex<BTreeMap<u32, RunnerOwner>>>,
}

/// What kind of runner-owned process a pid is. Typed so the built-in `assert_no_orphans` step
/// never has to recognise a process by its label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerOwner {
    /// A live `--stdio` engine sidecar (a scenario `sidecar_start`, or the engine session's).
    Sidecar,
    /// The app's step plugin.
    Plugin,
}

impl Tracker {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn register(&self, pid: u32, label: &str) {
        self.inner.lock().unwrap().insert(pid, label.to_string());
        self.persist();
    }
    /// [`Tracker::register`] for a process the runner owns for this scenario and retires itself at
    /// teardown (an engine sidecar, the step plugin). It is still registered, killed by
    /// [`Tracker::kill_all`] and checked by the strict [`Tracker::assert_no_orphans`] at teardown;
    /// only [`Tracker::assert_no_foreign_orphans`] (the mid-scenario step) leaves it out.
    pub fn register_owned(&self, pid: u32, label: &str, owner: RunnerOwner) {
        self.runner_owned.lock().unwrap().insert(pid, owner);
        self.register(pid, label);
    }
    /// True when `pid` was registered with [`Tracker::register_owned`] and not forgotten since.
    pub fn is_runner_owned(&self, pid: u32) -> bool {
        self.runner_owned.lock().unwrap().contains_key(&pid)
    }
    pub fn forget(&self, pid: u32) {
        self.runner_owned.lock().unwrap().remove(&pid);
        self.inner.lock().unwrap().remove(&pid);
        self.persist();
    }
    /// Record `pid` as a process that outlived its owner. Called BEFORE the cleanup that ends it, so
    /// the record survives the kill: [`Tracker::alive`] keeps returning it, and the scenario's
    /// teardown reads it as a survivor (`errored`). Silent sweeping is a defect: every cleanup that
    /// would kill a descendant records it here first.
    pub fn record_survivor(&self, pid: u32, label: &str) {
        self.recorded.lock().unwrap().insert(pid, label.to_string());
    }
    /// The processes recorded with [`Tracker::record_survivor`], whether or not they still run.
    pub fn recorded(&self) -> Vec<(u32, String)> {
        self.recorded
            .lock()
            .unwrap()
            .iter()
            .map(|(p, l)| (*p, l.clone()))
            .collect()
    }
    /// Registered processes that are still running (nothing recorded, nothing dead).
    fn live_registered(&self) -> Vec<(u32, String)> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| is_alive(**p))
            .map(|(p, l)| (*p, l.clone()))
            .collect()
    }
    /// Survivors: registered processes still running, then every process recorded as having
    /// outlived its owner (running or already ended by the cleanup that recorded it first).
    pub fn alive(&self) -> Vec<(u32, String)> {
        let mut out = self.live_registered();
        for (pid, label) in self.recorded() {
            if !out.iter().any(|(p, _)| *p == pid) {
                out.push((pid, label));
            }
        }
        out
    }
    pub fn pids(&self) -> Vec<u32> {
        self.inner.lock().unwrap().keys().copied().collect()
    }
    /// Kill every registered process still running. A recorded survivor is never signalled by pid:
    /// it was ended by the cleanup that recorded it, and its pid may name something else by now.
    pub fn kill_all(&self) {
        for (pid, _) in self.live_registered() {
            kill_tree(pid);
        }
    }
    /// Fail when any tracked process survived or any descendant was recorded as having outlived its
    /// owner; survivors still running are killed first so a failing assertion never leaves its own
    /// litter.
    pub fn assert_no_orphans(&self) -> Result<()> {
        let alive = self.alive();
        if alive.is_empty() {
            return Ok(());
        }
        self.kill_all();
        let list = alive
            .iter()
            .map(|(p, l)| format!("{p} ({l})"))
            .collect::<Vec<_>>()
            .join(", ");
        err(format!(
            "orphan processes outlived their owners or are still running: {list}"
        ))
    }
    /// The built-in `assert_no_orphans` step: [`Tracker::assert_no_orphans`] without the processes
    /// the runner owns for this scenario ([`Tracker::register_owned`]), which are alive on purpose
    /// and retired at teardown (later steps may use them). Everything else is still reported: any
    /// other live registered process, and every descendant recorded as having outlived its owner,
    /// including the descendants of a sidecar or the plugin (they have their own pids). Only the
    /// foreign survivors are killed on failure; the runner-owned ones are left to teardown.
    pub fn assert_no_foreign_orphans(&self) -> Result<()> {
        let owned = self.runner_owned.lock().unwrap().clone();
        // A recorded survivor is never excluded, even when its pid is an owned one: the record
        // says it outlived its owner (or could not be listed), which ownership does not excuse.
        let mut alive: Vec<(u32, String)> = self
            .live_registered()
            .into_iter()
            .filter(|(p, _)| !owned.contains_key(p))
            .collect();
        for (pid, label) in self.recorded() {
            if !alive.iter().any(|(p, _)| *p == pid) {
                alive.push((pid, label));
            }
        }
        if alive.is_empty() {
            return Ok(());
        }
        for (pid, _) in self.live_registered() {
            if !owned.contains_key(&pid) {
                kill_tree(pid);
            }
        }
        let list = alive
            .iter()
            .map(|(p, l)| format!("{p} ({l})"))
            .collect::<Vec<_>>()
            .join(", ");
        err(format!(
            "orphan processes outlived their owners or are still running: {list}"
        ))
    }
}

/// How long a finished command's tree is given to empty by itself before what is left in it counts
/// as descendants that outlived it (a console host, a helper mid-exit).
const SETTLE_GRACE: Duration = Duration::from_millis(250);

/// Record the processes `child` left running as survivors, BEFORE any kill reaches them, and return
/// their pids. Use it where a leader that exited on its own is about to have its tree swept. The
/// tree is polled for up to `grace` so members that leave by themselves are not recorded; what is
/// still there when the grace ends is a descendant that outlived the command. A tree that cannot
/// be listed fails closed: the leader's own pid is recorded, naming the listing failure, so the
/// scenario is not passed on an unknown.
pub fn record_survivors(
    child: &OwnedChild,
    label: &str,
    tracker: &Tracker,
    grace: Duration,
) -> Vec<u32> {
    let leader = child.id();
    let mut members = match child.tree_members() {
        Ok(m) => m,
        Err(e) => {
            tracker.record_survivor(
                leader,
                &format!("tree of {label} (pid {leader}) could not be listed before cleanup: {e}"),
            );
            return vec![leader];
        }
    };
    let end = Instant::now() + grace;
    while !members.is_empty() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
        if let Ok(m) = child.tree_members() {
            members = m;
        }
    }
    let name = format!("descendant of {label} (pid {leader})");
    for pid in &members {
        tracker.record_survivor(*pid, &name);
    }
    members
}

/// [`record_survivors`] with the default grace, for a leader that exited on its own. A leader that
/// is still running (`wait_exited_timeout` of zero says no) is being killed on purpose, so its
/// descendants are the expected part of that kill: nothing is recorded and the result is empty.
pub fn record_survivors_if_exited(child: &OwnedChild, label: &str, tracker: &Tracker) -> Vec<u32> {
    let exited = child
        .process_handle()
        .and_then(|h| h.wait_exited_timeout(Duration::ZERO))
        .unwrap_or(false);
    if exited {
        record_survivors(child, label, tracker, SETTLE_GRACE)
    } else {
        Vec::new()
    }
}

#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    /// When true the inherited environment is dropped first.
    pub clear_env: bool,
    pub timeout: Option<Duration>,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct Output {
    pub pid: u32,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// Run to completion with a hard deadline; on timeout the whole tree is killed.
pub fn run_owned(
    program: &str,
    args: &[String],
    opts: &RunOptions,
    tracker: &Tracker,
) -> Result<Output> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: never flash a console from a GUI host
    }
    if let Some(cwd) = &opts.cwd {
        cmd.current_dir(cwd);
    }
    if opts.clear_env {
        cmd.env_clear();
    }
    for (k, v) in &opts.env {
        cmd.env(k, v);
    }
    let mut owned = OwnedCommand::from_command(cmd);
    owned.windows_hide();
    let mut notes: Vec<String> = Vec::new();
    let start_err =
        |e: std::io::Error| crate::util::Error(format!("failed to start {program}: {e}"));
    #[cfg(unix)]
    let (mut child, job): (OwnedChild, JobSlot) = (owned.spawn().map_err(start_err)?, ());
    // Windows: the child runs inside a Job Object this function can query, so cleanup after it
    // exits is a handle query, not a `taskkill` process (about 0.4 s each).
    #[cfg(windows)]
    let (mut child, job) = {
        let (child, job, note) = spawn_in_job(owned).map_err(start_err)?;
        notes.extend(note);
        (child, job)
    };
    let pid = child.id();
    let label: &str = if opts.label.is_empty() {
        program
    } else {
        &opts.label
    };
    tracker.register(pid, label);
    let out_buf: Arc<Mutex<Vec<u8>>> = Arc::default();
    let err_buf: Arc<Mutex<Vec<u8>>> = Arc::default();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    for (stream, buf) in [
        (
            child
                .take_stdout()
                .map(|s| Box::new(s) as Box<dyn Read + Send>),
            out_buf.clone(),
        ),
        (
            child
                .take_stderr()
                .map(|s| Box::new(s) as Box<dyn Read + Send>),
            err_buf.clone(),
        ),
    ] {
        let tx = done_tx.clone();
        std::thread::spawn(move || {
            if let Some(mut s) = stream {
                let mut chunk = [0u8; 8192];
                while let Ok(n) = s.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    buf.lock().unwrap().extend_from_slice(&chunk[..n]);
                }
            }
            let _ = tx.send(());
        });
    }
    drop(done_tx);
    let timeout = opts.timeout.unwrap_or(Duration::from_secs(120));
    let status = child.wait_timeout(timeout)?;
    let (code, timed_out) = match status {
        Some(s) => {
            // owned descendants never outlive their command
            notes.extend(kill_group(pid, &job, label, tracker, GroupEnd::Exited));
            (s.code(), false)
        }
        None => {
            let (st, note) = terminate_bounded(&mut child);
            notes.extend(note);
            notes.extend(kill_group(pid, &job, label, tracker, GroupEnd::Terminated));
            (st.and_then(|s| s.code()), true)
        }
    };
    // Stream collection is bounded: a descendant that outlives the direct child and
    // keeps the pipes open must not hang the run. Give readers a short grace, then
    // kill the child's whole process group and give them a final bounded wait.
    let mut finished = 0;
    let grace = Instant::now() + Duration::from_secs(2);
    while finished < 2 {
        match done_rx.recv_timeout(grace.saturating_duration_since(Instant::now())) {
            Ok(()) => finished += 1,
            Err(_) => break,
        }
    }
    let mut leaked_descendants = false;
    if finished < 2 {
        leaked_descendants = true;
        // Still holding the output open after the group was settled: recorded before the kill.
        #[cfg(windows)]
        {
            record_survivors(&child, label, tracker, Duration::ZERO);
        }
        #[cfg(unix)]
        notes.extend(record_group_survivors(pid, label, tracker, Duration::ZERO));
        notes.extend(kill_group(pid, &job, label, tracker, GroupEnd::Terminated));
        let last = Instant::now() + Duration::from_secs(3);
        while finished < 2 {
            match done_rx.recv_timeout(last.saturating_duration_since(Instant::now())) {
                Ok(()) => finished += 1,
                Err(_) => break,
            }
        }
    }
    drop(child);
    #[cfg(windows)]
    drop(job);
    tracker.forget(pid);
    let mut stderr = String::from_utf8_lossy(&err_buf.lock().unwrap()).into_owned();
    if leaked_descendants {
        stderr.push_str("\n[rightkit-qa] descendant processes outlived the command and held its output open; the process group was killed");
    }
    for note in &notes {
        stderr.push('\n');
        stderr.push_str(note);
    }
    let stdout = String::from_utf8_lossy(&out_buf.lock().unwrap()).into_owned();
    Ok(Output {
        pid,
        code,
        stdout,
        stderr,
        timed_out,
    })
}

// ---------------------------------------------------------------- crash sweep

/// `ps` command line for a pid (unix); the fingerprint that makes a recorded pid
/// safe to kill later without risking an unrelated process that reused it.
pub fn fingerprint(pid: u32) -> Option<String> {
    #[cfg(unix)]
    {
        let out = output_bounded(
            Command::new("ps").args(["-o", "command=", "-p", &pid.to_string()]),
            KILL_HELPER_TIMEOUT,
        )
        .ok()??;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
    #[cfg(windows)]
    {
        let _ = pid;
        None
    }
}

impl Tracker {
    /// Persist every registration to `path` as `[{pid, label, command}]`, so a
    /// crashed run's survivors can be swept by the next run.
    pub fn with_registry(path: PathBuf) -> Self {
        let t = Self::default();
        *t.registry.lock().unwrap() = Some(path);
        t
    }

    pub(crate) fn persist(&self) {
        let Some(path) = self.registry.lock().unwrap().clone() else {
            return;
        };
        let rows: Vec<serde_json::Value> = self
            .inner
            .lock()
            .unwrap()
            .iter()
            .map(|(pid, label)| serde_json::json!({"pid": pid, "label": label, "command": fingerprint(*pid)}))
            .collect();
        if let Some(p) = path.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        let _ = std::fs::write(path, serde_json::to_vec_pretty(&rows).unwrap_or_default());
    }
}

/// Kill processes recorded by earlier runs that are still alive AND still have
/// the exact command line that was recorded. Returns the pids killed.
pub fn sweep_registries(runs_root: &std::path::Path, skip_run: &str) -> Vec<u32> {
    let mut killed = vec![];
    let Ok(entries) = std::fs::read_dir(runs_root) else {
        return killed;
    };
    for e in entries.flatten() {
        if e.file_name().to_string_lossy() == skip_run {
            continue;
        }
        let Ok(ft) = e.file_type() else { continue };
        if !ft.is_dir() || ft.is_symlink() {
            continue;
        }
        let Some(marker) = crate::storage::read_marker(&e.path().join(crate::storage::RUN_MARKER))
        else {
            continue;
        };
        if marker.run_id != e.file_name().to_string_lossy()
            || crate::storage::marker_active(&marker, crate::util::now_ms())
        {
            continue;
        }
        let file = e.path().join("pids.json");
        let Ok(bytes) = std::fs::read(&file) else {
            continue;
        };
        let rows: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap_or_default();
        for r in rows {
            let (Some(pid), Some(cmd)) =
                (r["pid"].as_u64().map(|p| p as u32), r["command"].as_str())
            else {
                continue;
            };
            if is_alive(pid) && fingerprint(pid).as_deref() == Some(cmd) {
                kill_tree(pid);
                killed.push(pid);
            }
        }
        let _ = std::fs::remove_file(&file);
    }
    killed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A command that runs far longer than any test timeout.
    fn slow() -> Command {
        #[cfg(windows)]
        {
            let mut c = Command::new("ping");
            c.args(["-n", "60", "127.0.0.1"]);
            c
        }
        #[cfg(unix)]
        {
            let mut c = Command::new("sleep");
            c.arg("60");
            c
        }
    }

    fn fast() -> Command {
        #[cfg(windows)]
        {
            let mut c = Command::new("cmd");
            c.args(["/C", "exit 0"]);
            c
        }
        #[cfg(unix)]
        {
            Command::new("true")
        }
    }

    #[test]
    fn slow_helper_is_bounded_and_falls_back() {
        let called = std::cell::Cell::new(None);
        let started = Instant::now();
        let note = kill_with_fallback(&mut slow(), 4242, Duration::from_millis(300), |pid| {
            called.set(Some(pid));
            true
        });
        assert!(started.elapsed() < Duration::from_secs(10), "bounded wait");
        assert_eq!(called.get(), Some(4242), "fallback ran on the known pid");
        let note = note.expect("timeout is reported");
        assert!(note.contains("timed out"), "{note}");
        assert!(note.contains("4242"), "{note}");
        assert!(note.contains("(terminated)"), "{note}");
    }

    #[test]
    fn finished_helper_skips_fallback_and_reports_nothing() {
        let called = std::cell::Cell::new(false);
        let note = kill_with_fallback(&mut fast(), 1, Duration::from_secs(20), |_| {
            called.set(true);
            true
        });
        assert!(note.is_none());
        assert!(!called.get());
    }

    #[test]
    fn unstartable_helper_falls_back_and_reports() {
        let called = std::cell::Cell::new(false);
        let mut missing = Command::new("rightkit-qa-no-such-helper-binary");
        let note = kill_with_fallback(&mut missing, 7, Duration::from_secs(1), |_| {
            called.set(true);
            false
        })
        .expect("start failure is reported");
        assert!(called.get());
        assert!(note.contains("could not run"), "{note}");
        assert!(note.contains("(not terminated)"), "{note}");
    }

    #[test]
    fn status_bounded_times_out_and_reaps() {
        let started = Instant::now();
        let st = status_bounded(
            slow().stdout(Stdio::null()).stderr(Stdio::null()),
            Duration::from_millis(200),
        )
        .unwrap();
        assert!(st.is_none());
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn output_bounded_times_out_and_returns_output_when_fast() {
        let started = Instant::now();
        assert!(output_bounded(&mut slow(), Duration::from_millis(200))
            .unwrap()
            .is_none());
        assert!(started.elapsed() < Duration::from_secs(10));
        let out = output_bounded(&mut fast(), Duration::from_secs(20))
            .unwrap()
            .expect("finishes");
        assert!(out.status.success());
    }

    #[test]
    fn runner_owned_sidecar_and_plugin_are_not_foreign_orphans_but_their_descendants_are() {
        // This test process is a live pid that stays alive: the stand-in for a running sidecar and
        // plugin, registered with ownership as `Sidecar::start` and `Plugin::start` do.
        let live = std::process::id();
        let tracker = Tracker::new();
        tracker.register_owned(live, "engine --stdio", RunnerOwner::Sidecar);
        tracker.register_owned(live.wrapping_add(1), "step plugin", RunnerOwner::Plugin);
        assert!(
            tracker.assert_no_foreign_orphans().is_ok(),
            "{:?}",
            tracker.alive()
        );
        // The strict teardown assertion still sees the live owned process.
        assert!(tracker.alive().iter().any(|(p, _)| *p == live));
        assert!(tracker.is_runner_owned(live));
        // A child that outlived its owner is recorded under its own pid and still fails.
        let child = live.wrapping_add(2);
        tracker.record_survivor(
            child,
            &format!("descendant of step plugin (pid {})", live.wrapping_add(1)),
        );
        let e = tracker.assert_no_foreign_orphans().unwrap_err();
        assert!(e.0.contains(&child.to_string()), "{}", e.0);
        assert!(!e.0.contains("engine --stdio"), "{}", e.0);
        assert!(
            is_alive(live),
            "the runner-owned process is not retired early"
        );
        assert!(
            tracker.is_runner_owned(live),
            "ownership survives the failed step"
        );
        // Forgetting drops the ownership with the registration.
        tracker.forget(live);
        assert!(!tracker.is_runner_owned(live));
    }

    #[test]
    fn a_recorded_survivor_is_reported_even_when_its_pid_is_runner_owned() {
        let live = std::process::id();
        let tracker = Tracker::new();
        tracker.register_owned(live, "step plugin", RunnerOwner::Plugin);
        tracker.record_survivor(live, "tree of step plugin could not be listed");
        let e = tracker.assert_no_foreign_orphans().unwrap_err();
        assert!(e.0.contains("could not be listed"), "{}", e.0);
    }
}

/// Windows: the Job Object cleanup after an owned child exits (no `taskkill` unless needed).
#[cfg(all(test, windows))]
mod job_cleanup_tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    /// A scripted job: `members` returns the next scripted snapshot (the last one repeats).
    struct Fake {
        job: bool,
        script: Vec<std::result::Result<Vec<u32>, ()>>,
        queries: Cell<usize>,
        terminated: Cell<u32>,
        killed: Vec<u32>,
        /// Pids that `alive` reports as gone.
        gone: Vec<u32>,
        tracker: Tracker,
        /// What the tracker held at the moment the job was terminated.
        registered_at_terminate: RefCell<Vec<u32>>,
    }

    impl Fake {
        fn new(tracker: &Tracker, script: Vec<std::result::Result<Vec<u32>, ()>>) -> Self {
            Fake {
                job: true,
                script,
                queries: Cell::new(0),
                terminated: Cell::new(0),
                killed: Vec::new(),
                gone: Vec::new(),
                tracker: tracker.clone(),
                registered_at_terminate: RefCell::new(Vec::new()),
            }
        }
    }

    impl GroupOps for Fake {
        fn has_job(&self) -> bool {
            self.job
        }
        fn members(&self) -> std::io::Result<Vec<u32>> {
            let i = self.queries.get();
            self.queries.set(i + 1);
            match &self.script[i.min(self.script.len() - 1)] {
                Ok(ids) => Ok(ids.clone()),
                Err(()) => Err(std::io::Error::other("query refused")),
            }
        }
        fn terminate(&self) -> std::io::Result<()> {
            self.terminated.set(self.terminated.get() + 1);
            *self.registered_at_terminate.borrow_mut() = self.tracker.pids();
            Ok(())
        }
        fn alive(&self, pid: u32) -> bool {
            !self.gone.contains(&pid)
        }
        fn taskkill(&mut self, pid: u32) -> Option<String> {
            self.killed.push(pid);
            None
        }
    }

    const LEADER: u32 = 100;
    const SHORT: Duration = Duration::from_millis(60);

    fn settle(fake: &mut Fake, tracker: &Tracker, end: GroupEnd) -> Vec<String> {
        settle_group(fake, LEADER, "probe", tracker, end, Duration::ZERO, SHORT)
    }

    #[test]
    fn a_member_that_leaves_by_itself_within_the_grace_is_not_a_descendant() {
        // The console host of a console child: listed right after the leader exited, gone a moment
        // later. Nothing is recorded, terminated or killed.
        let tracker = Tracker::new();
        let mut fake = Fake::new(&tracker, vec![Ok(vec![LEADER, 4005]), Ok(vec![])]);
        let notes = settle_group(
            &mut fake,
            LEADER,
            "probe",
            &tracker,
            GroupEnd::Exited,
            Duration::from_millis(500),
            SHORT,
        );
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(fake.terminated.get(), 0);
        assert!(fake.killed.is_empty());
        assert!(tracker.pids().is_empty());
        assert!(
            tracker.recorded().is_empty(),
            "a member that left by itself is not a survivor"
        );
    }

    #[test]
    fn a_member_still_there_when_the_grace_ends_is_still_reported() {
        // The member never leaves (the last snapshot repeats), so the grace runs out and it is a
        // descendant that outlived the command, exactly as before the grace existed.
        let tracker = Tracker::new();
        let mut fake = Fake::new(&tracker, vec![Ok(vec![LEADER, 4006])]);
        fake.gone = vec![4006];
        let notes = settle_group(
            &mut fake,
            LEADER,
            "probe",
            &tracker,
            GroupEnd::Exited,
            Duration::from_millis(30),
            SHORT,
        );
        assert_eq!(fake.terminated.get(), 1, "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("outlived it")), "{notes:?}");
    }

    #[test]
    fn child_with_no_descendants_spawns_no_kill_helper() {
        for snapshot in [vec![], vec![LEADER]] {
            let tracker = Tracker::new();
            let mut fake = Fake::new(&tracker, vec![Ok(snapshot)]);
            let notes = settle(&mut fake, &tracker, GroupEnd::Exited);
            assert!(notes.is_empty(), "{notes:?}");
            assert!(
                fake.killed.is_empty(),
                "taskkill must not run: {:?}",
                fake.killed
            );
            assert_eq!(fake.terminated.get(), 0, "an empty job is not terminated");
            assert!(tracker.pids().is_empty());
        }
    }

    #[test]
    fn leftover_grandchild_is_recorded_then_killed_with_the_job() {
        let tracker = Tracker::new();
        let mut fake = Fake::new(&tracker, vec![Ok(vec![LEADER, 4001]), Ok(vec![])]);
        let notes = settle(&mut fake, &tracker, GroupEnd::Exited);
        assert_eq!(fake.terminated.get(), 1, "the job is terminated in-process");
        assert_eq!(
            *fake.registered_at_terminate.borrow(),
            vec![4001],
            "the orphan is recorded before it is killed"
        );
        assert!(fake.killed.is_empty(), "no helper once the job emptied");
        assert!(tracker.pids().is_empty(), "a reaped orphan is forgotten");
        assert!(
            notes
                .iter()
                .any(|n| n.contains("4001") && n.contains("outlived")),
            "{notes:?}"
        );
        // Oracle item 1: the record outlives the kill, so the scenario's teardown reads it.
        assert_eq!(
            tracker
                .recorded()
                .iter()
                .map(|(p, _)| *p)
                .collect::<Vec<_>>(),
            vec![4001]
        );
        assert!(tracker.alive().iter().any(|(p, _)| *p == 4001));
        assert!(tracker.assert_no_orphans().is_err());
    }

    #[test]
    fn member_that_survives_job_termination_goes_to_taskkill_and_stays_registered() {
        let tracker = Tracker::new();
        // The job never empties and the pid is still alive afterwards.
        let mut fake = Fake::new(&tracker, vec![Ok(vec![LEADER, 4002])]);
        let notes = settle(&mut fake, &tracker, GroupEnd::Exited);
        assert_eq!(fake.terminated.get(), 1);
        assert_eq!(
            fake.killed,
            vec![4002],
            "taskkill runs once, on the survivor only"
        );
        assert_eq!(tracker.pids(), vec![4002], "a survivor stays registered");
        assert!(
            notes.iter().any(|n| n.contains("still running")),
            "{notes:?}"
        );
        assert!(notes.iter().any(|n| n.contains("survived")), "{notes:?}");
    }

    #[test]
    fn survivor_that_taskkill_stops_is_forgotten() {
        let tracker = Tracker::new();
        let mut fake = Fake::new(&tracker, vec![Ok(vec![LEADER, 4003])]);
        fake.gone = vec![4003];
        settle(&mut fake, &tracker, GroupEnd::Exited);
        assert_eq!(fake.killed, vec![4003]);
        assert!(tracker.pids().is_empty());
    }

    #[test]
    fn job_query_failure_falls_back_to_taskkill_on_the_leader() {
        let tracker = Tracker::new();
        let mut fake = Fake::new(&tracker, vec![Err(())]);
        let notes = settle(&mut fake, &tracker, GroupEnd::Exited);
        assert_eq!(fake.killed, vec![LEADER]);
        assert_eq!(fake.terminated.get(), 0);
        assert!(
            notes.iter().any(|n| n.contains("could not list")),
            "{notes:?}"
        );
        let recorded = tracker.recorded();
        assert_eq!(recorded.len(), 1, "{recorded:?}");
        assert_eq!(recorded[0].0, LEADER);
        assert!(recorded[0].1.contains("query refused"), "{recorded:?}");
    }

    #[test]
    fn child_without_a_job_falls_back_to_taskkill_on_the_leader() {
        let tracker = Tracker::new();
        let mut fake = Fake::new(&tracker, vec![Ok(vec![])]);
        fake.job = false;
        let notes = settle(&mut fake, &tracker, GroupEnd::Exited);
        assert_eq!(fake.killed, vec![LEADER]);
        assert_eq!(fake.queries.get(), 0, "no job, nothing to query");
        assert!(!notes.is_empty(), "{notes:?}");
        let recorded = tracker.recorded();
        assert_eq!(
            recorded.len(),
            1,
            "an unlisted kill is recorded: {recorded:?}"
        );
        assert_eq!(recorded[0].0, LEADER);
    }

    #[test]
    fn members_draining_after_a_deliberate_termination_are_not_reported() {
        let tracker = Tracker::new();
        let mut fake = Fake::new(&tracker, vec![Ok(vec![LEADER, 4004]), Ok(vec![])]);
        let notes = settle(&mut fake, &tracker, GroupEnd::Terminated);
        assert!(notes.is_empty(), "{notes:?}");
        assert!(fake.killed.is_empty());
        assert!(fake.registered_at_terminate.borrow().is_empty());
        assert!(tracker.pids().is_empty());
        assert!(
            tracker.recorded().is_empty(),
            "a deliberate termination records nothing"
        );
    }

    /// The real thing: a grandchild that holds the output pipes open dies with the job at once,
    /// instead of costing the stream grace and a `taskkill` that cannot see it.
    #[test]
    fn run_owned_takes_a_leftover_grandchild_down_with_the_job() {
        let tracker = Tracker::new();
        let started = Instant::now();
        let out = run_owned(
            "cmd",
            &[
                "/C".to_string(),
                "start /B ping -n 60 127.0.0.1".to_string(),
            ],
            &RunOptions {
                timeout: Some(Duration::from_secs(30)),
                label: "grandchild-probe".into(),
                ..RunOptions::default()
            },
            &tracker,
        )
        .expect("runs");
        assert!(!out.timed_out);
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "the grandchild must not hold the run for its 60 s"
        );
        let note = out
            .stderr
            .lines()
            .find(|l| l.contains("outlived it"))
            .unwrap_or_else(|| panic!("orphan note missing: {}", out.stderr));
        let pids: Vec<u32> = note
            .rsplit(": ")
            .next()
            .unwrap_or_default()
            .split(", ")
            .filter_map(|p| p.trim().parse().ok())
            .collect();
        assert!(!pids.is_empty(), "{note}");
        let recorded: Vec<u32> = tracker.recorded().into_iter().map(|(p, _)| p).collect();
        for pid in &pids {
            assert!(recorded.contains(pid), "{pid} not recorded: {recorded:?}");
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while pids.iter().any(|p| is_alive(*p)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(
            pids.iter().all(|p| !is_alive(*p)),
            "grandchildren still running: {pids:?}"
        );
        assert!(tracker.pids().is_empty(), "{:?}", tracker.pids());
    }

    #[test]
    fn run_owned_of_a_clean_child_reports_nothing_extra() {
        let tracker = Tracker::new();
        let out = run_owned(
            "cmd",
            &["/C".to_string(), "exit 0".to_string()],
            &RunOptions {
                label: "clean-probe".into(),
                ..RunOptions::default()
            },
            &tracker,
        )
        .expect("runs");
        assert_eq!(out.code, Some(0));
        assert!(out.stderr.is_empty(), "{}", out.stderr);
        assert!(tracker.pids().is_empty());
    }
}

/// Unix: the group of an owned child is listed and recorded before it is killed.
#[cfg(all(test, unix))]
mod group_survivor_tests {
    use super::*;

    /// Oracle item 1 (run_owned): a command that exits 0 and leaves a grandchild running. The
    /// grandchild's pid is recorded as a survivor before the group kill ends it.
    #[test]
    fn run_owned_records_a_leftover_grandchild_before_killing_it() {
        let tracker = Tracker::new();
        let out = run_owned(
            "sh",
            &["-c".to_string(), "sleep 60 & echo $!".to_string()],
            &RunOptions {
                timeout: Some(Duration::from_secs(30)),
                label: "grandchild-probe".into(),
                ..RunOptions::default()
            },
            &tracker,
        )
        .expect("runs");
        assert_eq!(out.code, Some(0));
        let grandchild: u32 = out
            .stdout
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("stdout: {:?}", out.stdout));
        let recorded = tracker.recorded();
        assert!(
            recorded
                .iter()
                .any(|(pid, label)| *pid == grandchild && label.contains("grandchild-probe")),
            "{recorded:?}"
        );
        assert!(
            out.stderr.contains("outlived it") && out.stderr.contains(&grandchild.to_string()),
            "{}",
            out.stderr
        );
        assert!(tracker.alive().iter().any(|(pid, _)| *pid == grandchild));
        let deadline = Instant::now() + Duration::from_secs(5);
        while is_alive(grandchild) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(!is_alive(grandchild), "the group kill still ends it");
    }

    #[test]
    fn run_owned_of_a_clean_command_records_nothing() {
        let tracker = Tracker::new();
        let out = run_owned(
            "sh",
            &["-c".to_string(), "exit 0".to_string()],
            &RunOptions::default(),
            &tracker,
        )
        .expect("runs");
        assert_eq!(out.code, Some(0));
        assert!(tracker.recorded().is_empty(), "{:?}", tracker.recorded());
        assert!(tracker.alive().is_empty());
    }

    /// A command killed at its deadline is a deliberate kill, so its group is not a survivor.
    #[test]
    fn run_owned_deadline_kill_records_nothing() {
        let tracker = Tracker::new();
        let out = run_owned(
            "sh",
            &["-c".to_string(), "sleep 60".to_string()],
            &RunOptions {
                timeout: Some(Duration::from_millis(300)),
                ..RunOptions::default()
            },
            &tracker,
        )
        .expect("runs");
        assert!(out.timed_out);
        assert!(tracker.recorded().is_empty(), "{:?}", tracker.recorded());
    }
}
