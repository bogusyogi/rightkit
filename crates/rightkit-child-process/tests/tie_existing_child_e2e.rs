//! End to end: `tie_group_to_parent` ties a child the caller spawned itself (rightkit#1688,
//! CodeRight #448). The test binary runs itself again as the supervisor (`RK_TIE_ROLE`). The
//! supervisor spawns `sh -c 'sleep A & sleep B'` with a plain `Command` in its own process group,
//! ties it (or not), prints the shell's pid and waits. The test SIGKILLs the supervisor: tied,
//! both sleeps are gone within three seconds; untied, they are still running (which is what the
//! tie is for, and shows the test can tell the difference).
#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const ROLE: &str = "RK_TIE_ROLE";
const TAG: &str = "RK_TIE_TAG";

fn sleeps_running(tag: &str) -> Vec<u32> {
    let output = Command::new("ps")
        .args(["-A", "-o", "pid=,args="])
        .output()
        .expect("ps runs");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let program = fields.next()?;
            let arg = fields.next()?;
            (program.ends_with("sleep") && arg.starts_with(&format!("300.{tag}"))).then_some(pid)
        })
        .collect()
}

/// Supervisor half: spawn the shell, tie it when `role` says so, report, wait to be killed.
fn supervise(role: &str) {
    let tag = std::env::var(TAG).expect("tag");
    let mut shell = Command::new("sh");
    shell
        .args(["-c", &format!("sleep 300.{tag}1 & sleep 300.{tag}2")])
        .process_group(0);
    let child = shell.spawn().expect("shell starts");
    if role == "tied" {
        rightkit_child_process::tie_group_to_parent(child.id()).expect("tie");
    }
    println!("shell {}", child.id());
    // `sh` exits once it has backgrounded both sleeps, and the sleeps keep their own process
    // group after that. Reap the shell so the supervisor does not leave a zombie behind.
    let _ = child.wait();
    std::thread::sleep(Duration::from_secs(600));
}

/// Runs the supervisor for `test`, SIGKILLs it once both sleeps run, and returns the sleeps
/// still alive three seconds later (after killing them).
fn survivors_after_kill(test: &str, role: &str, tag_base: u32) -> Vec<u32> {
    let tag = format!("{}", std::process::id() % 900 + tag_base);
    let mut supervisor = Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", test, "--nocapture"])
        .env(ROLE, role)
        .env(TAG, &tag)
        .stdout(Stdio::piped())
        .spawn()
        .expect("supervisor starts");
    let (report, reported) = std::sync::mpsc::channel();
    let stdout = supervisor.stdout.take().expect("stdout");
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if line.starts_with("shell ") {
                let _ = report.send(line);
            }
        }
    });
    reported
        .recv_timeout(Duration::from_secs(20))
        .expect("the supervisor reports the shell it started");
    let started = Instant::now();
    while sleeps_running(&tag).len() < 2 && started.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        sleeps_running(&tag).len(),
        2,
        "both sleeps run before the kill"
    );

    supervisor.kill().expect("SIGKILL the supervisor");
    let _ = supervisor.wait();
    let killed = Instant::now();
    while !sleeps_running(&tag).is_empty() && killed.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(50));
    }
    let survivors = sleeps_running(&tag);
    for pid in &survivors {
        let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
    }
    survivors
}

#[test]
fn a_tied_child_group_dies_with_the_killed_supervisor() {
    if let Ok(role) = std::env::var(ROLE) {
        return supervise(&role);
    }
    let survivors = survivors_after_kill(
        "a_tied_child_group_dies_with_the_killed_supervisor",
        "tied",
        1000,
    );
    assert!(
        survivors.is_empty(),
        "sleeps {survivors:?} outlived the supervisor"
    );
}

#[test]
fn an_untied_child_group_outlives_the_killed_supervisor() {
    if let Ok(role) = std::env::var(ROLE) {
        return supervise(&role);
    }
    let survivors = survivors_after_kill(
        "an_untied_child_group_outlives_the_killed_supervisor",
        "untied",
        2000,
    );
    assert_eq!(
        survivors.len(),
        2,
        "without the tie both sleeps keep running"
    );
}

#[test]
fn a_child_that_shares_its_parents_group_is_refused() {
    if std::env::var(ROLE).is_ok() {
        return;
    }
    let mut child = Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("sleep starts");
    let refused = rightkit_child_process::tie_group_to_parent(child.id());
    let _ = child.kill();
    let _ = child.wait();
    let error = refused.expect_err("a child in the caller's group is refused");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{error}");
}
