//! Job locks must not look held because another thread forked a child.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rightkit_job_queue::JobLocks;

#[test]
fn released_lock_is_free_while_other_threads_spawn_children() {
    let dir = tempfile::tempdir().unwrap();
    let locks = JobLocks::new(dir.path().join("locks"));
    let stop = Arc::new(AtomicBool::new(false));
    let spawner = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                let _ = std::process::Command::new("sh")
                    .args(["-c", "true"])
                    .status();
            }
        })
    };
    for round in 0..3000 {
        let held = locks
            .try_lock("job-1")
            .unwrap()
            .unwrap_or_else(|| panic!("round {round}: free lock reported busy"));
        assert!(
            locks.try_lock("job-1").unwrap().is_none(),
            "round {round}: live holder must block"
        );
        drop(held);
        assert!(
            locks.try_lock("job-1").unwrap().is_some(),
            "round {round}: released lock reported busy"
        );
    }
    stop.store(true, Ordering::SeqCst);
    spawner.join().unwrap();
}
