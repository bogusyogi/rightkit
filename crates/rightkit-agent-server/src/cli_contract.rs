//! `{app} contract`: the CLI's machine-readable manifest (headless-cli.md
//! section 2.2, R1 to R4, R25, R26, R28, R31). Available on a host that turns
//! `host_options::cli_contract` on; the same table then also gives every call its journal row and
//! the dry-run receipt of a tool whose approval is `yes`.
//!
//! The manifest is generated from the clap grammar and one table of effect
//! rows. It reads nothing but the binary itself: no daemon, no network, no
//! model call, no state. A clap command with no row in the table, or a row
//! that names no clap command, fails the tests below (R2), so the manifest and
//! `--help` cannot drift.
//!
//! Included as a module of `main_runtime` (see `main_runtime_entry.rs`).
//!
//! The manifest builder belongs to `rightkit-agent-tools` (R33, OD-13) once
//! that crate publishes `host::run`; until then this module holds the table
//! and the minimal builder.

use clap::{Arg, ArgAction, Command as ClapCommand};
use serde_json::{json, Map, Value};

/// `schema_version` of the manifest document.
pub(crate) const CONTRACT_SCHEMA: &str = "rightkit-agent-tools/app-contract/1";
/// Suite version of the shared schema (headless-cli.md section 2.2).
pub(crate) const CONTRACT_VERSION: u32 = 2;

/// Platforms with an installed qualification result (R26). No installed-build
/// result exists for any platform yet (D28), so none is stated.
const QUALIFIED_PLATFORMS: &[&str] = &[];

/// One effect row. A row applies to its command path and to every sub-verb
/// below it unless a longer row names the sub-verb.
pub(crate) struct Row {
    pub path: &'static str,
    pub effect: &'static str,
    pub approval: &'static str,
    pub availability: &'static str,
    pub interactive: bool,
    pub long_running: bool,
    pub spawns_daemon: bool,
    /// The flag that prints the dry-run receipt, for a tool that is not a
    /// dry run by default (`run`: `--dry-run`).
    pub dry_run: Option<&'static str>,
}

const fn row(path: &'static str, effect: &'static str, approval: &'static str) -> Row {
    Row {
        path,
        effect,
        approval,
        availability: "engine",
        interactive: false,
        long_running: false,
        spawns_daemon: false,
        dry_run: None,
    }
}

/// A host verb: needs the running daemon and starts one when none is found
/// (`cli_presence.rs`), unless `{ENV_PREFIX}_NO_AUTOSTART=1`.
const fn host(path: &'static str, effect: &'static str, approval: &'static str) -> Row {
    Row {
        availability: "host",
        spawns_daemon: true,
        ..row(path, effect, approval)
    }
}

/// The flag that prints this tool's dry-run receipt instead of performing it.
const fn dry_run(row: Row, flag: &'static str) -> Row {
    Row {
        dry_run: Some(flag),
        ..row
    }
}

const fn long(row: Row) -> Row {
    Row {
        long_running: true,
        ..row
    }
}

const fn interactive(row: Row) -> Row {
    Row {
        interactive: true,
        ..row
    }
}

const fn no_spawn(row: Row) -> Row {
    Row {
        spawns_daemon: false,
        ..row
    }
}

/// Commands that are not M22 tools, each with its reason (R2, R28, R36).
const NOT_A_TOOL: &[(&str, &str)] = &[
    (
        "serve",
        "the daemon itself: spawned by the desktop shell or by autostart, never called as a tool",
    ),
    (
        "mcp-server",
        "internal gateway the daemon spawns for foreign-CLI drivers, bound to one live session (--session-id); not a public MCP server",
    ),
];

