#[derive(Clone)]
struct RunArgs {
    prompt: String,
    print: bool,
    output_format: OutputFormat,
    json_event: bool,
    model: Option<String>,
    /// `None` means "no explicit provider flag": the daemon resolves the
    /// configured default/provider routes. Must stay distinguishable from an
    /// explicit `--provider` value so a bare run never pins a provider.
    provider: Option<String>,
    effort: Option<String>,
    workspace: Option<String>,
    /// Explicit session permission mode (`--permission-mode`, launch flags
    /// `--ask` / `--accept-edits` / `--auto` / `--yolo`, `/yolo`, Shift+Tab).
    /// Safe mode still wins. This is the field turn dispatch reads for the
    /// live mode (e.g. vendor-CLI runtimes mapping yolo to skip-permissions).
    permission_mode: Option<rightkit_agent_policy::PermissionMode>,
    stream_json: bool,
    exit_on_error: bool,
    max_turns: Option<u32>,
    allowed_tools: Vec<String>,
    disallowed_tools: Vec<String>,
    toolset: Option<String>,
    /// Tool profile for this session's own tools only (`--own-toolset`); children do not inherit it.
    own_toolset: Option<String>,
    /// `--trust-workspace`: a hosting daemon receives a per-session grant (issue 445).
    trust_workspace: bool,
    max_budget_usd: Option<f64>,
    bare: bool,
    /// Strict policy / permission tier for the whole run. When true the
    /// session engine runs in safe-mode (Untrusted trust, no permission
    /// rules, read-only-capable tool registry) regardless of --bypass or
    /// --auto-approve. See CLI --safe-mode doc in cli_definitions_command.rs.
    safe_mode: bool,
    system_prompt: Option<String>,
    system_prompt_file: Option<String>,
    append_system_prompt: Option<String>,
    append_system_prompt_file: Option<String>,
    mcp_config: Option<String>,
    session_id: Option<String>,
    add_dir: Vec<String>,
    /// Prompt input format. `text` (default) reads the positional prompt;
    /// `stream-json` reads JSONL events from stdin and drives the run from
    /// them (FEATURE-MATRIX par §#32 + #35).
    input_format: InputFormat,
    output_schema: Option<String>,
    image: Vec<String>,
    ephemeral: bool,
    output_last_message: Option<String>,
    color: crate::cli::CliColor,
    tools: Vec<String>,
    exclude_tools: Vec<String>,
    no_mcp: bool,
    /// True for a headless `run` (and `-p`): no person is attached to approve a call or answer
    /// a question. The session is told so (`UNATTENDED_RUN_NOTE`). False for the TUI's turns.
    unattended: bool,
    /// Hidden `--approval-callback` (issue 558): the run asks the attached caller to approve calls.
    approval_callback: bool,
}

/// Appended to the system prompt of a headless run. Task-set run on build 94 (2026-11-11):
/// after one refused shell call Haiku stopped and asked the user to approve it, in a run where
/// nobody can answer (5 of 25 tasks left undone).
pub(crate) const UNATTENDED_RUN_NOTE: &str = "\
<unattended_run>
This run is unattended: no person is attached, so nobody can approve a call or answer a
question. A refused call stays refused for the whole run. Do not stop to ask for approval,
confirmation or a decision: choose the most reasonable reading, continue with what is allowed
(file reads and searches, file edits, plain shell commands), and finish the task. In your last
message say what you could not run or verify.
</unattended_run>";

/// Classes the legacy error-text and protocol-event classifiers produce. The
/// process exit and `error.code` come from [`RunExitCode`] (the suite codes
/// 0 to 3); this enum only names what the classifiers found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CliExitCode {
    Generic,
    Usage,
    Auth,
    Permission,
    Budget,
    Sandbox,
    TimeoutStall,
}

type CliToolFilters = (Option<String>, Vec<String>, Vec<String>);

/// Built-in tools that front MCP servers. `--no-mcp` and `--exclude-tools
/// 'mcp__*'` remove them; `--tools` keeps them unless it names `mcp__` tools.
const MCP_GATEWAY_TOOLS: [&str; 5] = [
    "mcp_call",
    "list_mcp_tools",
    "list_mcp_sources",
    "list_mcp_resources",
    "read_mcp_resource",
];

/// Non-matching allowlist entry that keeps a fully-excluded allowlist
/// non-empty (an empty allowlist would mean "allow everything").
const CLI_NO_TOOLS_SENTINEL: &str = "__host_no_tools__";

#[cfg(test)] // used only by the #[cfg(test)] tool_delta_tests module (clippy sweep)
fn resolve_cli_tool_filters(
    toolset: Option<&str>,
    allowed_tools: Vec<String>,
    disallowed_tools: Vec<String>,
) -> Result<CliToolFilters, String> {
    resolve_cli_tool_filters_with_mcp(toolset, allowed_tools, disallowed_tools, false)
}

