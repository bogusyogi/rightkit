// Bare-invocation interactive session (par #222).
//
// `{app}` with no subcommand used to be a clap usage error. This module
// gives it the same shape as bare `claude`/`codex`/`hermes`: submit a prompt
// to the engine via the same `run_session` path `{app} run` uses (so
// provider/workspace/API-key resolution is identical), stream the response,
// loop. `/quit`, Ctrl-D on an empty line, or EOF exits cleanly.
//
// R6/CR-20: when stdin/stdout are a real terminal, `run()` now hands off to
// `crate::tui_host::drive` — the real event-loop/composer/scheduler host
// built for this lane, implementing `TurnHost` below via `EngineTurnHost`.
// The original line-oriented `run_loop`/`classify_line` stay in place as the
// fallback for a non-tty stdin (a pipe, CI), where raw mode is not an option
// at all, and remain covered by the tests at the bottom of this file.
//
// Included (not `mod`-declared with `use`) from `main_runtime.rs` so it
// shares that file's flat scope — `RunArgs`, `run_session`, and
// `InputFormat` below refer to the ones defined in the sibling
// `main_runtime_run_*.rs` files via the same `include!` mechanism the rest
// of this binary uses.
mod interactive_session {
    use std::io::Write as _;

    /// Interactive prompt shown before each turn: `{app}> `.
    fn prompt() -> String {
        format!("{}> ", crate::host_identity::app())
    }

    /// Banner printed once at session start.
    fn print_banner() {
        println!("RightKit interactive session. Type a prompt and press Enter.");
        println!("Type /quit or press Ctrl+D (EOF) to exit.");
    }

    /// Build a one-shot [`RunArgs`] for a single interactive turn. Provider is
    /// resolved once per session from the merged config (same order as
    /// `{app} serve`: configured route for the default model, then keyed
    /// hosted provider) so a bare session behaves identically to every other
    /// entry point instead of hardcoding `openrouter`. The selected
    /// repository root is carried on `RunArgs.workspace` so session
    /// admission, tool scope, and the canonical Membrane caller all see the
    /// selected repository instead of re-deriving scope from the
    /// process-global current directory.
    fn build_turn_args(
        prompt: String,
        session_id: &str,
        provider: &str,
        model: Option<&str>,
        workspace: Option<String>,
        effort: Option<String>,
        silent: bool,
    ) -> super::RunArgs {
        super::RunArgs {
            context_mode: interactive_context_mode(std::env::args()),
            prompt,
            print: false,
            // The line-oriented REPL fallback owns stdout and needs the
            // streamed Text projection. The Ratatui TUI owns the viewport and
            // renders turn events from the durable event stream instead, so a
            // raw print path would corrupt the frame wherever the cursor sat.
            output_format: if silent {
                super::OutputFormat::Silent
            } else {
                super::OutputFormat::Text
            },
            json_event: false,
            model: model.map(|m| m.to_string()),
            provider: (!provider.is_empty()).then(|| provider.to_string()),
            effort,
            workspace,
            auto_approve: false,
            bypass_approvals: false,
            permission_mode: None,
            stream_json: false,
            exit_on_error: false,
            max_turns: None,
            allowed_tools: Vec::new(),
            disallowed_tools: Vec::new(),
            toolset: None,
            max_budget_usd: None,
            bare: false,
            safe_mode: false,
            system_prompt: None,
            system_prompt_file: None,
            append_system_prompt: super::LAUNCH_PRESET_SYSTEM_PROMPT.get().cloned(),
            append_system_prompt_file: None,
            mcp_config: None,
            session_id: Some(session_id.to_string()),
            add_dir: Vec::new(),
            input_format: super::InputFormat::Text,
            output_schema: None,
            image: Vec::new(),
            ephemeral: false,
            output_last_message: None,
            color: crate::cli::CliColor::Auto,
            tools: super::LAUNCH_TOOL_FILTERS
                .get()
                .map(|f| f.tools.clone())
                .unwrap_or_default(),
            exclude_tools: super::LAUNCH_TOOL_FILTERS
                .get()
                .map(|f| f.exclude_tools.clone())
                .unwrap_or_default(),
            no_mcp: super::LAUNCH_TOOL_FILTERS.get().is_some_and(|f| f.no_mcp),
        }
    }

    fn interactive_context_mode<I>(_args: I) -> super::ContextMode
    where
        I: IntoIterator<Item = String>,
    {
        // `--context-mode` is a deprecated no-op; only the internal
        // `RIGHTKIT_CONTEXT=off` switch selects the Membrane-free path.
        super::ContextMode::effective()
    }

    /// True when the TUI was launched outside any project: the user's home
    /// folder or a filesystem root. Everywhere else it opens straight in.
    fn launch_needs_project_home() -> bool {
        let Ok(cwd) = std::env::current_dir() else {
            return true;
        };
        let cwd = cwd.canonicalize().unwrap_or(cwd);
        if cwd.parent().is_none() {
            return true;
        }
        ["USERPROFILE", "HOME"]
            .iter()
            .filter_map(std::env::var_os)
            .map(std::path::PathBuf::from)
            .any(|home| home.canonicalize().unwrap_or(home) == cwd)
    }

    /// KRN defect #4: the single place that drives InlineHost's context
    /// warm-up (`super::InlineHost::warm`) and publishes truthful readiness
    /// for it. Used both by the background warm-up kicked off at TUI start
    /// and by each turn's pre-admission wait, so both paths report the
    /// same states and share the same underlying single-flight cache
    /// (`inline_host_init`) -- a turn that starts after warm-up already
    /// succeeded observes `Ready` immediately instead of re-driving a slow
    /// native connect.
    async fn ensure_inline_context_ready(
        readiness_tx: &tokio::sync::watch::Sender<crate::tui_host::HostReadiness>,
        context_mode: super::ContextMode,
    ) -> Result<(), String> {
        let mut degraded: Option<String> = None;
        let result = publish_context_readiness(readiness_tx, async {
            crate::main_runtime::InlineHost::warm(context_mode)
                .await
                .map(|reason| degraded = reason)
        })
        .await;
        if result.is_ok() {
            if let Some(reason) = degraded {
                readiness_tx.send_replace(crate::tui_host::HostReadiness::Degraded { reason });
            }
        }
        result
    }

    async fn publish_context_readiness(
        readiness_tx: &tokio::sync::watch::Sender<crate::tui_host::HostReadiness>,
        warm: impl std::future::Future<Output = Result<(), String>>,
    ) -> Result<(), String> {
        // Warm-up can finish while picker setup is still running, before
        // the TUI subscribes. `send` discards updates with no receivers.
        readiness_tx.send_replace(crate::tui_host::HostReadiness::StartingContext);
        match warm.await {
            Ok(()) => {
                readiness_tx.send_replace(crate::tui_host::HostReadiness::Ready);
                Ok(())
            }
            Err(error) => {
                readiness_tx.send_replace(crate::tui_host::HostReadiness::Failed {
                    message: error.clone(),
                });
                Err(error)
            }
        }
    }

