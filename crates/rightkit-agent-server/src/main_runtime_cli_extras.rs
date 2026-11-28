// CLI verbs and flags that stand on their own: `login` / `logout` for any
// provider, `run --worktree` and `apply`, and `import claude-code`.
//
// Included as a module of `main_runtime` (see `main_runtime.rs`).

// ── login / logout ───────────────────────────────────────────────────────────

/// `{app} login PROVIDER [--with-api-key]`.
///
/// `github` is the device flow. `PROVIDER --with-api-key` reads the key from
/// stdin and stores it exactly as `provider set-key` does (a dry run without
/// `--yes`, like every other write). Without the flag a provider signs in the
/// way `provider login` does (OAuth, or a vendor CLI's own sign-in).
async fn cli_login(provider: &str, with_api_key: bool, yes: bool) -> i32 {
    let provider = provider.trim();
    if provider.eq_ignore_ascii_case("github") {
        if with_api_key {
            eprintln!(
                "GitHub signs in with the device flow, not an API key; run `{} login github`.",
                crate::host_identity::app()
            );
            return 2;
        }
        let result =
            tokio::task::spawn_blocking(crate::github_signin::device_sign_in_with_terminal).await;
        return match result {
            Ok(Ok(())) => {
                println!("GitHub sign-in saved.");
                0
            }
            Ok(Err(error)) => {
                eprintln!("GitHub sign-in failed: {error}");
                1
            }
            Err(error) => {
                eprintln!("GitHub sign-in failed: {error}");
                1
            }
        };
    }
    if provider.is_empty() {
        eprintln!(
            "`{} login` needs a provider id",
            crate::host_identity::app()
        );
        return 2;
    }
    if with_api_key {
        if !yes {
            return cli_gate::dry_run_exit(
                "login",
                "write",
                serde_json::json!({
                    "provider": provider,
                    "would": "read an API key from stdin and store it for this provider",
                }),
            );
        }
        return cli_provider_set_key(provider);
    }
    cli_provider(ProviderCommand::Login {
        provider: provider.to_string(),
    })
    .await
}

/// `{app} logout PROVIDER`: GitHub's sign-in, or what `provider logout`
/// removes (an OAuth sign-in, or the stored API key).
async fn cli_logout(provider: &str) -> i32 {
    let provider = provider.trim();
    if provider.eq_ignore_ascii_case("github") {
        return match crate::github_signin::sign_out() {
            Ok(()) => {
                println!("GitHub sign-in removed.");
                0
            }
            Err(error) => {
                eprintln!("GitHub sign-out failed: {error}");
                1
            }
        };
    }
    if provider.is_empty() {
        eprintln!(
            "`{} logout` needs a provider id",
            crate::host_identity::app()
        );
        return 2;
    }
    cli_provider(ProviderCommand::Logout {
        provider: provider.to_string(),
    })
    .await
}

// ── run --worktree, apply ────────────────────────────────────────────────────

