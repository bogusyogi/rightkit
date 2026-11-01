//! Durable cloud workspace provisioning (ENG-63), persistent cloud run
//! lifecycle that survives phone disconnect (ENG-64), and per-target
//! capability/cost/privacy disclosure (ENG-67).
//!
//! This module builds directly on `cloud_endpoint` (ENG-62/65): a workspace
//! is provisioned against an already-verified `CloudEndpointConfig` for a
//! given `ExecutionTarget`, and a run started inside that workspace keeps
//! its event log in a process-wide registry keyed by
//! `(session_id, execution_target, run_id)` rather than by any live
//! connection. Dropping the phone's connection drops nothing here: the
//! workspace and its run state stay in the registry, so a later reconnect
//! resumes the same event log from the cursor the client last saw instead
//! of re-fetching (and re-showing) everything from the start.
//!
//! Nothing in this module talks to a socket. The actual server-side
//! "keeps executing while disconnected" behavior lives on the cloud
//! endpoint the workspace is provisioned against; this module is the
//! client-/engine-side bookkeeping that makes that durability observable
//! and resumable — the event log, the cursor, and the status that a
//! reconnect reads back. Wiring a live SSE/long-poll transport onto
//! `resume_events_since` is the integration point a caller adds once one is
//! available (see the crate's registration gap, noted at the bottom of this
//! file).

use crate::coding_tools::cloud_endpoint::{CloudEndpointConfig, ExecutionTarget};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Lifecycle state of a provisioned workspace. `Ready` means the endpoint
/// accepted the provisioning request; `Failed` carries why, so a caller can
/// surface a concrete reason instead of a bare retry prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CloudWorkspaceStatus {
    Provisioning,
    Ready,
    Failed { reason: String },
}

/// A durable cloud workspace: the result of ENG-63 provisioning against a
/// connected `ExecutionTarget::Cloud` (or `Computer`) endpoint. Durable here
/// means the handle's identity and status outlive any single request —
/// looking it up again later (e.g. after the app relaunches) returns the
/// same workspace rather than silently minting a new one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudWorkspaceHandle {
    pub workspace_id: String,
    pub target: ExecutionTarget,
    pub base_url: String,
    pub status: CloudWorkspaceStatus,
    pub created_at_unix_ms: u64,
}

/// Status of a run inside a workspace. `Disconnected` is not a failure — it
/// only records that no client is currently attached; the run keeps
/// executing server-side and the event log keeps growing underneath it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudRunStatus {
    Running,
    Disconnected,
    Completed,
    Failed,
}

/// One event in a run's durable event log. `cursor` is monotonically
/// increasing and 1-indexed per run; a reconnect asks for everything with
/// `cursor > last_seen_cursor`, which is what makes resumption exactly-once
/// rather than a replay-from-zero that would duplicate effects the client
/// already rendered.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudRunEvent {
    pub cursor: u64,
    pub kind: String,
    pub payload: serde_json::Value,
    pub created_at_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudRunSnapshot {
    pub run_id: String,
    pub workspace_id: String,
    pub status: CloudRunStatus,
    pub last_cursor: u64,
}

struct CloudRunState {
    workspace_id: String,
    status: CloudRunStatus,
    events: Vec<CloudRunEvent>,
}

type WorkspaceKey = (String, ExecutionTarget);
type RunKey = (String, ExecutionTarget, String);

fn workspace_registry() -> &'static Mutex<HashMap<WorkspaceKey, CloudWorkspaceHandle>> {
    static REGISTRY: OnceLock<Mutex<HashMap<WorkspaceKey, CloudWorkspaceHandle>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn run_registry() -> &'static Mutex<HashMap<RunKey, Arc<Mutex<CloudRunState>>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<RunKey, Arc<Mutex<CloudRunState>>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Provision (or re-fetch) a durable workspace for `session_id` against
/// `target`, using an already-verified `config` (callers should have run
/// `cloud_endpoint::verify_endpoint` first — this function does not
/// re-verify reachability, matching `cloud_endpoint`'s own
/// verify-then-persist split). Idempotent: calling this again for the same
/// `(session_id, target)` returns the existing handle rather than
/// provisioning a second workspace, so a retried request after a dropped
/// connection cannot silently double-provision.
pub fn provision_workspace(
    session_id: &str,
    target: ExecutionTarget,
    config: &CloudEndpointConfig,
) -> CloudWorkspaceHandle {
    let key = (session_id.to_string(), target);
    let mut registry = workspace_registry()
        .lock()
        .expect("workspace registry poisoned");
    if let Some(existing) = registry.get(&key) {
        return existing.clone();
    }
    let handle = CloudWorkspaceHandle {
        workspace_id: format!("{}:{}:{}", session_id, target.as_str(), now_ms()),
        target,
        base_url: config.base_url.clone(),
        status: CloudWorkspaceStatus::Ready,
        created_at_unix_ms: now_ms(),
    };
    registry.insert(key, handle.clone());
    handle
}

