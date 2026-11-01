use super::*;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[test]
fn model_operation_lock_serializes_same_model_only() {
    let model = format!("lock-test-{}", std::process::id());
    let first = model_operation_lock(&model);
    let first_guard = first.lock().unwrap();
    let second = model_operation_lock(&model);
    assert!(Arc::ptr_eq(&first, &second));
    let other = model_operation_lock("lock-test-other");
    assert!(other.try_lock().is_ok());

    let (ready_tx, ready_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let _guard = second.lock().unwrap();
        ready_tx.send(()).unwrap();
    });
    assert!(ready_rx.recv_timeout(Duration::from_millis(50)).is_err());
    drop(first_guard);
    assert!(ready_rx.recv_timeout(Duration::from_secs(1)).is_ok());
    worker.join().unwrap();
}

#[test]
fn partial_archive_path_is_scoped_to_artifact_checksum() {
    let root = Path::new("/tmp/models");
    let first = partial_archive_path(root, "parakeet-tdt", "AA");
    let second = partial_archive_path(root, "parakeet-tdt", "BB");
    assert_ne!(first, second);
    assert!(first.to_string_lossy().contains("aa"));
    assert!(second.to_string_lossy().contains("bb"));
}
