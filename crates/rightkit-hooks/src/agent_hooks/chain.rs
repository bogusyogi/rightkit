//! Typed hook results, matchers, and ordered hook chains.
//!
//! This module is the single authority for interpreting a hook command's
//! result (exit code plus optional JSON on stdout) and for composing several
//! hooks that subscribe to the same event:
//!
//! * hooks run in configured order;
//! * any `deny` wins immediately and discards earlier mutations;
//! * `ask` outranks `allow`;
//! * `updatedInput` mutations compose in order (each hook sees the previous
//!   hook's output);
//! * a `PreToolUse` timeout, signal kill, invalid configuration, or malformed
//!   decision fails closed (deny).

use crate::agent_hooks::bus::ProcessLaunchAdapter;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

/// Default per-hook timeout.
pub const DEFAULT_HOOK_TIMEOUT: Duration = Duration::from_secs(30);
/// Upper bound accepted from configuration.
pub const MAX_HOOK_TIMEOUT_SECS: u64 = 600;
/// Env var holding the multi-hook configuration JSON (the JSON form of rule 58). The other
/// two surfaces are the `[hooks]` config key and the project hook file; there is no per-event
/// environment variable.
pub const HOOKS_JSON_ENV: &str = "RIGHTKIT_HOOKS_JSON";

/// Project hook file, relative to the workspace root. Read only in a trusted workspace (rules 13
/// and 58).
pub const PROJECT_HOOK_FILE: &str = ".rightkit/hooks.json";

/// The project hook file path under the host's state folder (`<state dir>/hooks.json`).
pub fn project_hook_file() -> String {
    format!("{}/hooks.json", crate::agent_hooks::state_dir_name())
}

/// Every event name in the supported taxonomy (rule 58, parity list Q44): Claude Code's events
/// including the permission-request and tool-failure events, and the Codex events this crate
/// lacked (`PreModelRequest`, `PostModelRequest`, `PostCompact`, `SubagentStart`, `Interrupt`).
/// The first nine are the events published on the bus as `HookEvent` variants, in that order;
/// `PreCommit` is the auditor gate that runs outside the bus enum.
pub const HOOK_EVENT_NAMES: &[&str] = &[
    "PreToolUse",
    "PostToolUse",
    "UserPromptSubmit",
    "Stop",
    "SubagentStop",
    "SessionStart",
    "SessionEnd",
    "PreCompact",
    "Notification",
    "PostToolUseFailure",
    "PermissionRequest",
    "PreModelRequest",
    "PostModelRequest",
    "PostCompact",
    "SubagentStart",
    "Interrupt",
    "PostToolBatch",
    "PermissionDenied",
    "PreModelSwitch",
    "PostModelSwitch",
    "DirectoryAdded",
    "FileChanged",
    "PreCommit",
];

/// Whether a timeout on `event` must be treated as a deny.
pub fn fails_closed(event: &str) -> bool {
    event == "PreToolUse"
}

/// Bytes of each hook stream the launchers keep (`HOOK_STREAM_CAPTURE_LIMIT` in `bus.rs` and in the
/// daemon's launcher, both 256 KiB). Stdout this long was cut, so it is not the hook's whole answer.
const HOOK_STDOUT_CAPTURE_LIMIT: usize = 256 * 1024;

/// Exit-0 stdout cut at the capture limit that opens a JSON object but does not parse, for example a
/// `{"decision":"deny",...}` truncated mid-document. A fail-closed event must not read it as "no
/// decision" (allow). Shorter stdout that does not parse is not truncated; [`parse_result_json`]
/// handles it (first JSON value, restrictive fields only).
fn is_truncated_json_object(stdout: &[u8]) -> bool {
    if stdout.len() < HOOK_STDOUT_CAPTURE_LIMIT {
        return false;
    }
    let text = String::from_utf8_lossy(stdout);
    let text = text.trim();
    text.starts_with('{') && serde_json::from_str::<Value>(text).is_err()
}

/// A permission decision expressed by a hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookDecision {
    Allow,
    Ask,
    Deny,
    /// `{"decision": "defer"}`: the hook does not decide now. The call does not run on this
    /// answer: an attended session asks the operator (as for `ask`), an unattended one denies it
    /// at once through the engine's unattended denial, and the model is told the call was
    /// deferred with the hook's reason. The chain reports it as an `ask` whose reason starts
    /// `deferred by <event> hook`.
    Defer,
}

