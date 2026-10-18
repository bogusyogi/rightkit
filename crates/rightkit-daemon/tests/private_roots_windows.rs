//! M6: daemon data roots on Windows go through the rightkit-fs private primitives, so an
//! explicit root is made owner-only and proved, not merely inherited from the profile.

#![cfg(windows)]

use rightkit_daemon::{Suite, SuiteError};

#[test]
fn private_directory_can_be_rechecked_with_exclusively_held_child() {
    use std::os::windows::fs::OpenOptionsExt;

    let dir = tempfile::tempdir().unwrap();
    let suite = Suite::at(dir.path().join("root")).unwrap();
    let area = suite.data_dir("state").unwrap();
    let held = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .share_mode(0)
        .open(area.join("election.lock"))
        .unwrap();
    assert_eq!(suite.data_dir("state").unwrap(), area);
    assert!(rightkit_fs::private::is_owner_only_strict(&area).unwrap());
    drop(held);
}

#[test]
fn new_private_child_waits_for_parent_creation_handle_to_close() {
    use std::os::windows::fs::OpenOptionsExt;
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    rightkit_fs::private::ensure_private_dir(&root).unwrap();
    // Match fs's new-directory verification handle: DELETE access remains until
    // verification/possible cleanup finishes; another creator cannot pin it yet.
    let held = std::fs::OpenOptions::new()
        .access_mode(0x0001_0000 | 0x80)
        .share_mode(0x3)
        .custom_flags(0x0200_0000 | 0x0020_0000)
        .open(&root)
        .unwrap();
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        drop(held);
    });
    let suite = Suite::at(root).unwrap();
    let result = suite.data_dir("state");
    release.join().unwrap();
    let child = result.unwrap();
    assert!(rightkit_fs::private::is_owner_only_strict(&child).unwrap());
}

#[test]
fn persistently_held_parent_refuses_without_creating_a_child() {
    use std::os::windows::fs::OpenOptionsExt;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    rightkit_fs::private::ensure_private_dir(&root).unwrap();
    let _held = std::fs::OpenOptions::new()
        .access_mode(0x0001_0000 | 0x80)
        .share_mode(0x3)
        .custom_flags(0x0200_0000 | 0x0020_0000)
        .open(&root)
        .unwrap();
    let suite = Suite::at(&root).unwrap();
    let error = suite.data_dir("state").unwrap_err();
    assert!(matches!(error, SuiteError::Io(ref e) if e.raw_os_error() == Some(32)));
    assert!(!root.join("state").exists());
}

fn widen_with_everyone(dir: &std::path::Path) {
    let status = std::process::Command::new("icacls")
        .arg(dir)
        .args(["/grant", "*S-1-1-0:(OI)(CI)(R)"])
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "icacls could not widen the directory");
}

#[test]
fn a_fresh_explicit_root_area_is_created_owner_only() {
    let dir = tempfile::tempdir().unwrap();
    let suite = Suite::at(dir.path().join("root")).unwrap();
    let area = suite.data_dir("state").unwrap();
    assert!(rightkit_fs::private::is_owner_only_strict(&area).unwrap());
    let shots = suite.private_service_subdir("svc", "shots").unwrap();
    assert!(rightkit_fs::private::is_owner_only_strict(&shots).unwrap());
}

#[test]
fn a_broad_preexisting_area_is_repaired_to_owner_only() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let area = root.join("state");
    std::fs::create_dir_all(&area).unwrap();
    widen_with_everyone(&area);
    assert!(
        !rightkit_fs::private::is_owner_only(&area).unwrap(),
        "precondition: the area starts broader than owner-only"
    );
    let suite = Suite::at(&root).unwrap();
    let made = suite.data_dir("state").unwrap();
    assert_eq!(made, area);
    assert!(rightkit_fs::private::is_owner_only_strict(&area).unwrap());
}

#[test]
fn a_symlinked_area_is_refused_as_insecure_and_never_followed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let target = dir.path().join("elsewhere");
    std::fs::create_dir_all(root.join(".keep")).unwrap();
    std::fs::create_dir_all(&target).unwrap();
    let link = root.join("linked");
    if std::os::windows::fs::symlink_dir(&target, &link).is_err() {
        eprintln!("symlink creation not permitted here; case skipped");
        return;
    }
    let suite = Suite::at(&root).unwrap();
    let err = suite.data_dir("linked").unwrap_err();
    assert!(
        matches!(err, SuiteError::Insecure(_)),
        "a reparse-point area is Insecure: {err:?}"
    );
    assert!(target.is_dir(), "the link target is untouched");
}
