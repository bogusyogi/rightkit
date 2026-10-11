//! `FileClockStore` and `CrossProcessLock`: the cross-process half of the anti-rollback
//! ledger (second Astra read 2026-10-11, finding 1). `ClockStore::compare_and_save` has no
//! default, and the only way for two processes to be atomic against each other is an OS
//! lock. Most tests use separate store handles on one path (an OS file lock arbitrates between
//! handles exactly as it does between processes); the interleaving test runs real child
//! processes.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use rightkit_license::{
    AntiRollbackClock, ClockDecodeError, ClockReading, ClockRefusal, ClockState, ClockStore,
    ClosureClockStore, ClosureClockStoreError, CrossProcessLock, FileClockStore,
    FileClockStoreError, LockFailure, LockWait, ProcessAnchor, TrustedClock,
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
        // Shared borrows for the thread; the senders move in so they live as long as it does.
        let store = &process_a;
        let stale_ref = &stale;
        scope.spawn(move || {
            started_tx.send(()).unwrap();
            let committed = store
                .compare_and_save(Some(stale_ref), &state(1_000, "a", 0))
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

const CHILD_ROLE_ENV: &str = "RK_LICENSE_TEST_CHILD_ROLE";
const CHILD_DIR_ENV: &str = "RK_LICENSE_TEST_CHILD_DIR";
const CHILD_TEST: &str = "clock_child_process";

fn wait_for_file(path: &Path) {
    let give_up = Instant::now() + Duration::from_secs(30);
    while !path.exists() {
        assert!(
            Instant::now() < give_up,
            "timed out waiting for {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

/// Body of one child process of `clocks_in_two_processes_...`. When the test binary is run
/// normally (no role in the environment) this is a no-op that passes.
#[test]
fn clock_child_process() {
    let Ok(role) = std::env::var(CHILD_ROLE_ENV) else {
        return;
    };
    let dir = PathBuf::from(std::env::var(CHILD_DIR_ENV).unwrap());
    let store = FileClockStore::new(dir.join("nested").join("ledger.json"));
    match role.as_str() {
        // Reads the ledger, then commits only after the other process has committed.
        "a" => {
            let clock = AntiRollbackClock::new(&store, ProcessAnchor::with_id("a", 1_000));
            let reading = clock.reconcile(1_000, true).unwrap();
            fs::write(dir.join("a.reconciled"), "x").unwrap();
            wait_for_file(&dir.join("b.committed"));
            clock.commit(&reading).unwrap();
        }
        // Reads and commits entirely between A's read and A's commit.
        "b" => {
            wait_for_file(&dir.join("a.reconciled"));
            let clock = AntiRollbackClock::new(&store, ProcessAnchor::with_id("b", 1_100));
            let reading = clock.reconcile(1_100, true).unwrap();
            clock.commit(&reading).unwrap();
            fs::write(dir.join("b.committed"), "x").unwrap();
        }
        other => panic!("unknown child role {other}"),
    }
}

fn spawn_child(role: &str, dir: &Path) -> std::process::Child {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads=1"])
        .env(CHILD_ROLE_ENV, role)
        .env(CHILD_DIR_ENV, dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap()
}

#[test]
fn clocks_in_two_processes_cannot_move_a_file_ledger_backwards() {
    let dir = TempDir::new();
    let path = dir.ledger();
    FileClockStore::new(&path)
        .save(&state(900, "seed", 0))
        .unwrap();

    // Two real child processes (this test binary re-run on `clock_child_process`) interleave:
    // A reads, B reads and commits, then A commits its stale reading.
    let a = spawn_child("a", &dir.0);
    let b = spawn_child("b", &dir.0);
    for (role, child) in [("a", a), ("b", b)] {
        let out = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "child {role} failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            stdout.contains(&format!("{CHILD_TEST} ... ok")),
            "child {role} did not run its test body:\n{stdout}"
        );
    }

    assert_eq!(
        FileClockStore::new(&path).load().unwrap(),
        Some(state(1_100, "b", 0)),
        "A's stale commit moved the mark backwards or B's commit was lost"
    );
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

// ------------------------------------------------------------------ ClosureClockStore

/// A "keychain" the closures read and write: one record behind a mutex, like an item.
#[derive(Default)]
struct FakeKeychain(std::sync::Mutex<Option<String>>);

impl FakeKeychain {
    fn get(&self) -> Result<Option<ClockState>, String> {
        match self.0.lock().unwrap().as_deref() {
            Some(raw) => ClockState::decode(raw).map(Some).map_err(|e| e.to_string()),
            None => Ok(None),
        }
    }
    fn set(&self, state: &ClockState) -> Result<(), String> {
        *self.0.lock().unwrap() = Some(state.encode());
        Ok(())
    }
}

#[test]
fn a_closure_store_round_trips_and_compares_before_it_writes() {
    let dir = TempDir::new();
    let keychain = FakeKeychain::default();
    let store = ClosureClockStore::new(
        dir.0.join("nested").join("clock.lock"),
        || keychain.get(),
        |state: &ClockState| keychain.set(state),
    );
    assert_eq!(store.load().unwrap(), None);
    assert!(store.compare_and_save(None, &state(900, "a", 0)).unwrap());
    // `None` no longer matches, and a stale expectation writes nothing.
    assert!(!store.compare_and_save(None, &state(5, "x", 0)).unwrap());
    assert!(!store
        .compare_and_save(Some(&state(800, "a", 0)), &state(1_000, "b", 0))
        .unwrap());
    assert_eq!(store.load().unwrap(), Some(state(900, "a", 0)));
    assert!(store
        .compare_and_save(Some(&state(900, "a", 0)), &state(1_000, "b", 0))
        .unwrap());
    store.save(&state(2_000, "c", 0)).unwrap();
    assert_eq!(keychain.get().unwrap(), Some(state(2_000, "c", 0)));
}

#[test]
fn closure_failures_are_errors_and_never_an_empty_ledger() {
    let dir = TempDir::new();
    let store = ClosureClockStore::new(
        dir.0.join("clock.lock"),
        || Err::<Option<ClockState>, _>("keychain locked".to_string()),
        |_: &ClockState| Err("keychain locked".to_string()),
    );
    assert!(matches!(
        store.load(),
        Err(ClosureClockStoreError::Store(ref e)) if e == "keychain locked"
    ));
    assert!(store.compare_and_save(None, &state(1, "a", 0)).is_err());
    assert!(store.save(&state(1, "a", 0)).is_err());
}

#[test]
fn a_closure_store_with_an_unusable_lock_path_does_not_run_the_closures() {
    let dir = TempDir::new();
    let blocker = dir.0.join("blocker");
    fs::write(&blocker, "x").unwrap();
    let calls = std::cell::Cell::new(0_u32);
    let store = ClosureClockStore::new(
        blocker.join("clock.lock"),
        || {
            calls.set(calls.get() + 1);
            Ok::<_, String>(None)
        },
        |_: &ClockState| {
            calls.set(calls.get() + 1);
            Ok(())
        },
    );
    assert!(matches!(store.load(), Err(ClosureClockStoreError::Lock(_))));
    assert!(matches!(
        store.compare_and_save(None, &state(1, "a", 0)),
        Err(ClosureClockStoreError::Lock(_))
    ));
    assert_eq!(calls.get(), 0, "no read or write happened without the lock");
}

/// The point of the lock: a closure-backed store waits for another holder of the same lock
/// file (another process) and then compares against what that holder wrote.
#[test]
fn a_closure_store_waits_for_the_cross_process_lock_and_then_sees_the_other_write() {
    let dir = TempDir::new();
    let lock_path = dir.0.join("nested").join("clock.lock");
    let keychain = FakeKeychain::default();
    keychain.set(&state(900, "p", 0)).unwrap();
    let store = ClosureClockStore::new(
        &lock_path,
        || keychain.get(),
        |state: &ClockState| keychain.set(state),
    );
    let stale = state(900, "p", 0);

    thread::scope(|scope| {
        // Another process is inside its write: it holds the lock.
        let other_process = CrossProcessLock::acquire(&lock_path).unwrap();

        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let store = &store;
        let stale = &stale;
        scope.spawn(move || {
            started_tx.send(()).unwrap();
            let committed = store
                .compare_and_save(Some(stale), &state(1_000, "a", 0))
                .unwrap();
            done_tx.send(committed).unwrap();
        });
        started_rx.recv().unwrap();
        assert!(
            matches!(
                done_rx.recv_timeout(Duration::from_millis(300)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "compare_and_save must wait while another process holds the lock"
        );

        keychain.set(&state(1_100, "b", 0)).unwrap();
        drop(other_process);
        assert_eq!(done_rx.recv_timeout(Duration::from_secs(30)), Ok(false));
    });
    assert_eq!(keychain.get().unwrap(), Some(state(1_100, "b", 0)));
}

// ------------------------------------------------------------------ lock failure policy

use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

/// A lock path that cannot be opened: its parent "directory" is a regular file.
fn unopenable_lock(dir: &TempDir) -> PathBuf {
    let blocker = dir.0.join("not-a-directory");
    fs::write(&blocker, b"file").unwrap();
    blocker.join("clock.lock")
}

type Cell = Arc<std::sync::Mutex<Option<ClockState>>>;
// The two closures are boxed so the store has a nameable type.
type MemoryRead = Box<dyn Fn() -> Result<Option<ClockState>, String>>;
type MemoryWrite = Box<dyn Fn(&ClockState) -> Result<(), String>>;
type MemoryStore = ClosureClockStore<MemoryRead, MemoryWrite>;

fn memory_store(lock: PathBuf, cell: &Cell) -> MemoryStore {
    let (read, write) = (Arc::clone(cell), Arc::clone(cell));
    let get: MemoryRead = Box::new(move || -> Result<Option<ClockState>, String> {
        Ok(read.lock().unwrap().clone())
    });
    let set: MemoryWrite = Box::new(move |s: &ClockState| -> Result<(), String> {
        *write.lock().unwrap() = Some(s.clone());
        Ok(())
    });
    ClosureClockStore::new(lock, get, set)
}

#[test]
fn an_unopenable_lock_fails_by_default() {
    let dir = TempDir::new();
    let cell: Cell = Arc::default();
    let store = memory_store(unopenable_lock(&dir), &cell);
    assert!(matches!(store.load(), Err(ClosureClockStoreError::Lock(_))));
    assert!(matches!(
        store.save(&state(1, "a", 1)),
        Err(ClosureClockStoreError::Lock(_))
    ));
    assert!(cell.lock().unwrap().is_none(), "nothing was written");
    assert!(!store.is_degraded());
}

#[test]
fn an_unopenable_lock_degrades_when_asked_and_reports_once() {
    let dir = TempDir::new();
    let cell: Cell = Arc::default();
    let reports = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&reports);
    let store = memory_store(unopenable_lock(&dir), &cell)
        .with_lock_failure(LockFailure::Degrade)
        .with_degraded_report(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        });

    assert_eq!(store.load().unwrap(), None);
    store.save(&state(5, "a", 4)).unwrap();
    assert_eq!(store.load().unwrap(), Some(state(5, "a", 4)));
    assert!(store
        .compare_and_save(Some(&state(5, "a", 4)), &state(6, "a", 5))
        .unwrap());
    assert_eq!(*cell.lock().unwrap(), Some(state(6, "a", 5)));
    assert_eq!(
        reports.load(Ordering::SeqCst),
        1,
        "reported once, not per call"
    );
    assert!(store.is_degraded());
}

#[test]
fn a_held_lock_is_never_bypassed_under_degrade() {
    let dir = TempDir::new();
    let lock_path = dir.0.join("clock.lock");
    let called = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&called);
    let store = ClosureClockStore::new(
        lock_path.clone(),
        move || {
            seen.fetch_add(1, Ordering::SeqCst);
            Ok::<_, String>(None)
        },
        |_: &ClockState| Ok(()),
    )
    .with_lock_failure(LockFailure::Degrade)
    .with_lock_wait(LockWait::DoNotWait);

    let held = CrossProcessLock::acquire(&lock_path).unwrap();
    match store.load() {
        Err(ClosureClockStoreError::Lock(error)) => {
            assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock)
        }
        other => panic!("expected a held-lock error, got {other:?}"),
    }
    assert_eq!(called.load(Ordering::SeqCst), 0, "the closure never ran");
    assert!(!store.is_degraded(), "a held lock is not a degradation");
    drop(held);
    assert_eq!(store.load().unwrap(), None);
    assert_eq!(called.load(Ordering::SeqCst), 1);
}

#[test]
fn a_held_lock_still_blocks_by_default_under_degrade() {
    let dir = TempDir::new();
    let lock_path = dir.0.join("clock.lock");
    let held = CrossProcessLock::acquire(&lock_path).unwrap();
    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let store =
            ClosureClockStore::new(lock_path, || Ok::<_, String>(None), |_: &ClockState| Ok(()))
                .with_lock_failure(LockFailure::Degrade);
        tx.send(store.load().is_ok()).unwrap();
    });
    assert!(
        rx.recv_timeout(Duration::from_millis(300)).is_err(),
        "the load waits while another handle holds the lock"
    );
    drop(held);
    assert!(rx.recv_timeout(Duration::from_secs(10)).unwrap());
    worker.join().unwrap();
}

#[test]
fn try_acquire_reports_a_held_lock_as_none() {
    let dir = TempDir::new();
    let lock_path = dir.0.join("clock.lock");
    let first = CrossProcessLock::try_acquire(&lock_path).unwrap();
    assert!(first.is_some());
    assert!(CrossProcessLock::try_acquire(&lock_path).unwrap().is_none());
    drop(first);
    assert!(CrossProcessLock::try_acquire(&lock_path).unwrap().is_some());
}
