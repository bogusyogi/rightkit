//! An unreadable pin list means the pins are unknown, so opening must fail and evict nothing.

use rightkit_cache::{Config, DiskCache, Key};
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

/// Entry files (everything inside a two-character shard directory).
fn entry_files(root: &Path) -> usize {
    fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir() && p.file_name().is_some_and(|n| n.len() == 2))
        .map(|shard| fs::read_dir(shard).unwrap().count())
        .sum()
}

#[test]
fn absent_pin_list_opens_and_pins_nothing() {
    let root = tempfile::tempdir().unwrap();
    {
        let cache = DiskCache::open(root.path(), Config::new(100)).unwrap();
        cache.put(&Key::new(&["a"]), b"aaaa").unwrap();
    }
    assert!(!root.path().join(".pinned").exists());
    let cache = DiskCache::open(root.path(), Config::new(100)).unwrap();
    assert_eq!(cache.pinned_bytes(), 0);
    assert!(cache.contains(&Key::new(&["a"])));
}

/// A directory at `<root>/.pinned` makes the read fail with an error that is not `NotFound`
/// (`IsADirectory` on Unix, `PermissionDenied` on Windows) on every platform.
#[test]
fn unreadable_pin_list_fails_the_open_and_evicts_nothing() {
    let root = tempfile::tempdir().unwrap();
    let pinned = Key::new(&["pinned"]);
    let other = Key::new(&["other"]);
    {
        let cache = DiskCache::open(root.path(), Config::new(100)).unwrap();
        cache.put_pinned(&pinned, b"pinned-bytes").unwrap();
        cache.put(&other, b"other-bytes").unwrap();
    }
    let pin_list = root.path().join(".pinned");
    let saved = fs::read(&pin_list).unwrap();
    fs::remove_file(&pin_list).unwrap();
    fs::create_dir(&pin_list).unwrap();
    assert_eq!(entry_files(root.path()), 2);

    // A cap far below the stored bytes would evict if the pins were taken as empty.
    let error = DiskCache::open(root.path(), Config::new(1))
        .err()
        .expect("an unreadable pin list must fail the open");
    assert_ne!(error.kind(), ErrorKind::NotFound);
    assert!(DiskCache::open_read_only(root.path()).is_err());
    assert_eq!(entry_files(root.path()), 2, "nothing may be evicted");

    // With the list readable again the pin is honoured.
    fs::remove_dir(&pin_list).unwrap();
    fs::write(&pin_list, saved).unwrap();
    let cache = DiskCache::open(root.path(), Config::new(100)).unwrap();
    assert_eq!(cache.pinned_bytes(), b"pinned-bytes".len() as u64);
    assert_eq!(entry_files(root.path()), 2);
}
