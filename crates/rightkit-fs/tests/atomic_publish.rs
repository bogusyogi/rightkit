//! The order and the failure semantics of an atomic write, observed through the `testing` seam:
//! which flush, replace and directory-sync calls run and in what order (the power-loss argument),
//! that the owner-only restriction is in place before the replace publishes the file, that a
//! required hardening step failing leaves nothing published, and that a failure after publication
//! is typed, reported only after the required directory sync, and says the content is in place.

use std::cell::RefCell;
use std::io::Write;
use std::path::Path;
use std::rc::Rc;

use rightkit_fs::atomic::testing::{fail_recovery_move, on_replace, take_steps, Step};
use rightkit_fs::{
    atomic_write_with_options, sweep_stale_temps, AtomicWriteOptions, FsError, ReplaceMode,
    StateDir,
};

/// What a hook saw just before the rename: the temp file's mode and the destination's content.
#[cfg(unix)]
type Observed = (u32, Option<Vec<u8>>);

fn write(path: &Path, options: AtomicWriteOptions, bytes: &[u8]) -> rightkit_fs::Result<()> {
    atomic_write_with_options(path, options, |f| f.write_all(bytes))
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

// ------------------------------------------------------------------ order of the sync calls

#[cfg(unix)]
#[test]
fn a_durable_write_flushes_the_temp_then_replaces_then_syncs_the_directory() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("state.json");
    let _ = take_steps();
    write(&target, AtomicWriteOptions::default(), b"x").unwrap();
    assert_eq!(
        take_steps(),
        vec![
            Step::TempFlushed,
            Step::Replace,
            Step::DirSynced(dir.path().to_path_buf())
        ],
        "the file is flushed before the rename and the directory after it"
    );
}

#[cfg(windows)]
#[test]
fn a_durable_write_on_windows_flushes_the_temp_and_claims_no_directory_sync() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("state.json");
    let _ = take_steps();
    write(&target, AtomicWriteOptions::default(), b"x").unwrap();
    assert_eq!(take_steps(), vec![Step::TempFlushed, Step::Replace]);
}

#[test]
fn a_non_durable_write_flushes_and_syncs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("cache.bin");
    let options = AtomicWriteOptions {
        durable: false,
        ..AtomicWriteOptions::default()
    };
    let _ = take_steps();
    write(&target, options, b"x").unwrap();
    assert_eq!(take_steps(), vec![Step::Replace]);
}

#[test]
fn a_failed_replace_syncs_no_directory_and_publishes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("state.json");
    std::fs::write(&target, b"old").unwrap();
    let _ = take_steps();
    let result = {
        let _guard = on_replace(|_| Some(std::io::Error::other("injected replace failure")));
        write(&target, AtomicWriteOptions::default(), b"new")
    };
    let error = result.expect_err("the replace failed");
    assert!(!error.target_replaced(), "{error:?}");
    assert_eq!(take_steps(), vec![Step::TempFlushed, Step::Replace]);
    assert_eq!(std::fs::read(&target).unwrap(), b"old");
    assert_eq!(names(dir.path()), vec!["state.json".to_string()]);
}

#[cfg(unix)]
#[test]
fn no_replace_flushes_publishes_exclusively_then_syncs_the_directory() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("once.json");
    let options = AtomicWriteOptions {
        replace: ReplaceMode::NoReplace,
        ..AtomicWriteOptions::default()
    };
    let _ = take_steps();
    write(&target, options, b"x").unwrap();
    assert_eq!(
        take_steps(),
        vec![
            Step::TempFlushed,
            Step::Replace,
            Step::DirSynced(dir.path().to_path_buf())
        ]
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"x");
}

#[cfg(unix)]
#[test]
fn a_no_replace_loser_syncs_no_directory_and_leaves_the_winner() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("once.json");
    std::fs::write(&target, b"winner").unwrap();
    let options = AtomicWriteOptions {
        replace: ReplaceMode::NoReplace,
        ..AtomicWriteOptions::default()
    };
    let _ = take_steps();
    let error = write(&target, options, b"loser").expect_err("the name is taken");
    assert!(matches!(error, FsError::AlreadyExists(_)), "{error:?}");
    assert!(!error.target_replaced());
    assert_eq!(take_steps(), vec![Step::TempFlushed, Step::Replace]);
    assert_eq!(std::fs::read(&target).unwrap(), b"winner");
    assert_eq!(names(dir.path()), vec!["once.json".to_string()]);
}

