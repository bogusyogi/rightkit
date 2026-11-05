//! Claude Code launch and stream adapter over `rightkit-agents` (`claude` feature).
//!
//! RightKit owns the argv builder (`build_args_with`), the `stream-json` line parser
//! (`StreamParser`, fail-closed on unknown required messages), the control and permission frames
//! and the typed init, result and rate-limit payloads. What stays here is the host app's own model:
//! the MCP gateway lockdown flags, the account/credential environment, the run flags and the
//! mapping of RightKit's `AgentEvent`s onto the host app's durable `NormalizedEvent` (stable event
//! keys, source offsets, `DriverHostError`).
use crate::claude as rk;
use crate::driver_host::cli_runtime::auth::FORBIDDEN_BYOK_ENV_VARS;
use crate::driver_host::{
    event_key, invalid, stable_event_key, DriverHostError, DriverKind, LaunchSpec, NormalizedEvent,
    NormalizedEventKind, RUN_SCOPED_POLICY,
};
use crate::event::{AgentEvent, ParseError, ResultSummary, SessionInit, UnknownPolicy};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeLaunchOptions {
    pub executable: PathBuf,
    pub cwd: PathBuf,
    pub mcp_config: String,
    pub session_id: Option<String>,
    pub resume_session_id: Option<String>,
    /// Optional caller-selected model: an alias (`sonnet`, `opus`, `haiku`, `fable`) or a full
    /// model name, appended as `--model <id>` per the Claude Code CLI reference. Validated by
    /// RightKit's argv builder before it is interpolated into the argv.
    pub model: Option<String>,
}

impl ClaudeLaunchOptions {
    pub fn launch_spec(&self) -> Result<LaunchSpec, DriverHostError> {
        self.launch_spec_with(&ClaudeLaunchExtras::default())
    }

    /// [`Self::launch_spec`] plus the optional session-fork and run-limit flags. With default
    /// extras the argv is identical to `launch_spec`.
    ///
    /// The argv is `crate::claude::build_args_with`, locked down for a the host app run:
    /// every native tool is disabled (`--tools ""`), only the `host app` MCP server's tools are
    /// allowed (`--allowedTools mcp__coderight`), the run-scoped policy is appended, and the
    /// strict MCP config below is the only MCP configuration the CLI may load.
    pub fn launch_spec_with(
        &self,
        extras: &ClaudeLaunchExtras,
    ) -> Result<LaunchSpec, DriverHostError> {
        let options = rk::ClaudeOptions {
            model: self.model.clone(),
            resume_session_id: self.resume_session_id.clone(),
            session_id: self.session_id.clone(),
            allowed_tools: vec!["mcp__rightkit".to_string()],
            permission_mode: extras.permission_mode.clone(),
            max_budget_usd: extras.max_budget_usd,
            append_system_prompt: Some(RUN_SCOPED_POLICY.to_string()),
            include_partial_messages: true,
            ..rk::ClaudeOptions::default()
        };
        let rk_extras = rk::ClaudeExtras {
            tools: Some(Vec::new()),
            no_chrome: true,
            fork_session: extras.fork_session,
            resume_session_at: extras.resume_session_at.clone(),
            max_turns: extras.max_turns,
            fallback_model: extras.fallback_model.clone(),
            ..rk::ClaudeExtras::default()
        };
        let mut args = rk::build_args_with(&options, &rk_extras)
            .map_err(|reason| invalid(DriverKind::Claude, reason))?;
        // RightKit's builder has no MCP-config field: the host app's gateway config is the only
        // MCP configuration the CLI loads.
        args.extend([
            "--strict-mcp-config".to_string(),
            "--mcp-config".to_string(),
            self.mcp_config.clone(),
        ]);
        Ok(LaunchSpec {
            program: self.executable.clone(),
            args,
            env: BTreeMap::new(),
            env_remove: FORBIDDEN_BYOK_ENV_VARS
                .iter()
                .map(|name| (*name).to_string())
                .collect(),
            cwd: self.cwd.clone(),
        })
    }
}

