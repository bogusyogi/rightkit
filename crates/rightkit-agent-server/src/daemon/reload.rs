//! Crash-safe daemon update/restart handoff (DL-014).
//!
//! The saga is RightKit's `rightkit_daemon::reload::ReloadCoordinator`: one reload at a time, the
//! durable marker `daemon.reload.json` (`starting`, `draining`, `handoff`, `socket_ready`,
//! `failed`) bound to the request id and hash, host hooks for persist, fence, drain, launch,
//! wait-ready, resume and rollback, and composed stage/rollback/marker errors. The replacement
//! receives `RIGHTKIT_RELOAD_REQUEST`/`RIGHTKIT_RELOAD_HASH` and publishes readiness with
//! `rightkit_daemon::reload::publish_socket_ready`.
//!
//! The host keeps the launch itself ([`spawn_replacement_for_request`], the existing
//! [`DaemonLaunchSpec`], Windows job breakaway, the worker journal) and the binary swap: the
//! staged artifact is activated through `daemon::migrate` (a `rightkit-fs` transaction, durable
//! and recovered at startup) inside the launch hook, not through RightKit's rename-only
//! `ActivationFence`. The hooks are in `lifecycle_backend.rs`.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use super::launch::DaemonLaunchSpec;

pub use rightkit_daemon::reload::{poll_ready, ReloadCoordinator, ReloadHost, ReloadRequest};

/// The directory that holds `daemon.reload.json`: the state root, beside `daemon.port`.
pub fn marker_dir() -> Option<PathBuf> {
    rightkit_agent_config::state_root()
}

/// The replacement command with detached stdio and the reload identity, stamped as environment so
/// the child can publish readiness for exactly this request.
pub fn replacement_command_for_request(
    spec: &DaemonLaunchSpec,
    identity: Option<(&str, &str)>,
) -> Command {
    let mut command = rightkit_child_process::command_std(&spec.executable);
    command
        .args(&spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    if let Some((request_id, hash)) = identity {
        command.env(rightkit_daemon::reload::ENV_RELOAD_REQUEST, request_id);
        command.env(rightkit_daemon::reload::ENV_RELOAD_HASH, hash);
    }
    command
}

/// Spawn the replacement daemon. `identity` carries the reload request this
/// replacement is fulfilling, so it can publish `SocketReady` for exactly that
/// request once it is listening; `None` is an ordinary launch.
pub fn spawn_replacement_for_request(
    spec: &DaemonLaunchSpec,
    identity: Option<(&str, &str)>,
) -> std::io::Result<Child> {
    // The replacement must outlive this daemon, so it breaks away from the
    // daemon's kill-on-close containment job. When an outer job forbids breakaway,
    // a plain spawn is only safe if this daemon is NOT in its own kill-on-close job;
    // otherwise the replacement would die with us, so fail the launch and let the
    // saga roll back to the running daemon.
    #[cfg(windows)]
    let child = {
        use std::os::windows::process::CommandExt as _;
        let mut command = replacement_command_for_request(spec, identity);
        command.creation_flags(
            rightkit_agent_coding_tools::no_window::CREATE_NO_WINDOW
                | rightkit_child_process::CREATE_BREAKAWAY_FROM_JOB,
        );
        match command.spawn() {
            Ok(child) => child,
            Err(error)
                if super::kill_tree::SELF_CONTAINED.load(std::sync::atomic::Ordering::SeqCst) =>
            {
                return Err(std::io::Error::other(format!(
                    "replacement cannot break away from the daemon's kill-on-close job: {error}"
                )))
            }
            Err(_) => replacement_command_for_request(spec, identity).spawn()?,
        }
    };
    #[cfg(not(windows))]
    let child = replacement_command_for_request(spec, identity).spawn()?;
    // DL-013: publish ownership immediately after spawn.  Startup recovery
    // re-checks exact PID/start identity before adopting or terminating it.
    if let Some(path) = super::recovery::default_journal_path() {
        super::recovery::record_worker_state(&path, child.id(), true);
    }
    Ok(child)
}

/// Whether process `pid` exists, through RightKit's process identity read (a zombie
/// still exists; on Windows an exited process whose handle is held elsewhere does not).
/// A read that fails for lack of permission counts as alive, as `kill(pid, 0)` did.
pub fn process_alive(pid: u32) -> bool {
    let Some(pid) = std::num::NonZeroU32::new(pid) else {
        return false;
    };
    match rightkit_child_process::AdoptedIdentity::capture(pid) {
        Ok(identity) => identity.is_some(),
        Err(_) => true,
    }
}

/// ra-002: best-effort termination used by [`super::lifecycle_backend`]'s
/// reload rollback when a spawned replacement never becomes ready (never
/// publishes `SocketReady` before the deadline, or exits on its own). This
/// intentionally does not require a pinned `boot_epoch_ms` the way
/// `kill_tree::terminate_pid_pinned` does: the pid here was returned by this
/// process's own immediately-preceding replacement spawn, so there is
/// no discovery-file race window to guard against — the caller already knows
/// exactly which process it is trying to stop.
pub fn terminate_pid_best_effort(pid: u32) -> bool {
    use rightkit_child_process::{AdoptedIdentity, AdoptedSignal, SignalResult};

    let Some(pid) = std::num::NonZeroU32::new(pid) else {
        return false;
    };
    // The identity is captured immediately before the signal, so the pid cannot change
    // hands in between. `Graceful` is SIGTERM on Unix and a terminate on Windows.
    let Ok(Some(identity)) = AdoptedIdentity::capture(pid) else {
        return false;
    };
    matches!(
        identity.signal(AdoptedSignal::Graceful),
        Ok(SignalResult::Delivered)
    )
}

/// Called by a daemon during startup, once its listener is up: if this process was launched as a
/// reload replacement, publish `socket_ready` for exactly that request so the coordinator's
/// wait observes real readiness. A no-op for every ordinary daemon start and for a half-set
/// identity; no daemon from before the RightKit reload saga is accepted.
pub fn publish_socket_ready_if_replacement(endpoint: &str) -> bool {
    let Some(dir) = marker_dir() else {
        return false;
    };
    let env_pair = |request: &str, hash: &str| match (std::env::var(request), std::env::var(hash)) {
        (Ok(request_id), Ok(hash)) => Some((request_id, hash)),
        // A half-set identity is not a reload: publishing against a guessed request id could
        // satisfy someone else's wait.
        _ => None,
    };
    if let Some((request_id, hash)) = env_pair(
        rightkit_daemon::reload::ENV_RELOAD_REQUEST,
        rightkit_daemon::reload::ENV_RELOAD_HASH,
    ) {
        return match rightkit_daemon::reload::publish_socket_ready(
            &dir,
            &request_id,
            &hash,
            endpoint,
        ) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(%error, "reload replacement could not publish socket readiness");
                false
            }
        };
    }
    false
}