// ------------------------------------------------ owner-only before publication (Unix, by mode)

#[cfg(unix)]
#[test]
fn the_temp_is_owner_only_before_the_replace_publishes_it() {
    use rightkit_fs::Privacy;
    use std::os::unix::fs::PermissionsExt;

    let base = AtomicWriteOptions::default();
    let cases = [
        (
            "private",
            AtomicWriteOptions {
                private: true,
                ..base
            },
        ),
        (
            "OwnerOnly",
            AtomicWriteOptions {
                privacy: Privacy::OwnerOnly,
                ..base
            },
        ),
        (
            "OwnerOnlyUnixInheritWindows",
            AtomicWriteOptions {
                privacy: Privacy::OwnerOnlyUnixInheritWindows,
                ..base
            },
        ),
    ];
    for (label, options) in cases {
        for existing in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("secret.json");
            if existing {
                std::fs::write(&target, b"old").unwrap();
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
            }
            // At the instant before the rename: the temp's mode, and what the destination name
            // shows (the old content, or nothing). Nothing readable by others may ever be
            // published under the destination name.
            let seen: Rc<RefCell<Vec<Observed>>> = Rc::default();
            let sink = seen.clone();
            let guard = on_replace(move |call| {
                let mode = std::fs::metadata(&call.temp).unwrap().permissions().mode() & 0o777;
                sink.borrow_mut()
                    .push((mode, std::fs::read(&call.target).ok()));
                None
            });
            write(&target, options, b"secret").unwrap();
            drop(guard);

            let seen = seen.borrow();
            assert_eq!(seen.len(), 1, "{label}: one replace call");
            assert_eq!(
                seen[0].0 & 0o077,
                0,
                "{label} existing={existing}: the temp was group/other accessible when published ({:o})",
                seen[0].0
            );
            assert_eq!(
                seen[0].1,
                existing.then(|| b"old".to_vec()),
                "{label}: the new content must not be visible before the replace"
            );
            let published = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                published & 0o077,
                0,
                "{label}: published mode {published:o}"
            );
            assert_eq!(std::fs::read(&target).unwrap(), b"secret");
        }
    }
}

// ----------------------------------------- post-publication failures (Unix link fallback)

#[cfg(unix)]
mod post_publication {
    use super::*;
    use rightkit_fs::atomic::testing::{fail_dir_sync, force_link_fallback};
    use rightkit_fs::{rename_exclusive, PostPublishStep, ReplaceMode};
    use std::io::ErrorKind;

    fn no_replace() -> AtomicWriteOptions {
        AtomicWriteOptions {
            replace: ReplaceMode::NoReplace,
            ..AtomicWriteOptions::default()
        }
    }

