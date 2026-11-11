#![allow(dead_code, unused)]
//! — the single binary: `serve` daemon + CLI subcommands + service management.

mod acp;
mod agent_roster;
mod api;
mod api_accounts;
mod api_adapt;
mod api_benchmarks;
mod api_browser_workspace;
mod api_clients;
mod api_command_mcp;
mod api_command_plugins;
mod api_command_renderers;
mod api_commands;
mod api_compaction_archives;
mod api_context_folds;
mod api_contract;
mod api_coordination;
mod api_delivery;
mod api_drivers;
mod api_environments;
mod api_execution;
mod api_external_observations;
mod api_fs_watch;
mod api_health;
mod api_local_models;
mod api_lsp;
mod api_mcp;
mod api_membrane_background;
mod api_memory;
mod api_notifications;
mod api_permissions;
mod api_plugins;
mod api_provider_auth;
mod api_recipes;
mod api_remote;
mod api_run_changes;
mod api_runtime;
mod api_schedules;
mod api_session_handoff;
mod api_sessions;
mod api_settings;
mod api_skills;
mod api_specs;
mod api_stores;
mod api_task_list;
mod api_teams;
mod api_tool_invocations;
mod api_tool_schemas;
mod api_toolsets;
mod api_triggers;
mod api_workflows;
mod api_workspace;
mod api_worktree_pool;
mod async_inbox_feed;
mod audit;
mod auth;
mod background_provider;
pub(crate) mod background_review_adapter;
mod cli;
mod cli_delegation;
mod cli_presence;
mod cli_recovery_runtime;
mod code_mode_context;
mod context_status;
mod custom_commands;
mod daemon;
mod daemon_self_review;
mod daemon_state;
mod doctor;
mod dogfood_inventory;
mod driver_host_runtime;
mod evo_db;
mod github_app;
mod github_signin;
mod goal_terminal;
mod hooks;
mod host_retained_context;
mod inference_runtime;
mod inline_host_init;
mod inline_shell_runner;
mod journal;
mod learning;
// The gate is attached by release builds.
mod license;
mod license_ipc;
mod main_runtime;
mod mcp_server;
mod membrane_binding;
mod model_policy;
mod native_transcript_exporter;
mod observation;
mod orchestrator;
mod plan_exit;
mod pre_gate;
mod project_binding;
mod provider_client;
mod rehydrate;
mod rollout_store;
mod router_seam;
mod run_origin_storage;
mod scheduler;
mod session_tool_catalog;
mod skill_loop;
mod startup;
mod store_write;
mod telegram_task;
mod trajectory;
mod tui_host;
mod tui_image_input;
mod tui_inspection;
mod tui_panes;
mod tui_projects;
mod workflow;

/// Crash capture for the engine process (M16, `rightkit-crash`).
///
/// Installed first so every later hook chains to it: the TUI's terminal-restore
/// hook (`tui_host::install_panic_hook`) takes this hook as its `previous` and
/// runs it after restoring the terminal. Reports are redacted, bounded, and
/// stay local under `<state root>/crash`; nothing is uploaded. When the state
/// root cannot be resolved the crate's private temp fallback is used.
fn install_crash_capture() {
    let crash_dir = rightkit_agent_config::state_root()
        .map(|root| root.join("crash"))
        .unwrap_or_else(|| rightkit_crash::fallback_crash_dir("RightKit"));
    rightkit_crash::install_process_hook(
        rightkit_crash::ProcessKind::Engine,
        rightkit_crash::CrashConfig::new("RightKit", env!("CARGO_PKG_VERSION"), crash_dir),
    );
}

/// Run the agent server host binary entry.
///
/// The product passes its skill implementations: `skill_catalog` answers skill discovery and
/// trigger queries for the agent crates, and `skill_host` supplies the slash-command table,
/// custom commands, skill manifests and trigger evaluation. Both are installed before anything runs.
pub fn run(
    skill_catalog: std::sync::Arc<dyn rightkit_agent_host_traits::SkillCatalog>,
    skill_host: std::sync::Arc<dyn rightkit_agent_host_traits::skill_host::SkillHost>,
) {
    rightkit_agent_host_traits::install_skill_catalog(skill_catalog);
    rightkit_agent_host_traits::skill_host::install_skill_host(skill_host);
    // First, before any other thread: read the licence locators the shell passed and remove every
    // licence-named variable from this process's environment, so no child process inherits one.
    license::capture_launch_locators();
    install_crash_capture();
    // Bind before *any* dispatch, including bare TUI, onboarding, ACP/MCP and
    // parser-worker entry. Per-host checks later may confirm this same profile,
    // but no command can execute while resource admission is unconfigured.
    if let Err(error) = install_binary_resource_profile() {
        eprintln!("{error}");
        std::process::exit(2);
    }
    if rightkit_agent_tools::coding_tools::rich_parser_worker::run_if_requested() {
        return;
    }
    main_runtime::main();
}

fn binary_resource_profile(
    command: Option<&std::ffi::OsStr>,
    local_override: Option<&str>,
) -> Result<rightkit_agent_tools::coding_tools::process_resource_authority::ResourceProfile, String>
{
    use rightkit_agent_tools::coding_tools::process_resource_authority::{
        local_resource_profile_from_value, ResourceProfile,
    };
    // Cli has no global options: the sole ServiceHost entry is the first
    // positional `serve` command. Remote/service clients do not themselves
    // become ServiceHost; a daemon they launch enters this branch separately.
    if command == Some(std::ffi::OsStr::new("serve")) {
        Ok(ResourceProfile::Expanded)
    } else {
        local_resource_profile_from_value(local_override)
    }
}

fn install_binary_resource_profile() -> Result<(), String> {
    use rightkit_agent_tools::coding_tools::process_resource_authority::{
        install_process_resource_profile, PROCESS_RESOURCE_PROFILE_ENV,
    };
    let command = std::env::args_os().nth(1);
    let raw = std::env::var_os(PROCESS_RESOURCE_PROFILE_ENV);
    let local_override = raw
        .as_deref()
        .map(|value| {
            value.to_str().ok_or_else(|| {
                format!("{PROCESS_RESOURCE_PROFILE_ENV} must be a UTF-8 profile name")
            })
        })
        .transpose()?;
    let profile = binary_resource_profile(command.as_deref(), local_override)?;
    install_process_resource_profile(profile)
        .map(|_| ())
        .map_err(|existing| {
            format!(
                "resource profile conflict: binary selected {profile:?}, already bound to {existing:?}"
            )
        })
}
