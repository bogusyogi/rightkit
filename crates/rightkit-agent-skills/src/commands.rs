//! Slash-command registry and execution framework.
//!
//! ## P1-1 / CR-24: this module is THE one command authority
//!
//! [`COMMAND_SPECS`] below is the single authoritative table for every verb
//! the host app exposes: canonical name, aliases, scope/effect/mid-turn
//! metadata, argument schema, help grouping, surface reachability, and —
//! for every row reachable from the daemon's HTTP slash API — the
//! executable handler itself (`daemon_handler`). One row owns metadata plus
//! executable identity; adding or renaming a command touches exactly this
//! table and nothing else.
//!
//! Dependency direction is `bins -> crates`, so the table lives HERE (in
//! `host_skills`) where both consumers can reach it without a cycle:
//!
//! - the binary's `host-app/src/command_registry.rs` is a thin view:
//!   it re-exports [`COMMAND_SPECS`] as `COMMAND_REGISTRY` and layers
//!   enforcement helpers (`enforce_mid_turn`, `CommandRouter`) on top. It
//!   holds no independent command-name table.
//! - the daemon's HTTP slash surface (`host-app/src/api_commands.rs`)
//!   lists and dispatches through this table: aliases are normalized to
//!   canonical names *before* execution (via [`resolve_query`]), and every
//!   row with a [`CommandSpec::daemon_handler`] has a live executor, so no
//!   recognized command can answer "recognized but no executor".

mod builtins;

use serde::{Deserialize, Serialize};

/// A parsed slash command from user input.
#[derive(Debug, Clone)]
pub struct SlashCommand {
    /// The command name (without the leading "/"), e.g. "help", "clear".
    pub name: String,
    /// Arguments after the command name, split by whitespace.
    pub args: Vec<String>,
    /// The raw input string.
    pub raw: String,
}

impl SlashCommand {
    /// Parse a line of user input into a `SlashCommand`.
    /// Returns `None` if the input doesn't start with "/".
    pub fn parse(input: &str) -> Option<Self> {
        let trimmed = input.trim();
        if !trimmed.starts_with('/') {
            return None;
        }
        let without_slash = &trimmed[1..];
        let mut parts = without_slash.split_whitespace();
        let name = parts.next()?.to_lowercase();
        let args: Vec<String> = parts.map(str::to_string).collect();
        Some(SlashCommand {
            name,
            args,
            raw: trimmed.to_string(),
        })
    }
}

/// The result of executing a slash command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResult {
    /// Whether the command executed successfully.
    pub success: bool,
    /// The output to display to the user (markdown).
    pub output: String,
    /// Whether the command should cause the UI to navigate (e.g. clear chat).
    pub action: CommandAction,
}

/// A side-effect action the UI should take after the command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandAction {
    /// No UI action needed — just display the output.
    None,
    /// Clear the chat transcript.
    ClearChat,
    /// Compact the current context.
    CompactContext,
    /// Switch to a different session (value is session ID).
    SwitchSession(String),
    /// List all available sessions.
    ListSessions,
    /// Submit a user message into the current session.
    SubmitMessage(String),
    /// Start a conductor orchestration run in the current session.
    ConductGoal(String),
    /// Run a memory dream consolidation pass.
    DreamNow(String),
}

// ── The shared spec model (P1-1: one table, both surfaces) ──────────────────

/// Where a command is allowed to run.
///
/// - `Session` — only meaningful inside a live interactive session (e.g.
///   `/model`, `/clear`).
/// - `Workspace` — operates on workspace-local state (e.g. `spec`, `diff`).
/// - `Daemon` — talks to the running daemon over HTTP (e.g. `session list`,
///   `mcp status`).
/// - `Client` — pure CLI/local behavior with no daemon or session
///   requirement (e.g. `completion`, `config`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandScope {
    Session,
    Workspace,
    Daemon,
    Client,
}

/// What a command does to state, for confirmation/rendering policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandEffect {
    Read,
    Mutate,
    Destructive,
}

/// A hint for how a renderer (CLI stdout, TUI transcript) should present a
/// command's output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendererHint {
    /// Plain text / markdown block.
    Text,
    /// Structured table (list-shaped output).
    Table,
    /// A modal picker (e.g. `/model` with no argument).
    Picker,
    /// Streamed/progressive output (long-running).
    Stream,
    /// No output beyond a status line.
    Status,
}

/// The help grouping a command renders under. `/help` shows `Common`;
/// `/help --all` shows `Common` and `Advanced`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpGroup {
    Common,
    Advanced,
}

/// A minimal, declarative argument schema shared by both surfaces. This is
/// intentionally not a full parser — clap already owns real CLI parsing;
/// this exists so a renderer (help text, TUI arg hints) can describe a
/// command without depending on clap's derive output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArgSpec {
    /// Argument name as it appears in usage text, e.g. "session_id" or "--json".
    pub name: &'static str,
    /// Human-readable description.
    pub help: &'static str,
    /// Required positional/flag vs optional.
    pub required: bool,
}

/// Which surface(s) expose a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Surfaces {
    pub cli: bool,
    pub interactive: bool,
}

pub(crate) const CLI_ONLY: Surfaces = Surfaces {
    cli: true,
    interactive: false,
};
pub(crate) const INTERACTIVE_ONLY: Surfaces = Surfaces {
    cli: false,
    interactive: true,
};
pub(crate) const BOTH: Surfaces = Surfaces {
    cli: true,
    interactive: true,
};

pub(crate) const NO_ARGS: &[ArgSpec] = &[];

/// The daemon-side slash executor signature. A plain function pointer so a
/// row can carry its own executor inside the `static` table.
pub type DaemonHandler = fn(&[String]) -> CommandResult;

/// One row of THE command registry: the single source of truth for a verb.
///
/// Every surface projects this row. `name` is canonical — slash-prefixed for
/// interactive commands (`"/model"`), bare subcommand paths for CLI verbs
/// (`"session list"`). `daemon_handler` is `Some` for every row reachable
/// from the daemon's HTTP slash API (a real executor or, where the work
/// genuinely belongs to another surface, an honest guidance answer) and
/// `None` only for CLI-only clap verbs that are not slash targets.
#[derive(Debug, Clone, Copy)]
pub struct CommandSpec {
    /// Canonical name. For CLI this is the subcommand path (e.g.
    /// `"session list"`); for a session-only command it's the slash form
    /// (e.g. `"/model"`).
    pub name: &'static str,
    /// Alternate names that resolve to the same entry. Slash forms include
    /// the leading `/`; CLI forms do not.
    pub aliases: &'static [&'static str],
    pub scope: CommandScope,
    pub effect: CommandEffect,
    /// Whether this command may run while a turn is in flight (mid-turn),
    /// as opposed to only between turns.
    pub valid_mid_turn: bool,
    pub args: &'static [ArgSpec],
    pub renderer: RendererHint,
    pub help_group: HelpGroup,
    /// One-line description for help text.
    pub summary: &'static str,
    /// Where this command is reachable: CLI subcommand, interactive slash
    /// command, or both.
    pub surfaces: Surfaces,
    /// Usage text for listings/help (`/model [model]`, `host app serve`, …).
    pub usage: &'static str,
    /// The daemon slash-API executor for this row, if it has one.
    pub daemon_handler: Option<DaemonHandler>,
}

