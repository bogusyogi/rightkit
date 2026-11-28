// R10 CR-26: terminal first-run onboarding. Declared here (inside `mod
// main_runtime`, via the include! in main_runtime.rs) so the module compiles
// and its shared picker is reachable as `crate::main_runtime::onboarding`.
#[path = "onboarding.rs"]
mod onboarding;

// R6/R7/R37/R38/R40: the `--yes` gate (dry-run receipts, bypass refusal).
#[path = "main_runtime_cli_gate.rs"]
mod cli_gate;

// R1 to R4: `{app} contract`, the machine-readable manifest and the per-command effect table
// (used only on a host that sets `host_options::cli_contract`).
#[path = "cli_contract.rs"]
mod cli_contract;

// The `/providers` screen's list, status projection and key/test actions,
// shared by the TUI host and `{app} provider ...`.
#[path = "provider_setup.rs"]
pub(crate) mod provider_setup;

#[cfg(unix)]
pub(crate) const TARGET_NOFILE_SOFT: u64 = 8_192;

/// Exit the process after the call's one journal line is written (headless
/// R19). Every exit of a CLI call goes through here.
fn cli_exit(code: i32) -> ! {
    // `std::process::exit` runs no destructor, so a browser this process launched would outlive it (issue 465).
    rightkit_agent_coding_tools::browser::close_owned_browsers_at_exit(
        std::time::Duration::from_secs(3),
    );
    crate::cli_journal::finish(code);
    crate::cli_log::flush();
    std::process::exit(code)
}

pub(crate) fn main() {
    let handle = std::thread::Builder::new()
        .name(crate::host_identity::prefixed("main"))
        .stack_size(16 * 1024 * 1024)
        .spawn(run_main)
        .unwrap_or_else(|error| {
            panic!(
                "failed to start {} main thread: {error}",
                crate::host_identity::app()
            )
        });
    if let Err(payload) = handle.join() {
        std::panic::resume_unwind(payload);
    }
}

/// Parse `args` through the host-localized command (product names in the help text), exiting
/// with the usage or help text like `Parser::parse_from`.
fn parse_cli<I, T>(args: I) -> Cli
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    use clap::FromArgMatches;
    let matches = crate::cli::try_cli_matches(args).unwrap_or_else(|error| error.exit());
    Cli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit())
}

/// Parse a bare launch (flags only) through the host-localized launch command, exiting with the
/// usage or help text like `Parser::parse_from`.
fn parse_launch_cli<I, T>(args: I) -> LaunchCli
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    use clap::FromArgMatches;
    let matches = crate::cli::localized_launch_command().get_matches_from(args);
    LaunchCli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit())
}

/// [`parse_cli`] returning the clap error instead of exiting.
fn try_parse_cli<I, T>(args: I) -> Result<Cli, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    use clap::FromArgMatches;
    let matches = crate::cli::try_cli_matches(args)?;
    Cli::from_arg_matches(&matches)
}

/// argv as text for routing decisions, the call journal and dry-run plans. `std::env::args`
/// panics on an argument that is not valid Unicode (a non-UTF-8 path on Linux or macOS, an
/// unpaired surrogate on Windows); this converts such an argument lossily (U+FFFD) instead. Clap
/// always parses the real `args_os`, so such an argument still ends as a usage error (exit 2).
fn lossy_args() -> Vec<String> {
    lossy_argv(cli_args_os())
}

fn lossy_argv(args: impl IntoIterator<Item = std::ffi::OsString>) -> Vec<String> {
    args.into_iter()
        .map(|arg| match arg.into_string() {
            Ok(text) => text,
            Err(raw) => raw.to_string_lossy().into_owned(),
        })
        .collect()
}

/// What kind of thing fd 0 is, for `-p`: only a pipe or a regular file can hold
/// piped data. A terminal, `/dev/null`, a closed handle or a socket never does.
#[cfg(unix)]
fn stdin_is_pipe_or_file() -> bool {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(0, &mut st) } != 0 {
        return false;
    }
    let kind = st.st_mode & libc::S_IFMT;
    kind == libc::S_IFIFO || kind == libc::S_IFREG
}

#[cfg(not(unix))]
fn stdin_is_pipe_or_file() -> bool {
    false
}

/// A pipe that is open but silent (an inherited handle nobody writes to) must not
/// stall `-p "text"`: wait briefly for data or end of input, then go on without it.
#[cfg(unix)]
fn stdin_has_data_or_eof() -> bool {
    let mut fd = libc::pollfd {
        fd: 0,
        events: libc::POLLIN,
        revents: 0,
    };
    unsafe { libc::poll(&mut fd, 1, 1500) > 0 }
}

#[cfg(not(unix))]
fn stdin_has_data_or_eof() -> bool {
    false
}

