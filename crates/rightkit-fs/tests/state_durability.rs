//! StateDir durability, naming, listing and options (ledger row 111). Fault injection uses the
//! `testing` feature's `state::testing` seam.

use rightkit_fs::{FsError, StateDir};
use std::path::PathBuf;

fn tmp() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().canonicalize().unwrap();
    (dir, path)
}

// ------------------------------------------------------------ 1: directory sync propagates

#[cfg(all(unix, feature = "testing"))]
mod dir_sync {
    use super::*;
    use rightkit_fs::state::testing::{dir_sync_count, fail_dir_sync};
    use std::io::ErrorKind;

    #[test]
    fn write_reports_a_failed_directory_sync_and_the_content_is_in_place() {
        let (_g, dir) = tmp();
        let state = StateDir::create(&dir.join("s")).unwrap();
        let guard = fail_dir_sync(ErrorKind::Other);
        let err = state.write("a", b"new").expect_err("directory sync failed");
        drop(guard);
        assert!(
            matches!(err, FsError::ReplacedButDirSyncFailed { .. }),
            "{err:?}"
        );
        assert!(err.target_replaced());
        assert_eq!(state.read("a", 10).unwrap(), b"new");
    }

    #[test]
    fn write_new_reports_a_failed_directory_sync() {
        let (_g, dir) = tmp();
        let state = StateDir::create(&dir.join("s")).unwrap();
        let guard = fail_dir_sync(ErrorKind::Other);
        let err = state
            .write_new("a", b"new")
            .expect_err("directory sync failed");
        drop(guard);
        assert!(
            matches!(err, FsError::ReplacedButDirSyncFailed { .. }),
            "{err:?}"
        );
        assert_eq!(state.read("a", 10).unwrap(), b"new");
    }

    #[test]
    fn remove_reports_a_failed_directory_sync_and_the_entry_is_gone() {
        let (_g, dir) = tmp();
        let state = StateDir::create(&dir.join("s")).unwrap();
        state.write("a", b"x").unwrap();
        let guard = fail_dir_sync(ErrorKind::Other);
        let err = state.remove("a").expect_err("directory sync failed");
        drop(guard);
        assert!(matches!(err, FsError::DirSyncFailed { .. }), "{err:?}");
        assert!(!state.exists("a").unwrap());
        // Removing something that is not there changes nothing, so there is nothing to sync.
        let guard = fail_dir_sync(ErrorKind::Other);
        assert!(!state.remove("a").unwrap());
        drop(guard);
    }

    #[test]
    fn creating_a_child_reports_a_failed_directory_sync_and_a_retry_publishes_it() {
        let (_g, dir) = tmp();
        let state = StateDir::create(&dir.join("s")).unwrap();
        let guard = fail_dir_sync(ErrorKind::Other);
        let err = state
            .child("kid")
            .map(|_| ())
            .expect_err("directory sync failed");
        assert!(matches!(err, FsError::DirSyncFailed { .. }), "{err:?}");
        // The directory exists now, but its link was never published: a retry must not accept
        // it as if it were, so with the sync still failing it fails again.
        let again = state
            .child("kid")
            .map(|_| ())
            .expect_err("the unpublished link is synced on retry, and that still fails");
        assert!(matches!(again, FsError::DirSyncFailed { .. }), "{again:?}");
        drop(guard);
        // Once the sync works the retry succeeds, and opening it again syncs nothing.
        state.child("kid").expect("retry publishes the link");
        let before = dir_sync_count();
        state.child("kid").expect("a published child opens plainly");
        assert_eq!(dir_sync_count(), before, "no sync for a published child");
    }

    #[test]
    fn a_successful_write_syncs_the_directory_once() {
        let (_g, dir) = tmp();
        let state = StateDir::create(&dir.join("s")).unwrap();
        let before = dir_sync_count();
        state.write("a", b"x").unwrap();
        assert_eq!(dir_sync_count() - before, 1);
    }
}

// ------------------------------------------------------------ 3: names

#[cfg(unix)]
#[test]
fn an_existing_name_over_the_write_cap_still_lists_reads_and_removes() {
    let (_g, dir) = tmp();
    let state = StateDir::create(&dir.join("s")).unwrap();
    let long = "n".repeat(230);
    std::fs::write(dir.join("s").join(&long), b"on-disk").unwrap();
    state.write("short", b"x").unwrap();
    let mut names = state.list(10).unwrap();
    names.sort();
    assert_eq!(names, vec![long.clone(), "short".to_string()]);
    assert_eq!(state.read(&long, 100).unwrap(), b"on-disk");
    assert!(state.exists(&long).unwrap());
    assert!(state.remove(&long).unwrap());
    // The write cap still holds by default.
    assert!(matches!(
        state.write(&"n".repeat(230), b"x"),
        Err(FsError::BadName(_))
    ));
}