/// Resolve an exact canonical name or alias (with or without a leading `/`)
/// to its registry entry. Exact-match core; prefer [`resolve_query`] at
/// dispatch points, which also accepts the bare form of a slash-canonical
/// name.
pub fn resolve(query: &str) -> Option<&'static CommandSpec> {
    COMMAND_SPECS
        .iter()
        .find(|entry| entry.name == query || entry.aliases.contains(&query))
}

/// Resolve ANY user-typed form — canonical, alias, bare, or slashed — to its
/// registry entry. This is the one place alias normalization happens; both
/// the daemon dispatcher and the binary's `CommandRouter` call this rather
/// than hand-rolling a match, so aliases are normalized identically before
/// listing, completion, gating, AND execution.
pub fn resolve_query(query: &str) -> Option<&'static CommandSpec> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return None;
    }
    resolve(trimmed).or_else(|| {
        if trimmed.starts_with('/') {
            None
        } else {
            resolve(&format!("/{trimmed}"))
        }
    })
}

/// Normalize a user-typed command word to the canonical BARE name (leading
/// `/` stripped) used by the daemon's dispatch arms. `None` when the word is
/// not a registered command (plugin and unknown names pass through).
pub fn canonical_bare_name(query: &str) -> Option<&'static str> {
    resolve_query(query).map(|entry| entry.name.trim_start_matches('/'))
}

/// All entries in a given help group, in registry order. Used to render
/// `/help` (common only) and `/help --all` (common + advanced) without
/// hand-maintaining a second list.
pub fn entries_in_group(group: HelpGroup) -> impl Iterator<Item = &'static CommandSpec> {
    COMMAND_SPECS
        .iter()
        .filter(move |entry| entry.help_group == group)
}

/// All entries reachable from a given surface (CLI or interactive).
pub fn entries_for_surface(cli: bool) -> impl Iterator<Item = &'static CommandSpec> {
    COMMAND_SPECS.iter().filter(move |entry| {
        if cli {
            entry.surfaces.cli
        } else {
            entry.surfaces.interactive
        }
    })
}

/// Semantic effects owned by interactive command grammar. TUI keeps its
/// public effect enum, but names, aliases, descriptions, and resolution
/// project from this canonical metadata instead of a second local grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractiveEffect {
    New,
    Reset,
    Rewind,
    Undo,
    Redo,
}

#[derive(Debug, Clone, Copy)]
pub struct InteractiveEffectSpec {
    pub effect: InteractiveEffect,
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub summary: &'static str,
}

pub static INTERACTIVE_EFFECT_SPECS: &[InteractiveEffectSpec] = &[
    InteractiveEffectSpec {
        effect: InteractiveEffect::New,
        name: "new",
        aliases: &["new", "clear", "cls"],
        summary: "Start a new session",
    },
    InteractiveEffectSpec {
        effect: InteractiveEffect::Reset,
        name: "reset",
        aliases: &["reset"],
        summary:
            "Cancel anything in flight and drop uncommitted edits, keeping this session's identity",
    },
    InteractiveEffectSpec {
        effect: InteractiveEffect::Rewind,
        name: "rewind",
        aliases: &["rewind"],
        summary: "Rewind to a prior turn boundary, discarding everything after it",
    },
    InteractiveEffectSpec {
        effect: InteractiveEffect::Undo,
        name: "undo",
        aliases: &["undo"],
        summary: "Undo the single most recent reversible action",
    },
    InteractiveEffectSpec {
        effect: InteractiveEffect::Redo,
        name: "redo",
        aliases: &["redo"],
        summary: "Redo the most recently undone action",
    },
];

pub fn resolve_interactive_effect(query: &str) -> Option<&'static InteractiveEffectSpec> {
    let normalized = query.trim().trim_start_matches('/').to_ascii_lowercase();
    INTERACTIVE_EFFECT_SPECS
        .iter()
        .find(|spec| spec.aliases.iter().any(|alias| *alias == normalized))
}

