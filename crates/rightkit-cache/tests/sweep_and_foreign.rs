//! Temp-file sweep on open, owner sidecars, foreign-file reporting, and `clear` accounting.

use rightkit_cache::{Config, DiskCache, Key};
use std::fs::{self, FileTimes};
use std::io::ErrorKind;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

/// A pid that belonged to a process which has now exited, so it is dead.
fn dead_pid() -> u32 {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .arg("no_such_test_filter_for_dead_pid")
        .spawn()
        .unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

fn owner_sidecar(tmp_file: &Path) -> std::path::PathBuf {
    let mut name = tmp_file.as_os_str().to_owned();
    name.push(".owner-pid");
    name.into()
}

fn age(path: &Path, by: Duration) {
    let when = SystemTime::now() - by;
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(when))
        .unwrap();
}

#[test]
fn open_keeps_a_live_owners_fresh_temp_file() {
    let root = tempfile::tempdir().unwrap();
    let cache = DiskCache::open(root.path(), Config::new(1 << 20)).unwrap();
    // The reservation is alive in this process, so its owner pid is alive and its file is fresh.
    let reservation = cache.reserve_path(&Key::new(&["live"]), "bin").unwrap();
    fs::write(reservation.path(), b"in flight").unwrap();
    let kept = reservation.path().to_path_buf();

    let _second = DiskCache::open(root.path(), Config::new(1 << 20)).unwrap();
    assert!(
        kept.exists(),
        "open must not wipe a live producer's temp file"
    );
    assert!(owner_sidecar(&kept).exists());
    drop(reservation);
    assert!(
        !kept.exists(),
        "dropping a reservation removes its file and sidecar"
    );
    assert!(!owner_sidecar(&kept).exists());
}

#[test]
fn open_removes_temp_files_whose_owner_is_dead_even_when_fresh() {
    let root = tempfile::tempdir().unwrap();
    let tmp = root.path().join(".tmp");
    fs::create_dir_all(&tmp).unwrap();
    let orphan = tmp.join("1-1.bin");
    fs::write(&orphan, b"orphan").unwrap();
    fs::write(owner_sidecar(&orphan), dead_pid().to_string()).unwrap();

    let _cache = DiskCache::open(root.path(), Config::new(1 << 20)).unwrap();
    assert!(!orphan.exists(), "a dead owner's fresh temp file is swept");
    assert!(
        !owner_sidecar(&orphan).exists(),
        "its sidecar is swept with it"
    );
}

#[test]
fn open_removes_temp_files_older_than_an_hour_and_keeps_younger_ones() {
    let root = tempfile::tempdir().unwrap();
    let tmp = root.path().join(".tmp");
    fs::create_dir_all(&tmp).unwrap();
    let old = tmp.join("1-1.bin");
    let young = tmp.join("1-2.bin");
    fs::write(&old, b"old").unwrap();
    fs::write(&young, b"young").unwrap();
    age(&old, Duration::from_secs(2 * 60 * 60));
    age(&young, Duration::from_secs(10 * 60));

    let _cache = DiskCache::open(root.path(), Config::new(1 << 20)).unwrap();
    assert!(!old.exists(), "a temp file older than 1 h is swept");
    assert!(
        young.exists(),
        "a younger temp file without a dead owner is kept"
    );
}

#[test]
fn an_orphaned_sidecar_is_removed_on_open() {
    let root = tempfile::tempdir().unwrap();
    let tmp = root.path().join(".tmp");
    fs::create_dir_all(&tmp).unwrap();
    let orphan = tmp.join("2-2.bin");
    fs::write(owner_sidecar(&orphan), std::process::id().to_string()).unwrap();

    let _cache = DiskCache::open(root.path(), Config::new(1 << 20)).unwrap();
    assert!(!owner_sidecar(&orphan).exists());
}

