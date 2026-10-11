use rightkit_cache::{Config, DiskCache, Key};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

type Entry = (PathBuf, u64, Option<SystemTime>);

/// Every file and directory under `root` with its size and mtime. Any filesystem write
/// (creating, deleting, renaming, or touching a file or its parent directory) changes it.
fn fs_snapshot(root: &Path) -> Vec<Entry> {
    fn walk(dir: &Path, out: &mut Vec<Entry>) {
        let mut paths: Vec<PathBuf> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        paths.sort();
        for path in paths {
            let meta = fs::metadata(&path).unwrap();
            out.push((path.clone(), meta.len(), meta.modified().ok()));
            if meta.is_dir() {
                walk(&path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, &mut out);
    out
}

#[test]
fn read_only_open_performs_zero_filesystem_writes() {
    let root = tempfile::tempdir().unwrap();
    let stored = Key::new(&["stored"]);
    let other = Key::new(&["other"]);
    {
        let cache = DiskCache::open(root.path(), Config::new(64)).unwrap();
        cache.put(&stored, b"hello").unwrap();
        cache.put(&other, b"world!!").unwrap();
    }
    // A crash leftover that a writable open would delete.
    let leftover = root.path().join(".tmp").join("crash-leftover");
    fs::create_dir_all(leftover.parent().unwrap()).unwrap();
    fs::write(&leftover, b"junk").unwrap();
    let before = fs_snapshot(root.path());

    let ro = DiskCache::open_read_only(root.path()).unwrap();
    assert!(!ro.is_writable());

    // Reads, stats and lookups all work.
    assert_eq!(ro.get(&stored).as_deref(), Some(b"hello".as_slice()));
    let path = ro.get_path(&stored).expect("indexed path");
    assert_eq!(fs::read(&path).unwrap(), b"hello");
    assert!(ro.contains(&other));
    let s = ro.stats();
    assert_eq!((s.entries, s.bytes), (2, 12));
    assert!(ro.get(&Key::new(&["missing"])).is_none());

    // Mutating calls refuse or do nothing.
    assert!(ro.put(&Key::new(&["new"]), b"x").is_err());
    assert_eq!(
        ro.reserve_path(&Key::new(&["new"]), "jpg")
            .err()
            .unwrap()
            .kind(),
        ErrorKind::PermissionDenied
    );
    assert!(!ro.remove(&stored));
    assert_eq!(ro.clear_group(&["group"]), 0);
    assert_eq!(ro.clear().entries, 0);
    assert_eq!(ro.clear_count(), 0);
    assert!(ro.clear_foreign().is_err());
    let fetched = ro.get_or_fetch(&Key::new(&["fetch"]), || Ok::<_, ()>(b"f".to_vec()));
    assert_eq!(fetched.unwrap(), b"f".to_vec());

    assert_eq!(
        fs_snapshot(root.path()),
        before,
        "a read-only handle must not write to the filesystem"
    );
    assert!(leftover.exists(), "stale .tmp files are not cleaned up");
    assert!(ro.contains(&stored) && ro.contains(&other));
    assert!(
        !ro.contains(&Key::new(&["fetch"])),
        "the failed write-through left no entry"
    );
}

#[test]
fn read_only_open_requires_an_existing_root_and_creates_nothing() {
    let parent = tempfile::tempdir().unwrap();
    let missing = parent.path().join("absent");
    let err = DiskCache::open_read_only(&missing).err().unwrap();
    assert_eq!(err.kind(), ErrorKind::NotFound);
    assert!(!missing.exists());
}