/// Optional launch flags beyond the frozen baseline argv (issues #331 and #339). Every field
/// defaults to "not set", which emits nothing, so existing launches are unchanged.
///
/// Mapping (Claude Code CLI reference):
/// - `fork_session` -> `--fork-session` (resume into a new session id; needs a resume id)
/// - `resume_session_at` -> `--resume-session-at <message uuid>` (needs a resume id)
/// - `max_turns` -> `--max-turns`
/// - `max_budget_usd` -> `--max-budget-usd`
/// - `permission_mode` -> `--permission-mode` (`bypassPermissions` is refused)
/// - `fallback_model` -> `--fallback-model` (must differ from the main model)
///
/// `ClaudeRunFlags::apply` also writes `--max-turns`/`--max-budget-usd`; it replaces an existing
/// value instead of emitting a second flag, so the two can be combined with the run flag winning.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClaudeLaunchExtras {
    pub fork_session: bool,
    pub resume_session_at: Option<String>,
    pub max_turns: Option<u32>,
    pub max_budget_usd: Option<f64>,
    pub permission_mode: Option<String>,
    pub fallback_model: Option<String>,
}

/// Set `flag value` in argv, replacing the value when the flag is already present.
fn set_flag_value(args: &mut Vec<String>, flag: &str, value: String) {
    if let Some(index) = args.iter().position(|arg| arg == flag) {
        if let Some(existing) = args.get_mut(index + 1) {
            *existing = value;
            return;
        }
    }
    args.extend([flag.to_string(), value]);
}

/// Per-run `host app run` flags forwarded to the Claude Code CLI's own flags.
///
/// Mapping (Claude Code CLI reference, <https://code.claude.com/docs/en/cli-reference>):
/// `--max-turns`, `--max-budget-usd` (print mode only), `--system-prompt`,
/// `--system-prompt-file`, `--append-system-prompt`, `--append-system-prompt-file`,
/// `--mcp-config` (extra servers; the the host app gateway config and
/// `--strict-mcp-config` stay in force) and `--add-dir`.
///
/// `RUN_SCOPED_POLICY` is never dropped: a replacement system prompt is passed through while the
/// policy stays appended, and caller append text is merged into the same
/// `--append-system-prompt` value rather than emitted as a second flag.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClaudeRunFlags {
    pub max_turns: Option<u32>,
    pub max_budget_usd: Option<f64>,
    pub system_prompt: Option<String>,
    pub system_prompt_file: Option<String>,
    pub append_system_prompt: Option<String>,
    pub append_system_prompt_file: Option<String>,
    /// File path or inline JSON, as accepted by `claude --mcp-config`.
    pub mcp_config: Option<String>,
    pub add_dirs: Vec<String>,
}

impl ClaudeRunFlags {
    /// Append the mapped flags to an argv built by [`ClaudeLaunchOptions::launch_spec`].
    pub fn apply(&self, args: &mut Vec<String>) {
        if let Some(turns) = self.max_turns {
            set_flag_value(args, "--max-turns", turns.to_string());
        }
        if let Some(budget) = self.max_budget_usd {
            set_flag_value(args, "--max-budget-usd", budget.to_string());
        }
        if let Some(text) = self.system_prompt.as_ref() {
            args.extend(["--system-prompt".into(), text.clone()]);
        }
        if let Some(path) = self.system_prompt_file.as_ref() {
            args.extend(["--system-prompt-file".into(), path.clone()]);
        }
        if let Some(text) = self.append_system_prompt.as_ref() {
            if let Some(index) = args.iter().position(|arg| arg == "--append-system-prompt") {
                if let Some(value) = args.get_mut(index + 1) {
                    value.push_str("\n\n");
                    value.push_str(text);
                }
            } else {
                args.extend(["--append-system-prompt".into(), text.clone()]);
            }
        }
        if let Some(path) = self.append_system_prompt_file.as_ref() {
            args.extend(["--append-system-prompt-file".into(), path.clone()]);
        }
        if let Some(config) = self.mcp_config.as_ref() {
            args.extend(["--mcp-config".into(), config.clone()]);
        }
        for dir in &self.add_dirs {
            args.extend(["--add-dir".into(), dir.clone()]);
        }
    }
}

/// Which Claude account a launched CLI runs under. Selection is explicit (session or project
/// default); no variant is ever chosen automatically or as a fallback.
#[derive(Clone, PartialEq, Eq)]
pub enum ClaudeAccountLaunch {
    /// A managed subscription profile: the CLI reads its own login from this home.
    Subscription { managed_home: PathBuf },
    /// An API-key account: the key is handed to this one child only.
    ApiKey { key: String },
}

// The key must never reach logs or panic output.
impl std::fmt::Debug for ClaudeAccountLaunch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Subscription { managed_home } => f
                .debug_struct("Subscription")
                .field("managed_home", managed_home)
                .finish(),
            Self::ApiKey { .. } => f
                .debug_struct("ApiKey")
                .field("key", &"<redacted>")
                .finish(),
        }
    }
}