/// THE one command table. Add a command by adding one row here — metadata,
/// aliases, and (when the daemon slash API can run it) the executor all live
/// on the row.
pub static COMMAND_SPECS: &[CommandSpec] = &[
    // ---- sessions & lifecycle --------------------------------------
    CommandSpec {
        name: "session list",
        aliases: &["ps", "sessions", "/sessions", "/history"],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Read,
        valid_mid_turn: true,
        args: &[
            ArgSpec { name: "--status", help: "Filter by status.", required: false },
            ArgSpec { name: "--tag", help: "Filter by tag.", required: false },
            ArgSpec { name: "--json", help: "Emit JSON.", required: false },
        ],
        renderer: RendererHint::Table,
        help_group: HelpGroup::Common,
        summary: "List active and dormant background sessions.",
        surfaces: BOTH,
        usage: "/session list [--status] [--tag] [--json]",
        daemon_handler: Some(builtins::sessions_handler),
    },
    CommandSpec {
        name: "resume",
        aliases: &["attach", "/resume", "/continue"],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Mutate,
        valid_mid_turn: false,
        args: &[
            ArgSpec { name: "session_id", help: "Session ID to resume.", required: false },
            ArgSpec { name: "--last", help: "Resume the most recent session.", required: false },
        ],
        renderer: RendererHint::Stream,
        help_group: HelpGroup::Common,
        summary: "Resume a dormant session and forward stdin/stdout interactively.",
        surfaces: BOTH,
        usage: "/resume [session_id|--last]",
        daemon_handler: Some(builtins::resume_handler),
    },
    CommandSpec {
        name: "/agents",
        aliases: &[],
        scope: CommandScope::Session,
        effect: CommandEffect::Read,
        valid_mid_turn: true,
        args: NO_ARGS,
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Open the subagent picker: live status, read-only transcript, stop.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/agents",
        daemon_handler: Some(tui_surface_guidance_handler),
    },
    CommandSpec {
        name: "run",
        aliases: &["exec"],
        scope: CommandScope::Workspace,
        effect: CommandEffect::Mutate,
        valid_mid_turn: false,
        args: NO_ARGS,
        renderer: RendererHint::Stream,
        help_group: HelpGroup::Common,
        summary: "Run a one-shot agent session in the terminal.",
        surfaces: CLI_ONLY,
        usage: "rightkit-agent run",
        daemon_handler: None,
    },
    CommandSpec {
        name: "/clear",
        aliases: &["/new", "/cls"],
        scope: CommandScope::Session,
        effect: CommandEffect::Mutate,
        valid_mid_turn: false,
        args: NO_ARGS,
        renderer: RendererHint::Status,
        help_group: HelpGroup::Common,
        summary: "Start a new session (the old one stays in history/resume).",
        surfaces: INTERACTIVE_ONLY,
        usage: "/clear",
        daemon_handler: Some(builtins::clear_handler),
    },
    CommandSpec {
        name: "/model",
        aliases: &[],
        scope: CommandScope::Session,
        effect: CommandEffect::Mutate,
        valid_mid_turn: true,
        args: &[ArgSpec {
            name: "name",
            help: "Model id or picker number (`/model <n>`) for the next turn. Omit to open the shared route picker.",
            required: false,
        }],
        renderer: RendererHint::Picker,
        help_group: HelpGroup::Common,
        summary: "Switch the model for the next turn without ending the session.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/model [model]",
        daemon_handler: Some(builtins::model_handler),
    },
    CommandSpec {
        name: "/effort",
        aliases: &[],
        scope: CommandScope::Session,
        effect: CommandEffect::Mutate,
        valid_mid_turn: true,
        args: &[ArgSpec {
            name: "level",
            help: "Reasoning effort for the next turn: low, medium, or high. Omit to show the current selection.",
            required: false,
        }],
        renderer: RendererHint::Status,
        help_group: HelpGroup::Common,
        summary: "Set reasoning effort for the next turn without ending the session.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/effort [low|medium|high]",
        daemon_handler: Some(effort_guidance_handler),
    },
    CommandSpec {
        name: "/repo",
        aliases: &["/repository"],
        scope: CommandScope::Session,
        effect: CommandEffect::Mutate,
        // A repository switch creates/selects a root session for the new
        // scope; it must never retarget a session while work is admitted.
        valid_mid_turn: false,
        args: &[ArgSpec {
            name: "path",
            help: "Repository root to select. Omit to list known repository roots.",
            required: false,
        }],
        renderer: RendererHint::Picker,
        help_group: HelpGroup::Common,
        summary: "Select the repository root this session's turns run against.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/repo [path]",
        daemon_handler: Some(repo_guidance_handler),
    },
    CommandSpec {
        name: "/goal",
        aliases: &[],
        scope: CommandScope::Session,
        effect: CommandEffect::Mutate,
        valid_mid_turn: true,
        args: &[ArgSpec { name: "set|edit|pause|resume|clear|<text>", help: "Goal action or the goal text.", required: false }],
        renderer: RendererHint::Status,
        help_group: HelpGroup::Common,
        summary: "Set, edit, pause, resume or clear this chat's goal; progress shows in the footer.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/goal [set <text>|edit <text>|pause|resume|clear]",
        daemon_handler: Some(tui_surface_guidance_handler),
    },
    CommandSpec {
        name: "/trust",
        aliases: &["trust"],
        scope: CommandScope::Client,
        effect: CommandEffect::Mutate,
        valid_mid_turn: false,
        args: &[ArgSpec { name: "path", help: "Workspace to trust; omit for this chat.", required: false }],
        renderer: RendererHint::Status,
        help_group: HelpGroup::Common,
        summary: "Trust this workspace using persisted workspace policy.",
        surfaces: BOTH,
        usage: "/trust | rightkit-agent trust [path]",
        daemon_handler: Some(tui_surface_guidance_handler),
    },
    CommandSpec {
        name: "/insights",
        aliases: &["insights"],
        scope: CommandScope::Client,
        effect: CommandEffect::Read,
        valid_mid_turn: true,
        args: NO_ARGS,
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Inspect Adapt reports from completed sessions.",
        surfaces: BOTH,
        usage: "/insights | rightkit-agent insights",
        daemon_handler: Some(tui_surface_guidance_handler),
    },
    CommandSpec {
        name: "/config",
        aliases: &["/settings"],
        scope: CommandScope::Session,
        effect: CommandEffect::Mutate,
        valid_mid_turn: true,
        args: NO_ARGS,
        renderer: RendererHint::Picker,
        help_group: HelpGroup::Common,
        summary: "View and edit TUI settings (theme, notify, vim, status line, keys).",
        surfaces: INTERACTIVE_ONLY,
        usage: "/config",
        daemon_handler: Some(tui_surface_guidance_handler),
    },
    CommandSpec {
        name: "/theme",
        aliases: &[],
        scope: CommandScope::Session,
        effect: CommandEffect::Mutate,
        valid_mid_turn: true,
        args: &[ArgSpec { name: "auto|dark|light", help: "Theme to apply and remember.", required: false }],
        renderer: RendererHint::Status,
        help_group: HelpGroup::Common,
        summary: "Show or set the TUI theme.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/theme [auto|dark|light]",
        daemon_handler: Some(tui_surface_guidance_handler),
    },
    CommandSpec {
        name: "/vim",
        aliases: &[],
        scope: CommandScope::Session,
        effect: CommandEffect::Mutate,
        valid_mid_turn: true,
        args: &[ArgSpec { name: "on|off", help: "Vim composer mode.", required: false }],
        renderer: RendererHint::Status,
        help_group: HelpGroup::Common,
        summary: "Toggle Vim editing in the composer.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/vim [on|off]",
        daemon_handler: Some(tui_surface_guidance_handler),
    },
    CommandSpec {
        name: "/projects",
        aliases: &["/home", "/sidebar"],
        scope: CommandScope::Session,
        effect: CommandEffect::Mutate,
        valid_mid_turn: true,
        args: NO_ARGS,
        renderer: RendererHint::Picker,
        help_group: HelpGroup::Common,
        summary: "Open the project and chat browser (Ctrl+B toggles the sidebar).",
        surfaces: INTERACTIVE_ONLY,
        usage: "/projects",
        daemon_handler: Some(tui_surface_guidance_handler),
    },
    CommandSpec {
        name: "/models",
        aliases: &["/scoped-models"],
        scope: CommandScope::Session,
        effect: CommandEffect::Read,
        valid_mid_turn: true,
        args: NO_ARGS,
        renderer: RendererHint::Table,
        help_group: HelpGroup::Common,
        summary: "Show the live model catalog and visibility/cycle set.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/models",
        daemon_handler: Some(builtins::models_handler),
    },
    CommandSpec {
        name: "/providers",
        aliases: &["/account"],
        scope: CommandScope::Session,
        effect: CommandEffect::Mutate,
        valid_mid_turn: true,
        args: NO_ARGS,
        renderer: RendererHint::Picker,
        help_group: HelpGroup::Common,
        summary: "Providers & account — sign in, API keys, models",
        surfaces: INTERACTIVE_ONLY,
        usage: "/providers",
        daemon_handler: Some(providers_guidance_handler),
    },
    CommandSpec {
        name: "/plan",
        aliases: &[],
        scope: CommandScope::Session,
        effect: CommandEffect::Mutate,
        valid_mid_turn: true,
        args: NO_ARGS,
        renderer: RendererHint::Status,
        help_group: HelpGroup::Common,
        summary: "Permission mode: read-only planning; nothing is changed until you approve a plan.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/plan",
        daemon_handler: Some(permission_mode_guidance_handler),
    },
    CommandSpec {
        name: "/ask",
        aliases: &[],
        scope: CommandScope::Session,
        effect: CommandEffect::Mutate,
        valid_mid_turn: true,
        args: NO_ARGS,
        renderer: RendererHint::Status,
        help_group: HelpGroup::Common,
        summary: "Permission mode: ask before edits and commands (default).",
        surfaces: INTERACTIVE_ONLY,
        usage: "/ask",
        daemon_handler: Some(permission_mode_guidance_handler),
    },
    CommandSpec {
        name: "/accept-edits",
        aliases: &[],
        scope: CommandScope::Session,
        effect: CommandEffect::Mutate,
        valid_mid_turn: true,
        args: NO_ARGS,
        renderer: RendererHint::Status,
        help_group: HelpGroup::Common,
        summary: "Permission mode: auto-apply file edits; commands still ask.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/accept-edits",
        daemon_handler: Some(permission_mode_guidance_handler),
    },
    CommandSpec {
        name: "/auto",
        aliases: &[],
        scope: CommandScope::Session,
        effect: CommandEffect::Mutate,
        valid_mid_turn: true,
        args: NO_ARGS,
        renderer: RendererHint::Status,
        help_group: HelpGroup::Common,
        summary: "Permission mode: auto-approve safe actions; destructive ones still ask.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/auto",
        daemon_handler: Some(permission_mode_guidance_handler),
    },
    CommandSpec {
        name: "/yolo",
        aliases: &[],
        scope: CommandScope::Session,
        effect: CommandEffect::Mutate,
        valid_mid_turn: true,
        args: NO_ARGS,
        renderer: RendererHint::Status,
        help_group: HelpGroup::Common,
        summary: "Permission mode: approve everything (locked workspaces stay blocked).",
        surfaces: INTERACTIVE_ONLY,
        usage: "/yolo",
        daemon_handler: Some(permission_mode_guidance_handler),
    },
    CommandSpec {
        name: "/compact",
        aliases: &[],
        scope: CommandScope::Session,
        effect: CommandEffect::Mutate,
        valid_mid_turn: false,
        args: NO_ARGS,
        renderer: RendererHint::Status,
        help_group: HelpGroup::Common,
        summary: "Compact the conversation history.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/compact",
        daemon_handler: Some(builtins::compact_handler),
    },
    CommandSpec {
        name: "/fork",
        aliases: &[],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Mutate,
        valid_mid_turn: false,
        args: &[ArgSpec { name: "--title", help: "Title for the forked session.", required: false }],
        renderer: RendererHint::Status,
        help_group: HelpGroup::Common,
        summary: "Fork a session into a new idle session with parent lineage.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/fork [--title]",
        daemon_handler: Some(builtins::fork_handler),
    },
    CommandSpec {
        name: "/help",
        aliases: &["/?"],
        scope: CommandScope::Client,
        effect: CommandEffect::Read,
        valid_mid_turn: true,
        args: &[ArgSpec { name: "--all", help: "Show advanced commands too.", required: false }],
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Show available commands, grouped.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/help [--all]",
        daemon_handler: Some(builtins::help_handler),
    },
    CommandSpec {
        name: "/onboard",
        aliases: &["/setup"],
        scope: CommandScope::Session,
        effect: CommandEffect::Mutate,
        valid_mid_turn: false,
        args: NO_ARGS,
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Run first-run onboarding: pick a route, store a credential, probe readiness.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/onboard",
        daemon_handler: Some(builtins::onboard_handler),
    },
    CommandSpec {
        name: "/quit",
        aliases: &["/exit", "/q"],
        scope: CommandScope::Session,
        effect: CommandEffect::Read,
        valid_mid_turn: true,
        args: NO_ARGS,
        renderer: RendererHint::Status,
        help_group: HelpGroup::Common,
        summary: "Quit Agent",
        surfaces: INTERACTIVE_ONLY,
        usage: "/quit",
        daemon_handler: Some(builtins::quit_handler),
    },
    CommandSpec {
        name: "fork",
        aliases: &[],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Mutate,
        valid_mid_turn: false,
        args: NO_ARGS,
        renderer: RendererHint::Status,
        help_group: HelpGroup::Common,
        summary: "Fork a session into a new idle session with parent lineage.",
        surfaces: CLI_ONLY,
        usage: "rightkit-agent fork",
        daemon_handler: None,
    },
    // ---- diagnostics & runtime inspection ---------------------------
    CommandSpec {
        name: "/doctor",
        aliases: &[],
        scope: CommandScope::Client,
        effect: CommandEffect::Read,
        valid_mid_turn: true,
        args: NO_ARGS,
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Preflight: check whether the engine is ready to run.",
        surfaces: BOTH,
        usage: "/doctor",
        daemon_handler: Some(builtins::doctor_handler),
    },
    CommandSpec {
        name: "/status",
        aliases: &[],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Read,
        valid_mid_turn: true,
        args: NO_ARGS,
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Show daemon status.",
        surfaces: BOTH,
        usage: "/status",
        daemon_handler: Some(builtins::status_handler),
    },
    CommandSpec {
        name: "/permissions",
        aliases: &["permission"],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Mutate,
        valid_mid_turn: true,
        args: NO_ARGS,
        renderer: RendererHint::Table,
        help_group: HelpGroup::Common,
        summary: "Inspect and adjust live session permissions.",
        surfaces: BOTH,
        usage: "/permissions",
        daemon_handler: Some(builtins::permissions_handler),
    },
    CommandSpec {
        name: "/skills",
        aliases: &["skill"],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Read,
        valid_mid_turn: true,
        args: NO_ARGS,
        renderer: RendererHint::Table,
        help_group: HelpGroup::Common,
        summary: "Govern local skills, proposals, benchmark gates, and training runs.",
        surfaces: BOTH,
        usage: "/skills",
        daemon_handler: Some(builtins::skills_handler),
    },
    CommandSpec {
        name: "/hooks",
        aliases: &["hook"],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Read,
        valid_mid_turn: true,
        args: NO_ARGS,
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Inspect daemon hook readiness without exposing hook values.",
        surfaces: BOTH,
        usage: "/hooks",
        daemon_handler: Some(builtins::hooks_handler),
    },
    CommandSpec {
        name: "/mcp",
        aliases: &[],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Read,
        valid_mid_turn: true,
        args: NO_ARGS,
        renderer: RendererHint::Table,
        help_group: HelpGroup::Common,
        summary: "Manage MCP servers through the running daemon.",
        surfaces: BOTH,
        usage: "/mcp",
        daemon_handler: Some(builtins::mcp_handler),
    },
    CommandSpec {
        name: "/caps",
        aliases: &[],
        scope: CommandScope::Client,
        effect: CommandEffect::Read,
        valid_mid_turn: true,
        args: NO_ARGS,
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "List Agent tool capabilities from the real runtime registry.",
        surfaces: BOTH,
        usage: "/caps",
        daemon_handler: Some(builtins::caps_handler),
    },
    CommandSpec {
        name: "/context",
        aliases: &[],
        scope: CommandScope::Client,
        effect: CommandEffect::Read,
        valid_mid_turn: true,
        args: &[ArgSpec { name: "workspace", help: "Workspace root.", required: false }],
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Show current runtime and workspace context.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/context [workspace]",
        daemon_handler: Some(builtins::context_handler),
    },
    CommandSpec {
        name: "/memory",
        aliases: &[],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Read,
        valid_mid_turn: true,
        args: NO_ARGS,
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Show current memory context.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/memory",
        daemon_handler: Some(builtins::memory_handler),
    },
    CommandSpec {
        name: "/instructions",
        aliases: &[],
        scope: CommandScope::Client,
        effect: CommandEffect::Read,
        valid_mid_turn: true,
        args: &[ArgSpec { name: "workspace", help: "Workspace root.", required: false }],
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Show loaded project instruction sources.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/instructions [workspace]",
        daemon_handler: Some(builtins::instructions_handler),
    },
    CommandSpec {
        name: "/rename",
        aliases: &[],
        scope: CommandScope::Session,
        effect: CommandEffect::Mutate,
        valid_mid_turn: false,
        args: &[ArgSpec { name: "arguments", help: "Session or workspace action arguments.", required: false }],
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Rename this chat.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/rename [arguments]",
        daemon_handler: Some(tui_surface_guidance_handler),
    },
    CommandSpec {
        name: "/copy",
        aliases: &[],
        scope: CommandScope::Session,
        effect: CommandEffect::Read,
        valid_mid_turn: false,
        args: &[ArgSpec { name: "arguments", help: "Session or workspace action arguments.", required: false }],
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Copy this chat transcript to clipboard.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/copy [arguments]",
        daemon_handler: Some(tui_surface_guidance_handler),
    },
    CommandSpec {
        name: "/clone",
        aliases: &[],
        scope: CommandScope::Session,
        effect: CommandEffect::Mutate,
        valid_mid_turn: false,
        args: &[ArgSpec { name: "arguments", help: "Session or workspace action arguments.", required: false }],
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Clone this chat into a separate session.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/clone [arguments]",
        daemon_handler: Some(tui_surface_guidance_handler),
    },
    CommandSpec {
        name: "/archive",
        aliases: &[],
        scope: CommandScope::Session,
        effect: CommandEffect::Mutate,
        valid_mid_turn: false,
        args: &[ArgSpec { name: "arguments", help: "Session or workspace action arguments.", required: false }],
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Archive a chat.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/archive [arguments]",
        daemon_handler: Some(tui_surface_guidance_handler),
    },
    CommandSpec {
        name: "/delete",
        aliases: &[],
        scope: CommandScope::Session,
        effect: CommandEffect::Destructive,
        valid_mid_turn: false,
        args: &[ArgSpec { name: "arguments", help: "Session or workspace action arguments.", required: false }],
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Delete a chat after confirmation.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/delete [arguments]",
        daemon_handler: Some(tui_surface_guidance_handler),
    },
    CommandSpec {
        name: "/init",
        aliases: &[],
        scope: CommandScope::Session,
        effect: CommandEffect::Mutate,
        valid_mid_turn: false,
        args: &[ArgSpec { name: "arguments", help: "Session or workspace action arguments.", required: false }],
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Create project instructions using current workspace.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/init [arguments]",
        daemon_handler: Some(tui_surface_guidance_handler),
    },
    CommandSpec {
        name: "/cd",
        aliases: &[],
        scope: CommandScope::Session,
        effect: CommandEffect::Mutate,
        valid_mid_turn: false,
        args: &[ArgSpec { name: "arguments", help: "Session or workspace action arguments.", required: false }],
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Switch this chat to another workspace.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/cd [arguments]",
        daemon_handler: Some(tui_surface_guidance_handler),
    },
    CommandSpec {
        name: "/session",
        aliases: &[],
        scope: CommandScope::Session,
        effect: CommandEffect::Read,
        valid_mid_turn: false,
        args: &[ArgSpec { name: "arguments", help: "Session or workspace action arguments.", required: false }],
        renderer: RendererHint::Picker,
        help_group: HelpGroup::Common,
        summary: "Inspect current session or open session picker.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/session [arguments]",
        daemon_handler: Some(tui_surface_guidance_handler),
    },
    CommandSpec {
        name: "/worktree",
        aliases: &[],
        scope: CommandScope::Session,
        effect: CommandEffect::Mutate,
        valid_mid_turn: false,
        args: &[ArgSpec { name: "arguments", help: "Session or workspace action arguments.", required: false }],
        renderer: RendererHint::Picker,
        help_group: HelpGroup::Common,
        summary: "Inspect and manage workspace worktrees.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/worktree [arguments]",
        daemon_handler: Some(tui_surface_guidance_handler),
    },
    // ---- transcripts & exports --------------------------------------
    CommandSpec {
        name: "export",
        aliases: &["transcript", "/export"],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Read,
        valid_mid_turn: false,
        args: &[ArgSpec { name: "--json", help: "Export as JSON.", required: false }],
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Export a session transcript from the daemon.",
        surfaces: BOTH,
        usage: "/export [--json]",
        daemon_handler: Some(builtins::export_handler),
    },
    CommandSpec {
        name: "/summary",
        aliases: &[],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Read,
        valid_mid_turn: true,
        args: NO_ARGS,
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Summarize the active session transcript.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/summary",
        daemon_handler: Some(builtins::summary_handler),
    },
    // ---- conductor flows ---------------------------------------------
    CommandSpec {
        name: "/review",
        aliases: &[],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Mutate,
        valid_mid_turn: false,
        args: NO_ARGS,
        renderer: RendererHint::Stream,
        help_group: HelpGroup::Common,
        summary: "Ask the auditor for a current-diff code review.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/review [focus]",
        daemon_handler: Some(builtins::review_handler),
    },
    CommandSpec {
        name: "/audit",
        aliases: &[],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Mutate,
        valid_mid_turn: false,
        args: NO_ARGS,
        renderer: RendererHint::Stream,
        help_group: HelpGroup::Common,
        summary: "Ask the auditor for a whole-repo audit, optionally fixing what it finds.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/audit [fix] [focus]",
        daemon_handler: Some(builtins::audit_handler),
    },
    CommandSpec {
        name: "/blueprint",
        aliases: &[],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Mutate,
        valid_mid_turn: false,
        args: NO_ARGS,
        renderer: RendererHint::Stream,
        help_group: HelpGroup::Common,
        summary: "Map the repository with researcher and architect roles.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/blueprint [focus]",
        daemon_handler: Some(builtins::blueprint_handler),
    },
    CommandSpec {
        name: "/architect",
        aliases: &[],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Mutate,
        valid_mid_turn: false,
        args: NO_ARGS,
        renderer: RendererHint::Stream,
        help_group: HelpGroup::Common,
        summary: "Run the architect method: explore, ADR, TDD plan, challenger gate.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/architect [focus]",
        daemon_handler: Some(builtins::architect_handler),
    },
    CommandSpec {
        name: "/commit",
        aliases: &[],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Mutate,
        valid_mid_turn: false,
        args: NO_ARGS,
        renderer: RendererHint::Stream,
        help_group: HelpGroup::Common,
        summary: "Run diff-scoped audit lenses then commit+push.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/commit [focus]",
        daemon_handler: Some(builtins::commit_handler),
    },
    CommandSpec {
        name: "/dream",
        aliases: &[],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Mutate,
        valid_mid_turn: false,
        args: NO_ARGS,
        renderer: RendererHint::Status,
        help_group: HelpGroup::Common,
        summary: "Run memory dream consolidation now.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/dream now",
        daemon_handler: Some(builtins::dream_handler),
    },
    // ---- panels & inventories ----------------------------------------
    CommandSpec {
        name: "/diff",
        aliases: &[],
        scope: CommandScope::Workspace,
        effect: CommandEffect::Read,
        valid_mid_turn: true,
        args: NO_ARGS,
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Ask the agent for a git diff review.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/diff",
        daemon_handler: Some(builtins::diff_handler),
    },
    CommandSpec {
        name: "/plugin",
        aliases: &[],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Read,
        valid_mid_turn: true,
        args: NO_ARGS,
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Show installed plugin or marketplace state.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/plugin [marketplace [query]]",
        daemon_handler: Some(builtins::plugin_handler),
    },
    CommandSpec {
        name: "/plugins",
        aliases: &[],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Read,
        valid_mid_turn: true,
        args: NO_ARGS,
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Show installed plugin or marketplace state.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/plugins [marketplace [query]]",
        daemon_handler: Some(builtins::plugins_handler),
    },
    CommandSpec {
        name: "/teams",
        aliases: &[],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Read,
        valid_mid_turn: true,
        args: NO_ARGS,
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Show team definitions.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/teams",
        daemon_handler: Some(builtins::teams_handler),
    },
    CommandSpec {
        name: "/messages",
        aliases: &[],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Read,
        valid_mid_turn: true,
        args: NO_ARGS,
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Show inter-session message history.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/messages",
        daemon_handler: Some(builtins::messages_handler),
    },
    CommandSpec {
        name: "/logs",
        aliases: &["trace"],
        scope: CommandScope::Session,
        effect: CommandEffect::Read,
        valid_mid_turn: false,
        args: NO_ARGS,
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Inspect durable events for the active session.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/logs",
        daemon_handler: Some(tui_surface_guidance_handler),
    },
    CommandSpec {
        name: "/usage",
        aliases: &[],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Read,
        valid_mid_turn: true,
        args: &[ArgSpec { name: "session-id", help: "Session id.", required: false }],
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Show current session token and cost usage.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/usage [session-id]",
        daemon_handler: Some(builtins::usage_handler),
    },
    CommandSpec {
        name: "/schedules",
        aliases: &[],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Read,
        valid_mid_turn: true,
        args: &[ArgSpec { name: "workspace", help: "Workspace root.", required: false }],
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Show workspace schedules.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/schedules [workspace]",
        daemon_handler: Some(builtins::schedules_panel_handler),
    },
    CommandSpec {
        name: "/tasks",
        aliases: &[],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Read,
        valid_mid_turn: true,
        args: NO_ARGS,
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Show task graph guidance.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/tasks",
        daemon_handler: Some(builtins::tasks_handler),
    },
    CommandSpec {
        name: "/benchmarks",
        aliases: &[],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Read,
        valid_mid_turn: true,
        args: NO_ARGS,
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Show benchmark history.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/benchmarks",
        daemon_handler: Some(builtins::benchmarks_handler),
    },
    CommandSpec {
        name: "/terminal-bench",
        aliases: &["/tbench"],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Read,
        valid_mid_turn: true,
        args: NO_ARGS,
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "List or run benchmark tasks.",
        surfaces: INTERACTIVE_ONLY,
        usage: "/terminal-bench [tasks] | /terminal-bench run <task-id> <agent> <session-id>",
        daemon_handler: Some(builtins::terminal_bench_handler),
    },
    // ---- install & service --------------------------------------------
    CommandSpec {
        name: "install-service",
        aliases: &["install", "/install"],
        scope: CommandScope::Client,
        effect: CommandEffect::Destructive,
        valid_mid_turn: false,
        args: NO_ARGS,
        renderer: RendererHint::Status,
        help_group: HelpGroup::Advanced,
        summary: "Install the engine as a background service.",
        surfaces: BOTH,
        usage: "/install",
        daemon_handler: Some(builtins::install_handler),
    },
    // ---- remaining CLI verbs (already CLI-only) -----------------------
    CommandSpec {
        name: "config",
        aliases: &[],
        scope: CommandScope::Client,
        effect: CommandEffect::Read,
        valid_mid_turn: false,
        args: NO_ARGS,
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Print the merged configuration.",
        surfaces: CLI_ONLY,
        usage: "rightkit-agent config",
        daemon_handler: None,
    },
    CommandSpec {
        name: "serve",
        aliases: &[],
        scope: CommandScope::Client,
        effect: CommandEffect::Mutate,
        valid_mid_turn: false,
        args: NO_ARGS,
        renderer: RendererHint::Stream,
        help_group: HelpGroup::Common,
        summary: "Run the HTTP daemon.",
        surfaces: CLI_ONLY,
        usage: "rightkit-agent serve",
        daemon_handler: None,
    },
    // ---- provider --------------------------------------------------------
    //
    // Executed through the CLI (`ProviderCommand`, matched exhaustively in
    // `main_runtime_cli_controls.rs`); not slash targets, so no daemon
    // handler.
    CommandSpec {
        name: "provider status",
        aliases: &[],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Read,
        valid_mid_turn: true,
        args: &[ArgSpec { name: "provider", help: "Provider id.", required: true }],
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Read provider key configuration status.",
        surfaces: CLI_ONLY,
        usage: "rightkit-agent provider status <provider>",
        daemon_handler: None,
    },
    CommandSpec {
        name: "provider connect",
        aliases: &[],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Mutate,
        valid_mid_turn: false,
        args: &[ArgSpec { name: "provider", help: "Provider id.", required: true }],
        renderer: RendererHint::Status,
        help_group: HelpGroup::Common,
        summary: "Connect a provider.",
        surfaces: CLI_ONLY,
        usage: "rightkit-agent provider connect <provider>",
        daemon_handler: None,
    },
    CommandSpec {
        name: "provider models",
        aliases: &[],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Read,
        valid_mid_turn: true,
        args: &[ArgSpec { name: "provider", help: "Provider id.", required: true }],
        renderer: RendererHint::Table,
        help_group: HelpGroup::Common,
        summary: "List models available through a provider.",
        surfaces: CLI_ONLY,
        usage: "rightkit-agent provider models <provider>",
        daemon_handler: None,
    },
    CommandSpec {
        name: "provider set-visibility",
        aliases: &[],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Mutate,
        valid_mid_turn: false,
        args: &[
            ArgSpec { name: "provider", help: "Provider id.", required: true },
            ArgSpec { name: "model", help: "Model slug.", required: true },
            ArgSpec { name: "--visible", help: "Set visible/hidden.", required: true },
        ],
        renderer: RendererHint::Status,
        help_group: HelpGroup::Common,
        summary: "Toggle a model's visibility for a provider.",
        surfaces: CLI_ONLY,
        usage: "rightkit-agent provider set-visibility <provider> <model> --visible <bool>",
        daemon_handler: None,
    },
    // Executed locally by the CLI (`Command::Models`); the live account
    // catalog for a native-subscription provider, else the canonical
    // table. Not slash targets, so no daemon handler.
    CommandSpec {
        name: "models",
        aliases: &[],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Read,
        valid_mid_turn: true,
        args: &[ArgSpec { name: "--provider", help: "Provider id (default: the configured provider).", required: false }],
        renderer: RendererHint::Text,
        help_group: HelpGroup::Common,
        summary: "Print the models a configured provider can serve, one per line.",
        surfaces: CLI_ONLY,
        usage: "rightkit-agent models [--provider <id>]",
        daemon_handler: None,
    },
    // ---- advanced group -------------------------------------------------
    CommandSpec {
        name: "conduct",
        aliases: &[],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Mutate,
        valid_mid_turn: false,
        args: NO_ARGS,
        renderer: RendererHint::Stream,
        help_group: HelpGroup::Advanced,
        summary: "Run a multi-agent conductor session against a goal.",
        surfaces: CLI_ONLY,
        usage: "rightkit-agent conduct",
        daemon_handler: None,
    },
    CommandSpec {
        name: "cli-runtime",
        aliases: &[],
        scope: CommandScope::Client,
        effect: CommandEffect::Mutate,
        valid_mid_turn: false,
        args: NO_ARGS,
        renderer: RendererHint::Text,
        help_group: HelpGroup::Advanced,
        summary: "Installed-runtime acceptance controls.",
        surfaces: CLI_ONLY,
        usage: "rightkit-agent cli-runtime",
        daemon_handler: None,
    },
    CommandSpec {
        name: "handoff",
        aliases: &["bridge", "ide", "desktop", "mobile", "teleport", "/handoff", "/bridge", "/ide", "/desktop", "/mobile", "/teleport"],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Read,
        valid_mid_turn: false,
        args: NO_ARGS,
        renderer: RendererHint::Text,
        help_group: HelpGroup::Advanced,
        summary: "Print a machine-readable attach payload for IDE, web, mobile, or remote clients.",
        surfaces: BOTH,
        usage: "/handoff",
        daemon_handler: Some(builtins::handoff_handler),
    },
    CommandSpec {
        name: "learning",
        aliases: &[],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Mutate,
        valid_mid_turn: false,
        args: NO_ARGS,
        renderer: RendererHint::Table,
        help_group: HelpGroup::Advanced,
        summary: "Approve or reject governed learning proposals.",
        surfaces: CLI_ONLY,
        usage: "rightkit-agent learning",
        daemon_handler: None,
    },
    CommandSpec {
        name: "spec",
        aliases: &["/spec"],
        scope: CommandScope::Workspace,
        effect: CommandEffect::Mutate,
        valid_mid_turn: false,
        args: NO_ARGS,
        renderer: RendererHint::Text,
        help_group: HelpGroup::Advanced,
        summary: "Create, inspect, analyze, and trace feature specs.",
        surfaces: BOTH,
        usage: "/spec [list|new|status|analyze|trace] ...",
        daemon_handler: Some(builtins::spec_handler),
    },
    CommandSpec {
        name: "schedule",
        aliases: &[],
        scope: CommandScope::Daemon,
        effect: CommandEffect::Mutate,
        valid_mid_turn: false,
        args: NO_ARGS,
        renderer: RendererHint::Table,
        help_group: HelpGroup::Advanced,
        summary: "Inspect and control workspace schedules.",
        surfaces: CLI_ONLY,
        usage: "rightkit-agent schedule",
        daemon_handler: None,
    },
    CommandSpec {
        name: "recipe",
        aliases: &[],
        scope: CommandScope::Workspace,
        effect: CommandEffect::Mutate,
        valid_mid_turn: false,
        args: NO_ARGS,
        renderer: RendererHint::Text,
        help_group: HelpGroup::Advanced,
        summary: "Validate, render, run, and deeplink portable recipes/runbooks.",
        surfaces: CLI_ONLY,
        usage: "rightkit-agent recipe",
        daemon_handler: None,
    },
    CommandSpec {
        name: "remote",
        aliases: &[],
        scope: CommandScope::Workspace,
        effect: CommandEffect::Mutate,
        valid_mid_turn: false,
        args: NO_ARGS,
        renderer: RendererHint::Text,
        help_group: HelpGroup::Advanced,
        summary: "Inspect remote-control integrations such as Telegram.",
        surfaces: CLI_ONLY,
        usage: "rightkit-agent remote",
        daemon_handler: None,
    },
];

