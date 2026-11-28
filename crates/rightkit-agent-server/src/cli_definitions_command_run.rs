// Flattened args for Command::Run. Included via #[path] mod from
// cli_definitions_command.rs.

#[derive(clap::Args, Debug, Clone, PartialEq)]
pub struct RunArgs {
    /// Deprecated and ignored. When context is unavailable the turn runs visibly degraded.
    #[arg(long, value_enum, default_value_t = ContextMode::Membrane, hide = true)]
    pub context_mode: ContextMode,
    /// The prompt. Optional when stdin or a session-only mode supplies it.
    #[arg(required_unless_present_any = ["pipe", "session_id", "print", "input_format"])]
    pub prompt: Option<String>,
    /// Headless print mode: suppress per-event streaming and print only the
    /// final assistant output once the run reaches a terminal event. This is
    /// NOT an alias for the default (Text) one-shot output, which prints
    /// every intermediate event as it streams; Print and Text are distinct
    /// `RunOutputMode` variants with distinct rendering paths (FM-107 —
    /// see `select_run_output_mode` / `process_daemon_event_with_mode` in
    /// `main_runtime_run_session.rs`).
    #[arg(short = 'p', long = "print")]
    pub print: bool,
    /// Output contract for print & JSON-event clients (`json` emits one envelope).
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub output_format: OutputFormat,
    /// Print exactly one JSON result value on stdout (the same as
    /// `--output-format json`); progress goes to stderr. `--stream-json` is the
    /// events-on-stdout form.
    #[arg(long)]
    pub json: bool,
    /// Emit durable daemon events as JSON lines.
    #[arg(long)]
    pub json_event: bool,
    /// Print one line per model request, retry and result on stderr; stdout is unchanged.
    #[arg(short = 'v', long)]
    pub verbose: bool,
    /// Model slug. Defaults to the selected provider's catalog default.
    #[arg(long)]
    pub model: Option<String>,
    /// Provider id for the session route. Omit to let the daemon resolve the
    /// configured default/provider routes — passing no flag must not silently
    /// pin a provider.
    #[arg(long)]
    pub provider: Option<String>,
    /// Reasoning effort override for the session route: low, medium, high, xhigh, max.
    #[arg(long)]
    pub effort: Option<String>,
    /// Workspace root directory (default: current directory).
    #[arg(long)]
    pub workspace: Option<String>,
    /// Allow all mutating tools without prompting.
    // The twin's `--yes` alias of this flag is the global `--yes` now (`Cli::yes`); a host that
    // sets `host_options::yes_flag_is_auto_approve` reads it as this flag (`effective_auto_approve`).
    #[arg(long)]
    pub auto_approve: bool,
    /// Allow all tools without prompting. Intended for disposable/sandboxed harness runs.
    #[arg(long)]
    pub bypass_approvals: bool,
    /// Permission mode for the tool calls inside the run. The default is
    /// `auto-safe` under the workspace sandbox: file writes and routine
    /// commands run, a destructive or unclassified command is denied at once
    /// as `unattended` and the run goes on. `plan` is read-only and ends the
    /// run with the plan (exit 1 `interaction_required`). `bypass` allows
    /// every tool call and needs a workspace trusted with `{app} trust`
    /// and `--yes` in the same call; `--yes` alone never selects a mode.
    #[arg(long = "permission-mode", value_enum, group = "run_permission_mode")]
    pub permission_mode: Option<CliPermissionMode>,
    /// Shorthand for `--permission-mode ask` (the same flag the TUI takes).
    #[arg(long, group = "run_permission_mode")]
    pub ask: bool,
    /// Shorthand for `--permission-mode accept-edits`.
    #[arg(long = "accept-edits", group = "run_permission_mode")]
    pub accept_edits: bool,
    /// Shorthand for `--permission-mode auto-safe` (the default for a run).
    #[arg(long, group = "run_permission_mode")]
    pub auto: bool,
    /// Shorthand for `--permission-mode bypass` (needs a trusted workspace and `--yes`).
    #[arg(long, group = "run_permission_mode")]
    pub yolo: bool,
    /// Internal: `{app} rpc` sets it. With `--input-format stream-json`, an approval request
    /// is NOT denied as `unattended`; it stays open until an `approval_response` input line
    /// answers it (or input ends, which denies it as `detached`).
    #[arg(long = "approval-callback", hide = true)]
    pub approval_callback: bool,
    /// Print the dry-run receipt (route, workspace, trust, permission mode,
    /// tools) and exit 0: nothing runs, no provider is called, no session is made.
    #[arg(long = "dry-run")]
    pub dry_run: bool,
    /// Trust this workspace for this run only. It is journaled and records
    /// nothing; `bypass` still needs recorded trust.
    #[arg(long = "trust-workspace")]
    pub trust_workspace: bool,
    /// Make a retried call idempotent: the same id returns the same session
    /// and never calls the provider a second time. It names the session, so it
    /// cannot be combined with `--session-id`, `--continue` or `--ephemeral`.
    #[arg(
        long = "request-id",
        value_name = "ID",
        value_parser = parse_request_id,
        conflicts_with_all = ["session_id", "continue_last", "ephemeral", "no_session", "fork"]
    )]
    pub request_id: Option<String>,
    /// Continue the most recent session of this workspace.
    #[arg(long = "continue", conflicts_with_all = ["session_id", "ephemeral", "no_session"])]
    pub continue_last: bool,
    /// Run in a fork of the session named by `--session-id` or `--continue`
    /// instead of the session itself.
    #[arg(long, conflicts_with_all = ["ephemeral", "no_session"])]
    pub fork: bool,
    /// Print events as JSONL to stdout.
    #[arg(long)]
    pub stream_json: bool,
    /// Exit non-zero when a run emits a typed terminal error event.
    #[arg(long)]
    pub exit_on_error: bool,
    /// Maximum number of agent turns (LLM calls) before the run stops. Default: unlimited.
    #[arg(long)]
    pub max_turns: Option<u32>,
    /// Read the prompt from stdin instead of the positional argument. Prompt arg is ignored.
    #[arg(long)]
    pub pipe: bool,
    /// Comma-separated list of tool names to allow (restricts to these + always-on tools).
    #[arg(long, value_delimiter = ',')]
    pub allowed_tools: Vec<String>,
    /// Comma-separated list of tool names to deny (removed from the default registry).
    #[arg(long, value_delimiter = ',')]
    pub disallowed_tools: Vec<String>,
    /// Named toolset profile to apply as a capability ceiling.
    #[arg(long)]
    pub toolset: Option<String>,
    /// Named toolset profile for this session's own tools only; sub-agents keep their own tools
    /// (e.g. `--own-toolset orchestrator`: delegate, never read or edit yourself).
    #[arg(long)]
    pub own_toolset: Option<String>,
    /// Budget ceiling in USD; the run is interrupted when spending exceeds this.
    #[arg(long)]
    pub max_budget_usd: Option<f64>,
    /// Run without project instruction loading or extra instruction roots.
    #[arg(long)]
    pub bare: bool,
    #[arg(
        long = "safe-mode",
        help = "Strict permission tier (Untrusted trust + read-only toolset intersect) — FEATURE-MATRIX §#25 worse row."
    )]
    pub safe_mode: bool,
    /// Override the system prompt injected before the first user message.
    #[arg(long)]
    pub system_prompt: Option<String>,
    /// Read the replacement system prompt from a file.
    #[arg(long)]
    pub system_prompt_file: Option<String>,
    /// Append extra system prompt text after project instructions and any replacement prompt.
    #[arg(long)]
    pub append_system_prompt: Option<String>,
    /// Read appended system prompt text from a file.
    #[arg(long)]
    pub append_system_prompt_file: Option<String>,
    /// Path to an MCP config JSON file ({"mcpServers": {...}}).
    #[arg(long)]
    pub mcp_config: Option<String>,
    /// Run in a new git worktree (a new branch `{app}/NAME` under
    /// `{STATE_DIR}/worktrees/NAME`) instead of the checkout; `{app} apply NAME` brings the
    /// result back. NAME is a short word (letters, digits, `-`, `_`, `.`); omit it for a
    /// generated one. The worktree starts from HEAD, so uncommitted changes stay behind.
    #[arg(long, value_name = "NAME", num_args = 0..=1, default_missing_value = "")]
    pub worktree: Option<String>,
    /// Resume this session ID instead of creating a new session.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Additional instruction root directories (may be repeated).
    #[arg(long)]
    pub add_dir: Vec<String>,
    /// Image files attached to the initial user message. May be repeated.
    #[arg(long = "image", value_name = "PATH")]
    pub image: Vec<String>,
    /// Do not persist this session after the run.
    #[arg(long, conflicts_with = "session_id")]
    pub ephemeral: bool,
    /// Alias for --ephemeral.
    #[arg(long, conflicts_with = "session_id")]
    pub no_session: bool,
    /// Write final assistant text to this path.
    #[arg(short = 'o', long = "output-last-message", value_name = "PATH")]
    pub output_last_message: Option<String>,
    /// Control ANSI color output.
    #[arg(long, value_enum, default_value_t = CliColor::Auto)]
    pub color: CliColor,
    /// Comma-separated tool names or glob patterns to include.
    #[arg(long = "tools", value_delimiter = ',')]
    pub tools: Vec<String>,
    /// Comma-separated tool names or glob patterns to exclude.
    #[arg(long = "exclude-tools", value_delimiter = ',')]
    pub exclude_tools: Vec<String>,
    /// Disable MCP tool discovery for this run.
    #[arg(long = "no-mcp")]
    pub no_mcp: bool,
    /// Use a local model server instead of a configured route (see the launch flag of the same
    /// name): the first of the usual local addresses that answers its model list.
    #[arg(long = "oss", conflicts_with = "local_provider")]
    pub oss: bool,
    /// Like `--oss`, for one server: `ollama`, `lmstudio`, `llamacpp` or an `http(s)://host:port/v1`
    /// base URL.
    #[arg(long = "local-provider", value_name = "NAME_OR_URL")]
    pub local_provider: Option<String>,
    /// Ship the `web_search` tool in every request of this run (it is otherwise found with
    /// `tool_search` first), and keep it when `--tools` names a list.
    #[arg(long = "search")]
    pub search: bool,
    /// Prompt input format.
    #[arg(long, value_enum, default_value = "text", conflicts_with = "pipe")]
    pub input_format: crate::main_runtime::InputFormat,
    /// Validate final assistant response against JSON Schema from this file
    /// (par §#214). When set, the final response must conform to the
    /// schema in <path>; non-conforming output exits with the
    /// schema-violation code.
    #[arg(long)]
    pub output_schema: Option<String>,
    /// With `--output-schema`: how many times to ask the model to correct an answer that does not
    /// match the schema (0 to 5). Each retry is a new follow-up message in the same session, so it
    /// costs one more model call; the default 0 keeps the first mismatch an exit 1 `schema_violation`.
    #[arg(
        long = "schema-retries",
        value_name = "N",
        default_value_t = 0,
        value_parser = clap::value_parser!(u32).range(0..=5),
        requires = "output_schema"
    )]
    pub schema_retries: u32,
}

