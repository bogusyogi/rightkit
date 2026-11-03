fn cli_json_line(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string())
}

fn emit_cli_json_line(value: &serde_json::Value) {
    println!("{}", cli_json_line(value));
    let _ = std::io::stdout().flush();
}

fn cli_error_exit(stream_json: bool, code: CliExitCode, message: &str) -> i32 {
    if stream_json {
        emit_cli_json_line(&cli_error_envelope(code, message));
    }
    code.as_i32()
}

fn cli_error_envelope(code: CliExitCode, message: &str) -> serde_json::Value {
    serde_json::json!({
        "schema_version": "coderight.cli.error.v1",
        "event": "error",
        "error": {
            "class": code.class(),
            "exit_code": code.as_i32(),
            "message": message,
        },
    })
}

fn cli_exit_code_from_events(
    events: &[rightkit_agent_engine_protocol::ProtocolEvent],
) -> Option<CliExitCode> {
    events.iter().find_map(cli_exit_code_from_event)
}

fn cli_exit_code_from_event(
    event: &rightkit_agent_engine_protocol::ProtocolEvent,
) -> Option<CliExitCode> {
    use rightkit_agent_engine_protocol::ProtocolEvent::*;
    match event {
        RunFailed { kind, error, .. } => Some(cli_exit_code_from_failure_kind(kind, error)),
        BudgetExceeded { .. } => Some(CliExitCode::Budget),
        ToolApprovalResolved {
            approved: false, ..
        } => Some(CliExitCode::Permission),
        RunInterrupted { reason, .. } => reason
            .as_deref()
            .map(cli_exit_code_from_error)
            .filter(|code| *code != CliExitCode::Generic),
        _ => None,
    }
}

fn cli_exit_code_from_failure_kind(kind: &str, error: &str) -> CliExitCode {
    match kind {
        "auth" => CliExitCode::Auth,
        "budget" => CliExitCode::Budget,
        "permission" => CliExitCode::Permission,
        "sandbox" => CliExitCode::Sandbox,
        "usage" | "max_iterations" => CliExitCode::Usage,
        _ => cli_exit_code_from_error(error),
    }
}

fn cli_exit_code_from_error(error: &str) -> CliExitCode {
    let error = error.to_ascii_lowercase();
    if error.contains("api key") || error.contains("unauthorized") || error.contains("auth") {
        CliExitCode::Auth
    } else if error.contains("permission")
        || error.contains("approval")
        || error.contains("denied")
        || error.contains("forbidden")
    {
        CliExitCode::Permission
    } else if error.contains("budget") || error.contains("ceiling") || error.contains("spend") {
        CliExitCode::Budget
    } else if error.contains("sandbox")
        || error.contains("landlock")
        || error.contains("seccomp")
        || error.contains("restricted")
    {
        CliExitCode::Sandbox
    } else {
        CliExitCode::Generic
    }
}

fn render_cli_stream_event(
    ev: &rightkit_agent_engine_protocol::ProtocolEvent,
) -> serde_json::Value {
    let data = serde_json::to_value(ev).unwrap_or_else(|_| serde_json::json!({}));
    let error = cli_exit_code_from_event(ev).map(|code| {
        let message = match ev {
            rightkit_agent_engine_protocol::ProtocolEvent::RunFailed { error, .. } => {
                error.as_str()
            }
            rightkit_agent_engine_protocol::ProtocolEvent::RunInterrupted { reason, .. } => {
                reason.as_deref().unwrap_or("run interrupted")
            }
            rightkit_agent_engine_protocol::ProtocolEvent::BudgetExceeded { hint, .. } => {
                hint.as_str()
            }
            rightkit_agent_engine_protocol::ProtocolEvent::ToolApprovalResolved { .. } => {
                "tool approval denied"
            }
            _ => "",
        };
        serde_json::json!({
            "class": code.class(),
            "exit_code": code.as_i32(),
            "message": message,
        })
    });
    let mut value = serde_json::json!({
        "schema_version": "coderight.cli.stream_event.v1",
        "event": data.get("$kind").and_then(|value| value.as_str()).unwrap_or("unknown"),
        "seq": data.get("seq").and_then(|value| value.as_i64()).unwrap_or_else(|| event_seq(ev)),
        "session_id": data.get("sessionId").and_then(|value| value.as_str()).unwrap_or(""),
        "ts": data.get("createdAt").and_then(|value| value.as_str()).unwrap_or(""),
        "data": data,
    });
    if let Some(error) = error {
        value["error"] = error;
    }
    value
}

