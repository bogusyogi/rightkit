#![cfg(windows)]

use std::{
    fs,
    num::NonZeroU32,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use rightkit_child_process::{AdoptedProcess, OwnedCommand, WaitOutcome};

fn powershell(script: &str) -> Command {
    let mut command = Command::new("powershell.exe");
    command.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        script,
    ]);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn wait_for_pid(path: &Path) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(text) = fs::read_to_string(path) {
            if let Ok(pid) = text.trim().parse() {
                return pid;
            }
        }
        assert!(Instant::now() < deadline, "child PID file was not created");
        thread::sleep(Duration::from_millis(20));
    }
}

fn tree_command(pid_file: &Path) -> Command {
    let quoted = pid_file.display().to_string().replace('\'', "''");
    powershell(&format!(
        "$child = Start-Process powershell.exe -WindowStyle Hidden -ArgumentList '-NoLogo','-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 60' -PassThru; Set-Content -LiteralPath '{quoted}' -Value $child.Id; Wait-Process -Id $child.Id"
    ))
}

fn wait_until_stopped(process: &AdoptedProcess) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while process.is_running().unwrap_or(false) {
        assert!(
            Instant::now() < deadline,
            "process {} remained alive",
            process.id()
        );
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn terminating_owned_tree_kills_parent_and_grandchild_but_not_sibling() {
    let temp = std::env::temp_dir().join(format!("rightkit-process-tree-{}", std::process::id()));
    let _ = fs::remove_file(&temp);
    let mut sibling = powershell("Start-Sleep -Seconds 60").spawn().unwrap();
    let sibling_pid = NonZeroU32::new(sibling.id()).unwrap();

    let mut command = OwnedCommand::from_command(tree_command(&temp));
    command.windows_hide();
    let mut owned = command.spawn().unwrap();
    let parent = AdoptedProcess::new(NonZeroU32::new(owned.id()).unwrap());
    let grandchild = AdoptedProcess::new(NonZeroU32::new(wait_for_pid(&temp)).unwrap());

    owned.terminate_tree().unwrap();
    wait_until_stopped(&parent);
    wait_until_stopped(&grandchild);
    assert!(AdoptedProcess::new(sibling_pid).is_running().unwrap());

    sibling.kill().unwrap();
    sibling.wait().unwrap();
    let _ = fs::remove_file(temp);
}

#[test]
fn graceful_exit_does_not_report_a_forced_kill() {
    let command = powershell("Start-Sleep -Milliseconds 50");
    let mut child = OwnedCommand::from_command(command).spawn().unwrap();

    assert!(matches!(
        child.wait_or_kill(Duration::from_secs(2)).unwrap(),
        WaitOutcome::Exited(_)
    ));
}

#[test]
fn grace_expiry_terminates_the_whole_tree() {
    let temp = std::env::temp_dir().join(format!("rightkit-process-grace-{}", std::process::id()));
    let _ = fs::remove_file(&temp);
    let mut child = OwnedCommand::from_command(tree_command(&temp))
        .spawn()
        .unwrap();
    let grandchild = AdoptedProcess::new(NonZeroU32::new(wait_for_pid(&temp)).unwrap());

    assert!(matches!(
        child.wait_or_kill(Duration::from_millis(50)).unwrap(),
        WaitOutcome::Terminated(_)
    ));
    wait_until_stopped(&grandchild);
    let _ = fs::remove_file(temp);
}

#[test]
fn dropping_owned_child_leaves_no_orphan() {
    let temp = std::env::temp_dir().join(format!("rightkit-process-drop-{}", std::process::id()));
    let _ = fs::remove_file(&temp);
    let grandchild = {
        let mut child = OwnedCommand::from_command(tree_command(&temp))
            .spawn()
            .unwrap();
        let pid = wait_for_pid(&temp);
        assert!(child.try_wait().unwrap().is_none());
        AdoptedProcess::new(NonZeroU32::new(pid).unwrap())
    };

    wait_until_stopped(&grandchild);
    let _ = fs::remove_file(temp);
}

/// A std-spawned child attached with `assign_child` dies with its grandchild when the job drops;
/// `assign_process_id` + `terminate` kill on demand; a zero cap is refused.
#[test]
fn windows_job_kills_an_already_spawned_tree_on_drop_and_terminate() {
    use rightkit_child_process::{WindowsJob, DEFAULT_ACTIVE_PROCESS_LIMIT};

    assert!(WindowsJob::new(Some(0)).is_err());

    let dir = std::env::temp_dir().join(format!("rightkit-process-job-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let pid_file = dir.join("dropped.pid");
    let job = WindowsJob::new(None).unwrap();
    let mut child = tree_command(&pid_file).spawn().unwrap();
    job.assign_child(&child).unwrap();
    let grandchild = AdoptedProcess::new(NonZeroU32::new(wait_for_pid(&pid_file)).unwrap());
    drop(job);
    let _ = child.wait();
    wait_until_stopped(&grandchild);

    let pid_file = dir.join("terminated.pid");
    let job = WindowsJob::new(Some(DEFAULT_ACTIVE_PROCESS_LIMIT)).unwrap();
    assert_eq!(job.active_process_count().unwrap(), 0);
    assert!(job.is_empty().unwrap());
    let mut child = tree_command(&pid_file).spawn().unwrap();
    job.assign_process_id(child.id()).unwrap();
    let grandchild = AdoptedProcess::new(NonZeroU32::new(wait_for_pid(&pid_file)).unwrap());
    assert!(job.active_process_count().unwrap() >= 1);
    assert!(!job.is_empty().unwrap());
    job.terminate().unwrap();
    let _ = child.wait();
    wait_until_stopped(&grandchild);
    assert!(job.wait_empty(Duration::from_secs(5)).unwrap());
    assert_eq!(job.active_process_count().unwrap(), 0);
    assert!(job.is_empty().unwrap());
    let _ = fs::remove_dir_all(&dir);
}

/// The leader writes the descendant's pid and exits at once, leaving a sleeper behind in the
/// job. `wait_or_kill` sees the leader exit inside the grace period; it must still bring the
/// retained job down and report the leader as `Exited`.
#[test]
fn leader_exiting_within_grace_does_not_leave_a_descendant_behind() {
    let temp = std::env::temp_dir().join(format!("rightkit-process-early-{}", std::process::id()));
    let _ = fs::remove_file(&temp);
    let quoted = temp.display().to_string().replace('\'', "''");
    let command = powershell(&format!(
        "$child = Start-Process powershell.exe -WindowStyle Hidden -ArgumentList '-NoLogo','-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 60' -PassThru; Set-Content -LiteralPath '{quoted}' -Value $child.Id"
    ));
    let mut owned = OwnedCommand::from_command(command).spawn().unwrap();
    let grandchild = AdoptedProcess::new(NonZeroU32::new(wait_for_pid(&temp)).unwrap());

    assert!(matches!(
        owned.wait_or_kill(Duration::from_secs(30)).unwrap(),
        WaitOutcome::Exited(_)
    ));
    wait_until_stopped(&grandchild);
    let _ = fs::remove_file(temp);
}

// ---- Breakaway on a caller-built job (the job `contain_current_process_with` installs) ----
//
// The helper is this test binary run again: a child that is assigned to the job under test,
// then asks Windows for a grandchild with or without `CREATE_BREAKAWAY_FROM_JOB`. It waits for a
// gate file so the assignment is always made before it creates anything.

const HELPER_ENV: &str = "RIGHTKIT_BREAKAWAY_HELPER";

/// Not a test by itself: it does nothing unless `RIGHTKIT_BREAKAWAY_HELPER` is set, which only
/// `run_breakaway_helper` does. Value: `<mode>|<gate file>|<out file>`; mode is `breakaway` or
/// `contained`.
#[test]
fn breakaway_helper_child() {
    use std::os::windows::process::CommandExt;

    let Ok(spec) = std::env::var(HELPER_ENV) else {
        return;
    };
    let mut parts = spec.splitn(3, '|');
    let mode = parts.next().unwrap().to_owned();
    let gate = std::path::PathBuf::from(parts.next().unwrap());
    let out = std::path::PathBuf::from(parts.next().unwrap());
    let deadline = Instant::now() + Duration::from_secs(20);
    while !gate.exists() {
        assert!(Instant::now() < deadline, "gate file was not created");
        thread::sleep(Duration::from_millis(20));
    }
    let mut command = Command::new("powershell.exe");
    command
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"])
        .arg("Start-Sleep -Seconds 60")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if mode == "breakaway" {
        command.creation_flags(rightkit_child_process::CREATE_BREAKAWAY_FROM_JOB);
    }
    let report = match command.spawn() {
        Ok(child) => format!("pid:{}", child.id()),
        Err(error) => format!("err:{}", error.raw_os_error().unwrap_or(-1)),
    };
    let tmp = out.with_extension("tmp");
    fs::write(&tmp, report).unwrap();
    fs::rename(&tmp, &out).unwrap();
    // Stay alive until the parent has looked, so a dying helper cannot be what ends the grandchild.
    thread::sleep(Duration::from_secs(2));
}

/// Starts the helper, assigns it to `job`, opens the gate and returns what it reported and the
/// helper child. The helper is assigned to `job` (kill-on-close) at once and every caller waits
/// on the returned child, so a panic between spawn and return cannot leave it running.
#[allow(clippy::zombie_processes)]
fn run_breakaway_helper(
    job: &rightkit_child_process::WindowsJob,
    mode: &str,
    dir: &Path,
) -> (String, std::process::Child) {
    let gate = dir.join(format!("{mode}.gate"));
    let out = dir.join(format!("{mode}.out"));
    let _ = fs::remove_file(&gate);
    let _ = fs::remove_file(&out);
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "breakaway_helper_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(
            HELPER_ENV,
            format!("{mode}|{}|{}", gate.display(), out.display()),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let child = command.spawn().unwrap();
    job.assign_child(&child).unwrap();
    fs::write(&gate, "go").unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(text) = fs::read_to_string(&out) {
            return (text, child);
        }
        assert!(Instant::now() < deadline, "helper reported nothing");
        thread::sleep(Duration::from_millis(20));
    }
}

fn reported_pid(report: &str) -> AdoptedProcess {
    let pid: u32 = report
        .strip_prefix("pid:")
        .unwrap_or_else(|| panic!("helper did not create a process: {report}"))
        .parse()
        .unwrap();
    AdoptedProcess::new(NonZeroU32::new(pid).unwrap())
}

fn kill_pid(process: &AdoptedProcess) {
    let _ = Command::new("taskkill.exe")
        .args(["/PID", &process.id().to_string(), "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// With `allow_breakaway(true)` a member that asks for breakaway leaves the job and survives the
/// job closing; a member that does not ask stays contained and dies with it.
#[test]
fn job_with_breakaway_lets_only_a_requesting_descendant_survive() {
    use rightkit_child_process::WindowsJob;

    let dir = std::env::temp_dir().join(format!("rightkit-job-breakaway-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();

    let job = WindowsJob::builder().allow_breakaway(true).build().unwrap();
    let (report, mut helper) = run_breakaway_helper(&job, "breakaway", &dir);
    let survivor = reported_pid(&report);
    let (report, mut helper2) = run_breakaway_helper(&job, "contained", &dir);
    let contained = reported_pid(&report);
    assert!(survivor.is_running().unwrap());
    assert!(contained.is_running().unwrap());

    drop(job);
    let _ = helper.wait();
    let _ = helper2.wait();
    wait_until_stopped(&contained);
    assert!(
        survivor.is_running().unwrap(),
        "the breakaway descendant must outlive the job"
    );
    kill_pid(&survivor);
    wait_until_stopped(&survivor);
    let _ = fs::remove_dir_all(&dir);
}

/// A job built without breakaway refuses a breakaway request with `ERROR_ACCESS_DENIED` (5),
/// which is the behaviour `contain_current_process()` keeps.
#[test]
fn job_without_breakaway_refuses_a_breakaway_request() {
    use rightkit_child_process::WindowsJob;

    let dir = std::env::temp_dir().join(format!("rightkit-job-nobreak-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let job = WindowsJob::builder().build().unwrap();
    let (report, mut helper) = run_breakaway_helper(&job, "breakaway", &dir);
    assert_eq!(report, "err:5");
    job.terminate().unwrap();
    let _ = helper.wait();
    let _ = fs::remove_dir_all(&dir);
}
