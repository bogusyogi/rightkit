//! Data model and host service for slash commands, custom commands, skill manifests and triggers.
//!
//! The types are plain data. Every function that parses, discovers or evaluates delegates to the
//! [`SkillHost`] the host installs once with [`install_skill_host`]; the command table itself
//! (`SkillHost::command_specs`) is the host's. With no host installed the functions answer empty.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

pub use commands::{CommandAction, CommandInfo, CommandRegistry, CommandResult, SlashCommand};
pub use manifest::{
    normalize_skill_pack_github_url, CodeRightSkillMetadata, SkillCategory, SkillEffects,
    SkillError, SkillGateMode, SkillLifecycle, SkillManifest, SkillPackageMetadata,
};
pub use trigger::{evaluate_trigger, TriggerContext, TriggerError};

/// Services a host provides for slash commands, custom commands, skill manifests and triggers.
pub trait SkillHost: Send + Sync {
    /// The slash command table.
    fn command_specs(&self) -> &'static [commands::CommandSpec];
    /// Parse a skill manifest from markdown.
    fn manifest_from_markdown(
        &self,
        text: &str,
    ) -> Result<manifest::SkillManifest, manifest::SkillError>;
    /// Validate a manifest.
    fn manifest_validate(
        &self,
        manifest: &manifest::SkillManifest,
    ) -> Result<(), manifest::SkillError>;
    /// Discover manifests from ordered roots.
    fn discover_skill_roots(
        &self,
        roots: &[(manifest::SkillSourceScope, PathBuf)],
    ) -> Vec<manifest::DiscoveredSkill>;
    /// Normalize a skill-pack GitHub URL.
    fn normalize_skill_pack_github_url(&self, raw: &str) -> Result<String, manifest::SkillError>;
    /// Evaluate a trigger expression.
    fn evaluate_trigger(
        &self,
        expr: &str,
        ctx: &trigger::TriggerContext,
    ) -> Result<bool, trigger::TriggerError>;
    /// Discover custom commands.
    fn custom_discover(
        &self,
        roots: &[(PathBuf, custom_commands::CommandScope)],
    ) -> Vec<custom_commands::CustomCommand>;
    /// Parse one custom command file.
    fn custom_parse_command(
        &self,
        name: &str,
        text: &str,
        scope: custom_commands::CommandScope,
        path: &Path,
    ) -> custom_commands::CustomCommand;
    /// Resolve a typed name to a custom command.
    fn custom_resolve<'a>(
        &self,
        commands: &'a [custom_commands::CustomCommand],
        typed: &str,
    ) -> Option<&'a custom_commands::CustomCommand>;
    /// Listing names for custom commands.
    fn custom_listing<'a>(
        &self,
        commands: &'a [custom_commands::CustomCommand],
    ) -> Vec<(String, &'a custom_commands::CustomCommand)>;
    /// Substitute argument placeholders.
    fn custom_substitute_arguments(&self, body: &str, args: &str) -> String;
    /// Split an argument string.
    fn custom_split_args(&self, args: &str) -> Vec<String>;
    /// Expand file references.
    fn custom_expand_file_references(&self, body: &str, workspace: &Path) -> String;
    /// Expand inline shell markers using `runner`.
    fn inline_shell_expand(
        &self,
        template: &str,
        runner: &mut dyn inline_shell::ShellRunner,
    ) -> String;
}

static HOST: RwLock<Option<Arc<dyn SkillHost>>> = RwLock::new(None);

/// Install the host's [`SkillHost`], replacing any earlier one.
pub fn install_skill_host(host: Arc<dyn SkillHost>) {
    *HOST.write().unwrap_or_else(|e| e.into_inner()) = Some(host);
}

/// The installed [`SkillHost`], if any.
#[must_use]
pub fn skill_host() -> Option<Arc<dyn SkillHost>> {
    HOST.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The slash command table, empty when no host is installed.
#[must_use]
pub fn command_specs() -> &'static [commands::CommandSpec] {
    match skill_host() {
        Some(h) => h.command_specs(),
        None => &[],
    }
}