/// `-p -` reads the whole prompt from stdin; `-p "text"` with piped stdin appends
/// the piped data after the text. Terminals, `/dev/null`, closed handles and
/// silent pipes are never waited on for the text form.
fn print_prompt_with_stdin(prompt: String) -> Result<String, String> {
    use std::io::IsTerminal;
    let explicit = prompt.trim() == "-";
    let read = |what: &str| {
        crate::main_runtime::headless_input::read_stdin_bounded(
            std::io::stdin(),
            crate::main_runtime::headless_input::MAX_PIPED_STDIN_BYTES,
        )
        .map_err(|error| format!("{what}: {error}"))
    };
    if explicit {
        if std::io::stdin().is_terminal() {
            return Err("`-p -` reads the prompt from stdin, but stdin is a terminal; pipe the prompt in or give it as text".to_string());
        }
        return match read("`-p -`")? {
            Some(text) if !text.trim().is_empty() => Ok(text.trim().to_string()),
            _ => Err("`-p -` found no prompt on stdin".to_string()),
        };
    }
    if std::io::stdin().is_terminal() || !stdin_is_pipe_or_file() || !stdin_has_data_or_eof() {
        return Ok(prompt);
    }
    let stdin_text = read("piped stdin")?;
    crate::main_runtime::headless_input::normalize_one_shot_input(
        crate::main_runtime::headless_input::RawOneShotInput {
            positional_prompt: Some(prompt),
            stdin_text,
        },
    )
    .map(|input| input.prompt)
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod lossy_args_tests {
    #[test]
    fn non_unicode_argument_is_converted_not_panicking() {
        #[cfg(unix)]
        let bad = {
            use std::os::unix::ffi::OsStringExt;
            std::ffi::OsString::from_vec(b"a\xffb.png".to_vec())
        };
        #[cfg(windows)]
        let bad = {
            use std::os::windows::ffi::OsStringExt;
            std::ffi::OsString::from_wide(&[0x61, 0xD800, 0x62])
        };
        let args = super::lossy_argv(vec!["app".into(), "run".into(), "--image".into(), bad]);
        assert_eq!(args.len(), 4);
        assert_eq!(&args[..3], ["app", "run", "--image"]);
        assert!(args[3].contains('\u{FFFD}'), "{:?}", args[3]);
        assert!(args[3].starts_with('a'), "{:?}", args[3]);
    }
}

fn run_main() {
    // Process-wide naming and wire conventions (`host_identity::install` and the host's own
    // once-per-process names) are set by `rightkit_agent_server::run` before this point.
    // Providers R4, R46: probe the credential store, read the environment and
    // `.env` once, and remove the key names from this process's environment
    // before any thread that spawns a child exists.
    let host_credentials = rightkit_llm::agent_providers::host_credentials::host();
    // D4: one subscriber writes every library and InlineHost event to the log
    // file under the state root; the terminal is never written to. `serve`
    // installs its own (`rightkit_agent_telemetry::init`, stderr, which the desktop
    // shell and the autostart capture keep); a process has exactly one.
    let launch_args = lossy_args();
    if crate::cli_log::installs_file_log(&launch_args) {
        crate::cli_log::install();
    }
    tracing::info!("{}", host_credentials.state().log_line());
    let nofile = raise_nofile_soft_limit();
    if nofile.supported
        && (nofile.applied_soft != nofile.target_soft || nofile.current_soft.is_none())
    {
        eprintln!("[runtime] {}", nofile.detail);
    }
    if crate::host_options::embedder_runtime_setup() {
        rightkit_onnx::agent_ort::configure_ort_dylib();
        rightkit_onnx::agent_ort::configure_embed_model_dir();
    }
    let raw_args = lossy_args();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to initialize tokio runtime");
    if interactive_layout_invocation(&raw_args) {
        // Bare invocation — no subcommand. Match bare `claude`/`codex`/`hermes`:
        // launch minimal interactive session instead of a clap usage error
        // (par #222). Layout and launch flags (permission mode, `--model`,
        // `-p`) are the only accepted extension here; every other argument
        // shape remains owned by `Cli` below. `LaunchCli` owns validation
        // (mutually exclusive permission flags, flag values).
        // Parsed from the real `args_os`, so an argument that is not valid Unicode is a clap
        // usage error (exit 2), never a panic or a silently altered value.
        let mut launch = parse_launch_cli(cli_args_os());
        apply_config_flags_or_exit(
            &launch.config_override,
            launch.ignore_user_config,
            launch.strict_config,
            launch.ignore_rules,
            launch.workspace.as_deref(),
        );
        if launch.search {
            apply_search_flag(&mut launch.tools);
        }
        NO_ALT_SCREEN.store(launch.no_alt_screen, std::sync::atomic::Ordering::Relaxed);
        set_cycle_model_scope(&launch.models);
        if launch.oss || launch.local_provider.is_some() {
            if let Err((code, message)) = runtime.block_on(apply_local_provider(
                launch.oss,
                launch.local_provider.as_deref(),
                &mut launch.model,
            )) {
                eprintln!("error: {message}");
                cli_exit(code.as_i32());
            }
        }
        if (launch.yes
            || launch.json
            || launch.dry_run
            || launch.trust_workspace
            || launch.fork
            || launch.request_id.is_some())
            && launch.print.is_none()
        {
            eprintln!(
                "error: `--yes`, `--json`, `--dry-run`, `--trust-workspace`, `--request-id` and `--fork` apply to `-p` only; the interactive session asks for itself"
            );
            cli_exit(2);
        }
        // `--trust-workspace` (only with `-p`, checked here) must govern every
        // config layer `-p` reads before `run` starts: workspace presets below
        // and the route catalog in `run_print_launch` (workspace_trust.rs:131-133,
        // issue 474). Same key and same process-local grant as the
        // `Command::Run` arm (`args.workspace` or `default_cli_workspace()`);
        // persisted trust, which the run gate reads, is unchanged.
        if launch.trust_workspace {
            rightkit_agent_config::trust_workspace_for_this_process(
                &launch
                    .workspace
                    .clone()
                    .unwrap_or_else(default_cli_workspace),
            );
        }
        if let Some(name) = launch.preset.clone() {
            let workspace = launch
                .workspace
                .clone()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            let resolved = rightkit_agent_config::presets::load_presets(&workspace)
                .and_then(|all| rightkit_agent_config::presets::resolve_preset(&all, &name));
            match resolved {
                Ok(preset) => {
                    if let Some(message) = LaunchCli::unknown_preset_permission_mode(&preset) {
                        eprintln!("error: preset `{name}`: {message}");
                        cli_exit(2);
                    }
                    // Carried in run options (`--append-system-prompt`) for `-p`.
                    launch.preset_system_prompt = launch.apply_preset(&preset);
                    if let Some(prompt) = launch.preset_system_prompt.clone() {
                        let _ = LAUNCH_PRESET_SYSTEM_PROMPT.set(prompt);
                    }
                }
                Err(message) => {
                    eprintln!("error: {message}");
                    cli_exit(2);
                }
            }
        }
        if let Err(message) = install_launch_tool_filters(&launch) {
            eprintln!("error: {message}");
            cli_exit(2);
        }
        // #227: validate `--image` files up front; the interactive launch
        // attaches them as `@path` mentions on the first message.
        if !launch.image.is_empty() {
            if let Err(message) = headless_input::read_cli_image_attachments(&launch.image) {
                eprintln!("error: {message}");
                cli_exit(2);
            }
            if launch.print.is_none() {
                launch.prompt = Some(crate::tui_image_input::prompt_with_images(
                    launch.prompt.as_deref(),
                    &launch.image,
                ));
            }
        }
        if let Some(prompt) = launch.print.clone() {
            // `-p`: one headless turn through the existing `run` path. A pipe
            // feeds it (`-p -`, or data appended to the text).
            // `!{cmd}` in the typed text runs under the shell policy; nobody can approve here, so
            // a command that would need approval is refused as `unattended`. Piped data is
            // appended afterwards and is never scanned.
            let prompt = if rightkit_agent_skills::inline_shell::find_markers(&prompt).is_empty() {
                prompt
            } else {
                let workspace = launch
                    .workspace
                    .clone()
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
                crate::inline_shell_runner::expand_typed_prompt_unattended(
                    &prompt,
                    launch
                        .permission_mode()
                        .unwrap_or(rightkit_agent_policy::PermissionMode::Ask),
                    workspace_trust_for(&workspace.to_string_lossy()),
                    &workspace,
                )
            };
            let prompt = match print_prompt_with_stdin(prompt) {
                Ok(prompt) => prompt,
                Err(error) => {
                    eprintln!("error: {error}");
                    cli_exit(2);
                }
            };
            runtime.block_on(Box::pin(run_print_launch(launch, prompt)));
            cli_exit(1);
        }
        // R19: the interactive session is one call too. Its journal line
        // names the tool `interactive`; approvals happen in the app.
        crate::cli_journal::begin(
            "interactive",
            "spend",
            "in_app",
            false,
            "inline",
            None,
            None,
        );
        let code = runtime.block_on(interactive_session::run(launch));
        cli_exit(code);
    }
    let cli = parse_cli(cli_args_os());
    apply_config_flags_or_exit(
        &cli.config_override,
        cli.ignore_user_config,
        cli.strict_config,
        cli.ignore_rules,
        None,
    );
    runtime.block_on(Box::pin(async_main(cli)));
}

/// Read workspace trust from the canonical persisted trust store. The CLI and
/// TUI share the same JSON store used by the workspace-trust API.
pub(crate) fn workspace_trust_for(workspace_root: &str) -> rightkit_agent_policy::WorkspaceTrust {
    use rightkit_agent_policy::WorkspaceTrust;

    // Rule 60: the nearest recorded ancestor decides.
    let path = crate::api_stores::default_workspace_trust_store_path();
    let store = crate::api_stores::load_workspace_trust_store(&path);
    rightkit_agent_config::nearest_trust_record(workspace_root, |key| store.projects.get(key))
        .and_then(|record| crate::api::parse_workspace_trust_label(&record.trust))
        .unwrap_or(WorkspaceTrust::Untrusted)
}

/// `run --fork`: fork the named session and return the fork's id.
async fn fork_run_session(source: &str, workspace_root: &str) -> Result<String, String> {
    let host = SessionExecutionHost::select_for_call().await?;
    // The fork works in the folder the run is in: the source's own folder may be gone.
    let forked = host
        .session_action("fork_into", source, Some(workspace_root))
        .await?;
    forked
        .get("id")
        .or_else(|| forked.get("sessionId"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "the fork did not return a session id".to_string())
}

/// Persist an explicit user trust decision through the existing workspace
/// trust store, preserving other workspaces' records (read-modify-write under
/// the store's cross-process lock; a corrupt store is refused, not replaced).
pub(crate) fn trust_workspace(workspace_root: &str) -> Result<(), String> {
    let key = crate::api::workspace_trust_key(workspace_root);
    if key.is_empty() {
        return Err("workspace root cannot be empty".to_string());
    }
    let path = crate::api_stores::default_workspace_trust_store_path();
    crate::api_stores::upsert_workspace_trust_record(
        &path,
        key,
        crate::api_stores::WorkspaceTrustRecord {
            trust: "trusted".to_string(),
            approved_by: Some("user".to_string()),
            updated_at: rightkit_agent_engine_protocol::now_iso(),
        },
    )?;
    crate::main_runtime::sync_inline_workspace_trust(workspace_root);
    Ok(())
}

/// R38: `{app} trust` is manifest approval `in_app`. It never takes
/// `--yes` and is granted only by a person at a terminal (or the desktop or
/// TUI trust card), so an agent caller cannot widen trust and then bypass.
/// Returns the refusal reason, or `None` when a person may be asked.
pub(crate) fn trust_command_refusal(yes: bool, person_at_terminal: bool) -> Option<String> {
    let app = crate::host_identity::app();
    if yes {
        return Some(format!(
            "`{app} trust` never takes `--yes`: trust is granted by a person in the app or at a terminal"
        ));
    }
    if !person_at_terminal {
        return Some(format!(
            "`{app} trust` needs a person at a terminal: trust it from the TUI or the desktop app (`/trust`)"
        ));
    }
    None
}

pub(crate) fn trust_workspace_command(path: Option<String>, yes: bool) -> i32 {
    use std::io::IsTerminal;
    let at_terminal = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    if let Some(reason) = trust_command_refusal(yes, at_terminal) {
        return cli_gate::refuse_exit("trust", RunExitCode::InteractiveOnly, &reason);
    }
    let workspace_root = match path {
        Some(path) => path,
        None => match std::env::current_dir() {
            Ok(path) => path.display().to_string(),
            Err(error) => {
                eprintln!("cannot resolve current workspace: {error}");
                return 1;
            }
        },
    };
    eprint!("Trust {workspace_root} and every folder beneath it? [y/N] ");
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err()
        || !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    {
        eprintln!("not trusted");
        return 1;
    }
    match trust_workspace(&workspace_root) {
        Ok(()) => {
            println!("Trusted workspace {}", workspace_root);
            0
        }
        Err(error) => {
            eprintln!("cannot trust workspace {}: {error}", workspace_root);
            1
        }
    }
}

/// Tool selection from the interactive launch flags (`--tools`,
/// `--exclude-tools`, `--no-mcp`), applied to every turn of the session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct LaunchToolFilters {
    tools: Vec<String>,
    exclude_tools: Vec<String>,
    no_mcp: bool,
}

/// Preset system prompt for interactive turns (`-p` carries it as
/// `--append-system-prompt`); appended to every turn's system prompt.
static LAUNCH_PRESET_SYSTEM_PROMPT: std::sync::OnceLock<String> = std::sync::OnceLock::new();

static LAUNCH_TOOL_FILTERS: std::sync::OnceLock<LaunchToolFilters> = std::sync::OnceLock::new();

/// Validate launch tool flags up front (unknown-only lists are a usage error,
/// exit 2) and record them for interactive turns.
fn install_launch_tool_filters(launch: &LaunchCli) -> Result<(), String> {
    let filters = LaunchToolFilters {
        tools: launch.tools.clone(),
        exclude_tools: launch.exclude_tools.clone(),
        no_mcp: launch.no_mcp,
    };
    resolve_cli_tool_filters_with_mcp(
        None,
        filters.tools.clone(),
        filters.exclude_tools.clone(),
        filters.no_mcp,
    )?;
    let _ = LAUNCH_TOOL_FILTERS.set(filters);
    Ok(())
}

/// Build the argv `{app} run` receives for a top-level `-p` launch.
/// Pure so it is unit-testable; `provider_model` is the already-resolved
/// route (`--model` never reaches `run` unresolved).
fn print_launch_run_argv(
    launch: &LaunchCli,
    prompt: &str,
    provider_model: Option<(&str, &str)>,
) -> Vec<String> {
    let mut argv = vec![
        crate::host_identity::app().to_string(),
        "run".to_string(),
        "--print".to_string(),
    ];
    if let Some((provider, model)) = provider_model {
        argv.extend(["--model".to_string(), model.to_string()]);
        if !launch.provider.as_deref().unwrap_or(provider).is_empty() {
            argv.extend([
                "--provider".to_string(),
                launch.provider.as_deref().unwrap_or(provider).to_string(),
            ]);
        }
    } else if let Some(provider) = launch.provider.as_deref() {
        argv.extend(["--provider".to_string(), provider.to_string()]);
    }
    if let Some(effort) = launch.effort.as_deref() {
        argv.extend(["--effort".to_string(), effort.to_string()]);
    }
    if let Some(workspace) = launch.workspace.as_deref() {
        argv.extend(["--workspace".to_string(), workspace.to_string()]);
    }
    if let Some(session_id) = launch.resume.as_deref() {
        argv.extend(["--session-id".to_string(), session_id.to_string()]);
    }
    if !launch.tools.is_empty() {
        argv.extend(["--tools".to_string(), launch.tools.join(",")]);
    }
    if !launch.exclude_tools.is_empty() {
        argv.extend([
            "--exclude-tools".to_string(),
            launch.exclude_tools.join(","),
        ]);
    }
    if launch.no_mcp {
        argv.push("--no-mcp".to_string());
    }
    if launch.search {
        argv.push("--search".to_string());
    }
    if let Some(mode) = launch.permission_mode() {
        argv.extend([
            "--permission-mode".to_string(),
            cli_gate::permission_mode_name(mode).to_string(),
        ]);
    }
    if launch.yes {
        argv.push("--yes".to_string());
    }
    if launch.dry_run {
        argv.push("--dry-run".to_string());
    }
    if launch.trust_workspace {
        argv.push("--trust-workspace".to_string());
    }
    if launch.fork {
        argv.push("--fork".to_string());
    }
    if launch.json {
        argv.push("--json".to_string());
    }
    if launch.stream_json {
        argv.push("--stream-json".to_string());
    }
    if let Some(format) = launch.output_format.as_deref() {
        argv.extend(["--output-format".to_string(), format.to_string()]);
    }
    if let Some(request_id) = launch.request_id.as_deref() {
        argv.extend(["--request-id".to_string(), request_id.to_string()]);
    }
    if let Some(caller) = launch.caller.as_deref() {
        argv.extend(["--caller".to_string(), caller.to_string()]);
    }
    for image in &launch.image {
        argv.extend(["--image".to_string(), image.clone()]);
    }
    if let Some(turns) = launch.max_turns {
        argv.extend(["--max-turns".to_string(), turns.to_string()]);
    }
    if let Some(budget) = launch.max_budget_usd.as_deref() {
        argv.extend(["--max-budget-usd".to_string(), budget.to_string()]);
    }
    if launch.bare {
        argv.push("--bare".to_string());
    }
    if let Some(text) = launch.system_prompt.as_deref() {
        argv.extend(["--system-prompt".to_string(), text.to_string()]);
    }
    if let Some(path) = launch.system_prompt_file.as_deref() {
        argv.extend(["--system-prompt-file".to_string(), path.to_string()]);
    }
    if let Some(path) = launch.append_system_prompt_file.as_deref() {
        argv.extend(["--append-system-prompt-file".to_string(), path.to_string()]);
    }
    let appended = match (
        launch.preset_system_prompt.as_deref(),
        launch.append_system_prompt.as_deref(),
    ) {
        (Some(preset), Some(user)) => Some(format!("{preset}\n\n{user}")),
        (Some(text), None) | (None, Some(text)) => Some(text.to_string()),
        (None, None) => None,
    };
    if let Some(text) = appended {
        argv.extend(["--append-system-prompt".to_string(), text]);
    }
    for dir in &launch.add_dir {
        argv.extend(["--add-dir".to_string(), dir.clone()]);
    }
    if let Some(path) = launch.output_last_message.as_deref() {
        argv.extend(["--output-last-message".to_string(), path.to_string()]);
    }
    if let Some(path) = launch.mcp_config.as_deref() {
        argv.extend(["--mcp-config".to_string(), path.to_string()]);
    }
    // `--` so a prompt that starts with `-` stays the positional prompt.
    argv.push("--".to_string());
    argv.push(prompt.to_string());
    argv
}

/// `{app} -p "<prompt>"`: resolve `--model` against the route catalog,
/// then hand off to the existing headless `Command::Run` path (which exits
/// the process with the run's exit code). Returns only on a launch error.
async fn run_print_launch(launch: LaunchCli, prompt: String) {
    let mut launch = launch;
    let session = match interactive_session::resolve_launch_session(&launch).await {
        Ok(session) => session,
        Err(error) => {
            eprintln!("{error}");
            cli_exit(2);
        }
    };
    if let Some(session) = session.as_ref() {
        launch.resume = Some(session.id.clone());
        if launch.workspace.is_none() {
            launch.workspace = session.workspace.clone();
        }
        if launch.effort.is_none() {
            launch.effort = session.effort.clone();
        }
    }
    let (provider, model) =
        match interactive_session::resolve_launch_route_state(&launch, session.as_ref()).await {
            Ok(route) => route,
            Err(error) => {
                eprintln!("{error}");
                cli_exit(2);
            }
        };
    let route = (!model.is_empty()).then_some((provider, model));
    let argv = print_launch_run_argv(
        &launch,
        &prompt,
        route.as_ref().map(|(p, m)| (p.as_str(), m.as_str())),
    );
    match try_parse_cli(argv) {
        Ok(cli) => async_main(cli).await,
        Err(error) => {
            eprintln!("{error}");
            cli_exit(2);
        }
    }
}

/// Commands that no longer exist. Typed alone they must fail as unknown
/// commands, not start a session with the word as the prompt.
const DELETED_COMMANDS: &[&str] = &["cli-runtime", "__dogfood-inventory"];

/// [`DELETED_COMMANDS`] plus the command this host's options removed (`acp-server` when
/// `host_options::acp_server` is off; its alias `acp` goes with it and is then an ordinary word).
fn deleted_command(name: &str) -> bool {
    DELETED_COMMANDS.contains(&name) || (!crate::host_options::acp_server() && name == "acp-server")
}

fn interactive_layout_invocation(args: &[String]) -> bool {
    if args.is_empty() || args.len() == 1 {
        return true;
    }
    if crate::cli::localized_command()
        .find_subcommand(args[1].as_str())
        .is_some()
    {
        return false;
    }
    let mut index = 1;
    let mut positional_seen = false;
    while index < args.len() {
        match args[index].as_str() {
            "--plain"
            | "--reduced-motion"
            | "--ask"
            | "--accept-edits"
            | "--auto"
            | "--yolo"
            | "--yes"
            | "--dry-run"
            | "--trust-workspace"
            | "--fork"
            | "--json"
            | "--stream-json"
            | "--bare"
            | "--search"
            | "--no-alt-screen"
            | "--ignore-user-config"
            | "--strict-config"
            | "--ignore-rules" => index += 1,
            // Value flags: a missing value is still a launch invocation so
            // `LaunchCli` reports "a value is required" instead of the
            // generic subcommand error.
            "--model"
            | "--models"
            | "--provider"
            | "--effort"
            | "--workspace"
            | "--resume"
            | "--preset"
            | "--image"
            | "-p"
            | "--print"
            | "--permission-mode"
            | "--request-id"
            | "--caller"
            | "--output-format"
            | "--max-turns"
            | "--max-budget-usd"
            | "--system-prompt"
            | "--system-prompt-file"
            | "--append-system-prompt"
            | "--append-system-prompt-file"
            | "--add-dir"
            | "--output-last-message"
            | "--mcp-config"
            | "--profile"
            | "-c"
            | "--config" => index += 2,
            "--continue" | "--no-mcp" => index += 1,
            "--tools" | "--exclude-tools" => index += 2,
            flag if flag.starts_with("--tools=") || flag.starts_with("--exclude-tools=") => {
                index += 1
            }
            flag if flag.starts_with("--model=")
                || flag.starts_with("--models=")
                || flag.starts_with("--provider=")
                || flag.starts_with("--effort=")
                || flag.starts_with("--workspace=")
                || flag.starts_with("--resume=")
                || flag.starts_with("--preset=")
                || flag.starts_with("--profile=")
                || flag.starts_with("--config=")
                || flag.starts_with("--image=")
                || flag.starts_with("--print=")
                || flag.starts_with("--permission-mode=")
                || flag.starts_with("--request-id=")
                || flag.starts_with("--caller=")
                || flag.starts_with("--output-format=")
                || flag.starts_with("--max-turns=")
                || flag.starts_with("--max-budget-usd=")
                || flag.starts_with("--system-prompt=")
                || flag.starts_with("--system-prompt-file=")
                || flag.starts_with("--append-system-prompt=")
                || flag.starts_with("--append-system-prompt-file=")
                || flag.starts_with("--add-dir=")
                || flag.starts_with("--output-last-message=")
                || flag.starts_with("--mcp-config=") =>
            {
                index += 1
            }
            // Ambiguous service verbs are never a bare prompt; they fall
            // through to the subcommand parser, which rejects them.
            "install" | "uninstall" if !positional_seen => return false,
            // A deleted command name is an unknown command (exit 2), never a
            // prompt (headless R22, R36).
            name if !positional_seen && deleted_command(name) => return false,
            // After the root flags (`-c KEY=VALUE config`), the first word that names a command is the
            // command, not the start of a prompt.
            value
                if !value.starts_with('-')
                    && !positional_seen
                    && crate::cli::localized_command()
                        .find_subcommand(value)
                        .is_some() =>
            {
                return false
            }
            value if !value.starts_with('-') && !positional_seen => {
                positional_seen = true;
                index += 1;
            }
            _ => return false,
        }
    }
    true
}

/// The journal's view of a call (headless R19): the tool path from the clap
/// grammar, its effect row and the host route. `-p` re-enters here with a
/// synthetic `run`, so a command that cannot be re-derived from argv is `run`.
fn begin_call_journal(cli: &Cli, route: CliHostRoute) {
    // A host that sets `host_options::cli_contract` gives every command its row from the effect
    // table; without it only `run` has a journal row. `-p` re-enters here with a synthetic `run`.
    let (path, effect, approval) = if crate::host_options::cli_contract() {
        let path = cli_contract::invoked_path(&lossy_args())
            .map(|(path, _)| path)
            .or_else(|| matches!(cli.command, Command::Run { .. }).then(|| "run".to_string()));
        let Some(path) = path else {
            return;
        };
        let table = cli_contract::rows();
        let Some(row) = cli_contract::row_for(&table, &path) else {
            return;
        };
        let (effect, approval) = (row.effect, row.approval);
        (path, effect, approval)
    } else if matches!(cli.command, Command::Run { .. }) {
        ("run".to_string(), "spend", "none")
    } else {
        return;
    };
    let host_route = match route {
        CliHostRoute::LocalUtility => "local",
        CliHostRoute::InlineHost => "inline",
        CliHostRoute::ServiceHost => "service",
        CliHostRoute::ServiceProjection => "service_projection",
    };
    let request_id = match &cli.command {
        Command::Run { args, .. } => args.request_id.as_deref(),
        _ => None,
    };
    crate::cli_journal::begin(
        &path,
        effect,
        approval,
        cli.yes,
        host_route,
        cli.caller.as_deref(),
        request_id,
    );
}

/// A command this host does not offer: one of the optional verbs whose host option is off (the
/// CLI contract manifest `cli_contract`, `rpc_mode`, `guest_shares`, `remote_ssh`,
/// `lan_listener` for `discover`, the web client files), or a command that was removed. Prints
/// the refusal and returns the usage exit code.
fn host_only_command_unavailable(name: &str) -> i32 {
    eprintln!(
        "{}: `{name}` is not available in this build",
        crate::host_identity::app()
    );
    2
}

async fn async_main(mut cli: Cli) {
    if let Command::Run { args, .. } = &mut cli.command {
        args.normalize_permission_flags();
    }
    let cli_host_route = classify_cli_host_route(&cli.command);
    begin_call_journal(&cli, cli_host_route);
    if !cli.yes {
        // R37: on a host that sets `destructive_cli_verbs_need_yes`, the permanent removals are
        // dry runs without `--yes`.
        if crate::host_options::destructive_cli_verbs_need_yes() {
            if let Some((method, plan)) = cli_gate::destroy_plan(&cli.command) {
                cli_exit(cli_gate::dry_run_exit(method, "destroy", plan));
            }
        }
        // R6: on a host that ships the contract table (`host_options::cli_contract`), every
        // other tool with approval `yes` is a dry run too.
        if crate::host_options::cli_contract() {
            if let Some((method, effect, plan)) = cli_contract::dry_run_plan(&lossy_args()) {
                cli_exit(cli_gate::dry_run_exit(&method, effect, plan));
            }
        }
    }
    let yes = cli.yes;
    match cli.command {
        Command::Contract { json } => {
            if !crate::host_options::cli_contract() {
                cli_exit(host_only_command_unavailable("contract"));
            }
            cli_exit(cli_contract::print(json))
        }
        Command::Login {
            provider,
            with_api_key,
        } => {
            let code = cli_login(&provider, with_api_key, yes).await;
            cli_exit(code);
        }
        Command::Logout { provider } => {
            let code = cli_logout(&provider).await;
            cli_exit(code);
        }
        Command::Import {
            source,
            dry_run,
            json,
            workspace,
        } => {
            let code = cli_import(source, dry_run, json, workspace.as_deref(), yes).await;
            cli_exit(code);
        }
        Command::Apply { name, workspace } => {
            let code = cli_apply_worktree(name.as_deref(), workspace.as_deref());
            cli_exit(code);
        }
        Command::Onboard => {
            let code = run_onboard_command().await;
            cli_exit(code);
        }
        Command::Serve {
            port,
            db,
            model,
            provider,
            resident,
            lan,
            lan_port,
            no_mdns,
            ..
        } => {
            // The network listener is a host's own addition: it needs `lan_listener` and the
            // guest links it serves (`guest_shares`).
            if lan {
                if !crate::lan_listener::lan_enabled() {
                    eprintln!(
                        "{}: `serve --lan` is not available in this build: the daemon listens on loopback only",
                        crate::host_identity::app()
                    );
                    cli_exit(2);
                }
                if crate::lan_listener::OPTIONS
                    .set(crate::lan_listener::LanOptions {
                        port: lan_port,
                        advertise: !no_mdns,
                    })
                    .is_err()
                {
                    eprintln!("[serve] the LAN options were already set; the first ones stay");
                }
            }
            if let Err(existing) =
                rightkit_agent_coding_tools::process_resource_authority::install_process_resource_profile(
                    rightkit_agent_coding_tools::process_resource_authority::ResourceProfile::Expanded,
                )
            {
                eprintln!(
                    "ServiceHost requires the expanded resource profile, but this process is already bound to {existing:?}"
                );
                cli_exit(2);
            }
            // The daemon's one subscriber writes stderr; the same lines also go
            // to `<state root>/logs/serve.log`.
            crate::cli_log::tee_serve_stderr_to_log_file();
            serve(port, db, model, provider, resident).await
        }
        Command::Run { args, .. } => {
            // R6/R8/R38/R40: `run`, `exec` and `-p` are `spend` with approval
            // `none`: they do the work with no flag under `auto-safe`.
            // `--dry-run` prints the receipt; `--yes` changes nothing here
            // except that `bypass` needs it; `bypass` needs recorded trust.
            let mut args = *args;
            hosted_run_session::SCHEMA_RETRIES
                .store(args.schema_retries, std::sync::atomic::Ordering::Relaxed);
            // `--oss` / `--local-provider`: route this call to a local model server.
            if args.oss || args.local_provider.is_some() {
                if let Err((code, message)) =
                    apply_local_provider(args.oss, args.local_provider.as_deref(), &mut args.model)
                        .await
                {
                    cli_exit(run_error_exit_on_stderr(true, code, &message));
                }
            }
            // `--search`: `web_search` ships from the first request and stays in a `--tools` list.
            if args.search {
                apply_search_flag(&mut args.tools);
            }
            let mut gate_workspace = args.workspace.clone().unwrap_or_else(default_cli_workspace);
            let gate_trust = workspace_trust_for(&gate_workspace);
            // `--auto-approve` (and the global `--yes` on a host that reads it as that flag, see
            // `cli::effective_auto_approve`) is the accept-edits mode unless a mode is named.
            let named_mode = args
                .permission_mode
                .map(|mode| mode.to_policy())
                .or_else(|| {
                    crate::cli::effective_auto_approve(args.auto_approve, yes)
                        .then_some(rightkit_agent_policy::PermissionMode::AcceptEdits)
                });
            if let Some(reason) = cli_gate::bypass_refusal(named_mode, yes, gate_trust) {
                cli_exit(cli_gate::refuse_exit(
                    "run",
                    RunExitCode::RefusedByPolicy,
                    &reason,
                ));
            }
            let requested_mode = Some(cli_gate::effective_run_mode(named_mode));
            if args.dry_run {
                cli_exit(cli_gate::dry_run_exit(
                    "run",
                    "spend",
                    cli_gate::run_plan(&args, &gate_workspace, gate_trust),
                ));
            }
            // R8: an untrusted workspace refuses the run before any provider
            // call; `--trust-workspace` trusts it for this run only.
            if let Some(reason) =
                cli_gate::untrusted_workspace_refusal(gate_trust, args.trust_workspace)
            {
                cli_exit(cli_gate::refuse_exit(
                    "run",
                    RunExitCode::UntrustedWorkspace,
                    &reason,
                ));
            }
            // `--worktree`: the run's workspace is a new git worktree of this repository,
            // made only now that the workspace is known to be trusted (and never for a dry run).
            if let Some(given) = args.worktree.clone() {
                let made = run_worktree_name(
                    &given,
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|elapsed| elapsed.as_secs())
                        .unwrap_or(0),
                )
                .and_then(|name| prepare_run_worktree(&gate_workspace, &name));
                match made {
                    Ok(path) => {
                        gate_workspace = path.to_string_lossy().into_owned();
                        args.workspace = Some(gate_workspace.clone());
                    }
                    Err(message) => {
                        cli_exit(run_error_exit_on_stderr(true, RunExitCode::Usage, &message))
                    }
                }
            }
            if args.trust_workspace {
                let _ = RUN_TRUSTED_WORKSPACE.set(gate_workspace.clone());
                rightkit_agent_config::trust_workspace_for_this_process(&gate_workspace);
                // Content-free journal line (R19): the flag, never the path.
                tracing::info!(
                    tool = "run",
                    trust_workspace_for_this_run = true,
                    "cli_call"
                );
            }
            // `--json` is `--output-format json`; naming the stream format too is
            // a usage error, never a silent override.
            if args.json && args.output_format == OutputFormat::StreamJson {
                cli_exit(run_error_exit_on_stderr(
                    true,
                    RunExitCode::Usage,
                    "--json prints one result value and cannot be combined with --output-format stream-json; pick one",
                ));
            }
            let mut run_session_id = args.session_id.clone();
            // R14: a request id names the session, and its message nonce makes
            // the retry replay the first admission instead of calling the
            // provider again.
            if let Some(request_id) = args.request_id.as_deref() {
                run_session_id = Some(cli_gate::request_session_id(request_id));
                let _ = RUN_REQUEST_ID.set(request_id.to_string());
            }
            if args.continue_last {
                let launch = LaunchCli {
                    continue_last: true,
                    workspace: args.workspace.clone(),
                    provider: args.provider.clone(),
                    ..LaunchCli::default()
                };
                match interactive_session::resolve_launch_session(&launch).await {
                    Ok(Some(session)) => run_session_id = Some(session.id),
                    Ok(None) => {}
                    Err(error) => {
                        cli_exit(run_error_exit_on_stderr(true, RunExitCode::Usage, &error))
                    }
                }
            }
            if args.fork {
                let Some(source) = run_session_id.clone() else {
                    cli_exit(run_error_exit_on_stderr(
                        true,
                        RunExitCode::Usage,
                        "--fork needs --session-id or --continue",
                    ));
                };
                match fork_run_session(&source, &gate_workspace).await {
                    Ok(forked) => {
                        hosted_run_session::RUN_FORKED_SESSION
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                        run_session_id = Some(forked)
                    }
                    Err(error) => {
                        cli_exit(run_error_exit_on_stderr(true, RunExitCode::Generic, &error))
                    }
                }
            }
            let actual_prompt = if matches!(args.input_format, InputFormat::StreamJson) {
                String::new()
            } else {
                let stdin_text = if args.pipe {
                    crate::main_runtime::headless_input::read_stdin_bounded(
                        std::io::stdin(),
                        crate::main_runtime::headless_input::MAX_PIPED_STDIN_BYTES,
                    )
                } else {
                    Ok(None)
                };
                match stdin_text.and_then(|stdin_text| {
                    crate::main_runtime::headless_input::normalize_one_shot_input(
                        crate::main_runtime::headless_input::RawOneShotInput {
                            positional_prompt: args.prompt.clone(),
                            stdin_text,
                        },
                    )
                }) {
                    Ok(input) => input.prompt,
                    Err(error) => {
                        let code = run_error_exit(
                            if args.stream_json
                                || args.json_event
                                || matches!(args.output_format, OutputFormat::StreamJson)
                                || matches!(args.input_format, InputFormat::StreamJson)
                            {
                                RunOutputMode::StreamJson
                            } else if args.output_schema.is_some() {
                                RunOutputMode::SchemaJson
                            } else if matches!(args.output_format, OutputFormat::Json) {
                                RunOutputMode::Json
                            } else if args.print {
                                RunOutputMode::Print
                            } else {
                                RunOutputMode::Text
                            },
                            RunExitCode::Usage,
                            &error.to_string(),
                        );
                        cli_exit(code);
                    }
                }
            };
            let presence = if service_host_explicitly_selected() {
                crate::cli_presence::attach_cli_presence("run").await
            } else {
                None
            };
            // R15: Ctrl-C cancels the run's token; the run interrupts its turn,
            // waits for the session to settle and exits 1 `interrupted`.
            let control = RunCancellation::new();
            let on_interrupt = control.clone();
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    on_interrupt.requested.cancel();
                }
            });
            if args.verbose {
                hosted_run_session::RUN_VERBOSE.store(true, std::sync::atomic::Ordering::Relaxed);
                eprintln!(
                    "[verbose] request: provider={} model={}",
                    args.provider.as_deref().unwrap_or("default"),
                    args.model.as_deref().unwrap_or("default")
                );
            }
            let code = run_session_controlled(
                RunArgs {
                    prompt: actual_prompt,
                    print: args.print,
                    output_format: if args.json {
                        OutputFormat::Json
                    } else {
                        args.output_format
                    },
                    json_event: args.json_event,
                    model: args.model.clone(),
                    provider: args.provider.clone(),
                    effort: args.effort.clone(),
                    workspace: args.workspace.clone(),
                    permission_mode: requested_mode,
                    stream_json: args.stream_json,
                    exit_on_error: args.exit_on_error,
                    max_turns: args.max_turns,
                    allowed_tools: args
                        .allowed_tools
                        .iter()
                        .chain(args.tools.iter())
                        .cloned()
                        .collect(),
                    disallowed_tools: args
                        .disallowed_tools
                        .iter()
                        .chain(args.exclude_tools.iter())
                        .cloned()
                        .collect(),
                    toolset: args.toolset.clone(),
                    own_toolset: args.own_toolset.clone(),
                    trust_workspace: args.trust_workspace,
                    max_budget_usd: args.max_budget_usd,
                    bare: args.bare,
                    safe_mode: args.safe_mode,
                    system_prompt: args.system_prompt.clone(),
                    system_prompt_file: args.system_prompt_file.clone(),
                    append_system_prompt: args.append_system_prompt.clone(),
                    append_system_prompt_file: args.append_system_prompt_file.clone(),
                    mcp_config: args.mcp_config.clone(),
                    session_id: run_session_id,
                    add_dir: args.add_dir.clone(),
                    input_format: args.input_format,
                    output_schema: args.output_schema.clone(),
                    image: args.image.clone(),
                    ephemeral: args.ephemeral || args.no_session,
                    output_last_message: args.output_last_message.clone(),
                    color: args.color,
                    tools: args.tools.clone(),
                    exclude_tools: args.exclude_tools.clone(),
                    no_mcp: args.no_mcp,
                    unattended: true,
                    approval_callback: args.approval_callback,
                },
                control,
            )
            .await;
            if let Some(presence) = presence {
                presence.detach().await;
            }
            cli_exit(code);
        }
        Command::Doctor {
            json,
            verbose,
            clipboard,
            network_only,
        } => {
            if !json {
                // Providers R4: where keys come from on this host, and why not
                // the OS store when none exists.
                let host = rightkit_llm::agent_providers::host_credentials::host();
                println!("{}", host.state().log_line());
                if let Some(error) = host.dotenv_error() {
                    println!("credential file: {error}");
                }
            }
            cli_exit(doctor::run(json, verbose, clipboard, network_only))
        }
        Command::Config { schema: true } => cli_exit(print_config_schema()),
        Command::Execpolicy {
            command:
                ExecpolicyCommand::Check {
                    mode,
                    trust,
                    json,
                    command,
                },
        } => cli_exit(execpolicy_check(mode, &trust, json, &command)),
        Command::Features {
            command: FeaturesCommand::List { json },
        } => cli_exit(features_list(json)),
        Command::Debug {
            command:
                DebugCommand::PromptInput {
                    json,
                    trust_workspace,
                    workspace,
                },
        } => cli_exit(debug_prompt_input(
            json,
            trust_workspace,
            workspace.as_deref(),
        )),
        Command::Config { schema: false } => match redacted_merged_config_view() {
            Ok(cfg) => println!("{}", serde_json::to_string_pretty(&cfg).unwrap_or_default()),
            Err(error) => {
                eprintln!("Failed to render redacted config: {error}");
                cli_exit(1);
            }
        },
        Command::Trust { path } => cli_exit(trust_workspace_command(path, yes)),
        Command::Status { json: _json } => {
            let code = daemon_status().await;
            cli_exit(code);
        }
        Command::Stop { force } => {
            let code = daemon_stop(force).await;
            cli_exit(code);
        }
        Command::Migrate { command } => {
            let code = cli_migrate(command).await;
            cli_exit(code);
        }
        Command::Info { json } => {
            let code = cli_info(json).await;
            cli_exit(code);
        }
        Command::Completion { shell } => {
            cli_exit(print_completion(shell));
        }
        Command::Remote { command } => {
            let code = cli_remote(command).await;
            cli_exit(code);
        }
        Command::Recipe { command } => {
            let presence = if matches!(&command, RecipeCommand::Run { .. })
                && service_host_explicitly_selected()
            {
                crate::cli_presence::attach_cli_presence("recipe-run").await
            } else {
                None
            };
            let code = cli_recipe(command, yes).await;
            if let Some(presence) = presence {
                presence.detach().await;
            }
            cli_exit(code);
        }
        Command::Schedule { command } => {
            let code = cli_schedule(command).await;
            cli_exit(code);
        }
        Command::Browser { command } => {
            let code = cli_browser(command).await;
            cli_exit(code);
        }
        Command::Bot { command } => {
            let code = cli_bot(command).await;
            cli_exit(code);
        }
        Command::Spec { command } => {
            let code = cli_spec(command).await;
            cli_exit(code);
        }
        Command::Skill { command } => {
            let code = cli_skill(command).await;
            cli_exit(code);
        }
        Command::Learning { command } => {
            let code = cli_learning(command).await;
            cli_exit(code);
        }
        Command::Insights {
            session_ids,
            scope,
            min_recurrence,
        } => {
            let code = cli_insights(session_ids, scope, min_recurrence).await;
            cli_exit(code);
        }
        Command::Diagnostics { command } => {
            let code = cli_diagnostics(command).await;
            cli_exit(code);
        }
        Command::Caps { json } => {
            let code = cli_caps(json);
            cli_exit(code);
        }
        Command::Ps { json } | Command::Sessions { json } => {
            let code = daemon_ps(json).await;
            cli_exit(code);
        }
        Command::Agents {
            session_id,
            json,
            all,
            cwd,
        } => {
            let code = daemon_agents(session_id.as_deref(), json, all, cwd).await;
            cli_exit(code);
        }
        Command::Resume {
            session_id,
            last,
            all,
            all_workspaces,
        } => {
            let code = daemon_resume(session_id.as_deref(), last, all, all_workspaces).await;
            cli_exit(code);
        }
        Command::Rpc {
            run_args,
            approval_timeout_secs,
        } => {
            if !crate::host_options::rpc_mode() {
                cli_exit(host_only_command_unavailable("rpc"));
            }
            if approval_timeout_secs == Some(0) {
                eprintln!("error: --approval-timeout-secs must be at least 1");
                cli_exit(2);
            }
            let code = crate::rpc_mode::run_rpc(
                run_args,
                std::time::Duration::from_secs(approval_timeout_secs.unwrap_or(300)),
            )
            .await;
            cli_exit(code);
        }
        Command::Share { command } => {
            if !crate::host_options::guest_shares() {
                cli_exit(host_only_command_unavailable("share"));
            }
            cli_exit(cli_share(command).await)
        }
        Command::Ssh {
            destination,
            port,
            ssh_port,
            ssh_option,
            remote_bin,
            open,
        } => {
            if !crate::host_options::remote_ssh() {
                cli_exit(host_only_command_unavailable("ssh"));
            }
            cli_exit(
                crate::remote_ssh::run(crate::remote_ssh::RemoteArgs {
                    destination,
                    local_port: port,
                    ssh_port,
                    ssh_options: ssh_option,
                    remote_bin,
                    open,
                })
                .await,
            )
        }
        Command::Discover { timeout, json } => {
            if !crate::host_options::lan_listener() {
                cli_exit(host_only_command_unavailable("discover"));
            }
            let wait = std::time::Duration::from_secs(timeout.clamp(1, 60));
            match crate::lan_listener::discover(wait) {
                Ok(found) if json => {
                    println!(
                        "{}",
                        serde_json::json!({"schema_version": crate::host_identity::schema("cli.discover.v1"), "daemons": found})
                    );
                    cli_exit(0);
                }
                Ok(found) if found.is_empty() => {
                    println!(
                        "No {} daemon advertised on this network (start one with `{}`).",
                        rightkit_agent_config::app_name(),
                        crate::host_identity::command("serve --lan")
                    );
                    cli_exit(0);
                }
                Ok(found) => {
                    for row in found {
                        println!(
                            "{}\t{}\t{}",
                            row.name,
                            row.url,
                            row.version.unwrap_or_default()
                        );
                    }
                    cli_exit(0);
                }
                Err(error) => {
                    eprintln!("discover: {error}");
                    cli_exit(1);
                }
            }
        }
        Command::Web { open } => {
            if !crate::api_web::web_client_enabled() {
                cli_exit(host_only_command_unavailable("web"));
            }
            let Some((base_url, token)) = crate::cli_presence::resolve_daemon_or_autostart().await
            else {
                cli_exit(host_unreachable_exit(false));
            };
            let url = crate::api_web::web_url(&base_url, &token);
            println!("{url}");
            if open {
                let opened = crate::open_url::open_url(&url);
                if !opened.is_ok_and(|status| status.success()) {
                    eprintln!("could not open a browser; open the URL above yourself");
                }
            }
            cli_exit(0);
        }
        Command::Fork {
            session_id,
            last,
            all,
            all_workspaces,
            title,
            before_seq,
            json: _json,
        } => {
            let code = daemon_fork(
                session_id.as_deref(),
                last,
                all,
                all_workspaces,
                title.as_deref(),
                before_seq,
            )
            .await;
            cli_exit(code);
        }
        Command::Session { command } => {
            let code = daemon_session(command).await;
            cli_exit(code);
        }
        Command::Logs { session_id } => {
            let code = daemon_logs(&session_id).await;
            cli_exit(code);
        }
        Command::Attach { session_id } => {
            let code = daemon_attach(&session_id).await;
            cli_exit(code);
        }
        Command::Conduct {
            goal,
            dry_run,
            safe_mode,
            workspace,
        } => {
            let code =
                daemon_conduct(&goal, dry_run, safe_mode, workspace.as_deref(), None, None).await;
            cli_exit(code);
        }
        Command::Queue {
            session_id,
            message,
            resume,
        } => {
            let code = daemon_queue(&session_id, &message, resume).await;
            cli_exit(code);
        }
        Command::Kill { session_id } => {
            let code = daemon_kill(&session_id).await;
            cli_exit(code);
        }
        Command::Handoff { session_id } => {
            let code = daemon_handoff(&session_id).await;
            cli_exit(code);
        }
        Command::App { command } => {
            let code = cli_app(command).await;
            cli_exit(code);
        }
        Command::Export { session_id, json } => {
            let code = daemon_export(&session_id, json).await;
            cli_exit(code);
        }
        Command::Diff { workspace, staged } => {
            let code = cli_diff(workspace.as_deref(), staged).await;
            cli_exit(code);
        }
        Command::Review {
            focus,
            session,
            json,
            base,
            uncommitted,
            commit,
            dirs,
            model,
            provider,
            workspace,
            safe_mode,
            auto_approve,
            permission_mode,
            stream_json,
            max_turns,
            max_budget_usd,
        } => {
            let permission_mode = permission_mode.map(|mode| mode.to_policy());
            let mut rejected = review_rejected_options(
                provider.as_deref(),
                stream_json,
                max_turns,
                max_budget_usd,
            );
            if is_review_findings_request(&focus) {
                // Findings is a read-only report of a completed review; no
                // run option can apply to it.
                if model.is_some() {
                    rejected.push("--model");
                }
                if permission_mode.is_some() {
                    rejected.push("--permission-mode");
                }
                if auto_approve {
                    rejected.push("--auto-approve");
                }

                if !rejected.is_empty() {
                    eprintln!(
                        "`{app} review findings` does not accept: {}",
                        rejected.join(", "),
                        app = crate::host_identity::app()
                    );
                    cli_exit(2);
                }
                let code = match review_findings_session_arg(&focus, session.as_deref()) {
                    Ok(session_id) => daemon_review_findings(&session_id, json).await,
                    Err(message) => {
                        eprintln!("{message}");
                        2
                    }
                };
                cli_exit(code);
            }
            if permission_mode == Some(rightkit_agent_policy::PermissionMode::Bypass) {
                eprintln!("`--permission-mode bypass` is available on `run` only");
                cli_exit(2);
            }
            if !rejected.is_empty() {
                eprintln!(
                    "`{app} review` does not accept: {} (the auditor dispatch owns provider, turn, budget, and output policy)",
                    rejected.join(", "),
                    app = crate::host_identity::app()
                );
                cli_exit(2);
            }
            // `--auto-approve` (and the global `--yes` on a host that reads it as that flag) is the
            // accept-edits mode unless a mode is named.
            let permission_mode = permission_mode.or_else(|| {
                crate::cli::effective_auto_approve(auto_approve, yes)
                    .then_some(rightkit_agent_policy::PermissionMode::AcceptEdits)
            });
            let scope = match review_scope(base.as_deref(), uncommitted, commit.as_deref()) {
                Ok(scope) => scope,
                Err(message) => {
                    eprintln!("{message}");
                    cli_exit(2);
                }
            };
            let code = daemon_conduct(
                &build_cli_review_goal(&focus, &scope, &dirs),
                false,
                safe_mode,
                workspace.as_deref(),
                model.as_deref(),
                permission_mode,
            )
            .await;
            cli_exit(code);
        }
        Command::Mcp { command } => {
            let code = cli_mcp(command).await;
            cli_exit(code);
        }
        Command::Plugin { command } => {
            let code = cli_plugin(command, yes).await;
            cli_exit(code);
        }
        Command::Permission { command } => {
            let code = cli_permission(command).await;
            cli_exit(code);
        }
        Command::Help { all } => {
            print!("{}", render_registry_help(all));
            cli_exit(0);
        }
        Command::Provider { command } => {
            let code = cli_provider(command).await;
            cli_exit(code);
        }
        Command::LocalModel { command } => {
            let code = cli_local_model(command).await;
            cli_exit(code);
        }
        Command::Models { provider, json } => {
            let code = interactive_session::cli_models(provider, json).await;
            cli_exit(code);
        }
        Command::Hooks { command } => {
            let code = cli_hooks(command).await;
            cli_exit(code);
        }
        Command::Toolset { command } => {
            let code = cli_toolset(command).await;
            cli_exit(code);
        }
        Command::Execution { command } => {
            let code = cli_execution(command).await;
            cli_exit(code);
        }
        Command::AcpServer { port, token } => {
            if !crate::host_options::acp_server() {
                cli_exit(host_only_command_unavailable("acp-server"));
            }
            let presence = crate::cli_presence::attach_cli_presence("acp-server").await;
            let code =
                crate::acp::run_stdio(crate::acp::config::AcpServerOptions { port, token }).await;
            if let Some(presence) = presence {
                presence.detach().await;
            }
            cli_exit(code);
        }
        Command::McpServer {
            workspace,
            allow_mutate,
            session_id,
        } => {
            let workspace = workspace.map(std::path::PathBuf::from);
            let presence = if cli_host_route == CliHostRoute::ServiceProjection {
                crate::cli_presence::attach_cli_presence("mcp-server").await
            } else {
                None
            };
            let code = mcp_server::run_stdio(mcp_server::McpServerOptions {
                workspace,
                allow_mutate,
                session_id,
            })
            .await;
            if let Some(presence) = presence {
                presence.detach().await;
            }
            cli_exit(code);
        }
    }
}