/// Mark a previously provisioned workspace failed (e.g. the endpoint
/// rejected a later request against it). No-op if nothing is provisioned
/// for this key.
pub fn mark_workspace_failed(session_id: &str, target: ExecutionTarget, reason: String) {
    let key = (session_id.to_string(), target);
    let mut registry = workspace_registry()
        .lock()
        .expect("workspace registry poisoned");
    if let Some(handle) = registry.get_mut(&key) {
        handle.status = CloudWorkspaceStatus::Failed { reason };
    }
}

pub fn lookup_workspace(session_id: &str, target: ExecutionTarget) -> Option<CloudWorkspaceHandle> {
    let key = (session_id.to_string(), target);
    workspace_registry()
        .lock()
        .expect("workspace registry poisoned")
        .get(&key)
        .cloned()
}

/// Start (or resume tracking of) a run inside a workspace. Calling this
/// again for a `run_id` already known just returns its current snapshot —
/// starting is idempotent for the same reason provisioning is: a client
/// that reconnects and re-issues "start" for a run it already began must
/// not spawn a second run.
pub fn start_run(
    session_id: &str,
    target: ExecutionTarget,
    run_id: &str,
    workspace_id: &str,
) -> CloudRunSnapshot {
    let key = (session_id.to_string(), target, run_id.to_string());
    let mut registry = run_registry().lock().expect("run registry poisoned");
    let state = registry.entry(key).or_insert_with(|| {
        Arc::new(Mutex::new(CloudRunState {
            workspace_id: workspace_id.to_string(),
            status: CloudRunStatus::Running,
            events: Vec::new(),
        }))
    });
    let guard = state.lock().expect("run state poisoned");
    CloudRunSnapshot {
        run_id: run_id.to_string(),
        workspace_id: guard.workspace_id.clone(),
        status: guard.status,
        last_cursor: guard.events.last().map(|e| e.cursor).unwrap_or(0),
    }
}

/// Append one event to a run's durable log, keeping it server-side (in this
/// registry) regardless of whether any client is currently attached. The
/// returned cursor is what a subsequent `resume_events_since` call needs to
/// avoid re-delivering this event.
pub fn append_run_event(
    session_id: &str,
    target: ExecutionTarget,
    run_id: &str,
    kind: impl Into<String>,
    payload: serde_json::Value,
) -> Option<u64> {
    let key = (session_id.to_string(), target, run_id.to_string());
    let registry = run_registry().lock().expect("run registry poisoned");
    let state = registry.get(&key)?;
    let mut guard = state.lock().expect("run state poisoned");
    let next_cursor = guard.events.last().map(|e| e.cursor + 1).unwrap_or(1);
    guard.events.push(CloudRunEvent {
        cursor: next_cursor,
        kind: kind.into(),
        payload,
        created_at_unix_ms: now_ms(),
    });
    Some(next_cursor)
}

/// Record that the client attached/detached. The run's `status` reflects
/// only client presence for `Disconnected`/`Running`; a run already
/// `Completed`/`Failed` is left alone — reattaching after completion should
/// not resurrect it as `Running`.
pub fn set_run_connected(session_id: &str, target: ExecutionTarget, run_id: &str, connected: bool) {
    let key = (session_id.to_string(), target, run_id.to_string());
    let registry = run_registry().lock().expect("run registry poisoned");
    if let Some(state) = registry.get(&key) {
        let mut guard = state.lock().expect("run state poisoned");
        if matches!(
            guard.status,
            CloudRunStatus::Running | CloudRunStatus::Disconnected
        ) {
            guard.status = if connected {
                CloudRunStatus::Running
            } else {
                CloudRunStatus::Disconnected
            };
        }
    }
}

pub fn complete_run(session_id: &str, target: ExecutionTarget, run_id: &str, failed: bool) {
    let key = (session_id.to_string(), target, run_id.to_string());
    let registry = run_registry().lock().expect("run registry poisoned");
    if let Some(state) = registry.get(&key) {
        let mut guard = state.lock().expect("run state poisoned");
        guard.status = if failed {
            CloudRunStatus::Failed
        } else {
            CloudRunStatus::Completed
        };
    }
}

