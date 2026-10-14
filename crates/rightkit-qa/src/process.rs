//! Owned processes: every child is spawned through `rightkit-child-process` (process
//! group / Job Object), registered with a [`Tracker`], and killed by recorded pid
//! only. Nothing here ever matches processes by name for killing.
use crate::util::{err, Result};
use rightkit_child_process::OwnedCommand;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub fn is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        let out =
            hidden(Command::new("tasklist.exe").args(["/FI", &format!("PID eq {pid}"), "/NH"]))
                .output();
        out.map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
            .unwrap_or(false)
    }
}

#[cfg(windows)]
fn hidden(cmd: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000)
}

#[cfg(unix)]
fn children_of(pid: u32) -> Vec<u32> {
    let out = Command::new("pgrep")
        .args(["-P", &pid.to_string()])
        .output();
    out.map(|o| {
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect()
    })
    .unwrap_or_default()
}

/// Terminate `pid` and its descendants. TERM first, KILL after a short grace.
pub fn kill_tree(pid: u32) {
    if !is_alive(pid) {
        return;
    }
    #[cfg(windows)]
    {
        let _ = hidden(Command::new("taskkill.exe").args(["/PID", &pid.to_string(), "/T", "/F"]))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
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
                let _ = Command::new("kill")
                    .args([sig, &p.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
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
fn kill_group(pid: u32) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{pid}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(windows)]
    {
        let _ = hidden(Command::new("taskkill.exe").args(["/PID", &pid.to_string(), "/T", "/F"]))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
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
    let timeout = opts.timeout.unwrap_or(Duration::from_secs(120));
    let status = child.wait_timeout(timeout)?;
    let (code, timed_out) = match status {
        Some(s) => {
            kill_group(pid); // owned descendants never outlive their command
            (s.code(), false)
        }
        None => {
            let st = child.terminate_tree().ok();
            kill_group(pid);
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
        kill_group(pid);
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
        let out = Command::new("ps")
            .args(["-o", "command=", "-p", &pid.to_string()])
            .output()
            .ok()?;
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
