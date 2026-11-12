// RIGHTKIT-SERIOUS: rightkit-daemon 0.3.0 is a user-only pipe/socket service with no TCP, token or port record, so the loopback-HTTP daemon that the window, CLI, iOS and SSH clients connect to cannot move onto it (report O-2)
//! Runtime binding for `rightkit_agent_daemon::LifecycleAuthority`.
//!
//! This module deliberately delegates to existing daemon launch, client
//! presence, schedule, and session APIs. It does not create another process
//! supervisor or replace CLI routes. Destructive lifecycle actions remain
//! unavailable until daemon owns authenticated control endpoints for them.

use std::collections::HashMap;
use std::sync::OnceLock;

use super::launch::DaemonLaunchSpec;
use super::reload::{ReloadHooks, ReloadRequest, ReloadStore};
use async_trait::async_trait;
use rightkit_agent_daemon::{
    BackendOutcome, LaunchMode, LifecycleAuthority, LifecycleBackend, ProcessEntry,
};
use serde_json::{json, Value};

/// Shared runtime authority. Every in-process surface obtains this handle,
/// preserving `LifecycleAuthority`'s serialized transition boundary.
pub(crate) fn authority() -> &'static LifecycleAuthority<DaemonLifecycleBackend> {
    static AUTHORITY: OnceLock<LifecycleAuthority<DaemonLifecycleBackend>> = OnceLock::new();
    AUTHORITY.get_or_init(|| LifecycleAuthority::new(DaemonLifecycleBackend::default()))
}

/// DL-014: resolve a reachable daemon endpoint through `authority()`'s
/// `Start { Background }` command instead of calling
/// `cli_presence::resolve_daemon_or_autostart()` directly. This is the
/// CLI-dispatch wiring: every flat CLI verb that needs "the daemon is up,
/// give me its (base_url, token)" now goes through the one lifecycle
/// authority rather than an independent ad hoc call to the same
/// autostart coordinator.
///
/// Behavior is unchanged for a fresh process (the common case: one flat CLI
/// invocation is one process, so `authority()`'s in-memory state always
/// starts `Stopped`, and `Start` always reaches
/// `DaemonLifecycleBackend::start`'s real background branch, which performs
/// the exact same `resolve_daemon_or_autostart()` call as before). The one
/// case this helper additionally covers is the idempotent "already running"
/// short-circuit (`LifecycleAuthority::idempotent_report`), which never
/// calls the backend and so never resolves an endpoint; on that branch this
/// helper falls back to `resolve_daemon_or_autostart()` directly so callers
/// still get a working endpoint rather than a spurious failure.
pub(crate) async fn resolve_daemon_endpoint_via_authority() -> Option<(String, String)> {
    use rightkit_agent_daemon::{LifecycleCommand, LifecycleSurface};

    match authority()
        .execute(
            LifecycleSurface::Cli,
            LifecycleCommand::Start {
                mode: LaunchMode::Background,
            },
        )
        .await
    {
        Ok(report) => match report.endpoint {
            Some(endpoint) => Some(endpoint),
            None => crate::cli_presence::resolve_daemon_or_autostart().await,
        },
        Err(_) => None,
    }
}

fn tui_presence_allowed(service_host_selected: bool) -> bool {
    // KRN-003: TUI presence is a ServiceHost lifecycle concern. InlineHost
    // must not probe discovery or create a client lease merely because a
    // daemon happens to exist on the machine.
    service_host_selected
}

