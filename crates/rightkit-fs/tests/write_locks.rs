//! `KeyedWriteLocks`: one writer per destination, no permanent map, optional process lock file.

use rightkit_fs::{AtomicWriteOptions, FileLock, FsError, KeyedWriteLocks, WriteLockOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[test]
fn one_destination_is_exclusive_across_threads_and_spellings() {
    let dir = tempfile::tempdir().unwrap();
    let locks = KeyedWriteLocks::new();
    let inside = Arc::new(AtomicUsize::new(0));
    let most = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for n in 0..8 {
        let locks = locks.clone();
        let (inside, most) = (Arc::clone(&inside), Arc::clone(&most));
        // Two spellings of one destination whose leaf does not exist yet.
        let path = if n % 2 == 0 {
            dir.path().join("a.txt")
        } else {
            dir.path().join(".").join("a.txt")
        };
        handles.push(std::thread::spawn(move || {
            for _ in 0..20 {
                locks
                    .with_destination(&path, |_| {
                        let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
                        most.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(1));
                        inside.fetch_sub(1, Ordering::SeqCst);
                    })
                    .unwrap();
            }
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
    assert_eq!(
        most.load(Ordering::SeqCst),
        1,
        "two writers held one destination at once"
    );
    assert_eq!(
        locks.active_destinations(),
        0,
        "the registry kept an entry nobody uses"
    );
}

#[test]
fn different_destinations_do_not_block_each_other() {
    let dir = tempfile::tempdir().unwrap();
    let locks = KeyedWriteLocks::new();
    let (a, b) = (dir.path().join("a.txt"), dir.path().join("b.txt"));
    let value = locks
        .with_destination(&a, |_| locks.try_with_destination(&b, |_| 7).unwrap())
        .unwrap();
    assert_eq!(value, 7);
    assert_eq!(locks.active_destinations(), 0);
}

#[test]
fn a_busy_destination_is_locked_when_not_waiting_and_times_out_when_waiting() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    let locks = KeyedWriteLocks::with_options(
        WriteLockOptions::default().timeout(Duration::from_millis(60)),
    );
    locks
        .with_destination(&path, |_| {
            let busy = locks.try_with_destination(&path, |_| ()).unwrap_err();
            assert!(matches!(busy, FsError::Locked { .. }), "{busy:?}");
            let waited = locks.with_destination(&path, |_| ()).unwrap_err();
            assert!(matches!(waited, FsError::LockTimeout { .. }), "{waited:?}");
            // The failed waiters left their references behind in no state: still one entry.
            assert_eq!(locks.active_destinations(), 1);
        })
        .unwrap();
    assert_eq!(
        locks.active_destinations(),
        0,
        "a timed-out waiter leaked its entry"
    );
    locks.try_with_destination(&path, |_| ()).unwrap();
}

#[test]
fn a_panic_in_the_operation_releases_the_lock_and_the_entry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    let locks = KeyedWriteLocks::new();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = locks.with_destination(&path, |_| panic!("boom"));
    }));
    assert!(outcome.is_err());
    assert_eq!(locks.active_destinations(), 0);
    locks.try_with_destination(&path, |_| ()).unwrap();
}

#[test]
fn an_unusable_destination_is_refused_before_anything_is_locked() {
    let dir = tempfile::tempdir().unwrap();
    let locks = KeyedWriteLocks::new();
    let missing_parent = locks
        .with_destination(&dir.path().join("missing").join("x"), |_| ())
        .unwrap_err();
    assert!(
        matches!(missing_parent, FsError::Io { .. }),
        "{missing_parent:?}"
    );
    std::fs::write(dir.path().join("file"), b"x").unwrap();
    let parent_is_file = locks
        .with_destination(&dir.path().join("file").join("x"), |_| ())
        .unwrap_err();
    assert!(
        matches!(parent_is_file, FsError::NotADirectory(_)),
        "{parent_is_file:?}"
    );
    let reserved = locks
        .with_destination(&dir.path().join(".x.rktmp-1-2-3"), |_| ())
        .unwrap_err();
    assert!(matches!(reserved, FsError::BadName(_)), "{reserved:?}");
    assert_eq!(locks.active_destinations(), 0);
}

