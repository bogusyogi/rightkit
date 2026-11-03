//! Run one process to completion with its output in files and a hard deadline.
//!
//! The child is an owned process (a process group on Unix, a Job Object on Windows), so a deadline kills
//! the whole tree and a leader that exits on its own has its leftover descendants recorded as survivors
//! and then killed. Standard output and error go straight to files: a test binary that prints a gigabyte
//! costs disk, never memory, and the files are what the receipt points at.
use crate::process::{settle_exit, Tracker};
use crate::util::{Error, Result};
use rightkit_child_process::{OwnedCommand, WaitOutcome};
use std::fs::File;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub struct ProcSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Set after `unset_env` is applied, so a variable named in both ends up set.
    pub env: Vec<(String, String)>,
    pub unset_env: Vec<String>,
    pub stdout: PathBuf,
    pub stderr: PathBuf,
    pub timeout: Duration,
    /// Names the process in survivor records.
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Survivor {
    pub pid: u32,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct ProcOutcome {
    /// The exit code. `None` when the process was killed (a deadline, or a signal on Unix).
    pub code: Option<i32>,
    /// The signal that ended it (Unix), when it was one.
    pub signal: Option<i32>,
    /// The deadline passed and the tree was killed.
    pub timed_out: bool,
    pub elapsed: Duration,
    /// Descendants still running when the leader exited on its own. They were recorded, then killed.
    pub survivors: Vec<Survivor>,
}

impl ProcOutcome {
    pub fn success(&self) -> bool {
        !self.timed_out && self.code == Some(0)
    }
}

#[cfg(unix)]
fn signal_of(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn signal_of(_: &std::process::ExitStatus) -> Option<i32> {
    None
}

/// Start `spec`, wait at most `spec.timeout`, and report how it ended. An error means the process could
/// not be started or its output files could not be created; a process that ran and failed is an `Ok`
/// outcome.
pub fn run_to_files(spec: &ProcSpec) -> Result<ProcOutcome> {
    let make = |path: &PathBuf| {
        File::create(path).map_err(|e| Error(format!("cannot create {}: {e}", path.display())))
    };
    let out = make(&spec.stdout)?;
    let err = make(&spec.stderr)?;
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args)
        .current_dir(&spec.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));
    for key in &spec.unset_env {
        cmd.env_remove(key);
    }
    for (key, value) in &spec.env {
        cmd.env(key, value);
    }
    let mut owned = OwnedCommand::from_command(cmd);
    owned.windows_hide();
    // If this process dies (Ctrl-C, a kill) the child must not outlive it: a test binary left running for
    // up to its deadline would keep the machine busy and its fixtures open. Windows ties the child through
    // its Job Object, macOS through a watchdog on the whole group, Linux with a SIGTERM to the child.
    owned.kill_on_parent_exit(true);
    let started = Instant::now();
    let mut child = owned.spawn().map_err(|e| {
        Error(format!(
            "failed to start {} in {}: {e}",
            spec.program.display(),
            spec.cwd.display()
        ))
    })?;
    let tracker = Tracker::new();
    let waited = settle_exit(&mut child, &spec.label, &tracker, spec.timeout)
        .map_err(|e| Error(format!("waiting for {}: {e}", spec.program.display())))?;
    let elapsed = started.elapsed();
    let survivors = tracker
        .recorded()
        .into_iter()
        .map(|(pid, name)| Survivor { pid, name })
        .collect();
    Ok(match waited {
        WaitOutcome::Exited(status) => ProcOutcome {
            code: status.code(),
            signal: signal_of(&status),
            timed_out: false,
            elapsed,
            survivors,
        },
        WaitOutcome::Terminated(status) => ProcOutcome {
            code: None,
            signal: signal_of(&status),
            timed_out: true,
            elapsed,
            survivors,
        },
    })
}
