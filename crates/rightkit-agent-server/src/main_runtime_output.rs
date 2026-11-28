fn cli_json_line(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string())
}

fn emit_cli_json_line(value: &serde_json::Value) {
    println!("{}", cli_json_line(value));
    let _ = std::io::stdout().flush();
}

fn emit_cli_error_json(value: &serde_json::Value) {
    eprintln!("{}", cli_json_line(value));
}

/// R10: every error prints the stderr object `{"error":{code,message,retryable}}`
/// in machine-output mode and the plain message otherwise; stdout carries no
/// error text.
fn cli_error_exit(stream_json: bool, code: CliExitCode, message: &str) -> i32 {
    run_error_exit_on_stderr(stream_json, run_exit_code_from_cli(code), message)
}

fn run_error_exit_on_stderr(machine: bool, code: RunExitCode, message: &str) -> i32 {
    if machine {
        eprintln!("{}", cli_json_line(&run_error_envelope(code, message)));
    } else {
        eprintln!("{message}");
    }
    code.as_i32()
}

/// Exit 3 `unreachable` (headless-cli.md section 2.5, R9, R10): a verb that needs
/// the host found none and nothing ran. The call is safe to retry, so the exit
/// is the suite code 3, not 1. `machine` selects the stderr object (`--json`)
/// over the plain message.
fn host_unreachable_exit(machine: bool) -> i32 {
    run_error_exit_on_stderr(machine, RunExitCode::Unreachable, &no_daemon_message())
}

/// The text of the "no daemon" refusal. Helpers that return `Err(String)` use it so their
/// callers can tell "no host" (exit 3) from any other failure (exit 1).
fn no_daemon_message() -> String {
    format!(
        "No running daemon found. Start one with `{app} serve`.",
        app = crate::host_identity::app()
    )
}

/// Exit code for a daemon-helper `Err(message)`: 3 when the message is the "no daemon" refusal
/// (nothing ran, safe to retry), 1 otherwise. Prints the message either way.
fn daemon_error_exit(machine: bool, message: &str) -> i32 {
    if message == no_daemon_message() {
        return host_unreachable_exit(machine);
    }
    eprintln!("{message}");
    1
}

/// The stable error object (headless-cli.md section 2.5): `code` is fixed per
/// contract version, `message` is the human text, `retryable` is fixed per code.
fn run_error_envelope(code: RunExitCode, message: &str) -> serde_json::Value {
    serde_json::json!({
        "error": {
            "code": code.class(),
            "message": message,
            "retryable": code.retryable(),
        },
    })
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
        "usage" => CliExitCode::Usage,
        // Same classes `run_exit_code_from_failure_kind` gives these kinds, so the
        // stream `error` object and the process exit agree and a retry can help.
        "max_iterations" | "network" | "rate_limited" => CliExitCode::TimeoutStall,
        _ => cli_exit_code_from_error(error),
    }
}

fn cli_exit_code_from_error(error: &str) -> CliExitCode {
    let error = error.to_ascii_lowercase();
    // Transient provider failures first: a retry can help, and their text may
    // carry words ("auth", "denied") that would otherwise misclassify them.
    if error.contains("rate limit")
        || error.contains("rate-limit")
        || error.contains("too many requests")
        || error.contains("overloaded")
        || error.contains("service unavailable")
        || error.contains("temporarily unavailable")
    {
        CliExitCode::TimeoutStall
    } else if error.contains("api key") || error.contains("unauthorized") || error.contains("auth")
    {
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
        let code = run_exit_code_from_cli(code);
        serde_json::json!({
            "code": code.class(),
            "message": message,
            "retryable": code.retryable(),
        })
    });
    let mut value = serde_json::json!({
        "schema_version": crate::host_identity::schema("cli.stream_event.v1"),
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

/// The sequence number of any protocol event. The one definition is RightKit's (#305).
use rightkit_bot_messaging::daemon::event_seq;

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
            reason,
            ..
        } if !approved => {
            // Say why. "denied" alone read as if a person had refused, when in a run with
            // nobody attached no one was asked.
            let why = match reason.as_deref() {
                Some("unattended") => {
                    "not run: this run has nobody to ask, so a call that needs approval is refused"
                }
                Some("detached") => "not run: the client that could approve it went away",
                Some("cancelled") => "not run: the turn was stopped",
                _ => "denied",
            };
            eprintln!("[{why}] {tool_call_id}");
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
    use rightkit_agent_coding_tools::Tool;
    use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};

    let workspace_path = workspace
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
    let ctx = rightkit_agent_coding_tools::ToolContext::new(
        "cli-diff",
        &workspace_path,
        PermissionMode::Bypass,
        WorkspaceTrust::Trusted,
    );
    let repo_path = workspace_path.display().to_string();

    let status = rightkit_agent_coding_tools::git_read::GitReadTool
        .execute(serde_json::json!({ "repo_path": repo_path.clone() }), &ctx)
        .await;
    if status.is_error {
        eprintln!("{}", status.content);
        return 1;
    }

    let diff = rightkit_agent_coding_tools::git_diff::GitDiffTool
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

/// What a `{app} review` looks at: the current changes (default), the
/// changes against a base ref, what is not committed yet, or one commit.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ReviewScope {
    Current,
    Base(String),
    Uncommitted,
    Commit(String),
}