/// `commands` data model and host-delegating functions.
pub mod commands {
    use super::command_specs;
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
        /// Usage text for listings/help (`/model [model]`, `coderight serve`, …).
        pub usage: &'static str,
        /// The daemon slash-API executor for this row, if it has one.
        pub daemon_handler: Option<DaemonHandler>,
    }
    /// Resolve an exact canonical name or alias (with or without a leading `/`)
    /// to its registry entry. Exact-match core; prefer [`resolve_query`] at
    /// dispatch points, which also accepts the bare form of a slash-canonical
    /// name.
    pub fn resolve(query: &str) -> Option<&'static CommandSpec> {
        command_specs()
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
        command_specs()
            .iter()
            .filter(move |entry| entry.help_group == group)
    }

    /// All entries reachable from a given surface (CLI or interactive).
    pub fn entries_for_surface(cli: bool) -> impl Iterator<Item = &'static CommandSpec> {
        command_specs().iter().filter(move |entry| {
            if cli {
                entry.surfaces.cli
            } else {
                entry.surfaces.interactive
            }
        })
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
    /// Built directly from [`command_specs()`]: every row carrying a
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
            for spec in command_specs().iter() {
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
}
/// `manifest` data model and host-delegating functions.
pub mod manifest {
    use serde::{Deserialize, Serialize};
    use std::path::PathBuf;

    fn default_skill_priority() -> i32 {
        100
    }

