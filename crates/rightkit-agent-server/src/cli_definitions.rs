//! CLI surface for the `{app}` binary.

use clap::{Parser, Subcommand, ValueEnum};

include!("cli_definitions_command.rs");
include!("cli_definitions_launch.rs");
include!("cli_definitions_mcp_plugins.rs");
include!("cli_definitions_execution_session.rs");
include!("cli_definitions_remote_recipe.rs");
include!("cli_definitions_schedule_spec.rs");
include!("cli_definitions_skills_learning_diag.rs");

#[path = "command_registry.rs"]
pub mod command_registry;

pub(crate) use cli_definitions_command_run::RunArgs as CliRunArgs;
pub(crate) use cli_definitions_command_run::{
    warn_deprecated_context_mode_flag, CliColor, CliPermissionMode, ContextMode, OutputFormat,
};

/// Whether `run`, `review` and `recipe run` auto-approve edit-class actions: their own
/// `--auto-approve`, or the global `--yes` on a host that sets
/// `host_options::yes_flag_is_auto_approve` (the twin's `--yes` was an alias of `--auto-approve`;
/// a host whose `--yes` only completes a dry run leaves it unset).
pub(crate) fn effective_auto_approve(auto_approve: bool, yes: bool) -> bool {
    auto_approve || (yes && crate::host_options::yes_flag_is_auto_approve())
}

/// The command line of the installed host: the derived [`Cli`] command with the product names
/// in its help text filled in. The derive attributes and doc comments are `'static` text, so
/// they carry the placeholders `{app}` (the command name), `{APP_NAME}` (the display name),
/// `{ENV_PREFIX}` (the environment-variable prefix, without the trailing `_`) and
/// `{STATE_DIR}` (the state folder name, with its dot); this function replaces them with owned
/// strings at use, so no product's name is compiled into the library. Parse through this
/// command (`Cli::from_arg_matches`), not through the bare derive, so the help names the host.
pub(crate) fn localized_command() -> clap::Command {
    localize_command(<Cli as clap::CommandFactory>::command())
}

fn localize_text(text: &str) -> String {
    text.replace("{app}", crate::host_identity::app())
        .replace("{APP_NAME}", rightkit_agent_config::app_name())
        .replace("{ENV_PREFIX}", rightkit_agent_config::env_prefix())
        .replace("{STATE_DIR}", rightkit_agent_config::state_dir_name())
}

fn localize_command(command: clap::Command) -> clap::Command {
    let mut command = command;
    if let Some(text) = command.get_about().map(ToString::to_string) {
        command = command.about(localize_text(&text));
    }
    if let Some(text) = command.get_long_about().map(ToString::to_string) {
        command = command.long_about(localize_text(&text));
    }
    if let Some(text) = command.get_before_help().map(ToString::to_string) {
        command = command.before_help(localize_text(&text));
    }
    if let Some(text) = command.get_after_help().map(ToString::to_string) {
        command = command.after_help(localize_text(&text));
    }
    if let Some(text) = command.get_after_long_help().map(ToString::to_string) {
        command = command.after_long_help(localize_text(&text));
    }
    command
        .mut_args(|arg| {
            let help = arg.get_help().map(ToString::to_string);
            let long_help = arg.get_long_help().map(ToString::to_string);
            let arg = match help {
                Some(text) => arg.help(localize_text(&text)),
                None => arg,
            };
            match long_help {
                Some(text) => arg.long_help(localize_text(&text)),
                None => arg,
            }
        })
        .mut_subcommands(localize_command)
}