// R10 CR-26: real entrypoint for terminal first-run onboarding (`{app}
// onboard`). Detects first-run / missing-configuration state from the merged
// config plus the real credential store, runs the interactive flow, and maps
// the outcome to a process exit code. Cancelling exits 0 with prior state
// intact; a failed readiness probe exits 1 having written nothing.
async fn run_onboard_command() -> i32 {
    use crate::main_runtime::onboarding::{
        detect_from_config_with_native_auth, load_current_config, run_onboarding, ConfigPersister,
        OnboardOutcome, PromptIo, RealReadinessRunner, TerminalIo,
    };
    let mut io = TerminalIo;
    let config = load_current_config();
    let backend = rightkit_llm::agent_providers::OsKeyStore;
    let state = detect_from_config_with_native_auth(&backend, &config).await;
    if state.needs_onboarding() {
        io.write(&format!(
            "{} needs provider configuration before it can run a turn.\n",
            rightkit_agent_config::app_name()
        ));
    } else {
        io.write(&format!(
            "{} already has a working configuration — onboarding will confirm before changing anything.\n",
            rightkit_agent_config::app_name()
        ));
    }
    let outcome = run_onboarding(
        &state,
        &backend,
        &RealReadinessRunner,
        &ConfigPersister,
        &mut io,
    )
    .await;
    match outcome {
        OnboardOutcome::Completed { .. } => 0,
        OnboardOutcome::Cancelled => {
            io.write("Onboarding cancelled — nothing was changed.\n");
            0
        }
        OnboardOutcome::Failed { .. } => 1,
    }
}

