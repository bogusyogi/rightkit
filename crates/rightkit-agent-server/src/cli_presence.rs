//! CLI ↔ daemon lifecycle glue: presence attach/heartbeat for long-lived
//! commands, and best-effort daemon auto-start when none is running.
//!
//! **Presence.** A long-lived CLI invocation (the bare REPL, `run`,
//! `acp-server`, `mcp-server serve`) registers itself with the daemon via
//! `POST /clients` so the daemon knows a client is depending on it staying
//! up. A background task sends a heartbeat every `heartbeatSeconds`; on
//! `Drop` the guard best-effort `DELETE`s its registration so the daemon can
//! notice the detach immediately instead of waiting out the expiry window.
//! Contract (client-presence v1, built in parallel on the daemon side):
//!
//! - `POST /clients {"kind","pid","label"}` + bearer →
//!   `{"clientId","heartbeatSeconds","expirySeconds"}`
//! - `POST /clients/{id}/heartbeat` → 204
//! - `DELETE /clients/{id}` → 204
//!
//! A 404/connection-refused on attach means an older daemon without the
//! `/clients` route — every caller treats `None` as "no presence tracking,
//! carry on" so this feature degrades to a no-op against older daemons.
//!
//! **Auto-start.** When `resolve_daemon()` finds nothing reachable, daemon-
//! backed one-shot commands (the generic `daemon_get_json_value` /
//! `daemon_post_pretty_json` / `daemon_delete_pretty_json` REST helpers, plus
//! the few call sites that talk to the daemon directly) spawn `{app}
//! serve` detached, poll for its startup file, and proceed. Set
//! `RIGHTKIT_NO_AUTOSTART=1` to disable (status/info commands never
//! auto-start — they exist to report truthfully on whether a daemon is up).

use crate::main_runtime::resolve_daemon;
use serde::Deserialize;
use std::time::Duration;

const MIN_HEARTBEAT_SECONDS: u64 = 1;
const MAX_HEARTBEAT_SECONDS: u64 = 60;
const AUTOSTART_LOG_MAX_BYTES: u64 = 1_048_576;
const AUTOSTART_LOG_HISTORY: u8 = 3;

fn clamp_heartbeat_seconds(seconds: u64) -> u64 {
    seconds.clamp(MIN_HEARTBEAT_SECONDS, MAX_HEARTBEAT_SECONDS)
}

fn presence_allowed_for_label(label: &str, service_host_selected: bool) -> bool {
    // KRN-003: ordinary `{app} run` is an InlineHost client by default,
    // so it must not discover/attach/lease a daemon merely because one happens
    // to be running. Other long-lived daemon-backed surfaces keep their
    // historical presence contract; explicit ServiceHost selection restores
    // the run presence lease.
    label != "run" || service_host_selected
}

/// One CLI process's registration with the daemon's client-presence tracker.
/// Holding this alive is what tells the daemon "someone still needs me" —
/// dropping it (End of scope, process exit via destructor, or explicit
/// `detach().await`) tells the daemon this client is gone.
pub struct PresenceGuard {
    base_url: String,
    token: String,
    client_id: String,
    heartbeat_handle: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Debug, Deserialize)]
struct AttachResponse {
    #[serde(rename = "clientId")]
    client_id: String,
    #[serde(rename = "heartbeatSeconds")]
    heartbeat_seconds: u64,
    #[allow(dead_code)]
    #[serde(rename = "expirySeconds")]
    expiry_seconds: u64,
}

/// Attach the current process to the daemon's client-presence tracker as a
/// long-lived CLI client. Returns `None` (never an error) when there is no
/// reachable daemon, when the daemon predates the `/clients` route (404), or
/// when the request otherwise fails — presence is a best-effort courtesy to
/// the daemon's keep-alive logic, never a hard requirement for the CLI
/// command it's wrapping to function.
pub(crate) async fn attach_cli_presence(label: &str) -> Option<PresenceGuard> {
    if !presence_allowed_for_label(
        label,
        crate::main_runtime::service_host_explicitly_selected(),
    ) {
        return None;
    }
    let (base_url, token) = resolve_daemon()?;
    drain_autostart_log();
    attach_cli_presence_to(&base_url, &token, label, std::process::id()).await
}