/// The effect table (headless-cli.md section 2.3). Longest matching path wins.
pub(crate) fn rows() -> Vec<Row> {
    vec![
        // Read, no approval, runs in-process.
        row("contract", "read", "none"),
        row("help", "read", "none"),
        row("completion", "read", "none"),
        row("info", "read", "none"),
        row("caps", "read", "none"),
        row("config", "read", "none"),
        row("models", "read", "none"),
        row("local-model", "read", "none"),
        row("toolset", "read", "none"),
        row("execution", "read", "none"),
        row("doctor", "read", "none"),
        row("execpolicy check", "read", "none"),
        row("features list", "read", "none"),
        row("debug prompt-input", "read", "none"),
        row("diff", "read", "none"),
        row("migrate status", "read", "none"),
        // Read through the daemon projection.
        no_spawn(host("status", "read", "none")),
        host("ps", "read", "none"),
        host("sessions", "read", "none"),
        host("agents", "read", "none"),
        host("logs", "read", "none"),
        host("export", "read", "none"),
        host("handoff", "read", "none"),
        host("web", "read", "none"),
        host("share create", "write", "none"),
        host("share list", "read", "none"),
        host("share revoke", "write", "none"),
        row("discover", "read", "none"),
        long(row("ssh", "read", "none")),
        host("insights", "read", "none"),
        host("diagnostics", "read", "none"),
        host("diagnostics bundle", "write", "yes"),
        // The user's workspace-trust store: journaled, never `none`.
        row("trust", "write", "in_app"),
        // Spend: the user's provider credits.
        dry_run(long(row("run", "spend", "none")), "--dry-run"),
        long(row("rpc", "spend", "none")),
        // The Agent Client Protocol bridge: an editor drives sessions of an already running
        // daemon (`--port`) over stdio; it starts none.
        long(no_spawn(host("acp-server", "spend", "none"))),
        long(host("conduct", "spend", "yes")),
        long(host("review", "spend", "yes")),
        long(row("recipe run", "spend", "yes")),
        row("recipe", "read", "none"),
        long(host("bot run", "spend", "yes")),
        host("bot", "write", "yes"),
        host("bot list", "read", "none"),
        host("bot show", "read", "none"),
        host("bot export", "read", "none"),
        host("spec", "read", "none"),
        host("spec new", "write", "yes"),
        long(host("spec clarify", "spend", "yes")),
        long(host("spec plan", "spend", "yes")),
        long(host("spec checklist", "spend", "yes")),
        long(host("spec implement", "spend", "yes")),
        // Skills and learning.
        host("skill", "read", "none"),
        host("skill propose", "write", "yes"),
        host("skill quarantine-proposal", "write", "yes"),
        host("skill benchmark-proposal", "write", "yes"),
        host("skill quarantine", "write", "yes"),
        host("skill activate", "write", "yes"),
        host("skill deactivate", "write", "yes"),
        host("skill retire", "write", "yes"),
        host("skill cancel-training-run", "write", "yes"),
        long(host("skill run-benchmark-proposal", "spend", "yes")),
        long(host("skill propose-training-candidate", "spend", "yes")),
        long(host("skill execute-training-run", "spend", "yes")),
        long(host("skill start-training-run", "spend", "yes")),
        long(host("skill resume-training-run", "spend", "yes")),
        host("learning", "write", "yes"),
        // Listings of what was learned only read it.
        host("learning list", "read", "none"),
        host("learning instincts", "read", "none"),
        // Providers, MCP, plugins, hooks, permissions.
        host("provider", "read", "none"),
        host("provider login", "write", "yes"),
        host("provider connect", "write", "yes"),
        host("provider rotate", "write", "yes"),
        host("provider set-visibility", "write", "yes"),
        host("provider test", "external", "yes"),
        row("provider set-key", "write", "yes"),
        host("provider logout", "destroy", "yes"),
        host("provider disconnect", "destroy", "yes"),
        host("mcp", "read", "none"),
        host("mcp status", "read", "none"),
        host("mcp test", "external", "yes"),
        host("mcp probe", "external", "yes"),
        host("mcp login", "external", "yes"),
        host("mcp approve", "write", "yes"),
        host("mcp add-json", "write", "yes"),
        host("mcp install", "write", "yes"),
        host("mcp import", "write", "yes"),
        host("mcp remove", "destroy", "yes"),
        host("plugin", "read", "none"),
        host("plugin install", "write", "yes"),
        host("plugin marketplace", "read", "none"),
        host("plugin install-marketplace", "write", "yes"),
        host("plugin marketplace-sync", "write", "yes"),
        host("plugin remove", "destroy", "yes"),
        host("plugin enable", "write", "yes"),
        host("plugin disable", "write", "yes"),
        host("plugin reload", "write", "yes"),
        host("hooks", "read", "none"),
        host("hooks test", "external", "yes"),
        host("hooks revoke", "write", "yes"),
        host("permission", "read", "none"),
        host("permission mode", "write", "yes"),
        host("permission rule-add", "write", "yes"),
        host("permission rule-remove", "destroy", "yes"),
        // Remote control, browser, schedules.
        host("remote", "read", "none"),
        host("remote test", "external", "yes"),
        host("remote smoke", "external", "yes"),
        host("remote setup", "write", "yes"),
        host("remote users", "write", "yes"),
        host("browser", "external", "yes"),
        host("browser status", "read", "none"),
        host("browser inspect", "read", "none"),
        host("schedule", "read", "none"),
        host("schedule create", "write", "yes"),
        host("schedule edit", "write", "yes"),
        host("schedule pause", "write", "yes"),
        host("schedule resume", "write", "yes"),
        long(host("schedule run", "spend", "yes")),
        host("schedule delete", "destroy", "yes"),
        // Session and process control.
        host("stop", "write", "yes"),
        host("kill", "write", "yes"),
        host("queue", "write", "yes"),
        host("fork", "write", "yes"),
        host("session", "read", "none"),
        host("session set", "write", "yes"),
        host("session archive", "write", "yes"),
        host("session restore", "write", "yes"),
        host("session delete", "destroy", "yes"),
        host("app", "read", "none"),
        host("app open", "write", "yes"),
        host("migrate apply", "destroy", "yes"),
        // Interactive: need a person at a terminal.
        interactive(host("resume", "write", "yes")),
        interactive(host("attach", "write", "yes")),
        interactive(row("onboard", "write", "yes")),
        interactive(row("login", "write", "yes")),
        row("logout", "destroy", "yes"),
        // Writes through the daemon's session store, but is its own dry run: without
        // `--yes` it prints what it would import (headless-cli R6 with a richer plan).
        host("import", "write", "none"),
        row("apply", "write", "yes"),
    ]
}