    /// Errors that can occur while loading or validating a skill manifest.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum SkillError {
        /// The frontmatter block was missing or malformed.
        Frontmatter(String),
        /// The frontmatter TOML failed to parse.
        Toml(String),
        /// The trigger expression was malformed.
        Trigger(super::trigger::TriggerError),
        /// Installation or provenance error for external skill packs.
        Install(String),
    }

    impl std::fmt::Display for SkillError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Frontmatter(m) => write!(f, "frontmatter error: {m}"),
                Self::Toml(m) => write!(f, "toml error: {m}"),
                Self::Trigger(e) => write!(f, "trigger error: {e}"),
                Self::Install(m) => write!(f, "install error: {m}"),
            }
        }
    }

    impl std::error::Error for SkillError {}

    impl From<super::trigger::TriggerError> for SkillError {
        fn from(e: super::trigger::TriggerError) -> Self {
            Self::Trigger(e)
        }
    }

    /// A skill manifest. Parsed from either Anthropic-style YAML frontmatter
    /// (`---` delimited — the `SKILL.md` standard) or CodeRight's native TOML
    /// frontmatter (`+++` delimited, which adds the trigger DSL).
    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    #[serde(rename_all = "camelCase")]
    #[derive(Default)]
    pub struct SkillEffects {
        /// Guidance execution mode the skill wants the agent to adopt when it
        /// fires, e.g. "review", "test-first", "plan-first", or "careful".
        #[serde(default, alias = "execution_mode")]
        pub execution_mode: String,
        /// Runtime-enforced allow-list for matching PreToolUse events.
        #[serde(default, alias = "allowed_tools")]
        pub allowed_tools: Vec<String>,
        /// Runtime-enforced deny-list for matching PreToolUse events.
        #[serde(default, alias = "disallowed_tools", alias = "denied_tools")]
        pub disallowed_tools: Vec<String>,
        /// Short guidance prompt fragment to include in triggered-skill hints.
        #[serde(default)]
        pub prompt: String,
    }

    #[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
    #[serde(rename_all = "camelCase")]
    pub enum SkillCategory {
        #[default]
        Tool,
        Behavioral,
    }

    #[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
    #[serde(rename_all = "camelCase")]
    pub enum SkillLifecycle {
        Proposed,
        Quarantined,
        Benchmarked,
        Approved,
        #[default]
        Active,
        Retired,
    }

    #[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "camelCase")]
    pub enum SkillGateMode {
        Hard,
        Soft,
        Mixed,
    }

    #[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
    #[serde(rename_all = "camelCase")]
    pub struct SkillPackageMetadata {
        #[serde(default)]
        pub coderight: CodeRightSkillMetadata,
    }

    #[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
    #[serde(rename_all = "camelCase")]
    pub struct CodeRightSkillMetadata {
        #[serde(default)]
        pub category: Option<SkillCategory>,
        #[serde(default, alias = "requires_toolsets")]
        pub requires_toolsets: Vec<String>,
        #[serde(default, alias = "fallback_for_toolsets")]
        pub fallback_for_toolsets: Vec<String>,
        #[serde(default)]
        pub config: serde_json::Value,
        #[serde(default, alias = "gate_mode")]
        pub gate_mode: Option<SkillGateMode>,
        #[serde(default, alias = "benchmark_ids")]
        pub benchmark_ids: Vec<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    pub struct SkillManifest {
        /// The skill's unique name.
        pub name: String,
        /// A human-readable description. For Anthropic skills this is how the model
        /// decides *when* to use the skill, so it must be present and specific.
        pub description: String,
        /// The trigger DSL expression that controls when the skill fires. Empty for
        /// Anthropic `SKILL.md` skills (the model invokes them by description).
        #[serde(default)]
        pub trigger: String,
        /// Whether the skill is enabled.
        #[serde(default)]
        pub enabled: bool,
        /// Governance lifecycle. Only active skills are loaded into model context
        /// or returned by `invoke_skill`; other states remain visible to inventory
        /// and review surfaces.
        #[serde(default, alias = "status")]
        pub lifecycle: SkillLifecycle,
        /// Optional package/version provenance fields accepted from external
        /// `SKILL.md` formats.
        #[serde(default)]
        pub version: Option<String>,
        #[serde(default)]
        pub author: Option<String>,
        #[serde(default)]
        pub compatibility: Vec<String>,
        #[serde(default)]
        pub platforms: Vec<String>,
        #[serde(
            default,
            alias = "requiredEnvironmentVariables",
            alias = "required_env",
            alias = "requiredEnv"
        )]
        pub required_environment_variables: Vec<String>,
        #[serde(default)]
        pub metadata: SkillPackageMetadata,
        /// Skill category. Tool skills are discoverable/invocable; behavioral
        /// skills compose into the session instruction layer.
        #[serde(default)]
        pub category: SkillCategory,
        /// Lower numbers compose earlier when multiple behavioral skills are active.
        #[serde(default = "default_skill_priority")]
        pub priority: i32,
        /// Prevent automatic model invocation hints/catalog entries for
        /// side-effectful skills. Direct user-requested `invoke_skill` calls can
        /// still load the skill by slug.
        #[serde(
            default,
            alias = "disableModelInvocation",
            alias = "disable-model-invocation"
        )]
        pub disable_model_invocation: bool,
        /// Free-form tags.
        #[serde(default)]
        pub tags: Vec<String>,
        /// Glob patterns for file-path affinity. Runtime file-path events can use
        /// these patterns to suggest the skill without duplicating that condition
        /// in the trigger DSL.
        #[serde(default)]
        pub path_affinity: Vec<String>,
        /// Runtime effects. Tool allow/deny lists are enforced for matching
        /// PreToolUse events; execution mode and prompt remain model guidance.
        #[serde(default)]
        pub effects: SkillEffects,
        /// The markdown body — the skill's actual instructions. Populated for
        /// `SKILL.md` skills (where the body *is* the skill); empty for trigger-only
        /// native manifests.
        #[serde(default)]
        pub body: String,
    }
    impl SkillManifest {
        /// Parse a skill from markdown through the installed host.
        ///
        /// # Errors
        ///
        /// Returns the host's parse error, or an install error when no host is installed.
        pub fn from_markdown(text: &str) -> Result<SkillManifest, SkillError> {
            match super::skill_host() {
                Some(h) => h.manifest_from_markdown(text),
                None => Err(SkillError::Install("no skill host installed".into())),
            }
        }

        /// Validate the manifest through the installed host.
        ///
        /// # Errors
        ///
        /// Returns the host's validation error.
        pub fn validate(&self) -> Result<(), SkillError> {
            match super::skill_host() {
                Some(h) => h.manifest_validate(self),
                None => Ok(()),
            }
        }

        /// Whether the skill is enabled and in the active lifecycle state.
        pub fn is_active(&self) -> bool {
            self.enabled && self.lifecycle == SkillLifecycle::Active
        }
    }

    /// Precedence class attached to every automatically discovered skill source.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum SkillSourceScope {
        Bundled,
        User,
        WorkspaceCompatibility,
        Workspace,
    }

    /// One normalized skill declaration used by the unified capability index.
    #[derive(Debug, Clone, PartialEq)]
    pub struct DiscoveredSkill {
        pub manifest: SkillManifest,
        pub path: PathBuf,
        pub scope: SkillSourceScope,
    }
    /// Discover skill manifests from ordered roots through the installed host.
    pub fn discover_skill_roots(roots: &[(SkillSourceScope, PathBuf)]) -> Vec<DiscoveredSkill> {
        super::skill_host()
            .map(|h| h.discover_skill_roots(roots))
            .unwrap_or_default()
    }

    /// Normalize a skill-pack GitHub URL through the installed host.
    ///
    /// # Errors
    ///
    /// Returns the host's error, or an install error when no host is installed.
    pub fn normalize_skill_pack_github_url(raw: &str) -> Result<String, SkillError> {
        match super::skill_host() {
            Some(h) => h.normalize_skill_pack_github_url(raw),
            None => Err(SkillError::Install("no skill host installed".into())),
        }
    }
}
/// `custom_commands` data model and host-delegating functions.
pub mod custom_commands {
    use std::path::{Path, PathBuf};

