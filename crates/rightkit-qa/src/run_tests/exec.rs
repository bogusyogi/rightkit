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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "rkqa-exec-{name}-{}-{}",
            std::process::id(),
            crate::util::new_id()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn sh(d: &std::path::Path, script: &str, timeout: Duration) -> ProcSpec {
        ProcSpec {
            program: PathBuf::from("/bin/sh"),
            args: vec!["-c".into(), script.into()],
            cwd: d.to_path_buf(),
            env: vec![],
            unset_env: vec![],
            stdout: d.join("out.log"),
            stderr: d.join("err.log"),
            timeout,
            label: "test".into(),
        }
    }

    #[test]
    fn captures_both_streams_into_files_and_reports_the_exit_code() {
        let d = dir("streams");
        let spec = sh(
            &d,
            "echo to-out; echo to-err >&2; exit 3",
            Duration::from_secs(20),
        );
        let o = run_to_files(&spec).unwrap();
        assert_eq!(o.code, Some(3));
        assert!(!o.timed_out && !o.success());
        assert_eq!(std::fs::read_to_string(&spec.stdout).unwrap(), "to-out\n");
        assert_eq!(std::fs::read_to_string(&spec.stderr).unwrap(), "to-err\n");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_zero_exit_is_a_success() {
        let d = dir("ok");
        let o = run_to_files(&sh(&d, "exit 0", Duration::from_secs(20))).unwrap();
        assert!(o.success());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_deadline_kills_the_process_and_says_so() {
        let d = dir("deadline");
        let started = Instant::now();
        let o = run_to_files(&sh(&d, "sleep 60", Duration::from_millis(300))).unwrap();
        assert!(o.timed_out, "{o:?}");
        assert_eq!(o.code, None);
        assert!(!o.success());
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the sleep was not killed: {:?}",
            started.elapsed()
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_deadline_kills_the_whole_tree_not_just_the_leader() {
        let d = dir("tree");
        let marker = d.join("grandchild-lived");
        // The grandchild would write the marker after 2 s if it survived the kill.
        let script = format!("(sleep 2; touch '{}') & sleep 60", marker.display());
        let o = run_to_files(&sh(&d, &script, Duration::from_millis(300))).unwrap();
        assert!(o.timed_out);
        std::thread::sleep(Duration::from_millis(2600));
        assert!(!marker.exists(), "a descendant outlived the deadline kill");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_descendant_left_behind_by_an_exited_leader_is_recorded_and_killed() {
        let d = dir("survivor");
        let marker = d.join("survivor-lived");
        let script = format!(
            "(sleep 2; touch '{}') >/dev/null 2>&1 & exit 0",
            marker.display()
        );
        let o = run_to_files(&sh(&d, &script, Duration::from_secs(20))).unwrap();
        assert!(o.success());
        assert!(
            !o.survivors.is_empty(),
            "the background sleep was not recorded: {o:?}"
        );
        std::thread::sleep(Duration::from_millis(2600));
        assert!(!marker.exists(), "the survivor was not killed");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn env_is_set_after_unset_and_inherited_values_can_be_removed() {
        let d = dir("env");
        std::env::set_var("RKQA_EXEC_INHERITED", "from-parent");
        let mut spec = sh(
            &d,
            "echo \"[$RKQA_EXEC_INHERITED][$RKQA_EXEC_BOTH][$RKQA_EXEC_NEW]\"",
            Duration::from_secs(20),
        );
        spec.unset_env = vec!["RKQA_EXEC_INHERITED".into(), "RKQA_EXEC_BOTH".into()];
        spec.env = vec![
            ("RKQA_EXEC_BOTH".into(), "set".into()),
            ("RKQA_EXEC_NEW".into(), "new".into()),
        ];
        assert!(run_to_files(&spec).unwrap().success());
        assert_eq!(
            std::fs::read_to_string(&spec.stdout).unwrap(),
            "[][set][new]\n"
        );
        std::env::remove_var("RKQA_EXEC_INHERITED");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_working_directory_is_the_one_asked_for() {
        let d = dir("cwd");
        let sub = d.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let mut spec = sh(&d, "pwd", Duration::from_secs(20));
        spec.cwd = sub.clone();
        assert!(run_to_files(&spec).unwrap().success());
        let printed = std::fs::read_to_string(&spec.stdout).unwrap();
        assert_eq!(
            std::fs::canonicalize(printed.trim()).unwrap(),
            std::fs::canonicalize(&sub).unwrap()
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_missing_program_is_an_error_not_an_outcome() {
        let d = dir("missing");
        let mut spec = sh(&d, "exit 0", Duration::from_secs(5));
        spec.program = d.join("no-such-binary");
        let e = run_to_files(&spec).unwrap_err();
        assert!(e.0.contains("failed to start"), "{e}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_signal_death_is_reported_with_no_exit_code() {
        let d = dir("signal");
        let o = run_to_files(&sh(&d, "kill -KILL $$", Duration::from_secs(20))).unwrap();
        assert_eq!(o.code, None);
        assert_eq!(o.signal, Some(9));
        assert!(!o.timed_out && !o.success());
        let _ = std::fs::remove_dir_all(&d);
    }
}
