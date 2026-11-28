// Top-level launch flags for a bare `{app}` start (no subcommand).
// Included from cli_definitions.rs (shares its `clap` imports).
//
// `main_runtime_entry.rs::interactive_layout_invocation` decides whether an
// argv is a launch invocation (only these flags, no subcommand); this struct
// then parses it so clap owns validation: the four permission flags are
// mutually exclusive, `--model` / `-p` need a value.

/// `{app} [--plain] [--reduced-motion] [--ask|--accept-edits|--auto|--yolo]
/// [--model ROUTE] [--provider ID] [--effort LEVEL] [--workspace PATH] [PROMPT]`.
#[derive(Parser, Debug, Clone, PartialEq, Eq, Default)]
#[command(
    name = crate::host_identity::app(),
    version,
    about = "{APP_NAME} interactive session (launch flags)",
    disable_help_subcommand = true
)]
pub struct LaunchCli {
    /// Plain output: no motion or decoration.
    #[arg(long = "plain")]
    pub plain: bool,
    /// Reduce terminal animation.
    #[arg(long = "reduced-motion")]
    pub reduced_motion: bool,
    /// Deprecated and ignored. When context is unavailable the turn runs visibly degraded.
    #[arg(long, value_enum, hide = true)]
    pub context_mode: Option<cli_definitions_command_run::ContextMode>,
    /// `-p` only: perform the call. `-p` does the work with no flag, so
    /// `--yes` changes nothing there; it only completes `--permission-mode
    /// bypass`, which needs a trusted workspace and `--yes` in the same call.
    #[arg(long)]
    pub yes: bool,
    /// `-p` only: print the dry-run receipt and exit 0; nothing runs.
    #[arg(long = "dry-run")]
    pub dry_run: bool,
    /// `-p` only: trust this workspace for this run only (journaled, records nothing).
    #[arg(long = "trust-workspace")]
    pub trust_workspace: bool,
    /// `-p` only: make a retried call idempotent (the same id returns the same
    /// session and never calls the provider again).
    #[arg(long = "request-id", value_name = "ID")]
    pub request_id: Option<String>,
    /// `-p` only: print exactly one JSON result value on stdout.
    #[arg(long)]
    pub json: bool,
    /// `-p` only: stream every event as one JSON line (same as `run --stream-json`).
    #[arg(long = "stream-json")]
    pub stream_json: bool,
    /// `-p` only: `text`, `json` or `stream-json` (same as `run --output-format`).
    #[arg(long = "output-format")]
    pub output_format: Option<String>,
    /// A label for the caller, journaled as a claim, never an identity.
    #[arg(long, value_name = "LABEL")]
    pub caller: Option<String>,
    /// `-p` only: run in a fork of the session named by `--resume` or `--continue`.
    #[arg(long)]
    pub fork: bool,
    /// Permission mode by name: plan, ask, accept-edits, auto-safe or bypass.
    /// The same option `run`, `exec` and `-p` take.
    #[arg(long = "permission-mode", value_enum, group = "launch_permission_mode")]
    pub permission_mode_name: Option<CliPermissionMode>,
    /// Permission mode: ask before edits and commands (the default).
    #[arg(long, group = "launch_permission_mode")]
    pub ask: bool,
    /// Permission mode: auto-apply file edits; commands still ask.
    #[arg(long = "accept-edits", group = "launch_permission_mode")]
    pub accept_edits: bool,
    /// Permission mode: auto-approve safe actions; destructive ones still ask.
    #[arg(long, group = "launch_permission_mode")]
    pub auto: bool,
    /// Permission mode: approve everything (a locked workspace still blocks tools).
    #[arg(long, group = "launch_permission_mode")]
    pub yolo: bool,
    /// Start on this route for this run only: `<provider>/<model>`,
    /// `<provider>::<model>`, or a bare `<model>`. Does not change the saved selection.
    #[arg(long, value_name = "ROUTE")]
    pub model: Option<String>,
    /// Provider id to use for this run.
    #[arg(long)]
    pub provider: Option<String>,
    /// Reasoning effort for this run.
    #[arg(long)]
    pub effort: Option<String>,
    /// Workspace root directory (default: current directory).
    #[arg(long)]
    pub workspace: Option<String>,
    /// Resume an existing session by ID.
    #[arg(long, conflicts_with = "continue_last")]
    pub resume: Option<String>,
    /// Continue the most recent session.
    #[arg(long = "continue", conflicts_with = "resume")]
    pub continue_last: bool,
    /// Prompt supplied as a bare positional argument.
    #[arg(value_name = "PROMPT", conflicts_with = "print")]
    pub prompt: Option<String>,
    /// Comma-separated tool names or `*` globs to enable (pi `--tools`); an
    /// allowlist that keeps MCP tools unless an entry starts with `mcp__`.
    #[arg(long = "tools", value_delimiter = ',', value_name = "GLOBS")]
    pub tools: Vec<String>,
    /// Comma-separated tool names or `*` globs to disable after selection,
    /// MCP tools included (pi `--exclude-tools`).
    #[arg(long = "exclude-tools", value_delimiter = ',', value_name = "GLOBS")]
    pub exclude_tools: Vec<String>,
    /// Apply a named preset (model, effort, tools, permission mode, prompts)
    /// from presets.toml; explicit flags override it.
    #[arg(long, value_name = "NAME", alias = "profile")]
    pub preset: Option<String>,
    /// System prompt text from the applied preset (not a flag).
    #[arg(skip)]
    pub preset_system_prompt: Option<String>,
    /// Disable MCP tool discovery for this run.
    #[arg(long = "no-mcp")]
    pub no_mcp: bool,
    /// Use a local model server instead of a configured route: the first of the usual local
    /// addresses (Ollama, LM Studio, llama.cpp) that answers its model list. `--model` names the
    /// model on it; without `--model` the first listed model is used. A server that does not answer
    /// exits 3 (`unreachable`) and nothing is sent anywhere else.
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
    /// Interactive session only: the models Alt+M / Alt+Shift+M cycle through, in this order
    /// (comma-separated). An entry is a model id, `<provider>/<model>`, or either with `*` as a
    /// wildcard. Without it the keys cycle through every model of the catalogue. The model picker
    /// still lists everything.
    #[arg(long = "models", value_name = "LIST", value_delimiter = ',')]
    pub models: Vec<String>,
    /// Interactive session only: never use the terminal's alternate screen, even when `[tui]
    /// fullscreen = true` is saved. The transcript stays in the normal scrollback.
    #[arg(long = "no-alt-screen")]
    pub no_alt_screen: bool,
    /// Override one config key for this call (repeatable): `-c model.small-model='"x"'`.
    #[arg(short = 'c', long = "config", value_name = "KEY=VALUE", action = clap::ArgAction::Append)]
    pub config_override: Vec<String>,
    /// Do not read the user's config.toml.
    #[arg(long = "ignore-user-config")]
    pub ignore_user_config: bool,
    /// Exit 2 when a config file has a key no setting reads, or cannot be parsed.
    #[arg(long = "strict-config")]
    pub strict_config: bool,
    /// Load no instruction files (CLAUDE.md, AGENTS.md, right.md) into the prompt.
    #[arg(long = "ignore-rules")]
    pub ignore_rules: bool,
    /// Image files attached to the first message (png, jpeg, gif, webp). May be repeated.
    #[arg(long = "image", value_name = "PATH")]
    pub image: Vec<String>,
    /// Run one non-interactive turn in the launch directory, print the
    /// final answer, and exit (non-zero on failure).
    #[arg(short = 'p', long = "print", value_name = "PROMPT")]
    pub print: Option<String>,
    /// Only with -p: stop after this many agent turns.
    #[arg(long = "max-turns", requires = "print")]
    pub max_turns: Option<u32>,
    /// Only with -p: budget ceiling in USD. Kept as text (the struct derives
    /// `Eq`) but parsed as a number here so a bad value gets the launch usage;
    /// `run` still checks it is positive and finite.
    #[arg(
        long = "max-budget-usd",
        value_name = "USD",
        requires = "print",
        value_parser = parse_launch_budget_usd
    )]
    pub max_budget_usd: Option<String>,
    /// Only with -p: no project instruction loading or extra instruction roots.
    #[arg(long, requires = "print")]
    pub bare: bool,
    /// Only with -p: replace the system prompt with this text.
    #[arg(long = "system-prompt", requires = "print")]
    pub system_prompt: Option<String>,
    /// Only with -p: replace the system prompt with this file's text.
    #[arg(
        long = "system-prompt-file",
        requires = "print",
        conflicts_with = "system_prompt"
    )]
    pub system_prompt_file: Option<String>,
    /// Only with -p: append this text to the system prompt.
    #[arg(long = "append-system-prompt", requires = "print")]
    pub append_system_prompt: Option<String>,
    /// Only with -p: append this file's text to the system prompt.
    #[arg(
        long = "append-system-prompt-file",
        requires = "print",
        conflicts_with_all = ["append_system_prompt", "preset"]
    )]
    pub append_system_prompt_file: Option<String>,
    /// Only with -p: extra instruction root directory (repeatable).
    #[arg(long = "add-dir", requires = "print")]
    pub add_dir: Vec<String>,
    /// Only with -p: write the final assistant text to this path.
    #[arg(long = "output-last-message", value_name = "PATH", requires = "print")]
    pub output_last_message: Option<String>,
    /// Only with -p: path to an MCP config JSON file.
    #[arg(long = "mcp-config", requires = "print")]
    pub mcp_config: Option<String>,
}

