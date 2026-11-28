#![allow(dead_code, unused)]
//! The agent server a product app runs as its one binary: the `serve` daemon, the CLI
//! subcommands, the interactive session and service management.
//!
//! A host binary is a thin `main`: it names itself to the shared crates, sets what it wants of
//! the server and calls [`run`] with its [`HostIdentity`]. The conventions one host wants and
//! another may not are [`host_options`], set before [`run`].
//!
//! # What a host does before [`run`]
//!
//! In this order, on the main thread, before it starts a thread (the shared crates fix most of
//! these settings for the process on first use, and a setter that answers `false` found a value
//! already set, which for a name is a start-up bug):
//!
//! 1. **Names and configuration.** `rightkit_agent_config::ensure_host_names` with the
//!    environment prefix, the state folder name and the display name, so every variable, path and
//!    message below carries the host's names. Then the configuration conventions of the host, in
//!    this order: `set_budget_defaults` (the spend ceilings a session starts with; a host that
//!    wants none passes `BudgetDefaults::no_ceiling()`), `set_retired_config_keys` (the config
//!    keys the host deleted: not read, not written, not in the schema and unknown under strict
//!    config; the Settings API of this crate hides the retired budget keys too),
//!    `set_hardened_config_loader`, `set_hardened_instructions` and `set_nearest_ancestor_trust`
//!    (the nearest recorded ancestor decides a folder's trust). The state folder name must be the
//!    one the [`HostIdentity`] declares ([`HostIdentity::with_home_dir`]); [`run`] refuses to
//!    start when they differ.
//! 2. **Licence scrub.** [`scrub_inherited_licence_env`] reads the licence locators the licence
//!    owner passed and removes every licence-named variable from the process environment (it
//!    reads the environment, so it comes straight after the names; [`run`] repeats it, which
//!    changes nothing).
//! 3. **Conventions of the shared crates.** The protocol (wire shape, untrusted-content tag and
//!    header style), the policy crate (state folder, product name, protected process name,
//!    fail-closed hint; set before any permission engine, deny list or policy point exists), the
//!    provider layer, the MCP layer, the hook crate, the tools crate (its options **and its skill
//!    catalog**: [`run`] installs none, and without one every skill that declares front matter
//!    is refused), the engine's once-per-process options, the driver host (vendor CLI launches),
//!    the session store (product name, journal-mode variable, browser artifact producer, latency
//!    observer), bot messaging, orchestration, the attention crate (state folder, source-trust
//!    header, default source), the daemon's source-trust header and HTTP user agent, and the
//!    removal of inherited provider hooks (`OAUTH_CALLBACK_HOST`, `RECORD_REPLAY`,
//!    `DISABLE_DEFAULT_MCP`, `REPLAY_CORPUS_DIR` under the shared and the host's prefix). The
//!    terminal-UI and hook crates are named by [`run`] itself from the configuration names.
//! 4. **Options of this crate.** The [`host_options`] the host sets, every one of them once, and
//!    the values it passes for the bundled skills, the web client files and the tool-registry
//!    controls.
//! 5. **Cargo features.** A host that offers the evaluation harness enables `benchmarks`; one
//!    that runs qualification enables `qualification`; one that ships the local embedder
//!    enables `fastembed`.
//!
//! [`run`] then checks the declared state folder, installs the identity, names the hook and
//! terminal-UI crates from the configuration names, makes the workspace trust store the options
//! chose the one every trust check reads, installs crash capture and the broken-pipe exit, binds
//! the resource profile, installs the stderr tracing and enters the CLI.

mod acp;
mod agent_roster;
mod api;
mod api_accounts;
mod api_adapt;
#[cfg(feature = "benchmarks")]
mod api_benchmarks;
#[cfg(not(feature = "benchmarks"))]
#[path = "api_benchmarks_off.rs"]
mod api_benchmarks;
mod api_browser_consent;
mod api_browser_workspace;
mod api_clients;
mod api_cloud_forge;
mod api_cloud_workspaces;
mod api_command_mcp;
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
mod api_license_status;
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
mod api_web;
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
mod cli_journal;
mod cli_log;
mod cli_presence;
mod cli_recovery_runtime;
mod code_mode_context;
mod context_status;
mod custom_commands;
mod daemon;
mod daemon_self_review;
mod daemon_state;
mod doctor;
mod driver_host_runtime;
mod evo_db;
mod github_signin;
mod goal_terminal;
mod guest_shares;
mod harness_radar_feed;
mod hooks;
pub mod host_identity;
pub mod host_options;
pub use host_identity::{HarnessRadarData, HostIdentity};
pub use license::scrub_inherited_licence_env;
mod inference_runtime;
mod inline_host_init;
mod inline_shell_runner;
mod lan_listener;
mod learning;
// The gate is attached by release builds.
mod license;
mod license_ipc;
mod listener_guard;
mod loopback_adapter;
mod main_runtime;
mod mcp_server;
mod membrane_binding;
mod memory_save_tool;
mod model_policy;
mod native_transcript_exporter;
mod observation;
mod open_url;
mod option_gated_routes;
mod orchestrator;
mod plan_exit;
mod pre_gate;
mod project_binding;
mod provider_client;
mod rehydrate;
mod remote_ssh;
mod rollout_store;
mod router_seam;
mod rpc_mode;
mod run_origin_storage;
mod scheduler;
mod session_lease;
mod session_seen;
mod session_startup_recovery;
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
mod tui_plugin_slots;
mod tui_projects;
mod tui_status_script;
mod unattended_guard;
mod vendor_mcp_pipe;
mod workflow;

