// Vendor-CLI turn path for `claude-code/<model>` and `antigravity/<model>`
// routes.
//
// A vendor route runs turns through the vendor's own installed CLI instead
// of the `SessionExecutionHost` run loop: `claude` reuses the Claude harness
// launch contract (stream-json in and out, the `--tools ""` delegation
// policy, a run-scoped `--mcp-config` exposing the `coderight` stdio server)
// and `agy` runs through `agy_harness` (`-p` + `--output-format
// stream-json`, `--conversation` for resume). Vendor stdout frames are
// parsed into the shared `NormalizedEvent` vocabulary and projected into the
// same `host_events` render queue `poll_timeline_events` drains, so vendor
// turns render exactly like native turns in the TUI transcript.
//
// Claude's MCP child proxies into this turn's parent InlineHost over a
// run-scoped authenticated local pipe. Its official permission-prompt tool
// uses normal RightKit approvals; the full catalog remains policy governed.
// Antigravity keeps its user-level MCP registration and native permission
// policy. Only Yolo maps to --dangerously-skip-permissions.
//
// Continuity: the vendor session/conversation id from the turn's
// `SessionStarted` event is keyed `"{provider_id}:{session_id}"` and
// replayed as `--resume` (Claude) or `--conversation` (agy) on the next
// turn of that RightKit session. The map is in-memory — a restarted TUI
// starts a fresh vendor conversation; RightKit's own transcript remains
// the durable continuity surface.

use rightkit_agents::driver_host::vendor_cli::{detect, VendorCli};
use rightkit_agents::driver_host::{
    agy_harness, apply_claude_account_env, parse_claude_stream_events, ClaudeAccountIdentity,
    ClaudeAccountLaunch, ClaudeLaunchOptions, NormalizedEvent, NormalizedEventKind,
};
use rightkit_terminal_ui::transcript::timeline::{DaemonEvent, ToolStatus};

type VendorEventQueue = std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<DaemonEvent>>>;
type VendorConversationMap =
    std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>;

/// Parse a `claude-code/<model>` or `antigravity/<model>` route string.
/// Returns `None` for every non-vendor route so dispatch can branch on
/// `Some(..)` alone; an empty model segment is not a vendor route (the route
/// namespace always carries one).
fn parse_vendor_route(route: &str) -> Option<(VendorCli, String)> {
    let (provider, model) = route.trim().trim_start_matches('/').split_once('/')?;
    let kind = VendorCli::from_provider_id(provider)?;
    let model = model.trim();
    if model.is_empty() {
        return None;
    }
    Some((kind, model.to_string()))
}

/// The vendor route carried by turn args (`--provider <id> --model <id>`),
/// or `None` when the turn belongs to the native provider path.
fn vendor_route_for_args(args: &RunArgs) -> Option<(VendorCli, Option<String>)> {
    let provider = args.provider.as_deref()?;
    let kind = VendorCli::from_provider_id(provider)?;
    Some((kind, args.model.clone()))
}

fn vendor_queue_push(queue: &VendorEventQueue, event: DaemonEvent) {
    const HOST_EVENT_CAP: usize = 512;
    let mut queue = queue.lock().unwrap_or_else(|e| e.into_inner());
    if queue.len() >= HOST_EVENT_CAP {
        queue.pop_front();
    }
    queue.push_back(event);
}

fn vendor_system(queue: &VendorEventQueue, text: impl Into<String>) {
    vendor_queue_push(queue, DaemonEvent::System { text: text.into() });
}

/// Project one normalized vendor event into the shared transcript event
/// vocabulary. Returns `(assistant_settled, terminal_failure)` bookkeeping
/// so the runner can finalize the entry and pick an exit code.
fn project_vendor_event(
    event: &NormalizedEvent,
    sink: &mut dyn FnMut(DaemonEvent),
    conversations: &VendorConversationMap,
    conversation_key: &str,
    saw_delta: &mut bool,
) {
    match &event.kind {
        NormalizedEventKind::SessionStarted {
            vendor_session_id, ..
        } => {
            if let Ok(mut map) = conversations.lock() {
                map.insert(conversation_key.to_string(), vendor_session_id.clone());
            }
        }
        NormalizedEventKind::AssistantDelta { text } => {
            *saw_delta = true;
            sink(DaemonEvent::AssistantDelta {
                token: text.clone(),
            });
        }
        NormalizedEventKind::AssistantCompleted { text } => {
            // Full-message frames repeat text already streamed as deltas;
            // only emit the body when no delta covered it.
            if !*saw_delta && !text.is_empty() {
                sink(DaemonEvent::AssistantDelta {
                    token: text.clone(),
                });
            }
            sink(DaemonEvent::AssistantSettled);
        }
        NormalizedEventKind::ToolCallStarted {
            call_id,
            tool_name,
            arguments,
        } => {
            sink(DaemonEvent::ToolStarted {
                call_id: call_id.clone(),
                name: tool_name.clone(),
                args_preview: preview_json(arguments, 160),
            });
        }
        NormalizedEventKind::ToolResult {
            call_id,
            is_error,
            content,
        } => {
            sink(DaemonEvent::ToolUpdated {
                call_id: call_id.clone(),
                status: if *is_error {
                    ToolStatus::Failed
                } else {
                    ToolStatus::Succeeded
                },
                output_preview: preview_json(content, 240),
            });
        }
        NormalizedEventKind::ApprovalRequested { action, .. } => {
            // Vendor CLIs run non-interactively here; there is no approval
            // channel back. Surface the gate rather than opening a TUI
            // prompt the vendor cannot consume an answer from.
            sink(DaemonEvent::System {
                text: format!(
                    "{action} — approval requested by vendor; unapproved tools are denied."
                ),
            });
        }
        NormalizedEventKind::TurnCompleted => {
            sink(DaemonEvent::AssistantSettled);
        }
        NormalizedEventKind::TurnCompletedWithUsage {
            prompt_tokens,
            completion_tokens,
            ..
        } => {
            sink(DaemonEvent::AssistantSettled);
            sink(DaemonEvent::System {
                text: format!("usage: {prompt_tokens} in / {completion_tokens} out"),
            });
        }
        NormalizedEventKind::TurnInterrupted => {
            sink(DaemonEvent::AssistantSettled);
            sink(DaemonEvent::System {
                text: "Turn interrupted".to_string(),
            });
        }
        NormalizedEventKind::TurnFailed { message } => {
            sink(DaemonEvent::AssistantSettled);
            sink(DaemonEvent::System {
                text: message.clone(),
            });
        }
        NormalizedEventKind::RateLimit {
            account,
            status,
            rate_limit_type,
            resets_at,
            utilization,
            ..
        } => {
            // Informational unless the account is actually capped; switching accounts is a
            // manual user action, never automatic.
            let who = account
                .as_ref()
                .map(|who| who.display())
                .unwrap_or_else(|| "Claude account".to_string());
            let window = rate_limit_type.as_deref().unwrap_or("usage");
            let text = if status == "rejected" {
                match resets_at {
                    Some(at) => format!(
                        "{who} is capped ({window} limit) until unix {at}; continue on another account to keep working."
                    ),
                    None => format!(
                        "{who} is capped ({window} limit); continue on another account to keep working."
                    ),
                }
            } else if let Some(used) = utilization {
                format!("{who}: {window} limit {status} ({:.0}% used)", used * 100.0)
            } else {
                format!("{who}: {window} limit {status}")
            };
            sink(DaemonEvent::System { text });
        }
        NormalizedEventKind::TurnCostReported {
            total_cost_usd: Some(cost),
            ..
        } => {
            sink(DaemonEvent::System {
                text: format!("cost: ${cost:.4}"),
            });
        }
        NormalizedEventKind::TurnCostReported { .. } => {}
        NormalizedEventKind::TurnStarted { .. } | NormalizedEventKind::GatewayReady => {}
    }
}