/// Run `git -C dir ARGS`; stdout on success, git's stderr as the error.
fn worktree_git(dir: &std::path::Path, args: &[&str]) -> Result<String, String> {
    let mut command = rightkit_child_process::command_std("git");
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(std::process::Stdio::null());
    let output = command
        .output()
        .map_err(|error| format!("git is not available: {error}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(detail.trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}

/// The name a `--worktree` flag asks for: the given word, or a generated one
/// when the flag had no value. A word with spaces is a prompt in the wrong
/// place, so it is refused with the way out.
fn run_worktree_name(given: &str, now_secs: u64) -> Result<String, String> {
    let given = given.trim();
    if given.is_empty() {
        return Ok(format!("run-{now_secs}"));
    }
    let valid = given.len() <= 64
        && !given.starts_with(['.', '-'])
        && !given.ends_with('.')
        && !given.contains("..")
        && given
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !valid {
        return Err(format!(
            "`--worktree` takes a short name (letters, digits, `-`, `_`, `.`), not {given:?}; \
             put the prompt first or write `--worktree=` for a generated name"
        ));
    }
    Ok(given.to_string())
}

/// Where the worktrees of `run --worktree` live: `<repo>/<state dir>/worktrees`
/// (the folder the engine already keeps out of `git status`).
fn run_worktrees_root(repo_top: &std::path::Path) -> std::path::PathBuf {
    repo_top
        .join(rightkit_agent_config::state_dir_name())
        .join("worktrees")
}

/// `run --worktree [NAME]`: add a git worktree on a new branch `{app}/NAME`
/// from HEAD and return the directory the run should use as its workspace (the
/// same sub-folder of the worktree when `workspace` is below the repository
/// root). The caller has already refused an untrusted workspace.
fn prepare_run_worktree(workspace: &str, name: &str) -> Result<std::path::PathBuf, String> {
    let workspace_path = std::path::Path::new(workspace)
        .canonicalize()
        .map_err(|error| format!("workspace {workspace}: {error}"))?;
    let top = worktree_git(&workspace_path, &["rev-parse", "--show-toplevel"])
        .map_err(|error| format!("`--worktree` needs a git repository: {error}"))?;
    let top = std::path::PathBuf::from(top)
        .canonicalize()
        .map_err(|error| error.to_string())?;
    worktree_git(&top, &["rev-parse", "--verify", "--quiet", "HEAD"])
        .map_err(|_| "`--worktree` needs a repository with at least one commit".to_string())?;
    let target = run_worktrees_root(&top).join(name);
    if target.exists() {
        return Err(format!(
            "worktree {name} already exists at {}; pick another name, or remove it with `git worktree remove`",
            target.display()
        ));
    }
    std::fs::create_dir_all(run_worktrees_root(&top)).map_err(|error| error.to_string())?;
    // Keep the engine's worktrees out of the user's `git status` (a no-op after the first time).
    let _ = rightkit_agent_coding_tools::git_read::ignore_runtime_worktrees(&top);
    let branch = format!("{}/{name}", crate::host_identity::app());
    let target_text = target.to_string_lossy().into_owned();
    worktree_git(
        &top,
        &["worktree", "add", "-b", &branch, &target_text, "HEAD"],
    )
    .map_err(|error| format!("git worktree add failed: {error}"))?;
    let relative = workspace_path
        .strip_prefix(&top)
        .map(std::path::Path::to_path_buf)
        .unwrap_or_default();
    eprintln!(
        "[worktree] {} on branch {branch}; `{} apply {name}` brings the result back",
        target.display(),
        crate::host_identity::app()
    );
    Ok(target.join(relative))
}

/// The worktree `apply` means: the named one, or the newest under the root.
fn pick_run_worktree(
    root: &std::path::Path,
    name: Option<&str>,
) -> Result<std::path::PathBuf, String> {
    if let Some(name) = name.map(str::trim).filter(|name| !name.is_empty()) {
        if name.contains(['/', '\\']) || name.starts_with('.') {
            return Err(format!("`{name}` is not a worktree name"));
        }
        let dir = root.join(name);
        return if dir.is_dir() {
            Ok(dir)
        } else {
            Err(format!(
                "no worktree named {name} under {} (`run --worktree {name}` makes one)",
                root.display()
            ))
        };
    }
    let mut newest: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let dir = entry.path();
            let Ok(meta) = std::fs::metadata(dir.join(".git")) else {
                continue;
            };
            let when = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            if newest.as_ref().is_none_or(|(best, _)| when > *best) {
                newest = Some((when, dir));
            }
        }
    }
    newest
        .map(|(_, dir)| dir)
        .ok_or_else(|| format!("no `run --worktree` worktree under {}", root.display()))
}

/// `{app} apply [NAME]`: `git apply` the worktree's diff (tracked edits and
/// new files; the state folder's bookkeeping left out) to this checkout. The
/// checkout is checked first and changed only when the whole diff applies;
/// nothing is committed or staged.
fn cli_apply_worktree(name: Option<&str>, workspace: Option<&str>) -> i32 {
    let workspace = workspace
        .map(str::to_string)
        .unwrap_or_else(default_cli_workspace);
    let top = match worktree_git(
        std::path::Path::new(&workspace),
        &["rev-parse", "--show-toplevel"],
    ) {
        Ok(top) => std::path::PathBuf::from(top),
        Err(error) => {
            eprintln!(
                "`{} apply` needs a git repository: {error}",
                crate::host_identity::app()
            );
            return 1;
        }
    };
    let worktree = match pick_run_worktree(&run_worktrees_root(&top), name) {
        Ok(dir) => dir,
        Err(message) => {
            eprintln!("{message}");
            return 1;
        }
    };
    // Stage the worktree's own index (it is scratch) so new files are in the diff.
    let exclude_state_dir = format!(":(exclude){}", rightkit_agent_config::state_dir_name());
    if let Err(error) = worktree_git(
        &worktree,
        &["add", "-A", "--", ".", exclude_state_dir.as_str()],
    ) {
        eprintln!("cannot read the worktree's changes: {error}");
        return 1;
    }
    let diff = {
        let mut command = rightkit_child_process::command_std("git");
        command
            .arg("-C")
            .arg(&worktree)
            .args(["diff", "--cached", "--binary", "--no-color", "HEAD"])
            .stdin(std::process::Stdio::null());
        match command.output() {
            Ok(output) if output.status.success() => output.stdout,
            Ok(output) => {
                eprintln!(
                    "cannot read the worktree's diff: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
                return 1;
            }
            Err(error) => {
                eprintln!("git is not available: {error}");
                return 1;
            }
        }
    };
    if diff.iter().all(u8::is_ascii_whitespace) {
        println!("No changes in worktree {} to apply.", worktree.display());
        return 0;
    }
    let files =
        worktree_git(&worktree, &["diff", "--cached", "--name-only", "HEAD"]).unwrap_or_default();
    for check_only in [true, false] {
        let mut command = rightkit_child_process::command_std("git");
        command.arg("-C").arg(&top).arg("apply").arg("--binary");
        if check_only {
            command.arg("--check");
        }
        command
            .arg("-")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                eprintln!("git is not available: {error}");
                return 1;
            }
        };
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            // A write error means git already refused the input; its exit status says why.
            let _ = stdin.write_all(&diff);
        }
        let output = match child.wait_with_output() {
            Ok(output) => output,
            Err(error) => {
                eprintln!("git apply failed: {error}");
                return 1;
            }
        };
        if !output.status.success() {
            eprintln!(
                "the checkout cannot take the worktree's changes; nothing was changed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            return 1;
        }
    }
    let count = files.lines().filter(|line| !line.trim().is_empty()).count();
    println!(
        "Applied {count} file(s) from worktree {} to {} (not committed):",
        worktree.display(),
        top.display()
    );
    for file in files.lines() {
        println!("  {file}");
    }
    0
}