#[cfg(unix)]
#[test]
fn a_symlinked_directory_reaches_the_same_key() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let locks = KeyedWriteLocks::new();
    locks
        .with_destination(&real.join("a.txt"), |_| {
            let busy = locks
                .try_with_destination(&link.join("a.txt"), |_| ())
                .unwrap_err();
            assert!(matches!(busy, FsError::Locked { .. }), "{busy:?}");
        })
        .unwrap();
}

#[cfg(any(windows, target_vendor = "apple"))]
#[test]
fn case_aliases_are_one_destination_where_the_platform_folds_case() {
    let dir = tempfile::tempdir().unwrap();
    let locks = KeyedWriteLocks::new();
    locks
        .with_destination(&dir.path().join("Report.TXT"), |_| {
            let busy = locks
                .try_with_destination(&dir.path().join("report.txt"), |_| ())
                .unwrap_err();
            assert!(matches!(busy, FsError::Locked { .. }), "{busy:?}");
        })
        .unwrap();
}

#[test]
fn the_process_lock_file_is_held_while_the_operation_runs() {
    let dir = tempfile::tempdir().unwrap();
    let locks = KeyedWriteLocks::with_options(
        WriteLockOptions::default().process_lock_dir(dir.path().join("locks")),
    );
    let destination = dir.path().join("a.txt");
    let lock_file = locks.lock_file_for(&destination).unwrap().unwrap();
    locks
        .with_destination(&destination, |_| {
            let busy = FileLock::try_acquire(&lock_file).unwrap_err();
            assert!(
                matches!(busy, FsError::Locked { .. } | FsError::LockTimeout { .. }),
                "{busy:?}"
            );
        })
        .unwrap();
    drop(FileLock::try_acquire(&lock_file).expect("released with the operation"));
    assert_eq!(locks.active_destinations(), 0);
}

#[test]
fn the_writer_commits_with_a_check_while_the_lock_is_held() {
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("doc.txt");
    std::fs::write(&destination, b"old").unwrap();
    let locks = KeyedWriteLocks::new();
    let saved = locks
        .with_destination(&destination, |held| -> Result<(), FsError> {
            let mut writer = held.atomic_writer(AtomicWriteOptions::default())?;
            writer.write_all(b"new")?;
            writer.commit_checked(|temp| {
                assert_eq!(std::fs::read(temp)?, b"new");
                Ok(())
            })
        })
        .unwrap();
    saved.unwrap();
    assert_eq!(std::fs::read(&destination).unwrap(), b"new");
}

// ---- a second real process ----

const HOLDER_ENV: &str = "RIGHTKIT_FS_TEST_WRITE_LOCK_HOLDER";

/// Child body: an ordinary passing test unless `HOLDER_ENV` (`<lock dir>|<destination>`) is set.
/// With it the child takes the keyed write lock for the destination through its own registry and
/// process lock directory, announces it and holds until its stdin closes or says anything.
#[test]
fn write_lock_holder_child() {
    let Ok(spec) = std::env::var(HOLDER_ENV) else {
        return;
    };
    let (lock_dir, destination) = spec.split_once('|').unwrap();
    let locks =
        KeyedWriteLocks::with_options(WriteLockOptions::default().process_lock_dir(lock_dir));
    locks
        .with_destination(Path::new(destination), |_| {
            println!("HELD");
            std::io::stdout().flush().unwrap();
            let mut line = String::new();
            let _ = std::io::stdin().read_line(&mut line);
        })
        .unwrap();
}