/// Typed JSON result of a hook:
/// `{decision, reason, updatedInput, additionalContext, continue, stopReason}`.
#[derive(Debug, Clone, PartialEq)]
pub struct HookResult {
    pub decision: Option<HookDecision>,
    pub reason: Option<String>,
    pub updated_input: Option<Value>,
    pub additional_context: Option<String>,
    /// A `PostToolUse` hook that declares `"rewrite": true` may replace the tool's result text.
    pub updated_output: Option<String>,
    pub continue_: bool,
    pub stop_reason: Option<String>,
}

impl Default for HookResult {
    fn default() -> Self {
        Self {
            decision: None,
            reason: None,
            updated_input: None,
            additional_context: None,
            updated_output: None,
            continue_: true,
            stop_reason: None,
        }
    }
}

/// Interpretation of one hook process outcome.
#[derive(Debug, Clone, PartialEq)]
pub enum ParsedHook {
    Result(HookResult),
    /// Non-blocking failure (exit 1 or any other non-zero, non-2 code).
    Error(String),
    /// Killed by a signal: no exit evidence.
    Terminated(String),
}

fn first_non_empty<'a>(candidates: &[&'a str]) -> Option<&'a str> {
    candidates.iter().copied().find(|text| !text.is_empty())
}

/// Interpret exit code + stdout + stderr.
///
/// * `0` — success; stdout is parsed as a typed JSON result when it is a JSON
///   object; stdout that only starts with one keeps the deny/ask/stop of its
///   first JSON value; anything else is ignored.
/// * `2` — block; stderr (falling back to stdout) is the reason.
/// * other — non-blocking error.
pub fn parse_hook_output(
    code: Option<i32>,
    stdout: &str,
    stderr: &str,
    default_deny: &str,
    default_error: &str,
) -> ParsedHook {
    let out = stdout.trim();
    let err = stderr.trim();
    match code {
        None => ParsedHook::Terminated(match first_non_empty(&[err]) {
            Some(text) => format!("hook terminated by signal: {text}"),
            None => "hook terminated by signal".to_string(),
        }),
        Some(2) => ParsedHook::Result(HookResult {
            decision: Some(HookDecision::Deny),
            reason: Some(
                first_non_empty(&[err, out])
                    .unwrap_or(default_deny)
                    .to_string(),
            ),
            ..HookResult::default()
        }),
        Some(0) => ParsedHook::Result(parse_result_json(out)),
        Some(1) => ParsedHook::Error(
            first_non_empty(&[err, out])
                .unwrap_or(default_error)
                .to_string(),
        ),
        Some(other) => ParsedHook::Error(format!(
            "hook exited with unexpected code {other}: {}",
            first_non_empty(&[err, out]).unwrap_or("")
        )),
    }
}

fn parse_result_json(stdout: &str) -> HookResult {
    if !stdout.starts_with('{') {
        return HookResult::default();
    }
    match serde_json::from_str::<Value>(stdout) {
        Ok(Value::Object(map)) => result_from_map(&map),
        Ok(_) => HookResult::default(),
        Err(_) => restrictive_prefix_result(stdout),
    }
}

/// Stdout that is not exactly one JSON document (a JSON object followed by a stray log line, two
/// objects, prose that starts with `{`). The first complete JSON value is read, and only what can
/// stop the tool is honoured: a deny, an ask, an unrecognised decision (deny) or `continue: false`.
/// An allow, `updatedInput` and `additionalContext` from such output are ignored, so ambiguous
/// output never widens authority; with nothing restrictive in it, the result is "no decision", as
/// before.
fn restrictive_prefix_result(stdout: &str) -> HookResult {
    let Some(Ok(Value::Object(map))) = serde_json::Deserializer::from_str(stdout)
        .into_iter::<Value>()
        .next()
    else {
        return HookResult::default();
    };
    let parsed = result_from_map(&map);
    let mut result = HookResult::default();
    if matches!(
        parsed.decision,
        Some(HookDecision::Deny) | Some(HookDecision::Ask) | Some(HookDecision::Defer)
    ) {
        result.decision = parsed.decision;
        result.reason = parsed.reason.clone();
    }
    if !parsed.continue_ {
        result.continue_ = false;
        result.stop_reason = parsed.stop_reason;
        if result.reason.is_none() {
            result.reason = parsed.reason;
        }
    }
    result
}

