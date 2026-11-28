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
//! `{ENV_PREFIX}_NO_AUTOSTART=1` to disable (status/info commands never
//! auto-start — they exist to report truthfully on whether a daemon is up).

use crate::main_runtime::resolve_daemon;
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

/// One CLI process's registration with the daemon's client-presence tracker (the shared
/// `rightkit_daemon::loopback::PresenceClient`: `POST /clients`, heartbeats on the granted
/// cadence, a re-attach when the daemon forgot the client, `DELETE /clients/{id}` on detach).
/// Holding this alive is what tells the daemon "someone still needs me"; dropping it, or
/// `detach().await`, tells the daemon this client is gone.
pub struct PresenceGuard {
    client: Option<std::sync::Arc<rightkit_daemon::loopback::PresenceClient>>,
    stop: rightkit_daemon::stop::StopSource,
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
    attach_cli_presence_to(&base_url, &token, label).await
}

/// The loopback record a presence client needs: the port of `base_url` and the bearer token.
fn loopback_record(
    base_url: &str,
    token: &str,
) -> Option<rightkit_daemon::loopback::LoopbackRecord> {
    let port = base_url
        .trim_end_matches('/')
        .rsplit(':')
        .next()?
        .parse::<u16>()
        .ok()?;
    Some(rightkit_daemon::loopback::LoopbackRecord {
        port,
        token: token.to_string(),
        database_path: String::new(),
        pid: 0,
        boot_epoch_ms: 0,
        protocol_version: 0,
        schema_id: String::new(),
        runtime: String::new(),
        generation: 0,
    })
}

async fn attach_cli_presence_to(base_url: &str, token: &str, label: &str) -> Option<PresenceGuard> {
    let record = loopback_record(base_url, token)?;
    let label = label.to_string();
    // The presence client is blocking (bounded retries and timeouts), so it attaches off the
    // async runtime.
    let client = tokio::task::spawn_blocking(move || {
        rightkit_daemon::loopback::PresenceClient::attach(&record, "cli", Some(&label)).ok()
    })
    .await
    .ok()??;
    let client = std::sync::Arc::new(client);
    let stop = rightkit_daemon::stop::StopSource::new();
    // The heartbeat thread runs until the guard stops it; a failed beat is retried at the next.
    spawn_heartbeat_thread(client.clone(), stop.token()).ok()?;
    Some(PresenceGuard {
        client: Some(client),
        stop,
    })
}

/// Heartbeat on the cadence the daemon granted, held to `MIN_HEARTBEAT_SECONDS..=MAX_HEARTBEAT_SECONDS`
/// so a daemon that grants an absurd cadence cannot silence the heartbeat, until `stop`. A failed
/// beat (the client retries a transport failure and re-attaches when the daemon forgot it) is
/// tried again at the next.
fn spawn_heartbeat_thread(
    client: std::sync::Arc<rightkit_daemon::loopback::PresenceClient>,
    stop: rightkit_daemon::stop::StopToken,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name(crate::host_identity::prefixed("presence"))
        .spawn(move || loop {
            let granted = client
                .heartbeat_interval()
                .map_or(MAX_HEARTBEAT_SECONDS, |every| every.as_secs());
            if stop
                .wait(Duration::from_secs(clamp_heartbeat_seconds(granted)))
                .is_some()
            {
                break;
            }
            if let Err(error) = client.heartbeat() {
                tracing::debug!(
                    ?error,
                    "presence heartbeat failed; trying again at the next beat"
                );
            }
        })
        .map(|_| ())
}

impl PresenceGuard {
    /// Explicit async detach — best-effort, bounded so a hung/unreachable daemon never blocks
    /// process exit. Prefer this over relying solely on `Drop` when the caller is already in an
    /// async context and can await it.
    pub(crate) async fn detach(mut self) {
        self.stop
            .trigger(rightkit_daemon::stop::StopCause::Requested);
        if let Some(client) = self.client.take() {
            let _ = tokio::time::timeout(
                Duration::from_secs(2),
                tokio::task::spawn_blocking(move || client.detach()),
            )
            .await;
        }
    }
}

