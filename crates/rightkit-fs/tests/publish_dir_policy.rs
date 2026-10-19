//! `publish_dir_with` strict policies: a destination that holds anything is refused and left
//! byte-identical, never replaced and never deleted. Imports go through the module path so this file
//! does not depend on the crate-root re-export.
#![cfg(feature = "fs")]

use rightkit_fs::publish_dir::{publish_dir, publish_dir_with, DirPublishMethod, DirPublishPolicy};
use rightkit_fs::FsError;
use std::fs;
use std::path::{Path, PathBuf};

fn staged(dir: &Path, name: &str, marker: &str) -> PathBuf {
    let path = dir.join(name);
    fs::create_dir_all(path.join("sub")).unwrap();
    fs::write(path.join("marker"), marker).unwrap();
    fs::write(path.join("sub/deep"), marker).unwrap();
    path
}

/// Every entry under `root` (relative path, directory marker or file bytes), sorted: two equal
/// snapshots mean a byte-identical tree.
fn snapshot(root: &Path) -> Vec<(String, Option<Vec<u8>>)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Option<Vec<u8>>)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if entry.file_type().unwrap().is_dir() {
                out.push((rel, None));
                walk(root, &path, out);
            } else {
                out.push((rel, Some(fs::read(&path).unwrap())));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

fn names(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

#[test]
fn the_default_policy_is_replace() {
    assert_eq!(DirPublishPolicy::default(), DirPublishPolicy::Replace);
}

#[test]
fn replace_policy_is_publish_dir() {
    let t = tempfile::tempdir().unwrap();
    let dest = staged(t.path(), "model", "old");
    fs::write(dest.join("only-in-old"), "x").unwrap();
    let src = staged(t.path(), "stage", "new");
    let out = publish_dir_with(&src, &dest, DirPublishPolicy::Replace).unwrap();
    assert!(out.replaced);
    assert_eq!(fs::read_to_string(dest.join("marker")).unwrap(), "new");
    assert!(!dest.join("only-in-old").exists());
    assert!(!src.exists());
    assert_eq!(names(t.path()), ["model"]);

    let again = staged(t.path(), "stage", "newer");
    publish_dir(&again, &dest).unwrap();
    assert_eq!(fs::read_to_string(dest.join("marker")).unwrap(), "newer");
}

#[test]
fn operand_checks_are_the_same_for_the_strict_policies() {
    for policy in [
        DirPublishPolicy::AbsentOnly,
        DirPublishPolicy::AbsentOrEmpty,
    ] {
        let t = tempfile::tempdir().unwrap();
        let src = staged(t.path(), "stage", "new");
        let before = snapshot(&src);
        for dest in [src.clone(), src.join("inside"), t.path().to_path_buf()] {
            let err = publish_dir_with(&src, &dest, policy).unwrap_err();
            assert_eq!(
                err.kind(),
                Some(std::io::ErrorKind::InvalidInput),
                "{policy:?} {dest:?}: {err:?}"
            );
        }
        let temp_shaped = t.path().join(".model.rktmp-123-1-0");
        let err = publish_dir_with(&src, &temp_shaped, policy).unwrap_err();
        assert!(matches!(err, FsError::BadName(_)), "{err:?}");
        let file = t.path().join("f");
        fs::write(&file, "x").unwrap();
        let err = publish_dir_with(&file, &t.path().join("out"), policy).unwrap_err();
        assert!(matches!(err, FsError::NotADirectory(_)), "{err:?}");
        assert_eq!(snapshot(&src), before);
        assert!(!t.path().join("out").exists());
    }
}

/// The native no-replace rename exists on these platforms.
#[cfg(any(windows, target_os = "linux", target_vendor = "apple"))]
mod native {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Barrier};

    #[test]
    fn absent_only_publishes_to_an_absent_name() {
        let t = tempfile::tempdir().unwrap();
        let src = staged(t.path(), "stage", "new");
        let dest = t.path().join("model");
        let out = publish_dir_with(&src, &dest, DirPublishPolicy::AbsentOnly).unwrap();
        assert_eq!(
            (out.replaced, out.method),
            (false, DirPublishMethod::ExclusiveRename)
        );
        assert_eq!(fs::read_to_string(dest.join("marker")).unwrap(), "new");
        assert_eq!(fs::read_to_string(dest.join("sub/deep")).unwrap(), "new");
        assert!(!src.exists());
        assert_eq!(names(t.path()), ["model"]);
    }

    #[test]
    fn absent_only_refuses_an_empty_destination_and_leaves_it() {
        let t = tempfile::tempdir().unwrap();
        let dest = t.path().join("model");
        fs::create_dir(&dest).unwrap();
        let src = staged(t.path(), "stage", "new");
        let src_before = snapshot(&src);
        let err = publish_dir_with(&src, &dest, DirPublishPolicy::AbsentOnly).unwrap_err();
        assert!(matches!(err, FsError::AlreadyExists(_)), "{err:?}");
        assert!(dest.is_dir());
        assert!(snapshot(&dest).is_empty(), "the empty destination changed");
        assert_eq!(snapshot(&src), src_before, "the source changed");
        assert_eq!(names(t.path()), ["model", "stage"]);
    }

    #[test]
    fn absent_only_refuses_a_non_empty_destination_and_leaves_it_byte_identical() {
        let t = tempfile::tempdir().unwrap();
        let dest = staged(t.path(), "model", "precious");
        fs::write(dest.join("binary"), [0u8, 159, 146, 150, 255]).unwrap();
        let before = snapshot(&dest);
        let src = staged(t.path(), "stage", "new");
        let src_before = snapshot(&src);
        let err = publish_dir_with(&src, &dest, DirPublishPolicy::AbsentOnly).unwrap_err();
        assert!(matches!(err, FsError::AlreadyExists(_)), "{err:?}");
        assert_eq!(snapshot(&dest), before);
        assert_eq!(snapshot(&src), src_before);
        assert_eq!(names(t.path()), ["model", "stage"]);
    }

    #[test]
    fn absent_only_refuses_a_file_destination() {
        let t = tempfile::tempdir().unwrap();
        let file = t.path().join("model");
        fs::write(&file, "not a directory").unwrap();
        let src = staged(t.path(), "stage", "new");
        let err = publish_dir_with(&src, &file, DirPublishPolicy::AbsentOnly).unwrap_err();
        assert!(matches!(err, FsError::AlreadyExists(_)), "{err:?}");
        assert_eq!(fs::read_to_string(&file).unwrap(), "not a directory");
        assert!(src.join("marker").exists());
    }

    #[test]
    fn absent_or_empty_publishes_to_an_absent_name() {
        let t = tempfile::tempdir().unwrap();
        let src = staged(t.path(), "stage", "new");
        let dest = t.path().join("model");
        let out = publish_dir_with(&src, &dest, DirPublishPolicy::AbsentOrEmpty).unwrap();
        assert_eq!(
            (out.replaced, out.method),
            (false, DirPublishMethod::ExclusiveRename)
        );
        assert_eq!(fs::read_to_string(dest.join("marker")).unwrap(), "new");
        assert!(!src.exists());
        assert_eq!(names(t.path()), ["model"]);
    }

    #[test]
    fn absent_or_empty_replaces_an_empty_directory() {
        let t = tempfile::tempdir().unwrap();
        let dest = t.path().join("model");
        fs::create_dir(&dest).unwrap();
        let src = staged(t.path(), "stage", "new");
        let out = publish_dir_with(&src, &dest, DirPublishPolicy::AbsentOrEmpty).unwrap();
        assert_eq!(
            (out.replaced, out.method),
            (true, DirPublishMethod::EmptyRemovedThenExclusiveRename)
        );
        assert_eq!(fs::read_to_string(dest.join("marker")).unwrap(), "new");
        assert_eq!(fs::read_to_string(dest.join("sub/deep")).unwrap(), "new");
        assert!(!src.exists());
        assert_eq!(names(t.path()), ["model"], "a stage or aside name was left");
    }

    #[test]
    fn absent_or_empty_refuses_a_non_empty_destination_and_keeps_every_file() {
        let t = tempfile::tempdir().unwrap();
        // One file at the top, one nested, and one empty subdirectory: each makes it non-empty.
        for shape in ["file", "nested", "empty-subdirectory"] {
            let dest = t.path().join(format!("model-{shape}"));
            fs::create_dir(&dest).unwrap();
            match shape {
                "file" => fs::write(dest.join("theirs"), b"theirs").unwrap(),
                "nested" => {
                    fs::create_dir_all(dest.join("a/b")).unwrap();
                    fs::write(dest.join("a/b/theirs"), b"theirs").unwrap();
                }
                _ => fs::create_dir(dest.join("only-a-directory")).unwrap(),
            }
            let before = snapshot(&dest);
            let src = staged(t.path(), &format!("stage-{shape}"), "new");
            let src_before = snapshot(&src);
            let err = publish_dir_with(&src, &dest, DirPublishPolicy::AbsentOrEmpty).unwrap_err();
            assert!(matches!(err, FsError::AlreadyExists(_)), "{shape}: {err:?}");
            assert_eq!(snapshot(&dest), before, "{shape}: the destination changed");
            assert_eq!(snapshot(&src), src_before, "{shape}: the source changed");
        }
        assert!(
            names(t.path()).iter().all(|n| !n.contains("rkold")),
            "{:?}",
            names(t.path())
        );
    }

    #[test]
    fn absent_or_empty_refuses_a_file_destination() {
        let t = tempfile::tempdir().unwrap();
        let file = t.path().join("model");
        fs::write(&file, "not a directory").unwrap();
        let src = staged(t.path(), "stage", "new");
        let err = publish_dir_with(&src, &file, DirPublishPolicy::AbsentOrEmpty).unwrap_err();
        assert!(matches!(err, FsError::NotADirectory(_)), "{err:?}");
        assert_eq!(fs::read_to_string(&file).unwrap(), "not a directory");
        assert!(src.join("marker").exists());
    }

    /// Make `link` a directory symlink to `target`; `false` when the platform will not allow it
    /// (Windows without the symlink privilege), in which case the test has nothing to check.
    fn make_dir_symlink(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_dir(target, link).is_ok()
        }
    }

    #[test]
    fn a_symlink_destination_is_refused_by_both_strict_policies() {
        for (policy, empty_target) in [
            (DirPublishPolicy::AbsentOnly, false),
            (DirPublishPolicy::AbsentOnly, true),
            (DirPublishPolicy::AbsentOrEmpty, false),
            (DirPublishPolicy::AbsentOrEmpty, true),
        ] {
            let t = tempfile::tempdir().unwrap();
            let real = if empty_target {
                let p = t.path().join("real");
                fs::create_dir(&p).unwrap();
                p
            } else {
                staged(t.path(), "real", "precious")
            };
            let real_before = snapshot(&real);
            let link = t.path().join("link");
            if !make_dir_symlink(&real, &link) {
                eprintln!("skipped: cannot create a directory symlink here");
                return;
            }
            let src = staged(t.path(), "stage", "new");
            let src_before = snapshot(&src);
            let err = publish_dir_with(&src, &link, policy).unwrap_err();
            match policy {
                DirPublishPolicy::AbsentOrEmpty => {
                    assert!(matches!(err, FsError::Symlink(_)), "{policy:?}: {err:?}")
                }
                _ => assert!(
                    matches!(err, FsError::AlreadyExists(_)),
                    "{policy:?}: {err:?}"
                ),
            }
            assert!(
                fs::symlink_metadata(&link)
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                "{policy:?}: the symlink was replaced or removed"
            );
            assert!(real.is_dir(), "{policy:?}: the target was removed");
            assert_eq!(snapshot(&real), real_before, "{policy:?}");
            assert_eq!(snapshot(&src), src_before, "{policy:?}");
        }
    }

    /// A competitor that puts a file into `dest` while `publish_dir_with(.., AbsentOrEmpty)` runs may
    /// win or lose, but whatever it managed to create is never deleted.
    #[test]
    fn a_competing_creator_never_loses_a_file() {
        let t = tempfile::tempdir().unwrap();
        let dest = t.path().join("model");
        let (mut refused, mut published) = (0u32, 0u32);
        let (mut created, mut empty_removed) = (0u32, 0u32);
        for round in 0..300 {
            fs::create_dir(&dest).unwrap();
            let src = staged(t.path(), "stage", "new");
            let ready = Arc::new(Barrier::new(2));
            let stop = Arc::new(AtomicBool::new(false));
            let competitor = {
                let (ready, stop, dest) = (ready.clone(), stop.clone(), dest.clone());
                std::thread::spawn(move || {
                    ready.wait();
                    let theirs = dest.join("theirs");
                    // Keep trying: the file can only be created while `dest` exists, so it lands in
                    // the empty directory, or in the published directory after the rename.
                    while !stop.load(Ordering::Acquire) {
                        if fs::write(&theirs, b"theirs").is_ok() {
                            return true;
                        }
                        std::thread::yield_now();
                    }
                    false
                })
            };
            ready.wait();
            let result = publish_dir_with(&src, &dest, DirPublishPolicy::AbsentOrEmpty);
            // Give the competitor a moment to land after the publish; it only matters if it can.
            std::thread::sleep(std::time::Duration::from_millis(1));
            stop.store(true, Ordering::Release);
            let wrote = competitor.join().unwrap();
            match &result {
                Ok(out) => {
                    published += 1;
                    empty_removed += u32::from(out.replaced);
                    assert_eq!(fs::read_to_string(dest.join("marker")).unwrap(), "new");
                    assert_eq!(fs::read_to_string(dest.join("sub/deep")).unwrap(), "new");
                    assert!(!src.exists(), "round {round}");
                }
                Err(FsError::AlreadyExists(_)) => {
                    refused += 1;
                    assert!(
                        wrote,
                        "round {round}: refused with nothing in the destination"
                    );
                    // Refused: the source is intact and the destination is the competitor's.
                    assert_eq!(fs::read_to_string(src.join("marker")).unwrap(), "new");
                }
                Err(other) => panic!("round {round}: {other:?}"),
            }
            if wrote {
                created += 1;
                assert_eq!(
                    fs::read(dest.join("theirs")).unwrap(),
                    b"theirs",
                    "round {round}: the competitor's file was deleted (result {result:?})"
                );
            } else {
                assert!(
                    result.is_ok(),
                    "round {round}: refused but nothing was written"
                );
            }
            // Reset for the next round without ever relying on the code under test.
            fs::remove_dir_all(&dest).unwrap();
            if src.exists() {
                fs::remove_dir_all(&src).unwrap();
            }
            assert!(
                names(t.path()).iter().all(|n| !n.contains("rkold")),
                "round {round}: {:?}",
                names(t.path())
            );
        }
        eprintln!(
            "competing creator: {published} published ({empty_removed} removed an empty dest), \
             {refused} refused, competitor wrote in {created} of 300 rounds"
        );
    }
}

/// Where there is no native no-replace rename the strict policies refuse and change nothing.
#[cfg(not(any(windows, target_os = "linux", target_vendor = "apple")))]
mod unsupported {
    use super::*;

    #[test]
    fn strict_policies_report_unavailable_and_change_nothing() {
        for policy in [
            DirPublishPolicy::AbsentOnly,
            DirPublishPolicy::AbsentOrEmpty,
        ] {
            let t = tempfile::tempdir().unwrap();
            let dest = t.path().join("model");
            fs::create_dir(&dest).unwrap();
            let src = staged(t.path(), "stage", "new");
            let src_before = snapshot(&src);
            let err = publish_dir_with(&src, &dest, policy).unwrap_err();
            assert!(
                matches!(err, FsError::Unavailable(_)),
                "{policy:?}: {err:?}"
            );
            assert!(dest.is_dir());
            assert!(snapshot(&dest).is_empty());
            assert_eq!(snapshot(&src), src_before);
        }
    }
}