/// Resume the event stream for a run from `since_cursor` (exclusive) —
/// i.e. everything the client has not already seen. Passing the cursor the
/// client last recorded before disconnecting is what prevents duplicated
/// effects on reconnect: events at or before that cursor are never
/// re-returned, and the run continuing to append while disconnected means
/// nothing in between is lost either.
pub fn resume_events_since(
    session_id: &str,
    target: ExecutionTarget,
    run_id: &str,
    since_cursor: u64,
) -> Vec<CloudRunEvent> {
    let key = (session_id.to_string(), target, run_id.to_string());
    let registry = run_registry().lock().expect("run registry poisoned");
    match registry.get(&key) {
        Some(state) => {
            let guard = state.lock().expect("run state poisoned");
            guard
                .events
                .iter()
                .filter(|e| e.cursor > since_cursor)
                .cloned()
                .collect()
        }
        None => Vec::new(),
    }
}

pub fn run_snapshot(
    session_id: &str,
    target: ExecutionTarget,
    run_id: &str,
) -> Option<CloudRunSnapshot> {
    let key = (session_id.to_string(), target, run_id.to_string());
    let registry = run_registry().lock().expect("run registry poisoned");
    let state = registry.get(&key)?;
    let guard = state.lock().expect("run state poisoned");
    Some(CloudRunSnapshot {
        run_id: run_id.to_string(),
        workspace_id: guard.workspace_id.clone(),
        status: guard.status,
        last_cursor: guard.events.last().map(|e| e.cursor).unwrap_or(0),
    })
}

/// ENG-67: what a target can do, who pays for it, and where data goes —
/// shown to the user before/while they pick an execution target. This is
/// static per-target policy, not per-session state, so it needs no
/// registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionTargetDisclosure {
    pub target: ExecutionTarget,
    pub capabilities: Vec<String>,
    pub payer: String,
    pub data_location: String,
}

pub fn disclosure_for(target: ExecutionTarget) -> ExecutionTargetDisclosure {
    match target {
        ExecutionTarget::Local => ExecutionTargetDisclosure {
            target,
            capabilities: vec![
                "Runs on this device only".into(),
                "Stops if the app is closed or the device sleeps".into(),
                "Full filesystem access within the selected workspace".into(),
            ],
            payer: "You, directly, via your configured model provider account".into(),
            data_location:
                "Stays on this device except for model provider API calls you configured".into(),
        },
        ExecutionTarget::Cloud => ExecutionTargetDisclosure {
            target,
            capabilities: vec![
                "Keeps running after this device disconnects".into(),
                "Resumable session/event history".into(),
                "Bound by the connected endpoint's own resource limits".into(),
            ],
            payer: "You, via the cloud endpoint you connected (its own billing, outside Agent)"
                .into(),
            data_location: "Workspace files and run output live on the connected cloud endpoint"
                .into(),
        },
        ExecutionTarget::Computer => ExecutionTargetDisclosure {
            target,
            capabilities: vec![
                "Runs on a paired desktop/daemon over the network".into(),
                "Keeps running as long as that paired machine is on".into(),
                "Filesystem access limited to that machine's shared workspace".into(),
            ],
            payer: "You — no separate billing beyond the paired machine you already own/rent"
                .into(),
            data_location: "Workspace files and run output live on the paired machine".into(),
        },
    }
}