#[cfg(unix)]
#[test]
fn the_write_cap_is_configurable_up_to_the_platform_limit() {
    use rightkit_fs::StateOptions;
    let (_g, dir) = tmp();
    let wide = StateOptions {
        max_name_len: Some(255),
        ..StateOptions::default()
    };
    let state = StateDir::create_with(&dir.join("s"), &wide).unwrap();
    let name = "w".repeat(255);
    state.write(&name, b"wide").unwrap();
    state.write_new(&"x".repeat(250), b"x").unwrap();
    assert_eq!(state.read(&name, 10).unwrap(), b"wide");
    assert_eq!(state.list(10).unwrap().len(), 2, "no temp leftovers");
    assert!(matches!(
        state.write(&"w".repeat(256), b"x"),
        Err(FsError::BadName(_))
    ));
    // A cap above the platform limit is clamped to it.
    let silly = StateOptions {
        max_name_len: Some(10_000),
        ..StateOptions::default()
    };
    let state = StateDir::open_with(&dir.join("s"), &silly).unwrap();
    assert!(matches!(
        state.write(&"w".repeat(256), b"x"),
        Err(FsError::BadName(_))
    ));
    // A narrow cap applies to writes only.
    let narrow = StateOptions {
        max_name_len: Some(5),
        ..StateOptions::default()
    };
    let state = StateDir::open_with(&dir.join("s"), &narrow).unwrap();
    assert!(matches!(
        state.write("sixsix", b"x"),
        Err(FsError::BadName(_))
    ));
    assert_eq!(state.read(&name, 10).unwrap(), b"wide");
}

// ------------------------------------------------------------ 4: sync level and policy

#[test]
fn durable_sync_with_never_reports_a_weaker_level_than_it_reached() {
    use rightkit_fs::{durable_sync_with, SyncLevel, SyncPolicy};
    let (_g, dir) = tmp();
    let file = std::fs::File::create(dir.join("f")).unwrap();
    // Strict is the strongest flush or an error, never a weaker success.
    if let Ok(level) = durable_sync_with(&file, SyncPolicy::Strict) {
        assert_eq!(level, SyncLevel::Full);
    }
    durable_sync_with(&file, SyncPolicy::Fallback).unwrap();
}

#[cfg(feature = "testing")]
mod policy {
    use super::*;
    use rightkit_fs::state::testing::without_full_sync;
    use rightkit_fs::{StateOptions, SyncLevel, SyncPolicy};

    #[test]
    fn fallback_reports_the_weaker_level_it_settled_for() {
        let (_g, dir) = tmp();
        let state = StateDir::create(&dir.join("s")).unwrap();
        let _no_full = without_full_sync();
        let level = state.write_reporting("a", b"x").unwrap();
        assert!(level < SyncLevel::Full, "{level:?}");
        assert!(state.write_new_reporting("b", b"x").unwrap() < SyncLevel::Full);
        assert_eq!(state.read("a", 10).unwrap(), b"x");
    }

    #[test]
    fn strict_refuses_to_publish_when_the_full_flush_is_unavailable() {
        let (_g, dir) = tmp();
        let strict = StateOptions {
            sync: SyncPolicy::Strict,
            ..StateOptions::default()
        };
        let state = StateDir::create_with(&dir.join("s"), &strict).unwrap();
        {
            let _no_full = without_full_sync();
            assert!(state.write("a", b"x").is_err());
            assert!(state.write_new("b", b"x").is_err());
        }
        assert!(
            state.list(10).unwrap().is_empty(),
            "nothing published, no temp left"
        );
        state
            .write("a", b"x")
            .expect("strict works when the flush is available");
    }
}

// ------------------------------------------------------------ 5: listing with kinds

#[test]
fn list_entries_returns_kinds_without_a_stat_per_entry() {
    use rightkit_fs::{DirEntry, EntryKind};
    let (_g, dir) = tmp();
    let state = StateDir::create(&dir.join("s")).unwrap();
    state.write("file", b"x").unwrap();
    state.child("sub").unwrap();
    for i in 0..40 {
        state.write(&format!("bulk-{i}"), b"x").unwrap();
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("file", dir.join("s/link")).unwrap();
        let _socket = std::os::unix::net::UnixListener::bind(dir.join("s/sock")).unwrap();
    }
    #[cfg(feature = "testing")]
    let stats_before = rightkit_fs::state::testing::fallback_stat_count();
    let entries = state.list_entries(100).unwrap();
    #[cfg(feature = "testing")]
    assert_eq!(
        rightkit_fs::state::testing::fallback_stat_count(),
        stats_before,
        "this filesystem reports d_type, so no entry needs a stat"
    );
    let kind = |name: &str| {
        entries
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("{name} missing from {entries:?}"))
            .kind
    };
    assert_eq!(kind("file"), EntryKind::File);
    assert_eq!(kind("sub"), EntryKind::Dir);
    assert_eq!(kind("bulk-7"), EntryKind::File);
    #[cfg(unix)]
    {
        assert_eq!(kind("link"), EntryKind::Symlink);
        assert_eq!(kind("sock"), EntryKind::Other);
    }
    assert!(entries.iter().all(|e| e.name != "." && e.name != ".."));
    // `list` is the names of the same entries.
    let mut names: Vec<String> = entries.iter().map(|e| e.name.clone()).collect();
    let mut listed = state.list(100).unwrap();
    names.sort();
    listed.sort();
    assert_eq!(names, listed);
    assert!(matches!(
        state.list_entries(3),
        Err(FsError::TooManyEntries(3))
    ));
    let _: Option<DirEntry> = entries.into_iter().next();
}