impl Drop for PresenceGuard {
    fn drop(&mut self) {
        // Stop the heartbeat; the client's own drop sends the best-effort detach once the
        // heartbeat thread has let go of it. Heartbeat silence past the expiry covers a daemon
        // that cannot be reached.
        self.stop
            .trigger(rightkit_daemon::stop::StopCause::Requested);
        if let Some(client) = self.client.take() {
            std::thread::spawn(move || client.detach());
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
        let explicit_daemon = std::env::var_os(rightkit_agent_config::env_name("DAEMON_URL"))
            .is_some()
            && std::env::var_os(rightkit_agent_config::env_name("DAEMON_TOKEN")).is_some();
        if explicit_daemon || daemon_health_ok(&found.0).await {
            return Some(found);
        }
        // One failed client probe is not authority to unlink shared discovery:
        // a live daemon alone owns removal of its port file on shutdown.
        // Startup may safely replace a stale record after its own lock/liveness
        // gate succeeds.
        tracing::debug!(url = %found.0, "daemon health probe failed; preserving daemon.port");
    }
    if !autostart_enabled_from_env(
        std::env::var(rightkit_agent_config::env_name("NO_AUTOSTART"))
            .ok()
            .as_deref(),
    ) {
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
            let interval = Duration::from_millis(250);
            let mut outcome = poll_for_daemon(&mut child, interval, 12).await;
            // `serve` exits 1 ("refusing second instance") while an incumbent holds
            // daemon.lock without a published daemon.port: either a draining incumbent
            // (shutdown removed daemon.port, drain lasts up to 10 s) or a starting one
            // (not published yet). Wait the incumbent out; respawn at most once if it
            // vacates. A free lock means the child failed for a real reason.
            if let SpawnPoll::ExitedEarly = outcome {
                if !daemon_lock_free() {
                    let deadline = crate::main_runtime::SHUTDOWN_DRAIN_TIMEOUT
                        + std::time::Duration::from_secs(2);
                    eprintln!(
                        "{app}: another {app} process holds the daemon lock; waiting up to {} s for it to finish (a daemon draining or starting)",
                        deadline.as_secs(),
                        app = crate::host_identity::app()
                    );
                    match wait_out_incumbent(interval, deadline).await {
                        IncumbentWait::Ready(found) => outcome = SpawnPoll::Ready(found),
                        IncumbentWait::Vacated => {
                            if let Some(mut retry) = spawn_detached_daemon() {
                                outcome = poll_for_daemon(&mut retry, interval, 12).await;
                                // Another CLI won the respawn race and is starting the
                                // daemon: follow it once, no further respawn.
                                if matches!(outcome, SpawnPoll::ExitedEarly) {
                                    if let Some(found) = poll_for_existing_daemon(interval, 9).await {
                                        outcome = SpawnPoll::Ready(found);
                                    }
                                }
                            }
                        }
                        IncumbentWait::TimedOut => {}
                    }
                }
            }
            Ok::<_, ()>(match outcome {
                SpawnPoll::Ready(found) => Some(found),
                _ => None,
            })
        })
        .await
        .ok()
        .flatten();
    drop(claim);
    resolved
}

/// Pure decision: should auto-start attempt to fire, given the
/// `{ENV_PREFIX}_NO_AUTOSTART` env value (or its absence)? Extracted so the
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
fn autostart_writer(path: &std::path::Path) -> rightkit_logs::RotatingTextWriter {
    rightkit_logs::RotatingTextWriter::new(path, AUTOSTART_LOG_MAX_BYTES)
        .with_generations(u32::from(AUTOSTART_LOG_HISTORY))
}

