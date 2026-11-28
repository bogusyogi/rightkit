//! Custom slash commands for the interactive host (Fixes #132;
//! <https://code.claude.com/docs/en/slash-commands>).
//!
//! Markdown files under the user state root `commands/` and, only when the
//! workspace is trusted, the project's `.claude/commands/` and
//! `.{app}/commands/` (later roots override earlier ones). Parsing,
//! discovery, `$ARGUMENTS`/`$1..$n` and `@file` live in
//! `rightkit_agent_skills::custom_commands`; a host that sets
//! `host_options::custom_command_template_arguments` uses this file's own template substitution
//! (`substitute`: `$@`, `${@:N:L}`, `${N:-default}`) instead; `!{cmd}` and backtick-bang run through
//! the policy-backed `inline_shell_runner::expand_custom_command`, so an
//! unapproved shell command is never executed.
//!
//! Frontmatter `allowed-tools` restricts, and `model` overrides, the one turn
//! the expanded command submits (`TurnOverrides`, applied in `spawn_turn`).

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
use rightkit_agent_skills::custom_commands::{
    discover, expand_file_references, listing, resolve, substitute_arguments, CommandScope,
    CustomCommand,
};

/// Per-turn settings from a command's frontmatter, tied to the prompt it expanded to.
/// `allowed_tools` holds host tool names (already mapped from Claude Code names).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TurnOverrides {
    pub prompt: String,
    pub allowed_tools: Vec<String>,
    pub model: Option<String>,
    /// Dropped names and notes to show the user; never a usage error.
    pub warnings: Vec<String>,
}

/// host tool names a Claude Code tool name stands for (empty: unknown).
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

/// Map Claude Code `allowed-tools` entries to host tool names available
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
                .join(rightkit_agent_config::state_dir_name())
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
    let mut roots = roots_for(
        workspace,
        trusted,
        rightkit_agent_config::state_root::state_root(),
    );
    // A host that sets `host_options::custom_commands_claude_user_folder` also reads the user's
    // own Claude Code commands folder, first, so the host's own and the project's files override it.
    if crate::host_options::custom_commands_claude_user_folder() {
        if let Some(home) = rightkit_agent_config::os_home_dir() {
            roots.insert(
                0,
                (home.join(".claude").join("commands"), CommandScope::User),
            );
        }
    }
    discover(&roots)
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
    let substituted = if crate::host_options::custom_command_template_arguments() {
        substitute(&command.body, args)
    } else {
        substitute_arguments(&command.body, args)
    };
    let shelled =
        crate::inline_shell_runner::expand_custom_command(&substituted, mode, trust, workspace);
    expand_file_references(&shelled, workspace)
        .trim()
        .to_string()
}

/// Split command arguments on whitespace with Pi semantics: a quote (`"` or
/// `'`) opens only at the start of a token, so `doesn't` stays one token. A
/// backslash before the closing quote character keeps it as text. An unmatched
/// opening quote is literal.
fn split_template_args(args: &str) -> Vec<String> {
    let chars: Vec<char> = args.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        let mut token = String::new();
        if chars[i] == '"' || chars[i] == '\'' {
            let quote = chars[i];
            let mut j = i + 1;
            let mut inner = String::new();
            let mut closed = false;
            while j < chars.len() {
                if chars[j] == '\\' && chars.get(j + 1) == Some(&quote) {
                    inner.push(quote);
                    j += 2;
                } else if chars[j] == quote {
                    closed = true;
                    break;
                } else {
                    inner.push(chars[j]);
                    j += 1;
                }
            }
            if closed {
                token = inner;
                i = j + 1;
            }
        }
        // Plain word; also the tail after a closing quote or an unmatched quote.
        while i < chars.len() && !chars[i].is_whitespace() {
            token.push(chars[i]);
            i += 1;
        }
        out.push(token);
    }
    out
}

/// `s` as a number when it is non-empty ASCII digits.
fn parse_digits(s: &str) -> Option<usize> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Expand the text between `${` and `}`; `None` leaves the form literal.
fn expand_braced(inner: &str, argv: &[String]) -> Option<String> {
    // Bare `${@}` and `${N}` (N >= 1), the brace spelling of `$@` and `$N`.
    if inner == "@" {
        return Some(argv.join(" "));
    }
    if let Some(n) = parse_digits(inner).filter(|&n| n >= 1) {
        return Some(argv.get(n - 1).cloned().unwrap_or_default());
    }
    if let Some(default) = inner.strip_prefix("@:-") {
        let all = argv.join(" ");
        return Some(if all.is_empty() {
            default.to_string()
        } else {
            all
        });
    }
    if let Some(spec) = inner.strip_prefix("@:") {
        let (start, len) = match spec.split_once(':') {
            Some((s, l)) => (parse_digits(s)?, Some(parse_digits(l)?)),
            None => (parse_digits(spec)?, None),
        };
        let from = start.max(1).saturating_sub(1).min(argv.len());
        let rest = &argv[from..];
        let take = len.unwrap_or(rest.len()).min(rest.len());
        return Some(rest[..take].join(" "));
    }
    let (n, default) = inner.split_once(":-")?;
    let n = parse_digits(n).filter(|&n| n >= 1)?;
    let value = argv.get(n - 1).map(String::as_str).unwrap_or("");
    Some(if value.is_empty() {
        default.to_string()
    } else {
        value.to_string()
    })
}

