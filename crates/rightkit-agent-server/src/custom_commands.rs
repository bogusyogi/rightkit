//! Custom slash commands for the interactive host (Fixes #132;
//! <https://code.claude.com/docs/en/slash-commands>).
//!
//! Markdown files under the user state root `commands/` and, only when the
//! workspace is trusted, the project's `.claude/commands/` and
//! `.{app}/commands/` (later roots override earlier ones). Parsing,
//! discovery, `$ARGUMENTS`/`$1..$n` and `@file` live in
//! `rightkit_agent_host_traits::skill_host::custom_commands`; `!{cmd}` and backtick-bang run through
//! the policy-backed `inline_shell_runner::expand_custom_command`, so an
//! unapproved shell command is never executed.
//!
//! Frontmatter `allowed-tools` restricts, and `model` overrides, the one turn
//! the expanded command submits (`TurnOverrides`, applied in `spawn_turn`).

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rightkit_agent_host_traits::skill_host::custom_commands::{
    discover, expand_file_references, listing, resolve, substitute_arguments, CommandScope,
    CustomCommand,
};
use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};

/// Per-turn settings from a command's frontmatter, tied to the prompt it expanded to.
/// `allowed_tools` holds RightKit tool names (already mapped from Claude Code names).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TurnOverrides {
    pub prompt: String,
    pub allowed_tools: Vec<String>,
    pub model: Option<String>,
    /// Dropped names and notes to show the user; never a usage error.
    pub warnings: Vec<String>,
}

/// RightKit tool names a Claude Code tool name stands for (empty: unknown).
fn claude_tool_names(tool: &str) -> &'static [&'static str] {
    match tool {
        "Bash" => &["shell"],
        "BashOutput" => &["get_shell_output"],
        "KillShell" | "KillBash" => &["kill_shell"],
        "Read" => &["read_file"],
        "Edit" => &["edit_file", "multi_edit"],
        "MultiEdit" => &["multi_edit"],
        "Write" => &["write_file"],
        "NotebookEdit" | "NotebookRead" => &["notebook"],
        "Grep" => &["search"],
        "Glob" | "LS" => &["list_files"],
        "WebFetch" => &["browser_fetch"],
        "WebSearch" => &["web_search"],
        "Task" => &["spawn_subagent"],
        "TodoWrite" => &["todo_write"],
        "ExitPlanMode" => &["exit_plan_mode"],
        _ => &[],
    }
}

/// Map Claude Code `allowed-tools` entries to RightKit tool names available
/// in `catalog`. Returns (names, warnings). Unknown entries are dropped with a
/// warning. `Bash(prefix:*)` allows only the shell tool: the prefix is NOT
/// installed as a turn-scoped rule (permission rules live on daemon sessions
/// with no per-turn scope reachable from here), so commands still go through
/// normal shell approval. `mcp__server__tool` maps to the MCP gateway tool.
pub(crate) fn map_allowed_tools(
    entries: &[String],
    catalog: &[String],
) -> (Vec<String>, Vec<String>) {
    let mut names: Vec<String> = Vec::new();
    let mut warnings = Vec::new();
    let push = |name: &str, names: &mut Vec<String>| {
        if !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
    };
    for entry in entries {
        let entry = entry.trim();
        let (tool, spec) = match entry.split_once('(') {
            Some((t, rest)) => (t.trim(), Some(rest.trim_end_matches(')').trim())),
            None => (entry, None),
        };
        let mapped: Vec<&str> = if tool.starts_with("mcp__") {
            vec!["mcp_call"]
        } else if !claude_tool_names(tool).is_empty() {
            claude_tool_names(tool).to_vec()
        } else if spec.is_none() && catalog.iter().any(|c| c == tool) {
            vec![catalog
                .iter()
                .find(|c| *c == tool)
                .map(String::as_str)
                .unwrap_or(tool)]
        } else {
            Vec::new()
        };
        let available: Vec<&str> = mapped
            .into_iter()
            .filter(|m| catalog.iter().any(|c| c == m))
            .collect();
        if available.is_empty() {
            warnings.push(format!(
                "allowed-tools: ignored unknown or unavailable tool `{entry}`"
            ));
            continue;
        }
        for name in available {
            push(name, &mut names);
        }
        if tool == "Bash" {
            if let Some(prefix) = spec.filter(|p| !p.is_empty() && *p != "*") {
                warnings.push(format!(
                    "allowed-tools: shell allowed; `Bash({prefix})` is left to normal approval (no turn-scoped shell rule)"
                ));
            }
        }
    }
    (names, warnings)
}

impl TurnOverrides {
    pub(crate) fn is_empty(&self) -> bool {
        self.allowed_tools.is_empty() && self.model.is_none()
    }