fn cli_stream_event_data(event: &serde_json::Value) -> &serde_json::Value {
    event.get("data").unwrap_or(event)
}

fn event_seq(ev: &rightkit_agent_engine_protocol::ProtocolEvent) -> i64 {
    use rightkit_agent_engine_protocol::ProtocolEvent::*;
    match ev {
        SessionStarted { seq, .. }
        | SessionTitleUpdated { seq, .. }
        | AssistantDelta { seq, .. }
        | AssistantMessageCompleted { seq, .. }
        | ToolCallStarted { seq, .. }
        | ToolApprovalRequested { seq, .. }
        | ToolApprovalResolved { seq, .. }
        | ToolResultAdded { seq, .. }
        | SessionTurnFinished { seq, .. }
        | StreamResyncRequired { seq, .. }
        | Overloaded { seq, .. }
        | PersistenceFailed { seq, .. }
        | ProviderRetrying { seq, .. }
        | SessionCompactionStarted { seq, .. }
        | VerificationFinished { seq, .. }
        | TaskGraphUpdated { seq, .. }
        | TaskStatusUpdated { seq, .. }
        | TaskArtifactUpdated { seq, .. }
        | ArtifactLineageUpdated { seq, .. }
        | RunInterrupted { seq, .. }
        | RunFailed { seq, .. }
        | SessionCompacted { seq, .. }
        | SessionContextFolded { seq, .. }
        | UsageUpdated { seq, .. }
        | BudgetExceeded { seq, .. }
        | ProviderRouteUpdated { seq, .. }
        | AccountRateLimited { seq, .. }
        | AccountUsageReported { seq, .. }
        | DispatchDecided { seq, .. }
        | OrchestrationObservationRecorded { seq, .. }
        | AgentQueryIssued { seq, .. }
        | AgentQueryResponded { seq, .. }
        | HumanSteeringInjected { seq, .. }
        | SubAgentSpawned { seq, .. }
        | SubAgentCompleted { seq, .. }
        | SubAgentCancelled { seq, .. }
        | UserMessageSubmitted { seq, .. }
        | UserQuestionAsked { seq, .. }
        | McpElicitationRequested { seq, .. }
        | CredentialRequested { seq, .. }
        | CredentialResolved { seq, .. }
        | UserQuestionAnswered { seq, .. }
        | PlanPresented { seq, .. }
        | AgentRosterUpdated { seq, .. }
        | CheckRunCompleted { seq, .. }
        | GoalProgressUpdated { seq, .. }
        | SelfReviewProposalReceived { seq, .. }
        | ToolApprovalReviewed { seq, .. }
        | RunGovernanceRejected { seq, .. }
        | TurnUsage { seq, .. }
        | PromptCacheBreak { seq, .. }
        | ToolOutputDelta { seq, .. }
        | BackgroundShellExited { seq, .. }
        | ModelRouteDecision { seq, .. } => *seq,
    }
}

/// `--color` resolution: `always`/`never` are absolute; `auto` colors only
/// when the destination stream is a terminal and `NO_COLOR` is unset.
pub(crate) fn resolve_color_enabled(
    color: crate::cli::CliColor,
    stream_is_terminal: bool,
    no_color_env: bool,
) -> bool {
    match color {
        crate::cli::CliColor::Always => true,
        crate::cli::CliColor::Never => false,
        crate::cli::CliColor::Auto => stream_is_terminal && !no_color_env,
    }
}