    #[test]
    fn a_failed_unlink_after_the_link_is_a_typed_error_and_the_directory_is_still_synced() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("once.bin");
        let _ = take_steps();
        let result = {
            let _fallback = force_link_fallback(true);
            write(&target, no_replace(), b"published")
        };
        let error = result.expect_err("the cleanup step failed");
        assert!(
            matches!(
                &error,
                FsError::ReplacedButPostStepFailed { path, step: PostPublishStep::TempCleanup, .. }
                    if path == &target
            ),
            "{error:?}"
        );
        assert!(
            error.target_replaced(),
            "published content is reported as such"
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"published");
        assert_eq!(
            take_steps(),
            vec![
                Step::TempFlushed,
                Step::Replace,
                Step::DirSynced(dir.path().to_path_buf())
            ],
            "the required directory sync ran before the error was returned"
        );
        assert_eq!(
            names(dir.path()),
            vec!["once.bin".to_string()],
            "the leftover second name is removed on the way out"
        );
    }

    #[test]
    fn the_link_fallback_publishes_cleanly_when_the_unlink_works() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("once.bin");
        {
            let _fallback = force_link_fallback(false);
            write(&target, no_replace(), b"first").unwrap();
            let again = write(&target, no_replace(), b"second").unwrap_err();
            assert!(matches!(again, FsError::AlreadyExists(_)), "{again:?}");
            assert!(!again.target_replaced());
        }
        assert_eq!(std::fs::read(&target).unwrap(), b"first");
        assert_eq!(names(dir.path()), vec!["once.bin".to_string()]);
    }

    #[test]
    fn a_failed_directory_sync_outranks_a_failed_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("once.bin");
        let result = {
            let _sync = fail_dir_sync(ErrorKind::Other);
            let _fallback = force_link_fallback(true);
            write(&target, no_replace(), b"published")
        };
        let error = result.expect_err("both post-publication steps failed");
        assert!(
            matches!(&error, FsError::ReplacedButDirSyncFailed { .. }),
            "{error:?}"
        );
        assert!(error.target_replaced());
        assert_eq!(std::fs::read(&target).unwrap(), b"published");
    }

    #[test]
    fn rename_exclusive_reports_a_published_target_whose_source_name_could_not_be_removed() {
        let dir = tempfile::tempdir().unwrap();
        let (from, to) = (dir.path().join("from"), dir.path().join("to"));
        std::fs::write(&from, b"payload").unwrap();
        let error = {
            let _fallback = force_link_fallback(true);
            rename_exclusive(&from, &to).expect_err("the unlink failed")
        };
        assert!(
            matches!(
                &error,
                FsError::ReplacedButPostStepFailed { path, step: PostPublishStep::TempCleanup, .. }
                    if path == &to
            ),
            "{error:?}"
        );
        assert!(error.target_replaced());
        assert_eq!(std::fs::read(&to).unwrap(), b"payload", "the target exists");
        assert!(from.exists(), "the source name is still there, as reported");
    }
}

// ------------------------------------------- an interrupted replace keeps both versions reachable

/// `ERROR_UNABLE_TO_MOVE_REPLACEMENT`: `ReplaceFileW` removed the destination (no backup) or
/// renamed it away, then could not move the replacement in.
const UNABLE_TO_MOVE_REPLACEMENT: i32 = 1176;

/// What `ReplaceFileW` leaves after 1176 without a backup: the destination is gone and the
/// replacement is still under its temp name. Returns the error and that temp name.
fn interrupted_write(target: &Path) -> (FsError, std::path::PathBuf) {
    let temp_seen: Rc<RefCell<Option<std::path::PathBuf>>> = Rc::default();
    let sink = temp_seen.clone();
    let result = {
        let _guard = on_replace(move |call| {
            std::fs::remove_file(&call.target).unwrap();
            *sink.borrow_mut() = Some(call.temp.clone());
            Some(std::io::Error::from_raw_os_error(
                UNABLE_TO_MOVE_REPLACEMENT,
            ))
        });
        write(target, AtomicWriteOptions::default(), b"new")
    };
    let temp = temp_seen.borrow().clone().expect("the hook ran");
    (result.expect_err("the replace was interrupted"), temp)
}

const SWEEP_ENV: &str = "RIGHTKIT_FS_TEST_SWEEP_DIR";

/// Runs in the re-executed child: a fresh process sweeps the directory named in the env var with
/// both sweepers at zero age, which reclaims every complete generated temp name there is.
#[test]
fn sweep_entry_point() {
    let Ok(dir) = std::env::var(SWEEP_ENV) else {
        return;
    };
    let dir = Path::new(&dir);
    let atomic = sweep_stale_temps(dir, std::time::Duration::ZERO).unwrap();
    let state = StateDir::open(dir)
        .unwrap()
        .sweep_temps(std::time::Duration::ZERO)
        .unwrap();
    println!("SWEPT {atomic} {state}");
}