    /// One resolved input line from the REPL loop.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum ReplLine {
        /// A non-empty prompt to submit to the engine.
        Prompt(String),
        /// Blank line — re-prompt without submitting a turn.
        Blank,
        /// `/quit` (or equivalent) — exit the loop.
        Quit,
        /// EOF (Ctrl+D / closed stdin) — exit the loop.
        Eof,
        /// `/model <model-id>` — queue a model switch for the *next* turn
        /// boundary (13a). Does not itself submit a turn.
        ModelSwitch(String),
        /// `/repo [path]` — select the repository root turns run against.
        /// Empty argument shows the current selection.
        RepoSelect(String),
        /// `/effort <level>` — queue a reasoning-effort value for the *next*
        /// turn. Does not itself submit a turn.
        EffortSelect(String),
    }

    /// Pure classifier for one raw line read from stdin. Split out from the
    /// I/O loop so the exit/quit/blank/prompt branching is unit-testable
    /// without a real stdin.
    ///
    /// This used to hand-match `/quit`/`/exit`/`/q` and hand-parse
    /// `/model <id>` as REPL-local literals (CR-24: the exact defect where
    /// `/model` here and `session set --model` on the CLI were two
    /// implementations that could disagree). Both now resolve through
    /// [`crate::cli::command_registry::CommandRouter`], the same registry
    /// the CLI and `/help` render from, so there is exactly one place that
    /// knows a line typed `/q` means `/quit`.
    fn classify_line(raw: Option<&str>) -> ReplLine {
        let Some(raw) = raw else {
            return ReplLine::Eof;
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return ReplLine::Blank;
        }
        if !trimmed.starts_with('/') {
            return ReplLine::Prompt(trimmed.to_string());
        }
        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let head = parts.next().unwrap_or("");
        let rest = parts.next().unwrap_or("").trim();

        let router = crate::cli::command_registry::CommandRouter;
        match router.resolve(head) {
            Some(name) if name == "/quit" => ReplLine::Quit,
            Some(name) if name == "/model" => {
                // Non-tty fallback only: bare `/model` falls through and is
                // submitted as a literal prompt so it is visibly re-prompted
                // rather than silently swallowed. The TTY/TUI path differs:
                // `EngineTurnHost::execute_command` honours
                // RendererHint::Picker there and presents the SHARED route
                // picker (`route_catalog`), then accepts `/model <n>`
                // or `/model <model-id>` via `resolve_model_selection`.
                if rest.is_empty() {
                    ReplLine::Prompt(trimmed.to_string())
                } else {
                    ReplLine::ModelSwitch(rest.to_string())
                }
            }
            Some(name) if name == "/repo" => ReplLine::RepoSelect(rest.to_string()),
            Some(name) if name == "/effort" => ReplLine::EffortSelect(rest.to_string()),
            _ => ReplLine::Prompt(trimmed.to_string()),
        }
    }

    /// Read one line of input, returning `None` on EOF. `rustyline`-free by
    /// design — this is the minimal line-oriented REPL the lane spec calls
    /// for, not a full editor/history TUI (follow-up scope).
    fn read_line<R: std::io::BufRead>(reader: &mut R) -> std::io::Result<Option<String>> {
        let mut buf = String::new();
        let bytes_read = reader.read_line(&mut buf)?;
        if bytes_read == 0 {
            return Ok(None);
        }
        Ok(Some(buf))
    }

    // ── Repository scope selection (1C) ──────────────────────────────────

    type SelectedRepository =
        rightkit_memory_client::backend::repository_scope::CanonicalRepositoryScope;

    /// Initialize the selected repository from the launch directory. The cwd
    /// is consulted ONCE here and only when the existing canonical
    /// repository scope resolves; an unenrolled or unresolvable launch
    /// directory starts with no repository selected (never a silently
    /// enrolled home directory, never a direct-mode fallback).
    fn resolve_launch_repository() -> Result<Option<SelectedRepository>, String> {
        let cwd = std::env::current_dir()
            .map_err(|error| format!("launch directory is unavailable: {error}"))?;
        resolve_repository(&cwd)
    }

    /// Resolve `root` to its registry binding. Enrolment is Pulse's concern;
    /// RightKit never runs `membrane init`.
    fn resolve_repository(root: &std::path::Path) -> Result<Option<SelectedRepository>, String> {
        use rightkit_memory_client::backend::repository_scope::resolve_canonical_repository_scope;
        resolve_canonical_repository_scope(root)
    }

    /// The selected canonical repository, else (D18: context unavailable runs
    /// visibly degraded, never refused) the launch folder — except the home
    /// folder or a filesystem root, which are never a workspace.
    fn turn_workspace(
        _mode: super::ContextMode,
        selected: Option<String>,
        launch: Option<String>,
    ) -> Option<String> {
        selected
            .or_else(|| launch.filter(|root| is_workspace_candidate(std::path::Path::new(root))))
    }

    fn is_workspace_candidate(root: &std::path::Path) -> bool {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(std::path::PathBuf::from)
            .map(|home| home.canonicalize().unwrap_or(home));
        root.is_dir() && root.parent().is_some() && home.as_deref() != Some(root.as_path())
    }

    fn fresh_interactive_session_id() -> String {
        format!("interactive-{}", rightkit_agent_engine_protocol::uuid_v7())
    }

    #[derive(Debug, Clone)]
    pub(super) struct LaunchSession {
        pub(super) id: String,
        pub(super) workspace: Option<String>,
        pub(super) provider: Option<String>,
        pub(super) model: Option<String>,
        pub(super) effort: Option<String>,
    }

    pub(super) async fn resolve_launch_session(
        launch: &super::LaunchCli,
    ) -> Result<Option<LaunchSession>, String> {
        if launch.resume.is_none() && !launch.continue_last {
            return Ok(None);
        }
        let rows = if super::service_host_explicitly_selected() {
            let provider = launch.provider.clone().unwrap_or_default();
            let host = EngineTurnHost::new(fresh_interactive_session_id(), provider, true, true);
            host.daemon_client().await?.list_sessions().await?
        } else {
            let context_mode = super::ContextMode::effective();
            super::InlineHost::warm(context_mode).await?;
            super::InlineHost::list_all_sessions(context_mode).await?
        };
        let selected = if let Some(id) = launch.resume.as_deref() {
            rows.into_iter()
                .find(|row| row.get("id").and_then(serde_json::Value::as_str) == Some(id))
                .ok_or_else(|| format!("session {id} was not found"))?
        } else {
            rows.into_iter()
                .filter(|row| {
                    row.get("parentSessionId")
                        .is_none_or(serde_json::Value::is_null)
                })
                .filter(|row| {
                    !matches!(
                        row.get("status").and_then(serde_json::Value::as_str),
                        Some("archived" | "deleted")
                    )
                })
                .max_by_key(|row| {
                    row.get("updatedAt")
                        .or_else(|| row.get("updated_at"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                        .to_string()
                })
                .ok_or_else(|| "no session is available to continue".to_string())?
        };
        let string = |key: &str| {
            selected
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        };
        let id = string("id").ok_or_else(|| "selected session has no id".to_string())?;
        Ok(Some(LaunchSession {
            id,
            workspace: string("workspaceRoot").or_else(|| string("workspace")),
            provider: string("providerId"),
            model: string("model"),
            effort: string("effort"),
        }))
    }

    pub(super) async fn resolve_launch_route_state(
        launch: &super::LaunchCli,
        session: Option<&LaunchSession>,
    ) -> Result<(String, String), String> {
        let route = if let Some(requested) = launch.model.as_deref() {
            resolve_launch_route(requested).await?
        } else if let Some(model) = session.and_then(|row| row.model.as_deref()) {
            match session.and_then(|row| row.provider.as_deref()) {
                Some(provider) => (provider.to_string(), model.to_string()),
                None => resolve_launch_route(model).await?,
            }
        } else if let Some(route) = super::load_model_selection() {
            route
        } else if launch.prompt.is_some() {
            let configured_model = super::load_current_merged_config().model.default_model;
            if configured_model.trim().is_empty() {
                (super::resolve_default_provider_id_checked(&super::load_current_merged_config()).unwrap_or_default(), String::new())
            } else {
                resolve_launch_route(configured_model.trim()).await?
            }
        } else {
            (
                super::resolve_default_provider_id_checked(&super::load_current_merged_config())
                    .unwrap_or_default(),
                String::new(),
            )
        };
        let route = match launch.provider.as_deref() {
            Some(provider) if !route.1.is_empty() => {
                resolve_launch_route(&format!("{provider}{ROUTE_SELECTION_SEP}{}", route.1)).await?
            }
            Some(provider) => (provider.to_string(), route.1),
            None => route,
        };
        Ok(route)
    }

    fn resolve_launch_effort(
        launch: &super::LaunchCli,
        session: Option<&LaunchSession>,
    ) -> Result<Option<String>, String> {
        let raw = launch
            .effort
            .as_deref()
            .or_else(|| session.and_then(|row| row.effort.as_deref()));
        crate::api::normalize_effort(raw.map(str::to_string))
    }

    /// Resolve a `/repo <path>` argument to an enrolled repository's
    /// canonical scope. `path` may be relative; it is interpreted against
    /// the process launch directory exactly once, then canonicalized by the
    /// registry resolver. Unenrolled roots fail with a typed
    /// setup/degradation answer — never an enrollment, index, or silent
    /// context-mode switch.
    fn resolve_repository_selection(path_arg: &str) -> Result<SelectedRepository, String> {
        let trimmed = path_arg.trim();
        if trimmed.is_empty() {
            return Err("usage: /repo [path]".to_string());
        }
        let candidate = std::path::PathBuf::from(trimmed);
        match resolve_repository(&candidate) {
            Ok(Some(scope)) => Ok(scope),
            Ok(None) => Err(format!("{trimmed} is not a directory RightKit can open")),
            Err(reason) => Err(format!("repository selection unavailable: {reason}")),
        }
    }

    /// Route/scope fields restored from one durable session row (the
    /// `session_snapshot` projection `list_sessions`/`session_row` emit).
    /// Missing fields stay `None` — no local guess ever substitutes for
    /// an absent durable value, and a missing/invalid root is a typed
    /// failure, never a silent keep of the previous session's scope.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    struct RestoredSessionRoute {
        workspace_root: Option<String>,
        provider_id: Option<String>,
        model: Option<String>,
        effort: Option<String>,
    }

    /// Project the durable route fields out of one session snapshot row.
    /// Pure: the row is the only input, so restoration semantics are
    /// unit-testable without a host or store.
    fn session_snapshot_route(row: &serde_json::Value) -> RestoredSessionRoute {
        let string = |key: &str| {
            row.get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        };
        RestoredSessionRoute {
            workspace_root: string("workspaceRoot").or_else(|| string("workspace")),
            provider_id: string("providerId"),
            model: string("model"),
            effort: string("effort"),
        }
    }

    /// Merge a field-level update into the pending no-repo route tuple
    /// `(model, provider, effort)`. `Some` wins over the stored field;
    /// `None` preserves it — an effort-only or model-only selection must
    /// never overwrite the other pending fields with empties.
    fn merge_pending_route(
        pending: Option<(String, String, Option<String>)>,
        model: Option<String>,
        provider: Option<String>,
        effort: Option<String>,
    ) -> (String, String, Option<String>) {
        let (old_model, old_provider, old_effort) = pending.unwrap_or_default();
        (
            model.unwrap_or(old_model),
            provider.unwrap_or(old_provider),
            effort.or(old_effort),
        )
    }

    /// The provider a durable session row's model resolves to when the row
    /// itself does not record `providerId`: `Some` only when the shared
    /// catalog names exactly ONE selectable provider for the model.
    /// Ambiguity or absence returns `None` — callers turn that into a typed
    /// refusal; the previous session's provider is never a guess.
    fn unique_route_provider(catalog: &[CatalogRoute], model: &str) -> Option<String> {
        let providers: std::collections::BTreeSet<&str> = catalog
            .iter()
            .filter(|route| route.model == model && route.selectable)
            .map(|route| route.provider.as_str())
            .collect();
        (providers.len() == 1).then(|| {
            providers
                .into_iter()
                .next()
                .expect("len checked")
                .to_string()
        })
    }

    /// Render the `/repo` picker as a plain list. `roots` are candidate
    /// repository roots already known to this host (session-attributed
    /// roots); `current` marks the selected one. The donor list widget
    /// replaces this projection when Stage 3 lands — the content contract
    /// (one canonical root per row, current marked) is what callers and
    /// tests pin down.
    fn format_repo_picker(roots: &[String], current: Option<&str>) -> String {
        use std::fmt::Write as _;
        let mut out =
            String::from("Select the repository for new turns — `/repo <path>` or `/repo <n>`:\n");
        for (i, root) in roots.iter().enumerate() {
            let marker = if Some(root.as_str()) == current {
                " (selected)"
            } else {
                ""
            };
            let _ = writeln!(out, "  {}) {}{}", i + 1, root, marker);
        }
        if roots.is_empty() {
            out.push_str("  (no repository roots are known to this session yet)\n");
        }
        out
    }

    /// Repository roots known to this host: the canonical roots of durable
    /// sessions this process created (`session_roots`) plus the current
    /// selection. Every entry came through `resolve_canonical_repository_scope`,
    /// so the list only ever holds enrolled roots — nothing is enrolled or
    /// indexed here. A registry-wide enumeration is a parent-owned seam (the
    /// bin has no `membrane_runtime` dependency and `SessionExecutionHost`
    /// exposes no root listing yet).
    fn known_repository_roots(
        session_roots: &std::collections::HashMap<String, String>,
        current: Option<&str>,
    ) -> Vec<String> {
        let mut roots: Vec<String> = session_roots.values().cloned().collect();
        if let Some(root) = current {
            if !roots.iter().any(|known| known == root) {
                roots.push(root.to_string());
            }
        }
        roots.sort();
        roots.dedup();
        roots
    }

    /// Materialize a durable root session for a repository scope through the
    /// shared `SessionExecutionHost::ensure_session` authority — the same
    /// session-create body admission uses, so the new session is canonical
    /// before any local state points at it.
    async fn ensure_root_session(
        context_mode: super::ContextMode,
        session_id: &str,
        workspace_root: &str,
        provider: &str,
        model: Option<&str>,
        effort: Option<String>,
    ) -> Result<(), String> {
        let host = super::SessionExecutionHost::select(context_mode).await?;
        let args = build_turn_args(
            String::new(),
            session_id,
            provider,
            model,
            Some(workspace_root.to_string()),
            effort,
            true,
        );
        host.ensure_session(&args, session_id, workspace_root).await
    }

    /// Generic CLI titles carry no searchable identity. Project the first
    /// admitted prompt from canonical history without renaming the session
    /// or making a provider request merely to populate a picker.
    async fn session_picker_label(
        context_mode: super::ContextMode,
        row: &serde_json::Value,
    ) -> Option<String> {
        let title = row
            .get("title")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if !title.trim().is_empty()
            && !["CLI session", "New session", "Default session"]
                .iter()
                .any(|generic| title.trim().eq_ignore_ascii_case(generic))
        {
            return Some(title.to_string());
        }
        let id = row.get("id").and_then(serde_json::Value::as_str)?;
        let host = super::SessionExecutionHost::select_if_ready(context_mode)?;
        let page = host.fetch_history(id, 65, 64).await.ok()?;
        let events = page.as_array()?;
        let text = events.iter().find_map(|event| {
            let kind = event
                .get("$kind")
                .or_else(|| event.get("type"))
                .and_then(serde_json::Value::as_str)?;
            if kind != "userMessageSubmitted" {
                return None;
            }
            event.get("text")?.as_str()
        })?;
        let label = text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(90)
            .collect::<String>();
        (!label.is_empty()).then_some(label)
    }

    /// Non-secret broker status for one provider, projected from the public
    /// `api_provider_auth::terminal_status` record (`configured`, `source`,
    /// `subscription_auth_state`, `available_model_ids`). Credential bytes
    /// are never read — `configured`/`available_model_ids` are the only
    /// signals this host consumes.
    #[derive(Debug, Clone, Default)]
    struct ProviderAuthStatus {
        /// The broker resolved some configured credential source.
        configured: bool,
        /// The configured source is the provider's subscription OAuth
        /// (`source == "supported_subscription_oauth"`).
        oauth: bool,
        /// Provider-scoped models the subscription flow reported (broker
        /// account lists or the live catalog fetch). Empty means the account
        /// proved no per-model list — never treated as a catalog.
        available_models: Vec<crate::api_provider_auth::CatalogModel>,
    }

    /// Read one provider's broker status. Providers the broker does not
    /// register (custom) fall back to presence-only keychain/env
    /// checks — the same substrate onboarding inspects.
    async fn provider_auth_status(provider_id: &str) -> ProviderAuthStatus {
        let status = match crate::api_provider_auth::terminal_status(provider_id).await {
            Ok(record) => ProviderAuthStatus {
                configured: record
                    .get("configured")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                oauth: record.get("source").and_then(serde_json::Value::as_str)
                    == Some("supported_subscription_oauth"),
                available_models: record
                    .get("available_model_ids")
                    .and_then(serde_json::Value::as_array)
                    .map(|ids| {
                        ids.iter()
                            .filter_map(serde_json::Value::as_str)
                            .filter(|id| !id.trim().is_empty())
                            .map(|id| crate::api_provider_auth::CatalogModel {
                                slug: id.to_string(),
                                display_name: id.to_string(),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            },
            // Broker knows nothing about custom endpoint: fall back to the production
            // keychain/env
            // presence check rather than declaring it unconfigured.
            Err(_) => ProviderAuthStatus {
                configured: rightkit_llm::agent_providers::load_api_key(provider_id).is_some()
                    || rightkit_llm::agent_providers::KeyStore::get(
                        rightkit_llm::agent_providers::KeyStore::default_service(),
                        provider_id,
                    )
                    .ok()
                    .is_some_and(|value| !value.trim().is_empty()),
                ..ProviderAuthStatus::default()
            },
        };
        // A ChatGPT sign-in proves no model list in the stored credential;
        // ask the account's live catalog so new models appear without a
        // RightKit release. A fetch failure keeps "no account list"
        // semantics here — `{app} models --provider openai` surfaces the
        // typed stage error instead.
        // The broker's status `source` can name a stored key even when the
        // credential turns resolve is the ChatGPT sign-in, so the resolved
        // credential decides: a catalog answer proves the sign-in.
        if status.configured && status.available_models.is_empty() {
            if let Ok(models) =
                crate::api_provider_auth::fetch_chatgpt_account_models(provider_id).await
            {
                if !models.is_empty() {
                    return ProviderAuthStatus {
                        oauth: true,
                        available_models: models,
                        ..status
                    };
                }
            }
        }
        status
    }

    /// One route the model picker can offer: the provider id, the NATIVE
    /// model id the route authority admits (`MiniMax-M3`, `gpt-6.1-sol`,
    /// `moonshotai/kimi-k2.6`), and its canonical capability row when the
    /// shared `model_metadata` table knows it.
    #[derive(Debug, Clone)]
    struct CatalogRoute {
        provider: String,
        model: String,
        display_name: String,
        /// `false` = a typed-unavailable row: shown (with `availability_note`)
        /// so the picker displays reality, but selecting it errors at the
        /// route authority instead of pretending to be a valid choice.
        selectable: bool,
        availability_note: Option<String>,
        /// The session-configured default pin (renders `(default)`).
        is_default: bool,
        capability: Option<&'static rightkit_llm::agent_providers::model_metadata::ModelCapability>,
    }

    /// Canonical metadata rows a provider can actually serve, as
    /// `(native model id, capability)` pairs. OpenRouter accepts catalog
    /// slugs verbatim; MiniMax/OpenAI translate to their native id forms
    /// through `provider_model_id`/prefix-stripping. Aggregator-unknown
    /// routes (custom) have no canonical projection — only
    /// committed/configured pins are authoritative there.
    fn canonical_models_for_provider(
        provider_id: &str,
    ) -> Vec<(
        String,
        &'static rightkit_llm::agent_providers::model_metadata::ModelCapability,
    )> {
        let mut rows: Vec<_> = Vec::new();
        for cap in rightkit_llm::agent_providers::model_metadata_table() {
            let (vendor, native) = match cap.slug.split_once('/') {
                Some(pair) => pair,
                None => continue,
            };
            let id = if provider_id == "openrouter" {
                // OpenRouter's native id IS the catalog slug.
                cap.slug.clone()
            } else if vendor == provider_id {
                // `provider_model_id` owns native-id translation (e.g.
                // minimax/minimax-m3 -> MiniMax-M3); the plain remainder is
                // already native for openai-style providers.
                rightkit_llm::agent_providers::provider_model_id(provider_id, native)
            } else {
                continue;
            };
            rows.push((id, cap));
        }
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        rows
    }

    /// Capability row for a `(provider, native model id)` pair: direct slug
    /// lookup (covers `vendor/model` ids and the tested `gpt-6.1-sol`
    /// native alias), then the provider-prefixed slug, then the provider's
    /// id translation over the canonical table.
    fn capability_for_route(
        provider_id: &str,
        model_id: &str,
    ) -> Option<&'static rightkit_llm::agent_providers::model_metadata::ModelCapability> {
        if let Some(cap) = rightkit_llm::agent_providers::capability_of(model_id) {
            return Some(cap);
        }
        if let Some(cap) =
            rightkit_llm::agent_providers::capability_of(&format!("{provider_id}/{model_id}"))
        {
            return Some(cap);
        }
        rightkit_llm::agent_providers::model_metadata_table()
            .iter()
            .find(|cap| {
                cap.slug.split('/').next() == Some(provider_id)
                    && rightkit_llm::agent_providers::provider_model_id(provider_id, &cap.slug)
                        == model_id
            })
    }

    /// Effort ids a model actually accepts: `normalize_effort`'s closed set
    /// (`low|medium|high`) intersected with the canonical capability row's
    /// explicit supported_efforts metadata. Reasoning depth never implies effort. `None` means the model has no canonical capability row:
    /// callers may keep the already-committed effort but must never invent
    /// a level.
    fn supported_effort_ids(
        capability: Option<&rightkit_llm::agent_providers::model_metadata::ModelCapability>,
    ) -> Option<Vec<&'static str>> {
        const LEVELS: [&str; 3] = ["low", "medium", "high"];
        let capability = capability?;
        capability.supported_efforts.as_ref()?;
        Some(
            LEVELS
                .iter()
                .filter(|id| {
                    capability
                        .supported_efforts
                        .as_ref()
                        .is_some_and(|supported| supported.iter().any(|value| value == **id))
                })
                .copied()
                .collect(),
        )
    }

    /// Effort rows a picker advertises for one model: the canonical
    /// capability row's supported levels, `current` marked. Unknown
    /// capability permits only the already-committed level, labelled so the
    /// picker does not invent modes. An empty result means the model takes
    /// no effort step (commits directly) or `/effort` reports unavailable.
    fn effort_choices(
        capability: Option<&rightkit_llm::agent_providers::model_metadata::ModelCapability>,
        committed_effort: Option<&str>,
    ) -> Vec<rightkit_terminal_ui::nav::EffortChoice> {
        use rightkit_terminal_ui::nav::EffortChoice;
        const LABELS: [(&str, &str); 3] = [("low", "Low"), ("medium", "Medium"), ("high", "High")];
        match supported_effort_ids(capability) {
            Some(ids) => LABELS
                .iter()
                .filter(|(id, _)| ids.contains(id))
                .map(|(id, label)| {
                    EffortChoice::new(*id, *label).current(committed_effort == Some(*id))
                })
                .collect(),
            None => committed_effort
                .map(|level| {
                    let label = LABELS
                        .iter()
                        .find(|(id, _)| *id == level)
                        .map(|(_, label)| *label)
                        .unwrap_or(level);
                    vec![EffortChoice::new(level, label)
                        .current(true)
                        .description("committed level — model capabilities are unknown")]
                })
                .unwrap_or_default(),
        }
    }

    /// Build the shared catalog projection every surface consumes (picker,
    /// `/model <n>`, `/model <id>`): canonical provider metadata for
    /// configured API-key routes, broker-reported account ids for OAuth
    /// routes, plus the always-present committed/configured authority rows
    /// and explicit `provider-routes.*` entries already resolved through
    /// the configured-route engine path (`configured_routes` carries
    /// pre-validated `(provider_id, model)` pairs — unresolvable rows
    /// never arrive here). `statuses` maps provider id → broker status
    /// (absent = unconfigured).
    fn build_route_catalog(
        committed: Option<(&str, &str)>,
        configured: Option<(&str, &str)>,
        statuses: &std::collections::HashMap<String, ProviderAuthStatus>,
        configured_routes: &[(String, String)],
    ) -> Vec<CatalogRoute> {
        let authoritative = |provider: &str, model: &str| -> bool {
            [committed, configured]
                .into_iter()
                .flatten()
                .any(|(m, p)| p == provider && !m.is_empty() && m == model)
        };
        let is_configured = |provider: &str, model: &str| -> bool {
            configured
                .map(|(m, p)| p == provider && !m.is_empty() && m == model)
                .unwrap_or(false)
        };
        let mut routes: Vec<CatalogRoute> = Vec::new();
        let mut push = |route: CatalogRoute| {
            if !routes
                .iter()
                .any(|r| r.provider == route.provider && r.model == route.model)
            {
                routes.push(route);
            }
        };
        for choice in super::runtime_route_picker() {
            let status = statuses.get(&choice.provider).cloned().unwrap_or_default();
            if !status.configured {
                continue;
            }
            if choice.native_subscription && status.oauth {
                if status.available_models.is_empty() {
                    // The account reported no per-model list: only the
                    // authoritative (committed/configured/route-default) row
                    // is selectable; other canonical provider models render
                    // as typed-unavailable rows rather than a fabricated
                    // account catalog.
                    for (id, cap) in canonical_models_for_provider(&choice.provider) {
                        let ok = authoritative(&choice.provider, &id) || id == choice.model;
                        push(CatalogRoute {
                            provider: choice.provider.clone(),
                            model: id.clone(),
                            display_name: id.clone(),
                            selectable: ok,
                            availability_note: (!ok).then(|| {
                                format!(
                                    "{} does not report this model as available on the account",
                                    choice.provider
                                )
                            }),
                            is_default: is_configured(&choice.provider, &id),
                            capability: Some(cap),
                        });
                    }
                } else {
                    for model in &status.available_models {
                        let cap = capability_for_route(&choice.provider, &model.slug);
                        push(CatalogRoute {
                            provider: choice.provider.clone(),
                            model: model.slug.clone(),
                            display_name: model.display_name.clone(),
                            selectable: cap.is_some(),
                            availability_note: cap.is_none().then(|| {
                                format!(
                                    "loaded model catalog has no capacity profile for {}",
                                    model.slug
                                )
                            }),
                            is_default: is_configured(&choice.provider, &model.slug),
                            capability: cap,
                        });
                    }
                }
            } else {
                // API-key configured route: the canonical provider model
                // metadata is the catalog.
                for (id, cap) in canonical_models_for_provider(&choice.provider) {
                    push(CatalogRoute {
                        provider: choice.provider.clone(),
                        display_name: id.clone(),
                        is_default: is_configured(&choice.provider, &id),
                        model: id,
                        selectable: true,
                        availability_note: None,
                        capability: Some(cap),
                    });
                }
            }
            // The route's configured default is an authority even when the
            // canonical projection does not carry it.
            if !choice.model.is_empty() {
                let cap = capability_for_route(&choice.provider, &choice.model);
                push(CatalogRoute {
                    provider: choice.provider.clone(),
                    display_name: choice.model.clone(),
                    is_default: is_configured(&choice.provider, &choice.model),
                    model: choice.model.clone(),
                    selectable: true,
                    availability_note: None,
                    capability: cap,
                });
            }
        }
        // Explicit `provider-routes.*` entries are a route authority this
        // host already owns: each row admitted by configured-route engine
        // resolution offers exactly its configured model — never a
        // fabricated account/catalog list. `push` dedup keeps the earlier
        // canonical row when a configured route names the same
        // provider+model, and `provider::model` qualification still
        // disambiguates a same-named model under distinct providers.
        for (provider_id, model) in configured_routes {
            // Account-reported availability remains authoritative for native
            // subscriptions, including explicit config rows.
            if statuses.get(provider_id).is_some_and(|status| status.oauth) {
                continue;
            }
            let cap = capability_for_route(provider_id, model);
            push(CatalogRoute {
                provider: provider_id.clone(),
                model: model.clone(),
                display_name: model.clone(),
                selectable: true,
                availability_note: None,
                is_default: is_configured(provider_id, model),
                capability: cap,
            });
        }
        // Committed and configured pins are ALWAYS offered — an authority
        // the picker must display even when no other choice exists.
        for pin in [committed, configured].into_iter().flatten() {
            let (model, provider) = pin;
            if model.is_empty() || provider.is_empty() {
                continue;
            }
            let cap = capability_for_route(provider, model);
            push(CatalogRoute {
                provider: provider.to_string(),
                model: model.to_string(),
                display_name: model.to_string(),
                selectable: true,
                availability_note: None,
                is_default: is_configured(provider, model),
                capability: cap,
            });
        }
        routes
    }

    /// Resolve per-picker-provider broker status, then project the shared
    /// catalog and hand back the statuses alongside it — `/providers` and
    /// the provider-first `/model` need the same status records the catalog
    /// was built from (auth-source labels, configured state) without a
    /// second broker round-trip. `committed`/`configured` are
    /// `(model, provider)` authority pins (committed route state,
    /// session-configured default).
    async fn route_catalog_with_statuses(
        committed: Option<(&str, &str)>,
        configured: Option<(&str, &str)>,
    ) -> (
        Vec<CatalogRoute>,
        std::collections::HashMap<String, ProviderAuthStatus>,
    ) {
        let mut statuses = std::collections::HashMap::new();
        for choice in super::runtime_route_picker() {
            statuses.insert(
                choice.provider.clone(),
                provider_auth_status(&choice.provider).await,
            );
        }
        // Explicit `provider-routes.*` entries come from the same resolved
        // config/session source that created this host —
        // `load_current_merged_config`, the one `resolve_engine_config`
        // itself consults — so a configured route the fixed picker cannot
        // know still offers its configured model.
        let configured_routes =
            super::configured_catalog_routes(&super::load_current_merged_config());
        let mut catalog = build_route_catalog(committed, configured, &statuses, &configured_routes);
        let vendors = tokio::task::spawn_blocking(|| {
            use rightkit_agents::driver_host::vendor_cli::{list_models, VendorCli};
            [VendorCli::ClaudeCode, VendorCli::Antigravity]
                .into_iter()
                .map(|kind| (kind.provider_id(), list_models(kind)))
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        for (provider, models) in vendors {
            append_vendor_routes(&mut catalog, provider, &models);
        }
        (catalog, statuses)
    }

    fn append_vendor_routes(
        catalog: &mut Vec<CatalogRoute>,
        provider: &str,
        models: &[(String, String)],
    ) {
        for (model, display_name) in models {
            if catalog
                .iter()
                .any(|route| route.provider == provider && route.model == *model)
            {
                continue;
            }
            catalog.push(CatalogRoute {
                provider: provider.to_string(),
                model: model.clone(),
                display_name: display_name.clone(),
                selectable: true,
                availability_note: None,
                is_default: false,
                capability: None,
            });
        }
    }

    async fn route_catalog(
        committed: Option<(&str, &str)>,
        configured: Option<(&str, &str)>,
    ) -> Vec<CatalogRoute> {
        route_catalog_with_statuses(committed, configured).await.0
    }

    /// Group the shared route catalog into provider rows for the
    /// provider-first `/model` flow and the `/providers` visibility
    /// manager. Every picker provider appears — configured or not — with
    /// its catalog rows carrying the persisted `model-visibility.json`
    /// flags; an unconfigured provider's model list falls back to its
    /// canonical projection so visibility can be managed before
    /// credentials exist. Pure: picker composition is unit-testable
    /// without a session host.
    fn group_catalog_by_provider(
        catalog: &[CatalogRoute],
        statuses: &std::collections::HashMap<String, ProviderAuthStatus>,
        visibility: &std::collections::BTreeMap<String, Vec<String>>,
        committed: Option<(&str, &str)>,
        committed_effort: Option<&str>,
    ) -> Vec<rightkit_terminal_ui::nav::ProviderChoice> {
        use rightkit_terminal_ui::nav::{ModelChoice, ProviderChoice, ProviderModel};
        // The `/providers` screen's fixed provider list, in screen order.
        // Providers outside it (minimax/opencode/custom) are hidden from
        // both `/providers` and `/model`; their code and keys stay.
        let provider_ids: Vec<String> = super::provider_setup::SCREEN_PROVIDERS
            .iter()
            .map(|screen| screen.id.to_string())
            .collect();
        let mut groups: Vec<ProviderChoice> = provider_ids
            .into_iter()
            .map(|provider| {
                let status = statuses.get(&provider);
                let oauth = status.map(|s| s.oauth).unwrap_or(false);
                let screen = super::provider_setup::screen_provider(&provider)
                    .expect("screen provider ids come from SCREEN_PROVIDERS");
                // A provider counts as configured when the broker says so
                // OR the catalog carries one of its rows — configured and
                // committed pins are real routes even without a broker
                // record.
                let credential_present = status.map(|s| s.configured).unwrap_or(false);
                let configured =
                    credential_present || catalog.iter().any(|route| route.provider == provider);
                let ready = match screen.kind {
                    rightkit_terminal_ui::nav::ProviderKind::OAuth => oauth,
                    _ => credential_present,
                };
                let status_text = super::provider_setup::project_status(
                    screen.kind,
                    credential_present,
                    oauth,
                    None,
                );
                let label = super::provider_setup::screen_label(screen, status_text);
                let hidden = |model: &str| {
                    visibility
                        .get(&provider)
                        .is_some_and(|ids| ids.iter().any(|id| id == model))
                };
                let mut models: Vec<ProviderModel> = catalog
                    .iter()
                    .filter(|route| route.provider == provider)
                    .map(|route| {
                        let is_current = committed
                            .map(|(model, p)| model == route.model && p == route.provider)
                            .unwrap_or(false);
                        let mut choice = ModelChoice::new(
                            route_selection_id(catalog, route, committed),
                            route.display_name.clone(),
                        )
                        .provider(route.provider.clone())
                        .current(is_current)
                        .default_flag(route.is_default);
                        if route.selectable {
                            choice =
                                choice.efforts(effort_choices(route.capability, committed_effort));
                        } else if let Some(note) = &route.availability_note {
                            choice = choice.description(note.clone());
                        }
                        ProviderModel {
                            model_id: route.model.clone(),
                            hidden: hidden(&route.model),
                            choice,
                        }
                    })
                    .collect();
                if models.is_empty() {
                    // No catalog rows (an unconfigured provider): the
                    // canonical projection still lets `/providers` manage
                    // visibility before credentials exist.
                    for (id, cap) in canonical_models_for_provider(&provider) {
                        models.push(ProviderModel {
                            model_id: id.clone(),
                            hidden: hidden(&id),
                            choice: ModelChoice::new(id.clone(), id.clone())
                                .provider(provider.clone())
                                .efforts(effort_choices(Some(cap), committed_effort)),
                        });
                    }
                }
                ProviderChoice {
                    id: provider.clone(),
                    label,
                    is_current: committed.map(|(_, p)| p == provider).unwrap_or(false),
                    configured,
                    models,
                    kind: screen.kind,
                    ready,
                    note: None,
                }
            })
            .collect();
        // Explicit `provider-routes.*` entries (and committed/configured
        // pins) the fixed screen cannot know still arrive as catalog rows.
        // `/model` offers them as their own provider groups so a configured
        // route is selectable; the hidden built-ins stay hidden.
        let mut custom_ids: Vec<&str> = Vec::new();
        for route in catalog {
            let id = route.provider.as_str();
            if super::provider_setup::screen_provider(id).is_none()
                && !["minimax", "opencode", "custom"]
                    .iter()
                    .any(|hidden| hidden.eq_ignore_ascii_case(id))
                && rightkit_agents::driver_host::vendor_cli::VendorCli::from_provider_id(id)
                    .is_none()
                && !custom_ids.contains(&id)
            {
                custom_ids.push(id);
            }
        }
        for id in custom_ids {
            let models: Vec<ProviderModel> = catalog
                .iter()
                .filter(|route| route.provider == id)
                .map(|route| {
                    let is_current = committed
                        .map(|(model, p)| model == route.model && p == route.provider)
                        .unwrap_or(false);
                    let mut choice = ModelChoice::new(
                        route_selection_id(catalog, route, committed),
                        route.display_name.clone(),
                    )
                    .provider(route.provider.clone())
                    .current(is_current)
                    .default_flag(route.is_default);
                    if route.selectable {
                        choice = choice.efforts(effort_choices(route.capability, committed_effort));
                    } else if let Some(note) = &route.availability_note {
                        choice = choice.description(note.clone());
                    }
                    ProviderModel {
                        model_id: route.model.clone(),
                        hidden: visibility
                            .get(id)
                            .is_some_and(|ids| ids.iter().any(|m| m == &route.model)),
                        choice,
                    }
                })
                .collect();
            groups.push(ProviderChoice {
                id: id.to_string(),
                label: id.to_string(),
                is_current: committed.map(|(_, p)| p == id).unwrap_or(false),
                configured: true,
                models,
                kind: rightkit_terminal_ui::nav::ProviderKind::ApiKey,
                ready: true,
                note: None,
            });
        }
        groups
    }

    /// `{app} models [--provider <id>]`: print one model id per line —
    /// the live account catalog for a native-subscription (ChatGPT sign-in)
    /// provider, the broker's account-reported list for other OAuth routes,
    /// or the canonical provider table for API-key routes. This is the
    /// direct verification surface for the live fetch: fetch failures print
    /// the typed stage error and exit non-zero instead of collapsing to an
    /// empty list.
    pub(crate) async fn cli_models(provider_arg: Option<String>) -> i32 {
        let provider = provider_arg
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| {
                super::resolve_default_provider_id(&super::load_current_merged_config())
            });
        if let Some(kind) =
            rightkit_agents::driver_host::vendor_cli::VendorCli::from_provider_id(&provider)
        {
            let models = tokio::task::spawn_blocking(move || {
                rightkit_agents::driver_host::vendor_cli::list_models(kind)
            })
            .await
            .unwrap_or_default();
            if models.is_empty() {
                eprintln!(
                    "{app} models {provider}: vendor catalog unavailable",
                    app = crate::host_identity::app()
                );
                return 1;
            }
            for (id, _) in models {
                println!("{id}");
            }
            return 0;
        }
        let native = super::runtime_route_picker()
            .iter()
            .any(|choice| choice.provider == provider && choice.native_subscription);
        if native {
            return match crate::api_provider_auth::fetch_chatgpt_account_models(&provider).await {
                Ok(models) => {
                    for model in &models {
                        println!("{}", model.slug);
                    }
                    0
                }
                Err(error) => {
                    eprintln!(
                        "{app} models {provider}: {error}",
                        app = crate::host_identity::app()
                    );
                    1
                }
            };
        }
        let status = provider_auth_status(&provider).await;
        if status.oauth {
            for model in &status.available_models {
                println!("{}", model.slug);
            }
            return 0;
        }
        let mut ids: Vec<String> = canonical_models_for_provider(&provider)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        if ids.is_empty() {
            // A configured custom endpoint carries only its pinned model.
            let configured_routes =
                super::configured_catalog_routes(&super::load_current_merged_config());
            ids.extend(
                configured_routes
                    .iter()
                    .filter(|(p, _)| p == &provider)
                    .map(|(_, model)| model.clone()),
            );
        }
        ids.sort();
        ids.dedup();
        if ids.is_empty() {
            eprintln!(
                "{app} models {provider}: provider is not configured or has no known models",
                app = crate::host_identity::app()
            );
            return 1;
        }
        for id in ids {
            println!("{id}");
        }
        0
    }

    /// Look up a model id in the shared catalog projection. Selectable rows
    /// resolve; a typed-unavailable row resolves to its provider so callers
    /// can report the real reason rather than "unknown model".
    fn catalog_route_for_model<'a>(
        catalog: &'a [CatalogRoute],
        model_id: &str,
    ) -> Option<&'a CatalogRoute> {
        catalog.iter().find(|route| route.model == model_id)
    }

    /// Separator inside one opaque picker selection id: `provider::model`.
    /// A picker row's `ModelChoice.id` carries this key whenever the same
    /// native model id is served by more than one provider — the bare id
    /// cannot name one route, and collapsing to "first match" silently
    /// picks a provider the user did not choose.
    const ROUTE_SELECTION_SEP: &str = "::";

    /// The picker-facing selection id for one catalog row: the bare native
    /// model id when no other provider offers that model, else the opaque
    /// `provider::model` key. The COMMITTED row keeps its bare id even
    /// under collision — the effort picker locates the committed model's
    /// effort rows by that id, and re-affirming the current route resolves
    /// deterministically to the committed provider.
    fn route_selection_id(
        catalog: &[CatalogRoute],
        route: &CatalogRoute,
        committed: Option<(&str, &str)>,
    ) -> String {
        let collides = catalog
            .iter()
            .any(|other| other.model == route.model && other.provider != route.provider);
        let is_committed = committed
            .map(|(model, provider)| model == route.model && provider == route.provider)
            .unwrap_or(false);
        if collides && !is_committed {
            format!("{}{ROUTE_SELECTION_SEP}{}", route.provider, route.model)
        } else {
            route.model.clone()
        }
    }

    /// Resolve one selection id to its catalog row: an opaque
    /// `provider::model` key matches the exact provider row; a bare model
    /// id resolves only when exactly ONE selectable route offers it — a
    /// same-named model under distinct providers is a typed ambiguity,
    /// never a first-match guess. Unresolvable input reports the real
    /// reason (unavailable note where one exists).
    fn resolve_catalog_route<'a>(
        catalog: &'a [CatalogRoute],
        selection: &str,
    ) -> Result<&'a CatalogRoute, String> {
        let trimmed = selection.trim();
        if let Some((kind, model)) = super::parse_vendor_route(trimmed) {
            return resolve_catalog_route(catalog, &format!("{}::{model}", kind.provider_id()));
        }
        if let Some((provider, model)) = trimmed.split_once(ROUTE_SELECTION_SEP) {
            return catalog
                .iter()
                .find(|route| route.provider == provider && route.model == model)
                .ok_or_else(|| format!("unknown route '{trimmed}'"));
        }
        let selectable: Vec<&CatalogRoute> = catalog
            .iter()
            .filter(|route| route.model == trimmed && route.selectable)
            .collect();
        match selectable.len() {
            1 => Ok(selectable[0]),
            n if n > 1 => Err(format!(
                "model '{trimmed}' is served by multiple providers; qualify it as '<provider>::{trimmed}'"
            )),
            _ => match catalog_route_for_model(catalog, trimmed) {
                Some(route) => Err(format!(
                    "model '{trimmed}' is unavailable: {}",
                    route
                        .availability_note
                        .as_deref()
                        .unwrap_or("provider has no configured credential")
                )),
                None => Err(format!("unknown model '{trimmed}'")),
            },
        }
    }

    /// Picker-path resolution: strict `resolve_catalog_route`, plus one
    /// deterministic fallback — a bare ambiguous id that names the
    /// COMMITTED route resolves to the committed provider's row (the
    /// committed row keeps its bare picker id; re-affirming the current
    /// route is not an ambiguity). Typed commands stay strict.
    fn resolve_picker_route<'a>(
        catalog: &'a [CatalogRoute],
        selection: &str,
        committed: Option<(&str, &str)>,
    ) -> Result<&'a CatalogRoute, String> {
        match resolve_catalog_route(catalog, selection) {
            Err(reason) => {
                let trimmed = selection.trim();
                if let Some((model, provider)) = committed {
                    if trimmed == model && !trimmed.contains(ROUTE_SELECTION_SEP) {
                        if let Some(route) = catalog
                            .iter()
                            .find(|route| route.model == model && route.provider == provider)
                        {
                            return Ok(route);
                        }
                    }
                }
                Err(reason)
            }
            resolved => resolved,
        }
    }

    /// Validate a requested effort against a model's canonical capability
    /// BEFORE any route commit: closed set from `normalize_effort`,
    /// intersected with explicit supported-effort metadata. Unknown capability
    /// permits only the already-committed value — typed unavailable, no
    /// invented mode.
    fn validate_effort_for_route(
        model_id: &str,
        capability: Option<&rightkit_llm::agent_providers::model_metadata::ModelCapability>,
        committed_effort: Option<&str>,
        requested: &str,
    ) -> Result<String, String> {
        let effort = crate::api::normalize_effort(Some(requested.to_string()))?
            .ok_or_else(|| "empty effort selection".to_string())?;
        match supported_effort_ids(capability) {
            Some(ids) if ids.contains(&effort.as_str()) => Ok(effort),
            Some(_) => Err(format!(
                "effort '{effort}' is not supported by model '{model_id}'"
            )),
            None if committed_effort == Some(effort.as_str()) => Ok(effort),
            None => Err(format!(
                "effort '{effort}' is unavailable for '{model_id}': the model's capabilities are unknown"
            )),
        }
    }

    /// Launch-time seed for [`run_loop`].
    struct LoopStart<'a> {
        session_id: &'a str,
        provider: &'a str,
        initial_model: Option<&'a str>,
        initial_workspace: Option<String>,
        initial_effort: Option<String>,
        initial_prompt: Option<String>,
    }

    /// Drive the interactive loop over the given reader/writer pair. Returns
    /// the process exit code (0 on clean `/quit`/EOF). Each turn calls
    /// `run_turn` once; kept generic over the turn function so the loop
    /// control-flow (blank/quit/EOF/prompt) is unit-testable without
    /// spinning up a real engine session.
    async fn run_loop<R, F, Fut>(mut reader: R, start: LoopStart<'_>, mut run_turn: F) -> i32
    where
        R: std::io::BufRead,
        F: FnMut(super::RunArgs) -> Fut,
        Fut: std::future::Future<Output = i32>,
    {
        let LoopStart {
            session_id,
            provider,
            initial_model,
            initial_workspace,
            initial_effort,
            initial_prompt,
        } = start;
        // 13a: the per-turn model is a session field, mutable at turn
        // boundaries only. `/model <id>` queues a switch that takes effect
        // starting the *next* turn (`begin_turn` below); it never rewrites
        // the turn currently in flight and never ends the session.
        let mut turn_model = rightkit_agent_orchestration::coding::SessionTurnModel::new(
            session_id,
            provider,
            initial_model.unwrap_or(""),
        );
        // 1C: the launch directory seeds the selected repository only when
        // the canonical scope resolves; every later switch goes through
        // `/repo` and selects/creates a root session for the new root rather
        // than retargeting this session's history.
        let mut selected_root: Option<String> = match initial_workspace.as_deref() {
            Some(path) => resolve_repository(std::path::Path::new(path))
                .ok()
                .flatten()
                .map(|scope| scope.root)
                .or_else(|| {
                    std::path::Path::new(path)
                        .canonicalize()
                        .ok()
                        .map(|root| root.to_string_lossy().into_owned())
                }),
            None => resolve_launch_repository()
                .ok()
                .flatten()
                .map(|scope| scope.root),
        };
        let mut selected_effort = initial_effort;
        let mut active_session_id = session_id.to_string();
        let mut initial_prompt = initial_prompt;
        loop {
            print!("{}", prompt());
            let _ = std::io::stdout().flush();
            let raw = match initial_prompt
                .take()
                .map(|prompt| Ok(Some(prompt)))
                .unwrap_or_else(|| read_line(&mut reader))
            {
                Ok(line) => line,
                Err(error) => {
                    eprintln!("[interactive] failed to read stdin: {error}");
                    return 1;
                }
            };
            match classify_line(raw.as_deref()) {
                ReplLine::Eof | ReplLine::Quit => return 0,
                ReplLine::Blank => continue,
                ReplLine::ModelSwitch(model_id) => {
                    // `/model <n>` and `/model <model-id>` resolve against the
                    // SAME shared catalog the picker presents (one source of
                    // truth — no second model list here, never a literal
                    // fallback). The non-TTY path awaits the same broker-backed
                    // projection the TUI uses.
                    let committed = {
                        let id = turn_model.model_id().to_string();
                        (!id.is_empty()).then(|| (id, turn_model.provider_id().to_string()))
                    };
                    let configured_model = {
                        let m = super::load_current_merged_config().model.default_model;
                        (!m.trim().is_empty()).then(|| m.trim().to_string())
                    };
                    let catalog = route_catalog(
                        committed.as_ref().map(|(m, p)| (m.as_str(), p.as_str())),
                        configured_model.as_deref().map(|m| (m, provider)),
                    )
                    .await;
                    match resolve_model_selection(&catalog, &model_id) {
                        Ok(selection) => {
                            let switch_provider = if selection.provider.is_empty() {
                                provider.to_string()
                            } else {
                                selection.provider
                            };
                            turn_model.request_switch_for_next_turn(
                                switch_provider,
                                selection.model,
                                rightkit_agent_orchestration::coding::PrefixChangeReason::UserRequestedSwitch,
                            );
                        }
                        Err(reason) => eprintln!("[interactive] {reason}"),
                    }
                    continue;
                }
                ReplLine::RepoSelect(rest) => {
                    if rest.is_empty() {
                        // Non-tty fallback: the picker is a text projection
                        // of roots this session process has selected; a
                        // registry-wide list is the parent-owned host seam.
                        let mut roots = known_repository_roots(
                            &std::collections::HashMap::new(),
                            selected_root.as_deref(),
                        );
                        if let Ok(scopes) =
                            rightkit_memory_client::backend::repository_scope::enrolled_repository_scopes()
                        {
                            roots.extend(scopes.into_iter().map(|scope| scope.root));
                        }
                        roots.sort();
                        roots.dedup();
                        if roots.is_empty() {
                            println!(
                                "No repository selected — `/repo <path>` selects an enrolled root."
                            );
                        } else {
                            println!("{}", format_repo_picker(&roots, selected_root.as_deref()));
                        }
                        continue;
                    }
                    match resolve_repository_selection(&rest) {
                        Ok(scope) => {
                            // A scope change materializes a fresh durable
                            // root session through `ensure_session` BEFORE the
                            // selection moves — admitted history is never
                            // retargeted, and a uuid id cannot collide across
                            // restarts.
                            let next_session =
                                format!("s-{}", rightkit_agent_engine_protocol::uuid_v7());
                            match ensure_root_session(
                                interactive_context_mode(std::env::args()),
                                &next_session,
                                &scope.root,
                                turn_model.provider_id(),
                                {
                                    let id = turn_model.model_id();
                                    (!id.is_empty()).then(|| id.to_string())
                                }
                                .as_deref(),
                                selected_effort.clone(),
                            )
                            .await
                            {
                                Ok(()) => {
                                    selected_root = Some(scope.root.clone());
                                    active_session_id = next_session.clone();
                                    turn_model.rebind_session(next_session);
                                    println!("Repository selected: {}", scope.root);
                                }
                                Err(reason) => eprintln!("[interactive] {reason}"),
                            }
                        }
                        Err(reason) => eprintln!("[interactive] {reason}"),
                    }
                    continue;
                }
                ReplLine::EffortSelect(rest) => {
                    match crate::api::normalize_effort((!rest.is_empty()).then(|| rest.to_string()))
                    {
                        Ok(effort) => {
                            selected_effort = effort.clone();
                            match effort {
                                Some(level) => {
                                    println!("Next turn will use effort {level}.")
                                }
                                None => println!("Usage: /effort <low|medium|high>"),
                            }
                        }
                        Err(reason) => eprintln!("[interactive] {reason}"),
                    }
                    continue;
                }
                ReplLine::Prompt(prompt) => {
                    // A model switch mutates the cached prompt prefix, so the
                    // break has to be attributable — otherwise a cache miss
                    // looks like provider flakiness rather than something we
                    // did on purpose. `begin_turn` returns the event exactly
                    // once, on the first turn after a switch; dropping it here
                    // is why a real `/model` switch produced no record at all.
                    if let Some(change) = turn_model.begin_turn() {
                        tracing::info!(
                            operation = "prefix_change",
                            session_id = %change.session_id,
                            turn_index = change.turn_index,
                            previous_provider_id =
                                change.previous_provider_id.as_deref().unwrap_or(""),
                            previous_model_id =
                                change.previous_model_id.as_deref().unwrap_or(""),
                            new_provider_id = %change.new_provider_id,
                            new_model_id = %change.new_model_id,
                            reason = ?change.reason,
                            "active model changed; prompt-cache prefix will break"
                        );
                    }
                    let model_for_turn = if turn_model.model_id().is_empty() {
                        None
                    } else {
                        Some(turn_model.model_id())
                    };
                    // 1C/1E: the selected repository root is the turn's
                    // workspace — never the process cwd — and the validated
                    // effort rides into the session-create body at admission
                    // (the canonical route authority's committed value).
                    let _ = run_turn(build_turn_args(
                        prompt,
                        &active_session_id,
                        turn_model.provider_id(),
                        model_for_turn,
                        selected_root.clone(),
                        selected_effort.clone(),
                        false,
                    ))
                    .await;
                }
            }
        }
    }

    /// Entry point called from `async_main` when `{app}` is invoked with
    /// no subcommand. A fresh UUID identifies each launch so PID reuse cannot
    /// implicitly resume an unrelated durable session; the
    /// provider id is resolved from the merged config like every other path.
    ///
    /// R6/CR-20: this used to run [`run_loop`] directly against raw stdin --
    /// a strictly sequential, line-oriented REPL with no reachable TUI. It
    /// now hands off to [`crate::tui_host::drive`], the real terminal host
    /// (event loop, composer, scheduler, panic-safe raw mode) built for this
    /// lane. [`run_loop`] and [`classify_line`] are kept and still tested
    /// below: they remain the fallback path for a non-interactive stdin (a
    /// pipe, a CI runner) where raw mode cannot be enabled at all --
    /// `crate::tui_host::terminal_available` reports that case rather than
    /// guessing, and this function falls back to the line-oriented loop when
    /// it does.
    pub(crate) async fn run(launch: super::LaunchCli) -> i32 {
        let requested_session = match resolve_launch_session(&launch).await {
            Ok(session) => session,
            Err(error) => {
                eprintln!("{error}");
                return 2;
            }
        };
        let session_id = requested_session
            .as_ref()
            .map(|row| row.id.clone())
            .unwrap_or_else(fresh_interactive_session_id);

        // KRN defect #1: startup truthfulness applies to the real TUI --
        // the surface that implied readiness it did not have. The
        // non-interactive fallback below (a pipe, CI, this file's own
        // non-tty tests) stays exactly as it was: a raw-mode-free REPL
        // that still resolves a real provider per-turn and, unconfigured,
        // fails at `run_session` like every other entry point rather than
        // running interactive onboarding prompts against a stdin that is
        // not a terminal.
        if !crate::tui_host::terminal_available() {
            let (provider, model) =
                match resolve_launch_route_state(&launch, requested_session.as_ref()).await {
                    Ok(route) => route,
                    Err(error) => {
                        eprintln!("{error}");
                        return 2;
                    }
                };
            let workspace = launch.workspace.clone().or_else(|| {
                requested_session
                    .as_ref()
                    .and_then(|row| row.workspace.clone())
            });
            if workspace
                .as_deref()
                .is_some_and(|path| !std::path::Path::new(path).is_dir())
            {
                eprintln!("launch workspace is not an accessible directory");
                return 2;
            }
            let effort = match resolve_launch_effort(&launch, requested_session.as_ref()) {
                Ok(effort) => effort,
                Err(error) => {
                    eprintln!("{error}");
                    return 2;
                }
            };
            print_banner();
            let stdin = std::io::stdin();
            let reader = stdin.lock();
            return run_loop(
                reader,
                LoopStart {
                    session_id: &session_id,
                    provider: &provider,
                    initial_model: (!model.is_empty()).then_some(model.as_str()),
                    initial_workspace: workspace,
                    initial_effort: effort,
                    initial_prompt: launch.prompt.clone(),
                },
                super::run_session,
            )
            .await;
        }

        // Bare `{app}` used to resolve a provider id (falling all the
        // way through to hardcoded "openrouter") and enter the TUI even
        // with nothing configured. Detect first-run state from the SAME
        // real config + credential store `{app} onboard` and
        // `/onboard` use, and run the SAME onboarding flow (never a second
        // implementation) before constructing any execution host.
        if requested_session.is_none() {
            if let Some(code) = run_onboarding_if_needed().await {
                return code;
            }
        }

        // Resolve the provider AFTER onboarding (so a fresh answer takes
        // effect this run) via the checked resolution that distinguishes
        // an actually configured provider from the unconditional
        // last-resort fallback. Onboarding either configured one or the
        // user explicitly aborted -- either way, entering the TUI with no
        // usable provider is never acceptable.
        // No built-in default model: start on the remembered explicit
        // selection, else with no model at all — the TUI then opens the
        // model picker and refuses turns until one is chosen.
        let (provider, initial_model) =
            match resolve_launch_route_state(&launch, requested_session.as_ref()).await {
                Ok(route) => route,
                Err(error) => {
                    eprintln!("{error}");
                    return 2;
                }
            };
        if launch.prompt.is_some() && initial_model.trim().is_empty() {
            eprintln!("a bare positional prompt requires a selected model; pass --model or configure a default model");
            return 2;
        }
        // DL-014: presence attach for the interactive TUI now submits
        // through the shared `authority()` singleton instead of calling
        // `cli_presence::attach_cli_presence` directly; see
        // `daemon::lifecycle_backend::attach_tui_presence`.
        let layout_flags = interactive_layout_flags(std::env::args_os());
        let host = EngineTurnHost::new(
            session_id.clone(),
            provider.clone(),
            layout_flags.0,
            layout_flags.1,
        );
        host.seed_initial_model(&initial_model);
        if let Some(workspace) = launch.workspace.as_deref().or_else(|| {
            requested_session
                .as_ref()
                .and_then(|row| row.workspace.as_deref())
        }) {
            if let Err(error) = host.set_initial_workspace(workspace) {
                eprintln!("{error}");
                return 2;
            }
        }
        let initial_effort = match resolve_launch_effort(&launch, requested_session.as_ref()) {
            Ok(effort) => effort,
            Err(error) => {
                eprintln!("{error}");
                return 2;
            }
        };
        if !initial_model.trim().is_empty() {
            if let Err(error) = host
                .apply_route_selection_inner(
                    &initial_model,
                    Some(&provider),
                    initial_effort.as_deref(),
                )
                .await
            {
                eprintln!("{error}");
                return 2;
            }
        }
        let attached = crate::daemon::lifecycle_backend::attach_tui_presence("interactive").await;
        if FIRST_RUN_PROVIDER_SCREEN.swap(false, std::sync::atomic::Ordering::SeqCst) {
            // Nothing configured: open the provider setup screen first.
            *host
                .picker_request
                .lock()
                .unwrap_or_else(|e| e.into_inner()) =
                Some(crate::tui_host::PickerRequest::Providers);
        }
        if let Some(mode) = launch.permission_mode() {
            host.store_permission_mode(mode);
        }
        if let Some(root) = launch.workspace.as_deref() {
            crate::custom_commands::set_active_workspace(std::path::Path::new(root));
        }
        // KRN defect #4: start InlineHost's context warm-up as soon as
        // the TUI starts (Membrane mode), not on the first submitted
        // message, so a slow cold worker connect never stalls the
        // first turn's admission.
        host.spawn_context_warmup();
        if let Some(prompt) = launch.prompt.clone() {
            // The turn is already spawned; its JoinHandle is detached on purpose
            // (the TUI drive loop owns turn completion).
            drop(crate::tui_host::TurnHost::spawn_turn(&host, prompt));
        }
        let code = crate::tui_host::drive(host).await;

        if attached {
            crate::daemon::lifecycle_backend::detach_tui_presence("interactive").await;
        }
        code
    }

    /// Run onboarding exactly when it is needed, before any execution host
    /// exists. Returns `Some(exit_code)` when the caller must exit instead
    /// of entering a session (onboarding cancelled or failed and nothing
    /// is configured); `None` means proceed -- either nothing was needed,
    /// or onboarding just completed successfully.
    async fn run_onboarding_if_needed() -> Option<i32> {
        use crate::main_runtime::onboarding::detect_from_config_with_native_auth;
        let config = super::load_current_merged_config();
        let backend = rightkit_llm::agent_providers::OsKeyStore;
        let state = detect_from_config_with_native_auth(&backend, &config).await;
        // A model is never required here — it is picked in the TUI. Only a
        // missing connection (no credential at all) needs setup, and setup
        // is the `/providers` screen itself (no separate prompt flow): the
        // TUI opens it at launch via `PickerRequest::Providers`.
        if first_run_needs_provider_screen(
            state.has_credential,
            super::load_model_selection().is_some(),
        ) {
            FIRST_RUN_PROVIDER_SCREEN.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        None
    }

    /// Set when no provider is configured at launch; consumed once to open
    /// the `/providers` screen instead of the old onboarding prompts.
    static FIRST_RUN_PROVIDER_SCREEN: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    /// First-run rule, pure: no credential and no remembered selection.
    fn first_run_needs_provider_screen(has_credential: bool, has_selection: bool) -> bool {
        !has_credential && !has_selection
    }

    /// A command name in the slash form the shared parser requires. Specs such
    /// as `resume` and `session list` carry bare canonical names.
    pub(super) fn slash_form(name: &str) -> String {
        if name.starts_with('/') {
            name.to_string()
        } else {
            format!("/{name}")
        }
    }

    /// The permission mode a `/ask` `/accept-edits` `/auto` `/yolo` slash
    /// command selects (registry-canonical names).
    fn permission_mode_for_command(name: &str) -> Option<rightkit_agent_policy::PermissionMode> {
        rightkit_agent_policy::PermissionMode::from_user_name(name.strip_prefix('/')?)
    }

    /// Resolve a launch-time `--model` value against the route catalog:
    /// `<provider>/<model>` (the provider part must be a catalog provider
    /// that serves the model), `<provider>::<model>`, or a bare `<model>`
    /// (ambiguity and unavailable routes are typed errors, exactly like
    /// `/model`). Picker numbers are not accepted — they are positional in
    /// an interactive picker, meaningless on a command line.
    fn resolve_launch_selection(
        catalog: &[CatalogRoute],
        requested: &str,
    ) -> Result<ModelSelection, String> {
        let requested = requested.trim();
        if requested.is_empty() {
            return Err("--model needs a value: <provider>/<model> or <model>".to_string());
        }
        if requested.parse::<usize>().is_ok() {
            return Err(format!(
                "--model '{requested}' is not a model id; use <provider>/<model> or <model>"
            ));
        }
        if !requested.contains(ROUTE_SELECTION_SEP) {
            if let Some((provider, model)) = requested.split_once('/') {
                if catalog
                    .iter()
                    .any(|route| route.provider == provider && route.model == model)
                {
                    return resolve_model_selection(
                        catalog,
                        &format!("{provider}{ROUTE_SELECTION_SEP}{model}"),
                    );
                }
            }
        }
        resolve_model_selection(catalog, requested)
    }

    /// Resolve `--model` to `(provider, model)` over the live route catalog.
    pub(crate) async fn resolve_launch_route(requested: &str) -> Result<(String, String), String> {
        let catalog = route_catalog(None, None).await;
        resolve_launch_selection(&catalog, requested)
            .map(|selection| (selection.provider, selection.model))
    }

    /// Startup layout flags are intentionally parsed at the interactive entry
    /// point so the TUI host receives the same explicit authority as its
    /// environment fallback. Unknown arguments remain owned by the outer CLI.
    fn interactive_layout_flags<I>(args: I) -> (bool, bool)
    where
        I: IntoIterator,
        I::Item: AsRef<std::ffi::OsStr>,
    {
        let mut plain = false;
        let mut reduced_motion = false;
        for arg in args {
            match arg.as_ref().to_str() {
                Some("--plain") => plain = true,
                Some("--reduced-motion") => reduced_motion = true,
                _ => {}
            }
        }
        (plain, reduced_motion)
    }

    // ── Shared picker + registry-driven help (CR-24 / R10 CR-26) ────────

    /// One resolved `/model` argument: a model id AND the catalog provider
    /// that owns it. Unknown ids never reach the route authority — a
    /// resolved selection is always a catalog route.
    #[derive(Debug)]
    struct ModelSelection {
        provider: String,
        model: String,
    }

    /// Resolve a `/model <rest>` argument against the SHARED catalog
    /// projection ([`route_catalog`] — the same rows the model picker shows,
    /// in the same order). Accepts a 1-based picker number, an exact
    /// catalog model id, or a provider-qualified `provider::model` key; a
    /// bare model id served by multiple providers is a typed ambiguity, a
    /// typed-unavailable row reports its real reason, and anything else —
    /// including plausible-looking literals — is a typed error, never a
    /// guess at a model the route authority may not know.
    fn resolve_model_selection(
        catalog: &[CatalogRoute],
        rest: &str,
    ) -> Result<ModelSelection, String> {
        let trimmed = rest.trim();
        if let Some((kind, model)) = super::parse_vendor_route(trimmed) {
            return resolve_model_selection(catalog, &format!("{}::{model}", kind.provider_id()));
        }
        if let Ok(n) = trimmed.parse::<usize>() {
            if let Some(route) = n.checked_sub(1).and_then(|i| catalog.get(i)) {
                if !route.selectable {
                    return Err(format!(
                        "route {n} ({}) is unavailable: {}",
                        route.model,
                        route
                            .availability_note
                            .as_deref()
                            .unwrap_or("no configured credential")
                    ));
                }
                return Ok(ModelSelection {
                    provider: route.provider.clone(),
                    model: route.model.clone(),
                });
            }
            return Err(format!(
                "no route {trimmed}; choose 1..={} (see `/model`)",
                catalog.len()
            ));
        }
        // `provider::model` qualifies the exact route row; a bare model id
        // resolves only when exactly ONE selectable route offers it — a
        // same-named model under distinct providers is a typed ambiguity,
        // never a first-match pick.
        if let Some((provider, model)) = trimmed.split_once(ROUTE_SELECTION_SEP) {
            if let Some(route) = catalog
                .iter()
                .find(|route| route.provider == provider && route.model == model)
            {
                if !route.selectable {
                    return Err(format!(
                        "route '{trimmed}' is unavailable: {}",
                        route
                            .availability_note
                            .as_deref()
                            .unwrap_or("no configured credential")
                    ));
                }
                return Ok(ModelSelection {
                    provider: route.provider.clone(),
                    model: route.model.clone(),
                });
            }
            return Err(format!(
                "unknown route '{trimmed}'; use `/model` to pick from the catalog"
            ));
        }
        let selectable: Vec<&CatalogRoute> = catalog
            .iter()
            .filter(|route| route.model == trimmed && route.selectable)
            .collect();
        match selectable.len() {
            1 => {
                return Ok(ModelSelection {
                    provider: selectable[0].provider.clone(),
                    model: selectable[0].model.clone(),
                });
            }
            n if n > 1 => {
                let providers: Vec<&str> = selectable
                    .iter()
                    .map(|route| route.provider.as_str())
                    .collect();
                return Err(format!(
                    "model '{trimmed}' is served by multiple providers ({}); \
                     qualify it as '<provider>::{trimmed}'",
                    providers.join(", ")
                ));
            }
            _ => {}
        }
        if let Some(route) = catalog_route_for_model(catalog, trimmed) {
            return Err(format!(
                "model '{trimmed}' is unavailable: {}",
                route
                    .availability_note
                    .as_deref()
                    .unwrap_or("no configured credential")
            ));
        }
        Err(format!(
            "unknown model '{trimmed}'; use `/model` to pick from the catalog"
        ))
    }

    /// Render the shared catalog as a numbered menu. The ONLY model list
    /// this host ever shows is projected from [`route_catalog`] — typed
    /// unavailable rows are labelled, never hidden or fabricated.
    fn format_route_picker(catalog: &[CatalogRoute]) -> String {
        use std::fmt::Write as _;
        let mut out =
            String::from("Pick a route for the next turn — `/model <n>` or `/model <model-id>`:\n");
        for (i, r) in catalog.iter().enumerate() {
            let state = r
                .availability_note
                .as_deref()
                .map(|note| format!(" (unavailable: {note})"))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "  {}) {} [{}]{state}",
                i + 1,
                r.display_name,
                r.provider,
            );
        }
        if catalog.is_empty() {
            out.push_str("  (no routes are configured for this install)\n");
        }
        out
    }

    /// Render `/help` by iterating COMMAND_REGISTRY filtered to the
    /// interactive surface — CR-24's fourth-surface requirement: any help
    /// text this host renders must be a projection of the one registry, not
    /// a hand-maintained list. `--all` adds the Advanced group.
    fn render_registry_help(include_advanced: bool) -> String {
        use crate::cli::command_registry::{entries_in_group, HelpGroup};
        use std::fmt::Write as _;
        let groups: &[HelpGroup] = if include_advanced {
            &[HelpGroup::Common, HelpGroup::Advanced]
        } else {
            &[HelpGroup::Common]
        };
        let mut out = String::from("Commands (interactive surface):\n");
        for group in groups {
            for entry in entries_in_group(*group).filter(|e| e.surfaces.interactive) {
                let _ = write!(out, "  {:<12} {}", entry.name, entry.summary);
                if !entry.aliases.is_empty() {
                    let _ = write!(out, " (aliases: {})", entry.aliases.join(", "));
                }
                out.push('\n');
            }
        }
        let custom = crate::custom_commands::popup_rows();
        if !custom.is_empty() {
            out.push_str("Custom commands:\n");
            for (name, summary) in custom {
                let _ = writeln!(out, "  /{name:<11} {summary}");
            }
        }
        out
    }

    /// Bridges [`crate::tui_host::TurnHost`] to the concrete engine: owns the
    /// per-turn model-switch bookkeeping ([`rightkit_agent_orchestration::coding::SessionTurnModel`])
    /// behind a mutex so `spawn_turn`/`execute_command` can take `&self` (the
    /// trait's contract -- see its doc for why) while still mutating turn
    /// state, and resolves/gates commands through the one shared registry
    /// (`crate::cli::command_registry`) exactly as `classify_line` used to
    /// hand-match, so a slash command typed into the TUI and one typed into
    /// this file's own test harness resolve identically.
    struct EngineTurnHost {
        session_id: String,
        provider: String,
        plain_output: bool,
        reduced_motion: bool,
        turn_model: std::sync::Mutex<rightkit_agent_orchestration::coding::SessionTurnModel>,
        /// Exclusive-pause handshake shared with the TUI input reader.
        /// `/onboard` requests it and BLOCKS until the reader has exited
        /// poll/read and acknowledged (P1-10), so the flow's line-based
        /// prompts own the tty instead of racing the crossterm reader.
        input_pause_handle: std::sync::Mutex<Option<std::sync::Arc<crate::tui_host::InputPause>>>,
        /// One-shot "a blocking command paused input; the drive loop must drop
        /// queued events when the command returns" (see `take_input_drain`).
        input_drain_pending: std::sync::atomic::AtomicBool,
        /// Lazily-resolved feed connection for explicit ServiceHost sessions.
        /// InlineHost never resolves or reuses a daemon connection.
        daemon: std::sync::Mutex<Option<std::sync::Arc<super::DaemonSessionClient>>>,
        service_context_reason: std::sync::Arc<std::sync::Mutex<Option<String>>>,
        /// Base URL + token captured when the daemon client resolves, so the
        /// P2-5 scrollback pager can call the shared page helper directly.
        stream_endpoint: std::sync::Mutex<Option<(String, String)>>,
        /// The session this host's turns target. Switched by `switch_session`
        /// (P1-5 sideways movement) without touching the session being left.
        active_session_id: std::sync::Mutex<String>,
        turn_cancellation: std::sync::Mutex<Option<std::sync::Arc<super::RunCancellation>>>,
        /// #132: tool/model overrides of the custom command whose expanded
        /// prompt the next matching turn submits.
        pending_command_turn: std::sync::Mutex<Option<crate::custom_commands::TurnOverrides>>,
        /// Live projection deltas drained from the turn's live stream, held
        /// until the durable cursor reaches their anchor seq so they render
        /// after the committed facts they follow (EVT-002).
        live_pending: std::sync::Mutex<Vec<(std::time::Instant, serde_json::Value)>>,
        /// Last-seen event sequence per VIEWED agent (P1-5 live transcript).
        /// Seeded by `fetch_agent_transcript` on Descend, advanced by
        /// `poll_agent_transcript`; a session switch drops the view and a
        /// later Descend re-seeds from scratch.
        viewed_cursors: std::sync::Mutex<std::collections::HashMap<String, i64>>,
        /// Last raw daemon sequence folded into each active-session S:I3
        /// timeline. Kept apart from viewed-agent cursors: one is semantic
        /// session state, one is a display-only agent excerpt.
        timeline_cursors: std::sync::Mutex<std::collections::HashMap<String, i64>>,
        /// Same `--context-mode` resolution `turn_args` uses, cached once so
        /// readiness gating and per-turn args agree on the same mode.
        context_mode: super::ContextMode,
        /// KRN defect #4: real InlineHost startup/context readiness, shared
        /// between the background warm-up kicked off at TUI start and every
        /// turn's pre-admission wait. `drive` renders it via `readiness()`.
        readiness_tx: tokio::sync::watch::Sender<crate::tui_host::HostReadiness>,
        /// 1C: the canonical repository scope this host's turns run against.
        /// `None` is a valid no-repo state (auth/model/help still work); a
        /// scoped turn requires a selection. Set once from the launch cwd
        /// when it resolves, thereafter only by `/repo`.
        selected_repository: std::sync::Mutex<Option<SelectedRepository>>,
        /// Captured launch root for explicitly requested direct mode only.
        launch_workspace: Option<String>,
        /// Why no repository is selected at startup (resolution failure or
        /// unenrolled cwd), so `/repo` can report the real reason.
        repository_notice: std::sync::Mutex<Option<String>>,
        /// Root session id -> selected repository root, for sessions this
        /// host created or observed. Backs resume only until the durable
        /// session row is readable — `session_snapshot` is authoritative.
        session_roots: std::sync::Mutex<std::collections::HashMap<String, String>>,
        /// 1E: validated `/effort` selection awaiting commit at the next
        /// turn's admission (session-create body). Cleared when consumed.
        selected_effort: std::sync::Mutex<Option<String>>,
        /// Route state as last committed by the authority (update_route
        /// response or session-create admission). Pending selections never
        /// write here — `pending_route` carries them until commit.
        committed_route_state: std::sync::Mutex<crate::tui_host::CommittedRoute>,
        /// The provider half of the committed route tuple. `CommittedRoute`
        /// (the shared trait projection) carries only model+effort, so the
        /// provider the authority committed is tracked here — pairing the
        /// committed model with the turn model's provider would silently
        /// mislabel a route switch.
        committed_provider: std::sync::Mutex<Option<String>>,
        /// A route selection made with no durable session bound (no-repo
        /// mode): (model_id, provider_id, effort). Committed at the next
        /// turn's admission; rendered as `pending` until then.
        pending_route: std::sync::Mutex<Option<(String, String, Option<String>)>>,
        /// One-shot picker request from a bare `/model`, `/effort`, or
        /// `/repo` command; the drive loop consumes it via
        /// `take_picker_request`.
        picker_request: std::sync::Mutex<Option<crate::tui_host::PickerRequest>>,
        /// Command output queued for the renderer (`poll_timeline_events`
        /// drains it into `DaemonEvent::System` transcript entries). This is
        /// the render-owner path for command/help/picker text: nothing is
        /// printed around raw mode while the viewport is active. Bounded —
        /// a flood of stale output is dropped rather than grown forever.
        host_events: std::sync::Arc<
            std::sync::Mutex<
                std::collections::VecDeque<rightkit_terminal_ui::transcript::timeline::DaemonEvent>,
            >,
        >,
        /// Vendor-CLI conversation ids captured from each turn's
        /// `SessionStarted` event, keyed `"{provider_id}:{session_id}"` so a
        /// later turn on the same session resumes with `--resume` (Claude)
        /// or `--conversation` (agy). In-memory only — a restarted TUI opens
        /// a fresh vendor conversation; the RightKit transcript remains the
        /// durable continuity surface.
        vendor_conversations:
            std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
        /// The live session permission mode: the launch flag's mode, then
        /// `/ask` `/accept-edits` `/auto` `/yolo` and Shift+Tab. Every turn's
        /// `RunArgs.permission_mode` is read from here; the engine adopts it
        /// at the start of the turn. Workspace trust is evaluated separately,
        /// so `Bypass` never overrides a `Locked` workspace.
        permission_mode: std::sync::Mutex<rightkit_agent_policy::PermissionMode>,
        /// Cached goal projection for the footer; refreshed through the canonical session API.
        goal_chip: std::sync::Mutex<Option<String>>,
        followup_prompt: std::sync::Mutex<Option<String>>,
    }

    impl EngineTurnHost {
        /// A070: the live inline background shell `id` (the PTY process the
        /// TUI attaches to), searched across this host's live sessions.
        fn live_pty_shell(
            &self,
            id: &str,
        ) -> Result<
            std::sync::Arc<rightkit_agent_coding_tools::background_shell::BgShell>,
            String,
        > {
            let Some(super::SessionExecutionHost::Inline(host)) =
                super::SessionExecutionHost::select_if_ready(self.context_mode)
            else {
                return Err("Interactive PTY control is unavailable for this host".into());
            };
            let sessions = host
                .state
                .sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            for live in sessions.values() {
                let shells = live
                    .background_shells
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if let Some(shell) = shells.get(id) {
                    return Ok(shell.clone());
                }
            }
            Err(format!("No live shell {id} is attached to this host"))
        }

        /// Map the view's placeholder `"local"` session to the active turn's
        /// real session (as approvals do); any other id passes through.
        fn resolve_turn_session(&self, session_id: &str) -> String {
            if session_id != "local" {
                return session_id.to_string();
            }
            let control = self
                .turn_cancellation
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            control
                .and_then(|control| control.host.get().map(|(_, id)| id.clone()))
                .unwrap_or_else(|| session_id.to_string())
        }

        /// The session the TUI is currently viewing (rewind target).
        fn rewind_session_id(&self) -> String {
            let id = self
                .active_session_id
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if id.is_empty() {
                self.session_id.clone()
            } else {
                id.clone()
            }
        }

        fn new(
            session_id: String,
            provider: String,
            plain_output: bool,
            reduced_motion: bool,
        ) -> Self {
            let turn_model = rightkit_agent_orchestration::coding::SessionTurnModel::new(
                session_id.clone(),
                provider.clone(),
                "",
            );
            let (readiness_tx, _) =
                tokio::sync::watch::channel(crate::tui_host::HostReadiness::StartingContext);
            // 1C: launch-cwd scope seeds the selection only when the
            // canonical repository scope resolves; otherwise the host starts
            // with no repository and reports why through `/repo`.
            let (selected_repository, repository_notice) = match resolve_launch_repository() {
                Ok(scope) => (scope, None),
                Err(reason) => (None, Some(reason)),
            };
            let mut session_roots = std::collections::HashMap::new();
            if let Some(scope) = selected_repository.as_ref() {
                session_roots.insert(session_id.clone(), scope.root.clone());
            }
            Self {
                session_id,
                provider,
                plain_output,
                reduced_motion,
                turn_model: std::sync::Mutex::new(turn_model),
                input_pause_handle: std::sync::Mutex::new(None),
                input_drain_pending: std::sync::atomic::AtomicBool::new(false),
                daemon: std::sync::Mutex::new(None),
                service_context_reason: std::sync::Arc::new(std::sync::Mutex::new(None)),
                stream_endpoint: std::sync::Mutex::new(None),
                active_session_id: std::sync::Mutex::new(String::new()),
                turn_cancellation: std::sync::Mutex::new(None),
                live_pending: std::sync::Mutex::new(Vec::new()),
                viewed_cursors: std::sync::Mutex::new(std::collections::HashMap::new()),
                timeline_cursors: std::sync::Mutex::new(std::collections::HashMap::new()),
                context_mode: interactive_context_mode(std::env::args()),
                readiness_tx,
                selected_repository: std::sync::Mutex::new(selected_repository),
                launch_workspace: std::env::current_dir()
                    .ok()
                    .and_then(|root| root.canonicalize().ok())
                    .map(|root| root.to_string_lossy().into_owned()),
                repository_notice: std::sync::Mutex::new(repository_notice),
                session_roots: std::sync::Mutex::new(session_roots),
                selected_effort: std::sync::Mutex::new(None),
                pending_command_turn: std::sync::Mutex::new(None),
                committed_route_state: std::sync::Mutex::new(
                    crate::tui_host::CommittedRoute::default(),
                ),
                committed_provider: std::sync::Mutex::new(None),
                pending_route: std::sync::Mutex::new(None),
                // D14: start in the launch directory. Only a launch outside any
                // project (home folder or a filesystem root) opens project/chat
                // Home first; all projects stay one Ctrl+B / `/projects` away.
                picker_request: std::sync::Mutex::new(
                    launch_needs_project_home().then_some(crate::tui_host::PickerRequest::Home),
                ),
                host_events: std::sync::Arc::new(std::sync::Mutex::new(
                    std::collections::VecDeque::new(),
                )),
                vendor_conversations: std::sync::Arc::new(std::sync::Mutex::new(
                    std::collections::HashMap::new(),
                )),
                permission_mode: std::sync::Mutex::new(rightkit_agent_policy::PermissionMode::Ask),
                goal_chip: std::sync::Mutex::new(None),
                followup_prompt: std::sync::Mutex::new(None),
            }
        }

        fn current_permission_mode(&self) -> rightkit_agent_policy::PermissionMode {
            *self
                .permission_mode
                .lock()
                .unwrap_or_else(|e| e.into_inner())
        }

        fn store_permission_mode(&self, mode: rightkit_agent_policy::PermissionMode) {
            *self
                .permission_mode
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = mode;
        }

        /// Queue a renderer-owned transcript event. Bounded: once the queue
        /// hits the cap the oldest entries are dropped — they are display
        /// copies of command output, and a wedged poll must not let this
        /// grow without bound.
        fn push_host_event(&self, event: rightkit_terminal_ui::transcript::timeline::DaemonEvent) {
            const HOST_EVENT_CAP: usize = 512;
            let mut queue = self.host_events.lock().unwrap();
            if queue.len() >= HOST_EVENT_CAP {
                queue.pop_front();
            }
            queue.push_back(event);
        }

        /// Queue free-form command/help/picker text for the transcript. The
        /// text travels inside `System` events whose `message` is the
        /// display payload; the renderer projects them verbatim.
        /// Start on an explicit model (the remembered selection), or with none:
        /// then the model picker opens at launch and turns wait for a choice.
        fn seed_initial_model(&self, model: &str) {
            let model = model.trim();
            if model.is_empty() {
                *self
                    .picker_request
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) =
                    Some(crate::tui_host::PickerRequest::Model);
                return;
            }
            *self.turn_model.lock().unwrap_or_else(|e| e.into_inner()) =
                rightkit_agent_orchestration::coding::SessionTurnModel::new(
                    self.session_id.clone(),
                    self.provider.clone(),
                    model,
                );
            {
                let mut committed = self
                    .committed_route_state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                committed.model = Some(model.to_string());
                // The footer reads `model · effort`; start on the remembered
                // or configured effort instead of leaving it blank.
                if committed.effort.is_none() {
                    committed.effort = super::launch_effort();
                }
            }
            *self
                .committed_provider
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(self.provider.clone());
        }

        /// The folder the active session's turns run in: the selected
        /// canonical repository, else (D18: unscoped session, context
        /// unavailable) the root recorded for the active session when it is
        /// a real project folder, else the launch folder — never home or a
        /// filesystem root.
        fn active_workspace_root(&self) -> Option<String> {
            let selected = self
                .selected_repository
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .map(|scope| scope.root.clone());
            if selected.is_some() {
                return selected;
            }
            let active = {
                let id = self
                    .active_session_id
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if id.is_empty() {
                    self.session_id.clone()
                } else {
                    id.clone()
                }
            };
            let recorded = self
                .session_roots
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&active)
                .cloned()
                .filter(|root| is_workspace_candidate(std::path::Path::new(root)));
            turn_workspace(self.context_mode, recorded, self.launch_workspace.clone())
        }

        fn set_initial_workspace(&self, workspace: &str) -> Result<(), String> {
            let path = std::path::PathBuf::from(workspace);
            let scope = resolve_repository(&path)?
                .ok_or_else(|| format!("workspace {workspace} cannot be opened as a repository"))?;
            *self
                .selected_repository
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(scope.clone());
            self.session_roots
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .insert(self.session_id.clone(), scope.root);
            Ok(())
        }

        /// True when no model has been chosen yet (nothing remembered, nothing
        /// picked this session, nothing pending).
        fn no_model_selected(&self) -> bool {
            let pending = self
                .pending_route
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .is_some_and(|(model, _, _)| !model.is_empty());
            !pending
                && self
                    .turn_model
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .model_id()
                    .is_empty()
        }

        fn enqueue_text(&self, text: &str) {
            for line in text.lines() {
                self.push_host_event(
                    rightkit_terminal_ui::transcript::timeline::DaemonEvent::System {
                        text: line.to_string(),
                    },
                );
            }
        }

        /// `/repo` handler. Bare lists enrolled repository roots (picker
        /// projection — the donor `PickerRequest::Repository` list replaces
        /// the rendering when the parent trait lands, not the selection
        /// contract); `<n>` selects a listed root; `<path>` canonicalizes
        /// through the registry resolver. A scope change first materializes
        /// a FRESH durable root session for the new repository via the host
        /// `ensure_session` authority — prior session history and tool
        /// authority are never retargeted — and is rejected while work is
        /// admitted or an approval is pending.
        async fn execute_repo_command(&self, rest: &str) {
            let arg = rest.trim();
            let current_root = self
                .selected_repository
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .map(|scope| scope.root.clone());
            if arg.is_empty() {
                // Bare `/repo` asks the surface to open the real list picker
                // over `repository_catalog()`; the renderer handles the
                // empty-catalog notice, so no text menu is printed while
                // the viewport is active.
                *self
                    .picker_request
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) =
                    Some(crate::tui_host::PickerRequest::Repository);
                return;
            }
            // Reject scope change while work is admitted or an approval is
            // pending: the live turn's cancellation control is still unsettled.
            let busy = self
                .turn_cancellation
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .map(|control| control.require_settled().is_err())
                .unwrap_or(false);
            if busy {
                self.enqueue_text(
                    "Cannot switch repository while a turn or approval is in progress.",
                );
                return;
            }
            // `<n>` selects an enrolled root from the picker projection.
            let arg_owned;
            let arg = if let Ok(index) = arg.parse::<usize>() {
                let roots: Vec<_> = crate::tui_host::TurnHost::repository_catalog(self)
                    .into_iter()
                    .map(|(root, _)| root)
                    .collect();
                match index.checked_sub(1).and_then(|i| roots.get(i).cloned()) {
                    Some(root) => {
                        arg_owned = root;
                        arg_owned.as_str()
                    }
                    None => {
                        self.enqueue_text(&format!("No repository #{index} in the list."));
                        return;
                    }
                }
            } else {
                arg
            };
            match self.select_repository_root(arg).await {
                Ok(scope) => {
                    if current_root.as_deref() == Some(scope.root.as_str()) {
                        self.enqueue_text(&format!("Repository already selected: {}", scope.root));
                    } else {
                        self.enqueue_text(&format!(
                            "Repository selected: {} (new session for this root)",
                            scope.root
                        ));
                    }
                }
                Err(reason) => self.enqueue_text(&reason),
            }
        }

        /// Shared repository-selection semantics for `/repo <path>` now and
        /// the parent-added `TurnHost::select_repository` callback later:
        /// resolve the canonical scope, materialize a fresh durable root
        /// session under it through `ensure_session`, and only then point
        /// this host at the new session. On failure nothing retargets.
        async fn select_repository_root(
            &self,
            path_arg: &str,
        ) -> Result<SelectedRepository, String> {
            let scope = resolve_repository_selection(path_arg)?;
            let current_root = self
                .selected_repository
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .map(|scope| scope.root.clone());
            if current_root.as_deref() == Some(scope.root.as_str()) {
                return Ok(scope);
            }
            self.start_session_in_scope(scope)
                .await
                .map(|(scope, _)| scope)
        }

        /// Create a fresh durable root session under `scope` and retarget
        /// this host at it; shared by `/repo` and `/new`.
        async fn start_session_in_scope(
            &self,
            scope: SelectedRepository,
        ) -> Result<(SelectedRepository, String), String> {
            let id = self
                .start_session_at_root(scope.root.clone(), Some(scope.clone()))
                .await?;
            Ok((scope, id))
        }

        /// Start a fresh durable root session at `root`. `scope` is the
        /// enrolled repository when one exists; `None` (D18: context
        /// unavailable) starts an unscoped session in that folder and never
        /// invents a repository identity locally.
        async fn start_session_at_root(
            &self,
            root: String,
            scope: Option<SelectedRepository>,
        ) -> Result<String, String> {
            // Fresh durable root session — a uuid_v7 id survives process
            // restarts without colliding with a counter that reset.
            let next_session = format!("s-{}", rightkit_agent_engine_protocol::uuid_v7());
            // A pending no-repo route selection must ride THIS session's
            // create body through the canonical `ensure_session` authority —
            // the durable row is written now, so the pending tuple is merged
            // in here rather than left for spawn-time consumption, where it
            // could diverge from the row that was just persisted.
            let (provider, model, effort) = self.next_repository_route();
            ensure_root_session(
                self.context_mode,
                &next_session,
                &root,
                &provider,
                model.as_deref(),
                effort.clone(),
            )
            .await?;
            // Local state commits only after the durable row exists with
            // the merged route — pending state is consumed, and the turn
            // model is rebuilt to attribute the route the row carries.
            *self
                .selected_repository
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = scope;
            *self
                .active_session_id
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = next_session.clone();
            self.session_roots
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(next_session.clone(), root.clone());
            *self.turn_model.lock().unwrap_or_else(|e| e.into_inner()) =
                rightkit_agent_orchestration::coding::SessionTurnModel::new(
                    next_session.clone(),
                    provider.clone(),
                    model.clone().unwrap_or_default(),
                );
            {
                let mut committed = self
                    .committed_route_state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                committed.model = model;
                if effort.is_some() {
                    committed.effort = effort;
                }
            }
            *self
                .committed_provider
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(provider);
            *self.pending_route.lock().unwrap_or_else(|e| e.into_inner()) = None;
            Ok(next_session)
        }

        /// The session id every session-scoped operation targets: the active
        /// session when set, else this host's launch session.
        fn current_session_id(&self) -> String {
            let id = self
                .active_session_id
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if id.is_empty() {
                self.session_id.clone()
            } else {
                id
            }
        }

        /// The provider a catalog model id belongs to, per the shared
        /// catalog projection. `Some` only when exactly ONE selectable
        /// route offers the model — a same-named model under distinct
        /// providers is `None` (callers turn that into "unknown", never a
        /// guessed provider).
        fn provider_for_model(catalog: &[CatalogRoute], model_id: &str) -> Option<String> {
            unique_route_provider(catalog, model_id)
        }

        /// The committed route's provider, exactly as the authority
        /// committed it — never the turn model's provider, which can still
        /// hold the PREVIOUS route after a switch.
        fn committed_provider_id(&self) -> String {
            self.committed_provider
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .or_else(|| {
                    let provider = self
                        .turn_model
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .provider_id()
                        .to_string();
                    (!provider.is_empty()).then_some(provider)
                })
                .unwrap_or_else(|| self.provider.clone())
        }

        /// A new repository inherits the committed next-turn choice, including
        /// a model switch not yet consumed by prompt admission.
        fn next_repository_route(&self) -> (String, Option<String>, Option<String>) {
            let (pending_model, pending_provider, pending_effort) = self
                .pending_route
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .unwrap_or_default();
            let committed = crate::tui_host::TurnHost::committed_route(self);
            let provider = if pending_provider.is_empty() {
                self.committed_provider_id()
            } else {
                pending_provider
            };
            let model = if pending_model.is_empty() {
                committed.model
            } else {
                Some(pending_model)
            };
            (provider, model, pending_effort.or(committed.effort))
        }

        /// The shared route catalog as this session sees it: live broker
        /// status plus this host's authority pins (committed route state
        /// with the turn model's bound route as fallback, and the
        /// session-configured provider/model default). Every model surface
        /// — picker, `/model <n>`, `/model <id>`, select_* — projects from
        /// this one build, so numbering and resolution never disagree.
        async fn current_route_catalog(&self) -> Vec<CatalogRoute> {
            let (committed_model, committed_provider) = {
                let committed = crate::tui_host::TurnHost::committed_route(self);
                (committed.model, self.committed_provider_id())
            };
            let configured_model = {
                let config = super::load_current_merged_config();
                let m = config.model.default_model.trim().to_string();
                (!m.is_empty()).then_some(m)
            };
            route_catalog(
                committed_model
                    .as_deref()
                    .map(|m| (m, committed_provider.as_str())),
                configured_model
                    .as_deref()
                    .map(|m| (m, self.provider.as_str())),
            )
            .await
        }

        /// Apply one model+effort pair through the canonical route authority
        /// — a single `update_route` (session PATCH), never two sequential
        /// commits. With no selected repository there is no durable session
        /// to patch: the selection is held as an explicit pending route that
        /// the next admitted turn's session-create body commits, and this
        /// returns Ok with the pending marker set — never a false "set".
        async fn apply_route_selection(
            &self,
            model_id: &str,
            provider_id: Option<&str>,
            effort_id: Option<&str>,
        ) -> Result<(), String> {
            self.apply_route_selection_inner(model_id, provider_id, effort_id)
                .await?;
            // Every selection path (picker, `/model <id>`, route) lands here:
            // remember the explicit choice for the next launch. A failed
            // write is surfaced but does not undo the live selection.
            let provider = provider_id
                .filter(|p| !p.is_empty())
                .unwrap_or(&self.provider);
            let committed_effort = crate::tui_host::TurnHost::committed_route(self).effort;
            if let Err(error) = super::save_model_selection_with_effort(
                provider,
                model_id,
                effort_id.or(committed_effort.as_deref()),
            ) {
                self.enqueue_text(&format!("Could not remember model selection: {error}"));
            }
            Ok(())
        }

        async fn apply_route_selection_inner(
            &self,
            model_id: &str,
            provider_id: Option<&str>,
            effort_id: Option<&str>,
        ) -> Result<(), String> {
            if let Some(provider) = provider_id.filter(|provider| {
                rightkit_agents::driver_host::vendor_cli::VendorCli::from_provider_id(provider)
                    .is_some()
            }) {
                if self
                    .turn_cancellation
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .as_ref()
                    .is_some_and(|control| control.require_settled().is_err())
                {
                    return Err(
                        "Cannot switch provider while a turn or approval is in progress.".into(),
                    );
                }
                // The vendor launch consumes this route, not ProviderLlmClient.
                // Keep the explicit TUI selection in its existing durable model
                // store; never manufacture a native API route for a vendor CLI.
                let mut committed = self
                    .committed_route_state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                committed.model = Some(model_id.to_string());
                if let Some(effort) = effort_id {
                    committed.effort = Some(effort.to_string());
                }
                drop(committed);
                *self
                    .committed_provider
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(provider.to_string());
                self.turn_model
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .request_switch_for_next_turn(
                    provider.to_string(),
                    model_id.to_string(),
                    rightkit_agent_orchestration::coding::PrefixChangeReason::UserRequestedSwitch,
                );
                *self.pending_route.lock().unwrap_or_else(|e| e.into_inner()) = None;
                return Ok(());
            }
            let has_repo = self
                .selected_repository
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some();
            if !has_repo {
                let mut pending = self.pending_route.lock().unwrap_or_else(|e| e.into_inner());
                *pending = Some(merge_pending_route(
                    pending.take(),
                    Some(model_id.to_string()),
                    provider_id.filter(|p| !p.is_empty()).map(str::to_string),
                    effort_id.map(str::to_string),
                ));
                return Ok(());
            }
            let session_id = self.current_session_id();
            let workspace_root = self
                .selected_repository
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .map(|scope| scope.root.clone())
                .ok_or_else(|| "no repository selected".to_string())?;
            let host = super::SessionExecutionHost::select(self.context_mode).await?;
            // Ensure the canonical root session exists before patching it —
            // a fresh TUI session has no durable row until first admission.
            ensure_root_session(
                self.context_mode,
                &session_id,
                &workspace_root,
                provider_id.unwrap_or(""),
                Some(model_id),
                effort_id.map(str::to_string),
            )
            .await?;
            let snapshot = host
                .update_route(
                    &session_id,
                    model_id,
                    provider_id.filter(|p| !p.is_empty()),
                    effort_id,
                )
                .await?;
            // Local state moves only AFTER the committed response landed.
            {
                let mut committed = self
                    .committed_route_state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                committed.model = Some(model_id.to_string());
                if let Some(effort) = effort_id {
                    committed.effort = Some(effort.to_string());
                } else if let Some(effort) = snapshot
                    .get("effort")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
                {
                    committed.effort = Some(effort);
                }
            }
            // The committed provider half: the explicit provider when one
            // was sent, else the committed durable row's `providerId`. A
            // provider-free (effort-only) update keeps the existing pair.
            let committed_provider = provider_id
                .filter(|p| !p.is_empty())
                .map(str::to_string)
                .or_else(|| {
                    snapshot
                        .get("providerId")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                });
            if committed_provider.is_some() {
                *self
                    .committed_provider
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = committed_provider;
            }
            let mut model = self.turn_model.lock().unwrap_or_else(|e| e.into_inner());
            model.request_switch_for_next_turn(
                provider_id.unwrap_or(&self.provider).to_string(),
                model_id.to_string(),
                rightkit_agent_orchestration::coding::PrefixChangeReason::UserRequestedSwitch,
            );
            *self.pending_route.lock().unwrap_or_else(|e| e.into_inner()) = None;
            Ok(())
        }

        /// Kick off InlineHost's context warm-up as its own background task
        /// and return immediately -- called once, right after the TUI
        /// starts (KRN defect #4), so the slow native connect (which can
        /// block for minutes on a cold worker) runs off the first turn's
        /// critical path. A no-op under explicit ServiceHost selection,
        /// which owns no such InlineHost lifecycle.
        fn spawn_context_warmup(&self) {
            if crate::main_runtime::service_host_explicitly_selected() {
                let _ = self
                    .readiness_tx
                    .send_replace(crate::tui_host::HostReadiness::Ready);
                let cached = self.service_context_reason.clone();
                tokio::spawn(async move {
                    if let Some((base_url, token)) =
                        crate::cli_presence::resolve_daemon_or_autostart().await
                    {
                        let client = super::DaemonSessionClient::new(base_url, token);
                        if let Ok(Some(reason)) = client.context_degraded_reason().await {
                            if let Ok(mut slot) = cached.lock() {
                                *slot = Some(reason);
                            }
                        }
                    }
                });
                return;
            }
            let readiness_tx = self.readiness_tx.clone();
            let context_mode = self.context_mode;
            tokio::spawn(async move {
                let _ = ensure_inline_context_ready(&readiness_tx, context_mode).await;
            });
        }

        /// Resolve the feed connection only after explicit ServiceHost selection.
        /// Every feed consumer passes this gate, including startup navigation,
        /// maintenance, commands and history. Hold no lock across an await.
        async fn daemon_client(
            &self,
        ) -> Result<std::sync::Arc<super::DaemonSessionClient>, String> {
            if !crate::main_runtime::service_host_explicitly_selected() {
                return Err("daemon feed unavailable for InlineHost".to_string());
            }
            {
                let slot = self.daemon.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(client) = slot.as_ref() {
                    return Ok(client.clone());
                }
            }
            let (base_url, token) = crate::cli_presence::resolve_daemon_or_autostart()
                .await
                .ok_or_else(|| "no running daemon found and automatic start failed".to_string())?;
            let client = std::sync::Arc::new(super::DaemonSessionClient::new(
                base_url.clone(),
                token.clone(),
            ));
            if let Ok(mut endpoint) = self.stream_endpoint.lock() {
                *endpoint = Some((base_url, token));
            }
            if let Ok(mut slot) = self.daemon.lock() {
                *slot = Some(client.clone());
            }
            Ok(client)
        }

        async fn execute_github_sign_in(&self, open_browser: bool) {
            let flow = match tokio::task::spawn_blocking(crate::github_signin::begin_device_sign_in)
                .await
            {
                Ok(Ok(flow)) => flow,
                Ok(Err(error)) => {
                    self.enqueue_text(&format!("GitHub sign-in failed: {error}"));
                    return;
                }
                Err(error) => {
                    self.enqueue_text(&format!("GitHub sign-in failed: {error}"));
                    return;
                }
            };
            let device = flow.authorization();
            let verification_uri = device.verification_uri.clone();
            let user_code = device.user_code.clone();
            self.enqueue_text(&format!(
                "GitHub sign-in: open {verification_uri} & enter code {user_code}. Use `/login github --open` to open browser; approval polling continues in background."
            ));
            if open_browser {
                if let Err(error) = crate::github_signin::open_verification_url(&verification_uri) {
                    self.enqueue_text(&error);
                }
            }
            let completion = tokio::task::spawn_blocking(move || {
                flow.complete(&crate::github_signin::DeviceWait)
            });
            let events = std::sync::Arc::clone(&self.host_events);
            tokio::spawn(async move {
                let message = match completion.await {
                    Ok(Ok(())) => {
                        "GitHub sign-in saved. It will apply to newly started sessions.".to_string()
                    }
                    Ok(Err(error)) => format!("GitHub sign-in failed: {error}"),
                    Err(error) => format!("GitHub sign-in failed: {error}"),
                };
                events
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push_back(
                        rightkit_terminal_ui::transcript::timeline::DaemonEvent::System {
                            text: message,
                        },
                    );
            });
        }
    }

    fn elapsed_since_epoch_ns(started_at_ns: u128) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let seconds = now.saturating_sub(started_at_ns) / 1_000_000_000;
        if seconds >= 60 {
            format!("{}m {:02}s", seconds / 60, seconds % 60)
        } else {
            format!("{seconds}s")
        }
    }

    fn elapsed_since_created(created_at: &str) -> String {
        created_at
            .strip_prefix("unix:")
            .and_then(|value| value.parse::<u64>().ok())
            .map(|start| {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |duration| duration.as_secs());
                let seconds = now.saturating_sub(start);
                if seconds >= 60 {
                    format!("{}m {:02}s", seconds / 60, seconds % 60)
                } else {
                    format!("{seconds}s")
                }
            })
            .unwrap_or_else(|| "active".into())
    }

    impl crate::tui_host::TurnHost for EngineTurnHost {
        async fn running_work(
            &self,
            session_id: &str,
        ) -> Vec<rightkit_terminal_ui::bots_panel::RunningProjection> {
            use rightkit_terminal_ui::bots_panel::RunningProjection;
            if super::service_host_explicitly_selected() {
                let Ok(client) = self.daemon_client().await else {
                    return Vec::new();
                };
                let processes = client.list_execution_processes().await.unwrap_or_default();
                let tasks = client
                    .get_session_tasks(session_id)
                    .await
                    .unwrap_or_default();
                let mut rows = Vec::new();
                for process in processes.into_iter().filter(|row| {
                    row.get("sessionId").and_then(serde_json::Value::as_str) == Some(session_id)
                        && row.get("status").and_then(serde_json::Value::as_str) == Some("running")
                }) {
                    let id = process
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    rows.push(RunningProjection::Shell {
                        id,
                        command: process
                            .get("command")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("shell")
                            .to_string(),
                        elapsed: process
                            .get("startedAtNs")
                            .and_then(serde_json::Value::as_u64)
                            .map(|n| elapsed_since_epoch_ns(n as u128))
                            .unwrap_or_else(|| "active".into()),
                        last_output: process
                            .get("lastOutputLine")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        port: None,
                        status: process
                            .get("statusLabel")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("running")
                            .to_string(),
                    });
                }
                for task in tasks.into_iter().filter(|task| {
                    task.get("state").and_then(serde_json::Value::as_str) == Some("inProgress")
                }) {
                    rows.push(RunningProjection::Task {
                        id: task
                            .get("id")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        name: task
                            .get("title")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("task")
                            .to_string(),
                        step: task
                            .get("category")
                            .and_then(serde_json::Value::as_str)
                            .filter(|value| !value.is_empty())
                            .unwrap_or("in progress")
                            .to_string(),
                        elapsed: task
                            .get("updatedAt")
                            .and_then(serde_json::Value::as_str)
                            .map(elapsed_since_created)
                            .unwrap_or_else(|| "active".into()),
                        status: "running".into(),
                    });
                }
                return rows;
            }
            let Some(super::SessionExecutionHost::Inline(host)) =
                super::SessionExecutionHost::select_if_ready(self.context_mode)
            else {
                return Vec::new();
            };
            let Some(live) = host.state.get_session(session_id) else {
                return Vec::new();
            };
            let mut rows = Vec::new();
            let shells = live
                .background_shells
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            for (id, shell) in shells.iter() {
                if !matches!(
                    shell.status(),
                    rightkit_agent_coding_tools::background_shell::BgStatus::Running
                ) {
                    continue;
                }
                let output = shell.output_snapshot();
                let last_output = output
                    .lines()
                    .rev()
                    .find(|line| !line.trim().is_empty())
                    .unwrap_or_default()
                    .chars()
                    .rev()
                    .take(100)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect();
                // Background shell metadata does not currently expose an
                // observed listening socket; do not infer ports from command text.
                let port = None;
                let elapsed = elapsed_since_epoch_ns(shell.started_at_ns());
                let status = match shell.status() {
                    rightkit_agent_coding_tools::background_shell::BgStatus::Running => {
                        "running".to_string()
                    }
                    rightkit_agent_coding_tools::background_shell::BgStatus::Exited(
                        code,
                    ) => {
                        format!("exited {code}")
                    }
                    rightkit_agent_coding_tools::background_shell::BgStatus::Killed => {
                        "stopped".to_string()
                    }
                    rightkit_agent_coding_tools::background_shell::BgStatus::SpawnFailed(
                        error,
                    ) => {
                        format!("failed: {error}")
                    }
                    rightkit_agent_coding_tools::background_shell::BgStatus::Unknown => {
                        "unknown".to_string()
                    }
                };
                rows.push(RunningProjection::Shell {
                    id: id.clone(),
                    command: shell.command.clone(),
                    elapsed,
                    last_output,
                    port,
                    status,
                });
            }
            drop(shells);
            let tasks = live.tasks.lock().unwrap_or_else(|e| e.into_inner());
            for task in tasks
                .iter()
                .filter(|task| task.state == rightkit_agent_engine_protocol::TaskState::InProgress)
            {
                rows.push(RunningProjection::Task {
                    id: task.id.clone(),
                    name: task.title.clone(),
                    step: if task.category.is_empty() {
                        "in progress".into()
                    } else {
                        task.category.clone()
                    },
                    elapsed: elapsed_since_created(&task.updated_at),
                    status: "running".into(),
                });
            }
            rows
        }

        async fn stop_running_work(
            &self,
            session_id: &str,
            kind: &str,
            id: &str,
        ) -> Result<String, String> {
            if super::service_host_explicitly_selected() {
                let client = self.daemon_client().await?;
                return match kind {
                    "shell" => {
                        client.stop_execution_process(id).await?;
                        Ok(format!("Stopped shell {id}."))
                    }
                    "task" => {
                        client.cancel_session_task(session_id, id).await?;
                        Ok(format!("Cancelled task {id}."))
                    }
                    other => Err(format!("Unsupported running work kind: {other}")),
                };
            }
            let Some(super::SessionExecutionHost::Inline(host)) =
                super::SessionExecutionHost::select_if_ready(self.context_mode)
            else {
                return Err("Live process control is unavailable for this host".into());
            };
            let sessions = host
                .state
                .sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let Some(live) = sessions.get(session_id) else {
                return Err(format!("Session {session_id} is not live"));
            };
            if kind == "task" {
                let mut tasks = live.tasks.lock().unwrap_or_else(|e| e.into_inner());
                let cancelled =
                    rightkit_agent_orchestration::coding::task_list_projection::mark_cancelled(
                        &mut tasks,
                        id,
                        format!(
                            "unix:{}",
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map_or(0, |d| d.as_secs())
                        ),
                    )
                    .map_err(|error| error.to_string())?;
                let task_snapshot = tasks.clone();
                drop(tasks);
                let seq = live.conduit.next_seq();
                live.conduit.publish(
                    rightkit_agent_engine_protocol::ProtocolEvent::TaskGraphUpdated {
                        session_id: session_id.to_string(),
                        seq,
                        created_at: format!(
                            "unix:{}",
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map_or(0, |d| d.as_secs())
                        ),
                        summary: format!("Task {} cancelled by user from TUI", cancelled.id),
                        tasks: task_snapshot,
                    },
                );
                let seq = live.conduit.next_seq();
                live.conduit.publish(
                    rightkit_agent_engine_protocol::ProtocolEvent::UserMessageSubmitted {
                        session_id: session_id.to_string(),
                        seq,
                        created_at: format!(
                            "unix:{}",
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map_or(0, |d| d.as_secs())
                        ),
                        text: format!(
                            "Cancelled task {} from the TUI Running panel.",
                            cancelled.id
                        ),
                        nonce: None,
                        trust: None,
                        attachments: Vec::new(),
                    },
                );
                return Ok(format!("Cancelled task {}.", cancelled.id));
            }
            if kind != "shell" {
                return Err(format!("Unsupported running work kind: {kind}"));
            }
            {
                let shells = live
                    .background_shells
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if let Some(shell) = shells.get(id) {
                    let killed = shell.kill();
                    if killed {
                        let seq = live.conduit.next_seq();
                        live.conduit.publish(
                            rightkit_agent_engine_protocol::ProtocolEvent::UserMessageSubmitted {
                                session_id: session_id.to_string(),
                                seq,
                                created_at: format!(
                                    "unix:{}",
                                    std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map_or(0, |d| d.as_secs())
                                ),
                                text: format!(
                                    "Stopped background shell {id} from the TUI Running panel."
                                ),
                                nonce: None,
                                trust: None,
                                attachments: Vec::new(),
                            },
                        );
                    }
                    return Ok(if killed {
                        format!("Stopped shell {id}.")
                    } else {
                        format!("Shell {id} was already stopping.")
                    });
                }
            }
            Err(format!("No live shell {id} is attached to this host"))
        }

        async fn running_output(&self, id: &str) -> Result<String, String> {
            if super::service_host_explicitly_selected() {
                return self.daemon_client().await?.execution_process_logs(id).await;
            }
            let Some(super::SessionExecutionHost::Inline(host)) =
                super::SessionExecutionHost::select_if_ready(self.context_mode)
            else {
                return Err("Live shell output is unavailable for this host".into());
            };
            let sessions = host
                .state
                .sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            for live in sessions.values() {
                let shells = live
                    .background_shells
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if let Some(shell) = shells.get(id) {
                    return Ok(shell.output_snapshot());
                }
            }
            Err(format!("No live shell {id} is attached to this host"))
        }

        async fn pty_attach(
            &self,
            id: &str,
        ) -> Result<rightkit_terminal_ui::pty_attach::PtyLease, String> {
            if super::service_host_explicitly_selected() {
                let (epoch, next_sequence) = self
                    .daemon_client()
                    .await?
                    .attach_execution_process(id)
                    .await?;
                return Ok(rightkit_terminal_ui::pty_attach::PtyLease {
                    epoch,
                    next_sequence,
                });
            }
            let shell = self.live_pty_shell(id)?;
            let lease = shell.attach().await?;
            // #291: a human is now attached; the agent's input yields to them.
            shell.set_human_attached(true);
            Ok(rightkit_terminal_ui::pty_attach::PtyLease {
                epoch: lease.epoch,
                next_sequence: lease.next_input_sequence,
            })
        }

        async fn pty_write(
            &self,
            id: &str,
            bytes: &[u8],
            epoch: u64,
            sequence: u64,
        ) -> Result<(), String> {
            if super::service_host_explicitly_selected() {
                let text = std::str::from_utf8(bytes)
                    .map_err(|_| "PTY input must be UTF-8".to_string())?;
                return self
                    .daemon_client()
                    .await?
                    .write_execution_process_stdin(id, text, epoch, sequence)
                    .await;
            }
            let shell = self.live_pty_shell(id)?;
            // #291: any human input releases the agent's lease.
            shell.note_human_input();
            shell
                .write_stdin_bytes_with_lease(bytes, epoch, sequence)
                .await
                .map(|_| ())
        }

        async fn pty_resize(&self, id: &str, rows: u16, cols: u16) -> Result<(), String> {
            if super::service_host_explicitly_selected() {
                return self
                    .daemon_client()
                    .await?
                    .resize_execution_process(id, rows, cols)
                    .await;
            }
            self.live_pty_shell(id)?
                .resize_pty(rightkit_sandbox::execution::PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .await
        }

        async fn pty_detach(&self, id: &str) -> Result<(), String> {
            if super::service_host_explicitly_selected() {
                return self
                    .daemon_client()
                    .await?
                    .detach_execution_process(id)
                    .await;
            }
            // Inline processes cannot be detached (the engine refuses); the
            // lease simply stays with this host, so there is nothing to release.
            let shell = self.live_pty_shell(id)?;
            shell.set_human_attached(false);
            let _ = shell.detach().await;
            Ok(())
        }

        fn plain_output(&self) -> bool {
            self.plain_output
        }

        fn reduced_motion(&self) -> bool {
            self.reduced_motion
        }

        fn permission_mode(&self) -> rightkit_agent_policy::PermissionMode {
            self.current_permission_mode()
        }

        fn permission_warning(&self) -> Option<&'static str> {
            let pending = self
                .pending_route
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .map(|(_, provider, _)| provider.clone());
            let provider = pending.unwrap_or_else(|| self.committed_provider_id());
            (provider == "antigravity"
                && self.current_permission_mode() != rightkit_agent_policy::PermissionMode::Bypass)
                .then_some("Antigravity uses its own permission rules")
        }

        fn set_permission_mode(&self, mode: rightkit_agent_policy::PermissionMode) {
            self.store_permission_mode(mode);
        }

        fn spawn_turn(&self, prompt: String) -> tokio::task::JoinHandle<i32> {
            // 1C: a scoped turn admits only with a selected canonical
            // repository scope, and runs against THAT root — not the
            // process cwd. Refuse before any cancellation state is
            // registered so a rejected spawn never poisons the next turn's
            // settle check; the typed refusal renders in the transcript.
            let workspace = self.active_workspace_root();
            let Some(workspace) = workspace else {
                self.enqueue_text(
                    "This folder can't be opened as a workspace (home or drive root). \
                     Launch RightKit inside a project folder, or run `/repo <path>`.",
                );
                return tokio::spawn(async move { 0 });
            };
            if self.no_model_selected() {
                self.enqueue_text("No model selected — choose one to run this prompt.");
                *self
                    .picker_request
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) =
                    Some(crate::tui_host::PickerRequest::Model);
                return tokio::spawn(async move { 0 });
            }
            let control = {
                let mut previous = self
                    .turn_cancellation
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if let Some(previous) = previous.as_ref() {
                    if let Err(error) = previous.require_settled() {
                        return tokio::spawn(async move {
                            // Raw stderr would corrupt the live viewport;
                            // the turn's non-zero exit surfaces through
                            // wait_turn, so the error goes to the trace log.
                            tracing::warn!("{error}");
                            1
                        });
                    }
                }
                let control = super::RunCancellation::new();
                *previous = Some(control.clone());
                control
            };
            let session_id = {
                let id = self
                    .active_session_id
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if id.is_empty() {
                    self.session_id.clone()
                } else {
                    id.clone()
                }
            };
            let (provider, model, change) = {
                // A pending route selection (made with no durable session)
                // commits at this turn's admission: queue it on the
                // SessionTurnModel so `begin_turn` attributes it as one
                // boundary change, then hand the effort to the create body.
                let pending = self
                    .pending_route
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
                let mut model = self.turn_model.lock().unwrap_or_else(|e| e.into_inner());
                if let Some((model_id, provider_id, effort_id)) = pending {
                    if !model_id.is_empty() {
                        let provider_id = if provider_id.is_empty() {
                            model.provider_id().to_string()
                        } else {
                            provider_id
                        };
                        model.request_switch_for_next_turn(
                            provider_id,
                            model_id,
                            rightkit_agent_orchestration::coding::PrefixChangeReason::UserRequestedSwitch,
                        );
                    }
                    if let Some(level) = effort_id {
                        *self
                            .selected_effort
                            .lock()
                            .unwrap_or_else(|e| e.into_inner()) = Some(level);
                    }
                }
                let change = model.begin_turn();
                // Admission consumes pending choices. Keep that admitted route
                // as the next chat's default even without an enrolled repository.
                if !model.model_id().is_empty() {
                    self.committed_route_state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .model = Some(model.model_id().to_string());
                }
                *self
                    .committed_provider
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(model.provider_id().to_string());
                (
                    model.provider_id().to_string(),
                    model.model_id().to_string(),
                    change,
                )
            };
            if let Some(change) = change {
                tracing::info!(
                    operation = "prefix_change",
                    session_id = %change.session_id,
                    turn_index = change.turn_index,
                    previous_provider_id = change.previous_provider_id.as_deref().unwrap_or(""),
                    previous_model_id = change.previous_model_id.as_deref().unwrap_or(""),
                    new_provider_id = %change.new_provider_id,
                    new_model_id = %change.new_model_id,
                    reason = ?change.reason,
                    "active model changed; prompt-cache prefix will break"
                );
            }
            let model_for_turn = if model.is_empty() { None } else { Some(model) };
            // Vendor-CLI routes (`claude-code/*`, `antigravity/*`) bypass the
            // session-execution host entirely: turns run through the vendor's
            // installed CLI and their normalized events are projected into
            // the same `host_events` render queue this host drains.
            if let Some(vendor_kind) =
                rightkit_agents::driver_host::vendor_cli::VendorCli::from_provider_id(&provider)
            {
                let events = std::sync::Arc::clone(&self.host_events);
                let conversations = std::sync::Arc::clone(&self.vendor_conversations);
                let permission_mode = self.current_permission_mode();
                return tokio::spawn(async move {
                    let code = super::run_vendor_turn(super::VendorTurnRequest {
                        kind: vendor_kind,
                        model: model_for_turn,
                        workspace,
                        prompt,
                        session_id,
                        permission_mode,
                        events,
                        conversations,
                        control: control.clone(),
                    })
                    .await;
                    control.settled();
                    code
                });
            }
            // 1E: an explicit `/effort` selection commits at this turn's
            // admission via the session-create body; afterwards the session's
            // committed effort is re-asserted on later turns. Model pending
            // state was already consumed above before `begin_turn`.
            let effort = {
                let selected = self
                    .selected_effort
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
                match selected {
                    Some(level) => {
                        self.committed_route_state
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .effort = Some(level.clone());
                        Some(level)
                    }
                    None => self
                        .committed_route_state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .effort
                        .clone(),
                }
            };
            let mut args = build_turn_args(
                prompt,
                &session_id,
                &provider,
                model_for_turn.as_deref(),
                Some(workspace),
                effort,
                true,
            );
            args.permission_mode = Some(self.current_permission_mode());
            // #132: a custom command's `allowed-tools` / `model` apply to its own turn only.
            {
                let mut pending = self
                    .pending_command_turn
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if let Some(overrides) = pending.as_ref() {
                    if overrides.apply(
                        &args.prompt,
                        &mut args.allowed_tools,
                        &mut args.tools,
                        &mut args.model,
                    ) {
                        *pending = None;
                    }
                }
            }
            // A pasted or dropped image file path attaches that image to this turn.
            args.image = crate::tui_image_input::image_paths_in_prompt(
                &args.prompt,
                args.workspace.as_deref().map(std::path::Path::new),
            );
            let readiness_tx = self.readiness_tx.clone();
            let context_mode = self.context_mode;
            let wait_control = control.clone();
            let failure_events = std::sync::Arc::clone(&self.host_events);
            tokio::spawn(async move {
                // KRN defect #4: await REAL context readiness before this
                // turn is admitted, instead of blocking the first message
                // on a synchronous connect inside admission. Cancellable:
                // a cancel arriving during this wait resolves promptly and
                // never touches `run_session`. Cheap once warm-up already
                // succeeded (shares the same single-flight cache). A no-op
                // under explicit ServiceHost selection, which never builds
                // an InlineHost at all.
                if !crate::main_runtime::service_host_explicitly_selected() {
                    tokio::select! {
                        result = ensure_inline_context_ready(&readiness_tx, context_mode) => {
                            if let Err(error) = result {
                                tracing::warn!("{error}");
                                // Never fail silently: the turn is refused
                                // before admission, so say why in the transcript.
                                if let Ok(mut queue) = failure_events.lock() {
                                    queue.push_back(
                                        rightkit_terminal_ui::transcript::timeline::DaemonEvent::System {
                                            text: format!("Turn not started: {error}"),
                                        },
                                    );
                                }
                                wait_control.settled();
                                return 1;
                            }
                        }
                        _ = wait_control.cancelled() => {
                            wait_control.settled();
                            return 0;
                        }
                    }
                }
                super::run_session_controlled(args, control).await
            })
        }

        fn readiness(
            &self,
        ) -> Option<tokio::sync::watch::Receiver<crate::tui_host::HostReadiness>> {
            Some(self.readiness_tx.subscribe())
        }

        async fn cancel_turn(&self) -> Result<(), String> {
            let control = self
                .turn_cancellation
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            match control {
                Some(control) => control.cancel_and_wait().await,
                None => Ok(()),
            }
        }

        async fn resolve_approval(
            &self,
            approval_session: &str,
            tool_call_id: &str,
            decision: rightkit_agent_engine_protocol::ReviewDecision,
        ) -> Result<(), String> {
            let control = self
                .turn_cancellation
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            let control = control.ok_or("No active turn owns this approval")?;
            let (host, session_id) = control.host.get().ok_or("Turn host is not ready")?;
            let approval_session = if approval_session == "local" {
                session_id.as_str()
            } else {
                approval_session
            };
            if session_id != approval_session {
                return Err("Approval belongs to a different session".into());
            }
            host.resolve_approval_decision(session_id, tool_call_id, decision)
                .await
        }

        async fn resolve_credential_request(
            &self,
            session_id: &str,
            request_id: &str,
            action: &str,
            secret: Option<&str>,
        ) -> Result<(), String> {
            // The TUI names the view's session "local" until the turn binds a
            // real one; resolve it to the active turn's session like approvals.
            let session_id = self.resolve_turn_session(session_id);
            let host = super::SessionExecutionHost::select(self.context_mode).await?;
            host.resolve_credential_request(&session_id, request_id, action, secret)
                .await
        }

        async fn permission_rules(
            &self,
            session_id: &str,
        ) -> Result<Vec<crate::tui_host::PermissionRuleView>, String> {
            let session_id = self.resolve_turn_session(session_id);
            let host = super::SessionExecutionHost::select(self.context_mode).await?;
            Ok(host
                .permission_rules(&session_id)
                .await?
                .into_iter()
                .map(|(pattern, effect)| crate::tui_host::PermissionRuleView { pattern, effect })
                .collect())
        }

        async fn install_permission_rule(
            &self,
            session_id: &str,
            pattern: &str,
            workspace_root: &str,
        ) -> Result<(), String> {
            let session_id = self.resolve_turn_session(session_id);
            let host = super::SessionExecutionHost::select(self.context_mode).await?;
            host.install_permission_rule(&session_id, pattern, workspace_root)
                .await
        }

        async fn resolve_mcp_elicitation(
            &self,
            session_id: &str,
            request_id: &str,
            action: rightkit_agent_engine_protocol::ElicitationAction,
            content: Option<serde_json::Value>,
        ) -> Result<(), String> {
            let session_id = self.resolve_turn_session(session_id);
            let host = super::SessionExecutionHost::select(self.context_mode).await?;
            host.resolve_mcp_elicitation(&session_id, request_id, action, content)
                .await
        }

        async fn steer_turn(&self, text: &str) -> Result<(), String> {
            let control = self
                .turn_cancellation
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
                .ok_or("No active turn to steer")?;
            let (host, session_id) = control.host.get().ok_or("Turn host is not ready")?;
            host.steer(session_id, text).await
        }

        async fn answer_question(
            &self,
            question_id: &str,
            answers: serde_json::Value,
        ) -> Result<(), String> {
            let control = self
                .turn_cancellation
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
                .ok_or("No active turn owns this question")?;
            let (host, session_id) = control.host.get().ok_or("Turn host is not ready")?;
            host.answer_question(session_id, question_id, answers).await
        }

        async fn resolve_plan(&self, plan_id: &str, approved: bool) -> Result<(), String> {
            let control = self
                .turn_cancellation
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
                .ok_or("No active turn owns this plan")?;
            let (host, session_id) = control.host.get().ok_or("Turn host is not ready")?;
            host.resolve_plan(session_id, plan_id, approved).await
        }

        fn resolve_command(&self, input: &str) -> Option<String> {
            if matches!(input.trim(), "/login" | "login" | "/logout" | "logout") {
                return Some(input.trim().trim_start_matches('/').to_string());
            }
            // Routed through the trait, not `CommandRouter`'s inherent
            // method, so `component::CommandSource` -- the seam
            // `CommandRouter` implements at `command_registry.rs:677` -- has
            // a real, non-test caller: this is exactly the "slash command
            // and its CLI subcommand are one implementation" contract the
            // trait exists to enforce.
            crate::cli::command_registry::CommandRouter.resolve(input)
        }

        fn custom_command_prompt(&self, input: &str) -> Option<String> {
            let trimmed = input.trim_start();
            if !trimmed.starts_with('/') {
                return None;
            }
            let head = trimmed.split_whitespace().next()?;
            // A built-in (or login/logout) always wins over a custom file.
            if crate::tui_host::TurnHost::resolve_command(self, head).is_some() {
                return None;
            }
            let workspace = std::path::PathBuf::from(self.active_workspace_root()?);
            crate::custom_commands::set_active_workspace(&workspace);
            let commands = crate::custom_commands::cached_commands();
            let rest = trimmed[head.len()..].trim();
            let trust = crate::main_runtime::workspace_trust_for(&workspace.to_string_lossy());
            let expanded = crate::custom_commands::expand_by_name(
                &commands,
                head,
                rest,
                self.current_permission_mode(),
                trust,
                &workspace,
            )?;
            let prompt = expanded.prompt.clone();
            for warning in &expanded.warnings {
                self.enqueue_text(warning);
            }
            *self
                .pending_command_turn
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = (!expanded.is_empty()).then_some(expanded);
            Some(prompt)
        }

        fn check_command(&self, name: &str, mid_turn: bool) -> Result<(), String> {
            if !mid_turn || matches!(name, "login" | "logout") {
                return Ok(());
            }
            crate::cli::command_registry::enforce_mid_turn(name)
        }

        async fn execute_command(&self, name: &str, rest: &str) -> bool {
            if name == "login" || name == "logout" {
                if !matches!(rest.trim(), "github" | "github --open") {
                    self.enqueue_text("usage: /login github [--open] | /logout github");
                } else if name == "logout" {
                    match crate::github_signin::sign_out() {
                        Ok(()) => self.enqueue_text("GitHub sign-in removed."),
                        Err(error) => {
                            self.enqueue_text(&format!("GitHub sign-out failed: {error}"))
                        }
                    }
                } else {
                    self.execute_github_sign_in(rest.trim() == "github --open")
                        .await;
                }
                return false;
            }
            if name == "/quit" {
                return true;
            }
            if name == "/help" {
                // CR-24 fourth surface: help text is generated by iterating
                // COMMAND_REGISTRY (interactive entries only), never a
                // hand-maintained list. Rendered through the transcript
                // queue, never printed around raw mode.
                self.enqueue_text(&render_registry_help(
                    rest.split_whitespace().any(|a| a == "--all"),
                ));
                return false;
            }
            if name == "/config" && rest.trim().is_empty() {
                *self
                    .picker_request
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) =
                    Some(crate::tui_host::PickerRequest::Config);
                return false;
            }
            if name == "/cd" && rest.trim().is_empty() {
                *self
                    .picker_request
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) =
                    Some(crate::tui_host::PickerRequest::Repository);
                return false;
            }
            if name == "/session" && (rest.trim().is_empty() || rest.trim() == "list") {
                *self
                    .picker_request
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) =
                    Some(crate::tui_host::PickerRequest::Sessions);
                return false;
            }
            if name == "/agents" && rest.trim().is_empty() {
                *self
                    .picker_request
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) =
                    Some(crate::tui_host::PickerRequest::Agents);
                return false;
            }
            if name == "/goal" {
                match crate::tui_host::goal_command(self, rest).await {
                    Ok(line) => self.enqueue_text(&line),
                    Err(reason) => self.enqueue_text(&reason),
                }
                return false;
            }
            if name == "/theme" || name == "/vim" {
                let (key, value) = if name == "/theme" {
                    ("theme", rest.trim().to_string())
                } else {
                    (
                        "vim",
                        if rest.trim().is_empty() {
                            if self.tui_settings().vim {
                                "off".to_string()
                            } else {
                                "on".to_string()
                            }
                        } else {
                            rest.trim().to_string()
                        },
                    )
                };
                if value.is_empty() {
                    self.enqueue_text("usage: /theme <auto|dark|light>");
                } else {
                    match self.set_tui_setting(key, &value).await {
                        Ok(line) => self.enqueue_text(&line),
                        Err(reason) => self.enqueue_text(&reason),
                    }
                }
                return false;
            }
            if name == "/trust" {
                match crate::tui_host::trust_active_workspace(self).await {
                    Ok(line) => self.enqueue_text(&line),
                    Err(reason) => self.enqueue_text(&reason),
                }
                return false;
            }
            if name == "/insights" {
                match self.insights().await {
                    Ok(report) => self.enqueue_text(&report),
                    Err(reason) => self.enqueue_text(&reason),
                }
                return false;
            }
            if name == "/diff" {
                match self.workspace_diff().await {
                    Ok(diff) => self.enqueue_text(&diff),
                    Err(reason) => self.enqueue_text(&reason),
                }
                return false;
            }
            if let Some(mut mode) = permission_mode_for_command(name) {
                // `/plan` toggles: a second `/plan` leaves plan mode for Ask.
                if mode == rightkit_agent_policy::PermissionMode::Plan
                    && self.permission_mode() == rightkit_agent_policy::PermissionMode::Plan
                {
                    mode = rightkit_agent_policy::PermissionMode::Ask;
                }
                self.store_permission_mode(mode);
                self.enqueue_text(&format!(
                    "Permission mode: {} (applies from the next turn).",
                    mode.display_label()
                ));
                return false;
            }
            if name == "/model" {
                if rest.is_empty() {
                    // RendererHint::Picker honoured for real: bare `/model`
                    // requests the actual ModelPicker over the host's real
                    // catalog — not a text menu.
                    *self
                        .picker_request
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) =
                        Some(crate::tui_host::PickerRequest::Model);
                    return false;
                }
                // `/model <n>` (picker number) and `/model <model-id>` both
                // resolve through the shared catalog projection first, then
                // the single route-authority call.
                match resolve_model_selection(&self.current_route_catalog().await, rest) {
                    // The resolved provider rides along as the opaque
                    // `provider::model` key — the bare model id alone is
                    // ambiguous whenever two providers serve the same model.
                    Ok(selection) => match self
                        .select_model(&format!(
                            "{}{ROUTE_SELECTION_SEP}{}",
                            selection.provider, selection.model
                        ))
                        .await
                    {
                        Ok(()) => {
                            // A no-repo selection is pending, not committed —
                            // say so honestly.
                            if self.pending_route_label().is_some() {
                                self.enqueue_text(&format!(
                                    "Model {} will apply to the next session.",
                                    selection.model
                                ));
                            } else {
                                self.enqueue_text(&format!("Model set to {}.", selection.model));
                            }
                        }
                        Err(reason) => self.enqueue_text(&reason),
                    },
                    Err(reason) => self.enqueue_text(&reason),
                }
                return false;
            }
            if slash_form(name) == "/resume" {
                // Resume in place: the TUI lists recent sessions (or switches
                // straight to `<id>`) and replays the transcript. `--last`
                // and a bare `/resume` both open the list.
                let target = rest.trim();
                let request = if target.is_empty() || target.starts_with("--") {
                    crate::tui_host::PickerRequest::Sessions
                } else {
                    crate::tui_host::PickerRequest::ResumeSession(target.to_string())
                };
                *self
                    .picker_request
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(request);
                return false;
            }
            if name == "/repo" {
                self.execute_repo_command(rest).await;
                return false;
            }
            if name == "/effort" {
                if rest.is_empty() {
                    // Bare `/effort` requests the effort picker for the
                    // committed model — same real ListPicker surface.
                    *self
                        .picker_request
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) =
                        Some(crate::tui_host::PickerRequest::Effort);
                    return false;
                }
                match self.select_effort(rest).await {
                    Ok(()) => {
                        if self.pending_route_label().is_some() {
                            self.enqueue_text(&format!(
                                "Effort {rest} will apply to the next session."
                            ));
                        } else {
                            self.enqueue_text(&format!("Effort set to {rest}."));
                        }
                    }
                    Err(reason) => self.enqueue_text(&reason),
                }
                return false;
            }
            if name == "/providers" {
                // `/providers` opens the provider/model visibility manager —
                // same modal discipline as `/model`, persisting toggles to
                // model-visibility.json as they happen.
                *self
                    .picker_request
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) =
                    Some(crate::tui_host::PickerRequest::Providers);
                return false;
            }
            if name == "/onboard" {
                self.run_onboarding_inline();
                return false;
            }
            // P1-1: every other registered command resolves and gates through
            // the shared registry, and EXECUTES through the host's own
            // command surface — SessionExecutionHost::execute_command, which
            // is the same shared dispatch InlineHost and ServiceHost project.
            // No per-surface executor re-implementation and no daemon
            // discovery on the Inline path.
            let name = slash_form(name);
            let input = if rest.is_empty() {
                name.clone()
            } else {
                format!("{name} {rest}")
            };
            let active: Option<String> = Some({
                let id = self
                    .active_session_id
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if id.is_empty() {
                    self.session_id.clone()
                } else {
                    id.clone()
                }
            });
            let result = if crate::main_runtime::service_host_explicitly_selected() {
                match self.daemon_client().await {
                    Ok(client) => client.execute_slash(&input, active.as_deref()).await,
                    Err(reason) => Err(reason),
                }
            } else {
                match super::SessionExecutionHost::select(self.context_mode).await {
                    Ok(host) => host.execute_command(&input, active.as_deref()).await,
                    Err(reason) => Err(reason),
                }
            };
            match result {
                Ok(result) => {
                    let success = result
                        .get("success")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    let output = result
                        .get("output")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("");
                    if output.is_empty() {
                        self.enqueue_text(&format!("[{}]", if success { "ok" } else { "failed" }));
                    } else {
                        self.enqueue_text(output);
                    }
                }
                Err(reason) => self.enqueue_text(&reason),
            }
            false
        }

        fn completions(&self, prefix: &str) -> Vec<String> {
            crate::cli::command_registry::CommandRouter.completions(prefix)
        }

        fn attach_input_pause(&self, pause: std::sync::Arc<crate::tui_host::InputPause>) {
            // Store the drive-loop's handshake so `/onboard` can take the
            // exclusive tty (reader-acknowledged) while its prompts run.
            let mut shared = self
                .input_pause_handle
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *shared = Some(pause);
        }

        fn input_paused(&self) -> bool {
            self.input_pause_handle
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .is_some_and(|pause| pause.is_paused())
        }

        fn take_input_drain(&self) -> bool {
            // One-shot: the drive loop reads this exactly once per pause and
            // discards the stale queued events. `pause_input` sets it; the
            // drive loop clears it by reading it.
            self.input_drain_pending
                .swap(false, std::sync::atomic::Ordering::SeqCst)
        }

        async fn refresh_nav(
            &self,
            nav: &mut rightkit_terminal_ui::nav::NavView,
            turn_in_flight: bool,
        ) {
            // The truthful local label: the resolved model once a turn has
            // bound one, else the configured provider — never "-".
            let local_model = {
                let model = self.turn_model.lock().unwrap_or_else(|e| e.into_inner());
                let m = model.model_id();
                if m.is_empty() {
                    // No model chosen yet: "-" is the nav's "unknown" marker,
                    // so no provider name masquerades as a model.
                    "-".to_string()
                } else {
                    m.to_string()
                }
            };
            // ServiceHost keeps its transport projection. InlineHost reads
            // sibling sessions from the same in-process composition/store
            // used by run_session, scoped to this workspace.
            let session_rows = if crate::main_runtime::service_host_explicitly_selected() {
                let Ok(client) = self.daemon_client().await else {
                    crate::tui_host::feed_local_stub(nav, turn_in_flight, &local_model);
                    return;
                };
                let Ok(rows) = client.list_sessions().await else {
                    crate::tui_host::feed_local_stub(nav, turn_in_flight, &local_model);
                    return;
                };
                rows
            } else {
                // 1C: navigation is scoped by the SELECTED repository
                // root, not the process cwd — the two can differ after a
                // `/repo` switch and must never silently disagree.
                // An unenrolled folder (context off, no canonical scope)
                // still has real sibling sessions: `active_workspace_root`
                // falls back to the folder the active session runs in.
                let workspace_root = self.active_workspace_root();
                // Without a workspace folder (home / filesystem root) or
                // a readable session store there are no sibling rows,
                // but the active session's own sub-agent roster is
                // still real and must reach the agent tree below — never
                // fall back to the childless local stub here.
                match workspace_root {
                    Some(workspace_root) => super::InlineHost::list_sessions_for_workspace(
                        self.context_mode,
                        &workspace_root,
                    )
                    .await
                    .unwrap_or_default(),
                    None => Vec::new(),
                }
            };
            let active_id = {
                let id = self
                    .active_session_id
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if id.is_empty() {
                    self.session_id.clone()
                } else {
                    id.clone()
                }
            };

            let mut sessions: Vec<rightkit_terminal_ui::nav::SessionSummary> = Vec::new();
            let mut active_row: Option<&serde_json::Value> = None;
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_secs() as i64)
                .unwrap_or(0);
            let mut details: std::collections::HashMap<
                String,
                rightkit_terminal_ui::nav::SessionDetail,
            > = std::collections::HashMap::new();
            for row in &session_rows {
                // Root sessions only: rows with a parent are sub-agents, which
                // belong in the agent tree, not the session picker.
                let is_subagent = row
                    .get("parentSessionId")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|s| !s.is_empty());
                if is_subagent {
                    continue;
                }
                let id = row
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let name = session_picker_label(self.context_mode, row)
                    .await
                    .unwrap_or_else(|| {
                        if id == active_id {
                            "this session".to_string()
                        } else {
                            // A generic title names nothing: show when it
                            // last moved and a short id to pick by.
                            let when = row
                                .get("updatedAt")
                                .or_else(|| row.get("createdAt"))
                                .and_then(serde_json::Value::as_str)
                                .map(|t| t.replace('T', " ").chars().take(16).collect::<String>())
                                .unwrap_or_default();
                            let tail: String = id
                                .chars()
                                .rev()
                                .take(8)
                                .collect::<Vec<_>>()
                                .into_iter()
                                .rev()
                                .collect();
                            format!("{when} \u{b7} {tail}").trim().to_string()
                        }
                    });
                details.insert(id.to_string(), session_picker_detail(row, now_secs));
                if id == active_id {
                    active_row = Some(row);
                }
                sessions.push(rightkit_terminal_ui::nav::SessionSummary {
                    id: rightkit_terminal_ui::nav::SessionId(id.to_string()),
                    name,
                });
            }
            // The fresh TUI session may not have a durable row until its first
            // turn. Keep it selectable, but never auto-switch to a prior row.
            if sessions.is_empty()
                || (!crate::main_runtime::service_host_explicitly_selected()
                    && !sessions.iter().any(|session| session.id.0 == active_id))
            {
                sessions.push(rightkit_terminal_ui::nav::SessionSummary {
                    id: rightkit_terminal_ui::nav::SessionId(active_id.clone()),
                    name: "this session".to_string(),
                });
            }
            let current = sessions
                .iter()
                .find(|s| s.id.0 == active_id)
                .cloned()
                .unwrap_or_else(|| sessions.first().cloned().unwrap());

            // Agent tree: the active session is the root; its roster rows are
            // its sub-agents (model, state, elapsed cost from the daemon),
            // and each sub-agent's roster is folded in recursively so the
            // tree spans the full daemon hierarchy, not one level (P1-5).
            let mut children: Vec<rightkit_terminal_ui::nav::AgentNode> = Vec::new();
            // The agent tree reads rosters through the shared
            // `SessionExecutionHost` seam (`get_session_agents`): InlineHost
            // serves its composed state's roster in-process, ServiceHost the
            // same roster through the client transport — one owner, both
            // host kinds.
            // `select_if_ready`: nav refresh runs on the terminal loop and
            // must never wait on a context build; no state, no roster.
            if let Some(host) = super::SessionExecutionHost::select_if_ready(self.context_mode) {
                if let Ok(roster) = host.get_session_agents(&active_id).await {
                    for row in roster {
                        let sub_id = row
                            .get("subSessionId")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        if sub_id.is_empty() {
                            continue;
                        }
                        let mut node = agent_node_from_roster(&row, sub_id.clone());
                        // Recurse into the sub-agent's own roster. Depth is
                        // bounded by AGENT_TREE_MAX_DEPTH AND by the ancestor
                        // `seen` guard — a cyclic roster (a session listing an
                        // ancestor as its own descendant) terminates instead
                        // of recursing forever.
                        let mut seen: Vec<String> = vec![active_id.clone()];
                        build_agent_subtree(&host, &sub_id, &mut seen, &mut node).await;
                        children.push(node);
                    }
                }
            }
            let root_model = active_row
                .and_then(|r| r.get("model").and_then(serde_json::Value::as_str))
                .filter(|model| !model.is_empty())
                .unwrap_or(&local_model)
                .to_string();
            let root_cost = active_row
                .and_then(|r| r.get("costUsd").and_then(serde_json::Value::as_f64))
                .map(|c| format!("${c:.4}"))
                .unwrap_or_default();
            let root_state = if turn_in_flight {
                rightkit_terminal_ui::nav::AgentState::Running
            } else {
                rightkit_terminal_ui::nav::AgentState::Done
            };
            nav.set_sessions(sessions, Some(current));
            nav.set_session_details(details);
            nav.set_tree(rightkit_terminal_ui::nav::AgentNode {
                id: rightkit_terminal_ui::nav::AgentId(active_id.clone()),
                name: "root agent".to_string(),
                model: root_model,
                elapsed_cost: root_cost,
                state: root_state,
                children,
            });
        }

        async fn stop_remote_subtree(
            &self,
            scope: &[rightkit_terminal_ui::nav::AgentId],
        ) -> Result<usize, String> {
            // Each id in the subtree scope is itself a daemon session (a roster
            // sub_session_id or the root session id); the daemon's /interrupt
            // recursively cancels a sub-session's descendants, so interrupting
            // every id in the scope stops exactly that subtree. A failure on
            // one id is reported; the rest are still attempted. Both host
            // kinds go through `SessionExecutionHost::interrupt` — InlineHost
            // interrupts in-process (the daemon client is never available
            // here), ServiceHost through the existing client transport.
            let host = super::SessionExecutionHost::select(self.context_mode).await?;
            let mut stopped = 0usize;
            let mut first_error: Option<String> = None;
            for id in scope {
                match host.interrupt(&id.0).await {
                    Ok(()) => stopped += 1,
                    Err(e) if first_error.is_none() => first_error = Some(e),
                    Err(_) => {}
                }
            }
            if let Some(error) = first_error {
                Err(format!("stopped {stopped} agent(s); {error}"))
            } else {
                Ok(stopped)
            }
        }

        async fn message_remote_agent(
            &self,
            agent: &rightkit_terminal_ui::nav::AgentId,
            text: &str,
        ) -> Result<(), String> {
            let host = super::SessionExecutionHost::select(self.context_mode).await?;
            host.steer(&agent.0, text).await
        }

        async fn new_session(&self) -> Result<rightkit_terminal_ui::nav::SessionId, String> {
            // A brand-new durable root session under the currently selected
            // repository root (else the launch workspace); the old session
            // keeps running.
            let root = {
                let selected = self
                    .selected_repository
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .as_ref()
                    .map(|scope| scope.root.clone());
                match selected {
                    Some(root) => root,
                    None => std::env::current_dir()
                        .map_err(|e| format!("workspace root unavailable: {e}"))?
                        .to_string_lossy()
                        .to_string(),
                }
            };
            let id = match resolve_repository_selection(&root) {
                Ok(scope) => self.start_session_in_scope(scope).await?.1,
                // D18: no enrollable repository (context unavailable) — start
                // an unscoped session in a real project folder, never refuse.
                Err(_) if is_workspace_candidate(std::path::Path::new(&root)) => {
                    self.start_session_at_root(root.clone(), None).await?
                }
                Err(error) => return Err(error),
            };
            self.timeline_cursors
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            Ok(rightkit_terminal_ui::nav::SessionId(id))
        }

        async fn switch_session(
            &self,
            id: &rightkit_terminal_ui::nav::SessionId,
        ) -> Result<(), String> {
            // Detach, not cancel (invariant AK): re-point the active session
            // so future turns target it; the tree being left keeps running.
            // The durable row is read and VALIDATED FIRST — active session,
            // turn model and selected repository commit together only once
            // the target session's scope is proven. A missing/unreadable row
            // or an invalid restored root leaves every local field as it was:
            // the previous session's repository must never serve the session
            // being entered (the cross-scope defect this ordering removes).
            // There is no cwd fallback and no re-enrollment.
            let host = super::SessionExecutionHost::select(self.context_mode)
                .await
                .map_err(|reason| format!("session host unavailable: {reason}"))?;
            let row = host
                .session_snapshot(&id.0)
                .await
                .map_err(|reason| format!("session scope could not be read: {reason}"))?;
            let restored = row.as_ref().map(session_snapshot_route);
            // The durable row is authoritative for scope; the in-process
            // root map only backs a session this host just created whose
            // row may not be listed yet.
            let recorded_root = restored
                .as_ref()
                .and_then(|restored| restored.workspace_root.clone())
                .or_else(|| {
                    self.session_roots
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&id.0)
                        .cloned()
                });
            let Some(root) = recorded_root else {
                return Err(format!(
                    "session {} has no durable repository scope; it was not selected",
                    id.0
                ));
            };
            // D18: with context unavailable no repository enrolls, but the
            // session's recorded folder is still a real project — enter it
            // unscoped (its root rides `session_roots`), never refuse it.
            let scope: Option<SelectedRepository> = match resolve_repository_selection(&root) {
                Ok(scope) => Some(scope),
                Err(_) if is_workspace_candidate(std::path::Path::new(&root)) => None,
                Err(reason) => {
                    return Err(format!(
                        "session {} repository scope is invalid: {reason}",
                        id.0
                    ));
                }
            };
            // Restore the committed route from the SAME durable snapshot:
            // model, provider and effort belong to the session being
            // entered — never inherited from the session being left.
            let (restored_provider, restored_model, restored_effort) = restored
                .map(|restored| (restored.provider_id, restored.model, restored.effort))
                .unwrap_or_default();
            let restored_provider = match (restored_provider, restored_model.as_deref()) {
                (provider @ Some(_), _) => provider,
                // A legacy row proving a model but no provider id resolves
                // ONLY through a uniquely-named route in the shared catalog.
                // Ambiguity or absence is a typed refusal BEFORE any local
                // state moves — a restored model is never paired with the
                // prior session's provider.
                (None, Some(model)) => {
                    match unique_route_provider(&self.current_route_catalog().await, model) {
                        Some(provider) => Some(provider),
                        None => {
                            return Err(format!(
                                "session {} durable model '{model}' has no recorded provider \
                                 and no unique catalog route resolves one; it was not selected",
                                id.0
                            ));
                        }
                    }
                }
                (None, None) => None,
            };
            // Forget any timeline cursor for the target so the next poll
            // replays its durable history into the cleared transcript.
            self.timeline_cursors
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id.0);
            // All durable reads resolved — commit local state as one unit.
            *self
                .active_session_id
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = id.0.clone();
            *self
                .selected_repository
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = scope.clone();
            self.session_roots
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(
                    id.0.clone(),
                    scope
                        .as_ref()
                        .map(|scope| scope.root.clone())
                        .unwrap_or(root),
                );
            {
                let mut committed = self
                    .committed_route_state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                // Absent durable fields RESET the corresponding committed
                // field — assigning only `Some` values would leak the
                // previous session's effort/model into this one.
                committed.model = restored_model.clone();
                committed.effort = restored_effort;
            }
            *self
                .committed_provider
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = restored_provider.clone();
            // A route parked for the old session must not leak into the
            // session being entered.
            *self.pending_route.lock().unwrap_or_else(|e| e.into_inner()) = None;
            let mut turn_model = self.turn_model.lock().unwrap_or_else(|e| e.into_inner());
            // The turn model is rebuilt from the durable route outright:
            // attribution now reflects the session being entered, and no
            // provider/model from the session being left survives. A row
            // with no durable route falls back to the host's configured
            // default provider with an empty model — never the previous
            // session's pair.
            *turn_model = rightkit_agent_orchestration::coding::SessionTurnModel::new(
                id.0.clone(),
                restored_provider.unwrap_or_else(|| self.provider.clone()),
                restored_model.unwrap_or_default(),
            );
            Ok(())
        }

        async fn fetch_agent_transcript(
            &self,
            id: &rightkit_terminal_ui::nav::AgentId,
        ) -> Option<String> {
            // Read-only view of a sub-agent: pull its bounded scrollback and
            // render the recent user/assistant/tool events as an excerpt, and
            // SEED the per-agent cursor the maintenance tick polls from. The
            // cursor is advanced past the FULL event set before bounding, so
            // events older than the newest window are deliberately skipped
            // once — never replayed by later polls. Events come through
            // `SessionExecutionHost::events_after`: InlineHost reads its
            // composed state's durable store (the daemon client is never
            // available here); ServiceHost uses the same client transport.
            let host = super::SessionExecutionHost::select(self.context_mode)
                .await
                .ok()?;
            let events = host.events_after(&id.0, 0).await.ok()?;
            let cursor = advance_agent_cursor(0, &events);
            if let Ok(mut cursors) = self.viewed_cursors.lock() {
                cursors.insert(id.0.clone(), cursor);
            }
            let window = newest_agent_events(&events, AGENT_TRANSCRIPT_WINDOW);
            let lines: Vec<String> = window
                .iter()
                .filter_map(|event| agent_event_display_line(event))
                .collect();
            if lines.is_empty() {
                None
            } else {
                Some(lines.join("\n"))
            }
        }

        async fn poll_agent_transcript(
            &self,
            id: &rightkit_terminal_ui::nav::AgentId,
        ) -> Option<Vec<String>> {
            // P1-5 live agent transcript: fetch ONLY events newer than this
            // agent's last-seen cursor (bounded increments, never a full
            // replay from 0), advance the cursor, and hand back the bounded
            // newest window of new display lines. Reading history never
            // pauses or owns the viewed worker. Same `SessionExecutionHost`
            // event seam as `fetch_agent_transcript` — never `daemon_client`,
            // which is unavailable under InlineHost.
            let host = super::SessionExecutionHost::select(self.context_mode)
                .await
                .ok()?;
            let cursor = self
                .viewed_cursors
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&id.0)
                .copied()
                .unwrap_or(0);
            let events = host.events_after(&id.0, cursor).await.ok()?;
            let fresh = fresh_agent_events(&events, cursor);
            if fresh.is_empty() {
                return None;
            }
            let next = advance_agent_cursor(cursor, &fresh);
            if let Ok(mut cursors) = self.viewed_cursors.lock() {
                cursors.insert(id.0.clone(), next);
            }
            let window = newest_agent_events(&fresh, AGENT_TRANSCRIPT_WINDOW);
            let lines: Vec<String> = window
                .iter()
                .filter_map(|event| agent_event_display_line(event))
                .collect();
            if lines.is_empty() {
                None
            } else {
                Some(lines)
            }
        }

        async fn poll_timeline_events(
            &self,
        ) -> Vec<rightkit_terminal_ui::transcript::timeline::DaemonEvent> {
            // Renderer-owned command output (help/model/repo/effort/status
            // and slash results) drains first so it is ordered before any
            // daemon events admitted since the last poll.
            let queued: Vec<rightkit_terminal_ui::transcript::timeline::DaemonEvent> = self
                .host_events
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .drain(..)
                .collect();
            let active_session = self
                .active_session_id
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let session_id = if active_session.is_empty() {
                self.session_id.clone()
            } else {
                active_session
            };
            let cursor = self
                .timeline_cursors
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&session_id)
                .copied()
                .unwrap_or(0);
            let control = self
                .turn_cancellation
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            let bound = control
                .as_ref()
                .and_then(|control| control.host.get())
                .filter(|(_, bound_session)| bound_session == &session_id);
            let result = if let Some((host, _)) = bound {
                Some(host.events_after(&session_id, cursor).await)
            } else if crate::main_runtime::service_host_explicitly_selected() {
                // Preserve ServiceHost history/navigation before a local turn
                // binds. Inline mode refuses daemon discovery in this helper.
                match self.daemon_client().await {
                    Ok(client) => Some(client.events_after(&session_id, cursor).await),
                    Err(_) => None,
                }
            } else {
                // InlineHost reads its composed state's durable store through
                // the shared host surface — never daemon discovery.
                // It never builds: a slow or failing context build must not
                // stall the terminal loop, and there is nothing to read
                // until the state exists.
                match super::SessionExecutionHost::select_if_ready(self.context_mode) {
                    Some(host) => Some(host.events_after(&session_id, cursor).await),
                    None => None,
                }
            };
            let durable = match result {
                Some(Ok(events)) => fresh_agent_events(&events, cursor),
                _ => Vec::new(),
            };
            let next = advance_agent_cursor(cursor, &durable);
            if next > cursor {
                if let Ok(mut cursors) = self.timeline_cursors.lock() {
                    cursors.insert(session_id.clone(), next);
                }
            }
            // Live projections (token/tool-output deltas) are not in the
            // durable journal; they arrive on the turn's live stream.
            let merged = {
                let mut pending = self.live_pending.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(control) = control.as_ref() {
                    let now = std::time::Instant::now();
                    pending.extend(
                        control
                            .drain_live_projections()
                            .into_iter()
                            .map(|e| (now, e)),
                    );
                }
                let now = std::time::Instant::now();
                let mut ready = Vec::new();
                pending.retain(|(seen, event)| {
                    // Anchor = newest durable seq allocated when the delta was
                    // emitted. Wait for those facts so the delta renders after
                    // them, but never hold a delta indefinitely.
                    if daemon_event_seq_of(event) <= next
                        || now.duration_since(*seen) > LIVE_PROJECTION_HOLD
                    {
                        ready.push(event.clone());
                        false
                    } else {
                        true
                    }
                });
                merge_live_projections(durable.clone(), ready)
            };
            if merged.is_empty() {
                return queued;
            }
            let can_respond = bound.is_some()
                && control
                    .as_ref()
                    .is_some_and(|control| control.completion.borrow().is_none());
            let mut events: Vec<_> = queued;
            events.extend(reasoning_before_answer(
                merged
                    .iter()
                    .filter_map(daemon_event_to_timeline_event)
                    .map(|event| project_owned_interaction(event, can_respond))
                    .collect(),
            ));
            events
        }

        /// Producers that can emit while no local turn runs: queued
        /// renderer-owned command output ALWAYS drains (it exists precisely
        /// when nothing else produces), an InlineHost has a live durable
        /// event source through its composed state, and a resolved
        /// ServiceHost feed can emit. Reading the already-populated slot —
        /// never calling `daemon_client()` — keeps an idle tick from
        /// autostarting a daemon purely to watch it.
        fn external_feed_available(&self) -> bool {
            if self
                .host_events
                .lock()
                .map(|queue| !queue.is_empty())
                .unwrap_or(false)
            {
                return true;
            }
            if !crate::main_runtime::service_host_explicitly_selected() {
                // InlineHost: `poll_timeline_events` reads the shared
                // in-process event store — live events are always possible.
                return true;
            }
            self.daemon
                .lock()
                .map(|slot| slot.is_some())
                .unwrap_or(false)
        }

        async fn fetch_older_history_page(
            &self,
            before_seq: i64,
            limit: usize,
        ) -> Result<Vec<crate::tui_host::HistoryPageEntry>, String> {
            // P2-5: page older durable history for the ACTIVE session. Both
            // transports go through `SessionExecutionHost::fetch_history`
            // (exclusive `before_seq` cursor, ascending page order, finite
            // limit, missing-session and corrupt-history errors): InlineHost
            // serves its composed state's durable store in-process with no
            // daemon discovery and no active model turn; ServiceHost uses
            // the same `?before&limit` endpoint through the client.
            let active_session = {
                let id = self
                    .active_session_id
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if id.is_empty() {
                    self.session_id.clone()
                } else {
                    id.clone()
                }
            };
            let host = super::SessionExecutionHost::select_if_ready(self.context_mode)
                .ok_or_else(|| "inline host state is not built yet".to_string())?;
            let page = host
                .fetch_history(&active_session, before_seq, limit)
                .await
                .map_err(|error| format!("scrollback page failed: {error}"))?;
            let events = page
                .as_array()
                .cloned()
                .or_else(|| {
                    page.get("events")
                        .and_then(serde_json::Value::as_array)
                        .cloned()
                })
                .ok_or_else(|| "scrollback page was not an event list".to_string())?;
            Ok(history_page_entries(&events))
        }

        /// The real catalog: the shared route projection — canonical
        /// provider model metadata for API-key configured routes,
        /// broker-reported account model ids for OAuth routes, and the
        /// committed/configured pins as always-present authority rows. Rows
        /// whose availability is unproven carry an explicit
        /// typed-unavailable description instead of fabricating account
        /// state. An empty result is the typed-unavailable state
        /// `open_route_picker` notices.
        async fn model_catalog(&self) -> Vec<rightkit_terminal_ui::nav::ModelChoice> {
            use rightkit_terminal_ui::nav::ModelChoice;
            let committed = crate::tui_host::TurnHost::committed_route(self);
            let committed_provider = self.committed_provider_id();
            let committed_pair = committed
                .model
                .as_deref()
                .map(|model| (model, committed_provider.as_str()));
            let catalog = self.current_route_catalog().await;
            catalog
                .iter()
                .map(|route| {
                    // The row id is the OPAQUE selection key when another
                    // provider serves the same model — `display_name` stays
                    // the native model id for presentation. `current` marks
                    // the exact committed (model, provider) row, never just
                    // the first same-named model.
                    let mut choice = ModelChoice::new(
                        route_selection_id(&catalog, route, committed_pair),
                        route.display_name.clone(),
                    )
                    .provider(route.provider.clone())
                    .current(
                        committed_pair
                            .map(|(model, provider)| {
                                model == route.model && provider == route.provider
                            })
                            .unwrap_or(false),
                    )
                    .default_flag(route.is_default);
                    if route.selectable {
                        choice = choice.efforts(effort_choices(
                            route.capability,
                            committed.effort.as_deref(),
                        ));
                    } else if let Some(note) = &route.availability_note {
                        choice = choice.description(note.clone());
                    }
                    choice
                })
                .collect()
        }

        /// The same shared route catalog [`Self::model_catalog`] projects,
        /// grouped by provider for the provider-first `/model` flow and the
        /// `/providers` visibility manager. Grouping itself is pure —
        /// [`group_catalog_by_provider`] — so picker composition is
        /// unit-testable without a session host.
        async fn provider_catalog(&self) -> Vec<rightkit_terminal_ui::nav::ProviderChoice> {
            let committed = crate::tui_host::TurnHost::committed_route(self);
            let committed_provider = self.committed_provider_id();
            let committed_pair = committed
                .model
                .as_deref()
                .map(|model| (model, committed_provider.as_str()));
            let configured_model = {
                let config = super::load_current_merged_config();
                let m = config.model.default_model.trim().to_string();
                (!m.is_empty()).then_some(m)
            };
            let (catalog, statuses) = route_catalog_with_statuses(
                committed_pair,
                configured_model
                    .as_deref()
                    .map(|m| (m, self.provider.as_str())),
            )
            .await;
            let visibility = super::load_model_visibility();
            let mut providers = group_catalog_by_provider(
                &catalog,
                &statuses,
                &visibility,
                committed_pair,
                committed.effort.as_deref(),
            );
            // Custom `provider-routes.*` groups carry their configured
            // display name instead of the bare route id.
            {
                let config = super::load_current_merged_config();
                for provider in providers
                    .iter_mut()
                    .filter(|p| super::provider_setup::screen_provider(&p.id).is_none())
                {
                    if let Some(route) = config.provider_routes.get(&provider.id) {
                        let name = route.display_name.trim();
                        if !name.is_empty() {
                            provider.label = name.to_string();
                        }
                    }
                }
            }
            // Vendor CLIs (Claude Code, Antigravity) carry detection facts
            // and their own live model lists; detection spawns the CLI, so
            // keep it off the async executor.
            for provider in providers
                .iter_mut()
                .filter(|p| p.kind == rightkit_terminal_ui::nav::ProviderKind::VendorCli)
            {
                let id = provider.id.clone();
                let hidden = visibility.get(&id).cloned().unwrap_or_default();
                let gathered = tokio::task::spawn_blocking({
                    let id = id.clone();
                    move || {
                        let facts = super::provider_setup::vendor_facts(&id);
                        let models = if facts.installed {
                            super::provider_setup::vendor_models(&id)
                        } else {
                            Vec::new()
                        };
                        let hints = super::provider_setup::vendor_hints(&id).unwrap_or_default();
                        (facts, models, hints)
                    }
                })
                .await;
                if let Ok((facts, models, hints)) = gathered {
                    super::provider_setup::apply_vendor_facts(
                        provider,
                        &facts,
                        &hints,
                        &models,
                        &hidden,
                        committed_pair,
                    );
                }
            }
            providers
        }

        /// `/providers` level-2 actions run in-process through the same
        /// functions `{app} provider ...` calls. The typed key (add /
        /// replace) goes straight to the keychain writer.
        async fn provider_action(
            &self,
            provider: &str,
            action: rightkit_terminal_ui::nav::ProviderAction,
            secret: Option<rightkit_terminal_ui::nav::SecretText>,
        ) -> Result<String, String> {
            super::provider_setup::perform_action(
                provider,
                action,
                secret
                    .as_ref()
                    .map(rightkit_terminal_ui::nav::SecretText::expose),
            )
            .await
        }

        /// `/providers` persistence: the durable `model-visibility.json`
        /// file this host owns. `model` is the real catalog id
        /// (`ProviderModel::model_id`), never the opaque selection key.
        async fn set_model_visibility(
            &self,
            provider: &str,
            model: &str,
            hidden: bool,
        ) -> Result<(), String> {
            super::set_model_hidden(provider, model, hidden)
        }

        fn committed_route(&self) -> crate::tui_host::CommittedRoute {
            let mut committed = self
                .committed_route_state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if committed.model.is_none() {
                let model = self.turn_model.lock().unwrap_or_else(|e| e.into_inner());
                let id = model.model_id();
                if !id.is_empty() {
                    committed.model = Some(id.to_string());
                }
            }
            // A selection parked until the session commits is still the
            // model the next turn runs on — show it rather than "no model".
            if let Some((model, _provider, effort)) = self
                .pending_route
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
            {
                if !model.is_empty() {
                    committed.model = Some(model.clone());
                }
                if effort.is_some() {
                    committed.effort = effort.clone();
                }
            }
            committed
        }

        fn pending_route_label(&self) -> Option<String> {
            self.pending_route
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .map(|(model, _provider, effort)| match effort {
                    Some(effort) if !model.is_empty() => format!("{model} · {effort}"),
                    Some(effort) => format!("effort {effort}"),
                    None => model.clone(),
                })
        }

        async fn select_model(&self, model_id: &str) -> Result<(), String> {
            let catalog = self.current_route_catalog().await;
            // The incoming id may be an opaque `provider::model` picker key;
            // decode it against the shared catalog so the EXACT route —
            // provider AND real model id — is what the authority applies.
            let committed_provider = self.committed_provider_id();
            let committed_model = self.committed_route().model;
            let committed = committed_model
                .as_deref()
                .map(|model| (model, committed_provider.as_str()));
            let route = resolve_picker_route(&catalog, model_id, committed)?;
            if !route.selectable {
                return Err(format!(
                    "model '{}' is unavailable: {}",
                    route.model,
                    route
                        .availability_note
                        .as_deref()
                        .unwrap_or("provider has no configured credential")
                ));
            }
            let model = route.model.clone();
            let provider_id = route.provider.clone();
            let capability = route.capability;
            // One atomic update carries BOTH model and effort — never a
            // model commit then a separate effort commit. The committed
            // effort rides along only if the target model accepts it (or
            // its capability is unknown — the committed value is the one
            // level permitted there); anything else is a typed error, not
            // a silent effort change.
            let committed_effort = self
                .committed_route_state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .effort
                .clone();
            let effort = match committed_effort {
                Some(effort) => Some(validate_effort_for_route(
                    &model,
                    capability,
                    Some(effort.as_str()),
                    effort.as_str(),
                )?),
                None => None,
            };
            self.apply_route_selection(&model, Some(provider_id.as_str()), effort.as_deref())
                .await
        }

        async fn select_effort(&self, effort_id: &str) -> Result<(), String> {
            match self.committed_route().model {
                Some(model) => {
                    let catalog = self.current_route_catalog().await;
                    // Effort validates against the COMMITTED provider's exact
                    // row — the first same-named model in the catalog may be
                    // a different provider's capability set.
                    let committed_provider = self
                        .committed_provider
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    let route = match committed_provider.as_deref() {
                        Some(provider) => catalog
                            .iter()
                            .find(|route| route.model == model && route.provider == provider)
                            .or_else(|| catalog_route_for_model(&catalog, &model)),
                        None => catalog_route_for_model(&catalog, &model),
                    };
                    let capability = route.and_then(|r| r.capability);
                    let committed_effort = self
                        .committed_route_state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .effort
                        .clone();
                    // Effort is validated against the committed model's real
                    // capability BEFORE the route commit — `/effort low` on
                    // a reasoning-3 model validates, an unsupported level on
                    // a known model fails, and unknown capability permits
                    // only the committed value.
                    let effort = validate_effort_for_route(
                        &model,
                        capability,
                        committed_effort.as_deref(),
                        effort_id,
                    )?;
                    // Effort-only commit rides the EXACT committed
                    // provider pair — a catalog lookup is only a fallback
                    // when no committed provider was recorded, never an
                    // inferred different provider.
                    let provider_id =
                        committed_provider.or_else(|| Self::provider_for_model(&catalog, &model));
                    self.apply_route_selection(
                        &model,
                        provider_id.as_deref(),
                        Some(effort.as_str()),
                    )
                    .await
                }
                // No-repo mode keeps effort usable: an effort-only pending
                // selection commits with the first turn's session-create
                // body. An existing pending model/provider tuple is
                // PRESERVED — this never overwrites it with empties — and
                // when a pending model exists the effort is validated
                // against THAT model's capability before it is stored.
                None => {
                    let effort = crate::api::normalize_effort(Some(effort_id.to_string()))?
                        .ok_or_else(|| "empty effort selection".to_string())?;
                    let has_repo = self
                        .selected_repository
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .is_some();
                    if has_repo {
                        return Err("no committed model to set effort on".to_string());
                    }
                    let pending = self
                        .pending_route
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    let effort = if let Some((model, provider, _)) =
                        pending.as_ref().filter(|(model, ..)| !model.is_empty())
                    {
                        let catalog = self.current_route_catalog().await;
                        // A pending provider pin resolves THAT row's
                        // capability; bare-model first-match is only the
                        // no-provider fallback.
                        let capability = if provider.is_empty() {
                            catalog_route_for_model(&catalog, model)
                        } else {
                            catalog
                                .iter()
                                .find(|route| route.model == *model && route.provider == *provider)
                        }
                        .and_then(|r| r.capability);
                        validate_effort_for_route(
                            model,
                            capability,
                            pending.as_ref().and_then(|(.., effort)| effort.as_deref()),
                            &effort,
                        )?
                    } else {
                        effort
                    };
                    *self.pending_route.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(merge_pending_route(pending, None, None, Some(effort)));
                    Ok(())
                }
            }
        }

        /// The picker's single semantic selector: one validated model+effort
        /// pair through ONE route-authority commit. `effort_id` arrives from
        /// the picker's per-model effort rows (catalog ids only); when absent
        /// the committed effort rides along so a model switch never silently
        /// clears effort.
        async fn select_route(
            &self,
            model_id: &str,
            effort_id: Option<&str>,
        ) -> Result<(), String> {
            let catalog = self.current_route_catalog().await;
            // The picker's `SelectModel` id may be an opaque
            // `provider::model` key; decode it here so one atomic commit
            // applies the exact (provider, model) route — never the first
            // same-named model.
            let committed_provider = self.committed_provider_id();
            let committed_model = self.committed_route().model;
            let committed = committed_model
                .as_deref()
                .map(|model| (model, committed_provider.as_str()));
            let route = resolve_picker_route(&catalog, model_id, committed)?;
            if !route.selectable {
                return Err(format!(
                    "model '{}' is unavailable: {}",
                    route.model,
                    route
                        .availability_note
                        .as_deref()
                        .unwrap_or("provider has no configured credential")
                ));
            }
            let model = route.model.clone();
            let provider_id = route.provider.clone();
            let capability = route.capability;
            let committed_effort = self
                .committed_route_state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .effort
                .clone();
            // Per-model effort validation happens BEFORE the commit: an
            // explicit picker/cli level is checked against the model's
            // canonical capability; an omitted level carries the committed
            // effort only when that model permits it.
            let effort = match effort_id {
                Some(id) => Some(validate_effort_for_route(
                    &model,
                    capability,
                    committed_effort.as_deref(),
                    id,
                )?),
                None => match committed_effort {
                    Some(effort) => Some(validate_effort_for_route(
                        &model,
                        capability,
                        Some(effort.as_str()),
                        effort.as_str(),
                    )?),
                    None => None,
                },
            };
            self.apply_route_selection(&model, Some(provider_id.as_str()), effort.as_deref())
                .await
        }

        /// `(root, label)` rows for the repository list picker: the
        /// host-known enrolled roots (sessions this process bound plus the
        /// current selection), labelled by the directory name.
        fn repository_catalog(&self) -> Vec<(String, String)> {
            let current = self
                .selected_repository
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .map(|scope| scope.root.clone());
            let mut roots = known_repository_roots(
                &self.session_roots.lock().unwrap_or_else(|e| e.into_inner()),
                current.as_deref(),
            );
            match rightkit_memory_client::backend::repository_scope::enrolled_repository_scopes() {
                Ok(scopes) => roots.extend(scopes.into_iter().map(|scope| scope.root)),
                Err(error) => tracing::warn!("repository catalog: {error}"),
            }
            roots.sort();
            roots.dedup();
            roots
                .into_iter()
                .map(|root| {
                    let label = std::path::Path::new(&root)
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .filter(|name| !name.is_empty())
                        .unwrap_or_else(|| root.clone());
                    (root, label)
                })
                .collect()
        }

        /// The picker's selection goes through the exact same semantic as
        /// `/repo <path>` — busy gate, canonical resolve, durable root
        /// session, then retarget. Errors propagate to the picker's notice.
        async fn select_repository(&self, root: &str) -> Result<(), String> {
            let busy = self
                .turn_cancellation
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .map(|control| control.require_settled().is_err())
                .unwrap_or(false);
            if busy {
                return Err(
                    "Cannot switch repository while a turn or approval is in progress.".to_string(),
                );
            }
            match self.select_repository_root(root).await {
                Ok(_) => Ok(()),
                // D18: context unavailable (nothing enrolls) — a real project
                // folder still opens as an unscoped session whose root rides
                // `session_roots`; only a non-folder is refused.
                Err(reason) => {
                    let path = std::path::PathBuf::from(root.trim());
                    if !is_workspace_candidate(&path) || resolve_repository_selection(root).is_ok()
                    {
                        return Err(reason);
                    }
                    let canonical = path.canonicalize().unwrap_or(path);
                    let canonical = canonical
                        .to_string_lossy()
                        .trim_start_matches(r"\\?\")
                        .to_string();
                    self.start_session_at_root(canonical, None)
                        .await
                        .map(|_| ())
                }
            }
        }

        fn active_directory(&self) -> Option<String> {
            // The directory turns actually run in: the selected scope, else
            // (direct context) the launch directory.
            self.active_workspace_root()
        }

        /// `@` file completion for the composer. The search scope is the
        /// SELECTED canonical repository root (the same scope turns admit
        /// against — after a `/repo` switch the picker must never list the
        /// old tree); the listing itself comes from the canonical read-only
        /// discovery `list_files` runs
        /// (`rightkit_agent_coding_tools::list_files::discover_workspace_files`):
        /// gitignore-aware, noise-filtered, workspace-relative. No
        /// TUI-local scanner, no FileService, no separate ranking
        /// subsystem — the filter below is a bounded substring match and
        /// the popup renders its own fuzzy highlights. The walk runs off
        /// the async executor (`spawn_blocking`) and is capped, so a large
        /// tree is bounded; the drive loop polls discovery alongside input.
        async fn file_search(&self, query: &str) -> Vec<rightkit_terminal_ui::composer::FileMatch> {
            /// Walk cap: plenty for a completion source without letting a
            /// huge tree dominate a keystroke.
            const MAX_SCANNED: usize = 10_000;
            /// Popup row cap on the host side (the popup itself shows at
            /// most `MAX_POPUP_ROWS`; the extra headroom keeps ordering
            /// stable while the popup truncates).
            const MAX_RESULTS: usize = 64;

            let needle = query.trim();
            if needle.is_empty() {
                return Vec::new();
            }
            // The selected repository, else (D18: context unavailable, visibly
            // degraded) the folder turns actually run in — `@` attachment is a
            // file-system convenience and must not need a Membrane binding.
            let Some(root) = self.active_workspace_root().map(std::path::PathBuf::from) else {
                return Vec::new();
            };
            let needle = needle.to_string();
            let walk_root = root.clone();
            let discovered = tokio::task::spawn_blocking(move || {
                rightkit_agent_coding_tools::list_files::discover_workspace_files_bounded(
                    &walk_root,
                    &walk_root,
                    None,
                    MAX_SCANNED,
                    Some(std::time::Duration::from_millis(250)),
                )
            })
            .await;
            let Ok(Ok((paths, _))) = discovered else {
                return Vec::new();
            };

            // Bounded substring filter + rank (match position, then shorter
            // path — shallower/closer names win ties) with per-char indices
            // for the popup's highlight. `needle` is matched against the
            // lowercase-folded path; indices are char positions of the
            // match span in the folded string.
            let needle_chars: Vec<char> = needle.chars().flat_map(|c| c.to_lowercase()).collect();
            let mut ranked: Vec<(usize, usize, String, Vec<u32>)> = paths
                .into_iter()
                .filter_map(|path| {
                    // Attachment spelling is portable Markdown, not a native
                    // path display; normalize before matching and highlighting.
                    let path = path.replace('\\', "/");
                    let hay: Vec<char> = path.chars().flat_map(|c| c.to_lowercase()).collect();
                    let start = hay
                        .windows(needle_chars.len())
                        .position(|window| window == needle_chars.as_slice())?;
                    let indices = (start..start + needle_chars.len())
                        .map(|i| i as u32)
                        .collect();
                    Some((start, path.len(), path, indices))
                })
                .collect();
            ranked.sort();
            ranked
                .into_iter()
                .take(MAX_RESULTS)
                .map(
                    |(start, _len, path, indices)| rightkit_terminal_ui::composer::FileMatch {
                        score: start as u32,
                        path: std::path::PathBuf::from(&path),
                        match_type: rightkit_terminal_ui::composer::MatchType::File,
                        root: root.clone(),
                        indices: Some(indices),
                    },
                )
                .collect()
        }

        fn take_picker_request(&self) -> Option<crate::tui_host::PickerRequest> {
            self.picker_request
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
        }

        async fn project_catalog(&self) -> Vec<crate::tui_projects::ProjectEntry> {
            let rows = if crate::main_runtime::service_host_explicitly_selected() {
                match self.daemon_client().await {
                    Ok(client) => client.list_sessions().await.unwrap_or_default(),
                    Err(_) => Vec::new(),
                }
            } else {
                super::InlineHost::list_all_sessions(self.context_mode)
                    .await
                    .unwrap_or_default()
            };
            let mut projects: std::collections::BTreeMap<
                String,
                Vec<crate::tui_projects::ChatEntry>,
            > = std::collections::BTreeMap::new();
            for row in rows {
                let Some(id) = row.get("id").and_then(serde_json::Value::as_str) else {
                    continue;
                };
                let root = row
                    .get("workspaceRoot")
                    .or_else(|| row.get("workspace"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .trim();
                if root.is_empty() {
                    continue;
                }
                let title = session_picker_label(self.context_mode, &row)
                    .await
                    .unwrap_or_else(|| id.to_string());
                let updated = row
                    .get("updatedAt")
                    .or_else(|| row.get("updated_at"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string();
                projects.entry(root.to_string()).or_default().push(
                    crate::tui_projects::ChatEntry {
                        id: id.to_string(),
                        title,
                        updated,
                    },
                );
            }
            projects
                .into_iter()
                .map(|(root, mut chats)| {
                    chats.sort_by(|a, b| b.updated.cmp(&a.updated));
                    crate::tui_projects::ProjectEntry {
                        label: crate::tui_projects::project_label(&root),
                        root,
                        chats,
                    }
                })
                .collect()
        }

        fn current_session(&self) -> Option<String> {
            let id = self.rewind_session_id();
            (!id.is_empty()).then_some(id)
        }

        fn tui_settings(&self) -> crate::tui_projects::TuiSettings {
            crate::tui_projects::TuiSettings::from_config(&super::load_current_merged_config().tui)
        }

        async fn set_tui_setting(&self, key: &str, value: &str) -> Result<String, String> {
            let config_path = crate::tui_projects::user_config_path()
                .ok_or_else(|| "cannot determine user config path".to_string())?;
            crate::tui_projects::config_value(key, value)?;
            // Validate against a scratch config first, so an invalid key or value never reaches the
            // locked read-modify-write below.
            crate::tui_projects::apply_tui_setting(
                &mut rightkit_agent_config::HostConfig::default(),
                key,
                value,
            )?;
            let loader = rightkit_agent_config::ConfigLoader::new();
            // The user file alone, edited under the config lock shared with every other writer.
            loader
                .update_user_from(
                    |loader| match std::fs::read_to_string(&config_path) {
                        Ok(contents) => loader.load_str(&contents),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            Ok(rightkit_agent_config::HostConfig::default())
                        }
                        Err(error) => {
                            Err(rightkit_agent_config::ConfigError::Io(error.to_string()))
                        }
                    },
                    |config| {
                        let _ = crate::tui_projects::apply_tui_setting(config, key, value);
                    },
                )
                .map_err(|e| format!("save TUI setting: {e}"))?;
            Ok(format!("{key} updated"))
        }

        async fn goal_op(&self, op: &str, text: Option<&str>) -> Result<serde_json::Value, String> {
            let session_id = self.rewind_session_id();
            // Startup/maintenance only project existing goal state. Joining a
            // cold context build here blocks the terminal before its first frame.
            let host = if op == "get" && !super::service_host_explicitly_selected() {
                super::SessionExecutionHost::select_if_ready(self.context_mode)
                    .ok_or_else(|| "Goal state is not ready yet".to_string())?
            } else {
                super::SessionExecutionHost::select(self.context_mode).await?
            };
            if op != "get" {
                // A fresh TUI session has no durable/live row until its first turn is admitted;
                // goals are session state, so materialize the canonical root session first
                // (idempotent) instead of letting the goal route answer 404.
                // The selection guard must be released before `active_directory`
                // re-locks the same mutex (a held temporary guard deadlocked
                // `/goal set` and froze the whole TUI frame loop).
                let selected_root = self
                    .selected_repository
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .as_ref()
                    .map(|scope| scope.root.clone());
                let workspace_root = selected_root
                    .or_else(|| crate::tui_host::TurnHost::active_directory(self))
                    .ok_or_else(|| {
                        "goal needs a project: open one (Ctrl+B) or launch inside a folder"
                            .to_string()
                    })?;
                let (provider, model, effort) = self.next_repository_route();
                ensure_root_session(
                    self.context_mode,
                    &session_id,
                    &workspace_root,
                    &provider,
                    model.as_deref(),
                    effort,
                )
                .await?;
            }
            let mut body = host.goal_op(&session_id, op, text).await?;
            if op != "get" {
                // Mutation routes answer with an acknowledgement (`{"set":true,...}`),
                // not the goal snapshot the footer chip and status text project.
                if let Ok(snapshot) = host.goal_op(&session_id, "get", None).await {
                    body = snapshot;
                }
            }
            *self.goal_chip.lock().unwrap_or_else(|e| e.into_inner()) =
                crate::tui_projects::goal_chip(&body);
            if op == "set" {
                *self
                    .followup_prompt
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = text.map(str::to_string);
            }
            Ok(body)
        }

        async fn session_action(
            &self,
            action: &str,
            session_id: &str,
            value: Option<&str>,
        ) -> Result<String, String> {
            let host = super::SessionExecutionHost::select(self.context_mode).await?;
            let response = host.session_action(action, session_id, value).await?;
            if action == "export" {
                return response
                    .get("content")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| "session export returned no content".to_string());
            }
            Ok(match action {
                "clone" => format!(
                    "Cloned session {}",
                    response
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                ),
                "archive" => format!("Archived session {session_id}"),
                "delete" => format!("Deleted session {session_id}"),
                "rename" => format!("Renamed session {session_id}"),
                _ => response.to_string(),
            })
        }

        async fn insights(&self) -> Result<String, String> {
            let host = super::SessionExecutionHost::select(self.context_mode).await?;
            let root = self.active_directory();
            let result = host.insights(root.as_deref()).await?;
            serde_json::to_string_pretty(&result)
                .map_err(|error| format!("format Insights report: {error}"))
        }

        async fn session_log(&self, session_id: &str) -> Result<String, String> {
            let host = super::SessionExecutionHost::select(self.context_mode).await?;
            let events = host.events_after(session_id, 0).await?;
            let mut lines = Vec::with_capacity(events.len().min(500));
            for event in events.iter().rev().take(500).rev() {
                let seq = event
                    .get("seq")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or_default();
                let kind = event
                    .get("$kind")
                    .or_else(|| event.get("kind"))
                    .or_else(|| event.get("type"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown");
                let timestamp = event
                    .get("createdAt")
                    .or_else(|| event.get("created_at"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("-");
                lines.push(format!("{seq:>8}  {timestamp}  {kind}"));
            }
            if events.len() > 500 {
                lines.insert(
                    0,
                    format!("Showing latest 500 of {} durable events", events.len()),
                );
            }
            let body = if lines.is_empty() {
                "No durable events recorded yet.".to_string()
            } else {
                lines.join("\n")
            };
            Ok(format!("Session event trace · {session_id}\n{body}"))
        }

        async fn hooks_status(&self) -> Result<String, String> {
            let host = super::SessionExecutionHost::select(self.context_mode).await?;
            let status = host.hooks_status().await?;
            // Compact and redacted: a summary line plus one line per CONFIGURED hook (values
            // are never part of the status). The full per-event matrix stays at `/hooks/status`.
            let count = |key: &str| {
                status
                    .get(key)
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0)
            };
            let entries = status
                .get("hooks")
                .and_then(serde_json::Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut text = format!(
                "{} configured (command {}, url {}, mcp {}); {} hook slots available",
                count("totalConfigured"),
                count("commandConfigured"),
                count("urlConfigured"),
                count("mcpConfigured"),
                entries.len(),
            );
            for entry in entries.iter().filter(|entry| {
                entry.get("configured").and_then(serde_json::Value::as_bool) == Some(true)
            }) {
                text.push_str(&format!(
                    "
  {} ({}): configured via {}",
                    entry
                        .get("event")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("?"),
                    entry
                        .get("kind")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("?"),
                    entry
                        .get("envVar")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("?"),
                ));
            }
            Ok(text)
        }

        fn goal_chip(&self) -> Option<String> {
            self.goal_chip
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        }

        fn take_followup_prompt(&self) -> Option<String> {
            self.followup_prompt
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
        }

        fn context_degraded_reason(&self) -> Option<String> {
            if crate::main_runtime::service_host_explicitly_selected() {
                return self
                    .service_context_reason
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone();
            }
            super::SessionExecutionHost::select_if_ready(self.context_mode)
                .and_then(|host| host.context_degraded_reason())
        }

        async fn workspace_trust(
            &self,
            root: &str,
        ) -> Result<rightkit_agent_policy::WorkspaceTrust, String> {
            Ok(crate::main_runtime::workspace_trust_for(root))
        }

        async fn trust_workspace(&self, root: &str) -> Result<(), String> {
            crate::main_runtime::trust_workspace(root)
        }

        async fn workspace_diff(&self) -> Result<String, String> {
            use rightkit_agent_coding_tools::Tool;
            let root = self
                .active_directory()
                .ok_or_else(|| "No workspace is selected".to_string())?;
            let trust = crate::main_runtime::workspace_trust_for(&root);
            let workspace = std::path::PathBuf::from(&root);
            let context = rightkit_agent_coding_tools::ToolContext::new(
                self.current_session_id(),
                &workspace,
                rightkit_agent_policy::PermissionMode::Ask,
                trust,
            );
            let result = rightkit_agent_coding_tools::git_diff::GitDiffTool
                .execute(
                    serde_json::json!({"repo_path": workspace, "staged": false}),
                    &context,
                )
                .await;
            if result.is_error {
                Err(result.content)
            } else {
                Ok(result.content)
            }
        }

        async fn init_workspace(&self) -> Result<String, String> {
            let root = self
                .active_directory()
                .ok_or_else(|| "Select a workspace before running /init".to_string())?;
            let prompt = format!(
                "Initialize this workspace for RightKit. Inspect the repository, then create or update its project guidance file using the normal FileService tools available to this turn. Keep instructions specific to this repository, preserve existing useful guidance, and report what changed. Workspace: {root}"
            );
            let result = self
                .spawn_turn(prompt)
                .await
                .map_err(|error| format!("Workspace initialization turn task failed: {error}"))?;
            if result == 0 {
                Ok("Workspace initialization turn completed.".to_string())
            } else {
                Err(format!(
                    "Workspace initialization turn failed with exit code {result}"
                ))
            }
        }

        async fn worktree_action(&self, action: &str, args: &[&str]) -> Result<String, String> {
            let root = self
                .active_directory()
                .ok_or_else(|| "No workspace is selected".to_string())?;
            let host = super::SessionExecutionHost::select(self.context_mode).await?;
            let result = host.worktree_action(action, &root, args).await?;
            serde_json::to_string_pretty(&result)
                .map_err(|error| format!("format worktree result: {error}"))
        }

        fn spawn_agent_turn(
            &self,
            agent_id: &rightkit_terminal_ui::nav::AgentId,
            prompt: String,
        ) -> Result<tokio::task::JoinHandle<i32>, String> {
            let child_id = agent_id.0.clone();
            let context_mode = self.context_mode;
            let catalog = tokio::runtime::Handle::try_current()
                .map_err(|error| format!("agent chat needs an active runtime: {error}"))?;
            Ok(catalog.spawn(async move {
                let result = async {
                    let host = super::SessionExecutionHost::select(context_mode).await?;
                    let snapshot = host
                        .session_snapshot(&child_id)
                        .await?
                        .ok_or_else(|| format!("agent session `{child_id}` no longer exists"))?;
                    let workspace = snapshot
                        .get("workspaceRoot")
                        .or_else(|| snapshot.get("workspace"))
                        .and_then(serde_json::Value::as_str)
                        .filter(|value| !value.is_empty())
                        .ok_or_else(|| "agent session has no workspace root".to_string())?
                        .to_string();
                    let model = snapshot
                        .get("model")
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| "agent session has no model route".to_string())?
                        .to_string();
                    let provider = snapshot
                        .get("providerId")
                        .or_else(|| snapshot.get("provider"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let effort = snapshot
                        .get("effort")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string);
                    let mut args = build_turn_args(
                        prompt,
                        &child_id,
                        &provider,
                        Some(&model),
                        Some(workspace),
                        effort,
                        true,
                    );
                    args.permission_mode = Some(rightkit_agent_policy::PermissionMode::Ask);
                    Ok::<i32, String>(super::run_session(args).await)
                }
                .await;
                match result {
                    Ok(code) => code,
                    Err(error) => {
                        tracing::warn!(%error, "live agent chat failed");
                        1
                    }
                }
            }))
        }

        async fn rewind_points(
            &self,
        ) -> Result<Vec<crate::api_sessions::rewind::RewindPoint>, String> {
            let active_session = self.rewind_session_id();
            let host = super::SessionExecutionHost::select_if_ready(self.context_mode)
                .ok_or_else(|| "Nothing to rewind yet: the session has not started.".to_string())?;
            host.rewind_points(&active_session).await
        }

        async fn rewind_to(
            &self,
            point: &crate::api_sessions::rewind::RewindPoint,
            mode: crate::api_sessions::rewind::RewindMode,
        ) -> Result<crate::api_sessions::rewind::RewindReport, String> {
            let active_session = self.rewind_session_id();
            let host = super::SessionExecutionHost::select_if_ready(self.context_mode)
                .ok_or_else(|| "Nothing to rewind yet: the session has not started.".to_string())?;
            host.rewind_to(&active_session, point, mode).await
        }
    }

    // ── P1-5 live agent transcript: pure event→line/cursor helpers ──────────────
    //
    // Extracted from the host so the cursor-refresh and bounding properties are
    // unit-testable without a daemon or a terminal.

    /// How many events a transcript excerpt/poll renders at most: always the
    /// NEWEST window, never an unbounded dump.
    const AGENT_TRANSCRIPT_WINDOW: usize = 40;
    /// Per-event text clip length for display lines.
    const AGENT_EVENT_CLIP_CHARS: usize = 500;

    fn daemon_event_seq_of(event: &serde_json::Value) -> i64 {
        event
            .get("seq")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0)
    }

    /// Preserve daemon event families at TUI boundary. Unknown wire events
    /// remain explicit system entries rather than masquerading as assistant
    /// prose, so future daemon additions cannot silently flatten timeline
    /// semantics.
    fn project_owned_interaction(
        event: rightkit_terminal_ui::transcript::timeline::DaemonEvent,
        can_respond: bool,
    ) -> rightkit_terminal_ui::transcript::timeline::DaemonEvent {
        use rightkit_terminal_ui::transcript::timeline::DaemonEvent;
        if can_respond {
            return event;
        }
        match event {
            DaemonEvent::UserQuestionAsked { questions, .. } => DaemonEvent::System {
                text: format!(
                    "Historical question: {}",
                    questions
                        .iter()
                        .map(|question| question.question.as_str())
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            },
            DaemonEvent::PlanPresented { plan, .. } => DaemonEvent::System {
                text: format!("Historical plan: {plan}"),
            },
            event => event,
        }
    }

    /// Cumulative live output per running tool call (see `toolOutputDelta`).
    static LIVE_TOOL_TAILS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, String>>,
    > = std::sync::OnceLock::new();

    /// One-line, user-facing notice for an UNEXPECTED prompt-cache break
    /// (expected ones are suppressed; the durable event and `/usage` keep them).
    fn prompt_cache_break_notice(event: &serde_json::Value) -> String {
        let lost = event
            .get("tokensLost")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        let reason = event
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        let cost = event
            .get("estimatedCostLost")
            .and_then(serde_json::Value::as_f64)
            .filter(|cost| cost.is_finite() && *cost > 0.0)
            .map(|cost| format!(", ~${cost:.4}"))
            .unwrap_or_default();
        let detail = event
            .get("detail")
            .and_then(serde_json::Value::as_str)
            .filter(|detail| !detail.is_empty())
            .map(|detail| format!(": {detail}"))
            .unwrap_or_default();
        format!("Prompt cache miss ({reason}): {lost} tokens re-billed{cost}{detail}")
    }

    /// Clear system line for the live-only persistence latch event.
    fn persistence_failed_notice(event: &serde_json::Value) -> String {
        let reason = event
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .filter(|reason| !reason.is_empty())
            .unwrap_or("unknown error");
        format!(
            "Session persistence failed: {reason}. In-flight tools were cancelled and new work is not being saved."
        )
    }

    pub(super) fn daemon_event_to_timeline_event(
        event: &serde_json::Value,
    ) -> Option<rightkit_terminal_ui::transcript::timeline::DaemonEvent> {
        use rightkit_terminal_ui::transcript::timeline::{
            DaemonEvent, RunSignal, TaskStatus, ToolStatus,
        };

        let kind = event
            .get("$kind")
            .or_else(|| event.get("kind"))
            .or_else(|| event.get("type"))
            .and_then(serde_json::Value::as_str)?;
        let text = || {
            event
                .get("text")
                .or_else(|| event.get("content"))
                .or_else(|| event.get("message"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let id = |name: &str| {
            event
                .get(name)
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let status = |value: Option<&str>| match value.unwrap_or("running") {
            "succeeded" | "completed" | "done" | "passed" => TaskStatus::Succeeded,
            "failed" | "error" => TaskStatus::Failed,
            "cancelled" | "canceled" => TaskStatus::Cancelled,
            _ => TaskStatus::Running,
        };
        let event_status = event.get("status").and_then(serde_json::Value::as_str);
        // Control-plane dispatch records and usage bookkeeping are
        // internal-only: they carry orchestration/ledger data, not transcript
        // content, and must never surface as raw wire labels. The canonical
        // usage ledger itself lives in the durable event row (`/usage` reads
        // it); suppressing here removes only the `[usageUpdated]` diagnostic.
        if matches!(
            kind,
            "dispatchDecided"
                | "orchestrationObservationRecorded"
                | "agentQueryIssued"
                | "agentQueryResponded"
                | "artifactLineageUpdated"
                // Runtime-injected steering (post-edit verification, goal
                // continuation) is model-facing text, never user transcript.
                | "humanSteeringInjected"
                // Background self-review proposals are payload-only internal
                // events; their bare name must never reach the transcript.
                | "selfReviewProposalReceived"
        ) {
            return None;
        }
        // A deliberate cache break (compaction, model switch, explicit tool-set
        // change, idle TTL) is bookkeeping; only a silent money burn is shown.
        if kind == "promptCacheBreak"
            && event.get("expected").and_then(serde_json::Value::as_bool) == Some(true)
        {
            return None;
        }
        Some(match kind {
            "promptCacheBreak" => DaemonEvent::System {
                text: prompt_cache_break_notice(event),
            },
            "persistenceFailed" => DaemonEvent::System {
                text: persistence_failed_notice(event),
            },
            // Not shown as a line: the host keeps the latest figures for the
            // footer's context gauge (the full ledger stays in `/usage`).
            "usageUpdated" => {
                let number = |field: &str| {
                    event
                        .get(field)
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(0)
                };
                DaemonEvent::Usage {
                    context_size: number("contextTokens"),
                    context_permille: (event
                        .get("contextPercent")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(0.0)
                        * 1000.0)
                        .round() as i64,
                    cache_read: number("cacheReadTokens"),
                    // `promptTokens` is cumulative UNCACHED input; the footer's
                    // hit rate needs every input bucket as its denominator.
                    prompt: number("promptTokens")
                        + number("cacheReadTokens")
                        + number("cacheCreationTokens"),
                    cost_micros: event
                        .get("costKnown")
                        .and_then(serde_json::Value::as_bool)
                        .filter(|known| *known)
                        .and_then(|_| event.get("costUsd").and_then(serde_json::Value::as_f64))
                        .filter(|cost| cost.is_finite() && *cost >= 0.0)
                        .map(|cost| (cost * 1_000_000.0).round() as i64),
                    cost_reported: true,
                }
            }
            // Per-response accounting: same gauge feed as `usageUpdated`.
            "turnUsage" => {
                let number = |field: &str| {
                    event
                        .get(field)
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(0)
                };
                DaemonEvent::Usage {
                    context_size: number("contextUsedTokens"),
                    context_permille: (event
                        .get("contextUsedPct")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(0.0)
                        * 10.0)
                        .round() as i64,
                    cache_read: number("cachedInputTokens"),
                    prompt: number("inputTokens"),
                    cost_micros: None,
                    cost_reported: false,
                }
            }
            // Live shell tail: the exec cell consumes a CUMULATIVE preview,
            // so chunks accumulate per call here (bounded) and the settled
            // `toolResultAdded` replaces them with the authoritative output.
            "toolOutputDelta" => {
                let call_id = id("callId");
                let chunk = event
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let mut tails = LIVE_TOOL_TAILS
                    .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if tails.len() > 64 && !tails.contains_key(&call_id) {
                    tails.clear();
                }
                let tail = tails.entry(call_id.clone()).or_default();
                if tail.len() < 64 * 1024 {
                    tail.push_str(chunk);
                }
                DaemonEvent::ToolUpdated {
                    call_id,
                    status: ToolStatus::Running,
                    output_preview: tail.clone(),
                }
            }
            "runFailed" => DaemonEvent::Status(RunSignal::Failed {
                reason: id("error"),
                hint: id("kind"),
            }),
            "providerRetrying" => DaemonEvent::Status(RunSignal::Retrying {
                provider: id("provider"),
                reason: id("reason"),
                attempt: Some((
                    event
                        .get("attempt")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0) as u32,
                    event
                        .get("maxAttempts")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0) as u32,
                ))
                .filter(|(attempt, max)| *attempt > 0 && *max > 0),
                delay_secs: event
                    .get("delayMs")
                    .and_then(serde_json::Value::as_u64)
                    .filter(|ms| *ms > 0)
                    .map(|ms| ms.div_ceil(1000)),
            }),
            "sessionCompactionStarted" => DaemonEvent::Status(RunSignal::Compacting),
            "sessionCompacted" => DaemonEvent::Status(RunSignal::Compacted {
                dropped: event
                    .get("droppedCount")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0) as i32,
            }),
            "verificationFinished" => DaemonEvent::Status(RunSignal::Verification {
                passed: event
                    .get("passed")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                tool: id("tool"),
                issues: event
                    .get("issueCount")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0) as i32,
            }),
            "checkRunCompleted" => DaemonEvent::Status(RunSignal::Check {
                passed: event
                    .get("passed")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                jury_skipped: event
                    .get("jurySkipped")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
            }),
            // Admission backpressure is the one retry-after the runtime
            // emits; provider-level retries are tracing-only (no event).
            "overloaded" => DaemonEvent::Status(RunSignal::Retrying {
                provider: String::new(),
                reason: format!("server busy ({})", id("scope")),
                attempt: None,
                delay_secs: event
                    .get("retryAfterMs")
                    .and_then(serde_json::Value::as_u64)
                    .filter(|ms| *ms > 0)
                    .map(|ms| ms.div_ceil(1000)),
            }),
            "runInterrupted" => {
                let reason = id("reason");
                DaemonEvent::System {
                    text: if reason.is_empty() {
                        "Turn interrupted".to_string()
                    } else {
                        format!("Turn interrupted: {reason}")
                    },
                }
            }
            "userMessageSubmitted" | "userMessage" => DaemonEvent::UserMessage { text: text() },
            "assistantDelta" | "assistantMessageDelta" => {
                // The runtime streams reasoning summaries on the same event
                // with `isReasoning: true`; they are working notes, not prose.
                if event
                    .get("isReasoning")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
                {
                    DaemonEvent::ReasoningDelta { token: text() }
                } else {
                    DaemonEvent::AssistantDelta { token: text() }
                }
            }
            "assistantMessageCompleted" => match event
                .get("message")
                .and_then(|message| message.get("content"))
                .and_then(serde_json::Value::as_str)
            {
                Some(content) => DaemonEvent::AssistantCompleted {
                    text: content.to_string(),
                },
                None => DaemonEvent::AssistantSettled,
            },
            "sessionTurnFinished" => DaemonEvent::AssistantSettled,
            "reasoningDelta" | "thinkingDelta" => DaemonEvent::ReasoningDelta { token: text() },
            "reasoningCompleted" | "thinkingCompleted" => DaemonEvent::ReasoningSettled,
            "taskGraphUpdated" => DaemonEvent::TaskGraph {
                todos: event
                    .get("tasks")
                    .and_then(serde_json::Value::as_array)
                    .map(|tasks| {
                        tasks
                            .iter()
                            .filter_map(|t| {
                                let title = t.get("title")?.as_str()?.trim();
                                if title.is_empty() {
                                    return None;
                                }
                                let status =
                                    match t.get("state").and_then(serde_json::Value::as_str) {
                                        Some("done" | "Done") => "completed",
                                        Some("inProgress" | "InProgress") => "in_progress",
                                        _ => "pending",
                                    };
                                Some((title.to_string(), status.to_string()))
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            },
            "toolExecutionStarted" | "toolCallStarted" => {
                // Actual wire schema is nested:
                // `{ "$kind": "toolCallStarted", "call": {id, name,
                // argumentsJson} }`. Flat legacy fields remain as a
                // fallback for `toolExecutionStarted` producers.
                let call = event.get("call");
                let call_str = |field: &str| {
                    call.and_then(|c| c.get(field))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                };
                DaemonEvent::ToolStarted {
                    call_id: call_str("id").unwrap_or_else(|| id("toolCallId")),
                    name: call_str("name").unwrap_or_else(|| id("toolName")),
                    args_preview: call_str("argumentsJson")
                        .or_else(|| event.get("arguments").map(ToString::to_string))
                        .unwrap_or_default(),
                }
            }
            "toolExecutionCompleted" | "toolResult" | "toolResultAdded" => {
                // Canonical wire schema nests the settled transcript message:
                // `{ "$kind": "toolResultAdded", "result": { "$kind": "tool",
                //   toolCallId, summary, output?: {preview,truncated},
                //   isError } }`. Flat `toolCallId`/`status`/`content` fields
                // remain as a fallback for legacy `toolExecutionCompleted` /
                // `toolResult` producers.
                if let Some(tails) = LIVE_TOOL_TAILS.get() {
                    if let Some(done) = event
                        .pointer("/result/toolCallId")
                        .and_then(serde_json::Value::as_str)
                    {
                        tails.lock().unwrap_or_else(|e| e.into_inner()).remove(done);
                    }
                }
                let result = event.get("result");
                let result_str = |field: &str| {
                    result
                        .and_then(|r| r.get(field))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                };
                let result_error = result
                    .and_then(|r| r.get("isError"))
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                // The stored `output.preview` is the bounded, sanitized
                // presentation of a possibly-large payload; `summary` is the
                // compact fallback when no preview was persisted.
                let preview = result
                    .and_then(|r| r.pointer("/output/preview"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| result_str("summary"))
                    .unwrap_or_else(text);
                DaemonEvent::ToolUpdated {
                    call_id: result_str("toolCallId")
                        .or_else(|| {
                            event
                                .pointer("/call/id")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_owned)
                        })
                        .unwrap_or_else(|| id("toolCallId")),
                    status: if result_error
                        || matches!(event_status, Some("failed") | Some("error"))
                    {
                        ToolStatus::Failed
                    } else {
                        ToolStatus::Succeeded
                    },
                    output_preview: preview,
                }
            }
            "approvalRequested" | "toolApprovalRequested" => {
                let request_id = event
                    .pointer("/call/id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| id("requestId"));
                let summary = event
                    .get("call")
                    .map(|call| {
                        format!(
                            "{} {}",
                            call["name"].as_str().unwrap_or("tool"),
                            call["argumentsJson"]
                                .as_str()
                                .unwrap_or("[arguments unavailable]")
                        )
                    })
                    .unwrap_or_else(text);
                match event.get("call") {
                    // The canonical nested `call{id,name,argumentsJson}`
                    // payload rides along verbatim so the host can build a
                    // typed Exec/Patch preview instead of a flattened
                    // description.
                    Some(call) => DaemonEvent::ToolApprovalRequested {
                        request_id,
                        summary,
                        call_name: call
                            .get("name")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        arguments_json: call
                            .get("argumentsJson")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        policy_source: event
                            .get("policySource")
                            .and_then(serde_json::Value::as_str)
                            .filter(|source| !source.trim().is_empty())
                            .map(str::to_owned),
                    },
                    None => DaemonEvent::ApprovalRequested {
                        request_id,
                        summary,
                    },
                }
            }
            "approvalResolved" | "toolApprovalResolved" => DaemonEvent::ApprovalResolved {
                request_id: event
                    .get("toolCallId")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| id("requestId")),
                approved: event
                    .get("approved")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
            },
            "credentialRequested" => DaemonEvent::CredentialRequested {
                tool_call_id: id("toolCallId"),
                provider: id("provider"),
            },
            "credentialResolved" => DaemonEvent::CredentialResolved {
                tool_call_id: id("toolCallId"),
                outcome: id("outcome"),
            },
            "mcpElicitationRequested" => {
                let request = event.get("request");
                let payload = request.and_then(|value| value.get("request"));
                let mode = payload
                    .and_then(|value| value.get("mode"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("form")
                    .to_owned();
                let message = payload
                    .and_then(|value| value.get("message"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let schema = payload
                    .and_then(|value| value.get("requestedSchema"))
                    .map(serde_json::Value::to_string);
                let url = payload
                    .and_then(|value| value.get("url"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
                DaemonEvent::McpElicitationRequested {
                    server_name: request
                        .and_then(|value| value.get("serverName"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    request_id: request
                        .and_then(|value| value.get("id"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    mode,
                    message,
                    requested_schema: schema,
                    url,
                    elicitation_id: payload
                        .and_then(|value| value.get("elicitationId"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                }
            }
            "userQuestionAsked" => {
                let question_id = id("questionId");
                let questions = event
                    .get("questions")
                    .and_then(serde_json::Value::as_array)
                    .map(|questions| {
                        questions
                            .iter()
                            .map(|question| {
                                rightkit_terminal_ui::transcript::timeline::UserQuestionPrompt {
                                    header: question
                                        .get("header")
                                        .and_then(serde_json::Value::as_str)
                                        .unwrap_or("")
                                        .to_string(),
                                    question: question
                                        .get("question")
                                        .and_then(serde_json::Value::as_str)
                                        .unwrap_or("")
                                        .to_string(),
                                    options: question
                                        .get("options")
                                        .and_then(serde_json::Value::as_array)
                                        .map(|options| {
                                            options
                                                .iter()
                                                .filter_map(|option| {
                                                    option
                                                        .get("label")
                                                        .and_then(serde_json::Value::as_str)
                                                        .map(str::to_owned)
                                                })
                                                .collect()
                                        })
                                        .unwrap_or_default(),
                                    // Descriptions stay index-aligned with labels: a label-less option is dropped from both.
                                    descriptions: question
                                        .get("options")
                                        .and_then(serde_json::Value::as_array)
                                        .map(|options| {
                                            options
                                                .iter()
                                                .filter(|option| {
                                                    option
                                                        .get("label")
                                                        .and_then(serde_json::Value::as_str)
                                                        .is_some()
                                                })
                                                .map(|option| {
                                                    option
                                                        .get("description")
                                                        .and_then(serde_json::Value::as_str)
                                                        .unwrap_or("")
                                                        .to_owned()
                                                })
                                                .collect()
                                        })
                                        .unwrap_or_default(),
                                    multi_select: question
                                        .get("multiSelect")
                                        .and_then(serde_json::Value::as_bool)
                                        .unwrap_or(false),
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                DaemonEvent::UserQuestionAsked {
                    question_id,
                    questions,
                }
            }
            "userQuestionAnswered" => DaemonEvent::UserQuestionAnswered {
                question_id: id("questionId"),
            },
            "planPresented" => DaemonEvent::PlanPresented {
                plan_id: id("planId"),
                plan: event
                    .get("plan")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(text),
            },
            "planResolved" | "planApproved" | "planRejected" => DaemonEvent::PlanResolved {
                plan_id: id("planId"),
                approved: event
                    .get("approved")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(kind != "planRejected"),
            },
            "routeDecided" | "modelSelected" => DaemonEvent::RouteDecided {
                project: id("project"),
                model: id("model"),
                actor: id("actor"),
            },
            "taskUpdated" => DaemonEvent::TaskUpdated {
                task_id: id("taskId"),
                label: event
                    .get("label")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                status: status(event_status),
            },
            "subagentUpdated" | "subagentStatus" => DaemonEvent::SubagentUpdated {
                subagent_id: id("subagentId"),
                label: event
                    .get("label")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                status: status(event_status),
            },
            "checkReported" | "verificationCompleted" => DaemonEvent::CheckReported {
                check_id: id("checkId"),
                label: event
                    .get("label")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                passed: event
                    .get("passed")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                detail: text(),
            },
            // Unknown meaningful events become a bounded typed diagnostic —
            // a clipped system line — rather than being dropped or crashing
            // the stream.
            _ => {
                let mut detail: String = text().chars().take(200).collect();
                if text().chars().count() > 200 {
                    detail.push('…');
                }
                DaemonEvent::System {
                    text: if detail.is_empty() {
                        format!("[{kind}]")
                    } else {
                        format!("[{kind}] {detail}")
                    },
                }
            }
        })
    }

    /// Providers deliver the reasoning summary when the response completes, so
    /// the durable stream carries it after the answer text it preceded. Within
    /// one batch, move such reasoning back ahead of the first answer event of
    /// its segment (a segment ends at a user message, tool call, or notice) so
    /// a replayed turn reads thought, then answer, as it happened.
    fn reasoning_before_answer(
        events: Vec<rightkit_terminal_ui::transcript::timeline::DaemonEvent>,
    ) -> Vec<rightkit_terminal_ui::transcript::timeline::DaemonEvent> {
        use rightkit_terminal_ui::transcript::timeline::DaemonEvent;
        let mut out: Vec<DaemonEvent> = Vec::with_capacity(events.len());
        let mut answer_start: Option<usize> = None;
        for event in events {
            match &event {
                DaemonEvent::AssistantDelta { .. } | DaemonEvent::AssistantCompleted { .. } => {
                    answer_start.get_or_insert(out.len());
                    out.push(event);
                }
                DaemonEvent::ReasoningDelta { .. } | DaemonEvent::ReasoningSettled => {
                    match answer_start {
                        Some(at) => {
                            out.insert(at, event);
                            answer_start = Some(at + 1);
                        }
                        None => out.push(event),
                    }
                }
                DaemonEvent::UserMessage { .. }
                | DaemonEvent::ToolStarted { .. }
                | DaemonEvent::System { .. } => {
                    answer_start = None;
                    out.push(event);
                }
                _ => out.push(event),
            }
        }
        out
    }

    /// Preserve every durable sequence even when its event has no display text.
    /// A nonempty tool-only page is not end-of-history.
    fn history_page_entries(
        events: &[serde_json::Value],
    ) -> Vec<crate::tui_host::HistoryPageEntry> {
        events
            .iter()
            .map(|event| crate::tui_host::HistoryPageEntry {
                seq: daemon_event_seq_of(event),
                line: agent_event_display_line(event),
            })
            .collect()
    }

    /// Render one daemon event as a transcript display line, labelling its kind
    /// and clipping long text at a character (not byte) boundary. `None` for
    /// events with no renderable text.
    fn agent_event_display_line(event: &serde_json::Value) -> Option<String> {
        let kind = event
            .get("$kind")
            .or_else(|| event.get("kind"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let text = event
            .get("text")
            .or_else(|| event.get("content"))
            .or_else(|| event.pointer("/message/content"))
            .or_else(|| event.pointer("/result/output/preview"))
            .or_else(|| event.pointer("/result/summary"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if text.is_empty() {
            return None;
        }
        let label = match kind {
            "userMessageSubmitted" | "userMessage" => "user",
            "assistantMessageCompleted"
            | "assistantMessage"
            | "assistantDelta"
            | "assistantMessageDelta" => "assistant",
            "toolExecutionCompleted" | "toolResult" | "toolResultAdded" => "tool",
            _ => "event",
        };
        let clipped = if text.chars().count() > AGENT_EVENT_CLIP_CHARS {
            format!(
                "{}…",
                text.chars()
                    .take(AGENT_EVENT_CLIP_CHARS)
                    .collect::<String>()
            )
        } else {
            text.to_string()
        };
        Some(format!("[{label}] {clipped}"))
    }

    /// Longest a live delta waits for the committed facts it is anchored to.
    const LIVE_PROJECTION_HOLD: std::time::Duration = std::time::Duration::from_millis(750);

    /// Interleave live projections with committed facts. A projection carries
    /// the newest durable seq allocated when it was emitted, so it renders
    /// after the fact with that seq and before the next one.
    fn merge_live_projections(
        durable: Vec<serde_json::Value>,
        live: Vec<serde_json::Value>,
    ) -> Vec<serde_json::Value> {
        let mut merged = Vec::with_capacity(durable.len() + live.len());
        let mut live = live.into_iter().peekable();
        for fact in durable {
            let seq = daemon_event_seq_of(&fact);
            while live
                .peek()
                .is_some_and(|event| daemon_event_seq_of(event) < seq)
            {
                merged.extend(live.next());
            }
            merged.push(fact);
        }
        merged.extend(live);
        merged
    }

    /// The new last-seen cursor after consuming `events`: the highest sequence
    /// present, never moving backwards over an empty or seq-less batch.
    fn advance_agent_cursor(current: i64, events: &[serde_json::Value]) -> i64 {
        events
            .iter()
            .map(daemon_event_seq_of)
            .fold(current, i64::max)
    }

    /// Events strictly AFTER `cursor` — the increment a poll delivers, so a
    /// viewed agent's transcript is built cursor-first instead of replaying
    /// history from 0 every time.
    fn fresh_agent_events(events: &[serde_json::Value], cursor: i64) -> Vec<serde_json::Value> {
        events
            .iter()
            .filter(|event| daemon_event_seq_of(event) > cursor)
            .cloned()
            .collect()
    }

    /// The newest `cap` events of `events`, order preserved. Bounded so one poll
    /// can never flood the view regardless of how much the worker produced.
    fn newest_agent_events(events: &[serde_json::Value], cap: usize) -> Vec<&serde_json::Value> {
        let start = events.len().saturating_sub(cap);
        events[start..].iter().collect()
    }

    /// Map one daemon `AgentRosterRow` (JSON) to a TUI `AgentNode` for the P1-5
    /// tree. The daemon is the owner of state, model, and clocks; this only
    /// translates, never infers. `status` is `running|done|failed|cancelled`;
    /// `WaitingApproval` has no roster source today, so nothing fabricates it.
    fn agent_node_from_roster(
        row: &serde_json::Value,
        sub_id: String,
    ) -> rightkit_terminal_ui::nav::AgentNode {
        let role = row
            .get("role")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("subagent")
            .to_string();
        let model = row
            .get("model")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("-")
            .to_string();
        let elapsed_ms = row
            .get("elapsedMs")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let tokens_in = row
            .get("tokensIn")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let tokens_out = row
            .get("tokensOut")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let state = match row
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("done")
        {
            "running" => rightkit_terminal_ui::nav::AgentState::Running,
            "cancelled" | "failed" => rightkit_terminal_ui::nav::AgentState::Stopped,
            _ => rightkit_terminal_ui::nav::AgentState::Done,
        };
        let elapsed_cost = if elapsed_ms > 0 {
            let secs = elapsed_ms / 1000;
            let cost = if tokens_in + tokens_out > 0 {
                format!(" · {tokens_in}→{tokens_out} tok")
            } else {
                String::new()
            };
            format!("{secs}s{cost}")
        } else {
            String::new()
        };
        rightkit_terminal_ui::nav::AgentNode {
            id: rightkit_terminal_ui::nav::AgentId(sub_id),
            name: role,
            model,
            elapsed_cost,
            state,
            children: Vec::new(),
        }
    }

    /// Hard bound on agent-tree depth: the ancestor guard already terminates
    /// cyclic rosters, but a non-cyclic pathological hierarchy must also stay
    /// finite — deeper levels are simply not expanded.
    const AGENT_TREE_MAX_DEPTH: usize = 16;

    /// Recursively fold a sub-agent's roster into its node so the TUI agent tree
    /// spans the full daemon hierarchy — for either host kind through the
    /// shared `SessionExecutionHost::get_session_agents` seam. `seen` holds the
    /// ancestor ids so a cyclic roster (a session listing an ancestor as its
    /// own descendant) terminates instead of recursing forever; depth is
    /// additionally capped by [`AGENT_TREE_MAX_DEPTH`]. The host is the owner
    /// of structure; this only reads and translates.
    async fn build_agent_subtree(
        host: &super::SessionExecutionHost,
        sub_session_id: &str,
        seen: &mut Vec<String>,
        node: &mut rightkit_terminal_ui::nav::AgentNode,
    ) {
        if seen.iter().any(|id| id == sub_session_id) || seen.len() >= AGENT_TREE_MAX_DEPTH {
            return;
        }
        seen.push(sub_session_id.to_string());
        let Ok(roster) = host.get_session_agents(sub_session_id).await else {
            return;
        };
        for row in roster {
            let sub_id = row
                .get("subSessionId")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string();
            if sub_id.is_empty() || seen.iter().any(|id| id == &sub_id) {
                continue;
            }
            let mut child = agent_node_from_roster(&row, sub_id.clone());
            Box::pin(build_agent_subtree(host, &sub_id, seen, &mut child)).await;
            node.children.push(child);
        }
    }

    impl EngineTurnHost {
        /// In-session `/onboard`: the SAME flow as `{app} onboard`
        /// (`run_onboard_command` in `main_runtime_entry.rs`) —
        /// `detect_from_config` -> `run_onboarding` with OsKeyStore /
        /// RealReadinessRunner / ConfigPersister. There is exactly one
        /// onboarding implementation; both surfaces reach it.
        ///
        /// # Terminal contention — resolved
        ///
        /// The TUI holds the terminal in raw mode and its input thread reads
        /// the tty concurrently. Before this lane wired a pause seam, raw mode
        /// could be lifted for the flow's line-based prompts but the input
        /// thread could not, so keystrokes raced the prompt. Now the host
        /// shares its input-pause flag with the TUI's input thread
        /// (`attach_input_pause`) and flips it for the whole flow: the thread
        /// stops reading (see `spawn_input_thread` in `tui_host.rs`), the
        /// drive loop drains-and-drops events that were already queued, and
        /// the flag is restored on every exit path via an RAII guard. This
        /// in-session surface is therefore equivalent to `{app} onboard`
        /// in a fresh terminal, not best-effort.
        ///
        /// Credential invariant: this wrapper never touches credential
        /// values — they live inside `run_onboarding`, which stores them via
        /// the OS keychain and never prints them (guarded by onboarding's own
        /// tests). Outcome reporting below only names provider/model/errors,
        /// never secrets.
        fn run_onboarding_inline(&self) {
            use crate::main_runtime::onboarding::{
                detect_from_config_with_native_auth, load_current_config, run_onboarding,
                ConfigPersister, OnboardOutcome, PromptIo, RealReadinessRunner, TerminalIo,
            };
            use rightkit_terminal_ui::crossterm::terminal as ct_terminal;

            // Pause the TUI's input thread for the whole flow: raw mode is
            // lifted so the line-based prompts behave, and no crossterm event
            // may race the prompt for a keystroke. The flag stays set until
            // the flow returns and raw mode is re-armed.
            self.pause_input();
            let _pause_guard = crate::tui_host::InputPauseGuard::new(|| self.resume_input());

            let was_raw = ct_terminal::is_raw_mode_enabled().unwrap_or(false);
            if was_raw {
                let _ = ct_terminal::disable_raw_mode();
            }
            println!();
            println!(
                "[hint] onboarding is taking over this terminal for the next few prompts. TUI input is paused; keystrokes go to the prompts. '{app} onboard' in a fresh terminal is equivalent.", app = crate::host_identity::app()
            );
            println!();
            let _ = std::io::stdout().flush();

            // `execute_command` is sync inside the session's async runtime, so
            // the async flow runs on a dedicated thread with its own
            // current-thread runtime (a nested `block_on` on the ambient
            // runtime would panic).
            let outcome = std::thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("failed to build onboarding runtime")
                    .block_on(async {
                        let mut io = TerminalIo;
                        let config = load_current_config();
                        let backend = rightkit_llm::agent_providers::OsKeyStore;
                        let state = detect_from_config_with_native_auth(&backend, &config).await;
                        if state.needs_onboarding() {
                            io.write(
                                "RightKit needs provider configuration before it can run a turn.\n",
                            );
                        } else {
                            io.write(
                                "RightKit already has a working configuration — onboarding will confirm before changing anything.\n",
                            );
                        }
                        run_onboarding(
                            &state,
                            &backend,
                            &RealReadinessRunner,
                            &ConfigPersister,
                            &mut io,
                        )
                        .await
                    })
            })
            .join()
            .unwrap_or(OnboardOutcome::Failed {
                detail: "onboarding thread panicked".to_string(),
            });

            match outcome {
                OnboardOutcome::Completed { provider, model } => {
                    // P1-10: successful onboarding must update provider/model
                    // used by later turns in the same TUI process.
                    if let Ok(mut turn_model) = self.turn_model.lock() {
                        turn_model.request_switch_for_next_turn(
                            provider.clone(),
                            model.clone(),
                            rightkit_agent_orchestration::coding::PrefixChangeReason::UserRequestedSwitch,
                        );
                    }
                    println!("Onboarding complete — next turns will use {provider}/{model}.");
                    println!("The switch applies to turns started after this point.");
                }
                OnboardOutcome::Cancelled => {
                    println!("Onboarding cancelled — nothing was changed.");
                }
                OnboardOutcome::Failed { detail } => {
                    println!("Onboarding failed: {detail}");
                    println!("Nothing was changed. Fix the error and re-run /onboard.");
                }
            }
            let _ = std::io::stdout().flush();
            if was_raw {
                let _ = ct_terminal::enable_raw_mode();
            }
        }

        /// Take the exclusive tty with a bounded acknowledgement. A dead
        /// reader must degrade interaction rather than freezing TUI forever.
        fn pause_input(&self) {
            if let Ok(handle) = self.input_pause_handle.lock() {
                if let Some(pause) = handle.as_ref() {
                    if !pause.request_with_deadline(std::time::Duration::from_secs(2)) {
                        tracing::warn!(
                            "tui input pause acknowledgement timed out; continuing degraded"
                        );
                    }
                }
            }
            // Record that a drain is owed: the drive loop is blocked while this
            // host surface runs, so it cannot drain the stale queued events
            // during the pause. It will read this flag when the command
            // returns and drop them then.
            self.input_drain_pending
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }

        /// Release the acknowledged pause; the TUI reader resumes on its next
        /// cycle.
        fn resume_input(&self) {
            if let Ok(handle) = self.input_pause_handle.lock() {
                if let Some(pause) = handle.as_ref() {
                    pause.resume();
                }
            }
        }
    }

    /// Picker row metadata from one session-list row: age from `updatedAt`,
    /// workspace short name from `workspaceRoot`, first-message `preview`.
    fn session_picker_detail(
        row: &serde_json::Value,
        now_secs: i64,
    ) -> rightkit_terminal_ui::nav::SessionDetail {
        let field = |key: &str| {
            row.get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string()
        };
        let root = field("workspaceRoot");
        let workspace = root
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("")
            .to_string();
        rightkit_terminal_ui::nav::SessionDetail {
            age: session_age_label(&field("updatedAt"), now_secs).unwrap_or_default(),
            workspace,
            preview: field("preview"),
        }
    }

    /// Human age such as `3h ago`; `None` when the timestamp does not parse.
    fn session_age_label(updated_at: &str, now_secs: i64) -> Option<String> {
        let then = rfc3339_utc_secs(updated_at)?;
        let delta = (now_secs - then).max(0);
        Some(match delta {
            0..=59 => "just now".to_string(),
            60..=3599 => format!("{}m ago", delta / 60),
            3600..=86_399 => format!("{}h ago", delta / 3600),
            _ => format!("{}d ago", delta / 86_400),
        })
    }

    /// Unix seconds for an RFC 3339 timestamp (optional fraction, then `Z` or a `+hh:mm`/`-hh:mm`
    /// offset); `None` when it does not parse.
    fn rfc3339_utc_secs(ts: &str) -> Option<i64> {
        rightkit_time::DateTime::parse_rfc3339(ts).map(|parsed| parsed.unix_seconds())
    }
}
