#![cfg(all(unix, any(target_os = "linux", target_os = "macos")))]

//! A tied child must reach "ready" promptly whichever thread spawns it. libtest runs a test
//! body on the process main thread under `--test-threads=1` and on a worker thread
//! otherwise, so this file is run both ways. The macOS tie makes `spawn` wait for its helper
//! to confirm the watch; that wait must stay far below a caller's own readiness budget.

use std::{
    io::{BufRead, BufReader},
    process::Stdio,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use rightkit_process::OwnedCommand;

const READY_BUDGET: Duration = Duration::from_secs(1);

/// Spawns `sh` that prints `ready`, with piped stdio like a sidecar. Returns the spawn time
/// and the time until the first line arrived on the child's stdout.
fn spawn_and_wait_ready() -> (Duration, Duration) {
    let mut command = OwnedCommand::new("/bin/sh");
    command
        .command_mut()
        .args(["-c", "echo ready; exec sleep 30"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.kill_on_parent_exit(true);
    let started = Instant::now();
    let mut child = command.spawn().expect("spawn");
    let spawned = started.elapsed();
    let stdout = child.take_stdout().expect("stdout");
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = sender.send(line);
    });
    let line = receiver
        .recv_timeout(READY_BUDGET)
        .expect("child reported ready within 1 s");
    assert_eq!(line.trim(), "ready");
    let ready = started.elapsed();
    let _ = child.terminate_tree();
    (spawned, ready)
}

#[test]
fn ready_within_budget_from_the_test_thread() {
    let (spawned, ready) = spawn_and_wait_ready();
    eprintln!("current-thread spawn {spawned:?}, ready {ready:?}");
    assert!(
        spawned < Duration::from_millis(800),
        "spawn took {spawned:?}"
    );
}

#[test]
fn ready_within_budget_from_a_worker_thread() {
    let (spawned, ready) = thread::spawn(spawn_and_wait_ready).join().unwrap();
    eprintln!("worker-thread spawn {spawned:?}, ready {ready:?}");
    assert!(
        spawned < Duration::from_millis(800),
        "spawn took {spawned:?}"
    );
}

/// Apps often raise the descriptor limit; the helper closes every descriptor below the
/// bound, so a very high limit must not turn into a long spawn.
#[test]
fn ready_within_budget_with_a_raised_descriptor_limit() {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: plain getrlimit/setrlimit on this process.
    unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) };
    let before = limit.rlim_cur;
    for wanted in [1_048_576u64, 524_288, 262_144, 65_536, 10_240] {
        let wanted = wanted.min(limit.rlim_max);
        let raised = libc::rlimit {
            rlim_cur: wanted,
            rlim_max: limit.rlim_max,
        };
        // SAFETY: as above.
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raised) } == 0 {
            break;
        }
    }
    let (spawned, ready) = spawn_and_wait_ready();
    // SAFETY: restore.
    unsafe {
        libc::setrlimit(
            libc::RLIMIT_NOFILE,
            &libc::rlimit {
                rlim_cur: before,
                rlim_max: limit.rlim_max,
            },
        )
    };
    eprintln!("raised-limit spawn {spawned:?}, ready {ready:?}");
    assert!(
        spawned < Duration::from_millis(800),
        "spawn took {spawned:?}"
    );
}