fn print_event(ev: &rightkit_agent_engine_protocol::ProtocolEvent, color: crate::cli::CliColor) {
    use std::io::IsTerminal as _;
    // The ANSI sequences below are written to stderr, so auto mode keys on
    // stderr being a terminal (Codex exec: --color auto) and honors NO_COLOR.
    let color_enabled = resolve_color_enabled(
        color,
        std::io::stderr().is_terminal(),
        std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()),
    );
    use rightkit_agent_engine_protocol::ProtocolEvent::*;
    match ev {
        AssistantDelta {
            text, is_reasoning, ..
        } if !is_reasoning => {
            print!("{text}");
            let _ = std::io::stdout().flush();
        }
        AssistantMessageCompleted { .. } => println!(),
        ToolCallStarted { call, .. } if color_enabled => {
            eprintln!("\n\x1b[36m[tool]\x1b[0m {}", call.name)
        }
        ToolCallStarted { call, .. } => eprintln!("\n[tool] {}", call.name),
        ToolApprovalRequested {
            call, risk_label, ..
        } => {
            eprintln!("[approval needed] {} (risk: {risk_label})", call.name);
        }
        ToolApprovalResolved {
            tool_call_id,
            approved,
            ..
        } if !approved => {
            eprintln!("[denied] {tool_call_id}");
        }
        RunFailed { error, .. } if color_enabled => {
            eprintln!("\n\x1b[31m[run failed]\x1b[0m {error}")
        }
        RunFailed { error, .. } => eprintln!("\n[run failed] {error}"),
        RunInterrupted { reason, .. } => {
            eprintln!("\n[interrupted] {}", reason.as_deref().unwrap_or(""));
        }
        BudgetExceeded {
            current_usd,
            ceiling_usd,
            ..
        } => {
            eprintln!("[budget] ${current_usd:.4} exceeded ceiling ${ceiling_usd:.4}");
        }
        SelfReviewProposalReceived { proposal, .. } => {
            eprintln!(
                "[self-review] proposal {}: {}",
                proposal.id, proposal.summary
            );
        }
        ModelRouteDecision { decision, .. } => {
            eprintln!(
                "[router] {} -> {} (tier={}, est. ${:.4}): {}",
                decision.agent_role,
                decision.model_id,
                decision.tier,
                decision.est_cost_usd,
                decision.reason
            );
        }
        _ => {}
    }
}

async fn cli_diff(workspace: Option<&str>, staged: bool) -> i32 {
    use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
    use rightkit_agent_tools::coding_tools::Tool;

    let workspace_path = workspace
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
    let ctx = rightkit_agent_tools::coding_tools::ToolContext::new(
        "cli-diff",
        &workspace_path,
        PermissionMode::Bypass,
        WorkspaceTrust::Trusted,
    );
    let repo_path = workspace_path.display().to_string();

    let status = rightkit_agent_tools::coding_tools::git_read::GitReadTool
        .execute(serde_json::json!({ "repo_path": repo_path.clone() }), &ctx)
        .await;
    if status.is_error {
        eprintln!("{}", status.content);
        return 1;
    }

    let diff = rightkit_agent_tools::coding_tools::git_diff::GitDiffTool
        .execute(
            serde_json::json!({
                "repo_path": repo_path,
                "staged": staged
            }),
            &ctx,
        )
        .await;
    if diff.is_error {
        eprintln!("{}", diff.content);
        return 1;
    }

    let scope = if staged { "staged" } else { "unstaged" };
    println!(
        "{}",
        render_cli_diff_output(&workspace_path, scope, &status.content, &diff.content)
    );
    0
}

fn render_cli_diff_output(
    workspace: &std::path::Path,
    scope: &str,
    status: &str,
    diff: &str,
) -> String {
    let mut output = format!(
        "**Diff:** {}\nScope: {scope}\n\n```text\n{}```\n\n",
        workspace.display(),
        status.trim_end()
    );
    if diff.trim() == "(no changes)" {
        output.push_str("No changes in selected scope.");
    } else {
        output.push_str("```diff\n");
        output.push_str(diff.trim_end());
        output.push_str("\n```");
    }
    output
}

fn build_cli_review_goal(focus: &[String], base: Option<&str>, dirs: &[String]) -> String {
    let mut prompt = "Run a read-only auditor pass for the current workspace changes. Create a task categorized as `code-review` so RightKit routes this to `auditor`, not the plan-review jury/council. Review git status, git diff, and relevant files as evidence. Findings should lead, ordered by severity, with concrete file/line references wherever possible. Keep summaries brief. If there are no actionable issues, say that clearly and mention any remaining test gaps or residual risk.".to_string();
    if let Some(base) = base.map(str::trim).filter(|value| !value.is_empty()) {
        prompt.push_str("\n\nReview scope base ref: ");
        prompt.push_str(&json_string(base));
        prompt.push_str(". Compare current changes against this ref when collecting git evidence.");
    }
    let dirs = dirs
        .iter()
        .map(|dir| dir.trim())
        .filter(|dir| !dir.is_empty())
        .collect::<Vec<_>>();
    if !dirs.is_empty() {
        prompt.push_str("\n\nReview path scopes: ");
        prompt.push_str(&serde_json::to_string(&dirs).unwrap_or_else(|_| "[]".to_string()));
        prompt.push_str(". Limit findings to these paths unless a cross-cutting issue is necessary to explain the risk.");
    }
    let focus = focus.join(" ");
    if !focus.trim().is_empty() {
        prompt.push_str("\n\nAdditional focus: ");
        prompt.push_str(focus.trim());
    }
    prompt
}