/// Rows for commands that only some builds of the grammar carry (a host that removes the plugin
/// marketplace or the MCP status verb drops the command, not the row).
#[cfg(test)]
const COMMANDS_THAT_MAY_BE_ABSENT: &[&str] = &[
    "mcp status",
    "plugin marketplace",
    "plugin install-marketplace",
    "plugin marketplace-sync",
];

/// The row that applies to a command path (space separated): the longest row
/// whose path equals it or is a prefix of it at a word boundary.
pub(crate) fn row_for<'a>(rows: &'a [Row], path: &str) -> Option<&'a Row> {
    rows.iter()
        .filter(|row| path == row.path || path.starts_with(&format!("{} ", row.path)))
        .max_by_key(|row| row.path.len())
}

/// Every clap leaf command (a command with no sub-commands) as a path, with
/// the clap command it names. Aliases are not separate entries.
pub(crate) fn leaves(command: &ClapCommand) -> Vec<(String, ClapCommand)> {
    fn walk(prefix: &str, command: &ClapCommand, out: &mut Vec<(String, ClapCommand)>) {
        for sub in command.get_subcommands() {
            // clap's generated `help` sub-verb under a group is not a tool;
            // the root `help` is the explicit `Command::Help`.
            if !prefix.is_empty()
                && sub.get_name() == "help"
                && sub.get_subcommands().next().is_none()
            {
                continue;
            }
            let path = if prefix.is_empty() {
                sub.get_name().to_string()
            } else {
                format!("{prefix} {}", sub.get_name())
            };
            if sub.get_subcommands().next().is_some() {
                walk(&path, sub, out);
            } else {
                out.push((path, sub.clone()));
            }
        }
    }
    let mut out = Vec::new();
    walk("", command, &mut out);
    out
}

