//! Owned processes: every child is spawned through `rightkit-child-process` (process
//! group / Job Object), registered with a [`Tracker`], and killed by recorded pid
//! only. Nothing here ever matches processes by name for killing.
use crate::util::{err, Result};
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
        quiet_ok(Command::new("kill").args(["-0", &pid.to_string()]))
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

/// Kill the process group led by `pid` (the owned-command leader) so descendants that
/// outlived the leader are cleaned up too. On Windows the Job Object dies with the child handle.
fn kill_group(pid: u32) -> Option<String> {
    #[cfg(unix)]
    {
        let _ = quiet_ok(Command::new("kill").args(["-KILL", "--", &format!("-{pid}")]));
        None
    }
    #[cfg(windows)]
    {
        taskkill_tree(pid)
    }
}

/// Registry of every pid a run started, so the run can assert nothing leaked.
#[derive(Clone, Default)]
pub struct Tracker {
    inner: Arc<Mutex<BTreeMap<u32, String>>>,
    registry: Arc<Mutex<Option<PathBuf>>>,
}

impl Tracker {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn register(&self, pid: u32, label: &str) {
        self.inner.lock().unwrap().insert(pid, label.to_string());
        self.persist();
    }
    pub fn forget(&self, pid: u32) {
        self.inner.lock().unwrap().remove(&pid);
        self.persist();
    }
    pub fn alive(&self) -> Vec<(u32, String)> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| is_alive(**p))
            .map(|(p, l)| (*p, l.clone()))
            .collect()
    }
    pub fn pids(&self) -> Vec<u32> {
        self.inner.lock().unwrap().keys().copied().collect()
    }
    pub fn kill_all(&self) {
        for (pid, _) in self.alive() {
            kill_tree(pid);
        }
    }
    /// Fail when any tracked process survived; survivors are killed first so a
    /// failing assertion never leaves its own litter.
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
        err(format!("orphan processes still running: {list}"))
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
    let mut child = owned
        .spawn()
        .map_err(|e| crate::util::Error(format!("failed to start {program}: {e}")))?;
    let pid = child.id();
    tracker.register(
        pid,
        if opts.label.is_empty() {
            program
        } else {
            &opts.label
        },
    );
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
    let mut notes: Vec<String> = Vec::new();
    let timeout = opts.timeout.unwrap_or(Duration::from_secs(120));
    let status = child.wait_timeout(timeout)?;
    let (code, timed_out) = match status {
        Some(s) => {
            // owned descendants never outlive their command
            notes.extend(kill_group(pid));
            (s.code(), false)
        }
        None => {
            let (st, note) = terminate_bounded(&mut child);
            notes.extend(note);
            notes.extend(kill_group(pid));
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
        notes.extend(kill_group(pid));
        let last = Instant::now() + Duration::from_secs(3);
        while finished < 2 {
            match done_rx.recv_timeout(last.saturating_duration_since(Instant::now())) {
                Ok(()) => finished += 1,
                Err(_) => break,
            }
        }
    }
    drop(child);
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
}