/// Credential variables a selected account must not inherit from the daemon environment.
const CLAUDE_ACCOUNT_STRIPPED_ENV: [&str; 2] = ["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"];

/// Apply the selected account to a Claude launch spec. `None` leaves the default behaviour
/// untouched.
///
/// - subscription: `CLAUDE_CONFIG_DIR` points at the profile's managed home and both ambient
///   credential variables are stripped, so the profile login is the only credential in play.
/// - api key: `ANTHROPIC_API_KEY` is set on this spec only (it is never removed-then-inherited)
///   and the ambient OAuth token is stripped.
pub fn apply_claude_account_env(spec: &mut LaunchSpec, account: Option<&ClaudeAccountLaunch>) {
    let Some(account) = account else { return };
    match account {
        ClaudeAccountLaunch::Subscription { managed_home } => {
            for name in CLAUDE_ACCOUNT_STRIPPED_ENV {
                spec.env.remove(name);
                spec.env_remove.insert(name.to_string());
            }
            spec.env.insert(
                "CLAUDE_CONFIG_DIR".into(),
                managed_home.to_string_lossy().into_owned(),
            );
        }
        ClaudeAccountLaunch::ApiKey { key } => {
            spec.env.remove("CLAUDE_CODE_OAUTH_TOKEN");
            spec.env_remove.insert("CLAUDE_CODE_OAUTH_TOKEN".into());
            spec.env_remove.remove("ANTHROPIC_API_KEY");
            spec.env.insert("ANTHROPIC_API_KEY".into(), key.clone());
        }
    }
}

/// Parse one stream line with RightKit's fail-closed `StreamParser`, returning the line's JSON
/// (for the durable event key) and the events RightKit derived from it.
fn parse_events(line: &str) -> Result<(String, Value, Vec<AgentEvent>), DriverHostError> {
    let value: Value = serde_json::from_str(line)
        .map_err(|error| invalid(DriverKind::Claude, error.to_string()))?;
    let message_type = value
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(DriverKind::Claude, "missing type"))?
        .to_string();
    let events = rk::StreamParser::with_policy(UnknownPolicy::FailClosed)
        .parse_line(line)
        .map_err(map_parse_error)?;
    Ok((message_type, value, events))
}

fn map_parse_error(error: ParseError) -> DriverHostError {
    match error.unknown_required_method() {
        Some(method) => DriverHostError::UnknownRequiredEvent {
            driver: DriverKind::Claude,
            method: method.to_string(),
        },
        None => invalid(DriverKind::Claude, error.to_string()),
    }
}

pub fn parse_claude_stream_line(
    line: &str,
    source_offset: u64,
) -> Result<Option<NormalizedEvent>, DriverHostError> {
    let (message_type, value, events) = parse_events(line)?;
    normalize(&message_type, &value, &events, source_offset)
}