/// The JSON-schema type of one clap argument.
fn arg_schema(arg: &Arg) -> Value {
    let described = |mut schema: Map<String, Value>| {
        if let Some(help) = arg.get_help() {
            schema.insert("description".into(), json!(help.to_string()));
        }
        Value::Object(schema)
    };
    let mut schema = Map::new();
    match arg.get_action() {
        ArgAction::SetTrue | ArgAction::SetFalse => {
            schema.insert("type".into(), json!("boolean"));
        }
        ArgAction::Count => {
            schema.insert("type".into(), json!("integer"));
        }
        ArgAction::Append => {
            schema.insert("type".into(), json!("array"));
            schema.insert("items".into(), json!({"type": "string"}));
        }
        _ => {
            if arg
                .get_num_args()
                .is_some_and(|range| range.max_values() > 1)
            {
                schema.insert("type".into(), json!("array"));
                schema.insert("items".into(), json!({"type": "string"}));
            } else {
                schema.insert("type".into(), json!("string"));
                let possible: Vec<String> = arg
                    .get_possible_values()
                    .iter()
                    .map(|value| value.get_name().to_string())
                    .collect();
                if !possible.is_empty() {
                    schema.insert("enum".into(), json!(possible));
                }
            }
        }
    }
    described(schema)
}

