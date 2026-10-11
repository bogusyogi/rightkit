#![cfg(all(unix, any(target_os = "linux", target_os = "macos")))]

//! Spawning with the parent-death tie must not fork again inside the new process. An
//! embedding that registered `pthread_atfork` handlers which lock in `prepare` and unlock only
//! in the parent handler leaves that lock held in the first forked child, so a second `fork`
//! there blocks forever, before any timeout has started.
//!
//! The handlers below stand in for that embedding: in a process that is not the test process
//! (a forked child), finding the lock held means the real thing would be deadlocked, and the
//! stand-in exits with status 113 instead of hanging the suite.

use std::{
    process::Stdio,
    sync::atomic::{AtomicBool, AtomicI32, Ordering},
    time::Duration,
};

use rightkit_process::OwnedCommand;

static LOCKED: AtomicBool = AtomicBool::new(false);
static TEST_PID: AtomicI32 = AtomicI32::new(0);

extern "C" fn prepare() {
    loop {
        if LOCKED
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return;
        }
        // SAFETY: getpid and _exit are async-signal-safe.
        if unsafe { libc::getpid() } != TEST_PID.load(Ordering::SeqCst) {
            unsafe { libc::_exit(113) };
        }
        std::thread::yield_now();
    }
}

extern "C" fn parent() {
    LOCKED.store(false, Ordering::SeqCst);
}

#[test]
fn spawn_does_not_fork_again_inside_the_new_process() {
    TEST_PID.store(std::process::id() as i32, Ordering::SeqCst);
    // SAFETY: registers two plain handlers; there is deliberately no child handler.
    assert_eq!(
        unsafe { libc::pthread_atfork(Some(prepare), Some(parent), None) },
        0
    );

    let mut command = OwnedCommand::new("/bin/sleep");
    command
        .command_mut()
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.kill_on_parent_exit(true);
    let mut child = command.spawn().expect("spawn");
    let early = child
        .wait_timeout(Duration::from_millis(700))
        .expect("wait_timeout");
    let _ = child.terminate_tree();
    assert!(
        early.is_none(),
        "the new process died before exec ({early:?}): it forked again and hit the lock"
    );
}
