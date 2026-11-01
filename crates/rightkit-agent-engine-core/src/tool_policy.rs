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
    #[allow(dead_code)]
    MutateArgs,
}

#[derive(Debug, Clone)]
pub(crate) struct ToolGuardrailDecision {
    pub(crate) policy_id: &'static str,
    pub(crate) action: ToolGuardrailAction,
    pub(crate) reason: String,
    pub(crate) mutated_arguments_json: Option<String>,
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
        mutated_arguments_json: None,
    }
}

fn guardrail_ask(policy_id: &'static str, reason: impl Into<String>) -> ToolGuardrailDecision {
    ToolGuardrailDecision {
        policy_id,
        action: ToolGuardrailAction::RequireApproval,
        reason: reason.into(),
        mutated_arguments_json: None,
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
