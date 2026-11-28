//! The `--yes` gate for the headless CLI (headless-cli.md R5 to R8, R37, R38,
//! R40).
//!
//! A tool with approval `yes` (the permanent removals of R37 and every other
//! `yes` row) is a dry run without `--yes`: it prints a `dryRun` receipt on
//! stdout, changes nothing, makes no provider request, creates no session and
//! exits 0. `run`, `exec` and `-p` are the exception (R6, R40): their row is
//! `spend` with approval `none`, they do the work with no flag under
//! `auto-safe`, `--dry-run` prints the receipt and `--yes` changes nothing.
//! `--yes` never changes the permission mode of tool calls inside a run and
//! never selects `bypass`.
//!
//! Included as a module of `main_runtime` (see `main_runtime_entry.rs`).

use crate::cli::{
    CliRunArgs, Command, McpCommand, MigrateCommand, PermissionCommand, PluginCommand,
    ProviderCommand, SessionCommand,
};
use serde_json::{json, Value};

use super::{run_error_exit_on_stderr, RunExitCode};

/// `schema_version` carried by every receipt this gate prints (R12).
fn receipt_schema() -> String {
    crate::host_identity::schema("cli.receipt.v1")
}

/// The CLI name of a permission mode, as `--permission-mode` spells it.
pub(crate) fn permission_mode_name(mode: rightkit_agent_policy::PermissionMode) -> &'static str {
    use rightkit_agent_policy::PermissionMode;
    match mode {
        PermissionMode::Plan => "plan",
        PermissionMode::Ask => "ask",
        PermissionMode::AcceptEdits => "accept-edits",
        PermissionMode::AutoSafe => "auto-safe",
        PermissionMode::Bypass => "bypass",
    }
}

/// Print one receipt as a single JSON value on stdout.
fn print_receipt(receipt: &Value) {
    println!("{receipt}");
}

/// A dry run: the receipt names the effect and the plan, nothing ran, exit 0.
pub(crate) fn dry_run_exit(method: &str, effect: &str, plan: Value) -> i32 {
    print_receipt(&json!({
        "schema_version": receipt_schema(),
        "method": method,
        "status": "dryRun",
        "effect": effect,
        "plan": plan,
    }));
    if method == "run" {
        eprintln!("dry run: nothing was run; drop --dry-run to perform `run`");
    } else {
        eprintln!("dry run: nothing was run; pass --yes to perform `{method}`");
    }
    0
}

/// A refusal before anything ran: the receipt on stdout, the error object on
/// stderr, and the exit code of `code` (2 for the refusal classes).
pub(crate) fn refuse_exit(method: &str, code: RunExitCode, reason: &str) -> i32 {
    print_receipt(&json!({
        "schema_version": receipt_schema(),
        "method": method,
        "status": "refused",
        "reason": reason,
        "code": code.class(),
    }));
    run_error_exit_on_stderr(true, code, reason)
}

/// R38: `--permission-mode bypass` runs headless only in a workspace the user
/// has trusted with `{app} trust`, and only with `--yes` named in the same
/// call. Returns the refusal reason, or `None` when the call may go on.
pub(crate) fn bypass_refusal(
    mode: Option<rightkit_agent_policy::PermissionMode>,
    yes: bool,
    trust: rightkit_agent_policy::WorkspaceTrust,
) -> Option<String> {
    if mode != Some(rightkit_agent_policy::PermissionMode::Bypass) {
        return None;
    }
    if !yes {
        return Some(
            "`--permission-mode bypass` needs `--yes` in the same call; `--yes` alone never selects it"
                .to_string(),
        );
    }
    if trust != rightkit_agent_policy::WorkspaceTrust::Trusted {
        return Some(format!(
            "`--permission-mode bypass` needs a workspace trusted with `{} trust`",
            crate::host_identity::app()
        ));
    }
    None
}

/// The session a `--request-id` names (R14): the same id is the same session
/// on every path. A digest, so the id itself is not in the session id.
pub(crate) fn request_session_id(request_id: &str) -> String {
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    hasher.update(format!("{}-run-request\n", crate::host_identity::app()).as_bytes());
    hasher.update(request_id.as_bytes());
    let digest = hasher.finalize();
    format!("req-{}", &digest[..32.min(digest.len())])
}