fn preview_json(value: &serde_json::Value, cap: usize) -> String {
    let text = match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let text = text.trim().replace('\n', " ");
    if text.chars().count() <= cap {
        return text;
    }
    let truncated: String = text.chars().take(cap).collect();
    format!("{truncated}…")
}

/// The MCP server argv shared by both vendors: the inline governed
/// projection scoped to the turn workspace. `--allow-mutate` (bypass-mode
/// inline session + mutating toolset) is only added when the session's
/// permission mode already auto-applies mutations.
fn vendor_mcp_args(workspace: &std::path::Path, allow_mutate: bool) -> Vec<String> {
    let mut args = vec![
        "mcp-server".to_string(),
        "--workspace".to_string(),
        workspace.to_string_lossy().into_owned(),
    ];
    if allow_mutate {
        args.push("--allow-mutate".to_string());
    }
    args
}

/// The Claude account a session selected, resolved to launch inputs plus the non-secret
/// identity stamped onto its rate-limit and cost events.
pub(crate) struct ResolvedClaudeAccount {
    pub launch: ClaudeAccountLaunch,
    pub identity: ClaudeAccountIdentity,
}

/// Turn one selected account id into launch inputs. `Ok(None)` when the selection belongs to
/// another provider. A selected account whose profile or key is missing is an error: the launch
/// must fail rather than run (and bill) under a different account.
pub(crate) fn claude_account_for_selection(
    account_id: &str,
    load_key: impl Fn(&str) -> Result<Option<String>, String>,
    load_profile: impl Fn(&str) -> Result<Option<(std::path::PathBuf, ClaudeAccountIdentity)>, String>,
) -> Result<Option<ResolvedClaudeAccount>, String> {
    account_for_selection("claude", account_id, load_key, load_profile)
}

/// Shared by every provider with per-account homes/keys (Claude and Codex): one code path.
pub(crate) fn account_for_selection(
    expected_provider: &str,
    account_id: &str,
    load_key: impl Fn(&str) -> Result<Option<String>, String>,
    load_profile: impl Fn(&str) -> Result<Option<(std::path::PathBuf, ClaudeAccountIdentity)>, String>,
) -> Result<Option<ResolvedClaudeAccount>, String> {
    let mut parts = account_id.split('/');
    let (kind, provider, tail) = match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(kind), Some(provider), Some(tail), None) => (kind, provider, tail),
        _ => return Err(format!("malformed account selection: {account_id}")),
    };
    if provider != expected_provider {
        return Ok(None);
    }
    match kind {
        "subscription" => {
            let (managed_home, identity) = load_profile(tail)?.ok_or_else(|| {
                format!("selected {provider} account {account_id} has no managed profile")
            })?;
            Ok(Some(ResolvedClaudeAccount {
                launch: ClaudeAccountLaunch::Subscription { managed_home },
                identity,
            }))
        }
        "api_key" => {
            let key = load_key(account_id)?.ok_or_else(|| {
                format!("selected {provider} account {account_id} has no stored API key")
            })?;
            Ok(Some(ResolvedClaudeAccount {
                launch: ClaudeAccountLaunch::ApiKey { key },
                identity: ClaudeAccountIdentity::api_key(provider, tail, tail),
            }))
        }
        _ => Err(format!("malformed account selection: {account_id}")),
    }
}

/// Resolve the session's selected Claude account (session selection, else project default,
/// else none). Never picks another account.
pub(crate) fn resolve_claude_account(
    session_id: &str,
    project: &str,
) -> Result<Option<ResolvedClaudeAccount>, String> {
    resolve_provider_account("claude", session_id, project)
}

/// Codex twin of [`resolve_claude_account`]: same registry, same precedence, no auto-switching.
pub(crate) fn resolve_codex_account(
    session_id: &str,
    project: &str,
) -> Result<Option<ResolvedClaudeAccount>, String> {
    resolve_provider_account("codex", session_id, project)
}

pub(crate) fn resolve_provider_account(
    provider_name: &'static str,
    session_id: &str,
    project: &str,
) -> Result<Option<ResolvedClaudeAccount>, String> {
    let store = match rightkit_app_settings::agent_config::accounts::AccountStore::default_location() {
        Ok(store) => store,
        Err(rightkit_app_settings::agent_config::accounts::AccountsError::NoStateRoot) => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let Some(account_id) = store
        .resolve(Some(session_id), Some(project))
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let mut resolved = account_for_selection(
        provider_name,
        &account_id,
        |id| rightkit_llm::agent_providers::client::load_account_key(id).map_err(|error| error.to_string()),
        |label| {
            let provider = rightkit_agents::driver_host::cli_runtime::CliProviderId::new(provider_name);
            let record =
                rightkit_agents::driver_host::cli_runtime::profile::load_profile(&provider, label)
                    .map_err(|error| error.to_string())?;
            Ok(record.map(|record| {
                let identity =
                    rightkit_agents::driver_host::cli_runtime::account_identity::identity_for_profile(
                        &format!("{provider_name}/{label}"),
                    )
                    .unwrap_or(ClaudeAccountIdentity {
                        provider_id: provider_name.to_string(),
                        label: Some(label.to_string()),
                        kind: Some("subscription".to_string()),
                        account_id: Some(format!("subscription/{provider_name}/{label}")),
                        ..Default::default()
                    });
                (record.managed_home, identity)
            }))
        },
    )?;
    if let Some(resolved) = resolved.as_mut() {
        apply_api_key_label(&mut resolved.identity, &account_id, &store);
    }
    Ok(resolved)
}

/// An API-key identity starts with its slug as the label; swap in the label the user gave the
/// config (for example "Team") when the registry has it.
fn apply_api_key_label(
    identity: &mut ClaudeAccountIdentity,
    account_id: &str,
    store: &rightkit_app_settings::agent_config::accounts::AccountStore,
) {
    if identity.kind.as_deref() != Some("api_key") {
        return;
    }
    if let Some(entry) = store.load().ok().and_then(|file| {
        file.api_key_accounts
            .into_iter()
            .find(|a| a.id == account_id)
    }) {
        identity.label = Some(entry.label);
    }
}

/// Credential variables that would outrank (or silently replace) a managed subscription login.
/// Claude Code reads these before the profile's own login (#344).
pub(crate) const CLAUDE_CREDENTIAL_ENV: [&str; 3] = [
    "CLAUDE_CODE_OAUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
];
/// Codex counterpart: only matters for a subscription account (an API-key account sets its own).
pub(crate) const CODEX_CREDENTIAL_ENV: [&str; 1] = ["OPENAI_API_KEY"];

/// Credential precedence guard (#344). Under a managed subscription profile the profile login
/// must be the only credential in play, so every ambient credential variable is removed from the
/// child environment. Under a Claude API-key account only the OAuth-style tokens are removed
/// (the account's own `ANTHROPIC_API_KEY` stays). Returns the names that were present in the
/// ambient environment or the spec and got stripped, so the caller can flag them.
pub(crate) fn apply_credential_precedence_guard(
    spec: &mut rightkit_agents::driver_host::LaunchSpec,
    provider: &str,
    account: Option<&ClaudeAccountLaunch>,
    ambient_present: &dyn Fn(&str) -> bool,
) -> Vec<&'static str> {
    let Some(account) = account else {
        return Vec::new();
    };
    let names: &[&'static str] = match (provider, account) {
        ("claude", ClaudeAccountLaunch::Subscription { .. }) => &CLAUDE_CREDENTIAL_ENV,
        ("claude", ClaudeAccountLaunch::ApiKey { .. }) => {
            &["CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_AUTH_TOKEN"]
        }
        ("codex", ClaudeAccountLaunch::Subscription { .. }) => &CODEX_CREDENTIAL_ENV,
        _ => &[],
    };
    let mut stripped = Vec::new();
    for name in names {
        if ambient_present(name) || spec.env.contains_key(*name) {
            stripped.push(*name);
        }
        spec.env.remove(*name);
        spec.env_remove.insert((*name).to_string());
    }
    stripped
}

