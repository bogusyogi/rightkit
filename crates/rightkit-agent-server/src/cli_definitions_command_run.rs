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
    /// Emit durable daemon events as JSON lines.
    #[arg(long)]
    pub json_event: bool,
    /// Model slug. Defaults to the selected provider's catalog default.
    #[arg(long)]
    pub model: Option<String>,
    /// Provider id for the session route. Omit to let the daemon resolve the
    /// configured default/provider routes — passing no flag must not silently
    /// pin a provider.
    #[arg(long)]
    pub provider: Option<String>,
    /// Reasoning effort override for the session route: low, medium, high.
    #[arg(long)]
    pub effort: Option<String>,
    /// Workspace root directory (default: current directory).
    #[arg(long)]
    pub workspace: Option<String>,
    /// Allow all mutating tools without prompting.
    #[arg(long, alias = "yes")]
    pub auto_approve: bool,
    /// Allow all tools without prompting. Intended for disposable/sandboxed harness runs.
    #[arg(long)]
    pub bypass_approvals: bool,
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
    /// Prompt input format.
    #[arg(long, value_enum, default_value = "text", conflicts_with = "pipe")]
    pub input_format: crate::main_runtime::InputFormat,
    /// Validate final assistant response against JSON Schema from this file
    /// (par §#214). When set, the final response must conform to the
    /// schema in <path>; non-conforming output exits with the
    /// schema-violation code.
    #[arg(long)]
    pub output_schema: Option<String>,
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
            "warning: --context-mode is deprecated and ignored; RightKit runs visibly degraded when context is unavailable."
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
