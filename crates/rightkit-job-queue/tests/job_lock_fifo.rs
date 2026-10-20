//! `FifoScope::CrossProcess`: waiters for one job lock are served in arrival order across
//! separate `JobLocks` values and across processes, and a waiter that dies leaves the line.
//! Written without running Cargo; not yet run.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rightkit_job_queue::{FifoScope, JobLocks, LockWait};

const DIR_ENV: &str = "RIGHTKIT_JOBS_TEST_FIFO_DIR";
const INDEX_ENV: &str = "RIGHTKIT_JOBS_TEST_FIFO_INDEX";
const OUT_ENV: &str = "RIGHTKIT_JOBS_TEST_FIFO_OUT";
const HOLD_ENV: &str = "RIGHTKIT_JOBS_TEST_FIFO_HOLD_MS";
const JOB: &str = "job-f";

fn cross(dir: &Path) -> JobLocks {
    JobLocks::new(dir).with_fifo_scope(FifoScope::CrossProcess)
}

fn tickets(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir.join(format!("{JOB}.queue")))
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".ticket"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Entry point of the waiter child processes; does nothing in a normal run.
#[test]
fn waiter_entry_point() {
    let Ok(dir) = std::env::var(DIR_ENV) else {
        return;
    };
    let index = std::env::var(INDEX_ENV).unwrap();
    let out = std::env::var(OUT_ENV).unwrap();
    let hold: u64 = std::env::var(HOLD_ENV)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(40);
    let locks = cross(Path::new(&dir));
    let held = locks
        .acquire(JOB, Duration::from_secs(60))
        .unwrap()
        .expect("acquired within the wait");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(out)
        .unwrap();
    writeln!(file, "{index}").unwrap();
    file.flush().unwrap();
    std::thread::sleep(Duration::from_millis(hold));
    drop(held);
}

fn spawn_waiter(dir: &Path, index: usize, out: &Path) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "waiter_entry_point",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(DIR_ENV, dir)
        .env(INDEX_ENV, index.to_string())
        .env(OUT_ENV, out)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn read_order(out: &Path) -> Vec<usize> {
    std::fs::read_to_string(out)
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().parse().unwrap())
        .collect()
}

#[test]
fn default_scope_is_per_process_and_leaves_no_ticket_files() {
    let dir = tempfile::tempdir().unwrap();
    let locks = JobLocks::new(dir.path().join("locks"));
    assert_eq!(locks.fifo_scope(), FifoScope::Process);
    let lock = locks.acquire(JOB, Duration::from_millis(200)).unwrap();
    assert!(lock.is_some());
    assert!(!dir
        .path()
        .join("locks")
        .join(format!("{JOB}.queue"))
        .exists());
}

#[test]
fn separate_job_locks_values_are_served_in_arrival_order() {
    let dir = tempfile::tempdir().unwrap();
    let locks_dir = dir.path().join("locks");
    let held = cross(&locks_dir).try_lock(JOB).unwrap().unwrap();
    let order = Arc::new(Mutex::new(Vec::new()));
    let mut waiters = Vec::new();
    for n in 0..4usize {
        let (waiter_dir, order) = (locks_dir.clone(), Arc::clone(&order));
        waiters.push(std::thread::spawn(move || {
            // A fresh value per waiter: no shared in-process queue, only the ticket files.
            let locks = cross(&waiter_dir);
            let got = locks.acquire(JOB, Duration::from_secs(20)).unwrap();
            assert!(got.is_some());
            order.lock().unwrap().push(n);
            std::thread::sleep(Duration::from_millis(20));
            drop(got);
        }));
        wait_until("the waiter's ticket", || tickets(&locks_dir).len() == n + 1);
    }
    drop(held);
    for w in waiters {
        w.join().unwrap();
    }
    assert_eq!(*order.lock().unwrap(), vec![0, 1, 2, 3]);
    assert!(tickets(&locks_dir).is_empty(), "every ticket is gone");
}

#[test]
fn exclusion_still_holds_and_try_lock_still_bypasses_the_line() {
    let dir = tempfile::tempdir().unwrap();
    let locks = cross(&dir.path().join("locks"));
    let held = locks.acquire(JOB, Duration::ZERO).unwrap().unwrap();
    assert!(locks.try_lock(JOB).unwrap().is_none());
    assert!(locks
        .acquire(JOB, Duration::from_millis(150))
        .unwrap()
        .is_none());
    assert!(tickets(&dir.path().join("locks")).is_empty());
    drop(held);
    assert!(locks.try_lock(JOB).unwrap().is_some());
}

