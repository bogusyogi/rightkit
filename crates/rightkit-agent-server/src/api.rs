//! The daemon HTTP API.
//!
//! Routes:
//!   GET  /health
//!   POST /diagnostics/bundle                          → create a redacted support bundle (RightKit ZIP)
//!   GET  /diagnostics/bundles/{id}                     → download it (base64 ZIP in JSON)
//!   GET  /api/contract                                → machine-readable daemon API contract
//!   GET  /sessions                                     → list sessions
//!   POST /sessions                                     → create session
//!   GET  /sessions/{id}/transcript                      → full transcript
//!   GET  /sessions/{id}/untrusted-content               → untrusted transcript content audit
//!   POST /sessions/{id}/messages                        → send message
//!   GET  /sessions/{id}/events?after=N                  → SSE event stream
//!   POST /sessions/{id}/compact                         → compact context
//!   POST /sessions/{id}/tool-approval                   → resolve pending approval
//!   GET  /tool-schemas                                 → list tool schemas
//!   POST /tool-schemas/validate                        → validate tool arguments
//!   GET  /toolsets                                     → list built-in toolset profiles
//!   GET  /sessions/{id}/tool-invocations                → list normalized tool lifecycle records
//!   GET  /tool-invocations/{id}                         → read one normalized tool lifecycle record
//!   GET  /tool-invocations/{id}/events                  → read lifecycle events for one tool call
//!   GET  /specs?workspaceRoot=X                        → list feature specs
//!   POST /specs                                        → create a feature spec
//!   GET  /specs/{id}                                    → read a feature spec with tasks
//!   PATCH /specs/{id}                                   → patch feature spec metadata
//!   POST /specs/{id}/tasks                              → add feature spec tasks
//!   GET  /specs/{id}/constitution/decisions             → list constitution decisions
//!   POST /specs/{id}/constitution/decisions             → record constitution decision
//!   POST /specs/{id}/analyze                            → run spec coverage analysis
//!   GET  /specs/{id}/trace                              → requirement/task/evidence trace
//!   GET  /sessions/{id}/spec/completion                 → bound spec completion gate
//!   GET  /execution/backends                           → list execution backends
//!   POST /execution/backends/{id}/doctor                → run backend doctor checks
//!   GET  /recipes                                      → list installed recipes
//!   POST /recipes/install                              → install a trusted/inline recipe
//!   POST /recipes/install-file                         → install a trusted local recipe file
//!   POST /recipes/install-url                          → install a GitHub/raw recipe file
//!   POST /recipes/validate                             → validate a recipe document
//!   POST /recipes/render                               → render recipe parameters
//!   POST /recipes/deeplink/parse                       → parse and validate a recipe deeplink
//!   POST /recipes/{id}/run                              → start a local recipe run
//!   POST /recipes/{id}/schedule                         → schedule a recipe run
//!   POST /recipes/{id}/deeplink                         → create a recipe deeplink
//!   GET  /recipes/{id}/runs                             → list recipe run history
//!   GET  /local-models/hardware                        → local hardware/runtime probe
//!   GET  /local-models/catalog                         → local model cookbook catalog
//!   POST /local-models/probe                           → probe OpenAI-compatible local endpoint
//!   POST /local-models/chat-smoke                      → smoke-test local chat completions
//!   POST /local-models/add-provider                    → persist/apply local provider route config
//!   GET  /hooks/status                                 → redacted hook readiness diagnostics
//!   POST /sessions/{id}/interrupt                       → stop a running turn
//!   POST /sessions/{id}/steer                           → inject steering message
//!   POST /sessions/{id}/goal                            → set persistent goal
//!   DELETE /sessions/{id}/goal                          → clear persistent goal
//!   POST /sessions/{id}/rewind                          → rewind to before last user msg
//!   GET  /settings/orchestration?workspaceRoot=X       → load settings
//!   PUT  /settings/orchestration                       → save settings
//!   GET  /commands                                     → list available slash commands
//!   POST /commands/execute                             → execute a slash command
//!   GET  /tasks?sessionId=X                            → task list
//!   GET  /sessions/{id}/runtime/artifacts               → artifacts
//!   GET  /sessions/{id}/runtime/taskgraph               → task graph
//!   GET  /runtime/learning                             → learning proposals
//!   GET  /runtime/benchmarks                           → benchmark reports
//!   GET  /runtime/benchmarks/proof                     → benchmark proof rollup
//!   GET  /runtime/terminal-bench/tasks                 → terminal-bench task catalog
//!   POST /runtime/terminal-bench/run                   → run a terminal-bench task
//!   POST /schedules                                    → create a workspace schedule
//!   PATCH /schedules/{id}                               → patch a workspace schedule
//!   GET  /skills                                       → managed local skill inventory
//!   GET  /skills/{slug}                                 → read a managed local skill
//!   POST /skills/install                               → install/update a local skill
//!   GET  /plugins/marketplace                          → list/search plugin marketplace catalog
//!   POST /plugins/marketplace/sync                     → trusted checksum-pinned marketplace sync
//!   POST /plugins/marketplace/{id}/install              → trusted checksum-pinned marketplace install
//!   POST /runtime/skills/synthesize                    → CoEvoSkills synthesis loop
//!
//! HTTP Routing (not `#[tauri::command]`):
//! This file defines endpoints via Axum Router (`build_router()`). The Tauri frontend invokes
//! these endpoints via localhost HTTP rather than direct Rust commands. This is intentional:
//! it allows the same engine binary to serve both Tauri and CLI clients. Endpoints are
//! mounted to the HTTP server at startup in `main.rs`.