    /// Where a command came from. Later roots override earlier ones by name.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum CommandScope {
        User,
        Project,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct CustomCommand {
        /// Slash name without the leading `/`, namespaced with `:`.
        pub name: String,
        pub description: String,
        pub argument_hint: Option<String>,
        pub allowed_tools: Vec<String>,
        pub model: Option<String>,
        /// Prompt template (frontmatter removed).
        pub body: String,
        pub scope: CommandScope,
        pub path: PathBuf,
    }

    impl CustomCommand {
        /// File stem: the bare name Claude Code shows (`button` for `frontend:button`).
        pub fn stem(&self) -> &str {
            self.name.rsplit(':').next().unwrap_or(&self.name)
        }

        /// Directory namespace (`frontend`, or `a:b` for nested), if any.
        pub fn namespace(&self) -> Option<&str> {
            self.name.rsplit_once(':').map(|(ns, _)| ns)
        }

        /// One-line popup / `/help` description, e.g. `Review code [file] (project)`.
        pub fn summary(&self) -> String {
            let scope = match self.scope {
                CommandScope::Project => "project",
                CommandScope::User => "user",
            };
            let mut out = if self.description.is_empty() {
                first_body_line(&self.body)
            } else {
                self.description.clone()
            };
            if let Some(hint) = &self.argument_hint {
                out.push_str(&format!(" {hint}"));
            }
            match self.namespace() {
                Some(ns) => out.push_str(&format!(" ({scope}:{ns})")),
                None => out.push_str(&format!(" ({scope})")),
            }
            out
        }
    }

    fn first_body_line(body: &str) -> String {
        body.lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(|l| l.chars().take(80).collect())
            .unwrap_or_default()
    }
    /// Discover custom commands under ordered roots through the installed host.
    pub fn discover(roots: &[(PathBuf, CommandScope)]) -> Vec<CustomCommand> {
        super::skill_host()
            .map(|h| h.custom_discover(roots))
            .unwrap_or_default()
    }

    /// Parse one custom command file through the installed host.
    pub fn parse_command(
        name: &str,
        text: &str,
        scope: CommandScope,
        path: &Path,
    ) -> CustomCommand {
        match super::skill_host() {
            Some(h) => h.custom_parse_command(name, text, scope, path),
            None => CustomCommand {
                name: name.to_string(),
                description: String::new(),
                argument_hint: None,
                allowed_tools: Vec::new(),
                model: None,
                body: text.to_string(),
                scope,
                path: path.to_path_buf(),
            },
        }
    }

