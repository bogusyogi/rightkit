// ── Daemon client helpers ─────────────────────────────────────────────────────

/// Machine-readable daemon state used by `coderight status`.
///
/// `Stopped` means no daemon could be resolved. `Unreachable` means a configured
/// or discovered daemon failed transport/probe connection. `Degraded` means
/// daemon transport responded but health or session data was not fully usable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DaemonMachineStatus {
    Healthy,
    Degraded,
    Stopped,
    Unreachable,
}

impl DaemonMachineStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Stopped => "stopped",
            Self::Unreachable => "unreachable",
        }
    }
}

enum DaemonResolution {
    Resolved((String, String)),
    Stopped,
    Unreachable { base_url: String, error: String },
}

/// Resolve the running daemon's base URL and bearer token.
///
/// Resolution order:
/// 1. `RIGHTKIT_DAEMON_URL` + `RIGHTKIT_DAEMON_TOKEN` env vars (set by callers / CI).
/// 2. `<state root>/daemon.port` — JSON file written by `serve` on startup.
pub(crate) fn resolve_daemon() -> Option<(String, String)> {
    match resolve_daemon_state() {
        DaemonResolution::Resolved(resolved) => Some(resolved),
        DaemonResolution::Stopped | DaemonResolution::Unreachable { .. } => None,
    }
}

fn resolve_daemon_state() -> DaemonResolution {
    let url = std::env::var("RIGHTKIT_DAEMON_URL").ok();
    let tok = std::env::var("RIGHTKIT_DAEMON_TOKEN").ok();
    let state_dir = rightkit_app_settings::agent_config::state_root().map(|p| p.display().to_string());
    resolve_daemon_state_from_sources(url, tok, state_dir.as_deref())
}

fn resolve_daemon_from_sources(
    url: Option<String>,
    tok: Option<String>,
    state_dir: Option<&str>,
) -> Option<(String, String)> {
    match resolve_daemon_state_from_sources(url, tok, state_dir) {
        DaemonResolution::Resolved(resolved) => Some(resolved),
        DaemonResolution::Stopped | DaemonResolution::Unreachable { .. } => None,
    }
}

fn resolve_daemon_state_from_sources(
    url: Option<String>,
    tok: Option<String>,
    state_dir: Option<&str>,
) -> DaemonResolution {
    // Env vars take highest precedence.
    if let (Some(u), Some(t)) = (url, tok) {
        return DaemonResolution::Resolved((u, t));
    }

    // Fall back to the <state root>/daemon.port lockfile.
    let Some(state_dir) = state_dir else {
        return DaemonResolution::Stopped;
    };
    let port_file = std::path::Path::new(state_dir).join("daemon.port");
    let Ok(content) = std::fs::read_to_string(&port_file) else {
        return DaemonResolution::Stopped;
    };
    let Some(info) = LoopbackRecord::parse(&content) else {
        return DaemonResolution::Stopped;
    };
    if info.protocol_version != 1
        || info.schema_id.trim().is_empty()
        || info.runtime.trim().is_empty()
        || info.generation == 0
        || info.pid == 0
        || info.boot_epoch_ms == 0
        || !process_matches_start(info.pid, info.boot_epoch_ms)
    {
        return DaemonResolution::Stopped;
    }
    let url = format!("http://127.0.0.1:{}", info.port);
    if !blocking_health_reachable_repeated(&url) {
        // Discovery removal is daemon-owned. Even after repeated health
        // failure clients only classify the record as unavailable; they never
        // unlink another process's file.
        return DaemonResolution::Unreachable {
            base_url: url,
            error: "daemon health probe failed".to_string(),
        };
    }
    DaemonResolution::Resolved((url, info.token))
}

/// Reachability probe, safe to call from sync *or* async callers: the blocking
/// `rightkit_http::Client` owns no async runtime, so there is nothing to drop inside
/// an async context.
fn blocking_health_reachable_repeated(base_url: &str) -> bool {
    let mut policy = rightkit_http::AddressPolicy::public_web();
    if let Some(host) = rightkit_http::http_client::host_of(base_url) {
        policy = policy.allow_if(vec![host], |_| true);
    }
    let client = rightkit_http::Client::new(rightkit_http::ClientConfig {
        connect_timeout: Some(std::time::Duration::from_millis(300)),
        request_timeout: std::time::Duration::from_millis(500),
        response_timeout: Some(std::time::Duration::from_millis(500)),
        retry: rightkit_http::RetryPolicy::none(),
        address_policy: policy,
        proxy: rightkit_http::ProxyMode::None,
        ..rightkit_http::ClientConfig::default()
    });
    repeated_health_probe(
        || {
            client
                .send_any(&rightkit_http::Request::get(format!("{base_url}/health")))
                // Any HTTP response proves transport reachability. HTTP
                // health failures are classified as degraded by async status.
                .is_ok()
        },
        || std::thread::sleep(std::time::Duration::from_millis(150)),
    )
}

fn repeated_health_probe(
    mut probe: impl FnMut() -> bool,
    mut between_attempts: impl FnMut(),
) -> bool {
    if probe() {
        return true;
    }
    between_attempts();
    probe()
}

/// Whether `pid` names a process that started within two seconds of `boot_epoch_ms`
/// (the epoch the daemon recorded for itself). RightKit owns the host start-time reads.
fn process_matches_start(pid: u32, boot_epoch_ms: u128) -> bool {
    let (Some(pid), Ok(recorded_ms)) =
        (std::num::NonZeroU32::new(pid), u64::try_from(boot_epoch_ms))
    else {
        return false;
    };
    matches!(
        rightkit_child_process::AdoptedIdentity::from_recorded_unix_ms(
            pid,
            recorded_ms,
            std::time::Duration::from_millis(2_000),
        ),
        Ok(Some(_))
    )
}

/// Start time of `identity`'s process as Unix epoch milliseconds.
pub(crate) fn identity_start_epoch_ms(
    identity: &rightkit_child_process::AdoptedIdentity,
) -> Option<u128> {
    identity
        .start_time()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_millis())
}

/// Start time of process `pid` as Unix epoch milliseconds, `None` when no such process
/// exists or the host reports no start time.
pub(crate) fn process_start_epoch_ms(pid: u32) -> Option<u128> {
    let identity =
        rightkit_child_process::AdoptedIdentity::capture(std::num::NonZeroU32::new(pid)?)
            .ok()??;
    identity_start_epoch_ms(&identity)
}
