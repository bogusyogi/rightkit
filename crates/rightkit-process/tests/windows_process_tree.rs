#![cfg(windows)]

use std::{
    fs,
    num::NonZeroU32,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use rightkit_process::{AdoptedProcess, OwnedCommand, WaitOutcome};

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
