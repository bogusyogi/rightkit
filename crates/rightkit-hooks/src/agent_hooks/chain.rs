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
use std::time::Duration;

/// Default per-hook timeout.
pub const DEFAULT_HOOK_TIMEOUT: Duration = Duration::from_secs(30);
/// Upper bound accepted from configuration.
pub const MAX_HOOK_TIMEOUT_SECS: u64 = 600;
/// Env var holding the multi-hook configuration JSON.
pub const HOOKS_JSON_ENV: &str = "RIGHTKIT_HOOKS_JSON";
/// Legacy single-command env var for `PreToolUse`; runs first in the chain.
pub const LEGACY_PRE_TOOL_USE_ENV: &str = "RIGHTKIT_HOOK_PRE_TOOL_USE";

/// Every event name in the supported taxonomy.
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
];

/// Whether a timeout on `event` must be treated as a deny.
pub fn fails_closed(event: &str) -> bool {
    event == "PreToolUse"
}

/// A permission decision expressed by a hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookDecision {
    Allow,
    Ask,
    Deny,
}

/// Typed JSON result of a hook:
/// `{decision, reason, updatedInput, additionalContext, continue, stopReason}`.
#[derive(Debug, Clone, PartialEq)]
pub struct HookResult {
    pub decision: Option<HookDecision>,
    pub reason: Option<String>,
    pub updated_input: Option<Value>,
    pub additional_context: Option<String>,
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
///   object, otherwise ignored.
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
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(stdout) else {
        return HookResult::default();
    };
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
    if let Some(false) = map.get("continue").and_then(Value::as_bool) {
        result.continue_ = false;
    }
    if let Some(stop) = map.get("stopReason").and_then(Value::as_str) {
        result.stop_reason = Some(stop.to_string());
    }
    result
}

/// One configured hook command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookSpec {
    /// Tool-name matcher: absent, empty, or `*` match everything; otherwise a
    /// regex that must match the whole tool name (`Edit|Write`, `mcp__.*`).
    pub matcher: Option<String>,
    pub command: String,
    pub timeout: Duration,
}

impl HookSpec {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            matcher: None,
            command: command.into(),
            timeout: DEFAULT_HOOK_TIMEOUT,
        }
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
    Ok(HookSpec {
        matcher,
        command: command.to_string(),
        timeout,
    })
}

/// Parse the hooks for `event` from a configuration JSON document:
///
/// ```json
/// {"PreToolUse":[{"matcher":"shell|write_file","command":"audit.sh","timeoutSecs":10}]}
/// ```
///
/// The nested Claude Code shape (`{"matcher":..,"hooks":[{"command":..}]}`) is
/// accepted too. Order of entries is execution order.
pub fn parse_hook_specs_json(json: &str, event: &str) -> Result<Vec<HookSpec>, String> {
    let root: Value =
        serde_json::from_str(json).map_err(|error| format!("invalid hooks JSON: {error}"))?;
    let Some(root) = root.as_object() else {
        return Err("hooks JSON must be an object keyed by event name".into());
    };
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

/// Load the configured hooks for `event`: the legacy single-command env var
/// (PreToolUse only) first, then `RIGHTKIT_HOOKS_JSON` entries in order.
pub fn load_hook_specs(event: &str) -> Result<Vec<HookSpec>, String> {
    let mut specs = Vec::new();
    if event == "PreToolUse" {
        if let Some(command) = crate::agent_hooks::hook_env(LEGACY_PRE_TOOL_USE_ENV) {
            specs.push(HookSpec::new(command));
        }
    }
    if let Some(json) = crate::agent_hooks::hook_env(HOOKS_JSON_ENV) {
        specs.extend(parse_hook_specs_json(&json, event)?);
    }
    Ok(specs)
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
    /// Non-blocking failures.
    pub errors: Vec<String>,
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
    let closed = fails_closed(event);

    for spec in specs.iter().filter(|spec| spec.matches(target)) {
        let mut envs: Vec<(&'static str, String)> = vec![
            ("RIGHTKIT_HOOK_EVENT", event.to_string()),
            ("RIGHTKIT_HOOK_SESSION", session_id.to_string()),
        ];
        if let Some(tool) = target {
            envs.push(("RIGHTKIT_HOOK_TOOL", tool.to_string()));
        }
        if let Some(value) = &current {
            envs.push(("RIGHTKIT_HOOK_ARGS_JSON", value.to_string()));
        }
        envs.extend(extra_envs.iter().cloned());

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
                    return report.deny(format!("{message} (fail closed)"));
                }
                report.errors.push(message);
                continue;
            }
            Ok(Err(error)) => {
                report.errors.push(format!("hook failed to spawn: {error}"));
                continue;
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
                report.errors.push(message);
                continue;
            }
            ParsedHook::Terminated(message) => {
                if closed {
                    return report.deny(format!("{message} (fail closed)"));
                }
                report.errors.push(message);
                continue;
            }
            ParsedHook::Result(result) => result,
        };

        if let Some(context) = result.additional_context {
            report.additional_context.push(context);
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
            Some(HookDecision::Allow) => {
                if report.decision.is_none() {
                    report.decision = Some(HookDecision::Allow);
                }
            }
            None => {}
        }
    }
    report
}