fn ambient_env_present(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

/// The active auth method of one session, for `/status`, the driver status route and the
/// desktop UI (#347). Never carries a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthMethodReport {
    pub provider: String,
    /// `subscription`, `api_key`, `api_key_env`, `oauth_token_env`, `ambient_login`, `unavailable`.
    pub method: &'static str,
    pub account_id: Option<String>,
    pub label: Option<String>,
    /// Credential variables stripped from the child env by the precedence guard (#344).
    pub stripped_env: Vec<&'static str>,
    pub warnings: Vec<String>,
}

impl AuthMethodReport {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "provider": self.provider,
            "method": self.method,
            "accountId": self.account_id,
            "label": self.label,
            "strippedEnv": self.stripped_env,
            "warnings": self.warnings,
        })
    }

    pub(crate) fn render_line(&self) -> String {
        let what = match self.method {
            "subscription" => "subscription login (managed profile)",
            "api_key" => "API key account",
            "api_key_env" => "API key from the environment",
            "oauth_token_env" => "OAuth token from the environment",
            "ambient_login" => "the CLI's own login",
            _ => "unavailable",
        };
        let mut line = format!("{} auth: {what}", self.provider);
        if let Some(label) = &self.label {
            line.push_str(&format!(" [{label}]"));
        }
        if !self.stripped_env.is_empty() {
            line.push_str(&format!(
                "\n  credential variables removed from the child environment: {}",
                self.stripped_env.join(", ")
            ));
        }
        for warning in &self.warnings {
            line.push_str(&format!("\n  warning: {warning}"));
        }
        line
    }
}

/// Pure core of [`session_auth_report`]: `account` is the resolved selection, `ambient_present`
/// answers whether a variable is set (and non-empty) in the daemon environment.
pub(crate) fn auth_method_report(
    provider: &str,
    account: Option<&ResolvedClaudeAccount>,
    ambient_present: &dyn Fn(&str) -> bool,
) -> AuthMethodReport {
    let mut report = AuthMethodReport {
        provider: provider.to_string(),
        method: "ambient_login",
        account_id: None,
        label: None,
        stripped_env: Vec::new(),
        warnings: Vec::new(),
    };
    match account {
        Some(resolved) => {
            report.account_id = resolved.identity.account_id.clone();
            report.label = Some(resolved.identity.display());
            match &resolved.launch {
                ClaudeAccountLaunch::Subscription { .. } => {
                    report.method = "subscription";
                    let guarded: &[&'static str] = match provider {
                        "claude" => &CLAUDE_CREDENTIAL_ENV,
                        "codex" => &CODEX_CREDENTIAL_ENV,
                        _ => &[],
                    };
                    report.stripped_env = guarded
                        .iter()
                        .copied()
                        .filter(|name| ambient_present(name))
                        .collect();
                }
                ClaudeAccountLaunch::ApiKey { .. } => {
                    report.method = "api_key";
                    if provider == "claude" {
                        report.stripped_env = ["CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_AUTH_TOKEN"]
                            .into_iter()
                            .filter(|name| ambient_present(name))
                            .collect();
                    }
                }
            }
        }
        None if provider == "claude" => {
            if ambient_present("ANTHROPIC_API_KEY") {
                report.method = "api_key_env";
                report.warnings.push(
                    "ANTHROPIC_API_KEY is set in the environment and overrides a subscription login; usage is billed to API credits, not your plan. Unset it, or pick a subscription account."
                        .to_string(),
                );
            } else if ambient_present("ANTHROPIC_AUTH_TOKEN") {
                report.method = "api_key_env";
                report.warnings.push(
                    "ANTHROPIC_AUTH_TOKEN is set in the environment and overrides a subscription login."
                        .to_string(),
                );
            } else if ambient_present("CLAUDE_CODE_OAUTH_TOKEN") {
                report.method = "oauth_token_env";
                report.warnings.push(
                    "CLAUDE_CODE_OAUTH_TOKEN is set in the environment and overrides the profile login."
                        .to_string(),
                );
            }
        }
        None => {}
    }
    report
}

/// Live auth report for one session: resolves the explicit selection (session, else project
/// default) the same way a launch does, then inspects the daemon environment.
pub(crate) fn session_auth_report(
    provider: &'static str,
    session_id: &str,
    project: &str,
) -> AuthMethodReport {
    match resolve_provider_account(provider, session_id, project) {
        Ok(account) => auth_method_report(provider, account.as_ref(), &ambient_env_present),
        Err(error) => AuthMethodReport {
            provider: provider.to_string(),
            method: "unavailable",
            account_id: None,
            label: None,
            stripped_env: Vec::new(),
            warnings: vec![format!("selected account unavailable: {error}")],
        },
    }
}

/// Claude's `--permission-mode` for a RightKit policy mode. RightKit stays the approval
/// authority (permission prompts route to `mcp__coderight__permission_prompt`), so only the two
/// modes that have a Claude equivalent change anything; `Bypass` is never forwarded because the
/// driver host refuses `bypassPermissions`.
fn claude_permission_mode_for(mode: rightkit_agent_policy::PermissionMode) -> &'static str {
    match mode {
        rightkit_agent_policy::PermissionMode::Plan => "plan",
        rightkit_agent_policy::PermissionMode::AcceptEdits => "acceptEdits",
        _ => "default",
    }
}

/// Launch extras derivable from the vendor turn's own options. `fork_session`,
/// `resume_session_at` and `fallback_model` have no source in the run/turn options; `max_turns`
/// and `max_budget_usd` come from `ClaudeRunFlags` (which overrides the same flags).
fn claude_launch_extras(
    permission_mode: rightkit_agent_policy::PermissionMode,
) -> rightkit_agents::driver_host::ClaudeLaunchExtras {
    rightkit_agents::driver_host::ClaudeLaunchExtras {
        permission_mode: Some(claude_permission_mode_for(permission_mode).to_string()),
        ..Default::default()
    }
}