// ------------------------------------------------------------ 2 and 7: options

#[test]
fn a_directory_renamed_while_open_keeps_receiving_writes() {
    use rightkit_fs::StateOptions;
    let (_g, dir) = tmp();
    let options = StateOptions {
        allow_rename_while_open: true,
        ..StateOptions::default()
    };
    let path = dir.join("cleanup");
    let state = StateDir::create_with(&path, &options).unwrap();
    state.write("before", b"1").unwrap();
    let moved = dir.join("cleanup-canonical");
    std::fs::rename(&path, &moved).expect("rename while open is allowed by the option");
    std::fs::create_dir(&path).unwrap();
    // Handle-relative operations follow the original directory, not the path.
    state.write("after", b"2").unwrap();
    assert_eq!(state.read("before", 10).unwrap(), b"1");
    assert_eq!(std::fs::read(moved.join("after")).unwrap(), b"2");
    assert!(std::fs::read_dir(&path).unwrap().next().is_none());
    assert!(state.remove("before").unwrap());
    assert!(!moved.join("before").exists());
}

#[test]
fn the_privacy_option_leaves_unix_directories_owner_only() {
    use rightkit_fs::{Privacy, StateOptions};
    let (_g, dir) = tmp();
    for privacy in [
        Privacy::OwnerOnly,
        Privacy::OwnerOnlyUnixInheritWindows,
        Privacy::Inherit,
    ] {
        let options = StateOptions {
            privacy,
            ..StateOptions::default()
        };
        let path = dir.join(format!("{privacy:?}/deep"));
        let state = StateDir::create_with(&path, &options).unwrap();
        state.write("f", b"x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(path.clone()), 0o700, "{privacy:?}");
            assert_eq!(mode(path.join("f")), 0o600, "{privacy:?}");
        }
    }
}

// ------------------------------------------------------------ 6: read limits

#[test]
fn read_with_no_limit_returns_the_content_instead_of_overflowing() {
    let (_g, dir) = tmp();
    let state = StateDir::create(&dir.join("s")).unwrap();
    state.write("record", b"unlimited").unwrap();
    // `u64::MAX` is "no limit": the one-byte-over probe must saturate, not wrap to a zero-byte
    // read (release) or panic (debug).
    assert_eq!(state.read("record", u64::MAX).unwrap(), b"unlimited");
    assert_eq!(
        state.read_opt("record", u64::MAX).unwrap().unwrap(),
        b"unlimited"
    );
    state.write("empty", b"").unwrap();
    assert_eq!(state.read("empty", u64::MAX).unwrap(), b"");
}

#[test]
fn read_limits_are_exact_at_the_boundary() {
    let (_g, dir) = tmp();
    let state = StateDir::create(&dir.join("s")).unwrap();
    state.write("record", b"12345").unwrap();
    assert_eq!(
        state.read("record", 5).unwrap(),
        b"12345",
        "exactly at the limit"
    );
    assert_eq!(state.read("record", u64::MAX - 1).unwrap(), b"12345");
    for limit in [0, 4] {
        assert!(
            matches!(
                state.read("record", limit),
                Err(FsError::TooLarge { size: 5, limit: l, .. }) if l == limit
            ),
            "limit {limit}"
        );
    }
}

// ------------------------------------------------------------ 7: concurrent listings

#[test]
fn concurrent_listings_each_see_every_entry() {
    // Enough names that one `getdents` buffer cannot hold them all, so a listing needs several
    // reads of its directory stream. Listings that shared a file offset (duplicated descriptors)
    // would consume each other's position and come back short.
    const ENTRIES: usize = 4000;
    let (_g, dir) = tmp();
    let state = StateDir::create(&dir.join("s")).unwrap();
    for i in 0..ENTRIES {
        std::fs::write(
            dir.join("s")
                .join(format!("entry-{i:06}-padding-to-lengthen-names")),
            b"",
        )
        .unwrap();
    }
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..4)
            .map(|worker| {
                let state = &state;
                scope.spawn(move || {
                    for round in 0..25 {
                        let names = state.list(usize::MAX).unwrap();
                        assert_eq!(
                            names.len(),
                            ENTRIES,
                            "worker {worker} round {round}: a concurrent listing came back partial"
                        );
                        let entries = state.list_entries(usize::MAX).unwrap();
                        assert_eq!(entries.len(), ENTRIES, "worker {worker} round {round}");
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
    });
}