/// Map the events RightKit derived from one line onto the host app's single normalized event.
fn normalize(
    message_type: &str,
    value: &Value,
    events: &[AgentEvent],
    source_offset: u64,
) -> Result<Option<NormalizedEvent>, DriverHostError> {
    let kind = match message_type {
        "system" => {
            let init = events.iter().find_map(|event| match event {
                AgentEvent::SessionInit(init) => Some(init),
                _ => None,
            });
            match init {
                Some(init) => {
                    if init.session_id.is_empty() {
                        return Err(invalid(
                            DriverKind::Claude,
                            "system/init missing session_id",
                        ));
                    }
                    NormalizedEventKind::SessionStarted {
                        vendor_session_id: init.session_id.clone(),
                        init: Some(Box::new(ClaudeSessionInit::from(init))),
                    }
                }
                None => {
                    // Hooks, status updates and compaction boundaries are bookkeeping; an
                    // unknown subtype RightKit did not refuse is progress or telemetry.
                    if events
                        .iter()
                        .any(|event| matches!(event, AgentEvent::Unknown { .. }))
                    {
                        let subtype = value.get("subtype").and_then(Value::as_str).unwrap_or("");
                        eprintln!(
                            "rightkit-agent-driver-host: ignoring Claude system/{subtype} (progress or telemetry)"
                        );
                    }
                    return Ok(None);
                }
            }
        }
        "stream_event" => match stream_event_kind(value, events) {
            Some(kind) => kind,
            None => return Ok(None),
        },
        "assistant" => match tool_uses_of(events)?.into_iter().next() {
            Some(tool) => NormalizedEventKind::ToolCallStarted {
                call_id: tool.call_id,
                tool_name: tool.tool_name,
                arguments: tool.arguments,
            },
            None => NormalizedEventKind::AssistantCompleted {
                text: events
                    .iter()
                    .filter_map(|event| match event {
                        AgentEvent::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(""),
            },
        },
        "user" => {
            let result = events.iter().find_map(|event| match event {
                AgentEvent::ToolResultContent {
                    id,
                    is_error,
                    content,
                    ..
                } => Some((id, *is_error, content)),
                _ => None,
            });
            let Some((id, is_error, content)) = result else {
                return Ok(None);
            };
            if id.is_empty() {
                return Err(invalid(
                    DriverKind::Claude,
                    "tool_result missing tool_use_id",
                ));
            }
            NormalizedEventKind::ToolResult {
                call_id: id.clone(),
                is_error,
                content: Value::String(content.clone()),
            }
        }
        "result" => {
            let summary = result_summary(events)
                .ok_or_else(|| invalid(DriverKind::Claude, "result message carried no summary"))?;
            result_kind(summary)
        }
        // `rate_limit_event` carries no account identity; the typed event leaves `account` unset
        // for the caller that owns the managed profile to stamp (`stamp_account`).
        "rate_limit_event" => {
            let info = events
                .iter()
                .find_map(|event| match event {
                    AgentEvent::RateLimit(info) => Some(info),
                    _ => None,
                })
                .ok_or_else(|| invalid(DriverKind::Claude, "rate_limit_event carried no info"))?;
            if info.status.is_empty() {
                return Err(invalid(
                    DriverKind::Claude,
                    "rate_limit_info missing status",
                ));
            }
            NormalizedEventKind::RateLimit {
                account: None,
                status: info.status.clone(),
                rate_limit_type: info.rate_limit_type.clone(),
                resets_at: info.resets_at,
                utilization: info.utilization,
                overage_status: info.overage_status.clone(),
                error_code: info.error_code.clone(),
            }
        }
        // Replay acknowledgements and permission requests are transport bookkeeping, not
        // session truth (the can_use_tool request is answered by `claude_permission`). Any
        // other top-level type was already refused by the fail-closed parser.
        _ => return Ok(None),
    };
    Ok(Some(NormalizedEvent {
        event_key: stable_event_key(DriverKind::Claude, message_type, value, source_offset),
        driver: DriverKind::Claude,
        source_offset,
        vendor_method: message_type.into(),
        kind,
    }))
}

/// Partial-message events: text deltas (RightKit's `TextDelta`) and the turn start, which
/// RightKit does not surface and is read from the raw `message_start`. Everything else on the
/// partial-message stream is presentation-only.
fn stream_event_kind(value: &Value, events: &[AgentEvent]) -> Option<NormalizedEventKind> {
    if let Some(text) = events.iter().find_map(|event| match event {
        AgentEvent::TextDelta { text } => Some(text.clone()),
        _ => None,
    }) {
        return Some(NormalizedEventKind::AssistantDelta { text });
    }
    let event = value.get("event")?;
    if event.get("type").and_then(Value::as_str) == Some("message_start") {
        return Some(NormalizedEventKind::TurnStarted {
            vendor_turn_id: event
                .get("message")
                .and_then(|message| message.get("id"))
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
        });
    }
    None
}

fn result_summary(events: &[AgentEvent]) -> Option<&ResultSummary> {
    events.iter().find_map(|event| match event {
        AgentEvent::ResultDetail(summary) => Some(summary),
        _ => None,
    })
}

fn result_kind(summary: &ResultSummary) -> NormalizedEventKind {
    if summary.is_error {
        return NormalizedEventKind::TurnFailed {
            message: failure_message(summary),
        };
    }
    match summary.subtype.as_str() {
        "success" | "" => match &summary.usage {
            Some(usage) => NormalizedEventKind::TurnCompletedWithUsage {
                prompt_tokens: saturating_u32(usage.input),
                completion_tokens: saturating_u32(usage.output),
                cache_read_tokens: saturating_u32(usage.cache_read),
                cache_creation_tokens: saturating_u32(usage.cache_write),
            },
            None => NormalizedEventKind::TurnCompleted,
        },
        "interrupted" => NormalizedEventKind::TurnInterrupted,
        _ => NormalizedEventKind::TurnFailed {
            message: failure_message(summary),
        },
    }
}

fn saturating_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// Message for a failed `result` line: RightKit's failure text (the joined `errors`, else the
/// `result` text of an error result), or the subtype when the CLI sent neither.
fn failure_message(summary: &ResultSummary) -> String {
    match summary
        .failure_message
        .as_deref()
        .filter(|text| !text.is_empty())
    {
        Some(text) => text.to_string(),
        None if summary.subtype.is_empty() => "Claude turn failed".to_string(),
        None => format!("Claude turn failed ({})", summary.subtype),
    }
}

/// One `tool_use` block of an assistant message (issue #334).
#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeToolUse {
    pub call_id: String,
    pub tool_name: String,
    pub arguments: Value,
    /// Set when the call was made inside a subagent: the id of the parent `Task` tool_use.
    pub parent_tool_use_id: Option<String>,
}

fn tool_uses_of(events: &[AgentEvent]) -> Result<Vec<ClaudeToolUse>, DriverHostError> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolUse {
                id,
                name,
                input,
                parent_tool_use_id,
            } => Some((id, name, input, parent_tool_use_id)),
            _ => None,
        })
        .map(
            |(id, name, input, parent)| -> Result<ClaudeToolUse, DriverHostError> {
                if id.is_empty() {
                    return Err(invalid(DriverKind::Claude, "tool_use missing id"));
                }
                if name.is_empty() {
                    return Err(invalid(DriverKind::Claude, "tool_use missing name"));
                }
                Ok(ClaudeToolUse {
                    call_id: id.clone(),
                    tool_name: name.clone(),
                    arguments: input.clone(),
                    parent_tool_use_id: parent.clone(),
                })
            },
        )
        .collect()
}

