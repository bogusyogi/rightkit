//! Product host adapter: unload before releasing shared model's OS lease.
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use rightkit_dictation_models::shared::store::{
    lease::{try_acquire_compat, ModelLease},
    paths,
};
use rightkit_speech_client::{HostRole, SPEECH_FAMILY};

struct State {
    lease: Option<ModelLease>,
    yielded_at: Option<Instant>,
    /// The app's own app engine never gives its model up: the user's next dictation must not
    /// pay a reload because a sibling app asked. Siblings use this host's speech service.
    keeps_model: bool,
}
static STATE: OnceLock<Mutex<State>> = OnceLock::new();

#[derive(Default)]
struct LeaseWait {
    since: Option<Instant>,
    yield_written_at: Option<Instant>,
}
static LEASE_WAIT: OnceLock<Mutex<LeaseWait>> = OnceLock::new();

impl LeaseWait {
    fn expired(&self, now: Instant, window: Duration) -> Option<Duration> {
        let waited = now.saturating_duration_since(self.since?);
        (waited >= window).then_some(waited)
    }
}

fn lease_wait_window_for(value: Option<&str>) -> Duration {
    Duration::from_secs(value.and_then(|v| v.parse::<u64>().ok()).unwrap_or(120))
}

pub(crate) fn lease_wait_window() -> Duration {
    lease_wait_window_for(
        rightkit_dictation_core::dev_var!("RIGHTKIT_MODEL_LEASE_WAIT_SECS")
            .ok()
            .as_deref(),
    )
}

pub(crate) fn lease_wait_active() -> bool {
    LEASE_WAIT
        .get()
        .is_some_and(|wait| wait.lock().since.is_some())
}

pub(crate) fn lease_wait_expired(now: Instant) -> Option<Duration> {
    LEASE_WAIT.get()?.lock().expired(now, lease_wait_window())
}

fn yield_request_body(pid: u32, app: &str, now_ms: u64) -> serde_json::Value {
    serde_json::json!({ "pid": pid, "app": app, "requested_at_ms": now_ms })
}

fn refused(now: Instant) {
    let mut wait = LEASE_WAIT
        .get_or_init(|| Mutex::new(LeaseWait::default()))
        .lock();
    let first = wait.since.is_none();
    let since = *wait.since.get_or_insert(now);
    if wait
        .yield_written_at
        .is_none_or(|t| now.saturating_duration_since(t) >= Duration::from_secs(5))
    {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u64::MAX as u128) as u64;
        let body = yield_request_body(std::process::id(), "heardright", now_ms);
        for path in request_paths() {
            let result = std::fs::create_dir_all(path.parent().expect("request parent"))
                .and_then(|_| super::files::write_json_atomic(&path, &body));
            if let Err(error) = result {
                tracing::warn!(event = "service_model_yield_request_failed", family = SPEECH_FAMILY, path = %path.display(), %error);
            }
        }
        wait.yield_written_at = Some(now);
    }
    if first {
        tracing::info!(
            event = "service_model_lease_refused",
            family = SPEECH_FAMILY,
            holder = "unknown",
            waited_ms = now.saturating_duration_since(since).as_millis() as u64
        );
    }
}

pub(crate) fn clear_lease_wait() {
    let Some(wait) = LEASE_WAIT.get() else {
        return;
    };
    let mut wait = wait.lock();
    if wait.since.is_none() {
        return;
    }
    *wait = LeaseWait::default();
    for path in request_paths() {
        if super::files::read_json::<serde_json::Value>(&path)
            .is_some_and(|r| r["pid"].as_u64() == Some(u64::from(std::process::id())))
        {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub(crate) fn install(role: HostRole, lease: Option<ModelLease>) {
    if let Some(lease) = lease {
        let _ = STATE.set(Mutex::new(State {
            lease: Some(lease),
            yielded_at: None,
            keeps_model: role == HostRole::HeardRightShell,
        }));
    }
}

fn request_paths() -> Vec<PathBuf> {
    let mut roots = vec![paths::suite_root(), paths::legacy_suite_root()];
    roots.sort();
    roots.dedup();
    roots
        .into_iter()
        .map(|root| {
            root.join("runtime/model-use")
                .join(format!("{SPEECH_FAMILY}.yield-request.json"))
        })
        .collect()
}

fn waiting_peer() -> bool {
    request_paths().iter().any(|path| {
        let request: Option<serde_json::Value> = super::files::read_json(path);
        request
            .and_then(|r| r["pid"].as_u64())
            .and_then(|pid| u32::try_from(pid).ok())
            .is_some_and(|pid| pid != std::process::id() && super::files::pid_alive(pid))
    })
}

pub(crate) fn should_yield() -> bool {
    may_yield() && waiting_peer()
}

/// True while this host holds a lease it would give to a waiting peer. Only then can
/// `should_yield` turn true without a mailbox notification (it polls the peer's request file),
/// so only then must the idle main-ASR thread keep polling.
pub(crate) fn may_yield() -> bool {
    STATE.get().is_some_and(|state| {
        let state = state.lock();
        state.lease.is_some() && !state.keeps_model
    })
}

/// Called exclusively by main ASR worker after its model has been dropped.
pub(crate) fn release() {
    if let Some(state) = STATE.get() {
        let mut state = state.lock();
        state.lease.take();
        state.yielded_at = Some(Instant::now());
        tracing::info!(
            event = "service_model_lease_yielded",
            family = SPEECH_FAMILY
        );
    }
}

/// Nonblocking so mailbox can keep prioritizing dictation over pending files.
pub(crate) fn try_resume() -> Result<bool, String> {
    let Some(state) = STATE.get() else {
        return Ok(true);
    };
    let mut state = state.lock();
    if state.lease.is_some() {
        clear_lease_wait();
        return Ok(true);
    }
    // Let waiter acquire first. Stale/dead requests never hold us back.
    if waiting_peer()
        && state
            .yielded_at
            .is_some_and(|t| t.elapsed() < Duration::from_secs(30))
    {
        refused(Instant::now());
        return Ok(false);
    }
    state.lease = try_acquire_compat(SPEECH_FAMILY, "heardright")?;
    if state.lease.is_some() {
        state.yielded_at = None;
        clear_lease_wait();
        tracing::info!(
            event = "service_model_lease_acquired",
            family = SPEECH_FAMILY
        );
    } else {
        refused(Instant::now());
    }
    Ok(state.lease.is_some())
}
