//! Client-presence routes and the session/worker supervisor index (U-F).
//!
//! The lease table, the 409 rule and the exit rule are `rightkit_daemon::loopback::Presence`
//! (`DaemonState::presence`); this file keeps only the axum glue over it and the supervisor.
//!
//! Contract:
//!   POST   /clients                → attach a client (app/CLI), 409 on a second live "app"
//!   POST   /clients/{id}/heartbeat  → keep a client's lease alive
//!   DELETE /clients/{id}            → explicit detach
//!   GET    /clients                → list live (unexpired) clients
//!
//! Exit rule: once ANY client has ever attached, the daemon becomes eligible to
//! self-exit whenever the live-client refcount drops to zero (explicit detach,
//! or heartbeat silence past `EXPIRY_SECONDS`, reaped by a background sweep) —
//! after a `LINGER_SECONDS` grace window and only if nothing is in flight. A
//! daemon that has never seen a client (bare `{app} serve` for dev/tests)
//! never self-exits — see `Presence::should_shut_down`.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::{delete, get, post},
    Router,
};
use rightkit_daemon::loopback::{AttachError, ClientRecord};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::api::{lock_recover, now_iso, now_millis, DaemonState, LiveSession};
use crate::daemon::clients::{
    ClientId, ConnectionId, DaemonSupervisor, SessionId as SupervisorSessionId,
};
use crate::daemon::idle_eviction::{
    run_idle_eviction_sweep, IdleEvictionSweepReport, MutationDrainFence,
};
use crate::daemon::residency::{
    can_passivate_session, IdleEvictionMinutes, SessionResidencySnapshot, WorkerLifecycle,
    WorkerResidencySnapshot,
};
use std::sync::Arc;

/// Process-local supervisor authority shared by HTTP attach/heartbeat/detach
/// paths. The durable client map remains the wire-facing lease record; this
/// index enforces one identity model for connection ownership and reconnects.
static CLIENT_SUPERVISOR: std::sync::OnceLock<std::sync::Mutex<DaemonSupervisor>> =
    std::sync::OnceLock::new();

fn client_supervisor() -> &'static std::sync::Mutex<DaemonSupervisor> {
    CLIENT_SUPERVISOR
        .get_or_init(|| std::sync::Mutex::new(DaemonSupervisor::start_empty(Vec::new(), 0)))
}

pub(crate) fn supervisor_attach_session(client_id: &str, session_id: &str) {
    if let Ok(mut supervisor) = client_supervisor().lock() {
        let _ = supervisor.attach_session(
            &ClientId(client_id.to_string()),
            crate::daemon::clients::SessionId(session_id.to_string()),
        );
    }
}

pub(crate) fn supervisor_pause_session(client_id: &str, session_id: &str) -> Option<String> {
    let mut supervisor = client_supervisor().lock().ok()?;
    supervisor
        .attach_session(
            &ClientId(client_id.to_string()),
            crate::daemon::clients::SessionId(session_id.to_string()),
        )
        .ok()?;
    supervisor
        .acquire_pause_lease(
            &ClientId(client_id.to_string()),
            &crate::daemon::clients::SessionId(session_id.to_string()),
        )
        .ok()
        .map(|lease| lease.0)
}

pub(crate) fn supervisor_claim_worker(worker_id: &str, session_id: &str) {
    if let Ok(mut supervisor) = client_supervisor().lock() {
        let _ = supervisor.claim_worker(
            None,
            crate::daemon::clients::SessionId(session_id.to_string()),
            crate::daemon::clients::WorkerId(worker_id.to_string()),
        );
    }
}

pub(crate) fn supervisor_resume_session(client_id: &str, session_id: &str) {
    if let Ok(mut supervisor) = client_supervisor().lock() {
        let _ = supervisor.release_pause_for_session(
            &ClientId(client_id.to_string()),
            &crate::daemon::clients::SessionId(session_id.to_string()),
        );
    }
}

pub(crate) fn supervisor_seed_startup_residency(scheduled_work: usize) {
    if let Ok(mut supervisor) = client_supervisor().lock() {
        supervisor.set_scheduled_work(scheduled_work);
        let _ = supervisor.startup_residency();
    }
}

