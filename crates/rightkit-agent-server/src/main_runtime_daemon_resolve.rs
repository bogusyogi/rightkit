// ── Daemon client helpers ─────────────────────────────────────────────────────

/// Machine-readable daemon state used by `{app} status`.
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
/// 1. `<PREFIX>_DAEMON_URL` + `<PREFIX>_DAEMON_TOKEN` env vars, only on a host whose
///    `host_options::daemon_endpoint_from_environment` is set (a host that follows client API R32
///    never reads them).
/// 2. `<state root>/daemon.port` — JSON file written by `serve` on startup.
pub(crate) fn resolve_daemon() -> Option<(String, String)> {
    match resolve_daemon_state(true) {
        DaemonResolution::Resolved(resolved) => Some(resolved),
        DaemonResolution::Stopped | DaemonResolution::Unreachable { .. } => None,
    }
}

/// [`resolve_daemon`] with only the quick health attempts (2 x 500 ms). For the autostart
/// poll loops in `cli_presence`, which repeat on their own schedule: the patient attempt
/// there would stretch an attempt-counted loop from about 20 s to minutes.
pub(crate) fn resolve_daemon_quick() -> Option<(String, String)> {
    match resolve_daemon_state(false) {
        DaemonResolution::Resolved(resolved) => Some(resolved),
        DaemonResolution::Stopped | DaemonResolution::Unreachable { .. } => None,
    }
}

/// `patient`: the second health attempt waits [`BUSY_DAEMON_HEALTH_TIMEOUT`] instead of 500 ms.
fn resolve_daemon_state(patient: bool) -> DaemonResolution {
    let (url, tok) = if crate::host_options::daemon_endpoint_from_environment() {
        (
            std::env::var(rightkit_agent_config::env_name("DAEMON_URL")).ok(),
            std::env::var(rightkit_agent_config::env_name("DAEMON_TOKEN")).ok(),
        )
    } else {
        (None, None)
    };
    let state_dir = rightkit_agent_config::state_root().map(|p| p.display().to_string());
    resolve_daemon_state_from_sources(url, tok, state_dir.as_deref(), patient)
}

fn resolve_daemon_state_from_sources(
    url: Option<String>,
    tok: Option<String>,
    state_dir: Option<&str>,
    patient: bool,
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
    if info.protocol_version != rightkit_agent_engine_api_platform::health::PROTOCOL_VERSION
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
    if !blocking_health_reachable_repeated(&url, patient) {
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

/// Response budget of the second health attempt. W2-10: with 64 chats starting at once the
/// daemon's runtime is busy (session creates, for one, run synchronously on its workers) and
/// `/health` can answer in seconds, not in 500 ms. Two 500 ms attempts then classified a live daemon,
/// whose pid and start time were just verified, as unreachable; `run` fell back to
/// the inline writer lock, found the daemon holding it, and gave up after its 10 s wait
/// ("holds the database lock ... and no daemon answers", exit 3, about 12.6 s).
/// A refused connection still fails at once whatever this budget, so only a daemon whose
/// listener accepts but does not answer pays it.
/// The cost, paid only by a wedged daemon (accepts, never answers): about 10.8 s per resolve, so
/// `status` takes about 11 s, a headless `run` about 21 s before its exit 3 (the first
/// resolve plus one pass of its 10 s lock wait), and each `cli_presence` poll up to about 21 s
/// (resolve plus its 2xx check, which only bites if the daemon answered and then wedged).
/// This is a client-side mitigation, not the cure: the daemon-side cause (synchronous session
/// creation under `SESSION_CREATE_GATE` on the runtime workers, `api_sessions/lifecycle.rs`) is
/// recorded as a cross-lane change in docs/reports/audit-2026-11-06/w2daemon.md.
pub(crate) const BUSY_DAEMON_HEALTH_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(10);

/// Reachability probe, safe to call from sync *or* async callers: the blocking
/// `rightkit_http::Client` owns no async runtime, so there is nothing to drop inside
/// an async context. A quick attempt first, then one more: patient
/// ([`BUSY_DAEMON_HEALTH_TIMEOUT`]) when `patient`, else another 500 ms.
fn blocking_health_reachable_repeated(base_url: &str, patient: bool) -> bool {
    let second_budget = if patient {
        BUSY_DAEMON_HEALTH_TIMEOUT
    } else {
        std::time::Duration::from_millis(500)
    };
    let probe = |response_timeout: std::time::Duration| {
        let mut policy = rightkit_http::AddressPolicy::public_web();
        if let Some(host) = rightkit_agent_daemon::http_client::host_of(base_url) {
            policy = policy.allow_if(vec![host], |_| true);
        }
        rightkit_http::Client::new(rightkit_http::ClientConfig {
            connect_timeout: Some(std::time::Duration::from_millis(300)),
            request_timeout: response_timeout,
            response_timeout: Some(response_timeout),
            retry: rightkit_http::RetryPolicy::none(),
            address_policy: policy,
            proxy: rightkit_http::ProxyMode::None,
            ..rightkit_http::ClientConfig::default()
        })
        .send_any(&rightkit_http::Request::get(format!("{base_url}/health")))
        // Any HTTP response proves transport reachability. HTTP
        // health failures are classified as degraded by async status.
        .is_ok()
    };
    repeated_health_probe(
        || probe(std::time::Duration::from_millis(500)),
        || std::thread::sleep(std::time::Duration::from_millis(150)),
        || probe(second_budget),
    )
}

fn repeated_health_probe(
    first: impl FnOnce() -> bool,
    between_attempts: impl FnOnce(),
    second: impl FnOnce() -> bool,
) -> bool {
    if first() {
        return true;
    }
    between_attempts();
    second()
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
