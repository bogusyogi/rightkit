//! App-lock gate for every database connection (a host implements [`GatedState`] over its state), plus the crate-wide lock order.
//!
//! LOCK ORDER (acquire strictly left to right, never the reverse):
//!   `transitions` gate  ->  `inner` (writer)  ->  `reader`  ->  `app_lock`
//! `reader` is the slot holding the read-pool HANDLE: lock it only to clone or
//! replace the handle, never while a query runs. The pool's own mutex is a
//! leaf under it, and a query may wait on that mutex's condvar only with no
//! other lock held.
//! `db_path` (an RwLock around the path string) is a leaf too: set or read it
//! while holding anything, never take another lock under it.
//! `app_lock` is a leaf: never hold it while taking another lock. Never hold
//! it across an `.await`.
//!
//! Locked-state rules (single choke point for the writer helpers and both
//! reader helpers):
//! * Locked (or app-lock unavailable) means NO connection may be used, opened,
//!   reopened, or substituted by the writer. Callers get [`LOCKED_ERROR`].
//! * A missing reader pool is created only when the app is unlocked or
//!   disabled, and a pooled connection is never obtained without a fresh
//!   unlocked check immediately before AND after `ReadPool::get`.
//! * Locking closes the pool under the transition gate: idle connections close
//!   at once, checked-out ones finish their statement and close on return.
//! * Commands that work while locked never touch the DB (lock status, unlock, lock, enable and
//!   disable, health): the host keeps the list and a test keeps it honest.
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, RwLock};

use rusqlite::Connection;

use crate::{DbError, ReadPool};

/// The one user-visible string for "app is locked" (also stored in
/// `DbInner::error` at lock time).
pub const LOCKED_ERROR: &str = "The app is locked.";