/// Build the vendor child's launch spec + prompt delivery for one turn.
/// `Err` strings are already presentation-ready.
fn vendor_launch_spec(
    kind: VendorCli,
    executable: &std::path::Path,
    workspace: &std::path::Path,
    model: Option<&str>,
    conversation_id: Option<&str>,
    permission_mode: rightkit_agent_policy::PermissionMode,
    prompt: &str,
    account: Option<&ClaudeAccountLaunch>,
) -> Result<(rightkit_agents::driver_host::LaunchSpec, Option<String>), String> {
    match kind {
        VendorCli::ClaudeCode => {
            let current_exe = std::env::current_exe()
                .map_err(|error| format!("cannot resolve coderight executable: {error}"))?;
            let mcp_config = serde_json::json!({
                "mcpServers": {
                    "coderight": {
                        "type": "stdio",
                        "command": current_exe,
                        "args": vendor_mcp_args(workspace, false),
                    }
                }
            })
            .to_string();
            let options = ClaudeLaunchOptions {
                executable: executable.to_path_buf(),
                cwd: workspace.to_path_buf(),
                mcp_config,
                session_id: None,
                resume_session_id: conversation_id.map(str::to_string),
                model: model.map(str::to_string),
            };
            let extras = claude_launch_extras(permission_mode);
            let mut spec = options
                .launch_spec_with(&extras)
                .map_err(|error| format!("claude launch refused: {error}"))?;
            apply_claude_account_env(&mut spec, account);
            apply_credential_precedence_guard(&mut spec, "claude", account, &ambient_env_present);
            if let Some(index) = spec.args.iter().position(|arg| arg == "--allowedTools") {
                spec.args.drain(index..index + 2);
            }
            spec.args.extend([
                "--permission-prompt-tool".into(),
                "mcp__coderight__permission_prompt".into(),
            ]);
            let frame = serde_json::json!({
                "type": "user",
                "message": {
                    "role": "user",
                    "content": [{ "type": "text", "text": prompt }]
                }
            });
            Ok((spec, Some(format!("{frame}\n"))))
        }
        VendorCli::Antigravity => {
            let options = agy_harness::AgyLaunchOptions {
                executable: executable.to_path_buf(),
                cwd: workspace.to_path_buf(),
                conversation_id: conversation_id.map(str::to_string),
                model: model.map(str::to_string),
                dangerously_skip_permissions: permission_mode
                    == rightkit_agent_policy::PermissionMode::Bypass,
                add_dirs: vec![workspace.to_path_buf()],
            };
            let mut spec = options
                .launch_spec()
                .map_err(|error| format!("agy launch refused: {error}"))?;
            // `-p` carries the prompt on argv; stdin stays closed.
            spec.args.extend(agy_harness::user_prompt_frame(prompt));
            Ok((spec, None))
        }
    }
}

/// Spawn + read one vendor turn to completion on a blocking thread.
/// `stdin_frame` is written once and closed (Claude's stream-json input);
/// agy takes its prompt on argv so `stdin_frame` is `None`. Events are
/// projected through `emit` as they arrive.
fn vendor_turn_blocking(
    kind: VendorCli,
    spec: rightkit_agents::driver_host::LaunchSpec,
    stdin_frame: Option<String>,
    conversations: VendorConversationMap,
    conversation_key: String,
    account: Option<ClaudeAccountIdentity>,
    mut emit: Box<dyn FnMut(DaemonEvent) + Send>,
) -> i32 {
    use std::io::{BufRead, BufReader, Write};

    let mut command = rightkit_child_process::command_std(&spec.program);
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    command.stdin(if stdin_frame.is_some() {
        std::process::Stdio::piped()
    } else {
        std::process::Stdio::null()
    });
    for name in &spec.env_remove {
        command.env_remove(name);
    }
    for (name, value) in &spec.env {
        command.env(name, value);
    }
    // agy's detached auto-updater spawns a console grandchild that our
    // CREATE_NO_WINDOW cannot reach; harmless for every other vendor.
    command.env(
        rightkit_agents::driver_host::vendor_cli::AGY_DISABLE_AUTO_UPDATE_ENV,
        "1",
    );
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            emit(DaemonEvent::System {
                text: format!("{} failed to launch: {error}", spec.program.display()),
            });
            return 1;
        }
    };
    if let Some(frame) = stdin_frame {
        if let Some(mut stdin) = child.stdin.take() {
            if let Err(error) = stdin.write_all(frame.as_bytes()) {
                emit(DaemonEvent::System {
                    text: format!("could not deliver prompt to vendor CLI: {error}"),
                });
            }
        }
    }
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let _ = child.kill();
            emit(DaemonEvent::System {
                text: "vendor CLI produced no stdout stream".to_string(),
            });
            return 1;
        }
    };
    // Drain stderr on its own thread so a chatty vendor cannot deadlock on
    // a full pipe; keep only a bounded tail for failure reporting.
    let stderr_tail = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    if let Some(mut stderr) = child.stderr.take() {
        let tail = std::sync::Arc::clone(&stderr_tail);
        std::thread::spawn(move || {
            const STDERR_CAP: usize = 8 * 1024;
            let mut buffer = [0_u8; 4096];
            loop {
                match std::io::Read::read(&mut stderr, &mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        let mut tail = tail.lock().unwrap_or_else(|e| e.into_inner());
                        tail.push_str(&String::from_utf8_lossy(&buffer[..read]));
                        let len = tail.len();
                        if len > STDERR_CAP {
                            let keep = tail.split_off(len - STDERR_CAP);
                            *tail = keep;
                        }
                    }
                }
            }
        });
    }

    let child = std::sync::Arc::new(std::sync::Mutex::new(child));
    // Publish the live child so a cancellation arriving from the TUI can
    // kill it while this blocking thread is parked on stdout reads. The
    // guard clears the slot on every exit path.
    if let Ok(mut slot) = VENDOR_ACTIVE_CHILD.lock() {
        *slot = Some(std::sync::Arc::clone(&child));
    }
    let _child_slot = ActiveChildGuard;
    let mut offset = 0_u64;
    let mut saw_delta = false;
    let mut saw_failure = false;
    let mut line = String::new();
    let mut reader = BufReader::new(stdout);
    loop {
        line.clear();
        let read = match reader.read_line(&mut line) {
            Ok(read) => read,
            Err(_) => break,
        };
        if read == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        offset = offset.saturating_add(1);
        let parsed = match kind {
            VendorCli::ClaudeCode => parse_claude_stream_events(trimmed, offset, account.as_ref()),
            VendorCli::Antigravity => agy_harness::parse_agy_stream_line(trimmed, offset)
                .map(|event| event.into_iter().collect::<Vec<_>>()),
        };
        match parsed {
            Ok(events) => {
                for event in events {
                    if matches!(event.kind, NormalizedEventKind::TurnFailed { .. }) {
                        saw_failure = true;
                    }
                    project_vendor_event(
                        &event,
                        &mut *emit,
                        &conversations,
                        &conversation_key,
                        &mut saw_delta,
                    );
                }
            }
            Err(error) => {
                // A malformed or newly-introduced vendor frame is
                // presentation noise, not a fatal turn error — the durable
                // driver path fails closed, this render path tolerates.
                tracing::warn!(error = %error, "vendor stream frame not projected");
            }
        }
    }
    let status = child.lock().unwrap_or_else(|e| e.into_inner()).wait();
    let failed = saw_failure || !matches!(status, Ok(exit) if exit.success());
    if failed {
        let tail = stderr_tail
            .lock()
            .map(|tail| tail.trim().to_string())
            .unwrap_or_default();
        if !tail.is_empty() {
            emit(DaemonEvent::System {
                text: format!("{}: {}", kind.binary_name(), preview_stderr(&tail)),
            });
        }
        return 1;
    }
    0
}

fn preview_stderr(tail: &str) -> String {
    let collapsed = tail.replace('\n', " ");
    let collapsed = collapsed.trim();
    if collapsed.chars().count() <= 240 {
        collapsed.to_string()
    } else {
        collapsed.chars().take(240).collect::<String>() + "…"
    }
}