/// `--max-budget-usd` on the launch line: must parse as a number; the
/// original text is forwarded to `run` unchanged.
fn parse_launch_budget_usd(value: &str) -> Result<String, String> {
    value
        .parse::<f64>()
        .map(|_| value.to_string())
        .map_err(|_| format!("`{value}` is not a number of US dollars"))
}

impl LaunchCli {
    /// Fill unset launch fields from a preset; explicit flags always win.
    /// Returns the preset's system prompt for the caller to append.
    pub fn apply_preset(
        &mut self,
        preset: &rightkit_agent_config::presets::Preset,
    ) -> Option<String> {
        if self.model.is_none() {
            self.model = preset.model.clone();
        }
        if self.provider.is_none() {
            self.provider = preset.provider.clone();
        }
        if self.effort.is_none() {
            self.effort = preset.effort.clone();
        }
        if self.tools.is_empty() {
            self.tools = preset.tools.clone();
        }
        if self.exclude_tools.is_empty() {
            self.exclude_tools = preset.exclude_tools.clone();
        }
        self.no_mcp |= preset.no_mcp;
        if self.permission_mode().is_none() {
            match preset
                .permission_mode
                .as_deref()
                .map(|m| m.trim().to_ascii_lowercase())
                .as_deref()
            {
                Some("ask") => self.ask = true,
                Some("accept-edits" | "acceptedits") => self.accept_edits = true,
                Some("auto" | "auto-safe" | "autosafe") => self.auto = true,
                Some("bypass" | "yolo") => self.yolo = true,
                // `plan` has no shorthand flag; a preset naming it must not fall back to the default mode.
                Some("plan") => self.permission_mode_name = Some(CliPermissionMode::Plan),
                _ => {}
            }
        }
        if self.prompt.is_none() && self.print.is_none() {
            self.prompt = preset.prompt.clone();
        }
        preset.system_prompt.clone()
    }

