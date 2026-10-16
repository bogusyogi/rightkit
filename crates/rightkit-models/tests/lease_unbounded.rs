//! The unbounded wait (`acquire_at_root`, the root-explicit form of `ModelLease::acquire`) stops
//! only when its `give_up` hook says so or the lease frees up. These tests pin both exits and guard
//! against a regression turning them into a hang.

use rightkit_models::suite::lease::{acquire_at_root, try_acquire_at_root};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

/// Hard ceiling: a regression fails the test instead of hanging CI.
const GUARD: Duration = Duration::from_secs(20);

/// Run `acquire_at_root` on a worker thread. `give_up` is consulted every poll; the worker also
/// stops itself if `stop` is set, so a failing test does not leak a looping thread.
fn spawn_acquire(
    root: PathBuf,
    give_up_after: Option<usize>,
    polls: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
) -> mpsc::Receiver<Result<(), String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = acquire_at_root(&root, "speech", "scraperight", &|_| {}, &|| {
            let n = polls.fetch_add(1, Ordering::SeqCst) + 1;
            stop.load(Ordering::SeqCst) || give_up_after.is_some_and(|limit| n >= limit)
        })
        .map(drop);
        let _ = tx.send(result);
    });
    rx
}

#[test]
fn give_up_ends_an_unbounded_wait_on_a_live_holder() {
    let root = tempfile::tempdir().unwrap();
    let _held = try_acquire_at_root(root.path(), "speech", "heardright")
        .unwrap()
        .expect("free lease");
    let polls = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let started = Instant::now();
    let rx = spawn_acquire(
        root.path().to_path_buf(),
        Some(3),
        Arc::clone(&polls),
        Arc::clone(&stop),
    );
    let outcome = rx.recv_timeout(GUARD);
    stop.store(true, Ordering::SeqCst);
    let result = outcome.expect("acquire must return once give_up is true, not hang");
    assert_eq!(result, Err("cancelled".to_string()));
    assert_eq!(
        polls.load(Ordering::SeqCst),
        3,
        "the wait consulted give_up on every failed attempt and stopped at the third"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn a_holder_releasing_before_give_up_lets_the_unbounded_wait_succeed() {
    let root = tempfile::tempdir().unwrap();
    let held = try_acquire_at_root(root.path(), "speech", "heardright")
        .unwrap()
        .expect("free lease");
    let polls = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    // give_up never fires on its own: success must come from the release.
    let rx = spawn_acquire(
        root.path().to_path_buf(),
        None,
        Arc::clone(&polls),
        Arc::clone(&stop),
    );
    // Let the waiter poll at least once against the live holder, then release.
    let deadline = Instant::now() + Duration::from_secs(10);
    while polls.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        polls.load(Ordering::SeqCst) >= 1,
        "the waiter polled while held"
    );
    drop(held);
    let outcome = rx.recv_timeout(GUARD);
    stop.store(true, Ordering::SeqCst);
    let result = outcome.expect("acquire must return after the holder releases, not hang");
    assert_eq!(result, Ok(()));
}
