//! `StateDir::create` publishes the link of every directory it walks, made or merely found: the
//! parent of each is synced before the walk descends into it (once per directory per process,
//! and only for directories the caller owns), and a failed sync stops the walk. The order is
//! observed through the `state::testing` event log (the power-loss argument is "the link is
//! flushed before anything is written below it", so the order is what is asserted).
//!
//! The record of published directories is process-wide and shared by parallel tests, so every
//! test starts by publishing its temp root and all that is above it (`tmp`); what is asserted
//! after that is the walk under the root only.

#![cfg(unix)]

use rightkit_fs::state::testing::{fail_dir_sync, take_dir_events, DirEvent};
use rightkit_fs::{FsError, StateDir};
use std::io::ErrorKind;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

fn tmp() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().canonicalize().unwrap();
    StateDir::create(&path).unwrap();
    let _ = take_dir_events();
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
fn a_directory_this_process_published_is_not_synced_again() {
    let (_g, root) = tmp();
    StateDir::create(&root.join("a/b")).unwrap();
    let _ = take_dir_events();
    StateDir::create(&root.join("a/b")).unwrap();
    assert!(
        take_dir_events().is_empty(),
        "both links were published by this process, so nothing is synced again"
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

/// The directory below was made by a different process (a real `mkdir`), exactly as a crashed
/// earlier run or a racing thread between its `mkdir` and its bookkeeping would leave it: it is
/// there, this process never recorded it, and nothing on disk says whether its parent was synced.
/// The walk that finds it must sync the link itself, before descending.
#[test]
fn a_directory_made_by_another_process_is_published_by_the_walk_that_finds_it() {
    let (_g, root) = tmp();
    let status = std::process::Command::new("mkdir")
        .arg(root.join("other"))
        .status()
        .unwrap();
    assert!(status.success());
    let _ = take_dir_events();
    let state = StateDir::create(&root.join("other/leaf")).unwrap();
    assert_eq!(
        take_dir_events(),
        vec![
            DirEvent::Synced(ino(&root)),
            DirEvent::Created("leaf".into()),
            DirEvent::Synced(ino(&root.join("other"))),
        ],
        "the found directory's link is synced before the walk descends into it"
    );
    state.write("record", b"x").unwrap();
    // A fresh handle on it is now known to be published.
    let _ = take_dir_events();
    StateDir::create(&root.join("other/leaf")).unwrap();
    assert!(take_dir_events().is_empty());
}

/// `child` has the same rule: an existing child nobody here published has its link synced, once.
#[test]
fn a_child_made_by_another_process_is_published_by_the_child_call() {
    let (_g, root) = tmp();
    let state = StateDir::create(&root.join("s")).unwrap();
    let status = std::process::Command::new("mkdir")
        .arg(root.join("s/found"))
        .status()
        .unwrap();
    assert!(status.success());
    let _ = take_dir_events();
    state.child("found").unwrap();
    assert_eq!(
        take_dir_events(),
        vec![DirEvent::Synced(ino(&root.join("s")))]
    );
    state.child("found").unwrap();
    assert!(take_dir_events().is_empty(), "published once");
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

#[test]
fn a_retry_after_a_failed_ancestor_sync_publishes_the_existing_directory_first() {
    let (_g, root) = tmp();
    let guard = fail_dir_sync(ErrorKind::Other);
    StateDir::create(&root.join("new/deep"))
        .map(|_| ())
        .expect_err("the first sync fails");
    drop(guard);
    let _ = take_dir_events();
    // `new` exists now, and its link in `root` was never flushed. The retry must flush it before
    // it creates `deep` below it and a write succeeds.
    let state = StateDir::create(&root.join("new/deep")).unwrap();
    assert_eq!(
        take_dir_events(),
        vec![
            DirEvent::Synced(ino(&root)),
            DirEvent::Created("deep".into()),
            DirEvent::Synced(ino(&root.join("new"))),
        ],
        "the existing, unpublished component is synced before the walk descends"
    );
    state.write("record", b"x").unwrap();
}

#[test]
fn a_retry_that_keeps_failing_never_writes_below_the_unpublished_directory() {
    let (_g, root) = tmp();
    let guard = fail_dir_sync(ErrorKind::Other);
    StateDir::create(&root.join("a/b"))
        .map(|_| ())
        .expect_err("the first sync fails");
    let _ = take_dir_events();
    let again = StateDir::create(&root.join("a/b"))
        .map(|_| ())
        .expect_err("the retry still owes the sync");
    drop(guard);
    assert!(
        matches!(&again, FsError::DirSyncFailed { path, .. } if path == &root.join("a")),
        "{again:?}"
    );
    assert!(!root.join("a/b").exists());
}
