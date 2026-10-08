//! clonefile(2) contract: clones on one APFS volume, refuses links and cross-volume requests, never copies.
#![cfg(target_os = "macos")]

use rightkit_fs::{clone_file_nofollow, CloneError};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

const MANAGED_TMP: &str = "/Volumes/D/.rightkit-managed/tmp";

fn managed_tmp() -> Option<PathBuf> {
    let base = Path::new(MANAGED_TMP);
    base.is_dir().then(|| base.to_path_buf())
}

#[test]
fn clones_a_regular_file_on_the_same_apfs_volume() {
    let Some(base) = managed_tmp() else {
        eprintln!("skip: {MANAGED_TMP} is not present");
        return;
    };
    let dir = tempfile::Builder::new()
        .prefix("clone-test-")
        .tempdir_in(&base)
        .expect("fresh subdir");
    let src = dir.path().join("source.rs");
    let dst = dir.path().join("clone.rs");
    fs::write(&src, b"pub fn a() {}\n").unwrap();

    clone_file_nofollow(&src, &dst).expect("same-volume clone must succeed");

    assert_eq!(fs::read(&dst).unwrap(), b"pub fn a() {}\n");
    assert!(!fs::symlink_metadata(&dst).unwrap().file_type().is_symlink());
    assert_ne!(
        fs::metadata(&src).unwrap().ino(),
        fs::metadata(&dst).unwrap().ino(),
        "clone is its own inode"
    );
    // The clone is independent: a write to the clone never reaches the source.
    fs::write(&dst, b"changed\n").unwrap();
    assert_eq!(fs::read(&src).unwrap(), b"pub fn a() {}\n");
}

#[test]
fn refuses_a_symlink_source_instead_of_cloning_or_following_it() {
    let Some(base) = managed_tmp() else {
        eprintln!("skip: {MANAGED_TMP} is not present");
        return;
    };
    let dir = tempfile::Builder::new()
        .prefix("clone-test-")
        .tempdir_in(&base)
        .expect("fresh subdir");
    let target = dir.path().join("target.rs");
    let link = dir.path().join("link.rs");
    fs::write(&target, b"x\n").unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let dst = dir.path().join("clone.rs");

    let error = clone_file_nofollow(&link, &dst).expect_err("symlink source must be refused");
    assert!(matches!(error, CloneError::Symlink(_)), "got {error:?}");
    assert_eq!(error.code(), "CLONE_SYMLINK_REFUSED");
    assert!(
        !dst.exists(),
        "no destination may be created for a refused link"
    );
}

#[test]
fn refuses_an_existing_destination_without_overwriting_it() {
    let Some(base) = managed_tmp() else {
        eprintln!("skip: {MANAGED_TMP} is not present");
        return;
    };
    let dir = tempfile::Builder::new()
        .prefix("clone-test-")
        .tempdir_in(&base)
        .expect("fresh subdir");
    let src = dir.path().join("source.rs");
    let dst = dir.path().join("clone.rs");
    fs::write(&src, b"src\n").unwrap();
    fs::write(&dst, b"keep\n").unwrap();

    let error =
        clone_file_nofollow(&src, &dst).expect_err("existing destination must not be replaced");
    assert!(matches!(error, CloneError::Io { .. }), "got {error:?}");
    assert_eq!(fs::read(&dst).unwrap(), b"keep\n");
}

#[test]
fn refuses_a_cross_volume_destination() {
    let Some(base) = managed_tmp() else {
        eprintln!("skip: {MANAGED_TMP} is not present");
        return;
    };
    let dir = tempfile::Builder::new()
        .prefix("clone-test-")
        .tempdir_in(&base)
        .expect("fresh subdir");
    let src = dir.path().join("source.rs");
    fs::write(&src, b"x\n").unwrap();
    // /private/tmp lives on the system volume. TMPDIR is not used: rightkit cargo points it at /Volumes/D.
    let system_tmp = Path::new("/private/tmp");
    if !system_tmp.is_dir() {
        eprintln!("skip: /private/tmp is not present");
        return;
    }
    let other = tempfile::Builder::new()
        .prefix("clone-xvol-")
        .tempdir_in(system_tmp)
        .expect("system temp dir");
    if fs::metadata(other.path()).unwrap().dev() == fs::metadata(&base).unwrap().dev() {
        panic!("/private/tmp shares a volume with {MANAGED_TMP}; cross-volume case cannot be exercised");
    }
    let dst = other.path().join("clone.rs");

    let error = clone_file_nofollow(&src, &dst).expect_err("cross-volume clone must be refused");
    assert!(
        matches!(error, CloneError::CrossVolume { .. }),
        "got {error:?}"
    );
    assert_eq!(error.code(), "CLONE_CROSS_VOLUME");
    assert!(!dst.exists(), "no partial destination may remain");
}

#[test]
fn missing_source_is_an_io_error_and_creates_nothing() {
    let Some(base) = managed_tmp() else {
        eprintln!("skip: {MANAGED_TMP} is not present");
        return;
    };
    let dir = tempfile::Builder::new()
        .prefix("clone-test-")
        .tempdir_in(&base)
        .expect("fresh subdir");
    let dst = dir.path().join("clone.rs");
    let error =
        clone_file_nofollow(&dir.path().join("absent.rs"), &dst).expect_err("missing source");
    assert!(matches!(error, CloneError::Io { .. }), "got {error:?}");
    assert!(!dst.exists());
}