/// The scope the review flags name. `--base`, `--uncommitted` and `--commit`
/// exclude one another (clap enforces it; this also refuses a blank value and a
/// commit that could be read as a git option).
fn review_scope(
    base: Option<&str>,
    uncommitted: bool,
    commit: Option<&str>,
) -> Result<ReviewScope, String> {
    if let Some(commit) = commit {
        let commit = commit.trim();
        if commit.is_empty()
            || commit.starts_with('-')
            || commit.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(format!(
                "`--commit` takes a commit sha or ref, not {commit:?}"
            ));
        }
        return Ok(ReviewScope::Commit(commit.to_string()));
    }
    if uncommitted {
        return Ok(ReviewScope::Uncommitted);
    }
    Ok(
        match base.map(str::trim).filter(|value| !value.is_empty()) {
            Some(base) => ReviewScope::Base(base.to_string()),
            None => ReviewScope::Current,
        },
    )
}

fn build_cli_review_goal(focus: &[String], scope: &ReviewScope, dirs: &[String]) -> String {
    let mut prompt = format!("Run a read-only auditor pass for the current workspace changes. Create a task categorized as `code-review` so {host} routes this to `auditor`, not the plan-review jury/council. Review git status, git diff, and relevant files as evidence. Findings should lead, ordered by severity, with concrete file/line references wherever possible. Keep summaries brief. If there are no actionable issues, say that clearly and mention any remaining test gaps or residual risk.", host = rightkit_agent_config::app_name());
    match scope {
        ReviewScope::Current => {}
        ReviewScope::Base(base) => {
            prompt.push_str("\n\nReview scope base ref: ");
            prompt.push_str(&json_string(base));
            prompt.push_str(
                ". Compare current changes against this ref when collecting git evidence.",
            );
        }
        ReviewScope::Uncommitted => {
            prompt.push_str("\n\nReview scope: uncommitted changes only. Review the working tree and the index against HEAD (`git status`, `git diff HEAD`, and the untracked files `git status` lists); ignore everything already committed.");
        }
        ReviewScope::Commit(commit) => {
            prompt.push_str("\n\nReview scope: one commit, ");
            prompt.push_str(&json_string(commit));
            prompt.push_str(". Read it with `git show ");
            prompt.push_str(commit);
            prompt.push_str("` and review only what that commit changed; ignore later changes and the uncommitted working tree.");
        }
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
/// discarded made `{app} review --max-budget-usd 1` look accepted while
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
        .ok_or_else(|| {
            format!(
                "Usage: {app} review findings <session-id> [--json]",
                app = crate::host_identity::app()
            )
        })
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
/// Piped input (`printf '%s' "$KEY" | {app} provider connect openrouter`)
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
            "{label}: no secret received on stdin (pipe it: printf '%s' \"$KEY\" | {app} ...)",
            app = crate::host_identity::app()
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

// ── S:H14: redacted config view ─────────────────────────────────────────────
//
// `{app} config` prints the merged configuration. The merged config can
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

/// Build the redacted merged-configuration view for `{app} config`.
/// Secret-bearing fields are replaced before the value ever reaches a
/// serializer; the raw config is never printed.
pub(crate) fn redacted_merged_config_view() -> Result<serde_json::Value, String> {
    let cfg = load_current_merged_config();
    let mut value = serde_json::to_value(&cfg)
        .map_err(|error| format!("config serialization failed: {error}"))?;
    redact_config_json(&mut value);
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "schema_version".to_string(),
            serde_json::json!(crate::host_identity::schema("cli.config.v1")),
        );
        // Keys in a config file that no setting reads (a typo such as `[permisions]`): named
        // here, and only when there are any, so a clean config prints exactly what it did.
        let unknown = rightkit_agent_config::ConfigLoader::unknown_keys();
        if !unknown.is_empty() {
            object.insert(
                "unknown_keys".to_string(),
                serde_json::Value::Array(
                    unknown
                        .iter()
                        .map(|layer| {
                            serde_json::json!({
                                "layer": layer.layer,
                                "path": layer.path.display().to_string(),
                                "keys": layer.keys,
                            })
                        })
                        .collect(),
                ),
            );
        }
    }
    Ok(value)
}