#[test]
fn timed_out_and_cancelled_waiters_leave_the_line() {
    let dir = tempfile::tempdir().unwrap();
    let locks_dir = dir.path().join("locks");
    let held = cross(&locks_dir).try_lock(JOB).unwrap().unwrap();
    assert!(cross(&locks_dir)
        .acquire(JOB, Duration::from_millis(150))
        .unwrap()
        .is_none());
    assert!(tickets(&locks_dir).is_empty(), "timed-out waiter left");

    let cancel = Arc::new(AtomicBool::new(false));
    let waiter = {
        let (locks_dir, cancel) = (locks_dir.clone(), Arc::clone(&cancel));
        std::thread::spawn(move || {
            matches!(
                cross(&locks_dir).acquire_cancellable(JOB, Duration::from_secs(30), || {
                    cancel.load(Ordering::SeqCst)
                }),
                Ok(LockWait::Cancelled)
            )
        })
    };
    wait_until("the waiter's ticket", || tickets(&locks_dir).len() == 1);
    cancel.store(true, Ordering::SeqCst);
    assert!(waiter.join().unwrap());
    assert!(tickets(&locks_dir).is_empty(), "cancelled waiter left");
    drop(held);
    assert!(matches!(
        cross(&locks_dir).acquire_cancellable(JOB, Duration::ZERO, || false),
        Ok(LockWait::Acquired(_))
    ));
}

#[test]
fn a_ticket_nobody_holds_is_swept_and_does_not_block() {
    let dir = tempfile::tempdir().unwrap();
    let locks_dir = dir.path().join("locks");
    let queue = locks_dir.join(format!("{JOB}.queue"));
    std::fs::create_dir_all(&queue).unwrap();
    // What a crashed waiter leaves: a ticket file whose owner (and so whose lock) is gone.
    std::fs::write(queue.join("00000000000000000001-4242.ticket"), b"").unwrap();
    std::fs::write(queue.join("00000000000000000002-4243.ticket"), b"").unwrap();
    let got = cross(&locks_dir)
        .acquire(JOB, Duration::from_secs(5))
        .unwrap();
    assert!(got.is_some(), "dead tickets must not block the line");
    assert_eq!(tickets(&locks_dir).len(), 0);
}

#[test]
fn remove_and_sweep_clear_an_empty_queue_directory_only() {
    let dir = tempfile::tempdir().unwrap();
    let locks_dir = dir.path().join("locks");
    let locks = cross(&locks_dir);
    drop(locks.acquire(JOB, Duration::ZERO).unwrap().unwrap());
    let queue = locks_dir.join(format!("{JOB}.queue"));
    assert!(queue.exists(), "the line's directory outlives the lock");
    assert!(locks.remove(JOB).unwrap());
    assert!(!queue.exists());

    // A waiter in line keeps the directory.
    let held = locks.try_lock(JOB).unwrap().unwrap();
    let waiter = {
        let locks_dir = locks_dir.clone();
        std::thread::spawn(move || {
            cross(&locks_dir)
                .acquire(JOB, Duration::from_secs(20))
                .unwrap()
                .is_some()
        })
    };
    wait_until("the waiter's ticket", || tickets(&locks_dir).len() == 1);
    locks.sweep(|_| false).unwrap();
    assert!(queue.exists() && tickets(&locks_dir).len() == 1);
    drop(held);
    assert!(waiter.join().unwrap());
    // Sweep of a finished job's emptied line.
    locks.sweep(|_| false).unwrap();
    assert!(!queue.exists());
}

#[test]
fn processes_are_served_in_arrival_order() {
    let dir = tempfile::tempdir().unwrap();
    let locks_dir: PathBuf = dir.path().join("locks");
    let out = dir.path().join("order.txt");
    let held = cross(&locks_dir).try_lock(JOB).unwrap().unwrap();
    let mut children = Vec::new();
    for n in 0..4usize {
        children.push(spawn_waiter(&locks_dir, n, &out));
        wait_until("the child's ticket", || tickets(&locks_dir).len() == n + 1);
    }
    drop(held);
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    assert_eq!(read_order(&out), vec![0, 1, 2, 3]);
}

#[test]
fn a_waiter_process_that_dies_leaves_the_line() {
    let dir = tempfile::tempdir().unwrap();
    let locks_dir: PathBuf = dir.path().join("locks");
    let out = dir.path().join("order.txt");
    let held = cross(&locks_dir).try_lock(JOB).unwrap().unwrap();
    let mut doomed = spawn_waiter(&locks_dir, 0, &out);
    wait_until("the first ticket", || tickets(&locks_dir).len() == 1);
    let mut survivors = Vec::new();
    for n in 1..3usize {
        survivors.push(spawn_waiter(&locks_dir, n, &out));
        wait_until("the next ticket", || tickets(&locks_dir).len() == n + 1);
    }
    doomed.kill().unwrap();
    doomed.wait().unwrap();
    drop(held);
    for mut child in survivors {
        assert!(child.wait().unwrap().success());
    }
    assert_eq!(
        read_order(&out),
        vec![1, 2],
        "the dead head did not block or reorder"
    );
}