#[test]
fn foreign_files_reports_only_files_the_cache_did_not_write() {
    let root = tempfile::tempdir().unwrap();
    let cache = DiskCache::open(root.path(), Config::new(1 << 20)).unwrap();
    cache.put(&Key::new(&["entry"]), b"cached").unwrap();
    assert_eq!(cache.foreign_files().unwrap().count, 0);

    fs::write(root.path().join("stray.txt"), b"12345").unwrap();
    let shard = root.path().join("ab");
    fs::create_dir_all(shard.join("nested")).unwrap();
    fs::write(shard.join("notes.txt"), b"123").unwrap();
    fs::write(shard.join("nested").join("deep.bin"), b"1234567").unwrap();
    // The cache's own temp directory is not foreign.
    fs::create_dir_all(root.path().join(".tmp")).unwrap();
    fs::write(root.path().join(".tmp").join("1-1.bin"), b"tmp").unwrap();

    let found = cache.foreign_files().unwrap();
    assert_eq!((found.count, found.bytes), (3, 5 + 3 + 7));

    let read_only = DiskCache::open_read_only(root.path()).unwrap();
    assert_eq!(read_only.foreign_files().unwrap(), found);
}

#[test]
fn clear_foreign_removes_only_foreign_files_and_keeps_entries() {
    let root = tempfile::tempdir().unwrap();
    let cache = DiskCache::open(root.path(), Config::new(1 << 20)).unwrap();
    let key = Key::new(&["keep"]);
    cache.put(&key, b"keep me").unwrap();
    fs::write(root.path().join("stray.txt"), b"12345").unwrap();

    let removed = cache.clear_foreign().unwrap();
    assert_eq!((removed.count, removed.bytes), (1, 5));
    assert!(!root.path().join("stray.txt").exists());
    assert_eq!(cache.foreign_files().unwrap().count, 0);
    assert_eq!(cache.get(&key).as_deref(), Some(&b"keep me"[..]));
}

#[test]
fn clear_reports_entries_and_bytes_removed() {
    let root = tempfile::tempdir().unwrap();
    let cache = DiskCache::open(root.path(), Config::new(1 << 20)).unwrap();
    cache.put(&Key::new(&["a"]), b"abc").unwrap();
    cache.put(&Key::new(&["b"]), b"defg").unwrap();

    let cleared = cache.clear();
    assert_eq!((cleared.entries, cleared.bytes), (2, 7));
    assert_eq!(cache.stats().entries, 0);

    cache.put(&Key::new(&["c"]), b"x").unwrap();
    assert_eq!(cache.clear_count(), 1);
}

#[test]
fn clear_foreign_on_a_read_only_handle_is_refused() {
    let root = tempfile::tempdir().unwrap();
    DiskCache::open(root.path(), Config::new(1 << 20)).unwrap();
    fs::write(root.path().join("stray.txt"), b"x").unwrap();
    let read_only = DiskCache::open_read_only(root.path()).unwrap();
    assert_eq!(
        read_only.clear_foreign().unwrap_err().kind(),
        ErrorKind::PermissionDenied
    );
    assert!(root.path().join("stray.txt").exists());
}

#[test]
fn the_stale_threshold_is_configurable_and_defaults_to_an_hour() {
    assert_eq!(Config::new(1).stale_tmp_after, Duration::from_secs(60 * 60));

    let root = tempfile::tempdir().unwrap();
    let tmp = root.path().join(".tmp");
    fs::create_dir_all(&tmp).unwrap();
    let ten_minutes = tmp.join("1-3.bin");
    fs::write(&ten_minutes, b"x").unwrap();
    age(&ten_minutes, Duration::from_secs(10 * 60));

    let _cache = DiskCache::open(
        root.path(),
        Config::new(1 << 20).stale_tmp_after(Duration::from_secs(5 * 60)),
    )
    .unwrap();
    assert!(
        !ten_minutes.exists(),
        "a 5 minute threshold sweeps a 10 minute old file"
    );
}