fn spawn_holder(lock_dir: &Path, destination: &Path) -> Child {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "write_lock_holder_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(
            HOLDER_ENV,
            format!("{}|{}", lock_dir.display(), destination.display()),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    while !line.contains("HELD") {
        line.clear();
        assert!(
            reader.read_line(&mut line).unwrap() > 0,
            "the holder exited before it held the lock"
        );
    }
    // Keep draining so the holder never blocks on a full pipe.
    std::thread::spawn(move || {
        let mut sink = String::new();
        while reader.read_line(&mut sink).unwrap_or(0) > 0 {
            sink.clear();
        }
    });
    child
}

#[test]
fn the_process_lock_excludes_a_writer_in_another_process() {
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("a.txt");
    let lock_dir = dir.path().join("locks");
    let mut holder = spawn_holder(&lock_dir, &destination);

    // This process has its own registry, so only the lock file can keep it out.
    let locks = KeyedWriteLocks::with_options(
        WriteLockOptions::default()
            .timeout(Duration::from_millis(100))
            .process_lock_dir(lock_dir.clone()),
    );
    let busy = locks.try_with_destination(&destination, |_| ()).unwrap_err();
    assert!(
        matches!(busy, FsError::Locked { .. } | FsError::LockTimeout { .. }),
        "{busy:?}"
    );
    let waited = locks.with_destination(&destination, |_| ()).unwrap_err();
    assert!(
        matches!(waited, FsError::Locked { .. } | FsError::LockTimeout { .. }),
        "{waited:?}"
    );
    assert_eq!(
        locks.active_destinations(),
        0,
        "a refused process lock leaked the in-process slot"
    );
    // Another destination is not held by anyone.
    locks
        .try_with_destination(&dir.path().join("b.txt"), |_| ())
        .unwrap();

    // The lock goes with the holder.
    drop(holder.stdin.take());
    holder.wait().unwrap();
    locks
        .try_with_destination(&destination, |_| ())
        .expect("free once the other process exited");
}

#[test]
fn a_killed_holder_does_not_leave_the_destination_locked() {
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("a.txt");
    let lock_dir = dir.path().join("locks");
    let mut holder = spawn_holder(&lock_dir, &destination);
    let locks = KeyedWriteLocks::with_options(
        WriteLockOptions::default().process_lock_dir(lock_dir.clone()),
    );
    assert!(locks.try_with_destination(&destination, |_| ()).is_err());
    holder.kill().unwrap();
    holder.wait().unwrap();
    locks
        .try_with_destination(&destination, |_| ())
        .expect("the operating system released the dead holder's lock");
}

#[test]
fn a_timeout_the_clock_cannot_add_waits_without_a_deadline_instead_of_panicking() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    let locks =
        KeyedWriteLocks::with_options(WriteLockOptions::default().timeout(Duration::MAX));
    assert_eq!(locks.with_destination(&path, |_| 5).unwrap(), 5);

    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let holder = {
        let (locks, path) = (locks.clone(), path.clone());
        std::thread::spawn(move || {
            locks
                .with_destination(&path, |_| {
                    held_tx.send(()).unwrap();
                    go_rx.recv_timeout(Duration::from_secs(30)).unwrap();
                })
                .unwrap();
        })
    };
    held_rx.recv_timeout(Duration::from_secs(30)).unwrap();
    // A zero wait is still an immediate refusal while the slot is held.
    let busy = locks.try_with_destination(&path, |_| ()).unwrap_err();
    assert!(matches!(busy, FsError::Locked { .. }), "{busy:?}");
    let waiter = {
        let (locks, path) = (locks.clone(), path.clone());
        std::thread::spawn(move || locks.with_destination(&path, |_| 7).unwrap())
    };
    std::thread::sleep(Duration::from_millis(100));
    assert!(!waiter.is_finished(), "the waiter did not wait");
    go_tx.send(()).unwrap();
    holder.join().unwrap();
    assert_eq!(waiter.join().unwrap(), 7);
    assert_eq!(locks.active_destinations(), 0);
}
