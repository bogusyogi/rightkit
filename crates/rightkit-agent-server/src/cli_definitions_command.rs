#[path = "cli_definitions_command_run.rs"]
#[allow(dead_code)]
mod cli_definitions_command_run;

#[derive(Parser, Debug)]
// `disable_help_subcommand` is required: `Command::Help` below is an explicit
// registry-rendered variant, and clap's auto-generated `help` subcommand would
// collide with it. `--help` is unaffected.
#[command(
    name = "coderight",
    version,
    about = "RightKit agent engine",
    after_help = "Launch flags (interactive session, no subcommand):\n  \
        --ask | --accept-edits | --auto | --yolo   start in that permission mode (mutually exclusive)\n  \
        --model <provider>/<model>                  start on that route for this run only\n  \
        -p, --print <prompt>                        run one non-interactive turn and print the answer",
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum Command {
    /// Run first-use terminal onboarding.
    Onboard,
    /// Run the HTTP daemon (what Tauri spawns). Prints the startup contract line first.
    Serve {
        /// Deprecated and ignored. When context is unavailable the turn runs visibly degraded.
        #[arg(long, value_enum, default_value_t = cli_definitions_command_run::ContextMode::Membrane, hide = true)]
        context_mode: cli_definitions_command_run::ContextMode,
        /// Port to bind (0 = OS-assigned).
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// Override the SQLite database path.
        #[arg(long)]
        db: Option<String>,
        /// Model slug. Defaults to the selected provider's catalog default.
        #[arg(long)]
        model: Option<String>,
        /// Provider id: openrouter (default).
        #[arg(long)]
        provider: Option<String>,
        /// Stay resident after the last client detaches (installed-service
        /// launches only; interactive daemons keep the linger window).
        #[arg(long)]
        resident: bool,
    },
    /// Run a one-shot agent session in the terminal.
    #[command(visible_alias = "exec")]
    Run {
        #[command(flatten)]
        args: cli_definitions_command_run::RunArgs,
    },
    /// Preflight: check whether the engine is ready to run (no models executed).
    Doctor {
        /// Emit the report as JSON (for the onboarding UI / scripts).
        #[arg(long)]
        json: bool,
        /// Include per-dimension evidence (sub-checks) in the report.
        #[arg(short, long)]
        verbose: bool,
        /// Copy the rendered report to the OS clipboard (stdout still prints
        /// the report).
        #[arg(short = 'c', long)]
        clipboard: bool,
    },
    /// Print the merged configuration.
    Config,
    /// Trust a workspace folder for this user (defaults to the current directory).
    Trust {
        /// Workspace directory to trust.
        path: Option<String>,
    },
    /// Show daemon status (running? which port? version?).
    Status {
        /// Accepted for parity; status already emits JSON.
        #[arg(long)]
        json: bool,
    },
    /// Stop the running daemon. DL-014: a new flat top-level verb mirroring
    /// `serve`/`status`/`ps` — the `service` subcommand's vocabulary
    /// deliberately excludes daemon-runtime verbs (see its doc comment), so
    /// this stays outside that group rather than becoming `service stop`.
    Stop {
        /// Skip the cooperative shutdown request and terminate immediately.
        #[arg(long, short = 'f')]
        force: bool,
    },
    /// Inspect or apply versioned daemon-state migrations for one database.
    Migrate {
        #[command(subcommand)]
        command: MigrateCommand,
    },
    /// Print CLI, daemon, and local runtime information.
    Info {
        /// Emit structured JSON.
        #[arg(long)]
        json: bool,
    },
    /// Generate shell completion script to stdout.
    Completion {
        /// Shell to generate completions for.
        shell: CompletionShell,
    },
    /// Inspect remote-control integrations such as Telegram.
    Remote {
        #[command(subcommand)]
        command: RemoteCommand,
    },
    /// Validate, render, run, and deeplink portable recipes/runbooks.
    Recipe {
        #[command(subcommand)]
        command: RecipeCommand,
    },
    /// Inspect and control workspace schedules through the running daemon.
    #[command(alias = "schedules")]
    Schedule {
        #[command(subcommand)]
        command: ScheduleCommand,
    },
    /// Supervise one daemon-owned browser workspace for a session.
    #[command(alias = "web")]
    Browser {
        #[command(subcommand)]
        command: BrowserCommand,
    },
    /// Manage saved Bot profiles through daemon-owned workflow storage.
    #[command(alias = "bots")]
    Bot {
        #[command(subcommand)]
        command: BotCommand,
    },
    /// Create, inspect, analyze, and trace feature specs.
    Spec {
        #[command(subcommand)]
        command: SpecCommand,
    },
    /// Govern local skills, proposals, benchmark gates, and SkillOpt training runs.
    Skill {
        #[command(subcommand)]
        command: SkillCommand,
    },
    /// Approve or reject governed learning proposals through the running daemon.
    Learning {
        #[command(subcommand)]
        command: LearningCommand,
    },
    /// Mine exported session evidence for report-only recurring failure Insights.
    Insights {
        /// Mine these completed sessions before returning Insights.
        #[arg(long = "session")]
        session_ids: Vec<String>,
        /// Adapt issue scope.
        #[arg(long, default_value = "workspace")]
        scope: String,
        /// Minimum recurrence count when mining sessions.
        #[arg(long, default_value_t = 2)]
        min_recurrence: u32,
    },
    /// Watch diagnostics and create redacted support bundles.
    Diagnostics {
        #[command(subcommand)]
        command: DiagnosticsCommand,
    },
    /// List RightKit tool capabilities from the real runtime registry.
    Caps {
        /// Emit structured JSON instead of markdown.
        #[arg(long)]
        json: bool,
    },
    /// List active and dormant background sessions (alias: sessions).
    Ps {
        /// Accepted for parity; ps already emits JSON.
        #[arg(long)]
        json: bool,
    },
    /// List active and dormant background sessions (alias for ps).
    Sessions {
        /// Accepted for parity; sessions already emits JSON.
        #[arg(long)]
        json: bool,
    },
    /// List background sessions or inspect agents for one session.
    Agents {
        /// Optional session ID whose agents should be inspected.
        session_id: Option<String>,
        /// Emit structured JSON.
        #[arg(long)]
        json: bool,
        /// Include archived/deleted sessions in the list view.
        #[arg(long)]
        all: bool,
        /// Limit the list view to the current working directory.
        #[arg(long)]
        cwd: bool,
    },
    /// Resume a dormant session and forward stdin/stdout interactively.
    Resume {
        /// Session ID to resume.
        #[arg(required_unless_present = "last")]
        session_id: Option<String>,
        /// Resume the most recently updated session for the current workspace.
        #[arg(long, conflicts_with = "session_id")]
        last: bool,
        /// With --last, include archived and deleted sessions.
        #[arg(long, requires = "last")]
        all: bool,
        /// With --last, search across all workspaces instead of only the current directory.
        #[arg(long, requires = "last")]
        all_workspaces: bool,
    },
    /// Fork a session into a new idle session with parent lineage.
    Fork {
        /// Session ID to fork.
        #[arg(required_unless_present = "last")]
        session_id: Option<String>,
        /// Fork the most recently updated session for the current workspace.
        #[arg(long, conflicts_with = "session_id")]
        last: bool,
        /// With --last, include archived and deleted sessions.
        #[arg(long, requires = "last")]
        all: bool,
        /// With --last, search across all workspaces instead of only the current directory.
        #[arg(long, requires = "last")]
        all_workspaces: bool,
        /// Title for the forked session.
        #[arg(long)]
        title: Option<String>,
        /// Accepted for parity; fork already emits JSON.
        #[arg(long)]
        json: bool,
    },
    /// Manage daemon sessions.
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// Tail SSE events for a background session.
    Logs {
        /// Session ID to stream logs from.
        session_id: String,
    },
    /// Resume a dormant session and forward stdin/stdout interactively.
    Attach {
        /// Session ID to attach to.
        session_id: String,
    },
    /// Run a multi-agent conductor session against a goal (headless), streaming
    /// progress events. Requires a running daemon (`coderight serve`).
    Conduct {
        /// The goal for the conductor to decompose and execute.
        goal: String,
        /// Plan and print the projected cost mix without spawning workers.
        #[arg(long)]
        dry_run: bool,
        /// Create the daemon session without user plugins, hooks, or persistent permission rules.
        #[arg(long = "safe-mode", alias = "safe")]
        safe_mode: bool,
        /// Workspace directory for the run (defaults to the current directory).
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Interrupt and kill a running session.
    Kill {
        /// Session ID to kill.
        session_id: String,
    },
    /// Print a machine-readable attach payload for IDE, web, mobile, or remote clients.
    #[command(aliases = ["bridge", "ide", "desktop", "mobile", "teleport"])]
    Handoff {
        /// Session ID to hand off.
        session_id: String,
    },
    /// Desktop app handoff helpers.
    App {
        #[command(subcommand)]
        command: AppCommand,
    },
    /// Export a session transcript from the daemon.
    #[command(alias = "transcript")]
    Export {
        /// Session ID to export.
        session_id: String,
        /// Emit structured JSON instead of markdown.
        #[arg(long)]
        json: bool,
    },
    /// Show git status plus unstaged or staged diff for a workspace.
    Diff {
        /// Workspace/repository path (default: current directory).
        workspace: Option<String>,
        /// Show staged changes instead of unstaged changes.
        #[arg(long)]
        staged: bool,
    },
    /// Run a read-only auditor review of the current git changes through the conductor.
    Review {
        /// Optional review focus, for example "security" or "test coverage".
        focus: Vec<String>,
        /// Session to read when using `coderight review findings`.
        #[arg(long)]
        session: Option<String>,
        /// Emit structured JSON for `coderight review findings`.
        #[arg(long)]
        json: bool,
        /// Review changes against a base git ref.
        #[arg(long)]
        base: Option<String>,
        /// Limit review scope to a directory/path. Repeat for multiple scopes.
        #[arg(long = "dir")]
        dirs: Vec<String>,
        /// Create the daemon review session without user plugins, hooks, or persistent permission rules.
        #[arg(long = "safe-mode", alias = "safe")]
        safe_mode: bool,
        /// Model for the review session; forwarded to the daemon.
        #[arg(long)]
        model: Option<String>,
        /// Legacy single-agent option; rejected by auditor review.
        #[arg(long)]
        provider: Option<String>,
        /// Workspace root directory (default: current directory).
        #[arg(long)]
        workspace: Option<String>,
        /// Auto-approve edit-class actions in the review session.
        #[arg(long, alias = "yes")]
        auto_approve: bool,
        /// Legacy single-agent option; rejected by auditor review.
        #[arg(long)]
        stream_json: bool,
        /// Legacy single-agent option; rejected by auditor review.
        #[arg(long)]
        max_turns: Option<u32>,
        /// Legacy single-agent option; rejected by auditor review.
        #[arg(long)]
        max_budget_usd: Option<f64>,
    },
    /// Manage MCP servers through the running daemon.
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    /// Manage local RightKit plugins through the running daemon.
    Plugin {
        #[command(subcommand)]
        command: PluginCommand,
    },
    /// Inspect and adjust live session permissions through the running daemon.
    #[command(alias = "permissions")]
    Permission {
        #[command(subcommand)]
        command: PermissionCommand,
    },
    /// Grouped command help, rendered from the single command registry.
    #[command(alias = "?")]
    Help {
        /// Show every command, not just the common group.
        #[arg(long)]
        all: bool,
    },
    /// Inspect and configure provider setup through the running daemon.
    Provider {
        #[command(subcommand)]
        command: ProviderCommand,
    },
    /// Inspect local model readiness through the running daemon.
    #[command(name = "local-model", alias = "local-models")]
    LocalModel {
        #[command(subcommand)]
        command: LocalModelCommand,
    },
    /// Print the models a configured provider can serve, one per line —
    /// the live account catalog for a ChatGPT sign-in provider, else the
    /// canonical provider table. Exits non-zero on a typed fetch error.
    Models {
        /// Provider id (default: the configured provider).
        #[arg(long)]
        provider: Option<String>,
    },
    /// Inspect daemon hook readiness without exposing hook values.
    #[command(alias = "hook")]
    Hooks {
        #[command(subcommand)]
        command: HooksCommand,
    },
    /// Inspect available toolset profiles and gateway readiness.
    #[command(alias = "toolsets")]
    Toolset {
        #[command(subcommand)]
        command: ToolsetCommand,
    },
    /// Inspect execution backend readiness through the running daemon.
    Execution {
        #[command(subcommand)]
        command: ExecutionCommand,
    },
    /// Installed-runtime acceptance controls. Intended for RightKit's matrix,
    /// not for synthetic fixture validation.
    CliRuntime {
        #[command(subcommand)]
        command: CliRuntimeCommand,
    },
    /// Run RightKit as a stdio MCP server exposing its tool registry.
    McpServer {
        /// Workspace root exposed to tools (default: current directory).
        #[arg(long)]
        workspace: Option<String>,
        /// Expose mutating tools as well as read-only tools.
        #[arg(long)]
        allow_mutate: bool,
        /// Bind this MCP process to a live RightKit session capability
        /// gateway. Secrets are read from RIGHTKIT_DAEMON_TOKEN and
        /// RIGHTKIT_GATEWAY_TOKEN, never command-line arguments.
        #[arg(long)]
        session_id: Option<String>,
    },
    /// Run RightKit as an Agent Client Protocol (ACP) agent over stdio.
    /// Bridges to an already-running daemon's session API; start `coderight
    /// serve` first and pass its port here.
    #[command(alias = "acp")]
    AcpServer {
        /// Local daemon HTTP port to bridge ACP sessions/messages through.
        #[arg(long)]
        port: u16,
        /// Daemon auth token (same value printed by `coderight serve`'s
        /// startup contract line). Omit only against a daemon started with
        /// auth disabled.
        #[arg(long)]
        token: Option<String>,
    },
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum MigrateCommand {
    /// Report schema version plus any crash-recovery migration journal.
    Status {
        /// SQLite database to inspect. This is required to avoid selecting an
        /// unrelated daemon database.
        #[arg(long)]
        db: String,
    },
    /// Apply pending migrations through the daemon migration wrapper.
    Apply {
        /// SQLite database to migrate. This is required to avoid selecting an
        /// unrelated daemon database.
        #[arg(long)]
        db: String,
    },
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum CliRuntimeCommand {
    /// Execute one real authenticated-CLI matrix scenario through daemon runtime.
    MatrixCell {
        #[arg(long)]
        provider: String,
        #[arg(long)]
        scenario: String,
        #[arg(long)]
        workspace: String,
        #[arg(long)]
        child_provider: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum BrowserCommand {
    /// Show browser sessions, control owner, URL, & recent activity.
    Status { session_id: String },
    /// Open a daemon-configured local browser session at URL. Headless is the
    /// daemon default; visible takes its existing human-control lease after
    /// opening rather than creating a second browser state.
    Open {
        session_id: String,
        url: String,
        #[arg(long, conflicts_with = "visible")]
        headless: bool,
        #[arg(long, conflicts_with = "headless")]
        visible: bool,
    },
    /// Inspect daemon-owned browser workspace state, including sessions,
    /// control lease, URL, policy, & latest activity.
    Inspect { session_id: String },
    /// Execute one browser action already exposed by daemon workspace routes.
    /// Unsupported CDP actions are intentionally not invented by this CLI.
    Act {
        session_id: String,
        browser_session_id: String,
        /// navigate, reload, back, or forward
        action: String,
        /// Required only by navigate.
        #[arg(long)]
        url: Option<String>,
    },
    /// Navigate existing browser session through daemon policy.
    Navigate {
        session_id: String,
        browser_session_id: String,
        url: String,
    },
    /// Capture a screenshot artifact from current browser session.
    Screenshot {
        session_id: String,
        browser_session_id: String,
    },
    /// Take exclusive human control lease.
    Take { session_id: String },
    /// Release human control lease back to runtime.
    Release { session_id: String },
    /// Close one browser session.
    Close {
        session_id: String,
        browser_session_id: String,
    },
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum BotCommand {
    /// List saved Bot profiles.
    List,
    /// Show one full Bot profile.
    Show { id: String },
    /// Create a Bot from a BotProfileV1 JSON file.
    Create { profile: String },
    /// Replace one Bot profile from a BotProfileV1 JSON file.
    Edit { id: String, profile: String },
    /// Import an authority-free Bot package JSON file.
    Import { package: String },
    /// Export authority-free Bot package JSON.
    Export { id: String },
    /// Start one fresh isolated Bot task now.
    Run {
        id: String,
        #[arg(long)]
        workspace_root: Option<String>,
    },
    /// Disable future Bot runs while retaining profile & schedule history.
    Pause { id: String },
    /// Re-enable a previously paused Bot profile.
    Resume { id: String },
}

#[cfg(test)]
mod command_group_tests {
    use super::*;
    #[test]
    fn grouped_help_and_headless_run_options_are_real_cli_variants() {
        let help = Cli::try_parse_from(["coderight", "help", "--all"])
            .expect("registry help command must parse");
        assert!(matches!(help.command, Command::Help { all: true }));

        let run = Cli::try_parse_from([
            "coderight",
            "run",
            "--print",
            "--output-schema",
            "schema.json",
        ])
        .expect("headless run contract must parse");
        assert!(matches!(
            run.command,
            Command::Run { args } if args.print && args.output_schema.as_deref() == Some("schema.json")
        ));
    }
}
