// R10 CR-26: terminal first-run onboarding. Declared here (inside `mod
// main_runtime`, via the include! in main_runtime.rs) so the module compiles
// and its shared picker is reachable as `crate::main_runtime::onboarding`.
#[path = "onboarding.rs"]
mod onboarding;

// The `/providers` screen's list, status projection and key/test actions,
// shared by the TUI host and `coderight provider ...`.
#[path = "provider_setup.rs"]
pub(crate) mod provider_setup;

pub(crate) const TARGET_NOFILE_SOFT: u64 = 8_192;

pub(crate) fn main() {
    let handle = std::thread::Builder::new()
        .name("coderight-main".to_string())
        .stack_size(16 * 1024 * 1024)
        .spawn(run_main)
        .expect("failed to start coderight main thread");
    if let Err(payload) = handle.join() {
        std::panic::resume_unwind(payload);
    }
}

fn run_main() {
    let nofile = raise_nofile_soft_limit();
    if nofile.supported
        && (nofile.applied_soft != nofile.target_soft || nofile.current_soft.is_none())
    {
        eprintln!("[runtime] {}", nofile.detail);
    }
    crate::cli::warn_deprecated_context_mode_flag();
    rightkit_onnx::agent_ort::configure_ort_dylib();
    rightkit_onnx::agent_ort::configure_embed_model_dir();
    let raw_args = std::env::args().collect::<Vec<_>>();
    if let Some(command) = disabled_cli_acquisition_command(&raw_args) {
        eprintln!(
            "`coderight {command}` is disabled by the cli-acquisition-v1 feature gate. Use the older command path or enable the gate."
        );
        std::process::exit(2);
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to initialize tokio runtime");
    // R10 CR-26: `coderight onboard` — terminal first-run onboarding. This is
    // the real call site for the onboarding flow (first-run detection, shared
    // route/model picker, credential storage, real readiness probe).
    // Intercepted before `Cli::parse` because the clap definitions live in an
    // out-of-allowlist file; the in-session `/onboard` alias is owned there.
    if raw_args.len() == 2 && raw_args[1] == "onboard" {
        let code = runtime.block_on(run_onboard_command());
        std::process::exit(code);
    }
    if raw_args.get(1).map(String::as_str) == Some("login")
        && raw_args.get(2).map(String::as_str) == Some("github")
        && raw_args.len() == 3
    {
        let result = runtime.block_on(tokio::task::spawn_blocking(
            crate::github_signin::device_sign_in_with_terminal,
        ));
        let mut output = std::io::stdout().lock();
        let mut errors = std::io::stderr().lock();
        let code = match result {
            Ok(Ok(())) => {
                let _ = writeln!(output, "GitHub sign-in saved.");
                0
            }
            Ok(Err(error)) => {
                let _ = writeln!(errors, "GitHub sign-in failed: {error}");
                1
            }
            Err(error) => {
                let _ = writeln!(errors, "GitHub sign-in failed: {error}");
                1
            }
        };
        std::process::exit(code);
    }
    if raw_args.get(1).map(String::as_str) == Some("logout")
        && raw_args.get(2).map(String::as_str) == Some("github")
        && raw_args.len() == 3
    {
        let result = crate::github_signin::sign_out();
        let mut output = std::io::stdout().lock();
        let mut errors = std::io::stderr().lock();
        match result {
            Ok(()) => {
                let _ = writeln!(output, "GitHub sign-in removed.");
                std::process::exit(0);
            }
            Err(error) => {
                let _ = writeln!(errors, "GitHub sign-out failed: {error}");
                std::process::exit(1);
            }
        }
    }
    // Hidden dogfood inventory dump. Intercepted here, before `Cli::parse`, so
    // no `Command` variant is added and the grammar this emits is exactly what
    // ships (see `dogfood_inventory`). Not a product command; harness-only.
    if raw_args.get(1).map(String::as_str) == Some("__dogfood-inventory") {
        println!("{}", crate::dogfood_inventory::inventory_json());
        std::process::exit(0);
    }
    if interactive_layout_invocation(&raw_args) {
        // Bare invocation — no subcommand. Match bare `claude`/`codex`/`hermes`:
        // launch minimal interactive session instead of a clap usage error
        // (par #222). Layout and launch flags (permission mode, `--model`,
        // `-p`) are the only accepted extension here; every other argument
        // shape remains owned by `Cli` below. `LaunchCli` owns validation
        // (mutually exclusive permission flags, flag values).
        let mut launch = LaunchCli::parse_from(&raw_args);
        if let Some(name) = launch.preset.clone() {
            let workspace = launch
                .workspace
                .clone()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            let resolved = rightkit_app_settings::agent_config::presets::load_presets(&workspace)
                .and_then(|all| {
                    rightkit_app_settings::agent_config::presets::resolve_preset(&all, &name)
                });
            match resolved {
                Ok(preset) => {
                    // Carried in run options (`--append-system-prompt`) for `-p`.
                    launch.preset_system_prompt = launch.apply_preset(&preset);
                    if let Some(prompt) = launch.preset_system_prompt.clone() {
                        let _ = LAUNCH_PRESET_SYSTEM_PROMPT.set(prompt);
                    }
                }
                Err(message) => {
                    eprintln!("error: {message}");
                    std::process::exit(2);
                }
            }
        }
        if let Err(message) = install_launch_tool_filters(&launch) {
            eprintln!("error: {message}");
            std::process::exit(2);
        }
        // #227: validate `--image` files up front; the interactive launch
        // attaches them as `@path` mentions on the first message.
        if !launch.image.is_empty() {
            if let Err(message) = headless_input::read_cli_image_attachments(&launch.image) {
                eprintln!("error: {message}");
                std::process::exit(2);
            }
            if launch.print.is_none() {
                launch.prompt = Some(crate::tui_image_input::prompt_with_images(
                    launch.prompt.as_deref(),
                    &launch.image,
                ));
            }
        }
        if let Some(prompt) = launch.print.clone() {
            // `-p`: one headless turn through the existing `run` path.
            runtime.block_on(Box::pin(run_print_launch(launch, prompt)));
            std::process::exit(1);
        }
        let code = runtime.block_on(interactive_session::run(launch));
        std::process::exit(code);
    }
    let cli = Cli::parse();
    runtime.block_on(Box::pin(async_main(cli)));
}

/// Read workspace trust from the canonical persisted trust store. The CLI and
/// TUI share the same JSON store used by the workspace-trust API.
pub(crate) fn workspace_trust_for(workspace_root: &str) -> rightkit_agent_policy::WorkspaceTrust {
    use rightkit_agent_policy::WorkspaceTrust;

    let key = crate::api::workspace_trust_key(workspace_root);
    if key.is_empty() {
        return WorkspaceTrust::Untrusted;
    }
    let path = crate::api_stores::default_workspace_trust_store_path();
    crate::api_stores::load_workspace_trust_store(&path)
        .projects
        .get(&key)
        .and_then(|record| crate::api::parse_workspace_trust_label(&record.trust))
        .unwrap_or(WorkspaceTrust::Untrusted)
}

/// Persist an explicit user trust decision through the existing workspace
/// trust store, preserving other workspaces' records.
pub(crate) fn trust_workspace(workspace_root: &str) -> Result<(), String> {
    static TRUST_STORE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = TRUST_STORE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let key = crate::api::workspace_trust_key(workspace_root);
    if key.is_empty() {
        return Err("workspace root cannot be empty".to_string());
    }
    let path = crate::api_stores::default_workspace_trust_store_path();
    let mut store = crate::api_stores::load_workspace_trust_store(&path);
    store.projects.insert(
        key,
        crate::api_stores::WorkspaceTrustRecord {
            trust: "trusted".to_string(),
            approved_by: Some("user".to_string()),
            updated_at: rightkit_agent_engine_protocol::now_iso(),
        },
    );
    crate::api_stores::write_workspace_trust_store(&path, &store)?;
    crate::main_runtime::sync_inline_workspace_trust(workspace_root);
    Ok(())
}

pub(crate) fn trust_workspace_command(path: Option<String>) -> i32 {
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

/// Permission mode chosen by a top-level launch flag for a headless `-p`
/// run. `Command::Run` reads it into `RunArgs.permission_mode`; the explicit
/// `coderight run` flags (`--auto-approve`, `--bypass-approvals`) are untouched.
static LAUNCH_PERMISSION_MODE: std::sync::OnceLock<rightkit_agent_policy::PermissionMode> =
    std::sync::OnceLock::new();

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

/// Build the argv `coderight run` receives for a top-level `-p` launch.
/// Pure so it is unit-testable; `provider_model` is the already-resolved
/// route (`--model` never reaches `run` unresolved).
fn print_launch_run_argv(
    launch: &LaunchCli,
    prompt: &str,
    provider_model: Option<(&str, &str)>,
) -> Vec<String> {
    let mut argv = vec![
        "coderight".to_string(),
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
    for image in &launch.image {
        argv.extend(["--image".to_string(), image.clone()]);
    }
    if let Some(prompt) = launch.preset_system_prompt.as_deref() {
        argv.extend(["--append-system-prompt".to_string(), prompt.to_string()]);
    }
    // `--` so a prompt that starts with `-` stays the positional prompt.
    argv.push("--".to_string());
    argv.push(prompt.to_string());
    argv
}

/// `coderight -p "<prompt>"`: resolve `--model` against the route catalog,
/// then hand off to the existing headless `Command::Run` path (which exits
/// the process with the run's exit code). Returns only on a launch error.
async fn run_print_launch(launch: LaunchCli, prompt: String) {
    let mut launch = launch;
    let session = match interactive_session::resolve_launch_session(&launch).await {
        Ok(session) => session,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
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
                std::process::exit(2);
            }
        };
    let route = (!model.is_empty()).then_some((provider, model));
    if let Some(mode) = launch.permission_mode() {
        let _ = LAUNCH_PERMISSION_MODE.set(mode);
    }
    let argv = print_launch_run_argv(
        &launch,
        &prompt,
        route.as_ref().map(|(p, m)| (p.as_str(), m.as_str())),
    );
    match Cli::try_parse_from(argv) {
        Ok(cli) => async_main(cli).await,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    }
}

fn interactive_layout_invocation(args: &[String]) -> bool {
    if args.is_empty() || args.len() == 1 {
        return true;
    }
    if Cli::command().find_subcommand(args[1].as_str()).is_some() {
        return false;
    }
    let mut index = 1;
    let mut positional_seen = false;
    while index < args.len() {
        match args[index].as_str() {
            "--plain" | "--reduced-motion" | "--ask" | "--accept-edits" | "--auto" | "--yolo" => {
                index += 1
            }
            // Value flags: a missing value is still a launch invocation so
            // `LaunchCli` reports "a value is required" instead of the
            // generic subcommand error.
            "--model" | "--provider" | "--effort" | "--workspace" | "--resume" | "--preset"
            | "-p" | "--print" => index += 2,
            "--continue" | "--no-mcp" => index += 1,
            "--tools" | "--exclude-tools" => index += 2,
            flag if flag.starts_with("--tools=") || flag.starts_with("--exclude-tools=") => {
                index += 1
            }
            flag if flag.starts_with("--model=")
                || flag.starts_with("--provider=")
                || flag.starts_with("--effort=")
                || flag.starts_with("--workspace=")
                || flag.starts_with("--resume=")
                || flag.starts_with("--preset=")
                || flag.starts_with("--print=") =>
            {
                index += 1
            }
            "--context-mode"
                if matches!(
                    args.get(index + 1).map(String::as_str),
                    Some("direct" | "membrane")
                ) =>
            {
                index += 2;
            }
            // Ambiguous service verbs are never a bare prompt; they fall
            // through to the subcommand parser, which rejects them.
            "install" | "uninstall" if !positional_seen => return false,
            value if !value.starts_with('-') && !positional_seen => {
                positional_seen = true;
                index += 1;
            }
            _ => return false,
        }
    }
    true
}

fn disabled_cli_acquisition_command(args: &[String]) -> Option<&str> {
    let command = cli_acquisition_wrapper_command(args)?;
    if cli_acquisition_v1_enabled() {
        None
    } else {
        Some(command)
    }
}

fn cli_acquisition_wrapper_command(args: &[String]) -> Option<&str> {
    let command = args.get(1)?.as_str();
    match command {
        "exec" | "info" | "completion" | "agents" | "resume" | "fork" | "app" | "mcp"
        | "plugin" | "schedule" | "schedules" | "permission" | "permissions" | "provider"
        | "local-model" | "local-models" | "hooks" | "hook" | "toolset" | "toolsets"
        | "execution" | "learning" => Some(command),
        _ => None,
    }
}

fn cli_acquisition_v1_enabled() -> bool {
    cli_acquisition_v1_enabled_from_config(&load_current_merged_config())
}

fn cli_acquisition_v1_enabled_from_config(
    config: &rightkit_app_settings::agent_config::HostConfig,
) -> bool {
    config.features.cli_acquisition_v1.unwrap_or(true)
}

async fn async_main(cli: Cli) {
    let cli_host_route = classify_cli_host_route(&cli.command);
    match cli.command {
        Command::Onboard => {
            let code = run_onboard_command().await;
            std::process::exit(code);
        }
        Command::Serve {
            context_mode,
            port,
            db,
            model,
            provider,
            resident,
        } => {
            if let Err(existing) =
                rightkit_agent_tools::coding_tools::process_resource_authority::install_process_resource_profile(
                    rightkit_agent_tools::coding_tools::process_resource_authority::ResourceProfile::Expanded,
                )
            {
                eprintln!(
                    "ServiceHost requires the expanded resource profile, but this process is already bound to {existing:?}"
                );
                std::process::exit(2);
            }
            let _ = context_mode;
            serve(
                port,
                db,
                model,
                provider,
                ContextMode::effective(),
                resident,
            )
            .await
        }
        Command::Run { args, .. } => {
            let actual_prompt = if matches!(args.input_format, InputFormat::StreamJson) {
                String::new()
            } else {
                let raw_input = crate::main_runtime::headless_input::RawOneShotInput {
                    positional_prompt: args.prompt.clone(),
                    stdin_text: if args.pipe {
                        crate::main_runtime::headless_input::read_stdin_to_string(std::io::stdin())
                    } else {
                        None
                    },
                };
                match crate::main_runtime::headless_input::normalize_one_shot_input(raw_input) {
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
                        std::process::exit(code);
                    }
                }
            };
            let presence = if service_host_explicitly_selected() {
                crate::cli_presence::attach_cli_presence("run").await
            } else {
                None
            };
            let code = run_session(RunArgs {
                context_mode: ContextMode::effective(),
                prompt: actual_prompt,
                print: args.print,
                output_format: args.output_format,
                json_event: args.json_event,
                model: args.model.clone(),
                provider: args.provider.clone(),
                effort: args.effort.clone(),
                workspace: args.workspace.clone(),
                auto_approve: args.auto_approve,
                bypass_approvals: args.bypass_approvals,
                permission_mode: LAUNCH_PERMISSION_MODE.get().copied(),
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
                max_budget_usd: args.max_budget_usd,
                bare: args.bare,
                safe_mode: args.safe_mode,
                system_prompt: args.system_prompt.clone(),
                system_prompt_file: args.system_prompt_file.clone(),
                append_system_prompt: args.append_system_prompt.clone(),
                append_system_prompt_file: args.append_system_prompt_file.clone(),
                mcp_config: args.mcp_config.clone(),
                session_id: args.session_id.clone(),
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
            })
            .await;
            if let Some(presence) = presence {
                presence.detach().await;
            }
            std::process::exit(code);
        }
        Command::Doctor {
            json,
            verbose,
            clipboard,
        } => std::process::exit(doctor::run(json, verbose, clipboard)),
        Command::Config => match redacted_merged_config_view() {
            Ok(cfg) => println!("{}", serde_json::to_string_pretty(&cfg).unwrap_or_default()),
            Err(error) => {
                eprintln!("Failed to render redacted config: {error}");
                std::process::exit(1);
            }
        },
        Command::Trust { path } => std::process::exit(trust_workspace_command(path)),
        Command::Status { json: _json } => {
            let code = daemon_status().await;
            std::process::exit(code);
        }
        Command::Stop { force } => {
            let code = daemon_stop(force).await;
            std::process::exit(code);
        }
        Command::Migrate { command } => {
            let code = cli_migrate(command).await;
            std::process::exit(code);
        }
        Command::Info { json } => {
            let code = cli_info(json).await;
            std::process::exit(code);
        }
        Command::Completion { shell } => {
            std::process::exit(print_completion(shell));
        }
        Command::Remote { command } => {
            let code = cli_remote(command).await;
            std::process::exit(code);
        }
        Command::Recipe { command } => {
            let presence = if matches!(&command, RecipeCommand::Run { .. })
                && service_host_explicitly_selected()
            {
                crate::cli_presence::attach_cli_presence("recipe-run").await
            } else {
                None
            };
            let code = cli_recipe(command).await;
            if let Some(presence) = presence {
                presence.detach().await;
            }
            std::process::exit(code);
        }
        Command::Schedule { command } => {
            let code = cli_schedule(command).await;
            std::process::exit(code);
        }
        Command::Browser { command } => {
            let code = cli_browser(command).await;
            std::process::exit(code);
        }
        Command::Bot { command } => {
            let code = cli_bot(command).await;
            std::process::exit(code);
        }
        Command::Spec { command } => {
            let code = cli_spec(command).await;
            std::process::exit(code);
        }
        Command::Skill { command } => {
            let code = cli_skill(command).await;
            std::process::exit(code);
        }
        Command::Learning { command } => {
            let code = cli_learning(command).await;
            std::process::exit(code);
        }
        Command::Insights {
            session_ids,
            scope,
            min_recurrence,
        } => {
            let code = cli_insights(session_ids, scope, min_recurrence).await;
            std::process::exit(code);
        }
        Command::Diagnostics { command } => {
            let code = cli_diagnostics(command).await;
            std::process::exit(code);
        }
        Command::Caps { json } => {
            let code = cli_caps(json);
            std::process::exit(code);
        }
        Command::Ps { json: _json } | Command::Sessions { json: _json } => {
            let code = daemon_ps().await;
            std::process::exit(code);
        }
        Command::Agents {
            session_id,
            json,
            all,
            cwd,
        } => {
            let code = daemon_agents(session_id.as_deref(), json, all, cwd).await;
            std::process::exit(code);
        }
        Command::Resume {
            session_id,
            last,
            all,
            all_workspaces,
        } => {
            let code = daemon_resume(session_id.as_deref(), last, all, all_workspaces).await;
            std::process::exit(code);
        }
        Command::Fork {
            session_id,
            last,
            all,
            all_workspaces,
            title,
            json: _json,
        } => {
            let code = daemon_fork(
                session_id.as_deref(),
                last,
                all,
                all_workspaces,
                title.as_deref(),
            )
            .await;
            std::process::exit(code);
        }
        Command::Session { command } => {
            let code = daemon_session(command).await;
            std::process::exit(code);
        }
        Command::Logs { session_id } => {
            let code = daemon_logs(&session_id).await;
            std::process::exit(code);
        }
        Command::Attach { session_id } => {
            let code = daemon_attach(&session_id).await;
            std::process::exit(code);
        }
        Command::Conduct {
            goal,
            dry_run,
            safe_mode,
            workspace,
        } => {
            let code =
                daemon_conduct(&goal, dry_run, safe_mode, workspace.as_deref(), None, false).await;
            std::process::exit(code);
        }
        Command::Kill { session_id } => {
            let code = daemon_kill(&session_id).await;
            std::process::exit(code);
        }
        Command::Handoff { session_id } => {
            let code = daemon_handoff(&session_id).await;
            std::process::exit(code);
        }
        Command::App { command } => {
            let code = cli_app(command).await;
            std::process::exit(code);
        }
        Command::Export { session_id, json } => {
            let code = daemon_export(&session_id, json).await;
            std::process::exit(code);
        }
        Command::Diff { workspace, staged } => {
            let code = cli_diff(workspace.as_deref(), staged).await;
            std::process::exit(code);
        }
        Command::Review {
            focus,
            session,
            json,
            base,
            dirs,
            model,
            provider,
            workspace,
            safe_mode,
            auto_approve,
            stream_json,
            max_turns,
            max_budget_usd,
        } => {
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
                if auto_approve {
                    rejected.push("--auto-approve");
                }
                if !rejected.is_empty() {
                    eprintln!(
                        "`coderight review findings` does not accept: {}",
                        rejected.join(", ")
                    );
                    std::process::exit(2);
                }
                let code = match review_findings_session_arg(&focus, session.as_deref()) {
                    Ok(session_id) => daemon_review_findings(&session_id, json).await,
                    Err(message) => {
                        eprintln!("{message}");
                        2
                    }
                };
                std::process::exit(code);
            }
            if !rejected.is_empty() {
                eprintln!(
                    "`coderight review` does not accept: {} (the auditor dispatch owns provider, turn, budget, and output policy)",
                    rejected.join(", ")
                );
                std::process::exit(2);
            }
            let code = daemon_conduct(
                &build_cli_review_goal(&focus, base.as_deref(), &dirs),
                false,
                safe_mode,
                workspace.as_deref(),
                model.as_deref(),
                auto_approve,
            )
            .await;
            std::process::exit(code);
        }
        Command::Mcp { command } => {
            let code = cli_mcp(command).await;
            std::process::exit(code);
        }
        Command::Plugin { command } => {
            let code = cli_plugin(command).await;
            std::process::exit(code);
        }
        Command::Permission { command } => {
            let code = cli_permission(command).await;
            std::process::exit(code);
        }
        Command::Help { all } => {
            print!("{}", render_registry_help(all));
            std::process::exit(0);
        }
        Command::Provider { command } => {
            let code = cli_provider(command).await;
            std::process::exit(code);
        }
        Command::LocalModel { command } => {
            let code = cli_local_model(command).await;
            std::process::exit(code);
        }
        Command::Models { provider } => {
            let code = interactive_session::cli_models(provider).await;
            std::process::exit(code);
        }
        Command::Hooks { command } => {
            let code = cli_hooks(command).await;
            std::process::exit(code);
        }
        Command::Toolset { command } => {
            let code = cli_toolset(command).await;
            std::process::exit(code);
        }
        Command::Execution { command } => {
            let code = cli_execution(command).await;
            std::process::exit(code);
        }
        Command::CliRuntime { command } => {
            let code = cli_runtime(command).await;
            std::process::exit(code);
        }
        Command::McpServer {
            workspace,
            allow_mutate,
            session_id,
        } => {
            let workspace = workspace.map(std::path::PathBuf::from).unwrap_or_else(|| {
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
            });
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
            std::process::exit(code);
        }
        Command::AcpServer { port, token } => {
            let presence = crate::cli_presence::attach_cli_presence("acp-server").await;
            let code =
                crate::acp::run_stdio(crate::acp::config::AcpServerOptions { port, token }).await;
            if let Some(presence) = presence {
                presence.detach().await;
            }
            std::process::exit(code);
        }
    }
}

// R10 CR-26: real entrypoint for terminal first-run onboarding (`coderight
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
        io.write("RightKit needs provider configuration before it can run a turn.\n");
    } else {
        io.write(
            "RightKit already has a working configuration — onboarding will confirm before changing anything.\n",
        );
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
    let capabilities = rightkit_agent_tools::coding_tools::default_tool_capabilities();
    if json {
        let body = serde_json::json!({
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
            rightkit_agent_tools::coding_tools::render_tool_capabilities_markdown(&capabilities)
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
    let mut cmd = Cli::command();
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
