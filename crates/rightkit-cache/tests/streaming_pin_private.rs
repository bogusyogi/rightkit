//! Streaming writers, the pinned class, owner-only entries and the strict open.

use rightkit_cache::disk::{foreign_content_error, pinned_capacity_error};
use rightkit_cache::{Config, DiskCache, Key, PutOutcome};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

fn temp_root() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

/// Yields at most `chunk` bytes per call, like a network body or a slow file.
struct ChunkReader {
    data: Vec<u8>,
    pos: usize,
    chunk: usize,
}

impl Read for ChunkReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.chunk.min(buf.len()).min(self.data.len() - self.pos);
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Files directly inside `<root>/.tmp/` (sidecars included).
fn tmp_files(root: &Path) -> Vec<PathBuf> {
    fs::read_dir(root.join(".tmp"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect()
}

#[test]
fn put_reader_streams_a_value_read_in_small_chunks() {
    let root = temp_root();
    let cache = DiskCache::open(root.path(), Config::new(1 << 20)).unwrap();
    let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let key = Key::new(&["big"]);
    let mut reader = ChunkReader {
        data: data.clone(),
        pos: 0,
        chunk: 3,
    };

    assert_eq!(
        cache.put_reader(&key, &mut reader).unwrap(),
        PutOutcome::Stored
    );
    assert_eq!(cache.get(&key).as_deref(), Some(data.as_slice()));
    assert_eq!(
        cache.stats().bytes,
        data.len() as u64,
        "byte accounting at commit"
    );
    assert!(
        tmp_files(root.path()).is_empty(),
        "commit leaves no temp file"
    );
}

#[test]
fn an_abandoned_writer_leaves_no_file_and_no_entry() {
    let root = temp_root();
    let cache = DiskCache::open(root.path(), Config::new(1 << 20)).unwrap();
    let key = Key::new(&["abandoned"]);
    {
        let mut writer = cache.put_writer(&key).unwrap();
        writer.write_all(b"half of the value").unwrap();
        assert!(
            !tmp_files(root.path()).is_empty(),
            "the writer holds a temp file"
        );
    }
    assert!(
        tmp_files(root.path()).is_empty(),
        "drop without commit removes it"
    );
    assert!(!cache.contains(&key));
    assert_eq!(cache.stats().bytes, 0);
}

#[test]
fn a_streamed_value_over_the_entry_limit_is_too_large_and_leaves_nothing() {
    let root = temp_root();
    let cache = DiskCache::open(root.path(), Config::new(1 << 20).max_entry_bytes(10)).unwrap();
    let key = Key::new(&["huge"]);
    let mut reader = ChunkReader {
        data: vec![7u8; 64],
        pos: 0,
        chunk: 4,
    };
    assert_eq!(
        cache.put_reader(&key, &mut reader).unwrap(),
        PutOutcome::TooLarge
    );
    assert!(!cache.contains(&key));
    assert!(tmp_files(root.path()).is_empty());
}

#[test]
fn pinned_entries_are_never_evicted_by_size_pressure() {
    let root = temp_root();
    let cache = DiskCache::open(root.path(), Config::new(8)).unwrap();
    let pinned = Key::new(&["pinned"]);
    let other = Key::new(&["other"]);
    let newest = Key::new(&["newest"]);
    assert_eq!(
        cache.put_pinned(&pinned, b"aaaa").unwrap(),
        PutOutcome::Stored
    );
    assert_eq!(cache.put(&other, b"bbbb").unwrap(), PutOutcome::Stored);
    assert_eq!(cache.put(&newest, b"cccc").unwrap(), PutOutcome::Stored);

    assert!(
        cache.contains(&pinned),
        "the pinned entry survives pressure"
    );
    assert!(
        !cache.contains(&other),
        "the unpinned entry is the one evicted"
    );
    assert!(cache.contains(&newest));
    assert_eq!(
        cache.pinned_bytes(),
        4,
        "pinned bytes count in the total and are reported"
    );
}

#[test]
fn an_unpinned_put_fails_with_a_typed_error_when_pinned_bytes_leave_no_room() {
    let root = temp_root();
    let cache = DiskCache::open(root.path(), Config::new(8)).unwrap();
    let pinned = Key::new(&["fills-the-cap"]);
    let small = Key::new(&["small"]);
    cache.put_pinned(&pinned, b"aaaaaaaa").unwrap();

    let err = cache.put(&small, b"b").unwrap_err();
    let typed = pinned_capacity_error(&err).expect("typed capacity error");
    assert_eq!(
        (typed.pinned_bytes, typed.entry_bytes, typed.max_bytes),
        (8, 1, 8)
    );
    assert!(!cache.contains(&small), "nothing was written or evicted");
    assert!(cache.contains(&pinned));
    assert!(tmp_files(root.path()).is_empty());
}

#[test]
fn a_pinned_put_that_would_overfill_the_pinned_class_is_refused() {
    let root = temp_root();
    let cache = DiskCache::open(root.path(), Config::new(8)).unwrap();
    cache.put_pinned(&Key::new(&["a"]), b"aaaaa").unwrap();
    let err = cache.put_pinned(&Key::new(&["b"]), b"bbbb").unwrap_err();
    assert!(pinned_capacity_error(&err).is_some());
    assert_eq!(cache.stats().entries, 1);
}

#[test]
fn unpinning_makes_an_entry_evictable_again() {
    let root = temp_root();
    let cache = DiskCache::open(root.path(), Config::new(4)).unwrap();
    let key = Key::new(&["a"]);
    cache.put_pinned(&key, b"aaaa").unwrap();
    assert!(cache.unpin(&key).unwrap());
    cache.put(&Key::new(&["b"]), b"bbbb").unwrap();
    assert!(!cache.contains(&key));
}

#[test]
fn pins_survive_a_reopen() {
    let root = temp_root();
    let key = Key::new(&["durable-pin"]);
    {
        let cache = DiskCache::open(root.path(), Config::new(8)).unwrap();
        cache.put_pinned(&key, b"aaaa").unwrap();
    }
    let cache = DiskCache::open(root.path(), Config::new(8)).unwrap();
    assert_eq!(cache.pinned_bytes(), 4, "the pin is read back on open");
    cache.put(&Key::new(&["x"]), b"xxxx").unwrap();
    cache.put(&Key::new(&["y"]), b"yyyy").unwrap();
    assert!(
        cache.contains(&key),
        "a pinned entry is not evicted after restart"
    );
}

#[test]
fn owner_only_mode_creates_entries_and_temp_directory_private() {
    let root = temp_root();
    let cache = DiskCache::open(root.path(), Config::new(1 << 20).owner_only()).unwrap();
    let key = Key::new(&["secret"]);
    cache.put(&key, b"private bytes").unwrap();
    let entry = cache.get_path(&key).expect("stored entry has a path");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&entry).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(root.path().join(".tmp"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    #[cfg(not(unix))]
    assert!(entry.is_file());
    assert_eq!(cache.get(&key).as_deref(), Some(&b"private bytes"[..]));
}

#[test]
fn strict_open_refuses_a_root_with_foreign_files_and_deletes_nothing() {
    let root = temp_root();
    {
        let cache = DiskCache::open(root.path(), Config::new(1 << 20)).unwrap();
        cache.put(&Key::new(&["entry"]), b"cached").unwrap();
    }
    let stray = root.path().join("notes.txt");
    fs::write(&stray, b"someone else's file").unwrap();
    fs::write(root.path().join(".tmp").join("scratch.txt"), b"x").unwrap();

    let err = DiskCache::open_strict(root.path(), Config::new(1 << 20))
        .err()
        .expect("strict open refuses foreign files");
    let typed = foreign_content_error(&err).expect("typed foreign-content error");
    assert_eq!(typed.count, 2);
    assert!(stray.exists(), "strict open never deletes");
    assert!(root.path().join(".tmp").join("scratch.txt").exists());
}

#[test]
fn strict_open_accepts_a_root_that_holds_only_cache_files() {
    let root = temp_root();
    {
        let cache = DiskCache::open_strict(root.path(), Config::new(1 << 20)).unwrap();
        cache.put(&Key::new(&["entry"]), b"cached").unwrap();
    }
    let cache = DiskCache::open_strict(root.path(), Config::new(1 << 20)).unwrap();
    assert_eq!(cache.stats().entries, 1);
}

#[test]
fn default_open_keeps_an_old_foreign_file_in_the_temp_directory() {
    let root = temp_root();
    let tmp = root.path().join(".tmp");
    fs::create_dir_all(&tmp).unwrap();
    let foreign = tmp.join("notes.txt");
    fs::write(&foreign, b"not ours").unwrap();
    let when = SystemTime::now() - Duration::from_secs(2 * 60 * 60);
    fs::OpenOptions::new()
        .write(true)
        .open(&foreign)
        .unwrap()
        .set_modified(when)
        .unwrap();

    let _cache = DiskCache::open(root.path(), Config::new(1 << 20)).unwrap();
    assert!(
        foreign.exists(),
        "open must never delete a file the cache did not create"
    );
}
