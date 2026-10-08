//! File locks must not look held because another thread forked a child.
//!
//! A child forked while a lock descriptor is open inherits that descriptor until it execs.
//! `flock` locks belong to the open file description, so the child keeps the lock alive after
//! the parent releases it. POSIX record locks belong to the process and are not inherited.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rightkit_fs::{FileLock, LockOptions};

/// Spawns children from a sibling thread. `pre_exec` forces a plain fork (as
/// `rightkit-process` does), so each child exists with the parent's descriptors open until
/// it execs, and the sleep widens that window.
fn spawner(stop: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
    use std::os::unix::process::CommandExt;
    std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            let mut command = std::process::Command::new("sh");
            command.args(["-c", "true"]);
            // SAFETY: the closure only sleeps; it touches no locks or allocator state.
            unsafe {
                command.pre_exec(|| {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    Ok(())
                });
            }
            let _ = command.status();
        }
    })
}

#[test]
fn released_exclusive_lock_is_free_while_other_threads_spawn_children() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.lock");
    let stop = Arc::new(AtomicBool::new(false));
    let spawn_thread = spawner(stop.clone());
    for round in 0..400 {
        let held = FileLock::try_acquire(&path)
            .unwrap_or_else(|e| panic!("round {round}: free lock reported busy: {e}"));
        assert!(
            FileLock::try_acquire(&path).is_err(),
            "round {round}: live exclusive holder must block"
        );
        drop(held);
        FileLock::try_acquire(&path)
            .unwrap_or_else(|e| panic!("round {round}: released lock reported busy: {e}"));
    }
    stop.store(true, Ordering::SeqCst);
    spawn_thread.join().unwrap();
}

#[test]
fn released_shared_lock_is_free_while_other_threads_spawn_children() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.lock");
    let stop = Arc::new(AtomicBool::new(false));
    let spawn_thread = spawner(stop.clone());
    let options = LockOptions::default();
    for round in 0..400 {
        let held = FileLock::acquire_shared(&path, &options)
            .unwrap_or_else(|e| panic!("round {round}: free lock reported busy: {e}"));
        drop(held);
        FileLock::try_acquire(&path).unwrap_or_else(|e| {
            panic!("round {round}: released shared lock blocks exclusive: {e}")
        });
    }
    stop.store(true, Ordering::SeqCst);
    spawn_thread.join().unwrap();
}

#[test]
fn shared_locks_coexist_and_exclude_exclusive() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.lock");
    let options = LockOptions::default();

    let first = FileLock::acquire_shared(&path, &options).unwrap();
    let second = FileLock::acquire_shared(&path, &options)
        .expect("shared holders must not block each other");
    assert!(
        FileLock::try_acquire(&path).is_err(),
        "exclusive must be refused while shared holders exist"
    );

    drop(first);
    assert!(
        FileLock::try_acquire(&path).is_err(),
        "exclusive must stay refused while one shared holder remains"
    );
    let third = FileLock::acquire_shared(&path, &options)
        .expect("shared must still be granted beside a remaining shared holder");

    drop(second);
    drop(third);
    let exclusive = FileLock::try_acquire(&path).expect("exclusive free after all shared drop");
    assert!(
        FileLock::acquire_shared(&path, &options).is_err(),
        "shared must be refused while exclusive is held"
    );
    drop(exclusive);
    FileLock::acquire_shared(&path, &options).expect("shared free after exclusive drops");
}

#[test]
fn exclusive_then_shared_holders_stay_in_order_under_spawning() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.lock");
    let stop = Arc::new(AtomicBool::new(false));
    let spawn_thread = spawner(stop.clone());
    let options = LockOptions::default();
    for round in 0..200 {
        let exclusive = FileLock::try_acquire(&path)
            .unwrap_or_else(|e| panic!("round {round}: free lock reported busy: {e}"));
        assert!(
            FileLock::acquire_shared(&path, &options).is_err(),
            "round {round}: shared must be refused beside exclusive"
        );
        drop(exclusive);
        let a = FileLock::acquire_shared(&path, &options)
            .unwrap_or_else(|e| panic!("round {round}: shared reported busy: {e}"));
        let b = FileLock::acquire_shared(&path, &options)
            .unwrap_or_else(|e| panic!("round {round}: second shared reported busy: {e}"));
        drop(a);
        drop(b);
    }
    stop.store(true, Ordering::SeqCst);
    spawn_thread.join().unwrap();
}

#[test]
fn failed_acquire_releases_lock_while_other_threads_spawn_children() {
    // A directory where the owner record belongs makes `acquire` fail after the lock is
    // taken. The lock must not outlive that failed call, even while a child forked during it
    // still holds an inherited descriptor.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.lock");
    let owner = dir.path().join("state.lock.owner");
    let stop = Arc::new(AtomicBool::new(false));
    let spawn_thread = spawner(stop.clone());
    for round in 0..400 {
        std::fs::create_dir(&owner).unwrap();
        assert!(
            FileLock::try_acquire(&path).is_err(),
            "round {round}: owner write should fail"
        );
        std::fs::remove_dir(&owner).unwrap();
        FileLock::try_acquire(&path)
            .unwrap_or_else(|e| panic!("round {round}: lock leaked by failed acquire: {e}"));
    }
    stop.store(true, Ordering::SeqCst);
    spawn_thread.join().unwrap();
}