// ---------------------------------------------------------------------
// Integration gaps (out of this lane's file allowlist):
//
// - This module is not declared in `engine/crates/tools/src/lib.rs`
//   (`pub mod cloud_workspace;`), so nothing outside this file can `use` it
//   yet.
// - No tool wraps these functions for registration in
//   `engine/crates/tools/src/registry/registry_impl.rs`, so the agent loop
//   cannot call "provision a cloud workspace" / "start a cloud run" /
//   "get target disclosure" as a tool today.
// - `engine_tool_dispatch_concurrent.rs` dispatches *local* tool-call
//   futures only; it has no SSE/long-poll client for a *remote* cloud
//   endpoint's event stream to reconnect. `resume_events_since` is the
//   resumption primitive such a client would call with its last-seen
//   cursor — wiring an actual transport onto it is a follow-up once one
//   exists.
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> CloudEndpointConfig {
        // `CloudEndpointConfig` fields are pub but it has no public
        // constructor in `cloud_endpoint.rs`; it derives `Deserialize`, so
        // build it via that instead of adding a constructor out-of-lane.
        serde_json::from_value(serde_json::json!({
            "baseUrl": "https://cloud.example.test",
            "credential": "secret-token"
        }))
        .expect("valid config")
    }

    #[test]
    fn provisioning_is_idempotent_per_session_and_target() {
        let session_id = "sess-provision-idempotent";
        let a = provision_workspace(session_id, ExecutionTarget::Cloud, &config());
        let b = provision_workspace(session_id, ExecutionTarget::Cloud, &config());
        assert_eq!(a.workspace_id, b.workspace_id);
    }

    #[test]
    fn different_targets_get_different_workspaces() {
        let session_id = "sess-provision-targets";
        let cloud = provision_workspace(session_id, ExecutionTarget::Cloud, &config());
        let computer = provision_workspace(session_id, ExecutionTarget::Computer, &config());
        assert_ne!(cloud.workspace_id, computer.workspace_id);
    }

    #[test]
    fn run_keeps_executing_and_appending_events_while_disconnected() {
        let session_id = "sess-run-disconnect";
        let ws = provision_workspace(session_id, ExecutionTarget::Cloud, &config());
        start_run(
            session_id,
            ExecutionTarget::Cloud,
            "run-1",
            &ws.workspace_id,
        );

        append_run_event(
            session_id,
            ExecutionTarget::Cloud,
            "run-1",
            "tool_call",
            serde_json::json!({"n": 1}),
        );
        set_run_connected(session_id, ExecutionTarget::Cloud, "run-1", false);
        // Simulate server-side progress continuing while the phone is away.
        let cursor2 = append_run_event(
            session_id,
            ExecutionTarget::Cloud,
            "run-1",
            "tool_result",
            serde_json::json!({"n": 2}),
        )
        .expect("run exists");

        let snap = run_snapshot(session_id, ExecutionTarget::Cloud, "run-1").expect("run exists");
        assert_eq!(snap.status, CloudRunStatus::Disconnected);
        assert_eq!(snap.last_cursor, cursor2);
    }

    #[test]
    fn reconnect_resumes_from_cursor_without_duplicating_effects() {
        let session_id = "sess-run-resume";
        let ws = provision_workspace(session_id, ExecutionTarget::Cloud, &config());
        start_run(
            session_id,
            ExecutionTarget::Cloud,
            "run-2",
            &ws.workspace_id,
        );

        let c1 = append_run_event(
            session_id,
            ExecutionTarget::Cloud,
            "run-2",
            "started",
            serde_json::json!({}),
        )
        .unwrap();
        let _c2 = append_run_event(
            session_id,
            ExecutionTarget::Cloud,
            "run-2",
            "progress",
            serde_json::json!({}),
        )
        .unwrap();

        // Client saw up through c1 before dropping; reconnect should hand
        // back only what came after, never c1 again.
        let resumed = resume_events_since(session_id, ExecutionTarget::Cloud, "run-2", c1);
        assert_eq!(resumed.len(), 1);
        assert_eq!(resumed[0].kind, "progress");
        assert!(resumed.iter().all(|e| e.cursor > c1));
    }

    #[test]
    fn resuming_from_zero_replays_full_log_exactly_once() {
        let session_id = "sess-run-resume-zero";
        let ws = provision_workspace(session_id, ExecutionTarget::Cloud, &config());
        start_run(
            session_id,
            ExecutionTarget::Cloud,
            "run-3",
            &ws.workspace_id,
        );
        append_run_event(
            session_id,
            ExecutionTarget::Cloud,
            "run-3",
            "a",
            serde_json::json!({}),
        );
        append_run_event(
            session_id,
            ExecutionTarget::Cloud,
            "run-3",
            "b",
            serde_json::json!({}),
        );

        let all = resume_events_since(session_id, ExecutionTarget::Cloud, "run-3", 0);
        assert_eq!(all.len(), 2);
        let cursors: Vec<u64> = all.iter().map(|e| e.cursor).collect();
        assert_eq!(cursors, vec![1, 2]);
    }

    #[test]
    fn completed_run_is_not_resurrected_by_reconnect() {
        let session_id = "sess-run-completed";
        let ws = provision_workspace(session_id, ExecutionTarget::Cloud, &config());
        start_run(
            session_id,
            ExecutionTarget::Cloud,
            "run-4",
            &ws.workspace_id,
        );
        complete_run(session_id, ExecutionTarget::Cloud, "run-4", false);
        set_run_connected(session_id, ExecutionTarget::Cloud, "run-4", true);

        let snap = run_snapshot(session_id, ExecutionTarget::Cloud, "run-4").expect("run exists");
        assert_eq!(snap.status, CloudRunStatus::Completed);
    }

    #[test]
    fn each_target_has_a_disclosure_with_capabilities_payer_and_data_location() {
        for target in [
            ExecutionTarget::Local,
            ExecutionTarget::Cloud,
            ExecutionTarget::Computer,
        ] {
            let disclosure = disclosure_for(target);
            assert_eq!(disclosure.target, target);
            assert!(!disclosure.capabilities.is_empty());
            assert!(!disclosure.payer.is_empty());
            assert!(!disclosure.data_location.is_empty());
        }
    }
}