/// The slice of a host application state the gate works on: the writer slot, the read-pool slot,
/// the database path and the app-lock answer. The host implements this over its own state type.
pub trait GatedState {
    /// The transition gate (first in the lock order).
    fn transitions(&self) -> &Transitions;
    /// The writer slot.
    fn inner(&self) -> &Mutex<DbInner>;
    /// The database path, readable without the writer mutex (a leaf lock).
    fn db_path(&self) -> &RwLock<String>;
    /// The read-pool handle slot.
    fn reader(&self) -> &Mutex<Option<ReadPool>>;
    /// True when the app lock forbids any database access. A poisoned lock state counts as
    /// locked (fail closed).
    fn blocks_db(&self) -> bool;
    /// Opens the read pool at `path`; `None` when it cannot be opened.
    fn open_read_pool(&self, path: &str) -> Option<ReadPool>;
    /// Timing hook: how long taking the `which` lock ("writer" or "reader") waited.
    fn record_lock_wait(&self, _which: &'static str, _since: std::time::Instant) {}
    /// Timing hook: a read by `caller_op` fell back to the writer.
    fn record_reader_fallback(&self, _caller_op: &'static str) {}
}

/// The writer slot: connection, database path and last open error.
pub struct DbInner {
    /// The writer connection, absent while locked or closed.
    pub conn: Option<Connection>,
    /// The database path.
    pub path: String,
    /// The last open or lock error shown to the user.
    pub error: Option<String>,
}

/// Serialises lock / unlock / enable / disable / key-rotation transitions.
#[derive(Default)]
pub struct Transitions {
    gate: Mutex<()>,
    generation: AtomicU64,
}

impl Transitions {
    /// Acquire the transition gate (first in the lock order). Poison-tolerant.
    pub fn enter(&self) -> MutexGuard<'_, ()> {
        self.gate.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// Current generation. An unlock captures this before verification and
    /// discards its result if it changed by the time it holds the gate.
    pub fn current(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }
    /// Invalidate every in-flight unlock. A lock request calls this BEFORE
    /// waiting on the gate.
    pub fn bump(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }
}

/// True when the app lock forbids any DB access. Poisoned runtime counts as
/// locked (fail closed).
pub fn is_locked<S: GatedState + ?Sized>(state: &S) -> bool {
    state.blocks_db()
}

pub fn check_unlocked<S: GatedState + ?Sized>(state: &S) -> Result<(), String> {
    if is_locked(state) {
        Err(LOCKED_ERROR.into())
    } else {
        Ok(())
    }
}

/// Error text for a closed writer: the locked error when locked, else the
/// historical "database not open".
pub fn closed_error<S: GatedState + ?Sized>(state: &S) -> String {
    if is_locked(state) {
        LOCKED_ERROR.into()
    } else {
        "database not open".into()
    }
}

/// Record the database path in `AppState::db_path` (a LEAF lock: held only
/// for a string read or write, so it is safe to call while holding `inner`).
/// Lets the lazy pool creation learn the path without the writer mutex, so a
/// caller that already holds `inner` and then uses a reader helper cannot
/// deadlock on itself.
///
/// Set at startup (`tauri_run`), refreshed by every writer helper and by the
/// pool warm-up, so it follows `inner.path` (changed only by open and unlock;
/// rekey and compaction reopen at the same path). An empty path is ignored.
pub fn set_db_path<S: GatedState + ?Sized>(state: &S, path: &str) {
    if path.is_empty() {
        return;
    }
    let same = state
        .db_path()
        .read()
        .map(|p| p.as_str() == path)
        .unwrap_or(false);
    if !same {
        *state.db_path().write().unwrap_or_else(|e| e.into_inner()) = path.to_string();
    }
}

/// Database path, never touching the writer mutex. Empty when unknown: the
/// pool is then not created and the read falls back to the writer.
fn db_path_nowait<S: GatedState + ?Sized>(state: &S) -> String {
    state
        .db_path()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Run `f` on the writer connection, refusing when locked.
pub fn with_writer_gated<S: GatedState + ?Sized, T>(
    state: &S,
    f: impl FnOnce(&Connection) -> Result<T, String>,
) -> Result<T, String> {
    let wait = std::time::Instant::now();
    let guard = state.inner().lock().unwrap();
    state.record_lock_wait("writer", wait);
    check_unlocked(state)?;
    set_db_path(state, &guard.path);
    let conn = guard.conn.as_ref().ok_or_else(|| closed_error(state))?;
    f(conn)
}

/// `&mut` variant of [`with_writer_gated`].
pub fn with_writer_mut_gated<S: GatedState + ?Sized, T>(
    state: &S,
    f: impl FnOnce(&mut Connection) -> Result<T, String>,
) -> Result<T, String> {
    let wait = std::time::Instant::now();
    let mut guard = state.inner().lock().unwrap();
    state.record_lock_wait("writer", wait);
    check_unlocked(state)?;
    set_db_path(state, &guard.path);
    let conn = guard.conn.as_mut().ok_or_else(|| closed_error(state))?;
    f(conn)
}

/// How long a lock waits for checked-out readers to come back before it gives up.
pub const POOL_CLOSE_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Drop the read pool handle and close its connections (app lock), waiting up to
/// [`POOL_CLOSE_WAIT`] for checked-out readers (`ReadPool::close_and_wait`). `Ok` means every
/// connection is closed and the database file released; `Err` means a reader is still out, and the
/// caller must keep the store-wide lock. Callers hold the transition gate and no reader.
pub fn close_reader_pool<S: GatedState + ?Sized>(state: &S) -> Result<(), DbError> {
    close_reader_pool_within(state, POOL_CLOSE_WAIT)
}

/// [`close_reader_pool`] with an explicit wait (tests use a short one).
pub fn close_reader_pool_within<S: GatedState + ?Sized>(
    state: &S,
    wait: std::time::Duration,
) -> Result<(), DbError> {
    let pool = state.reader().lock().expect("reader mutex poisoned").take();
    match pool {
        Some(pool) => pool.close_and_wait(wait),
        None => Ok(()),
    }
}

/// Handle to the current read pool, creating it when missing (unlocked only),
/// with the pool opener injected. `Ok(None)` means the pool could not be
/// opened: the caller falls back to the writer. The reader slot is never held
/// while a query runs.
fn current_reader_pool_with<S: GatedState + ?Sized>(
    state: &S,
    open: impl FnOnce(&str) -> Option<ReadPool>,
) -> Result<Option<ReadPool>, String> {
    {
        let wait = std::time::Instant::now();
        let guard = state.reader().lock().unwrap();
        state.record_lock_wait("reader", wait);
        check_unlocked(state)?;
        if let Some(pool) = guard.as_ref() {
            return Ok(Some(pool.clone()));
        }
    }
    // Pool missing. Learn the path WITHOUT waiting on the writer mutex (the
    // caller may already hold it) and with NO reader guard held, then re-lock
    // the reader and re-check everything.
    let db_path = db_path_nowait(state);
    if db_path.is_empty() {
        // Path not known yet: no pool; the caller falls back to the writer.
        return Ok(None);
    }
    let mut guard = state.reader().lock().unwrap();
    check_unlocked(state)?;
    if let Some(pool) = guard.as_ref() {
        return Ok(Some(pool.clone()));
    }
    // One creation attempt (never loops). Unlocked/disabled only.
    Ok(open(&db_path).inspect(|pool| *guard = Some(pool.clone())))
}

/// Attempts to obtain a connection when the pool is swapped under us (key
/// rotation). Each retry re-reads the slot, which waits out the rotation.
const POOL_ATTEMPTS: usize = 3;

/// Longest a read waits for a busy pooled reader before using the writer.
pub const READER_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Open the read pool off the main thread (startup warm-up). Same checks as a
/// read: opens nothing while locked, one attempt, never holds a lock across a
/// query. A read that arrives first simply creates the pool itself.
pub fn warm_reader_pool<S: GatedState + ?Sized>(state: &S) {
    warm_with(state, |path| state.open_read_pool(path));
}

fn warm_with<S: GatedState + ?Sized>(state: &S, open: impl FnOnce(&str) -> Option<ReadPool>) {
    // Only when the writer opened (as the old eager open did); the guard is
    // released before the reader slot is touched (writer-before-reader).
    let writer_open = state
        .inner()
        .lock()
        .map(|g| {
            set_db_path(state, &g.path);
            g.conn.is_some()
        })
        .unwrap_or(false);
    if writer_open {
        let _ = current_reader_pool_with(state, open);
    }
}

/// Run `f` on a pooled read connection (shared by the command and repo
/// layers). Pool creation and the writer fallback apply only when
/// unlocked/disabled; the lock is re-checked before and after `get`.
pub fn with_reader_gated<S: GatedState + ?Sized, T>(
    state: &S,
    caller_op: &'static str,
    f: impl FnOnce(&Connection) -> Result<T, String>,
) -> Result<T, String> {
    with_reader_gated_using(
        state,
        caller_op,
        READER_WAIT,
        |path| state.open_read_pool(path),
        f,
    )
}

/// [`with_reader_gated`] with the wait bound and pool opener injected (tests).
fn with_reader_gated_using<S: GatedState + ?Sized, T>(
    state: &S,
    caller_op: &'static str,
    wait: std::time::Duration,
    open: impl Fn(&str) -> Option<ReadPool>,
    f: impl FnOnce(&Connection) -> Result<T, String>,
) -> Result<T, String> {
    check_unlocked(state)?;
    for _ in 0..POOL_ATTEMPTS {
        let Some(pool) = current_reader_pool_with(state, &open)? else {
            break;
        };
        // Pool exists: wait for a reader (condvar, no other lock held) rather
        // than queue behind the writer. Expiry falls back to the writer.
        match pool.get_timeout(wait) {
            Ok(Some(conn)) => {
                // Lock published after our slot read: never run on it.
                check_unlocked(state)?;
                return f(&conn);
            }
            Ok(None) => break,
            // Closed: the app locked or the key rotated; re-read the slot.
            Err(DbError::ReadPoolClosed) => check_unlocked(state)?,
            // A lazy connection open failed: use the writer.
            Err(_) => break,
        }
    }
    // No pool, open failed, or the bounded wait expired: fall back to the
    // writer (unlocked only; gated again).
    state.record_reader_fallback(caller_op);
    with_writer_gated(state, f)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        transitions: Transitions,
        inner: Mutex<DbInner>,
        path: RwLock<String>,
        reader: Mutex<Option<ReadPool>>,
        locked: bool,
    }

    impl GatedState for Fake {
        fn transitions(&self) -> &Transitions {
            &self.transitions
        }
        fn inner(&self) -> &Mutex<DbInner> {
            &self.inner
        }
        fn db_path(&self) -> &RwLock<String> {
            &self.path
        }
        fn reader(&self) -> &Mutex<Option<ReadPool>> {
            &self.reader
        }
        fn blocks_db(&self) -> bool {
            self.locked
        }
        fn open_read_pool(&self, _path: &str) -> Option<ReadPool> {
            None
        }
    }

    fn fake(locked: bool) -> Fake {
        Fake {
            transitions: Transitions::default(),
            inner: Mutex::new(DbInner {
                conn: Some(Connection::open_in_memory().unwrap()),
                path: String::new(),
                error: None,
            }),
            path: RwLock::new(String::new()),
            reader: Mutex::new(None),
            locked,
        }
    }

    #[test]
    fn a_locked_state_refuses_the_writer() {
        let state = fake(true);
        let result = with_writer_gated(&state, |_| Ok(1));
        assert_eq!(result, Err(LOCKED_ERROR.to_string()));
    }

    #[test]
    fn an_unlocked_state_runs_the_writer_and_records_the_path() {
        let state = fake(false);
        state.inner().lock().unwrap().path = "db.sqlite".to_string();
        let result = with_writer_gated(&state, |c| {
            c.execute_batch("CREATE TABLE t(x)")
                .map_err(|e| e.to_string())?;
            Ok(2)
        });
        assert_eq!(result, Ok(2));
        assert_eq!(*state.db_path().read().unwrap(), "db.sqlite");
    }

    #[test]
    fn a_read_without_a_pool_falls_back_to_the_writer() {
        let state = fake(false);
        let result = with_reader_gated(&state, "test", |c| {
            c.query_row("SELECT 7", [], |r| r.get::<_, i64>(0))
                .map_err(|e| e.to_string())
        });
        assert_eq!(result, Ok(7));
    }
}
