#[derive(Clone)]
struct RunArgs {
    context_mode: ContextMode,
    prompt: String,
    print: bool,
    output_format: OutputFormat,
    json_event: bool,
    model: Option<String>,
    /// `None` means "no explicit provider flag": the daemon resolves the
    /// configured default/provider routes. Must stay distinguishable from an
    /// explicit `--provider` value so a bare run never pins a provider.
    provider: Option<String>,
    effort: Option<String>,
    workspace: Option<String>,
    auto_approve: bool,
    bypass_approvals: bool,
    /// Explicit session permission mode (launch flags `--ask` /
    /// `--accept-edits` / `--auto` / `--yolo`, `/yolo`, Shift+Tab). When set it
    /// supersedes `auto_approve` / `bypass_approvals` for the session's mode
    /// (safe mode still wins). This is the field turn dispatch reads for the
    /// live mode (e.g. vendor-CLI runtimes mapping yolo to skip-permissions).
    permission_mode: Option<rightkit_agent_policy::PermissionMode>,
    stream_json: bool,
    exit_on_error: bool,
    max_turns: Option<u32>,
    allowed_tools: Vec<String>,
    disallowed_tools: Vec<String>,
    toolset: Option<String>,
    max_budget_usd: Option<f64>,
    bare: bool,
    /// Strict policy / permission tier for the whole run. When true the
    /// session engine runs in safe-mode (Untrusted trust, no permission
    /// rules, read-only-capable tool registry) regardless of --bypass or
    /// --auto-approve. See CLI --safe-mode doc in cli_definitions_command.rs.
    safe_mode: bool,
    system_prompt: Option<String>,
    system_prompt_file: Option<String>,
    append_system_prompt: Option<String>,
    append_system_prompt_file: Option<String>,
    mcp_config: Option<String>,
    session_id: Option<String>,
    add_dir: Vec<String>,
    /// Prompt input format. `text` (default) reads the positional prompt;
    /// `stream-json` reads JSONL events from stdin and drives the run from
    /// them (FEATURE-MATRIX par §#32 + #35).
    input_format: InputFormat,
    output_schema: Option<String>,
    image: Vec<String>,
    ephemeral: bool,
    output_last_message: Option<String>,
    color: crate::cli::CliColor,
    tools: Vec<String>,
    exclude_tools: Vec<String>,
    no_mcp: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CliExitCode {
    #[allow(dead_code)]
    Success = 0,
    Generic = 1,
    Usage = 2,
    Auth = 3,
    Permission = 4,
    Budget = 5,
    Sandbox = 6,
    TimeoutStall = 7,
    UserAbort = 8,
    SchemaViolation = 9,
}

impl CliExitCode {
    fn as_i32(self) -> i32 {
        self as i32
    }

    fn class(self) -> &'static str {
        match self {
            CliExitCode::Success => "success",
            CliExitCode::Generic => "generic",
            CliExitCode::Usage => "usage",
            CliExitCode::Auth => "auth",
            CliExitCode::Permission => "permission",
            CliExitCode::Budget => "budget",
            CliExitCode::Sandbox => "sandbox",
            CliExitCode::TimeoutStall => "timeout_stall",
            CliExitCode::UserAbort => "user_abort",
            CliExitCode::SchemaViolation => "schema_violation",
        }
    }
}

type CliToolFilters = (Option<String>, Vec<String>, Vec<String>);

/// Built-in tools that front MCP servers. `--no-mcp` and `--exclude-tools
/// 'mcp__*'` remove them; `--tools` keeps them unless it names `mcp__` tools.
const MCP_GATEWAY_TOOLS: [&str; 5] = [
    "mcp_call",
    "list_mcp_tools",
    "list_mcp_sources",
    "list_mcp_resources",
    "read_mcp_resource",
];

/// Non-matching allowlist entry that keeps a fully-excluded allowlist
/// non-empty (an empty allowlist would mean "allow everything").
const CLI_NO_TOOLS_SENTINEL: &str = "__host_no_tools__";

fn resolve_cli_tool_filters(
    toolset: Option<&str>,
    allowed_tools: Vec<String>,
    disallowed_tools: Vec<String>,
) -> Result<CliToolFilters, String> {
    resolve_cli_tool_filters_with_mcp(toolset, allowed_tools, disallowed_tools, false)
}