/// DL-014: the TUI's interactive-session presence attach
/// (`main_runtime_interactive_session.rs::InteractiveSession::run`) also
/// submits through `authority()` instead of calling
/// `cli_presence::attach_cli_presence` directly. Presence-attach is
/// deliberately best-effort with **no autostart**
/// (`cli_presence::attach_cli_presence`'s doc comment), while
/// `LifecycleCommand::Start`'s background branch always autostarts. Routing
/// through `Start` unconditionally would change behavior (a bare `{app}`
/// invocation would start launching a daemon it previously left alone), so
/// this probes reachability first with the same non-autostart
/// `crate::main_runtime::resolve_daemon()` the direct call effectively used,
/// and only submits `Start` + `Attach` through the authority when a
/// daemon is already reachable. That keeps "no autostart from TUI attach"
/// exactly as before while making `authority()` the one place that performs
/// the presence-attach effect and records the resulting attached-client
/// state. Returns whether the presence attach is now tracked by the
/// authority (mirrors the previous `Option<PresenceGuard>::is_some()`
/// check at the call site).
pub(crate) async fn attach_tui_presence(client_id: &str) -> bool {
    use rightkit_agent_daemon::{LifecycleCommand, LifecycleSurface};

    if !tui_presence_allowed(crate::main_runtime::service_host_explicitly_selected()) {
        return false;
    }
    if crate::main_runtime::resolve_daemon().is_none() {
        return false;
    }
    let authority = authority();
    if authority
        .execute(
            LifecycleSurface::Tui,
            LifecycleCommand::Start {
                mode: LaunchMode::Background,
            },
        )
        .await
        .is_err()
    {
        return false;
    }
    authority
        .execute(
            LifecycleSurface::Tui,
            LifecycleCommand::Attach {
                client_id: client_id.to_string(),
            },
        )
        .await
        .is_ok()
}

/// Counterpart to [`attach_tui_presence`]: detach through the same
/// authority-owned guard rather than calling `PresenceGuard::detach`
/// directly, since the authority's backend now owns that guard.
pub(crate) async fn detach_tui_presence(client_id: &str) {
    use rightkit_agent_daemon::{LifecycleCommand, LifecycleSurface};

    let _ = authority()
        .execute(
            LifecycleSurface::Tui,
            LifecycleCommand::Detach {
                client_id: Some(client_id.to_string()),
            },
        )
        .await;
}

/// Concrete daemon adapter. Presence guards retain daemon-issued client ids
/// while callers keep their stable surface client ids in `LifecycleAuthority`.
#[derive(Default)]
pub(crate) struct DaemonLifecycleBackend {
    attached: HashMap<String, crate::cli_presence::PresenceGuard>,
}

#[async_trait]
impl LifecycleBackend for DaemonLifecycleBackend {
    async fn start(&mut self, mode: LaunchMode) -> Result<BackendOutcome, String> {
        if mode == LaunchMode::Foreground {
            return Ok(BackendOutcome {
                detail: format!(
                    "foreground selected; caller owns `{app} serve` process",
                    app = crate::host_identity::app()
                )
                .to_string(),
                ..Default::default()
            });
        }
        let (base_url, token) = crate::cli_presence::resolve_daemon_or_autostart()
            .await
            .ok_or_else(|| "daemon launch did not produce a reachable endpoint".to_string())?;
        Ok(BackendOutcome {
            detail: format!("daemon reachable at {base_url} via existing launch coordinator"),
            endpoint: Some((base_url, token)),
            ..Default::default()
        })
    }

    async fn attach(&mut self, client_id: &str) -> Result<BackendOutcome, String> {
        if self.attached.contains_key(client_id) {
            return Ok(BackendOutcome {
                detail: format!("client {client_id} already attached"),
                ..Default::default()
            });
        }
        let guard = crate::cli_presence::attach_cli_presence(client_id)
            .await
            .ok_or_else(|| format!("daemon rejected client presence for {client_id}"))?;
        self.attached.insert(client_id.to_string(), guard);
        Ok(BackendOutcome {
            detail: format!("client {client_id} attached through daemon presence"),
            ..Default::default()
        })
    }

    async fn detach(&mut self, client_id: Option<&str>) -> Result<BackendOutcome, String> {
        let detached = match client_id {
            Some(client_id) => match self.attached.remove(client_id) {
                Some(guard) => {
                    guard.detach().await;
                    1
                }
                None => 0,
            },
            None => {
                let guards = std::mem::take(&mut self.attached);
                let count = guards.len();
                for (_, guard) in guards {
                    guard.detach().await;
                }
                count
            }
        };
        Ok(BackendOutcome {
            detail: format!("detached {detached} daemon presence client(s)"),
            ..Default::default()
        })
    }

    async fn stop(&mut self, force: bool) -> Result<BackendOutcome, String> {
        stop_daemon(force).await
    }