fn result_from_map(map: &serde_json::Map<String, Value>) -> HookResult {
    let specific = map.get("hookSpecificOutput").and_then(Value::as_object);
    let lookup = |top: &str, nested: &str| -> Option<&Value> {
        map.get(top)
            .or_else(|| specific.and_then(|s| s.get(nested)))
    };
    let mut result = HookResult::default();

    // Versioned legacy transform envelope.
    if map.get("schemaVersion") == Some(&serde_json::json!(1))
        && map.get("action").and_then(Value::as_str) == Some("transform")
    {
        if let Some(arguments) = map.get("arguments") {
            result.updated_input = Some(arguments.clone());
        }
    }

    let decision = lookup("decision", "permissionDecision");
    if let Some(decision) = decision {
        result.decision = Some(
            match decision.as_str().map(str::to_ascii_lowercase).as_deref() {
                Some("allow") | Some("approve") => HookDecision::Allow,
                Some("ask") => HookDecision::Ask,
                Some("defer") => HookDecision::Defer,
                Some("deny") | Some("block") => HookDecision::Deny,
                // A gate that answers with something unintelligible must not
                // silently allow.
                _ => {
                    result.reason =
                        Some(format!("hook returned unrecognized decision: {decision}"));
                    HookDecision::Deny
                }
            },
        );
    }
    if let Some(reason) = lookup("reason", "permissionDecisionReason").and_then(Value::as_str) {
        result.reason = Some(reason.to_string());
    }
    if let Some(updated) = lookup("updatedInput", "updatedInput") {
        result.updated_input = Some(updated.clone());
    }
    if let Some(context) = lookup("additionalContext", "additionalContext").and_then(Value::as_str)
    {
        result.additional_context = Some(context.to_string());
    }
    if let Some(output) = lookup("updatedOutput", "updatedOutput").and_then(Value::as_str) {
        result.updated_output = Some(output.to_string());
    }
    if let Some(false) = map.get("continue").and_then(Value::as_bool) {
        result.continue_ = false;
    }
    if let Some(stop) = map.get("stopReason").and_then(Value::as_str) {
        result.stop_reason = Some(stop.to_string());
    }
    result
}

/// What a configured hook does when its event fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HookKind {
    /// A shell command run by the chain; it can allow, ask, deny and mutate.
    #[default]
    Command,
    /// A URL POSTed the event payload by the daemon (fire-and-forget).
    Url,
    /// A qualified MCP tool invoked with the event payload by the daemon (fire-and-forget).
    Mcp,
}

impl HookKind {
    /// The `type` spelling in the configuration.
    pub fn as_str(self) -> &'static str {
        match self {
            HookKind::Command => "command",
            HookKind::Url => "url",
            HookKind::Mcp => "mcp",
        }
    }
}

/// One configured hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookSpec {
    /// Tool-name matcher: absent, empty, or `*` match everything; otherwise a
    /// regex that must match the whole tool name (`Edit|Write`, `mcp__.*`).
    pub matcher: Option<String>,
    /// The command line for [`HookKind::Command`], the URL for [`HookKind::Url`], the qualified
    /// tool name for [`HookKind::Mcp`].
    pub command: String,
    pub timeout: Duration,
    pub kind: HookKind,
    /// A `PostToolUse` command hook that may rewrite the tool result (`"rewrite": true`). The
    /// engine awaits it before the result is stored; every other hook stays fire-and-forget.
    pub rewrite: bool,
    /// `"onFailure": "block"`: a failure of this hook (exit 1 or any other non-zero code that is
    /// not a decision, a start failure, a timeout, a signal kill, an unreadable answer) denies
    /// the event instead of being a non-blocking error. Without it, only a fail-closed event
    /// (`PreToolUse`) denies on those failures.
    pub block_on_failure: bool,
}