/// Resolve `--tools` / `--exclude-tools` / `--no-mcp` into the canonical
/// allow and deny lists consumed by the session registry.
///
/// Semantics are pinned to pi's CLI reference
/// <https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/docs/cli.md>:
/// entries are tool names or patterns where `*` matches any characters;
/// `--tools` is an allowlist that keeps MCP tools unless an entry starts with
/// `mcp__`; `--exclude-tools` applies after selection and removes MCP tools
/// too; `--no-mcp` disables MCP. A list whose entries all name nothing known
/// is a usage error. Qualified `mcp__server__tool` entries are discovered at
/// runtime, so they pass through verbatim for the MCP manager to match.
fn resolve_cli_tool_filters_with_mcp(
    toolset: Option<&str>,
    allowed_tools: Vec<String>,
    disallowed_tools: Vec<String>,
    no_mcp: bool,
) -> Result<CliToolFilters, String> {
    let catalog = rightkit_agent_tools::coding_tools::registry::default_tool_capabilities()
        .into_iter()
        .map(|capability| capability.name)
        .collect::<Vec<_>>();
    let mut allowed_tools = expand_cli_tool_globs("--tools", allowed_tools, &catalog)?;
    let mut disallowed_tools =
        expand_cli_tool_globs("--exclude-tools", disallowed_tools, &catalog)?;
    // Pi keeps MCP tools under `--tools`; the gateway tools are how this
    // runtime reaches them, so they stay in a non-empty allowlist.
    if !allowed_tools.is_empty() {
        for gateway in MCP_GATEWAY_TOOLS {
            if !allowed_tools.iter().any(|name| name == gateway) {
                allowed_tools.push(gateway.to_string());
            }
        }
    }
    if no_mcp
        || disallowed_tools
            .iter()
            .any(|name| cli_tool_glob_covers_all_mcp(name))
    {
        for gateway in MCP_GATEWAY_TOOLS {
            if !disallowed_tools.iter().any(|name| name == gateway) {
                disallowed_tools.push(gateway.to_string());
            }
        }
    }
    // Exclusion wins over inclusion: drop every excluded name from the
    // allowlist. An empty allowlist means "allow everything", so if this
    // empties a non-empty allowlist, keep a placeholder that names no real
    // tool; the allowlist stays non-empty and still denies everything.
    if !allowed_tools.is_empty() {
        allowed_tools.retain(|name| {
            !disallowed_tools.iter().any(|excluded| {
                excluded == name
                    || (excluded.contains('*') && cli_tool_glob_matches(excluded, name))
            })
        });
        if allowed_tools.is_empty() {
            allowed_tools.push(CLI_NO_TOOLS_SENTINEL.to_string());
        }
    }
    let profile = match toolset.map(str::trim).filter(|value| !value.is_empty()) {
        Some(id) => Some(
            api_toolsets::resolve_toolset_profile(id)
                .map_err(|_| format!("Unknown toolset '{id}'."))?,
        ),
        None => None,
    };
    let active_toolset = profile.as_ref().map(|profile| profile.id.clone());
    let (allowed_tools, disallowed_tools) =
        api_toolsets::apply_toolset_profile(profile.as_ref(), allowed_tools, disallowed_tools);
    Ok((active_toolset, allowed_tools, disallowed_tools))
}

/// True for an exclude pattern that covers every MCP tool name (`mcp__*`,
/// `mcp_*`, `*`), so the MCP gateway tools must go with it.
fn cli_tool_glob_covers_all_mcp(pattern: &str) -> bool {
    pattern.contains('*')
        && cli_tool_glob_matches(pattern, "mcp__x__y")
        && cli_tool_glob_matches(pattern, "mcp__")
}

fn expand_cli_tool_globs(
    flag: &str,
    patterns: Vec<String>,
    catalog: &[String],
) -> Result<Vec<String>, String> {
    let mut expanded = Vec::new();
    let mut unknown = Vec::new();
    let mut resolved_any = false;
    let mut seen_any = false;
    for pattern in patterns {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            continue;
        }
        seen_any = true;
        let is_mcp = pattern.starts_with("mcp__");
        if pattern.contains('*') {
            let matches = catalog
                .iter()
                .filter(|name| cli_tool_glob_matches(pattern, name))
                .cloned()
                .collect::<Vec<_>>();
            if is_mcp {
                // Discovered at runtime; the MCP manager matches the pattern.
                expanded.push(pattern.to_string());
                resolved_any = true;
            } else if matches.is_empty() {
                unknown.push(pattern.to_string());
            } else {
                resolved_any = true;
                expanded.extend(matches);
            }
        } else if is_mcp || catalog.iter().any(|name| name == pattern) {
            resolved_any = true;
            expanded.push(pattern.to_string());
        } else {
            unknown.push(pattern.to_string());
            // Exact names stay in the list: plugin tools register after
            // discovery, so they cannot be proven absent from the catalog.
            expanded.push(pattern.to_string());
        }
    }
    if seen_any && !resolved_any {
        return Err(format!(
            "{flag} matched no known tools: {}",
            unknown.join(", ")
        ));
    }
    expanded.sort();
    expanded.dedup();
    Ok(expanded)
}