/// `/repo` executes inside the interactive session host (which owns the
/// selected repository root and session admission). The daemon answers
/// honestly rather than pretending to hold TUI-local selection state.
fn repo_guidance_handler(_args: &[String]) -> CommandResult {
    CommandResult {
        success: true,
        output: "Repository selection runs inside the Host session (`/repo [path]`); \
                 the daemon carries no interactive repository picker state."
            .into(),
        action: CommandAction::None,
    }
}

/// TUI-local surfaces (`/goal`, `/config`, `/theme`, `/vim`, `/projects`) run
/// inside the interactive session host, which owns the view and goal wiring.
fn tui_surface_guidance_handler(_args: &[String]) -> CommandResult {
    CommandResult {
        success: true,
        output: "This command runs inside the interactive Agent terminal session; the daemon slash surface holds no TUI view state."
            .into(),
        action: CommandAction::None,
    }
}

/// `/effort` mutates the next turn's reasoning effort through the session's
/// validated route updater in the session host; the daemon surface answers
/// honestly when that updater is not what received the request.
fn effort_guidance_handler(_args: &[String]) -> CommandResult {
    CommandResult {
        success: true,
        output: "Reasoning effort is selected in the Host session (`/effort low|medium|high`); \
                 it applies to the next admitted turn."
            .into(),
        action: CommandAction::None,
    }
}

