#[path = "cli_definitions_command_run.rs"]
mod cli_definitions_command_run;

#[derive(Parser, Debug)]
// `disable_help_subcommand` is required: `Command::Help` below is an explicit
// registry-rendered variant, and clap's auto-generated `help` subcommand would
// collide with it. `--help` is unaffected.
#[command(
    name = crate::host_identity::app(),
    version,
    about = "{APP_NAME} agent engine",
    after_help = "Launch flags (interactive session, no subcommand):\n  \
        --ask | --accept-edits | --auto | --yolo   start in that permission mode (mutually exclusive)\n  \
        --model <provider>/<model>                  start on that route for this run only\n  \
        -p, --print <prompt>                        run one non-interactive turn and print the answer",
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Perform the call. A command whose approval is `yes` (the permanent
    /// removals and the write verbs) is a dry run without `--yes`: it prints a
    /// `dryRun` receipt and changes nothing. `run`, `exec` and `-p` are the
    /// work with no flag (`--dry-run` is their receipt), so `--yes` changes
    /// nothing there. `--yes` never changes the permission mode of tool calls
    /// inside a run.
    #[arg(long, global = true)]
    pub yes: bool,
    /// A label for the caller of this call (a tool or script name). It is
    /// journaled as a claim, never used as an identity.
    #[arg(long, global = true, value_name = "LABEL")]
    pub caller: Option<String>,
    /// Override one config key for this call (repeatable): `-c model.small-model='"x"'`. The value is
    /// TOML, or plain text when it is not valid TOML. It beats the user and workspace files and is
    /// never saved. Also accepted after the command; the CLI moves it here.
    #[arg(
        short = 'c',
        long = "config",
        value_name = "KEY=VALUE",
        action = clap::ArgAction::Append
    )]
    pub config_override: Vec<String>,
    /// Do not read the user's config.toml (the workspace file and `-c` still apply).
    #[arg(long = "ignore-user-config", global = true)]
    pub ignore_user_config: bool,
    /// Exit 2 when a config file has a key no setting reads, or cannot be parsed.
    #[arg(long = "strict-config", global = true)]
    pub strict_config: bool,
    /// Load no instruction files (CLAUDE.md, AGENTS.md, right.md) into the prompt. Permission
    /// rules are a safety layer and still apply.
    #[arg(long = "ignore-rules", global = true)]
    pub ignore_rules: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum Command {
    /// Print the machine-readable command manifest (tools, effects, approvals, exit codes).
    Contract {
        /// Print one JSON line instead of the indented document.
        #[arg(long)]
        json: bool,
    },
    /// Sign in to a service. `login github` and an OAuth provider need a person at a terminal;
    /// `login PROVIDER --with-api-key` reads an API key from stdin instead.
    Login {
        /// `github` (device flow), an OAuth provider, or a provider that takes an API key.
        provider: String,
        /// Read the provider's API key from stdin (the first line; never from argv) and store it
        /// the way `provider set-key` does. A dry run without `--yes`.
        #[arg(long = "with-api-key")]
        with_api_key: bool,
    },
    /// Remove a stored sign-in: GitHub's, a provider's OAuth sign-in or its stored API key.
    Logout {
        /// `github` or a provider id.
        provider: String,
    },
    /// Import another tool's chats for this workspace into {APP_NAME} sessions (text of each turn
    /// only; no model is called). Without `--yes` it only lists what it would import.
    Import {
        /// The tool whose chats to import.
        #[arg(value_enum)]
        source: ImportSource,
        /// List what would be imported and change nothing, even with `--yes`.
        #[arg(long)]
        dry_run: bool,
        /// Print one JSON value instead of lines.
        #[arg(long)]
        json: bool,
        /// Workspace whose chats to import (default: current directory).
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Apply the changes a `run --worktree` session made in its worktree to this checkout
    /// (a `git apply` of the worktree's diff; nothing is committed).
    Apply {
        /// The worktree name (the one `run --worktree NAME` printed). Default: the newest.
        name: Option<String>,
        /// Repository whose worktrees to look in (default: current directory).
        #[arg(long)]
        workspace: Option<String>,
    },
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
        /// Also listen on the network address for guest links (`{app}
        /// share`); the owner token is refused there. Plain HTTP.
        #[arg(long)]
        lan: bool,
        /// Port for the LAN listener (0 = OS-assigned).
        #[arg(long, default_value_t = 0, requires = "lan")]
        lan_port: u16,
        /// Do not advertise the LAN listener over mDNS (`_{app}._tcp`).
        #[arg(long, requires = "lan")]
        no_mdns: bool,
    },
    /// Run a one-shot agent session in the terminal.
    #[command(visible_alias = "exec")]
    Run {
        #[command(flatten)]
        args: Box<cli_definitions_command_run::RunArgs>,
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
        /// Show only the checks that need the network: whether the configured provider answers,
        /// and whether a local socket can be bound. The verdict covers those alone.
        #[arg(long = "network-only")]
        network_only: bool,
    },
    /// Print the merged configuration.
    Config {
        /// Print the JSON Schema of config.toml (the same text as
        /// schemas/{app}-config.v1.schema.json) instead of the merged configuration.
        #[arg(long)]
        schema: bool,
    },
    /// Decide a shell command with the permission policy without running it.
    Execpolicy {
        #[command(subcommand)]
        command: ExecpolicyCommand,
    },
    /// List the feature flags and their state.
    Features {
        #[command(subcommand)]
        command: FeaturesCommand,
    },
    /// Debugging aids: print what the model is sent.
    Debug {
        #[command(subcommand)]
        command: DebugCommand,
    },
    /// Trust a workspace folder for this user (defaults to the current directory).
    Trust {
        /// Workspace directory to trust.
        path: Option<String>,
    },
    /// Show daemon status (running? which port? version?).
    Status {
        /// Accepted for scripts; the status output is always JSON.
        #[arg(long)]
        json: bool,
    },
    // DL-014: a flat top-level verb mirroring `serve`/`status`/`ps`; the
    // `service` subcommand's vocabulary excludes daemon-runtime verbs.
    /// Stop the running daemon (asks it to shut down cleanly; `--force` ends it at once).
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
    /// List {APP_NAME} tool capabilities from the real runtime registry.
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
    /// JSON-RPC 2.0 session over stdio (`initialize`, `prompt`, `abort`, `ping`, `shutdown`,
    /// `approval/respond`; every session event as an `event` notification, plus the
    /// `approval/request` and `approval/expired` notifications). Trailing flags are `run` flags.
    Rpc {
        /// Seconds an approval request waits for `approval/respond` before it is denied (default 300).
        #[arg(long = "approval-timeout-secs")]
        approval_timeout_secs: Option<u64>,
        /// `run` flags for the session (for example `--model`, `--permission-mode`).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        run_args: Vec<String>,
    },
    /// Guest links: let someone follow one session (and optionally answer its
    /// approvals) in their browser, without your daemon token.
    Share {
        #[command(subcommand)]
        command: ShareCommand,
    },
    /// Use {APP_NAME} on another machine over SSH: starts or finds its daemon,
    /// forwards it to a local port and prints the browser-client URL. For the
    /// terminal UI, run `ssh -t <destination> {app}`.
    Ssh {
        /// SSH destination (`user@host` or a `~/.ssh/config` host alias).
        destination: String,
        /// Local port for the forward (default: a free one).
        #[arg(long)]
        port: Option<u16>,
        /// SSH port on the remote machine.
        #[arg(long)]
        ssh_port: Option<u16>,
        /// Extra `ssh -o` option, repeatable (for example `IdentityFile=~/.ssh/work`).
        #[arg(long = "ssh-option")]
        ssh_option: Vec<String>,
        /// The {APP_NAME} command on the remote machine.
        #[arg(long, default_value = crate::host_identity::app())]
        remote_bin: String,
        /// Also open the URL in the default browser.
        #[arg(long)]
        open: bool,
    },
    /// Find {APP_NAME} daemons advertising on the local network (mDNS).
    Discover {
        /// Seconds to listen.
        #[arg(long, default_value_t = 3)]
        timeout: u64,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Print the browser client URL for the running daemon (starting one if
    /// needed). The URL carries the daemon token in its fragment; treat it as
    /// a credential.
    Web {
        /// Also open the URL in the default browser.
        #[arg(long)]
        open: bool,
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
        /// Branch from just before the user turn with this event seq (a turn-scoped
        /// fork); omit it to copy the whole session. A user turn's seq is on its
        /// `userMessageSubmitted` event (`GET /sessions/{id}/events/history`, `{app} logs`).
        #[arg(long = "before-seq", value_name = "SEQ")]
        before_seq: Option<i64>,
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
    /// progress events. Requires a running daemon (`{app} serve`).
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
    /// Queue a message for a running session: it is delivered to the model after the running
    /// turn, never inside it. Returns at once with the queued item id.
    Queue {
        /// Session ID to queue the message for.
        session_id: String,
        /// The message to queue.
        #[arg(long)]
        message: String,
        /// Resume the session first when it is dormant (not live in the daemon).
        #[arg(long)]
        resume: bool,
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
        /// Session to read when using `{app} review findings`.
        #[arg(long)]
        session: Option<String>,
        /// Emit structured JSON for `{app} review findings`.
        #[arg(long)]
        json: bool,
        /// Review changes against a base git ref.
        #[arg(long)]
        base: Option<String>,
        /// Review what is not committed yet: the working tree and the index against HEAD.
        #[arg(long, conflicts_with_all = ["base", "commit"])]
        uncommitted: bool,
        /// Review one commit (its `git show`) instead of the current changes.
        #[arg(long, value_name = "SHA", conflicts_with_all = ["base", "uncommitted"])]
        commit: Option<String>,
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
        // The twin's `--yes` alias of this flag is the global `--yes` now (`Cli::yes`); a host that
        // sets `host_options::yes_flag_is_auto_approve` reads it as this flag.
        #[arg(long)]
        auto_approve: bool,
        /// Permission mode of the review session: plan, ask, accept-edits or
        /// auto-safe (`bypass` is `run` only).
        #[arg(long = "permission-mode", value_enum)]
        permission_mode: Option<CliPermissionMode>,
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
    /// Manage local {APP_NAME} plugins through the running daemon.
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
        /// Print one JSON object `{schema_version, provider, models}`.
        #[arg(long)]
        json: bool,
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
    /// Installed-runtime acceptance controls. Intended for the host's matrix,
    /// not for synthetic fixture validation.
    CliRuntime {
        #[command(subcommand)]
        command: CliRuntimeCommand,
    },
    /// Run {APP_NAME} as a stdio MCP server exposing its tool registry.
    McpServer {
        /// Workspace root exposed to tools (default: current directory).
        #[arg(long)]
        workspace: Option<String>,
        /// Expose mutating tools as well as read-only tools.
        #[arg(long)]
        allow_mutate: bool,
        /// Bind this MCP process to a live {APP_NAME} session capability
        /// gateway. Secrets are read from {ENV_PREFIX}_DAEMON_TOKEN and
        /// {ENV_PREFIX}_GATEWAY_TOKEN, never command-line arguments.
        #[arg(long)]
        session_id: Option<String>,
    },
    /// Run {APP_NAME} as an Agent Client Protocol (ACP) agent over stdio.
    /// Bridges to an already-running daemon's session API; start `{app}
    /// serve` first and pass its port here.
    #[command(alias = "acp")]
    AcpServer {
        /// Local daemon HTTP port to bridge ACP sessions/messages through.
        #[arg(long)]
        port: u16,
        /// Daemon auth token (same value printed by `{app} serve`'s
        /// startup contract line). Omit only against a daemon started with
        /// auth disabled.
        #[arg(long)]
        token: Option<String>,
    },
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum ExecpolicyCommand {
    /// Print the policy decision (`allow`, `ask` or `deny`) for a shell command, with the reason
    /// and the rule that decided it. Nothing runs. Exit 0 whatever the decision.
    Check {
        /// Permission mode to decide under.
        #[arg(long, value_enum, default_value = "auto-safe")]
        mode: cli_definitions_command_run::CliPermissionMode,
        /// Workspace trust to decide under: `trusted`, `untrusted`, `sandboxed` or `locked`.
        #[arg(long, default_value = "trusted")]
        trust: String,
        /// Print one JSON value instead of a line.
        #[arg(long)]
        json: bool,
        /// The command, after `--` (the words are joined with spaces and read as shell text).
        #[arg(last = true, required = true, value_name = "COMMAND")]
        command: Vec<String>,
    },
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum FeaturesCommand {
    /// List every feature flag with whether it is on and whether that is its default.
    List {
        /// Print one JSON value instead of lines.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum DebugCommand {
    /// Print the system prompt a new session in this workspace would send the model, composed by the
    /// same function a session uses. Parts that exist only inside a running session (learned
    /// instincts, an agent role) are not included; `--json` lists them under `omitted`.
    #[command(name = "prompt-input")]
    PromptInput {
        /// Print one JSON value instead of the text.
        #[arg(long)]
        json: bool,
        /// Treat the workspace as trusted for this call (as `run --trust-workspace`), so its
        /// instruction files are included.
        #[arg(long = "trust-workspace")]
        trust_workspace: bool,
        /// Workspace to compose for (default: current directory).
        #[arg(long)]
        workspace: Option<String>,
    },
}

/// A tool whose chats `{app} import` can read.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportSource {
    /// Claude Code project transcripts (`~/.claude/projects/<project>/<session>.jsonl`).
    ClaudeCode,
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

#[derive(clap::Subcommand, Debug, PartialEq)]
pub enum ShareCommand {
    /// Mint a guest link for one session. Prints the link once.
    Create {
        /// Session id.
        session_id: String,
        /// Lifetime: seconds, or a number with s, m, h or d (default 1h, at most 7d).
        #[arg(long, default_value = "1h")]
        expires: String,
        /// Let the guest answer this session's tool approvals.
        #[arg(long)]
        approve: bool,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// List live guest links (never their tokens).
    List,
    /// Revoke a guest link now.
    Revoke {
        /// Share id from `share list`.
        share_id: String,
    },
}
