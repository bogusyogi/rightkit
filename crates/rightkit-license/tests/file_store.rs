//! `FileClockStore` and `CrossProcessLock`: the cross-process half of the anti-rollback
//! ledger (second Astra read 2026-10-11, finding 1). `ClockStore::compare_and_save` has no
//! default, and the only way for two processes to be atomic against each other is an OS
//! lock, so these tests use separate store handles on one path as separate processes: an
//! OS file lock arbitrates between handles exactly as it does between processes.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use rightkit_license::{
    AntiRollbackClock, ClockDecodeError, ClockReading, ClockRefusal, ClockState, ClockStore,
    CrossProcessLock, FileClockStore, FileClockStoreError, ProcessAnchor, TrustedClock,
};

fn state(highest_seen: i64, trusted_process: &str, last_server_iat: i64) -> ClockState {
    ClockState {
        highest_seen,
        trusted_process: trusted_process.to_string(),
        last_server_iat,
    }
}

/// A unique scratch directory removed on drop. Not the user's data and not a credential.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rightkit-license-file-store-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    /// A ledger path inside a directory that does not exist yet.
    fn ledger(&self) -> PathBuf {
        self.0.join("nested").join("ledger.json")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

#[test]
fn a_missing_ledger_is_none_and_a_saved_ledger_round_trips() {
    let dir = TempDir::new();
    let store = FileClockStore::new(dir.ledger());
    assert_eq!(store.load().unwrap(), None);

    store.save(&state(1_000, "a", 900)).unwrap();
    assert_eq!(store.load().unwrap(), Some(state(1_000, "a", 900)));
    // A second handle on the same path (another process) sees it.
    assert_eq!(
        FileClockStore::new(dir.ledger()).load().unwrap(),
        Some(state(1_000, "a", 900))
    );
    assert!(
        !with_suffix(&dir.ledger(), ".tmp").exists(),
        "the temporary file is renamed away"
    );
}

#[test]
fn the_record_on_disk_is_the_apps_json_layout() {
    let dir = TempDir::new();
    let store = FileClockStore::new(dir.ledger());
    store.save(&state(12_345, "p", 12_000)).unwrap();
    assert_eq!(
        fs::read_to_string(dir.ledger()).unwrap(),
        r#"{"v":1,"highest_seen":12345,"trusted_process":"p","last_server_iat":12000}"#
    );
    // And the legacy scalar form already stored by older installs is accepted.
    fs::write(dir.ledger(), "10000").unwrap();
    assert_eq!(store.load().unwrap(), Some(state(10_000, "legacy", 0)));
}

#[test]
fn a_damaged_ledger_is_an_error_and_never_a_fresh_start() {
    let dir = TempDir::new();
    let store = FileClockStore::new(dir.ledger());
    store.save(&state(1_000, "a", 0)).unwrap();
    fs::write(dir.ledger(), "not a ledger").unwrap();

    assert!(matches!(
        store.load(),
        Err(FileClockStoreError::Corrupt(ClockDecodeError::Malformed))
    ));
    // A compare-and-save against a damaged ledger must neither claim "no ledger" nor
    // overwrite it.
    assert!(store.compare_and_save(None, &state(2_000, "b", 0)).is_err());
    assert_eq!(fs::read_to_string(dir.ledger()).unwrap(), "not a ledger");
}

#[test]
fn compare_and_save_only_writes_when_the_ledger_still_equals_expected() {
    let dir = TempDir::new();
    let store = FileClockStore::new(dir.ledger());
    assert!(store.compare_and_save(None, &state(900, "a", 0)).unwrap());
    // `None` no longer matches.
    assert!(!store.compare_and_save(None, &state(5, "x", 0)).unwrap());
    assert_eq!(store.load().unwrap(), Some(state(900, "a", 0)));
    // A stale expectation is refused and writes nothing.
    assert!(!store
        .compare_and_save(Some(&state(800, "a", 0)), &state(1_000, "b", 0))
        .unwrap());
    assert_eq!(store.load().unwrap(), Some(state(900, "a", 0)));
    assert!(store
        .compare_and_save(Some(&state(900, "a", 0)), &state(1_000, "b", 0))
        .unwrap());
    assert_eq!(store.load().unwrap(), Some(state(1_000, "b", 0)));
}

/// The audit interleaving: A checks a ledger of 900, B saves 1100, A saves 1000. With the
/// ledger lock held by B while it writes, A's compare-and-save cannot run until B is done,
/// and then it sees 1100 and refuses. Without a cross-process lock A would be answered
/// straight away from the 900 it loaded.
#[test]
fn compare_and_save_waits_for_a_writer_in_another_process_and_then_sees_its_write() {
    let dir = TempDir::new();
    let path = dir.ledger();
    let process_a = FileClockStore::new(&path);
    process_a.save(&state(900, "p", 0)).unwrap();
    let stale = state(900, "p", 0);

    thread::scope(|scope| {
        // Process B is inside its write: it holds the ledger lock.
        let process_b_writing = CrossProcessLock::acquire(process_a.lock_path()).unwrap();

        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        scope.spawn(|| {
            started_tx.send(()).unwrap();
            let committed = process_a
                .compare_and_save(Some(&stale), &state(1_000, "a", 0))
                .unwrap();
            done_tx.send(committed).unwrap();
        });
        started_rx.recv().unwrap();

        assert!(
            matches!(
                done_rx.recv_timeout(Duration::from_millis(300)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "compare_and_save must wait while another process holds the ledger lock"
        );

        // B finishes its write (it holds the lock, so it writes the file directly) and
        // releases.
        fs::write(&path, state(1_100, "b", 0).encode()).unwrap();
        drop(process_b_writing);

        assert_eq!(
            done_rx.recv_timeout(Duration::from_secs(30)),
            Ok(false),
            "A planned against 900 and must be refused once the ledger is 1100"
        );
    });

    assert_eq!(
        FileClockStore::new(&path).load().unwrap(),
        Some(state(1_100, "b", 0)),
        "the trusted mark did not move backwards"
    );
}

#[test]
fn concurrent_compare_and_save_through_separate_handles_loses_no_update() {
    const THREADS: i64 = 6;
    const PER_THREAD: i64 = 25;
    let dir = TempDir::new();
    let path = dir.ledger();

    thread::scope(|scope| {
        for _ in 0..THREADS {
            let path = path.clone();
            scope.spawn(move || {
                // Each thread has its own handle, as each process would.
                let store = FileClockStore::new(path);
                for _ in 0..PER_THREAD {
                    loop {
                        let current = store.load().unwrap();
                        let next =
                            state(current.as_ref().map_or(0, |s| s.highest_seen) + 1, "p", 0);
                        if store.compare_and_save(current.as_ref(), &next).unwrap() {
                            break;
                        }
                    }
                }
            });
        }
    });

    assert_eq!(
        FileClockStore::new(&path)
            .load()
            .unwrap()
            .unwrap()
            .highest_seen,
        THREADS * PER_THREAD,
        "a lost update would leave the count short"
    );
}

#[test]
fn clocks_in_two_processes_cannot_move_a_file_ledger_backwards() {
    let dir = TempDir::new();
    let path = dir.ledger();
    FileClockStore::new(&path)
        .save(&state(900, "seed", 0))
        .unwrap();

    let store_a = FileClockStore::new(&path);
    let store_b = FileClockStore::new(&path);
    let a = AntiRollbackClock::new(&store_a, ProcessAnchor::with_id("a", 1_000));
    let b = AntiRollbackClock::new(&store_b, ProcessAnchor::with_id("b", 1_100));
    let reading_a = a.reconcile(1_000, true).unwrap();
    let reading_b = b.reconcile(1_100, true).unwrap();
    b.commit(&reading_b).unwrap();
    a.commit(&reading_a).unwrap();
    assert_eq!(store_a.load().unwrap(), Some(state(1_100, "b", 0)));

    let store_c = FileClockStore::new(&path);
    let restarted = AntiRollbackClock::new(&store_c, ProcessAnchor::with_id("c", 1_001));
    assert_eq!(
        restarted.reconcile(1_001, true).unwrap(),
        ClockReading::Rollback {
            highest_seen: 1_100
        }
    );
}

#[test]
fn a_file_backed_clock_serves_the_trusted_hook_across_a_restart() {
    let dir = TempDir::new();
    let path = dir.ledger();
    let first_store = FileClockStore::new(&path);
    let first = AntiRollbackClock::new(&first_store, ProcessAnchor::with_id("a", 10_000));
    assert_eq!(first.trusted_now(10_000, false), Ok(10_000));
    assert_eq!(
        FileClockStore::new(&path).load().unwrap(),
        Some(state(10_000, "a", 0))
    );

    let second_store = FileClockStore::new(&path);
    let second = AntiRollbackClock::new(&second_store, ProcessAnchor::with_id("b", 3_000));
    assert_eq!(
        second.trusted_now(3_000, true),
        Err(ClockRefusal::Rollback {
            highest_seen: 10_000
        })
    );
    assert_eq!(second.trusted_now(10_500, true), Ok(10_500));
}

#[test]
fn the_cross_process_lock_excludes_every_other_handle_until_it_is_dropped() {
    let dir = TempDir::new();
    let lock_path = dir.0.join("nested").join("ledger.lock");
    let held = CrossProcessLock::acquire(&lock_path).unwrap();

    let other = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    assert!(matches!(
        other.try_lock(),
        Err(fs::TryLockError::WouldBlock)
    ));
    drop(held);
    other.try_lock().unwrap();
}

#[test]
fn a_store_reports_an_unusable_lock_path_instead_of_writing_unlocked() {
    let dir = TempDir::new();
    // The ledger's parent is a regular file, so neither the lock nor the ledger can exist.
    let blocker = dir.0.join("blocker");
    fs::write(&blocker, "x").unwrap();
    let store = FileClockStore::new(blocker.join("ledger.json"));
    assert!(matches!(
        store.save(&state(1, "a", 0)),
        Err(FileClockStoreError::Io(_))
    ));
    assert!(store.compare_and_save(None, &state(1, "a", 0)).is_err());
    assert!(store.load().is_err());
}
