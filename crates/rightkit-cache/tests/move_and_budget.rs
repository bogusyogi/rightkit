//! `DiskCache::move_to` (relocate keeping pins and index) and `DiskCache::set_budget` (runtime cap).

use rightkit_cache::{pinned_capacity_error, Config, DiskCache, Key, PutOutcome};
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

fn key(name: &str) -> Key {
    Key::new(&["entry", name])
}

/// Entry files (everything inside a two-character shard directory).
fn entry_files(root: &Path) -> usize {
    let Ok(dir) = fs::read_dir(root) else {
        return 0;
    };
    dir.map(|e| e.unwrap().path())
        .filter(|p| p.is_dir() && p.file_name().is_some_and(|n| n.len() == 2))
        .map(|shard| fs::read_dir(shard).unwrap().count())
        .sum()
}

#[test]
fn move_to_relocates_entries_pins_and_index() {
    let base = tempfile::tempdir().unwrap();
    let old = base.path().join("old");
    let new = base.path().join("new");
    let cache = DiskCache::open(&old, Config::new(1_000)).unwrap();
    cache.put(&key("a"), b"aaaa").unwrap();
    cache.put(&key("b"), b"bbbbbb").unwrap();
    cache.put_pinned(&key("p"), b"pinned").unwrap();
    let before = cache.stats();

    let report = cache.move_to(&new).unwrap();
    assert_eq!((report.moved, report.redundant), (3, 0));
    assert!(report.failed.is_empty(), "{:?}", report.failed);

    // The same handle keeps working, from the new root.
    assert_eq!(cache.location(), new);
    assert_eq!(cache.root(), old, "root() stays the opening root");
    assert_eq!(cache.get(&key("a")).unwrap(), b"aaaa");
    assert_eq!(cache.get(&key("b")).unwrap(), b"bbbbbb");
    assert_eq!(cache.get(&key("p")).unwrap(), b"pinned");
    let after = cache.stats();
    assert_eq!((after.entries, after.bytes), (before.entries, before.bytes));
    assert_eq!(cache.pinned_bytes(), 6);

    // Nothing of the cache is left behind in the old root; the root directory stays.
    assert!(old.is_dir());
    assert_eq!(entry_files(&old), 0);
    assert!(!old.join(".pinned").exists());
    assert_eq!(entry_files(&new), 3);
    assert!(new.join(".pinned").is_file());

    // New writes land in the new root.
    cache.put(&key("c"), b"cc").unwrap();
    assert_eq!(entry_files(&new), 4);
    assert_eq!(entry_files(&old), 0);

    // A fresh open of the new root sees the entries and the pin.
    drop(cache);
    let reopened = DiskCache::open(&new, Config::new(1_000)).unwrap();
    assert_eq!(reopened.stats().entries, 4);
    assert_eq!(reopened.pinned_bytes(), 6);
}

#[test]
fn moved_pins_still_survive_eviction() {
    let base = tempfile::tempdir().unwrap();
    let cache = DiskCache::open(base.path().join("a"), Config::new(20)).unwrap();
    cache.put_pinned(&key("p"), b"pppppppp").unwrap();
    cache.put(&key("x"), b"xxxxxxxx").unwrap();
    cache.move_to(base.path().join("b")).unwrap();
    // Over the cap: the unpinned entry goes, the pin stays.
    cache.put(&key("y"), b"yyyyyyyy").unwrap();
    assert!(cache.contains(&key("p")));
    assert!(!cache.contains(&key("x")));
}

#[test]
fn move_to_keeps_recency_order() {
    let base = tempfile::tempdir().unwrap();
    let cache = DiskCache::open(base.path().join("a"), Config::new(24)).unwrap();
    for n in ["one", "two", "three"] {
        cache.put(&key(n), b"12345678").unwrap();
    }
    cache.get(&key("one")); // "two" is now the least recently used
    cache.move_to(base.path().join("b")).unwrap();
    cache.put(&key("four"), b"12345678").unwrap();
    assert!(!cache.contains(&key("two")));
    assert!(cache.contains(&key("one")));
}

#[test]
fn move_to_counts_an_existing_destination_as_redundant() {
    let base = tempfile::tempdir().unwrap();
    let old = base.path().join("old");
    let new = base.path().join("new");
    let cache = DiskCache::open(&old, Config::new(1_000)).unwrap();
    cache.put(&key("a"), b"aaaa").unwrap();
    cache.put(&key("b"), b"bbbb").unwrap();
    // Pre-seed the destination with the same entry (same key, so the same file name).
    {
        let seed = DiskCache::open(&new, Config::new(1_000)).unwrap();
        seed.put(&key("a"), b"aaaa").unwrap();
    }
    let report = cache.move_to(&new).unwrap();
    assert_eq!((report.moved, report.redundant), (1, 1));
    assert!(report.failed.is_empty());
    assert_eq!(cache.get(&key("a")).unwrap(), b"aaaa");
    assert_eq!(cache.get(&key("b")).unwrap(), b"bbbb");
    assert_eq!(entry_files(&old), 0);
}

#[test]
fn move_to_reports_a_failed_entry_and_drops_it() {
    let base = tempfile::tempdir().unwrap();
    let old = base.path().join("old");
    let cache = DiskCache::open(&old, Config::new(1_000)).unwrap();
    cache.put(&key("a"), b"aaaa").unwrap();
    cache.put(&key("b"), b"bbbb").unwrap();
    // Remove one file behind the cache's back: it cannot be moved.
    let lost = fs::read_dir(&old)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir() && p.file_name().is_some_and(|n| n.len() == 2))
        .flat_map(|shard| fs::read_dir(shard).unwrap().map(|e| e.unwrap().path()))
        .next()
        .unwrap();
    fs::remove_file(&lost).unwrap();
    let report = cache.move_to(base.path().join("new")).unwrap();
    assert_eq!(report.moved, 1);
    assert_eq!(report.failed.len(), 1, "{:?}", report.failed);
    let stats = cache.stats();
    assert_eq!((stats.entries, stats.bytes), (1, 4));
}

