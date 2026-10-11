//! `Privacy`: owner-only mode bits on Unix for `OwnerOnly` and `OwnerOnlyUnixInheritWindows`, the
//! previous behaviour for `private: bool`, and no change for `Inherit`. The Windows ACLs for each
//! variant are read back in `atomic_dacl_windows.rs`.

#![cfg(unix)]

use std::io::Write;
use std::os::unix::fs::PermissionsExt;

use rightkit_fs::{atomic_write_with_options, AtomicWriteOptions, Privacy};

fn mode_after(options: AtomicWriteOptions) -> u32 {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.bin");
    atomic_write_with_options(&path, options, |f| f.write_all(b"x")).unwrap();
    std::fs::metadata(&path).unwrap().permissions().mode() & 0o777
}

#[test]
fn private_true_and_the_owner_only_variants_are_0600_on_unix() {
    let base = AtomicWriteOptions::default();
    assert_eq!(
        mode_after(AtomicWriteOptions {
            private: true,
            ..base
        }),
        0o600
    );
    assert_eq!(
        mode_after(AtomicWriteOptions {
            privacy: Privacy::OwnerOnly,
            ..base
        }),
        0o600
    );
    assert_eq!(
        mode_after(AtomicWriteOptions {
            privacy: Privacy::OwnerOnlyUnixInheritWindows,
            ..base
        }),
        0o600
    );
}

#[test]
fn inherit_is_unchanged_and_private_true_wins_over_privacy() {
    let base = AtomicWriteOptions::default();
    let inherit = mode_after(AtomicWriteOptions {
        privacy: Privacy::Inherit,
        ..base
    });
    assert_ne!(
        inherit, 0o600,
        "the default write is not forced to 0600 (umask applies)"
    );
    assert_eq!(inherit, mode_after(base));
    assert_eq!(
        mode_after(AtomicWriteOptions {
            private: true,
            privacy: Privacy::Inherit,
            ..base
        }),
        0o600
    );
    assert_eq!(base.effective_privacy(), Privacy::Inherit);
    assert_eq!(
        AtomicWriteOptions {
            private: true,
            ..base
        }
        .effective_privacy(),
        Privacy::OwnerOnly
    );
}

#[test]
fn parents_created_for_an_owner_only_write_are_0700_on_unix() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a").join("b").join("f.bin");
    let options = AtomicWriteOptions {
        create_parents: true,
        privacy: Privacy::OwnerOnlyUnixInheritWindows,
        ..AtomicWriteOptions::default()
    };
    atomic_write_with_options(&path, options, |f| f.write_all(b"x")).unwrap();
    let parent = std::fs::metadata(path.parent().unwrap())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(parent, 0o700);
}
