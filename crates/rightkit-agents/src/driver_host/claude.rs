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
        let mut rk_extras = rk::ClaudeExtras::default();
        rk_extras.tools = Some(Vec::new());
        rk_extras.no_chrome = true;
        rk_extras.fork_session = extras.fork_session;
        rk_extras.resume_session_at = extras.resume_session_at.clone();
        rk_extras.max_turns = extras.max_turns;
        rk_extras.fallback_model = extras.fallback_model.clone();
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
/// Mapping (Claude Code CLI reference, https://code.claude.com/docs/en/cli-reference):
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

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> ClaudeLaunchOptions {
        ClaudeLaunchOptions {
            executable: "/usr/local/bin/claude".into(),
            cwd: "/work".into(),
            mcp_config: r#"{"mcpServers":{"host":{}}}"#.into(),
            session_id: Some("550e8400-e29b-41d4-a716-446655440000".into()),
            resume_session_id: None,
            model: None,
        }
    }

    #[test]
    fn current_claude_launch_uses_bidirectional_strict_mcp_flags() {
        let spec = options().launch_spec().unwrap();
        assert_eq!(
            spec.args,
            [
                "--print",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
                "--session-id",
                "550e8400-e29b-41d4-a716-446655440000",
                "--tools",
                "",
                "--no-chrome",
                "--allowedTools",
                "mcp__rightkit",
                "--append-system-prompt",
                RUN_SCOPED_POLICY,
                "--strict-mcp-config",
                "--mcp-config",
                r#"{"mcpServers":{"host":{}}}"#,
            ]
        );
    }

    #[test]
    fn launch_locks_native_tools_and_mcp_to_the_host_gateway() {
        let args = options().launch_spec().unwrap().args;
        assert!(args.windows(2).any(|w| w == ["--tools", ""]));
        assert!(args
            .windows(2)
            .any(|w| w == ["--allowedTools", "mcp__rightkit"]));
        assert!(args.iter().any(|arg| arg == "--no-chrome"));
        assert!(args.iter().any(|arg| arg == "--strict-mcp-config"));
        assert!(args
            .windows(2)
            .any(|w| w == ["--mcp-config", r#"{"mcpServers":{"host":{}}}"#]));
        assert!(args
            .windows(2)
            .any(|w| w == ["--append-system-prompt", RUN_SCOPED_POLICY]));
        assert!(!args
            .iter()
            .any(|arg| arg == "--dangerously-skip-permissions"));
    }

    #[test]
    fn run_flags_map_to_claude_cli_flags() {
        let mut args = options().launch_spec().unwrap().args;
        ClaudeRunFlags {
            max_turns: Some(3),
            max_budget_usd: Some(1.5),
            system_prompt: Some("be terse".into()),
            system_prompt_file: Some("sp.txt".into()),
            append_system_prompt_file: Some("extra.txt".into()),
            mcp_config: Some("extra.json".into()),
            add_dirs: vec!["/a".into(), "/b".into()],
            ..Default::default()
        }
        .apply(&mut args);
        for pair in [
            ["--max-turns", "3"],
            ["--max-budget-usd", "1.5"],
            ["--system-prompt", "be terse"],
            ["--system-prompt-file", "sp.txt"],
            ["--append-system-prompt-file", "extra.txt"],
            ["--mcp-config", "extra.json"],
            ["--add-dir", "/a"],
            ["--add-dir", "/b"],
        ] {
            assert!(args.windows(2).any(|w| w == pair), "{pair:?}");
        }
        assert!(args.iter().any(|a| a == "--strict-mcp-config"));
    }

    #[test]
    fn caller_append_text_merges_into_policy_flag_once() {
        let mut args = options().launch_spec().unwrap().args;
        ClaudeRunFlags {
            append_system_prompt: Some("Always cite files.".into()),
            ..Default::default()
        }
        .apply(&mut args);
        assert_eq!(
            args.iter()
                .filter(|a| *a == "--append-system-prompt")
                .count(),
            1
        );
        let index = args
            .iter()
            .position(|a| a == "--append-system-prompt")
            .unwrap();
        assert_eq!(
            args[index + 1],
            format!("{RUN_SCOPED_POLICY}\n\nAlways cite files.")
        );
    }

    #[test]
    fn empty_run_flags_leave_argv_unchanged() {
        let base = options().launch_spec().unwrap().args;
        let mut args = base.clone();
        ClaudeRunFlags::default().apply(&mut args);
        assert_eq!(args, base);
    }

    #[test]
    fn selected_model_is_appended_as_a_model_flag() {
        let mut opts = options();
        opts.model = Some("sonnet".into());
        let spec = opts.launch_spec().unwrap();
        assert!(spec
            .args
            .windows(2)
            .any(|pair| pair == ["--model", "sonnet"]));
    }

    #[test]
    fn no_model_selected_omits_the_model_flag_entirely() {
        let spec = options().launch_spec().unwrap();
        assert!(!spec.args.iter().any(|arg| arg == "--model"));
    }

    #[test]
    fn hostile_model_string_is_rejected_before_reaching_argv() {
        let mut opts = options();
        opts.model = Some("sonnet; rm -rf /".into());
        let error = opts.launch_spec().unwrap_err();
        assert!(matches!(
            error,
            DriverHostError::InvalidMessage {
                driver: DriverKind::Claude,
                ..
            }
        ));
    }

    #[test]
    fn launch_spec_denies_provider_keys_and_the_daemon_auth_token() {
        let spec = options().launch_spec().unwrap();
        let expected: std::collections::BTreeSet<String> =
            crate::driver_host::cli_runtime::auth::FORBIDDEN_BYOK_ENV_VARS
                .iter()
                .map(|name| (*name).to_string())
                .collect();
        assert_eq!(spec.env_remove, expected);
        assert!(spec.env_remove.contains("MINIMAX_API_KEY"));
        assert!(spec.env_remove.contains("RIGHTKIT_GATEWAY_TOKEN"));
    }

    #[test]
    fn resume_replaces_new_session_flag() {
        let mut opts = options();
        opts.session_id = None;
        opts.resume_session_id = Some("existing-session".into());
        let spec = opts.launch_spec().unwrap();
        assert!(spec
            .args
            .windows(2)
            .any(|pair| pair == ["--resume", "existing-session"]));
        assert!(!spec.args.iter().any(|arg| arg == "--session-id"));
    }

    #[test]
    fn parses_streaming_text_delta() {
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"hello"}}}"#;
        let event = parse_claude_stream_line(line, 3).unwrap().unwrap();
        assert_eq!(
            event.kind,
            NormalizedEventKind::AssistantDelta {
                text: "hello".into()
            }
        );
    }

    #[test]
    fn message_start_yields_the_turn_start() {
        let line =
            r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"msg_1"}}}"#;
        let event = parse_claude_stream_line(line, 1).unwrap().unwrap();
        assert_eq!(
            event.kind,
            NormalizedEventKind::TurnStarted {
                vendor_turn_id: "msg_1".into()
            }
        );
        let bare = r#"{"type":"stream_event","event":{"type":"message_start"}}"#;
        assert_eq!(
            parse_claude_stream_line(bare, 2).unwrap().unwrap().kind,
            NormalizedEventKind::TurnStarted {
                vendor_turn_id: "unknown".into()
            }
        );
    }

    #[test]
    fn other_partial_stream_events_are_ignored() {
        for kind in [
            "content_block_stop",
            "message_delta",
            "message_stop",
            "filesystem_mutated",
        ] {
            let line = format!(r#"{{"type":"stream_event","event":{{"type":"{kind}"}}}}"#);
            assert!(
                parse_claude_stream_line(&line, 4).unwrap().is_none(),
                "{kind}"
            );
        }
    }

    #[test]
    fn unknown_top_level_message_fails_closed() {
        let line = r#"{"type":"future_kind"}"#;
        assert!(matches!(
            parse_claude_stream_line(line, 4),
            Err(DriverHostError::UnknownRequiredEvent { method, .. }) if method == "future_kind"
        ));
        assert!(matches!(
            parse_claude_stream_line("not json", 4),
            Err(DriverHostError::InvalidMessage { .. })
        ));
        assert!(matches!(
            parse_claude_stream_line(r#"{"nothing":1}"#, 4),
            Err(DriverHostError::InvalidMessage { .. })
        ));
    }

    #[test]
    fn control_lines_are_transport_bookkeeping() {
        let request = r#"{"type":"control_request","request_id":"a","request":{"subtype":"can_use_tool","tool_name":"x","input":{}}}"#;
        assert!(parse_claude_stream_line(request, 1).unwrap().is_none());
        let response =
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"a"}}"#;
        assert!(parse_claude_stream_line(response, 2).unwrap().is_none());
    }

    #[test]
    fn api_key_identity_is_typed_and_never_an_email() {
        let who = ClaudeAccountIdentity::api_key("claude", "team", "Team");
        assert_eq!(who.email, None);
        assert_eq!(who.label.as_deref(), Some("Team"));
        assert_eq!(who.kind.as_deref(), Some("api_key"));
        assert_eq!(who.account_id.as_deref(), Some("api_key/claude/team"));
        assert_eq!(who.display(), "Team");
        // Old payloads without the typed fields still deserialize.
        let old: ClaudeAccountIdentity =
            serde_json::from_str(r#"{"providerId":"claude","email":"a@b.co","organization":null}"#)
                .unwrap();
        assert_eq!(old.display(), "a@b.co");
        assert!(old.kind.is_none());
    }

    #[test]
    fn stream_line_emits_rate_limit_event_and_stamps_account() {
        let who = ClaudeAccountIdentity {
            provider_id: "claude".into(),
            email: Some("a@example.com".into()),
            organization: None,
            ..Default::default()
        };
        let line = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","rateLimitType":"five_hour","resetsAt":1790000000,"utilization":1.0}}"#;
        let bare = parse_claude_stream_line(line, 1).unwrap().unwrap();
        assert!(matches!(
            bare.kind,
            NormalizedEventKind::RateLimit { account: None, ref status, resets_at: Some(1790000000), .. }
                if status == "rejected"
        ));
        let stamped = parse_claude_stream_events(line, 1, Some(&who)).unwrap();
        assert_eq!(stamped.len(), 1);
        assert!(matches!(
            &stamped[0].kind,
            NormalizedEventKind::RateLimit { account: Some(a), .. } if *a == who
        ));
        let bad = r#"{"type":"rate_limit_event"}"#;
        assert!(parse_claude_stream_line(bad, 1).is_err());
    }

    fn bare_spec() -> LaunchSpec {
        options().launch_spec().unwrap()
    }

    #[test]
    fn account_env_none_keeps_default_launch() {
        let mut spec = bare_spec();
        let before = spec.clone();
        apply_claude_account_env(&mut spec, None);
        assert_eq!(spec, before);
        assert!(spec.env_remove.contains("ANTHROPIC_API_KEY"));
        assert!(!spec.env.contains_key("CLAUDE_CONFIG_DIR"));
    }

    #[test]
    fn account_env_subscription_pins_home_and_strips_credentials() {
        let mut spec = bare_spec();
        spec.env
            .insert("ANTHROPIC_API_KEY".into(), "ambient".into());
        let who = ClaudeAccountLaunch::Subscription {
            managed_home: PathBuf::from("/profiles/claude/work"),
        };
        apply_claude_account_env(&mut spec, Some(&who));
        assert_eq!(
            spec.env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some("/profiles/claude/work")
        );
        assert!(!spec.env.contains_key("ANTHROPIC_API_KEY"));
        assert!(spec.env_remove.contains("ANTHROPIC_API_KEY"));
        assert!(spec.env_remove.contains("CLAUDE_CODE_OAUTH_TOKEN"));
    }

    #[test]
    fn account_env_api_key_sets_key_only_on_this_spec_and_never_debug_prints_it() {
        let mut spec = bare_spec();
        let who = ClaudeAccountLaunch::ApiKey {
            key: concat!("sk", "-ant-secret-value").into(),
        };
        assert!(!format!("{who:?}").contains(concat!("sk", "-ant-secret-value")));
        apply_claude_account_env(&mut spec, Some(&who));
        assert_eq!(
            spec.env.get("ANTHROPIC_API_KEY").map(String::as_str),
            Some(concat!("sk", "-ant-secret-value"))
        );
        assert!(!spec.env_remove.contains("ANTHROPIC_API_KEY"));
        assert!(spec.env_remove.contains("CLAUDE_CODE_OAUTH_TOKEN"));
        assert!(!spec.env.contains_key("CLAUDE_CONFIG_DIR"));
        assert!(!spec.args.iter().any(|arg| arg.contains("sk-ant")));
    }

    #[test]
    fn result_line_yields_completion_then_account_cost() {
        let who = ClaudeAccountIdentity {
            provider_id: "claude".into(),
            email: None,
            organization: None,
            ..Default::default()
        };
        let line = r#"{"type":"result","subtype":"success","total_cost_usd":0.5,"num_turns":3,"usage":{"input_tokens":1,"output_tokens":2},"modelUsage":{"m":{"inputTokens":1,"outputTokens":2,"cacheReadInputTokens":3,"cacheCreationInputTokens":4,"costUSD":0.5}}}"#;
        let events = parse_claude_stream_events(line, 9, Some(&who)).unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[0].kind,
            NormalizedEventKind::TurnCompletedWithUsage { .. }
        ));
        match &events[1].kind {
            NormalizedEventKind::TurnCostReported {
                account,
                total_cost_usd,
                num_turns,
                model_usage,
                ..
            } => {
                assert_eq!(account.as_ref(), Some(&who));
                assert_eq!(*total_cost_usd, Some(0.5));
                assert_eq!(*num_turns, Some(3));
                assert_eq!(
                    *model_usage,
                    vec![ClaudeModelUsage {
                        model: "m".into(),
                        input_tokens: 1,
                        output_tokens: 2,
                        cache_read_input_tokens: 3,
                        cache_creation_input_tokens: 4,
                    }]
                );
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_ne!(events[0].event_key, events[1].event_key);
    }

    #[test]
    fn result_without_usage_completes_and_interrupt_is_its_own_outcome() {
        let done = r#"{"type":"result","subtype":"success"}"#;
        assert_eq!(
            parse_claude_stream_line(done, 1).unwrap().unwrap().kind,
            NormalizedEventKind::TurnCompleted
        );
        let bare = r#"{"type":"result"}"#;
        assert_eq!(
            parse_claude_stream_line(bare, 1).unwrap().unwrap().kind,
            NormalizedEventKind::TurnCompleted
        );
        let interrupted = r#"{"type":"result","subtype":"interrupted"}"#;
        assert_eq!(
            parse_claude_stream_line(interrupted, 1)
                .unwrap()
                .unwrap()
                .kind,
            NormalizedEventKind::TurnInterrupted
        );
    }

    #[test]
    fn user_tool_result_carries_the_joined_text() {
        let line = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}]}}"#;
        let event = parse_claude_stream_line(line, 1).unwrap().unwrap();
        assert_eq!(
            event.kind,
            NormalizedEventKind::ToolResult {
                call_id: "t1".into(),
                is_error: true,
                content: Value::String("a\nb".into()),
            }
        );
        let plain = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":"ok"}]}}"#;
        assert_eq!(
            parse_claude_stream_line(plain, 2).unwrap().unwrap().kind,
            NormalizedEventKind::ToolResult {
                call_id: "t2".into(),
                is_error: false,
                content: Value::String("ok".into()),
            }
        );
        let no_id =
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"x"}]}}"#;
        assert!(parse_claude_stream_line(no_id, 3).is_err());
        let echo = r#"{"type":"user","message":{"content":[{"type":"text","text":"hi"}]}}"#;
        assert!(parse_claude_stream_line(echo, 4).unwrap().is_none());
    }

    #[test]
    fn accepts_observed_startup_hooks_and_fails_observed_auth_result() {
        let hook = r#"{"type":"system","subtype":"hook_started","session_id":"session-1"}"#;
        assert!(parse_claude_stream_line(hook, 1).unwrap().is_none());

        let init = r#"{"type":"system","subtype":"init","session_id":"session-1"}"#;
        assert!(matches!(
            parse_claude_stream_line(init, 2).unwrap().unwrap().kind,
            NormalizedEventKind::SessionStarted { .. }
        ));

        let failed =
            r#"{"type":"result","subtype":"success","is_error":true,"result":"Not logged in"}"#;
        assert!(matches!(
            parse_claude_stream_line(failed, 3).unwrap().unwrap().kind,
            NormalizedEventKind::TurnFailed { .. }
        ));
    }

    fn system_line(subtype: &str, extra: &str) -> String {
        format!(r#"{{"type":"system","subtype":"{subtype}","session_id":"s"{extra}}}"#)
    }

    #[test]
    fn hook_progress_is_ignored() {
        let line = system_line("hook_progress", r#","hook_name":"pre""#);
        assert!(parse_claude_stream_line(&line, 1).unwrap().is_none());
    }

    #[test]
    fn task_subtypes_are_ignored() {
        for subtype in [
            "task_started",
            "task_progress",
            "task_updated",
            "task_notification",
        ] {
            let line = system_line(subtype, r#","task_id":"t1""#);
            assert!(
                parse_claude_stream_line(&line, 1).unwrap().is_none(),
                "{subtype} must be ignored"
            );
        }
    }

    #[test]
    fn api_retry_telemetry_is_ignored() {
        let line = system_line("api_retry", r#","attempt":2,"delay_ms":500"#);
        assert!(parse_claude_stream_line(&line, 1).unwrap().is_none());
    }

    #[test]
    fn permission_denied_fails_the_turn_closed() {
        let line = system_line("permission_denied", r#","tool_name":"Bash""#);
        assert!(matches!(
            parse_claude_stream_line(&line, 1),
            Err(DriverHostError::UnknownRequiredEvent { method, .. }) if method == "system:permission_denied"
        ));
    }

    #[test]
    fn unknown_result_bearing_subtype_fails_closed() {
        let by_payload = system_line(
            "future_outcome",
            r#","tool_use_id":"toolu_1","is_error":true"#,
        );
        assert!(matches!(
            parse_claude_stream_line(&by_payload, 1),
            Err(DriverHostError::UnknownRequiredEvent { .. })
        ));
        let by_name = system_line("tool_result_synthesized", "");
        assert!(matches!(
            parse_claude_stream_line(&by_name, 2),
            Err(DriverHostError::UnknownRequiredEvent { .. })
        ));
    }

    #[test]
    fn unknown_plain_subtype_is_logged_and_ignored() {
        let line = system_line("future_banner", r#","text":"hello""#);
        assert!(parse_claude_stream_line(&line, 1).unwrap().is_none());
    }

    fn resume_options() -> ClaudeLaunchOptions {
        let mut opts = options();
        opts.session_id = None;
        opts.resume_session_id = Some("existing-session".into());
        opts
    }

    #[test]
    fn default_extras_leave_the_baseline_argv_identical() {
        let opts = options();
        assert_eq!(
            opts.launch_spec().unwrap(),
            opts.launch_spec_with(&ClaudeLaunchExtras::default())
                .unwrap()
        );
    }

    #[test]
    fn fork_session_and_resume_at_extend_a_resume_launch() {
        let spec = resume_options()
            .launch_spec_with(&ClaudeLaunchExtras {
                fork_session: true,
                resume_session_at: Some("msg-uuid_01:a".into()),
                ..Default::default()
            })
            .unwrap();
        assert!(spec
            .args
            .windows(2)
            .any(|pair| pair == ["--resume", "existing-session"]));
        assert!(spec.args.iter().any(|arg| arg == "--fork-session"));
        assert!(spec
            .args
            .windows(2)
            .any(|pair| pair == ["--resume-session-at", "msg-uuid_01:a"]));
    }

    #[test]
    fn fork_and_resume_at_require_a_resume_session() {
        for extras in [
            ClaudeLaunchExtras {
                fork_session: true,
                ..Default::default()
            },
            ClaudeLaunchExtras {
                resume_session_at: Some("msg-1".into()),
                ..Default::default()
            },
        ] {
            let error = options().launch_spec_with(&extras).unwrap_err();
            assert!(matches!(
                error,
                DriverHostError::InvalidMessage {
                    driver: DriverKind::Claude,
                    ..
                }
            ));
        }
    }

    #[test]
    fn hostile_resume_at_id_is_rejected() {
        for hostile in ["", "a b", "x; rm -rf /", "-flag"] {
            let result = resume_options().launch_spec_with(&ClaudeLaunchExtras {
                resume_session_at: Some(hostile.into()),
                ..Default::default()
            });
            assert!(result.is_err(), "{hostile:?}");
        }
    }

    #[test]
    fn limit_and_mode_extras_become_cli_flags() {
        let mut opts = options();
        opts.model = Some("sonnet".into());
        let spec = opts
            .launch_spec_with(&ClaudeLaunchExtras {
                max_turns: Some(4),
                max_budget_usd: Some(2.5),
                permission_mode: Some("plan".into()),
                fallback_model: Some("haiku".into()),
                ..Default::default()
            })
            .unwrap();
        for pair in [
            ["--max-turns", "4"],
            ["--max-budget-usd", "2.5"],
            ["--permission-mode", "plan"],
            ["--fallback-model", "haiku"],
        ] {
            assert!(spec.args.windows(2).any(|w| w == pair), "{pair:?}");
        }
        assert!(spec.args.windows(2).any(|w| w == ["--model", "sonnet"]));
        // Baseline governance flags are untouched.
        assert!(spec.args.iter().any(|arg| arg == "--strict-mcp-config"));
        assert!(spec.args.windows(2).any(|w| w == ["--tools", ""]));
    }

    #[test]
    fn invalid_mode_and_model_extras_are_rejected() {
        let cases = [
            ClaudeLaunchExtras {
                permission_mode: Some("bypassPermissions".into()),
                ..Default::default()
            },
            ClaudeLaunchExtras {
                permission_mode: Some("yolo".into()),
                ..Default::default()
            },
            ClaudeLaunchExtras {
                fallback_model: Some("opus; rm -rf /".into()),
                ..Default::default()
            },
        ];
        for extras in cases {
            assert!(options().launch_spec_with(&extras).is_err(), "{extras:?}");
        }
        let mut opts = options();
        opts.model = Some("sonnet".into());
        assert!(opts
            .launch_spec_with(&ClaudeLaunchExtras {
                fallback_model: Some("sonnet".into()),
                ..Default::default()
            })
            .is_err());
    }

    #[test]
    fn every_permission_mode_in_the_allowlist_is_accepted() {
        for mode in ["default", "acceptEdits", "dontAsk", "plan"] {
            let spec = options()
                .launch_spec_with(&ClaudeLaunchExtras {
                    permission_mode: Some(mode.into()),
                    ..Default::default()
                })
                .unwrap();
            assert!(spec
                .args
                .windows(2)
                .any(|w| w == ["--permission-mode", mode]));
        }
    }

    #[test]
    fn run_flags_replace_extras_instead_of_duplicating_limits() {
        let mut args = options()
            .launch_spec_with(&ClaudeLaunchExtras {
                max_turns: Some(4),
                max_budget_usd: Some(2.5),
                ..Default::default()
            })
            .unwrap()
            .args;
        ClaudeRunFlags {
            max_turns: Some(9),
            max_budget_usd: Some(7.0),
            ..Default::default()
        }
        .apply(&mut args);
        assert_eq!(args.iter().filter(|a| *a == "--max-turns").count(), 1);
        assert_eq!(args.iter().filter(|a| *a == "--max-budget-usd").count(), 1);
        assert!(args.windows(2).any(|w| w == ["--max-turns", "9"]));
        assert!(args.windows(2).any(|w| w == ["--max-budget-usd", "7"]));
    }

    #[test]
    fn init_payload_is_kept_on_session_started() {
        let line = r#"{"type":"system","subtype":"init","session_id":"s1","cwd":"/work","model":"claude-opus-4-8","permissionMode":"default","tools":["mcp__host__run","Read"],"mcp_servers":[{"name":"host","status":"connected"},{"name":"other","status":"failed"}],"capabilities":["interrupt_receipt_v1"]}"#;
        let event = parse_claude_stream_line(line, 1).unwrap().unwrap();
        let NormalizedEventKind::SessionStarted {
            vendor_session_id,
            init,
        } = event.kind
        else {
            panic!("expected SessionStarted");
        };
        assert_eq!(vendor_session_id, "s1");
        let init = init.expect("init payload kept");
        assert_eq!(init.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(init.cwd.as_deref(), Some("/work"));
        assert_eq!(init.permission_mode.as_deref(), Some("default"));
        assert_eq!(init.tools, ["mcp__rightkit__run", "Read"]);
        assert_eq!(init.mcp_servers.len(), 2);
        assert_eq!(init.mcp_servers[0].name, "rightkit-agent");
        assert_eq!(init.mcp_servers[1].status.as_deref(), Some("failed"));
        assert!(init.supports_interrupt_receipt());
    }

    #[test]
    fn init_without_a_session_id_is_refused() {
        let line = r#"{"type":"system","subtype":"init"}"#;
        assert!(parse_claude_stream_line(line, 1).is_err());
    }

    #[test]
    fn sparse_init_yields_empty_typed_fields() {
        let line = r#"{"type":"system","subtype":"init","session_id":"s2"}"#;
        let event = parse_claude_stream_line(line, 1).unwrap().unwrap();
        let NormalizedEventKind::SessionStarted { init, .. } = event.kind else {
            panic!("expected SessionStarted");
        };
        let init = init.unwrap();
        assert_eq!(*init, ClaudeSessionInit::default());
        assert!(!init.supports_interrupt_receipt());
    }

    #[test]
    fn session_started_without_init_serializes_without_the_field() {
        let kind = NormalizedEventKind::SessionStarted {
            vendor_session_id: "x".into(),
            init: None,
        };
        let value = serde_json::to_value(&kind).unwrap();
        assert!(value.get("init").is_none());
        let back: NormalizedEventKind = serde_json::from_value(value).unwrap();
        assert_eq!(back, kind);
    }

    const TWO_TOOLS: &str = r#"{"type":"assistant","uuid":"u1","parent_tool_use_id":"toolu_parent","message":{"content":[{"type":"text","text":"working"},{"type":"tool_use","id":"toolu_a","name":"mcp__host__a","input":{"n":1}},{"type":"tool_use","id":"toolu_b","name":"mcp__host__b","input":{"n":2}}]}}"#;

    #[test]
    fn assistant_line_with_two_tool_uses_yields_two_tool_events_in_order() {
        let events = parse_claude_stream_events(TWO_TOOLS, 5, None).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0].kind,
            NormalizedEventKind::ToolCallStarted {
                call_id: "toolu_a".into(),
                tool_name: "mcp__rightkit__a".into(),
                arguments: serde_json::json!({"n": 1}),
            }
        );
        assert_eq!(
            events[1].kind,
            NormalizedEventKind::ToolCallStarted {
                call_id: "toolu_b".into(),
                tool_name: "mcp__rightkit__b".into(),
                arguments: serde_json::json!({"n": 2}),
            }
        );
        // Siblings share the message uuid, so their keys must not collapse into one.
        assert_ne!(events[0].event_key, events[1].event_key);
        assert_eq!(events[1].event_key, "claude:tool_use:toolu_b");
        // The single-event parser keeps its original first-block behaviour.
        let first = parse_claude_stream_line(TWO_TOOLS, 5).unwrap().unwrap();
        assert_eq!(first.kind, events[0].kind);
        // The extra-events helper the harness calls yields only the siblings.
        let extra = claude_extra_tool_use_events(TWO_TOOLS, 5).unwrap();
        assert_eq!(extra.len(), 1);
        assert_eq!(extra[0], events[1]);
    }

    #[test]
    fn tool_uses_carry_parent_tool_use_id_in_order() {
        let tools = parse_claude_assistant_tool_uses(TWO_TOOLS).unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].call_id, "toolu_a");
        assert_eq!(tools[1].call_id, "toolu_b");
        assert!(tools
            .iter()
            .all(|tool| tool.parent_tool_use_id.as_deref() == Some("toolu_parent")));
        let top_level = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t","name":"n","input":{}}]}}"#;
        assert_eq!(
            parse_claude_assistant_tool_uses(top_level).unwrap()[0].parent_tool_use_id,
            None
        );
        assert!(parse_claude_assistant_tool_uses(r#"{"type":"result"}"#)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn single_tool_use_and_text_only_assistant_lines_are_unchanged() {
        let one = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"n","input":{}}]}}"#;
        assert_eq!(parse_claude_stream_events(one, 1, None).unwrap().len(), 1);
        let text = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}"#;
        let events = parse_claude_stream_events(text, 1, None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].kind,
            NormalizedEventKind::AssistantCompleted { text: "hi".into() }
        );
        let split = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"a"},{"type":"thinking","thinking":"x"},{"type":"text","text":"b"}]}}"#;
        assert_eq!(
            parse_claude_stream_line(split, 1).unwrap().unwrap().kind,
            NormalizedEventKind::AssistantCompleted { text: "ab".into() }
        );
    }

    #[test]
    fn extra_tool_use_without_an_id_fails_closed() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"n","input":{}},{"type":"tool_use","name":"m","input":{}}]}}"#;
        assert!(parse_claude_stream_events(line, 1, None).is_err());
    }

    fn failure_message_of(line: &str) -> String {
        match parse_claude_stream_line(line, 1).unwrap().unwrap().kind {
            NormalizedEventKind::TurnFailed { message } => message,
            other => panic!("expected TurnFailed, got {other:?}"),
        }
    }

    #[test]
    fn error_subtypes_without_text_name_the_subtype() {
        let message = failure_message_of(
            r#"{"type":"result","subtype":"error_max_turns","is_error":true,"num_turns":7}"#,
        );
        assert_eq!(message, "Claude turn failed (error_max_turns)");
        // Not flagged is_error: still a failure because the subtype is not a success subtype.
        let unknown = failure_message_of(r#"{"type":"result","subtype":"error_future_thing"}"#);
        assert_eq!(unknown, "Claude turn failed (error_future_thing)");
    }

    #[test]
    fn error_list_and_plain_error_text_are_preserved() {
        let message = failure_message_of(
            r#"{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["boom","bang"]}"#,
        );
        assert_eq!(message, "boom\nbang");
        let plain = failure_message_of(
            r#"{"type":"result","subtype":"success","is_error":true,"result":"Not logged in"}"#,
        );
        assert_eq!(plain, "Not logged in");
        let none = failure_message_of(r#"{"type":"result","is_error":true}"#);
        assert_eq!(none, "Claude turn failed");
    }
}