fn is_review_findings_request(focus: &[String]) -> bool {
    focus
        .first()
        .map(|word| word == "findings")
        .unwrap_or(false)
}

/// Explicitly-passed options the auditor dispatch cannot honor. The flags
/// survive from the retired single-agent review; leaving them silently
/// discarded made `coderight review --max-budget-usd 1` look accepted while
/// having no effect. Reject them before a session is created instead.
fn review_rejected_options(
    provider: Option<&str>,
    stream_json: bool,
    max_turns: Option<u32>,
    max_budget_usd: Option<f64>,
) -> Vec<&'static str> {
    let mut rejected = Vec::new();
    if provider.is_some() {
        rejected.push("--provider");
    }
    if stream_json {
        rejected.push("--stream-json");
    }
    if max_turns.is_some() {
        rejected.push("--max-turns");
    }
    if max_budget_usd.is_some() {
        rejected.push("--max-budget-usd");
    }
    rejected
}

fn review_findings_session_arg(focus: &[String], explicit: Option<&str>) -> Result<String, String> {
    explicit
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            focus
                .get(1)
                .map(String::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .ok_or_else(|| "Usage: coderight review findings <session-id> [--json]".to_string())
}

fn is_auditor_transcript_artifact(artifact: &serde_json::Value) -> bool {
    let kind = artifact.get("kind").and_then(|value| value.as_str());
    let producer = artifact
        .get("producer")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    kind == Some("subagent_transcript") && producer.contains("auditor")
}

fn final_assistant_content(messages: &serde_json::Value) -> Option<String> {
    messages.as_array()?.iter().rev().find_map(|message| {
        (message.get("$kind").and_then(|value| value.as_str()) == Some("assistant"))
            .then(|| message.get("content").and_then(|value| value.as_str()))
            .flatten()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

// ── S:H13: hidden-secret entry (no argv secrets) ────────────────────────────
//
// Provider connect/rotate must never receive the API key as a command-line
// argument: argv is world-readable through process listing and persists in
// shell history. The secret enters through stdin instead — piped from a
// secret manager (preferred) or typed at a prompt.

/// Upper bound for a CLI-entered secret; matches the daemon's own
/// `SaveProviderKey` bound so both sides reject the same input.
const CLI_SECRET_MAX_LEN: usize = 16_384;

/// Read one secret from stdin without echoing or logging it.
///
/// Piped input (`printf '%s' "$KEY" | coderight provider connect openrouter`)
/// is the preferred path and needs no prompt. On a TTY a prompt is printed to
/// stderr instead of stdout so stdout stays machine-readable, with an honest
/// note that typed input is visible locally; piping keeps it out of view.
/// Trailing whitespace is trimmed to match the daemon-side handler.
pub(crate) fn read_secret_line(label: &str) -> Result<String, String> {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        eprintln!(
            "{label} (typed input is visible on this terminal; pipe the secret to keep it hidden):"
        );
    }
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(0) => Err(format!(
            "{label}: no secret received on stdin (pipe it: printf '%s' \"$KEY\" | coderight ...)"
        )),
        Ok(_) => {
            let secret = line.trim();
            if secret.is_empty() {
                Err(format!("{label}: secret is empty"))
            } else if secret.len() > CLI_SECRET_MAX_LEN {
                Err(format!(
                    "{label}: secret exceeds {CLI_SECRET_MAX_LEN} bytes"
                ))
            } else {
                Ok(secret.to_string())
            }
        }
        Err(error) => Err(format!(
            "{label}: could not read secret from stdin: {error}"
        )),
    }
}

/// Canonical connect/rotate request body for the provider-key PUT.
///
/// Uses the daemon's camelCase wire name (`apiKey`); the handler also accepts
/// the snake_case `api_key` spelling as an alias so both callers deserialize.
pub(crate) fn provider_key_put_body(secret: &str) -> serde_json::Value {
    serde_json::json!({ "apiKey": secret.trim() })
}

// ── S:H14: redacted config view ─────────────────────────────────────────────
//
// `coderight config` prints the merged configuration. The merged config can
// carry secret material (today: remote.telegram_token), so the view redacts
// secret-bearing fields before serialization instead of printing raw config.

/// Replace the string values of secret-bearing config keys with the shared
/// [`rightkit_llm::agent_providers::keystore::REDACTED`] placeholder. Non-string values
/// under a matching key are recursed into (a bool or number is not credential
/// material); non-matching keys are recursed into unconditionally.
pub(crate) fn redact_config_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if config_key_is_secret(key) {
                    match child {
                        serde_json::Value::String(_) => {
                            *child = serde_json::Value::String(
                                rightkit_llm::agent_providers::keystore::REDACTED.to_string(),
                            );
                        }
                        _ => redact_config_json(child),
                    }
                } else {
                    redact_config_json(child);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items.iter_mut() {
                redact_config_json(item);
            }
        }
        _ => {}
    }
}

