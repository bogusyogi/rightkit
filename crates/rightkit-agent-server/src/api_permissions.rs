use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{delete, get, post},
    Router,
};
use coderight_skills::{CommandAction, CommandResult};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::api::{
    lock_recover, now_iso, parse_workspace_trust_label, workspace_trust_key, workspace_trust_label,
    DaemonState, LiveSession,
};
use crate::api_stores::{
    write_permission_rule_store, write_workspace_trust_store, PersistentPermissionRule,
    PersistentPermissionRuleStore, WorkspaceTrustRecord,
};

pub fn permission_routes() -> Router<DaemonState> {
    Router::new()
        .route(
            "/sessions/{id}/permission-mode",
            post(set_permission_mode_handler),
        )
        .route(
            "/sessions/{id}/permission-rules",
            get(list_permission_rules_handler).post(add_permission_rule_handler),
        )
        .route(
            "/sessions/{id}/permission-rules/{index}",
            delete(delete_permission_rule_handler),
        )
        .route(
            "/permission-rules",
            get(list_persistent_permission_rules_handler)
                .post(add_persistent_permission_rule_handler),
        )
        .route(
            "/permission-rules/export",
            get(export_permission_rules_handler),
        )
        .route(
            "/permission-rules/import",
            post(import_permission_rules_handler),
        )
        .route(
            "/workspace/trust",
            get(get_workspace_trust_handler).patch(set_workspace_trust_handler),
        )
        .route(
            "/permission-rules/{index}",
            delete(delete_persistent_permission_rule_handler),
        )
        .route("/permission-denials", get(list_permission_denials_handler))
}

#[derive(Deserialize)]
pub struct SetPermissionMode {
    pub mode: String,
}

#[derive(Deserialize)]
pub struct AddPermissionRule {
    pub pattern: String,
    pub effect: String,
}

#[derive(Deserialize)]
pub struct PersistentPermissionRuleQuery {
    pub scope: Option<String>,
    #[serde(rename = "workspaceRoot", alias = "workspace_root")]
    pub workspace_root: Option<String>,
}

#[derive(Deserialize)]
pub struct PermissionDenialQuery {
    #[serde(rename = "sessionId", alias = "session_id")]
    pub session_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Deserialize)]
pub struct WorkspaceTrustQuery {
    #[serde(rename = "workspaceRoot", alias = "workspace_root")]
    pub workspace_root: String,
}

#[derive(Deserialize)]
pub struct AddPersistentPermissionRule {
    pub scope: String,
    pub pattern: String,
    pub effect: String,
    #[serde(rename = "workspaceRoot", alias = "workspace_root")]
    pub workspace_root: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetWorkspaceTrust {
    pub workspace_root: String,
    pub trust: String,
    pub approved_by: Option<String>,
}

include!("api_permissions/openapi.rs");

// `enforce_trusted_source_authority` / `lower_source_trust` moved to the
// binary-independent `coderight-daemon-core` crate (engine/crates/daemon_core
// /src/authority.rs) so extracted domain crates (e.g. `coderight-attention`)
// can call the gate without depending on this binary crate. Re-exported here
// so this module's ~58 existing `crate::api_permissions::...` call sites are
// unaffected.
pub(crate) use rightkit_daemon::agent_daemon_core::{enforce_trusted_source_authority, lower_source_trust};

fn enforce_policy_mutation_authority(headers: &HeaderMap) -> Result<(), StatusCode> {
    enforce_trusted_source_authority(headers)
}

include!("api_permissions/handlers.rs");

/// In-process entry for the interactive host's mode switch (`/yolo`,
/// Shift+Tab): the same live-session change and descendant tightening the
/// `permission-mode` route performs, without an HTTP round trip.
pub(crate) fn apply_live_permission_mode(
    state: &DaemonState,
    live: &std::sync::Arc<LiveSession>,
    mode: rightkit_agent_policy::PermissionMode,
) {
    set_permission_mode_and_tighten_descendants(state, live, mode);
}

include!("api_permissions/serialization.rs");

include!("api_permissions/command_renderer.rs");

include!("api_permissions/tests.rs");