/// Resolve `--tools` / `--exclude-tools` / `--no-mcp` into the canonical
/// allow and deny lists consumed by the session registry.
///
/// Semantics are pinned to pi's CLI reference
/// <https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/docs/cli.md>:
/// entries are tool names or patterns where `*` matches any characters;
/// `--tools` is an allowlist that keeps MCP tools unless an entry starts with
/// `mcp__`; `--exclude-tools` applies after selection and removes MCP tools
/// too; `--no-mcp` disables MCP. A list whose entries all name nothing known
/// is a usage error. Qualified `mcp__server__tool` entries are discovered at
/// runtime, so they pass through verbatim for the MCP manager to match.
fn resolve_cli_tool_filters_with_mcp(
    toolset: Option<&str>,
    allowed_tools: Vec<String>,
    disallowed_tools: Vec<String>,
    no_mcp: bool,
) -> Result<CliToolFilters, String> {
    let catalog = rightkit_agent_coding_tools::registry::default_tool_capabilities()
        .into_iter()
        .map(|capability| capability.name)
        .collect::<Vec<_>>();
    let (allowed_tools, added_tools, removed_tools) = split_cli_tool_deltas(allowed_tools);
    // `--tools -name` removes a tool from the default set, `--tools +name` names one
    // that is already in it: both are deltas, never a plain allowlist.
    let removed_tools = expand_cli_tool_globs("--tools -name", removed_tools, &catalog)?;
    let added_tools = expand_cli_tool_globs("--tools +name", added_tools, &catalog)?;
    let mut allowed_tools = expand_cli_tool_globs("--tools", allowed_tools, &catalog)?;
    if !allowed_tools.is_empty() {
        // A plain allowlist exists: a `+name` extends it.
        allowed_tools.extend(added_tools);
        allowed_tools.sort();
        allowed_tools.dedup();
    }
    let mut disallowed_tools =
        expand_cli_tool_globs("--exclude-tools", disallowed_tools, &catalog)?;
    disallowed_tools.extend(removed_tools);
    disallowed_tools.sort();
    disallowed_tools.dedup();
    // Pi keeps MCP tools under `--tools`; the gateway tools are how this
    // runtime reaches them, so they stay in a non-empty allowlist.
    if !allowed_tools.is_empty() {
        for gateway in MCP_GATEWAY_TOOLS {
            if !allowed_tools.iter().any(|name| name == gateway) {
                allowed_tools.push(gateway.to_string());
            }
        }
    }
    if no_mcp
        || disallowed_tools
            .iter()
            .any(|name| cli_tool_glob_covers_all_mcp(name))
    {
        for gateway in MCP_GATEWAY_TOOLS {
            if !disallowed_tools.iter().any(|name| name == gateway) {
                disallowed_tools.push(gateway.to_string());
            }
        }
    }
    // Exclusion wins over inclusion: drop every excluded name from the
    // allowlist. An empty allowlist means "allow everything", so if this
    // empties a non-empty allowlist, keep a placeholder that names no real
    // tool; the allowlist stays non-empty and still denies everything.
    if !allowed_tools.is_empty() {
        allowed_tools.retain(|name| {
            !disallowed_tools.iter().any(|excluded| {
                excluded == name
                    || (excluded.contains('*') && cli_tool_glob_matches(excluded, name))
            })
        });
        if allowed_tools.is_empty() {
            allowed_tools.push(CLI_NO_TOOLS_SENTINEL.to_string());
        }
    }
    let profile = match toolset.map(str::trim).filter(|value| !value.is_empty()) {
        Some(id) => Some(
            api_toolsets::resolve_toolset_profile(id)
                .map_err(|_| format!("Unknown toolset '{id}'."))?,
        ),
        None => None,
    };
    let active_toolset = profile.as_ref().map(|profile| profile.id.clone());
    let (allowed_tools, disallowed_tools) =
        api_toolsets::apply_toolset_profile(profile.as_ref(), allowed_tools, disallowed_tools);
    Ok((active_toolset, allowed_tools, disallowed_tools))
}

/// Split `--tools` entries into plain names, `+name` additions and `-name`
/// removals (the sign is stripped). With no plain name the allowlist stays
/// empty, which means "the default tool set", so a list of only deltas edits
/// that set instead of replacing it.
fn split_cli_tool_deltas(entries: Vec<String>) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut plain = Vec::new();
    let mut added = Vec::new();
    let mut removed = Vec::new();
    for entry in entries {
        let trimmed = entry.trim();
        if let Some(name) = trimmed.strip_prefix('+') {
            added.push(name.trim().to_string());
        } else if let Some(name) = trimmed.strip_prefix('-') {
            removed.push(name.trim().to_string());
        } else {
            plain.push(entry);
        }
    }
    (plain, added, removed)
}

/// True for an exclude pattern that covers every MCP tool name (`mcp__*`,
/// `mcp_*`, `*`), so the MCP gateway tools must go with it.
fn cli_tool_glob_covers_all_mcp(pattern: &str) -> bool {
    pattern.contains('*')
        && cli_tool_glob_matches(pattern, "mcp__x__y")
        && cli_tool_glob_matches(pattern, "mcp__")
}