fn autostart_capture(path: &std::path::Path) -> rightkit_logs::DetachedLog {
    let sink = std::sync::Arc::new(autostart_writer(path));
    rightkit_logs::DetachedLog::new(sink)
        .with_known_secrets(rightkit_llm::agent_providers::host_credentials::host().known_secrets())
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

enum SpawnPoll {
    Ready((String, String)),
    /// The spawned child exited before publishing a daemon.
    ExitedEarly,
    GaveUp,
}

enum IncumbentWait {
    Ready((String, String)),
    /// Lock released and no daemon record appeared: the incumbent exited.
    Vacated,
    TimedOut,
}

async fn poll_for_daemon(
    child: &mut rightkit_child_process::OwnedChild,
    interval: Duration,
    max_attempts: u32,
) -> SpawnPoll {
    let mut delay = interval;
    for _ in 0..max_attempts {
        match child.try_wait() {
            Ok(Some(status)) => {
                eprintln!(
                    "[{app}] auto-started daemon exited early with {status}",
                    app = crate::host_identity::app()
                );
                finish_autostart_capture_after_exit();
                return SpawnPoll::ExitedEarly;
            }
            Err(error) => {
                eprintln!(
                    "[{app}] could not inspect auto-started daemon: {error}",
                    app = crate::host_identity::app()
                );
                return SpawnPoll::GaveUp;
            }
            Ok(None) => {}
        }
        tokio::time::sleep(delay).await;
        if let Some(found) = adopt_live_daemon().await {
            return SpawnPoll::Ready(found);
        }
        delay = (delay * 2).min(Duration::from_secs(2));
    }
    SpawnPoll::GaveUp
}

/// Non-destructive probe of `daemon.lock`: acquire and drop at once. Only a free lock
/// is ever taken, and never held. An unresolvable path or I/O error counts as free.
fn daemon_lock_free() -> bool {
    let Some(path) = crate::daemon::ownership::default_lockfile_path() else {
        return true;
    };
    !matches!(
        crate::daemon::ownership::OwnershipLock::acquire(&path),
        Ok(None)
    )
}

/// Poll for the incumbent daemon that holds the lock; report `Vacated` as soon as the
/// lock is free while no daemon record exists.
async fn wait_out_incumbent(interval: Duration, deadline: Duration) -> IncumbentWait {
    let started = std::time::Instant::now();
    let mut delay = interval;
    while started.elapsed() < deadline {
        tokio::time::sleep(delay).await;
        if let Some(found) = adopt_live_daemon().await {
            return IncumbentWait::Ready(found);
        }
        if crate::main_runtime::resolve_daemon_quick().is_none() && daemon_lock_free() {
            return IncumbentWait::Vacated;
        }
        delay = (delay * 2).min(Duration::from_secs(2));
    }
    IncumbentWait::TimedOut
}

/// Wait for the daemon another launcher is starting (RightKit's
/// `loopback::wait_for_claim_winner`): its record must name the live process that wrote it and
/// answer `/health`. `max_attempts` polls, `interval` apart at first and doubling to 2 s, as before.
async fn poll_for_existing_daemon(
    interval: Duration,
    max_attempts: u32,
) -> Option<(String, String)> {
    let path = crate::daemon::ownership::default_discovery_path()?;
    let mut budget = Duration::ZERO;
    let mut delay = interval;
    for _ in 0..max_attempts {
        budget += delay;
        delay = (delay * 2).min(Duration::from_secs(2));
    }
    tokio::task::spawn_blocking(move || {
        rightkit_daemon::loopback::wait_for_claim_winner(&path, budget, interval, |record| {
            rightkit_daemon::loopback::probe_health(record, POLL_HEALTH_TIMEOUT)
        })
        .map(|record| (record.base_url(), record.token))
    })
    .await
    .ok()
    .flatten()
}

/// The daemon `daemon.port` names, when it is the live process that wrote the record and answers
/// `/health` (RightKit's `loopback::adopt_or_spawn`; a dead or recycled pid is never adopted).
async fn adopt_live_daemon() -> Option<(String, String)> {
    let path = crate::daemon::ownership::default_discovery_path()?;
    tokio::task::spawn_blocking(move || {
        match rightkit_daemon::loopback::adopt_or_spawn(&path, |record| {
            rightkit_daemon::loopback::probe_health(record, POLL_HEALTH_TIMEOUT)
        }) {
            rightkit_daemon::loopback::LoopbackAdopt::Adopt(record) => {
                Some((record.base_url(), record.token))
            }
            rightkit_daemon::loopback::LoopbackAdopt::Spawn => None,
        }
    })
    .await
    .ok()
    .flatten()
}

/// Health budget inside the autostart poll loops (`poll_for_daemon`, `wait_out_incumbent`,
/// `poll_for_existing_daemon`). They are attempt-counted and repeat on their own schedule, so
/// they keep the quick budget; with the patient one, 12 attempts against a daemon that answers
/// and then wedges would take minutes before "automatic start failed".
const POLL_HEALTH_TIMEOUT: Duration = Duration::from_millis(500);

/// The readiness check after the first, patient `resolve_daemon()`: same patient budget, so
/// a busy daemon that resolve just reached is not failed again by a 500 ms check (W2-10).
/// Resolve accepts any HTTP answer while this needs a 2xx, so a daemon answering non-2xx
/// (for example during startup) still sends the caller into autostart; that spawn exits
/// "refusing second instance" and the caller waits out the incumbent.
async fn daemon_health_ok(base_url: &str) -> bool {
    daemon_health_ok_with(base_url, crate::main_runtime::BUSY_DAEMON_HEALTH_TIMEOUT).await
}

async fn daemon_health_ok_with(base_url: &str, response_budget: Duration) -> bool {
    let Ok(client) = rightkit_agent_daemon::http_client::daemon_client_probe(
        base_url,
        Duration::from_millis(300),
        response_budget,
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

/// The claim is held for the duration of the spawn; dropping it releases the `rightkit_fs`
/// advisory lock (a crashed holder never leaves a stale claim behind).
fn acquire_autostart_claim() -> Option<rightkit_fs::FileLock> {
    let path = host_home_dir()?.join("daemon-autostart.claim");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match rightkit_fs::FileLock::acquire(
        &path,
        &rightkit_fs::LockOptions::default().labeled(&crate::host_identity::prefixed("autostart")),
    ) {
        Ok(lock) => Some(lock),
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
