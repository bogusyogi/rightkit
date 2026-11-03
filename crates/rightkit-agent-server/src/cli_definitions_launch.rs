// Top-level launch flags for a bare `coderight` start (no subcommand).
// Included from cli_definitions.rs (shares its `clap` imports).
//
// `main_runtime_entry.rs::interactive_layout_invocation` decides whether an
// argv is a launch invocation (only these flags, no subcommand); this struct
// then parses it so clap owns validation: the four permission flags are
// mutually exclusive, `--model` / `-p` need a value.

/// `coderight [--plain] [--reduced-motion] [--ask|--accept-edits|--auto|--yolo]
/// [--model ROUTE] [--provider ID] [--effort LEVEL] [--workspace PATH] [PROMPT]`.
#[derive(Parser, Debug, Clone, PartialEq, Eq, Default)]
#[command(
    name = "coderight",
    version,
    about = "RightKit interactive session (launch flags)",
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
    #[arg(long, value_name = "NAME")]
    pub preset: Option<String>,
    /// System prompt text from the applied preset (not a flag).
    #[arg(skip)]
    pub preset_system_prompt: Option<String>,
    /// Disable MCP tool discovery for this run.
    #[arg(long = "no-mcp")]
    pub no_mcp: bool,
    /// Image files attached to the first message (png, jpeg, gif, webp). May be repeated.
    #[arg(long = "image", value_name = "PATH")]
    pub image: Vec<String>,
    /// Run one non-interactive turn in the launch directory, print the
    /// final answer, and exit (non-zero on failure).
    #[arg(short = 'p', long = "print", value_name = "PROMPT")]
    pub print: Option<String>,
}

impl LaunchCli {
    /// Fill unset launch fields from a preset; explicit flags always win.
    /// Returns the preset's system prompt for the caller to append.
    pub fn apply_preset(
        &mut self,
        preset: &rightkit_app_settings::agent_config::presets::Preset,
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
                Some("auto") => self.auto = true,
                Some("bypass" | "yolo") => self.yolo = true,
                _ => {}
            }
        }
        if self.prompt.is_none() && self.print.is_none() {
            self.prompt = preset.prompt.clone();
        }
        preset.system_prompt.clone()
    }

    /// The permission mode a launch flag selected, if any. The flags are
    /// mutually exclusive, so at most one is set.
    pub fn permission_mode(&self) -> Option<rightkit_agent_policy::PermissionMode> {
        use rightkit_agent_policy::PermissionMode;
        if self.ask {
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
