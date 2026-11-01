//! The native (rightkit-file-watch) backend must surface source changes on its own:
//! the snapshot interval is set far beyond the test window so a periodic
//! reconcile cannot stand in for native events.
use rightkit_repo_graph::watch::{NativeWatcher, SnapshotConfig};
use std::time::{Duration, Instant};

fn wait_for_event(watcher: &mut NativeWatcher, file: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        let events = watcher.poll(|_| Ok(())).expect("poll succeeds");
        if events.iter().any(|event| event.path.ends_with(file)) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn native_backend_reports_created_and_modified_source_files() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src/lib.rs"), "pub fn a() {}\n").unwrap();
    let config = SnapshotConfig::new(root.path())
        .debounce_ms(20)
        .snapshot_interval_ms(600_000);
    let mut watcher = NativeWatcher::start(config).expect("native watcher starts");

    std::fs::write(root.path().join("src/added.rs"), "pub fn b() {}\n").unwrap();
    assert!(
        wait_for_event(&mut watcher, "added.rs"),
        "no native event for a created file"
    );

    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn a() { let _ = 1; }\n",
    )
    .unwrap();
    assert!(
        wait_for_event(&mut watcher, "lib.rs"),
        "no native event for a modified file"
    );

    watcher.shutdown().expect("watcher shuts down");
}
