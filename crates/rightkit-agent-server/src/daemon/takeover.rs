//! Safe stale-owner takeover & PID-reuse defense for the RightKit daemon
//! (DL-002).
//!
//! Ported from `grok-build`
//! `crates/codegen/xai-grok-workspace-daemon/src/daemonize.rs`
//! (`PidFile::acquire_or_take_over` + `PredecessorTarget`), adapted to the
//! RightKit seam:
//! - `ownership::OwnershipLock` replaces `PidFile`.
//! - Exact-process pinning is `rightkit_child_process::AdoptedIdentity` (pid plus
//!   the host's start token, re-verified before every signal), plus a local
//!   `identity_matches_current_binary` executable-path check (this file) to
//!   close the residual pid+start-time collision gap. Any unresolved
//!   identity is ambiguous ownership & refuses takeover.
//! - Termination is delegated to `kill_tree::terminate_pid_pinned` (DL-012)
//!   rather than reimplemented here, since exact-process pinning plus
//!   graceful→forceful escalation is exactly the forced-kill-backstop
//!   contract that module owns.

use std::num::NonZeroU32;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use rightkit_child_process::{AdoptedIdentity, IdentityCheck};

use super::kill_tree;
use super::ownership::{read_lockfile_pid, OwnershipLock};

/// How long a takeover waits for the gracefully-terminated predecessor to
/// release the lockfile lock before escalating to a forceful kill.
pub(crate) const TAKEOVER_GRACE: Duration = Duration::from_secs(2);

/// How long a takeover waits for the lock after the forceful kill (process
/// death releases the flock) before declining.
const TAKEOVER_KILL_GRACE: Duration = Duration::from_secs(1);

/// Poll interval while waiting for the predecessor to release the lock.
const TAKEOVER_POLL: Duration = Duration::from_millis(50);

/// Stable identity captured for a predecessor before any signal is sent.
/// PID alone is reusable; RightKit's start-time identity plus the canonical
/// executable path make the target explicit and allow each later signal to be
/// revalidated.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PredecessorTarget {
    identity: AdoptedIdentity,
    pid: u32,
    boot_epoch: u128,
    executable: std::path::PathBuf,
}

impl PredecessorTarget {
    fn capture(pid: u32) -> Option<Self> {
        let identity = AdoptedIdentity::capture(NonZeroU32::new(pid)?).ok()??;
        let boot_epoch = crate::main_runtime::identity_start_epoch_ms(&identity)?;
        let executable = std::fs::canonicalize(identity.exe()?).ok()?;
        Some(Self {
            identity,
            pid,
            boot_epoch,
            executable,
        })
    }

    /// Re-resolve every identity component immediately before signaling.
    fn still_exact(&self) -> bool {
        let Some(path) = process_exe_path(self.pid) else {
            return false;
        };
        matches!(self.identity.verify(), Ok(IdentityCheck::Same))
            && std::fs::canonicalize(path).ok() == Some(self.executable.clone())
    }
}