/// Single-pass template substitution; argument text and defaults are written
/// straight to the output and never rescanned. Forms:
/// - `$ARGUMENTS`: the raw args, trimmed.
/// - `$N` (N >= 1): the Nth positional argument, or empty.
/// - `$@`: all positionals joined with one space.
/// - `${@:N}` / `${@:N:L}`: positionals from N (1-based; 0 counts as 1), or L of
///   them; the start is clamped, never a panic.
/// - `${N:-default}` / `${@:-default}`: the value, or `default` (up to the
///   first `}`) when it is empty.
/// - `${N}` / `${@}`: the same as `$N` / `$@`.
///
/// `$0`, a bare `$`, an unclosed `${` and any unknown form stay literal.
fn substitute(body: &str, args: &str) -> String {
    let argv = split_template_args(args);
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(pos) = rest.find('$') {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 1..];
        if let Some(tail) = after.strip_prefix("ARGUMENTS") {
            out.push_str(args.trim());
            rest = tail;
        } else if let Some(tail) = after.strip_prefix('@') {
            out.push_str(&argv.join(" "));
            rest = tail;
        } else if let Some(braced) = after.strip_prefix('{') {
            let expanded = braced
                .find('}')
                .and_then(|end| expand_braced(&braced[..end], &argv).map(|v| (v, end)));
            if let Some((value, end)) = expanded {
                out.push_str(&value);
                rest = &braced[end + 1..];
            } else {
                out.push('$');
                rest = after;
            }
        } else {
            let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
            match digits.parse::<usize>() {
                Ok(n) if n >= 1 => {
                    out.push_str(argv.get(n - 1).map(String::as_str).unwrap_or(""));
                    rest = &after[digits.len()..];
                }
                _ => {
                    out.push('$');
                    rest = after;
                }
            }
        }
    }
    out.push_str(rest);
    out
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
    let catalog: Vec<String> = rightkit_agent_coding_tools::registry::default_tool_capabilities()
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
    // `allowed-tools` limits the turn (it does not pre-approve calls): say so, so a command copied
    // from Claude Code does not silently lose the tools it did not list.
    if !allowed_tools.is_empty() {
        warnings.push(format!(
            "allowed-tools: this turn can use only {}; other tools are off for it",
            allowed_tools.join(", ")
        ));
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
    use super::{split_template_args, substitute};

    #[test]
    fn reference_case_and_no_rescan() {
        assert_eq!(
            substitute("a $1 ${2:-x} $@ ${@:2} ${@:1:1} $ARGUMENTS $0 $", "'$2' b"),
            "a $2 b $2 b b $2 '$2' b $0 $"
        );
        assert_eq!(substitute("$1 $2", "$2 b"), "$2 b");
        assert_eq!(substitute("$@", "$ARGUMENTS x"), "$ARGUMENTS x");
        assert_eq!(substitute("${1:-$2}", ""), "$2");
    }

    #[test]
    fn arguments_and_positionals() {
        assert_eq!(substitute("[$ARGUMENTS]", "  a b  "), "[a b]");
        assert_eq!(substitute("$1-$3", "a b"), "a-");
        assert_eq!(substitute("$10", "1 2 3 4 5 6 7 8 9 ten"), "ten");
        assert_eq!(substitute("$@", "a 'b c'"), "a b c");
    }

    #[test]
    fn slices_clamp_and_never_panic() {
        assert_eq!(substitute("${@:2}", "a b c"), "b c");
        assert_eq!(substitute("${@:2:1}", "a b c"), "b");
        assert_eq!(substitute("${@:0}", "a b c"), "a b c");
        assert_eq!(substitute("${@:2:0}", "a b c"), "");
        assert_eq!(substitute("${@:9}", "a b c"), "");
        assert_eq!(substitute("${@:2:99}", "a b c"), "b c");
    }

    #[test]
    fn defaults() {
        assert_eq!(substitute("${1:-x}", ""), "x");
        assert_eq!(substitute("${1:-x}", "a"), "a");
        assert_eq!(substitute("${1:-x}", "\"\""), "x");
        assert_eq!(substitute("${@:-none}", ""), "none");
        assert_eq!(substitute("${@:-none}", "a b"), "a b");
        assert_eq!(substitute("${2:-a b}!", "z"), "a b!");
    }

    #[test]
    fn unknown_forms_stay_literal() {
        assert_eq!(substitute("$0 $ $x", "a"), "$0 $ $x");
        assert_eq!(substitute("${1 and $", "a"), "${1 and $");
        assert_eq!(
            substitute("${@:z} ${0:-d} ${0}", "a"),
            "${@:z} ${0:-d} ${0}"
        );
        assert_eq!(substitute("${1}|${2}|${@}", "a b"), "a|b|a b");
    }

    #[test]
    fn tokenizer_uses_pi_quote_rules() {
        assert_eq!(split_template_args("doesn't matter"), ["doesn't", "matter"]);
        assert_eq!(substitute("$1|$2", "doesn't matter"), "doesn't|matter");
        assert_eq!(split_template_args("'a b' c"), ["a b", "c"]);
        assert_eq!(split_template_args("\"abc d"), ["\"abc", "d"]);
        assert_eq!(
            split_template_args(r#""the \"fast\" path""#),
            ["the \"fast\" path"]
        );
        assert!(split_template_args("  ").is_empty());
    }
}