use axum::{
    extract::{Query, State},
    http::{Method, StatusCode},
    middleware,
    response::Json,
    routing::{get, post},
    Router,
};
use coderight_recipes::RecipeStore;
use coderight_skills::{CommandAction, CommandResult, SlashCommand};
use rightkit_agent_engine_core::agents::WEBSITE_DESIGNER_SYSTEM_PROMPT;
use rightkit_agent_engine_core::Conduit;
use rightkit_agent_engine_protocol::{
    ContentTrust, ProtocolEvent, TranscriptMessage, TrustEnvelope,
};
use rightkit_agent_engine_runtime_middleware::{MiddlewarePhase, RuntimeSpan};
use rightkit_agent_orchestration::registry::AgentRegistry;
use rightkit_agent_orchestration::{
    ClassifierComplexity, ClassifierOutcome, DispatchClassifier, DispatchDecision, DispatchRouter,
    ExecutionStrategy, JudgeSource,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path as FsPath, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use tower_http::cors::{AllowOrigin, Any, CorsLayer};

use crate::api_benchmarks::benchmark_routes;
#[cfg(test)]
use crate::api_benchmarks::{verification_shell_command, CompareRequest};
use crate::api_clients::client_routes;
use crate::api_command_mcp::command_mcp;
use crate::api_command_renderers::{
    command_benchmarks, command_caps, command_context, command_diff, command_doctor,
    command_instructions, command_plugins, command_schedules, command_skills, command_spec,
    command_status, command_tasks, command_terminal_bench,
};
use crate::api_commands::command_routes;
use crate::api_compaction_archives::compaction_archive_routes;
use crate::api_context_folds::context_fold_routes;
use crate::api_contract::api_contract_routes;
use crate::api_coordination::{coordination_routes, render_messages_command, render_teams_command};
use crate::api_execution::{
    execution_routes, resolve_execution_backend_network_profile,
    resolve_execution_backend_resource_limits,
};
use crate::api_health::health_routes;
use crate::api_local_models::local_model_routes;
use crate::api_lsp::lsp_routes;
use crate::api_mcp::{default_mcp_user_config_path, mcp_routes};
use crate::api_memory::{memory_routes, render_memory_command};
use crate::api_notifications::notification_routes;
#[cfg(test)]
use crate::api_permissions::live_session_permission_mode;
use crate::api_permissions::{
    parse_permission_decision, permission_routes, render_permissions_command,
};
#[cfg(test)]
use crate::api_plugins::plugin_source_sha256;
use crate::api_plugins::{
    default_plugin_dir, default_plugin_marketplace_path, plugin_manifests_with_roots, plugin_routes,
};
use crate::api_recipes::{load_recipe_store, recipe_routes};
use crate::api_remote::remote_routes;
use crate::api_runtime::runtime_routes;
use crate::api_schedules::schedule_routes;
use crate::api_session_handoff::{
    command_export, command_handoff, command_summary, session_handoff_routes,
};
use crate::api_sessions::sessions_routes;
use crate::api_settings::settings_routes;
#[cfg(test)]
use crate::api_settings::SaveSettings;
use crate::api_skills::{
    default_skills_dirs, skill_runtime_context_with_behavioral, skills_routes,
};
use crate::api_specs::spec_routes;
use crate::api_stores::{
    default_instincts_store_path, default_message_store_path, default_permission_denial_store_path,
    default_permission_rule_store_path, default_team_store_path,
    default_workspace_trust_store_path, load_message_history_store, load_permission_denial_store,
    load_permission_rule_store, load_team_store, load_workspace_trust_store,
    write_message_history_store, write_permission_denial_store, MessageHistoryRow,
    PermissionDenialRow, PersistentPermissionRuleStore, WorkspaceTrustStore,
};
use crate::api_tool_invocations::tool_invocation_routes;
use crate::api_tool_schemas::tool_schema_routes;
use crate::api_toolsets::{
    apply_toolset_profile, resolve_toolset_profile, resolve_toolset_profile_with_config,
    toolset_routes, SCHEDULE_ADMIN_TOOLSET,
};
use crate::api_triggers::trigger_routes;

#[path = "api_router.rs"]
mod api_router;
#[path = "api_session_bindings.rs"]
mod api_session_bindings;
#[path = "api_session_config.rs"]
mod api_session_config;
#[path = "api_session_context.rs"]
mod api_session_context;
#[path = "api_session_contract.rs"]
mod api_session_contract;
#[path = "api_session_utils.rs"]
mod api_session_utils;
#[path = "api_slash_session_commands.rs"]
mod api_slash_session_commands;
#[path = "api_state.rs"]
mod api_state;
pub(crate) use api_state::native_spawn_route;

pub use api_router::build_router;
#[cfg(test)]
pub(crate) use api_session_bindings::session_spec_id;
pub(crate) use api_session_bindings::{
    ensure_bound_spec_can_complete, session_binding_row, session_spec_completion_summary,
    session_workspace_root, update_session_spec_binding, ListSessionsQuery, SessionSearchQuery,
};
#[cfg(test)]
pub(crate) use api_session_config::build_session_tool_registry;
pub(crate) use api_session_config::{
    api_workspace_root, build_session_tool_registry_for_plugins, compose_agent_persona,
    effective_tool_filter, initial_license_features, load_workspace_config_with_env,
    merge_active_skill_ids, normalize_driver_kind, normalize_effort, normalize_mcp_channel,
    normalize_thinking_budget, parse_workspace_trust_label, require_license_feature,
    run_dispatched_session_entry, session_row, session_runtime_options_from_config,
    session_title_prompt, should_auto_title_for_workspace, spawn_auto_title_task, touch_session,
    validate_execution_backend, web_search_config_from_config, workspace_trust_key,
    workspace_trust_label, AfterQuery, AnswerQuestion, Attachment, AwaySummaryQuery,
    BindSessionSpecRequest, CreateSession, LlmDispatchClassifier, OrchestrateBody, PatchSession,
    PostMessage, ResolveApproval, ResolvePlan, SessionQuery, SetGoal, WorkspaceQuery,
};
#[cfg(test)]
pub(crate) use api_session_config::{
    sanitize_generated_session_title, should_auto_title, SessionEntryOutcome,
};
pub(crate) use api_session_context::{
    apply_brief_default, apply_fable_mode, build_session_environment_snapshot,
    compose_session_system_prompt, decorate_user_payload_with_system_reminder,
    direct_message_engine_payload, discover_instruction_files, dynamic_user_turn_reminder,
    execute_daemon_command, get_instructions_handler, goal_state_from_row, instinct_guidance_block,
    load_instruction_context, load_stored_artifacts, now_iso, now_millis,
    persist_artifacts_snapshot, plugin_prompt_context, push_session_artifact,
    remote_channel_engine_payload, retrieve_session_memory, trust_envelope,
};
#[cfg(test)]
pub(crate) use api_session_context::{
    instinct_block_from_rules, retrieve_workspace_memory, session_start_snapshot_block,
    MAX_INSTINCT_BYTES,
};
pub(crate) use api_session_contract::{
    answer_question_request_schema, answer_question_response_schema, clear_goal_response_schema,
    edit_goal_request_schema, edit_goal_response_schema, get_goal_response_schema,
    instruction_status_response_schema, interrupt_session_response_schema,
    orchestrate_session_request_schema, orchestrate_session_response_schema,
    pause_goal_response_schema, resolve_plan_request_schema, resolve_plan_response_schema,
    resume_goal_response_schema, resume_session_response_schema, rewind_session_response_schema,
    session_openapi_components, session_openapi_request_schema, session_openapi_response_schema,
    set_goal_request_schema, set_goal_response_schema, steer_session_request_schema,
    steer_session_response_schema, untrusted_content_item,
};
#[cfg(test)]
pub(crate) use api_session_contract::{
    create_session_request_schema, fork_session_request_schema, fork_session_response_schema,
    post_message_response_schema, task_nodes_response_schema,
};
pub(crate) use api_session_utils::{
    await_event_persistence, away_summary_line, kind_matches, live_shell_statuses_for_workspace,
    load_workspace_task_records, normalize_session_status, publish_agent_roster,
    record_subagent_transcript_artifact, same_path, set_goal_status_field, spawn_event_persister,
    spawn_latch_cancel, summarize_text, task_node_json, task_record_json, transcript_created_at,
    transcript_for_session, untrusted_policy_action, update_session_effort, update_session_model,
    update_session_route, workspace_tasks_path, EditGoal,
};
#[cfg(test)]
pub(crate) use api_slash_session_commands::read_session_model;
pub(crate) use api_slash_session_commands::{command_effort, command_model, command_usage};
#[cfg(test)]
pub(crate) use api_state::effective_child_permission_mode_for_test;
#[cfg(test)]
pub(crate) use api_state::session_user_surfaces_enabled;
pub(crate) use api_state::{
    build_subagent_fork, cleanup_subagent_worktree, lock_recover, missing_provider_key_message,
    prepare_subagent_workspace, record_engine_unavailable_reason, record_subagent_runtime_span,
    BudgetDecision, DaemonSubagentSupervisor, DriverToolRequest, RunSubagentBudget,
    SessionRuntimeOptions, SubagentFork, SubagentLifecycleMark,
};
pub(crate) use api_state::{governed_workspace_worktree_pool, open_subagent_worktree_pool};
#[cfg(test)]
pub(crate) use api_state::{AdmissionGuard, RunBudgetLedger};
pub use api_state::{
    DaemonSettingsStore, DaemonState, EngineConfig, LiveSession, PendingElicitation,
    PendingElicitationCleanup, SharedStorage,
};

/// Canonical composition-root ingress for durable event writes.
///
/// API projections provide event data, but they do not select or call a
/// storage writer directly. Keeping sequence assignment/replay handling here
/// makes the host-composed `SharedStorage` writer the sole durable authority.
pub(crate) fn append_event_via_storage_authority(
    storage: &mut rightkit_agent_engine_storage::Storage,
    session_id: &str,
    seq: i64,
    kind: &str,
    payload: &str,
) -> Result<(), String> {
    if seq >= 1 {
        storage
            .append_event_at_seq(session_id, seq, kind, payload)
            .map_err(|error| error.to_string())
    } else {
        storage
            .append_event(session_id, kind, payload)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
include!("api_tests.rs");