/// The permission mode a headless `run`, `exec` or `-p` uses (R8): the one
/// named with `--permission-mode`, else `auto-safe`.
pub(crate) fn effective_run_mode(
    named: Option<rightkit_agent_policy::PermissionMode>,
) -> rightkit_agent_policy::PermissionMode {
    named.unwrap_or(rightkit_agent_policy::PermissionMode::AutoSafe)
}

/// R8: a workspace with no trust record (and no trusted ancestor) refuses the
/// run before any provider call, unless the caller passed `--trust-workspace`.
/// Returns the refusal message, which names the two ways in.
pub(crate) fn untrusted_workspace_refusal(
    trust: rightkit_agent_policy::WorkspaceTrust,
    trust_workspace_flag: bool,
) -> Option<String> {
    if trust == rightkit_agent_policy::WorkspaceTrust::Trusted || trust_workspace_flag {
        return None;
    }
    Some(
        "this workspace is not trusted: trust it from the TUI or the desktop app (`/trust`), or pass `--trust-workspace` on this call"
            .to_string(),
    )
}

fn trust_label(trust: rightkit_agent_policy::WorkspaceTrust) -> &'static str {
    match trust {
        rightkit_agent_policy::WorkspaceTrust::Trusted => "trusted",
        _ => "untrusted",
    }
}

/// The plan a `run` dry run reports: the route as given, the workspace and
/// its trust, the permission mode, the tool selection and the optional budget
/// cap. No daemon is contacted and no model is called to build it.
pub(crate) fn run_plan(
    args: &CliRunArgs,
    workspace_root: &str,
    trust: rightkit_agent_policy::WorkspaceTrust,
) -> Value {
    let mode = args
        .permission_mode
        .map(|mode| mode.to_policy())
        .unwrap_or(rightkit_agent_policy::PermissionMode::AutoSafe);
    json!({
        "provider": args.provider,
        "model": args.model,
        "workspace": workspace_root,
        "trust": trust_label(trust),
        "permission_mode": permission_mode_name(mode),
        "bypass_possible": mode == rightkit_agent_policy::PermissionMode::Bypass,
        "toolset": args.toolset,
        "tools": args.allowed_tools.iter().chain(args.tools.iter()).collect::<Vec<_>>(),
        "exclude_tools": args.disallowed_tools.iter().chain(args.exclude_tools.iter()).collect::<Vec<_>>(),
        "max_budget_usd": args.max_budget_usd,
        "max_turns": args.max_turns,
        "session_id": args.session_id,
        "continue": args.continue_last,
        "fork": args.fork,
        "worktree": args.worktree,
        "trust_workspace": args.trust_workspace,
        "ephemeral": args.ephemeral || args.no_session,
    })
}

/// The destroy verbs of R37, with the plan each would carry out. `None` for
/// every other command.
pub(crate) fn destroy_plan(command: &Command) -> Option<(&'static str, Value)> {
    match command {
        Command::Session {
            command: SessionCommand::Delete { session_id },
        } => Some((
            "session delete",
            json!({ "session_id": session_id, "would": "delete the session record and its descendants and stop the live session" }),
        )),
        Command::Provider {
            command: ProviderCommand::Logout { provider },
        } => Some((
            "provider logout",
            json!({ "provider": provider, "would": "remove the provider OAuth and API credentials" }),
        )),
        Command::Provider {
            command: ProviderCommand::Disconnect { provider },
        } => Some((
            "provider disconnect",
            json!({ "provider": provider, "would": "remove the stored API key" }),
        )),
        Command::Mcp {
            command: McpCommand::Remove { name, scope, .. },
        } => Some((
            "mcp remove",
            json!({ "name": name, "scope": scope, "would": "remove the MCP server from config" }),
        )),
        Command::Plugin {
            command: PluginCommand::Remove { id },
        } => Some((
            "plugin remove",
            json!({ "id": id, "would": "delete the plugin folder and its enabled record" }),
        )),
        Command::Permission {
            command:
                PermissionCommand::RuleRemove {
                    session_id, index, ..
                },
        } => Some((
            "permission rule-remove",
            json!({ "session_id": session_id, "index": index, "would": "remove the permission rule" }),
        )),
        Command::Migrate {
            command: MigrateCommand::Apply { db },
        } => Some((
            "migrate apply",
            json!({ "db": db, "would": "rewrite the database schema in place" }),
        )),
        _ => None,
    }
}