#[test]
fn move_to_rejects_nested_roots_and_read_only_and_noops_on_same_root() {
    let base = tempfile::tempdir().unwrap();
    let old = base.path().join("old");
    let cache = DiskCache::open(&old, Config::new(1_000)).unwrap();
    cache.put(&key("a"), b"aaaa").unwrap();

    let inside = cache.move_to(old.join("inner")).unwrap_err();
    assert_eq!(inside.kind(), ErrorKind::InvalidInput);
    let containing = cache.move_to(base.path()).unwrap_err();
    assert_eq!(containing.kind(), ErrorKind::InvalidInput);
    assert_eq!(cache.location(), old, "a refused move changes nothing");
    assert_eq!(cache.get(&key("a")).unwrap(), b"aaaa");

    let same = cache.move_to(&old).unwrap();
    assert_eq!((same.moved, same.redundant), (0, 0));
    assert_eq!(cache.get(&key("a")).unwrap(), b"aaaa");

    let ro = DiskCache::open_read_only(&old).unwrap();
    assert_eq!(
        ro.move_to(base.path().join("new")).unwrap_err().kind(),
        ErrorKind::PermissionDenied
    );
}

#[test]
fn set_budget_evicts_down_least_recently_used_first() {
    let root = tempfile::tempdir().unwrap();
    let cache = DiskCache::open(root.path(), Config::new(100)).unwrap();
    for n in ["a", "b", "c", "d"] {
        cache.put(&key(n), &[7; 10]).unwrap();
    }
    cache.get(&key("a")); // "b" is the least recently used
    assert_eq!(cache.stats().bytes, 40);

    let cleared = cache.set_budget(25).unwrap();
    assert_eq!((cleared.entries, cleared.bytes), (2, 20));
    assert_eq!(cache.config().max_bytes, 25);
    assert_eq!(cache.status().max_bytes, 25);
    assert!(!cache.contains(&key("b")));
    assert!(!cache.contains(&key("c")));
    assert!(cache.contains(&key("a")));
    assert!(cache.contains(&key("d")));
    assert!(cache.stats().bytes <= 25);

    // The new cap governs later writes.
    cache.put(&key("e"), &[7; 10]).unwrap();
    assert!(cache.stats().bytes <= 25);
    assert_eq!(cache.put(&key("big"), &[7; 26]).unwrap(), PutOutcome::TooLarge);
}

#[test]
fn set_budget_growing_evicts_nothing_and_allows_more() {
    let root = tempfile::tempdir().unwrap();
    let cache = DiskCache::open(root.path(), Config::new(20)).unwrap();
    cache.put(&key("a"), &[1; 10]).unwrap();
    cache.put(&key("b"), &[1; 10]).unwrap();
    let cleared = cache.set_budget(100).unwrap();
    assert_eq!((cleared.entries, cleared.bytes), (0, 0));
    cache.put(&key("c"), &[1; 10]).unwrap();
    assert_eq!(cache.stats().entries, 3);
    assert_eq!(cache.stats().evictions, 0);
}

#[test]
fn set_budget_respects_pinned_bytes() {
    let root = tempfile::tempdir().unwrap();
    let cache = DiskCache::open(root.path(), Config::new(100)).unwrap();
    cache.put_pinned(&key("p"), &[1; 30]).unwrap();
    cache.put(&key("a"), &[1; 10]).unwrap();
    cache.put(&key("b"), &[1; 10]).unwrap();

    // Budget above the pinned bytes: the oldest unpinned entry goes, to fit around the pin.
    let cleared = cache.set_budget(45).unwrap();
    assert_eq!(cleared.entries, 1);
    assert!(cache.contains(&key("p")));
    assert!(!cache.contains(&key("a")));
    assert!(cache.contains(&key("b")));
    assert_eq!(cache.stats().bytes, 40);

    // Budget below the pinned bytes: everything unpinned goes, the pin stays, the cache sits over
    // its cap, and an unpinned write is refused with the typed error.
    let cleared = cache.set_budget(10).unwrap();
    assert_eq!(cleared.entries, 1);
    assert!(cache.contains(&key("p")));
    assert!(!cache.contains(&key("b")));
    assert_eq!(cache.pinned_bytes(), 30);
    assert_eq!(cache.stats().bytes, 30);
    let refused = cache.put(&key("c"), &[1; 5]).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::StorageFull);
    assert_eq!(pinned_capacity_error(&refused).unwrap().max_bytes, 10);

    // Raising the cap again makes room.
    cache.set_budget(100).unwrap();
    assert_eq!(cache.put(&key("c"), &[1; 5]).unwrap(), PutOutcome::Stored);
}

#[test]
fn set_budget_fails_on_a_read_only_handle() {
    let root = tempfile::tempdir().unwrap();
    DiskCache::open(root.path(), Config::new(100))
        .unwrap()
        .put(&key("a"), b"aaaa")
        .unwrap();
    let ro = DiskCache::open_read_only(root.path()).unwrap();
    assert_eq!(
        ro.set_budget(1).unwrap_err().kind(),
        ErrorKind::PermissionDenied
    );
    assert!(ro.contains(&key("a")));
}
