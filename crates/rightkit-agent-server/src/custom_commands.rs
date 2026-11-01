//! Custom slash commands for the interactive host (Fixes #132;
//! https://code.claude.com/docs/en/slash-commands).
//!
//! Markdown files under the user state root `commands/` and, only when the
//! workspace is trusted, the project's `.claude/commands/` and
//! `.coderight/commands/` (later roots override earlier ones). Parsing,
//! discovery, `$ARGUMENTS`/`$1..$n` and `@file` live in
//! `coderight_skills::custom_commands`; `!{cmd}` and backtick-bang run through
//! the policy-backed `inline_shell_runner::expand_custom_command`, so an
//! unapproved shell command is never executed.
//!
//! Frontmatter `allowed-tools` restricts, and `model` overrides, the one turn
//! the expanded command submits (`TurnOverrides`, applied in `spawn_turn`).

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use coderight_skills::custom_commands::{
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
            workspace.join(".coderight").join("commands"),
            CommandScope::Project,
        ));
    }
    roots
}

/// Load commands visible from `workspace` using the persisted trust store and
/// the real user state root.
pub(crate) fn load(workspace: &Path) -> Vec<CustomCommand> {
    let trusted =
        rightkit_app_settings::agent_config::workspace_trust::is_workspace_trusted(workspace);
    discover(&roots_for(
        workspace,
        trusted,
        rightkit_app_settings::agent_config::state_root::state_root(),
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
        rightkit_agent_tools::coding_tools::registry::default_tool_capabilities()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, text: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn project_roots_require_workspace_trust() {
        let ws = Path::new("/w");
        let user = Some(PathBuf::from("/u"));
        let untrusted = roots_for(ws, false, user.clone());
        assert_eq!(untrusted.len(), 1);
        assert_eq!(untrusted[0].1, CommandScope::User);
        let trusted = roots_for(ws, true, user);
        assert_eq!(trusted.len(), 3);
        assert!(
            trusted[1].0.ends_with(".claude/commands")
                || trusted[1].0.ends_with(".claude\\commands")
        );
        assert!(trusted[2].0.to_string_lossy().contains(".coderight"));
    }

    #[test]
    fn untrusted_workspace_commands_are_not_discovered() {
        let ws = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        write(ws.path(), ".claude/commands/evil.md", "x");
        write(
            ws.path(),
            ".coderight/commands/team/deploy.md",
            "---\ndescription: Ship\n---\nd",
        );
        write(user.path(), "commands/mine.md", "m");
        let names = |trusted| {
            discover(&roots_for(
                ws.path(),
                trusted,
                Some(user.path().to_path_buf()),
            ))
            .into_iter()
            .map(|c| c.name)
            .collect::<Vec<_>>()
        };
        assert_eq!(names(false), vec!["mine"]);
        assert_eq!(names(true), vec!["evil", "mine", "team:deploy"]);
    }

    #[test]
    fn render_substitutes_arguments_and_file_references() {
        let ws = tempfile::tempdir().unwrap();
        write(ws.path(), "notes.txt", "remember\n");
        let cmd = coderight_skills::custom_commands::parse_command(
            "fix",
            "---\ndescription: Fix\n---\nFix $1 ($ARGUMENTS) using @notes.txt",
            CommandScope::Project,
            Path::new("fix.md"),
        );
        let out = render(
            &cmd,
            "bug-7 now",
            PermissionMode::Ask,
            WorkspaceTrust::Trusted,
            ws.path(),
        );
        assert!(
            out.starts_with("Fix bug-7 (bug-7 now) using @notes.txt\n```\nremember\n```"),
            "{out}"
        );
    }

    #[test]
    fn bare_name_resolves_namespaced_command() {
        let cmds = vec![coderight_skills::custom_commands::parse_command(
            "team:deploy",
            "go",
            CommandScope::Project,
            Path::new("d.md"),
        )];
        let ws = tempfile::tempdir().unwrap();
        let out = expand_by_name(
            &cmds,
            "/deploy",
            "",
            PermissionMode::Ask,
            WorkspaceTrust::Trusted,
            ws.path(),
        );
        assert_eq!(out.unwrap().prompt, "go");
    }

    #[test]
    fn frontmatter_tools_and_model_become_turn_overrides() {
        let cmds = vec![coderight_skills::custom_commands::parse_command(
            "audit",
            "---\nallowed-tools: Read, Grep\nmodel: fast-model\n---\nlook $1",
            CommandScope::Project,
            Path::new("a.md"),
        )];
        let ws = tempfile::tempdir().unwrap();
        let o = expand_by_name(
            &cmds,
            "/audit",
            "x",
            PermissionMode::Ask,
            WorkspaceTrust::Trusted,
            ws.path(),
        )
        .unwrap();
        assert_eq!(o.prompt, "look x");
        assert!(!o.is_empty());

        let mut allowed = vec!["bash".to_string()];
        let mut launch_tools = vec!["edit_*".to_string()];
        let mut model = Some("slow".to_string());
        // A different prompt is untouched.
        assert!(!o.apply(
            "something else",
            &mut allowed,
            &mut launch_tools,
            &mut model
        ));
        assert_eq!(allowed, vec!["bash"]);
        assert_eq!(model.as_deref(), Some("slow"));
        // The command's own (possibly framed) prompt restricts tools and swaps the model.
        assert!(o.apply("[ctx]\nlook x", &mut allowed, &mut launch_tools, &mut model));
        assert_eq!(allowed, vec!["read_file", "search"]);
        assert!(launch_tools.is_empty());
        assert_eq!(model.as_deref(), Some("fast-model"));
    }

    fn catalog() -> Vec<String> {
        [
            "shell",
            "read_file",
            "edit_file",
            "multi_edit",
            "write_file",
            "search",
            "list_files",
            "browser_fetch",
            "mcp_call",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    fn entries(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn claude_tool_names_map_to_coderight_tools() {
        let (names, warnings) = map_allowed_tools(
            &entries(&[
                "Read",
                "Edit",
                "Write",
                "Grep",
                "Glob",
                "WebFetch",
                "mcp__github__create_issue",
            ]),
            &catalog(),
        );
        assert_eq!(
            names,
            vec![
                "read_file",
                "edit_file",
                "multi_edit",
                "write_file",
                "search",
                "list_files",
                "browser_fetch",
                "mcp_call"
            ]
        );
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn bash_prefix_allows_only_the_shell_tool_and_leaves_prefix_to_approval() {
        let (names, warnings) = map_allowed_tools(&entries(&["Bash(git status:*)"]), &catalog());
        assert_eq!(names, vec!["shell"]);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("git status:*") && warnings[0].contains("normal approval"));
        let (names, warnings) = map_allowed_tools(&entries(&["Bash"]), &catalog());
        assert_eq!(names, vec!["shell"]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn unknown_and_unavailable_names_are_dropped_with_a_warning() {
        let (names, warnings) = map_allowed_tools(
            &entries(&["Read", "Teleport", "NotebookEdit", "read_file"]),
            &catalog(),
        );
        assert_eq!(names, vec!["read_file"]);
        assert_eq!(warnings.len(), 2);
        assert!(warnings[0].contains("Teleport"));
        assert!(warnings[1].contains("NotebookEdit"));
    }

    #[test]
    fn all_unknown_entries_keep_the_normal_toolset() {
        let cmds = vec![coderight_skills::custom_commands::parse_command(
            "odd",
            "---\nallowed-tools: Teleport\n---\nhi",
            CommandScope::User,
            Path::new("o.md"),
        )];
        let ws = tempfile::tempdir().unwrap();
        let o = expand_by_name(
            &cmds,
            "/odd",
            "",
            PermissionMode::Ask,
            WorkspaceTrust::Trusted,
            ws.path(),
        )
        .unwrap();
        assert!(o.allowed_tools.is_empty());
        assert!(o.warnings.iter().any(|w| w.contains("Teleport")));
        assert!(o.warnings.iter().any(|w| w.contains("normal toolset")));
    }

    #[test]
    fn command_without_frontmatter_overrides_nothing() {
        let o = TurnOverrides {
            prompt: "p".into(),
            allowed_tools: vec![],
            model: None,
            warnings: vec![],
        };
        let (mut a, mut t, mut m) = (vec!["x".to_string()], vec![], Some("m".to_string()));
        assert!(o.is_empty());
        assert!(o.apply("p", &mut a, &mut t, &mut m));
        assert_eq!(a, vec!["x"]);
        assert_eq!(m.as_deref(), Some("m"));
    }

    #[test]
    fn render_never_runs_unapproved_shell() {
        let ws = tempfile::tempdir().unwrap();
        let marker = ws.path().join("ran.txt");
        let body = format!("a !{{echo hi > \"{}\"}} b", marker.display());
        let cmd = coderight_skills::custom_commands::parse_command(
            "sh",
            &body,
            CommandScope::User,
            Path::new("sh.md"),
        );
        let out = render(
            &cmd,
            "",
            PermissionMode::Ask,
            WorkspaceTrust::Trusted,
            ws.path(),
        );
        assert!(out.contains("command not run"), "{out}");
        assert!(!marker.exists());
    }

    #[test]
    fn expand_by_name_finds_namespaced_commands_case_insensitively() {
        let cmds = vec![coderight_skills::custom_commands::parse_command(
            "team:deploy",
            "go $1",
            CommandScope::Project,
            Path::new("d.md"),
        )];
        let ws = tempfile::tempdir().unwrap();
        let out = expand_by_name(
            &cmds,
            "/Team:Deploy",
            "prod",
            PermissionMode::Ask,
            WorkspaceTrust::Trusted,
            ws.path(),
        );
        assert_eq!(out.unwrap().prompt, "go prod");
        assert!(expand_by_name(
            &cmds,
            "/nope",
            "",
            PermissionMode::Ask,
            WorkspaceTrust::Trusted,
            ws.path()
        )
        .is_none());
    }
}
