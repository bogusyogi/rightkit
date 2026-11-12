#[derive(Subcommand, Debug, PartialEq)]
pub enum RemoteCommand {
    /// Print secret-redacted Telegram remote setup and runtime status.
    Status,
    /// Print pass/warn/block readiness checks for Telegram remote control.
    Test,
    /// Persist Telegram remote credentials to the user config.
    Setup {
        /// Bot token from Telegram BotFather. Omit with --clear-token to remove it.
        #[arg(long)]
        telegram_token: Option<String>,
        /// Telegram user ids allowed to interact with the bot. May be repeated or comma-separated.
        #[arg(long = "allowed-user", value_delimiter = ',')]
        allowed_users: Vec<u64>,
        /// Remove the saved Telegram bot token.
        #[arg(long)]
        clear_token: bool,
        /// Enable the remote web surface in config.
        #[arg(long)]
        enable_web: bool,
        /// Disable the remote web surface in config.
        #[arg(long, conflicts_with = "enable_web")]
        disable_web: bool,
    },
    /// Validate the configured Telegram bot token against the Telegram Bot API.
    Validate {
        /// Override the configured BotFather token for this check only.
        #[arg(long)]
        telegram_token: Option<String>,
        /// Telegram API base URL. Defaults to https://api.telegram.org.
        #[arg(long, default_value = "https://api.telegram.org")]
        api_base: String,
    },
    /// List recent Telegram sender ids from getUpdates to help build the allowlist.
    Users {
        /// Override the configured BotFather token for this check only.
        #[arg(long)]
        telegram_token: Option<String>,
        /// Telegram API base URL. Defaults to https://api.telegram.org.
        #[arg(long, default_value = "https://api.telegram.org")]
        api_base: String,
        /// Maximum recent update rows to inspect.
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Wait for live Telegram daemon readiness and an allowed chat message.
    Smoke {
        /// Maximum seconds to wait for daemon readiness and an allowed Telegram chat.
        #[arg(long, default_value_t = 120)]
        timeout_seconds: u64,
        /// Seconds between daemon readiness polls.
        #[arg(long, default_value_t = 5)]
        poll_interval_seconds: u64,
        /// Pass when long-poll readiness is green even if no chat has messaged the bot yet.
        #[arg(long)]
        no_require_chat: bool,
    },
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum RecipeCommand {
    /// List installed recipes from the local recipe store.
    List {
        /// Override the recipe store path. Defaults beside the daemon DB.
        #[arg(long)]
        store: Option<String>,
        /// Emit structured JSON instead of a text table.
        #[arg(long)]
        json: bool,
    },
    /// Validate an installed recipe id or local JSON/YAML recipe file.
    Validate {
        /// Installed recipe id or path to a .json/.yaml/.yml recipe file.
        target: String,
        /// Override the recipe store path. Defaults beside the daemon DB.
        #[arg(long)]
        store: Option<String>,
        /// Emit structured JSON instead of a text summary.
        #[arg(long)]
        json: bool,
    },
    /// Render an installed recipe id or local JSON/YAML recipe file.
    Render {
        /// Installed recipe id or path to a .json/.yaml/.yml recipe file.
        target: String,
        /// Recipe parameter in key=value form. May be repeated.
        #[arg(short = 'p', long = "param")]
        params: Vec<String>,
        /// Override the recipe store path. Defaults beside the daemon DB.
        #[arg(long)]
        store: Option<String>,
        /// Emit structured JSON instead of the rendered prompt.
        #[arg(long)]
        json: bool,
    },
    /// Run an installed recipe id or local JSON/YAML recipe file as a one-shot CLI session.
    Run {
        /// Installed recipe id or path to a .json/.yaml/.yml recipe file.
        target: String,
        /// Recipe parameter in key=value form. May be repeated.
        #[arg(short = 'p', long = "param")]
        params: Vec<String>,
        /// Override the recipe store path. Defaults beside the daemon DB.
        #[arg(long)]
        store: Option<String>,
        /// Model slug. Defaults to recipe settings, then the normal CLI default.
        #[arg(long)]
        model: Option<String>,
        /// Provider id: openrouter.
        #[arg(long, default_value = "openrouter")]
        provider: String,
        /// Workspace root directory. Defaults to recipe settings, then current directory.
        #[arg(long)]
        workspace: Option<String>,
        /// Allow all mutating tools without prompting.
        #[arg(long, alias = "yes")]
        auto_approve: bool,
        /// Print events as JSONL to stdout.
        #[arg(long)]
        stream_json: bool,
        /// Maximum number of agent turns.
        #[arg(long)]
        max_turns: Option<u32>,
        /// Named toolset profile to apply as a capability ceiling.
        #[arg(long)]
        toolset: Option<String>,
        /// Budget ceiling in USD.
        #[arg(long)]
        max_budget_usd: Option<f64>,
    },
    /// Create a {app}:// recipe deeplink.
    Deeplink {
        /// Installed recipe id or path to a .json/.yaml/.yml recipe file.
        target: String,
        /// Recipe parameter in key=value form. May be repeated.
        #[arg(short = 'p', long = "param")]
        params: Vec<String>,
        /// Deeplink action: run, render, or schedule.
        #[arg(long, default_value = "run")]
        action: String,
        /// Override the recipe store path. Defaults beside the daemon DB.
        #[arg(long)]
        store: Option<String>,
        /// Emit structured JSON instead of only the URL.
        #[arg(long)]
        json: bool,
    },
}