/// Every `tool_use` block of an assistant line, in order, with the message's
/// `parent_tool_use_id`. Empty for any other line type.
pub fn parse_claude_assistant_tool_uses(line: &str) -> Result<Vec<ClaudeToolUse>, DriverHostError> {
    if !line.contains("tool_use") {
        return Ok(Vec::new());
    }
    let value: Value = serde_json::from_str(line)
        .map_err(|error| invalid(DriverKind::Claude, error.to_string()))?;
    if value.get("type").and_then(Value::as_str) != Some("assistant") {
        return Ok(Vec::new());
    }
    let events = rk::StreamParser::new()
        .parse_line(line)
        .map_err(map_parse_error)?;
    tool_uses_of(&events)
}

/// `ToolCallStarted` events for the second and later `tool_use` blocks of an assistant line.
/// [`parse_claude_stream_line`] already yields the first. Sibling keys are derived from the tool
/// call id because the message `uuid` is shared by every block and would dedupe them away.
pub(crate) fn claude_extra_tool_use_events(
    line: &str,
    source_offset: u64,
) -> Result<Vec<NormalizedEvent>, DriverHostError> {
    Ok(extra_tool_use_events(
        parse_claude_assistant_tool_uses(line)?,
        source_offset,
    ))
}

fn extra_tool_use_events(tools: Vec<ClaudeToolUse>, source_offset: u64) -> Vec<NormalizedEvent> {
    tools
        .into_iter()
        .skip(1)
        .map(|tool| NormalizedEvent {
            event_key: format!("claude:tool_use:{}", tool.call_id),
            driver: DriverKind::Claude,
            source_offset,
            vendor_method: "assistant/tool_use".into(),
            kind: NormalizedEventKind::ToolCallStarted {
                call_id: tool.call_id,
                tool_name: tool.tool_name,
                arguments: tool.arguments,
            },
        })
        .collect()
}

/// MCP server entry of the `system/init` payload.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeMcpServerStatus {
    pub name: String,
    pub status: Option<String>,
}

/// Typed `system/init` payload (issue #332), as `crate::event::SessionInit` reports
/// it. Absent fields stay empty; unknown fields are ignored.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeSessionInit {
    pub model: Option<String>,
    pub cwd: Option<String>,
    pub permission_mode: Option<String>,
    pub tools: Vec<String>,
    pub mcp_servers: Vec<ClaudeMcpServerStatus>,
    /// Protocol capabilities the CLI advertises, such as `interrupt_receipt_v1`.
    pub capabilities: Vec<String>,
}

impl ClaudeSessionInit {
    pub fn supports_interrupt_receipt(&self) -> bool {
        self.capabilities
            .iter()
            .any(|capability| capability == rk::INTERRUPT_RECEIPT_CAPABILITY)
    }
}

