// rightkit-cache 0.2.3 consumed by exact registry pin (no features: the crate has none, and the
// apps use it that way), outside the workspace.
//
// Proves, through the public API only, in per-scenario directories under one temp directory:
// - Key: length-prefixed SHA-256 (two literal digests computed outside Rust), no collision between
//   ("ab","c") and ("a","bc"), grouped keys and group_id, Key::url;
// - DiskCache byte entries: put and get, replace, remove, contains, hits and misses in Stats, the
//   hard cap with least-recently-used eviction (a get refreshes recency), the per-entry limit
//   (PutOutcome::TooLarge leaves existing entries alone), clear_group and clear (Cleared),
//   get_or_fetch (fetch once, a fetch error caches nothing), set_budget shrinking, status, config;
// - recency and the cap surviving a restart (reopen with a smaller cap evicts the least recently
//   used entry); streaming writes (put_writer, put_reader, an uncommitted writer leaves no entry,
//   an over-limit writer reports TooLarge);
// - pinning: put_pinned, pin, unpin, pinned_bytes, a write that leaves no room fails with
//   PinnedCapacityError (kind StorageFull, read by pinned_capacity_error) and writes nothing, a pin
//   survives a restart and size pressure never evicts it;
// - file producers: reserve_path then commit (get_path, get), an empty producer file is an error,
//   abort and drop remove the temp file, an invalid extension is refused, an over-limit file is
//   TooLarge and removed;
// - open_read_only (reads and stats work; every mutation fails or does nothing; a missing root is
//   NotFound), open_strict refusing a root with a foreign file (ForeignContentError), foreign_files
//   and clear_foreign, move_to relocating entries and pins (and refusing a nested root), owner_only;
// - CacheSet: open_namespaces, valid_namespace, add refusing a repeated name or directory,
//   aggregated stats, and enforce_budget evicting the globally least recently used entry;
// - prefix: fnv1a_64 reference vectors, StableCacheKey and ConductorStableKey strings (literals
//   computed outside Rust), CacheTracker Fresh / Hit / Break and the break attribution,
//   system_prompt_only_cache_key, parse_iso_day, and the unchanged legacy fnv1a basis.
// Does not prove: the 100,000-entry runtime budget, behaviour under concurrent writers, a cache on
// a removable or network volume, a full disk, or crash recovery of a half-written entry. Public API
// only.
// State: a per-process directory under the OS temp directory, removed at the end. No network, no
// process spawn, no real user data.
#![allow(deprecated)]
use rightkit_cache::prefix::{
    fnv1a, fnv1a_64, parse_iso_day, system_prompt_only_cache_key, CONDUCTOR_CACHE_KEY_PREFIX,
};
use rightkit_cache::{
    foreign_content_error, group_id, pinned_capacity_error, valid_namespace, CacheSet, CacheStatus,
    CacheTracker, Cleared, ConductorStableKey, Config, DiskCache, Key, PutOutcome, StableCacheKey,
};
use std::fs;
use std::io::{self, Cursor, Write};
use std::path::PathBuf;
use std::thread::sleep;
use std::time::Duration;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("rk-cache-consumer-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create consumer temp dir");
        TempDir(dir)
    }

    fn sub(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn main() {
    let temp = TempDir::new();
    keys();
    byte_entries(&temp);
    restart(&temp);
    fetch_and_budget(&temp);
    streaming(&temp);
    pinning(&temp);
    file_producers(&temp);
    read_only_strict_and_move(&temp);
    namespaces(&temp);
    prompt_prefix();
    println!("OK rightkit-cache 0.2.3 (registry consumer)");
}

/// Recency on disk is the file modification time: leave a visible gap between operations whose
/// order a reopen must recover.
fn pause() {
    sleep(Duration::from_millis(120));
}

// ---- keys ----------------------------------------------------------------------------------

fn keys() {
    assert_eq!(
        Key::new(&["a", "b"]).hash(),
        "cf6ab613e3942391f88ed698557e1680f160bd10e88c6b668c50360c10930e2b"
    );
    assert_eq!(
        Key::new(&["ab", "c"]).hash(),
        "43ee655579de01ca739b3f95c1c2d3f46d353b2c0df818064ea594506cdb2617"
    );
    assert_eq!(
        Key::new(&["a", "bc"]).hash(),
        "9a8acca1b6c6c0befd3fbc756aed625da998c998f7252e738c4ef061906b9b21"
    );
    assert_ne!(
        Key::new(&["ab", "c"]),
        Key::new(&["a", "bc"]),
        "length prefixes keep the parts unambiguous"
    );
    assert_eq!(Key::new(&["a", "b"]), Key::new(&["a", "b"]));
    assert_eq!(
        Key::url("https://example.test/a.png").hash(),
        "71f4a9d9124e45b97ae85dcb4482652786781641c60285fa3178989670a96820"
    );
    assert_eq!(
        Key::url("https://example.test/a.png"),
        Key::new(&["https://example.test/a.png"])
    );

    let grouped = Key::grouped(&["account-a"], &["old"]);
    assert_eq!(
        grouped.hash(),
        "b11a8462b564d11fbe76f1043a8ba65ff4608158e7dc81097581d540a74b2531"
    );
    assert_eq!(group_id(&["account-a"]), "7aaf3fc5a89c8958");
    assert_eq!(group_id(&["account-a"]).len(), 16);
    assert_ne!(
        grouped,
        Key::new(&["account-a", "old"]),
        "the group is part of the slot"
    );
}

// ---- byte entries --------------------------------------------------------------------------

fn byte_entries(temp: &TempDir) {
    let config = Config::new(8).max_entry_bytes(8);
    assert_eq!(
        (config.max_bytes, config.max_entry_bytes, config.owner_only),
        (8, Some(8), false)
    );
    assert_eq!(
        Config::new(8)
            .stale_tmp_after(Duration::from_secs(5))
            .stale_tmp_after,
        Duration::from_secs(5)
    );
    assert_eq!(
        Config::new(8).stale_tmp_after,
        Duration::from_secs(3600),
        "leftover temp files are swept after an hour by default"
    );

    let cache = DiskCache::open(temp.sub("bytes"), config).expect("open");
    assert!(cache.is_writable());
    assert_eq!(cache.root(), temp.sub("bytes"));
    assert_eq!(cache.location(), temp.sub("bytes"));
    assert_eq!(cache.config().max_bytes, 8);
    let old = Key::grouped(&["account-a"], &["old"]);
    let touched = Key::grouped(&["account-a"], &["touched"]);
    let newest = Key::grouped(&["account-a"], &["newest"]);
    assert_eq!(cache.put(&old, b"1111").unwrap(), PutOutcome::Stored);
    assert_eq!(cache.put(&touched, b"2222").unwrap(), PutOutcome::Stored);
    assert_eq!(
        cache.get(&old).as_deref(),
        Some(b"1111".as_slice()),
        "a get refreshes recency"
    );
    assert_eq!(cache.put(&newest, b"3333").unwrap(), PutOutcome::Stored);
    assert!(
        !cache.contains(&touched),
        "the least recently used entry is evicted to hold the cap"
    );
    assert!(cache.contains(&old) && cache.contains(&newest));
    let stats = cache.stats();
    assert_eq!(
        (
            stats.entries,
            stats.bytes,
            stats.evictions,
            stats.hits,
            stats.misses
        ),
        (2, 8, 1, 1, 0)
    );
    assert!(stats.oldest.is_some() && stats.newest.is_some() && stats.oldest <= stats.newest);
    assert!(cache.get(&touched).is_none());
    assert_eq!(cache.stats().misses, 1);

    assert_eq!(
        cache.put(&old, b"9999").unwrap(),
        PutOutcome::Stored,
        "replacing keeps one entry"
    );
    assert_eq!(cache.get(&old).as_deref(), Some(b"9999".as_slice()));
    assert_eq!((cache.stats().entries, cache.stats().bytes), (2, 8));
    assert_eq!(
        cache.put(&old, b"123456789").unwrap(),
        PutOutcome::TooLarge,
        "over the per-entry limit"
    );
    assert_eq!(
        cache.get(&old).as_deref(),
        Some(b"9999".as_slice()),
        "a refused write leaves the old value"
    );
    assert_eq!((cache.stats().entries, cache.stats().bytes), (2, 8));

    assert_eq!(cache.clear_group(&["account-a"]), 2);
    assert_eq!(cache.clear_group(&["account-a"]), 0);
    assert!(cache.get(&old).is_none() && cache.get(&newest).is_none());

    let a = Key::new(&["a"]);
    let b = Key::new(&["b"]);
    cache.put(&a, b"aa").unwrap();
    cache.put(&b, b"bbb").unwrap();
    assert!(cache.remove(&a));
    assert!(!cache.remove(&a));
    assert_eq!(
        cache.clear(),
        Cleared {
            entries: 1,
            bytes: 3
        }
    );
    assert_eq!(cache.clear(), Cleared::default());
    assert_eq!(cache.stats().entries, 0);
    cache.put(&a, b"x").unwrap();
    assert_eq!(cache.clear_count(), 1);

    // owner_only entries read back the same; on Unix they are 0600 files.
    let private = DiskCache::open(temp.sub("private"), Config::new(64).owner_only())
        .expect("open owner-only");
    assert!(private.config().owner_only);
    assert_eq!(private.put(&a, b"secret").unwrap(), PutOutcome::Stored);
    assert_eq!(private.get(&a).as_deref(), Some(b"secret".as_slice()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut seen = 0;
        for shard in fs::read_dir(temp.sub("private"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().len() == 2)
        {
            for entry in fs::read_dir(shard.path()).unwrap().flatten() {
                assert_eq!(
                    entry.metadata().unwrap().permissions().mode() & 0o777,
                    0o600
                );
                seen += 1;
            }
        }
        assert_eq!(seen, 1);
    }
}

// ---- restart -------------------------------------------------------------------------------

fn restart(temp: &TempDir) {
    let root = temp.sub("restart");
    let (a, b, c) = (Key::new(&["a"]), Key::new(&["b"]), Key::new(&["c"]));
    {
        let cache = DiskCache::open(&root, Config::new(12)).unwrap();
        cache.put(&a, b"1111").unwrap();
        pause();
        cache.put(&b, b"2222").unwrap();
        pause();
        cache.put(&c, b"3333").unwrap();
        pause();
        assert_eq!(cache.get(&a).as_deref(), Some(b"1111".as_slice()));
        assert_eq!(cache.stats().entries, 3);
    }
    let reopened = DiskCache::open(&root, Config::new(8)).unwrap();
    assert!(
        !reopened.contains(&b),
        "b was the least recently used before the restart"
    );
    assert!(
        reopened.contains(&a) && reopened.contains(&c),
        "a refreshed recency, c was written last"
    );
    assert_eq!((reopened.stats().entries, reopened.stats().bytes), (2, 8));

    // Shrinking the cap again on the next reopen keeps only what fits, most recent first.
    drop(reopened);
    let shrunk = DiskCache::open(&root, Config::new(5).max_entry_bytes(3)).unwrap();
    assert_eq!((shrunk.stats().entries, shrunk.stats().bytes), (1, 4));
    assert!(shrunk.contains(&a) && !shrunk.contains(&c));
    assert_eq!(shrunk.put(&b, b"too-large").unwrap(), PutOutcome::TooLarge);
    assert!(shrunk.contains(&a), "a refused write destroys nothing");
}

// ---- fetch, shrinking, status --------------------------------------------------------------

fn fetch_and_budget(temp: &TempDir) {
    let cache = DiskCache::open(temp.sub("fetch"), Config::new(100)).unwrap();
    let key = Key::url("https://example.test/logo.png");
    let mut fetches = 0;
    let first: Result<Vec<u8>, String> = cache.get_or_fetch(&key, || {
        fetches += 1;
        Ok(b"png-bytes".to_vec())
    });
    assert_eq!(first.as_deref(), Ok(b"png-bytes".as_slice()));
    let second: Result<Vec<u8>, String> = cache.get_or_fetch(&key, || {
        fetches += 1;
        Ok(b"different".to_vec())
    });
    assert_eq!(
        second.as_deref(),
        Ok(b"png-bytes".as_slice()),
        "a hit never calls fetch"
    );
    assert_eq!(fetches, 1);
    let other = Key::url("https://example.test/missing.png");
    let failed: Result<Vec<u8>, String> = cache.get_or_fetch(&other, || Err("offline".to_owned()));
    assert_eq!(failed, Err("offline".to_owned()));
    assert!(!cache.contains(&other), "a fetch error caches nothing");

    let status = cache.status();
    assert_eq!(status.max_bytes, 100);
    assert_eq!(status.stats.entries, 1);

    let many = DiskCache::open(temp.sub("shrink"), Config::new(40)).unwrap();
    for name in ["a", "b", "c", "d"] {
        many.put(&Key::new(&[name]), b"0123456789").unwrap();
    }
    assert_eq!(many.stats().bytes, 40);
    let cleared = many.set_budget(20).unwrap();
    assert_eq!(
        cleared,
        Cleared {
            entries: 2,
            bytes: 20
        }
    );
    assert_eq!(many.config().max_bytes, 20, "the new cap applies at once");
    assert_eq!((many.stats().entries, many.stats().bytes), (2, 20));
    assert_eq!(
        many.set_budget(100).unwrap(),
        Cleared::default(),
        "growing the cap evicts nothing"
    );
}

// ---- streaming -----------------------------------------------------------------------------

fn streaming(temp: &TempDir) {
    let cache = DiskCache::open(temp.sub("stream"), Config::new(64).max_entry_bytes(16)).unwrap();
    let key = Key::new(&["stream"]);
    let mut writer = cache.put_writer(&key).unwrap();
    writer.write_all(b"hello ").unwrap();
    writer.write_all(b"world").unwrap();
    writer.flush().unwrap();
    assert_eq!(writer.bytes_written(), 11);
    assert!(!cache.contains(&key), "nothing is published before commit");
    assert_eq!(writer.commit().unwrap(), PutOutcome::Stored);
    assert_eq!(cache.get(&key).as_deref(), Some(b"hello world".as_slice()));

    let dropped = Key::new(&["dropped"]);
    let mut writer = cache.put_writer(&dropped).unwrap();
    writer.write_all(b"abandoned").unwrap();
    drop(writer);
    assert!(
        !cache.contains(&dropped),
        "a writer dropped without commit leaves no entry"
    );

    let from_reader = Key::new(&["reader"]);
    assert_eq!(
        cache
            .put_reader(&from_reader, &mut Cursor::new(b"copied in chunks".to_vec()))
            .unwrap(),
        PutOutcome::Stored
    );
    assert_eq!(
        cache.get(&from_reader).as_deref(),
        Some(b"copied in chunks".as_slice())
    );
    let too_big = Key::new(&["big"]);
    assert_eq!(
        cache
            .put_reader(&too_big, &mut Cursor::new(vec![b'x'; 17]))
            .unwrap(),
        PutOutcome::TooLarge
    );
    assert!(!cache.contains(&too_big));
    let mut writer = cache.put_writer(&too_big).unwrap();
    assert!(
        writer.write_all(&[b'x'; 17]).is_err(),
        "writing past the entry limit fails"
    );
    assert_eq!(writer.commit().unwrap(), PutOutcome::TooLarge);
    assert_eq!(
        cache.get(&key).as_deref(),
        Some(b"hello world".as_slice()),
        "existing entries are untouched"
    );

    let pinned = Key::new(&["pinned-stream"]);
    let mut writer = cache.put_writer(&pinned).unwrap().pinned();
    writer.write_all(b"keep").unwrap();
    writer.commit().unwrap();
    assert_eq!(cache.pinned_bytes(), 4);
}

// ---- pinning -------------------------------------------------------------------------------

fn pinning(temp: &TempDir) {
    let root = temp.sub("pins");
    let (a, b, c, d) = (
        Key::new(&["a"]),
        Key::new(&["b"]),
        Key::new(&["c"]),
        Key::new(&["d"]),
    );
    let cache = DiskCache::open(&root, Config::new(10)).unwrap();
    assert_eq!(cache.put_pinned(&a, b"aaaaaa").unwrap(), PutOutcome::Stored);
    assert_eq!(cache.pinned_bytes(), 6);

    let error = cache.put(&b, b"bbbbbb").unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::StorageFull);
    let detail = pinned_capacity_error(&error).expect("typed pinned-capacity error");
    assert_eq!(
        (detail.pinned_bytes, detail.entry_bytes, detail.max_bytes),
        (6, 6, 10)
    );
    assert!(detail.to_string().contains("10-byte cap"));
    assert!(
        !cache.contains(&b) && cache.contains(&a),
        "a refused write writes and evicts nothing"
    );
    assert!(pinned_capacity_error(&io::Error::other("unrelated")).is_none());

    assert_eq!(cache.put(&b, b"bbbb").unwrap(), PutOutcome::Stored);
    assert_eq!(
        cache.put(&c, b"cccc").unwrap(),
        PutOutcome::Stored,
        "size pressure evicts the unpinned entry"
    );
    assert!(!cache.contains(&b) && cache.contains(&a) && cache.contains(&c));
    assert_eq!(cache.pin(&c).unwrap(), true);
    assert_eq!(cache.pinned_bytes(), 10);
    assert_eq!(
        cache.pin(&d).unwrap(),
        false,
        "an absent key cannot be pinned"
    );
    assert_eq!(cache.unpin(&d).unwrap(), false);
    drop(cache);

    let reopened = DiskCache::open(&root, Config::new(10)).unwrap();
    assert_eq!(reopened.pinned_bytes(), 10, "pins survive a restart");
    assert!(reopened.get(&a).is_some() && reopened.get(&c).is_some());
    assert_eq!(reopened.unpin(&c).unwrap(), true);
    assert_eq!(reopened.pinned_bytes(), 6);
    assert_eq!(reopened.put(&d, b"dddd").unwrap(), PutOutcome::Stored);
    assert!(!reopened.contains(&c) && reopened.contains(&a) && reopened.contains(&d));
}

// ---- file producers ------------------------------------------------------------------------

fn file_producers(temp: &TempDir) {
    let cache = DiskCache::open(temp.sub("files"), Config::new(64).max_entry_bytes(32)).unwrap();
    let key = Key::grouped(&["video-1"], &["frame-0001"]);

    let reservation = cache.reserve_path(&key, "jpg").unwrap();
    let tmp = reservation.path().to_path_buf();
    assert_eq!(
        tmp.extension().and_then(|e| e.to_str()),
        Some("jpg"),
        "the producer sees the extension it needs"
    );
    fs::write(&tmp, b"jpeg-bytes").unwrap();
    assert_eq!(reservation.commit().unwrap(), PutOutcome::Stored);
    assert!(!tmp.exists(), "the file moved into the cache");
    let stored = cache.get_path(&key).expect("stored path");
    assert_eq!(stored.extension().and_then(|e| e.to_str()), Some("jpg"));
    assert_eq!(fs::read(&stored).unwrap(), b"jpeg-bytes");
    assert_eq!(cache.get(&key).as_deref(), Some(b"jpeg-bytes".as_slice()));
    assert_eq!((cache.stats().entries, cache.stats().bytes), (1, 10));
    assert!(cache.get_path(&Key::new(&["nothing"])).is_none());

    let reservation = cache.reserve_path(&Key::new(&["empty"]), "png").unwrap();
    let tmp = reservation.path().to_path_buf();
    fs::write(&tmp, b"").unwrap();
    assert_eq!(
        reservation.commit().unwrap_err().kind(),
        io::ErrorKind::InvalidData,
        "a failed encoder leaves no empty entry"
    );
    assert!(!tmp.exists());
    let reservation = cache.reserve_path(&Key::new(&["missing"]), "png").unwrap();
    assert!(
        reservation.commit().is_err(),
        "no producer output is an error"
    );

    let reservation = cache.reserve_path(&Key::new(&["aborted"]), "png").unwrap();
    let tmp = reservation.path().to_path_buf();
    fs::write(&tmp, b"partial").unwrap();
    reservation.abort();
    assert!(!tmp.exists());
    let reservation = cache.reserve_path(&Key::new(&["dropped"]), "png").unwrap();
    let tmp = reservation.path().to_path_buf();
    fs::write(&tmp, b"partial").unwrap();
    drop(reservation);
    assert!(
        !tmp.exists(),
        "dropping a reservation removes its temp file"
    );

    let reservation = cache.reserve_path(&Key::new(&["big"]), "bin").unwrap();
    let tmp = reservation.path().to_path_buf();
    fs::write(&tmp, vec![b'x'; 33]).unwrap();
    assert_eq!(reservation.commit().unwrap(), PutOutcome::TooLarge);
    assert!(!tmp.exists());
    assert_eq!(cache.stats().entries, 1, "existing entries are untouched");

    for bad in ["", "toolongext", "j p", "j/g", "jp.g"] {
        let error = cache
            .reserve_path(&key, bad)
            .err()
            .unwrap_or_else(|| panic!("`{bad}` must be refused"));
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{bad}");
    }
}

// ---- read-only, strict, foreign files, move ------------------------------------------------

fn read_only_strict_and_move(temp: &TempDir) {
    let root = temp.sub("inspect");
    let key = Key::new(&["inspect"]);
    {
        let cache = DiskCache::open(&root, Config::new(64)).unwrap();
        cache.put(&key, b"value").unwrap();
        cache.put_pinned(&Key::new(&["kept"]), b"kept").unwrap();
    }
    let view = DiskCache::open_read_only(&root).unwrap();
    assert!(!view.is_writable());
    assert_eq!(view.get(&key).as_deref(), Some(b"value".as_slice()));
    assert_eq!((view.stats().entries, view.stats().bytes), (2, 9));
    assert_eq!(view.pinned_bytes(), 4);
    assert_eq!(
        view.config().max_bytes,
        u64::MAX,
        "a read-only handle never evicts"
    );
    assert_eq!(
        view.put(&key, b"x").unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        view.put_pinned(&key, b"x").unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert!(view.put_writer(&key).is_err() && view.reserve_path(&key, "jpg").is_err());
    assert!(!view.remove(&key));
    assert_eq!(view.clear_group(&["g"]), 0);
    assert_eq!(view.clear(), Cleared::default());
    assert!(
        view.set_budget(1).is_err()
            && view.pin(&key).is_err()
            && view.move_to(temp.sub("nowhere")).is_err()
    );
    assert_eq!(
        view.get(&key).as_deref(),
        Some(b"value".as_slice()),
        "nothing was removed"
    );
    assert_eq!(
        DiskCache::open_read_only(temp.sub("does-not-exist"))
            .err()
            .map(|e| e.kind()),
        Some(io::ErrorKind::NotFound)
    );
    assert!(
        !temp.sub("does-not-exist").exists(),
        "a read-only open creates nothing"
    );

    // Strict open refuses a root that holds files the cache did not write, and touches nothing.
    let shared = temp.sub("shared");
    fs::create_dir_all(&shared).unwrap();
    fs::write(shared.join("notes.txt"), b"not a cache file").unwrap();
    let error = DiskCache::open_strict(&shared, Config::new(64))
        .err()
        .expect("strict open refuses");
    let foreign = foreign_content_error(&error).expect("typed foreign-content error");
    assert_eq!(foreign.count, 1);
    assert_eq!(foreign.first, shared.join("notes.txt"));
    assert!(foreign.to_string().contains("did not write"));
    assert!(shared.join("notes.txt").exists() && !shared.join(".tmp").exists());
    assert!(foreign_content_error(&io::Error::other("unrelated")).is_none());
    let tolerant = DiskCache::open(&shared, Config::new(64)).unwrap();
    assert!(
        shared.join("notes.txt").exists(),
        "the default open never deletes a file it did not write"
    );
    let found = tolerant.foreign_files().unwrap();
    assert_eq!((found.count, found.bytes), (1, 16));
    let removed = tolerant.clear_foreign().unwrap();
    assert_eq!((removed.count, removed.bytes), (1, 16));
    assert!(!shared.join("notes.txt").exists());
    assert_eq!(tolerant.foreign_files().unwrap().count, 0);
    drop(tolerant);
    assert!(
        DiskCache::open_strict(&shared, Config::new(64)).is_ok(),
        "a clean root opens strictly"
    );
    assert!(
        DiskCache::open_strict(temp.sub("fresh-strict"), Config::new(64)).is_ok(),
        "a root that does not exist yet is created"
    );

    // Move: entries, pins and recency go with the cache; the handle keeps working.
    let cache = DiskCache::open(&root, Config::new(64)).unwrap();
    let new_root = temp.sub("moved");
    let report = cache.move_to(&new_root).unwrap();
    assert_eq!((report.moved, report.redundant), (2, 0));
    assert!(report.failed.is_empty());
    assert_eq!(cache.location(), new_root);
    assert_eq!(cache.root(), root, "root() is still the opening root");
    assert_eq!(cache.get(&key).as_deref(), Some(b"value".as_slice()));
    assert_eq!(cache.pinned_bytes(), 4);
    assert_eq!(
        cache.put(&Key::new(&["after"]), b"after").unwrap(),
        PutOutcome::Stored
    );
    assert_eq!(
        cache.move_to(&new_root).unwrap(),
        Default::default(),
        "moving to the current root is a no-op"
    );
    assert_eq!(
        cache.move_to(new_root.join("inside")).unwrap_err().kind(),
        io::ErrorKind::InvalidInput,
        "a nested root is refused"
    );
    drop(cache);
    let reopened = DiskCache::open(&new_root, Config::new(64)).unwrap();
    assert_eq!((reopened.stats().entries, reopened.pinned_bytes()), (3, 4));
}

// ---- cache sets ----------------------------------------------------------------------------

fn namespaces(temp: &TempDir) {
    for good in ["thumbs", "a", "frames_2", "wave-form", &"x".repeat(32)] {
        assert!(valid_namespace(good), "{good}");
    }
    for bad in [
        "",
        "Upper",
        "has space",
        "dot.dot",
        "slash/slash",
        &"x".repeat(33),
    ] {
        assert!(!valid_namespace(bad), "{bad}");
    }

    let root = temp.sub("set");
    let set = CacheSet::open_namespaces(
        &root,
        &[("thumbs", Config::new(256)), ("waveform", Config::new(64))],
    )
    .unwrap();
    assert!(root.join("thumbs").is_dir() && root.join("waveform").is_dir());
    let thumbs = set.get("thumbs").expect("thumbs member");
    let waveform = set.get("waveform").expect("waveform member");
    assert!(set.get("frames").is_none());
    let (t1, t2, w1) = (Key::new(&["t1"]), Key::new(&["t2"]), Key::new(&["w1"]));
    thumbs.put(&t1, b"thumbnail-1").unwrap();
    thumbs.put(&t2, b"thumbnail-2").unwrap();
    waveform.put(&w1, b"wave").unwrap();
    let totals = set.stats();
    assert_eq!((totals.entries, totals.bytes), (3, 26));
    assert_eq!(
        totals
            .members
            .iter()
            .map(|(name, s)| (name.as_str(), s.entries))
            .collect::<Vec<_>>(),
        [("thumbs", 2), ("waveform", 1)]
    );
    assert!(totals.oldest.is_some() && totals.newest.is_some());
    assert_eq!(
        set.enforce_budget(),
        0,
        "no combined budget set, nothing is evicted"
    );
    assert!(CacheSet::new().stats().oldest.is_none());

    let before = fs::read_dir(&root).unwrap().count();
    let refused = CacheSet::open_namespaces(
        &root,
        &[("Not Valid", Config::new(8)), ("fine", Config::new(8))],
    );
    assert_eq!(
        refused.err().map(|e| e.kind()),
        Some(io::ErrorKind::InvalidInput)
    );
    assert_eq!(
        fs::read_dir(&root).unwrap().count(),
        before,
        "an invalid name creates nothing"
    );

    let mut sets = CacheSet::new();
    sets.add("one", thumbs.clone()).unwrap();
    assert_eq!(
        sets.add("one", waveform.clone()).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(
        sets.add("two", thumbs.clone()).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists,
        "one directory, one member"
    );
    assert_eq!(
        sets.add("BAD", waveform.clone()).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );

    // A combined budget across two roots evicts the globally least recently used entry.
    let a = DiskCache::open(temp.sub("budget-a"), Config::new(100)).unwrap();
    let b = DiskCache::open(temp.sub("budget-b"), Config::new(100)).unwrap();
    let mut budgeted = CacheSet::new().with_budget(12);
    budgeted.add("a", a.clone()).unwrap();
    budgeted.add("b", b.clone()).unwrap();
    let (k1, k2, k3, k4) = (
        Key::new(&["k1"]),
        Key::new(&["k2"]),
        Key::new(&["k3"]),
        Key::new(&["k4"]),
    );
    a.put(&k1, b"aaaa").unwrap();
    b.put(&k2, b"bbbb").unwrap();
    a.put(&k3, b"cccc").unwrap();
    assert_eq!(
        budgeted.enforce_budget(),
        0,
        "12 bytes is within the budget"
    );
    assert!(a.get(&k1).is_some());
    b.put(&k4, b"dddd").unwrap();
    assert_eq!(budgeted.enforce_budget(), 1);
    assert!(
        !b.contains(&k2),
        "the globally oldest entry goes first, even in another root"
    );
    assert!(a.contains(&k1) && a.contains(&k3) && b.contains(&k4));
    assert_eq!((budgeted.stats().entries, budgeted.stats().bytes), (3, 12));
    assert_eq!(budgeted.enforce_budget(), 0);
}

// ---- stable-prefix prompt-cache keys -------------------------------------------------------

fn prompt_prefix() {
    assert_eq!(fnv1a_64(""), 0xcbf29ce484222325);
    assert_eq!(fnv1a_64("a"), 0xaf63dc4c8601ec8c);
    assert_eq!(fnv1a_64("foobar"), 0x85944171f73967e8);
    assert_eq!(
        fnv1a(""),
        1469598103934665603,
        "the legacy basis stays unchanged for keys already persisted"
    );
    assert_eq!(fnv1a("foobar"), 9870438755804841970);
    assert_ne!(fnv1a("foobar"), fnv1a_64("foobar"));
    assert_eq!(parse_iso_day("2026-10-24T09:30:00Z"), "2026-10-24");
    assert_eq!(parse_iso_day("2026-10-24"), "2026-10-24");

    let stable = StableCacheKey::new("identity-1", "2026-10-24");
    assert_eq!(stable.compute(), "cache:prompt:stable:29bf9a3c7bf26e0e");
    assert_eq!(
        StableCacheKey::default().compute(),
        StableCacheKey::new("", "").compute()
    );
    assert_ne!(
        stable.compute(),
        StableCacheKey::new("identity-1", "2026-10-25").compute(),
        "the UTC day is part of the stable tier"
    );
    let conductor = ConductorStableKey::new("identity-1", "2026-10-24");
    assert_eq!(
        conductor.compute(),
        "cache:prompt:conductor:29bf9a3c7bf26e0e"
    );
    assert!(conductor.compute().starts_with(CONDUCTOR_CACHE_KEY_PREFIX));
    assert!(
        !stable.compute().starts_with(CONDUCTOR_CACHE_KEY_PREFIX),
        "a conductor slot never collides with a session slot"
    );
    assert_eq!(
        system_prompt_only_cache_key("You are a careful assistant."),
        "cache:prompt:08065aeb7d6ca66f"
    );

    let mut tracker = CacheTracker::new();
    assert_eq!(tracker.observe_stable(&stable), CacheStatus::Fresh);
    assert_eq!(tracker.observe_stable(&stable), CacheStatus::Hit);
    assert_eq!(
        tracker.observe_stable(&StableCacheKey::new("identity-1", "2026-10-25")),
        CacheStatus::Break
    );
    tracker.reset();
    assert_eq!(
        tracker.observe_prefix("system", "prefix"),
        CacheStatus::Fresh
    );
    assert_eq!(tracker.observe_prefix("system", "prefix"), CacheStatus::Hit);
    assert_eq!(
        tracker.observe_prefix("system", "prefix 2"),
        CacheStatus::Break
    );

    let mut tracker = CacheTracker::new();
    assert_eq!(
        tracker.observe_stable_with_tools(&stable, Some("tools-v1")),
        (CacheStatus::Fresh, None)
    );
    assert_eq!(
        tracker.observe_stable_with_tools(&stable, Some("tools-v1")),
        (CacheStatus::Hit, None)
    );
    assert_eq!(
        tracker.observe_stable_with_tools(&stable, Some("tools-v2")),
        (CacheStatus::Break, Some("tool-defs-changed".to_owned()))
    );
    let next_day = StableCacheKey::new("identity-1", "2026-10-25");
    assert_eq!(
        tracker.observe_stable_with_tools(&next_day, Some("tools-v2")),
        (CacheStatus::Break, Some("system-prefix-changed".to_owned()))
    );
    assert_eq!(
        tracker.observe_stable_with_tools(&next_day, None),
        (CacheStatus::Break, Some("tool-defs-changed".to_owned())),
        "dropping the tool digest is a tool change"
    );
}
