//! End to end: `OwnedCommand::kill_on_parent_exit` takes the child's whole process group with the
//! supervisor, including what the child started (CodeRight #448: a `sh -c` command outlived a
//! SIGKILLed daemon because Linux's parent-death signal reaches the direct child only).
//!
//! The test binary runs itself a second time as the supervisor (`RK_PARENT_DEATH_ROLE`). The
//! supervisor starts `sh -c 'sleep A & sleep B'` tied to it, so the shell's children are not the
//! supervisor's own children, prints the shell's pid and waits. The test kills the supervisor with
//! SIGKILL (no cleanup runs) and expects both sleeps to be gone within three seconds.
#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const ROLE: &str = "RK_PARENT_DEATH_ROLE";
const TAG: &str = "RK_PARENT_DEATH_TAG";

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
            // The command itself must be `sleep`: the shell's own line names the sleeps too.
            let program = fields.next()?;
            let arg = fields.next()?;
            (program.ends_with("sleep") && arg.starts_with(&format!("300.{tag}"))).then_some(pid)
        })
        .collect()
}

#[test]
fn a_killed_supervisor_takes_the_childs_whole_group_with_it() {
    if std::env::var(ROLE).as_deref() == Ok("supervisor") {
        let tag = std::env::var(TAG).expect("tag");
        let mut shell = Command::new("sh");
        shell.args(["-c", &format!("sleep 300.{tag}1 & sleep 300.{tag}2")]);
        let mut owned = rightkit_child_process::OwnedCommand::from_command(shell);
        owned.kill_on_parent_exit(true);
        let child = owned.spawn().expect("tied spawn");
        println!("shell {}", child.id());
        std::thread::sleep(Duration::from_secs(600));
        return;
    }

    let tag = format!("{}", std::process::id() % 9000 + 1000);
    let mut supervisor = Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "a_killed_supervisor_takes_the_childs_whole_group_with_it",
            "--nocapture",
        ])
        .env(ROLE, "supervisor")
        .env(TAG, &tag)
        .stdout(Stdio::piped())
        .spawn()
        .expect("supervisor starts");
    // The test harness prints its own lines first; keep draining the pipe after the report so
    // the supervisor never writes into a closed pipe.
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
        "both sleeps should be running before the kill"
    );

    let before = Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,pgid=,sid=,stat=,args="])
        .output()
        .expect("ps");
    let before = String::from_utf8_lossy(&before.stdout).into_owned();
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
    assert!(
        survivors.is_empty(),
        "sleeps {survivors:?} outlived the SIGKILLed supervisor; process table at the kill (supervisor {}):\n{}",
        supervisor.id(),
        before.lines().filter(|l| l.contains("sleep") || l.contains("parent_death")).collect::<Vec<_>>().join("\n")
    );
}