/// Interactive vendor turn (TUI path): emits into the shared host event
/// queue the renderer drains, honours `control` cancellation by killing the
/// child, and always settles the cancellation slot so the next turn admits.
async fn run_vendor_turn(
    kind: VendorCli,
    model: Option<String>,
    workspace: String,
    prompt: String,
    session_id: String,
    permission_mode: rightkit_agent_policy::PermissionMode,
    events: VendorEventQueue,
    conversations: VendorConversationMap,
    control: std::sync::Arc<RunCancellation>,
) -> i32 {
    control
        .admission_started
        .store(true, std::sync::atomic::Ordering::Release);
    let status = detect(kind);
    if !status.installed {
        vendor_system(
            &events,
            format!(
                "{} is not installed — {}.",
                kind.display_name(),
                kind.install_hint()
            ),
        );
        control.settled();
        return 1;
    }
    let executable = status.path.expect("installed status carries a path");
    let workspace_path = std::path::PathBuf::from(&workspace);
    let conversation_key = format!("{}:{}", kind.provider_id(), session_id);
    let stored = conversations
        .lock()
        .ok()
        .and_then(|map| map.get(&conversation_key).cloned());
    let account = if kind == VendorCli::ClaudeCode {
        match resolve_claude_account(&session_id, &workspace) {
            Ok(account) => account,
            Err(message) => {
                vendor_system(&events, message);
                control.settled();
                return 1;
            }
        }
    } else {
        None
    };
    let (mut spec, stdin_frame) = match vendor_launch_spec(
        kind,
        &executable,
        &workspace_path,
        model.as_deref(),
        stored.as_deref(),
        permission_mode,
        &prompt,
        account.as_ref().map(|resolved| &resolved.launch),
    ) {
        Ok(launch) => launch,
        Err(message) => {
            vendor_system(&events, message);
            control.settled();
            return 1;
        }
    };
    let _bridge = match start_vendor_tool_bridge(
        kind,
        &workspace_path,
        &session_id,
        permission_mode,
        &control,
        Some(events.clone()),
        &mut spec,
    )
    .await
    {
        Ok(bridge) => bridge,
        Err(error) => {
            vendor_system(&events, error);
            control.settled();
            return 1;
        }
    };
    // Cancellation: a kill task watches the shared token and terminates the
    // vendor child; the reader sees EOF and settles the slot.
    let cancel = control.requested.clone();
    let emit_queue = std::sync::Arc::clone(&events);
    let mut handle = tokio::task::spawn_blocking(move || {
        vendor_turn_blocking(
            kind,
            spec,
            stdin_frame,
            conversations,
            conversation_key,
            account.map(|resolved| resolved.identity),
            Box::new(move |event| vendor_queue_push(&emit_queue, event)),
        )
    });
    let code = tokio::select! {
        result = &mut handle => result.unwrap_or(1),
        _ = cancel.cancelled() => kill_watched(&mut handle).await,
    };
    if cancel.is_cancelled() {
        vendor_system(&events, "Turn interrupted");
        control.settled();
        return 130;
    }
    control.settled();
    code
}

/// Kill the in-flight vendor child after cancellation and await reader
/// settlement. The child lives inside `vendor_turn_blocking`'s blocking
/// task and cannot be cloned, so `vendor_turn_blocking` publishes it into
/// `VENDOR_ACTIVE_CHILD` for exactly this moment; the kill turns the
/// reader's blocking `read_line` into EOF and the JoinHandle resolves
/// normally instead of being abandoned.
async fn kill_watched(handle: &mut tokio::task::JoinHandle<i32>) -> i32 {
    if let Some(child) = VENDOR_ACTIVE_CHILD
        .lock()
        .ok()
        .and_then(|slot| slot.as_ref().map(std::sync::Arc::clone))
    {
        if let Ok(mut child) = child.lock() {
            let _ = child.kill();
        }
    }
    handle.await.unwrap_or(1)
}

/// The single in-flight vendor child, published by `vendor_turn_blocking` so
/// `kill_watched` can terminate a process owned by the blocking reader task.
/// `RunCancellation::require_settled` admits one turn at a time, so one slot
/// is the whole concurrency surface.
static VENDOR_ACTIVE_CHILD: std::sync::Mutex<
    Option<std::sync::Arc<std::sync::Mutex<std::process::Child>>>,
> = std::sync::Mutex::new(None);

struct VendorToolBridge {
    #[cfg(windows)]
    _pipe: rightkit_agents::vendor_mcp_pipe::VendorMcpPipe,
    events: tokio::task::JoinHandle<()>,
    live: std::sync::Arc<crate::api::LiveSession>,
    queue: Option<VendorEventQueue>,
}

impl Drop for VendorToolBridge {
    fn drop(&mut self) {
        self.events.abort();
        // A vendor exit or cancellation must not strand an unanswered prompt.
        let mut pending = self
            .live
            .pending_approvals
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for (request_id, sender) in pending.drain() {
            let _ = sender.send(false);
            if let Some(queue) = &self.queue {
                vendor_queue_push(
                    queue,
                    DaemonEvent::ApprovalResolved {
                        request_id,
                        approved: false,
                    },
                );
            }
        }
    }
}

async fn start_vendor_tool_bridge(
    kind: VendorCli,
    workspace: &std::path::Path,
    session_id: &str,
    mode: rightkit_agent_policy::PermissionMode,
    control: &std::sync::Arc<RunCancellation>,
    queue: Option<VendorEventQueue>,
    spec: &mut rightkit_agents::driver_host::LaunchSpec,
) -> Result<Option<VendorToolBridge>, String> {
    if kind == VendorCli::Antigravity {
        let current_exe = std::env::current_exe().map_err(|error| error.to_string())?;
        agy_harness::ensure_coderight_mcp_registered(&spec.program, &current_exe)
            .map_err(|error| format!("Antigravity MCP registration failed: {error}"))?;
        return Ok(None);
    }
    #[cfg(not(windows))]
    return Err("Claude approval transport is currently available on Windows".into());
    #[cfg(windows)]
    {
        let (projection, mut stream) =
            InlineMcpProjection::for_vendor(workspace, session_id, mode, control).await?;
        let live = projection
            .state
            .get_session(session_id)
            .ok_or("vendor session unavailable")?;
        let server =
            crate::mcp_server::McpStdioServer::for_vendor(workspace.to_path_buf(), projection)
                .await?;
        let pipe = rightkit_agents::vendor_mcp_pipe::VendorMcpPipe::start(server)?;
        pipe.attach(spec);
        let control = control.clone();
        let session_id = session_id.to_string();
        let cleanup_queue = queue.clone();
        let events = tokio::spawn(async move {
            while let Ok(Some(event)) = stream.next_event().await {
                let Some(event) = interactive_session::daemon_event_to_timeline_event(&event)
                else {
                    continue;
                };
                if let Some(queue) = &queue {
                    if matches!(
                        &event,
                        DaemonEvent::ToolApprovalRequested { .. }
                            | DaemonEvent::ApprovalResolved { .. }
                            | DaemonEvent::UserQuestionAsked { .. }
                            | DaemonEvent::UserQuestionAnswered { .. }
                            | DaemonEvent::PlanPresented { .. }
                    ) {
                        vendor_queue_push(queue, event);
                    }
                } else if let DaemonEvent::ToolApprovalRequested { request_id, .. } = event {
                    // Print mode has no interactive approval surface; fail closed promptly.
                    if let Some((host, _)) = control.host.get() {
                        let _ = host.resolve_approval(&session_id, &request_id, false).await;
                    }
                }
            }
        });
        Ok(Some(VendorToolBridge {
            _pipe: pipe,
            events,
            live,
            queue: cleanup_queue,
        }))
    }
}

/// Clears `VENDOR_ACTIVE_CHILD` when the blocking reader exits on any path.
struct ActiveChildGuard;

impl Drop for ActiveChildGuard {
    fn drop(&mut self) {
        if let Ok(mut slot) = VENDOR_ACTIVE_CHILD.lock() {
            *slot = None;
        }
    }
}

