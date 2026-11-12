// Refactor-1 KRN session-execution host seam.
//
// This sits below CLI/TUI presentation. `EngineTurnHost` remains a terminal
// adapter; ordinary turns converge here through `main_runtime::run_session`.
// InlineHost reuses the same in-process `DaemonState`/`LiveSession` runtime
// without binding HTTP. ServiceHost is the existing daemon transport.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use axum::extract::{Path as AxumPath, Query as AxumQuery, State as AxumState};
use axum::http::HeaderMap;
use axum::response::Json as AxumJson;
use rightkit_agent_engine_core::conduit::{
    seq_of, ConduitReceive, ConduitReplay, ConduitSubscription,
};
use rightkit_agent_engine_protocol::{ProtocolEvent, ToolCall};

const RUN_HOST_ENV: &str = "RIGHTKIT_RUN_HOST";

/// Per-presentation-turn cancellation. Completion is published only before
/// admission or after the engine has settled; dropping a feed is not completion.
struct RunCancellation {
    host: OnceLock<(SessionExecutionHost, String)>,
    requested: tokio_util::sync::CancellationToken,
    admission_started: std::sync::atomic::AtomicBool,
    completion: tokio::sync::watch::Sender<Option<Result<(), String>>>,
    /// Live projection events (token/tool-output deltas) observed on this
    /// turn's live conduit stream, drained by the interactive presentation.
    /// The durable journal does not store them (EVT-002).
    live_projections: Mutex<VecDeque<serde_json::Value>>,
}

impl RunCancellation {
    fn new() -> Arc<Self> {
        let (completion, _) = tokio::sync::watch::channel(None);
        Arc::new(Self {
            host: OnceLock::new(),
            requested: tokio_util::sync::CancellationToken::new(),
            admission_started: std::sync::atomic::AtomicBool::new(false),
            completion,
            live_projections: Mutex::new(VecDeque::new()),
        })
    }

    /// Bound on undrained live projections; the oldest are dropped first
    /// (a projection may be dropped without changing settled truth).
    const LIVE_PROJECTION_CAP: usize = 4096;

    fn push_live_projection(&self, event: &serde_json::Value) {
        if !matches!(
            event.get("$kind").and_then(serde_json::Value::as_str),
            Some("assistantDelta" | "toolOutputDelta")
        ) {
            return;
        }
        let mut queue = self
            .live_projections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if queue.len() >= Self::LIVE_PROJECTION_CAP {
            queue.pop_front();
        }
        queue.push_back(event.clone());
    }

    fn drain_live_projections(&self) -> Vec<serde_json::Value> {
        self.live_projections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
            .collect()
    }

    async fn cancel_and_wait(&self) -> Result<(), String> {
        self.requested.cancel();
        let mut completion = self.completion.subscribe();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if let Some(result) = completion.borrow_and_update().clone() {
                    return result;
                }
                completion
                    .changed()
                    .await
                    .map_err(|_| "turn completion channel closed".to_string())?;
            }
        })
        .await
        .map_err(|_| {
            "Turn cancellation is still awaiting engine settlement; replacement was not started."
                .to_string()
        })?
    }

    fn settled(&self) {
        self.completion.send_replace(Some(Ok(())));
    }

    /// Await until cancellation is requested. Used for cancellable
    /// pre-admission waits (e.g. context readiness) that run before any
    /// engine turn is actually admitted, so a cancel arriving during that
    /// wait resolves promptly instead of only being observable afterward.
    async fn cancelled(&self) {
        self.requested.cancelled().await
    }

    fn require_settled(&self) -> Result<(), String> {
        self.completion.borrow().clone().unwrap_or_else(|| {
            Err("Previous engine turn has not settled; new turn was not started.".to_string())
        })
    }

    fn finish(&self) {
        if self.completion.borrow().is_none() {
            let result = if self
                .admission_started
                .load(std::sync::atomic::Ordering::Acquire)
            {
                Err("Turn feed ended without confirmed engine settlement.".to_string())
            } else {
                Ok(())
            };
            self.completion.send_replace(Some(result));
        }
    }
}

struct RunCompletionGuard(Arc<RunCancellation>);