// The runtime `/model` and `/providers` surfaces consume this list. It is the
// catalog-backed subset of the `/providers` screen (OpenAI, OpenCode Go,
// Command Code, OpenRouter); minimax/opencode/custom are hidden here and in
// the screen without deleting their code or stored keys. Vendor CLIs
// (Claude Code, Antigravity) are layered on by `provider_setup`.
pub fn runtime_route_picker() -> Vec<crate::main_runtime::onboarding::RouteChoice> {
    crate::main_runtime::provider_setup::screen_route_choices()
}

fn cli_caps(json: bool) -> i32 {
    let capabilities = rightkit_agent_coding_tools::default_tool_capabilities();
    if json {
        let body = serde_json::json!({
            "schema_version": crate::host_identity::schema("cli.caps.v1"),
            "toolCount": capabilities.len(),
            "readOnlyCount": capabilities.iter().filter(|cap| cap.read_only).count(),
            "mutatingCount": capabilities.iter().filter(|cap| !cap.read_only).count(),
            "capabilities": capabilities,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&body).unwrap_or_default()
        );
    } else {
        println!(
            "{}",
            rightkit_agent_coding_tools::render_tool_capabilities_markdown(&capabilities)
        );
    }
    0
}

fn print_completion(shell: CompletionShell) -> i32 {
    let output = generate_completion(shell);
    match std::io::stdout().write_all(&output) {
        Ok(()) => 0,
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => 0,
        Err(error) => {
            eprintln!("failed to write completion script: {error}");
            1
        }
    }
}