/// `--request-id` is a non-blank id of at most 256 bytes.
fn parse_request_id(value: &str) -> Result<String, String> {
    let id = value.trim();
    if id.is_empty() {
        return Err("--request-id must not be blank".to_string());
    }
    if id.len() > 256 {
        return Err("--request-id is at most 256 bytes".to_string());
    }
    Ok(id.to_string())
}

/// The one permission-mode option of `run`, `exec` and `-p` (R38).
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
#[clap(rename_all = "kebab-case")]
pub enum CliPermissionMode {
    Plan,
    Ask,
    AcceptEdits,
    AutoSafe,
    Bypass,
}

impl RunArgs {
    /// Fold the shorthand mode flags into `permission_mode`, so every later
    /// reader sees one field.
    pub fn normalize_permission_flags(&mut self) {
        if self.permission_mode.is_none() {
            self.permission_mode = if self.ask {
                Some(CliPermissionMode::Ask)
            } else if self.accept_edits {
                Some(CliPermissionMode::AcceptEdits)
            } else if self.auto {
                Some(CliPermissionMode::AutoSafe)
            } else if self.yolo {
                Some(CliPermissionMode::Bypass)
            } else {
                None
            };
        }
    }
}

impl CliPermissionMode {
    pub fn to_policy(self) -> rightkit_agent_policy::PermissionMode {
        match self {
            CliPermissionMode::Plan => rightkit_agent_policy::PermissionMode::Plan,
            CliPermissionMode::Ask => rightkit_agent_policy::PermissionMode::Ask,
            CliPermissionMode::AcceptEdits => rightkit_agent_policy::PermissionMode::AcceptEdits,
            CliPermissionMode::AutoSafe => rightkit_agent_policy::PermissionMode::AutoSafe,
            CliPermissionMode::Bypass => rightkit_agent_policy::PermissionMode::Bypass,
        }
    }
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[clap(rename_all = "kebab-case")]
pub enum CliColor {
    Always,
    Never,
    #[default]
    Auto,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[clap(rename_all = "kebab-case")]
pub enum ContextMode {
    #[default]
    Membrane,
    Direct,
}

impl ContextMode {
    /// The effective internal context mode. The `--context-mode` flag is a
    /// deprecated no-op; Membrane is always attempted and unavailable
    /// context degrades visibly at runtime, never to a shadow store.
    pub fn effective() -> Self {
        Self::Membrane
    }
}

/// Print the one-line deprecation warning when argv still carries the
/// removed `--context-mode` flag. The flag keeps parsing so scripts do not break.
pub fn warn_deprecated_context_mode_flag() {
    if std::env::args().any(|arg| arg == "--context-mode" || arg.starts_with("--context-mode=")) {
        eprintln!(
            "warning: --context-mode is deprecated and ignored; {} runs visibly degraded when context is unavailable.",
            rightkit_agent_config::app_name()
        );
    }
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[clap(rename_all = "kebab-case")]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
    StreamJson,
    /// Emit nothing on stdout/stderr. Internal-only: interactive surfaces
    /// that render turn events through their own projection (the TUI's
    /// transcript) set this so `print_event` cannot write raw bytes into a
    /// live terminal viewport. Not selectable from the CLI.
    #[value(skip)]
    Silent,
}