    /// CLI-15: a preset naming a permission mode this build does not know (a typo, or a name from
    /// another tool) used to be ignored, so a preset meant to be read-only ran in the default mode.
    /// Returns the message to refuse the launch with.
    pub fn unknown_preset_permission_mode(
        preset: &rightkit_agent_config::presets::Preset,
    ) -> Option<String> {
        let name = preset
            .permission_mode
            .as_deref()?
            .trim()
            .to_ascii_lowercase();
        if matches!(
            name.as_str(),
            "ask"
                | "accept-edits"
                | "acceptedits"
                | "auto"
                | "auto-safe"
                | "autosafe"
                | "bypass"
                | "yolo"
                | "plan"
        ) {
            return None;
        }
        Some(format!(
            "the preset's permission_mode `{name}` is not a permission mode; use ask, accept-edits, auto-safe, plan or bypass"
        ))
    }

    /// The permission mode a launch flag selected, if any. The flags are
    /// mutually exclusive, so at most one is set.
    pub fn permission_mode(&self) -> Option<rightkit_agent_policy::PermissionMode> {
        use rightkit_agent_policy::PermissionMode;
        if let Some(mode) = self.permission_mode_name {
            Some(mode.to_policy())
        } else if self.ask {
            Some(PermissionMode::Ask)
        } else if self.accept_edits {
            Some(PermissionMode::AcceptEdits)
        } else if self.auto {
            Some(PermissionMode::AutoSafe)
        } else if self.yolo {
            Some(PermissionMode::Bypass)
        } else {
            None
        }
    }
}
