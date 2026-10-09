#![cfg(unix)]
//! A job lock held by another process reads as busy, and frees when that process dies.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use rightkit_jobs::JobLocks;

const HOLDER_ENV: &str = "RIGHTKIT_JOBS_TEST_LOCK_HOLDER";

#[test]
fn holder_entry_point() {
    let Ok(dir) = std::env::var(HOLDER_ENV) else {
        return;
    };
    let locks = JobLocks::new(dir);
    let _held = locks
        .try_lock("job-x")
        .unwrap()
        .expect("free in the holder");
    println!("HELD");
    std::io::stdout().flush().unwrap();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
}

#[test]
fn lock_in_another_process_is_busy_until_that_process_dies() {
    let dir = tempfile::tempdir().unwrap();
    let locks_dir = dir.path().join("locks");
    let mut holder = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "holder_entry_point",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(HOLDER_ENV, &locks_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(holder.stdout.take().unwrap());
    let mut line = String::new();
    while !line.contains("HELD") {
        line.clear();
        assert!(
            reader.read_line(&mut line).unwrap() > 0,
            "holder exited early"
        );
    }

    let locks = JobLocks::new(&locks_dir);
    assert!(
        locks.try_lock("job-x").unwrap().is_none(),
        "live holder must block"
    );
    assert!(
        locks.try_lock("job-y").unwrap().is_some(),
        "other jobs stay free"
    );

    holder.kill().unwrap();
    holder.wait().unwrap();
    assert!(
        locks.try_lock("job-x").unwrap().is_some(),
        "a dead runner's job is recoverable"
    );
}