#[test]
fn a_replace_that_lost_the_destination_keeps_the_new_content_where_a_sweep_cannot_reach_it() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("state.json");
    std::fs::write(&target, b"old").unwrap();
    let (error, temp) = interrupted_write(&target);
    let recovery = match &error {
        FsError::ReplaceInterrupted {
            path,
            new_content,
            previous_content,
            ..
        } => {
            assert_eq!(path, &target);
            assert_eq!(previous_content, &None, "the old file is gone");
            new_content.clone().expect("the new content is named")
        }
        other => panic!("expected ReplaceInterrupted, got {other:?}"),
    };
    assert!(!error.target_replaced());
    assert_ne!(recovery, temp, "moved off the sweepable temp name");
    assert!(!temp.exists());
    let name = recovery.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.starts_with(".state.json.rktnew-"), "{name}");
    assert_eq!(recovery.parent(), Some(dir.path()));
    assert_eq!(
        std::fs::read(&recovery).expect("the recovery file holds the only copy of the new content"),
        b"new"
    );

    // The writer is gone and any age counts as old: a sweep in this process, then a sweep in a
    // fresh process (a restart), must both leave it. Nothing but the file name protects it.
    assert_eq!(
        sweep_stale_temps(dir.path(), std::time::Duration::ZERO).unwrap(),
        0
    );
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "sweep_entry_point",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(SWEEP_ENV, dir.path())
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("SWEPT 0 0"),
        "the restarted sweep removed something: {text}"
    );
    assert_eq!(
        std::fs::read(&recovery).unwrap(),
        b"new",
        "still there after the restart"
    );
}

#[test]
fn an_interrupted_replace_that_cannot_be_protected_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("state.json");
    std::fs::write(&target, b"old").unwrap();
    let (error, temp) = {
        let _fail = fail_recovery_move();
        interrupted_write(&target)
    };
    match &error {
        FsError::ReplaceInterrupted { new_content, .. } => {
            assert_eq!(
                new_content.as_ref(),
                Some(&temp),
                "still under the temp name"
            );
        }
        other => panic!("expected ReplaceInterrupted, got {other:?}"),
    }
    assert!(
        error.to_string().contains("sweep may delete it"),
        "the message must say the copy is not protected: {error}"
    );
    assert_eq!(
        std::fs::read(&temp).expect("the writer must not delete the only copy"),
        b"new"
    );
}

#[test]
fn error_1176_with_the_destination_intact_is_an_ordinary_failure_and_cleans_up() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("state.json");
    std::fs::write(&target, b"old").unwrap();
    let result = {
        let _guard = on_replace(|_| {
            Some(std::io::Error::from_raw_os_error(
                UNABLE_TO_MOVE_REPLACEMENT,
            ))
        });
        write(&target, AtomicWriteOptions::default(), b"new")
    };
    let error = result.expect_err("the replace failed");
    assert!(matches!(error, FsError::Io { .. }), "{error:?}");
    assert_eq!(std::fs::read(&target).unwrap(), b"old");
    assert_eq!(names(dir.path()), vec!["state.json".to_string()]);
}

// ------------------------------------------------- parent directories are published (Unix)

#[cfg(unix)]
mod parents {
    use super::*;
    use rightkit_fs::atomic::testing::fail_dir_sync;

    fn with_parents() -> AtomicWriteOptions {
        AtomicWriteOptions {
            create_parents: true,
            ..AtomicWriteOptions::default()
        }
    }

    /// Publishes `root` and everything above it, so the step log of the test shows only the walk
    /// under `root` (the record of published directories is process-wide and shared by the
    /// parallel tests), then empties the log.
    fn warm(root: &Path) {
        rightkit_fs::atomic::create_dir_all_durable(root, false).unwrap();
        let _ = take_steps();
    }