/// `/providers` owns the session's per-provider model-visibility manager;
/// the daemon surface answers honestly — the picker and its durable file
/// live in the session host, not here.
fn providers_guidance_handler(_args: &[String]) -> CommandResult {
    CommandResult {
        success: true,
        output: "Provider inspection and per-model visibility are managed in the Host session (`/providers`); \
                 the daemon carries no interactive provider picker state."
            .into(),
        action: CommandAction::None,
    }
}

/// `/ask`, `/accept-edits`, `/auto`, `/yolo` switch the live session's
/// permission mode inside the the host app session host; the daemon's slash
/// surface points at the session-scoped permission-mode endpoint instead.
fn permission_mode_guidance_handler(_args: &[String]) -> CommandResult {
    CommandResult {
        success: true,
        output: "Permission modes are switched in the Host session (`/ask`, `/accept-edits`, \
                 `/auto`, `/yolo`) or via `POST /sessions/{id}/permission-mode`; the change applies \
                 to the next turn."
            .into(),
        action: CommandAction::None,
    }
}

/// A registered slash command definition.
type CommandHandler = dyn Fn(&[String]) -> CommandResult + Send + Sync;

pub struct CommandDefinition {
    pub name: &'static str,
    pub description: &'static str,
    pub usage: &'static str,
    pub handler: Box<CommandHandler>,
}