fn cli_tool_glob_matches(pattern: &str, value: &str) -> bool {
    // Pi semantics: `*` is the only wildcard; every other character is literal.
    let pattern = pattern.chars().collect::<Vec<_>>();
    let value = value.chars().collect::<Vec<_>>();
    let mut previous = vec![false; value.len() + 1];
    previous[0] = true;
    for token in pattern {
        let mut current = vec![false; value.len() + 1];
        if token == '*' {
            current[0] = previous[0];
            for index in 1..=value.len() {
                current[index] = previous[index] || current[index - 1];
            }
        } else {
            for index in 1..=value.len() {
                current[index] = previous[index - 1] && token == value[index - 1];
            }
        }
        previous = current;
    }
    previous[value.len()]
}

fn safe_mode_workspace_trust(safe_mode: bool) -> rightkit_agent_policy::WorkspaceTrust {
    if safe_mode {
        rightkit_agent_policy::WorkspaceTrust::Untrusted
    } else {
        rightkit_agent_policy::WorkspaceTrust::Trusted
    }
}

fn cli_provider_route(
    api_key: String,
    model_id: String,
    base_url: impl Into<String>,
    fallback_models: Vec<String>,
) -> provider_client::ProviderRoute {
    let mut route = provider_client::ProviderRoute::from_model_auth(
        model_id,
        base_url,
        rightkit_llm::agent_providers::subscription_auth::ModelAuth::api_key(api_key),
    )
    .expect("CLI provider route must contain configured API auth and endpoint");
    route.effort = None;
    route.thinking_budget_tokens = None;
    route.disable_thinking = false;
    route.fallback_models = fallback_models;
    route.approved_private = false;
    route
}

#[derive(Clone)]
struct CliSubagentSpawner {
    workspace_root: std::path::PathBuf,
}

impl CliSubagentSpawner {
    fn new(workspace_root: std::path::PathBuf) -> Self {
        Self { workspace_root }
    }
}

const DEFAULT_CLI_SUBAGENT_TIMEOUT_SECONDS: u64 = 300;
const MIN_CLI_SUBAGENT_TIMEOUT_SECONDS: u64 = 60;
const MAX_CLI_SUBAGENT_TIMEOUT_SECONDS: u64 = 900;

fn cli_subagent_timeout(
    options: &rightkit_agent_tools::coding_tools::SubagentSpawnOptions,
) -> std::time::Duration {
    let env_seconds = std::env::var("RIGHTKIT_CLI_SUBAGENT_TIMEOUT_SECONDS").ok();
    std::time::Duration::from_secs(cli_subagent_timeout_seconds(
        options,
        env_seconds.as_deref(),
    ))
}

fn cli_subagent_timeout_seconds(
    options: &rightkit_agent_tools::coding_tools::SubagentSpawnOptions,
    env_seconds: Option<&str>,
) -> u64 {
    if let Some(raw) = env_seconds {
        if let Ok(seconds) = raw.trim().parse::<u64>() {
            if seconds > 0 {
                return seconds.clamp(
                    MIN_CLI_SUBAGENT_TIMEOUT_SECONDS,
                    MAX_CLI_SUBAGENT_TIMEOUT_SECONDS,
                );
            }
        }
    }

    options
        .max_iterations
        .map(|turns| u64::from(turns).saturating_mul(45))
        .unwrap_or(DEFAULT_CLI_SUBAGENT_TIMEOUT_SECONDS)
        .clamp(
            MIN_CLI_SUBAGENT_TIMEOUT_SECONDS,
            MAX_CLI_SUBAGENT_TIMEOUT_SECONDS,
        )
}

async fn read_cli_child_pipe<R>(mut pipe: R) -> Result<Vec<u8>, std::io::Error>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;

    let mut buffer = Vec::new();
    pipe.read_to_end(&mut buffer).await?;
    Ok(buffer)
}

