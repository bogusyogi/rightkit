//! Machine inventory dump for the dogfood qualification harness.
//!
//! Emits the two authorities that have no HTTP surface — the Clap grammar and
//! the shared slash registry (`command_specs()`) — as sorted, deterministic JSON.
//! The daemon OpenAPI operation set is served live at `/api/contract`, so it is
//! captured harness-side, not here.
//!
//! This is invoked via the hidden `__dogfood-inventory` pseudo-command, which
//! is intercepted on raw args BEFORE `Cli::parse()` (see `main_runtime_entry`).
//! Intercepting pre-parse is deliberate: it means we add NO variant to the
//! `Command` enum, so the grammar this dumper reports is exactly the grammar
//! that ships — no synthetic leaf contaminates the drift baseline.
//!
//! Coverage note: clap 4.6 exposes names, aliases, longs/shorts, positional
//! flag, required, defaults and possible-values (enums) through stable public
//! getters; it does NOT expose per-arg conflicts publicly, so conflicts are not
//! part of this digest. The harness records that limitation rather than
//! implying full conflict coverage.

use clap::{Command as ClapCommand, CommandFactory};
use rightkit_agent_host_traits::skill_host::command_specs;

use crate::cli::Cli;

/// Build the full inventory JSON string (`{"cli": [...], "slash": [...]}`).
pub(crate) fn inventory_json() -> String {
    let mut cli = Vec::new();
    walk(&Cli::command(), &mut Vec::new(), &mut cli);
    cli.sort();
    cli.dedup();

    let mut slash: Vec<String> = command_specs().iter().map(slash_descriptor).collect();
    slash.sort();
    slash.dedup();

    serde_json::json!({ "cli": cli, "slash": slash }).to_string()
}

/// Depth-first traversal emitting one deterministic descriptor per command
/// node (root excluded, every subcommand at every depth included).
fn walk(cmd: &ClapCommand, path: &mut Vec<String>, out: &mut Vec<String>) {
    let name = cmd.get_name().to_string();
    // Skip the synthetic root frame; record every real command node.
    if !path.is_empty() || name != Cli::command().get_name() {
        path.push(name);
        out.push(command_descriptor(cmd, path));
    }
    for sub in cmd.get_subcommands() {
        walk(sub, path, out);
    }
    if !path.is_empty() {
        path.pop();
    }
}

fn command_descriptor(cmd: &ClapCommand, path: &[String]) -> String {
    let mut aliases: Vec<String> = cmd.get_all_aliases().map(|a| a.to_string()).collect();
    aliases.sort();

    let mut args: Vec<String> = cmd.get_arguments().map(arg_descriptor).collect();
    args.sort();

    format!(
        "{} :: aliases=[{}] args=[{}]",
        path.join(" "),
        aliases.join(","),
        args.join(" "),
    )
}

fn arg_descriptor(arg: &clap::Arg) -> String {
    let id = arg.get_id().as_str();
    let long = arg.get_long().map(|l| format!("--{l}")).unwrap_or_default();
    let short = arg.get_short().map(|c| format!("-{c}")).unwrap_or_default();
    let kind = if arg.is_positional() { "pos" } else { "opt" };
    let required = arg.is_required_set();

    let mut defaults: Vec<String> = arg
        .get_default_values()
        .iter()
        .map(|v| v.to_string_lossy().into_owned())
        .collect();
    defaults.sort();

    let mut values: Vec<String> = arg
        .get_possible_values()
        .iter()
        .map(|v| v.get_name().to_string())
        .collect();
    values.sort();

    format!(
        "{id}({kind}{}{}{}req={required}{}{})",
        if long.is_empty() {
            String::new()
        } else {
            format!(",{long}")
        },
        if short.is_empty() {
            String::new()
        } else {
            format!(",{short}")
        },
        ",",
        if defaults.is_empty() {
            String::new()
        } else {
            format!(",default=[{}]", defaults.join("|"))
        },
        if values.is_empty() {
            String::new()
        } else {
            format!(",enum=[{}]", values.join("|"))
        },
    )
}

fn slash_descriptor(
    spec: &rightkit_agent_host_traits::skill_host::commands::CommandSpec,
) -> String {
    let mut aliases: Vec<&str> = spec.aliases.to_vec();
    aliases.sort_unstable();
    let mut args: Vec<String> = spec
        .args
        .iter()
        .map(|a| format!("{}(req={})", a.name, a.required))
        .collect();
    args.sort();
    format!(
        "{} :: aliases=[{}] scope={:?} effect={:?} mid_turn={} \
         surfaces=cli:{},int:{} args=[{}]",
        spec.name,
        aliases.join(","),
        spec.scope,
        spec.effect,
        spec.valid_mid_turn,
        spec.surfaces.cli,
        spec.surfaces.interactive,
        args.join(" "),
    )
}