    #[test]
    fn every_created_ancestor_is_published_before_the_file_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let target = root.join("new/deep/settings.json");
        warm(&root);
        write(&target, with_parents(), b"x").unwrap();
        assert_eq!(
            take_steps(),
            vec![
                Step::DirSynced(root.clone()),
                Step::DirSynced(root.join("new")),
                Step::TempFlushed,
                Step::Replace,
                Step::DirSynced(root.join("new/deep")),
            ],
            "each new directory's link is flushed, top down, before the file below it"
        );
    }

    #[test]
    fn a_failed_ancestor_sync_stops_the_write_and_the_retry_completes_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let target = root.join("first/second/settings.json");
        warm(&root);
        let guard = fail_dir_sync(std::io::ErrorKind::Other);
        let error = write(&target, with_parents(), b"x").expect_err("the sync failed");
        drop(guard);
        assert!(
            matches!(&error, FsError::DirSyncFailed { path, .. } if path == &root.join("first")),
            "{error:?}"
        );
        assert!(!target.exists());
        let _ = take_steps();
        // `first` exists now but its link was never flushed: the retry flushes it before it
        // creates anything below it.
        write(&target, with_parents(), b"x").unwrap();
        let steps = take_steps();
        assert_eq!(
            &steps[..2],
            [
                Step::DirSynced(root.clone()),
                Step::DirSynced(root.join("first"))
            ],
            "{steps:?}"
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"x");
    }

    /// A real `mkdir` in another process stands for a crashed earlier run or a racing thread: the
    /// directory exists, this process never published it, and nothing on disk says whether its
    /// parent was synced. The write that finds it must sync that link before writing below it.
    #[test]
    fn a_directory_made_by_another_process_is_published_before_the_file_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        warm(&root);
        let status = std::process::Command::new("mkdir")
            .arg(root.join("found"))
            .status()
            .unwrap();
        assert!(status.success());
        write(
            &root.join("found/inner/settings.json"),
            with_parents(),
            b"x",
        )
        .unwrap();
        assert_eq!(
            take_steps(),
            vec![
                Step::DirSynced(root.clone()),
                Step::DirSynced(root.join("found")),
                Step::TempFlushed,
                Step::Replace,
                Step::DirSynced(root.join("found/inner")),
            ],
            "the found directory's link is synced like the one this call makes"
        );
    }

    #[test]
    fn a_published_directory_is_not_synced_again() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        warm(&root);
        write(&root.join("a/b/one.json"), with_parents(), b"1").unwrap();
        let _ = take_steps();
        write(&root.join("a/b/two.json"), with_parents(), b"2").unwrap();
        assert_eq!(
            take_steps(),
            vec![
                Step::TempFlushed,
                Step::Replace,
                Step::DirSynced(root.join("a/b"))
            ],
            "only the write's own directory sync remains"
        );
    }

    #[test]
    fn a_non_durable_write_makes_parents_without_syncing() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a/b/cache.bin");
        let options = AtomicWriteOptions {
            durable: false,
            ..with_parents()
        };
        let _ = take_steps();
        write(&target, options, b"x").unwrap();
        assert_eq!(take_steps(), vec![Step::Replace]);
    }
}

// ------------------------------------ Windows: creation-time descriptor and fail-closed hardening