/// Registry of all registered slash commands.
///
/// Built directly from [`COMMAND_SPECS`]: every row carrying a
/// [`CommandSpec::daemon_handler`] registers under its canonical bare name,
/// so the executor set and the name table cannot drift apart.
pub struct CommandRegistry {
    commands: Vec<CommandDefinition>,
}

impl CommandRegistry {
    /// Create a new registry with all built-in commands registered from the
    /// one authoritative table.
    ///
    /// Every row carrying a [`CommandSpec::daemon_handler`] registers under
    /// its canonical bare name; each row's declared aliases additionally
    /// register as direct entries pointing at the SAME handler. Those alias
    /// entries are a pure projection of the table (never hand-maintained):
    /// they keep `list()` complete for clients that enumerate runnable
    /// names, while [`Self::execute`] still normalizes through
    /// [`canonical_bare_name`] first, so alias and canonical execution can
    /// never diverge.
    pub fn new() -> Self {
        let mut registry = Self {
            commands: Vec::new(),
        };
        for spec in COMMAND_SPECS.iter() {
            let Some(handler) = spec.daemon_handler else {
                continue;
            };
            let bare: &'static str = spec.name.trim_start_matches('/');
            registry.register(CommandDefinition {
                name: bare,
                description: spec.summary,
                usage: spec.usage,
                handler: Box::new(handler),
            });
            for alias in spec.aliases {
                let alias_bare = alias.trim_start_matches('/');
                if alias_bare == bare || registry.has_executor(alias_bare) {
                    continue;
                }
                registry.register(CommandDefinition {
                    name: alias_bare,
                    description: spec.summary,
                    usage: spec.usage,
                    handler: Box::new(handler),
                });
            }
        }
        registry
    }