async fn attach_cli_presence_to(
    base_url: &str,
    token: &str,
    label: &str,
    pid: u32,
) -> Option<PresenceGuard> {
    let client = rightkit_agent_daemon::http_client::daemon_client_probe(
        base_url,
        Duration::from_millis(500),
        Duration::from_secs(3),
    )
    .ok()?;
    let body = serde_json::json!({ "kind": "cli", "pid": pid, "label": label });
    let resp = client
        .send_any(
            &rightkit_http::Request::post(format!("{base_url}/clients"))
                .bearer(token)
                .json(&body)
                .ok()?,
        )
        .await
        .ok()?;
    if !rightkit_agent_daemon::http_client::is_success(resp.status) {
        return None;
    }
    let attach: AttachResponse = resp.json().ok()?;
    let heartbeat_period = Duration::from_secs(clamp_heartbeat_seconds(attach.heartbeat_seconds));
    let heartbeat_handle = spawn_heartbeat_task(
        client,
        base_url.to_string(),
        token.to_string(),
        attach.client_id.clone(),
        heartbeat_period,
    );
    Some(PresenceGuard {
        base_url: base_url.to_string(),
        token: token.to_string(),
        client_id: attach.client_id,
        heartbeat_handle: Some(heartbeat_handle),
    })
}

fn spawn_heartbeat_task(
    client: rightkit_http::AsyncClient,
    base_url: String,
    token: String,
    client_id: String,
    period: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(period);
        interval.tick().await; // first tick fires immediately; skip it, attach already counted
        loop {
            interval.tick().await;
            let _ = client
                .send_any(
                    &rightkit_http::Request::post(format!(
                        "{base_url}/clients/{client_id}/heartbeat"
                    ))
                    .bearer(&token),
                )
                .await;
        }
    })
}

impl PresenceGuard {
    /// Explicit async detach — best-effort, bounded to 2s so a hung/unreachable
    /// daemon never blocks process exit. Prefer this over relying solely on
    /// `Drop` when the caller is already in an async context and can await it
    /// (`Drop` can only fire a detached best-effort task, never await).
    pub(crate) async fn detach(mut self) {
        self.stop_heartbeat();
        let _ = tokio::time::timeout(Duration::from_secs(2), self.send_detach()).await;
    }

    fn stop_heartbeat(&mut self) {
        if let Some(handle) = self.heartbeat_handle.take() {
            handle.abort();
        }
    }

    async fn send_detach(&self) {
        if let Ok(client) =
            rightkit_agent_daemon::http_client::daemon_client_with(&self.base_url, Duration::from_secs(2))
        {
            let _ = client
                .send_any(
                    &rightkit_http::Request::delete(format!(
                        "{}/clients/{}",
                        self.base_url, self.client_id
                    ))
                    .bearer(&self.token),
                )
                .await;
        }
    }
}

impl Drop for PresenceGuard {
    fn drop(&mut self) {
        self.stop_heartbeat();
        // Best-effort fire-and-forget detach on unexpected drop (e.g. panic
        // unwind) — spawn a bounded-lifetime task rather than block a
        // destructor on network I/O. If there's no runtime left to spawn onto
        // (process is already tearing down), this silently no-ops, which is
        // fine: heartbeat silence past expirySeconds covers this case too.
        let base_url = self.base_url.clone();
        let token = self.token.clone();
        let client_id = self.client_id.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                if let Ok(client) = rightkit_agent_daemon::http_client::daemon_client_with(
                    &base_url,
                    Duration::from_secs(2),
                ) {
                    let _ = client
                        .send_any(
                            &rightkit_http::Request::delete(format!(
                                "{base_url}/clients/{client_id}"
                            ))
                            .bearer(&token),
                        )
                        .await;
                }
            });
        }
    }
}

// ── Auto-start ─────────────────────────────────────────────────────────────

/// Resolve a running daemon the same way `resolve_daemon()` does, but when
/// none is found, best-effort spawn `{app} serve` detached and poll for
/// its startup file before giving up. Intended for daemon-backed *action*
/// commands only (the generic REST helpers, kill/conduct/session/etc.) —
/// `{app} status`/`{app} info` must keep calling `resolve_daemon()`
/// directly so they report the true current state rather than mutating it.
pub(crate) async fn resolve_daemon_or_autostart() -> Option<(String, String)> {
    if let Some(found) = resolve_daemon() {
        let explicit_daemon = std::env::var_os("RIGHTKIT_DAEMON_URL").is_some()
            && std::env::var_os("RIGHTKIT_DAEMON_TOKEN").is_some();
        if explicit_daemon || daemon_health_ok(&found.0).await {
            return Some(found);
        }
        // One failed client probe is not authority to unlink shared discovery:
        // a live daemon alone owns removal of its port file on shutdown.
        // Startup may safely replace a stale record after its own lock/liveness
        // gate succeeds.
        tracing::debug!(url = %found.0, "daemon health probe failed; preserving daemon.port");
    }
    if !autostart_enabled_from_env(std::env::var("RIGHTKIT_NO_AUTOSTART").ok().as_deref()) {
        return None;
    }
    let Some(claim) = acquire_autostart_claim() else {
        // Another CLI/app may own the short-lived spawn claim. It is the only
        // launcher allowed to create the daemon; contenders wait for and adopt
        // its discovery record instead of failing or blind-spawning.
        return poll_for_existing_daemon(Duration::from_millis(250), 12).await;
    };
    // Coalesce concurrent detached launches onto the same readiness cycle.
    // The daemon owns publication; this caller only returns after its
    // ownership/transport/runtime identity/adoption gate has published a
    // current, health-reachable discovery record.
    let resolved = crate::daemon::launch::DaemonLaunchCoordinator::global()
        .ensure("daemon.port", || async {
            let mut child = spawn_detached_daemon().ok_or(())?;
            Ok::<_, ()>(poll_for_daemon(&mut child, Duration::from_millis(250), 12).await)
        })
        .await
        .ok()
        .flatten();
    drop(claim);
    resolved
}