/// Crash capture for the engine process (M16, `rightkit-crash`).
///
/// Installed first so every later hook chains to it: the TUI's terminal-restore
/// hook (`tui_host::install_panic_hook`) takes this hook as its `previous` and
/// runs it after restoring the terminal. Reports are redacted, bounded, and
/// stay local under `<state root>/crash`; nothing is uploaded. When the state
/// root cannot be resolved the crate's private temp fallback is used.
fn install_crash_capture() {
    let app_name = rightkit_agent_config::app_name();
    let crash_dir = rightkit_agent_config::state_root()
        .map(|root| root.join("crash"))
        .unwrap_or_else(|| rightkit_crash::fallback_crash_dir(app_name));
    // Providers R46: every key the host holds is removed from a report. This reads the
    // environment and `.env` once and scrubs the process environment, so it runs here, before
    // any thread exists.
    let secrets = rightkit_llm::agent_providers::host_credentials::host().secret_values();
    let mut config =
        rightkit_crash::CrashConfig::new(app_name, host_identity::version(), crash_dir.clone())
            .redact_values(secrets);
    // Storage D-S38: the newest 20 reports are kept.
    config.retention = cli_log::crash_retention();
    rightkit_crash::install_process_hook(rightkit_crash::ProcessKind::Engine, config);
    // Reports an older build left behind are pruned at start, not only after the next crash.
    rightkit_crash::prune(&crash_dir, cli_log::crash_retention());
}

/// A reader that closes the pipe early (`{app} contract | head`) makes the next print fail with
/// "Broken pipe". That is normal Unix behaviour, not a crash: exit quietly with 128 + SIGPIPE and
/// write no crash report. Every other panic goes to the crash reporter as before.
fn exit_quietly_on_broken_pipe() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info.payload();
        let message = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or("");
        if message.contains("failed printing to std") && message.contains("Broken pipe") {
            std::process::exit(141);
        }
        previous(info);
    }));
}

/// Names the host to the hook and terminal-UI crates from the configuration names, so the hook
/// variables (`<PREFIX>_HOOK_EVENT`, ...), the project hook file, the notification and title
/// variables and the product name in titles follow the host. Each crate fixes its names for the
/// process on first use; a host that already named one of them (the call then answers `false`)
/// must have used the same names, and any other value is a start-up bug that ends the process.
fn name_shared_crates() -> Result<(), String> {
    use rightkit_hooks::agent_hooks;
    use rightkit_terminal_ui::terminal::notify;
    let prefix = rightkit_agent_config::env_prefix();
    let state_dir = rightkit_agent_config::state_dir_name();
    if !agent_hooks::set_env_prefix(prefix) && agent_hooks::env_prefix() != prefix {
        return Err(format!(
            "the hook environment prefix is `{}` but the host's is `{prefix}`",
            agent_hooks::env_prefix()
        ));
    }
    if !agent_hooks::set_state_dir_name(state_dir) && agent_hooks::state_dir_name() != state_dir {
        return Err(format!(
            "the hook state folder is `{}` but the host's is `{state_dir}`",
            agent_hooks::state_dir_name()
        ));
    }
    if !notify::set_env_prefix(prefix) && notify::env_prefix() != prefix {
        return Err(format!(
            "the terminal UI environment prefix is `{}` but the host's is `{prefix}`",
            notify::env_prefix()
        ));
    }
    notify::set_app_name(rightkit_agent_config::app_name());
    Ok(())
}

/// The command word of an invocation: the first argument that is not a flag. `-c`, `--config` and
/// `--caller` take a value, which is skipped with them. `None` for a bare launch.
fn command_word(args: impl IntoIterator<Item = std::ffi::OsString>) -> Option<std::ffi::OsString> {
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("-c" | "--config" | "--caller") => {
                args.next();
            }
            Some(flag) if flag.starts_with('-') => {}
            _ => return Some(arg),
        }
    }
    None
}

