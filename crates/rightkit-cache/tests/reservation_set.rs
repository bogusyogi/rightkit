use rightkit_cache::{CacheSet, Config, DiskCache, Key, PutOutcome};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// Stored files under `root`, excluding the `.tmp` staging directory.
fn stored_files(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().is_some_and(|n| n == ".tmp") {
                continue;
            }
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, &mut out);
    out.sort();
    out
}

#[test]
fn reservation_commit_moves_producer_output_into_the_cache() {
    let root = tempfile::tempdir().unwrap();
    let cache = DiskCache::open(root.path(), Config::new(64)).unwrap();
    let key = Key::grouped(&["thumbs", "asset-1"], &["1000", "320"]);

    let reservation = cache.reserve_path(&key, "jpg").unwrap();
    let tmp = reservation.path().to_path_buf();
    assert!(tmp.starts_with(root.path().join(".tmp")));
    assert_eq!(tmp.extension().unwrap(), "jpg");
    assert!(!cache.contains(&key));

    fs::write(&tmp, b"JPEGDATA").unwrap();
    assert_eq!(reservation.commit().unwrap(), PutOutcome::Stored);
    assert!(!tmp.exists(), "the temp file is moved, not copied");
    assert!(cache.contains(&key));

    let stored = cache.get_path(&key).expect("stored path");
    assert_eq!(stored.extension().unwrap(), "jpg");
    assert_eq!(fs::read(&stored).unwrap(), b"JPEGDATA");
    assert_eq!(cache.get(&key).as_deref(), Some(b"JPEGDATA".as_slice()));

    let s = cache.stats();
    assert_eq!((s.entries, s.bytes), (1, 8));
    assert!(s.oldest.is_some() && s.newest == s.oldest);

    // The extension and accounting survive a restart.
    drop(cache);
    let reopened = DiskCache::open(root.path(), Config::new(64)).unwrap();
    assert_eq!(reopened.get_path(&key).as_deref(), Some(stored.as_path()));
    assert_eq!(reopened.stats().bytes, 8);
}

#[test]
fn abort_and_drop_remove_the_temp_file_and_leave_no_entry() {
    let root = tempfile::tempdir().unwrap();
    let cache = DiskCache::open(root.path(), Config::new(64)).unwrap();
    let aborted = Key::new(&["aborted"]);
    let dropped = Key::new(&["dropped"]);

    let reservation = cache.reserve_path(&aborted, "png").unwrap();
    let tmp_aborted = reservation.path().to_path_buf();
    fs::write(&tmp_aborted, b"partial").unwrap();
    reservation.abort();
    assert!(!tmp_aborted.exists());

    let reservation = cache.reserve_path(&dropped, "png").unwrap();
    let tmp_dropped = reservation.path().to_path_buf();
    fs::write(&tmp_dropped, b"partial").unwrap();
    drop(reservation);
    assert!(!tmp_dropped.exists());

    assert!(!cache.contains(&aborted) && !cache.contains(&dropped));
    let s = cache.stats();
    assert_eq!((s.entries, s.bytes), (0, 0));
    assert!(s.oldest.is_none() && s.newest.is_none());
    assert!(stored_files(root.path()).is_empty());
}

#[test]
fn reservation_refuses_bad_extensions_and_empty_or_missing_output() {
    let root = tempfile::tempdir().unwrap();
    let cache = DiskCache::open(root.path(), Config::new(64)).unwrap();
    let key = Key::new(&["k"]);

    for bad in ["", "../x", "jpg.gz", "averyveryverylong", "j p"] {
        let err = cache
            .reserve_path(&key, bad)
            .err()
            .expect("bad extension refused");
        assert_eq!(err.kind(), ErrorKind::InvalidInput, "extension {bad:?}");
    }

    // Producer wrote nothing: commit fails and the temp path is gone.
    let reservation = cache.reserve_path(&key, "jpg").unwrap();
    let tmp = reservation.path().to_path_buf();
    assert!(reservation.commit().is_err());
    assert!(!tmp.exists());

    // Producer wrote an empty file (a failed encoder): refused, never cached.
    let reservation = cache.reserve_path(&key, "jpg").unwrap();
    fs::write(reservation.path(), b"").unwrap();
    assert_eq!(
        reservation.commit().unwrap_err().kind(),
        ErrorKind::InvalidData
    );
    assert!(!cache.contains(&key));
    assert_eq!(cache.stats().entries, 0);
}

