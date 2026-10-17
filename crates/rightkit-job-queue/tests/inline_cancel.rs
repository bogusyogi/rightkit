//! Public API: cancelling inline work needs neither a store nor protocol features.
use rightkit_job_queue::{InlineCancelRegistry, JOB_EXISTS};
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc, Barrier};
use std::time::Duration;

#[test]
fn cancel_across_requests_then_reuse_id() {
    let registry = InlineCancelRegistry::new();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let worker_registry = registry.clone();
    let worker = std::thread::spawn(move || {
        let (_guard, flag) = worker_registry.register("render-42").unwrap();
        ready_tx.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !flag.load(Ordering::SeqCst) {
            assert!(
                std::time::Instant::now() < deadline,
                "cancel was not observed"
            );
            std::thread::yield_now();
        }
        done_tx.send(()).unwrap();
    });
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        registry.register("render-42").err().unwrap().code,
        JOB_EXISTS
    );
    assert!(registry.cancel("render-42"));
    done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    worker.join().unwrap();
    assert!(!registry.cancel("render-42"));
    let (_guard, fresh) = registry.register("render-42").unwrap();
    assert!(!fresh.load(Ordering::SeqCst));
}

#[test]
fn concurrent_registration_has_one_owner() {
    let registry = InlineCancelRegistry::new();
    let start = Arc::new(Barrier::new(8));
    let finish = Arc::new(Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let registry = registry.clone();
            let start = Arc::clone(&start);
            let finish = Arc::clone(&finish);
            std::thread::spawn(move || {
                start.wait();
                let registration = registry.register("assemble-1");
                finish.wait(); // keep winning guard until all registrations attempted
                registration.is_ok()
            })
        })
        .collect();
    let winners = threads
        .into_iter()
        .map(|t| usize::from(t.join().unwrap()))
        .sum::<usize>();
    assert_eq!(winners, 1);
    assert!(!registry.cancel("assemble-1"));
}