fn generate_completion(shell: CompletionShell) -> Vec<u8> {
    let mut cmd = crate::cli::localized_command();
    let name = cmd.get_name().to_string();
    let mut output = Vec::new();
    match shell {
        CompletionShell::Bash => {
            clap_complete::generate(clap_complete::shells::Bash, &mut cmd, name, &mut output)
        }
        CompletionShell::Zsh => {
            clap_complete::generate(clap_complete::shells::Zsh, &mut cmd, name, &mut output)
        }
        CompletionShell::Fish => {
            clap_complete::generate(clap_complete::shells::Fish, &mut cmd, name, &mut output)
        }
        CompletionShell::Powershell => clap_complete::generate(
            clap_complete::shells::PowerShell,
            &mut cmd,
            name,
            &mut output,
        ),
        CompletionShell::Elvish => {
            clap_complete::generate(clap_complete::shells::Elvish, &mut cmd, name, &mut output)
        }
    }
    output
}

/// `lifetime` as seconds: `90`, `90s`, `30m`, `1h`, `2d`.
fn parse_share_lifetime(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let (number, unit) = match text.char_indices().last() {
        Some((index, c)) if c.is_ascii_alphabetic() => (&text[..index], c),
        _ => (text, 's'),
    };
    let value: u64 = number
        .parse()
        .map_err(|_| format!("--expires {text:?}: expected a number with s, m, h or d"))?;
    let per_unit: u64 = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        'd' => 86_400,
        _ => return Err(format!("--expires {text:?}: unit must be s, m, h or d")),
    };
    // A product too large for u64 is out of range, not a wrapped small number.
    let secs = value
        .checked_mul(per_unit)
        .ok_or_else(|| "--expires must be between 1m and 7d".to_string())?;
    if !(crate::guest_shares::MIN_TTL_SECS..=crate::guest_shares::MAX_TTL_SECS).contains(&secs) {
        return Err("--expires must be between 1m and 7d".to_string());
    }
    Ok(secs)
}

