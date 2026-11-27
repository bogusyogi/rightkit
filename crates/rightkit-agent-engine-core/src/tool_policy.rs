//! Tool-call policy helpers used by the session loop.
//!
//! `SessionEngine` owns permission flow and event emission; this module owns the
//! small pure helpers for extracting tool arguments, shell guardrails, and rule
//! targets.

use rightkit_agent_engine_protocol::ToolCall;
use rightkit_agent_policy::{PermissionVerdict, ToolRuleTarget};

pub(crate) fn extract_path_arg(arguments_json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(arguments_json).ok()?;
    v.get("path")
        .and_then(|p| p.as_str())
        .map(|s| s.to_string())
}

pub(crate) fn extract_command_arg(arguments_json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(arguments_json).ok()?;
    v.get("command")
        .or_else(|| v.get("cmd"))
        .and_then(|p| p.as_str())
        .map(|s| s.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ToolGuardrailAction {
    Deny,
    RequireApproval,
}

#[derive(Debug, Clone)]
pub(crate) struct ToolGuardrailDecision {
    pub(crate) policy_id: &'static str,
    pub(crate) action: ToolGuardrailAction,
    pub(crate) reason: String,
}

pub(crate) fn pre_tool_approval_reason(
    verdict: &PermissionVerdict,
    guardrail: Option<&ToolGuardrailDecision>,
) -> String {
    if let Some(guardrail) = guardrail {
        format!("{}: {}", guardrail.policy_id, guardrail.reason)
    } else {
        format!("{}: {}", verdict.policy_source, verdict.reason)
    }
}

pub(crate) fn evaluate_tool_guardrails(call: &ToolCall) -> Option<ToolGuardrailDecision> {
    if !matches!(call.name.as_str(), "shell" | "powershell") {
        return None;
    }

    let command = extract_command_arg(&call.arguments_json)?;
    shell_guardrail_decision(&command)
}

pub(crate) fn shell_guardrail_decision(command: &str) -> Option<ToolGuardrailDecision> {
    let normalized = normalize_shell_command(command);

    if is_fork_bomb(&normalized) {
        return Some(guardrail_deny(
            "guardrail.shell.fork_bomb",
            "fork bomb pattern detected",
        ));
    }
    if is_chmod_777(&normalized) {
        return Some(guardrail_deny(
            "guardrail.shell.chmod_777",
            "world-writable chmod 777 detected",
        ));
    }
    if is_pipe_to_shell(&normalized) {
        return Some(guardrail_ask(
            "guardrail.shell.pipe_to_shell",
            "download or stream piped into an interpreter",
        ));
    }
    if is_base64_decode_exec(&normalized) {
        return Some(guardrail_ask(
            "guardrail.shell.base64_decode_exec",
            "base64 decode chained into execution",
        ));
    }
    if normalized.contains("ld_preload=") {
        return Some(guardrail_ask(
            "guardrail.shell.ld_preload",
            "LD_PRELOAD environment injection detected",
        ));
    }
    if contains_shell_token(&normalized, "sudo") {
        return Some(guardrail_ask(
            "guardrail.shell.sudo",
            "sudo elevation detected",
        ));
    }
    if is_package_install(&normalized) {
        return Some(guardrail_ask(
            "guardrail.shell.package_install",
            "package installation command detected",
        ));
    }

    None
}

fn guardrail_deny(policy_id: &'static str, reason: impl Into<String>) -> ToolGuardrailDecision {
    ToolGuardrailDecision {
        policy_id,
        action: ToolGuardrailAction::Deny,
        reason: reason.into(),
    }
}

fn guardrail_ask(policy_id: &'static str, reason: impl Into<String>) -> ToolGuardrailDecision {
    ToolGuardrailDecision {
        policy_id,
        action: ToolGuardrailAction::RequireApproval,
        reason: reason.into(),
    }
}

fn normalize_shell_command(command: &str) -> String {
    let mut normalized = command
        .to_ascii_lowercase()
        .replace(['\r', '\n', '\t'], " ");
    while normalized.contains("  ") {
        normalized = normalized.replace("  ", " ");
    }
    normalized
}

fn is_pipe_to_shell(command: &str) -> bool {
    let pipes_to_interpreter = [
        "| sh",
        "| bash",
        "| zsh",
        "| dash",
        "| pwsh",
        "| powershell",
        "| iex",
        "| invoke-expression",
    ]
    .iter()
    .any(|needle| command.contains(needle));
    let downloads = [
        "curl",
        "wget",
        "irm",
        "iwr",
        "invoke-webrequest",
        "invoke-restmethod",
    ]
    .iter()
    .any(|needle| contains_shell_token(command, needle));

    pipes_to_interpreter && downloads
}

fn is_base64_decode_exec(command: &str) -> bool {
    let decodes = [
        "base64 -d",
        "base64 --decode",
        "frombase64string",
        "certutil -decode",
    ]
    .iter()
    .any(|needle| command.contains(needle));
    let executes = [
        "| sh",
        "| bash",
        "| pwsh",
        "| powershell",
        "| iex",
        "invoke-expression",
        "bash -c",
        "sh -c",
    ]
    .iter()
    .any(|needle| command.contains(needle));

    decodes && executes
}

fn is_fork_bomb(command: &str) -> bool {
    let compact = command.replace(' ', "");
    compact.contains(":(){:|:&};:") || compact.contains("function(){function|function&};function")
}

fn is_chmod_777(command: &str) -> bool {
    command.contains("chmod 777") || command.contains("chmod -r 777")
}

fn is_package_install(command: &str) -> bool {
    [
        "apt install",
        "apt-get install",
        "dnf install",
        "yum install",
        "apk add",
        "brew install",
        "choco install",
        "winget install",
        "scoop install",
        "npm install",
        "pnpm add",
        "yarn add",
        "pip install",
        "pip3 install",
        "cargo install",
    ]
    .iter()
    .any(|needle| command.contains(needle))
}

fn contains_shell_token(command: &str, token: &str) -> bool {
    command
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'))
        .any(|part| part == token)
}

pub(crate) fn rule_target_for_call(call: &ToolCall) -> ToolRuleTarget {
    let mut target = ToolRuleTarget::new(&call.name);
    if let Some(command) = extract_command_arg(&call.arguments_json) {
        target = target.with_command(command);
    }
    if let Some(path) = extract_path_arg(&call.arguments_json) {
        target = target.with_path(path);
    }
    target
}

/// The qualified `mcp__server__tool` name an `mcp_call` invokes, so hooks and rules can name
/// one MCP tool instead of the generic wrapper.
pub(crate) fn mcp_qualified_name(call: &ToolCall) -> Option<String> {
    if call.name != "mcp_call" {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&call.arguments_json).ok()?;
    v.get("name")
        .and_then(|n| n.as_str())
        .filter(|n| !n.is_empty())
        .map(|n| n.to_string())
}

/// The tool name hooks see: the qualified MCP name for `mcp_call`, else the call name.
pub(crate) fn hook_tool_name(call: &ToolCall) -> String {
    mcp_qualified_name(call).unwrap_or_else(|| call.name.clone())
}

/// Rule match for a call. An `mcp_call` is checked under its qualified name and under
/// `mcp_call`; a deny from either wins, so rules written for `mcp_call` keep working.
pub(crate) fn effective_rule_match_for_call(
    rules: &rightkit_agent_policy::RuleStore,
    call: &ToolCall,
) -> Option<rightkit_agent_policy::Rule> {
    let generic = rule_target_for_call(call);
    let Some(qualified_name) = mcp_qualified_name(call) else {
        return rules.effective_match_for(&generic);
    };
    let mut qualified = generic.clone();
    qualified.tool = qualified_name;
    let first = rules.effective_match_for(&qualified);
    let second = rules.effective_match_for(&generic);
    match (first, second) {
        (Some(a), _) if a.effect == rightkit_agent_policy::PermissionDecision::Deny => Some(a),
        (_, Some(b)) if b.effect == rightkit_agent_policy::PermissionDecision::Deny => Some(b),
        (Some(a), _) => Some(a),
        (None, b) => b,
    }
}

/// Tell configured `PermissionDenied` hooks that a call was refused (by policy, a rule or the
/// person at the card). They only observe; the denial stands.
pub(crate) async fn run_permission_denied_hooks(
    bus: Option<rightkit_hooks::agent_hooks::HookBus>,
    session_id: &str,
    call: &ToolCall,
    reason: &str,
) {
    let Some(bus) = bus else {
        return;
    };
    bus.run_event_hooks(
        "PermissionDenied",
        session_id,
        Some(&hook_tool_name(call)),
        None,
        &[
            ("RIGHTKIT_HOOK_REASON", reason.to_string()),
            ("RIGHTKIT_HOOK_CALL_ID", call.id.clone()),
        ],
    )
    .await;
}
