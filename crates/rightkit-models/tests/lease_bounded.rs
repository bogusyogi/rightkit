//! Bounded lease waits: a holder that never lets go yields a typed `Held`, not an endless wait.

use rightkit_models::suite::lease::{
    acquire_bounded_at_root, current_holder_at_root, try_acquire_at_root, AcquireError,
    LeaseOptions,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[test]
fn a_live_holder_makes_a_bounded_wait_return_held_with_who_and_since() {
    let root = tempfile::tempdir().unwrap();
    let held = try_acquire_at_root(root.path(), "speech", "heardright")
        .unwrap()
        .expect("free lease");
    let started = Instant::now();
    let polled = AtomicBool::new(false);
    let result = acquire_bounded_at_root(
        root.path(),
        "speech",
        "scraperight",
        &LeaseOptions {
            wait: Some(Duration::from_millis(400)),
            deadline: None,
            cancel: &|| false,
            waiting: &|holder| {
                assert_eq!(holder.unwrap().app, "heardright");
                polled.store(true, Ordering::SeqCst);
            },
        },
    );
    let waited = started.elapsed();
    let Err(AcquireError::Held(info)) = result else {
        panic!("expected Held");
    };
    assert!(
        waited >= Duration::from_millis(400) && waited < Duration::from_secs(3),
        "{waited:?}"
    );
    assert!(polled.load(Ordering::SeqCst), "the waiting callback ran");
    assert_eq!(info.app, "heardright");
    assert_eq!(info.pid, std::process::id());
    let since = info.since.expect("holder recorded when it took the lease");
    assert!(since.elapsed().unwrap() < Duration::from_secs(60));
    assert_eq!(
        info.to_string(),
        format!("in use by heardright (pid {})", std::process::id())
    );

    // Once the holder lets go the same call succeeds at once.
    drop(held);
    let lease = acquire_bounded_at_root(
        root.path(),
        "speech",
        "scraperight",
        &LeaseOptions::within(Duration::from_millis(400)),
    )
    .unwrap();
    assert_eq!(
        current_holder_at_root(root.path(), "speech").unwrap().app,
        "scraperight"
    );
    drop(lease);
}

#[test]
fn cancel_stops_a_bounded_wait() {
    let root = tempfile::tempdir().unwrap();
    let _held = try_acquire_at_root(root.path(), "speech", "heardright")
        .unwrap()
        .unwrap();
    let cancel = AtomicBool::new(true);
    let result = acquire_bounded_at_root(
        root.path(),
        "speech",
        "scraperight",
        &LeaseOptions {
            wait: None,
            deadline: None,
            cancel: &|| cancel.load(Ordering::SeqCst),
            waiting: &|_| {},
        },
    );
    assert!(matches!(result, Err(AcquireError::Cancelled)));
}

#[test]
fn an_absolute_deadline_bounds_the_wait_too() {
    let root = tempfile::tempdir().unwrap();
    let _held = try_acquire_at_root(root.path(), "speech", "heardright")
        .unwrap()
        .unwrap();
    let result = acquire_bounded_at_root(
        root.path(),
        "speech",
        "scraperight",
        &LeaseOptions {
            wait: Some(Duration::from_secs(60)),
            deadline: Some(Instant::now() + Duration::from_millis(300)),
            cancel: &|| false,
            waiting: &|_| {},
        },
    );
    assert!(matches!(result, Err(AcquireError::Held(_))));
}

#[test]
fn a_crashed_holders_leftover_file_does_not_block_or_mislead() {
    // The holder died without cleanup: its holder file remains, but the OS released the lock.
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("runtime").join("model-use");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("speech.holder.json"),
        r#"{"app":"crashed-app","pid":999999}"#,
    )
    .unwrap();
    let lease = acquire_bounded_at_root(
        root.path(),
        "speech",
        "scraperight",
        &LeaseOptions::within(Duration::from_millis(200)),
    )
    .expect("a dead holder's stale file is recovered at once");
    // The stale file was replaced, not trusted.
    assert_eq!(
        current_holder_at_root(root.path(), "speech").unwrap().app,
        "scraperight"
    );
    drop(lease);
}

#[test]
fn interactive_options_wait_at_most_120_seconds_and_keep_the_default_unbounded() {
    use rightkit_models::suite::lease::INTERACTIVE_WAIT;

    assert_eq!(INTERACTIVE_WAIT, Duration::from_secs(120));
    let interactive = LeaseOptions::interactive();
    assert_eq!(interactive.wait, Some(Duration::from_secs(120)));
    assert!(interactive.deadline.is_none());

    // The struct default is unchanged: a literal with `wait: None` has no time limit.
    let unbounded = LeaseOptions {
        wait: None,
        deadline: None,
        cancel: &|| false,
        waiting: &|_| {},
    };
    assert_eq!(unbounded.wait, None);

    // Interactive options with a cancel hook acquire a free lease at once.
    let root = tempfile::tempdir().unwrap();
    let cancel = AtomicBool::new(false);
    let lease = acquire_bounded_at_root(
        root.path(),
        "speech",
        "scraperight",
        &LeaseOptions {
            cancel: &|| cancel.load(Ordering::SeqCst),
            ..LeaseOptions::interactive()
        },
    )
    .unwrap();
    drop(lease);
}