    /// Resolve a typed name to a command, by exact name or unique stem.
    pub fn resolve<'a>(commands: &'a [CustomCommand], typed: &str) -> Option<&'a CustomCommand> {
        super::skill_host().and_then(|h| h.custom_resolve(commands, typed))
    }

    /// Commands as (listing name, command) pairs.
    pub fn listing(commands: &[CustomCommand]) -> Vec<(String, &CustomCommand)> {
        super::skill_host()
            .map(|h| h.custom_listing(commands))
            .unwrap_or_default()
    }

    /// Substitute `$ARGUMENTS`-style placeholders in a command body.
    pub fn substitute_arguments(body: &str, args: &str) -> String {
        super::skill_host()
            .map(|h| h.custom_substitute_arguments(body, args))
            .unwrap_or_else(|| body.to_string())
    }

    /// Split an argument string into words.
    pub fn split_args(args: &str) -> Vec<String> {
        super::skill_host()
            .map(|h| h.custom_split_args(args))
            .unwrap_or_default()
    }

    /// Expand `@file` references in a command body.
    pub fn expand_file_references(body: &str, workspace: &Path) -> String {
        super::skill_host()
            .map(|h| h.custom_expand_file_references(body, workspace))
            .unwrap_or_else(|| body.to_string())
    }
}
/// `inline_shell` data model and host-delegating functions.
pub mod inline_shell {
    /// Result of asking the permission-gated runner to execute one command.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum ShellOutcome {
        /// Command ran; stdout (already size-bounded by the runner).
        Output(String),
        /// Policy denied or approval was not granted; command did not run.
        Denied(String),
        /// Command was approved but failed to run or exited non-zero.
        Failed(String),
    }

    /// Executes a command under the normal shell permission policy.
    pub trait ShellRunner {
        fn run(&mut self, command: &str) -> ShellOutcome;
    }

    impl<F: FnMut(&str) -> ShellOutcome> ShellRunner for F {
        fn run(&mut self, command: &str) -> ShellOutcome {
            self(command)
        }
    }
    /// Expand inline shell markers in `template` through the installed host.
    pub fn expand(template: &str, runner: &mut dyn ShellRunner) -> String {
        match super::skill_host() {
            Some(h) => h.inline_shell_expand(template, runner),
            None => template.to_string(),
        }
    }
}
/// `trigger` data model and host-delegating functions.
pub mod trigger {
    use std::collections::HashMap;

    /// A trigger expression failed to lex, parse or evaluate.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct TriggerError(pub String);

    impl std::fmt::Display for TriggerError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.0)
        }
    }

    impl std::error::Error for TriggerError {}

    /// Evaluation context: a string-to-string map of variables plus structured trigger metadata.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct TriggerContext {
        vars: HashMap<String, String>,
        /// The trigger type that fired this context evaluation, for example `message`.
        pub trigger_type: Option<String>,
        /// The raw payload associated with the trigger.
        pub trigger_payload: Option<String>,
    }

    impl TriggerContext {
        /// Create an empty context.
        pub fn new() -> Self {
            Self::default()
        }

        /// Set a variable in the context.
        pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) -> &mut Self {
            self.vars.insert(key.into(), value.into());
            self
        }

        /// Get a variable from the context.
        pub fn get(&self, key: &str) -> Option<&str> {
            self.vars.get(key).map(|s| s.as_str())
        }

        /// Present, non-empty and not `false`.
        pub fn truthy(&self, key: &str) -> bool {
            match self.vars.get(key) {
                Some(v) => !v.is_empty() && v != "false",
                None => false,
            }
        }
    }

    /// Evaluate a trigger expression through the installed host.
    ///
    /// # Errors
    ///
    /// Returns the host's evaluation error, or an error when no host is installed.
    pub fn evaluate_trigger(expr: &str, ctx: &TriggerContext) -> Result<bool, TriggerError> {
        match super::skill_host() {
            Some(h) => h.evaluate_trigger(expr, ctx),
            None => Err(TriggerError("no skill host installed".into())),
        }
    }
}