/// The `input_schema` of one tool: an object with one property per argument.
fn input_schema(command: &ClapCommand) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for arg in command.get_arguments() {
        let id = arg.get_id().as_str();
        if matches!(id, "help" | "version") || arg.is_hide_set() {
            continue;
        }
        properties.insert(id.to_string(), arg_schema(arg));
        if arg.is_required_set() {
            required.push(id.to_string());
        }
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

fn output_schema(row: &Row) -> Value {
    let mut schema = json!({
        "type": "object",
        "properties": { "schema_version": { "type": "string" } },
    });
    if row.approval == "yes" {
        schema["description"] = json!(
            "Without --yes: a dryRun receipt {method, status, effect, plan}. With --yes: the result."
        );
    }
    schema
}

/// Whether this host offers the command. The commands of an optional feature are in the grammar
/// of every host and refuse where their option is off, so the manifest leaves them out there.
fn offered(path: &str) -> bool {
    match path.split(' ').next().unwrap_or(path) {
        "rpc" => crate::host_options::rpc_mode(),
        "share" => crate::host_options::guest_shares(),
        "ssh" => crate::host_options::remote_ssh(),
        "discover" => crate::host_options::lan_listener(),
        "web" => crate::api_web::web_client_enabled(),
        _ => true,
    }
}

/// Build the manifest. Pure: no I/O.
pub(crate) fn manifest() -> Value {
    let root = crate::cli::localized_command();
    let table = rows();
    let mut tools = Vec::new();
    for (path, command) in leaves(&root) {
        if NOT_A_TOOL
            .iter()
            .any(|(name, _)| path == *name || path.starts_with(&format!("{name} ")))
        {
            continue;
        }
        if !offered(&path) {
            continue;
        }
        let Some(row) = row_for(&table, &path) else {
            continue;
        };
        let mut tool = json!({
            "name": path,
            "summary": command.get_about().map(|about| about.to_string()).unwrap_or_default(),
            "input_schema": input_schema(&command),
            "output_schema": output_schema(row),
            "effect": row.effect,
            "approval": row.approval,
            "availability": row.availability,
            "licence": "free",
            "long_running": row.long_running,
            "idempotent": row.effect == "read",
            "spawns_daemon": row.spawns_daemon,
            "interactive": row.interactive,
        });
        if row.long_running {
            tool["events_schema"] = json!({"type": "object"});
        }
        if let Some(flag) = row.dry_run {
            tool["dry_run"] = json!(flag);
        }
        if path == "run" {
            tool["bypass_possible"] = json!(true);
        }
        tools.push(tool);
    }
    let not_a_tool: Vec<Value> = NOT_A_TOOL
        .iter()
        .map(|(name, reason)| json!({"name": name, "reason": reason}))
        .collect();
    let app = crate::host_identity::app();
    json!({
        "contract": format!("{app}-cli"),
        "contract_version": CONTRACT_VERSION,
        "schema": CONTRACT_SCHEMA,
        "schema_version": CONTRACT_SCHEMA,
        "app": app,
        "app_version": env!("CARGO_PKG_VERSION"),
        "bundle_id": format!("app.{app}"),
        "platforms": QUALIFIED_PLATFORMS,
        "exit_codes": {
            "0": "success, dry run, or accepted",
            "1": "the call ran and the requested state was not reached",
            "2": "refused or bad usage; nothing ran",
            "3": "no result is known (unreachable: nothing ran; unknown: the call may have run)",
        },
        "error_codes": {
            "1": ["provider_auth", "tool_denied", "interaction_required", "budget_exceeded", "sandbox", "timeout", "schema_violation", "interrupted", "generic"],
            "2": ["usage", "unknown_command", "untrusted_workspace", "licence_required", "interactive_only", "refused_by_policy"],
            "3": ["unreachable", "unknown"],
        },
        "conventions": {
            "yes": "--yes performs the call; a tool with approval `yes` is a dry run without it. `run`, `exec` and `-p` are the work with no flag (`--dry-run` prints the receipt) and `--yes` changes nothing there. It never changes the permission mode of tool calls inside a run and never selects bypass.",
            "json": "--json prints exactly one JSON value on stdout; every value carries schema_version.",
            "request_id": "--request-id makes a retried call idempotent.",
            "stdout": "the result or receipt, one JSON value per result in --json mode",
            "stderr": "the error object {error:{code,message,retryable}}, progress events and human text",
        },
        "not_a_tool": not_a_tool,
        "tools": tools,
    })
}

/// The command path (`session delete`) and the leaf's own command-line
/// arguments of a parsed invocation, derived from clap so aliases resolve to
/// the canonical name.
pub(crate) fn invoked_path(raw_args: &[String]) -> Option<(String, Map<String, Value>)> {
    let matches = crate::cli::localized_command()
        .try_get_matches_from(raw_args.iter())
        .ok()?;
    let mut path = Vec::new();
    let mut command = crate::cli::localized_command();
    let mut current = &matches;
    while let Some((name, sub)) = current.subcommand() {
        path.push(name.to_string());
        command = command.find_subcommand(name)?.clone();
        current = sub;
    }
    let mut given = Map::new();
    for arg in command.get_arguments() {
        let id = arg.get_id().as_str();
        if matches!(id, "help" | "version" | "yes") || arg.is_hide_set() {
            continue;
        }
        if current.value_source(id) != Some(clap::parser::ValueSource::CommandLine) {
            continue;
        }
        let values: Vec<String> = current
            .get_raw(id)
            .map(|raw| {
                raw.map(|value| value.to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        let value = match arg.get_action() {
            ArgAction::SetTrue => json!(true),
            ArgAction::SetFalse => json!(false),
            ArgAction::Count => json!(values.len()),
            _ if values.len() == 1 && !matches!(arg.get_action(), ArgAction::Append) => {
                json!(values[0])
            }
            _ => json!(values),
        };
        given.insert(id.to_string(), value);
    }
    Some((path.join(" "), given))
}

/// R6: a tool with approval `yes` is a dry run without `--yes`. Returns the
/// method, the effect and the plan for an invocation that must stop at a dry
/// run, and `None` for one that may go on (a read tool, an interactive
/// command, `run`, which has its own richer gate, and `review findings`, which
/// reads a finished review).
pub(crate) fn dry_run_plan(raw_args: &[String]) -> Option<(String, &'static str, Value)> {
    let (path, given) = invoked_path(raw_args)?;
    let table = rows();
    let row = row_for(&table, &path)?;
    if row.approval != "yes" || row.interactive {
        return None;
    }
    let first_focus = given.get("focus").and_then(|focus| match focus {
        Value::Array(items) => items.first().and_then(Value::as_str),
        other => other.as_str(),
    });
    if path == "review" && first_focus == Some("findings") {
        return None;
    }
    let given = redact_secret_arguments(given);
    Some((
        path,
        row.effect,
        json!({ "arguments": given, "availability": row.availability }),
    ))
}

/// A dry-run plan echoes the arguments it would act on; a credential given as an argument
/// (`--telegram-token`, any `*_token`, `*_secret`, `*_password`, `*api_key`) is shown as
/// `[REDACTED]` instead. Flags (`--clear-token`) keep their boolean value.
fn redact_secret_arguments(mut given: Map<String, Value>) -> Map<String, Value> {
    for (id, value) in given.iter_mut() {
        let id = id.to_ascii_lowercase().replace('-', "_");
        let secret = ["token", "secret", "password", "api_key", "apikey"]
            .iter()
            .any(|suffix| id.ends_with(suffix));
        if !secret {
            continue;
        }
        match value {
            Value::String(_) => *value = json!("[REDACTED]"),
            Value::Array(items) => {
                for item in items.iter_mut().filter(|item| item.is_string()) {
                    *item = json!("[REDACTED]");
                }
            }
            _ => {}
        }
    }
    given
}

/// `{app} contract [--json]`: print the manifest and return exit 0.
pub(crate) fn print(json_mode: bool) -> i32 {
    let document = manifest();
    let text = if json_mode {
        document.to_string()
    } else {
        serde_json::to_string_pretty(&document).unwrap_or_default()
    };
    println!("{text}");
    0
}

#[cfg(test)]
mod secret_argument_tests {
    #[test]
    fn dry_run_plan_never_echoes_a_token() {
        let argv: Vec<String> = [
            "app",
            "remote",
            "setup",
            "--telegram-token",
            "123456:SECRET-bot-token",
            "--allowed-user",
            "1",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let (path, _, plan) = super::dry_run_plan(&argv).expect("remote setup is a dry run");
        assert_eq!(path, "remote setup");
        let text = plan.to_string();
        assert!(!text.contains("SECRET"), "{text}");
        assert_eq!(plan["arguments"]["telegram_token"], "[REDACTED]");
    }

    #[test]
    fn flags_and_plain_values_are_kept() {
        let mut given = serde_json::Map::new();
        given.insert("clear_token".into(), serde_json::json!(true));
        given.insert("allowed_users".into(), serde_json::json!(["1"]));
        given.insert("api_key".into(), serde_json::json!(["k1", "k2"]));
        let out = super::redact_secret_arguments(given);
        assert_eq!(out["clear_token"], true);
        assert_eq!(out["allowed_users"], serde_json::json!(["1"]));
        assert_eq!(
            out["api_key"],
            serde_json::json!(["[REDACTED]", "[REDACTED]"])
        );
    }
}

#[cfg(test)]
mod table_tests {
    use super::{leaves, row_for, rows, COMMANDS_THAT_MAY_BE_ABSENT, NOT_A_TOOL};

    fn is_not_a_tool(path: &str) -> bool {
        NOT_A_TOOL
            .iter()
            .any(|(name, _)| path == *name || path.starts_with(&format!("{name} ")))
    }

    /// R2: a clap command with no row fails here, so the manifest and `--help` cannot drift.
    #[test]
    fn every_command_has_a_row_or_is_not_a_tool() {
        let table = rows();
        let missing: Vec<String> = leaves(&crate::cli::localized_command())
            .into_iter()
            .map(|(path, _)| path)
            .filter(|path| !is_not_a_tool(path) && row_for(&table, path).is_none())
            .collect();
        assert!(
            missing.is_empty(),
            "commands with no contract row: {missing:?}"
        );
    }

    /// R2: a row that names no command fails here.
    #[test]
    fn every_row_names_a_command_the_grammar_has() {
        let paths: Vec<String> = leaves(&crate::cli::localized_command())
            .into_iter()
            .map(|(path, _)| path)
            .collect();
        let stray: Vec<&str> = rows()
            .iter()
            .map(|row| row.path)
            .filter(|row| {
                !COMMANDS_THAT_MAY_BE_ABSENT.contains(row)
                    && !paths
                        .iter()
                        .any(|path| path == row || path.starts_with(&format!("{row} ")))
            })
            .collect();
        assert!(stray.is_empty(), "rows that name no command: {stray:?}");
    }

    #[test]
    fn a_command_that_is_not_a_tool_has_no_row() {
        let table = rows();
        for (name, _) in NOT_A_TOOL {
            assert!(row_for(&table, name).is_none(), "{name}");
        }
    }
}