/// Pure decision: should auto-start attempt to fire, given the
/// `RIGHTKIT_NO_AUTOSTART` env value (or its absence)? Extracted so the
/// on/off logic is unit-testable without touching a real process env.
fn autostart_enabled_from_env(no_autostart: Option<&str>) -> bool {
    !matches!(no_autostart, Some("1") | Some("true") | Some("TRUE"))
}

/// Spawn `{app} serve` as a detached background process, stdout/stderr
/// redirected to a log file under `~/.{app}/` so the parent CLI's own
/// stdout stays clean for the command that triggered the auto-start. Returns
/// `None` (logging to stderr) on any failure to locate the current exe, open
/// the log file, or spawn — callers treat that as "no daemon available,"
/// which is exactly what would have happened before auto-start existed.
fn spawn_detached_daemon() -> Option<rightkit_child_process::OwnedChild> {
    let exe = std::env::current_exe().ok()?;
    let log_path = host_home_dir().map(|dir| {
        let _ = std::fs::create_dir_all(&dir);
        dir.join("daemon-autostart.log")
    });
    // The daemon writes to a private raw file; RightKit's `DetachedLog` drains it into the
    // scrubbed, rotated `daemon-autostart.log`. We are here after a failed health probe, which
    // does not prove the previous daemon is gone: it may still hold the raw file open, and the
    // raw file is shared by every autostarted daemon. So this only ever drains (`finish` would
    // unlink the file under a live writer on Unix and lose its later output).
    let capture = log_path
        .as_deref()
        .map(autostart_capture)
        .map(std::sync::Arc::new);
    if let Some(capture) = capture.as_ref() {
        let _ = capture.drain();
    }

    // `OwnedCommand` hides the console, and `spawn_uncontained_detached` starts the daemon
    // detached from this CLI (setsid on Unix; breakaway / DETACHED_PROCESS on Windows) with no
    // terminate-on-drop, so it survives the parent exiting.
    let mut owned = rightkit_child_process::OwnedCommand::new(exe);
    {
        let command = owned.command_mut();
        command.arg("serve");
        if let Some(capture) = capture.as_ref() {
            if let (Ok(out), Ok(err)) = (capture.stderr_file(), capture.stderr_file()) {
                command.stdout(out);
                command.stderr(err);
            }
        }
        command.stdin(std::process::Stdio::null());
    }
    match owned.spawn_uncontained_detached() {
        Ok(child) => {
            // Drain while this CLI lives; a later CLI run drains the rest (`drain_autostart_log`).
            if let Some(capture) = capture.as_ref() {
                if let Ok(drainer) = capture.spawn_drainer(AUTOSTART_LOG_DRAIN_INTERVAL) {
                    if let Ok(mut guard) = AUTOSTART_DRAINER.lock() {
                        *guard = Some((capture.clone(), drainer));
                    }
                }
            }
            if let Some(path) = log_path.as_ref() {
                eprintln!(
                    "[{app}] no daemon found — started one; log: {}",
                    path.display(),
                    app = crate::host_identity::app()
                );
            } else {
                eprintln!(
                    "[{app}] no daemon found — started one in the background.",
                    app = crate::host_identity::app()
                );
            }
            Some(child)
        }
        Err(e) => {
            eprintln!(
                "[{app}] failed to auto-start daemon: {e}",
                app = crate::host_identity::app()
            );
            None
        }
    }
}

/// The capture for the daemon's autostart output: raw file `<log>.raw` beside the log, drained
/// into `path` scrubbed of credentials and rotated at [`AUTOSTART_LOG_MAX_BYTES`] with
/// [`AUTOSTART_LOG_HISTORY`] numbered predecessors. Autostart logs are diagnostic evidence, so
/// each launch appends to the existing file instead of truncating it.
fn autostart_capture(path: &std::path::Path) -> rightkit_logs::DetachedLog {
    let sink = std::sync::Arc::new(
        rightkit_logs::RotatingTextWriter::new(path, AUTOSTART_LOG_MAX_BYTES)
            .with_generations(u32::from(AUTOSTART_LOG_HISTORY)),
    );
    rightkit_logs::DetachedLog::new(sink)
}