fn expand_cli_tool_globs(
    flag: &str,
    patterns: Vec<String>,
    catalog: &[String],
) -> Result<Vec<String>, String> {
    let mut expanded = Vec::new();
    let mut unknown = Vec::new();
    let mut resolved_any = false;
    let mut seen_any = false;
    for pattern in patterns {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            continue;
        }
        seen_any = true;
        let is_mcp = pattern.starts_with("mcp__");
        if pattern.contains('*') {
            let matches = catalog
                .iter()
                .filter(|name| cli_tool_glob_matches(pattern, name))
                .cloned()
                .collect::<Vec<_>>();
            if is_mcp {
                // Discovered at runtime; the MCP manager matches the pattern.
                expanded.push(pattern.to_string());
                resolved_any = true;
            } else if matches.is_empty() {
                unknown.push(pattern.to_string());
            } else {
                resolved_any = true;
                expanded.extend(matches);
            }
        } else if is_mcp || catalog.iter().any(|name| name == pattern) {
            resolved_any = true;
            expanded.push(pattern.to_string());
        } else {
            unknown.push(pattern.to_string());
            // Exact names stay in the list: plugin tools register after
            // discovery, so they cannot be proven absent from the catalog.
            expanded.push(pattern.to_string());
        }
    }
    if seen_any && !resolved_any {
        return Err(format!(
            "{flag} matched no known tools: {}",
            unknown.join(", ")
        ));
    }
    expanded.sort();
    expanded.dedup();
    Ok(expanded)
}

fn cli_tool_glob_matches(pattern: &str, value: &str) -> bool {
    // Pi semantics: `*` is the only wildcard; every other character is literal.
    let pattern = pattern.chars().collect::<Vec<_>>();
    let value = value.chars().collect::<Vec<_>>();
    let mut previous = vec![false; value.len() + 1];
    previous[0] = true;
    for token in pattern {
        let mut current = vec![false; value.len() + 1];
        if token == '*' {
            current[0] = previous[0];
            for index in 1..=value.len() {
                current[index] = previous[index] || current[index - 1];
            }
        } else {
            for index in 1..=value.len() {
                current[index] = previous[index - 1] && token == value[index - 1];
            }
        }
        previous = current;
    }
    previous[value.len()]
}

#[cfg(test)]
mod tool_delta_tests {
    use super::*;

    fn filters(tools: &[&str], exclude: &[&str]) -> (Vec<String>, Vec<String>) {
        let (_, allowed, denied) = resolve_cli_tool_filters(
            None,
            tools.iter().map(|s| s.to_string()).collect(),
            exclude.iter().map(|s| s.to_string()).collect(),
        )
        .expect("filters resolve");
        (allowed, denied)
    }

    #[test]
    fn a_minus_entry_removes_one_tool_from_the_default_set() {
        let (allowed, denied) = filters(&["-shell"], &[]);
        assert!(allowed.is_empty(), "the default set stays: {allowed:?}");
        assert!(denied.iter().any(|name| name == "shell"));
    }

    #[test]
    fn a_plus_entry_beside_a_minus_entry_is_still_a_delta() {
        let (allowed, denied) = filters(&["+read_file", "-shell"], &[]);
        assert!(allowed.is_empty(), "no plain name: {allowed:?}");
        assert!(denied.iter().any(|name| name == "shell"));
    }

    #[test]
    fn a_minus_glob_expands_over_the_catalogue() {
        let (_, denied) = filters(&["-read_*"], &[]);
        assert!(denied.iter().any(|name| name == "read_file"), "{denied:?}");
    }

    #[test]
    fn a_plain_allowlist_keeps_working_and_a_plus_entry_extends_it() {
        let (allowed, denied) = filters(&["read_file", "+list_files"], &[]);
        assert!(allowed.iter().any(|name| name == "read_file"));
        assert!(allowed.iter().any(|name| name == "list_files"));
        assert!(denied.is_empty());
        // The only plain name is removed again: what is left of the allowlist is the MCP gateway, which an
        // allowlist keeps (`--tools` keeps MCP tools unless an entry starts with `mcp__`).
        let (allowed, _) = filters(&["read_file", "-read_file"], &[]);
        assert!(
            !allowed.iter().any(|name| name == "read_file"),
            "{allowed:?}"
        );
        assert!(
            allowed
                .iter()
                .all(|name| MCP_GATEWAY_TOOLS.contains(&name.as_str())),
            "{allowed:?}"
        );
    }

    #[test]
    fn a_delta_that_names_nothing_known_is_a_usage_error() {
        let error = resolve_cli_tool_filters(None, vec!["-nosuchtool".into()], vec![])
            .expect_err("unknown removal");
        assert!(error.contains("matched no known tools"), "{error}");
    }

    #[test]
    fn deltas_are_split_without_their_sign() {
        let (plain, added, removed) =
            split_cli_tool_deltas(vec!["a".into(), "+b".into(), " -c ".into()]);
        assert_eq!(
            (plain, added, removed),
            (
                vec!["a".to_string()],
                vec!["b".to_string()],
                vec!["c".to_string()]
            )
        );
    }
}
