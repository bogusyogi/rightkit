//! The CLI call journal (headless-cli.md R19): one content-free JSONL line per
//! call, with the tool, its effect class, the approval state, the caller
//! label, the request id, the exit code, the duration and the host route.
//! Never a prompt, a path or any output.
//!
//! The journal is `rightkit-agent-tools`' (R33): `CompletedCallJournal` owns the
//! file `<state root>/cli-journal.jsonl`, its 1 MiB rotation with four numbered
//! predecessors and the validation of every field. This module only records
//! the call before it runs and hands the finished call to the owner.

use std::sync::Mutex;
use std::time::Instant;

/// Longest caller label kept. The label is a claim, never an identity.
const CALLER_MAX_CHARS: usize = 64;
/// Longest request id kept in a line.
const REQUEST_ID_MAX_CHARS: usize = 256;
/// `schema_version` of one journal line: `{app}.cli.journal.v1`.
pub(crate) fn journal_schema() -> String {
    crate::host_identity::schema("cli.journal.v1")
}

/// What is known about a call before it runs.
pub(crate) struct PendingCall {
    pub tool: String,
    pub effect: &'static str,
    pub approval_state: &'static str,
    pub caller: String,
    pub request_id: Option<String>,
    pub host_route: &'static str,
    started: Instant,
}

static PENDING: Mutex<Option<PendingCall>> = Mutex::new(None);

/// The caller label: `RIGHT_CTL_CALLER` as given, trimmed, control
/// characters dropped, bounded; `unknown` when absent. Never an identity.
pub(crate) fn caller_label(raw: Option<&str>) -> String {
    let cleaned: String = raw
        .unwrap_or("")
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(CALLER_MAX_CHARS)
        .collect();
    if cleaned.is_empty() {
        "unknown".to_string()
    } else {
        cleaned
    }
}

/// The approval state of a call: what `--yes` did to it.
pub(crate) fn approval_state(approval: &str, yes: bool) -> &'static str {
    match (approval, yes) {
        ("yes", true) => "granted",
        ("yes", false) => "dry_run",
        ("in_app", _) => "in_app",
        _ => "none",
    }
}

/// Record the call about to run. One call per process.
pub(crate) fn begin(
    tool: &str,
    effect: &'static str,
    approval: &str,
    yes: bool,
    host_route: &'static str,
    caller: Option<&str>,
    request_id: Option<&str>,
) {
    // `--caller` wins over `RIGHT_CTL_CALLER`; both are claims.
    let env_caller = std::env::var("RIGHT_CTL_CALLER").ok();
    let call = PendingCall {
        tool: tool.to_string(),
        effect,
        approval_state: approval_state(approval, yes),
        caller: caller_label(caller.or(env_caller.as_deref())),
        request_id: request_id.map(|id| id.chars().take(REQUEST_ID_MAX_CHARS).collect()),
        host_route,
        started: Instant::now(),
    };
    *PENDING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(call);
}

/// Write the one line of the call recorded by [`begin`], once. A process with
/// no recorded call (`--help`, `--version`, a parse error) writes nothing.
pub(crate) fn finish(exit_code: i32) {
    let call = PENDING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    let Some(call) = call else {
        return;
    };
    let Some(root) = rightkit_agent_config::state_root() else {
        return;
    };
    let millis = u64::try_from(call.started.elapsed().as_millis()).unwrap_or(u64::MAX);
    // The journal wants a `'static` schema id. `finish` records one call per process (`PENDING`
    // is taken once), so the id is leaked once, after the host identity is installed.
    let schema: &'static str = Box::leak(journal_schema().into_boxed_str());
    let journal = rightkit_agent_tools::CompletedCallJournal::new(root, schema);
    // A journal that cannot be written never changes the call's exit code.
    if let Err(error) = journal.append(&rightkit_agent_tools::CompletedCall {
        tool: &call.tool,
        effect: call.effect,
        approval_state: call.approval_state,
        caller: &call.caller,
        request_id: call.request_id.as_deref(),
        exit_code,
        duration_ms: millis,
        host_route: call.host_route,
    }) {
        tracing::debug!(%error, "could not write the CLI call journal line");
    }
}
