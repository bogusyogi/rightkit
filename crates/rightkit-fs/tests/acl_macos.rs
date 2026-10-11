//! Owner-only means owner-only under macOS extended ACLs. The mode bits `0600`/`0700` do not
//! constrain an ACL: macOS evaluates ACL `allow` entries first, and a new file or directory
//! inherits the inheritable entries of its parent's ACL. A parent carrying
//! `everyone allow read,file_inherit,directory_inherit` would make every `0600` file created
//! below it readable by everyone.
//!
//! The crate reads the parent's ACL before it creates anything private and refuses
//! (`FsError::UnsafeAcl`) when an inheritable `allow` entry would be handed on, then removes any
//! ACL the new entry got anyway. These tests make the ACLs with `/bin/chmod +a`, read them back
//! with `/bin/ls -lde`, and need a macOS volume that holds ACLs (APFS does). They have not been
//! run in the tree that wrote them.

#![cfg(target_os = "macos")]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use rightkit_fs::{
    atomic_write, atomic_write_with_options, create_private_new, ensure_private_dir,
    restrict_owner_only, AtomicWriteOptions, FsError, Privacy, StateDir,
};

fn is_owner_only_for_test(path: &Path) -> bool {
    rightkit_fs::private::is_owner_only(path).unwrap()
}

const INHERITING_ALLOW: &str = "everyone allow read,file_inherit,directory_inherit";

fn tmp() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().canonicalize().unwrap();
    (dir, path)
}

fn add_acl(path: &Path, entry: &str) {
    let status = Command::new("/bin/chmod")
        .arg("+a")
        .arg(entry)
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success(), "chmod +a {entry:?} {}", path.display());
}

/// The ACL entry lines `ls -lde` prints under the file line (` 0: group:everyone allow read`).
fn acl_entries(path: &Path) -> Vec<String> {
    let output = Command::new("/bin/ls")
        .arg("-lde")
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .skip(1)
        .filter(|line| line.trim_start().starts_with(|c: char| c.is_ascii_digit()))
        .map(str::to_string)
        .collect()
}

fn parent_with(entry: &str) -> (tempfile::TempDir, PathBuf) {
    let (guard, root) = tmp();
    let parent = root.join("p");
    std::fs::create_dir(&parent).unwrap();
    add_acl(&parent, entry);
    assert!(
        !acl_entries(&parent).is_empty(),
        "the volume holds ACLs (precondition)"
    );
    (guard, parent)
}

fn names(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect()
}

fn private_options() -> [AtomicWriteOptions; 3] {
    let base = AtomicWriteOptions::default();
    [
        AtomicWriteOptions {
            private: true,
            ..base
        },
        AtomicWriteOptions {
            privacy: Privacy::OwnerOnly,
            ..base
        },
        AtomicWriteOptions {
            privacy: Privacy::OwnerOnlyUnixInheritWindows,
            ..base
        },
    ]
}

#[test]
fn an_inheriting_parent_refuses_a_private_atomic_write_and_creates_nothing() {
    let (_g, parent) = parent_with(INHERITING_ALLOW);
    for options in private_options() {
        let error =
            atomic_write_with_options(&parent.join("secret"), options, |f| f.write_all(b"secret"))
                .expect_err("the owner-only guarantee cannot be established");
        assert!(matches!(error, FsError::UnsafeAcl(_)), "{error:?}");
    }
    assert!(
        names(&parent).is_empty(),
        "no temp, no file: {:?}",
        names(&parent)
    );
    // A write that did not ask for privacy is the caller's business and is not refused.
    atomic_write(&parent.join("plain"), b"x").unwrap();
}

#[test]
fn an_inheriting_parent_refuses_private_parents() {
    let (_g, parent) = parent_with(INHERITING_ALLOW);
    let options = AtomicWriteOptions {
        create_parents: true,
        privacy: Privacy::OwnerOnly,
        ..AtomicWriteOptions::default()
    };
    let error =
        atomic_write_with_options(&parent.join("new/deep/f"), options, |f| f.write_all(b"x"))
            .expect_err("a private directory cannot be made under this parent");
    assert!(matches!(error, FsError::UnsafeAcl(_)), "{error:?}");
    assert!(!parent.join("new").exists());
    let error = ensure_private_dir(&parent.join("dir")).expect_err("ensure_private_dir");
    assert!(matches!(error, FsError::UnsafeAcl(_)), "{error:?}");
    assert!(!parent.join("dir").exists());
}