/// Whether a config key names secret material. Matching is tolerant of the
/// serde casing variants the same field can serialize under
/// (snake_case/camelCase/kebab-case): separators are stripped and the name is
/// lowercased before matching.
fn config_key_is_secret(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect();
    ["apikey", "token", "secret", "password", "passwd"]
        .iter()
        .any(|marker| normalized.contains(marker))
}

/// Build the redacted merged-configuration view for `coderight config`.
/// Secret-bearing fields are replaced before the value ever reaches a
/// serializer; the raw config is never printed.
pub(crate) fn redacted_merged_config_view() -> Result<serde_json::Value, String> {
    let cfg = load_current_merged_config();
    let mut value = serde_json::to_value(&cfg)
        .map_err(|error| format!("config serialization failed: {error}"))?;
    redact_config_json(&mut value);
    Ok(value)
}

#[cfg(test)]
mod output_secret_hygiene_tests {
    use super::*;

    #[test]
    fn color_always_and_never_ignore_terminal_and_no_color() {
        use crate::cli::CliColor::*;
        assert!(resolve_color_enabled(Always, false, true));
        assert!(!resolve_color_enabled(Never, true, false));
    }

    #[test]
    fn color_auto_requires_terminal_and_no_no_color() {
        use crate::cli::CliColor::Auto;
        assert!(resolve_color_enabled(Auto, true, false));
        assert!(!resolve_color_enabled(Auto, false, false));
        assert!(!resolve_color_enabled(Auto, true, true));
    }

    #[test]
    fn provider_key_put_body_uses_the_daemon_wire_name() {
        let body = provider_key_put_body("  sk-value  ");
        assert_eq!(body["apiKey"], "sk-value");
    }

    #[test]
    fn redaction_scan_config_view_hides_secret_fields_and_keeps_the_rest() {
        let mut config = serde_json::json!({
            "remote": {
                "telegram_token": "tok-super-secret-telegram",
                "telegramAllowedUsers": [1, 2],
                "webEnabled": true,
            },
            "orchestration": {
                "agents": {
                    "coder": { "model_id": "test/model", "apiKey": "sk-nested-secret" }
                }
            },
            "api_key": "sk-top-secret",
            "budget": { "perTaskMaxUsd": 1.5 },
        });
        redact_config_json(&mut config);
        let serialized = config.to_string();
        // Redaction scan: no secret substring may survive serialization.
        for secret in [
            "tok-super-secret-telegram",
            "sk-nested-secret",
            "sk-top-secret",
        ] {
            assert!(
                !serialized.contains(secret),
                "redaction scan: {secret} survived the redacted config view"
            );
        }
        assert!(serialized.contains("[redacted]"));
        // Non-secret structure must survive untouched.
        assert_eq!(
            config["remote"]["telegramAllowedUsers"],
            serde_json::json!([1, 2])
        );
        assert_eq!(config["remote"]["webEnabled"], true);
        assert_eq!(
            config["orchestration"]["agents"]["coder"]["model_id"],
            "test/model"
        );
        assert_eq!(config["budget"]["perTaskMaxUsd"], 1.5);
        // Secret keys keep their place, with the placeholder value.
        assert_eq!(
            config["remote"]["telegram_token"],
            rightkit_llm::agent_providers::keystore::REDACTED
        );
    }

    #[test]
    fn redaction_scan_does_not_mangle_non_string_values_under_secret_named_keys() {
        // A key that merely mentions a marker must not have a bool/number
        // replaced with a placeholder — only string credential material is.
        let mut config = serde_json::json!({
            "tokenBudget": 4096,
            "secretsEnabled": false,
        });
        redact_config_json(&mut config);
        assert_eq!(config["tokenBudget"], 4096);
        assert_eq!(config["secretsEnabled"], false);
    }
}