impl Drop for RunCompletionGuard {
    fn drop(&mut self) {
        self.0.finish();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunHostMode {
    Inline,
    Service,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum CliHostRoute {
    LocalUtility,
    InlineHost,
    ServiceHost,
    ServiceProjection,
}

/// Production-owned top-level CLI capability classification used by entry
/// dispatch for host-sensitive projections. The table below remains a
/// qualification matrix covering every command and material mode.
pub(crate) fn classify_cli_host_route(command: &Command) -> CliHostRoute {
    classify_cli_host_route_with_service_selection(command, service_host_explicitly_selected())
}

/// Classify a command without reading process state. Production dispatch
/// supplies explicit ServiceHost selection; tests cover both choices without
/// mutating a shared environment variable.
fn classify_cli_host_route_with_service_selection(
    command: &Command,
    service_selected: bool,
) -> CliHostRoute {
    match command {
        Command::Serve { .. } => CliHostRoute::ServiceHost,
        Command::Run { .. } => {
            if service_selected {
                CliHostRoute::ServiceHost
            } else {
                CliHostRoute::InlineHost
            }
        }
        Command::McpServer { session_id, .. } => {
            if session_id.is_some() {
                CliHostRoute::ServiceProjection
            } else {
                CliHostRoute::InlineHost
            }
        }
        Command::AcpServer { .. }
        | Command::Remote { .. }
        | Command::Schedule { .. }
        | Command::Browser { .. }
        | Command::Bot { .. }
        | Command::Skill { .. }
        | Command::Learning { .. }
        | Command::Diagnostics { .. }
        | Command::Status { .. }
        | Command::Stop { .. }
        | Command::Ps { .. }
        | Command::Sessions { .. }
        | Command::Agents { .. }
        | Command::Resume { .. }
        | Command::Fork { .. }
        | Command::Session { .. }
        | Command::Logs { .. }
        | Command::Attach { .. }
        | Command::Conduct { .. }
        | Command::Kill { .. }
        | Command::Handoff { .. }
        | Command::App { .. }
        | Command::Export { .. }
        | Command::Review { .. }
        | Command::Mcp { .. }
        | Command::Plugin { .. }
        | Command::Permission { .. }
        | Command::Provider { .. }
        | Command::LocalModel { .. }
        | Command::Hooks { .. }
        | Command::Toolset { .. }
        | Command::Execution { .. }
        | Command::CliRuntime { .. } => CliHostRoute::ServiceProjection,
        Command::Insights { .. } => CliHostRoute::ServiceProjection,
        Command::Spec { command } => match command {
            SpecCommand::Clarify { .. }
            | SpecCommand::Plan { .. }
            | SpecCommand::Checklist { .. }
            | SpecCommand::Implement { .. } => CliHostRoute::ServiceProjection,
            _ => CliHostRoute::LocalUtility,
        },
        Command::Onboard
        | Command::Trust { .. }
        | Command::Doctor { .. }
        | Command::Models { .. }
        | Command::Config
        | Command::Migrate { .. }
        | Command::Info { .. }
        | Command::Completion { .. }
        | Command::Caps { .. }
        | Command::Diff { .. }
        | Command::Help { .. } => CliHostRoute::LocalUtility,
        Command::Recipe { command } => match command {
            RecipeCommand::Run { .. } => {
                if service_selected {
                    CliHostRoute::ServiceHost
                } else {
                    CliHostRoute::InlineHost
                }
            }
            _ => CliHostRoute::LocalUtility,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostCapabilityRequirement {
    OrdinaryLocal,
    /// Detached/process-surviving residency; ordinary durable storage stays inline.
    Persistence,
    MultiClient,
    Remote,
    Background,
    Scheduled,
    ExplicitService,
}

impl HostCapabilityRequirement {
    const fn host_mode(self) -> RunHostMode {
        match self {
            Self::OrdinaryLocal => RunHostMode::Inline,
            Self::Persistence
            | Self::MultiClient
            | Self::Remote
            | Self::Background
            | Self::Scheduled
            | Self::ExplicitService => RunHostMode::Service,
        }
    }
}

fn parse_run_host_mode(raw: Option<&str>) -> Result<RunHostMode, String> {
    match raw.map(str::trim).filter(|value| !value.is_empty()) {
        None | Some("inline") | Some("local") => {
            Ok(HostCapabilityRequirement::OrdinaryLocal.host_mode())
        }
        Some("service") | Some("daemon") => {
            Ok(HostCapabilityRequirement::ExplicitService.host_mode())
        }
        Some(other) => Err(format!(
            "invalid {RUN_HOST_ENV}={other:?}; expected inline or service"
        )),
    }
}

/// Whether this process explicitly selected ServiceHost for ordinary run/TUI
/// execution. Presentation/lifecycle adapters use this to avoid taking daemon
/// presence/discovery leases on the default InlineHost path.
pub(crate) fn service_host_explicitly_selected() -> bool {
    let raw = std::env::var(RUN_HOST_ENV).ok();
    matches!(
        parse_run_host_mode(raw.as_deref()),
        Ok(RunHostMode::Service)
    )
}

/// The value composed once (per context mode) by [`InlineHost::build`] and
/// cached by [`inline_host_initializer`]'s single-flight initializer.
type InlineHostState = (api::DaemonState, ());

/// Compose the memory backend for a fresh inline `DaemonState` through the
/// Membrane CLI adapter. Pulse absent resolves to the adapter's `Unavailable`
/// backend: context off with `rightkit_memory_client::membrane::OFF_REASON`, memory save
/// an explicit tool error. Every adapter call carries its own timeout.
fn apply_membrane_cli_composition(mut state: api::DaemonState) -> api::DaemonState {
    state = state.with_membrane(crate::native_transcript_exporter::membrane_backend());
    // Same transcript exporter wiring as ServiceHost; inert when Pulse is absent.
    state = state.with_transcript_exporter(Some(Arc::new(
        crate::native_transcript_exporter::NativeTranscriptExporter::new(),
    )));
    match rightkit_memory_client::backend::native::connect_from_environment() {
        Ok(backend) => {
            let provenance =
                rightkit_memory_client::memory_provenance::MemoryProvenanceStore::from_backend(
                    backend.clone(),
                );
            state = state
                .with_memory_backend(backend)
                .with_memory_provenance(provenance);
            // Pulse absent connects to the typed unavailable backend; the
            // context is still off and must be reported degraded.
            if rightkit_memory_client::backend::native::context_is_off() {
                state =
                    state.with_context_degraded(rightkit_memory_client::membrane::OFF_REASON.to_string());
            }
        }
        Err(error) => {
            let reason = rightkit_memory_client::membrane::OFF_REASON.to_string();
            tracing::warn!(host = "inline", error = %error, "Membrane CLI context unavailable; turns run degraded (context off)");
            state = state
                .with_memory_backend(std::sync::Arc::new(
                    rightkit_memory_client::backend::unavailable::UnavailableMemoryBackend::new(
                        error.code(),
                        error.message(),
                    ),
                ))
                .with_memory_provenance(
                    rightkit_memory_client::memory_provenance::MemoryProvenanceStore::unavailable(
                        reason.clone(),
                    ),
                )
                .with_context_degraded(reason);
        }
    }
    state
}

/// The process-wide single-flight initializer for one `ContextMode`'s
/// InlineHost composition. Replaces the permanent-failure-caching
/// `OnceLock<Result<...>>` this used to be: a failed attempt is never
/// cached, concurrent callers share one in-flight attempt, and a stale
/// cached value can be forced to rebuild via `invalidate()` (used when a
/// state must be rebuilt).
fn inline_host_initializer(
    context_mode: ContextMode,
) -> &'static crate::inline_host_init::InlineHostInitializer<InlineHostState> {
    static MEMBRANE_INIT: OnceLock<
        crate::inline_host_init::InlineHostInitializer<InlineHostState>,
    > = OnceLock::new();
    static DIRECT_INIT: OnceLock<crate::inline_host_init::InlineHostInitializer<InlineHostState>> =
        OnceLock::new();
    match context_mode {
        ContextMode::Membrane => MEMBRANE_INIT.get_or_init(Default::default),
        ContextMode::Direct => DIRECT_INIT.get_or_init(Default::default),
    }
}

/// Keep an already-built InlineHost's in-memory trust view in step with the
/// canonical persisted store after an explicit TUI/CLI trust decision.
pub(crate) fn sync_inline_workspace_trust(workspace_root: &str) {
    let key = api::workspace_trust_key(workspace_root);
    if key.is_empty() {
        return;
    }
    let Some((state, _)) = inline_host_initializer(ContextMode::effective()).peek() else {
        return;
    };
    let mut store = state
        .workspace_trust_store
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    store.projects.insert(
        key,
        crate::api_stores::WorkspaceTrustRecord {
            trust: "trusted".to_string(),
            approved_by: Some("user".to_string()),
            updated_at: rightkit_agent_engine_protocol::now_iso(),
        },
    );
}

#[derive(Clone)]
struct InlineHost {
    state: api::DaemonState,
}

impl InlineHost {
    fn build(context_mode: ContextMode) -> Result<InlineHostState, String> {
        // InlineHost owns a session-lifetime Membrane holder while retaining
        // local execution. ServiceHost has an independent holder path.
        let config = load_current_merged_config();
        let db_path = default_db_path();
        let (storage, migration_report) =
            crate::daemon::migrate::open_with_migration_report(&db_path)
                .map_err(|error| format!("open inline RightKit storage: {error}"))?;
        if migration_report.applied_migrations {
            tracing::info!(
                host = "inline",
                "database migrations applied on inline-host startup"
            );
        }

        let mut state = api::DaemonState::new(generate_token())
            .with_context_mode(context_mode)
            .with_runtime_info(0, db_path.clone())
            .with_runtime_middleware_store_path(runtime_middleware_store_path(&db_path))
            .with_recipe_store_path(recipe_store_path(&db_path))
            .with_skillopt_store_path(skillopt_store_path(&db_path))
            .with_spec_store_path(spec_store_path(&db_path))
            .with_storage(api::SharedStorage::new(storage));

        // Seed budget ceilings and iteration limits from config exactly as
        // ServiceHost startup does, so an inline run honours the same
        // `[orchestration.budget]` the daemon would.
        {
            let budget = &config.orchestration.budget;
            let mut s = state.settings.lock().unwrap_or_else(|e| e.into_inner());
            s.per_task_max_usd = budget.per_task_max_usd;
            s.per_goal_max_usd = budget.per_goal_max_usd;
            s.per_session_max_usd = budget.per_session_max_usd;
            s.per_agent_call_max_usd = budget.per_agent_call_max_usd;
            s.max_retries_per_role = budget.max_retries_per_role;
            s.max_wall_clock_minutes = budget.max_wall_clock_minutes;
            s.max_iterations = budget.max_iterations;
            s.max_workers = budget.max_workers;
        }

        // Use the same current provider-resolution contract as ServiceHost.
        // Per-session model selection is still applied by create_session_handler.
        if let Some(mut engine_cfg) = resolve_engine_config(&config, None, None) {
            engine_cfg.fallback_models = config.model.fallback_models.clone();
            state = state.with_engine(engine_cfg);
        }

        // Membrane is Pulse's CLI only (Orthic-Labs/pulse#26); absence is typed-unavailable.
        state = apply_membrane_cli_composition(state);

        if let Err(error) = state.register_provider_extensions() {
            tracing::warn!(host = "inline", error = %error, "provider extension registration unavailable");
        }

        Ok((state, ()))
    }

    /// Resolve InlineHost's composed state through the shared single-flight
    /// initializer (see `inline_host_init.rs`). A cached success
    /// is never rebuilt; a cached
    /// failure is never returned twice -- the next call always retries.
    async fn shared(context_mode: ContextMode) -> Result<Self, String> {
        let (state, ()) = inline_host_initializer(context_mode)
            .ensure(move || Self::build(context_mode))
            .await?;
        Ok(Self { state })
    }

    /// Warm the cached composition for `context_mode` without acquiring a
    /// resident lease. Presentation hosts (`EngineTurnHost`) call this as
    /// soon as the TUI starts, so the slow native connect (which can block
    /// for minutes on a cold worker) runs off the first turn's critical
    /// path instead of stalling the first submitted message. `shared()`
    /// still performs its own -- cheap once the cache is warm -- lease
    /// acquisition per call; this only ensures the expensive part is done.
    async fn warm(context_mode: ContextMode) -> Result<Option<String>, String> {
        inline_host_initializer(context_mode)
            .ensure(move || Self::build(context_mode))
            .await
            .map(|(state, _)| state.context_degraded_reason().map(str::to_string))
    }

    /// Read the canonical session projection from InlineHost's existing
    /// composition and durable store. This is a local read-only view; it does
    /// not discover/bind ServiceHost or create another persistence authority.
    async fn list_sessions_for_workspace(
        context_mode: ContextMode,
        workspace_root: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        // Never builds: this feeds navigation from the terminal loop, which
        // must not wait on (or retry) a slow context build. Until the state
        // exists the caller shows its local stub.
        let (state, _) = inline_host_initializer(context_mode)
            .peek()
            .ok_or_else(|| "inline host state is not built yet".to_string())?;
        let AxumJson(rows) = crate::api_sessions::lifecycle::list_sessions(
            AxumState(state),
            AxumQuery(crate::api_sessions::lifecycle::SessionListQuery::default()),
        )
        .await
        .map_err(|status| format!("list inline sessions: HTTP {status}"))?;
        let workspace = std::path::Path::new(workspace_root);
        Ok(rows
            .into_iter()
            .filter(|row| {
                row["workspaceRoot"]
                    .as_str()
                    .is_some_and(|root| api::same_path(root, workspace))
            })
            .collect())
    }

    /// Every durable session row (all workspaces) from the already-built
    /// inline composition -- the project/chat browser's feed. Never builds.
    async fn list_all_sessions(
        context_mode: ContextMode,
    ) -> Result<Vec<serde_json::Value>, String> {
        let (state, _) = inline_host_initializer(context_mode)
            .peek()
            .ok_or_else(|| "inline host state is not built yet".to_string())?;
        let AxumJson(rows) = crate::api_sessions::lifecycle::list_sessions(
            AxumState(state),
            AxumQuery(crate::api_sessions::lifecycle::SessionListQuery::default()),
        )
        .await
        .map_err(|status| format!("list inline sessions: HTTP {status}"))?;
        Ok(rows)
    }

    async fn ensure_session(
        &self,
        args: &RunArgs,
        session_id: &str,
        workspace_root: &str,
    ) -> Result<(), String> {
        if let Some(existing) = self.state.get_session(session_id) {
            if args.safe_mode && !existing.safe_mode {
                return Err(format!(
                    "safe mode cannot resume unrestricted inline session {session_id}"
                ));
            }
            return Ok(());
        }

        if let Some(existing) = self
            .state
            .resume_session_with_ws(session_id, Some(workspace_root))
            .await
        {
            if args.safe_mode && !existing.safe_mode {
                return Err(format!(
                    "safe mode cannot resume unrestricted inline session {session_id}"
                ));
            }
            return Ok(());
        }

        // One canonical mapping from CLI RunArgs -> session admission payload
        // is shared with ServiceHost. Do not let host selection change policy,
        // tool ceilings, safe-mode, model, or workspace semantics.
        let request: api::CreateSession =
            serde_json::from_value(daemon_session_create_body(args, session_id, workspace_root))
                .map_err(|error| format!("build inline session request: {error}"))?;
        crate::api_sessions::lifecycle::create_session_handler(
            AxumState(self.state.clone()),
            AxumJson(request),
        )
        .await
        .map(|_| ())
        .map_err(|status| format!("create inline session {session_id}: HTTP {status}"))
    }

    async fn submit_message(
        &self,
        session_id: &str,
        prompt: &str,
        workspace_root: &str,
    ) -> Result<i64, String> {
        self.submit_message_with_attachments(session_id, prompt, workspace_root, None, None)
            .await
    }

    async fn submit_message_with_attachments(
        &self,
        session_id: &str,
        prompt: &str,
        workspace_root: &str,
        attachments: Option<Vec<api::Attachment>>,
        sandbox_grant_prompts: Option<bool>,
    ) -> Result<i64, String> {
        let request: api::PostMessage = serde_json::from_value(serde_json::json!({
            "text": prompt,
            "workspaceRoot": workspace_root,
            "attachments": attachments,
            "sandboxGrantPrompts": sandbox_grant_prompts,
        }))
        .map_err(|error| format!("build inline message request: {error}"))?;
        match crate::api_sessions::messages::post_message(
            AxumState(self.state.clone()),
            AxumPath(session_id.to_string()),
            HeaderMap::new(),
            AxumJson(request),
        )
        .await
        {
            Ok(AxumJson(response)) => Ok(response
                .get("seq")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0)),
            Err((status, AxumJson(error))) => {
                let detail = error
                    .get("detail")
                    .or_else(|| error.get("error"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("inline message admission failed");
                Err(format!("submit inline message: HTTP {status}: {detail}"))
            }
        }
    }

    async fn open_event_stream(
        &self,
        session_id: &str,
        after: i64,
    ) -> Result<InlineEventStream, String> {
        let live = self
            .state
            .get_session(session_id)
            .ok_or_else(|| format!("inline session {session_id} is not live"))?;

        // Match the daemon SSE race-proof ordering: subscribe first, then replay.
        let subscription = live.conduit.subscribe();
        let mut pending = VecDeque::new();
        match live.conduit.replay_after(after) {
            ConduitReplay::Events(events) => pending.extend(events),
            ConduitReplay::ResyncRequired {
                after_seq,
                oldest_available_seq,
                high_water_mark,
            } => pending.push_back(inline_resync_event(
                session_id,
                after_seq,
                oldest_available_seq,
                high_water_mark,
            )),
        }

        Ok(InlineEventStream {
            live,
            subscription,
            pending,
            last_delivered_seq: after,
        })
    }

    async fn events_after(
        &self,
        session_id: &str,
        after: i64,
    ) -> Result<Vec<serde_json::Value>, String> {
        let query = api::AfterQuery {
            after,
            kind: None,
            before: None,
            limit: None,
        };
        match crate::api_sessions::sse::get_events_history(
            AxumState(self.state.clone()),
            AxumPath(session_id.to_string()),
            AxumQuery(query),
        )
        .await
        {
            Ok(AxumJson(events)) => events
                .into_iter()
                .map(|event| serde_json::to_value(event).map_err(|error| error.to_string()))
                .collect(),
            Err((status, AxumJson(error))) => Err(format!(
                "read inline event history: HTTP {status}: {}",
                error
                    .get("detail")
                    .or_else(|| error.get("error"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("history unavailable")
            )),
        }
    }

    /// Bounded backwards history page over the durable event log. Reuses
    /// `api_sessions::sse::get_events_history`'s `before`/`limit` scrollback
    /// semantics verbatim: `Storage::events_before` (exclusive `before_seq`
    /// cursor, ascending page order, finite limit), the corrupt-payload /
    /// storage-fault continuity error, and the missing-session distinction.
    /// This never substitutes `events_after` and never materializes the
    /// full log; an exhausted page returns an empty JSON array.
    async fn fetch_history(
        &self,
        session_id: &str,
        before_seq: i64,
        limit: usize,
    ) -> Result<serde_json::Value, String> {
        let query = api::AfterQuery {
            after: 0,
            kind: None,
            before: Some(before_seq),
            limit: Some(limit),
        };
        match crate::api_sessions::sse::get_events_history(
            AxumState(self.state.clone()),
            AxumPath(session_id.to_string()),
            AxumQuery(query),
        )
        .await
        {
            Ok(AxumJson(events)) => serde_json::to_value(events)
                .map_err(|error| format!("encode inline history page: {error}")),
            Err((status, AxumJson(error))) => Err(format!(
                "read inline history page: HTTP {status}: {}: {}",
                error
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("history_unavailable"),
                error
                    .get("detail")
                    .or_else(|| error.get("error"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("history unavailable")
            )),
        }
    }

    /// Execute one slash command in-process through the same dispatch flow
    /// the ServiceHost `/commands/execute` route uses
    /// (`api_commands::execute_command_on_state`): shared-table name
    /// normalization, daemon executors (`execute_daemon_command`), shared
    /// `CommandRegistry`, then plugin commands. Returns the canonical
    /// `CommandResult` JSON; there is no second command executor.
    async fn execute_command(
        &self,
        input: &str,
        session_id: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let result =
            crate::api_commands::execute_command_on_state(&self.state, input, session_id).await;
        serde_json::to_value(result)
            .map_err(|error| format!("encode inline command result: {error}"))
    }
}

/// Projection used by the standalone MCP presentation. It owns no tool
/// executor: schemas come from the live session's catalog and calls are sent
/// through that session's normal governed external-call seam.
pub(crate) struct InlineMcpProjection {
    state: api::DaemonState,
    session_id: String,
    vendor_grants: Mutex<Vec<(String, serde_json::Value, String, std::time::Instant)>>,
}

impl InlineMcpProjection {
    async fn for_vendor(
        workspace: &std::path::Path,
        session_id: &str,
        mode: rightkit_agent_policy::PermissionMode,
        control: &RunCancellation,
    ) -> Result<(Self, InlineEventStream), String> {
        let host = InlineHost::shared(ContextMode::Membrane).await?;
        if host.state.get_session(session_id).is_none() {
            let request = serde_json::from_value(serde_json::json!({
                "id": session_id, "title": "Vendor tools",
                "workspaceRoot": workspace.to_string_lossy(),
                "safeMode": false, "permissionMode": mode, "mcpChannel": "cli"
            }))
            .map_err(|error| format!("build vendor session: {error}"))?;
            let _ = crate::api_sessions::lifecycle::create_session_handler(
                AxumState(host.state.clone()),
                AxumJson(request),
            )
            .await
            .map_err(|status| format!("create vendor session: HTTP {status}"))?;
        }
        let live = host
            .state
            .get_session(session_id)
            .ok_or("vendor session unavailable")?;
        crate::api_permissions::apply_live_permission_mode(&host.state, &live, mode);
        let stream = host
            .open_event_stream(session_id, live.conduit.current_high_water())
            .await?;
        let projection = Self {
            state: host.state.clone(),
            session_id: session_id.to_string(),
            vendor_grants: Mutex::new(Vec::new()),
        };
        control
            .host
            .set((
                SessionExecutionHost::Inline(Box::new(host)),
                session_id.to_string(),
            ))
            .map_err(|_| "vendor host already bound".to_string())?;
        Ok((projection, stream))
    }

    pub(crate) async fn permission_prompt(
        &self,
        request: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let input = request
            .get("input")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let name = request
            .get("tool_name")
            .and_then(serde_json::Value::as_str)
            .and_then(|name| name.strip_prefix(&format!("mcp__{app}__", app = crate::host_identity::app())));
        let Some(name) = name.filter(|name| *name != "permission_prompt") else {
            return Ok(
                serde_json::json!({"behavior": "deny", "message": "Only RightKit tools are permitted"}),
            );
        };
        if !input.is_object() {
            return Ok(serde_json::json!({"behavior": "deny", "message": "Invalid tool input"}));
        }
        {
            let mut grants = self
                .vendor_grants
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            grants.retain(|(_, _, _, issued)| issued.elapsed().as_secs() < 300);
            if grants
                .iter()
                .any(|(tool, args, _, _)| tool == name && args == &input)
            {
                return Ok(
                    serde_json::json!({"behavior": "deny", "message": "Identical approval is already awaiting execution"}),
                );
            }
        }
        let call_id = format!(
            "mcp:{}:{}",
            self.session_id,
            rightkit_agent_engine_protocol::uuid_v7()
        );
        let result = self
            .dispatch_call(call_id.clone(), name, input.clone(), true)
            .await?;
        if result.is_error {
            return Ok(serde_json::json!({"behavior": "deny", "message": result.content}));
        }
        let mut grants = self
            .vendor_grants
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        grants.retain(|(_, _, _, issued)| issued.elapsed().as_secs() < 300);
        if grants.len() >= 64 {
            return Ok(
                serde_json::json!({"behavior": "deny", "message": "Too many outstanding approvals"}),
            );
        }
        grants.push((
            name.to_string(),
            input.clone(),
            call_id,
            std::time::Instant::now(),
        ));
        Ok(serde_json::json!({"behavior": "allow", "updatedInput": input}))
    }

    pub(crate) async fn build(
        workspace: &std::path::Path,
        allow_mutate: bool,
    ) -> Result<Self, String> {
        let host = InlineHost::shared(ContextMode::Membrane).await?;
        Self::build_with_state_impl(host.state, workspace, allow_mutate).await
    }

    async fn build_with_state_impl(
        state: api::DaemonState,
        workspace: &std::path::Path,
        allow_mutate: bool,
    ) -> Result<Self, String> {
        let permission_mode = if allow_mutate { "bypass" } else { "ask" };
        Self::build_with_state_and_mode(state, workspace, allow_mutate, permission_mode).await
    }

    async fn build_with_state_and_mode(
        state: api::DaemonState,
        workspace: &std::path::Path,
        allow_mutate: bool,
        permission_mode: &str,
    ) -> Result<Self, String> {
        let session_id = format!("mcp-{}", rightkit_agent_engine_protocol::uuid_v7());
        let toolset = (!allow_mutate).then_some("read-only");
        let request: api::CreateSession = serde_json::from_value(serde_json::json!({
            "id": session_id,
            "title": "MCP stdio",
            "workspaceRoot": workspace.to_string_lossy(),
            "toolset": toolset,
            "safeMode": !allow_mutate,
            "permissionMode": permission_mode,
            "mcpChannel": "cli"
        }))
        .map_err(|error| format!("build inline MCP session request: {error}"))?;
        let _ = crate::api_sessions::lifecycle::create_session_handler(
            AxumState(state.clone()),
            AxumJson(request),
        )
        .await
        .map_err(|status| format!("create inline MCP session: HTTP {status}"))?;
        Ok(Self {
            state,
            session_id,
            vendor_grants: Mutex::new(Vec::new()),
        })
    }

    pub(crate) async fn tools(&self) -> Result<Vec<serde_json::Value>, String> {
        // Session creation starts the engine task asynchronously. Wait briefly for its
        // creation-time catalog closure to bind before publishing MCP schemas.
        for _ in 0..200 {
            let live = self
                .state
                .get_session(&self.session_id)
                .ok_or_else(|| "inline MCP session is no longer live".to_string())?;
            if let Some(registry) = live.tool_catalog.snapshot() {
                return Ok(registry
                    .all()
                    .iter()
                    .map(|tool| {
                        serde_json::json!({
                            "name": tool.name(),
                            "description": tool.description(),
                            "inputSchema": tool.input_schema(),
                            "annotations": { "readOnlyHint": tool.is_read_only() }
                        })
                    })
                    .collect());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        Err("inline MCP tool catalog is unavailable".to_string())
    }

    pub(crate) async fn call(
        &self,
        request_id: &serde_json::Value,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let call_id = {
            let mut grants = self
                .vendor_grants
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            grants.retain(|(_, _, _, issued)| issued.elapsed().as_secs() < 300);
            grants
                .iter()
                .position(|(tool, input, _, _)| tool == name && input == &arguments)
                .map(|index| grants.remove(index).2)
                .unwrap_or_else(|| {
                    format!(
                        "mcp:{}:{}",
                        self.session_id,
                        request_id
                            .as_str()
                            .map(ToString::to_string)
                            .unwrap_or_else(|| request_id.to_string())
                    )
                })
        };
        let result = self.dispatch_call(call_id, name, arguments, false).await?;
        Ok(serde_json::json!({
            "content": [{"type": "text", "text": result.content}],
            "isError": result.is_error
        }))
    }

    async fn dispatch_call(
        &self,
        call_id: String,
        name: &str,
        arguments: serde_json::Value,
        permission_only: bool,
    ) -> Result<rightkit_agent_tools::coding_tools::ToolResult, String> {
        let live = self
            .state
            .get_session(&self.session_id)
            .ok_or_else(|| "inline MCP session is no longer live".to_string())?;
        let sender = live
            .driver_tool_tx
            .as_ref()
            .ok_or_else(|| "inline MCP governed executor is unavailable".to_string())?;
        let (response, receiver) = tokio::sync::oneshot::channel();
        sender
            .send(crate::api::DriverToolRequest {
                permission_only,
                call: ToolCall {
                    id: call_id,
                    name: name.to_string(),
                    arguments_json: arguments.to_string(),
                },
                response,
            })
            .await
            .map_err(|_| "inline MCP governed executor stopped".to_string())?;
        receiver
            .await
            .map_err(|_| "inline MCP governed executor dropped its result".to_string())
    }
}

/// Host-composed MCP child-process launcher shared by daemon reloads and the
/// inline MCP execution host. Callers fail closed when resource authority is
/// unavailable; no presentation surface gets a native-launch fallback.
pub(crate) fn governed_mcp_process_launcher(
) -> Result<Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter>, String> {
    let governor = rightkit_agent_tools::coding_tools::process_resource_authority::require_process_resource_governor()
        .map_err(|error| format!("MCP process authority unavailable: {error}"))?;
    Ok(Arc::new(
        rightkit_agent_tools::coding_tools::process_resource_authority::GovernedProcessLaunchAdapter::new(
            // MCP callers declare confinement through ProcessLaunchContext.
            // Honor normal-mode launches & fail closed for required sandboxing.
            rightkit_agent_tools::coding_tools::process_launch::LocalProcessLaunchAdapter,
            governor,
            rightkit_agent_tools::coding_tools::process_resource_authority::ResourceKind::Mcp,
            true,
        ),
    ))
}

/// Host-composed launcher for plugin child processes. The native adapter keeps
/// delivery sandbox preparation intact; governor admission remains attached to
/// returned child until wait/cancel/drop completes.
pub(crate) fn governed_child_process_launcher(
) -> Result<Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter>, String> {
    let governor = rightkit_agent_tools::coding_tools::process_resource_authority::require_process_resource_governor()
        .map_err(|error| format!("child-process authority unavailable: {error}"))?;
    Ok(Arc::new(
        rightkit_agent_tools::coding_tools::process_resource_authority::GovernedProcessLaunchAdapter::new(
            rightkit_agent_tools::coding_tools::shell::NativeCommandSandboxAdapter,
            governor,
            rightkit_agent_tools::coding_tools::process_resource_authority::ResourceKind::ChildProcess,
            true,
        ),
    ))
}

#[derive(Clone)]
struct ServiceHost {
    client: Arc<DaemonSessionClient>,
    context_degraded_reason: Option<String>,
}

impl ServiceHost {
    async fn connect() -> Result<Self, String> {
        let (base_url, token) = crate::cli_presence::resolve_daemon_or_autostart()
            .await
            .ok_or_else(|| {
                format!("No running ServiceHost found and automatic start failed. Start one with `{app} serve`.", app = crate::host_identity::app())
                    .to_string()
            })?;
        let client = Arc::new(DaemonSessionClient::new(base_url, token));
        let context_degraded_reason = client.context_degraded_reason().await.ok().flatten();
        Ok(Self {
            client,
            context_degraded_reason,
        })
    }
}

#[derive(Clone)]
enum SessionExecutionHost {
    Inline(Box<InlineHost>),
    Service(ServiceHost),
}

impl SessionExecutionHost {
    /// The inline host if its composed state is already built -- never builds
    /// one. A read-only presentation poll has nothing to read before the
    /// state exists, and must not wait on (or retry) a slow context build.
    fn select_if_ready(context_mode: ContextMode) -> Option<Self> {
        let raw = std::env::var(RUN_HOST_ENV).ok();
        match parse_run_host_mode(raw.as_deref()).ok()? {
            RunHostMode::Inline => inline_host_initializer(context_mode)
                .peek()
                .map(|(state, _source)| Self::Inline(Box::new(InlineHost { state }))),
            RunHostMode::Service => None,
        }
    }

    async fn session_snapshot(
        &self,
        session_id: &str,
    ) -> Result<Option<serde_json::Value>, String> {
        let rows = match self {
            Self::Inline(host) => {
                let AxumJson(rows) = crate::api_sessions::lifecycle::list_sessions(
                    AxumState(host.state.clone()),
                    AxumQuery(crate::api_sessions::lifecycle::SessionListQuery::default()),
                )
                .await
                .map_err(|status| format!("list inline sessions: HTTP {status}"))?;
                rows
            }
            Self::Service(host) => host.client.list_sessions().await?,
        };
        Ok(rows
            .into_iter()
            .find(|row| row["id"].as_str() == Some(session_id)))
    }

    async fn get_session_agents(&self, session_id: &str) -> Result<Vec<serde_json::Value>, String> {
        match self {
            Self::Inline(host) => {
                let AxumJson(rows) = crate::api_sessions::orchestration::get_session_agents(
                    AxumState(host.state.clone()),
                    AxumPath(session_id.to_string()),
                )
                .await
                .map_err(|status| format!("read inline agent roster: HTTP {status}"))?;
                rows.as_array()
                    .cloned()
                    .ok_or_else(|| "agent roster is not an array".to_string())
            }
            Self::Service(host) => host.client.get_session_agents(session_id).await,
        }
    }

    async fn update_route(
        &self,
        session_id: &str,
        model: &str,
        provider_id: Option<&str>,
        effort: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let body = serde_json::json!({"model": model, "providerId": provider_id, "effort": effort});
        match self {
            Self::Inline(host) => {
                let request = serde_json::from_value(body)
                    .map_err(|error| format!("build route update: {error}"))?;
                let _ = crate::api_sessions::lifecycle::patch_session(
                    AxumState(host.state.clone()),
                    AxumPath(session_id.to_owned()),
                    HeaderMap::new(),
                    AxumJson(request),
                )
                .await
                .map_err(|status| format!("update inline route: HTTP {status}"))?;
            }
            Self::Service(host) => {
                host.client.patch_route(session_id, &body).await?;
            }
        }
        self.session_snapshot(session_id)
            .await?
            .ok_or_else(|| format!("session {session_id} disappeared after route update"))
    }

    /// Goal control (`/goal`) over the same handlers the daemon routes use.
    /// `op`: `get|set|edit|pause|resume|clear`. `text` is the goal text for
    /// `set`/`edit`. Returns the route's JSON body.
    async fn goal_op(
        &self,
        session_id: &str,
        op: &str,
        text: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let body = serde_json::json!({ "text": text });
        match self {
            Self::Inline(host) => {
                let state = AxumState(host.state.clone());
                let id = AxumPath(session_id.to_owned());
                let fail = |status: axum::http::StatusCode| format!("goal {op}: HTTP {status}");
                let result = match op {
                    "get" => crate::api_sessions::messages::get_goal_snapshot(state, id).await,
                    "clear" => {
                        crate::api_sessions::messages::clear_goal(state, id, HeaderMap::new()).await
                    }
                    "pause" => {
                        crate::api_sessions::messages::pause_goal(state, id, HeaderMap::new()).await
                    }
                    "resume" => {
                        crate::api_sessions::messages::resume_goal(state, id, HeaderMap::new())
                            .await
                    }
                    "set" => {
                        let request = serde_json::from_value(body)
                            .map_err(|error| format!("build goal request: {error}"))?;
                        crate::api_sessions::messages::set_goal(
                            state,
                            id,
                            HeaderMap::new(),
                            AxumJson(request),
                        )
                        .await
                    }
                    "edit" => {
                        let request = serde_json::from_value(body)
                            .map_err(|error| format!("build goal edit: {error}"))?;
                        crate::api_sessions::messages::edit_goal(
                            state,
                            id,
                            HeaderMap::new(),
                            AxumJson(request),
                        )
                        .await
                    }
                    other => return Err(format!("unknown goal operation `{other}`")),
                };
                result.map(|AxumJson(value)| value).map_err(fail)
            }
            Self::Service(host) => host.client.goal_op(session_id, op, &body).await,
        }
    }

    async fn session_action(
        &self,
        action: &str,
        session_id: &str,
        value: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        match self {
            Self::Service(host) => host.client.session_action(action, session_id, value).await,
            Self::Inline(host) => {
                let state = AxumState(host.state.clone());
                let path = AxumPath(session_id.to_owned());
                match action {
                    "rename" => {
                        let body = serde_json::from_value(
                            serde_json::json!({"title": value.unwrap_or_default()}),
                        )
                        .map_err(|error| format!("build rename request: {error}"))?;
                        crate::api_sessions::lifecycle::patch_session(
                            state,
                            path,
                            HeaderMap::new(),
                            AxumJson(body),
                        )
                        .await
                        .map(|AxumJson(json)| json)
                        .map_err(|status| format!("rename session: HTTP {status}"))
                    }
                    "archive" => {
                        crate::api_sessions::lifecycle::update_session_lifecycle_status(
                            &host.state,
                            session_id,
                            "archived",
                        )
                        .await
                        .map_err(|status| format!("archive session: HTTP {status}"))?;
                        Ok(serde_json::json!({"id":session_id,"status":"archived"}))
                    }
                    "delete" => crate::api_sessions::lifecycle::delete_session_handler(
                        state,
                        path,
                        HeaderMap::new(),
                    )
                    .await
                    .map(|AxumJson(json)| json)
                    .map_err(|status| format!("delete session: HTTP {status}")),
                    "clone" => {
                        let body = serde_json::from_value(serde_json::json!({"title": value}))
                            .map_err(|error| format!("build clone request: {error}"))?;
                        crate::api_sessions::lifecycle::clone_session_handler(
                            state,
                            path,
                            HeaderMap::new(),
                            AxumJson(body),
                        )
                        .await
                        .map(|AxumJson(json)| json)
                        .map_err(|status| format!("clone session: HTTP {status}"))
                    }
                    "export" => {
                        let query = serde_json::from_value(
                            serde_json::json!({"format":value.unwrap_or("md")}),
                        )
                        .map_err(|error| format!("build export query: {error}"))?;
                        crate::api_sessions::export::export_session_handler(
                            state,
                            path,
                            AxumQuery(query),
                        )
                        .await
                        .map(|AxumJson(json)| json)
                        .map_err(|status| format!("export session: HTTP {status}"))
                    }
                    _ => Err(format!("unsupported session action `{action}`")),
                }
            }
        }
    }

    async fn insights(&self, workspace_root: Option<&str>) -> Result<serde_json::Value, String> {
        let body = serde_json::json!({
            "sessionIds": [],
            "workspaceRoot": workspace_root,
            "scope": "workspace",
            "minRecurrence": 2,
        });
        match self {
            Self::Service(host) => host.client.adapt_insights(&body).await,
            Self::Inline(host) => {
                let request = serde_json::from_value(body)
                    .map_err(|error| format!("build Insights request: {error}"))?;
                crate::api_runtime::post_adapt_insights(
                    AxumState(host.state.clone()),
                    AxumJson(request),
                )
                .await
                .map(|AxumJson(value)| value)
                .map_err(|(status, AxumJson(detail))| {
                    format!("Adapt insights: HTTP {status}: {detail}")
                })
            }
        }
    }

    async fn hooks_status(&self) -> Result<serde_json::Value, String> {
        match self {
            Self::Inline(_) => Ok(crate::hooks::current_hook_status_value()),
            Self::Service(host) => host.client.hooks_status().await,
        }
    }

    async fn worktree_action(
        &self,
        action: &str,
        workspace_root: &str,
        args: &[&str],
    ) -> Result<serde_json::Value, String> {
        let body = match action {
            "list" => serde_json::json!({"workspaceRoot": workspace_root}),
            "add" => {
                let Some(branch) = args.first() else {
                    return Err("usage: /worktree add <branch> [base] [path]".into());
                };
                serde_json::json!({"workspaceRoot":workspace_root,"branch":branch,"base":args.get(1),"path":args.get(2)})
            }
            "remove" => {
                let Some(path) = args.first() else {
                    return Err("usage: /worktree remove <path>".into());
                };
                let active = std::path::PathBuf::from(workspace_root).canonicalize().ok();
                let requested = std::path::PathBuf::from(path);
                let requested = if requested.is_absolute() {
                    requested
                } else {
                    std::path::PathBuf::from(workspace_root).join(requested)
                };
                if active.is_some() && requested.canonicalize().ok().as_ref() == active.as_ref() {
                    return Err("refusing to remove the active workspace worktree".into());
                }
                serde_json::json!({"workspaceRoot":workspace_root,"path":path})
            }
            _ => return Err("usage: /worktree <list|add|remove>".into()),
        };
        match self {
            Self::Service(host) => host.client.worktree_action(action, &body).await,
            Self::Inline(host) => match action {
                "list" => {
                    let query = serde_json::from_value(body)
                        .map_err(|error| format!("build worktree query: {error}"))?;
                    crate::api_delivery::list_worktrees_handler(
                        AxumState(host.state.clone()),
                        AxumQuery(query),
                    )
                    .await
                    .map(|AxumJson(value)| value)
                    .map_err(|status| format!("list worktrees: HTTP {status}"))
                }
                "add" => {
                    let request = serde_json::from_value(body)
                        .map_err(|error| format!("build worktree request: {error}"))?;
                    crate::api_delivery::create_worktree_handler(
                        AxumState(host.state.clone()),
                        HeaderMap::new(),
                        AxumJson(request),
                    )
                    .await
                    .map(|AxumJson(value)| value)
                    .map_err(|status| format!("add worktree: HTTP {status}"))
                }
                "remove" => {
                    let request = serde_json::from_value(body)
                        .map_err(|error| format!("build worktree request: {error}"))?;
                    crate::api_delivery::remove_worktree_handler(
                        AxumState(host.state.clone()),
                        HeaderMap::new(),
                        AxumJson(request),
                    )
                    .await
                    .map(|AxumJson(value)| value)
                    .map_err(|status| format!("remove worktree: HTTP {status}"))
                }
                _ => Err(format!("unsupported worktree action `{action}`")),
            },
        }
    }

    async fn init_workspace(&self, session_id: &str) -> Result<serde_json::Value, String> {
        self.execute_command("/init", Some(session_id)).await
    }

    async fn resolve_approval(
        &self,
        session_id: &str,
        tool_call_id: &str,
        approved: bool,
    ) -> Result<(), String> {
        self.resolve_approval_body(
            session_id,
            serde_json::json!({
                "toolCallId": tool_call_id, "approved": approved,
            }),
        )
        .await
    }

    async fn resolve_approval_decision(
        &self,
        session_id: &str,
        tool_call_id: &str,
        decision: rightkit_agent_engine_protocol::ReviewDecision,
    ) -> Result<(), String> {
        self.resolve_approval_body(session_id, serde_json::json!({
            "toolCallId": tool_call_id, "approved": decision.is_approved(), "decision": decision,
        })).await
    }

    async fn resolve_approval_body(
        &self,
        session_id: &str,
        response: serde_json::Value,
    ) -> Result<(), String> {
        match self {
            Self::Inline(host) => {
                let body = serde_json::from_value(response)
                    .map_err(|error| format!("build approval response: {error}"))?;
                crate::api_sessions::approvals::resolve_tool_approval(
                    AxumState(host.state.clone()),
                    AxumPath(session_id.to_owned()),
                    HeaderMap::new(),
                    AxumJson(body),
                )
                .await
                .map(|_| ())
                .map_err(|status| format!("resolve inline approval: HTTP {status}"))
            }
            Self::Service(host) => {
                host.client
                    .respond_interaction(session_id, "/tool-approval", &response)
                    .await
            }
        }
    }

    async fn permission_rules(&self, session_id: &str) -> Result<Vec<(String, String)>, String> {
        match self {
            Self::Inline(host) => {
                let session = crate::api_permissions::list_permission_rules_handler(
                    AxumState(host.state.clone()),
                    AxumPath(session_id.to_owned()),
                )
                .await
                .map_err(|status| format!("read session permission rules: HTTP {status}"))?
                .0;
                let persistent = crate::api_permissions::list_persistent_permission_rules_handler(
                    AxumState(host.state.clone()),
                    AxumQuery(crate::api_permissions::PersistentPermissionRuleQuery {
                        scope: Some("all".to_string()),
                        workspace_root: None,
                    }),
                )
                .await
                .map_err(|status| format!("read persistent permission rules: HTTP {status}"))?
                .0;
                Ok(permission_rule_pairs(&session, &persistent))
            }
            Self::Service(host) => host.client.permission_rules(session_id).await,
        }
    }

    async fn install_permission_rule(
        &self,
        session_id: &str,
        pattern: &str,
        workspace_root: &str,
    ) -> Result<(), String> {
        match self {
            Self::Inline(host) => {
                crate::api_permissions::add_permission_rule_handler(
                    AxumState(host.state.clone()),
                    AxumPath(session_id.to_owned()),
                    HeaderMap::new(),
                    AxumJson(
                        serde_json::from_value(
                            serde_json::json!({"pattern": pattern, "effect": "allow"}),
                        )
                        .map_err(|error| format!("build permission rule: {error}"))?,
                    ),
                )
                .await
                .map_err(|status| format!("install session permission rule: HTTP {status}"))?;
                crate::api_permissions::add_persistent_permission_rule_handler(
                    AxumState(host.state.clone()),
                    HeaderMap::new(),
                    AxumJson(serde_json::from_value(serde_json::json!({
                        "scope": "project", "workspaceRoot": workspace_root, "pattern": pattern, "effect": "allow"
                    })).map_err(|error| format!("build permission rule: {error}"))?),
                )
                .await
                .map(|_| ())
                .map_err(|status| format!("install project permission rule: HTTP {status}"))
            }
            Self::Service(host) => {
                host.client
                    .install_permission_rule(session_id, pattern, workspace_root)
                    .await
            }
        }
    }

    async fn resolve_credential_request(
        &self,
        session_id: &str,
        tool_call_id: &str,
        action: &str,
        secret: Option<&str>,
    ) -> Result<(), String> {
        let body =
            serde_json::json!({"toolCallId": tool_call_id, "action": action, "value": secret});
        match self {
            Self::Inline(host) => crate::api_sessions::credentials::resolve_credential_request(
                AxumState(host.state.clone()),
                AxumPath(session_id.to_owned()),
                HeaderMap::new(),
                AxumJson(
                    serde_json::from_value(body)
                        .map_err(|error| format!("build credential response: {error}"))?,
                ),
            )
            .await
            .map(|_| ())
            .map_err(|status| format!("resolve inline credential request: HTTP {status}")),
            Self::Service(host) => {
                host.client
                    .resolve_credential_request(session_id, &body)
                    .await
            }
        }
    }

    async fn resolve_mcp_elicitation(
        &self,
        session_id: &str,
        request_id: &str,
        action: rightkit_agent_engine_protocol::ElicitationAction,
        content: Option<serde_json::Value>,
    ) -> Result<(), String> {
        let body =
            serde_json::json!({"requestId": request_id, "action": action, "content": content});
        match self {
            Self::Inline(host) => crate::api_sessions::elicitation::resolve_elicitation(
                AxumState(host.state.clone()),
                AxumPath(session_id.to_owned()),
                HeaderMap::new(),
                AxumJson(
                    serde_json::from_value(body)
                        .map_err(|error| format!("build elicitation response: {error}"))?,
                ),
            )
            .await
            .map(|_| ())
            .map_err(|status| format!("resolve inline MCP elicitation: HTTP {status}")),
            Self::Service(host) => host.client.resolve_mcp_elicitation(session_id, &body).await,
        }
    }

    async fn answer_question(
        &self,
        session_id: &str,
        question_id: &str,
        answers: serde_json::Value,
    ) -> Result<(), String> {
        match self {
            Self::Inline(host) => crate::api_sessions::approvals::answer_question(
                AxumState(host.state.clone()),
                AxumPath(session_id.to_owned()),
                HeaderMap::new(),
                AxumJson(crate::api::AnswerQuestion {
                    question_id: question_id.to_owned(),
                    answers,
                }),
            )
            .await
            .map(|_| ())
            .map_err(|status| format!("answer inline question: HTTP {status}")),
            Self::Service(host) => {
                host.client
                    .respond_interaction(
                        session_id,
                        "/answer-question",
                        &serde_json::json!({"questionId": question_id, "answers": answers}),
                    )
                    .await
            }
        }
    }

    async fn resolve_plan(
        &self,
        session_id: &str,
        plan_id: &str,
        approved: bool,
    ) -> Result<(), String> {
        match self {
            Self::Inline(host) => crate::api_sessions::approvals::resolve_plan(
                AxumState(host.state.clone()),
                AxumPath(session_id.to_owned()),
                HeaderMap::new(),
                AxumJson(crate::api::ResolvePlan {
                    plan_id: plan_id.to_owned(),
                    approved,
                }),
            )
            .await
            .map(|_| ())
            .map_err(|status| format!("resolve inline plan: HTTP {status}")),
            Self::Service(host) => {
                host.client
                    .respond_interaction(
                        session_id,
                        "/resolve-plan",
                        &serde_json::json!({"planId": plan_id, "approved": approved}),
                    )
                    .await
            }
        }
    }

    async fn steer(&self, session_id: &str, text: &str) -> Result<(), String> {
        match self {
            Self::Inline(host) => crate::api_sessions::messages::steer_session(
                AxumState(host.state.clone()),
                AxumPath(session_id.to_owned()),
                HeaderMap::new(),
                AxumJson(serde_json::json!({"text": text})),
            )
            .await
            .map(|_| ())
            .map_err(|status| format!("steer inline session: HTTP {status}")),
            Self::Service(host) => host.client.steer(session_id, text).await,
        }
    }

    async fn interrupt(&self, session_id: &str) -> Result<(), String> {
        match self {
            Self::Inline(host) => crate::api_sessions::lifecycle::interrupt_session(
                AxumState(host.state.clone()),
                AxumPath(session_id.to_string()),
                HeaderMap::new(),
            )
            .await
            .map(|_| ())
            .map_err(|status| format!("interrupt inline session: HTTP {status}")),
            Self::Service(host) => host.client.interrupt(session_id).await,
        }
    }

    /// Prior user prompts of the session, oldest first (rewind picker rows).
    /// InlineHost reads its in-process session; the service transport has no
    /// rewind-to-message route yet and says so rather than faking one.
    async fn rewind_points(
        &self,
        session_id: &str,
    ) -> Result<Vec<crate::api_sessions::rewind::RewindPoint>, String> {
        match self {
            Self::Inline(host) => {
                crate::api_sessions::rewind::rewind_points(&host.state, session_id)
            }
            Self::Service(_) => {
                Err("Rewind to a message is only available with the inline host.".to_string())
            }
        }
    }

    async fn rewind_to(
        &self,
        session_id: &str,
        point: &crate::api_sessions::rewind::RewindPoint,
        mode: crate::api_sessions::rewind::RewindMode,
    ) -> Result<crate::api_sessions::rewind::RewindReport, String> {
        match self {
            Self::Inline(host) => {
                crate::api_sessions::rewind::rewind_to_message(&host.state, session_id, point, mode)
                    .await
            }
            Self::Service(_) => {
                Err("Rewind to a message is only available with the inline host.".to_string())
            }
        }
    }

    async fn wait_for_quiescence(&self, session_id: &str) -> Result<(), String> {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let active = match self {
                    Self::Inline(host) => host
                        .state
                        .get_session(session_id)
                        .ok_or_else(|| "inline session disappeared before settlement".to_string())?
                        .live_turn_handle
                        .is_active(),
                    Self::Service(host) => host.client.turn_is_active(session_id).await?,
                };
                if !active {
                    return Ok(());
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| "engine turn did not settle within ten seconds".to_string())?
    }

    async fn select(context_mode: ContextMode) -> Result<Self, String> {
        let raw = std::env::var(RUN_HOST_ENV).ok();
        match parse_run_host_mode(raw.as_deref())? {
            RunHostMode::Inline => Ok(Self::Inline(Box::new(
                InlineHost::shared(context_mode).await?,
            ))),
            RunHostMode::Service => Ok(Self::Service(ServiceHost::connect().await?)),
        }
    }

    /// Why Membrane context is off for this host, when turns run degraded.
    fn context_degraded_reason(&self) -> Option<String> {
        match self {
            Self::Inline(host) => host.state.context_degraded_reason().map(str::to_string),
            Self::Service(host) => host.context_degraded_reason.clone(),
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Inline(_) => "inline",
            Self::Service(_) => "service",
        }
    }

    async fn ensure_session(
        &self,
        args: &RunArgs,
        session_id: &str,
        workspace_root: &str,
    ) -> Result<(), String> {
        match self {
            Self::Inline(host) => {
                host.ensure_session(args, session_id, workspace_root)
                    .await?;
            }
            Self::Service(host) => {
                host.client
                    .ensure_session(
                        session_id,
                        daemon_session_create_body(args, session_id, workspace_root),
                    )
                    .await?;
            }
        }
        // A session that already existed keeps the mode it was created with
        // unless the caller carries an explicit live mode (`/yolo`,
        // Shift+Tab); the engine adopts it at the start of this turn. Safe
        // mode is never relaxed.
        if let (false, Some(mode)) = (args.safe_mode, args.permission_mode) {
            self.apply_permission_mode(session_id, mode).await?;
        }
        Ok(())
    }

    /// Push the live permission mode onto an existing session. Inline goes
    /// through the same in-process path as the `permission-mode` route
    /// (including descendant tightening); Service posts to that route.
    async fn apply_permission_mode(
        &self,
        session_id: &str,
        mode: rightkit_agent_policy::PermissionMode,
    ) -> Result<(), String> {
        match self {
            Self::Inline(host) => {
                if let Some(live) = host.state.get_session(session_id) {
                    crate::api_permissions::apply_live_permission_mode(&host.state, &live, mode);
                }
                Ok(())
            }
            Self::Service(host) => {
                host.client
                    .respond_interaction(
                        session_id,
                        "/permission-mode",
                        &serde_json::json!({ "mode": mode.wire_name() }),
                    )
                    .await
            }
        }
    }

    async fn submit_message(
        &self,
        session_id: &str,
        prompt: &str,
        workspace_root: &str,
    ) -> Result<i64, String> {
        self.submit_message_with_attachments(session_id, prompt, workspace_root, None, None)
            .await
    }

    async fn submit_message_with_attachments(
        &self,
        session_id: &str,
        prompt: &str,
        workspace_root: &str,
        attachments: Option<Vec<api::Attachment>>,
        sandbox_grant_prompts: Option<bool>,
    ) -> Result<i64, String> {
        match self {
            Self::Inline(host) => {
                host.submit_message_with_attachments(
                    session_id,
                    prompt,
                    workspace_root,
                    attachments,
                    sandbox_grant_prompts,
                )
                .await
            }
            Self::Service(host) => {
                host.client
                    .submit_message_with_attachments(
                        session_id,
                        prompt,
                        workspace_root,
                        attachments,
                        sandbox_grant_prompts,
                    )
                    .await
            }
        }
    }

    async fn open_event_stream(
        &self,
        session_id: &str,
        after: i64,
    ) -> Result<SessionEventStream, String> {
        match self {
            Self::Inline(host) => host
                .open_event_stream(session_id, after)
                .await
                .map(SessionEventStream::Inline),
            Self::Service(host) => host
                .client
                .open_event_stream(session_id, after)
                .await
                .map(SessionEventStream::Service),
        }
    }

    async fn events_after(
        &self,
        session_id: &str,
        after: i64,
    ) -> Result<Vec<serde_json::Value>, String> {
        match self {
            Self::Inline(host) => host.events_after(session_id, after).await,
            Self::Service(host) => host.client.events_after(session_id, after).await,
        }
    }

    /// One bounded older-history page (`seq < before_seq`, ascending, at
    /// most `limit` events) for transcript scrollback. InlineHost serves it
    /// from its composed state's durable store; ServiceHost uses the same
    /// `?before&limit` history endpoint through the existing client
    /// transport. Paging never requires an active turn and never triggers
    /// daemon discovery on the InlineHost path.
    async fn fetch_history(
        &self,
        session_id: &str,
        before_seq: i64,
        limit: usize,
    ) -> Result<serde_json::Value, String> {
        match self {
            Self::Inline(host) => host.fetch_history(session_id, before_seq, limit).await,
            Self::Service(host) => host
                .client
                .events_before(session_id, before_seq, Some(limit))
                .await
                .map(serde_json::Value::Array),
        }
    }

    /// Execute a slash command through the host's own command surface.
    /// InlineHost runs the shared in-process dispatch on its composed
    /// state; ServiceHost keeps the existing `/commands/execute` client
    /// transport. Both return the canonical `CommandResult` JSON
    /// (`success`, `output`, `action`).
    async fn execute_command(
        &self,
        input: &str,
        session_id: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        match self {
            Self::Inline(host) => host.execute_command(input, session_id).await,
            Self::Service(host) => host.client.execute_slash(input, session_id).await,
        }
    }
}

enum SessionEventStream {
    Inline(InlineEventStream),
    Service(DaemonEventStream),
}

impl SessionEventStream {
    async fn next_event(&mut self) -> Result<Option<serde_json::Value>, String> {
        match self {
            Self::Inline(stream) => stream.next_event().await,
            Self::Service(stream) => stream.next_event().await,
        }
    }
}

struct InlineEventStream {
    live: Arc<api::LiveSession>,
    subscription: ConduitSubscription,
    pending: VecDeque<ProtocolEvent>,
    last_delivered_seq: i64,
}

impl InlineEventStream {
    async fn next_event(&mut self) -> Result<Option<serde_json::Value>, String> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                if !matches!(event, ProtocolEvent::StreamResyncRequired { .. }) {
                    self.last_delivered_seq = self.last_delivered_seq.max(seq_of(&event));
                }
                return serde_json::to_value(event)
                    .map(Some)
                    .map_err(|error| format!("encode inline event: {error}"));
            }

            match self.subscription.recv().await {
                ConduitReceive::Event(event) => {
                    let seq = seq_of(&event);
                    // Live projections ride the anchor of the newest durable
                    // seq and never advance or dedup against the durable cursor.
                    if event.is_durable_fact() {
                        if seq <= self.last_delivered_seq {
                            continue;
                        }
                        self.last_delivered_seq = seq;
                    }
                    return serde_json::to_value(event)
                        .map(Some)
                        .map_err(|error| format!("encode inline event: {error}"));
                }
                ConduitReceive::Lagged { .. } => {
                    let event = match self.live.conduit.replay_after(self.last_delivered_seq) {
                        ConduitReplay::ResyncRequired {
                            after_seq,
                            oldest_available_seq,
                            high_water_mark,
                        } => inline_resync_event(
                            &self.live.id,
                            after_seq,
                            oldest_available_seq,
                            high_water_mark,
                        ),
                        ConduitReplay::Events(events) => {
                            let high_water_mark = self.live.conduit.current_high_water();
                            let oldest_available_seq = events
                                .first()
                                .map(seq_of)
                                .unwrap_or_else(|| high_water_mark.saturating_add(1));
                            inline_resync_event(
                                &self.live.id,
                                self.last_delivered_seq,
                                oldest_available_seq,
                                high_water_mark,
                            )
                        }
                    };
                    return serde_json::to_value(event)
                        .map(Some)
                        .map_err(|error| format!("encode inline resync event: {error}"));
                }
                ConduitReceive::Closed => return Ok(None),
            }
        }
    }
}

fn inline_resync_event(
    session_id: &str,
    after_seq: i64,
    oldest_available_seq: i64,
    high_water_mark: i64,
) -> ProtocolEvent {
    ProtocolEvent::StreamResyncRequired {
        session_id: session_id.to_string(),
        seq: high_water_mark.saturating_add(1),
        created_at: now_iso(),
        after_seq,
        oldest_available_seq,
        high_water_mark,
    }
}