#[cfg(windows)]
mod windows {
    use super::*;
    use rightkit_fs::atomic::testing::on_harden;
    use rightkit_fs::atomic::HardenStage;
    use rightkit_fs::{Privacy, ReplaceMode};
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn GetNamedSecurityInfoW(
            path: *const u16,
            object_type: u32,
            information: u32,
            owner: *mut *mut c_void,
            group: *mut *mut c_void,
            dacl: *mut *mut c_void,
            sacl: *mut *mut c_void,
            descriptor: *mut *mut c_void,
        ) -> u32;
        fn ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor: *mut c_void,
            revision: u32,
            information: u32,
            out: *mut *mut u16,
            len: *mut u32,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }

    /// The DACL of `path` as SDDL, with the `AI` bookkeeping flag normalised away.
    fn dacl_sddl(path: &Path) -> String {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        unsafe {
            let mut descriptor = std::ptr::null_mut();
            let status = GetNamedSecurityInfoW(
                wide.as_ptr(),
                1,
                4,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut descriptor,
            );
            assert_eq!(status, 0, "GetNamedSecurityInfoW({})", path.display());
            let mut text = std::ptr::null_mut();
            assert_ne!(
                ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    descriptor,
                    1,
                    4,
                    &mut text,
                    std::ptr::null_mut()
                ),
                0
            );
            let len = (0..).take_while(|&i| *text.add(i) != 0).count();
            let sddl = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
            LocalFree(text.cast());
            LocalFree(descriptor);
            sddl.replacen("D:PAI", "D:P", 1)
        }
    }

    fn private(mode: ReplaceMode) -> AtomicWriteOptions {
        AtomicWriteOptions {
            private: true,
            replace: mode,
            ..AtomicWriteOptions::default()
        }
    }

    /// Owner-only: protected, exactly one ACE, none for a broad principal.
    fn assert_owner_only(sddl: &str, what: &str) {
        assert!(sddl.starts_with("D:P("), "{what} must be protected: {sddl}");
        assert_eq!(sddl.matches('(').count(), 1, "{what}: one ACE: {sddl}");
        for broad in [";;;WD)", ";;;AU)", ";;;BU)", ";;;IU)"] {
            assert!(
                !sddl.contains(broad),
                "{what} names a broad principal: {sddl}"
            );
        }
    }

    #[test]
    fn the_temp_carries_its_owner_only_descriptor_from_creation_and_is_never_restricted_later() {
        for mode in [
            ReplaceMode::Rename,
            ReplaceMode::MoveWriteThrough,
            ReplaceMode::ReplaceFile,
        ] {
            for existing in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let target = dir.path().join("secret.json");
                if existing {
                    std::fs::write(&target, b"old").unwrap();
                }
                let log: Rc<RefCell<Vec<String>>> = Rc::default();
                let (harden_log, replace_log) = (log.clone(), log.clone());
                let _harden = on_harden(move |call| {
                    harden_log.borrow_mut().push(format!("{:?}", call.stage));
                    None
                });
                let _replace = on_replace(move |call| {
                    // The descriptor the temp has at the instant it is published.
                    replace_log
                        .borrow_mut()
                        .push(format!("Replace {}", dacl_sddl(&call.temp)));
                    None
                });
                write(&target, private(mode), b"secret").unwrap();

                let log = log.borrow();
                let at_publication = log
                    .iter()
                    .find_map(|entry| entry.strip_prefix("Replace "))
                    .expect("the replace ran");
                assert_owner_only(at_publication, "the temp at publication");
                assert_eq!(
                    log[0], "TempCreate",
                    "{mode:?} existing={existing}: the descriptor is applied at creation"
                );
                assert_eq!(
                    log.iter().filter(|e| *e == "TempCreate").count(),
                    1,
                    "{mode:?}: and the temp is never restricted again"
                );
                // No step touches the destination: the temp's descriptor travels with it.
                assert_eq!(log.len(), 2, "{mode:?}: creation, then the replace only");
                assert_owner_only(&dacl_sddl(&target), "the published file");
                assert_eq!(std::fs::read(&target).unwrap(), b"secret");
            }
        }
    }

    #[test]
    fn a_failed_owner_only_creation_publishes_nothing_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("secret.json");
        std::fs::write(&target, b"old").unwrap();
        let _ = take_steps();
        let result = {
            let _harden = on_harden(|call| {
                (call.stage == HardenStage::TempCreate)
                    .then(|| std::io::Error::other("injected ACL failure"))
            });
            write(&target, private(ReplaceMode::Rename), b"secret")
        };
        let error = result.expect_err("a private write must fail closed");
        assert!(!error.target_replaced(), "{error:?}");
        assert!(
            error.to_string().contains("injected ACL failure"),
            "{error}"
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        assert_eq!(names(dir.path()), vec!["secret.json".to_string()]);
        assert!(
            !take_steps().contains(&Step::Replace),
            "nothing reached the replace call"
        );
    }

    /// A private `ReplaceFile` write over a broad destination is published by a move, so the new
    /// secret never takes the destination's ACL, at any instant.
    #[test]
    fn a_private_replace_file_write_over_a_broad_destination_is_a_move_and_ends_owner_only() {
        use rightkit_fs::atomic::ReplaceApi;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("secret.json");
        std::fs::write(&target, b"old").unwrap();
        let seen: Rc<RefCell<Vec<ReplaceApi>>> = Rc::default();
        let sink = seen.clone();
        let _replace = on_replace(move |call| {
            sink.borrow_mut().push(call.api);
            None
        });
        write(&target, private(ReplaceMode::ReplaceFile), b"secret").unwrap();
        assert_eq!(*seen.borrow(), vec![ReplaceApi::MoveFileEx]);
        assert_owner_only(&dacl_sddl(&target), "the published file");
        assert_eq!(std::fs::read(&target).unwrap(), b"secret");
        assert_eq!(names(dir.path()), vec!["secret.json".to_string()]);
    }

    #[test]
    fn inherit_privacy_takes_no_owner_only_step() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("plain.json");
        let stages: Rc<RefCell<Vec<HardenStage>>> = Rc::default();
        let sink = stages.clone();
        let _harden = on_harden(move |call| {
            sink.borrow_mut().push(call.stage);
            None
        });
        let options = AtomicWriteOptions {
            privacy: Privacy::OwnerOnlyUnixInheritWindows,
            ..AtomicWriteOptions::default()
        };
        write(&target, options, b"x").unwrap();
        assert!(stages.borrow().is_empty());
    }
}