    async fn restart(&mut self) -> Result<BackendOutcome, String> {
        run_reload(None).await
    }

    async fn update(&mut self, artifact: &str) -> Result<BackendOutcome, String> {
        run_reload(Some(artifact)).await
    }

    async fn schedule_list(&mut self, _include_inactive: bool) -> Result<BackendOutcome, String> {
        let (_, schedules) = daemon_json(rightkit_http::Method::Get, "/schedules", None).await?;
        let count = schedules
            .get("schedules")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        Ok(BackendOutcome {
            detail: format!("listed {count} daemon schedule(s)"),
            ..Default::default()
        })
    }

    async fn schedule_add(
        &mut self,
        session_id: &str,
        schedule: &str,
        prompt: &str,
    ) -> Result<BackendOutcome, String> {
        let (_, response) = daemon_json(
            rightkit_http::Method::Post,
            "/schedules",
            Some(json!({
                "id": session_id,
                "cronExpr": schedule,
                "prompt": prompt,
            })),
        )
        .await?;
        let id = response
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(session_id);
        Ok(BackendOutcome {
            detail: format!("created daemon schedule {id}"),
            ..Default::default()
        })
    }

    async fn schedule_cancel(&mut self, job_id: &str) -> Result<BackendOutcome, String> {
        let path = format!("/schedules/{}", path_segment(job_id));
        daemon_json(rightkit_http::Method::Delete, &path, None).await?;
        Ok(BackendOutcome {
            detail: format!("cancelled daemon schedule {job_id}"),
            ..Default::default()
        })
    }

    async fn process_list(&mut self, _reap: bool, _force: bool) -> Result<BackendOutcome, String> {
        let (_, sessions) = daemon_json(rightkit_http::Method::Get, "/sessions", None).await?;
        let processes = sessions
            .as_array()
            .into_iter()
            .flatten()
            .map(|session| ProcessEntry {
                pid: 0,
                role: "daemon-session".to_string(),
                state: session
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                owned: true,
            })
            .collect::<Vec<_>>();
        Ok(BackendOutcome {
            detail: format!("listed {} daemon session process row(s)", processes.len()),
            processes,
            ..Default::default()
        })
    }
}

/// Existing lifecycle authority producer for the shared reload saga. The
/// daemon remains the authenticated caller; `ReloadCoordinator` owns only
/// ordering, durable phases, activation, & rollback.
async fn run_reload(artifact: Option<&str>) -> Result<BackendOutcome, String> {
    let executable =
        std::env::current_exe().map_err(|e| format!("resolve daemon executable: {e}"))?;
    // A service-launched (`serve --resident`) daemon keeps residency across
    // reloads: the respawned process inherits this daemon's own launch flag.
    let resident = std::env::args_os().skip(2).any(|a| a == "--resident");
    let mut args = vec!["serve".into()];
    if resident {
        args.push("--resident".into());
    }
    let launch = DaemonLaunchSpec {
        executable: executable.to_string_lossy().into_owned(),
        args,
        cwd: std::env::current_dir()
            .ok()
            .map(|path| path.to_string_lossy().into_owned()),
        socket_path: crate::daemon::ownership::default_lockfile_path()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
    };
    let request_id = format!("lifecycle-{}", std::process::id());
    let hash = artifact.unwrap_or("restart").to_string();
    let mut request = ReloadRequest::new(request_id, hash, launch);
    if let Some(staged) = artifact {
        request = request.with_activation(staged, executable);
    }
    let store = ReloadStore::default_for_user()
        .ok_or_else(|| "reload state directory unavailable".to_string())?;
    let coordinator = crate::daemon::reload::ReloadCoordinator::new(
        LifecycleReloadHooks::new(store.clone()),
        store,
    );
    let report = coordinator.run(request).await.map_err(|e| e.to_string())?;
    Ok(BackendOutcome {
        detail: format!("reload {} completed", report.request_id),
        ..Default::default()
    })
}