/// Acquire the ownership lock, taking over from a live predecessor RightKit
/// daemon if one holds it: graceful termination (its normal shutdown runs
/// per `shutdown.rs`), `grace` to release the lock, then a forceful kill via
/// `kill_tree::terminate_pid_pinned` (process death releases the flock). The lock is
/// never bypassed — a guard is returned only with the flock held.
///
/// `Ok(None)` means the caller should exit quietly: the holder's identity
/// could not be pinned (pid recycled or process gone), or the lock could not
/// be won after escalation (e.g. a concurrent newer spawn took it).
pub fn acquire_or_take_over(
    path: &Path,
    grace: Duration,
) -> std::io::Result<Option<OwnershipLock>> {
    if let Some(guard) = OwnershipLock::acquire(path)? {
        return Ok(Some(guard));
    }

    let Some(pid) = read_lockfile_pid(path) else {
        return Ok(None);
    };
    if pid == std::process::id() {
        return Ok(None);
    }

    // Pin all predecessor identity before signaling anything. Any unresolved
    // component is ambiguous ownership & must refuse takeover.
    let Some(target) = PredecessorTarget::capture(pid) else {
        return Ok(None);
    };
    match identity_matches_current_binary(pid) {
        Some(false) => {
            eprintln!(
                "lockfile pid {pid} does not resolve to a coderight executable; declining takeover (likely pid reuse)"
            );
            return Ok(None);
        }
        Some(true) => {}
        None => {
            eprintln!(
                "could not verify executable identity for pid {pid}; declining ambiguous takeover"
            );
            return Ok(None);
        }
    }

    eprintln!("taking over from predecessor coderight daemon (pid {pid})");
    if request_cooperative_shutdown(path, pid) {
        if let Some(guard) = poll_acquire(path, grace)? {
            return Ok(Some(guard));
        }
    }
    if target.still_exact() {
        if let Err(e) = kill_tree::terminate_pid_pinned(target.pid, target.boot_epoch, false) {
            eprintln!("failed to signal predecessor (pid {pid}): {e}");
        }
    } else {
        eprintln!("predecessor identity changed before graceful signal; declining takeover");
        return Ok(None);
    }
    if let Some(guard) = poll_acquire(path, grace)? {
        return Ok(Some(guard));
    }

    eprintln!("predecessor (pid {pid}) did not release the lock in time; killing it");
    if target.still_exact() {
        if let Err(e) = kill_tree::terminate_pid_pinned(target.pid, target.boot_epoch, true) {
            eprintln!("failed to kill predecessor (pid {pid}): {e}");
        }
    } else {
        eprintln!("predecessor identity changed before forceful signal; declining takeover");
        return Ok(None);
    }
    if let Some(guard) = poll_acquire(path, TAKEOVER_KILL_GRACE)? {
        return Ok(Some(guard));
    }

    // The holder we signaled is dead yet the lock is still owned — a
    // concurrent newer spawn won it. Decline rather than double-run.
    eprintln!("lock is still held after killing pid {pid}; exiting");
    Ok(None)
}

fn request_cooperative_shutdown(lock_path: &Path, pid: u32) -> bool {
    let Some(discovery_path) = lock_path.parent().map(|parent| parent.join("daemon.port")) else {
        return false;
    };
    let Ok(content) = std::fs::read_to_string(discovery_path) else {
        return false;
    };
    let Some(info) = rightkit_daemon::loopback::LoopbackRecord::parse(&content) else {
        return false;
    };
    if info.pid != pid || info.port == 0 || info.token.is_empty() {
        return false;
    }
    // The blocking `rightkit_http::Client` owns no async runtime, so it is safe to call
    // from sync and async contexts alike. Loopback only: the discovery record names
    // this machine's daemon.
    let client = rightkit_http::Client::new(rightkit_http::ClientConfig {
        connect_timeout: Some(Duration::from_millis(500)),
        request_timeout: Duration::from_millis(500),
        response_timeout: Some(Duration::from_millis(500)),
        retry: rightkit_http::RetryPolicy::none(),
        address_policy: rightkit_http::AddressPolicy::public_web()
            .allow_if(vec!["127.0.0.1".to_string()], |_| true),
        proxy: rightkit_http::ProxyMode::None,
        ..rightkit_http::ClientConfig::default()
    });
    client
        .send_any(
            &rightkit_http::Request::post(format!(
                "http://127.0.0.1:{}/internal/takeover",
                info.port
            ))
            .bearer(&info.token),
        )
        .is_ok_and(|response| (200..300).contains(&response.status))
}

/// Exact executable-identity check: does `pid` resolve to this binary's
/// canonical path? `None` means identity could not be determined
/// (unsupported platform, permissions, or a mid-check exit); callers must
/// refuse takeover rather than signal an ambiguous owner.
fn identity_matches_current_binary(pid: u32) -> Option<bool> {
    let candidate = std::fs::canonicalize(process_exe_path(pid)?).ok()?;
    let current = std::fs::canonicalize(std::env::current_exe().ok()?).ok()?;
    Some(candidate == current)
}

/// Resolve `pid`'s executable path, best-effort, through RightKit's process
/// identity read. `None` on any failure (process gone, permission denied,
/// unsupported platform) - never panics.
fn process_exe_path(pid: u32) -> Option<std::path::PathBuf> {
    let identity = AdoptedIdentity::capture(NonZeroU32::new(pid)?).ok()??;
    identity.exe().map(Path::to_path_buf)
}

/// Retry [`OwnershipLock::acquire`] until it succeeds or `budget` elapses.
fn poll_acquire(path: &Path, budget: Duration) -> std::io::Result<Option<OwnershipLock>> {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(guard) = OwnershipLock::acquire(path)? {
            return Ok(Some(guard));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(TAKEOVER_POLL);
    }
}