async fn cli_share(command: crate::cli::ShareCommand) -> i32 {
    use crate::cli::ShareCommand;
    match command {
        ShareCommand::List => daemon_get_pretty_json("/shares", "share list").await,
        ShareCommand::Revoke { share_id } => {
            daemon_delete_pretty_json(
                &format!("/shares/{}", url_encode_component(&share_id)),
                "share revoke",
            )
            .await
        }
        ShareCommand::Create {
            session_id,
            expires,
            approve,
            json,
        } => {
            let secs = match parse_share_lifetime(&expires) {
                Ok(secs) => secs,
                Err(message) => {
                    eprintln!("error: {message}");
                    return 2;
                }
            };
            let Some((base_url, token)) = crate::cli_presence::resolve_daemon_or_autostart().await
            else {
                return host_unreachable_exit(false);
            };
            let body = serde_json::json!({"expiresInSecs": secs, "canApprove": approve});
            let response = match rightkit_agent_daemon::http_client::daemon_call(
                rightkit_http::Method::Post,
                &format!(
                    "{base_url}/sessions/{}/shares",
                    url_encode_component(&session_id)
                ),
                &token,
                Some(&body),
            )
            .await
            {
                Ok(response) => response,
                Err(error) => {
                    eprintln!("Failed to contact daemon: {error}");
                    return 1;
                }
            };
            if !rightkit_agent_daemon::http_client::is_success(response.status) {
                let status = response.status;
                eprintln!(
                    "share create: daemon returned {status}: {}",
                    response.text()
                );
                return 1;
            }
            let mut share = response.json::<serde_json::Value>().unwrap_or_default();
            let local = format!(
                "{}{}",
                base_url.trim_end_matches('/'),
                share["path"].as_str().unwrap_or_default()
            );
            share["localUrl"] = serde_json::json!(local);
            if json {
                println!("{share}");
                return 0;
            }
            println!(
                "Guest link for session {session_id} (expires {}):",
                share["expiresAt"].as_str().unwrap_or("?")
            );
            println!("  on this machine: {local}");
            match share["lanUrl"].as_str() {
                Some(lan) => println!("  on your network: {lan}"),
                None => println!(
                    "  (start the daemon with `{}` to reach it from other machines, or forward the port over SSH)",
                    crate::host_identity::command("serve --lan")
                ),
            }
            println!(
                "The guest can follow this session{}. Revoke it with: {}",
                if approve {
                    " and answer its approvals"
                } else {
                    " (read only)"
                },
                crate::host_identity::command(&format!(
                    "share revoke {}",
                    share["shareId"].as_str().unwrap_or("?")
                ))
            );
            0
        }
    }
}

#[cfg(test)]
mod share_lifetime_tests {
    use super::parse_share_lifetime;

    #[test]
    fn a_lifetime_is_a_number_with_a_unit_between_one_minute_and_seven_days() {
        assert_eq!(parse_share_lifetime("90"), Ok(90));
        assert_eq!(parse_share_lifetime("90s"), Ok(90));
        assert_eq!(parse_share_lifetime("30m"), Ok(1800));
        assert_eq!(parse_share_lifetime("1h"), Ok(3600));
        assert_eq!(parse_share_lifetime("7d"), Ok(7 * 86_400));
        assert!(parse_share_lifetime("59s").is_err());
        assert!(parse_share_lifetime("8d").is_err());
        assert!(parse_share_lifetime("1w").is_err());
        assert!(parse_share_lifetime("h").is_err());
        assert!(parse_share_lifetime("").is_err());
        // A product that overflows is refused, never wrapped into a small lifetime.
        assert!(parse_share_lifetime("999999999999999999999d").is_err());
        assert!(parse_share_lifetime("18446744073709551615d").is_err());
    }
}