#[test]
fn oversized_commit_is_refused_and_leaves_existing_entries_alone() {
    let root = tempfile::tempdir().unwrap();
    let cache = DiskCache::open(root.path(), Config::new(8).max_entry_bytes(8)).unwrap();
    let small = Key::new(&["small"]);
    let big = Key::new(&["big"]);
    cache.put(&small, b"1234").unwrap();

    let reservation = cache.reserve_path(&big, "jpg").unwrap();
    let tmp = reservation.path().to_path_buf();
    fs::write(&tmp, b"123456789").unwrap();
    assert_eq!(reservation.commit().unwrap(), PutOutcome::TooLarge);
    assert!(!tmp.exists());
    assert!(!cache.contains(&big));
    assert_eq!(cache.get(&small).as_deref(), Some(b"1234".as_slice()));
    assert_eq!(cache.stats().bytes, 4);
}

#[test]
fn committing_over_an_existing_slot_replaces_bytes_and_the_old_file() {
    let root = tempfile::tempdir().unwrap();
    let cache = DiskCache::open(root.path(), Config::new(64)).unwrap();
    let key = Key::new(&["slot"]);
    cache.put(&key, b"123456").unwrap();
    assert_eq!(cache.stats().bytes, 6);

    let reservation = cache.reserve_path(&key, "jpg").unwrap();
    fs::write(reservation.path(), b"ab").unwrap();
    assert_eq!(reservation.commit().unwrap(), PutOutcome::Stored);

    let s = cache.stats();
    assert_eq!((s.entries, s.bytes), (1, 2));
    assert_eq!(cache.get(&key).as_deref(), Some(b"ab".as_slice()));
    let files = stored_files(root.path());
    assert_eq!(files.len(), 1, "the byte-only file is gone: {files:?}");
    assert_eq!(files[0].extension().unwrap(), "jpg");
}

#[test]
fn stats_report_recency_bounds_and_clear_resets_them() {
    let root = tempfile::tempdir().unwrap();
    let cache = DiskCache::open(root.path(), Config::new(64)).unwrap();
    let a = Key::new(&["a"]);
    let b = Key::new(&["b"]);
    let c = Key::new(&["c"]);
    cache.put(&a, b"a").unwrap();
    cache.put(&b, b"b").unwrap();
    cache.put(&c, b"c").unwrap();

    let first = cache.stats();
    assert_eq!(first.entries, 3);
    let (oldest0, newest0) = (first.oldest.unwrap(), first.newest.unwrap());
    assert!(oldest0 < newest0);

    // Touching the oldest entry makes it the newest and moves the oldest bound forward.
    assert!(cache.get(&a).is_some());
    let touched = cache.stats();
    assert!(touched.oldest.unwrap() > oldest0);
    assert!(touched.newest.unwrap() > newest0);

    assert_eq!(cache.clear().entries, 3);
    let cleared = cache.stats();
    assert_eq!(
        (cleared.entries, cleared.oldest, cleared.newest),
        (0, None, None)
    );
}