pub(crate) fn supervisor_release_worker(worker_id: &str) {
    if let Ok(mut supervisor) = client_supervisor().lock() {
        let _ = supervisor.release_worker(
            None,
            &crate::daemon::clients::WorkerId(worker_id.to_string()),
        );
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttachClientRequest {
    kind: String,
    pid: i64,
    #[serde(default)]
    label: Option<String>,
}

pub fn client_routes() -> Router<DaemonState> {
    Router::new()
        .route("/clients", get(list_clients).post(attach_client))
        .route("/clients/{id}/heartbeat", post(heartbeat_client))
        .route("/clients/{id}", delete(detach_client))
}

async fn attach_client(
    State(state): State<DaemonState>,
    Json(req): Json<AttachClientRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let grant = state
        .presence
        .attach(&req.kind, req.pid, req.label, Instant::now())
        .map_err(|error| match error {
            AttachError::BadKind => StatusCode::BAD_REQUEST,
            AttachError::Conflict => StatusCode::CONFLICT,
            AttachError::Full => StatusCode::SERVICE_UNAVAILABLE,
        })?;

    let _ = client_supervisor()
        .lock()
        .map_err(|_| ())
        .and_then(|mut supervisor| {
            supervisor
                .register_client(
                    ClientId(grant.client_id.clone()),
                    ConnectionId(grant.client_id.clone()),
                )
                .map_err(|_| ())
        });

    Ok(Json(grant.to_json()))
}

async fn heartbeat_client(State(state): State<DaemonState>, Path(id): Path<String>) -> StatusCode {
    if state.presence.heartbeat(&id, Instant::now()) {
        if let Ok(mut supervisor) = client_supervisor().lock() {
            let _ = supervisor.heartbeat(&ClientId(id));
        }
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

async fn detach_client(State(state): State<DaemonState>, Path(id): Path<String>) -> StatusCode {
    if state.presence.detach(&id, Instant::now()) {
        if let Ok(mut supervisor) = client_supervisor().lock() {
            let _ = supervisor.disconnect(&ConnectionId(id));
        }
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

async fn list_clients(State(state): State<DaemonState>) -> Json<serde_json::Value> {
    let live: Vec<serde_json::Value> = state
        .presence
        .list(Instant::now())
        .iter()
        .map(ClientRecord::to_json)
        .collect();
    Json(serde_json::json!({ "clients": live }))
}

/// Remove expired client leases (the owner's `Presence::reap`) and release the supervisor's
/// connection for each one. Returns the number reaped.
pub(crate) fn reap_expired_clients(state: &DaemonState) -> usize {
    let now = Instant::now();
    let reaped = state.presence.reap(now);
    let live: std::collections::HashSet<String> = state
        .presence
        .list(now)
        .into_iter()
        .map(|client| client.client_id)
        .collect();
    if let Ok(mut supervisor) = client_supervisor().lock() {
        for id in supervisor.live_client_ids() {
            if !live.contains(&id.0) {
                let _ = supervisor.disconnect(&ConnectionId(id.0));
            }
        }
        // Durable wire leases expire immediately, while supervisor-owned
        // session/worker/pause relationships remain under disconnect grace.
        let _ = supervisor.reap_disconnect_grace(
            now,
            Duration::from_secs(state.presence.config().expiry_seconds),
        );
    }
    reaped
}

/// Startup guard decision: given the target health JSON of a candidate
/// existing daemon (as returned by `GET /health`) and this process's own
/// resolved database path, decide whether to refuse startup.
///
/// Returns `Ok(())` to proceed, `Err(message)` to refuse (caller prints the
/// message and exits 1). A non-OK / unreachable / malformed health response is
/// treated as "no live daemon there" — proceed. Different db paths always
/// proceed (tests use distinct temp DBs).
pub(crate) fn startup_guard_decision(
    existing_health: Option<&serde_json::Value>,
    own_db_path: &str,
    existing_url: &str,
) -> Result<(), String> {
    let Some(health) = existing_health else {
        return Ok(());
    };
    let ok = health.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    if !ok {
        return Ok(());
    }
    let Some(other_db) = health.get("databasePath").and_then(|v| v.as_str()) else {
        return Ok(());
    };
    if normalize_db_path(other_db) == normalize_db_path(own_db_path) {
        Err(format!(
            "daemon already running at {existing_url} on the same database; refusing to start a second (set RIGHTKIT_SERVE_FORCE=1 to override)"
        ))
    } else {
        Ok(())
    }
}

fn normalize_db_path(path: &str) -> String {
    std::path::Path::new(path)
        .canonicalize()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.to_string())
}

// ── RA-003: real worker residency snapshots + the production sweep ─────────
//
// `run_idle_eviction_sweep` (daemon/idle_eviction.rs) previously had no
// non-test caller: the resident `IdleEvictionLoop` only reaped expired
// client leases. The pieces below build a `WorkerResidencySnapshot` per
// supervisor-owned worker from live `DaemonState` (in-memory sessions, the
// client supervisor's attachment map, and `LiveTurnHandle` busy state) and
// drive the sweep from `main_runtime_server.rs`'s idle-eviction timer.
//
// `has_registered_cron_job` and `has_durable_scheduled_work` are always
// `false`: there is no session-scoped cron/durable-schedule registry
// anywhere in this codebase to query honestly (schedules are keyed by
// recipe/bot id, not by a live session id). The residency predicates in
// `daemon/residency.rs` are unmodified and remain fail-closed on every
// signal this producer *can* supply (busy, attached, descendant, missing
// timestamp) — see that module's doc comment for the full predicate list.

/// Process-local idle-eviction mutation fence, mirroring `CLIENT_SUPERVISOR`'s
/// `OnceLock` pattern above. One fence per daemon process guards every
/// mutating session handler against a concurrent eviction sweep.
static IDLE_MUTATION_FENCE: std::sync::OnceLock<MutationDrainFence> = std::sync::OnceLock::new();

pub(crate) fn idle_mutation_fence() -> &'static MutationDrainFence {
    IDLE_MUTATION_FENCE.get_or_init(MutationDrainFence::new)
}

/// Idle-eviction policy, read once from `RIGHTKIT_IDLE_EVICTION_MINUTES`.
/// Unset or `0` keeps the existing (pre-RA-003) behavior — nothing is ever
/// evicted — so wiring this sweep into the resident loop does not change any
/// running daemon's behavior until an operator explicitly opts in.
pub(crate) fn idle_eviction_policy_from_env() -> IdleEvictionMinutes {
    match std::env::var("RIGHTKIT_IDLE_EVICTION_MINUTES")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
    {
        Some(0) | None => IdleEvictionMinutes::Off,
        Some(minutes) => IdleEvictionMinutes::minutes(minutes),
    }
}

/// Parse a `now_iso()`-formatted (`YYYY-MM-DDTHH:MM:SSZ`) timestamp back to
/// milliseconds since epoch. Returns `None` on any malformed input so a
/// corrupt/foreign timestamp fails closed (`MissingActivityTimestamp`)
/// instead of being silently treated as "just active".
fn parse_iso_to_millis(value: &str) -> Option<u128> {
    let seconds = rightkit_time::DateTime::parse_rfc3339(value)?.unix_seconds();
    u128::try_from(seconds)
        .ok()
        .map(|seconds| seconds.saturating_mul(1000))
}

/// Parent → direct-children map over every currently live in-memory session,
/// used to walk one worker's tree (root session plus subagent descendants).
fn session_children_map(
    sessions: &HashMap<String, Arc<LiveSession>>,
) -> HashMap<String, Vec<String>> {
    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    for session in sessions.values() {
        if let Some(parent) = &session.parent_session_id {
            children
                .entry(parent.clone())
                .or_default()
                .push(session.id.clone());
        }
    }
    children
}

/// Pre-order ids of `root` plus every transitive descendant.
fn session_tree_ids(children: &HashMap<String, Vec<String>>, root: &str) -> Vec<String> {
    let mut result = vec![root.to_string()];
    let mut stack = vec![root.to_string()];
    while let Some(id) = stack.pop() {
        if let Some(kids) = children.get(&id) {
            for kid in kids {
                result.push(kid.clone());
                stack.push(kid.clone());
            }
        }
    }
    result
}

/// True if any transitive descendant of `id` currently owns a live engine
/// turn (`LiveTurnHandle::is_active`). This is the busy signal that must
/// block passivation/eviction of an ancestor even when the ancestor itself
/// is idle.
fn has_active_descendant(
    sessions: &HashMap<String, Arc<LiveSession>>,
    children: &HashMap<String, Vec<String>>,
    id: &str,
) -> bool {
    let Some(kids) = children.get(id) else {
        return false;
    };
    kids.iter().any(|kid| {
        sessions
            .get(kid)
            .is_some_and(|session| session.live_turn_handle.is_active())
            || has_active_descendant(sessions, children, kid)
    })
}

fn session_residency_snapshot(
    sessions: &HashMap<String, Arc<LiveSession>>,
    children: &HashMap<String, Vec<String>>,
    id: &str,
) -> SessionResidencySnapshot {
    let Some(session) = sessions.get(id) else {
        // The tree was walked a moment ago and this id is already gone
        // (concurrent removal). Fail closed: never eligible.
        return SessionResidencySnapshot {
            is_session_active: true,
            has_active_descendant: false,
            attached_clients: 0,
            has_registered_heartbeat: false,
            has_registered_cron_job: false,
            has_durable_scheduled_work: false,
            last_activity_at_ms: None,
        };
    };
    let attached_clients = supervisor_attached_client_count(id);
    let updated_at = lock_recover(&session.updated_at, "session.updated_at").clone();
    SessionResidencySnapshot {
        is_session_active: session.live_turn_handle.is_active(),
        has_active_descendant: has_active_descendant(sessions, children, id),
        attached_clients,
        // No independent heartbeat registry exists beyond client attachment
        // (DL-006's supervisor is the sole presence authority) — an attached
        // live client is the only "heartbeat" this daemon can observe.
        has_registered_heartbeat: attached_clients != 0,
        has_registered_cron_job: false,
        has_durable_scheduled_work: false,
        last_activity_at_ms: parse_iso_to_millis(&updated_at),
    }
}

/// Build one worker's residency snapshot from live `DaemonState`. Public so
/// tests in this module and `main_runtime_server.rs`'s wiring share one
/// implementation.
pub(crate) fn build_worker_residency_snapshot(
    state: &DaemonState,
    worker: &crate::daemon::clients::WorkerOwner,
) -> WorkerResidencySnapshot {
    let sessions = lock_recover(&state.sessions, "sessions");
    let root_id = worker.session_id.0.as_str();
    if !sessions.contains_key(root_id) {
        // The claimed root session no longer exists in memory (already
        // ended some other way). Fail closed: not ready, no sessions.
        return WorkerResidencySnapshot {
            lifecycle: WorkerLifecycle::Failed,
            is_connected: false,
            is_stopping: false,
            has_owner_client: false,
            is_preparing_update_restart: false,
            sessions: Vec::new(),
        };
    }
    let children = session_children_map(&sessions);
    let tree_ids = session_tree_ids(&children, root_id);
    let session_snapshots = tree_ids
        .iter()
        .map(|id| session_residency_snapshot(&sessions, &children, id))
        .collect();
    let has_owner_client = worker
        .client_id
        .as_ref()
        .is_some_and(|client_id| supervisor_client_is_live(&client_id.0));
    let is_connected = worker.client_id.is_none() || has_owner_client;
    WorkerResidencySnapshot {
        lifecycle: WorkerLifecycle::Ready,
        is_connected,
        is_stopping: false,
        has_owner_client,
        is_preparing_update_restart: false,
        sessions: session_snapshots,
    }
}

/// Same tree-walk `build_worker_residency_snapshot` performs, returned as
/// bare ids (not snapshots) so eviction/passivation can act on them.
/// `SessionResidencySnapshot` carries no identity, so this is recomputed
/// rather than threaded through the sweep's generic `WorkerResidencySnapshot`
/// return type.
fn worker_tree_ids(
    state: &DaemonState,
    worker: &crate::daemon::clients::WorkerOwner,
) -> Vec<String> {
    let sessions = lock_recover(&state.sessions, "sessions");
    let root_id = worker.session_id.0.as_str();
    if !sessions.contains_key(root_id) {
        return Vec::new();
    }
    let children = session_children_map(&sessions);
    session_tree_ids(&children, root_id)
}

/// Interrupt and remove every live in-memory session in `ids`, then
/// best-effort persist a terminal "dormant" status for each — the same
/// shutdown shape `kill_session` uses (see `api_sessions/lifecycle.rs`), so
/// an evicted worker leaves storage in the same state an explicit kill does.
fn terminate_sessions(state: &DaemonState, ids: &[String]) {
    if ids.is_empty() {
        return;
    }
    {
        let sessions = lock_recover(&state.sessions, "sessions");
        for id in ids {
            if let Some(session) = sessions.get(id) {
                if let Some(tx) = &session.interrupt_tx {
                    let _ = tx.send(true);
                }
            }
        }
    }
    {
        let mut sessions = lock_recover(&state.sessions, "sessions");
        for id in ids {
            sessions.remove(id);
        }
    }
    if let Some(store) = state.storage.clone() {
        let now = now_iso();
        let ids = ids.to_vec();
        tokio::spawn(async move {
            let s = store.lock().await;
            for id in ids {
                if let Err(error) = s.update_session_status(&id, "dormant", &now) {
                    tracing::warn!(session_id = %id, %error, "idle eviction status update failed");
                }
            }
        });
    }
}

/// Whole-tree eviction (RA-003): stop every session under `worker`, matching
/// `evict_worker`'s contract in `run_idle_eviction_sweep` ("must stop the
/// complete worker tree, not only its root session").
pub(crate) fn evict_worker_tree(state: &DaemonState, worker: &crate::daemon::clients::WorkerOwner) {
    let ids = worker_tree_ids(state, worker);
    terminate_sessions(state, &ids);
    supervisor_release_worker(&worker.worker_id.0);
}

/// Passivate only the non-root descendants of `worker` that
/// `can_passivate_session` allows. The root session is never passivated here
/// — it remains owned by whole-worker eviction, per `residency.rs`'s doc
/// comment on `can_passivate_session`.
pub(crate) fn passivate_idle_children(
    state: &DaemonState,
    worker: &crate::daemon::clients::WorkerOwner,
    policy: IdleEvictionMinutes,
    now_ms: u128,
) -> Result<(), String> {
    let root_id = worker.session_id.0.clone();
    let to_passivate: Vec<String> = {
        let sessions = lock_recover(&state.sessions, "sessions");
        if !sessions.contains_key(root_id.as_str()) {
            return Ok(());
        }
        let children = session_children_map(&sessions);
        let tree_ids = session_tree_ids(&children, &root_id);
        tree_ids
            .into_iter()
            .filter(|id| id != &root_id)
            .filter(|id| sessions.contains_key(id))
            .filter(|id| {
                let snapshot = session_residency_snapshot(&sessions, &children, id);
                let has_non_passive_descendants = children.get(id).is_some_and(|kids| {
                    kids.iter().any(|kid| {
                        has_active_descendant(&sessions, &children, kid)
                            || sessions
                                .get(kid)
                                .is_some_and(|s| s.live_turn_handle.is_active())
                    })
                });
                can_passivate_session(
                    &snapshot,
                    true,
                    has_non_passive_descendants,
                    false,
                    policy,
                    now_ms,
                )
            })
            .collect()
    };
    terminate_sessions(state, &to_passivate);
    Ok(())
}

static IDLE_PASSIVATIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static IDLE_EVICTIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Content-free process-lifetime counters from completed idle sweeps.
pub(crate) fn idle_resource_metrics() -> serde_json::Value {
    serde_json::json!({
        "passivations": IDLE_PASSIVATIONS.load(Ordering::Relaxed),
        "evictions": IDLE_EVICTIONS.load(Ordering::Relaxed),
        "mutationDrainFenced": idle_mutation_fence().is_fenced(),
        "activeMutations": idle_mutation_fence().active_mutations(),
    })
}

/// The production idle-eviction sweep (RA-003): the resident timer's
/// non-test caller for `run_idle_eviction_sweep`. Builds real snapshots from
/// `state`, refreshes every candidate under the mutation fence before
/// evicting (per `run_idle_eviction_sweep`'s own re-check contract), and
/// returns the same report the timer used to discard.
pub(crate) fn run_daemon_idle_eviction_sweep(
    state: &DaemonState,
    policy: IdleEvictionMinutes,
) -> IdleEvictionSweepReport {
    let workers = client_supervisor()
        .lock()
        .map(|supervisor| supervisor.worker_owners())
        .unwrap_or_default();
    let now_ms = now_millis();
    let report = run_idle_eviction_sweep(
        workers,
        policy,
        now_ms,
        idle_mutation_fence(),
        Duration::from_secs(5),
        |worker| Ok(build_worker_residency_snapshot(state, worker)),
        |worker, _snapshot, policy, now_ms| passivate_idle_children(state, worker, policy, now_ms),
        |worker| {
            evict_worker_tree(state, worker);
            Ok(())
        },
    );
    IDLE_PASSIVATIONS.fetch_add(report.passivated_workers as u64, Ordering::Relaxed);
    IDLE_EVICTIONS.fetch_add(report.evicted_workers as u64, Ordering::Relaxed);
    report
}

pub(crate) fn supervisor_worker_owners() -> Vec<crate::daemon::clients::WorkerOwner> {
    client_supervisor()
        .lock()
        .map(|supervisor| supervisor.worker_owners())
        .unwrap_or_default()
}

pub(crate) fn supervisor_attached_client_count(session_id: &str) -> usize {
    client_supervisor()
        .lock()
        .map(|supervisor| {
            supervisor.attached_client_count(&SupervisorSessionId(session_id.to_string()))
        })
        .unwrap_or(0)
}

pub(crate) fn supervisor_client_is_live(client_id: &str) -> bool {
    client_supervisor()
        .lock()
        .map(|supervisor| supervisor.client_is_live(&ClientId(client_id.to_string())))
        .unwrap_or(false)
}
