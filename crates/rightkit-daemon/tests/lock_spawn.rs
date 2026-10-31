#![cfg(unix)]
//! A released daemon lock is free at once, even while another thread of the process forks
//! children (rightkit#1323, #788).
//!
//! The exclusive locks the daemon takes (spawn claim, writer lock, cutover lock, election) are
//! `flock`s, which belong to the open file description. A child that another thread forks, or
//! `posix_spawn`s, while a lock descriptor is open holds a copy of that description until its
//! `exec`. A lock that was only closed, never unlocked, therefore reads as held for the whole
//! fork-to-exec window; on macOS that window is milliseconds. `pre_exec` forces a plain fork and
//! its sleep widens the window, so a missing explicit unlock fails within the first few rounds.
//! The guard unlocks on drop, so the re-acquire after a release must succeed on the first try,
//! with no retry and no sleep to wait the window out.

use std::os::unix::process::CommandExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rightkit_daemon::{
    CutoverContext, CutoverError, CutoverParticipant, ParticipantError, RootCutover, SpawnClaim,
    SpawnClaimError, Suite,
};

/// Forks `sh` children from a sibling thread until dropped. Each child sits between fork and
/// exec for a few milliseconds with every descriptor of this process open.
struct Spawner {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Spawner {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = thread::spawn(move || {
            while !flag.load(Ordering::SeqCst) {
                let mut command = std::process::Command::new("sh");
                command.args(["-c", "true"]);
                // SAFETY: the closure only sleeps; it touches no locks or allocator state.
                unsafe {
                    command.pre_exec(|| {
                        thread::sleep(Duration::from_millis(2));
                        Ok(())
                    });
                }
                let _ = command.status();
            }
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Spawner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[test]
fn a_released_spawn_claim_is_free_while_other_threads_fork_children() {
    let dir = tempfile::tempdir().unwrap();
    let suite = Suite::at(dir.path().join("suite")).unwrap();
    let _spawner = Spawner::start();
    for round in 0..100 {
        let held = SpawnClaim::acquire(&suite, "demo")
            .unwrap_or_else(|e| panic!("round {round}: free claim reported busy: {e}"));
        assert!(
            matches!(
                SpawnClaim::acquire(&suite, "demo"),
                Err(SpawnClaimError::AlreadyHeld(_))
            ),
            "round {round}: a live claim must block"
        );
        // Hold across a fork or two, so a child exists that copied the open descriptor.
        thread::sleep(Duration::from_millis(4));
        drop(held);
        drop(
            SpawnClaim::acquire(&suite, "demo")
                .unwrap_or_else(|e| panic!("round {round}: released claim reported busy: {e}")),
        );
    }
}

#[test]
fn a_released_writer_lock_is_free_while_other_threads_fork_children() {
    let dir = tempfile::tempdir().unwrap();
    let app = Suite::at(dir.path().join("suite"))
        .unwrap()
        .app_root("LockSpawn")
        .unwrap();
    let _spawner = Spawner::start();
    for round in 0..100 {
        let held = app
            .lock_writer()
            .unwrap_or_else(|e| panic!("round {round}: free writer lock reported busy: {e:?}"));
        assert!(
            app.lock_writer().is_err(),
            "round {round}: a live writer lock must block"
        );
        thread::sleep(Duration::from_millis(4));
        drop(held);
        drop(app.lock_writer().unwrap_or_else(|e| {
            panic!("round {round}: released writer lock reported busy: {e:?}")
        }));
    }
}

/// Keeps the cutover (and its lock) busy for a few milliseconds, then stops it before the switch.
struct SlowPrepare;

impl CutoverParticipant for SlowPrepare {
    fn name(&self) -> &str {
        "slow-prepare"
    }

    fn prepare(&mut self, _: &CutoverContext) -> Result<(), ParticipantError> {
        thread::sleep(Duration::from_millis(4));
        Err(ParticipantError::new("stop before the switch"))
    }
}

#[test]
fn a_released_cutover_lock_is_free_while_other_threads_fork_children() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("old");
    std::fs::create_dir_all(&old).unwrap();
    let app = Suite::at(dir.path().join("suite"))
        .unwrap()
        .app_root("LockSpawn")
        .unwrap();
    let _spawner = Spawner::start();
    for round in 0..50 {
        // Each run takes the cutover lock, fails at `prepare` and releases it. The next run takes
        // it again at once: `Busy` here means the released lock still read as held.
        let result = RootCutover::new(app.clone(), &old)
            .participant(SlowPrepare)
            .run();
        assert!(
            matches!(result, Err(CutoverError::Participant { .. })),
            "round {round}: the cutover lock must be free and the run must stop at prepare: {result:?}"
        );
    }
}