/// Drain whatever the autostarted daemon has written since the last drain. Called when a CLI run
/// finds a daemon already up, so output from a daemon that outlived the CLI that started it still
/// reaches `daemon-autostart.log`. Best-effort.
pub(crate) fn drain_autostart_log() {
    let Some(path) = host_home_dir().map(|dir| dir.join("daemon-autostart.log")) else {
        return;
    };
    let capture = autostart_capture(&path);
    if capture.has_pending() {
        let _ = capture.drain();
    }
}

/// Stop this process's drainer after a last drain. The raw file is shared by every daemon this
/// machine ever autostarted: an older daemon that survived a failed health probe may still hold
/// it open, and one child's exit does not prove that writer is gone. So the raw file is never
/// removed here (`finish` would unlink it under a live writer on Unix); it only ever shrinks
/// through the owner's compaction after a full drain.
fn finish_autostart_capture_after_exit() {
    let started = AUTOSTART_DRAINER
        .lock()
        .ok()
        .and_then(|mut guard| guard.take());
    if let Some((capture, drainer)) = started {
        drainer.stop();
        let _ = capture.drain();
    }
}

/// The drainer of the capture this process started, kept until the process exits.
static AUTOSTART_DRAINER: std::sync::Mutex<
    Option<(
        std::sync::Arc<rightkit_logs::DetachedLog>,
        rightkit_logs::Drainer,
    )>,
> = std::sync::Mutex::new(None);
const AUTOSTART_LOG_DRAIN_INTERVAL: Duration = Duration::from_secs(1);

async fn poll_for_daemon(
    child: &mut rightkit_child_process::OwnedChild,
    interval: Duration,
    max_attempts: u32,
) -> Option<(String, String)> {
    let mut delay = interval;
    for _ in 0..max_attempts {
        match child.try_wait() {
            Ok(Some(status)) => {
                eprintln!(
                    "[{app}] auto-started daemon exited early with {status}",
                    app = crate::host_identity::app()
                );
                finish_autostart_capture_after_exit();
                return None;
            }
            Err(error) => {
                eprintln!(
                    "[{app}] could not inspect auto-started daemon: {error}",
                    app = crate::host_identity::app()
                );
                return None;
            }
            Ok(None) => {}
        }
        tokio::time::sleep(delay).await;
        if let Some((base_url, token)) = resolve_daemon() {
            if daemon_health_ok_with(&base_url).await {
                return Some((base_url, token));
            }
        }
        delay = (delay * 2).min(Duration::from_secs(2));
    }
    None
}

async fn poll_for_existing_daemon(
    interval: Duration,
    max_attempts: u32,
) -> Option<(String, String)> {
    let mut delay = interval;
    for _ in 0..max_attempts {
        tokio::time::sleep(delay).await;
        if let Some((base_url, token)) = resolve_daemon() {
            if daemon_health_ok_with(&base_url).await {
                return Some((base_url, token));
            }
        }
        delay = (delay * 2).min(Duration::from_secs(2));
    }
    None
}

async fn daemon_health_ok(base_url: &str) -> bool {
    daemon_health_ok_with(base_url).await
}

async fn daemon_health_ok_with(base_url: &str) -> bool {
    let Ok(client) = rightkit_agent_daemon::http_client::daemon_client_probe(
        base_url,
        Duration::from_millis(300),
        Duration::from_millis(500),
    ) else {
        return false;
    };
    matches!(
        client
            .send_any(&rightkit_http::Request::get(format!("{base_url}/health")))
            .await,
        Ok(resp) if rightkit_agent_daemon::http_client::is_success(resp.status)
    )
}

fn host_home_dir() -> Option<std::path::PathBuf> {
    rightkit_agent_config::state_root()
}

/// Held for the duration of the spawn; dropping releases the `rightkit_fs`
/// advisory lock (a crashed holder never leaves a stale claim behind).
struct AutostartClaim(#[allow(dead_code)] rightkit_fs::FileLock);

fn acquire_autostart_claim() -> Option<AutostartClaim> {
    let path = host_home_dir()?.join("daemon-autostart.claim");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match rightkit_fs::FileLock::acquire(
        &path,
        &rightkit_fs::LockOptions::default().labeled(&crate::host_identity::prefixed("autostart")),
    ) {
        Ok(lock) => Some(AutostartClaim(lock)),
        Err(rightkit_fs::FsError::Locked { .. } | rightkit_fs::FsError::LockTimeout { .. }) => {
            tracing::debug!("daemon autostart already claimed by another client");
            None
        }
        Err(error) => {
            eprintln!(
                "[{app}] unable to claim daemon autostart: {error}",
                app = crate::host_identity::app()
            );
            None
        }
    }
}