/// Headless vendor turn for `coderight run --provider <vendor>`: same child
/// machinery, events projected to stdout/stderr instead of the TUI queue.
async fn run_vendor_turn_headless(
    args: &RunArgs,
    kind: VendorCli,
    model: Option<String>,
    session_id: String,
    workspace_root: String,
    control: std::sync::Arc<RunCancellation>,
) -> i32 {
    control
        .admission_started
        .store(true, std::sync::atomic::Ordering::Release);
    let status = detect(kind);
    if !status.installed {
        let message = format!(
            "{} is not installed — {}.",
            kind.display_name(),
            kind.install_hint()
        );
        eprintln!("{message}");
        control.settled();
        return 1;
    }
    let executable = status.path.expect("installed status carries a path");
    let workspace_path = std::path::PathBuf::from(&workspace_root);
    let conversations: VendorConversationMap = Default::default();
    let conversation_key = format!("{}:{}", kind.provider_id(), session_id);
    let permission_mode = args
        .permission_mode
        .unwrap_or(rightkit_agent_policy::PermissionMode::Ask);
    let account = if kind == VendorCli::ClaudeCode {
        match resolve_claude_account(&session_id, &workspace_root) {
            Ok(account) => account,
            Err(message) => {
                eprintln!("{message}");
                control.settled();
                return 1;
            }
        }
    } else {
        None
    };
    let (mut spec, stdin_frame) = match vendor_launch_spec(
        kind,
        &executable,
        &workspace_path,
        model.as_deref(),
        None,
        permission_mode,
        &args.prompt,
        account.as_ref().map(|resolved| &resolved.launch),
    ) {
        Ok(launch) => launch,
        Err(message) => {
            eprintln!("{message}");
            control.settled();
            return 1;
        }
    };
    // Forward the run flags `validate_vendor_cli_options` admitted for this vendor.
    match kind {
        VendorCli::ClaudeCode => rightkit_agents::driver_host::ClaudeRunFlags {
            max_turns: args.max_turns,
            max_budget_usd: args.max_budget_usd,
            system_prompt: args.system_prompt.clone(),
            system_prompt_file: args.system_prompt_file.clone(),
            append_system_prompt: args.append_system_prompt.clone(),
            append_system_prompt_file: args.append_system_prompt_file.clone(),
            mcp_config: args.mcp_config.clone(),
            add_dirs: args.add_dir.clone(),
        }
        .apply(&mut spec.args),
        VendorCli::Antigravity => {
            for dir in &args.add_dir {
                spec.args.extend(["--add-dir".into(), dir.clone()]);
            }
        }
    }
    let _bridge = match start_vendor_tool_bridge(
        kind,
        &workspace_path,
        &session_id,
        permission_mode,
        &control,
        None,
        &mut spec,
    )
    .await
    {
        Ok(bridge) => bridge,
        Err(error) => {
            eprintln!("{error}");
            control.settled();
            return 1;
        }
    };
    let cancel = control.requested.clone();
    let mut handle = tokio::task::spawn_blocking(move || {
        vendor_turn_blocking(
            kind,
            spec,
            stdin_frame,
            conversations,
            conversation_key,
            account.map(|resolved| resolved.identity),
            Box::new(|event| {
                use std::io::Write;
                match event {
                    DaemonEvent::AssistantDelta { token } => {
                        let mut out = std::io::stdout().lock();
                        let _ = out.write_all(token.as_bytes());
                        let _ = out.flush();
                    }
                    DaemonEvent::AssistantSettled => {
                        let mut out = std::io::stdout().lock();
                        let _ = out.write_all(b"\n");
                        let _ = out.flush();
                    }
                    DaemonEvent::System { text } => {
                        eprintln!("{text}");
                    }
                    _ => {}
                }
            }),
        )
    });
    let code = tokio::select! {
        result = &mut handle => result.unwrap_or(1),
        _ = cancel.cancelled() => kill_watched(&mut handle).await,
    };
    if cancel.is_cancelled() {
        control.settled();
        return 130;
    }
    control.settled();
    code
}

#[cfg(test)]
mod vendor_turn_tests {
    use super::*;

    #[test]
    fn claude_code_route_selects_the_claude_vendor_harness() {
        let parsed = parse_vendor_route("claude-code/sonnet");
        assert_eq!(parsed, Some((VendorCli::ClaudeCode, "sonnet".to_string())));
        let parsed = parse_vendor_route("/claude-code/opus");
        assert_eq!(parsed, Some((VendorCli::ClaudeCode, "opus".to_string())));
    }

    #[test]
    fn antigravity_route_selects_the_agy_vendor_harness() {
        let parsed = parse_vendor_route("antigravity/gemini-3.8-flash-high");
        assert_eq!(
            parsed,
            Some((VendorCli::Antigravity, "gemini-3.8-flash-high".to_string()))
        );
    }

    #[test]
    fn non_vendor_and_malformed_routes_are_not_dispatched() {
        assert_eq!(parse_vendor_route("openai/gpt-5"), None);
        assert_eq!(parse_vendor_route("claude-code"), None);
        assert_eq!(parse_vendor_route("claude-code/"), None);
        assert_eq!(parse_vendor_route(""), None);
        assert_eq!(
            VendorCli::from_provider_id("anthropic"),
            None,
            "the API provider id is not the vendor CLI route id"
        );
    }

    #[test]
    fn vendor_route_for_args_dispatches_on_provider_id() {
        let mut args = run_session_unit_tests::output_mode_args();
        args.provider = Some("claude-code".to_string());
        args.model = Some("sonnet".to_string());
        assert_eq!(
            vendor_route_for_args(&args),
            Some((VendorCli::ClaudeCode, Some("sonnet".to_string())))
        );
        args.provider = Some("antigravity".to_string());
        args.model = None;
        assert_eq!(
            vendor_route_for_args(&args),
            Some((VendorCli::Antigravity, None))
        );
        args.provider = Some("openai".to_string());
        assert_eq!(vendor_route_for_args(&args), None);
    }

    #[test]
    fn only_yolo_disables_antigravity_permission_checks() {
        use rightkit_agent_policy::PermissionMode;
        for mode in [
            PermissionMode::Plan,
            PermissionMode::Ask,
            PermissionMode::AcceptEdits,
            PermissionMode::AutoSafe,
            PermissionMode::Bypass,
        ] {
            let (spec, _) = vendor_launch_spec(
                VendorCli::Antigravity,
                std::path::Path::new("agy.exe"),
                std::path::Path::new("D:/work"),
                None,
                None,
                mode,
                "fixture",
                None,
            )
            .unwrap();
            assert_eq!(
                spec.args
                    .iter()
                    .any(|arg| arg == "--dangerously-skip-permissions"),
                mode == PermissionMode::Bypass
            );
        }
    }

    #[test]
    fn mcp_args_add_allow_mutate_only_when_mode_allows() {
        let workspace = std::path::Path::new("D:\\work");
        let read_only = vendor_mcp_args(workspace, false);
        assert_eq!(read_only, ["mcp-server", "--workspace", "D:\\work"]);
        let mutate = vendor_mcp_args(workspace, true);
        assert!(mutate.iter().any(|arg| arg == "--allow-mutate"));
    }

