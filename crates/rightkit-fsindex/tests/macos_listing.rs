//! Public `list_dir_fd` against real directories. macOS only.
#![cfg(target_os = "macos")]

use rightkit_fsindex::{list_dir_fd, Entry, EntryKind, EntrySize, ListOptions, Listing};
use std::collections::HashMap;
use std::fs::{self, File};
use std::os::fd::AsFd;
use std::os::unix::fs::{symlink, MetadataExt};
use std::path::{Path, PathBuf};

fn tree() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    (dir, root)
}

fn list(dir: &Path, opts: ListOptions) -> Listing {
    let f = File::open(dir).unwrap();
    list_dir_fd(f.as_fd(), opts).unwrap()
}

fn by_name(l: &Listing) -> HashMap<String, &Entry> {
    l.entries
        .iter()
        .map(|e| (e.name.to_string_lossy().into_owned(), e))
        .collect()
}

#[test]
fn unlimited_listing_returns_every_child_and_no_dot_entries() {
    let (_d, root) = tree();
    for i in 0..10 {
        fs::write(root.join(format!("f{i}.txt")), b"x").unwrap();
    }
    fs::create_dir(root.join("sub")).unwrap();
    let l = list(&root, ListOptions::default());
    assert!(!l.truncated);
    assert_eq!(l.entries.len(), 11);
    assert!(l.entries.iter().all(|e| e.name != "." && e.name != ".."));
}

#[test]
fn limit_truncates_only_when_more_entries_exist() {
    let (_d, root) = tree();
    for i in 0..10 {
        fs::write(root.join(format!("f{i}.txt")), b"x").unwrap();
    }
    let under = list(
        &root,
        ListOptions {
            limit: Some(3),
            ..ListOptions::default()
        },
    );
    assert_eq!(under.entries.len(), 3);
    assert!(under.truncated);

    let exact = list(
        &root,
        ListOptions {
            limit: Some(10),
            ..ListOptions::default()
        },
    );
    assert_eq!(exact.entries.len(), 10);
    assert!(
        !exact.truncated,
        "exactly `limit` entries is not truncation"
    );

    let zero = list(
        &root,
        ListOptions {
            limit: Some(0),
            ..ListOptions::default()
        },
    );
    assert!(zero.entries.is_empty());
    assert!(zero.truncated);

    let empty_dir = root.join("empty");
    fs::create_dir(&empty_dir).unwrap();
    let none = list(
        &empty_dir,
        ListOptions {
            limit: Some(0),
            ..ListOptions::default()
        },
    );
    assert!(none.entries.is_empty());
    assert!(!none.truncated);
}

#[test]
fn sizes_equal_lstat_for_files_symlinks_and_directories() {
    let (_d, root) = tree();
    fs::write(root.join("empty"), b"").unwrap();
    fs::write(root.join("one"), b"x").unwrap();
    fs::write(root.join("block"), vec![7u8; 5000]).unwrap();
    fs::write(root.join("big"), vec![3u8; 3 * 1024 * 1024 + 17]).unwrap();
    symlink("/some/target/path", root.join("link")).unwrap();
    fs::create_dir(root.join("sub")).unwrap();

    let l = list(&root, ListOptions::default());
    let map = by_name(&l);
    for name in ["empty", "one", "block", "big"] {
        let meta = fs::symlink_metadata(root.join(name)).unwrap();
        let e = map[name];
        assert_eq!(e.kind, EntryKind::File, "{name}");
        assert_eq!(
            e.size,
            EntrySize::Bytes {
                logical: meta.len(),
                allocated: meta.blocks() * 512
            },
            "{name}"
        );
        assert_eq!((e.dev, e.ino), (meta.dev(), meta.ino()), "{name}");
        assert!(!e.dataless);
    }
    let link = fs::symlink_metadata(root.join("link")).unwrap();
    assert_eq!(map["link"].kind, EntryKind::Symlink);
    assert_eq!(
        map["link"].size,
        EntrySize::Bytes {
            logical: link.len(),
            allocated: link.blocks() * 512
        }
    );
    assert_eq!(map["sub"].kind, EntryKind::Dir);
    assert_eq!(map["sub"].size, EntrySize::NoSize);
}

#[test]
fn resource_fork_is_excluded_from_the_logical_size() {
    let (_d, root) = tree();
    let file = root.join("forked.bin");
    fs::write(&file, vec![1u8; 1000]).unwrap();
    // A 4 KiB resource fork: st_size must still be the 1000 data bytes.
    let c_path = std::ffi::CString::new(file.as_os_str().as_encoded_bytes()).unwrap();
    let name = c"com.apple.ResourceFork";
    let fork = vec![9u8; 4096];
    let rc = unsafe {
        libc::setxattr(
            c_path.as_ptr(),
            name.as_ptr(),
            fork.as_ptr().cast(),
            fork.len(),
            0,
            0,
        )
    };
    assert_eq!(rc, 0, "setxattr for the resource fork failed");
    let stored = unsafe {
        libc::getxattr(
            c_path.as_ptr(),
            name.as_ptr(),
            std::ptr::null_mut(),
            0,
            0,
            0,
        )
    };
    assert_eq!(stored, 4096, "the resource fork was not created");

    let meta = fs::symlink_metadata(&file).unwrap();
    assert_eq!(meta.len(), 1000);
    let l = list(&root, ListOptions::default());
    match by_name(&l)["forked.bin"].size {
        EntrySize::Bytes { logical, .. } => assert_eq!(logical, 1000),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn creation_time_is_returned_only_when_asked_and_matches_stat() {
    let (_d, root) = tree();
    let file = root.join("born.txt");
    fs::write(&file, b"hello").unwrap();
    let want = list(
        &root,
        ListOptions {
            want_creation_time: true,
            ..ListOptions::default()
        },
    );
    let map = by_name(&want);
    let e = map["born.txt"];
    let expected = fs::metadata(&file).unwrap().created().unwrap();
    assert_eq!(e.created, Some(expected));
    assert_eq!(e.modified, fs::metadata(&file).unwrap().modified().unwrap());

    let plain = list(&root, ListOptions::default());
    let plain_map = by_name(&plain);
    assert_eq!(plain_map["born.txt"].created, None);
}

#[test]
fn refuse_dataless_leaves_local_files_alone() {
    let (_d, root) = tree();
    fs::write(root.join("local.txt"), b"abc").unwrap();
    let l = list(
        &root,
        ListOptions {
            refuse_dataless: true,
            ..ListOptions::default()
        },
    );
    let map = by_name(&l);
    let e = map["local.txt"];
    assert!(!e.dataless);
    assert!(matches!(e.size, EntrySize::Bytes { logical: 3, .. }));
}

#[test]
fn a_regular_file_descriptor_is_rejected() {
    let (_d, root) = tree();
    let file = root.join("plain.txt");
    fs::write(&file, b"x").unwrap();
    let f = File::open(&file).unwrap();
    assert!(list_dir_fd(f.as_fd(), ListOptions::default()).is_err());
}