#[test]
fn create_private_new_refuses_an_inheriting_parent_and_creates_nothing() {
    let (_g, parent) = parent_with(INHERITING_ALLOW);
    let error = create_private_new(&parent.join("log")).expect_err("refused");
    assert!(matches!(error, FsError::UnsafeAcl(_)), "{error:?}");
    assert!(names(&parent).is_empty());
}

#[test]
fn state_dir_refuses_an_inheriting_parent() {
    let (_g, parent) = parent_with(INHERITING_ALLOW);
    let error = StateDir::create(&parent.join("s"))
        .map(|_| ())
        .expect_err("mkdir refused");
    assert!(matches!(error, FsError::UnsafeAcl(_)), "{error:?}");
    assert!(!parent.join("s").exists());

    // A state directory that gets the ACL later refuses to make children and temp files in it.
    let clean = parent.parent().unwrap().join("clean");
    let state = StateDir::create(&clean).unwrap();
    add_acl(&clean, INHERITING_ALLOW);
    let error = state.child("kid").map(|_| ()).expect_err("child refused");
    assert!(matches!(error, FsError::UnsafeAcl(_)), "{error:?}");
    let error = state.write("record", b"secret").expect_err("temp refused");
    assert!(matches!(error, FsError::UnsafeAcl(_)), "{error:?}");
    assert!(
        names(&clean).is_empty(),
        "nothing was created: {:?}",
        names(&clean)
    );
}

#[cfg(feature = "lock")]
#[test]
fn a_lock_file_is_refused_under_an_inheriting_parent() {
    let (_g, parent) = parent_with(INHERITING_ALLOW);
    let error = rightkit_fs::FileLock::try_acquire(&parent.join("x.lock"))
        .map(|_| ())
        .expect_err("refused");
    assert!(matches!(error, FsError::UnsafeAcl(_)), "{error:?}");
    assert!(!parent.join("x.lock").exists());
}

/// An allow entry that is not inheritable cannot reach a new entry, and an inheritable deny entry
/// cannot widen access: both are accepted, and whatever the new file inherited is removed.
#[test]
fn entries_that_cannot_widen_access_are_accepted_and_the_new_file_ends_without_an_acl() {
    for entry in [
        "everyone allow read",
        "everyone deny delete,file_inherit,directory_inherit",
    ] {
        let (_g, parent) = parent_with(entry);
        let target = parent.join("secret");
        atomic_write_with_options(&target, private_options()[0], |f| f.write_all(b"secret"))
            .unwrap_or_else(|e| panic!("{entry}: {e:?}"));
        assert_eq!(std::fs::read(&target).unwrap(), b"secret");
        assert!(
            acl_entries(&target).is_empty(),
            "{entry}: {:?}",
            acl_entries(&target)
        );
        let file = create_private_new(&parent.join("log")).unwrap();
        drop(file);
        assert!(acl_entries(&parent.join("log")).is_empty(), "{entry}");
    }
}

/// Repair and verification: a `0600` file with an ACL `allow` entry is not owner-only, and
/// `restrict_owner_only` removes the entry although the mode bits were already right.
#[test]
fn restrict_owner_only_removes_an_acl_the_mode_check_would_miss() {
    use std::os::unix::fs::PermissionsExt;
    let (_g, root) = tmp();
    let file = root.join("f");
    std::fs::write(&file, b"x").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    add_acl(&file, "everyone allow read");
    assert!(
        !is_owner_only_for_test(&file),
        "mode 0600 but readable through the ACL"
    );
    restrict_owner_only(&file).unwrap();
    assert!(acl_entries(&file).is_empty(), "{:?}", acl_entries(&file));
    assert!(is_owner_only_for_test(&file));

    let dir = root.join("d");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    add_acl(&dir, "everyone allow list,search");
    assert!(!is_owner_only_for_test(&dir));
    restrict_owner_only(&dir).unwrap();
    assert!(acl_entries(&dir).is_empty());
}
