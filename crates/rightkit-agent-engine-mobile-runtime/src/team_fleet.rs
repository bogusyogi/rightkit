//! iOS bridge for ENG-96/97 portable peer fleets.
//!
//! Swift remains owner of the user's manifest and shared brief. Every bridge
//! call rebuilds a [`TeamRuntime`] from those user-owned inputs, while its
//! SQLite mailbox persists only imported fleet identity and dispatched work.
//! A dispatch cannot contact a peer until the host explicitly marks this
//! exact action user-approved; the shared engine then mints its opaque,
//! requester/target-bound approval before writing one mailbox item.

use std::collections::BTreeMap;
use std::ffi::{c_char, CStr, CString};
use std::path::PathBuf;

use rightkit_agent_orchestration::teams::section_context::SectionContext;
use rightkit_agent_orchestration::teams::{
    import_manifest, DelegationRequest, PeerDispatch, ReceivedPeerDelegation,
    TeamCoordinationStore, TeamRuntime,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FleetRequest {
    storage_dir: PathBuf,
    manifest_json: String,
    #[serde(default)]
    shared_context: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FleetDispatchRequest {
    #[serde(flatten)]
    fleet: FleetRequest,
    requester_peer_id: String,
    target_peer_id: String,
    task: String,
    depth: u8,
    /// This field is accepted only at the Swift-to-Rust host boundary. No
    /// agent tool exposes it, and `TeamRuntime` still turns it into an opaque
    /// exact-pair capability before dispatch.
    user_approved: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FleetReceiveRequest {
    #[serde(flatten)]
    fleet: FleetRequest,
    peer_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FleetExecuteRequest {
    #[serde(flatten)]
    fleet: FleetRequest,
    peer_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FleetSummary {
    team_id: String,
    peer_ids: Vec<String>,
    shared_context_prompt: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FleetDispatchSummary {
    message_id: String,
    team_id: String,
    requester_peer_id: String,
    target_peer_id: String,
    depth: u8,
    non_blocking: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FleetReceivedTask {
    message_id: String,
    requester_peer_id: String,
    target_peer_id: String,
    task: String,
    depth: u8,
    shared_context_prompt: Option<String>,
}

/// Visible host-side projection of one queued mailbox item. This only prepares
/// input for a later iOS peer-runtime turn: it neither consumes work nor
/// reports a model completion.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FleetExecutionResult {
    message_id: String,
    prepared: bool,
    status: &'static str,
    prompt: String,
}

fn fleet_runtime(request: FleetRequest) -> Result<TeamRuntime, String> {
    if !request.storage_dir.is_absolute() {
        return Err("storageDir must be an absolute app-private path".to_string());
    }
    let manifest = import_manifest(&request.manifest_json).map_err(|error| error.to_string())?;
    let context = user_context(request.shared_context)?;
    let store = TeamCoordinationStore::open(request.storage_dir.join("peer-fleet.sqlite"))
        .map_err(|error| error.to_string())?;
    TeamRuntime::new(manifest, store, context).map_err(|error| error.to_string())
}

fn user_context(sections: BTreeMap<String, String>) -> Result<SectionContext, String> {
    let mut context = SectionContext::new();
    for (section, content) in sections {
        context
            .set_by_user(section, content)
            .map_err(|error| error.to_string())?;
    }
    Ok(context)
}

fn fleet_summary(runtime: &TeamRuntime) -> FleetSummary {
    FleetSummary {
        team_id: runtime.manifest().team_id().to_string(),
        peer_ids: runtime
            .manifest()
            .peers()
            .iter()
            .map(|peer| peer.peer_id().to_string())
            .collect(),
        shared_context_prompt: runtime.shared_context_prompt(),
    }
}

fn dispatch_summary(dispatch: PeerDispatch) -> FleetDispatchSummary {
    let request = dispatch.delegation.request();
    FleetDispatchSummary {
        message_id: dispatch.message_id,
        team_id: request.team_id.clone(),
        requester_peer_id: request.requester_peer_id.clone(),
        target_peer_id: request.target_peer_id.clone(),
        depth: request.depth,
        non_blocking: true,
    }
}

fn received_task(runtime: &TeamRuntime, received: ReceivedPeerDelegation) -> FleetReceivedTask {
    let request = received.delegation.request();
    FleetReceivedTask {
        message_id: received.message_id,
        requester_peer_id: request.requester_peer_id.clone(),
        target_peer_id: request.target_peer_id.clone(),
        task: request.task.clone(),
        depth: request.depth,
        shared_context_prompt: runtime.shared_context_prompt(),
    }
}

fn dispatch_fleet(request: &FleetDispatchRequest) -> Result<FleetDispatchSummary, String> {
    if !request.user_approved {
        return Err("explicit user approval is required before peer contact".to_string());
    }
    let mut runtime = fleet_runtime(FleetRequest {
        storage_dir: request.fleet.storage_dir.clone(),
        manifest_json: request.fleet.manifest_json.clone(),
        shared_context: request.fleet.shared_context.clone(),
    })?;
    let approval = runtime
        .approve_peer_contact(&request.requester_peer_id, &request.target_peer_id)
        .map_err(|error| error.to_string())?;
    runtime
        .dispatch(
            Some(&approval),
            DelegationRequest {
                team_id: runtime.manifest().team_id().to_string(),
                requester_peer_id: request.requester_peer_id.clone(),
                target_peer_id: request.target_peer_id.clone(),
                task: request.task.clone(),
                depth: request.depth,
            },
        )
        .map(dispatch_summary)
        .map_err(|error| error.to_string())
}

fn execute_next_fleet(
    request: FleetExecuteRequest,
) -> Result<Option<FleetExecutionResult>, String> {
    let runtime = fleet_runtime(request.fleet)?;
    let Some(received) = runtime
        .receive_for(&request.peer_id)
        .map_err(|error| error.to_string())?
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    let delegation = received.delegation.request();
    let mut prompt = format!(
        "## Peer Delegation\nYou are peer `{}`.\n\nTask from `{}`:\n{}",
        delegation.target_peer_id, delegation.requester_peer_id, delegation.task
    );
    if let Some(shared_context) = runtime.shared_context_prompt() {
        prompt.push_str("\n\n");
        prompt.push_str(&shared_context);
    }
    Ok(Some(FleetExecutionResult {
        message_id: received.message_id,
        prepared: true,
        status: "pendingPeerExecution",
        prompt,
    }))
}

fn parse_request<T: serde::de::DeserializeOwned>(ptr: *const c_char) -> Result<T, String> {
    if ptr.is_null() {
        return Err("request_json must be non-null UTF-8".to_string());
    }
    let raw = unsafe { CStr::from_ptr(ptr) }
        .to_str()
        .map_err(|_| "request_json must be non-null UTF-8".to_string())?;
    serde_json::from_str(raw).map_err(|error| format!("invalid fleet request: {error}"))
}

fn result_json<T: Serialize>(result: Result<T, String>) -> *mut c_char {
    let value = match result {
        Ok(value) => serde_json::json!({ "ok": true, "value": value }),
        Err(error) => serde_json::json!({ "ok": false, "error": error }),
    };
    CString::new(value.to_string())
        .map(CString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

/// Import a strict named fleet plus user-owned shared context.
/// Request: `{storageDir, manifestJson, sharedContext: {section: content}}`.
#[no_mangle]
pub unsafe extern "C" fn host_mobile_team_fleet_import(request_json: *const c_char) -> *mut c_char {
    result_json((|| {
        let request = parse_request(request_json)?;
        let runtime = fleet_runtime(request)?;
        Ok(fleet_summary(&runtime))
    })())
}

/// Dispatch to one named peer after an explicit host-side user approval.
/// Request adds `{requesterPeerId, targetPeerId, task, depth, userApproved}`.
/// This returns after durable mailbox admission; it never waits on peer work.
#[no_mangle]
pub unsafe extern "C" fn host_mobile_team_fleet_dispatch_after_user_approval(
    request_json: *const c_char,
) -> *mut c_char {
    result_json((|| {
        let request: FleetDispatchRequest = parse_request(request_json)?;
        dispatch_fleet(&request)
    })())
}

/// Read outstanding direct work for one named peer. Shared context is only a
/// prompt fragment in this projection: this bridge exposes no agent write API.
#[no_mangle]
pub unsafe extern "C" fn host_mobile_team_fleet_receive(
    request_json: *const c_char,
) -> *mut c_char {
    result_json((|| {
        let request: FleetReceiveRequest = parse_request(request_json)?;
        let runtime = fleet_runtime(request.fleet)?;
        runtime
            .receive_for(&request.peer_id)
            .map(|messages| {
                messages
                    .into_iter()
                    .map(|message| received_task(&runtime, message))
                    .collect::<Vec<_>>()
            })
            .map_err(|error| error.to_string())
    })())
}

/// Project one queued peer delegation into its target's exact prompt without
/// consuming mailbox work. Shared context is injected read-only; a later
/// local peer-runtime turn owns execution and acknowledgement.
#[no_mangle]
pub unsafe extern "C" fn host_mobile_team_fleet_execute_next(
    request_json: *const c_char,
) -> *mut c_char {
    result_json((|| {
        let request: FleetExecuteRequest = parse_request(request_json)?;
        execute_next_fleet(request)
    })())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> String {
        r#"{"version":1,"team_id":"ios-team","max_delegation_depth":1,"peers":[{"peer_id":"lead","role":"lead"},{"peer_id":"worker","role":"worker"}]}"#.to_string()
    }

    fn fleet_request(storage_dir: PathBuf) -> FleetRequest {
        FleetRequest {
            storage_dir,
            manifest_json: manifest(),
            shared_context: BTreeMap::from([(
                "brief".to_string(),
                "User decides scope.".to_string(),
            )]),
        }
    }

    #[test]
    fn imports_user_owned_context_and_dispatches_only_after_approval() {
        let directory = tempfile::tempdir().unwrap();
        let mut request = FleetDispatchRequest {
            fleet: fleet_request(directory.path().to_path_buf()),
            requester_peer_id: "lead".to_string(),
            target_peer_id: "worker".to_string(),
            task: "review change".to_string(),
            depth: 1,
            user_approved: false,
        };
        assert!(matches!(
            dispatch_fleet(&request),
            Err(error) if error.contains("explicit user approval")
        ));

        request.user_approved = true;
        let dispatched = dispatch_fleet(&request).unwrap();
        assert!(dispatched.non_blocking);
        let runtime = fleet_runtime(fleet_request(directory.path().to_path_buf())).unwrap();
        let received = runtime.receive_for("worker").unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(
            runtime.shared_context_prompt().as_deref(),
            Some("## Shared Team Context\n\n### brief\nUser decides scope.")
        );
    }

    #[test]
    fn depth_cap_blocks_mailbox_contact() {
        let directory = tempfile::tempdir().unwrap();
        let request = FleetDispatchRequest {
            fleet: fleet_request(directory.path().to_path_buf()),
            requester_peer_id: "lead".to_string(),
            target_peer_id: "worker".to_string(),
            task: "too deep".to_string(),
            depth: 2,
            user_approved: true,
        };
        assert!(matches!(
            dispatch_fleet(&request),
            Err(error) if error.contains("exceeds manifest maximum")
        ));
        let runtime = fleet_runtime(fleet_request(directory.path().to_path_buf())).unwrap();
        assert!(runtime.receive_for("worker").unwrap().is_empty());
    }

    #[test]
    fn preparing_next_projects_user_context_without_consuming_work() {
        let directory = tempfile::tempdir().unwrap();
        let dispatched = FleetDispatchRequest {
            fleet: fleet_request(directory.path().to_path_buf()),
            requester_peer_id: "lead".to_string(),
            target_peer_id: "worker".to_string(),
            task: "review change".to_string(),
            depth: 1,
            user_approved: true,
        };
        dispatch_fleet(&dispatched).unwrap();

        let result = execute_next_fleet(FleetExecuteRequest {
            fleet: fleet_request(directory.path().to_path_buf()),
            peer_id: "worker".to_string(),
        })
        .unwrap()
        .unwrap();
        assert!(result.prepared);
        assert_eq!(result.status, "pendingPeerExecution");
        assert!(result.prompt.contains("review change"));
        assert!(result.prompt.contains("User decides scope."));

        let runtime = fleet_runtime(fleet_request(directory.path().to_path_buf())).unwrap();
        assert_eq!(runtime.receive_for("worker").unwrap().len(), 1);
    }
}
