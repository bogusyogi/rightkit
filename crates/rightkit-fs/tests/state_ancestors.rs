//! `StateDir::create` publishes every directory it makes: the parent of each newly created
//! directory is synced before the walk descends into it, and a failed sync stops the walk. The
//! order is observed through the `state::testing` event log (the power-loss argument is "the link
//! is flushed before anything is written below it", so the order is what is asserted).

#![cfg(unix)]

use rightkit_fs::state::testing::{fail_dir_sync, take_dir_events, DirEvent};
use rightkit_fs::{FsError, StateDir};
use std::io::ErrorKind;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

fn tmp() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().canonicalize().unwrap();
    (dir, path)
}

fn ino(path: &Path) -> u64 {
    std::fs::metadata(path).unwrap().ino()
}

#[test]
fn create_syncs_the_parent_of_each_new_directory_before_descending() {
    let (_g, root) = tmp();
    let _ = take_dir_events();
    let state = StateDir::create(&root.join("new/deep")).unwrap();
    assert_eq!(
        take_dir_events(),
        vec![
            DirEvent::Created("new".into()),
            DirEvent::Synced(ino(&root)),
            DirEvent::Created("deep".into()),
            DirEvent::Synced(ino(&root.join("new"))),
        ],
        "each new directory is published by a sync of its parent before the next is made"
    );
    // The pinned leaf is usable and its own write syncs the leaf, as before.
    state.write("record", b"x").unwrap();
    assert_eq!(
        take_dir_events(),
        vec![DirEvent::Synced(ino(&root.join("new/deep")))]
    );
}

#[test]
fn existing_directories_are_not_synced_again() {
    let (_g, root) = tmp();
    std::fs::create_dir_all(root.join("a/b")).unwrap();
    let _ = take_dir_events();
    StateDir::create(&root.join("a/b")).unwrap();
    assert!(
        take_dir_events().is_empty(),
        "nothing was created, so nothing is published"
    );
    // Only the missing tail is created and published.
    StateDir::create(&root.join("a/b/c")).unwrap();
    assert_eq!(
        take_dir_events(),
        vec![
            DirEvent::Created("c".into()),
            DirEvent::Synced(ino(&root.join("a/b")))
        ]
    );
    StateDir::open(&root.join("a/b/c")).unwrap();
    assert!(take_dir_events().is_empty(), "open never creates");
}

#[test]
fn a_failed_ancestor_sync_stops_the_walk_with_a_typed_error() {
    let (_g, root) = tmp();
    let _ = take_dir_events();
    let guard = fail_dir_sync(ErrorKind::Other);
    let error = StateDir::create(&root.join("new/deep"))
        .map(|_| ())
        .expect_err("a failed directory sync must propagate");
    drop(guard);
    assert!(
        matches!(&error, FsError::DirSyncFailed { path, .. } if path == &root.join("new")),
        "{error:?}"
    );
    assert_eq!(
        take_dir_events(),
        vec![
            DirEvent::Created("new".into()),
            DirEvent::Synced(ino(&root))
        ],
        "the walk did not descend past the unpublished directory"
    );
    assert!(
        !root.join("new/deep").exists(),
        "no directory was made below an unpublished one"
    );
}

#[test]
fn a_child_created_through_the_handle_is_published_through_its_parent() {
    let (_g, root) = tmp();
    let state = StateDir::create(&root.join("s")).unwrap();
    let _ = take_dir_events();
    state.child("kid").unwrap();
    assert_eq!(
        take_dir_events(),
        vec![
            DirEvent::Created("kid".into()),
            DirEvent::Synced(ino(&root.join("s")))
        ]
    );
}
