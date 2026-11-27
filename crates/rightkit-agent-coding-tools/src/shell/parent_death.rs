//! Unix: children the host launches die with the host, when it asks for that with
//! [`set_anchor_children_to_parent`](super::set_anchor_children_to_parent).
//!
//! On Linux `PR_SET_PDEATHSIG` fires when the *thread* that forked the child
//! exits, not when the process does. A host forks from tokio workers that can
//! retire as blocking-pool threads, so every Linux spawn goes through one
//! process-lifetime thread ([`spawn_anchored`] / [`run_anchored`]). Every launch
//! path registers the hook: the foreground and background shells,
//! `NativeCommandSandboxAdapter`, `process_launch::LocalProcessLaunchAdapter`
//! (MCP servers, vendor drivers, operator terminals, worktree tooling); a host that starts its
//! own children (a driver host's direct vendor launch) calls these functions itself. The Linux PTY
//! transport (`managed_pty_linux`) and the streamed PTY backend do not anchor their child, so a
//! PTY child is not tied to the host by this module.
//!
//! The death signal reaches the direct child only, so every anchored spawn also
//! calls [`tie_group_or_kill`], which ties the child's whole process group to
//! the host; a child that cannot be tied is killed and the launch fails.
//! macOS has no death signal and relies on that watchdog only. A grandchild that
//! left the group with its own `setsid` is not covered.

#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
#[cfg(target_os = "linux")]
use std::sync::{mpsc, OnceLock};

#[cfg(target_os = "linux")]
type Job = Box<dyn FnOnce() + Send>;

// `mpsc::Sender` is `Sync` (Rust 1.72+; the toolchain is pinned to 1.98). A failed
// anchor spawn is cached, so every anchored Linux spawn then fails: deliberately, there
// is no silent unprotected fallback.
#[cfg(target_os = "linux")]
static ANCHOR: OnceLock<Option<mpsc::Sender<Job>>> = OnceLock::new();

/// Ask the kernel to SIGKILL the child when its spawning thread dies. Callers
/// must spawn through [`spawn_anchored`], and must register this before any
/// seccomp `confine` hook (hooks run in registration order).
#[cfg(target_os = "linux")]
pub fn kill_on_parent_exit(command: &mut tokio::process::Command) {
    kill_on_parent_exit_std(command.as_std_mut());
}

/// [`kill_on_parent_exit`] for a `std::process::Command`. Callers must spawn
/// through [`run_anchored`] (or [`spawn_anchored`] for a tokio command).
#[cfg(target_os = "linux")]
pub fn kill_on_parent_exit_std(command: &mut std::process::Command) {
    let parent = std::process::id() as libc::pid_t;
    // SAFETY: the hook calls only `prctl` and `getppid` (raw syscalls): no
    // allocation, locks or formatting, so it is async-signal-safe after fork.
    unsafe {
        command.pre_exec(move || {
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

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
fn unavailable() -> std::io::Error {
    std::io::Error::other("spawn anchor thread unavailable")
}

/// Tie the process group led by `pid` to the host: when the host dies the
/// whole group is killed. If the tie fails the group is killed here and the
/// error is returned, so an untied child is never handed back to a caller.
///
/// The child must already lead its own process group: every call site runs
/// `lead_session` (setsid) or `OwnedCommand` (own group) first. `pid` 0 and
/// values above `i32::MAX` are refused without signalling anything, so this
/// never calls `killpg` with 0, a negative number or the host's own group.
pub fn tie_group_or_kill(pid: u32) -> std::io::Result<()> {
    if pid == 0 || pid > i32::MAX as u32 {
        return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
    }
    rightkit_child_process::tie_group_to_parent(pid).inspect_err(|_| {
        // SAFETY: `pid` is in 1..=i32::MAX and names the child's own group.
        unsafe {
            libc::killpg(pid as libc::pid_t, libc::SIGKILL);
        }
    })
}

/// Spawn `command` on the anchor thread so the parent-death signal is keyed to
/// a thread that lives as long as the process, then tie the child's group to
/// the host. If the anchor is unavailable this fails: it never falls back to
/// spawning on the current (retirable) thread, which would silently drop the
/// guarantee. A child that cannot be tied is killed and the spawn fails.
#[cfg(target_os = "linux")]
pub async fn spawn_anchored(
    mut command: tokio::process::Command,
) -> std::io::Result<tokio::process::Child> {
    let handle = tokio::runtime::Handle::current();
    let (done, result) = tokio::sync::oneshot::channel();
    let job: Job = Box::new(move || {
        // Register the child with the caller's runtime (reaper, pipes).
        let _guard = handle.enter();
        let spawned = command.spawn().and_then(|child| match child.id() {
            Some(pid) => tie_group_or_kill(pid).map(|()| child),
            None => Ok(child),
        });
        if let Err(Ok(mut child)) = done.send(spawned) {
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

/// Non-Linux Unix (macOS): no anchor thread, the watchdog is a helper process
/// and not bound to the spawning thread. Spawn, then tie the child's group; a
/// child that cannot be tied is killed and the spawn fails.
#[cfg(not(target_os = "linux"))]
pub async fn spawn_anchored(
    mut command: tokio::process::Command,
) -> std::io::Result<tokio::process::Child> {
    let child = command.spawn()?;
    match child.id() {
        Some(pid) => tie_group_or_kill(pid).map(|()| child),
        None => Ok(child),
    }
}

/// Run a synchronous spawn (`spawn` of a `std::process::Command` with
/// [`kill_on_parent_exit_std`] registered) on the anchor thread and wait for
/// its result. The job must only fork/exec and return: it runs on the one
/// thread every anchored spawn shares. Fails, never falls back, when the
/// anchor is unavailable. Callers tie the std child themselves with
/// [`tie_group_or_kill`] inside the closure.
#[cfg(target_os = "linux")]
pub fn run_anchored<T: Send + 'static>(
    spawn: impl FnOnce() -> T + Send + 'static,
) -> std::io::Result<T> {
    let (done, result) = mpsc::sync_channel::<T>(1);
    let job: Job = Box::new(move || {
        // `done` is sent once and its receiver stays blocked in `recv` below, so a failed send
        // means the waiting thread is gone. The value cannot be killed here (`T` is opaque), so
        // it is dropped, which runs its destructor.
        if let Err(unclaimed) = done.send(spawn()) {
            drop(unclaimed.0);
        }
    });
    anchor()
        .ok_or_else(unavailable)?
        .send(job)
        .map_err(|_| unavailable())?;
    result.recv().map_err(|_| unavailable())
}

/// Non-Linux Unix: no anchor thread, so the spawn runs inline. Callers tie the
/// std child themselves with [`tie_group_or_kill`] inside the closure.
#[cfg(not(target_os = "linux"))]
pub fn run_anchored<T: Send + 'static>(
    spawn: impl FnOnce() -> T + Send + 'static,
) -> std::io::Result<T> {
    Ok(spawn())
}