impl From<&SessionInit> for ClaudeSessionInit {
    fn from(init: &SessionInit) -> Self {
        ClaudeSessionInit {
            model: init.model.clone(),
            cwd: init.cwd.clone(),
            permission_mode: init.permission_mode.clone(),
            tools: init.tools.clone(),
            mcp_servers: init
                .mcp_servers
                .iter()
                .map(|server| ClaudeMcpServerStatus {
                    name: server.name.clone(),
                    status: Some(server.status.clone()),
                })
                .collect(),
            capabilities: init.capabilities.clone(),
        }
    }
}

/// Parse one stream line into all events it implies. Identical to `parse_claude_stream_line`
/// except that further `tool_use` blocks follow the first, a `result` line also yields a
/// trailing `TurnCostReported`, and `account` (when known) is stamped onto account-scoped
/// events.
pub fn parse_claude_stream_events(
    line: &str,
    source_offset: u64,
    account: Option<&ClaudeAccountIdentity>,
) -> Result<Vec<NormalizedEvent>, DriverHostError> {
    let (message_type, value, events) = parse_events(line)?;
    let mut out = Vec::new();
    if let Some(mut event) = normalize(&message_type, &value, &events, source_offset)? {
        stamp_account(&mut event.kind, account);
        out.push(event);
    }
    // Parallel tool calls: the first `tool_use` is the line's event; the rest follow here.
    out.extend(extra_tool_use_events(tool_uses_of(&events)?, source_offset));
    let cost = events.iter().find_map(|event| match event {
        AgentEvent::Result {
            cost_usd,
            duration_ms,
            num_turns,
            ..
        } => Some((*cost_usd, *duration_ms, *num_turns)),
        _ => None,
    });
    if let (Some((total_cost_usd, duration_ms, num_turns)), Some(summary)) =
        (cost, result_summary(&events))
    {
        let mut kind = NormalizedEventKind::TurnCostReported {
            account: None,
            total_cost_usd,
            duration_ms,
            num_turns,
            model_usage: summary
                .model_usage
                .iter()
                .map(|(model, usage)| ClaudeModelUsage {
                    model: model.clone(),
                    input_tokens: usage.input,
                    output_tokens: usage.output,
                    cache_read_input_tokens: usage.cache_read,
                    cache_creation_input_tokens: usage.cache_write,
                })
                .collect(),
        };
        stamp_account(&mut kind, account);
        out.push(NormalizedEvent {
            event_key: event_key(DriverKind::Claude, "result:cost", source_offset),
            driver: DriverKind::Claude,
            source_offset,
            vendor_method: "result".into(),
            kind,
        });
    }
    Ok(out)
}

/// Fill the unset `account` of an account-scoped event.
pub fn stamp_account(kind: &mut NormalizedEventKind, who: Option<&ClaudeAccountIdentity>) {
    if let NormalizedEventKind::RateLimit { account, .. }
    | NormalizedEventKind::TurnCostReported { account, .. } = kind
    {
        if account.is_none() {
            *account = who.cloned();
        }
    }
}

/// Account identity stamped on rate-limit telemetry. The caller fills it from the managed account
/// profile (the `CLAUDE_CONFIG_DIR` the CLI ran under); this crate never reads or stores
/// Claude.ai credentials or session tokens.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeAccountIdentity {
    pub provider_id: String,
    /// Account email, only when the vendor CLI reports one. Never a label or a key tail.
    pub email: Option<String>,
    pub organization: Option<String>,
    /// Stable account id (`subscription/<provider>/<label>` or `api_key/<provider>/<slug>`).
    #[serde(default)]
    pub account_id: Option<String>,
    /// Human label the user gave the account (profile label or API-config label).
    #[serde(default)]
    pub label: Option<String>,
    /// `subscription` or `api_key`.
    #[serde(default)]
    pub kind: Option<String>,
}

impl ClaudeAccountIdentity {
    /// Typed identity for an API-key config: label and kind, never an email.
    pub fn api_key(provider_id: &str, slug: &str, label: &str) -> Self {
        Self {
            provider_id: provider_id.to_string(),
            email: None,
            organization: None,
            account_id: Some(format!("api_key/{provider_id}/{slug}")),
            label: Some(label.to_string()),
            kind: Some("api_key".to_string()),
        }
    }

    /// Display name for logs and messages: label, else email, else the account id.
    pub fn display(&self) -> String {
        self.label
            .clone()
            .or_else(|| self.email.clone())
            .or_else(|| self.account_id.clone())
            .unwrap_or_else(|| self.provider_id.clone())
    }
}

/// Per-model token usage of one turn (`result.modelUsage`, as RightKit's `ResultSummary`
/// reports it).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeModelUsage {
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
}