    /// Apply to a turn's run args when `turn_prompt` carries this command's
    /// prompt. `allowed-tools` replaces the launch `--tools` allowlist for the
    /// turn (it restricts, never unions); `model` replaces the turn model.
    pub(crate) fn apply(
        &self,
        turn_prompt: &str,
        allowed_tools: &mut Vec<String>,
        launch_tools: &mut Vec<String>,
        model: &mut Option<String>,
    ) -> bool {
        if !turn_prompt.contains(&self.prompt) {
            return false;
        }
        if !self.allowed_tools.is_empty() {
            launch_tools.clear();
            *allowed_tools = self.allowed_tools.clone();
        }
        if let Some(m) = &self.model {
            *model = Some(m.clone());
        }
        true
    }
}

/// Command roots in override order. Project roots only when `trusted`.
pub(crate) fn roots_for(
    workspace: &Path,
    trusted: bool,
    user_root: Option<PathBuf>,
) -> Vec<(PathBuf, CommandScope)> {
    let mut roots = Vec::new();
    if let Some(user) = user_root {
        roots.push((user.join("commands"), CommandScope::User));
    }
    if trusted {
        roots.push((
            workspace.join(".claude").join("commands"),
            CommandScope::Project,
        ));
        roots.push((
            workspace
                .join(crate::host_identity::home_dir())
                .join("commands"),
            CommandScope::Project,
        ));
    }
    roots
}

/// Load commands visible from `workspace` using the persisted trust store and
/// the real user state root.
pub(crate) fn load(workspace: &Path) -> Vec<CustomCommand> {
    let trusted = rightkit_agent_config::workspace_trust::is_workspace_trusted(workspace);
    discover(&roots_for(
        workspace,
        trusted,
        rightkit_agent_config::state_root::state_root(),
    ))
}

/// Build the prompt a custom command submits: arguments, then policy-gated
/// shell expansion, then `@file` references.
pub(crate) fn render(
    command: &CustomCommand,
    args: &str,
    mode: PermissionMode,
    trust: WorkspaceTrust,
    workspace: &Path,
) -> String {
    let substituted = substitute_arguments(&command.body, args);
    let shelled =
        crate::inline_shell_runner::expand_custom_command(&substituted, mode, trust, workspace);
    expand_file_references(&shelled, workspace)
        .trim()
        .to_string()
}

/// Find `name` (case-insensitive, leading `/` optional) and render it.
pub(crate) fn expand_by_name(
    commands: &[CustomCommand],
    name: &str,
    args: &str,
    mode: PermissionMode,
    trust: WorkspaceTrust,
    workspace: &Path,
) -> Option<TurnOverrides> {
    let command = resolve(commands, name)?;
    let catalog: Vec<String> =
        rightkit_agent_coding_tools::registry::default_tool_capabilities()
            .into_iter()
            .map(|capability| capability.name)
            .collect();
    let (allowed_tools, mut warnings) = map_allowed_tools(&command.allowed_tools, &catalog);
    // Fail open to the normal toolset (with a warning) rather than an empty
    // allowlist, which the run path would read as "restrict to nothing usable".
    if allowed_tools.is_empty() && !command.allowed_tools.is_empty() {
        warnings
            .push("allowed-tools: no recognized tools; the turn keeps its normal toolset".into());
    }
    for warning in &warnings {
        tracing::warn!(command = %command.name, "{warning}");
    }
    Some(TurnOverrides {
        prompt: render(command, args, mode, trust, workspace),
        allowed_tools,
        model: command.model.clone(),
        warnings,
    })
}

// ── popup / help projection ────────────────────────────────────────────────

struct Cache {
    workspace: PathBuf,
    loaded: Instant,
    commands: Vec<CustomCommand>,
}

static ACTIVE_WORKSPACE: Mutex<Option<PathBuf>> = Mutex::new(None);
static CACHE: Mutex<Option<Cache>> = Mutex::new(None);
const CACHE_TTL: Duration = Duration::from_secs(2);

/// Record the folder the interactive session runs in (launch folder, `/cd`).
pub(crate) fn set_active_workspace(workspace: &Path) {
    *ACTIVE_WORKSPACE.lock().unwrap_or_else(|e| e.into_inner()) = Some(workspace.to_path_buf());
}

fn active_workspace() -> PathBuf {
    ACTIVE_WORKSPACE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default()
}

/// Cached commands for the active workspace (re-read every couple of seconds
/// so a new file shows up without a restart).
pub(crate) fn cached_commands() -> Vec<CustomCommand> {
    let workspace = active_workspace();
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(cache) = guard.as_ref() {
        if cache.workspace == workspace && cache.loaded.elapsed() < CACHE_TTL {
            return cache.commands.clone();
        }
    }
    let commands = load(&workspace);
    *guard = Some(Cache {
        workspace,
        loaded: Instant::now(),
        commands: commands.clone(),
    });
    commands
}

/// `(name, description)` rows for the slash popup and `/help`. Names that a
/// built-in command already owns are dropped so a custom file never shadows one.
pub(crate) fn popup_rows() -> Vec<(String, String)> {
    let commands = cached_commands();
    listing(&commands)
        .into_iter()
        .filter(|(name, _)| {
            crate::cli::command_registry::CommandRouter
                .resolve(name)
                .is_none()
        })
        .map(|(name, c)| (name, c.summary()))
        .collect()
}