async fn join_cli_child_pipe(
    handle: tokio::task::JoinHandle<Result<Vec<u8>, std::io::Error>>,
    label: &str,
) -> Result<Vec<u8>, String> {
    match handle.await {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(err)) => Err(format!("failed to read subagent {label}: {err}")),
        Err(err) => Err(format!("subagent {label} reader failed: {err}")),
    }
}

fn tail_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }

    let mut tail = value.chars().rev().take(max_chars).collect::<Vec<_>>();
    tail.reverse();
    format!("...{}", tail.into_iter().collect::<String>())
}

impl rightkit_agent_tools::coding_tools::SubagentSpawner for CliSubagentSpawner {
    fn spawn(
        &self,
        role: &str,
        task: &str,
        _parent_session_id: &str,
        options: rightkit_agent_tools::coding_tools::SubagentSpawnOptions,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> {
        let role = role.to_string();
        let task = task.to_string();
        let workspace_root = self.workspace_root.clone();
        Box::pin(async move {
            let mut registry =
                rightkit_agent_orchestration::coding::registry::AgentRegistry::with_builtins();
            let merged_config = load_merged_config_for_workspace(
                &workspace_root,
                &std::env::vars().collect::<std::collections::HashMap<_, _>>(),
            );
            // Provider is passed through verbatim: `apply_config_overrides`
            // derives the provider from the override's model when it is empty.
            // Pre-filling a default provider id here would force every
            // overridden agent onto a provider the user never configured.
            let agent_overrides: std::collections::HashMap<String, (String, String)> =
                merged_config
                    .orchestration
                    .agents
                    .iter()
                    .map(|(name, ov)| (name.clone(), (ov.model_id.clone(), ov.provider_id.clone())))
                    .collect();
            if !agent_overrides.is_empty() {
                registry.apply_config_overrides(&agent_overrides);
            }
            let descriptor = registry
                .get(&role)
                .or_else(|| registry.all().iter().find(|agent| agent.role == role))
                .ok_or_else(|| format!("unknown subagent role: {role}"))?
                .clone();
            let exe = std::env::current_exe()
                .map_err(|e| format!("failed to locate current executable: {e}"))?;
            let allowed_tools = {
                let mut names = descriptor.allowed_tools.clone();
                names.push("list_files".to_string());
                names.sort();
                names.dedup();
                names
                    .into_iter()
                    .filter(|name| name != "spawn_subagent")
                    .collect::<Vec<_>>()
                    .join(",")
            };
            let prompt = format!(
                "{}\n\nParent-assigned task:\n{}\n\nWorker constraints:\n- Read-only: do not write, patch, format, or refactor files.\n- Do not spawn further subagents.\n- Use tools silently; do not narrate what you are about to do.\n- Your final answer must be findings only: severity, path:line where possible, evidence, why it matters, and remediation.\n- If you cannot finish within the turn budget, return `INCOMPLETE` plus the evidence you actually checked. Do not return a progress preamble as the final answer.",
                descriptor.system_prompt, task
            );

            let mut argv = vec![
                "run".to_string(),
                "--model".to_string(),
                descriptor.model_id.clone(),
                "--provider".to_string(),
                descriptor.provider_id.clone(),
                "--workspace".to_string(),
                workspace_root.to_string_lossy().into_owned(),
                "--bypass-approvals".to_string(),
                "--stream-json".to_string(),
                "--allowed-tools".to_string(),
                allowed_tools,
                "--disallowed-tools".to_string(),
                "write_file,apply_patch,spawn_subagent".to_string(),
                "--pipe".to_string(),
            ];
            if let Some(max_turns) = options.max_iterations {
                argv.push("--max-turns".to_string());
                argv.push(max_turns.to_string());
            }

            use rightkit_sandbox::execution::{
                CommandLaunchSpec, ProcessLaunchAdapter, ProcessLaunchContext,
            };
            let adapter = rightkit_agent_tools::coding_tools::shell::NativeCommandSandboxAdapter;
            let mut child = adapter
                .spawn(CommandLaunchSpec {
                    program: exe.to_string_lossy().into_owned(),
                    argv,
                    cwd: workspace_root.to_string_lossy().into_owned(),
                    context: ProcessLaunchContext {
                        workspace_root: workspace_root.to_string_lossy().into_owned(),
                        sandbox_profile: Some("workspace".to_string()),
                        sandbox_required: true,
                        network_required: true,
                    },
                    env: Vec::new(),
                })
                .await
                .map_err(|e| format!("failed to spawn subagent process: {e}"))?;
            let stdout_pipe = child
                .take_stdout()
                .ok_or_else(|| "failed to capture subagent stdout".to_string())?;
            let stderr_pipe = child
                .take_stderr()
                .ok_or_else(|| "failed to capture subagent stderr".to_string())?;
            let stdout_task = tokio::spawn(read_cli_child_pipe(stdout_pipe));
            let stderr_task = tokio::spawn(read_cli_child_pipe(stderr_pipe));
            if let Some(mut stdin) = child.take_stdin() {
                use tokio::io::AsyncWriteExt;
                stdin
                    .write_all(prompt.as_bytes())
                    .await
                    .map_err(|e| format!("failed to write subagent prompt: {e}"))?;
            }

            let timeout_duration = cli_subagent_timeout(&options);
            let status = match tokio::time::timeout(timeout_duration, child.wait()).await {
                Ok(Ok(status)) => status,
                Ok(Err(err)) => {
                    return Err(format!("failed to wait for subagent process: {err}"));
                }
                Err(_) => {
                    let _ = child.cancel().await;
                    let _ =
                        tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await;
                    let stdout = join_cli_child_pipe(stdout_task, "stdout")
                        .await
                        .unwrap_or_default();
                    let stderr = join_cli_child_pipe(stderr_task, "stderr")
                        .await
                        .unwrap_or_default();
                    let stdout = String::from_utf8_lossy(&stdout);
                    let stderr = String::from_utf8_lossy(&stderr);
                    return Err(format!(
                        "subagent `{role}` timed out after {}s and was stopped. stdoutTail={}, stderrTail={}",
                        timeout_duration.as_secs(),
                        tail_chars(stdout.trim(), 2000),
                        tail_chars(stderr.trim(), 2000)
                    ));
                }
            };
            let stdout = join_cli_child_pipe(stdout_task, "stdout").await?;
            let stderr = join_cli_child_pipe(stderr_task, "stderr").await?;
            let stderr = String::from_utf8_lossy(&stderr);
            if status != 0 {
                let msg = stderr.trim();
                return Err(if msg.is_empty() {
                    format!("subagent process exited with status {status}")
                } else {
                    format!(
                        "subagent process exited with status {status}: {}",
                        tail_chars(msg, 4000)
                    )
                });
            }

            let stdout = String::from_utf8_lossy(&stdout);
            let mut visible = String::new();
            let mut terminal_seen = false;
            let mut prompt_tokens = 0u64;
            let mut completion_tokens = 0u64;
            for line in stdout.lines() {
                let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                let event_data = cli_stream_event_data(&event);
                match event_data.get("$kind").and_then(|value| value.as_str()) {
                    Some("assistantDelta")
                        if !event_data
                            .get("isReasoning")
                            .and_then(|value| value.as_bool())
                            .unwrap_or(false) =>
                    {
                        if let Some(text) = event_data.get("text").and_then(|value| value.as_str())
                        {
                            visible.push_str(text);
                        }
                    }
                    Some("assistantMessageCompleted") => terminal_seen = true,
                    Some("usageUpdated") => {
                        prompt_tokens = event_data
                            .get("promptTokens")
                            .and_then(|value| value.as_u64())
                            .unwrap_or(prompt_tokens);
                        completion_tokens = event_data
                            .get("completionTokens")
                            .and_then(|value| value.as_u64())
                            .unwrap_or(completion_tokens);
                    }
                    Some("runFailed") | Some("runInterrupted") => {
                        return Err(format!("subagent run did not complete: {line}"));
                    }
                    _ => {}
                }
            }
            let visible = visible.trim();
            if visible.is_empty() {
                return Err("subagent completed without visible output".to_string());
            }
            if !terminal_seen {
                return Err("subagent output ended without completion event".to_string());
            }
            let (prompt_per_1k, completion_per_1k) =
                rightkit_llm::agent_providers::price_per_1k(&descriptor.model_id);
            let estimated_cost_usd = (prompt_tokens as f64 * prompt_per_1k / 1000.0)
                + (completion_tokens as f64 * completion_per_1k / 1000.0);
            Ok(format!(
                "Subagent `{}` using `{}` completed.\nSubagent telemetry: promptTokens={}, completionTokens={}, estimatedCostUsd={:.6}\n\n{}",
                descriptor.name,
                descriptor.model_id,
                prompt_tokens,
                completion_tokens,
                estimated_cost_usd,
                visible
            ))
        })
    }
}
