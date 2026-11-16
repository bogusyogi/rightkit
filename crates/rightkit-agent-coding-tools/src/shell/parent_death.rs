//! Linux only: shell children die with the host process, when the host asks for it with
//! [`set_anchor_children_to_parent`].
//!
//! `PR_SET_PDEATHSIG` fires when the *thread* that forked the child exits, not
//! when the process does. The daemon forks from tokio workers that can retire
//! as blocking-pool threads, so every spawn goes through one process-lifetime
//! thread (`spawn_anchored`). Grandchildren are not covered: they are reached by
//! the session-wide tree kill, not by the death signal.

use std::os::unix::process::CommandExt;
use std::sync::{mpsc, OnceLock};

type Job = Box<dyn FnOnce() + Send>;

// `mpsc::Sender` is `Sync` (Rust 1.72+; the toolchain is pinned to 1.98). A failed
// anchor spawn is cached, so every Linux shell spawn then fails: deliberately, there
// is no silent unprotected fallback.
static ANCHOR: OnceLock<Option<mpsc::Sender<Job>>> = OnceLock::new();

/// Ask the kernel to SIGKILL the child when its spawning thread dies. Callers
/// must spawn through [`spawn_anchored`], and must register this before any
/// seccomp `confine` hook (hooks run in registration order).
pub(crate) fn kill_on_parent_exit(command: &mut tokio::process::Command) {
    let parent = std::process::id() as libc::pid_t;
    // SAFETY: the hook calls only `prctl` and `getppid` (raw syscalls): no
    // allocation, locks or formatting, so it is async-signal-safe after fork.
    unsafe {
        command.as_std_mut().pre_exec(move || {
            // Variadic arguments: pass explicit unsigned longs, not i32 literals.
            let (sig, zero) = (libc::SIGKILL as libc::c_ulong, 0 as libc::c_ulong);
            if libc::prctl(libc::PR_SET_PDEATHSIG, sig, zero, zero, zero) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // The parent may have died between fork and prctl.
            if libc::getppid() != parent {
                return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
}

fn anchor() -> Option<&'static mpsc::Sender<Job>> {
    ANCHOR
        .get_or_init(|| {
            let (tx, rx) = mpsc::channel::<Job>();
            std::thread::Builder::new()
                .name("spawn-anchor".into())
                .spawn(move || {
                    while let Ok(job) = rx.recv() {
                        job();
                    }
                })
                .ok()?;
            Some(tx)
        })
        .as_ref()
}

/// Spawn `command` on the anchor thread so the parent-death signal is keyed to
/// a thread that lives as long as the process. If the anchor is unavailable
/// this fails: it never falls back to spawning on the current (retirable)
/// thread, which would silently drop the guarantee.
pub(crate) async fn spawn_anchored(
    mut command: tokio::process::Command,
) -> std::io::Result<tokio::process::Child> {
    let unavailable = || std::io::Error::other("spawn anchor thread unavailable");
    let handle = tokio::runtime::Handle::current();
    let (done, result) = tokio::sync::oneshot::channel();
    let job: Job = Box::new(move || {
        // Register the child with the caller's runtime (reaper, pipes).
        let _guard = handle.enter();
        if let Err(Ok(mut child)) = done.send(command.spawn()) {
            // The caller gave up while we were spawning: do not leak the child.
            let _ = child.start_kill();
        }
    });
    anchor()
        .ok_or_else(unavailable)?
        .send(job)
        .map_err(|_| unavailable())?;
    result.await.map_err(|_| unavailable())?
}