// ── import claude-code ───────────────────────────────────────────────────────

/// One chat read from another tool's files.
#[derive(Debug, Clone, PartialEq)]
struct ImportedChat {
    source_id: String,
    title: String,
    cwd: String,
    started: Option<String>,
    /// `(role, text, timestamp)`, role `user` or `assistant`.
    messages: Vec<(&'static str, String, Option<String>)>,
}

/// Biggest transcript file the importer reads.
const IMPORT_MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// The text of one Claude Code message: a string, or the `text` blocks of a
/// content array (tool calls, tool results and thinking are not text).
fn claude_code_message_text(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(text) => text.trim().to_string(),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(serde_json::Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(serde_json::Value::as_str))
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Parse one Claude Code project transcript (JSONL: one JSON object per line, `type` `user`
/// or `assistant`, `message.role`, `message.content`, `cwd`, `sessionId`, `timestamp`).
/// Side-chain, meta and compaction-summary lines are skipped; consecutive turns of one role
/// are merged. `None` when the file holds no text turn.
fn parse_claude_code_transcript(file_stem: &str, text: &str) -> Option<ImportedChat> {
    let mut chat = ImportedChat {
        source_id: String::new(),
        title: String::new(),
        cwd: String::new(),
        started: None,
        messages: Vec::new(),
    };
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if chat.source_id.is_empty() {
            if let Some(id) = value.get("sessionId").and_then(serde_json::Value::as_str) {
                chat.source_id = id.to_string();
            }
        }
        if chat.cwd.is_empty() {
            if let Some(cwd) = value.get("cwd").and_then(serde_json::Value::as_str) {
                chat.cwd = cwd.to_string();
            }
        }
        let role = match value.get("type").and_then(serde_json::Value::as_str) {
            Some("user") => "user",
            Some("assistant") => "assistant",
            _ => continue,
        };
        let flag = |key: &str| value.get(key).and_then(serde_json::Value::as_bool) == Some(true);
        if flag("isSidechain") || flag("isMeta") || flag("isCompactSummary") {
            continue;
        }
        let Some(content) = value.get("message").and_then(|m| m.get("content")) else {
            continue;
        };
        let body = claude_code_message_text(content);
        if body.is_empty() {
            continue;
        }
        let timestamp = value
            .get("timestamp")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        if chat.started.is_none() {
            chat.started = timestamp.clone();
        }
        match chat.messages.last_mut() {
            Some((last_role, last_text, _)) if *last_role == role => {
                last_text.push_str("\n\n");
                last_text.push_str(&body);
            }
            _ => chat.messages.push((role, body, timestamp)),
        }
    }
    if chat.messages.is_empty() {
        return None;
    }
    if chat.source_id.is_empty() {
        chat.source_id = file_stem.to_string();
    }
    chat.title = chat
        .messages
        .iter()
        .find(|(role, _, _)| *role == "user")
        .map(|(_, text, _)| {
            let first = text.lines().next().unwrap_or("").trim();
            let mut title: String = first.chars().take(80).collect();
            if first.chars().count() > 80 {
                title.push_str("...");
            }
            title
        })
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| "Imported chat".to_string());
    Some(chat)
}

/// `~/.claude/projects`, where Claude Code keeps one folder per project.
fn claude_code_projects_dir() -> Option<std::path::PathBuf> {
    rightkit_agent_config::os_home_dir().map(|home| home.join(".claude").join("projects"))
}

/// The chats of one workspace found under `projects`: every `<project>/<session>.jsonl`
/// whose recorded `cwd` is the workspace or below it. Symlinks and files over
/// [`IMPORT_MAX_FILE_BYTES`] are skipped.
fn collect_claude_code_chats(
    projects: &std::path::Path,
    workspace: &std::path::Path,
) -> Vec<ImportedChat> {
    let mut chats = Vec::new();
    let Ok(project_dirs) = std::fs::read_dir(projects) else {
        return chats;
    };
    for project in project_dirs.flatten() {
        let Ok(files) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if !meta.is_file() || meta.len() > IMPORT_MAX_FILE_BYTES {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("chat")
                .to_string();
            let Some(chat) = parse_claude_code_transcript(&stem, &String::from_utf8_lossy(&bytes))
            else {
                continue;
            };
            if std::path::Path::new(&chat.cwd).starts_with(workspace) {
                chats.push(chat);
            }
        }
    }
    chats.sort_by(|a, b| (&a.started, &a.source_id).cmp(&(&b.started, &b.source_id)));
    chats
}

/// The session id one imported chat gets: stable, so a second import finds the first.
fn imported_session_id(source: ImportSource, source_id: &str) -> String {
    let safe: String = source_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .take(120)
        .collect();
    match source {
        ImportSource::ClaudeCode => format!("imported-claude-code-{safe}"),
    }
}

/// `{app} import claude-code [--dry-run] [--json] [--workspace PATH]`.
///
/// Without `--yes` (or with `--dry-run`) it lists what it would import and changes
/// nothing. With `--yes` it creates one dormant session per chat through
/// the daemon (`POST /sessions/import`): the text of each turn only, no model call.
/// The workspace must be trusted: reading another tool's chats is the trust gate's
/// business. Importing the same chat twice is a no-op (`already_imported`).
async fn cli_import(
    source: ImportSource,
    dry_run: bool,
    json: bool,
    workspace: Option<&str>,
    yes: bool,
) -> i32 {
    let workspace = workspace
        .map(str::to_string)
        .unwrap_or_else(default_cli_workspace);
    let workspace_path = std::path::Path::new(&workspace)
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(&workspace));
    let source_name = match source {
        ImportSource::ClaudeCode => "claude-code",
    };
    let perform = yes && !dry_run;
    if perform
        && workspace_trust_for(&workspace_path.to_string_lossy())
            != rightkit_agent_policy::WorkspaceTrust::Trusted
    {
        return cli_gate::refuse_exit(
            "import",
            RunExitCode::UntrustedWorkspace,
            "this workspace is not trusted: trust it from the TUI or the desktop app (`/trust`) before importing chats into it; `--dry-run` lists them without that",
        );
    }
    let Some(projects) = claude_code_projects_dir() else {
        eprintln!("cannot find the home directory, so there is no Claude Code data to read");
        return 1;
    };
    let chats = match source {
        ImportSource::ClaudeCode => collect_claude_code_chats(&projects, &workspace_path),
    };
    let listing: Vec<serde_json::Value> = chats
        .iter()
        .map(|chat| {
            serde_json::json!({
                "source_id": chat.source_id,
                "title": chat.title,
                "messages": chat.messages.len(),
                "cwd": chat.cwd,
                "started": chat.started,
            })
        })
        .collect();
    if !perform {
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "source": source_name,
                    "workspace": workspace_path,
                    "dry_run": true,
                    "chats": listing,
                })
            );
        } else {
            println!(
                "{} chat(s) from {source_name} would be imported for {}:",
                chats.len(),
                workspace_path.display()
            );
            for chat in &chats {
                println!(
                    "  {} ({} messages) {}",
                    chat.source_id,
                    chat.messages.len(),
                    chat.title
                );
            }
            if !chats.is_empty() {
                println!("Pass --yes to import them.");
            }
        }
        return 0;
    }
    let Some((base_url, token)) = crate::cli_presence::resolve_daemon_or_autostart().await else {
        return host_unreachable_exit(false);
    };
    let mut results = Vec::new();
    let mut failed = false;
    for chat in &chats {
        let session_id = imported_session_id(source, &chat.source_id);
        let messages: Vec<serde_json::Value> = chat
            .messages
            .iter()
            .map(|(role, text, timestamp)| {
                serde_json::json!({ "role": role, "text": text, "timestamp": timestamp })
            })
            .collect();
        let body = serde_json::json!({
            "id": session_id,
            "title": chat.title,
            "workspaceRoot": workspace_path,
            "source": source_name,
            "messages": messages,
        });
        let status = match rightkit_agent_daemon::http_client::daemon_call(
            rightkit_http::Method::Post,
            &format!("{base_url}/sessions/import"),
            &token,
            Some(&body),
        )
        .await
        {
            Ok(resp) if rightkit_agent_daemon::http_client::is_success(resp.status) => "imported",
            Ok(resp) if resp.status == 409 => "already_imported",
            Ok(resp) => {
                eprintln!(
                    "daemon returned {} importing chat {}",
                    resp.status, chat.source_id
                );
                failed = true;
                "failed"
            }
            Err(error) => {
                eprintln!("Failed to contact daemon: {error}");
                failed = true;
                "failed"
            }
        };
        results.push(serde_json::json!({
            "source_id": chat.source_id,
            "session_id": session_id,
            "status": status,
            "messages": chat.messages.len(),
        }));
    }
    if json {
        println!(
            "{}",
            serde_json::json!({
                "source": source_name,
                "workspace": workspace_path,
                "dry_run": false,
                "chats": results,
            })
        );
    } else {
        for result in &results {
            println!(
                "{} {} -> session {}",
                result["status"].as_str().unwrap_or(""),
                result["source_id"].as_str().unwrap_or(""),
                result["session_id"].as_str().unwrap_or("")
            );
        }
        if results.is_empty() {
            println!(
                "No {source_name} chats found for {}.",
                workspace_path.display()
            );
        }
    }
    if failed {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod cli_extras_tests {
    use super::*;

    #[test]
    fn worktree_names_are_words_and_prompts_are_refused() {
        assert_eq!(run_worktree_name("fix-1", 7).unwrap(), "fix-1");
        assert_eq!(run_worktree_name("", 7).unwrap(), "run-7");
        assert!(run_worktree_name("fix the bug", 7).is_err());
        assert!(run_worktree_name("../x", 7).is_err());
        assert!(run_worktree_name("-rf", 7).is_err());
        assert!(run_worktree_name(".hidden", 7).is_err());
    }

    #[test]
    fn review_scope_names_the_scope_and_refuses_an_option_looking_commit() {
        assert_eq!(
            review_scope(None, false, None).unwrap(),
            ReviewScope::Current
        );
        assert_eq!(
            review_scope(Some(" main "), false, None).unwrap(),
            ReviewScope::Base("main".to_string())
        );
        assert_eq!(
            review_scope(None, true, None).unwrap(),
            ReviewScope::Uncommitted
        );
        assert_eq!(
            review_scope(None, false, Some("abc1234")).unwrap(),
            ReviewScope::Commit("abc1234".to_string())
        );
        assert!(review_scope(None, false, Some("--output=x")).is_err());
        assert!(review_scope(None, false, Some("a b")).is_err());
        let goal = build_cli_review_goal(&[], &ReviewScope::Uncommitted, &[]);
        assert!(goal.contains("uncommitted") && goal.contains("working tree"));
        let goal = build_cli_review_goal(&[], &ReviewScope::Commit("abc1234".to_string()), &[]);
        assert!(goal.contains("git show abc1234"));
    }

    #[test]
    fn a_claude_code_transcript_keeps_text_turns_only() {
        let lines = [
            r#"{"type":"queue-operation","sessionId":"s1"}"#,
            r#"{"type":"user","sessionId":"s1","cwd":"/w","timestamp":"2026-11-01T10:00:00Z","message":{"role":"user","content":"first question"}}"#,
            r#"{"type":"assistant","sessionId":"s1","cwd":"/w","timestamp":"2026-11-01T10:00:02Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"part one"}]}}"#,
            r#"{"type":"assistant","sessionId":"s1","cwd":"/w","message":{"role":"assistant","content":[{"type":"tool_use","id":"t","name":"Bash","input":{}}]}}"#,
            r#"{"type":"assistant","sessionId":"s1","cwd":"/w","message":{"role":"assistant","content":[{"type":"text","text":"part two"}]}}"#,
            r#"{"type":"user","sessionId":"s1","cwd":"/w","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":"x"}]}}"#,
            r#"{"type":"user","sessionId":"s1","cwd":"/w","isMeta":true,"message":{"role":"user","content":"meta"}}"#,
            r#"{"type":"user","sessionId":"s1","cwd":"/w","isSidechain":true,"message":{"role":"user","content":"side"}}"#,
            "not json",
        ]
        .join("\n");
        let chat = parse_claude_code_transcript("file-stem", &lines).expect("a chat");
        assert_eq!(chat.source_id, "s1");
        assert_eq!(chat.cwd, "/w");
        assert_eq!(chat.title, "first question");
        assert_eq!(chat.messages.len(), 2);
        assert_eq!(chat.messages[0].0, "user");
        assert_eq!(chat.messages[1].0, "assistant");
        assert_eq!(chat.messages[1].1, "part one\n\npart two");
        assert!(parse_claude_code_transcript("x", r#"{"type":"mode"}"#).is_none());
    }

    #[test]
    fn a_plugin_ref_of_seven_to_forty_hex_digits_is_a_commit() {
        assert!(is_commit_sha_ref("abc1234"));
        assert!(is_commit_sha_ref(&"a".repeat(40)));
        assert!(!is_commit_sha_ref("abc123"));
        assert!(!is_commit_sha_ref(&"a".repeat(41)));
        assert!(!is_commit_sha_ref("v1.2.3"));
        assert!(!is_commit_sha_ref("release-1"));
    }

    #[test]
    fn imported_session_ids_are_stable_and_safe() {
        assert_eq!(
            imported_session_id(ImportSource::ClaudeCode, "ab12/../cd"),
            "imported-claude-code-ab12----cd"
        );
    }
}