#[test]
fn combined_budget_evicts_the_globally_least_recent_entry_across_roots() {
    let root_a = tempfile::tempdir().unwrap();
    let root_b = tempfile::tempdir().unwrap();
    let a = DiskCache::open(root_a.path(), Config::new(100)).unwrap();
    let b = DiskCache::open(root_b.path(), Config::new(100)).unwrap();
    let mut set = CacheSet::new().with_budget(12);
    set.add("a", a.clone()).unwrap();
    set.add("b", b.clone()).unwrap();

    let k1 = Key::new(&["k1"]);
    let k2 = Key::new(&["k2"]);
    let k3 = Key::new(&["k3"]);
    let k4 = Key::new(&["k4"]);
    a.put(&k1, b"aaaa").unwrap();
    b.put(&k2, b"bbbb").unwrap();
    a.put(&k3, b"cccc").unwrap();
    assert_eq!(set.enforce_budget(), 0, "12 bytes is within the budget");

    // Recency order is now k2, k3, k1. Adding 4 more bytes exceeds the combined budget.
    assert!(a.get(&k1).is_some());
    b.put(&k4, b"dddd").unwrap();
    assert_eq!(set.enforce_budget(), 1);
    assert!(
        !b.contains(&k2),
        "the globally oldest entry goes first, even in another root"
    );
    assert!(a.contains(&k1) && a.contains(&k3) && b.contains(&k4));

    let s = set.stats();
    assert_eq!((s.entries, s.bytes), (3, 12));
    assert_eq!(s.members.len(), 2);
    assert_eq!(s.members[0].0, "a");
    assert_eq!(s.members[0].1.entries, 2);
    assert_eq!(s.members[1].1.entries, 1);
    assert_eq!(set.enforce_budget(), 0);
}

#[test]
fn a_set_without_a_combined_budget_never_evicts_across_members() {
    let root_a = tempfile::tempdir().unwrap();
    let root_b = tempfile::tempdir().unwrap();
    let a = DiskCache::open(root_a.path(), Config::new(100)).unwrap();
    let b = DiskCache::open(root_b.path(), Config::new(100)).unwrap();
    let mut set = CacheSet::new();
    set.add("a", a.clone()).unwrap();
    set.add("b", b.clone()).unwrap();
    a.put(&Key::new(&["x"]), b"xxxxxxxx").unwrap();
    b.put(&Key::new(&["y"]), b"yyyyyyyy").unwrap();
    assert_eq!(set.enforce_budget(), 0);
    assert_eq!(set.stats().entries, 2);
}

#[test]
fn namespaces_share_one_root_with_separate_budgets() {
    let root = tempfile::tempdir().unwrap();
    let namespaces = [("thumbs", Config::new(8)), ("waveform", Config::new(8))];
    let set = CacheSet::open_namespaces(root.path(), &namespaces).unwrap();
    let thumbs = set.get("thumbs").unwrap().clone();
    let waves = set.get("waveform").unwrap().clone();
    let t1 = Key::new(&["t1"]);
    let t2 = Key::new(&["t2"]);
    let w1 = Key::new(&["w1"]);

    thumbs.put(&t1, b"11111111").unwrap();
    waves.put(&w1, b"wwww").unwrap();
    // Filling thumbnails past their budget evicts only thumbnails.
    thumbs.put(&t2, b"2222").unwrap();
    assert!(!thumbs.contains(&t1));
    assert!(thumbs.contains(&t2));
    assert!(waves.contains(&w1), "the waveform budget is independent");
    assert!(root.path().join("thumbs").is_dir());
    assert!(root.path().join("waveform").is_dir());

    drop((set, thumbs, waves));
    let reopened = CacheSet::open_namespaces(root.path(), &namespaces).unwrap();
    assert_eq!(
        reopened.get("thumbs").unwrap().get(&t2).as_deref(),
        Some(b"2222".as_slice())
    );
    assert_eq!(reopened.stats().entries, 2);

    let invalid = CacheSet::open_namespaces(root.path(), &[("../escape", Config::new(8))]);
    assert_eq!(invalid.err().unwrap().kind(), ErrorKind::InvalidInput);
}

#[test]
fn a_set_refuses_duplicate_names_and_shared_directories() {
    let root = tempfile::tempdir().unwrap();
    let first = DiskCache::open(root.path().join("first"), Config::new(8)).unwrap();
    let second_dir = DiskCache::open(root.path().join("second"), Config::new(8)).unwrap();
    let mut set = CacheSet::new();
    set.add("one", first.clone()).unwrap();

    assert_eq!(
        set.add("one", second_dir).unwrap_err().kind(),
        ErrorKind::AlreadyExists
    );
    assert_eq!(
        set.add("two", first).unwrap_err().kind(),
        ErrorKind::AlreadyExists,
        "the same directory cannot back two members"
    );
    let other = DiskCache::open(root.path().join("third"), Config::new(8)).unwrap();
    assert_eq!(
        set.add("Bad Name", other).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
    assert!(set.get("two").is_none());
}