/// ra-002: previously every stage below (`fence_intake`/`drain`/
/// `wait_ready`/`resume`/`rollback`) was an unconditional `Ok(())`, so
/// `ReloadCoordinator::run` always returned `SocketReady` immediately after
/// `launch_replacement` spawned the child process — whether or not that
/// child ever bound the endpoint, or even survived past spawn.
///
/// `wait_ready` now polls the same durable [`ReloadStore`] marker the
/// replacement daemon itself publishes through
/// (`ReloadStore::publish_socket_ready`, called by the replacement once it
/// has rebound the endpoint) and, in parallel, watches the spawned pid via
/// `reload::process_alive`. Success requires the marker to reach
/// `ReloadPhase::SocketReady` *for this exact request id/hash* before a
/// bounded deadline; the replacement exiting early or the deadline elapsing
/// is now a real hard failure instead of a false success, and `rollback`
/// terminates a replacement that failed to become ready (`reload::
/// terminate_pid_best_effort`) so a stuck/broken child does not linger
/// alongside the still-running original.
///
/// `fence_intake`/`drain`/`persist_recovery`/`resume` remain no-ops: making
/// those real requires plumbing this hook into the live `DaemonState`
/// mutation-admission fence (`main_runtime_server.rs`/`api_clients.rs`),
/// which this struct — a free-standing `ReloadHooks` impl with no reference
/// to that state — does not have access to. That remains open.
struct LifecycleReloadHooks {
    store: ReloadStore,
    replacement_pid: std::sync::Mutex<Option<u32>>,
}

impl LifecycleReloadHooks {
    fn new(store: ReloadStore) -> Self {
        Self {
            store,
            replacement_pid: std::sync::Mutex::new(None),
        }
    }
}

/// Bound on how long a replacement gets to publish `SocketReady` before
/// `wait_ready` gives up and the coordinator rolls back. Generous enough for
/// normal daemon startup (model/index warmup) while still failing a truly
/// stuck replacement in finite time instead of hanging the reload forever.
const REPLACEMENT_READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);
const REPLACEMENT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(150);

#[async_trait]
impl ReloadHooks for LifecycleReloadHooks {
    async fn persist_recovery(&mut self, _: &ReloadRequest) -> Result<(), String> {
        Ok(())
    }
    async fn fence_intake(&mut self) -> Result<(), String> {
        Ok(())
    }
    async fn drain(&mut self) -> Result<(), String> {
        Ok(())
    }
    async fn launch_replacement(&mut self, request: &ReloadRequest) -> Result<(), String> {
        // Stamp the reload identity onto the replacement so it can publish its
        // own `SocketReady`; without it `wait_ready` below can never observe
        // readiness and would roll back a healthy replacement at the deadline.
        let child = crate::daemon::reload::spawn_replacement_for_request(
            &request.launch,
            Some((request.request_id.as_str(), request.hash.as_str())),
        )
        .map_err(|e| format!("launch replacement: {e}"))?;
        *self
            .replacement_pid
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(child.id());
        Ok(())
    }
    async fn wait_ready(&mut self, request: &ReloadRequest) -> Result<(), String> {
        let pid = *self
            .replacement_pid
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let deadline = tokio::time::Instant::now() + REPLACEMENT_READY_TIMEOUT;
        loop {
            if let Some(state) = self.store.load() {
                if state.request_id == request.request_id && state.hash == request.hash {
                    match state.phase {
                        crate::daemon::reload::ReloadPhase::SocketReady => return Ok(()),
                        crate::daemon::reload::ReloadPhase::Failed => {
                            return Err(state
                                .detail
                                .unwrap_or_else(|| "replacement reported failure".to_string()));
                        }
                        _ => {}
                    }
                }
            }
            if let Some(pid) = pid {
                if !crate::daemon::reload::process_alive(pid) {
                    return Err(format!(
                        "replacement process {pid} exited before publishing readiness"
                    ));
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(format!(
                    "timed out after {:?} waiting for replacement readiness",
                    REPLACEMENT_READY_TIMEOUT
                ));
            }
            tokio::time::sleep(REPLACEMENT_POLL_INTERVAL).await;
        }
    }
    async fn resume(&mut self, _: &ReloadRequest) -> Result<(), String> {
        Ok(())
    }
    async fn rollback(&mut self, _: &ReloadRequest) -> Result<(), String> {
        if let Some(pid) = self
            .replacement_pid
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            if crate::daemon::reload::process_alive(pid) {
                crate::daemon::reload::terminate_pid_best_effort(pid);
            }
        }
        Ok(())
    }
}