impl HookSpec {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            matcher: None,
            command: command.into(),
            timeout: DEFAULT_HOOK_TIMEOUT,
            kind: HookKind::Command,
            rewrite: false,
            block_on_failure: false,
        }
    }

    pub fn with_block_on_failure(mut self) -> Self {
        self.block_on_failure = true;
        self
    }

    pub fn with_rewrite(mut self) -> Self {
        self.rewrite = true;
        self
    }

    pub fn with_kind(mut self, kind: HookKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn with_matcher(mut self, matcher: impl Into<String>) -> Self {
        self.matcher = Some(matcher.into());
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Whether this hook applies to `target` (a tool name, when the event has
    /// one). Events without a target ignore the matcher. An invalid regex
    /// matches (fails closed: the hook runs rather than being skipped).
    pub fn matches(&self, target: Option<&str>) -> bool {
        let Some(matcher) = self.matcher.as_deref().map(str::trim) else {
            return true;
        };
        if matcher.is_empty() || matcher == "*" {
            return true;
        }
        let Some(target) = target else {
            return true;
        };
        match regex::Regex::new(&format!("^(?:{matcher})$")) {
            Ok(re) => re.is_match(target),
            Err(_) => true,
        }
    }
}

fn spec_from_entry(entry: &Value, inherited_matcher: Option<&str>) -> Result<HookSpec, String> {
    let command = entry
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .ok_or_else(|| "hook entry is missing a non-empty \"command\"".to_string())?;
    let matcher = entry
        .get("matcher")
        .and_then(Value::as_str)
        .or(inherited_matcher)
        .map(str::to_string);
    let timeout = match entry.get("timeoutSecs").or_else(|| entry.get("timeout")) {
        None => DEFAULT_HOOK_TIMEOUT,
        Some(value) => {
            let secs = value
                .as_u64()
                .ok_or_else(|| "hook timeout must be a positive integer".to_string())?;
            Duration::from_secs(secs.clamp(1, MAX_HOOK_TIMEOUT_SECS))
        }
    };
    let kind = match entry.get("type").and_then(Value::as_str) {
        None | Some("command") => HookKind::Command,
        Some("url") => HookKind::Url,
        Some("mcp") => HookKind::Mcp,
        Some(other) => return Err(format!("unknown hook type `{other}` (command, url or mcp)")),
    };
    let rewrite = entry
        .get("rewrite")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if rewrite && kind != HookKind::Command {
        return Err("only a command hook can set \"rewrite\"".to_string());
    }
    let block_on_failure = match entry.get("onFailure") {
        None => false,
        Some(value) => match value.as_str() {
            Some("block") => true,
            Some("continue") => false,
            _ => {
                return Err("hook \"onFailure\" must be \"block\" or \"continue\"".to_string());
            }
        },
    };
    if block_on_failure && kind != HookKind::Command {
        return Err("only a command hook can set \"onFailure\"".to_string());
    }
    Ok(HookSpec {
        matcher,
        command: command.to_string(),
        timeout,
        kind,
        rewrite,
        block_on_failure,
    })
}

/// Parse the hooks for `event` from a configuration JSON document:
///
/// ```json
/// {"PreToolUse":[{"matcher":"shell|write_file","command":"audit.sh","timeoutSecs":10}]}
/// ```
///
/// The nested Claude Code shape (`{"matcher":..,"hooks":[{"command":..}]}`) is
/// accepted too. Order of entries is execution order. An event name outside the taxonomy is an
/// error, so a typo never silently disables a hook.
pub fn parse_hook_specs_json(json: &str, event: &str) -> Result<Vec<HookSpec>, String> {
    let root: Value =
        serde_json::from_str(json).map_err(|error| format!("invalid hooks JSON: {error}"))?;
    parse_hook_specs_value(&root, event)
}

/// [`parse_hook_specs_json`] for an already parsed document (the `[hooks]` config table is
/// converted to a JSON value by the config loader).
pub fn parse_hook_specs_value(root: &Value, event: &str) -> Result<Vec<HookSpec>, String> {
    let Some(root) = root.as_object() else {
        return Err("hooks JSON must be an object keyed by event name".into());
    };
    if let Some(unknown) = root
        .keys()
        .find(|name| !HOOK_EVENT_NAMES.contains(&name.as_str()))
    {
        return Err(format!(
            "unknown hook event `{unknown}` (known: {})",
            HOOK_EVENT_NAMES.join(", ")
        ));
    }
    let Some(entries) = root.get(event) else {
        return Ok(Vec::new());
    };
    let Some(entries) = entries.as_array() else {
        return Err(format!("hooks for {event} must be an array"));
    };
    let mut specs = Vec::new();
    for entry in entries {
        if let Some(nested) = entry.get("hooks").and_then(Value::as_array) {
            let matcher = entry.get("matcher").and_then(Value::as_str);
            for inner in nested {
                specs.push(spec_from_entry(inner, matcher)?);
            }
        } else {
            specs.push(spec_from_entry(entry, None)?);
        }
    }
    Ok(specs)
}

/// Hooks from the JSON-form environment variable only (`RIGHTKIT_HOOKS_JSON`), in order. The
/// per-event command variables no longer exist (rule 58).
pub fn env_hook_specs(event: &str) -> Result<Vec<HookSpec>, String> {
    match crate::agent_hooks::hook_env(HOOKS_JSON_ENV) {
        Some(json) => parse_hook_specs_json(&json, event),
        None => Ok(Vec::new()),
    }
}

/// Read the project hook file of `workspace_root`. A workspace that is not `trusted` yields
/// `Ok(None)` without touching the file (project-scoped config takes no effect until trusted,
/// rule 13). A trusted workspace with an unreadable or invalid file yields an error: the bus then
/// fails closed for `PreToolUse` instead of dropping the hooks.
pub fn load_project_hook_file(
    workspace_root: &std::path::Path,
    trusted: bool,
) -> Result<Option<Value>, String> {
    if !trusted {
        return Ok(None);
    }
    let path = workspace_root.join(project_hook_file());
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    let value: Value = serde_json::from_str(&raw)
        .map_err(|error| format!("invalid hooks in {}: {error}", path.display()))?;
    // Validate every entry now so one bad entry is reported once, with the file name.
    for event in HOOK_EVENT_NAMES {
        parse_hook_specs_value(&value, event)
            .map_err(|error| format!("invalid hooks in {}: {error}", path.display()))?;
    }
    Ok(Some(value))
}

/// Hooks handed to a [`crate::agent_hooks::HookBus`] by the host from its two file-backed surfaces: the
/// `[hooks]` config key and the project hook file. Order of execution: config, project, then the
/// JSON environment variable.
#[derive(Debug, Clone, Default)]
pub struct ConfiguredHooks {
    config: Option<Value>,
    project: Option<Value>,
    /// First problem found while validating either surface; makes `PreToolUse` fail closed.
    error: Option<String>,
}

impl ConfiguredHooks {
    /// Validate and keep both surfaces. An invalid document is recorded as an error rather than
    /// dropped.
    pub fn new(config: Option<Value>, project: Option<Value>) -> Self {
        let mut error = None;
        for (label, doc) in [("[hooks]", &config), ("project hook file", &project)] {
            if let Some(doc) = doc {
                for event in HOOK_EVENT_NAMES {
                    if let Err(problem) = parse_hook_specs_value(doc, event) {
                        error.get_or_insert(format!("{label}: {problem}"));
                    }
                }
            }
        }
        Self {
            config,
            project,
            error,
        }
    }

    /// An error carried from a surface that could not be read at all (an unreadable or invalid
    /// project file).
    pub fn with_error(mut self, error: String) -> Self {
        self.error.get_or_insert(error);
        self
    }

    /// The validation or read error, if any.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Hooks for `event` from the config key then the project file.
    pub fn specs(&self, event: &str) -> Result<Vec<HookSpec>, String> {
        Ok(self
            .specs_labelled(event)?
            .into_iter()
            .map(|(_, spec)| spec)
            .collect())
    }

    /// Like [`specs`](Self::specs), each tagged with its origin (`config` or `project`).
    pub fn specs_labelled(&self, event: &str) -> Result<Vec<(&'static str, HookSpec)>, String> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        let mut specs = Vec::new();
        for (origin, doc) in [("config", &self.config), ("project", &self.project)] {
            if let Some(doc) = doc {
                specs.extend(
                    parse_hook_specs_value(doc, event)?
                        .into_iter()
                        .map(|spec| (origin, spec)),
                );
            }
        }
        Ok(specs)
    }
}

/// A hook command shortened for messages and stored events.
fn hook_label(command: &str) -> String {
    const MAX: usize = 120;
    let command = command.trim();
    if command.chars().count() <= MAX {
        command.to_string()
    } else {
        let head: String = command.chars().take(MAX).collect();
        format!("{head}...")
    }
}

/// Aggregated outcome of running every matching hook for one event.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChainReport {
    /// Combined decision; `None` means no hook expressed one.
    pub decision: Option<HookDecision>,
    /// Reason attached to the winning deny/ask.
    pub reason: Option<String>,
    /// Composed input after every mutation (only when some hook mutated it).
    pub updated_input: Option<Value>,
    /// `additionalContext` from each hook, in order.
    pub additional_context: Vec<String>,
    /// The last `updatedOutput` a hook returned (a `PostToolUse` rewrite).
    pub updated_output: Option<String>,
    /// Non-blocking failures.
    pub errors: Vec<String>,
    /// The subset of `errors` where a hook could not run at all (timeout or failure to start), as
    /// opposed to running and exiting with its own non-blocking verdict. Gates that audit (a
    /// `PreCommit` hook) block on these (rule 54).
    pub unrunnable: Vec<String>,
    /// Number of hooks that actually ran.
    pub ran: usize,
}

impl ChainReport {
    fn deny(mut self, reason: String) -> Self {
        self.decision = Some(HookDecision::Deny);
        self.reason = Some(reason);
        self.updated_input = None;
        self
    }

    pub fn is_denied(&self) -> bool {
        self.decision == Some(HookDecision::Deny)
    }
}

/// Largest tool-arguments JSON passed to a hook in `RIGHTKIT_HOOK_ARGS_JSON`. Linux `execve`
/// fails with E2BIG for any single string over `MAX_ARG_STRLEN` (128 KiB) and Windows limits the
/// whole environment block to 32 Ki characters; 120 KiB leaves room for the variable name.
const MAX_HOOK_ARGS_ENV_BYTES: usize = if cfg!(windows) { 32 * 1024 } else { 120 * 1024 };

/// Run `specs` in order for `event`.
///
/// `input` is the mutable payload (tool arguments for `PreToolUse`); it is
/// exposed to each hook as `RIGHTKIT_HOOK_ARGS_JSON` after earlier mutations.
#[allow(clippy::too_many_arguments)]
pub async fn run_chain(
    launcher: &Arc<dyn ProcessLaunchAdapter>,
    launch_context: &rightkit_sandbox::execution::ProcessLaunchContext,
    specs: &[HookSpec],
    event: &str,
    session_id: &str,
    target: Option<&str>,
    input: Option<&Value>,
    extra_envs: &[(&'static str, String)],
) -> ChainReport {
    let mut report = ChainReport::default();
    let mut current = input.cloned();
    let fail_closed_event = fails_closed(event);

    // Only command hooks run here; URL and MCP hooks are daemon sinks (fire-and-forget).
    for spec in specs
        .iter()
        .filter(|spec| spec.kind == HookKind::Command && spec.matches(target))
    {
        // This hook's failure denies: the event fails closed, or the hook asked for it.
        let closed = fail_closed_event || spec.block_on_failure;
        let closed_note = if fail_closed_event {
            "fail closed"
        } else {
            "onFailure: block"
        };
        let mut envs: Vec<(&'static str, String)> = vec![
            ("RIGHTKIT_HOOK_EVENT", event.to_string()),
            ("RIGHTKIT_HOOK_SESSION", session_id.to_string()),
        ];
        if let Some(tool) = target {
            envs.push(("RIGHTKIT_HOOK_TOOL", tool.to_string()));
        }
        if let Some(value) = &current {
            let args_json = value.to_string();
            // execve fails with E2BIG on an oversized environment; refuse up front with a cause.
            if args_json.len() > MAX_HOOK_ARGS_ENV_BYTES {
                let message = format!(
                    "{event} hook `{}` cannot inspect {}-byte tool arguments (limit {} bytes); pass large content another way",
                    hook_label(&spec.command),
                    args_json.len(),
                    MAX_HOOK_ARGS_ENV_BYTES
                );
                if closed {
                    return report.deny(format!("{message} ({closed_note})"));
                }
                report.unrunnable.push(message.clone());
                report.errors.push(message);
                continue;
            }
            envs.push(("RIGHTKIT_HOOK_ARGS_JSON", args_json));
        }
        envs.extend(extra_envs.iter().cloned());
        for (name, _) in envs.iter_mut() {
            *name = crate::agent_hooks::host_env_name(name);
        }

        report.ran += 1;
        let run = tokio::time::timeout(
            spec.timeout,
            launcher.run(spec.command.clone(), envs, launch_context.clone()),
        )
        .await;
        let parsed = match run {
            Err(_) => {
                let message = format!(
                    "{event} hook timed out after {}s",
                    spec.timeout.as_secs_f64()
                );
                if closed {
                    return report.deny(format!("{message} ({closed_note})"));
                }
                report.unrunnable.push(message.clone());
                report.errors.push(message);
                continue;
            }
            Ok(Err(error)) => {
                // Rule 54: a hook that cannot start is never a silent allow. The message names
                // the hook and the cause so the stored event is actionable.
                let message = format!(
                    "{event} hook `{}` failed to start: {error}",
                    hook_label(&spec.command)
                );
                if closed {
                    return report.deny(format!("{message} ({closed_note})"));
                }
                report.unrunnable.push(message.clone());
                report.errors.push(message);
                continue;
            }
            // The hook runs through a shell, so a missing or non-executable command still
            // "starts": the shell reports it as exit 127/126 (`sh`) or 9009 (`cmd`). For a
            // fail-closed event that is the same case as above (rule 54), not a hook error.
            Ok(Ok(output)) if closed && matches!(output.status.code(), Some(126 | 127 | 9009)) => {
                return report.deny(format!(
                    "{event} hook `{}` could not run (exit {}): {} ({closed_note})",
                    hook_label(&spec.command),
                    output.status.code().unwrap_or_default(),
                    hook_label(&String::from_utf8_lossy(&output.stderr))
                ));
            }
            Ok(Ok(output))
                if closed
                    && output.status.code() == Some(0)
                    && is_truncated_json_object(&output.stdout) =>
            {
                return report.deny(format!(
                    "{event} hook `{}` printed a JSON object cut at the {} byte capture limit ({closed_note})",
                    hook_label(&spec.command),
                    HOOK_STDOUT_CAPTURE_LIMIT
                ));
            }
            Ok(Ok(output)) => parse_hook_output(
                output.status.code(),
                &String::from_utf8_lossy(&output.stdout),
                &String::from_utf8_lossy(&output.stderr),
                &format!("Denied by {event} hook"),
                &format!("{event} hook exited with error (exit 1)"),
            ),
        };
        let result = match parsed {
            ParsedHook::Error(message) => {
                // Exit 1 or another non-zero code is a non-blocking error unless the hook said
                // `"onFailure": "block"`.
                if spec.block_on_failure {
                    return report.deny(format!("{message} (onFailure: block)"));
                }
                report.errors.push(message);
                continue;
            }
            ParsedHook::Terminated(message) => {
                if closed {
                    return report.deny(format!("{message} ({closed_note})"));
                }
                report.errors.push(message);
                continue;
            }
            ParsedHook::Result(result) => result,
        };

        if let Some(context) = result.additional_context {
            report.additional_context.push(context);
        }
        if let Some(output) = result.updated_output {
            report.updated_output = Some(output);
        }
        if let Some(updated) = result.updated_input {
            if current.is_some() {
                if updated.is_object() {
                    current = Some(updated);
                    report.updated_input = current.clone();
                } else {
                    return report.deny("hook updatedInput must be a JSON object".into());
                }
            }
        }
        if !result.continue_ {
            let reason = result
                .stop_reason
                .or(result.reason)
                .unwrap_or_else(|| format!("{event} hook requested stop"));
            return report.deny(reason);
        }
        match result.decision {
            Some(HookDecision::Deny) => {
                let reason = result
                    .reason
                    .unwrap_or_else(|| format!("Denied by {event} hook"));
                return report.deny(reason);
            }
            Some(HookDecision::Ask) => {
                if report.decision != Some(HookDecision::Ask) {
                    report.decision = Some(HookDecision::Ask);
                    report.reason = result.reason;
                }
            }
            Some(HookDecision::Defer) => {
                // Held like an `ask` (it outranks an allow), with the hook's own reason under a
                // "deferred" heading so the model and the operator see what happened.
                if report.decision != Some(HookDecision::Ask) {
                    report.decision = Some(HookDecision::Ask);
                    report.reason = Some(match result.reason {
                        Some(reason) => format!("deferred by {event} hook: {reason}"),
                        None => format!("deferred by {event} hook"),
                    });
                }
            }
            Some(HookDecision::Allow) if report.decision.is_none() => {
                report.decision = Some(HookDecision::Allow);
            }
            Some(HookDecision::Allow) | None => {}
        }
    }
    report
}
