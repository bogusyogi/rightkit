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

#[test]
fn acquire_waits_times_out_and_serves_waiters_in_arrival_order() {
    use std::sync::Mutex;
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let locks = JobLocks::new(dir.path().join("locks"));
    assert_eq!(locks.dir(), dir.path().join("locks"));
    assert_eq!(
        locks.lock_path("job-9").unwrap(),
        dir.path().join("locks").join("job-9.lock")
    );
    assert!(locks.lock_path("a/b").is_err());
    let held = locks.try_lock("job-9").unwrap().unwrap();
    // Bounded wait against a live holder times out with no lock.
    assert!(locks
        .acquire("job-9", Duration::from_millis(150))
        .unwrap()
        .is_none());
    let order = Arc::new(Mutex::new(Vec::new()));
    let mut waiters = Vec::new();
    for n in 0..3 {
        let (locks, order) = (locks.clone(), Arc::clone(&order));
        waiters.push(std::thread::spawn(move || {
            let got = locks.acquire("job-9", Duration::from_secs(10)).unwrap();
            assert!(got.is_some());
            order.lock().unwrap().push(n);
            drop(got);
        }));
        std::thread::sleep(Duration::from_millis(60));
    }
    drop(held);
    for w in waiters {
        w.join().unwrap();
    }
    assert_eq!(*order.lock().unwrap(), vec![0, 1, 2], "FIFO within process");
}