    #[test]
    fn claude_launch_preserves_delegation_policy_and_mcp_config() {
        let workspace = std::path::Path::new("D:\\work");
        let (spec, stdin_frame) = vendor_launch_spec(
            VendorCli::ClaudeCode,
            std::path::Path::new("claude.exe"),
            workspace,
            Some("sonnet"),
            Some("session-9"),
            rightkit_agent_policy::PermissionMode::Ask,
            "hello",
            None,
        )
        .expect("claude launch builds");
        for expected in [
            "--print",
            "--input-format",
            "--output-format",
            "--strict-mcp-config",
            "--permission-prompt-tool",
            "--resume",
            "--model",
        ] {
            assert!(spec.args.iter().any(|arg| arg == expected), "{expected}");
        }
        assert!(spec
            .args
            .windows(2)
            .any(|pair| pair == ["--model", "sonnet"]));
        assert!(spec
            .args
            .windows(2)
            .any(|pair| pair == ["--resume", "session-9"]));
        let config = spec
            .args
            .windows(2)
            .find(|pair| pair[0] == "--mcp-config")
            .map(|pair| pair[1].clone())
            .expect("mcp config flag present");
        let config: serde_json::Value = serde_json::from_str(&config).unwrap();
        let args = config["mcpServers"]["coderight"]["args"]
            .as_array()
            .unwrap();
        assert!(args.iter().any(|arg| arg == "mcp-server"));
        assert!(!args.iter().any(|arg| arg == "--allow-mutate"));
        // Claude takes its prompt as a stream-json stdin frame.
        let frame: serde_json::Value = serde_json::from_str(stdin_frame.unwrap().trim()).unwrap();
        assert_eq!(frame["type"], "user");
        assert_eq!(
            frame["message"]["content"][0]["text"],
            serde_json::json!("hello")
        );
    }

    #[test]
    fn claude_launch_maps_policy_mode_to_one_permission_mode_flag() {
        use rightkit_agent_policy::PermissionMode;
        for (mode, expected) in [
            (PermissionMode::Plan, "plan"),
            (PermissionMode::Ask, "default"),
            (PermissionMode::AcceptEdits, "acceptEdits"),
            (PermissionMode::AutoSafe, "default"),
            // bypassPermissions is refused by the driver host, so Bypass is never forwarded.
            (PermissionMode::Bypass, "default"),
        ] {
            let (spec, _) = vendor_launch_spec(
                VendorCli::ClaudeCode,
                std::path::Path::new("claude.exe"),
                std::path::Path::new("D:\\work"),
                None,
                None,
                mode,
                "hello",
                None,
            )
            .expect("claude launch builds");
            let flags: Vec<_> = spec
                .args
                .windows(2)
                .filter(|pair| pair[0] == "--permission-mode")
                .map(|pair| pair[1].clone())
                .collect();
            assert_eq!(flags, [expected], "{mode:?}");
            assert!(!spec.args.iter().any(|arg| arg == "bypassPermissions"));
            assert!(spec.args.windows(2).any(|pair| pair
                == [
                    "--permission-prompt-tool",
                    "mcp__coderight__permission_prompt"
                ]));
        }
    }

    #[test]
    fn run_flags_override_launch_extras_without_duplicating_flags() {
        let (mut spec, _) = vendor_launch_spec(
            VendorCli::ClaudeCode,
            std::path::Path::new("claude.exe"),
            std::path::Path::new("D:\\work"),
            None,
            None,
            rightkit_agent_policy::PermissionMode::Ask,
            "hello",
            None,
        )
        .unwrap();
        rightkit_agents::driver_host::ClaudeRunFlags {
            max_turns: Some(3),
            max_budget_usd: Some(1.5),
            ..Default::default()
        }
        .apply(&mut spec.args);
        for flag in ["--max-turns", "--max-budget-usd"] {
            assert_eq!(
                spec.args.iter().filter(|arg| *arg == flag).count(),
                1,
                "{flag}"
            );
        }
        assert!(spec
            .args
            .windows(2)
            .any(|pair| pair == ["--max-turns", "3"]));
    }

    #[test]
    fn sibling_tool_calls_from_one_assistant_line_are_all_projected() {
        let line = r#"{"type":"assistant","uuid":"u1","parent_tool_use_id":"toolu_parent","message":{"content":[{"type":"tool_use","id":"toolu_a","name":"mcp__coderight__a","input":{"n":1}},{"type":"tool_use","id":"toolu_b","name":"mcp__coderight__b","input":{"n":2}}]}}"#;
        let events = parse_claude_stream_events(line, 1, None).unwrap();
        // Keys must differ or the inbox dedupe would drop the second sibling.
        let keys: std::collections::BTreeSet<_> =
            events.iter().map(|event| event.event_key.clone()).collect();
        assert_eq!(keys.len(), events.len());
        let conversations: VendorConversationMap = Default::default();
        let mut started = Vec::new();
        let mut saw_delta = false;
        for event in &events {
            project_vendor_event(
                event,
                &mut |daemon_event| {
                    if let DaemonEvent::ToolStarted { call_id, .. } = daemon_event {
                        started.push(call_id);
                    }
                },
                &conversations,
                "claude:s",
                &mut saw_delta,
            );
        }
        assert_eq!(started, ["toolu_a", "toolu_b"]);
    }

    #[test]
    fn agy_launch_maps_bypass_and_carries_prompt_on_argv() {
        let workspace = std::path::Path::new("D:\\work");
        let (spec, stdin_frame) = vendor_launch_spec(
            VendorCli::Antigravity,
            std::path::Path::new("agy.exe"),
            workspace,
            Some("gemini-3.8-flash-high"),
            Some("conv-7"),
            rightkit_agent_policy::PermissionMode::Bypass,
            "fix the typo",
            None,
        )
        .expect("agy launch builds");
        assert!(spec
            .args
            .iter()
            .any(|arg| arg == "--dangerously-skip-permissions"));
        assert!(spec
            .args
            .windows(2)
            .any(|pair| pair == ["--conversation", "conv-7"]));
        assert!(spec
            .args
            .windows(2)
            .any(|pair| pair == ["-p", "fix the typo"]));
        assert!(stdin_frame.is_none());
    }

    #[test]
    fn missing_vendor_binary_fails_with_install_hint() {
        // `detect` is the single probe; a vendor that resolves to nothing
        // must surface "<display> is not installed — <install_hint>".
        for kind in [VendorCli::ClaudeCode, VendorCli::Antigravity] {
            let message = format!(
                "{} is not installed — {}.",
                kind.display_name(),
                kind.install_hint()
            );
            assert!(message.contains("is not installed"), "{message}");
            assert!(
                message.contains(kind.install_hint()),
                "message must carry the vendor's install hint: {message}"
            );
        }
    }
}

#[cfg(test)]
mod account_launch_tests {
    use super::*;

    fn identity(email: &str) -> ClaudeAccountIdentity {
        ClaudeAccountIdentity {
            provider_id: "claude".into(),
            email: Some(email.into()),
            ..Default::default()
        }
    }

