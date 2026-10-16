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
    let mut child = tree_command(&pid_file).spawn().unwrap();
    job.assign_process_id(child.id()).unwrap();
    let grandchild = AdoptedProcess::new(NonZeroU32::new(wait_for_pid(&pid_file)).unwrap());
    job.terminate().unwrap();
    let _ = child.wait();
    wait_until_stopped(&grandchild);
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
