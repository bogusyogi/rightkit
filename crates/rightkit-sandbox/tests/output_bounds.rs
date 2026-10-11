//! Bounded output capture and a deadline that covers the whole process tree
//! and its pipes. Unix shell fixtures; the Windows build compiles these tests
//! out, since they depend on `/bin/sh`.

#![cfg(unix)]

use rightkit_sandbox::*;
use std::time::{Duration, Instant};

fn sandbox() -> Sandbox {
    Sandbox::new(ResourceAuthority::new(ResourceProfile::Expanded))
}

fn sh(script: &str) -> SandboxCommand {
    SandboxCommand::new("/bin/sh").arg("-c").arg(script)
}

#[test]
fn normal_run_is_unchanged() {
    let child = sandbox()
        .spawn_supervised(
            sh("echo hello; echo oops 1>&2; exit 0"),
            &EnvPolicy::default(),
        )
        .unwrap();
    let out = child.wait_with_output(Duration::from_secs(10)).unwrap();
    assert!(!out.timed_out);
    assert!(!out.truncated);
    assert!(out.status.unwrap().success());
    assert_eq!(out.stdout, b"hello\n");
    assert_eq!(out.stderr, b"oops\n");
}

#[test]
fn grandchild_holding_stdout_cannot_exceed_the_deadline() {
    // The direct child exits at once; a background grandchild keeps stdout open.
    let child = sandbox()
        .spawn_supervised(sh("sleep 30 & echo started; exit 0"), &EnvPolicy::default())
        .unwrap();
    let t0 = Instant::now();
    let out = child.wait_with_output(Duration::from_secs(1)).unwrap();
    let elapsed = t0.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "returned after {elapsed:?}, the grandchild must not extend the deadline"
    );
    assert!(
        out.timed_out,
        "pipes held open past the deadline are a timeout"
    );
    assert!(
        !out.truncated,
        "killing the tree closed the pipes, so the capture is complete"
    );
    assert_eq!(
        out.stdout, b"started\n",
        "output produced before the deadline is kept"
    );
}

#[test]
fn child_that_outlives_the_deadline_is_killed_with_its_tree() {
    let child = sandbox()
        .spawn_supervised(sh("sleep 30 & sleep 30"), &EnvPolicy::default())
        .unwrap();
    let t0 = Instant::now();
    let out = child.wait_with_output(Duration::from_millis(500)).unwrap();
    assert!(t0.elapsed() < Duration::from_secs(5));
    assert!(out.timed_out);
}

#[test]
fn huge_output_is_capped_and_flagged() {
    let script = "i=0; while [ $i -lt 4000 ]; do printf 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\\n'; i=$((i+1)); done; \
                  printf 'eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee' 1>&2; exit 0";
    let child = sandbox()
        .spawn_supervised(sh(script), &EnvPolicy::default())
        .unwrap();
    let out = child
        .wait_with_output_capped(Duration::from_secs(20), 1024)
        .unwrap();
    assert!(!out.timed_out, "a capped run still finishes normally");
    assert!(out.truncated, "bytes were discarded past the cap");
    assert!(
        out.status.unwrap().success(),
        "the child is never blocked by the cap"
    );
    assert_eq!(out.stdout.len(), 1024);
    assert!(out.stdout.iter().all(|b| *b == b'x' || *b == b'\n'));
    assert_eq!(
        out.stderr,
        vec![b'e'; 62],
        "small stream under the cap is kept whole"
    );
}

#[test]
fn small_output_under_cap_is_not_truncated() {
    let child = sandbox()
        .spawn_supervised(sh("printf 'abc'; exit 0"), &EnvPolicy::default())
        .unwrap();
    let out = child
        .wait_with_output_capped(Duration::from_secs(10), 3)
        .unwrap();
    assert!(!out.truncated, "exactly at the cap is not truncation");
    assert_eq!(out.stdout, b"abc");
}

/// Whether `pid` is running and not merely a zombie awaiting a reaper.
fn process_running(pid: i32) -> bool {
    let state = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default();
    !state.is_empty() && !state.starts_with('Z')
}

#[test]
fn a_descendant_that_redirected_its_output_does_not_outlive_a_normal_run() {
    // The leader exits 0 and both pipes close at once, but the sleeper keeps running with its
    // output sent elsewhere. A normal completion must still take the group down; before, the
    // final reap was a bare `wait`, which left the sleeper behind.
    let child = sandbox()
        .spawn_supervised(
            sh("sleep 600 </dev/null >/dev/null 2>&1 & echo $!; exit 0"),
            &EnvPolicy::default(),
        )
        .unwrap();
    let out = child.wait_with_output(Duration::from_secs(10)).unwrap();
    assert!(!out.timed_out);
    assert!(out.status.unwrap().success());
    let pid: i32 = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .expect("the script prints the sleeper's pid");
    let deadline = Instant::now() + Duration::from_secs(10);
    while process_running(pid) {
        assert!(
            Instant::now() < deadline,
            "descendant {pid} survived a normal run"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
