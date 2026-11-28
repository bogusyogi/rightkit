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
pub(crate) use cli_definitions_command_run::{CliColor, CliPermissionMode, OutputFormat};

/// Whether `run`, `review` and `recipe run` auto-approve edit-class actions: the `--auto-approve`
/// flag of `run` and `review` (a host whose `legacy_auto_approve_flags` is off has none, and
/// `recipe run` never had one), or the global `--yes` on a host that sets
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
    hide_removed_items(localize_command(<Cli as clap::CommandFactory>::command()))
        .version(crate::host_identity::version())
}

/// The subcommands a host option removes from the command line, as paths from the root
/// (`["plugin", "marketplace"]`). The derive grammar carries them for every host; clap cannot drop
/// a subcommand from a built command, so a removed one is hidden from the help and refused as an
/// unrecognised subcommand by [`reject_removed_items`], which is what a host that never had it
/// answers.
fn removed_subcommands() -> Vec<&'static [&'static str]> {
    let mut removed: Vec<&'static [&'static str]> = Vec::new();
    if !crate::host_options::acp_server() {
        removed.push(&["acp-server"]);
    }
    if !crate::host_options::plugin_marketplace() {
        removed.push(&["plugin", "marketplace"]);
        removed.push(&["plugin", "install-marketplace"]);
        removed.push(&["plugin", "marketplace-sync"]);
    }
    if !crate::host_options::mcp_public_server() {
        removed.push(&["mcp", "status"]);
    }
    removed
}

/// The flags a host option removes from the command line: the command path and the argument id.
fn removed_flags() -> Vec<(&'static [&'static str], &'static str)> {
    let mut removed: Vec<(&'static [&'static str], &'static str)> = Vec::new();
    if !crate::host_options::legacy_auto_approve_flags() {
        removed.push((&["run"], "auto_approve"));
        removed.push((&["review"], "auto_approve"));
    }
    removed
}

fn hide_removed_items(mut command: clap::Command) -> clap::Command {
    fn hide_at(command: clap::Command, path: &[&'static str]) -> clap::Command {
        match path {
            [] => command,
            // The removed command is replaced by a hidden stand-in with the same name, no alias,
            // no help flag and one catch-all argument, so any way of typing it parses and is
            // then refused by `reject_removed_items` as an unknown subcommand.
            [last] => command.mut_subcommand(*last, |_| {
                clap::Command::new(*last)
                    .hide(true)
                    .disable_help_flag(true)
                    .arg(
                        clap::Arg::new("removed_command_arguments")
                            .num_args(0..)
                            .trailing_var_arg(true)
                            .allow_hyphen_values(true),
                    )
            }),
            [first, rest @ ..] => command.mut_subcommand(*first, |sub| hide_at(sub, rest)),
        }
    }
    for path in removed_subcommands() {
        command = hide_at(command, path);
    }
    for (path, id) in removed_flags() {
        fn hide_flag(command: clap::Command, path: &[&str], id: &str) -> clap::Command {
            match path {
                [] => command.mut_arg(id.to_string(), |arg| arg.hide(true)),
                [first, rest @ ..] => {
                    command.mut_subcommand(*first, |sub| hide_flag(sub, rest, id))
                }
            }
        }
        command = hide_flag(command, path, id);
    }
    if !crate::host_options::mcp_public_server() {
        // On this host `mcp-server` is the session-bound driver helper, not a command a person runs.
        command = command.mut_subcommand("mcp-server", |sub| {
            sub.hide(true).about(
                "Internal driver helper: the stdio MCP gateway bound to one live session (`--session-id`). Not a public MCP server (contract R32); hidden from help.",
            )
        });
    }
    command
}

/// Refuse what a host option removed ([`removed_subcommands`], [`removed_flags`]) the way clap
/// refuses an item that is not in the grammar. `command` is the command `matches` came from.
pub(crate) fn reject_removed_items(
    matches: &clap::ArgMatches,
    command: &clap::Command,
) -> Result<(), clap::Error> {
    // The error is raised on the command that owns the removed item, so its usage line is the
    // one clap would print for an unknown subcommand or flag there.
    let refuse = |owner: &[&str], kind: clap::error::ErrorKind, text: String| {
        let mut owning = command.clone();
        for segment in owner {
            match owning.find_subcommand(segment) {
                Some(sub) => owning = sub.clone(),
                None => break,
            }
        }
        Err(owning.error(kind, text))
    };
    for path in removed_subcommands() {
        let mut current = matches;
        let mut present = true;
        for segment in path {
            match current.subcommand() {
                Some((name, sub)) if name == *segment => current = sub,
                _ => {
                    present = false;
                    break;
                }
            }
        }
        if present {
            return refuse(
                &path[..path.len().saturating_sub(1)],
                clap::error::ErrorKind::InvalidSubcommand,
                format!(
                    "unrecognized subcommand '{}'",
                    path.last().copied().unwrap_or_default()
                ),
            );
        }
    }
    for (path, id) in removed_flags() {
        let mut current = matches;
        let mut present = true;
        for segment in path {
            match current.subcommand() {
                Some((name, sub)) if name == *segment => current = sub,
                _ => {
                    present = false;
                    break;
                }
            }
        }
        if present
            && matches!(current.try_contains_id(id), Ok(true))
            && current.value_source(id) == Some(clap::parser::ValueSource::CommandLine)
        {
            return refuse(
                path,
                clap::error::ErrorKind::UnknownArgument,
                format!("unexpected argument '--{}' found", id.replace('_', "-")),
            );
        }
    }
    Ok(())
}

/// Parse `args` through [`localized_command`] and refuse what the host's options removed.
pub(crate) fn try_cli_matches<I, T>(args: I) -> Result<clap::ArgMatches, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let command = localized_command();
    let matches = command.clone().try_get_matches_from(args)?;
    reject_removed_items(&matches, &command)?;
    Ok(matches)
}

/// The command line of a bare launch (flags only, no subcommand), localized like
/// [`localized_command`]. Parse [`LaunchCli`] through it (`LaunchCli::from_arg_matches`), not
/// through the bare derive, so its help names the host.
pub(crate) fn localized_launch_command() -> clap::Command {
    localize_command(<LaunchCli as clap::CommandFactory>::command())
        .version(crate::host_identity::version())
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