/// Stop the daemon whose identity is published in the discovery record
/// (`~/.{app}/daemon.port`) through the exact same control surface
/// `takeover.rs::acquire_or_take_over` already uses in production to
/// displace a live predecessor: a cooperative `POST /internal/takeover`
/// first (unless `force`), then `kill_tree::terminate_pid_pinned` — which
/// re-validates the pinned `boot_epoch_ms` immediately before signaling, so
/// a pid recycled between discovery-read and signal is never touched.
///
/// This does not invent a new shutdown path: `/internal/takeover` and
/// `kill_tree::terminate_pid_pinned` are both pre-existing, already-shipped
/// daemon control primitives (DL-002 / DL-012); this function only gives
/// `LifecycleAuthority::stop` a way to reach them.
async fn stop_daemon(force: bool) -> Result<BackendOutcome, String> {
    let discovery_path = crate::daemon::ownership::default_lockfile_path()
        .and_then(|lock| lock.parent().map(|parent| parent.join("daemon.port")))
        .ok_or_else(|| "daemon discovery path unavailable".to_string())?;

    let content = match std::fs::read_to_string(&discovery_path) {
        Ok(content) => content,
        Err(_) => {
            return Ok(BackendOutcome {
                detail: "no daemon discovery record; nothing to stop".to_string(),
                ..Default::default()
            });
        }
    };
    let info = rightkit_daemon::loopback::LoopbackRecord::parse(&content)
        .ok_or_else(|| "invalid daemon discovery record".to_string())?;
    if info.pid == 0 || info.boot_epoch_ms == 0 {
        return Ok(BackendOutcome {
            detail: "daemon discovery record has no live pid".to_string(),
            ..Default::default()
        });
    }

    if !force {
        let takeover_url = format!("http://127.0.0.1:{}/internal/takeover", info.port);
        let cooperative = rightkit_agent_daemon::http_client::daemon_client_probe(
            &takeover_url,
            std::time::Duration::from_millis(500),
            std::time::Duration::from_millis(500),
        )
        .map_err(|e| format!("build stop client: {e}"))?
        .send_any(&rightkit_http::Request::post(takeover_url).bearer(&info.token))
        .await;
        if cooperative.is_ok_and(|response| rightkit_agent_daemon::http_client::is_success(response.status))
        {
            // The takeover ack only means the request was accepted; confirm the
            // process actually exited and escalate if it lingers.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while crate::daemon::reload::process_alive(info.pid)
                && std::time::Instant::now() < deadline
            {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            if !crate::daemon::reload::process_alive(info.pid) {
                return Ok(BackendOutcome {
                    detail: format!("daemon pid {} stopped cooperatively", info.pid),
                    ..Default::default()
                });
            }
        }
    }

    super::kill_tree::terminate_pid_pinned(info.pid, info.boot_epoch_ms, force)
        .map_err(|e| format!("stop daemon pid {}: {e}", info.pid))?;
    Ok(BackendOutcome {
        detail: format!(
            "daemon pid {} terminated ({})",
            info.pid,
            if force { "forced" } else { "signal escalation" }
        ),
        ..Default::default()
    })
}

async fn daemon_json(
    method: rightkit_http::Method,
    path: &str,
    body: Option<Value>,
) -> Result<(String, Value), String> {
    let (base_url, token) = crate::cli_presence::resolve_daemon_or_autostart()
        .await
        .ok_or_else(|| "no reachable daemon endpoint".to_string())?;
    let response = rightkit_agent_daemon::http_client::daemon_call(
        method,
        &format!("{base_url}{path}"),
        &token,
        body.as_ref(),
    )
    .await
    .map_err(|error| format!("daemon request {path}: {error}"))?;
    let status = response.status;
    let payload = response.text();
    if !rightkit_agent_daemon::http_client::is_success(status) {
        return Err(format!(
            "daemon request {path} failed with {status}: {payload}"
        ));
    }
    let value = if payload.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&payload)
            .map_err(|error| format!("decode daemon response {path}: {error}"))?
    };
    Ok((base_url, value))
}

fn path_segment(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}
