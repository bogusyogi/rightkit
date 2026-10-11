//! `TypedWatcher::metrics` keeps its counters consistent with what the caller received.

use std::time::Duration;

use rightkit_watch::TypedWatcher;

#[test]
fn metrics_count_queued_received_and_backlog() {
    let dir = tempfile::tempdir().unwrap();
    let watch = TypedWatcher::watch(dir.path()).unwrap();
    assert_eq!(watch.metrics().queued_total, 0);
    assert_eq!(watch.metrics().received_total, 0);

    std::fs::write(dir.path().join("a.txt"), b"a").unwrap();
    watch
        .recv_timeout(Duration::from_secs(20))
        .expect("an event for the new file");
    let m = watch.metrics();
    assert!(m.received_total >= 1, "{m:?}");
    assert!(m.queued_total >= m.received_total, "{m:?}");
    assert_eq!(m.queue_depth, m.queued_total - m.received_total, "{m:?}");

    // Typed and record receives share one counter; the backlog never goes negative.
    std::fs::write(dir.path().join("b.txt"), b"b").unwrap();
    let _ = watch.recv_record_timeout(Duration::from_secs(20));
    while watch.try_recv_record().is_ok() {}
    let after = watch.metrics();
    assert!(after.received_total > m.received_total, "{after:?}");
    assert_eq!(
        after.queue_depth,
        after.queued_total - after.received_total,
        "{after:?}"
    );
    assert_eq!(after.overflow_total, 0, "{after:?}");
}