    /// Register a command. If a command with the same name exists, it is replaced.
    pub fn register(&mut self, def: CommandDefinition) {
        self.commands.retain(|c| c.name != def.name);
        self.commands.push(def);
    }

    /// Execute a slash command by name with the given args.
    ///
    /// P1-1: `name` is normalized through [`resolve_query`] FIRST, so every
    /// alias (`"sessions"`, `"q"`, `"tbench"`, …) executes exactly as its
    /// canonical name would — normalization happens before execution, not
    /// only before listing. Unregistered names (plugin commands) pass
    /// through unchanged.
    pub fn execute(&self, name: &str, args: &[String]) -> Option<CommandResult> {
        let target = canonical_bare_name(name).unwrap_or(name);
        self.commands
            .iter()
            .find(|c| c.name == target)
            .map(|c| (c.handler)(args))
    }

    /// Whether a canonical bare command name has a registered executor.
    pub fn has_executor(&self, bare_name: &str) -> bool {
        self.commands.iter().any(|c| c.name == bare_name)
    }

    /// List all registered command names and descriptions.
    pub fn list(&self) -> Vec<CommandInfo> {
        self.commands
            .iter()
            .map(|c| CommandInfo {
                name: c.name,
                description: c.description,
                usage: c.usage,
            })
            .collect()
    }
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Metadata about a registered command (for listing/help).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandInfo {
    pub name: &'static str,
    pub description: &'static str,
    pub usage: &'static str,
}
