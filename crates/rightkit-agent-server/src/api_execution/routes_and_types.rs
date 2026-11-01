use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::{get, patch, post, put},
    Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::path::PathBuf;

use crate::api::{lock_recover, require_license_feature, DaemonState};
use crate::api_permissions::enforce_trusted_source_authority;

pub(crate) fn execution_routes() -> Router<DaemonState> {
    Router::new()
        .route("/execution/backends", get(list_execution_backends))
        .route(
            "/execution/backends/default",
            patch(set_execution_backend_default),
        )
        .route("/execution/backends/{id}", patch(update_execution_backend))
        .route(
            "/execution/network-profiles/{id}",
            put(save_execution_network_profile).delete(delete_execution_network_profile),
        )
        .route(
            "/execution/workspaces",
            get(list_execution_workspaces).post(create_execution_workspace),
        )
        .route(
            "/execution/processes",
            get(list_execution_processes).post(create_execution_process),
        )
        .route("/execution/processes/{id}", get(get_execution_process))
        .route(
            "/execution/processes/{id}/logs",
            get(get_execution_process_logs),
        )
        .route(
            "/execution/processes/{id}/stdin",
            post(write_execution_process_stdin),
        )
        .route(
            "/execution/processes/{id}/resize",
            post(resize_execution_process),
        )
        .route(
            "/execution/processes/{id}/kill",
            post(kill_execution_process),
        )
        .route(
            "/execution/processes/{id}/attach",
            post(attach_execution_process),
        )
        .route(
            "/execution/processes/{id}/detach",
            post(detach_execution_process),
        )
        .route("/execution/processes/{id}/ack", post(ack_execution_process))
        .route(
            "/execution/backends/{id}/doctor",
            post(doctor_execution_backend),
        )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BackendDoctorQuery {
    workspace_root: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BackendRegistryQuery {
    workspace_root: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExecutionWorkspaceQuery {
    workspace_root: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExecutionProcessQuery {
    workspace_root: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExecutionProcessLogsQuery {
    workspace_root: Option<String>,
    tail_bytes: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateExecutionWorkspaceRequest {
    backend_id: String,
    workspace_id: Option<String>,
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateExecutionProcessRequest {
    session_id: String,
    /// Optional caller restriction; never replaces the host-owned governor.
    #[serde(default)]
    resource_profile: Option<String>,
    #[serde(default)]
    execution_channel: Option<String>,
    #[serde(default)]
    pty_size: Option<rightkit_sandbox::execution::PtySize>,
    #[serde(default)]
    persistence: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetExecutionBackendDefaultRequest {
    backend_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateExecutionBackendRequest {
    enabled: Option<bool>,
    resource_limits: Option<rightkit_sandbox::execution::ResourceLimits>,
    network_profile: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveExecutionNetworkProfileRequest {
    name: Option<String>,
    default: Option<rightkit_sandbox::execution::NetworkDefault>,
    allow_hosts: Option<Vec<String>>,
    allow_private_networks: Option<bool>,
    deny_private_networks: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WriteExecutionProcessStdinRequest {
    #[serde(default)]
    input: Option<String>,
    #[serde(default)]
    eof: bool,
    append_newline: Option<bool>,
    #[serde(default)]
    input_base64: Option<String>,
    #[serde(default)]
    lease_epoch: Option<u64>,
    #[serde(default)]
    input_sequence: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AcknowledgeExecutionProcessRequest {
    lease_epoch: u64,
    sequence: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExecutionWorkspacePaths {
    root: String,
    uploads: String,
    workspace: String,
    outputs: String,
    logs: String,
    skills: String,
    metadata: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExecutionWorkspaceRecord {
    id: String,
    backend_id: String,
    name: String,
    status: String,
    paths: ExecutionWorkspacePaths,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExecutionProcessRow {
    id: String,
    kind: String,
    session_id: String,
    workspace_root: String,
    backend_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    execution_workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    network_profile: Option<String>,
    command: String,
    execution_channel: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pty_size: Option<rightkit_sandbox::execution::PtySize>,
    persistence: String,
    attachment: String,
    lease_epoch: u64,
    next_input_sequence: u64,
    last_acked_input_sequence: u64,
    status: String,
    status_label: String,
    last_output_line: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    started_at_ns: Option<u128>,
    live: bool,
    control_mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    control_unavailable_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_seen_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExecutionProcessRecord {
    id: String,
    kind: String,
    session_id: String,
    workspace_root: String,
    backend_id: String,
    #[serde(default)]
    execution_workspace_id: Option<String>,
    #[serde(default)]
    network_profile: Option<String>,
    command: String,
    #[serde(default = "default_pipe_channel")]
    execution_channel: String,
    #[serde(default = "default_inline_persistence")]
    persistence: String,
    #[serde(default = "default_attached")]
    attachment: String,
    #[serde(default)]
    lease_epoch: u64,
    #[serde(default)]
    next_input_sequence: u64,
    #[serde(default)]
    last_acked_input_sequence: u64,
    status: String,
    status_label: String,
    last_seen_at: String,
}

fn default_pipe_channel() -> String {
    "pipe".to_string()
}
fn default_inline_persistence() -> String {
    "inline".to_string()
}
fn default_attached() -> String {
    "attached".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExecutionBackendDefaultsRecord {
    default_backend: String,
    #[serde(default)]
    backend_overrides: Vec<ExecutionBackendOverrideRecord>,
    #[serde(default)]
    network_profile_overrides: Vec<rightkit_sandbox::execution::NetworkProfile>,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExecutionBackendOverrideRecord {
    id: String,
    enabled: Option<bool>,
    resource_limits: Option<rightkit_sandbox::execution::ResourceLimits>,
    network_profile: Option<Option<String>>,
}