    #[test]
    fn subscription_selection_resolves_to_profile_home_without_touching_keys() {
        let resolved = claude_account_for_selection(
            "subscription/claude/work",
            |_| panic!("subscription must not read the key store"),
            |label| {
                assert_eq!(label, "work");
                Ok(Some(("/profiles/claude/work".into(), identity("w@x.io"))))
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            resolved.launch,
            ClaudeAccountLaunch::Subscription {
                managed_home: "/profiles/claude/work".into()
            }
        );
        assert_eq!(resolved.identity.email.as_deref(), Some("w@x.io"));
    }

    #[test]
    fn api_key_selection_reads_the_stored_key_for_that_account_only() {
        let resolved = claude_account_for_selection(
            "api_key/claude/team",
            |id| {
                assert_eq!(id, "api_key/claude/team");
                Ok(Some("sk-ant-test".into()))
            },
            |_| panic!("api key must not read profiles"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            resolved.launch,
            ClaudeAccountLaunch::ApiKey {
                key: "sk-ant-test".into()
            }
        );
    }

    #[test]
    fn api_key_identity_has_label_and_kind_but_no_email() {
        let mut resolved = claude_account_for_selection(
            "api_key/claude/team",
            |_| Ok(Some("sk-ant-test".into())),
            |_| panic!("api key must not read profiles"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(resolved.identity.email, None);
        assert_eq!(resolved.identity.kind.as_deref(), Some("api_key"));
        assert_eq!(
            resolved.identity.account_id.as_deref(),
            Some("api_key/claude/team")
        );
        let dir = tempfile::tempdir().unwrap();
        let store = rightkit_app_settings::agent_config::accounts::AccountStore::at(dir.path().join("accounts.json"));
        let entry = store.add_api_key_account("Team", "claude").unwrap();
        assert_eq!(entry.id, "api_key/claude/team");
        apply_api_key_label(&mut resolved.identity, &entry.id, &store);
        assert_eq!(resolved.identity.label.as_deref(), Some("Team"));
        assert_eq!(resolved.identity.email, None);
    }

    #[test]
    fn codex_selection_mirrors_claude_and_never_crosses_providers() {
        let sub = account_for_selection(
            "codex",
            "subscription/codex/personal",
            |_| panic!("subscription must not read the key store"),
            |label| {
                assert_eq!(label, "personal");
                Ok(Some((
                    "/profiles/codex/personal".into(),
                    identity("c@x.io"),
                )))
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            sub.launch,
            ClaudeAccountLaunch::Subscription {
                managed_home: "/profiles/codex/personal".into()
            }
        );
        let key = account_for_selection(
            "codex",
            "api_key/codex/team",
            |id| {
                assert_eq!(id, "api_key/codex/team");
                Ok(Some("sk-test".into()))
            },
            |_| panic!("api key must not read profiles"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(key.identity.provider_id, "codex");
        assert_eq!(key.identity.kind.as_deref(), Some("api_key"));
        assert_eq!(key.identity.email, None);
        // A Claude selection is ignored by Codex and vice versa; no fallback account.
        assert!(account_for_selection(
            "codex",
            "subscription/claude/work",
            |_| Ok(None),
            |_| Ok(None)
        )
        .unwrap()
        .is_none());
        assert!(claude_account_for_selection(
            "subscription/codex/personal",
            |_| Ok(None),
            |_| Ok(None)
        )
        .unwrap()
        .is_none());
        assert!(account_for_selection(
            "codex",
            "subscription/codex/ghost",
            |_| Ok(None),
            |_| Ok(None)
        )
        .is_err());
        assert!(
            account_for_selection("codex", "api_key/codex/x", |_| Ok(None), |_| Ok(None)).is_err()
        );
    }

    #[test]
    fn missing_credentials_fail_closed_and_other_providers_are_ignored() {
        assert!(
            claude_account_for_selection("api_key/claude/x", |_| Ok(None), |_| Ok(None)).is_err()
        );
        assert!(
            claude_account_for_selection("subscription/claude/x", |_| Ok(None), |_| Ok(None))
                .is_err()
        );
        let err = claude_account_for_selection(
            "api_key/claude/x",
            |_| Err("keyring locked".into()),
            |_| Ok(None),
        )
        .err()
        .unwrap();
        assert!(!err.contains("sk-"));
        assert!(
            claude_account_for_selection("api_key/openai/x", |_| Ok(None), |_| Ok(None))
                .unwrap()
                .is_none()
        );
        assert!(claude_account_for_selection("bogus", |_| Ok(None), |_| Ok(None)).is_err());
    }

    fn resolved(launch: ClaudeAccountLaunch, kind: &str) -> ResolvedClaudeAccount {
        ResolvedClaudeAccount {
            launch,
            identity: ClaudeAccountIdentity {
                provider_id: "claude".into(),
                label: Some("Work".into()),
                kind: Some(kind.into()),
                account_id: Some(format!("{kind}/claude/work")),
                ..Default::default()
            },
        }
    }

    fn blank_spec() -> rightkit_agents::driver_host::LaunchSpec {
        rightkit_agents::driver_host::LaunchSpec {
            program: "claude".into(),
            args: vec![],
            env: Default::default(),
            env_remove: Default::default(),
            cwd: ".".into(),
        }
    }

    #[test]
    fn guard_strips_every_ambient_credential_under_a_managed_profile() {
        let mut spec = blank_spec();
        spec.env.insert("ANTHROPIC_AUTH_TOKEN".into(), "x".into());
        let launch = ClaudeAccountLaunch::Subscription {
            managed_home: "/h".into(),
        };
        let stripped =
            apply_credential_precedence_guard(&mut spec, "claude", Some(&launch), &|n| {
                n == "ANTHROPIC_API_KEY"
            });
        assert_eq!(stripped, vec!["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"]);
        for name in CLAUDE_CREDENTIAL_ENV {
            assert!(spec.env_remove.contains(name), "{name}");
            assert!(!spec.env.contains_key(name), "{name}");
        }
    }

    #[test]
    fn guard_keeps_the_api_key_of_an_api_key_account_and_ignores_no_account() {
        let mut spec = blank_spec();
        spec.env.insert("ANTHROPIC_API_KEY".into(), "sk-own".into());
        let launch = ClaudeAccountLaunch::ApiKey {
            key: "sk-own".into(),
        };
        apply_credential_precedence_guard(&mut spec, "claude", Some(&launch), &|_| true);
        assert!(spec.env.contains_key("ANTHROPIC_API_KEY"));
        assert!(spec.env_remove.contains("ANTHROPIC_AUTH_TOKEN"));
        assert!(!spec.env_remove.contains("ANTHROPIC_API_KEY"));
        let mut untouched = blank_spec();
        assert!(
            apply_credential_precedence_guard(&mut untouched, "claude", None, &|_| true).is_empty()
        );
        assert!(untouched.env_remove.is_empty());
    }

    #[test]
    fn codex_guard_strips_openai_key_only_for_subscription_accounts() {
        let sub = ClaudeAccountLaunch::Subscription {
            managed_home: "/h".into(),
        };
        let mut spec = blank_spec();
        let stripped = apply_credential_precedence_guard(&mut spec, "codex", Some(&sub), &|_| true);
        assert_eq!(stripped, vec!["OPENAI_API_KEY"]);
        assert!(spec.env_remove.contains("OPENAI_API_KEY"));
        let key = ClaudeAccountLaunch::ApiKey { key: "sk".into() };
        let mut keyed = blank_spec();
        apply_credential_precedence_guard(&mut keyed, "codex", Some(&key), &|_| true);
        assert!(keyed.env_remove.is_empty());
    }

    #[test]
    fn auth_report_warns_when_api_key_env_would_override_the_subscription() {
        let report = auth_method_report("claude", None, &|n| n == "ANTHROPIC_API_KEY");
        assert_eq!(report.method, "api_key_env");
        assert!(report.warnings[0].contains("ANTHROPIC_API_KEY"));
        let clean = auth_method_report("claude", None, &|_| false);
        assert_eq!(clean.method, "ambient_login");
        assert!(clean.warnings.is_empty());
    }

    #[test]
    fn auth_report_under_a_managed_profile_flags_stripped_vars_without_warning() {
        let account = resolved(
            ClaudeAccountLaunch::Subscription {
                managed_home: "/h".into(),
            },
            "subscription",
        );
        let report = auth_method_report("claude", Some(&account), &|n| n == "ANTHROPIC_API_KEY");
        assert_eq!(report.method, "subscription");
        assert_eq!(report.stripped_env, vec!["ANTHROPIC_API_KEY"]);
        assert!(report.warnings.is_empty());
        assert!(report
            .render_line()
            .contains("removed from the child environment"));
        let json = report.to_json();
        assert_eq!(json["method"], "subscription");
        assert_eq!(json["accountId"], "subscription/claude/work");
        let key = resolved(
            ClaudeAccountLaunch::ApiKey {
                key: "sk-SECRET".into(),
            },
            "api_key",
        );
        let report = auth_method_report("claude", Some(&key), &|_| false);
        assert_eq!(report.method, "api_key");
        assert!(!report.to_json().to_string().contains("SECRET"));
    }
}