/// Run the agent server host binary entry.
///
/// The product passes its names in `identity`: every product name the server writes (command
/// name, state folder, schema ids, headers, licence app, credential namespace, version; see
/// [`HostIdentity`]). It must already have done every step the crate documentation lists, in
/// that order (the names, the configuration hardening, retired keys and budget defaults,
/// [`scrub_inherited_licence_env`], the conventions of the shared crates including the skill
/// catalog, and [`host_options`]); the licence scrub repeats here and changes nothing if it ran.
///
/// In order: the licence scrub, the check that the identity's state folder is the configured one
/// (a mismatch ends the process with status 2: two names for one folder would split the host's
/// state between two places), the identity, the hook and terminal-UI names, the workspace trust
/// store the options chose, crash capture (reports redacted with every key the host holds,
/// newest 20 kept, older ones pruned), the broken-pipe exit (status 141), the process resource
/// profile, the parser-worker entry, the stderr tracing (`info` for `serve`, off otherwise; only
/// hook errors always show; a subscriber installed earlier stays, silently) and the CLI.
pub fn run(identity: HostIdentity) {
    // First, before any other thread: read the licence locators the shell passed and remove every
    // licence-named variable from this process's environment, so no child process inherits one.
    // The names carry the host's environment prefix, which the host set before calling `run`.
    license::capture_launch_locators();
    // One name for the state folder: every path in this crate is built from
    // `rightkit_agent_config::state_dir_name()`, so an identity that declares another one would
    // name a folder nothing reads.
    if identity.home_dir() != rightkit_agent_config::state_dir_name() {
        eprintln!(
            "{}: the host identity names the state folder `{}` but the configured state folder is `{}`; they must be the same",
            rightkit_agent_config::host_slug(),
            identity.home_dir(),
            rightkit_agent_config::state_dir_name()
        );
        std::process::exit(2);
    }
    host_identity::install(identity);
    if let Err(error) = name_shared_crates() {
        eprintln!("{error}");
        std::process::exit(2);
    }
    #[cfg(feature = "state-store-env")]
    api_stores::install_workspace_trust_rule();
    install_crash_capture();
    exit_quietly_on_broken_pipe();
    // Bind before *any* dispatch, including bare TUI, onboarding, ACP/MCP and
    // parser-worker entry. Per-host checks later may confirm this same profile,
    // but no command can execute while resource admission is unconfigured.
    if let Err(error) = install_binary_resource_profile() {
        eprintln!("{error}");
        std::process::exit(2);
    }
    if rightkit_agent_coding_tools::rich_parser_worker::run_if_requested() {
        return;
    }
    // `tracing` events go to stderr (scrubbed), which is the daemon log for a daemon. Only hook
    // errors show by default, as they always did; `RUST_LOG` widens or narrows that. The daemon
    // is the exception: its stderr is the serve log, which has always carried the engine's
    // warnings and info lines, so a daemon keeps that default.
    let is_daemon =
        command_word(std::env::args_os().skip(1)).is_some_and(|command| command == "serve");
    // A subscriber installed earlier stays; the failure is not reported (a host that installed its
    // own gets no line about it).
    rightkit_logs::install_stderr_tracing(if is_daemon {
        "info,rightkit_hook=warn"
    } else {
        "off,rightkit_hook=warn"
    })
    .ok();
    main_runtime::main();
}

fn binary_resource_profile(
    command: Option<&std::ffi::OsStr>,
    local_override: Option<&str>,
) -> Result<rightkit_agent_coding_tools::process_resource_authority::ResourceProfile, String> {
    use rightkit_agent_coding_tools::process_resource_authority::{
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
    use rightkit_agent_coding_tools::process_resource_authority::{
        install_process_resource_profile, process_resource_profile_env_name,
    };
    // The command is the first word that is not a flag: `-c`/`--config`/`--caller` take a value.
    let command = command_word(std::env::args_os().skip(1));
    // `<PREFIX>_RESOURCE_PROFILE`: the name follows the environment prefix the host set.
    let env_name = process_resource_profile_env_name();
    let raw = std::env::var_os(&env_name);
    let local_override = raw
        .as_deref()
        .map(|value| {
            value
                .to_str()
                .ok_or_else(|| format!("{env_name} must be a UTF-8 profile name"))
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

#[cfg(test)]
mod tests {
    use super::command_word;
    use std::ffi::OsString;

    fn words(args: &[&str]) -> Option<OsString> {
        command_word(args.iter().map(OsString::from))
    }

    #[test]
    fn the_command_is_the_first_word_that_is_not_a_flag() {
        assert_eq!(words(&["serve", "--port", "0"]), Some("serve".into()));
        assert_eq!(words(&["--plain", "serve"]), Some("serve".into()));
        assert_eq!(words(&[]), None);
        assert_eq!(words(&["--plain"]), None);
    }

    #[test]
    fn config_and_caller_flags_hide_their_value_from_the_command() {
        assert_eq!(words(&["-c", "serve", "run"]), Some("run".into()));
        assert_eq!(
            words(&["--config", "a=b", "--caller", "ide", "serve"]),
            Some("serve".into())
        );
        assert_eq!(words(&["--caller"]), None);
    }
}
