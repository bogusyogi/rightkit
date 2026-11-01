//! The TUI host (R6 / CR-20): the real terminal session that actually runs
//! `engine/crates/tui`.
//!
//! Everything in `coderight-tui` -- the composer, the transcript, session/agent
//! navigation, the frame scheduler, terminal capability detection -- was
//! shipped with 189 passing tests and reachable from nothing. This module is
//! the thing that reaches it: it owns the event loop, feeds `Event`s to
//! components in focus order, drives the scheduler, and renders frames.
//!
//! # Why this file does not call `run_session` directly
//!
//! `RunArgs` and `run_session` are private items of the `main_runtime` module
//! tree (defined via `include!` inside `main_runtime_run_session.rs`, reached
//! from sibling modules only through `super::` from *within* that tree --
//! see `main_runtime_interactive_session.rs`). `tui_host` is a normal sibling
//! module declared from `main.rs`, so it cannot name either type. Rather than
//! pull `main_runtime_run_session.rs` into this lane's allowlist, this module
//! stays engine-agnostic: it defines [`TurnHost`], a small trait the caller
//! implements once (in `main_runtime_interactive_session.rs`, which already
//! has `super::run_session` in scope) to bridge a submitted prompt to a real
//! turn. `TurnHost` methods take `&self`, not `&mut self` -- an
//! implementation holds whatever session state it needs (a `SessionTurnModel`,
//! provider id, session id) behind interior mutability, so [`TurnHost::spawn_turn`]
//! can hand back a genuine `tokio::spawn`'d, `'static` task instead of a
//! borrowed future this loop would otherwise have to poll inline and block
//! the input thread on.
//!
//! # Why input is not `crossterm::event::EventStream`
//!
//! The constraint this lane was given is real and is honored: input must come
//! from crossterm's own timed read, never a hand-rolled byte reader, because
//! only crossterm's parser knows to hold an incomplete `ESC` sequence open
//! long enough to tell a bare Escape from the first byte of an arrow key
//! (`component.rs:20-28`). What changed is *which* crossterm entry point.
//! `EventStream` needs crossterm's `event-stream` cargo feature, which is not
//! enabled anywhere in this workspace (checked `engine/Cargo.lock`: no crate
//! unifies it in), and this lane is forbidden from touching any `Cargo.toml`.
//! `crossterm::event::poll` / `crossterm::event::read` -- the synchronous API
//! `EventStream` itself is built on -- do the exact same buffered
//! disambiguation and need no extra feature. This module runs them on a
//! dedicated OS thread and forwards results over a channel, which gets the
//! same input-correctness guarantee into an async `select!` loop without an
//! `EventStream` in sight. If `event-stream` is ever enabled workspace-wide,
//! swapping this thread for a real `EventStream` is a same-shape change
//! entirely local to [`spawn_input_thread`].

#[path = "tui_render_metrics.rs"]
mod render_metrics_export;
mod rewind;

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use rightkit_terminal_ui::app::{dispatch, ModalKind, ScreenMode, Surface};
use rightkit_terminal_ui::bots_panel::{LiveSubagentsIntent, LiveSubagentsPanel};
use rightkit_terminal_ui::command_effects::CommandEffect;
use rightkit_terminal_ui::component::{Component, Event as TuiEvent, Outcome, Role};
use rightkit_terminal_ui::composer::completion::CompletionCandidate;
use rightkit_terminal_ui::composer::{Composer, Intent as ComposerIntent};
use rightkit_terminal_ui::identity_strip::IdentityStrip;
use rightkit_terminal_ui::interject::{Directive, InterjectionBuffer, Kind as InterjectKind};
use rightkit_terminal_ui::layout_mode::LayoutMode;
use rightkit_terminal_ui::nav::{
    AgentId, AgentNode, AgentState, ModelPicker, NavIntent, NavView, ProvidersPicker, SessionId,
    SessionSummary,
};
use rightkit_terminal_ui::render::approval::{
    approval_decision_cell, ApprovalDecision, ApprovalIntent, ApprovalKind, ApprovalOverlay,
    ApprovalRequest, DecisionActor, DecisionOutcome,
};
use rightkit_terminal_ui::render::cell::{Cell, NoticeCell, ReasoningCell};
use rightkit_terminal_ui::render::exec::{CommandOutput, ExecCall, ExecCell};
use rightkit_terminal_ui::render::RenderClass;
use rightkit_terminal_ui::terminal::capability::SystemEnv;
use rightkit_terminal_ui::terminal::TerminalProfile;
use rightkit_terminal_ui::transcript::timeline::{DaemonEvent, Timeline, TimelineReducer};
use rightkit_terminal_ui::transcript::{Message, Speaker, Transcript};
use rightkit_terminal_ui::{FrameRequester, Theme};

use rightkit_terminal_ui::crossterm::cursor as ct_cursor;
use rightkit_terminal_ui::crossterm::terminal as ct_terminal;
use rightkit_terminal_ui::crossterm::{
    event::{
        self as ct_event, DisableFocusChange, DisableMouseCapture, EnableFocusChange,
        EnableMouseCapture, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
    },
    execute,
};
use rightkit_terminal_ui::ratatui::backend::CrosstermBackend;
use rightkit_terminal_ui::ratatui::layout::Rect;
use rightkit_terminal_ui::terminal::custom_terminal::Terminal;
use rightkit_terminal_ui::terminal::history::HistoryLineWrapPolicy;
use rightkit_terminal_ui::terminal::hyperlinks::HyperlinkLine;

use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

/// The seam onto whatever actually runs a turn.
///
/// Implemented once, in `main_runtime_interactive_session.rs`, where
/// `RunArgs`/`run_session`/`SessionTurnModel` are already in scope. This
/// trait is the only thing this module knows about the engine. Methods take
/// `&self`: an implementation owns its mutable session state (turn-boundary
/// model switches, provider id) behind its own interior mutability (e.g.
/// `Arc<Mutex<SessionTurnModel>>`), which is what lets [`Self::spawn_turn`]
/// return a real `'static` task instead of a future borrowed from `&mut
/// self` that this loop would have to poll without ever yielding back to the
/// input thread.
/// One permission rule as the REST API reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRuleView {
    pub pattern: String,
    pub effect: String,
}

/// True when a deny rule already blocks this request, so "always allow"
/// must not be offered or installed for it.
fn request_denied_by_rules(kind: &ApprovalKind, rules: &[PermissionRuleView]) -> bool {
    let target = match kind {
        ApprovalKind::Exec { command, .. } => {
            rightkit_agent_policy::ToolRuleTarget::new("shell").with_command(command.trim())
        }
        ApprovalKind::Patch { changes, .. } => {
            let mut target = rightkit_agent_policy::ToolRuleTarget::new("edit_file");
            if let Some(path) = changes.keys().next() {
                target = target.with_path(path.display().to_string());
            }
            target
        }
        ApprovalKind::Generic { .. } => return false,
    };
    rules
        .iter()
        .filter(|rule| rule.effect.eq_ignore_ascii_case("deny"))
        .any(|rule| {
            rightkit_agent_policy::Rule::new(
                rule.pattern.clone(),
                rightkit_agent_policy::PermissionDecision::Deny,
                rightkit_agent_policy::RuleSource::Session,
            )
            .matches_target(&target)
        })
}

/// Install the exact rule the card showed. Re-reads the rules first: a deny
/// that appeared since the card was drawn, or rules that cannot be read,
/// stop the install. Patterns are installed in order; the first rejection
/// (the routes validate) aborts.
async fn install_allow_rule(
    host: &impl TurnHost,
    entry: Option<(rightkit_terminal_ui::render::allow_rule::AllowRule, ApprovalKind)>,
    session_id: &str,
) -> Result<(), String> {
    let (rule, kind) = entry.ok_or("no rule was offered for this request")?;
    let rules = host.permission_rules(session_id).await?;
    if request_denied_by_rules(&kind, &rules) {
        return Err("a deny rule blocks this request".into());
    }
    let root = host
        .active_directory()
        .map(|dir| dir.strip_prefix(r"\\?\").unwrap_or(&dir).to_string())
        .ok_or("no workspace to scope the rule to")?;
    for pattern in &rule.patterns {
        host.install_permission_rule(session_id, pattern, &root)
            .await?;
    }
    Ok(())
}

async fn refresh_deny_rules(
    host: &impl TurnHost,
    session: &mut SessionState,
    events: &[DaemonEvent],
) {
    if events
        .iter()
        .any(|event| matches!(event, DaemonEvent::ToolApprovalRequested { .. }))
    {
        session.deny_rules = host.permission_rules(&session.active_session).await.ok();
    }
}

pub trait TurnHost: Send + Sync + 'static {
    /// Snapshot active shells and tasks owned by current execution host.
    async fn running_work(
        &self,
        _session_id: &str,
    ) -> Vec<rightkit_terminal_ui::bots_panel::RunningProjection> {
        Vec::new()
    }

    async fn stop_running_work(
        &self,
        _session_id: &str,
        _kind: &str,
        _id: &str,
    ) -> Result<String, String> {
        Err("Running work control is unavailable for this host".into())
    }

    async fn running_output(&self, _id: &str) -> Result<String, String> {
        Err("Live shell output is unavailable for this host".into())
    }

    /// A070: take the input lease of a live interactive PTY process.
    async fn pty_attach(&self, _id: &str) -> Result<rightkit_terminal_ui::pty_attach::PtyLease, String> {
        Err("Interactive PTY attach is unavailable for this host".into())
    }

    /// A070: lease-fenced stdin write to an attached PTY process.
    async fn pty_write(
        &self,
        _id: &str,
        _bytes: &[u8],
        _epoch: u64,
        _sequence: u64,
    ) -> Result<(), String> {
        Err("Interactive PTY input is unavailable for this host".into())
    }

    async fn pty_resize(&self, _id: &str, _rows: u16, _cols: u16) -> Result<(), String> {
        Err("Interactive PTY resize is unavailable for this host".into())
    }

    async fn pty_detach(&self, _id: &str) -> Result<(), String> {
        Err("Interactive PTY detach is unavailable for this host".into())
    }

    /// Start a new turn for `prompt` as its own task and return a handle to
    /// it. The handle's output is the process exit code `run_session` would
    /// have returned.
    fn spawn_turn(&self, prompt: String) -> JoinHandle<i32>;

    /// D16: start a turn addressed to one child agent, never the root route.
    fn spawn_agent_turn(
        &self,
        _agent_id: &AgentId,
        _prompt: String,
    ) -> Result<JoinHandle<i32>, String> {
        Err("Direct child-agent chat is unavailable for this host".into())
    }

    /// P5/P6: mutate or export a selected session through canonical session APIs.
    async fn session_action(
        &self,
        _action: &str,
        _session_id: &str,
        _value: Option<&str>,
    ) -> Result<String, String> {
        Err("Session action is unavailable for this host".into())
    }

    async fn init_workspace(&self) -> Result<String, String> {
        Err("Workspace initialization is unavailable for this host".into())
    }

    async fn worktree_action(&self, _action: &str, _args: &[&str]) -> Result<String, String> {
        Err("Worktree operations are unavailable for this host".into())
    }

    /// P6: retrieve session insight projection from Adapt.
    async fn insights(&self) -> Result<String, String> {
        Err("Session insights are unavailable for this host".into())
    }

    /// Settle execution before the presentation handle is discarded. A failed
    /// acknowledgement must leave replacement work unstarted.
    async fn cancel_turn(&self) -> Result<(), String>;

    /// Deliver `text` to the running turn now (Tab push). Hosts without a
    /// mid-turn delivery seam return `Err`, and the caller queues instead.
    async fn steer_turn(&self, _text: &str) -> Result<(), String> {
        Err("this host cannot steer a running turn".to_string())
    }

    async fn resolve_approval(
        &self,
        _session_id: &str,
        _tool_call_id: &str,
        _decision: rightkit_agent_engine_protocol::ReviewDecision,
    ) -> Result<(), String> {
        Err("Approval controls are unavailable for this host".into())
    }

    /// Session and persistent permission rules visible to `session_id`
    /// (`GET /sessions/{id}/permission-rules` + `GET /permission-rules`).
    /// `Err` makes the approval card withhold "always allow": a deny that
    /// cannot be read is treated as present.
    async fn permission_rules(&self, _session_id: &str) -> Result<Vec<PermissionRuleView>, String> {
        Err("Permission rules are unavailable for this host".into())
    }

    /// Install one allow rule for `session_id` (now, via the session-rule
    /// route) and for `workspace_root` (durably, via the project-scope
    /// persistent-rule route). Both routes validate the pattern first.
    async fn install_permission_rule(
        &self,
        _session_id: &str,
        _pattern: &str,
        _workspace_root: &str,
    ) -> Result<(), String> {
        Err("Permission rules are unavailable for this host".into())
    }

    async fn answer_question(
        &self,
        _question_id: &str,
        _answers: serde_json::Value,
    ) -> Result<(), String> {
        Err("Question responses are unavailable for this host".into())
    }

    async fn resolve_credential_request(
        &self,
        _session_id: &str,
        _request_id: &str,
        _action: &str,
        _secret: Option<&str>,
    ) -> Result<(), String> {
        Err("Credential recovery is unavailable for this host".into())
    }

    async fn resolve_mcp_elicitation(
        &self,
        _session_id: &str,
        _request_id: &str,
        _action: rightkit_agent_engine_protocol::ElicitationAction,
        _content: Option<serde_json::Value>,
    ) -> Result<(), String> {
        Err("MCP elicitation is unavailable for this host".into())
    }

    async fn resolve_plan(&self, _plan_id: &str, _approved: bool) -> Result<(), String> {
        Err("Plan responses are unavailable for this host".into())
    }

    /// Resolve typed input (with or without a leading `/`) to a command's
    /// canonical name, via the one shared registry
    /// (`crate::cli::command_registry::CommandRouter`). `None` means the
    /// text is not a command at all -- an ordinary prompt or interjection.
    fn resolve_command(&self, input: &str) -> Option<String>;

    /// Gate a resolved command name. `mid_turn` is true exactly when a turn
    /// is currently in flight. This is the real, non-test call site for
    /// `crate::cli::command_registry::enforce_mid_turn`: a bare, strictly
    /// sequential REPL has no moment where a command and a running turn
    /// coexist to gate between; this concurrent loop is that moment.
    fn check_command(&self, name: &str, mid_turn: bool) -> Result<(), String>;

    /// Execute an already-resolved, already-gated command. `rest` is
    /// whatever followed the command word, already trimmed. Returns `true`
    /// if the host should exit after this call. Async because the host may
    /// delegate the command to the daemon slash surface (P1-1), which is an
    /// HTTP round trip.
    async fn execute_command(&self, name: &str, rest: &str) -> bool;

    /// Command-name completions for a typed prefix. Not consumed by this
    /// render loop today (the composer has no completion popup yet), but
    /// real and reachable so `CommandSource::completions` is not left
    /// orphaned by this host either.
    fn completions(&self, prefix: &str) -> Vec<String>;

    /// Give the host a handle to the TUI input reader's exclusive-pause
    /// handshake. When a host surface needs exclusive use of the tty (e.g.
    /// `/onboard`'s line-based prompts, which must not race the TUI's
    /// crossterm reader), it calls [`InputPause::request`], which completes
    /// only AFTER the reader has exited `poll`/`read` and acknowledged it
    /// will not touch the tty again until resume. Default no-op for hosts
    /// that own no TUI input thread.
    fn attach_input_pause(&self, _pause: Arc<InputPause>) {}

    /// Whether the host is currently holding TUI input paused (requested or
    /// reader-acknowledged). `drive` uses this to drop events racing the
    /// pause window, so stale keystrokes are never replayed into the
    /// composer while a blocking surface owns the tty.
    fn input_paused(&self) -> bool {
        false
    }

    /// One-shot "a blocking command consumed the tty and queued events must be
    /// dropped". `drive` calls this AFTER a command returns (the loop cannot
    /// drain while the command blocks it), and discards everything queued in
    /// the input channel when it returns `true`. The host sets it when it
    /// pauses input; the drive loop clears it by reading it.
    fn take_input_drain(&self) -> bool {
        false
    }

    /// Feed [`NavView`] from the host's live session/agent state. This is the
    /// daemon feed seam (P1-5): a daemon-backed host lists real sessions and
    /// agent trees here; the default feeds the honest local stub (one session,
    /// one agent — the state a host with no daemon connection actually has).
    /// Called whenever a picker is about to open, so running/done state always
    /// reflects what is true right now.
    async fn refresh_nav(&self, nav: &mut NavView, turn_in_flight: bool) {
        refresh_nav(nav, turn_in_flight, "-");
    }

    /// Scoped stop of a remote agent (an id this host does not own a local
    /// turn for), covering the exact subtree scope. A daemon-backed host
    /// interrupts each daemon session in the scope (the daemon recursively
    /// cancels a sub-session's descendants), stopping exactly that subtree —
    /// never its parent or siblings. The default refuses: no daemon feed
    /// means remote agents cannot be reached, and a refusal is never widened
    /// into "stop everything".
    /// #140: deliver a follow-up message to a running subagent (its own
    /// daemon session) over the live steer channel `followup_subagent` uses.
    async fn message_remote_agent(&self, _agent: &AgentId, _text: &str) -> Result<(), String> {
        Err("Cannot message a remote agent: no daemon feed is connected.".to_string())
    }

    async fn stop_remote_subtree(&self, _scope: &[AgentId]) -> Result<usize, String> {
        Err(
            "Cannot stop a remote agent: no daemon feed is connected. Nothing was stopped."
                .to_string(),
        )
    }

    /// Re-point this host at another root session (P1-5 sideways movement).
    /// The default refuses: a single-session host has nothing to switch to.
    async fn switch_session(&self, id: &SessionId) -> Result<(), String> {
        Err(format!(
            "Cannot switch to session {id}: no daemon feed is connected."
        ))
    }

    /// Create a brand-new root session and return its id (S:I5's `/new`).
    /// The default refuses: creating a daemon session needs the daemon
    /// feed, and refusing is honest -- `/new` then reports that nothing was
    /// started instead of clearing the view and calling it a new session.
    /// A daemon-backed host overrides this; see the lane report.
    async fn new_session(&self) -> Result<SessionId, String> {
        Err(
            "Cannot start a new session: no daemon feed is connected, so only this session exists."
                .to_string(),
        )
    }

    /// `--plain` exactly as the CLI parsed it (S:I7). Defaults to "not
    /// requested": a host that does not carry the flag still resolves
    /// reduced motion from the environment.
    fn plain_output(&self) -> bool {
        false
    }

    /// `--reduced-motion` exactly as the CLI parsed it (S:I7). Same default
    /// reasoning as [`Self::plain_output`].
    fn reduced_motion(&self) -> bool {
        false
    }

    /// The live session permission mode (`/ask`, `/accept-edits`, `/auto`,
    /// `/yolo`, Shift+Tab). Hosts that own no permission state report `Ask`.
    fn permission_mode(&self) -> rightkit_agent_policy::PermissionMode {
        rightkit_agent_policy::PermissionMode::Ask
    }

    fn permission_warning(&self) -> Option<&'static str> {
        None
    }

    /// Switch the live permission mode; the engine adopts it at the start of
    /// the next turn. Default no-op for hosts with no permission state.
    fn set_permission_mode(&self, _mode: rightkit_agent_policy::PermissionMode) {}

    /// Fetch a remote agent's recent transcript for read-only viewing on
    /// Descend. A daemon-backed host returns a bounded, rendered excerpt AND
    /// records the last-seen event sequence for this agent as the starting
    /// cursor; the default (no feed) returns `None`, so the host says so
    /// honestly.
    async fn fetch_agent_transcript(&self, _id: &AgentId) -> Option<String> {
        None
    }

    /// Poll for NEW events of a viewed agent after the host's last-seen
    /// cursor (P1-5 live agent transcript). Returns display-ready lines for
    /// events newer than that cursor (bounded to a newest window) and
    /// advances the cursor; `None` means nothing new. The default (no feed)
    /// never yields anything. Called on the maintenance tick while an agent
    /// view is open — polling never pauses or owns the viewed worker.
    async fn poll_agent_transcript(&self, _id: &AgentId) -> Option<Vec<String>> {
        None
    }

    /// Poll typed events for active session after host-owned cursor. This is
    /// deliberately separate from agent transcript projection: daemon event
    /// families retain their semantic shape through S:I3's reducer instead
    /// of being flattened into assistant text before the TUI sees them.
    async fn poll_timeline_events(&self) -> Vec<DaemonEvent> {
        Vec::new()
    }

    /// Whether a producer other than the in-flight turn can emit timeline
    /// events right now — i.e. an already-connected daemon feed under
    /// ServiceHost. Must never resolve or autostart a connection to answer:
    /// the default is `false`, so an inline host's idle loop does not poll a
    /// feed that cannot produce, and a disconnected service feed cannot
    /// spawn a daemon just to watch for events.
    fn external_feed_available(&self) -> bool {
        false
    }

    /// Page one bounded batch of the ACTIVE session's older durable history
    /// from the daemon's SQLite ledger (P2-5). `before_seq` is an exclusive
    /// cursor; `limit` bounds the page. Entries are oldest-first with their
    /// sequence numbers so the caller can advance its cursor. The default
    /// refuses: no feed means there is no ledger to page from.
    async fn fetch_older_history_page(
        &self,
        _before_seq: i64,
        _limit: usize,
    ) -> Result<Vec<HistoryPageEntry>, String> {
        Err("no daemon feed is connected; older history cannot be paged".to_string())
    }

    /// Truthful startup/context readiness (KRN defect #4). A host backed by
    /// a lazily-initialized context worker (InlineHost/Membrane) publishes
    /// its real state here so `drive` can post a concise notice instead of
    /// implying readiness the host does not have; typing, redraw, cancel,
    /// and exit stay responsive regardless of what this reports. The
    /// default is `None`: a host with no such background lifecycle (e.g.
    /// an already-connected ServiceHost) has nothing to report.
    fn readiness(&self) -> Option<tokio::sync::watch::Receiver<HostReadiness>> {
        None
    }

    /// The shared model catalog as [`ModelChoice`] rows for the model/effort
    /// picker (Stage 3). Async because a truthful catalog resolves live
    /// broker auth status (subscription account model lists) rather than a
    /// stale sync snapshot. Rows must mark `is_current`/`is_default` from the
    /// *committed* route (`SessionRuntimeOptions`), not a pending draft —
    /// the picker only displays. The default is an empty catalog: a host
    /// with no route authority opens the picker over nothing instead of
    /// fabricating entries.
    async fn model_catalog(&self) -> Vec<rightkit_terminal_ui::nav::ModelChoice> {
        Vec::new()
    }

    /// The committed route: the model/effort the session's next admitted
    /// turn will actually use. Drives the identity strip's effort field and
    /// picker `(current)` marking. `None` fields mean "not known" — they are
    /// never guessed here.
    fn committed_route(&self) -> CommittedRoute {
        CommittedRoute::default()
    }

    /// A requested-but-not-yet-committed route selection, rendered by the
    /// identity strip as `→ model·effort` until the authority settles it.
    /// `None` when nothing is pending.
    fn pending_route_label(&self) -> Option<String> {
        None
    }

    /// Apply a model selection through the existing route authority
    /// (`model_route`/`inference_selection`), exactly as `/model` would.
    /// The picker emits ids from [`Self::model_catalog`] verbatim; unknown
    /// or unavailable ids must error here, never be clamped to another
    /// model. The default refuses: no route authority means no selection.
    async fn select_model(&self, _model_id: &str) -> Result<(), String> {
        Err("model selection is unavailable for this host".to_string())
    }

    /// Apply an effort selection through the same route authority, exactly
    /// as `/effort` would. Same refusal semantics as [`Self::select_model`].
    async fn select_effort(&self, _effort_id: &str) -> Result<(), String> {
        Err("effort selection is unavailable for this host".to_string())
    }

    /// Commit one validated model/effort pair through one route update.
    async fn select_route(&self, _model_id: &str, _effort_id: Option<&str>) -> Result<(), String> {
        Err("route selection is unavailable for this host".into())
    }

    /// The provider-grouped catalog for the provider-first `/model` picker
    /// and the `/providers` visibility manager: configured providers with
    /// their catalog rows (visibility flags included) for `/model`, plus
    /// every other known provider for `/providers`. Same build as
    /// [`Self::model_catalog`], grouped differently. Empty means the host
    /// has no route authority — the pickers notice instead of fabricating.
    async fn provider_catalog(&self) -> Vec<rightkit_terminal_ui::nav::ProviderChoice> {
        Vec::new()
    }

    /// Persist one model's `/model` visibility for a provider (`/providers`
    /// Space toggle). `model` is the real catalog model id; `hidden` is the
    /// NEW state. Errors surface as transcript notices.
    async fn set_model_visibility(
        &self,
        _provider: &str,
        _model: &str,
        _hidden: bool,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Run one `/providers` level-2 action (sign in/out, add/replace/remove
    /// key, test connection, status) and return the message line to show.
    /// `secret` is the typed API key for add/replace only.
    async fn provider_action(
        &self,
        _provider: &str,
        _action: rightkit_terminal_ui::nav::ProviderAction,
        _secret: Option<rightkit_terminal_ui::nav::SecretText>,
    ) -> Result<String, String> {
        Err("provider actions are unavailable for this host".to_string())
    }

    /// Canonical root & display label for each enrolled repository choice.
    fn repository_catalog(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    async fn select_repository(&self, _root: &str) -> Result<(), String> {
        Err("repository selection is unavailable for this host".into())
    }

    /// The active tool/admission scope, which may differ from the launch cwd.
    fn active_directory(&self) -> Option<String> {
        std::env::current_dir()
            .ok()
            .map(|root| root.display().to_string())
    }

    /// Answer one `@` file-search query from the host's canonical file
    /// source — the same gitignore-aware workspace discovery `list_files`
    /// runs (`rightkit_agent_tools::coding_tools::list_files::discover_workspace_files`),
    /// scoped to the selected repository root, never a TUI-local scanner.
    /// The drive loop hands the composer's latest request here verbatim and
    /// handles empty/cancel itself, so `query` is always non-empty.
    /// Implementations must bound the work (a capped, off-executor walk);
    /// the drive loop cannot stall input on discovery. The default yields
    /// no matches: a host with no selected repository has no canonical
    /// scope to search and must not fabricate a listing.
    async fn file_search(&self, _query: &str) -> Vec<rightkit_terminal_ui::composer::FileMatch> {
        Vec::new()
    }

    /// One-shot: a command surface (`/model`, `/effort`) asked the host to
    /// open a route picker. The command adapter owns the request queue; this
    /// host only consumes it. Default `None`: nothing requests a picker.
    fn take_picker_request(&self) -> Option<PickerRequest> {
        None
    }

    /// D3 rewind: this session's prior user prompts, oldest first. Default
    /// refuses: a host without a session ledger has nothing to rewind.
    async fn rewind_points(&self) -> Result<Vec<crate::api_sessions::rewind::RewindPoint>, String> {
        Err("Rewind is not available from this host.".to_string())
    }

    /// D3 rewind: restore conversation and/or files to just before `point`.
    async fn rewind_to(
        &self,
        _point: &crate::api_sessions::rewind::RewindPoint,
        _mode: crate::api_sessions::rewind::RewindMode,
    ) -> Result<crate::api_sessions::rewind::RewindReport, String> {
        Err("Rewind is not available from this host.".to_string())
    }

    /// The home screen closed (a choice was made or Esc): hosts release
    /// any picker request they deferred behind it.
    fn home_closed(&self) {}

    /// D14/D15: every known project with its chats, across ALL projects.
    async fn project_catalog(&self) -> Vec<crate::tui_projects::ProjectEntry> {
        Vec::new()
    }

    /// The session id the host's next turn targets, when it owns one.
    fn current_session(&self) -> Option<String> {
        None
    }

    /// D5: resolved `[tui]` preferences.
    fn tui_settings(&self) -> crate::tui_projects::TuiSettings {
        crate::tui_projects::TuiSettings::default()
    }

    /// D5: persist one `[tui]` setting (`theme`, `notify`, `vim`,
    /// `statusline`) and return a confirmation line.
    async fn set_tui_setting(&self, _key: &str, _value: &str) -> Result<String, String> {
        Err("settings are unavailable for this host".into())
    }

    /// D6: the footer chip for the active chat's goal, if one is open.
    fn goal_chip(&self) -> Option<String> {
        None
    }

    /// D6: read or mutate the active chat's canonical goal through the same
    /// session authority used by `/goal` in other surfaces. `op` is one of
    /// `get|set|edit|pause|resume|clear`; `text` is required for set/edit.
    /// Returns the API's goal snapshot so the host can refresh its footer
    /// projection without creating a TUI-owned goal store.
    async fn goal_op(&self, _op: &str, _text: Option<&str>) -> Result<serde_json::Value, String> {
        Err("goals are unavailable for this host".into())
    }

    /// D2: current policy for one canonical workspace root.
    async fn workspace_trust(
        &self,
        _root: &str,
    ) -> Result<rightkit_agent_policy::WorkspaceTrust, String> {
        Err("workspace trust is unavailable for this host".into())
    }

    /// D2: persist an explicit user trust decision for one workspace root.
    async fn trust_workspace(&self, _root: &str) -> Result<(), String> {
        Err("workspace trust is unavailable for this host".into())
    }

    /// P3: canonical workspace diff text for read-only inspection.
    async fn workspace_diff(&self) -> Result<String, String> {
        Err("Workspace diff is unavailable for this host".into())
    }

    /// P15: bounded durable session event trace for native inspection.
    async fn session_log(&self, _session_id: &str) -> Result<String, String> {
        Err("Session event trace is unavailable for this host".into())
    }

    /// P15: redacted canonical hook readiness from `/hooks/status`.
    async fn hooks_status(&self) -> Result<String, String> {
        Err("Hook readiness is unavailable for this host".into())
    }

    /// D18: reason displayed when context is unavailable and turns run
    /// visibly degraded. The readiness channel remains the startup source;
    /// this accessor keeps the reason visible after startup settles.
    fn context_degraded_reason(&self) -> Option<String> {
        None
    }

    /// D6: a prompt the host wants submitted as the next turn right after a
    /// command ran (`/goal set` starts working toward the goal).
    fn take_followup_prompt(&self) -> Option<String> {
        None
    }

    /// #132: expand a custom slash command (`/name args`) to the prompt it
    /// submits. `None` for anything that is not a custom command.
    fn custom_command_prompt(&self, _input: &str) -> Option<String> {
        None
    }
}

/// Which route picker a [`TurnHost::take_picker_request`] asked to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerRequest {
    /// `/model`: provider first, then that provider's model list, then
    /// per-model effort when it has >1.
    Model,
    /// `/effort`: effort list for the committed model only.
    Effort,
    /// `/providers`: provider list, then per-provider model visibility.
    Providers,
    Repository,
    /// `/resume`: the session list, picked in place.
    Sessions,
    /// `/agents`: the subagent picker for the active root's agent tree.
    Agents,
    /// `/resume <id>`: switch straight to that session.
    ResumeSession(String),
    /// D14: the launch home screen (projects + recent chats).
    Home,
    /// D15: the pop-out project/chat sidebar.
    Sidebar,
    /// D5: the `/config` settings screen.
    Config,
}

/// `/trust` command helper. It targets the active workspace and reports the
/// persisted policy result, so command routing cannot trust the process cwd
/// after a project switch.
pub async fn trust_active_workspace(host: &impl TurnHost) -> Result<String, String> {
    let root = host.active_directory().ok_or_else(|| {
        "No workspace is selected. Open a project before trusting it.".to_string()
    })?;
    match host.workspace_trust(&root).await? {
        rightkit_agent_policy::WorkspaceTrust::Trusted => {
            Ok(format!("Workspace is already trusted: {root}"))
        }
        _ => {
            host.trust_workspace(&root).await?;
            Ok(format!("Trusted workspace: {root}"))
        }
    }
}

/// Transcript notice for a subagent follow-up attempt (#140).
pub fn subagent_message_notice(agent: &AgentId, result: Result<(), String>) -> String {
    match result {
        Ok(()) => format!("Sent follow-up to agent {agent}."),
        Err(reason) => format!("Follow-up to agent {agent} not delivered: {reason}"),
    }
}

/// Execute one `/goal` command against the host's canonical goal API and
/// return display text for the transcript. Setting a goal leaves the host's
/// follow-up prompt queued for the normal turn-start path.
fn clipboard_copy_notice(text: &str) -> String {
    let env = rightkit_terminal_ui::terminal::capability::SystemEnv;
    let strategy = rightkit_terminal_ui::terminal::clipboard_strategy(&env);
    let result = if matches!(
        strategy,
        rightkit_terminal_ui::terminal::ClipboardStrategy::NativeWithOsc52Fallback
    ) {
        crate::doctor::try_copy_to_clipboard(text)
            .map(|tool| format!("native:{tool}"))
            .or_else(|_| {
                let mut out = std::io::stdout();
                rightkit_terminal_ui::terminal::copy_via_osc52(&mut out, text, &env)
                    .map(|_| "OSC52 fallback".to_string())
                    .map_err(|error| error.to_string())
            })
    } else {
        let mut out = std::io::stdout();
        rightkit_terminal_ui::terminal::copy_via_osc52(&mut out, text, &env)
            .map(|_| "remote OSC52".to_string())
            .map_err(|error| error.to_string())
    };
    match result {
        Ok(_backend) => format!(
            "Copied to clipboard ({} characters, {} lines)",
            text.chars().count(),
            text.lines().count()
        ),
        Err(error) => format!("Clipboard copy failed: {error}"),
    }
}

fn selection_point(session: &SessionState, column: u16, row: u16) -> Option<(usize, usize)> {
    let area = session.selection_area?;
    if column < area.x || column >= area.right() || row < area.y || row >= area.bottom() {
        return None;
    }
    let visible_start = session
        .selection_lines
        .len()
        .saturating_sub(area.height as usize);
    let line = visible_start + usize::from(row - area.y);
    let width = session.selection_lines.get(line)?.chars().count();
    Some((line, usize::from(column - area.x).min(width)))
}

fn selection_text(session: &SessionState) -> Option<String> {
    let (mut start, mut end) = (session.selection_anchor?, session.selection_extent?);
    if (start.0, start.1) > (end.0, end.1) {
        std::mem::swap(&mut start, &mut end);
    }
    let mut selected = String::new();
    for row in start.0..=end.0 {
        let chars: Vec<char> = session.selection_lines.get(row)?.chars().collect();
        let from = if row == start.0 { start.1 } else { 0 };
        let to = if row == end.0 { end.1 } else { chars.len() };
        selected.extend(chars.get(from..to.min(chars.len()))?.iter());
        if row != end.0 {
            selected.push('\n');
        }
    }
    (!selected.is_empty()).then_some(selected)
}

fn selection_bounds(session: &SessionState) -> Option<((usize, usize), (usize, usize))> {
    let (mut start, mut end) = (session.selection_anchor?, session.selection_extent?);
    if start > end {
        std::mem::swap(&mut start, &mut end);
    }
    Some((start, end))
}

fn word_selection(line: &str, column: usize) -> (usize, usize) {
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return (0, 0);
    }
    let at = column.min(chars.len().saturating_sub(1));
    let whitespace = chars[at].is_whitespace();
    let mut start = at;
    let mut end = at + 1;
    while start > 0 && chars[start - 1].is_whitespace() == whitespace {
        start -= 1;
    }
    while end < chars.len() && chars[end].is_whitespace() == whitespace {
        end += 1;
    }
    (start, end)
}

pub async fn goal_command(host: &impl TurnHost, rest: &str) -> Result<String, String> {
    use crate::tui_projects::GoalCmd;
    let (op, text) = match crate::tui_projects::parse_goal_args(rest)? {
        GoalCmd::Show => ("get", None),
        GoalCmd::Set(text) => ("set", Some(text)),
        GoalCmd::Edit(text) => ("edit", Some(text)),
        GoalCmd::Pause => ("pause", None),
        GoalCmd::Resume => ("resume", None),
        GoalCmd::Clear => ("clear", None),
    };
    let snapshot = host.goal_op(op, text.as_deref()).await?;
    Ok(crate::tui_projects::goal_status_text(&snapshot))
}

/// The committed route selection, as the host authority last settled it.
/// Pending drafts live in [`TurnHost::pending_route_label`], never here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommittedRoute {
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// Real startup/context state a [`TurnHost`] can report through
/// [`TurnHost::readiness`]. `Configuring` covers provider/model resolution
/// (onboarding); `StartingContext` covers the InlineHost native connect,
/// which can block for minutes on a cold worker; `Failed` names the error
/// and is recoverable -- submitting again retries rather than repeating a
/// permanently cached failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostReadiness {
    Configuring,
    StartingContext,
    Ready,
    /// Context (Membrane) is unavailable; turns run with no context
    /// injection and no shadow store. Not an error: the turn proceeds.
    Degraded {
        reason: String,
    },
    Failed {
        message: String,
    },
}

/// One entry paged back from the daemon's durable history ledger: its
/// sequence number (for cursor advancement) & optional display-ready line.
/// Non-display events still belong to the page & must advance its cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryPageEntry {
    pub seq: i64,
    pub line: Option<String>,
}

/// Cursor for the next backwards page: the smallest sequence in the page we
/// just inserted (exclusive `before`), or `None` when the page was empty.
fn next_paging_cursor(page: &[HistoryPageEntry]) -> Option<i64> {
    page.iter().map(|entry| entry.seq).min()
}

/// P2-5: scrollback paging state for the session whose transcript this view
/// currently shows — the durable-history cursor (smallest seq still needed),
/// whether the ledger has been exhausted, and how far eviction had advanced
/// when we last paged. All three fields describe ONE session's ledger, so a
/// session switch must rebind them together: a cursor or exhaustion flag
/// carried over from session A would otherwise page — or refuse to page —
/// session B's history.
#[derive(Debug, Default, PartialEq, Eq)]
struct HistoryPaging {
    cursor: Option<i64>,
    exhausted: bool,
    paged_through_evictions: usize,
}

impl HistoryPaging {
    /// Rebind the paging state to a different session's durable ledger.
    /// Called at the same moment the transcript view is cleared, so the next
    /// page fetch starts from the new session's newest seq (`before` falls
    /// back to `i64::MAX`) instead of a cursor that only means something in
    /// the session just left — and an `exhausted` flag earned on the old
    /// ledger cannot silence paging on the new one.
    fn rebind_session(&mut self) {
        *self = Self::default();
    }
}

/// A still-moving typed projection rendered in the live tail region:
/// reasoning blocks and tool/exec cells whose streamed content updates in
/// place until the event stream settles them. Settled entries are moved
/// into the transcript as plain system text so they flush to scrollback
/// under the transcript's own drain contract — the two never double-paint.
enum LiveCell {
    /// An open or just-settled reasoning block (`text` accumulates deltas).
    Reasoning { text: String, settled: bool },
    /// One or more grouped tool calls (`ExecCell` owns per-call state).
    Exec(ExecCell),
}

/// Upper bound on simultaneously live cells; a flood of unresolved calls
/// drops the oldest *settled* entries first so a stuck running call is
/// never silently evicted while still needed for output routing.
const LIVE_CELL_MAX: usize = 16;

/// Extract plain unstyled text from a cell — the form transcript system
/// messages and scrollback inserts take.
fn cell_plain_text(cell: &dyn Cell) -> String {
    cell.plain_lines()
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Upper bound on a single approval-preview payload (command, file content,
/// or unified diff) so a hostile or oversized tool argument cannot grow the
/// modal's retained state without bound.
const APPROVAL_PREVIEW_MAX_CHARS: usize = 40_000;

fn clip_approval_payload(text: &str) -> String {
    if text.chars().count() <= APPROVAL_PREVIEW_MAX_CHARS {
        return text.to_string();
    }
    let mut clipped: String = text.chars().take(APPROVAL_PREVIEW_MAX_CHARS).collect();
    clipped.push('…');
    clipped
}

/// The diff an exact-substring edit would make. Reads the file (bounded) so
/// the hunk carries real line numbers and context; any edit that does not
/// apply cleanly to what is on disk falls back to the bare replacement hunks,
/// never to a guess.
fn edit_preview_change(
    path: &str,
    cwd: Option<&str>,
    pairs: &[(String, String)],
    replace_all: bool,
) -> rightkit_terminal_ui::render::diff::FileChange {
    use rightkit_terminal_ui::render::diff::FileChange;
    let full = {
        let candidate = std::path::PathBuf::from(path);
        match cwd {
            Some(dir) if candidate.is_relative() => std::path::Path::new(dir).join(candidate),
            _ => candidate,
        }
    };
    let before = std::fs::metadata(&full)
        .ok()
        .filter(|meta| meta.len() <= 512 * 1024)
        .and_then(|_| std::fs::read_to_string(&full).ok());
    if let Some(before) = before {
        let mut after = before.clone();
        let mut applied = true;
        for (old, new) in pairs {
            if old.is_empty() || !after.contains(old.as_str()) {
                applied = false;
                break;
            }
            after = if replace_all {
                after.replace(old.as_str(), new)
            } else {
                after.replacen(old.as_str(), new, 1)
            };
        }
        if applied && after != before {
            return FileChange::replacement(&before, &after);
        }
    }
    FileChange::from_replacements(pairs)
}

/// The wire summary for an unknown tool is `name {"k":"v",...}`. Show it as
/// `name · k: v · k2: v2` so the card never leads with raw JSON; anything that
/// is not that shape passes through unchanged.
fn humanize_generic_summary(call_name: &str, summary: &str) -> String {
    let Some(rest) = summary.strip_prefix(call_name).map(str::trim_start) else {
        return summary.to_string();
    };
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(rest) else {
        return summary.to_string();
    };
    if map.is_empty() {
        return call_name.to_string();
    }
    let fields: Vec<String> = map
        .iter()
        .map(|(key, value)| match value {
            serde_json::Value::String(text) => format!("{key}: {text}"),
            other => format!("{key}: {other}"),
        })
        .collect();
    format!("{call_name} · {}", fields.join(" · "))
}

/// Build the typed preview the approval overlay renders for a canonical
/// `toolApprovalRequested` call. A literal shell-command argument yields the
/// donor `Exec` preview; a unified-diff or whole-file write yields the donor
/// `Patch` diff preview. Unknown tools, absent arguments, and malformed JSON
/// degrade to the `Generic` description — no preview may fabricate an action
/// the wire did not name. `ApprovalRequest::new` re-sanitizes every string,
/// so this mapper never needs its own escape pass.
fn approval_kind_for_call(
    call_name: &str,
    arguments_json: &str,
    summary: &str,
    default_cwd: Option<&str>,
) -> ApprovalKind {
    // Generic description: the human summary when present, else the tool
    // name — never an empty string in the modal.
    let generic = || ApprovalKind::Generic {
        description: if summary.is_empty() {
            call_name.to_string()
        } else {
            humanize_generic_summary(call_name, summary)
        },
    };
    let Ok(args) = serde_json::from_str::<serde_json::Value>(arguments_json) else {
        return generic();
    };
    let arg_str = |keys: &[&str]| {
        keys.iter().find_map(|key| {
            args.get(*key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
    };
    let reason = || arg_str(&["reason", "justification", "description"]);
    // Exec: `command`/`cmd` is RightKit's shell-tool argument spelling;
    // argv-style `args`/`argv` arrays are joined for tools that spell the
    // command that way.
    let command = arg_str(&["command", "cmd", "script"]).or_else(|| {
        args.get("argv")
            .or_else(|| args.get("args"))
            .and_then(serde_json::Value::as_array)
            .map(|argv| {
                argv.iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|joined| !joined.is_empty())
    });
    if let Some(command) = command {
        // Where it runs: the call's own cwd, else the session directory the
        // shell tool executes in. Writes and network appear only when the
        // call declares them; nothing is inferred from the command text.
        let declared_bool = |keys: &[&str]| {
            keys.iter().find_map(|key| match args.get(*key) {
                Some(serde_json::Value::Bool(true)) => Some("on".to_string()),
                Some(serde_json::Value::Bool(false)) => Some("off".to_string()),
                Some(serde_json::Value::String(text)) if !text.trim().is_empty() => {
                    Some(text.trim().to_string())
                }
                _ => None,
            })
        };
        let writes = ["writes", "write_paths", "writable_paths"]
            .iter()
            .find_map(|key| match args.get(*key) {
                Some(serde_json::Value::String(text)) if !text.trim().is_empty() => {
                    Some(text.trim().to_string())
                }
                Some(serde_json::Value::Array(items)) => {
                    let joined = items
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ");
                    (!joined.is_empty()).then_some(joined)
                }
                _ => None,
            });
        return ApprovalKind::Exec {
            command: clip_approval_payload(&command),
            cwd: arg_str(&["cwd", "workdir", "workingDirectory", "working_directory"])
                .or_else(|| default_cwd.map(str::to_string)),
            reason: reason(),
            writes,
            network: declared_bool(&["network", "allow_network", "network_access"]),
        };
    }
    // Exact-substring edits (`edit_file` / `multi_edit`): preview the real
    // before/after as a diff, numbered from the file when it can be read.
    if let Some(path) = arg_str(&["path", "file", "filePath", "file_path", "target"]) {
        let pair = |item: &serde_json::Value| {
            let old = item
                .get("old_string")
                .or_else(|| item.get("oldString"))?
                .as_str()?;
            let new = item
                .get("new_string")
                .or_else(|| item.get("newString"))?
                .as_str()?;
            Some((old.to_string(), new.to_string()))
        };
        let pairs: Vec<(String, String)> =
            match args.get("edits").and_then(serde_json::Value::as_array) {
                Some(items) => items.iter().filter_map(pair).collect(),
                None => pair(&args).into_iter().collect(),
            };
        if !pairs.is_empty() {
            let replace_all = args
                .get("replace_all")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let change = edit_preview_change(&path, default_cwd, &pairs, replace_all);
            let mut changes = std::collections::BTreeMap::new();
            changes.insert(std::path::PathBuf::from(&path), change);
            return ApprovalKind::Patch {
                changes,
                cwd: std::path::PathBuf::new(),
                reason: reason(),
            };
        }
    }
    // Patch: a unified diff argument renders as `Update`; `path`+`content`
    // (whole-file write) renders as `Add`. A patch-sounding tool name alone
    // never invents a diff — the shape of the arguments decides.
    let path = arg_str(&["path", "file", "filePath", "file_path", "target"]);
    if let Some(diff) = arg_str(&["patch", "diff", "unified_diff", "unifiedDiff"]) {
        if diff.contains("@@") || diff.starts_with("---") {
            let mut changes = std::collections::BTreeMap::new();
            changes.insert(
                std::path::PathBuf::from(path.unwrap_or_else(|| "patch".to_string())),
                rightkit_terminal_ui::render::diff::FileChange::update(clip_approval_payload(&diff)),
            );
            return ApprovalKind::Patch {
                changes,
                cwd: std::path::PathBuf::new(),
                reason: reason(),
            };
        }
    }
    if let (Some(path), Some(content)) = (path, arg_str(&["content", "new_content", "newContent"]))
    {
        let mut changes = std::collections::BTreeMap::new();
        changes.insert(
            std::path::PathBuf::from(path),
            rightkit_terminal_ui::render::diff::FileChange::add(clip_approval_payload(&content)),
        );
        return ApprovalKind::Patch {
            changes,
            cwd: std::path::PathBuf::new(),
            reason: reason(),
        };
    }
    generic()
}

/// The policy's own reason for asking, from the runtime-reported policy
/// source. Only sources whose meaning is known are worded; anything else is
/// shown as the source itself rather than paraphrased, and an absent source
/// yields no reason at all.
fn approval_policy_reason(policy_source: Option<&str>, kind: &ApprovalKind) -> Option<String> {
    let source = policy_source?.trim();
    if source.is_empty() {
        return None;
    }
    // (subject, verb) agree: "shell commands need", "this action needs".
    let (subject, verb) = match kind {
        ApprovalKind::Exec { .. } => ("shell commands", "need"),
        ApprovalKind::Patch { .. } => ("file changes", "need"),
        ApprovalKind::Generic { .. } => ("this action", "needs"),
    };
    Some(match source {
        "mode:ask" => format!("{subject} {verb} approval in Ask mode"),
        "mode:accept-edits" => format!("{subject} {verb} approval in Accept edits mode"),
        "mode:auto-safe" => format!("{subject} {verb} approval in Auto mode"),
        "mode:plan" => format!("{subject} {verb} approval in Plan mode"),
        other => format!("policy {other} requires approval"),
    })
}

/// A short human subject for a request, used by its decision record
/// (`✗ rejected shell · echo hi`). Never raw JSON: a command or a path when
/// the typed preview has one, else the request's own description.
fn approval_subject(kind: &ApprovalKind, call_name: &str, summary: &str) -> String {
    let clip = |text: &str| {
        let line = text.lines().next().unwrap_or("").trim();
        if line.chars().count() > 72 {
            let mut cut: String = line.chars().take(71).collect();
            cut.push('…');
            cut
        } else {
            line.to_string()
        }
    };
    match kind {
        ApprovalKind::Exec { command, .. } => format!("shell · {}", clip(command)),
        ApprovalKind::Patch { changes, .. } => match changes.keys().next() {
            Some(path) => format!("edit · {}", clip(&path.display().to_string())),
            None => "edit".to_string(),
        },
        ApprovalKind::Generic { description } => {
            // A description that is just the tool name plus raw arguments is
            // reduced to the tool name; real prose is kept.
            let raw_call = !call_name.is_empty() && summary.trim_start().starts_with(call_name);
            if description.trim().is_empty()
                || description.trim_start().starts_with('{')
                || raw_call
            {
                clip(if call_name.is_empty() {
                    summary
                } else {
                    call_name
                })
            } else {
                clip(description)
            }
        }
    }
}

/// S:I3/S:I6 — the typed session state the host owns beside the transcript.
///
/// [`Transcript`] holds `(Speaker, String)` lines: exactly right for
/// scrollback, useless for asking "which model is producing this turn?" or
/// "is anything still open?". [`Timeline`] (S:I3) is the typed answer to
/// those, and [`IdentityStrip`] (S:I6) is a projection of it. Bundled into
/// one struct because every host path that records a session event needs
/// all of it and `handle_event` is already at its argument budget.
struct SessionState {
    theme: Theme,
    /// Authority-facing pending list `(session, request_id)`, kept because
    /// the overlay alone cannot answer "is there a pending approval for
    /// THIS session" once deferred requests close its current view.
    pending_approvals: std::collections::VecDeque<(String, String)>,
    /// Stage-3 modal approval queue bound to `active_session`. It never
    /// executes; it emits intents the host maps onto `resolve_approval`.
    approval_overlay: ApprovalOverlay,
    /// Exact allow rule each pending request's "always allow" option installs.
    approval_allow_rules: std::collections::HashMap<
        String,
        (rightkit_terminal_ui::render::allow_rule::AllowRule, ApprovalKind),
    >,
    /// Permission rules read when the latest approval arrived; `None` means
    /// unknown, and unknown withholds "always allow".
    deny_rules: Option<Vec<PermissionRuleView>>,
    /// Set when a new approval request was enqueued: the drive loop must
    /// drain buffered input before the modal's first draw so keystrokes
    /// typed against the pre-modal screen can never decide the request.
    quarantine_input: bool,
    approval_input_safe: bool,
    pending_question: Option<PendingQuestion>,
    pending_credential: Option<PendingCredential>,
    pending_elicitation: Option<PendingElicitation>,
    pending_plan: Option<PendingPlan>,
    /// D2: first-launch "Trust this folder? (yes/no)" card for an untrusted workspace root.
    pending_trust: Option<String>,
    /// Ctrl+L: the next frame clears the visible screen (session and scrollback history stay).
    clear_screen_requested: bool,
    /// In-flight typed projections (reasoning, exec/tool cells).
    live_cells: Vec<LiveCell>,
    /// `call_id` → index into `live_cells` for routing tool updates; the
    /// cells worker's `ExecCell` deliberately exposes no id-membership
    /// probe, so the host tracks the association itself.
    exec_cell_index: std::collections::HashMap<String, usize>,
    /// Per-call start times so settled exec cells carry a real duration.
    exec_started: std::collections::HashMap<String, Instant>,
    /// Per-call count of output-preview chars already appended, so a
    /// cumulative `output_preview` is fed to `ExecCell::append_output` as
    /// a delta rather than duplicated in full on every update.
    exec_output_seen: std::collections::HashMap<String, usize>,
    /// The open route picker (`/model`, `/effort`), if any.
    model_picker: Option<ModelPicker>,
    /// The open `/providers` visibility manager, if any.
    providers_picker: Option<ProvidersPicker>,
    repository_picker: Option<rightkit_terminal_ui::nav::ListPicker>,
    repository_choices: Vec<String>,
    /// D14/D15/D5: project home, sidebar and settings overlays + `[tui]`.
    proj: crate::tui_projects::ProjUi,
    /// Live child-agent list shown directly below the composer.
    subagents: LiveSubagentsPanel,
    /// A190: elapsed-time registry for active tools (chip + rows).
    running_tools: rightkit_terminal_ui::running_panel::RunningRegistry,
    subagents_focused: bool,
    /// A070/P-16: the interactive PTY process the keyboard is attached to.
    pty: Option<rightkit_terminal_ui::pty_attach::PtyAttach>,
    /// #126: the attach pane entered the alternate screen (leave it on detach).
    pty_modal: bool,
    active_agent: Option<AgentId>,
    selection_area: Option<Rect>,
    selection_lines: Vec<String>,
    selection_anchor: Option<(usize, usize)>,
    selection_extent: Option<(usize, usize)>,
    last_selection_click: Option<(usize, usize, Instant, u8)>,
    copy_picker: Option<CopyPickerState>,
    selected_copy_message: Option<usize>,
    inspection: Option<crate::tui_inspection::InspectionState>,
    session_action_picker: Option<rightkit_terminal_ui::nav::ListPicker>,
    /// D3: the open Esc-Esc rewind picker, if any.
    rewind: Option<rewind::RewindFlow>,
    session_action_id: Option<String>,
    session_action_kinds: Vec<&'static str>,
    pending_rename: Option<String>,
    /// Typed history, folded by `reducer`.
    timeline: Timeline,
    reducer: TimelineReducer,
    /// Projection of the timeline's latest route decision.
    identity: IdentityStrip,
    /// Whether latest route attribution came from the daemon event stream.
    /// Navigation is only a startup/fallback seed; it must not overwrite an
    /// event-persisted decision when its refresh is stale or view-scoped.
    authoritative_route: bool,
    /// The session the current route is attributed to. Also the key the
    /// composer's per-view draft is stored under (S:I7).
    active_session: String,
    /// Latest host startup readiness, shown in the footer rather than the
    /// transcript. `None` until the host reports (or for hosts without one).
    readiness: Option<HostReadiness>,
    /// Admission time, including context preparation and provider wait.
    working_started: Option<Instant>,
    /// Latest real runtime signal (failure, retry, compaction, verify) for
    /// the working line; cleared when a turn starts or settles.
    working_signal: Option<rightkit_terminal_ui::transcript::timeline::RunSignal>,
    /// A failure kept on the working row after its turn settles, with the
    /// elapsed seconds at failure. Cleared by the next submit or Esc.
    failed_hold: Option<(rightkit_terminal_ui::transcript::timeline::RunSignal, u64)>,
    /// Latest bold reasoning-summary heading (Codex-style status header).
    reasoning_heading: Option<String>,
    /// Raw reasoning text of the open block, scanned for its heading.
    reasoning_buf: String,
    /// Tool calls currently running: `(call_id, activity label)`.
    active_tools: Vec<(String, String)>,
    /// Raw text of the messages queued behind the running turn, oldest
    /// first. Shown above the composer; never in the transcript until sent.
    queued_preview: Vec<String>,
    /// Latest usage accounting, for the footer's context gauge: fraction of
    /// the context window in use (`0.0..=1.0`).
    context_fraction: Option<f64>,
    /// Cumulative priced cost for active session; None means unavailable.
    cost_usd: Option<f64>,
    /// Latest prompt size and how much of it the provider served from cache.
    last_prompt: Option<(i64, i64)>,
    /// Human subject per pending approval (`shell · echo hi`), for its
    /// decision record.
    approval_subjects: std::collections::HashMap<String, String>,
    /// Approvals the user answered with "allow for this session".
    session_grants: std::collections::HashSet<String>,
    /// Calls whose result row already reached the transcript, so a repeated
    /// delivery of the same result (live plus durable) never paints twice.
    settled_tool_calls: std::collections::HashSet<String>,
    /// Session directory shell calls run in, for the approval card's
    /// `runs in` line when the call itself names none.
    approval_cwd: Option<String>,
    /// An Esc rejection already announced its own stop; the runtime's
    /// follow-up `Turn interrupted` notice for it is dropped so one stop
    /// notice shows, not two.
    swallow_interrupt_notice: bool,
    /// When the host launched, for the startup clock on a long context start.
    started_at: Instant,
    /// Calls that started together (one batch) as `(call_id, name, args)`
    /// with the batch's first start; merged into one parallel cell.
    batch: Option<(Instant, Vec<(String, String, String)>)>,
    /// Full (ctrl+o) lines of the most recently settled reasoning/exec cell.
    expandable: Option<Vec<rightkit_terminal_ui::ratatui::text::Line<'static>>>,
    /// Plan/todo list, rebuilt from durable `todo_write` calls (live and
    /// replayed on resume) and from `rehydrate_tasks`; Alt+P shows it.
    todo_overlay: rightkit_terminal_ui::todo_overlay::TodoOverlay,
    /// #136 panes: observability (Alt+O), diff review (Alt+D/]/[/A/R), PR/CI band (Alt+B).
    panes: crate::tui_panes::PaneState,
    /// Whether the running turn has shown any outcome (answer, tool, notice,
    /// failure). A turn that ends without one is reported, never left silent.
    turn_outcome_seen: bool,
    /// Last known terminal width, for computing expanded lines at settle.
    width_hint: u16,
    /// Last `(model, actor)` a route divider was drawn for.
    divider_route: Option<(String, String)>,
}

#[derive(Debug, Clone)]
struct CopyPickerState {
    markdown: String,
    code_blocks: Vec<(Option<String>, String)>,
    writing_block: bool,
}

#[derive(Clone)]
struct PendingQuestion {
    question_id: String,
    prompts: Vec<rightkit_terminal_ui::transcript::timeline::UserQuestionPrompt>,
    current: usize,
    answers: serde_json::Map<String, serde_json::Value>,
    answer_text: String,
    /// Highlighted row: `0..options.len()` are options, `options.len()` is
    /// the "type my own answer" row.
    selected: usize,
    /// Multi-select toggles for the current prompt (option index).
    toggled: Vec<usize>,
}

struct PendingPlan {
    plan_id: String,
    plan: String,
    decision: Option<bool>,
}

struct PendingCredential {
    session_id: String,
    request_id: String,
    provider: String,
    reason: String,
    secret: String,
}

fn credential_prompt_for_provider(provider: &str) -> String {
    match provider.trim().to_ascii_lowercase().as_str() {
        "cloudflare" => "Provide Cloudflare credential to continue.".to_string(),
        "flyio" | "fly.io" => "Provide Fly.io credential to continue.".to_string(),
        _ => format!("Provide credential for {} to continue.", provider.trim()),
    }
}

struct PendingElicitation {
    session_id: String,
    request_id: String,
    server_name: String,
    message: String,
    mode: rightkit_agent_engine_protocol::ElicitationRequest,
    content: String,
    /// Field-by-field form for a schema it can represent; `None` falls back
    /// to raw JSON entry in `content`.
    form: Option<rightkit_terminal_ui::render::elicitation_form::ElicitationForm>,
}

impl SessionState {
    /// A new turn starts: drop the previous turn's held failure and status
    /// header state.
    fn begin_turn_status(&mut self) {
        self.turn_outcome_seen = false;
        self.failed_hold = None;
        self.reasoning_heading = None;
        self.reasoning_buf.clear();
        self.active_tools.clear();
        self.running_tools = Default::default();
    }

    /// Operation text for the working line when no runtime signal owns it:
    /// reasoning heading, else running tool, else "Thinking".
    fn default_operation(&self) -> String {
        if let Some(heading) = &self.reasoning_heading {
            return heading.clone();
        }
        if let Some((_, label)) = self.active_tools.last() {
            return label.clone();
        }
        "Thinking".to_string()
    }

    fn clear_interaction(&mut self) {
        self.timeline.cancel_pending_interactions();
        self.pending_question = None;
        self.pending_credential = None;
        self.pending_elicitation = None;
        self.pending_plan = None;
    }

    fn interaction_text(&self) -> Option<String> {
        if let Some(root) = &self.pending_trust {
            return Some(format!(
                "Trust this folder? (yes/no)\n{}\nRightKit can read, edit and run commands here once trusted.\ny trust | n stay untrusted (/trust later) | Esc decline",
                rightkit_terminal_ui::render::display_path_str(root)
            ));
        }
        if let Some(pending) = &self.pending_credential {
            return Some(format!(
                "Credential needed · {}\n{}\n{} · Enter submit · Esc cancel",
                pending.provider,
                pending.reason,
                "•".repeat(pending.secret.chars().count().min(24))
            ));
        }
        if let Some(pending) = &self.pending_elicitation {
            return Some(match &pending.mode {
                rightkit_agent_engine_protocol::ElicitationRequest::Form { .. } if pending.form.is_some() => {
                    let form = pending.form.as_ref().map(|f| f.render_lines().join("\n"));
                    format!(
                        "MCP request · {}\n{}\n{}",
                        pending.server_name,
                        pending.message,
                        form.unwrap_or_default()
                    )
                }
                rightkit_agent_engine_protocol::ElicitationRequest::Form {
                    requested_schema, ..
                } => format!(
                    "MCP request · {}\n{}\nSchema: {}\nJSON: {}\nEnter submit · Esc decline",
                    pending.server_name, pending.message, requested_schema, pending.content
                ),
                rightkit_agent_engine_protocol::ElicitationRequest::Url { url, .. } => format!(
                    "MCP request · {}\n{}\nOpen this URL: {}\nEnter accept · Esc cancel",
                    pending.server_name, pending.message, url
                ),
            });
        }
        if let Some(pending) = &self.pending_plan {
            let choice = match pending.decision {
                Some(true) => "approve",
                Some(false) => "reject",
                None => "choose y/n",
            };
            return Some(format!(
                "Plan: {choice} | Enter submits | Esc cancels turn\n{}",
                pending.plan
            ));
        }
        let pending = self.pending_question.as_ref()?;
        let Some(prompt) = pending.prompts.get(pending.current) else {
            return Some("Answers ready: Enter retries submission | Esc declines".into());
        };
        // Same visual family as the approval card: numbered rows, `›` marks
        // the selection, dim-by-indent descriptions, then the free-text row.
        let mut lines: Vec<String> = Vec::new();
        let chip = if prompt.header.is_empty() {
            String::new()
        } else {
            format!("[{}] ", prompt.header)
        };
        lines.push(format!(
            "{chip}Question {}/{}: {}",
            pending.current + 1,
            pending.prompts.len(),
            prompt.question
        ));
        lines.push(String::new());
        let other = prompt.options.len();
        for (index, option) in prompt.options.iter().enumerate() {
            let marker = if pending.selected == index {
                "› "
            } else {
                "  "
            };
            let check = if prompt.multi_select {
                if pending.toggled.contains(&index) {
                    "[x] "
                } else {
                    "[ ] "
                }
            } else {
                ""
            };
            lines.push(format!("{marker}{}  {check}{option}", index + 1));
            if let Some(description) = prompt.descriptions.get(index).filter(|d| !d.is_empty()) {
                lines.push(format!("       {description}"));
            }
        }
        let marker = if pending.selected == other {
            "› "
        } else {
            "  "
        };
        lines.push(format!("{marker}{}  Type my own answer", other + 1));
        if pending.selected == other || !pending.answer_text.is_empty() {
            lines.push(format!("     Answer: {}", pending.answer_text));
        }
        lines.push(String::new());
        lines.push(format!(
            "↑↓ select · 1-{} pick{} · enter confirm · esc decline",
            other.max(1),
            if prompt.multi_select {
                " · space toggle"
            } else {
                ""
            }
        ));
        Some(lines.join("\n"))
    }

    #[cfg(test)]
    fn approval_key(&self, key: &KeyEvent) -> Option<(usize, bool)> {
        if key.kind != KeyEventKind::Press || !key.modifiers.contains(KeyModifiers::CONTROL) {
            return None;
        }
        let approved = match key.code {
            KeyCode::Char('y') => true,
            KeyCode::Char('n') => false,
            _ => return None,
        };
        self.pending_approvals
            .iter()
            .position(|(owner, _)| owner == &self.active_session)
            .map(|index| (index, approved))
    }

    fn new(active_session: &str) -> Self {
        Self {
            theme: Theme::new(
                rightkit_terminal_ui::terminal::Background::Dark,
                rightkit_terminal_ui::terminal::ColorDepth::TrueColor,
            ),
            pending_approvals: std::collections::VecDeque::new(),
            approval_overlay: ApprovalOverlay::new(active_session),
            approval_allow_rules: std::collections::HashMap::new(),
            deny_rules: None,
            quarantine_input: false,
            approval_input_safe: true,
            pending_question: None,
            pending_credential: None,
            pending_elicitation: None,
            pending_plan: None,
            pending_trust: None,
            clear_screen_requested: false,
            live_cells: Vec::new(),
            exec_started: std::collections::HashMap::new(),
            exec_output_seen: std::collections::HashMap::new(),
            model_picker: None,
            providers_picker: None,
            repository_picker: None,
            repository_choices: Vec::new(),
            proj: crate::tui_projects::ProjUi::default(),
            subagents: LiveSubagentsPanel::default(),
            running_tools: Default::default(),
            subagents_focused: false,
            pty: None,
            pty_modal: false,
            active_agent: None,
            selection_area: None,
            selection_lines: Vec::new(),
            selection_anchor: None,
            selection_extent: None,
            last_selection_click: None,
            copy_picker: None,
            selected_copy_message: None,
            inspection: None,
            session_action_picker: None,
            rewind: None,
            session_action_id: None,
            session_action_kinds: Vec::new(),
            pending_rename: None,
            exec_cell_index: std::collections::HashMap::new(),
            timeline: Timeline::new(),
            reducer: TimelineReducer::new(),
            identity: IdentityStrip::new(),
            authoritative_route: false,
            active_session: active_session.to_string(),
            readiness: None,
            working_started: None,
            working_signal: None,
            failed_hold: None,
            reasoning_heading: None,
            reasoning_buf: String::new(),
            active_tools: Vec::new(),
            queued_preview: Vec::new(),
            context_fraction: None,
            cost_usd: None,
            last_prompt: None,
            approval_subjects: std::collections::HashMap::new(),
            session_grants: std::collections::HashSet::new(),
            settled_tool_calls: std::collections::HashSet::new(),
            approval_cwd: None,
            swallow_interrupt_notice: false,
            started_at: Instant::now(),
            batch: None,
            expandable: None,
            todo_overlay: rightkit_terminal_ui::todo_overlay::TodoOverlay::default(),
            panes: crate::tui_panes::PaneState::default(),
            turn_outcome_seen: true,
            width_hint: 100,
            divider_route: None,
        }
    }

    /// Take (and clear) the pre-modal input-quarantine flag.
    fn take_input_quarantine(&mut self) -> bool {
        std::mem::take(&mut self.quarantine_input)
    }

    /// Reset every per-session view for a switch/new/clear. The approval
    /// overlay is rebound to `session` so a request keyed to the session
    /// being left can never be answered from the new one's view.
    fn reset_view(&mut self, session: &str) {
        self.active_agent = None;
        self.running_tools = Default::default();
        self.subagents_focused = false;
        self.pty = None;
        self.selection_anchor = None;
        self.selection_extent = None;
        self.last_selection_click = None;
        self.session_action_picker = None;
        self.rewind = None;
        self.session_action_id = None;
        self.session_action_kinds.clear();
        self.pending_rename = None;
        self.pending_approvals.clear();
        self.approval_subjects.clear();
        self.session_grants.clear();
        self.settled_tool_calls.clear();
        self.swallow_interrupt_notice = false;
        self.approval_overlay = ApprovalOverlay::new(session);
        self.quarantine_input = false;
        self.approval_input_safe = true;
        self.pending_question = None;
        self.pending_plan = None;
        self.live_cells.clear();
        self.batch = None;
        self.expandable = None;
        self.divider_route = None;
        self.exec_cell_index.clear();
        self.exec_started.clear();
        self.exec_output_seen.clear();
        self.model_picker = None;
        self.providers_picker = None;
        self.repository_picker = None;
        self.repository_choices.clear();
        self.working_started = None;
        self.working_signal = None;
        self.timeline.clear();
        self.identity = IdentityStrip::new();
        self.authoritative_route = false;
    }

    /// Push one bounded live cell, evicting the oldest settled entry first
    /// so capacity never silently drops a still-running call. `call_ids`
    /// registers the exec cell's contained ids (known at construction; the
    /// cell exposes no membership probe) for update routing. The
    /// `call_id` → index map is rebuilt on eviction since indices shift.
    fn push_live_cell(&mut self, cell: LiveCell, call_ids: Vec<String>) {
        if self.live_cells.len() >= LIVE_CELL_MAX {
            let evict = self
                .live_cells
                .iter()
                .position(|c| match c {
                    LiveCell::Reasoning { settled, .. } => *settled,
                    LiveCell::Exec(exec) => !exec.is_active(),
                })
                .unwrap_or(0);
            self.live_cells.remove(evict);
            self.exec_cell_index.retain(|_, slot| *slot != evict);
            for slot in self.exec_cell_index.values_mut() {
                if *slot > evict {
                    *slot -= 1;
                }
            }
        }
        for call in call_ids {
            self.exec_cell_index.insert(call, self.live_cells.len());
        }
        self.live_cells.push(cell);
    }

    /// The open exec cell holding `call_id`, if any.
    fn exec_cell_for(&mut self, call_id: &str) -> Option<&mut ExecCell> {
        let index = self.exec_cell_index.get(call_id).copied()?;
        match self.live_cells.get_mut(index) {
            Some(LiveCell::Exec(exec)) => Some(exec),
            _ => {
                self.exec_cell_index.remove(call_id);
                None
            }
        }
    }

    /// Settle the exec cell holding `call_id` into a transcript system
    /// message and drop it from the live set. Returns false when the id
    /// is unknown — an orphan settle must not attach to an unrelated cell.
    fn settle_exec_cell(&mut self, transcript: &mut Transcript, call_id: &str) -> bool {
        let Some(index) = self.exec_cell_index.remove(call_id) else {
            return false;
        };
        if !matches!(self.live_cells.get(index), Some(LiveCell::Exec(_))) {
            return false;
        }
        let LiveCell::Exec(cell) = self.live_cells.remove(index) else {
            return false;
        };
        // Index bookkeeping: entries after the removed cell shift down.
        for slot in self.exec_cell_index.values_mut() {
            if *slot > index {
                *slot -= 1;
            }
        }
        self.expandable = Some(cell.transcript_lines(self.width_hint, &self.theme));
        transcript.push_cell(Box::new(cell), &self.theme);
        true
    }

    /// Settle the open reasoning block (always the last live cell) into the
    /// transcript as its one-line summary, so the live rail and the scrollback
    /// copy never paint the same text twice. The runtime streams reasoning
    /// without an end marker, so prose, a tool call and the end of the turn
    /// each settle it in the order things happened.
    fn settle_reasoning(&mut self, transcript: &mut Transcript) {
        // The open block is normally the last live cell, but a tool row that
        // started after it must not strand it: settle wherever it sits.
        let Some(index) = self
            .live_cells
            .iter()
            .rposition(|c| matches!(c, LiveCell::Reasoning { settled: false, .. }))
        else {
            return;
        };
        if let LiveCell::Reasoning { text, .. } = self.live_cells.remove(index) {
            // Index bookkeeping: cells after the removed one shift down.
            for slot in self.exec_cell_index.values_mut() {
                if *slot > index {
                    *slot -= 1;
                }
            }
            if !text.trim().is_empty() {
                let cell = ReasoningCell::new(text, false);
                self.expandable = Some(cell.transcript_lines(self.width_hint, &self.theme));
                transcript.push_cell(Box::new(cell), &self.theme);
            }
        }
    }

    /// Calls still shown as running once no turn is in flight (a stop that
    /// never delivered their results) are settled as failed and moved into the
    /// transcript, so a cancelled command does not keep a live `●` row.
    /// Returns whether anything was settled.
    fn settle_stale_live_tools(&mut self, transcript: &mut Transcript) -> bool {
        if self.working_started.is_some() {
            return false;
        }
        self.settle_reasoning(transcript);
        let stale: Vec<String> = self
            .exec_cell_index
            .iter()
            .filter(|(_, index)| {
                matches!(self.live_cells.get(**index), Some(LiveCell::Exec(exec)) if exec.is_active())
            })
            .map(|(id, _)| id.clone())
            .collect();
        let mut settled = false;
        for id in stale {
            if let Some(exec) = self.exec_cell_for(&id) {
                exec.mark_failed();
            }
            settled |= self.settle_exec_cell(transcript, &id);
        }
        if settled {
            self.active_tools.clear();
            self.running_tools = Default::default();
        }
        settled
    }

    /// Display lines of all live cells at `width`, for the render path.
    /// The queue list: one pending row per message (last three) — a dashed
    /// amber rail `┆`, dimmed text and a right-aligned amber `queued · N` tag
    /// (N is the message's place in the queue) — then ONE hint row for the
    /// whole block. The solid red `▌` is reserved for messages that were sent.
    fn queue_list_lines(
        &self,
        width: u16,
        theme: &Theme,
    ) -> Vec<rightkit_terminal_ui::ratatui::text::Line<'static>> {
        use rightkit_terminal_ui::component::Role;
        use rightkit_terminal_ui::ratatui::style::Style;
        use rightkit_terminal_ui::ratatui::text::{Line, Span};
        use rightkit_terminal_ui::transcript::wrap::display_width;
        if self.queued_preview.is_empty() {
            return Vec::new();
        }
        let dim = Style::default().fg(theme.color(Role::Dim));
        let warn = Style::default().fg(theme.color(Role::Attention));
        let total = self.queued_preview.len();
        let skip = total.saturating_sub(3);
        let w = usize::from(width);
        let mut lines: Vec<Line<'static>> = self
            .queued_preview
            .iter()
            .enumerate()
            .skip(skip)
            .map(|(index, text)| {
                let first = rightkit_terminal_ui::render::sanitize(text.lines().next().unwrap_or(""));
                let tag = format!("queued · {}", index + 1);
                // Rail (2) + text + gap (2) + tag; drop the tag before
                // clipping the text when the row is too narrow for both.
                let show_tag = w >= 2 + 8 + 2 + display_width(&tag);
                let tag_w = if show_tag { display_width(&tag) + 2 } else { 0 };
                let room = w.saturating_sub(2 + tag_w).max(1);
                let mut body = first;
                if display_width(&body) > room {
                    let mut cut = String::new();
                    for ch in body.chars() {
                        if display_width(&cut) + display_width(ch.encode_utf8(&mut [0; 4])) + 1
                            > room
                        {
                            break;
                        }
                        cut.push(ch);
                    }
                    cut.push('…');
                    body = cut;
                }
                let pad = w.saturating_sub(2 + display_width(&body) + tag_w);
                let mut spans = vec![Span::styled("┆ ", warn), Span::styled(body, dim)];
                if show_tag {
                    spans.push(Span::raw(" ".repeat(pad + 2)));
                    spans.push(Span::styled(tag, warn));
                }
                Line::from(spans)
            })
            .collect();
        // While a turn runs the queue sends when it ends; after a stop (an
        // Esc rejection) nothing is running, so say what actually happens.
        let running = self.working_started.is_some();
        let (variants, last): (&[&str], &str) = if running {
            (
                &[
                    "  ↳ sends when this turn ends · ↑ edit · ctrl+x remove · ⇥ push now",
                    "  ↳ sends at turn end · ↑ edit · ctrl+x remove · ⇥ push now",
                    "  ↳ ↑ edit · ctrl+x remove · ⇥ push now",
                    "  ↳ ↑ edit · ^x remove · ⇥ push",
                    "  ↳ ↑ edit · ⇥ push",
                ],
                "  ↳ ⇥ push",
            )
        } else {
            (
                &[
                    "  ↳ held · sends after your next message · ↑ edit · ctrl+x remove",
                    "  ↳ sends after your next message · ↑ edit · ctrl+x remove",
                    "  ↳ held · ↑ edit · ctrl+x remove",
                    "  ↳ ↑ edit · ^x remove",
                ],
                "  ↳ ↑ edit",
            )
        };
        let hint = variants
            .iter()
            .copied()
            .find(|h| display_width(h) <= w)
            .unwrap_or(last);
        lines.push(rightkit_terminal_ui::render::cell::clipped_line(
            Line::from(Span::styled(hint, dim)),
            width,
        ));
        lines
    }

    fn live_cell_lines(
        &self,
        width: u16,
        theme: &Theme,
    ) -> Vec<rightkit_terminal_ui::ratatui::text::Line<'static>> {
        let mut lines = Vec::new();
        for live in &self.live_cells {
            match live {
                LiveCell::Reasoning { text, .. } => {
                    lines
                        .extend(ReasoningCell::streaming(text.clone()).display_lines(width, theme));
                }
                LiveCell::Exec(exec) => lines.extend(exec.display_lines(width, theme)),
            }
        }
        lines
    }

    /// Shared approval admission for both request event shapes: dedup on
    /// (session, request_id), bind the overlay request to this session's
    /// authority ids, and quarantine pre-modal input. `kind` is the typed
    /// preview the wire supplied — `Generic` when only a description exists.
    fn enqueue_approval(
        &mut self,
        _transcript: &mut Transcript,
        request_id: String,
        subject: String,
        kind: ApprovalKind,
        policy_source: Option<&str>,
    ) {
        let key = (self.active_session.clone(), request_id.clone());
        if self.pending_approvals.contains(&key) {
            return;
        }
        self.pending_approvals.push_back(key);
        self.approval_subjects.insert(request_id.clone(), subject);
        // A command can be granted for the session: `ApprovedForSession`
        // reaches `resolve_tool_approval`, which records a session policy
        // rule on the exact command. Other kinds have no such grant.
        let mut options = vec![ApprovalDecision::Approved];
        // "Always allow" is offered only for an eligible request that no
        // known deny rule blocks; unknown rules withhold it.
        let allow_rule = rightkit_terminal_ui::render::allow_rule::derive_allow_rule(&kind, policy_source)
            .filter(|_| match &self.deny_rules {
                Some(rules) => !request_denied_by_rules(&kind, rules),
                None => false,
            });
        if allow_rule.is_some() {
            options.push(ApprovalDecision::ApprovedForPrefix);
        }
        if matches!(kind, ApprovalKind::Exec { .. }) {
            options.push(ApprovalDecision::ApprovedForSession);
        }
        if let Some(rule) = &allow_rule {
            self.approval_allow_rules
                .insert(request_id.clone(), (rule.clone(), kind.clone()));
        }
        options.push(ApprovalDecision::Denied);
        options.push(ApprovalDecision::Redirect);
        let policy_reason = approval_policy_reason(policy_source, &kind);
        // The overlay is bound to this session, so a request from any other
        // session is rejected inside `enqueue` and can never be answered
        // from this view.
        self.approval_overlay.enqueue(
            ApprovalRequest::new(self.active_session.clone(), request_id, kind, options)
                .with_policy_reason(policy_reason)
                .with_allow_rule(allow_rule),
        );
        // Buffered keystrokes belong to the pre-modal screen: the drive
        // loop drains them before the first draw. The card itself is the
        // only approval prompt; nothing is echoed into the transcript.
        self.quarantine_input = true;
        self.approval_input_safe = false;
    }

    /// S:I3 — fold one typed event into the timeline and push the matching
    /// presentation into the transcript (settled text) or the live-cell
    /// rail (still-moving projections), so nothing arrives as a dropped or
    /// raw-labelled event.
    /// Seed the plan list from durable task-graph rows (`rehydrate_tasks`) so
    /// it survives compaction/resume without transcript rows.
    fn seed_todo_from_tasks(&mut self, tasks: &[rightkit_agent_engine_protocol::TaskNode]) {
        self.todo_overlay
            .apply_todo_write(&todo_args_from_tasks(tasks));
    }

    fn record(&mut self, transcript: &mut Transcript, event: DaemonEvent) {
        if let DaemonEvent::TaskGraph { todos } = &event {
            // Durable plan state: rebuilds the list on resume after compaction.
            let items: Vec<serde_json::Value> = todos
                .iter()
                .map(|(title, status)| serde_json::json!({"content": title, "status": status}))
                .collect();
            self.todo_overlay
                .apply_todo_write(&serde_json::json!({ "todos": items }));
        }
        if let DaemonEvent::ToolStarted {
            name, args_preview, ..
        } = &event
        {
            if name == "todo_write" {
                if let Ok(args) = serde_json::from_str::<serde_json::Value>(args_preview) {
                    self.todo_overlay.apply_todo_write(&args);
                }
            }
        }
        // #136 panes: usage and live tool timings (replayed events carry no
        // live turn, so they never record a timing).
        match &event {
            DaemonEvent::Usage {
                context_size,
                context_permille,
                cache_read,
                prompt,
                cost_micros,
                cost_reported,
            } => {
                self.panes.on_usage(
                    *context_size,
                    *context_permille,
                    *cache_read,
                    *prompt,
                    *cost_micros,
                    *cost_reported,
                );
                if self.working_started.is_some() {
                    self.panes
                        .poll_ci(&std::env::current_dir().unwrap_or_default(), false);
                }
            }
            DaemonEvent::ToolStarted { call_id, name, .. } if self.working_started.is_some() => {
                self.panes.tool_started(call_id, name);
            }
            DaemonEvent::ToolUpdated {
                call_id, status, ..
            } if *status != rightkit_terminal_ui::transcript::timeline::ToolStatus::Running => {
                self.panes.tool_finished(call_id);
            }
            _ => {}
        }
        if matches!(&event, DaemonEvent::RouteDecided { .. }) {
            self.authoritative_route = true;
        }
        let was_route = matches!(&event, DaemonEvent::RouteDecided { .. });
        match &event {
            DaemonEvent::CredentialRequested {
                tool_call_id,
                provider,
            } => {
                self.pending_credential = Some(PendingCredential {
                    session_id: self.active_session.clone(),
                    request_id: tool_call_id.clone(),
                    provider: provider.clone(),
                    reason: credential_prompt_for_provider(provider),
                    secret: String::new(),
                });
                self.quarantine_input = true;
                self.approval_input_safe = false;
            }
            DaemonEvent::CredentialResolved { tool_call_id, .. } => {
                if self
                    .pending_credential
                    .as_ref()
                    .is_some_and(|pending| pending.request_id == *tool_call_id)
                {
                    if let Some(mut pending) = self.pending_credential.take() {
                        pending.secret.clear();
                    }
                    self.approval_input_safe = true;
                }
            }
            DaemonEvent::McpElicitationRequested {
                server_name,
                request_id,
                mode,
                message,
                requested_schema,
                url,
                elicitation_id,
            } => {
                let mode = match mode.as_str() {
                    "form" => rightkit_agent_engine_protocol::ElicitationRequest::Form {
                        message: message.clone(),
                        requested_schema: requested_schema
                            .as_deref()
                            .and_then(|json| serde_json::from_str(json).ok())
                            .unwrap_or_else(|| serde_json::json!({})),
                    },
                    _ => rightkit_agent_engine_protocol::ElicitationRequest::Url {
                        message: message.clone(),
                        url: url.clone().unwrap_or_default(),
                        elicitation_id: elicitation_id.clone().unwrap_or_default(),
                    },
                };
                let form = match &mode {
                    rightkit_agent_engine_protocol::ElicitationRequest::Form {
                        requested_schema, ..
                    } => rightkit_terminal_ui::render::elicitation_form::ElicitationForm::from_schema(
                        requested_schema,
                    )
                    .ok(),
                    _ => None,
                };
                self.pending_elicitation = Some(PendingElicitation {
                    form,
                    session_id: self.active_session.clone(),
                    request_id: format!("{server_name}:{request_id}"),
                    server_name: server_name.clone(),
                    message: message.clone(),
                    mode,
                    content: String::new(),
                });
                self.quarantine_input = true;
                self.approval_input_safe = false;
            }
            DaemonEvent::Usage {
                context_permille,
                cache_read,
                prompt,
                cost_micros,
                cost_reported,
                ..
            } => {
                // The gauge reads the runtime's own figures; nothing is
                // computed here. A zero fraction means "window unknown".
                self.context_fraction =
                    (*context_permille > 0).then(|| *context_permille as f64 / 1000.0);
                self.last_prompt = (*prompt > 0).then_some((*prompt, *cache_read));
                if *cost_reported {
                    self.cost_usd = cost_micros.map(|micros| micros as f64 / 1_000_000.0);
                }
            }
            DaemonEvent::ReasoningDelta { token } => {
                if self.reasoning_buf.len() < 8192 {
                    self.reasoning_buf.push_str(token);
                }
                if let Some(heading) = reasoning_heading_of(&self.reasoning_buf) {
                    self.reasoning_heading = Some(heading);
                }
            }
            DaemonEvent::ReasoningSettled => self.reasoning_buf.clear(),
            DaemonEvent::ToolStarted {
                call_id,
                name,
                args_preview,
            } => {
                // Prose that came before a tool call is complete: settle it so
                // it flushes above the live tool rows, in the order it was said.
                transcript.finalize_last();
                // A tool phase replaces the reasoning heading as the header.
                self.reasoning_heading = None;
                self.active_tools
                    .push((call_id.clone(), tool_activity_label(name, args_preview)));
                self.running_tools.start(
                    rightkit_terminal_ui::running_panel::WorkKind::Tool,
                    call_id,
                    name,
                    Instant::now(),
                );
            }
            DaemonEvent::ToolUpdated {
                call_id, status, ..
            } if *status != rightkit_terminal_ui::transcript::timeline::ToolStatus::Running => {
                self.active_tools.retain(|(id, _)| id != call_id);
                self.running_tools
                    .finish(rightkit_terminal_ui::running_panel::WorkKind::Tool, call_id);
            }
            _ => {}
        }
        // An interrupted turn leaves its partial answer open ("Assistant …");
        // the stop notice that follows is the point it settles.
        if matches!(&event, DaemonEvent::System { text } if text.starts_with("Turn interrupted")) {
            transcript.finalize_last();
        }
        // The runtime marks no end of reasoning: prose or a tool call is it.
        // Any other visible transcript row (a completed answer replayed from
        // durable history without deltas, a user message, a notice, a failure)
        // also ends it, so the Thought row never lands after what followed it.
        if matches!(
            &event,
            DaemonEvent::AssistantDelta { .. }
                | DaemonEvent::AssistantCompleted { .. }
                | DaemonEvent::AssistantSettled
                | DaemonEvent::ToolStarted { .. }
                | DaemonEvent::UserMessage { .. }
                | DaemonEvent::System { .. }
                | DaemonEvent::Status(_)
        ) {
            self.settle_reasoning(transcript);
        }
        if match &event {
            DaemonEvent::AssistantDelta { token } => !token.trim().is_empty(),
            DaemonEvent::AssistantCompleted { text } => !text.trim().is_empty(),
            DaemonEvent::System { text } => !is_internal_wire_label(text),
            DaemonEvent::ToolStarted { .. } | DaemonEvent::Status(_) => true,
            _ => false,
        } {
            self.turn_outcome_seen = true;
        }
        match &event {
            // These are represented by request cards/state above; the timeline
            // reducer records their durable form without a duplicate transcript row.
            DaemonEvent::McpElicitationRequested { .. }
            | DaemonEvent::CredentialRequested { .. }
            | DaemonEvent::TaskGraph { .. }
            | DaemonEvent::CredentialResolved { .. } => {}
            DaemonEvent::UserMessage { text } => {
                self.swallow_interrupt_notice = false;
                // Ctrl+O expands the latest cell of the current turn only; a
                // cell from an earlier turn would be re-appended out of order.
                self.expandable = None;
                transcript.push(Message::user(text.clone()));
            }
            // An Esc rejection already printed its own stop notice; the
            // runtime's matching `Turn interrupted` is the same stop, so it
            // is recorded in the timeline but not shown a second time.
            DaemonEvent::System { text }
                if self.swallow_interrupt_notice && text.starts_with("Turn interrupted") =>
            {
                self.swallow_interrupt_notice = false;
            }
            // Internal wire events that fell through to a `[camelCaseName]`
            // diagnostic carry no user-facing content; never print the name.
            DaemonEvent::System { text } if is_internal_wire_label(text) => {}
            DaemonEvent::System { text } => transcript.push(Message::system(text.clone())),
            DaemonEvent::Status(signal) => {
                let view = rightkit_terminal_ui::render::status::signal_view(signal);
                transcript.push(Message::system(view.notice));
                if let rightkit_terminal_ui::transcript::timeline::RunSignal::Failed { .. } = signal {
                    let elapsed = self.working_started.map_or(0, |s| s.elapsed().as_secs());
                    self.failed_hold = Some((signal.clone(), elapsed));
                }
                self.working_signal = Some(signal.clone());
            }
            DaemonEvent::AssistantDelta { token } => {
                // Mirror `Transcript::append_stream`'s own contract: a delta
                // can only extend an open assistant message, so open one
                // first when the last live message is not that.
                let needs_open = transcript
                    .live_messages()
                    .last()
                    .map(|m| m.speaker != Speaker::Assistant || m.finalized)
                    .unwrap_or(true);
                if needs_open {
                    transcript.push(Message::assistant_streaming());
                }
                transcript.append_stream(token);
            }
            // Folded into the gauge above; usage is never a transcript line.
            DaemonEvent::Usage { .. } => {}
            DaemonEvent::AssistantSettled => transcript.finalize_last(),
            DaemonEvent::AssistantCompleted { text } => transcript.complete_assistant(text),
            DaemonEvent::ReasoningDelta { token } => match self.live_cells.last_mut() {
                Some(LiveCell::Reasoning {
                    text,
                    settled: false,
                }) => text.push_str(token),
                _ => self.push_live_cell(
                    LiveCell::Reasoning {
                        text: token.clone(),
                        settled: false,
                    },
                    Vec::new(),
                ),
            },
            DaemonEvent::ReasoningSettled => self.settle_reasoning(transcript),
            DaemonEvent::ToolStarted {
                call_id,
                name,
                args_preview,
            } => {
                self.exec_started.insert(call_id.clone(), Instant::now());
                self.exec_output_seen.insert(call_id.clone(), 0);
                // Calls that start within one batch (same instant, no output
                // yet) are drawn as one parallel block.
                let merge = match (&self.batch, self.live_cells.last()) {
                    (Some((at, ids)), Some(LiveCell::Exec(_))) => {
                        at.elapsed() < Duration::from_millis(400)
                            && ids.iter().all(|(id, _, _)| {
                                self.exec_output_seen.get(id).copied().unwrap_or(0) == 0
                                    && self.exec_cell_index.contains_key(id)
                            })
                    }
                    _ => false,
                };
                if merge {
                    let (at, mut ids) = self.batch.take().expect("batch");
                    ids.push((call_id.clone(), name.clone(), args_preview.clone()));
                    let calls = ids
                        .iter()
                        .map(|(id, n, a)| ExecCall::tool(id.clone(), n.clone(), a.clone()))
                        .collect();
                    let slot = self.live_cells.len() - 1;
                    self.live_cells[slot] = LiveCell::Exec(ExecCell::parallel(calls));
                    self.exec_cell_index.insert(call_id.clone(), slot);
                    self.batch = Some((at, ids));
                } else {
                    let call = ExecCall::tool(call_id.clone(), name.clone(), args_preview.clone());
                    self.batch = Some((
                        Instant::now(),
                        vec![(call_id.clone(), name.clone(), args_preview.clone())],
                    ));
                    self.push_live_cell(LiveCell::Exec(ExecCell::new(call)), vec![call_id.clone()]);
                }
            }
            DaemonEvent::ToolUpdated {
                call_id,
                status,
                output_preview,
            } => {
                use rightkit_terminal_ui::transcript::timeline::ToolStatus;
                match status {
                    ToolStatus::Running => {
                        // The preview is cumulative; feed only the unseen tail.
                        let seen = self.exec_output_seen.get(call_id).copied().unwrap_or(0);
                        let chunk = output_preview.get(seen..).unwrap_or("").to_string();
                        if let Some(exec) = self.exec_cell_for(call_id) {
                            exec.append_output(call_id, &chunk);
                        }
                        self.exec_output_seen
                            .insert(call_id.clone(), seen + chunk.len());
                    }
                    ToolStatus::Succeeded | ToolStatus::Failed => {
                        // A196: the engine appends a `[file collision: ...]`
                        // note when another session edited the same file.
                        for line in output_preview
                            .lines()
                            .filter(|line| line.starts_with("[file collision:"))
                        {
                            transcript.push(Message::system(
                                line.trim_start_matches('[')
                                    .trim_end_matches(']')
                                    .to_string(),
                            ));
                        }
                        let exit_code = if *status == ToolStatus::Succeeded {
                            0
                        } else {
                            tool_exit_code(output_preview)
                        };
                        let duration = self
                            .exec_started
                            .remove(call_id)
                            .map(|started| started.elapsed())
                            .unwrap_or(Duration::ZERO);
                        let mut unplaced =
                            Some(CommandOutput::new(exit_code, output_preview.clone()));
                        if let Some(exec) = self.exec_cell_for(call_id) {
                            if let Some(output) = unplaced.take() {
                                exec.complete_call(call_id, output, duration);
                            }
                        }
                        if let Some(output) =
                            unplaced.filter(|_| !self.settled_tool_calls.contains(call_id))
                        {
                            // No live row holds this call: its start never
                            // arrived (a session-granted run is auto-approved
                            // and can reach the result without a request
                            // card) or the row was evicted. Rebuild the row
                            // from the timeline so the run is never invisible.
                            let (name, args) = self
                                .timeline
                                .entries()
                                .iter()
                                .rev()
                                .find_map(|e| match e {
                                    rightkit_terminal_ui::transcript::timeline::TimelineEntry::Tool {
                                        call_id: id,
                                        name,
                                        args_preview,
                                        ..
                                    } if id == call_id => {
                                        Some((name.clone(), args_preview.clone()))
                                    }
                                    _ => None,
                                })
                                .unwrap_or_else(|| ("tool".to_string(), String::new()));
                            let mut cell =
                                ExecCell::new(ExecCall::tool(call_id.clone(), name, args));
                            cell.complete_call(call_id, output, duration);
                            self.push_live_cell(LiveCell::Exec(cell), vec![call_id.clone()]);
                        }
                        self.exec_output_seen.remove(call_id);
                        if self.settle_exec_cell(transcript, call_id) {
                            if self.settled_tool_calls.len() > 512 {
                                self.settled_tool_calls.clear();
                            }
                            self.settled_tool_calls.insert(call_id.clone());
                        }
                    }
                }
            }
            DaemonEvent::ApprovalRequested {
                request_id,
                summary,
            } => {
                self.enqueue_approval(
                    transcript,
                    request_id.clone(),
                    summary.clone(),
                    ApprovalKind::Generic {
                        description: summary.clone(),
                    },
                    None,
                );
            }
            DaemonEvent::ToolApprovalRequested {
                request_id,
                summary,
                call_name,
                arguments_json,
                policy_source,
            } => {
                // Rich wire payload: the nested call's name/argumentsJson
                // become the donor Exec/Patch preview instead of a flattened
                // Generic description. Identity, dedup, session binding and
                // the input quarantine are identical to the free-text path.
                let kind = approval_kind_for_call(
                    call_name,
                    arguments_json,
                    summary,
                    self.approval_cwd.as_deref(),
                );
                let subject = approval_subject(&kind, call_name, summary);
                self.enqueue_approval(
                    transcript,
                    request_id.clone(),
                    subject,
                    kind,
                    policy_source.as_deref(),
                );
            }
            DaemonEvent::ApprovalResolved {
                request_id,
                approved,
            } => {
                self.pending_approvals
                    .retain(|(session, id)| session != &self.active_session || id != request_id);
                self.approval_overlay
                    .resolve_external(&self.active_session, request_id);
                // Settled approvals get the donor decision-cell record in
                // the transcript; the summary is read back off the timeline
                // entry the request event already folded in.
                let (timeline_summary, already_recorded) = self
                    .timeline
                    .entries()
                    .iter()
                    .rev()
                    .find_map(|e| match e {
                        rightkit_terminal_ui::transcript::timeline::TimelineEntry::Approval {
                            request_id: id,
                            summary,
                            resolution,
                            ..
                        } if id == request_id => Some((summary.clone(), resolution.is_some())),
                        _ => None,
                    })
                    .unwrap_or_default();
                // The local decision and the durable `toolApprovalResolved`
                // event both land here; only the first paints a cell. The
                // subject is the human one captured with the request
                // (`shell · echo hi`), never the raw `name {json}` summary.
                let summary = self
                    .approval_subjects
                    .remove(request_id)
                    .unwrap_or(timeline_summary);
                let granted_for_session = self.session_grants.remove(request_id);
                let cell = approval_decision_cell(
                    &summary,
                    if *approved && granted_for_session {
                        DecisionOutcome::ApprovedForSession
                    } else if *approved {
                        DecisionOutcome::Approved
                    } else {
                        DecisionOutcome::Denied
                    },
                    DecisionActor::User,
                );
                if !already_recorded {
                    transcript.push_cell(cell, &self.theme);
                }
            }
            DaemonEvent::UserQuestionAsked {
                question_id,
                questions,
            } => {
                if !questions.is_empty() {
                    self.pending_question = Some(PendingQuestion {
                        question_id: question_id.clone(),
                        prompts: questions.clone(),
                        current: 0,
                        answers: serde_json::Map::new(),
                        answer_text: String::new(),
                        selected: 0,
                        toggled: Vec::new(),
                    });
                    self.quarantine_input = true;
                    self.approval_input_safe = false;
                }
                let lines = questions
                    .iter()
                    .enumerate()
                    .map(|(index, question)| {
                        let options = question
                            .options
                            .iter()
                            .enumerate()
                            .map(|(option, label)| format!("{}:{label}", option + 1))
                            .collect::<Vec<_>>()
                            .join(" ");
                        format!("{}. {} [{}]", index + 1, question.question, options)
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                transcript.push(Message::system(format!("Question: {lines}")));
            }
            DaemonEvent::UserQuestionAnswered { question_id } => {
                if self
                    .pending_question
                    .as_ref()
                    .is_some_and(|pending| pending.question_id == *question_id)
                {
                    self.pending_question = None;
                }
            }
            DaemonEvent::PlanPresented { plan_id, plan } => {
                self.pending_plan = Some(PendingPlan {
                    plan_id: plan_id.clone(),
                    plan: plan.clone(),
                    decision: None,
                });
                self.quarantine_input = true;
                self.approval_input_safe = false;
                transcript.push(Message::system(format!("Plan awaiting approval:\n{plan}\nPress y/n to choose, Enter submit, Esc cancel.")));
            }
            DaemonEvent::PlanResolved { plan_id, .. } => {
                if self
                    .pending_plan
                    .as_ref()
                    .is_some_and(|pending| pending.plan_id == *plan_id)
                {
                    self.pending_plan = None;
                }
            }
            DaemonEvent::RouteDecided { model, actor, .. } => {
                // The first route only seeds the identity strip; a later
                // change of model or actor draws one dim divider.
                let now = (model.clone(), actor.clone());
                if let Some(previous) = &self.divider_route {
                    if *previous != now && !model.is_empty() {
                        transcript.push_cell(
                            Box::new(NoticeCell::new(
                                format!(
                                    "\u{2500}\u{2500} now {actor} \u{b7} {model} \u{2500}\u{2500}"
                                ),
                                Role::Dim,
                            )),
                            &self.theme,
                        );
                    }
                }
                if !model.is_empty() {
                    self.divider_route = Some(now);
                }
            }
            DaemonEvent::TaskUpdated { label, status, .. } => {
                transcript.push_cell(
                    Box::new(NoticeCell::new(
                        format!("Task {label}: {status:?}"),
                        Role::Dim,
                    )),
                    &self.theme,
                );
            }
            DaemonEvent::SubagentUpdated { label, status, .. } => {
                transcript.push_cell(
                    Box::new(NoticeCell::new(
                        format!("Subagent {label}: {status:?}"),
                        Role::Dim,
                    )),
                    &self.theme,
                );
            }
            DaemonEvent::CheckReported {
                label,
                passed,
                detail,
                ..
            } => {
                let mut text = format!(
                    "Check {label}: {}",
                    if *passed { "passed" } else { "failed" }
                );
                if !detail.is_empty() {
                    text.push_str(&format!(" — {detail}"));
                }
                let role = if *passed {
                    Role::Success
                } else {
                    Role::Failure
                };
                transcript.push_cell(Box::new(NoticeCell::new(text, role)), &self.theme);
            }
        }
        self.reducer.apply(&mut self.timeline, event);
        if was_route {
            let before = self.identity.attribution().map(|a| a.model.clone());
            self.identity.sync(&self.timeline);
            if self.identity.boundary_crossed() {
                // The route changed mid-session: mark the boundary in the
                // transcript so earlier output stays attributed to the
                // earlier model.
                let after = self
                    .identity
                    .attribution()
                    .map(|a| a.model.clone())
                    .unwrap_or_default();
                let text = match before {
                    Some(before) if !before.is_empty() => {
                        format!("\u{2500}\u{2500} route changed: {before} \u{2192} {after} \u{2500}\u{2500}")
                    }
                    _ => format!("\u{2500}\u{2500} route: {after} \u{2500}\u{2500}"),
                };
                transcript.push_cell(Box::new(NoticeCell::new(text, Role::Dim)), &self.theme);
            }
        }
    }

    /// S:I3 — the common case: one host-side notice, recorded in both
    /// structures so the timeline is never a subset of the transcript.
    fn notice(&mut self, transcript: &mut Transcript, text: impl Into<String>) {
        // A stop notice reads after the call it stopped, so settle any row
        // the stop left running before the notice lands.
        self.settle_stale_live_tools(transcript);
        self.record(transcript, DaemonEvent::System { text: text.into() });
    }
}

/// Convert durable task-graph rows into `todo_write`-shaped arguments.
fn todo_args_from_tasks(tasks: &[rightkit_agent_engine_protocol::TaskNode]) -> serde_json::Value {
    use rightkit_agent_engine_protocol::TaskState;
    let todos: Vec<serde_json::Value> = tasks
        .iter()
        .map(|t| {
            let status = match t.state {
                TaskState::Done => "completed",
                TaskState::InProgress => "in_progress",
                _ => "pending",
            };
            serde_json::json!({"content": t.title, "status": status})
        })
        .collect();
    serde_json::json!({ "todos": todos })
}

/// S:I7 — read the `--plain` / `--reduced-motion` pair the CLI would pass,
/// combined with the environment conventions `LayoutMode::resolve` expects.
///
/// `NO_COLOR` and `TERM=dumb` are read here rather than in the TUI crate so
/// this binary keeps owning process-environment access; the crate stays a
/// pure function of the four booleans it is given.
fn resolve_layout_mode(host: &impl TurnHost) -> LayoutMode {
    let (no_color, term_dumb) = env_layout_signals(
        std::env::var("NO_COLOR").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
    );
    LayoutMode::resolve(
        host.plain_output(),
        host.reduced_motion(),
        no_color,
        term_dumb,
    )
}

/// `NO_COLOR` set to anything non-empty, and `TERM=dumb`. Pure (the raw
/// values are passed in) so the resolution is testable without mutating
/// the process environment.
fn env_layout_signals(no_color: Option<&str>, term: Option<&str>) -> (bool, bool) {
    // NO_COLOR is a presence convention: an explicitly empty value still
    // means the user opted out. Match terminal capability detection rather
    // than silently re-enabling motion for `NO_COLOR=`.
    let no_color = no_color.is_some();
    let term_dumb = term
        .map(|value| value.eq_ignore_ascii_case("dumb"))
        .unwrap_or(false);
    (no_color, term_dumb)
}

/// Restores the terminal on every exit path: normal quit, error, or panic.
///
/// Armed as soon as raw mode is enabled and dropped exactly once, at the very
/// end of [`drive`]. A panic anywhere after `engage()` still runs `Drop`
/// during unwind; the panic hook installed alongside it covers output
/// written before the default hook prints, so the message itself is not
/// swallowed by a broken terminal.
struct TerminalGuard {
    active: Arc<AtomicBool>,
}

impl TerminalGuard {
    fn engage() -> io::Result<Self> {
        Self::engage_with(rightkit_terminal_ui::terminal::session::set_modes)
    }

    fn engage_with(set_modes: impl FnOnce() -> io::Result<()>) -> io::Result<Self> {
        if let Err(error) = set_modes() {
            restore_terminal();
            return Err(error);
        }
        let active = Arc::new(AtomicBool::new(true));
        install_panic_hook(active.clone());
        stderr_guard::silence();
        Ok(Self { active })
    }
}

/// While the live viewport owns the terminal, raw writes to the process's
/// stderr (a library `eprintln!`, a child's diagnostic) land on the same
/// screen as the painted frame and corrupt it. The guard points stderr at the
/// null device for the lifetime of the viewport and puts the original back in
/// [`restore_terminal`], so errors printed after the TUI exits (and the panic
/// message, which prints after the hook restores the terminal) still reach the
/// user. Compiled out of unit-test binaries, where fd 2 belongs to the harness.
#[cfg(not(test))]
mod stderr_guard {
    #[cfg(windows)]
    mod imp {
        use std::sync::atomic::{AtomicIsize, Ordering};

        static SAVED: AtomicIsize = AtomicIsize::new(0);
        const STD_ERROR_HANDLE: u32 = -12i32 as u32;

        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetStdHandle(which: u32) -> isize;
            fn SetStdHandle(which: u32, handle: isize) -> i32;
        }

        pub fn silence() {
            if SAVED.load(Ordering::SeqCst) != 0 {
                return;
            }
            let Ok(null) = std::fs::OpenOptions::new().write(true).open("NUL") else {
                return;
            };
            // SAFETY: plain handle queries/swaps on this process's own std handle.
            let saved = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
            if saved == 0 || saved == -1 {
                return;
            }
            use std::os::windows::io::IntoRawHandle;
            let null = null.into_raw_handle() as isize;
            if unsafe { SetStdHandle(STD_ERROR_HANDLE, null) } != 0 {
                SAVED.store(saved, Ordering::SeqCst);
            }
        }

        pub fn restore() {
            let saved = SAVED.swap(0, Ordering::SeqCst);
            if saved != 0 {
                // SAFETY: `saved` is the handle this process owned before `silence`.
                unsafe { SetStdHandle(STD_ERROR_HANDLE, saved) };
            }
        }
    }

    #[cfg(unix)]
    mod imp {
        use std::sync::atomic::{AtomicI32, Ordering};

        static SAVED: AtomicI32 = AtomicI32::new(-1);

        pub fn silence() {
            if SAVED.load(Ordering::SeqCst) >= 0 {
                return;
            }
            // SAFETY: duplicating/redirecting this process's own fd 2.
            unsafe {
                let saved = libc::dup(2);
                if saved < 0 {
                    return;
                }
                let null = libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY);
                if null < 0 {
                    libc::close(saved);
                    return;
                }
                if libc::dup2(null, 2) < 0 {
                    libc::close(saved);
                } else {
                    SAVED.store(saved, Ordering::SeqCst);
                }
                libc::close(null);
            }
        }

        pub fn restore() {
            let saved = SAVED.swap(-1, Ordering::SeqCst);
            if saved >= 0 {
                // SAFETY: `saved` is the duplicate of the original fd 2.
                unsafe {
                    libc::dup2(saved, 2);
                    libc::close(saved);
                }
            }
        }
    }

    pub fn silence() {
        imp::silence();
    }

    pub fn restore() {
        imp::restore();
    }
}

#[cfg(test)]
mod stderr_guard {
    pub fn silence() {}
    pub fn restore() {}
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
        self.active.store(false, Ordering::SeqCst);
    }
}

/// Best-effort terminal restoration: leave raw mode, leave any alternate
/// screen, show the cursor, reset any leaked scroll region. Safe to call more
/// than once and safe to call from a panic hook (no allocation beyond the
/// escape-sequence writes, no locking beyond `Stdout`'s own internal lock).
fn restore_terminal() {
    stderr_guard::restore();
    #[cfg(all(test, windows))]
    let mut out = tests::RestoreOutput(io::stdout());
    #[cfg(not(all(test, windows)))]
    let mut out = io::stdout();
    let _ = execute!(out, DisableMouseCapture, DisableFocusChange);
    let _ = rightkit_terminal_ui::terminal::session::restore_after_exit();
    // Also reset VT mouse/focus explicitly: Crossterm may take its Win32
    // fallback, which restores console flags without emitting protocol resets.
    // `\x1b[r` clears any scroll region left set by `terminal::history`;
    // `\x1b[?1049l` leaves the alternate screen if a modal was open;
    // `\x1b[?25h` guarantees the cursor is visible.
    let _ = out.write_all(b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1015l\x1b[r\x1b[?1049l\x1b[?25h\x1b[?1004l");
    let _ = out.flush();
}

#[cfg(unix)]
fn suspend_tui() {
    restore_terminal();
    // `suspend_current_process` raises SIGTSTP on this process and returns once it is
    // continued (SIGCONT).
    let _ = rightkit_child_process::suspend_current_process();
    let _ = rightkit_terminal_ui::terminal::session::set_modes();
    stderr_guard::silence();
}

/// RAII guard that runs a resume closure on drop. A host surface that pauses
/// TUI input (e.g. onboarding's line prompts) wraps its work in this guard so
/// the input thread is always resumed — even on panic/early return.
pub struct InputPauseGuard<F: FnOnce()> {
    resume: Option<F>,
}

impl<F: FnOnce()> InputPauseGuard<F> {
    pub fn new(resume: F) -> Self {
        Self {
            resume: Some(resume),
        }
    }
}

impl<F: FnOnce()> Drop for InputPauseGuard<F> {
    fn drop(&mut self) {
        if let Some(resume) = self.resume.take() {
            resume();
        }
    }
}

/// Install a panic hook that restores the terminal before the default hook
/// prints, so a panicking component leaves a usable shell instead of one
/// stuck in raw mode with the cursor hidden and a stale scroll region.
///
/// Installed once per [`TerminalGuard::engage`] call; chains to whatever hook
/// was previously registered so panic output (backtraces, `RUST_BACKTRACE`)
/// is unaffected.
fn install_panic_hook(active: Arc<AtomicBool>) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if active.load(Ordering::SeqCst) {
            restore_terminal();
        }
        previous(info);
    }));
}

/// The one root session this host actually owns.
///
/// This host runs a single local session against the engine directly; there
/// is no daemon feed yet, so the session list and agent tree are exactly one
/// entry each. The ids are stable strings so [`NavIntent::SwitchSession`] and
/// [`NavIntent::Stop`] can tell "the thing I control" from "a stale or
/// remote id nobody here can reach".
const LOCAL_SESSION_ID: &str = "local";
const LOCAL_AGENT_ID: &str = "local-root";

/// Intentional render class for every built-in production tool name.
///
/// TEST FIXTURE ONLY (P1-4): the production registration in `drive()` is
/// derived live from `coderight-tools default metadata projection` via
/// `tool_render_class_to_tui`; this compile-time table is never registered on
/// the production path. It remains as the closed expected-value set the drift
/// tests below enumerate: one test proves every real production tool has an
/// intentional row here, another proves every row is still a real tool, and a
/// third proves drive()'s live-derived registration resolves through
/// `classify` to exactly these classes.
const PRODUCTION_RENDER_CLASSES: &[(&str, RenderClass)] = &[
    // Reads from the workspace or local state.
    ("read_file", RenderClass::FileRead),
    // `read_tool_result` deliberately absent: CR-30 removed that schema from
    // the catalog entirely (externalized results are retrieved through an
    // ordinary file read), so a mapping here would be a name
    // the default tool catalog no longer produces -- caught by
    // `every_mapped_render_class_name_is_a_real_production_tool` below.
    ("view_image", RenderClass::FileRead),
    ("list_files", RenderClass::FileRead),
    ("git_read", RenderClass::FileRead),
    ("git_diff", RenderClass::FileRead),
    ("lsp", RenderClass::FileRead),
    ("get_shell_output", RenderClass::FileRead),
    // Writes to the workspace.
    ("write_file", RenderClass::FileWrite),
    ("edit_file", RenderClass::FileWrite),
    ("multi_edit", RenderClass::FileWrite),
    ("undo_edit", RenderClass::FileWrite),
    ("apply_patch", RenderClass::FileWrite),
    ("todo_write", RenderClass::FileWrite),
    ("notebook", RenderClass::FileWrite),
    ("file_service", RenderClass::FileWrite),
    ("git_write", RenderClass::FileWrite),
    // Runs or controls local processes.
    ("shell", RenderClass::Shell),
    ("powershell", RenderClass::Shell),
    ("repl", RenderClass::Shell),
    ("kill_shell", RenderClass::Shell),
    ("process", RenderClass::Shell),
    // Searches content.
    ("search", RenderClass::Search),
    ("tool_search", RenderClass::Search),
    // Talks to the network or remote services.
    ("web_search", RenderClass::Network),
    ("cloudflare", RenderClass::Network),
    ("browser_fetch", RenderClass::Network),
    ("browser_open", RenderClass::Network),
    ("browser_read", RenderClass::Network),
    ("browser_act", RenderClass::Network),
    ("browser_screenshot", RenderClass::Network),
    ("browser_console", RenderClass::Network),
    ("browser_close", RenderClass::Network),
    ("recursive_fetch", RenderClass::Network),
    ("github", RenderClass::Network),
    ("context7", RenderClass::Network),
    ("mcp_call", RenderClass::Network),
    ("list_mcp_tools", RenderClass::Network),
    ("list_mcp_resources", RenderClass::Network),
    ("read_mcp_resource", RenderClass::Network),
    ("mcp_auth", RenderClass::Network),
    // Deliberately Generic: no more specific bounded presentation today.
    ("spawn_subagent", RenderClass::Generic),
    // Added by b9c2e957 (iOS Dispatch M3-A) without render classes; Generic
    // matches what `classify` derives for each from the live registry.
    ("async_inbox", RenderClass::Generic),
    ("tunnel", RenderClass::Generic),
    ("sequentialthinking", RenderClass::Generic),
    ("get_time", RenderClass::Generic),
    ("ask_user", RenderClass::Generic),
    ("exit_plan_mode", RenderClass::Generic),
    ("attach_verdict", RenderClass::Generic),
    ("finalize_artifact", RenderClass::Generic),
    ("research_report", RenderClass::Generic),
    ("sleep", RenderClass::Generic),
    ("brief", RenderClass::Generic),
    ("task_create", RenderClass::Generic),
    ("task_update", RenderClass::Generic),
    ("task_get", RenderClass::Generic),
    ("task_list", RenderClass::Generic),
    ("task_output", RenderClass::Generic),
    ("task_stop", RenderClass::Generic),
    ("schedule_cron", RenderClass::Generic),
    ("config", RenderClass::Generic),
    ("send_message", RenderClass::Generic),
    ("invoke_skill", RenderClass::Generic),
    ("team_create", RenderClass::Generic),
    ("team_delete", RenderClass::Generic),
    ("followup_subagent", RenderClass::Generic),
    ("wait_for_subagents", RenderClass::Generic),
    ("interrupt_subagent", RenderClass::Generic),
    ("worktree_enter", RenderClass::Generic),
    ("worktree_exit", RenderClass::Generic),
    // Media tools (`media_tools::MEDIA_TOOL_NAMES`).
    ("ocr", RenderClass::Shell),
    ("ocr_onnx", RenderClass::Shell),
    ("transcribe", RenderClass::Shell),
    ("web_extract", RenderClass::Shell),
    ("media_fetch", RenderClass::Shell),
    ("ffmpeg_extract", RenderClass::Shell),
    // Tools registered after this table was last updated, added to clear the
    // `every_production_tool_name_has_an_intentional_render_class` drift
    // guard.
    //
    // These rows MIRROR the authority, they do not second-guess it. The
    // registry's `tool_render_class`
    // (`engine/crates/tools/src/registry/native_types.rs:54`) declares itself
    // the single source of truth, and none of the names below appears in any
    // of its match arms — every one falls through to
    // `ToolRenderClass::Generic`. `live_registry_derived_registration_resolves
    // _through_classify` below compares this table against the live registry,
    // so recording a semantically nicer class here (say `FileWrite` for
    // `artifact_create`) would fail it: the row would disagree with what
    // `drive()` actually registers.
    //
    // Upgrading them is therefore a registry change, not a TUI change: add the
    // names to `tool_render_class` first, then move these rows to match. That
    // file is in the `coderight-tools` crate and sits outside Dispatch A's
    // file allowlist, so this lane records the gap instead of closing it.
    // `RenderClass` is presentation-only today regardless — every class
    // renders the same bordered frame — so no rendering differs either way.
    ("artifact_create", RenderClass::Generic),
    ("artifact_update", RenderClass::Generic),
    ("artifact_delete", RenderClass::Generic),
    ("artifact_read", RenderClass::Generic),
    ("artifact_list", RenderClass::Generic),
    ("flyio", RenderClass::Generic),
    ("git_clone", RenderClass::Generic),
    ("git_fetch", RenderClass::Generic),
    ("git_lfs_fetch", RenderClass::Generic),
    ("git_lfs_scan", RenderClass::Generic),
    ("git_merge", RenderClass::Generic),
    ("git_history_ops", RenderClass::Generic),
    ("image_generate", RenderClass::Generic),
    ("dev_tunnel_proxy", RenderClass::Generic),
    ("wasm_compute", RenderClass::Generic),
    ("workspace_checkpoint", RenderClass::Generic),
];

/// Reader-acknowledged exclusive-pause handshake (P1-10).
///
/// The old mechanism was a bare `AtomicBool` the reader checked only BETWEEN
/// poll cycles: a keystroke arriving while a pause was being requested could
/// still be consumed, and the pausing surface got no acknowledgement that the
/// reader had actually exited `poll`/`read`. The handshake closes both holes:
///
/// - [`InputPause::request`] blocks until the reader has observed the
///   request at a cycle boundary — i.e. AFTER its last `poll`/`read`
///   completed — and has acknowledged it will not touch the tty again until
///   resume. When `request` returns, the tty is exclusively the requester's.
/// - [`InputPause::resume`] releases the reader; it re-enters `poll` on its
///   next cycle.
///
/// Stale events that were queued in the window between the last reader cycle
/// and the acknowledgement are drained by the host (`take_input_drain`) and,
/// belt-and-braces, dropped by the drive loop while [`Self::is_paused`].
#[derive(Debug)]
pub struct InputPause {
    state: Mutex<PauseState>,
    signal: Condvar,
    quarantine: AtomicBool,
    quarantine_result: Mutex<Result<(), String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PauseState {
    /// Reader is polling/reading normally.
    Running,
    /// A pause was requested; the reader may still be inside poll/read.
    Requested,
    /// The reader exited poll/read and is parked; the tty is exclusive.
    Acknowledged,
}

impl InputPause {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(PauseState::Running),
            signal: Condvar::new(),
            quarantine: AtomicBool::new(false),
            quarantine_result: Mutex::new(Ok(())),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PauseState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Request an exclusive pause and BLOCK until the input reader confirms
    /// it has exited poll/read and will not touch the tty until resume.
    /// Idempotent when a pause is already acknowledged.
    pub fn request(&self) {
        let mut state = self.lock();
        if *state == PauseState::Running {
            *state = PauseState::Requested;
            self.signal.notify_all();
        }
        while *state != PauseState::Acknowledged {
            state = self.signal.wait(state).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// [`Self::request`] with a deadline, for callers (and tests) that must
    /// not block forever if the reader thread is gone. Returns whether the
    /// acknowledgement arrived in time.
    pub fn request_with_deadline(&self, deadline: Duration) -> bool {
        let started = std::time::Instant::now();
        let mut state = self.lock();
        if *state == PauseState::Running {
            *state = PauseState::Requested;
            self.signal.notify_all();
        }
        while *state != PauseState::Acknowledged {
            let elapsed = started.elapsed();
            if elapsed >= deadline {
                return false;
            }
            let (next, _timed_out) = self
                .signal
                .wait_timeout(state, deadline - elapsed)
                .unwrap_or_else(|e| e.into_inner());
            state = next;
        }
        true
    }

    /// Release an acknowledged pause; the reader resumes on its next cycle.
    pub fn resume(&self) {
        let mut state = self.lock();
        self.quarantine.store(false, Ordering::Release);
        if *state != PauseState::Running {
            *state = PauseState::Running;
            self.signal.notify_all();
        }
    }

    /// Ask the sole reader to discard terminal input before acknowledging.
    /// Success leaves it parked until the drive loop drains its FIFO & resumes.
    fn quarantine_and_park(&self) -> Result<(), String> {
        {
            let mut state = self.lock();
            if *state != PauseState::Running {
                return Err("terminal input is already owned by another interaction".into());
            }
            *self
                .quarantine_result
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Ok(());
            self.quarantine.store(true, Ordering::Release);
            *state = PauseState::Requested;
            self.signal.notify_all();
        }
        if !self.request_with_deadline(Duration::from_secs(2)) {
            return Err("terminal input quarantine acknowledgement timed out".into());
        }
        self.quarantine_result
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Whether a pause has been requested or acknowledged (the window in
    /// which queued events are stale from the composer's point of view).
    pub fn is_paused(&self) -> bool {
        matches!(
            *self.lock(),
            PauseState::Requested | PauseState::Acknowledged
        )
    }

    /// Reader-side checkpoint, called at the TOP of every read cycle — i.e.
    /// always OUTSIDE crossterm's poll/read. Returns `true` when the reader
    /// must stay parked: the caller skips straight to its next loop iteration
    /// without touching the tty. Observing a request here IS the
    /// acknowledgement, because this runs after the previous poll/read
    /// completed.
    fn reader_park(&self) -> bool {
        self.reader_park_with(|| Ok(()))
    }

    fn reader_park_with(&self, discard: impl FnOnce() -> io::Result<()>) -> bool {
        let mut state = self.lock();
        if *state == PauseState::Running {
            return false;
        }
        if self.quarantine.swap(false, Ordering::AcqRel) {
            *self
                .quarantine_result
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = discard().map_err(|error| error.to_string());
        }
        // Acknowledge: we are out of poll/read and will not touch the tty.
        *state = PauseState::Acknowledged;
        self.signal.notify_all();
        // Park until resumed; a fresh request arriving between resume and here
        // parks us again before any read.
        while matches!(*state, PauseState::Acknowledged | PauseState::Requested) {
            state = self.signal.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        true
    }
}

/// Runs a dedicated OS thread reading crossterm's synchronous, disambiguating
/// input API and forwarding every event over a fixed-capacity FIFO channel.
/// See the module doc for why this thread exists instead of `EventStream`.
///
/// P1-10: each cycle starts with the [`InputPause`] checkpoint, so the thread
/// never enters poll/read once a pause has been requested, and a pausing
/// surface's `request()` returns only after this thread has acknowledged from
/// outside poll/read. The thread stays alive while parked so crossterm's
/// incomplete-ESC disambiguation never has to re-learn the terminal state.
const INPUT_QUEUE_CAPACITY: usize = 32;

// Draw notifications carry no frame payload: the renderer always reads the
// current state, so stale notifications may be dropped by this bounded ring.
// The scheduler owns rate limiting/coalescing before notifications reach it.
const DRAW_QUEUE_CAPACITY: usize = 8;

#[derive(Debug, PartialEq, Eq)]
enum InputSendOutcome {
    Delivered,
    DiscardedForPause,
    ReceiverClosed,
}

fn send_input_event(
    pause: &InputPause,
    tx: &mpsc::Sender<io::Result<ct_event::Event>>,
    mut event: io::Result<ct_event::Event>,
) -> InputSendOutcome {
    loop {
        match tx.try_send(event) {
            Ok(()) => return InputSendOutcome::Delivered,
            Err(mpsc::error::TrySendError::Closed(_)) => return InputSendOutcome::ReceiverClosed,
            Err(mpsc::error::TrySendError::Full(returned)) => {
                event = returned;
                // A pause must interrupt backpressure. The event just read is
                // the only stale item held outside the bounded FIFO; events
                // already accepted remain FIFO and are drained by the host.
                if pause.is_paused() {
                    return InputSendOutcome::DiscardedForPause;
                }
                std::thread::yield_now();
            }
        }
    }
}

/// Tracks pressed chars so a `Release`-kind `Char` event with no matching
/// Press can be promoted: ConPTY encodes input chars that exist in the
/// console's OEM codepage but not on the keyboard layout as Alt-numpad
/// sequences, and the char then arrives ONLY on the Alt key-release record.
/// Crossterm reports that as a Release-kind `Char` event with no preceding
/// Press; consumers drop Release events, so the char would be lost. An
/// orphan unmodified release is exactly that case and becomes a Press.
fn normalize_alt_code_release(
    pressed_chars: &mut std::collections::HashSet<char>,
    event: ct_event::Event,
) -> ct_event::Event {
    // Windows releases can report the unshifted character after Shift is up.
    // Match the physical ASCII key instead of promoting that release twice.
    fn physical_char(c: char) -> char {
        match c {
            '?' => '/',
            '!' => '1',
            '@' => '2',
            '#' => '3',
            '$' => '4',
            '%' => '5',
            '^' => '6',
            '&' => '7',
            '*' => '8',
            '(' => '9',
            ')' => '0',
            '_' => '-',
            '+' => '=',
            '{' => '[',
            '}' => ']',
            '|' => '\\',
            ':' => ';',
            '"' => '\'',
            '<' => ',',
            '>' => '.',
            '~' => '`',
            _ => c.to_ascii_lowercase(),
        }
    }
    match event {
        ct_event::Event::Key(mut key) => {
            match (key.kind, key.code) {
                (KeyEventKind::Press | KeyEventKind::Repeat, KeyCode::Char(c)) => {
                    pressed_chars.insert(physical_char(c));
                }
                (KeyEventKind::Release, KeyCode::Char(c)) => {
                    if !pressed_chars.remove(&physical_char(c)) && key.modifiers.is_empty() {
                        key.kind = KeyEventKind::Press;
                    }
                }
                _ => {}
            }
            ct_event::Event::Key(key)
        }
        other => other,
    }
}

fn discard_protected_input() -> io::Result<()> {
    // Only the sole reader calls this, before acknowledging its protected pause.
    rightkit_terminal_ui::terminal::session::discard_pending_terminal_input()
}

fn quarantine_approval_input(
    pause: &InputPause,
    input_rx: &mut mpsc::Receiver<io::Result<ct_event::Event>>,
    session: &mut SessionState,
    transcript: &mut Transcript,
) {
    let result = pause.quarantine_and_park();
    while input_rx.try_recv().is_ok() {}
    session.approval_input_safe = result.is_ok();
    pause.resume();
    if let Err(error) = result {
        session.notice(transcript, format!("Approval input unavailable: {error}"));
    }
}

fn spawn_input_thread(pause: Arc<InputPause>) -> mpsc::Receiver<io::Result<ct_event::Event>> {
    let (tx, rx) = mpsc::channel(INPUT_QUEUE_CAPACITY);
    let mut pressed_chars = std::collections::HashSet::<char>::new();
    std::thread::Builder::new()
        .name("coderight-tui-input".into())
        .spawn(move || loop {
            if pause.reader_park_with(discard_protected_input) {
                pressed_chars.clear();
                continue;
            }
            match ct_event::poll(Duration::from_millis(50)) {
                Ok(true) => {
                    let event = ct_event::read()
                        .map(|event| normalize_alt_code_release(&mut pressed_chars, event));
                    let stop = event.is_err();
                    let outcome = send_input_event(&pause, &tx, event);
                    if matches!(outcome, InputSendOutcome::ReceiverClosed) || stop {
                        return;
                    }
                }
                Ok(false) => continue,
                Err(e) => {
                    let _ = send_input_event(&pause, &tx, Err(e));
                    return;
                }
            }
        })
        .expect("spawn coderight-tui-input thread");
    rx
}

/// Translate every crossterm event the terminal delivers into the library's
/// event contract. Mouse/focus are retained for modal navigation & focus
/// state; inline scrollback selection remains terminal-native.
fn translate(event: ct_event::Event) -> Option<TuiEvent> {
    match event {
        ct_event::Event::Key(key) => Some(TuiEvent::Key(key)),
        ct_event::Event::Paste(text) => Some(TuiEvent::Paste(text)),
        ct_event::Event::Resize(width, height) => Some(TuiEvent::Resize { width, height }),
        ct_event::Event::Mouse(mouse) => Some(TuiEvent::Mouse(mouse)),
        ct_event::Event::FocusGained => Some(TuiEvent::Focus(true)),
        ct_event::Event::FocusLost => Some(TuiEvent::Focus(false)),
    }
}

/// Whether a real interactive terminal is available at all.
///
/// `false` for a pipe, a CI runner, or anything else stdin/stdout are
/// redirected from/to -- raw mode either fails outright or succeeds against
/// a non-tty and then behaves like nothing a user would recognize.
/// `main_runtime_interactive_session.rs::run` checks this before calling
/// [`drive`] and falls back to the older line-oriented loop when it is
/// false, rather than this module silently degrading mid-session.
pub fn terminal_available() -> bool {
    use std::io::IsTerminal;
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

fn is_ctrl_c(key: &KeyEvent) -> bool {
    key.kind != KeyEventKind::Release
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
}

/// Drive one interactive TUI session to completion. Returns the process exit
/// code.
///
/// # User experience this loop implements
///
/// 1. **Launch**: the terminal enters raw mode; the composer occupies the
///    bottom rows; everything above it is the terminal's own (empty)
///    scrollback.
/// 2. **First frame**: an empty composer, cursor parked at column 0 of the
///    bottom row.
/// 3. **Typing** echoes immediately (`Lane::Immediate` in `app::lane_for`,
///    honored here by calling `FrameRequester::schedule_immediate` on every
///    `Outcome::Redraw`).
/// 4. **Submitting a turn**: Enter on non-empty text pushes the prompt into
///    the transcript, flushes it into real scrollback above the composer via
///    `Surface::insert_history`, and starts a turn with
///    `TurnHost::spawn_turn`.
/// 5. **Interjecting mid-turn**: the event loop keeps running while a turn is
///    in flight. Typing more text and pressing Enter again does *not* start
///    a second concurrent turn -- plain text queues as a FIFO steer that is
///    drained and run as sequential follow-up turns once the current one
///    finishes. A typed `/cancel` (or `/stop`, `/abort`) aborts the
///    in-flight turn IMMEDIATELY, and `/reprompt ...` aborts it and spawns
///    its replacement immediately; both discard everything else queued.
///    Typing a slash command mid-turn is gated through
///    `TurnHost::check_command` before it is allowed to execute.
/// 6. **Ctrl-C**: while a turn is in flight, a bare Ctrl-C aborts that turn
///    immediately (independent of the composer's own semantics, which have
///    no notion of an in-flight engine turn). At idle, Ctrl-C is the
///    composer's existing double-tap-to-quit: one press clears the
///    composer, a second within 500ms quits.
/// 7. **Quit**: `/quit`, Ctrl-D on an empty composer, double Ctrl-C, or a
///    closed input channel (EOF/error from the input thread) all exit
///    cleanly, restoring the terminal via `TerminalGuard`'s `Drop` on the
///    way out.
fn tool_render_class_to_tui(class: rightkit_agent_tools::coding_tools::ToolRenderClass) -> RenderClass {
    match class {
        rightkit_agent_tools::coding_tools::ToolRenderClass::FileRead => RenderClass::FileRead,
        rightkit_agent_tools::coding_tools::ToolRenderClass::FileWrite => RenderClass::FileWrite,
        rightkit_agent_tools::coding_tools::ToolRenderClass::Shell => RenderClass::Shell,
        rightkit_agent_tools::coding_tools::ToolRenderClass::Search => RenderClass::Search,
        rightkit_agent_tools::coding_tools::ToolRenderClass::Network => RenderClass::Network,
        rightkit_agent_tools::coding_tools::ToolRenderClass::Generic => RenderClass::Generic,
    }
}

pub async fn drive(host: impl TurnHost) -> i32 {
    // P1-4/KRN-007: coderight-tools owns executable registry construction.
    // TUI consumes only the tools-owned metadata projection; unknown/dynamic
    // tools remain bounded `Generic`.
    let live_classes = rightkit_agent_tools::coding_tools::default_tool_render_classes();
    // Leak the owned metadata names so they are `'static` for the TUI's
    // process-wide presentation map. No executable tool authority crosses
    // into the frontend.
    let owned: Vec<(&'static str, RenderClass)> = live_classes
        .into_iter()
        .map(|(name, class)| {
            let leaked: &'static str = Box::leak(name.into_boxed_str());
            (leaked, tool_render_class_to_tui(class))
        })
        .collect();
    rightkit_terminal_ui::register_render_classes(owned);
    // P1-4: the production path registers ONLY the live registry-derived
    // classes above. The compile-time PRODUCTION_RENDER_CLASSES table is a
    // test fixture; registering it here too (the old "last-write wins")
    // let the static table silently overwrite whatever the production
    // registry actually says.

    let _guard = match TerminalGuard::engage() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("[tui] failed to enable raw mode: {e}");
            return 1;
        }
    };

    let profile = TerminalProfile::from_env(&SystemEnv, None);
    let tui_settings = host.tui_settings();
    let mut theme = Theme::new(
        effective_background(&tui_settings.theme, profile.background),
        profile.capabilities.color_depth,
    );

    let (cols, rows) = ct_terminal::size().unwrap_or((80, 24));
    let mut composer = Composer::new();
    // Lane 4: cross-session prompt history (Up / Ctrl+R) and the shared
    // input-pause handle Ctrl+G's editor hand-off needs.
    if let Some(path) = rightkit_terminal_ui::composer::default_history_path() {
        composer.enable_persistent_history(path);
    }
    let mut notifier = build_notifier(
        &tui_settings.notify,
        &std::env::current_dir().unwrap_or_default(),
    );
    if tui_settings.vim {
        composer.set_vim_enabled(true);
    }
    let mut nav = NavView::new();
    let mut transcript = Transcript::new();
    transcript.set_theme(&theme);
    // S:I3/S:I6 — typed timeline + its identity-strip projection, seeded
    // with the session this host starts on.
    let initial_session = host
        .current_session()
        .unwrap_or_else(|| LOCAL_SESSION_ID.into());
    let mut session = SessionState::new(&initial_session);
    session.theme = theme;
    let fullscreen_enabled = tui_settings.fullscreen;
    session.proj.settings = tui_settings;
    let interjections = InterjectionBuffer::new();
    // S:I7 — resolve the render mode once (CLI flags via the host, the rest
    // from the environment) and hand it to the composer, which owns the
    // per-component consequences of it.
    let layout = resolve_layout_mode(&host);
    composer.set_layout_mode(layout);
    // Seed the composer's geometry inputs (height cap, wrap width) from the
    // real terminal before anything asks it for a desired height — its own
    // Resize handler is the single place those inputs are set.
    let _ = composer.handle(&TuiEvent::Resize {
        width: cols,
        height: rows,
    });
    // Launch-at-cursor (Codex `tui.rs` init, donor commit
    // 3074be908ac7f270b1b4781f2f8c55662067b96a, Apache-2.0): read the cursor
    // BEFORE `spawn_input_thread` below starts -- a running input reader
    // would race the CPR reply. On Windows this is a console-API read that
    // never touches stdin; on Unix it is crossterm's own poll-bounded
    // query, so it cannot hang indefinitely. A failure or timeout falls
    // back to row 0 -- the donor's "defaulting to origin" choice: the
    // viewport then simply owns the screen top rather than guessing a row.
    let launch_row = match ct_cursor::position() {
        Ok((_, row)) => row,
        Err(error) => {
            tracing::warn!("initial cursor position probe failed, anchoring at origin: {error}");
            0
        }
    };
    // Bottom-anchor the first frame: the input frame owns the last rows of
    // the screen from the start, so it does not hop down on first input.
    let initial_height =
        inline_viewport_height(&composer, &session, &mut transcript, &theme, cols, rows);
    let launch_row = bottom_anchored_launch_row(launch_row, rows, initial_height);
    let mut surface = Surface::new(cols, rows, launch_row, initial_height, profile.insert_mode);
    if fullscreen_enabled {
        surface.enter_modal(ModalKind::Fullscreen);
    }

    let (draw_tx, mut draw_rx) = broadcast::channel::<()>(DRAW_QUEUE_CAPACITY);
    let requester: FrameRequester = rightkit_terminal_ui::scheduler::spawn(draw_tx);
    let _metrics_export = render_metrics_export::start(requester.metrics());

    let backend = CrosstermBackend::new(io::stdout());
    let mut term = match Terminal::with_options_and_cursor_position(backend, (0, launch_row).into())
    {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[tui] failed to start renderer: {e}");
            return 1;
        }
    };

    term.set_viewport_area(inline_rect(&surface));

    let input_pause = InputPause::new();
    host.attach_input_pause(input_pause.clone());
    let _ = EDITOR_INPUT_PAUSE.set(input_pause.clone());
    let mut input_rx = spawn_input_thread(input_pause.clone());
    let mut turn: Option<JoinHandle<i32>> = None;
    // Drained interjection directives awaiting a free turn slot. Kept outside
    // `interjections` because they are already framed; they run one at a
    // time, each spawned only after the previous turn settles.
    let mut pending_followups: std::collections::VecDeque<Directive> =
        std::collections::VecDeque::new();
    // Submissions echo into the transcript immediately so the user sees
    // their message the instant they submit. The same text then arrives a
    // second time from the durable event stream as `userMessageSubmitted`;
    // this FIFO suppresses exactly those echoes so the prompt renders once.
    let mut pending_echoes: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    let mut exit_code: i32 = 0;
    let mut should_exit = false;

    // P1-5: which agent's transcript is currently viewed (Descend), so the
    // maintenance tick can keep it live. Cleared on Ascend and session
    // switch (the view itself is reset via `Transcript::clear`).
    let mut viewed_agent: Option<AgentId> = None;
    // P2-5: scrollback paging state — the durable-history cursor (smallest
    // seq still needed), whether the ledger has been exhausted, and how far
    // eviction had advanced when we last paged. Owned as one struct because
    // all three describe the currently viewed session's ledger and must be
    // rebound together when the view moves to a different session.
    let mut paging = HistoryPaging::default();
    // Maintenance tick: keeps a descended agent transcript live (P1-5) and
    // pages evicted history into terminal scrollback (P2-5).
    let mut maintenance = tokio::time::interval(Duration::from_millis(750));
    maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut shell_tick = tokio::time::interval(Duration::from_millis(100));
    shell_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // What the renderer last painted — geometry reconciliation state for the
    // whole session, so transitions erase/reclaim exactly the released rows.
    let mut render_state = RenderState::default();

    // S:I6 — seed attribution before the first frame: refresh the feed once
    // so the identity strip names a real project/model/actor instead of
    // waiting for the user to open a picker.
    host.refresh_nav(&mut nav, false).await;
    let running_session = host
        .current_session()
        .unwrap_or_else(|| session.active_session.clone());
    session
        .subagents
        .set_running(host.running_work(&running_session).await);
    if let Some(root) = nav.viewed_agent() {
        session.subagents.set_tree(root);
    }
    if session.proj.settings.agents_view_default && !session.subagents.is_empty() {
        session.subagents_focused = true;
        session.subagents.handle(&TuiEvent::Focus(true));
    }
    let _ = host.goal_op("get", None).await;
    let mut pending_trust_root: Option<String> = None;
    if let Some(root) = host.active_directory() {
        if matches!(
            host.workspace_trust(&root).await,
            Ok(rightkit_agent_policy::WorkspaceTrust::Untrusted)
        ) {
            // Deferred until the launch welcome card is in scrollback: a
            // card raised before the welcome slide displaces the
            // composer's top rule.
            pending_trust_root = Some(root.clone());
        }
    }
    sync_route(&nav, &mut session, &mut transcript);
    // CG-A01/S:I10 — seed both shared command grammar & local truthful
    // effects; rebuild whenever turn state changes.
    refresh_completion_catalog(&mut composer, &session, false);
    // No built-in model: a host that starts without one asks for the picker
    // at launch instead of waiting for a keystroke.
    if let Some(request) = host.take_picker_request() {
        service_picker_request(
            &host,
            request,
            &mut composer,
            &mut nav,
            &mut session,
            &mut transcript,
            &mut surface,
            false,
            &mut viewed_agent,
            &mut paging,
            &requester,
        )
        .await;
    }

    requester.schedule_immediate();

    // KRN defect #4: truthful readiness. `None` means this host has no such
    // background lifecycle (e.g. an already-connected ServiceHost) -- the
    // arm below then never fires again once the channel is confirmed absent.
    let mut readiness_rx = host.readiness();
    let mut last_readiness: Option<HostReadiness> = None;
    let mut degraded_notified = false;
    // A host with a readiness lifecycle starts "not ready" in the footer, so
    // the ready hints never appear before the host actually reports Ready.
    if let Some(rx) = readiness_rx.as_ref() {
        session.readiness = Some(rx.borrow().clone());
    }

    let mut paste_tick = tokio::time::interval(Composer::recommended_paste_flush_delay());
    paste_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // One redraw a second while the host is still starting, so the footer's
    // startup clock advances without any other input.
    let mut startup_tick = tokio::time::interval(Duration::from_secs(1));
    startup_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // A070: poll the attached PTY's output so a quiet child still shows output.
    let mut pty_tick = tokio::time::interval(Duration::from_millis(150));
    pty_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Borrowed futures are polled beside input; discovery never owns input latency.
    // The maintenance poll reads host state (InlineHost may rebuild its
    // composed state, which can take seconds on a cold or failing context
    // worker). It is never awaited inside an arm body: that froze the whole
    // loop -- input, timers, redraws -- for the poll's duration and made
    // typed keys arrive in bunches that read as a paste burst. It lives here
    // and is polled as its own arm instead.
    let mut timeline_poll: Option<
        std::pin::Pin<Box<dyn std::future::Future<Output = Vec<DaemonEvent>> + '_>>,
    > = None;
    let mut file_search: Option<
        std::pin::Pin<
            Box<
                dyn std::future::Future<Output = (String, Vec<rightkit_terminal_ui::composer::FileMatch>)>
                    + '_,
            >,
        >,
    > = None;
    while !should_exit {
        // Lane 4: window title + unfocused completion/approval notification.
        let _ = notifier.observe(
            &mut io::stdout(),
            turn.is_some(),
            session.approval_overlay.current_request().is_some(),
        );
        tokio::select! {
            biased;

            readiness_changed = async {
                match readiness_rx.as_mut() {
                    Some(rx) => rx.changed().await,
                    None => std::future::pending().await,
                }
            }, if readiness_rx.is_some() => {
                match readiness_changed {
                    Ok(()) => {
                        let state = readiness_rx.as_ref().map(|rx| rx.borrow().clone());
                        if let Some(state) = state {
                            if last_readiness.as_ref() != Some(&state) {
                                // Progress lives in the footer; only a failure
                                // is transcript-worthy.
                                if let HostReadiness::Degraded { reason } = &state {
                                    if !degraded_notified {
                                        degraded_notified = true;
                                        session.notice(
                                            &mut transcript,
                                            format!(
                                                "Context is off ({reason}); this chat runs without memory or repository context."
                                            ),
                                        );
                                        flush_history(&mut transcript, &mut surface, &requester);
                                    }
                                }
                                if let HostReadiness::Failed { message } = &state {
                                    session.notice(
                                        &mut transcript,
                                        format!(
                                            "Context start failed: {message} Submitting again will retry."
                                        ),
                                    );
                                    flush_history(&mut transcript, &mut surface, &requester);
                                }
                                session.readiness = Some(state.clone());
                                if matches!(state, HostReadiness::Ready | HostReadiness::Degraded { .. }) {
                                    // Cold startup skipped read-only goal projection.
                                    // Durable session state remains available without context.
                                    let _ = host.goal_op("get", None).await;
                                }
                                // The home screen opened before the context
                                // had a session list; fill it in once ready.
                                if matches!(state, HostReadiness::Ready | HostReadiness::Degraded { .. })
                                    && session.proj.home_refresh
                                    && session.proj.picker.as_ref().is_some_and(|p| {
                                        p.kind == crate::tui_projects::PickKind::Home
                                            && p.picker.query().is_empty()
                                    })
                                {
                                    open_project_picker(
                                        &host, &mut session, crate::tui_projects::PickKind::Home,
                                    )
                                    .await;
                                }
                                last_readiness = Some(state);
                            }
                        }
                        requester.schedule_frame();
                    }
                    Err(_) => {
                        // Sender dropped: this host will never report again.
                        readiness_rx = None;
                    }
                }
            }

            _ = startup_tick.tick(), if matches!(
                session.readiness,
                Some(HostReadiness::Configuring | HostReadiness::StartingContext)
            ) => {
                requester.schedule_frame();
            }

            _ = pty_tick.tick(), if session.pty.is_some() => {
                pty_poll(&host, &mut session, &mut transcript).await;
                pty_show_partial(&mut composer, &session);
                requester.schedule_frame();
            }

            _ = paste_tick.tick(), if composer.is_in_paste_burst() => {
                if flush_paste_burst_if_input_drained(&mut composer, &input_rx) {
                    requester.schedule_frame();
                }
                // A flushed burst can move the `@` token too; drain any
                // file-search requests it queued before sleeping again.
            }

            maybe_event = input_rx.recv() => {
                match maybe_event {
                    Some(Ok(raw)) => {
                        let input_received_at = Instant::now();
                        if host.input_paused() {
                            // P1-10: events racing the pause window (queued
                            // between the reader's last cycle and its
                            // acknowledgement) are stale from the composer's
                            // point of view — the pausing surface's own
                            // prompts consumed the live keystrokes. Drop them.
                        } else if let Some(event) = translate(raw) {
                            if let TuiEvent::Focus(focused) = &event {
                                notifier.set_focus(*focused);
                            }
                            composer.set_defer_paste_burst_idle_flush(true);
                            handle_event(
                                &host,
                                &event,
                                &mut composer,
                                &mut nav,
                                &mut session,
                                &mut transcript,
                                &mut surface,
                                &interjections,
                                &mut pending_echoes,
                                &mut turn,
                                &requester,
                                &mut exit_code,
                                &mut should_exit,
                                &mut viewed_agent,
                                &mut paging,
                            )
                            .await;
                            if session.proj.settings_dirty {
                                session.proj.settings = host.tui_settings();
                                theme = Theme::new(
                                    effective_background(&session.proj.settings.theme, profile.background),
                                    profile.capabilities.color_depth,
                                );
                                session.theme = theme.clone();
                                transcript.set_theme(&theme);
                                composer.set_vim_enabled(session.proj.settings.vim);
                                notifier = build_notifier(
                                    &session.proj.settings.notify,
                                    &std::env::current_dir().unwrap_or_default(),
                                );
                                session.proj.settings_dirty = false;
                            }
                            composer.set_defer_paste_burst_idle_flush(false);
                        }
                        requester.record_input_latency(input_received_at.elapsed());
                    }
                    // A poll/read error or a closed channel both mean input
                    // is gone (e.g. stdin closed under a supervisor) --
                    // exit cleanly rather than spin.
                    Some(Err(_)) | None => should_exit = true,
                }
                // A command surface may have asked for a route picker
                // (e.g. `/model`, `/effort` executed above); consume the
                // one-shot request and open over the real catalog.
                if let Some(request) = host.take_picker_request() {
                    service_picker_request(&host, request, &mut composer, &mut nav, &mut session, &mut transcript, &mut surface, turn.is_some(), &mut viewed_agent, &mut paging, &requester).await;
                }
                // A new approval request enqueued while handling this event
                // (rare — requests normally arrive via the polled feed, but
                // a local effect can raise one): quarantine whatever input
                // is still buffered so nothing typed against the pre-modal
                // screen can reach the armed overlay.
                if session.take_input_quarantine() {
                    quarantine_approval_input(&input_pause, &mut input_rx, &mut session, &mut transcript);
                }
                // A blocking command (e.g. /onboard) paused input and ran its
                // own prompts while THIS loop was blocked, so the drain check
                // above could not run during it. The host records that a drain
                // is owed; when the command returns, drop every stale event
                // queued before/around the ACKNOWLEDGED pause window so the
                // composer never replays keystrokes the user typed at a prompt
                // that has moved on.
                if host.take_input_drain() {
                    while input_rx.try_recv().is_ok() {}
                }
            }

            _ = draw_rx.recv() => {
                render(
                    &host,
                    &mut term,
                    &mut surface,
                    &mut composer,
                    &mut nav,
                    &mut transcript,
                    &theme,
                    &mut session,
                    layout,
                    &requester,
                    &mut render_state,
                );
                if render_state.welcome_shown {
                    if let Some(root) = pending_trust_root.take() {
                        session.pending_trust = Some(root);
                        requester.schedule_immediate();
                    }
                }
            }

            result = wait_turn(&mut turn), if turn.is_some() => {
                turn = None;
                composer.set_streaming(false);
                session.working_started = None;
                session.working_signal = None;
                exit_code = result;
                session.clear_interaction();

                // Drain the events the turn produced between the last
                // maintenance tick and settle, so the tail (final deltas,
                // tool results, approvals) lands immediately instead of
                // waiting for the next poll — which idle gating may never
                // run once no producer remains.
                // Settle any in-flight maintenance poll first so the cursor
                // advances once and queued host events are not dropped.
                let mut tail_events = match timeline_poll.take() {
                    Some(poll) => poll.await,
                    None => Vec::new(),
                };
                tail_events.extend(host.poll_timeline_events().await);
                refresh_deny_rules(&host, &mut session, &tail_events).await;
                if !tail_events.is_empty() {
                    for event in tail_events {
                        record_polled_event(
                            &mut session,
                            &mut transcript,
                            &mut pending_echoes,
                            event,
                        );
                    }
                    flush_history(&mut transcript, &mut surface, &requester);
                    session.clear_interaction();
                    // Stage-3 modal quarantine: a request folded in above
                    // armed the overlay; keystrokes buffered before its
                    // first draw must be dropped, never replayed into it.
                    if session.take_input_quarantine() {
                        quarantine_approval_input(&input_pause, &mut input_rx, &mut session, &mut transcript);
                    }
                }
                // The turn is over: re-read the agent tree so subagents it spawned (and their
                // final state) show in the list under the composer.
                host.refresh_nav(&mut nav, false).await;
                // The turn is over: nothing in it is still moving, so a dangling
                // call or already-answered card must not keep `/new` disabled.
                session.timeline.settle_open_entries();
                session.settle_reasoning(&mut transcript);
                // Every terminal state is visible: a turn that ended with no
                // answer, tool, notice or failure must not look like success.
                if !session.turn_outcome_seen
                    && session.failed_hold.is_none()
                    && session.pending_question.is_none()
                    && session.pending_plan.is_none()
                {
                    session.notice(
                        &mut transcript,
                        if exit_code == 0 {
                            "Turn ended with no response from the model. Try again or switch model."
                        } else {
                            "Turn ended without a response and without an error report."
                        },
                    );
                    flush_history(&mut transcript, &mut surface, &requester);
                }
                session.turn_outcome_seen = true;
                if let Some(request) = host.take_picker_request() {
                    service_picker_request(&host, request, &mut composer, &mut nav, &mut session, &mut transcript, &mut surface, turn.is_some(), &mut viewed_agent, &mut paging, &requester).await;
                }

                // Each drained entry is a `Directive`, not bare text: the kind
                // the user meant survives all the way here, so a cancel can
                // never be run as a steer. Order is FIFO, and a directive that
                // stops the turn discards everything queued behind it -- once
                // the user has said "stop", the messages they typed before
                // that are no longer requests they want carried out.
                //
                // P1-7: with immediate-interrupt semantics this arm normally
                // sees Steers only (Cancel/Reprompt act at enqueue time), but
                // every kind stays handled here so a directive queued in a
                // race window can never be silently dropped.
                //
                // A follow-up is spawned as the next in-flight turn and NOT
                // awaited inside this branch: awaiting its JoinHandle here
                // would hold the whole select! loop for the turn's duration,
                // freezing input, rendering, and cancellation until the
                // follow-up finished. Remaining directives stay queued and
                // are serviced when that turn ends.
                if pending_followups.is_empty() {
                    if let Some(handle) = send_queued(
                        &host, &interjections, &mut session, &mut transcript,
                        &mut pending_echoes, &mut composer,
                    ) {
                        turn = Some(handle);
                    }
                }
                while let Some(directive) = pending_followups.pop_front() {
                    match directive {
                        Directive::Steer(framed) | Directive::Reprompt(framed) => {
                            // The raw user text was already echoed at enqueue.
                            pending_echoes.push_back(framed.clone());
                            composer.set_streaming(true);
                            session.working_started = Some(Instant::now());
                            session.working_signal = None;
                            session.begin_turn_status();
                            turn = Some(host.spawn_turn(framed));
                            break;
                        }
                        // A cancelled turn has no reader left to deliver to;
                        // the text was recorded in the transcript when it was
                        // typed. Nothing is sent to the model.
                        Directive::Cancel => {
                            session.notice(&mut transcript, "Turn cancelled.");
                            pending_followups.clear();
                            break;
                        }
                    }
                }
                // S:I10 — nothing is in flight now (unless a steer just
                // spawned a follow-up), so the popup's disabled reasons are
                // rebuilt from the registry against the new turn state.
                composer.set_queued_count(interjections.len());
                session.queued_preview = interjections.texts();
                refresh_completion_catalog(&mut composer, &session, turn.is_some());
                requester.schedule_frame();
            }

            // The maintenance tick is enabled only while a producer or
            // pending work exists: an in-flight turn, an open agent view,
            // unpaged evictions, or an already-connected service feed.
            // Otherwise the loop sleeps on real events instead of polling a
            // feed that cannot produce (or autostarting a daemon to watch).
            (query, matches) = async {
                match file_search.as_mut() {
                    Some(search) => search.as_mut().await,
                    None => std::future::pending().await,
                }
            }, if file_search.is_some() => {
                file_search = None;
                composer.on_file_search_result(query, matches);
                requester.schedule_frame();
            }

            timeline_events = async {
                match timeline_poll.as_mut() {
                    Some(poll) => poll.as_mut().await,
                    None => std::future::pending().await,
                }
            }, if timeline_poll.is_some() => {
                timeline_poll = None;
                refresh_deny_rules(&host, &mut session, &timeline_events).await;
                if !timeline_events.is_empty() {
                    // Subagent spawn/finish changes the agent tree behind the list under the
                    // composer; re-read it when the batch carries such an event.
                    let roster_changed = timeline_events.iter().any(is_roster_event);
                    for event in timeline_events {
                        record_polled_event(
                            &mut session,
                            &mut transcript,
                            &mut pending_echoes,
                            event,
                        );
                    }
                    flush_history(&mut transcript, &mut surface, &requester);
                    if session.take_input_quarantine() {
                        quarantine_approval_input(&input_pause, &mut input_rx, &mut session, &mut transcript);
                    }
                    if roster_changed {
                        host.refresh_nav(&mut nav, turn.is_some()).await;
                    }
                    requester.schedule_frame();
                }
            }

            // `!cmd` stream/settle tick; enabled only while one is running.
            _ = shell_tick.tick(), if rightkit_terminal_ui::user_shell::is_active() => {
                drain_user_shell(&mut composer, &mut session, &mut transcript, &mut surface, &requester);
            }

            _ = maintenance.tick(), if turn.is_some()
                || !session.subagents.is_empty()
                || viewed_agent.is_some()
                || transcript.evicted_count() > paging.paged_through_evictions
                || host.external_feed_available() => {
                let running_session = host.current_session().unwrap_or_else(|| session.active_session.clone());
                session.subagents.set_running(host.running_work(&running_session).await);
                // S:I3: fold raw active-session daemon events through the
                // typed reducer. Agent transcript viewing remains a distinct,
                // display-only projection below; it must not erase tool,
                // approval, route, task, or check event kinds.
                if timeline_poll.is_none() {
                    timeline_poll = Some(Box::pin(host.poll_timeline_events()));
                }
                if let Some(request) = host.take_picker_request() {
                    service_picker_request(&host, request, &mut composer, &mut nav, &mut session, &mut transcript, &mut surface, turn.is_some(), &mut viewed_agent, &mut paging, &requester).await;
                }
                // P1-5: while an agent view is open, poll for NEW events after
                // the host's per-agent cursor and append them. Reading history
                // never pauses or owns the viewed worker.
                if let Some(id) = viewed_agent.clone() {
                    if let Some(lines) = host.poll_agent_transcript(&id).await {
                        if !lines.is_empty() {
                            // S:I3 — the viewed agent's new output lands in
                            // the typed timeline exactly as it lands in the
                            // transcript: one open assistant entry, streamed
                            // into and then settled.
                            session.record(
                                &mut transcript,
                                DaemonEvent::AssistantDelta {
                                    token: lines.join("\n"),
                                },
                            );
                            session.record(&mut transcript, DaemonEvent::AssistantSettled);
                            flush_history(&mut transcript, &mut surface, &requester);
                        }
                        requester.schedule_frame();
                    }
                }
                // P2-5: evicted content must stay visible in terminal
                // scrollback while RSS stays bounded. First salvage anything
                // evicted before it was ever shown, then page older durable
                // history from the daemon's SQLite ledger through the shared
                // scrollback helper whenever the transcript reports evictions.
                let (width, _) = surface.screen_size();
                // Keep resident overflow until inline scrollback is writable;
                // alternate-screen/modal states must not consume unseen rows.
                if surface.can_insert_history() {
                    let salvaged = transcript.drain_eviction_overflow_lines(width);
                    insert_scrollback_lines(&mut surface, &salvaged);
                }
                if !paging.exhausted {
                    if let Some((_, limit)) = transcript.paging_window() {
                        if transcript.evicted_count() > paging.paged_through_evictions {
                            let before = paging.cursor.unwrap_or(i64::MAX);
                            if !surface.can_insert_history() {
                                continue;
                            }
                            match host.fetch_older_history_page(before, limit).await {
                                Ok(page) if page.is_empty() => {
                                    paging.exhausted = true;
                                    paging.paged_through_evictions =
                                        transcript.evicted_count();
                                }
                                Ok(page) => {
                                    let lines: Vec<HyperlinkLine> =
                                        page.iter().filter_map(|entry| entry.line.clone().map(Into::into)).collect();
                                    insert_scrollback_lines(&mut surface, &lines);
                                    paging.cursor = next_paging_cursor(&page);
                                    paging.paged_through_evictions =
                                        transcript.evicted_count();
                                }
                                Err(_) => {
                                    // No feed / unreachable daemon: stop
                                    // retrying on every tick rather than
                                    // spinning an HTTP round trip forever.
                                    paging.exhausted = true;
                                }
                            }
                            requester.schedule_frame();
                        }
                    }
                }
                // A026: the full-screen transcript reader asks for an older
                // durable page when scrolled to its top; prepend what arrives.
                if let Some(inspection) = session.inspection.as_mut() {
                    if let Some(request) = inspection.take_older_request() {
                        match host
                            .fetch_older_history_page(request.before_seq, request.limit)
                            .await
                        {
                            Ok(page) => {
                                let lowest = next_paging_cursor(&page);
                                let lines = page
                                    .iter()
                                    .filter_map(|entry| entry.line.clone())
                                    .collect();
                                inspection.older_page_loaded(lines, lowest);
                            }
                            Err(_) => inspection.older_page_failed(),
                        }
                        requester.schedule_frame();
                    }
                }
                // S:I7 — only an animating mode repaints on a timer while a
                // turn is in flight. Plain/ReducedMotion repaint on real
                // change (a keystroke, a turn ending) and never for motion's
                // sake; the scheduler coalesces whatever is requested.
                if turn.is_some() {
                    requester.schedule_frame();
                }
            }
        }
        if let Some(query) = composer.drain_file_search_requests().into_iter().last() {
            file_search = None; // Drop stale query before admitting its replacement.
            if !query.is_empty() {
                let source = &host;
                file_search = Some(Box::pin(async move {
                    let matches = source.file_search(&query).await;
                    (query, matches)
                }));
            }
        }
    }
    drop(file_search);
    drop(timeline_poll);
    let _ = notifier.clear_title(&mut io::stdout());

    if let Err(error) = cancel_active_turn(&host, &mut turn).await {
        eprintln!("{error}");
        return 1;
    }
    exit_code
}

/// Keep a delayed paste burst intact while the input reader still has events
/// queued. Rendering maintenance can lag behind a ConPTY burst; flushing at
/// that boundary would classify one paste as several ordinary fragments.
fn flush_paste_burst_if_input_drained(
    composer: &mut Composer,
    input_rx: &mpsc::Receiver<io::Result<ct_event::Event>>,
) -> bool {
    if !input_rx.is_empty() {
        return false;
    }
    composer.flush_paste_burst_if_due()
}

/// Await the in-flight turn if there is one; otherwise never resolves, so it
/// is safe as a `select!` branch gated on `turn.is_some()`.
async fn wait_turn(turn: &mut Option<JoinHandle<i32>>) -> i32 {
    match turn {
        Some(handle) => handle.await.unwrap_or(1),
        None => std::future::pending().await,
    }
}

async fn cancel_active_turn(
    host: &impl TurnHost,
    turn: &mut Option<JoinHandle<i32>>,
) -> Result<(), String> {
    if turn.is_some() {
        host.cancel_turn().await?;
        if let Some(handle) = turn.take() {
            handle.abort();
            let _ = handle.await;
        }
    }
    Ok(())
}

/// The fixed area ratatui owns: the surface's own inline viewport when
/// showing the composer, or the whole screen while a modal has taken the
/// alternate buffer. Recomputed every render rather than cached, since the
/// surface is the single source of truth for this geometry.
fn inline_rect(surface: &Surface) -> Rect {
    let (width, _) = surface.screen_size();
    let viewport = surface.viewport();
    Rect {
        x: 0,
        y: viewport.top,
        width,
        height: viewport.height.max(1),
    }
}

/// The total inline row budget: how tall the live region needs to be to show
/// the prompt affordance, the status chrome, and the live transcript tail.
///
/// This is the fix for the "bare startup" defect: the old code sized the
/// surface to `composer.desired_height`, which returned the composer's
/// maximum unconditionally, so the whole viewport was composer and nothing
/// above it could ever render. Here the composer's content-aware height is
/// one term alongside real chrome and live-output rows.
fn inline_viewport_height(
    composer: &Composer,
    session: &SessionState,
    transcript: &mut Transcript,
    theme: &Theme,
    width: u16,
    screen_height: u16,
) -> u16 {
    let composer_rows = composer.desired_height(width, screen_height);
    if session.model_picker.is_some()
        || session.providers_picker.is_some()
        || session.repository_picker.is_some()
        || session.rewind.is_some()
        || session.proj.picker.is_some()
        || session.approval_overlay.current_request().is_some()
        || session.pending_question.is_some()
        || session.pending_plan.is_some()
        || session.pending_trust.is_some()
    {
        return screen_height
            .saturating_sub(1)
            .max(composer_rows)
            .min(screen_height);
    }
    // Input frame: a rule above and below the composer plus the footer row,
    // the working line, and the live subagent list (D16) when it has agents.
    let chrome = COMPOSER_CHROME_ROWS
        + u16::from(session.working_started.is_some() || session.failed_hold.is_some())
        + session.subagents.rows();
    // The live tail — typed cells plus transcript — is bounded to a third
    // of the screen so streaming output can never crowd the composer off
    // the bottom.
    let tail_cap = (screen_height / 3).clamp(4, 16);
    let cells = session.live_cell_lines(width, theme).len() as u16
        + session.queue_list_lines(width, theme).len() as u16;
    let tail = transcript
        .live_tail_height(width, tail_cap.saturating_sub(cells))
        .saturating_add(cells)
        .min(tail_cap);
    composer_rows + chrome + tail
}

/// What the renderer last put on the screen, so a geometry transition can
/// reconcile old and new footprints exactly once instead of resizing the
/// terminal every frame and silently dropping the failure modes that matter.
#[derive(Default)]
struct RenderState {
    /// The viewport rect ratatui last drew into (screen coordinates).
    rect: Option<Rect>,
    /// The screen mode the last draw happened in.
    mode: Option<ScreenMode>,
    /// The terminal size the last draw was against — distinguishes a pure
    /// viewport move (reclaim/erase released rows) from a terminal resize
    /// (the terminal itself already reflowed; only stale-band cleanup is ours).
    screen: Option<(u16, u16)>,
    /// The last reported resize/draw failure; dedupes the notice so a
    /// persistently failing terminal produces one notice, not one per frame.
    last_error: Option<String>,
    /// The launch welcome box has been written to scrollback.
    welcome_shown: bool,
    /// Text of every row of the frame last drawn. A width-changing resize
    /// makes the terminal wrap those rows, pushing the old viewport's top up
    /// by the wrapped rows above the cursor; the stale-paint clear must start
    /// at that real top or the live tail is left painted twice.
    drawn_rows: Vec<String>,
    /// Plain text of the newest rows inserted into scrollback. A row that
    /// matches both the old frame and one of these is real history (the live
    /// tail it once was got flushed), never stale paint to erase.
    recent_history: std::collections::VecDeque<String>,
}

/// Rows a width-shrinking reflow adds above the cursor: every row of the
/// previous frame above the cursor's row that is wider than `new_cols` wraps
/// onto `ceil(width / new_cols) - 1` more rows.
fn reflow_rows_above(drawn_rows: &[String], cursor_row: usize, new_cols: u16) -> u16 {
    use rightkit_terminal_ui::transcript::wrap::display_width;
    if new_cols == 0 {
        return 0;
    }
    drawn_rows
        .iter()
        .take(cursor_row)
        .map(|row| {
            let width = u16::try_from(display_width(row)).unwrap_or(u16::MAX);
            width.saturating_sub(1) / new_cols
        })
        .fold(0u16, u16::saturating_add)
}

/// Blank rows sitting between the transcript above and the viewport at `top`,
/// beyond the one spacer row a message leaves. A resize that moves the viewport
/// but not the history (the terminal keeps content top-anchored) opens this
/// gap; the caller slides the history down over it. Zero when nothing above is
/// blank-separated, when no transcript is above at all (a short session whose
/// top is empty), or when a row cannot be read.
fn blank_gap_above(top: u16, read_row: impl Fn(u16) -> Option<String>) -> u16 {
    let mut blank_run = 0u16;
    for row in (0..top).rev() {
        match read_row(row) {
            Some(text) if text.trim().is_empty() => blank_run += 1,
            Some(_) => return blank_run.saturating_sub(1),
            None => return 0,
        }
    }
    0
}

/// Rows the cursor-displacement estimate can miss: how a console wraps a row
/// that was written with trailing spaces is not knowable from the drawn
/// text, so the estimate can land below the real old top and leave the first
/// rows of the old viewport painted. Walk up from `estimate`, and extend the
/// top over every row that still shows a piece of the old frame (each old row
/// cut into `new_cols`-wide segments, which is how a reflow lays it out),
/// skipping blank rows, stopping at the first row that is something else.
fn stale_top_above(
    estimate: u16,
    old_rows: &[String],
    new_cols: u16,
    history: &std::collections::VecDeque<String>,
    read_row: impl Fn(u16) -> Option<String>,
) -> u16 {
    use rightkit_terminal_ui::transcript::wrap::display_width;
    const MAX_ROWS: u16 = 16;
    if new_cols == 0 {
        return estimate;
    }
    let mut segments: std::collections::HashSet<String> = std::collections::HashSet::new();
    for row in old_rows {
        let mut segment = String::new();
        let mut used = 0usize;
        for ch in row.chars() {
            let w = display_width(&ch.to_string());
            if used + w > usize::from(new_cols) {
                let piece = segment.trim().to_string();
                if !piece.is_empty() {
                    segments.insert(piece);
                }
                segment.clear();
                used = 0;
            }
            segment.push(ch);
            used += w;
        }
        let piece = segment.trim().to_string();
        if !piece.is_empty() {
            segments.insert(piece);
        }
    }
    let mut top = estimate;
    let mut row = estimate;
    for _ in 0..MAX_ROWS {
        if row == 0 {
            break;
        }
        row -= 1;
        let Some(text) = read_row(row) else {
            break;
        };
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        if history.iter().any(|h| h == text) {
            break;
        }
        if segments.contains(text) {
            top = row;
        } else {
            break;
        }
    }
    top
}

/// Blank the rows the previous footprint released but the new one does not
/// cover. Those rows still show the previous frame's painted cells; leaving
/// them is the "old prompt stays visible at the old position while a new one
/// draws at the new position" artifact. Row erases only: real scrollback
/// above the footprint belongs to the terminal and is never cleared by us.
///
/// Released rows exist on BOTH sides now that the viewport is not
/// bottom-anchored: moving up (growth overflow, screen shrink) releases a
/// band below the new footprint, and a history slide moving down releases a
/// band above it.
///
/// `history_slide` is how many rows a scroll-region history insertion
/// physically pushed the screen band down. The slide moves the old
/// footprint's paint with everything at and below `old.y`, so the stale
/// band's screen coordinates are `[old.y + slide, old.bottom() + slide)` --
/// not `old` itself. The `slide` rows at `old.y..old.y + slide` hold the
/// inserted history tail and are spared by starting the stale band there;
/// the slide-shifted rows at `old.bottom()..old.bottom() + slide` carry the
/// released tail (the streamed cell and composer prompt the settled frame
/// shrank off) and must be erased too.
fn erase_released_rows(
    writer: &mut impl Write,
    old: Rect,
    new: Rect,
    history_slide: u16,
) -> io::Result<()> {
    // Screen coordinates of the stale paint band after the slide moved it.
    // With no pending slide this is `old` unchanged.
    let stale_top = old.y.saturating_add(history_slide);
    let stale_bottom = old.bottom().saturating_add(history_slide);
    // Rows above the new top the (slid) footprint painted.
    for row in stale_top..new.y.min(stale_bottom) {
        write!(writer, "\x1b[{};1H\x1b[K", row + 1)?;
    }
    // Rows below the new bottom the (slid) footprint painted (released when
    // the viewport moved up or shrank upward off them).
    for row in new.bottom().max(stale_top)..stale_bottom {
        write!(writer, "\x1b[{};1H\x1b[K", row + 1)?;
    }
    Ok(())
}

/// Surface a resize/draw failure in the transcript exactly once per distinct
/// message, then keep trying — a transient PTY stall must not kill the UI,
/// and a persistent one must not spin silently.
fn note_render_fault(
    state: &mut RenderState,
    session: &mut SessionState,
    transcript: &mut Transcript,
    msg: String,
) {
    if state.last_error.as_deref() != Some(msg.as_str()) {
        session.notice(transcript, format!("render: {msg}"));
        state.last_error = Some(msg);
    }
}

/// Feed [`NavView`] from the session/agent state this host actually has.
///
/// There is no daemon feed yet, so the honest model is exactly one root
/// session (this one) and one agent (the local turn's root). Feeding it is
/// what makes `/sessions` and `/agents` render real content instead of an
/// empty list. Called whenever a picker is about to open, so the running/
/// done state always reflects whether a turn is in flight right now.
fn refresh_nav(nav: &mut NavView, turn_in_flight: bool, model: &str) {
    let session = SessionSummary {
        id: SessionId(LOCAL_SESSION_ID.to_string()),
        name: "this session".to_string(),
    };
    nav.set_sessions(vec![session.clone()], Some(session));
    nav.set_tree(AgentNode {
        id: AgentId(LOCAL_AGENT_ID.to_string()),
        name: "root agent".to_string(),
        model: model.to_string(),
        elapsed_cost: String::new(),
        state: if turn_in_flight {
            AgentState::Running
        } else {
            AgentState::Done
        },
        children: Vec::new(),
    });
}

/// Public local-stub feed, so a daemon-backed host can fall back to it when
/// the daemon is unreachable (e.g. `EngineTurnHost::refresh_nav`). `model`
/// is the caller's truthful label — the resolved model when known, else the
/// configured provider — never a fabricated value.
pub fn feed_local_stub(nav: &mut NavView, turn_in_flight: bool, model: &str) {
    refresh_nav(nav, turn_in_flight, model);
}

/// S:I10 — local effect rows for the completion catalog, with their distinct
/// descriptions & disabled reasons. Shared grammar rows join these below.
fn command_effect_catalog(busy: bool) -> Vec<CompletionCandidate> {
    CommandEffect::ALL
        .iter()
        .map(|effect| {
            let candidate = CompletionCandidate::new(effect.name(), effect.description())
                .with_aliases(effect.aliases().iter().copied());
            match effect {
                // Refused outright by `apply_command_effect`: the durable
                // turn ledger is readable from this host, not truncatable.
                CommandEffect::Rewind => {
                    candidate.disabled("the session ledger is read-only from this surface")
                }
                // A turn in flight owns the session handle; `apply_command_effect`
                // refuses `New` while one is running.
                CommandEffect::New if busy => candidate.disabled("a turn is in flight"),
                _ => candidate,
            }
        })
        .collect()
}

/// CG-A01 — project interactive entries from shared grammar into popup rows.
///
/// Command effects retain ownership of their distinct session semantics, so
/// their names do not get a second row from the older `/clear` grammar entry.
fn shared_command_catalog(busy: bool) -> Vec<CompletionCandidate> {
    crate::cli::command_registry::entries_for_surface(false)
        .filter(|entry| CommandEffect::resolve(entry.name.trim_start_matches('/')).is_none())
        .map(|entry| {
            let candidate =
                CompletionCandidate::new(entry.name.trim_start_matches('/'), entry.summary)
                    .with_aliases(
                        entry
                            .aliases
                            .iter()
                            .map(|alias| alias.trim_start_matches('/')),
                    );
            if busy && !entry.valid_mid_turn {
                candidate.disabled("unavailable while a turn is in flight")
            } else {
                candidate
            }
        })
        .collect()
}

/// S:I10 — rebuild popup catalog from shared grammar plus truthful effects.
///
/// Rebuilt (not merely re-filtered) whenever turn state changes, because a
/// command that cannot run right now has to carry the reason — the popup
/// keeps disabled rows visible rather than hiding them.
fn refresh_completion_catalog(
    composer: &mut Composer,
    session: &SessionState,
    turn_in_flight: bool,
) {
    // `has_open_entry` covers work the timeline knows is still moving even
    // when this loop has no handle for it; the handle covers the turn the
    // loop itself spawned.
    let busy = turn_in_flight || session.timeline.has_open_entry();
    let mut catalog = command_effect_catalog(busy);
    catalog.extend(shared_command_catalog(busy));
    // #132: custom slash commands (`.claude/commands`, `.coderight/commands`, user root).
    catalog.extend(
        crate::custom_commands::popup_rows()
            .into_iter()
            .map(|(name, summary)| CompletionCandidate::new(name, summary)),
    );
    composer.set_completion_catalog(catalog);
}

/// S:I6 — record a route decision whenever the attribution the nav model
/// reports differs from the timeline's latest one.
///
/// Nothing here invents a value: `active_session` is the session this host
/// started on or switched to, and the actor/model come from the agent tree
/// the daemon feed last refreshed. Comparing first keeps one route entry
/// per actual change — recording one per frame would flood the timeline and
/// could never mark a boundary.
fn sync_route(nav: &NavView, session: &mut SessionState, transcript: &mut Transcript) {
    // Once daemon attribution arrives, it is authoritative. Nav refreshes may
    // describe a selected child or stale summary and must not manufacture a
    // model boundary over the persisted route decision.
    if session.authoritative_route {
        return;
    }
    let Some(agent) = nav.viewed_agent() else {
        return;
    };
    // "-" is `refresh_nav`'s honest placeholder for "this host does not know
    // the model"; recording it would paint a fabricated attribution.
    if agent.model.is_empty() || agent.model == "-" {
        return;
    }
    let changed = match session.timeline.latest_route() {
        Some((project, model, actor)) => {
            project != session.active_session.as_str()
                || model != agent.model.as_str()
                || actor != agent.name.as_str()
        }
        None => true,
    };
    if changed {
        let project = session.active_session.clone();
        session.record(
            transcript,
            DaemonEvent::RouteDecided {
                project,
                model: agent.model.clone(),
                actor: agent.name.clone(),
            },
        );
        // This route was inferred from nav fallback, not persisted by daemon;
        // keep fallback eligible until an authoritative RouteDecided arrives.
        session.authoritative_route = false;
    }
}

/// D5: the terminal background after the `[tui] theme` preference.
fn effective_background(
    setting: &str,
    detected: rightkit_terminal_ui::terminal::Background,
) -> rightkit_terminal_ui::terminal::Background {
    match setting {
        "dark" => rightkit_terminal_ui::terminal::Background::Dark,
        "light" => rightkit_terminal_ui::terminal::Background::Light,
        _ => detected,
    }
}

/// D5: the notifier for the `[tui] notify` preference. An explicit
/// `RIGHTKIT_NOTIFY` environment value still wins over the file.
fn build_notifier(
    setting: &str,
    cwd: &std::path::Path,
) -> rightkit_terminal_ui::terminal::notify::TerminalNotifier {
    use rightkit_terminal_ui::terminal::notify::{NotifyMethod, TerminalNotifier};
    let env_set = std::env::var("RIGHTKIT_NOTIFY")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);
    let method = match setting {
        "bel" => Some(NotifyMethod::Bel),
        "osc9" => Some(NotifyMethod::Osc9),
        "off" => Some(NotifyMethod::Off),
        _ => None,
    };
    match (env_set, method) {
        (false, Some(method)) => {
            let title_enabled = !matches!(
                std::env::var("RIGHTKIT_TERMINAL_TITLE")
                    .ok()
                    .map(|v| v.trim().to_ascii_lowercase())
                    .as_deref(),
                Some("off" | "0" | "false" | "no")
            );
            TerminalNotifier::new(method, title_enabled, cwd)
        }
        _ => TerminalNotifier::from_env(cwd),
    }
}

/// Footer-sized degradation code: the reason's leading identifier (before any
/// `:` detail), capped so the route and model stay visible. The full reason
/// is in the one-time "Context is off (...)" notice.
fn context_off_code(reason: &str) -> String {
    let code = reason.split(':').next().unwrap_or(reason).trim();
    let code = if code.is_empty() { "unavailable" } else { code };
    code.chars().take(24).collect()
}

/// D14/D15: open the project home or sidebar over the host's catalog of ALL
/// projects and chats.
async fn open_project_picker(
    host: &impl TurnHost,
    session: &mut SessionState,
    kind: crate::tui_projects::PickKind,
) {
    use crate::tui_projects::{build_home, build_sidebar, PickKind};
    let projects = host.project_catalog().await;
    let current_dir = host.active_directory();
    let current_session = host.current_session();
    session.proj.home_refresh =
        kind == PickKind::Home && projects.iter().all(|project| project.chats.is_empty());
    session.proj.picker = Some(match kind {
        PickKind::Sidebar => {
            build_sidebar(projects, current_dir.as_deref(), current_session.as_deref())
        }
        _ => build_home(projects, current_dir.as_deref(), current_session.as_deref()),
    });
}

/// Move the view onto whatever session the host now targets (after a project
/// switch or a new chat): fresh transcript and view state, per-session draft
/// isolation, nav re-read for the new scope.
#[allow(clippy::too_many_arguments)]
async fn rebind_view_to_active(
    host: &impl TurnHost,
    composer: &mut Composer,
    nav: &mut NavView,
    session: &mut SessionState,
    transcript: &mut Transcript,
    interjections: &InterjectionBuffer,
    viewed_agent: &mut Option<AgentId>,
    paging: &mut HistoryPaging,
    note: String,
) {
    let Some(id) = host.current_session() else {
        session.notice(transcript, note);
        return;
    };
    let _ = host.goal_op("get", None).await;
    let from = view_key(session, viewed_agent.as_ref());
    interjections.clear();
    transcript.clear();
    session.reset_view(&id);
    *viewed_agent = None;
    paging.rebind_session();
    session.active_session = id;
    let to = view_key(session, viewed_agent.as_ref());
    switch_view(composer, &from, &to);
    host.refresh_nav(nav, false).await;
    session.notice(transcript, note);
}

/// D15/D5: a configured chord (sidebar, projects, settings) opens its
/// overlay. Only when nothing else owns the keys.
async fn try_open_overlay_chord(
    host: &impl TurnHost,
    event: &TuiEvent,
    session: &mut SessionState,
    requester: &FrameRequester,
) -> bool {
    use crate::tui_projects::{key_matches, PickKind};
    let TuiEvent::Key(key) = event else {
        return false;
    };
    if key.kind != KeyEventKind::Press
        || session.proj.picker.is_some()
        || session.model_picker.is_some()
        || session.providers_picker.is_some()
        || session.repository_picker.is_some()
        || session.rewind.is_some()
        || session.pending_question.is_some()
        || session.pending_plan.is_some()
        || session.pending_trust.is_some()
    {
        return false;
    }
    let sidebar = key_matches(&session.proj.settings.key_for("sidebar"), key);
    let projects = key_matches(&session.proj.settings.key_for("projects"), key);
    let config = key_matches(&session.proj.settings.key_for("config"), key);
    if sidebar {
        open_project_picker(host, session, PickKind::Sidebar).await;
    } else if projects {
        open_project_picker(host, session, PickKind::Home).await;
    } else if config {
        session.proj.settings = host.tui_settings();
        session.proj.picker = Some(crate::tui_projects::build_config(&session.proj.settings));
    } else {
        return false;
    }
    requester.schedule_immediate();
    true
}

/// Drive the open project/sidebar/config overlay with one event. Returns
/// `true` when the overlay consumed it.
#[allow(clippy::too_many_arguments)]
async fn handle_action_picker(
    host: &impl TurnHost,
    event: &TuiEvent,
    composer: &mut Composer,
    nav: &mut NavView,
    session: &mut SessionState,
    transcript: &mut Transcript,
    surface: &mut Surface,
    interjections: &InterjectionBuffer,
    turn_in_flight: bool,
    viewed_agent: &mut Option<AgentId>,
    paging: &mut HistoryPaging,
    requester: &FrameRequester,
) -> bool {
    use crate::tui_projects::{
        expand_path, key_matches, looks_like_path, project_label, same_root, PickAction, PickKind,
    };
    let TuiEvent::Key(key) = event else {
        return false;
    };
    let Some(mut active) = session.proj.picker.take() else {
        return false;
    };
    if key.kind != KeyEventKind::Press {
        session.proj.picker = Some(active);
        return true;
    }
    let kind = active.kind;
    // The sidebar chord toggles it closed again.
    if kind == PickKind::Sidebar && key_matches(&session.proj.settings.key_for("sidebar"), key) {
        requester.schedule_immediate();
        return true;
    }
    let mut chosen: Option<PickAction> = None;
    let mut typed_path: Option<String> = None;
    if kind == PickKind::Home
        && key.code == KeyCode::Enter
        && active.picker.selected().is_none()
        && looks_like_path(active.picker.query())
    {
        typed_path = Some(expand_path(active.picker.query()));
    }
    if typed_path.is_none() {
        let _ = active.picker.handle(event);
        for action in active.picker.drain_actions() {
            if let rightkit_terminal_ui::nav::PickerAction::Accepted { index, .. } = action {
                chosen = active
                    .picker
                    .filtered_idx(index)
                    .and_then(|row| active.actions.get(row))
                    .cloned();
            }
        }
    }
    // A settings row that only displays keeps the screen open.
    let keep_open = kind == PickKind::Config && matches!(chosen, Some(PickAction::Noop));
    if !(active.picker.is_done() || chosen.is_some() || typed_path.is_some()) || keep_open {
        session.proj.picker = Some(active);
        requester.schedule_immediate();
        return true;
    }
    if kind == PickKind::Home {
        host.home_closed();
    }
    let action = typed_path.map(PickAction::EnterProject).or(chosen);
    match action {
        Some(PickAction::EnterProject(root)) => {
            let same = host
                .active_directory()
                .is_some_and(|current| same_root(&current, &root));
            let label = project_label(&root);
            if same && kind == PickKind::Home {
                // Enter on the launch folder goes straight in.
            } else if same {
                match host.new_session().await {
                    Ok(_) => {
                        rebind_view_to_active(
                            host,
                            composer,
                            nav,
                            session,
                            transcript,
                            interjections,
                            viewed_agent,
                            paging,
                            format!("Started a new chat in {label}."),
                        )
                        .await;
                    }
                    Err(reason) => session.notice(transcript, reason),
                }
            } else {
                match host.select_repository(&root).await {
                    Ok(()) => {
                        rebind_view_to_active(
                            host,
                            composer,
                            nav,
                            session,
                            transcript,
                            interjections,
                            viewed_agent,
                            paging,
                            format!("Opened project {label} · new chat."),
                        )
                        .await;
                    }
                    Err(reason) => session.notice(transcript, reason),
                }
            }
        }
        Some(PickAction::OpenChat(id)) => {
            switch_to_session(
                host,
                SessionId(id),
                composer,
                nav,
                session,
                transcript,
                surface,
                turn_in_flight,
                viewed_agent,
                paging,
            )
            .await;
        }
        Some(PickAction::SetConfig(key, value)) => {
            let outcome = match host.set_tui_setting(&key, &value).await {
                Ok(message) => {
                    session.proj.settings = host.tui_settings();
                    if key == "vim" {
                        composer.set_vim_enabled(session.proj.settings.vim);
                    }
                    session.proj.settings_dirty = true;
                    session.notice(transcript, message.clone());
                    message
                }
                Err(reason) => {
                    session.notice(transcript, reason.clone());
                    reason
                }
            };
            let selected = active.picker.selected();
            let mut refreshed = crate::tui_projects::build_config_with_notice(
                &session.proj.settings,
                Some(&outcome),
            );
            if let Some(selected) = selected {
                refreshed.picker.select(selected);
            }
            session.proj.picker = Some(refreshed);
        }
        Some(PickAction::Noop) | None => {}
    }
    requester.schedule_immediate();
    true
}

/// S:I7 — the key the composer's draft store files unsent text under: the
/// active session, or that session plus the agent being viewed.
fn view_key(session: &SessionState, viewed_agent: Option<&AgentId>) -> String {
    match viewed_agent {
        Some(id) => format!("{}/{}", session.active_session, id.0),
        None => session.active_session.clone(),
    }
}

/// S:I7 — per-view draft preservation: stash what is being typed for the
/// view we are leaving, restore what was being typed in the one we enter.
/// Silent either way — a restored draft is self-evident in the composer, and
/// an empty buffer simply clears the stash.
fn switch_view(composer: &mut Composer, from: &str, to: &str) {
    if from == to {
        return;
    }
    composer.save_draft(from);
    // `restore_draft` deliberately leaves the buffer alone when the target
    // view has no draft; without this clear the previous view's text would
    // sit in the new view while also being stashed under the old one.
    if !composer.restore_draft(to) {
        composer.set_text("");
    } else {
        // Restore can happen after the event dispatcher's normal sync pass;
        // refresh here so a restored `/command` cannot inherit stale popup
        // rows from the view we just left.
        composer.sync_completion();
    }
}

/// Translate the documented Ctrl-Y/Ctrl-N approval bindings into the
/// overlay's own option-digit vocabulary. The translated key still passes
/// through the overlay's arming gate, so a buffered or racing keypress can
/// never decide a request it was not typed against.
fn approval_overlay_key(key: &KeyEvent, request: &ApprovalRequest) -> TuiEvent {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        let target = match key.code {
            KeyCode::Char('y') => Some(ApprovalDecision::Approved),
            KeyCode::Char('n') => Some(ApprovalDecision::Denied),
            _ => None,
        };
        if let Some(target) = target {
            if let Some(index) = request.options.iter().position(|d| *d == target) {
                if let Some(digit) = char::from_digit(index as u32 + 1, 10) {
                    return TuiEvent::Key(KeyEvent::new(KeyCode::Char(digit), KeyModifiers::NONE));
                }
            }
        }
    }
    TuiEvent::Key(*key)
}

/// Apply one route-selection intent through the host authority. Called for
/// intents from the model picker AND from `nav.drain_intents` — either
/// source lands on the same `select_model`/`select_effort` seam.
async fn apply_route_intent(
    host: &impl TurnHost,
    session: &mut SessionState,
    transcript: &mut Transcript,
    intent: NavIntent,
) {
    match intent {
        NavIntent::SelectModel(id) => match host.select_model(&id).await {
            Ok(()) => session.notice(transcript, format!("Model set to {id}.")),
            Err(error) => session.notice(transcript, error),
        },
        NavIntent::SelectEffort(id) => match host.select_effort(&id).await {
            Ok(()) => session.notice(transcript, format!("Effort set to {id}.")),
            Err(error) => session.notice(transcript, error),
        },
        // Navigation intents are owned by the caller's match; reaching here
        // with one means a picker emitted something unexpected — record it
        // as a typed notice rather than dropping it silently.
        other => session.notice(
            transcript,
            format!("Ignored route-picker intent: {other:?}"),
        ),
    }
}

/// Service a one-shot picker request raised by a command: route pickers go
/// to `open_route_picker`; `/resume` opens the session list or switches in
/// place and replays that session's history.
async fn service_picker_request(
    host: &impl TurnHost,
    request: PickerRequest,
    composer: &mut Composer,
    nav: &mut NavView,
    session: &mut SessionState,
    transcript: &mut Transcript,
    surface: &mut Surface,
    turn_in_flight: bool,
    viewed_agent: &mut Option<AgentId>,
    paging: &mut HistoryPaging,
    requester: &FrameRequester,
) {
    match request {
        PickerRequest::Sessions => {
            host.refresh_nav(nav, turn_in_flight).await;
            surface.enter_modal(ModalKind::SessionPicker);
            nav.open_sessions();
            nav.drain_intents();
            requester.schedule_immediate();
        }
        PickerRequest::Agents => {
            host.refresh_nav(nav, turn_in_flight).await;
            surface.enter_modal(ModalKind::AgentPicker);
            nav.open_agents();
            nav.drain_intents();
            requester.schedule_immediate();
        }
        PickerRequest::ResumeSession(id) => {
            switch_to_session(
                host,
                SessionId(id),
                composer,
                nav,
                session,
                transcript,
                surface,
                turn_in_flight,
                viewed_agent,
                paging,
            )
            .await;
            requester.schedule_immediate();
        }
        PickerRequest::Home => {
            open_project_picker(host, session, crate::tui_projects::PickKind::Home).await;
            requester.schedule_immediate();
        }
        PickerRequest::Sidebar => {
            open_project_picker(host, session, crate::tui_projects::PickKind::Sidebar).await;
            requester.schedule_immediate();
        }
        PickerRequest::Config => {
            session.proj.settings = host.tui_settings();
            session.proj.picker = Some(crate::tui_projects::build_config(&session.proj.settings));
            requester.schedule_immediate();
        }
        other => open_route_picker(host, session, transcript, other, requester).await,
    }
}

/// Open the route picker a command surface requested, sourcing rows from
/// the host's real catalog and committed route — never a donor registry.
/// Async because `model_catalog` resolves live broker status; it is awaited
/// at the drive loop's picker-request drain points.
async fn open_route_picker(
    host: &impl TurnHost,
    session: &mut SessionState,
    transcript: &mut Transcript,
    request: PickerRequest,
    requester: &FrameRequester,
) {
    const MODEL_PICKER_VIEW_ID: u32 = 1;
    const PROVIDERS_PICKER_VIEW_ID: u32 = 3;
    match request {
        PickerRequest::Model => {
            // Provider-first: level 1 lists only configured providers —
            // offering a provider the route authority cannot serve would
            // dead-end one step later.
            let providers: Vec<rightkit_terminal_ui::nav::ProviderChoice> = host
                .provider_catalog()
                .await
                .into_iter()
                .filter(|provider| provider.offered_in_model_picker())
                .collect();
            if providers.is_empty() {
                // A host without provider groups keeps the flat picker —
                // the model-first list is the same catalog, ungrouped.
                let catalog = host.model_catalog().await;
                if catalog.is_empty() {
                    session.notice(
                        transcript,
                        "No model catalog is available from this host; nothing to pick.",
                    );
                    return;
                }
                session.model_picker = Some(ModelPicker::new(MODEL_PICKER_VIEW_ID, catalog));
                return;
            }
            session.model_picker =
                Some(ModelPicker::provider_first(MODEL_PICKER_VIEW_ID, providers));
        }
        PickerRequest::Effort => {
            let catalog = host.model_catalog().await;
            let committed = host.committed_route();
            let efforts = committed
                .model
                .as_ref()
                .and_then(|m| catalog.iter().find(|c| &c.id == m))
                .map(|m| m.efforts.clone())
                .unwrap_or_default();
            if efforts.is_empty() {
                session.notice(
                    transcript,
                    "No effort options are available for the committed model.",
                );
                return;
            }
            let label = committed
                .model
                .unwrap_or_else(|| "current model".to_string());
            session.model_picker = Some(ModelPicker::effort_only(
                MODEL_PICKER_VIEW_ID,
                &label,
                efforts,
            ));
        }
        PickerRequest::Providers => {
            // `/providers` lists every known provider (configured or not)
            // with its full model list — visibility management happens
            // against the same provider-grouped catalog `/model` consumes.
            // The screen is the fixed provider list; custom configured
            // routes belong to `/model` only (no key/sign-in actions).
            let providers: Vec<_> = host
                .provider_catalog()
                .await
                .into_iter()
                .filter(|provider| {
                    crate::main_runtime::provider_setup::screen_provider(&provider.id).is_some()
                })
                .collect();
            if providers.is_empty() {
                session.notice(
                    transcript,
                    "No provider catalog is available from this host.",
                );
                return;
            }
            session.providers_picker = Some(rightkit_terminal_ui::nav::ProvidersPicker::new(
                PROVIDERS_PICKER_VIEW_ID,
                providers,
            ));
        }
        PickerRequest::Sessions
        | PickerRequest::Agents
        | PickerRequest::ResumeSession(_)
        | PickerRequest::Home
        | PickerRequest::Sidebar
        | PickerRequest::Config => {}
        PickerRequest::Repository => {
            let choices = host.repository_catalog();
            if choices.is_empty() {
                session.notice(transcript, "No enrolled repositories available. Use /repo <path> to select an existing scope.");
                return;
            }
            let items = choices
                .iter()
                .map(|(root, label)| {
                    rightkit_terminal_ui::nav::SelectionItem::new(label.clone())
                        .description(root.clone())
                        .search_value(root.clone())
                })
                .collect();
            session.repository_choices = choices.into_iter().map(|(root, _)| root).collect();
            session.repository_picker = Some(rightkit_terminal_ui::nav::ListPicker::new(
                rightkit_terminal_ui::nav::PickerParams::picker(2, "Select repository", items),
            ));
        }
    }
    requester.schedule_immediate();
}

/// Switch the view and host to another root session and replay its durable
/// history into the (cleared) transcript. Shared by the session picker and
/// `/resume`.
async fn switch_to_session(
    host: &impl TurnHost,
    id: SessionId,
    composer: &mut Composer,
    nav: &mut NavView,
    session: &mut SessionState,
    transcript: &mut Transcript,
    surface: &mut Surface,
    turn_in_flight: bool,
    viewed_agent: &mut Option<AgentId>,
    paging: &mut HistoryPaging,
) {
    surface.leave_modal();
    if id.0 == LOCAL_SESSION_ID || host.current_session().as_deref() == Some(id.0.as_str()) {
        session.notice(
            transcript,
            "Already on this session. Your next turn continues this conversation.",
        );
    } else {
        // P1-5: switching to another root session is a detach, never
        // a cancel (invariant AK); the daemon-backed host re-points
        // its active session and leaves the old tree running. The
        // local transcript view is cleared so the old tree's
        // messages never linger under the new session's breadcrumb.
        match host.switch_session(&id).await {
            Ok(()) => {
                // S:I7 — a sideways session switch is a view
                // switch too: stash the draft under the session
                // being left, restore the one filed under the new
                // session (the old tree keeps running).
                let from = view_key(session, viewed_agent.as_ref());
                transcript.clear();
                // Rebinds the approval overlay to the new
                // session so the old session's pending ids can
                // never be answered from this view.
                session.reset_view(&id.0);
                // P1-5: the session switch resets both the view
                // and any live-transcript cursor for the tree we
                // left; a later Descend re-seeds a fresh one.
                *viewed_agent = None;
                // P2-5: the durable ledger this view pages from
                // is now the new session's, not the old one's.
                // Rebind here — inside the accepted-switch arm,
                // before this handler returns to the select loop —
                // so no later maintenance tick can fetch the new
                // session's first page with the old session's
                // cursor or skip paging under a stale exhaustion
                // flag.
                paging.rebind_session();
                session.active_session = id.0.clone();
                let _ = host.goal_op("get", None).await;
                let to = view_key(session, viewed_agent.as_ref());
                switch_view(composer, &from, &to);
                // S:I6 — the new session's own attribution is
                // recorded as a route decision on the next sync.
                host.refresh_nav(nav, turn_in_flight).await;
                session.notice(
                    transcript,
                    format!(
                        "Switched to session {id}. Your next turn continues this conversation."
                    ),
                );
            }
            Err(reason) => {
                session.notice(transcript, reason);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_event(
    host: &impl TurnHost,
    event: &TuiEvent,
    composer: &mut Composer,
    nav: &mut NavView,
    session: &mut SessionState,
    transcript: &mut Transcript,
    surface: &mut Surface,
    interjections: &InterjectionBuffer,
    pending_echoes: &mut std::collections::VecDeque<String>,
    turn: &mut Option<JoinHandle<i32>>,
    requester: &FrameRequester,
    exit_code: &mut i32,
    should_exit: &mut bool,
    viewed_agent: &mut Option<AgentId>,
    paging: &mut HistoryPaging,
) {
    let _ = exit_code;
    // Whether a turn was already in flight when this event arrived: a turn
    // that starts *during* this event is the moment attribution (S:I6) and
    // the popup's disabled reasons (S:I10) have to be re-evaluated.
    let turn_was_in_flight = turn.is_some();

    if let TuiEvent::Key(key) = event {
        if key.kind != KeyEventKind::Release
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('l')
        {
            // The frame loop owns the terminal: clear + repaint happen in `render`.
            session.clear_screen_requested = true;
            requester.schedule_immediate();
            return;
        }
        #[cfg(unix)]
        if key.kind != KeyEventKind::Release
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('z')
        {
            suspend_tui();
            requester.schedule_immediate();
            return;
        }
        if key.kind != KeyEventKind::Release
            && key.modifiers.contains(KeyModifiers::ALT)
            && matches!(key.code, KeyCode::Char('m' | 'M' | 'e' | 'E'))
        {
            let catalog = host.model_catalog().await;
            let route = host.committed_route();
            let direction = if key.code == KeyCode::Char('M') || key.code == KeyCode::Char('E') {
                rightkit_terminal_ui::nav::CycleDirection::Previous
            } else {
                rightkit_terminal_ui::nav::CycleDirection::Next
            };
            let selected = if matches!(key.code, KeyCode::Char('m' | 'M')) {
                rightkit_terminal_ui::nav::cycle_model_intents(
                    &catalog,
                    route.model.as_deref().unwrap_or(""),
                    direction,
                )
            } else {
                route
                    .model
                    .as_ref()
                    .and_then(|model| catalog.iter().find(|choice| &choice.id == model))
                    .and_then(|choice| {
                        rightkit_terminal_ui::nav::cycle_effort_intent(
                            &choice.efforts,
                            route.effort.as_deref().unwrap_or(""),
                            direction,
                        )
                    })
                    .into_iter()
                    .collect()
            };
            let mut error = None;
            for intent in selected {
                let result = match intent {
                    NavIntent::SelectModel(model) => host.select_model(&model).await,
                    NavIntent::SelectEffort(effort) => host.select_effort(&effort).await,
                    _ => continue,
                };
                if let Err(reason) = result {
                    error = Some(reason);
                    break;
                }
            }
            if let Some(reason) = error {
                session.notice(transcript, reason);
            }
            requester.schedule_immediate();
            return;
        }
    }

    if session.pending_trust.is_some()
        && !matches!(event, TuiEvent::Key(key) if key.modifiers.contains(KeyModifiers::CONTROL))
    {
        // D2: the first-launch trust card decides on one key: y trusts, n or
        // Esc stays untrusted (the folder remains reachable via /trust).
        // Ctrl-chords (Ctrl-C quit) are never swallowed by the card.
        if let TuiEvent::Key(key) = event {
            if key.kind != KeyEventKind::Release {
                let decision = match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => Some(true),
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Some(false),
                    _ => None,
                };
                if let Some(trust) = decision {
                    if let Some(root) = session.pending_trust.take() {
                        if trust {
                            match host.trust_workspace(&root).await {
                                Ok(()) => session.notice(
                                    transcript,
                                    format!(
                                        "Trusted workspace: {}",
                                        rightkit_terminal_ui::render::display_path_str(&root)
                                    ),
                                ),
                                Err(error) => session.notice(transcript, error),
                            }
                        } else {
                            session.notice(
                                transcript,
                                "Workspace left untrusted. Run /trust to grant access later.",
                            );
                        }
                    }
                }
            }
        }
        requester.schedule_immediate();
        flush_history(transcript, surface, requester);
        return;
    }

    if session.pending_credential.is_some() {
        if !session.approval_input_safe {
            return;
        }
        match event {
            TuiEvent::Paste(text) => {
                if let Some(pending) = session.pending_credential.as_mut() {
                    for ch in text.chars() {
                        if pending.secret.len() + ch.len_utf8() > 8192 {
                            break;
                        }
                        pending.secret.push(ch);
                    }
                }
            }
            TuiEvent::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Esc => {
                    if let Some(mut pending) = session.pending_credential.take() {
                        pending.secret.clear();
                        if let Err(error) = host
                            .resolve_credential_request(
                                &pending.session_id,
                                &pending.request_id,
                                "cancel",
                                None,
                            )
                            .await
                        {
                            session.notice(transcript, error);
                        }
                    }
                }
                KeyCode::Enter => {
                    if let Some(mut pending) = session.pending_credential.take() {
                        let result = host
                            .resolve_credential_request(
                                &pending.session_id,
                                &pending.request_id,
                                if pending.secret.is_empty() {
                                    "decline"
                                } else {
                                    "supply"
                                },
                                (!pending.secret.is_empty()).then_some(pending.secret.as_str()),
                            )
                            .await;
                        pending.secret.clear();
                        if let Err(error) = result {
                            session.notice(transcript, error);
                        }
                    }
                }
                KeyCode::Backspace => {
                    if let Some(pending) = session.pending_credential.as_mut() {
                        pending.secret.pop();
                    }
                }
                KeyCode::Char(ch)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    if let Some(pending) = session.pending_credential.as_mut() {
                        if pending.secret.len() + ch.len_utf8() <= 8192 {
                            pending.secret.push(ch);
                        }
                    }
                }
                _ => {}
            },
            _ => {}
        }
        requester.schedule_immediate();
        flush_history(transcript, surface, requester);
        return;
    }

    if session.pending_elicitation.is_some() {
        if !session.approval_input_safe {
            return;
        }
        if session
            .pending_elicitation
            .as_ref()
            .is_some_and(|pending| pending.form.is_some())
        {
            use rightkit_terminal_ui::render::elicitation_form::FormOutcome;
            let outcome = match event {
                TuiEvent::Paste(text) => {
                    if let Some(form) = session
                        .pending_elicitation
                        .as_mut()
                        .and_then(|pending| pending.form.as_mut())
                    {
                        form.handle_paste(text);
                    }
                    FormOutcome::Continue
                }
                TuiEvent::Key(key) if key.kind != KeyEventKind::Release => {
                    let typed_with_modifier = matches!(key.code, KeyCode::Char(_))
                        && key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
                    match session
                        .pending_elicitation
                        .as_mut()
                        .and_then(|pending| pending.form.as_mut())
                    {
                        Some(form) if !typed_with_modifier => form.handle_key(key.code),
                        _ => FormOutcome::Continue,
                    }
                }
                _ => FormOutcome::Continue,
            };
            let answer = match outcome {
                FormOutcome::Continue => None,
                FormOutcome::Decline => {
                    Some((rightkit_agent_engine_protocol::ElicitationAction::Decline, None))
                }
                FormOutcome::Submit(value) => {
                    Some((rightkit_agent_engine_protocol::ElicitationAction::Accept, Some(value)))
                }
            };
            if let Some((action, content)) = answer {
                if let Some(pending) = session.pending_elicitation.take() {
                    if let Err(error) = host
                        .resolve_mcp_elicitation(
                            &pending.session_id,
                            &pending.request_id,
                            action,
                            content,
                        )
                        .await
                    {
                        session.notice(transcript, error);
                    }
                }
            }
            requester.schedule_immediate();
            flush_history(transcript, surface, requester);
            return;
        }
        match event {
            TuiEvent::Paste(text) => {
                if let Some(pending) = session.pending_elicitation.as_mut() {
                    if matches!(
                        &pending.mode,
                        rightkit_agent_engine_protocol::ElicitationRequest::Form { .. }
                    ) {
                        for ch in text.chars() {
                            if pending.content.len() + ch.len_utf8() > 8192 {
                                break;
                            }
                            pending.content.push(ch);
                        }
                    }
                }
            }
            TuiEvent::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Esc => {
                    if let Some(pending) = session.pending_elicitation.take() {
                        if let Err(error) = host
                            .resolve_mcp_elicitation(
                                &pending.session_id,
                                &pending.request_id,
                                match pending.mode {
                                    rightkit_agent_engine_protocol::ElicitationRequest::Url { .. } => {
                                        rightkit_agent_engine_protocol::ElicitationAction::Cancel
                                    }
                                    _ => rightkit_agent_engine_protocol::ElicitationAction::Decline,
                                },
                                None,
                            )
                            .await
                        {
                            session.notice(transcript, error);
                        }
                    }
                }
                KeyCode::Enter => {
                    let (is_url, form_content) = session
                        .pending_elicitation
                        .as_ref()
                        .map(|pending| {
                            (
                                matches!(
                                    &pending.mode,
                                    rightkit_agent_engine_protocol::ElicitationRequest::Url { .. }
                                ),
                                pending.content.clone(),
                            )
                        })
                        .unwrap_or((false, String::new()));
                    let decision = if is_url {
                        Some((rightkit_agent_engine_protocol::ElicitationAction::Accept, None))
                    } else if session.pending_elicitation.is_some() {
                        match serde_json::from_str::<serde_json::Value>(&form_content) {
                            Ok(value) if value.is_object() => {
                                Some((rightkit_agent_engine_protocol::ElicitationAction::Accept, Some(value)))
                            }
                            Ok(_) => {
                                session.notice(transcript, "MCP response must be a JSON object.");
                                None
                            }
                            Err(error) => {
                                session.notice(transcript, format!("MCP response JSON: {error}"));
                                None
                            }
                        }
                    } else {
                        None
                    };
                    if let Some((action, content)) = decision {
                        if let Some(mut pending) = session.pending_elicitation.take() {
                            let result = host
                                .resolve_mcp_elicitation(
                                    &pending.session_id,
                                    &pending.request_id,
                                    action,
                                    content,
                                )
                                .await;
                            pending.content.clear();
                            if let Err(error) = result {
                                session.notice(transcript, error);
                            }
                        }
                    }
                }
                KeyCode::Backspace => {
                    if let Some(pending) = session.pending_elicitation.as_mut() {
                        pending.content.pop();
                    }
                }
                KeyCode::Char(ch)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    if let Some(pending) = session.pending_elicitation.as_mut() {
                        if matches!(
                            &pending.mode,
                            rightkit_agent_engine_protocol::ElicitationRequest::Form { .. }
                        ) && pending.content.len() + ch.len_utf8() <= 8192
                        {
                            pending.content.push(ch);
                        }
                    }
                }
                _ => {}
            },
            _ => {}
        }
        requester.schedule_immediate();
        flush_history(transcript, surface, requester);
        return;
    }

    if session.pending_plan.is_some() || session.pending_question.is_some() {
        if let TuiEvent::Paste(text) = event {
            if session.approval_input_safe {
                if let Some(pending) = session.pending_question.as_mut() {
                    for character in text.chars() {
                        if pending.answer_text.len() + character.len_utf8() > 8192 {
                            break;
                        }
                        pending.answer_text.push(character);
                    }
                }
            }
            requester.schedule_immediate();
            return;
        }
    }

    // Resize/Resume update geometry before anything else sees the event, so
    // components that read width/height off this event (the composer does)
    // and the surface agree on the same frame.
    match event {
        TuiEvent::Resize { width, height } => {
            surface.resize(*width, *height);
            requester.record_reflow();
        }
        TuiEvent::Resume => {
            // Resizes are not delivered while suspended -- re-query real
            // geometry rather than trusting whatever the surface last held.
            if let Ok((w, h)) = ct_terminal::size() {
                surface.resize(w, h);
                requester.record_reflow();
            }
        }
        _ => {}
    }

    // A070/P-16: while attached to an interactive PTY every key, paste and
    // resize belongs to the child; Ctrl+] detaches.
    if session.pty.is_some() {
        pty_attached_event(host, event, composer, session, transcript, requester).await;
        return;
    }

    // Ctrl-C during a turn cancels the turn immediately, ahead of the
    // composer's own double-tap-to-quit semantics -- the composer has no
    // notion of an in-flight engine turn, only of `streaming`/`shell_running`
    // flags the host sets, so this is the host's own gate, applied before the
    // event ever reaches a component.
    session.width_hint = surface.screen_size().0.max(20);
    if surface.modal() == Some(ModalKind::Fullscreen)
        && matches!(event, TuiEvent::Key(key) if key.kind == KeyEventKind::Press && key.code == KeyCode::Esc)
    {
        surface.leave_modal();
        session.proj.settings.fullscreen = false;
        let _ = host.set_tui_setting("fullscreen", "off").await;
        requester.schedule_immediate();
        return;
    }
    if let Some(key) = match event {
        TuiEvent::Key(key) if session.copy_picker.is_some() && key.kind == KeyEventKind::Press => {
            Some(key)
        }
        _ => None,
    } {
        let mut picker = session.copy_picker.take().expect("checked above");
        let code = match key.code {
            KeyCode::Esc => {
                session.notice(transcript, "Copy cancelled.");
                requester.schedule_immediate();
                return;
            }
            KeyCode::Char(ch) => ch,
            _ => return,
        };
        if picker.writing_block {
            picker.writing_block = false;
            if let Some((index, (language, block))) = code
                .to_digit(10)
                .and_then(|n| n.checked_sub(1))
                .map(|n| n as usize)
                .and_then(|index| picker.code_blocks.get(index).map(|entry| (index, entry)))
            {
                let language = language.as_deref().unwrap_or("text");
                let extension = match language {
                    "rust" => "rs",
                    "python" => "py",
                    "javascript" => "js",
                    "typescript" => "ts",
                    "shell" | "bash" | "sh" => "sh",
                    "text" | "plaintext" => "txt",
                    lang if !lang.is_empty() && lang.chars().all(|c| c.is_ascii_alphanumeric()) => {
                        lang
                    }
                    _ => "txt",
                };
                let mut path = std::env::current_dir().unwrap_or_default().join(format!(
                    "code-block-{}.{}",
                    index + 1,
                    extension
                ));
                let mut suffix = 1usize;
                while path.exists() {
                    path = std::env::current_dir().unwrap_or_default().join(format!(
                        "code-block-{}-{suffix}.{}",
                        index + 1,
                        extension
                    ));
                    suffix += 1;
                }
                match std::fs::write(&path, block) {
                    Ok(()) => session.notice(
                        transcript,
                        format!("Wrote code block to {}.", path.display()),
                    ),
                    Err(error) => {
                        session.notice(transcript, format!("Could not write code block: {error}"))
                    }
                }
            } else {
                session.notice(transcript, "Choose a listed code block number.");
                session.copy_picker = Some(picker);
            }
        } else if code == 'f' {
            let text = picker.markdown.clone();
            session.notice(transcript, clipboard_copy_notice(&text));
        } else if code == 'w' {
            if picker.code_blocks.is_empty() {
                session.notice(transcript, "No code blocks in this response.");
            } else {
                picker.writing_block = true;
                session.notice(transcript, "Write which code block? Press its number.");
                session.copy_picker = Some(picker);
            }
        } else if let Some(index) = code
            .to_digit(10)
            .and_then(|n| n.checked_sub(1))
            .map(|n| n as usize)
        {
            if let Some((_, text)) = picker.code_blocks.get(index) {
                let text = text.clone();
                session.notice(transcript, clipboard_copy_notice(&text));
            } else {
                session.notice(transcript, "Choose a listed code block number.");
                session.copy_picker = Some(picker);
            }
        } else {
            session.notice(transcript, "Copy picker: f full response · 1–9 copy block · w then number writes block · Esc cancel.");
            session.copy_picker = Some(picker);
        }
        requester.schedule_immediate();
        return;
    }
    if let Some(mouse) = match event {
        TuiEvent::Mouse(mouse) if surface.modal() == Some(ModalKind::Fullscreen) => Some(mouse),
        _ => None,
    } {
        use ct_event::{MouseButton, MouseEventKind};
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((line, column)) = selection_point(session, mouse.column, mouse.row) {
                    if session
                        .selection_lines
                        .get(line)
                        .is_some_and(|text| text.contains("copy · c"))
                    {
                        if let (Some(markdown), blocks) = (
                            transcript
                                .assistant_markdown_at_line(line, surface.screen_size().0)
                                .map(str::to_string),
                            transcript.assistant_code_blocks_at_line(line, surface.screen_size().0),
                        ) {
                            if session.proj.settings.always_copy_full_response {
                                session.notice(transcript, clipboard_copy_notice(&markdown));
                            } else {
                                session.copy_picker = Some(CopyPickerState {
                                    markdown,
                                    code_blocks: blocks,
                                    writing_block: false,
                                });
                                session.notice(transcript, "Copy picker: f full response · 1–9 copy block · w then number writes block · Esc cancel.");
                            }
                        }
                        requester.schedule_immediate();
                        return;
                    }
                    let now = Instant::now();
                    let clicks = session
                        .last_selection_click
                        .filter(|(old_line, old_column, at, _)| {
                            *old_line == line
                                && *old_column == column
                                && now.saturating_duration_since(*at) <= Duration::from_millis(450)
                        })
                        .map_or(1, |(_, _, _, count)| count % 3 + 1);
                    session.last_selection_click = Some((line, column, now, clicks));
                    match clicks {
                        2 => {
                            let (from, to) = word_selection(&session.selection_lines[line], column);
                            session.selection_anchor = Some((line, from));
                            session.selection_extent = Some((line, to));
                        }
                        3 => {
                            session.selection_anchor = Some((line, 0));
                            session.selection_extent =
                                Some((line, session.selection_lines[line].chars().count()));
                        }
                        _ => {
                            session.selection_anchor = Some((line, column));
                            session.selection_extent = Some((line, column));
                        }
                    }
                }
                requester.schedule_immediate();
                return;
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(point) = selection_point(session, mouse.column, mouse.row) {
                    session.selection_extent = Some(point);
                    requester.schedule_immediate();
                }
                return;
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let word_or_line_click =
                    session
                        .last_selection_click
                        .is_some_and(|(_, _, at, count)| {
                            count > 1
                                && Instant::now().saturating_duration_since(at)
                                    <= Duration::from_millis(450)
                        });
                if let Some(point) = (!word_or_line_click)
                    .then(|| selection_point(session, mouse.column, mouse.row))
                    .flatten()
                {
                    session.selection_extent = Some(point);
                }
                if selection_text(session).is_some() {
                    if let Some(text) = session
                        .proj
                        .settings
                        .copy_on_select
                        .then(|| selection_text(session))
                        .flatten()
                    {
                        session.notice(transcript, clipboard_copy_notice(&text));
                    }
                }
                requester.schedule_immediate();
                return;
            }
            _ => {}
        }
    }
    if surface.modal() == Some(ModalKind::Fullscreen)
        && matches!(event, TuiEvent::Mouse(mouse) if mouse.kind == ct_event::MouseEventKind::Down(ct_event::MouseButton::Left)
            && mouse.row >= surface.viewport().bottom_exclusive().saturating_sub(1))
        && !session.subagents.is_empty()
    {
        session.subagents_focused = true;
        session.subagents.handle(&TuiEvent::Focus(true));
        let session_id = host
            .current_session()
            .unwrap_or_else(|| session.active_session.clone());
        session
            .subagents
            .set_running(host.running_work(&session_id).await);
        requester.schedule_immediate();
        return;
    }
    if let Some(picker) = session.session_action_picker.as_mut() {
        let outcome = picker.handle(event);
        let accepted = picker
            .drain_actions()
            .into_iter()
            .find_map(|action| match action {
                rightkit_terminal_ui::nav::PickerAction::Accepted { index, .. } => picker
                    .filtered_idx(index)
                    .and_then(|original| session.session_action_kinds.get(original).copied()),
                _ => None,
            });
        let closed = picker.is_done();
        if let Some(action) = accepted {
            if action == "delete" {
                let items = vec![
                    rightkit_terminal_ui::nav::SelectionItem::new("Delete session")
                        .description("permanently removes selected chat"),
                    rightkit_terminal_ui::nav::SelectionItem::new("Cancel"),
                ];
                session.session_action_picker = Some(rightkit_terminal_ui::nav::ListPicker::new(
                    rightkit_terminal_ui::nav::PickerParams::picker(9502, "Confirm delete", items),
                ));
                session.session_action_kinds = vec!["delete-confirm", "cancel"];
            } else {
                session.session_action_picker = None;
                match action {
                    "rename" => {
                        session.pending_rename = session.session_action_id.clone();
                        // The session picker must release input along with
                        // the alternate screen, or it eats the title's keys.
                        nav.close_picker();
                        composer.set_text("");
                        composer.set_placeholder_override(Some(
                            "Type new session title and press Enter".into(),
                        ));
                        surface.leave_modal();
                    }
                    "delete-confirm" | "cancel" => {
                        if action == "delete-confirm" {
                            if let Some(id) = session.session_action_id.as_deref() {
                                match host.session_action("delete", id, None).await {
                                    Ok(line) => session.notice(transcript, line),
                                    Err(error) => session.notice(transcript, error),
                                }
                                host.refresh_nav(nav, turn.is_some()).await;
                            }
                        }
                        session.session_action_id = None;
                        session.session_action_kinds.clear();
                    }
                    "archive" | "clone" | "export" => {
                        if let Some(id) = session.session_action_id.as_deref() {
                            match host.session_action(action, id, None).await {
                                Ok(line) => session.notice(transcript, line),
                                Err(error) => session.notice(transcript, error),
                            }
                            host.refresh_nav(nav, turn.is_some()).await;
                        }
                        session.session_action_id = None;
                        session.session_action_kinds.clear();
                    }
                    _ => {}
                }
            }
        } else if closed {
            session.session_action_picker = None;
            session.session_action_id = None;
            session.session_action_kinds.clear();
        }
        if !matches!(outcome, Outcome::Ignored) || closed {
            requester.schedule_immediate();
        }
        flush_history(transcript, surface, requester);
        return;
    }
    if let TuiEvent::Key(key) = event {
        if key.code == KeyCode::Char('m')
            && key.modifiers == KeyModifiers::NONE
            && key.kind == KeyEventKind::Press
            && surface.mode() == ScreenMode::Alternate
            && nav.sessions_picker_open()
            && !nav.session_filter_active()
        {
            if let Some(selected) = nav.selected_session().cloned() {
                session.session_action_id = Some(selected.id.0.clone());
                let labels = ["Rename", "Clone", "Export", "Archive", "Delete"];
                let items = labels
                    .iter()
                    .map(|label| rightkit_terminal_ui::nav::SelectionItem::new(*label))
                    .collect();
                session.session_action_kinds =
                    vec!["rename", "clone", "export", "archive", "delete"];
                session.session_action_picker = Some(rightkit_terminal_ui::nav::ListPicker::new(
                    rightkit_terminal_ui::nav::PickerParams::picker(
                        9501,
                        format!("Session actions · {}", selected.name),
                        items,
                    ),
                ));
                requester.schedule_immediate();
            } else {
                session.notice(transcript, "No chat is selected.");
            }
            return;
        }
        if let Some(inspection) = session.inspection.as_mut() {
            let outcome = inspection.handle(event);
            if inspection.is_closed() {
                session.inspection = None;
                surface.leave_modal();
            }
            if !matches!(outcome, Outcome::Ignored) {
                requester.schedule_immediate();
            }
            return;
        }
        if key.code == KeyCode::Char('t')
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && key.kind == KeyEventKind::Press
            && surface.mode() == ScreenMode::Inline
            && session.subagents.is_empty()
            && session.pending_question.is_none()
            && session.pending_plan.is_none()
        {
            session.inspection = Some(crate::tui_inspection::InspectionState::transcript(
                transcript,
                surface.screen_size().0,
                paging.cursor,
                paging.exhausted,
            ));
            surface.enter_modal(ModalKind::Transcript);
            requester.schedule_immediate();
            return;
        }
        if ((key.modifiers == KeyModifiers::ALT
            && matches!(
                key.code,
                KeyCode::Char('o' | 'b' | 'd' | ']' | '[' | 'a' | 'r')
            ))
            || session.panes.confirming_revert())
            && session.pending_plan.is_none()
            && session.pending_question.is_none()
        {
            let cwd = host
                .active_directory()
                .map(std::path::PathBuf::from)
                .unwrap_or_default();
            if let Some(action) = session.panes.handle_key(key, &cwd) {
                match action {
                    crate::tui_panes::PaneAction::Show(lines) => {
                        transcript.push_cell(
                            Box::new(rightkit_terminal_ui::render::cell::PlainCell::new(lines)),
                            &session.theme,
                        );
                        flush_history(transcript, surface, requester);
                    }
                    crate::tui_panes::PaneAction::Notice(text) => {
                        session.notice(transcript, text);
                    }
                }
                requester.schedule_immediate();
                return;
            }
        }
        if key.code == KeyCode::Char('p')
            && key.modifiers == KeyModifiers::ALT
            && key.kind == KeyEventKind::Press
            && session.pending_plan.is_none()
            && session.pending_question.is_none()
        {
            let lines = session.todo_overlay.lines();
            if lines.is_empty() {
                session.notice(transcript, "No plan yet.");
            } else {
                transcript.push_cell(
                    Box::new(rightkit_terminal_ui::render::cell::PlainCell::new(lines)),
                    &session.theme,
                );
                flush_history(transcript, surface, requester);
            }
            return;
        }
        if key.code == KeyCode::Char('o')
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && key.kind == KeyEventKind::Press
            && session.pending_plan.is_none()
            && session.pending_question.is_none()
        {
            match session.expandable.take() {
                Some(lines) if !lines.is_empty() => {
                    transcript.push_cell(
                        Box::new(rightkit_terminal_ui::render::cell::PlainCell::new(lines)),
                        &session.theme,
                    );
                    flush_history(transcript, surface, requester);
                }
                _ => session.notice(transcript, "Nothing to expand."),
            }
            return;
        }
        if is_ctrl_c(key) && turn.is_some() {
            if let Err(error) = cancel_active_turn(host, turn).await {
                session.notice(transcript, error);
                flush_history(transcript, surface, requester);
                return;
            }
            composer.set_streaming(false);
            session.working_started = None;
            session.working_signal = None;
            session.clear_interaction();
            session.approval_input_safe = true;
            session.notice(transcript, "Turn cancelled (Ctrl-C).");
            flush_history(transcript, surface, requester);
            return;
        }

        // Esc on an idle composer dismisses a failure held on the working row.
        if key.code == KeyCode::Esc
            && key.kind == KeyEventKind::Press
            && turn.is_none()
            && session.failed_hold.is_some()
            && composer.is_empty()
        {
            session.failed_hold = None;
            requester.schedule_immediate();
            return;
        }

        // Interaction gates own every key, including keys that arrive before
        // the input quarantine has completed. No answers reach the composer.
        if session.pending_plan.is_some() || session.pending_question.is_some() {
            if !session.approval_input_safe || key.kind != KeyEventKind::Press {
                return;
            }
            if key.code == KeyCode::Esc
                && (session.pending_plan.is_some() || session.pending_question.is_none())
            {
                match cancel_active_turn(host, turn).await {
                    Ok(()) => {
                        composer.set_streaming(false);
                        session.working_started = None;
                        session.working_signal = None;
                        session.clear_interaction();
                        session.notice(transcript, "Turn cancelled (Esc).");
                    }
                    Err(error) => session.notice(transcript, error),
                }
            } else if key.code == KeyCode::Esc {
                // Esc on a question declines it: the model is told the user
                // declined and the turn keeps going (Ctrl-C still cancels).
                if let Some(question_id) = session
                    .pending_question
                    .as_ref()
                    .map(|pending| pending.question_id.clone())
                {
                    let declined = serde_json::json!({ "__declined": true });
                    match host.answer_question(&question_id, declined).await {
                        Ok(()) => {
                            session.record(
                                transcript,
                                DaemonEvent::UserQuestionAnswered { question_id },
                            );
                            session.notice(transcript, "Question declined.");
                        }
                        Err(error) => session.notice(transcript, error),
                    }
                }
            } else if let Some(pending) = session.pending_plan.as_mut() {
                match key.code {
                    KeyCode::Char('y' | 'Y') => pending.decision = Some(true),
                    KeyCode::Char('n' | 'N') => pending.decision = Some(false),
                    KeyCode::Enter => {
                        if let Some(approved) = pending.decision {
                            let plan_id = pending.plan_id.clone();
                            match host.resolve_plan(&plan_id, approved).await {
                                Ok(()) => session.record(
                                    transcript,
                                    DaemonEvent::PlanResolved { plan_id, approved },
                                ),
                                Err(error) => session.notice(transcript, error),
                            }
                        }
                    }
                    _ => {}
                }
            } else if let Some(pending) = session.pending_question.as_mut() {
                let current_prompt = pending.prompts.get(pending.current).cloned();
                let options_len = current_prompt.as_ref().map_or(0, |p| p.options.len());
                let multi = current_prompt.as_ref().is_some_and(|p| p.multi_select);
                let plain = !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
                let mut commit: Option<serde_json::Value> = None;
                let mut try_submit = false;
                match key.code {
                    KeyCode::Up => pending.selected = pending.selected.saturating_sub(1),
                    KeyCode::Down => pending.selected = (pending.selected + 1).min(options_len),
                    KeyCode::Char(' ')
                        if plain
                            && multi
                            && pending.answer_text.is_empty()
                            && pending.selected < options_len =>
                    {
                        let index = pending.selected;
                        if let Some(at) = pending.toggled.iter().position(|i| *i == index) {
                            pending.toggled.remove(at);
                        } else {
                            pending.toggled.push(index);
                        }
                    }
                    KeyCode::Char(digit @ '1'..='9')
                        if plain
                            && pending.answer_text.is_empty()
                            && (digit as usize - '0' as usize) <= options_len =>
                    {
                        let index = digit as usize - '0' as usize - 1;
                        if multi {
                            pending.selected = index;
                            if let Some(at) = pending.toggled.iter().position(|i| *i == index) {
                                pending.toggled.remove(at);
                            } else {
                                pending.toggled.push(index);
                            }
                        } else if let Some(prompt) = &current_prompt {
                            commit = Some(serde_json::Value::String(prompt.options[index].clone()));
                        }
                    }
                    KeyCode::Char(character) if plain && pending.answer_text.len() < 8192 => {
                        pending.answer_text.push(character);
                        pending.selected = options_len;
                    }
                    KeyCode::Backspace => {
                        pending.answer_text.pop();
                    }
                    KeyCode::Enter => {
                        let text = pending.answer_text.trim().to_string();
                        if let Some(prompt) = &current_prompt {
                            if !text.is_empty() {
                                let indices: Option<Vec<usize>> = text
                                    .split(',')
                                    .map(|part| {
                                        part.trim().parse::<usize>().ok().filter(|index| {
                                            *index > 0 && *index <= prompt.options.len()
                                        })
                                    })
                                    .collect();
                                commit = Some(match indices {
                                    Some(indices) if prompt.multi_select => {
                                        let mut selected = Vec::new();
                                        for index in indices {
                                            let value = serde_json::Value::String(
                                                prompt.options[index - 1].clone(),
                                            );
                                            if !selected.contains(&value) {
                                                selected.push(value);
                                            }
                                        }
                                        serde_json::Value::Array(selected)
                                    }
                                    Some(indices) if indices.len() == 1 => {
                                        serde_json::Value::String(
                                            prompt.options[indices[0] - 1].clone(),
                                        )
                                    }
                                    _ => serde_json::Value::String(text),
                                });
                            } else if multi {
                                let mut picked = pending.toggled.clone();
                                if picked.is_empty() && pending.selected < options_len {
                                    picked.push(pending.selected);
                                }
                                picked.sort_unstable();
                                if !picked.is_empty() {
                                    commit = Some(serde_json::Value::Array(
                                        picked
                                            .into_iter()
                                            .map(|i| {
                                                serde_json::Value::String(prompt.options[i].clone())
                                            })
                                            .collect(),
                                    ));
                                }
                            } else if pending.selected < options_len {
                                commit = Some(serde_json::Value::String(
                                    prompt.options[pending.selected].clone(),
                                ));
                            }
                        } else {
                            // Every prompt answered but the submission failed: retry.
                            try_submit = true;
                        }
                    }
                    _ => {}
                }
                if let Some(answer) = commit {
                    if let Some(prompt) = &current_prompt {
                        pending.answers.insert(prompt.header.clone(), answer);
                        pending.current += 1;
                        pending.answer_text.clear();
                        pending.selected = 0;
                        pending.toggled.clear();
                        try_submit = true;
                    }
                }
                if try_submit && pending.current >= pending.prompts.len() {
                    let question_id = pending.question_id.clone();
                    let answers = serde_json::Value::Object(pending.answers.clone());
                    match host.answer_question(&question_id, answers).await {
                        Ok(()) => session.record(
                            transcript,
                            DaemonEvent::UserQuestionAnswered { question_id },
                        ),
                        Err(error) => session.notice(transcript, error),
                    }
                }
            }
            requester.schedule_immediate();
            flush_history(transcript, surface, requester);
            return;
        }

        // Stage-3 approval modal: while a request is on screen AND the
        // surface is inline (the overlay only draws there), it owns EVERY
        // key — nothing leaks to the composer, and the overlay's own
        // arming gate swallows input that arrived before its first draw
        // (the quarantine drain empties the pre-modal buffer; this gate
        // covers whatever races in after it). On the alternate screen a
        // nav picker owns the keys and requests stay pending — safe.
        // Ctrl-Y/Ctrl-N are translated to the overlay's option digits so
        // the documented bindings stay live without bypassing that gate.
        if surface.mode() == ScreenMode::Inline
            && session.approval_overlay.current_request().is_some()
        {
            if !session.approval_input_safe {
                return;
            }
            let forwarded = session
                .approval_overlay
                .current_request()
                .map(|request| approval_overlay_key(key, request))
                .unwrap_or(TuiEvent::Key(*key));
            let outcome = session.approval_overlay.handle(&forwarded);
            let mut rejected_any = false;
            for intent in session.approval_overlay.drain_intents() {
                match intent {
                    ApprovalIntent::Resolve {
                        session_id,
                        request_id,
                        decision,
                    } => {
                        let review_decision = match decision {
                            ApprovalDecision::Approved => {
                                rightkit_agent_engine_protocol::ReviewDecision::Approved
                            }
                            ApprovalDecision::ApprovedForSession => {
                                rightkit_agent_engine_protocol::ReviewDecision::ApprovedForSession
                            }
                            ApprovalDecision::ApprovedForPrefix => {
                                let entry = session.approval_allow_rules.remove(&request_id);
                                let outcome = install_allow_rule(host, entry, &session_id).await;
                                if let Err(error) = outcome {
                                    session.notice(
                                        transcript,
                                        format!("Always-allow rule not installed ({error}); allowed once."),
                                    );
                                }
                                rightkit_agent_engine_protocol::ReviewDecision::Approved
                            }
                            ApprovalDecision::Denied | ApprovalDecision::Redirect => {
                                rightkit_agent_engine_protocol::ReviewDecision::Abort
                            }
                        };
                        let approved = review_decision.is_approved();
                        let for_session = review_decision
                            == rightkit_agent_engine_protocol::ReviewDecision::ApprovedForSession;
                        if for_session {
                            session.session_grants.insert(request_id.clone());
                        }
                        match host
                            .resolve_approval(&session_id, &request_id, review_decision.clone())
                            .await
                        {
                            Ok(()) => {
                                session.pending_approvals.retain(|(owner, id)| {
                                    !(owner == &session_id && id == &request_id)
                                });
                                session.record(
                                    transcript,
                                    DaemonEvent::ApprovalResolved {
                                        request_id,
                                        approved,
                                    },
                                );
                            }
                            Err(error) => {
                                session.session_grants.remove(&request_id);
                                session.notice(transcript, error)
                            }
                        }
                    }
                    // Esc: reject the request (denied through the same
                    // authority as option 2) and stop the turn that raised
                    // it. Queued messages stay queued; the composer gets
                    // focus with a "tell the model" placeholder.
                    ApprovalIntent::Rejected {
                        session_id,
                        request_id,
                    } => {
                        session.approval_allow_rules.remove(&request_id);
                        match host
                            .resolve_approval(
                                &session_id,
                                &request_id,
                                rightkit_agent_engine_protocol::ReviewDecision::Abort,
                            )
                            .await
                        {
                            Ok(()) => {
                                session.pending_approvals.retain(|(owner, id)| {
                                    !(owner == &session_id && id == &request_id)
                                });
                                session.record(
                                    transcript,
                                    DaemonEvent::ApprovalResolved {
                                        request_id,
                                        approved: false,
                                    },
                                );
                            }
                            Err(error) => session.notice(transcript, error),
                        }
                        rejected_any = true;
                    }
                }
            }
            if rejected_any {
                if let Err(error) = cancel_active_turn(host, turn).await {
                    session.notice(transcript, error);
                }
                composer.set_streaming(false);
                session.working_started = None;
                session.working_signal = None;
                session.approval_input_safe = true;
                let model = footer_model(host, session);
                composer.set_placeholder_override(Some(reject_placeholder(&model)));
                // One stop notice: the rejection itself is already on the
                // ledger as `✗ rejected …`, and the runtime's own
                // `Turn interrupted` for this stop is suppressed.
                session.swallow_interrupt_notice = true;
                session.notice(transcript, "Turn stopped · request rejected");
            }
            match outcome {
                Outcome::Redraw => requester.schedule_immediate(),
                Outcome::Consumed => requester.schedule_frame(),
                Outcome::Ignored => {}
            }
            flush_history(transcript, surface, requester);
            return;
        }
    }

    // Route picker (`/model`, `/effort`): while open AND the surface is
    // inline it owns key input — on the alternate screen a nav picker
    // already holds the keys. Route selections are applied through the
    // host's route authority — the picker never writes route state itself.
    if surface.mode() == ScreenMode::Inline {
        if session.active_agent.is_some()
            && matches!(event, TuiEvent::Key(key) if key.kind == KeyEventKind::Press && key.code == KeyCode::Esc)
        {
            let from = view_key(session, viewed_agent.as_ref());
            *viewed_agent = None;
            session.active_agent = None;
            let to = view_key(session, viewed_agent.as_ref());
            switch_view(composer, &from, &to);
            session.notice(transcript, "Returned to parent chat.");
            requester.schedule_immediate();
            return;
        }
        if let TuiEvent::Key(key) = event {
            let chat_shortcuts = session.rewind.is_none()
                && session.repository_picker.is_none()
                && session.providers_picker.is_none()
                && session.model_picker.is_none();
            if chat_shortcuts
                && !session.subagents_focused
                && key.kind == KeyEventKind::Press
                && (key.modifiers == KeyModifiers::CONTROL
                    || (key.modifiers == KeyModifiers::NONE
                        && session.selected_copy_message.is_some()))
                && composer.is_empty()
                && key.code == KeyCode::Up
            {
                // CG-A05: plain Up belongs to prompt history. Ctrl+Up enters
                // assistant-copy selection; plain arrows then navigate it.
                let count = transcript.assistant_markdown_count();
                if count > 0 {
                    let newest = count - 1;
                    session.selected_copy_message = Some(
                        session
                            .selected_copy_message
                            .map_or(newest, |selected| selected.saturating_sub(1)),
                    );
                    let selected = session.selected_copy_message.unwrap_or(newest);
                    session.notice(
                        transcript,
                        format!("Assistant message {} selected · ↑ older · ↓ newer · c copy · Esc clear", selected + 1),
                    );
                    requester.schedule_immediate();
                    return;
                }
            }
            if chat_shortcuts
                && !session.subagents_focused
                && key.kind == KeyEventKind::Press
                && key.modifiers == KeyModifiers::NONE
                && composer.is_empty()
                && key.code == KeyCode::Down
                && session.selected_copy_message.is_some()
            {
                let selected = session.selected_copy_message.expect("checked above");
                let newest = transcript.assistant_markdown_count().saturating_sub(1);
                if selected >= newest {
                    session.selected_copy_message = None;
                    session.notice(transcript, "Assistant message selection cleared.");
                } else {
                    let selected = selected + 1;
                    session.selected_copy_message = Some(selected);
                    session.notice(
                        transcript,
                        format!("Assistant message {} selected · ↑ older · ↓ newer · c copy · Esc clear", selected + 1),
                    );
                }
                requester.schedule_immediate();
                return;
            }
            if chat_shortcuts
                && !session.subagents_focused
                && key.kind == KeyEventKind::Press
                && key.code == KeyCode::Esc
                && session.selected_copy_message.take().is_some()
            {
                session.notice(transcript, "Assistant message selection cleared.");
                requester.schedule_immediate();
                return;
            }
            if chat_shortcuts
                && !session.subagents_focused
                && key.kind == KeyEventKind::Press
                && key.code == KeyCode::Char('c')
                && key.modifiers == KeyModifiers::NONE
                && composer.is_empty()
            {
                if let Some(markdown) = session
                    .selected_copy_message
                    .and_then(|index| transcript.assistant_markdown_at(index))
                    .or_else(|| transcript.latest_assistant_markdown())
                {
                    let copied = markdown.to_string();
                    session.notice(transcript, clipboard_copy_notice(&copied));
                    requester.schedule_immediate();
                    return;
                }
            }
            if chat_shortcuts
                && !session.subagents_focused
                && key.kind == KeyEventKind::Press
                && (key.code == KeyCode::Left
                    || (key.code == KeyCode::Char('t')
                        && key.modifiers.contains(KeyModifiers::CONTROL)))
                && composer.is_empty()
                && !session.subagents.is_empty()
            {
                session.subagents_focused = true;
                session.subagents.handle(&TuiEvent::Focus(true));
                let session_id = host
                    .current_session()
                    .unwrap_or_else(|| session.active_session.clone());
                session
                    .subagents
                    .set_running(host.running_work(&session_id).await);
                requester.schedule_immediate();
                return;
            }
        }
        if session.subagents_focused {
            let outcome = session.subagents.handle(event);
            for intent in session.subagents.drain_intents() {
                match intent {
                    LiveSubagentsIntent::Open(id) => {
                        let from = view_key(session, viewed_agent.as_ref());
                        *viewed_agent = Some(id.clone());
                        session.active_agent = Some(id.clone());
                        let to = view_key(session, viewed_agent.as_ref());
                        switch_view(composer, &from, &to);
                        session.subagents_focused = false;
                        session.subagents.handle(&TuiEvent::Focus(false));
                        session.notice(
                            transcript,
                            format!("Chatting with child agent {id}. Esc returns to parent."),
                        );
                        // Match /agents: fetch existing history and seed the
                        // polling cursor before consuming only fresh events.
                        if let Some(excerpt) = host.fetch_agent_transcript(&id).await {
                            session.notice(transcript, excerpt);
                        }
                        flush_history(transcript, surface, requester);
                        requester.schedule_immediate();
                    }
                    LiveSubagentsIntent::ReturnToParent => {
                        let from = view_key(session, viewed_agent.as_ref());
                        *viewed_agent = None;
                        session.active_agent = None;
                        let to = view_key(session, viewed_agent.as_ref());
                        switch_view(composer, &from, &to);
                        session.subagents_focused = false;
                        session.subagents.handle(&TuiEvent::Focus(false));
                    }
                    LiveSubagentsIntent::StopAgent(id) => {
                        session.notice(
                            transcript,
                            format!("Stopping agent {id} requires confirmation."),
                        );
                    }
                    LiveSubagentsIntent::ConfirmStopAgent(id) => {
                        match host.stop_remote_subtree(std::slice::from_ref(&id)).await {
                            Ok(count) => session.notice(
                                transcript,
                                format!("Stopped {count} agent(s) in subtree {}.", id.0),
                            ),
                            Err(error) => session.notice(transcript, error),
                        }
                        host.refresh_nav(nav, turn.is_some()).await;
                    }
                    LiveSubagentsIntent::ShowShellOutput(id) => {
                        match host.running_output(&id).await {
                            Ok(output) => session.notice(transcript, output),
                            Err(error) => session.notice(transcript, error),
                        }
                    }
                    LiveSubagentsIntent::AttachShell(id) => {
                        session.subagents_focused = false;
                        session.subagents.handle(&TuiEvent::Focus(false));
                        run_pty_command(
                            host,
                            rightkit_terminal_ui::pty_attach::PtyCommand::Attach(id),
                            composer,
                            session,
                            transcript,
                            surface,
                            requester,
                        )
                        .await;
                    }
                    LiveSubagentsIntent::StopShell(id) => {
                        let session_id = host
                            .current_session()
                            .unwrap_or_else(|| session.active_session.clone());
                        match host.stop_running_work(&session_id, "shell", &id).await {
                            Ok(_) => {}
                            Err(error) => session.notice(transcript, error),
                        }
                        session
                            .subagents
                            .set_running(host.running_work(&session_id).await);
                    }
                    LiveSubagentsIntent::StopTask(id) => {
                        let session_id = host
                            .current_session()
                            .unwrap_or_else(|| session.active_session.clone());
                        match host.stop_running_work(&session_id, "task", &id).await {
                            Ok(_) => {}
                            Err(error) => session.notice(transcript, error),
                        }
                        session
                            .subagents
                            .set_running(host.running_work(&session_id).await);
                    }
                }
            }
            if session.subagents_focused && !matches!(outcome, Outcome::Ignored) {
                requester.schedule_immediate();
            }
            if session.subagents_focused || matches!(event, TuiEvent::Key(_)) {
                return;
            }
        }
        if session.proj.picker.is_some() {
            if handle_action_picker(
                host,
                event,
                composer,
                nav,
                session,
                transcript,
                surface,
                interjections,
                turn.is_some(),
                viewed_agent,
                paging,
                requester,
            )
            .await
            {
                flush_history(transcript, surface, requester);
                return;
            }
        } else if try_open_overlay_chord(host, event, session, requester).await {
            return;
        }
        if session.rewind.is_some() {
            rewind::handle_key(host, event, composer, session, transcript, requester).await;
            return;
        }
        if let Some(picker) = session.repository_picker.as_mut() {
            let outcome = picker.handle(event);
            let selected = picker
                .drain_actions()
                .into_iter()
                .find_map(|action| match action {
                    rightkit_terminal_ui::nav::PickerAction::Accepted { index, .. } => picker
                        .filtered_idx(index)
                        .and_then(|index| session.repository_choices.get(index))
                        .cloned(),
                    _ => None,
                });
            if picker.is_done() {
                session.repository_picker = None;
            }
            if let Some(root) = selected {
                match host.select_repository(&root).await {
                    Ok(()) => {
                        rebind_view_to_active(
                            host,
                            composer,
                            nav,
                            session,
                            transcript,
                            interjections,
                            viewed_agent,
                            paging,
                            format!("Opened project {root} · new chat."),
                        )
                        .await;
                    }
                    Err(error) => session.notice(transcript, error),
                }
            }
            if outcome.is_terminal() {
                requester.schedule_immediate();
                return;
            }
        }
        if let Some(picker) = session.providers_picker.as_mut() {
            // `/providers`: checkbox toggles persist through the host's
            // visibility authority as they happen — there is no commit
            // step, closing never commits more.
            let outcome = picker.handle(event);
            let intents = picker.drain_intents();
            let done = picker.is_done();
            if done {
                session.providers_picker = None;
            }
            for intent in intents {
                match intent {
                    NavIntent::ToggleModelVisibility {
                        provider,
                        model,
                        hidden,
                    } => {
                        if let Err(error) =
                            host.set_model_visibility(&provider, &model, hidden).await
                        {
                            session.notice(transcript, error);
                        }
                    }
                    NavIntent::ProviderAction {
                        provider,
                        action,
                        secret,
                    } => {
                        let message = match host.provider_action(&provider, action, secret).await {
                            Ok(message) => message,
                            Err(error) => error,
                        };
                        // Fresh status (key set, signed in, ...) back into
                        // the open screen, with the result line.
                        let fresh: Vec<_> = host
                            .provider_catalog()
                            .await
                            .into_iter()
                            .filter(|p| {
                                crate::main_runtime::provider_setup::screen_provider(&p.id)
                                    .is_some()
                            })
                            .collect();
                        if let Some(picker) = session.providers_picker.as_mut() {
                            picker.update_providers(fresh, Some(message));
                        }
                    }
                    _ => {}
                }
            }
            match outcome {
                Outcome::Redraw => requester.schedule_immediate(),
                Outcome::Consumed => requester.schedule_frame(),
                Outcome::Ignored => {}
            }
            if !matches!(outcome, Outcome::Ignored) {
                flush_history(transcript, surface, requester);
                return;
            }
        }
        if let Some(picker) = session.model_picker.as_mut() {
            let outcome = picker.handle(event);
            let intents = picker.drain_intents();
            let done = picker.is_done();
            if done {
                session.model_picker = None;
            }
            let selected_model = intents.iter().find_map(|intent| match intent {
                NavIntent::SelectModel(model) => Some(model.as_str()),
                _ => None,
            });
            let selected_effort = intents.iter().find_map(|intent| match intent {
                NavIntent::SelectEffort(effort) => Some(effort.as_str()),
                _ => None,
            });
            if let Some(model) = selected_model {
                match host.select_route(model, selected_effort).await {
                    Ok(()) => session.notice(transcript, format!("Route selected: {model}.")),
                    Err(error) => session.notice(transcript, error),
                }
            } else {
                for intent in intents {
                    apply_route_intent(host, session, transcript, intent).await;
                }
            }
            match outcome {
                Outcome::Redraw => requester.schedule_immediate(),
                Outcome::Consumed => requester.schedule_frame(),
                Outcome::Ignored => {}
            }
            if !matches!(outcome, Outcome::Ignored) {
                flush_history(transcript, surface, requester);
                return;
            }
        }
    }

    // #227: a Ctrl+V / empty paste with an image on the clipboard attaches it
    // as an `@path` mention the submit path turns into an image block.
    let image_paste;
    let event = if crate::tui_image_input::is_image_paste_gesture(event) {
        match crate::tui_image_input::capture_clipboard_image() {
            Some(path) => {
                image_paste = TuiEvent::Paste(crate::tui_image_input::mention_for(&path));
                &image_paste
            }
            None => event,
        }
    } else {
        event
    };

    let outcome = {
        let mut components: [&mut dyn Component; 2] = [nav, composer];
        dispatch(&mut components, event)
    };

    match outcome {
        Outcome::Redraw => requester.schedule_immediate(),
        Outcome::Consumed => requester.schedule_frame(),
        Outcome::Ignored => {}
    }

    // S:I10 — the composer re-syncs its own popup after every key; the host
    // re-syncs here too so an edit that arrives by any other path (a paste,
    // a host-driven `set_text`) cannot leave the popup stale.
    composer.sync_completion();

    for nav_intent in nav.drain_intents() {
        match nav_intent {
            NavIntent::OpenSessions => {
                host.refresh_nav(nav, turn.is_some()).await;
                surface.enter_modal(ModalKind::SessionPicker);
                nav.open_sessions();
                // `open_sessions` re-emits the very intent being handled here;
                // drop the echo so it cannot loop back through this arm on the
                // next event.
                nav.drain_intents();
            }
            NavIntent::OpenAgents => {
                host.refresh_nav(nav, turn.is_some()).await;
                surface.enter_modal(ModalKind::AgentPicker);
                nav.open_agents();
                nav.drain_intents();
            }
            NavIntent::Ascend => {
                if surface.mode() == ScreenMode::Alternate {
                    surface.leave_modal();
                }
                // S:I7 — leaving the agent view is a view switch: stash the
                // composer text for the view being left, restore the one for
                // the view being returned to (a no-op when nothing was
                // descended into, i.e. a picker simply closing).
                let from = view_key(session, viewed_agent.as_ref());
                // P1-5: leaving the agent view stops its live transcript
                // polling; the view itself stays in scrollback history.
                *viewed_agent = None;
                let to = view_key(session, viewed_agent.as_ref());
                switch_view(composer, &from, &to);
            }
            NavIntent::Descend(id) => {
                if id.0 == LOCAL_AGENT_ID {
                    // The local root's turns stream into this very view, so
                    // there is nothing remote to poll — but the breadcrumb
                    // still names it and the notice still fires.
                    *viewed_agent = None;
                    session.notice(
                        transcript,
                        "Already viewing this session's root agent -- it has no subagents.",
                    );
                } else {
                    // P1-5: a daemon-backed host fetches the remote agent's
                    // transcript for read-only viewing AND seeds the per-agent
                    // cursor that the maintenance tick polls from; navigation
                    // never takes execution ownership of the agent (AJ).
                    match host.fetch_agent_transcript(&id).await {
                        Some(excerpt) => {
                            // S:I7 — same switch on the way in.
                            let from = view_key(session, viewed_agent.as_ref());
                            *viewed_agent = Some(id.clone());
                            let to = view_key(session, viewed_agent.as_ref());
                            switch_view(composer, &from, &to);
                            session.notice(transcript, format!("Viewing agent {id} (read-only):"));
                            session
                                .record(transcript, DaemonEvent::AssistantDelta { token: excerpt });
                            session.record(transcript, DaemonEvent::AssistantSettled);
                        }
                        None => {
                            session.notice(
                                transcript,
                                format!(
                                    "Cannot view agent {id}: no daemon feed is connected, so only this session's local agent tree exists."
                                ),
                            );
                        }
                    }
                }
            }
            NavIntent::SwitchSession(id) => {
                switch_to_session(
                    host,
                    id,
                    composer,
                    nav,
                    session,
                    transcript,
                    surface,
                    turn.is_some(),
                    viewed_agent,
                    paging,
                )
                .await;
            }
            NavIntent::Stop(id) => {
                // Scoped stop: the only agent whose lifecycle this host owns is
                // the local root. Settle its engine before discarding its
                // presentation handle. Any other id resolves to its subtree scope
                // (via the nav model) and each id in that scope is interrupted
                // through the daemon — recursion is the daemon's, but the scope
                // must be the subtree, never "everything".
                if id.0 == LOCAL_AGENT_ID {
                    if turn.is_some() {
                        if let Err(error) = cancel_active_turn(host, turn).await {
                            session.notice(transcript, error);
                            continue;
                        }
                        composer.set_streaming(false);
                        session.working_started = None;
                        session.working_signal = None;
                        // Aborting skips the `wait_turn` branch, so queued
                        // interjections would silently never run; report them
                        // dropped, same as an explicit cancel.
                        let dropped = interjections
                            .drain()
                            .iter()
                            .filter(|d| matches!(d, Directive::Steer(_) | Directive::Reprompt(_)))
                            .count();
                        session.notice(transcript, if dropped == 0 {
                            "Stopped the running turn (scoped: this session's root agent only).".to_string()
                        } else if dropped == 1 {
                            "Stopped the running turn. 1 queued message was discarded.".to_string()
                        } else {
                            format!("Stopped the running turn. {dropped} queued messages were discarded.")
                        });
                    } else {
                        session.notice(
                            transcript,
                            "The root agent has no turn in flight; nothing to stop.",
                        );
                    }
                } else {
                    // Resolve the exact subtree scope from the loaded tree, so a
                    // stop of a sub-agent also interrupts its descendants
                    // (invariant AL: that subtree only, never parent/siblings).
                    let scope = nav.stop_scope(&id);
                    let scope_text = scope
                        .iter()
                        .map(|agent| agent.0.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    match host.stop_remote_subtree(&scope).await {
                        Ok(stopped) => {
                            session.notice(
                                transcript,
                                format!("Stopped {stopped} agent(s) (scope: {scope_text})."),
                            );
                        }
                        Err(reason) => {
                            session.notice(transcript, reason);
                        }
                    }
                }
            }
            // Route selections emitted by nav-owned surfaces land on the
            // same host authority the model picker uses.
            NavIntent::SelectModel(_) | NavIntent::SelectEffort(_) => {
                apply_route_intent(host, session, transcript, nav_intent).await;
            }
            // A visibility toggle routed here instead of through the
            // `/providers` picker's own drain — same host authority either
            // way.
            NavIntent::ToggleModelVisibility {
                provider,
                model,
                hidden,
            } => {
                if let Err(error) = host.set_model_visibility(&provider, &model, hidden).await {
                    session.notice(transcript, error);
                }
            }
            // `/providers` actions are drained by the picker's own handler
            // above; nav never emits one.
            NavIntent::ProviderAction { .. } => {}
            // Picker `r`: same title prompt the `m` -> Rename action opens.
            NavIntent::RenameSession(id) => {
                session.pending_rename = Some(id.0);
                nav.close_picker();
                composer.set_text("");
                composer.set_placeholder_override(Some(
                    "Type new session title and press Enter".into(),
                ));
                surface.leave_modal();
            }
            // Picker `d` + `y`: confirmation already happened in the picker.
            NavIntent::DeleteSession(id) => {
                if id.0 == session.active_session {
                    session.notice(
                        transcript,
                        "Cannot delete the active session from the picker; use /delete.",
                    );
                } else {
                    match host.session_action("delete", &id.0, None).await {
                        Ok(line) => session.notice(transcript, line),
                        Err(error) => session.notice(transcript, error),
                    }
                    host.refresh_nav(nav, turn.is_some()).await;
                }
            }
        }
    }

    for intent in composer.drain_intents() {
        match intent {
            ComposerIntent::Submit(text) => {
                let session_before = host.current_session();
                if handle_submit(
                    host,
                    text,
                    composer,
                    nav,
                    session,
                    transcript,
                    surface,
                    interjections,
                    pending_echoes,
                    viewed_agent,
                    paging,
                    turn,
                    requester,
                )
                .await
                {
                    *should_exit = true;
                }
                if let Some(id) = host
                    .current_session()
                    .filter(|id| Some(id.as_str()) != session_before.as_deref())
                {
                    let _ = id;
                    rebind_view_to_active(
                        host,
                        composer,
                        nav,
                        session,
                        transcript,
                        interjections,
                        viewed_agent,
                        paging,
                        "Switched to selected workspace.".into(),
                    )
                    .await;
                }
            }
            ComposerIntent::CommandEffect(effect) => {
                // S:I5 — the six truthfully-distinct session effects. The
                // composer resolved the typed verb (name or alias) before it
                // got here; `quit`/`exit`/`q` are deliberately NOT in this
                // registry and still arrive as `ComposerIntent::Quit`.
                apply_command_effect(
                    host,
                    effect,
                    composer,
                    nav,
                    session,
                    transcript,
                    interjections,
                    turn,
                    requester,
                    viewed_agent,
                    paging,
                )
                .await;
            }
            ComposerIntent::Quit => *should_exit = true,
            ComposerIntent::CyclePermissionMode => {
                let next = host.permission_mode().next_in_cycle();
                host.set_permission_mode(next);
                // The footer shows the mode; the transcript note says when it
                // takes effect so a mid-turn Shift+Tab is not misread.
                if turn.is_some() {
                    session.notice(
                        transcript,
                        format!(
                            "Permission mode: {} (applies from the next turn).",
                            next.display_label()
                        ),
                    );
                }
            }
            ComposerIntent::CancelStreamingTurn => {
                if let Err(error) = cancel_active_turn(host, turn).await {
                    session.notice(transcript, error);
                    continue;
                }
                composer.set_streaming(false);
                session.working_started = None;
                session.working_signal = None;
                // Cancel stops the turn, then every queued message is sent
                // at once, oldest first, as the next turn.
                match send_queued(
                    host,
                    interjections,
                    session,
                    transcript,
                    pending_echoes,
                    composer,
                ) {
                    Some(handle) => {
                        session.notice(transcript, "Turn cancelled. Sending the queued messages.");
                        *turn = Some(handle);
                    }
                    None => session.notice(transcript, "Turn cancelled."),
                }
                // The stop is ours: show it once, instead of also printing the
                // runtime's matching `Turn interrupted` (set last: recording a
                // user message clears the flag).
                session.swallow_interrupt_notice = true;
            }
            ComposerIntent::Push(text) => {
                if turn.is_none() || text.trim_start().starts_with('/') {
                    let session_before = host.current_session();
                    if handle_submit(
                        host,
                        text,
                        composer,
                        nav,
                        session,
                        transcript,
                        surface,
                        interjections,
                        pending_echoes,
                        viewed_agent,
                        paging,
                        turn,
                        requester,
                    )
                    .await
                    {
                        *should_exit = true;
                    }
                    if let Some(id) = host
                        .current_session()
                        .filter(|id| Some(id.as_str()) != session_before.as_deref())
                    {
                        let _ = id;
                        rebind_view_to_active(
                            host,
                            composer,
                            nav,
                            session,
                            transcript,
                            interjections,
                            viewed_agent,
                            paging,
                            "Switched to selected workspace.".into(),
                        )
                        .await;
                    }
                } else {
                    let framed = InterjectionBuffer::frame_steer(&text);
                    match host.steer_turn(&framed).await {
                        Ok(()) => {
                            // A steer is injected into the running turn; the
                            // runtime records no durable `userMessageSubmitted`
                            // for it, so no echo is owed (a stale entry would
                            // block every later echo from being matched).
                            session.record(
                                transcript,
                                DaemonEvent::UserMessage { text: text.clone() },
                            );
                            session.notice(transcript, "Pushed into the running turn.");
                        }
                        Err(_) => {
                            // No steer seam: keep the message, as a queued one.
                            interjections.push(text, InterjectKind::Steer);
                            session
                                .notice(transcript, "Cannot push into this turn; queued instead.");
                        }
                    }
                }
            }
            ComposerIntent::EditLastQueued => {
                if let Some(text) = interjections.pop_last() {
                    composer.set_text(&text);
                }
            }
            ComposerIntent::RemoveLastQueued => {
                if interjections.pop_last().is_some() {
                    session.notice(transcript, "Removed the last queued message.");
                }
            }
            ComposerIntent::AbortRunningShell => {
                // `!cmd` runs in the background; Esc / Ctrl+C kills it.
                if rightkit_terminal_ui::user_shell::cancel_active() {
                    session.notice(transcript, "Stopping shell command...");
                }
            }
            ComposerIntent::ExitShellInputMode => {}
            ComposerIntent::OpenExternalEditor(text) => {
                run_external_editor(composer, session, transcript, surface, requester, &text);
            }
            ComposerIntent::OpenPicker => {
                // D3: Esc-Esc on an idle empty composer opens the rewind
                // message picker (the session list stays on /resume).
                rewind::open(host, session, transcript, requester).await;
            }
            ComposerIntent::Copy(text) => {
                session.notice(transcript, clipboard_copy_notice(&text));
            }
        }
    }
    composer.set_queued_count(interjections.len());
    session.queued_preview = interjections.texts();

    // `@` file search: the composer queued its requests during the handle +
    // intent processing above; answer the latest one now that every text
    // mutation for this event has settled.

    // S:I6 — a turn that JUST started is the moment the model/actor serving
    // it becomes knowable, so refresh the feed then and record the route
    // decision: that is what the identity strip attributes this turn to (and
    // what marks a boundary if the model changed since the last one).
    if !turn_was_in_flight && turn.is_some() {
        // A new turn may switch model/provider before its daemon route event
        // reaches this host. Permit the refreshed nav snapshot to seed that
        // transition; any subsequent RouteDecided event becomes authoritative
        // again through SessionState::record.
        session.authoritative_route = false;
        host.refresh_nav(nav, turn.is_some()).await;
    }
    sync_route(nav, session, transcript);
    // S:I10 — turn state may have changed above; the popup's disabled
    // reasons have to match what the effects will actually do now.
    refresh_completion_catalog(composer, session, turn.is_some());

    flush_history(transcript, surface, requester);
    requester.schedule_frame();
}

/// S:I5 — carry out one of the six truthfully-distinct command effects.
///
/// Each arm changes a *different* piece of session state and reports exactly
/// what it did, so no two of them can collapse into the same behaviour:
///
/// - `New`    — a new daemon session (refused while a turn is in flight); also `/clear`, `/cls`.
/// - `Reset`  — turn state: in-flight turn, queued directives, unsent text.
/// - `Undo` / `Redo` — the composer's edit ledger, the only reversible
///   ledger this host can reach (Ctrl+Z's).
/// - `Rewind` — refused outright: the durable turn ledger is readable from
///   here (`TurnHost::fetch_older_history_page`) but not truncatable, so
///   there is nothing honest to do. Says so instead of clearing the view
///   and calling that a rewind.
#[allow(clippy::too_many_arguments)]
async fn apply_command_effect(
    host: &impl TurnHost,
    effect: CommandEffect,
    composer: &mut Composer,
    nav: &mut NavView,
    session: &mut SessionState,
    transcript: &mut Transcript,
    interjections: &InterjectionBuffer,
    turn: &mut Option<JoinHandle<i32>>,
    requester: &FrameRequester,
    viewed_agent: &mut Option<AgentId>,
    paging: &mut HistoryPaging,
) {
    match effect {
        CommandEffect::New => {
            if turn.is_some() {
                session.notice(
                    transcript,
                    "Cannot start a new session while a turn is in flight; cancel it first. Nothing was changed.",
                );
            } else {
                match host.new_session().await {
                    Ok(id) => {
                        // A brand-new session: the old one keeps running, but
                        // this view belongs to the new one, so every piece of
                        // per-session state starts over.
                        let from = view_key(session, viewed_agent.as_ref());
                        interjections.clear();
                        transcript.clear();
                        session.reset_view(&id.0);
                        *viewed_agent = None;
                        // P2-5: `/new` moves the view to a different durable
                        // ledger exactly like a session switch — the old
                        // session's paging cursor/exhaustion must not carry
                        // into the new session's first page fetch.
                        paging.rebind_session();
                        session.active_session = id.0.clone();
                        let to = view_key(session, viewed_agent.as_ref());
                        switch_view(composer, &from, &to);
                        // S:I6 — re-read the feed for the session we are now
                        // on, so the strip's attribution (recorded by
                        // `sync_route` once this returns) names the new one.
                        host.refresh_nav(nav, false).await;
                        session.notice(
                            transcript,
                            format!(
                                "Started a new session {id}. The previous session keeps running."
                            ),
                        );
                    }
                    Err(reason) => session.notice(transcript, reason),
                }
            }
        }
        CommandEffect::Reset => {
            // Turn state only: cancel anything in flight, discard the queued
            // directives and the unsent composer text. Session identity,
            // history, and the visible transcript all survive — that is what
            // makes this distinct from New (drops the view) and Rewind
            // (drops history after a boundary).
            let mut parts: Vec<String> = Vec::new();
            if turn.is_some() {
                // Settle the engine before dropping its presentation handle.
                // A refusal must leave both the handle and busy presentation
                // state intact so reset does not claim work was cancelled.
                if let Err(error) = cancel_active_turn(host, turn).await {
                    session.notice(transcript, error);
                    requester.schedule_frame();
                    return;
                }
                composer.set_streaming(false);
                session.working_started = None;
                session.working_signal = None;
                session.clear_interaction();
                session.approval_input_safe = true;
                parts.push("cancelled the in-flight turn".to_string());
            }
            let dropped = interjections.drain().len();
            if dropped > 0 {
                parts.push(format!("discarded {dropped} queued message(s)"));
            }
            composer.set_text("");
            parts.push("dropped the unsent composer text".to_string());
            session.notice(
                transcript,
                format!(
                    "Reset: {}. Session identity and history are unchanged.",
                    parts.join(", ")
                ),
            );
        }
        CommandEffect::Rewind => {
            session.notice(
                transcript,
                "Rewind is not available from this surface: the session's durable turn ledger is \
                 readable here (paging) but not truncatable. Nothing was rewound.",
            );
        }
        CommandEffect::Undo => {
            // The only reversible ledger reachable from this host is the
            // composer's edit stack; the session-level one lives behind the
            // daemon. Undo the one that exists and say which it was.
            if composer.undo_edit() {
                session.notice(
                    transcript,
                    "Undid the last composer edit (the only reversible ledger this host owns; \
                     session history is unchanged).",
                );
            } else {
                session.notice(
                    transcript,
                    "Nothing to undo: the composer's edit ledger is empty.",
                );
            }
        }
        CommandEffect::Redo => {
            if composer.redo_edit() {
                session.notice(
                    transcript,
                    "Redid the last undone composer edit (the only reversible ledger this host owns; \
                     session history is unchanged).",
                );
            } else {
                session.notice(
                    transcript,
                    "Nothing to redo: no composer edit has been undone since the last one.",
                );
            }
        }
    }
    requester.schedule_frame();
}

/// Fold one polled timeline event into the session, suppressing the durable
/// `userMessageSubmitted` echo of a submission the composer already rendered
/// locally. FIFO matching on the exact echoed text keeps remote/other-client
/// user messages visible — only the head echo is consumed.
/// Send every queued message, oldest first, as ONE next turn. Each raw message
/// is echoed to the transcript now (queued messages stay out of it until
/// sent). Returns the spawned turn, or `None` when nothing was queued.
fn send_queued(
    host: &impl TurnHost,
    interjections: &InterjectionBuffer,
    session: &mut SessionState,
    transcript: &mut Transcript,
    pending_echoes: &mut std::collections::VecDeque<String>,
    composer: &mut Composer,
) -> Option<JoinHandle<i32>> {
    let mut texts = Vec::new();
    while let Some(text) = interjections.pop_first() {
        texts.push(text);
    }
    if texts.is_empty() {
        return None;
    }
    for text in &texts {
        session.record(transcript, DaemonEvent::UserMessage { text: text.clone() });
    }
    let framed = InterjectionBuffer::frame_steer(&texts.join(
        "

",
    ));
    pending_echoes.push_back(framed.clone());
    composer.set_streaming(true);
    session.working_started = Some(Instant::now());
    session.working_signal = None;
    session.begin_turn_status();
    match spawn_target_turn(host, session, framed) {
        Ok(turn) => Some(turn),
        Err(error) => {
            composer.set_streaming(false);
            session.working_started = None;
            session.notice(transcript, error);
            None
        }
    }
}

fn spawn_target_turn(
    host: &impl TurnHost,
    session: &SessionState,
    prompt: String,
) -> Result<JoinHandle<i32>, String> {
    match session.active_agent.as_ref() {
        Some(agent) => host.spawn_agent_turn(agent, prompt),
        None => Ok(host.spawn_turn(prompt)),
    }
}

/// Whether a system line is the bare label of an internal wire event: a
/// bracketed camelCase identifier (`[selfReviewProposalReceived]`), optionally
/// followed by clipped payload text. Human notices (`[exit 3]`,
/// `[post-edit verification] ...`) never match: the label must be one
/// alphanumeric word that starts lowercase and contains an uppercase letter.
fn is_internal_wire_label(text: &str) -> bool {
    let Some(rest) = text.strip_prefix('[') else {
        return false;
    };
    let Some((label, tail)) = rest.split_once(']') else {
        return false;
    };
    (tail.is_empty() || tail.starts_with(' '))
        && label.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && label.chars().all(|c| c.is_ascii_alphanumeric())
        && label.chars().any(|c| c.is_ascii_uppercase())
}

/// The real exit code of a failed shell result. The runtime puts the
/// `[exit N]` header at the very start of the output; a `[exit N]` string
/// printed later by the command itself must not override it. Only when no
/// leading header survives does the first line-start marker count, and a
/// failure with no recoverable code is reported as 1.
fn tool_exit_code(output: &str) -> i32 {
    fn parse(at: &str) -> Option<i32> {
        let digits: String = at
            .strip_prefix("[exit ")?
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '-')
            .collect();
        digits.parse::<i32>().ok().filter(|code| *code != 0)
    }
    parse(output.trim_start())
        .or_else(|| output.lines().find_map(|line| parse(line.trim_start())))
        .unwrap_or(1)
}

/// Whether a polled event reports a subagent/roster change (typed, or the daemon's unmapped
/// `[subAgent*]` / `[agentRoster*]` diagnostic lines).
fn is_roster_event(event: &DaemonEvent) -> bool {
    match event {
        DaemonEvent::SubagentUpdated { .. } => true,
        DaemonEvent::System { text } => {
            text.starts_with("[subAgent") || text.starts_with("[agentRoster")
        }
        _ => false,
    }
}

fn record_polled_event(
    session: &mut SessionState,
    transcript: &mut Transcript,
    pending_echoes: &mut std::collections::VecDeque<String>,
    event: DaemonEvent,
) {
    if let DaemonEvent::UserMessage { text } = &event {
        // Durable events arrive in submission order, so a match deeper in the
        // list means everything before it never got an echo (a stop or a
        // discarded draft): drop those rather than let one stale head block
        // every later suppression and paint each prompt twice.
        if let Some(index) = pending_echoes.iter().position(|pending| pending == text) {
            pending_echoes.drain(..=index);
            return;
        }
    }
    // The daemon's unmapped `[subAgent*]` / `[agentRoster*]` diagnostics only
    // signal an agent-tree change (handled by `refresh_nav`); their bracketed
    // internal names are never transcript content.
    if matches!(&event, DaemonEvent::System { .. }) && is_roster_event(&event) {
        return;
    }
    session.record(transcript, event);
}

/// Record a locally submitted prompt: echo it now for immediate feedback and
/// remember the text so its durable `userMessageSubmitted` event is dropped
/// on arrival instead of painting a second identical `You` block.
fn record_local_submission(
    session: &mut SessionState,
    transcript: &mut Transcript,
    pending_echoes: &mut std::collections::VecDeque<String>,
    text: &str,
) {
    session.record(
        transcript,
        DaemonEvent::UserMessage {
            text: text.to_string(),
        },
    );
    pending_echoes.push_back(text.to_string());
}

/// What a `/export` argument list resolves to.
#[derive(Debug, PartialEq, Eq)]
enum ExportTarget {
    Current(&'static str),
    Other(String, &'static str),
}

fn export_format(word: &str) -> Option<&'static str> {
    match word.to_ascii_lowercase().as_str() {
        "md" | "markdown" => Some("md"),
        "html" => Some("html"),
        _ => None,
    }
}

/// Parse `/export [session-id] [md|html]`.
fn parse_export_args(rest: &str) -> Result<ExportTarget, ()> {
    let words: Vec<&str> = rest.split_whitespace().collect();
    match words.as_slice() {
        [] => Ok(ExportTarget::Current("md")),
        [one] => Ok(match export_format(one) {
            Some(format) => ExportTarget::Current(format),
            None => ExportTarget::Other((*one).to_string(), "md"),
        }),
        [id, format] => export_format(format)
            .map(|format| ExportTarget::Other((*id).to_string(), format))
            .ok_or(()),
        _ => Err(()),
    }
}

/// `coderight-session-<first 8 id chars>.<ext>`, filesystem-safe.
fn export_file_name(session_id: &str, format: &str) -> String {
    let short: String = session_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect();
    let short = if short.is_empty() {
        "export".to_string()
    } else {
        short
    };
    format!("coderight-session-{short}.{format}")
}

/// Write the exported content into `dir` and return the full path.
fn write_session_export(
    dir: &std::path::Path,
    session_id: &str,
    format: &str,
    content: &str,
) -> Result<std::path::PathBuf, String> {
    let path = dir.join(export_file_name(session_id, format));
    std::fs::write(&path, content)
        .map_err(|error| format!("Could not write {}: {error}", path.display()))?;
    Ok(path)
}

/// Pull the new id out of the host's "Cloned session <id>" confirmation.
fn parse_cloned_session_id(output: &str) -> Option<String> {
    output
        .strip_prefix("Cloned session ")
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
}

/// `/delete` is destructive: it needs an explicit `--yes`. Returns the
/// optional explicit session id and whether it was confirmed.
fn parse_delete_args(rest: &str) -> (Option<String>, bool) {
    let mut confirmed = false;
    let mut id = None;
    for word in rest.split_whitespace() {
        if matches!(word, "--yes" | "-y") {
            confirmed = true;
        } else if id.is_none() {
            id = Some(word.to_string());
        }
    }
    (id, confirmed)
}

/// `/copy`, `/rename`, `/clone`, `/export`, `/archive`, `/delete` (STATUS P-5).
#[allow(clippy::too_many_arguments)]
async fn run_session_command(
    host: &impl TurnHost,
    name: &str,
    rest: &str,
    composer: &mut Composer,
    nav: &mut NavView,
    session: &mut SessionState,
    transcript: &mut Transcript,
    surface: &mut Surface,
    interjections: &InterjectionBuffer,
    viewed_agent: &mut Option<AgentId>,
    paging: &mut HistoryPaging,
    turn: &Option<JoinHandle<i32>>,
) {
    let current = host
        .current_session()
        .unwrap_or_else(|| session.active_session.clone());
    let rest = rest.trim();
    match name {
        "/copy" => {
            // Codex/Claude Code: copy the latest assistant response.
            let payload = if !rest.is_empty() {
                rest.to_string()
            } else if let Some(text) = transcript.latest_assistant_markdown() {
                text.to_string()
            } else {
                session.notice(transcript, "Nothing to copy yet: no assistant response.");
                return;
            };
            session.notice(transcript, clipboard_copy_notice(&payload));
        }
        "/rename" => {
            if rest.is_empty() {
                session.notice(transcript, "usage: /rename <new title>");
                return;
            }
            match host.session_action("rename", &current, Some(rest)).await {
                Ok(_) => session.notice(transcript, format!("Renamed session to \"{rest}\"")),
                Err(error) => session.notice(transcript, error),
            }
        }
        "/export" | "export" => {
            let (id, format) = match parse_export_args(rest) {
                Ok(ExportTarget::Current(format)) => (current, format),
                Ok(ExportTarget::Other(id, format)) => (id, format),
                Err(()) => {
                    session.notice(transcript, "usage: /export [session-id] [md|html]");
                    return;
                }
            };
            match host.session_action("export", &id, Some(format)).await {
                Ok(content) => {
                    let dir = host
                        .active_directory()
                        .map(std::path::PathBuf::from)
                        .filter(|dir| dir.is_dir())
                        .or_else(|| std::env::current_dir().ok())
                        .unwrap_or_else(|| std::path::PathBuf::from("."));
                    match write_session_export(&dir, &id, format, &content) {
                        Ok(path) => session.notice(
                            transcript,
                            format!(
                                "Exported session {id} ({format}) to {}",
                                rightkit_terminal_ui::render::display_path_str(
                                    &path.display().to_string()
                                )
                            ),
                        ),
                        Err(error) => session.notice(transcript, error),
                    }
                }
                Err(error) => session.notice(transcript, error),
            }
        }
        "/clone" => {
            let source = if rest.is_empty() {
                current
            } else {
                rest.to_string()
            };
            match host.session_action("clone", &source, None).await {
                Ok(output) => match parse_cloned_session_id(&output) {
                    Some(id) if turn.is_none() => {
                        switch_to_session(
                            host,
                            SessionId(id),
                            composer,
                            nav,
                            session,
                            transcript,
                            surface,
                            false,
                            viewed_agent,
                            paging,
                        )
                        .await;
                    }
                    _ => session.notice(transcript, output),
                },
                Err(error) => session.notice(transcript, error),
            }
        }
        "/archive" | "/delete" => {
            let action = name.trim_start_matches('/');
            let (explicit, confirmed) = if action == "delete" {
                parse_delete_args(rest)
            } else {
                (Some(rest.to_string()).filter(|id| !id.is_empty()), true)
            };
            let target = explicit.unwrap_or_else(|| current.clone());
            if !confirmed {
                session.notice(
                    transcript,
                    format!(
                        "This permanently deletes session {target}. Re-run `/delete {target} --yes` to confirm."
                    ),
                );
                return;
            }
            let is_current = target == current;
            if is_current && turn.is_some() {
                session.notice(
                    transcript,
                    format!("Cannot {action} the active session while a turn is in flight; cancel it first."),
                );
                return;
            }
            match host.session_action(action, &target, None).await {
                Ok(output) => {
                    session.notice(transcript, output);
                    if is_current {
                        // The viewed session left the active list: land on a
                        // fresh one rather than a dead id.
                        match host.new_session().await {
                            Ok(id) => {
                                let from = view_key(session, viewed_agent.as_ref());
                                interjections.clear();
                                transcript.clear();
                                session.reset_view(&id.0);
                                *viewed_agent = None;
                                paging.rebind_session();
                                session.active_session = id.0.clone();
                                let to = view_key(session, viewed_agent.as_ref());
                                switch_view(composer, &from, &to);
                                host.refresh_nav(nav, false).await;
                                session.notice(
                                    transcript,
                                    format!("Session {action}d. Started a new session {id}."),
                                );
                            }
                            Err(reason) => session.notice(transcript, reason),
                        }
                    }
                }
                Err(error) => session.notice(transcript, error),
            }
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_submit(
    host: &impl TurnHost,
    text: String,
    composer: &mut Composer,
    nav: &mut NavView,
    session: &mut SessionState,
    transcript: &mut Transcript,
    surface: &mut Surface,
    interjections: &InterjectionBuffer,
    pending_echoes: &mut std::collections::VecDeque<String>,
    viewed_agent: &mut Option<AgentId>,
    paging: &mut HistoryPaging,
    turn: &mut Option<JoinHandle<i32>>,
    requester: &FrameRequester,
) -> bool {
    // #140: while an agent view is open (picker Enter), a plain message goes
    // to that running subagent through the live followup/steer channel.
    if let Some(agent) = viewed_agent.clone() {
        if session.pending_rename.is_none() && !text.trim_start().starts_with('/') {
            let body = text.trim();
            if !body.is_empty() {
                session.notice(
                    transcript,
                    subagent_message_notice(&agent, host.message_remote_agent(&agent, body).await),
                );
                requester.schedule_frame();
            }
            return false;
        }
    }
    if let Some(session_id) = session.pending_rename.take() {
        let title = text.trim();
        if title.is_empty() {
            session.notice(transcript, "Session rename cancelled: title is empty.");
        } else {
            match host
                .session_action("rename", &session_id, Some(title))
                .await
            {
                Ok(message) => {
                    session.notice(transcript, message);
                }
                Err(error) => session.notice(transcript, error),
            }
        }
        composer.set_placeholder_override(None);
        session.session_action_id = None;
        session.session_action_kinds.clear();
        requester.schedule_frame();
        return false;
    }
    if let Some(command) = rightkit_terminal_ui::pty_attach::parse_command(&text) {
        run_pty_command(
            host, command, composer, session, transcript, surface, requester,
        )
        .await;
        return false;
    }
    // `!cmd` / `!!cmd`: user-initiated shell command, not a prompt.
    if let Some(shell) = rightkit_terminal_ui::user_shell::parse(&text) {
        run_user_shell(host, shell, session, transcript, surface, requester).await;
        composer.set_shell_running(rightkit_terminal_ui::user_shell::is_active());
        return false;
    }
    // #132: a custom command submits its expanded prompt instead.
    let text = host.custom_command_prompt(&text).unwrap_or(text);
    let mid_turn = turn.is_some();
    let command_text = text.trim_start();
    let head = command_text.split_whitespace().next().unwrap_or("");
    let rest = command_text[head.len()..].trim().to_string();

    // TUI commands are explicit slash inputs. Bare words are prompts even
    // when they happen to equal a registered command name (for example
    // `quit`, `help`, or `run`).
    if head.starts_with('/') {
        if head.eq_ignore_ascii_case("/tui") && rest.eq_ignore_ascii_case("fullscreen") {
            let enabled = surface.modal() != Some(ModalKind::Fullscreen);
            match host
                .set_tui_setting("fullscreen", if enabled { "on" } else { "off" })
                .await
            {
                Ok(_) => {
                    if enabled {
                        surface.enter_modal(ModalKind::Fullscreen);
                    } else {
                        surface.leave_modal();
                    }
                    session.proj.settings = host.tui_settings();
                }
                Err(error) => session.notice(transcript, error),
            }
            requester.schedule_frame();
            return false;
        }
        if let Some(name) = host.resolve_command(head) {
            match host.check_command(&name, mid_turn) {
                Ok(()) => {
                    if name == "/goal" {
                        match goal_command(host, &rest).await {
                            Ok(line) => session.notice(transcript, line),
                            Err(reason) => session.notice(transcript, reason),
                        }
                        let followup = host.take_followup_prompt();
                        requester.schedule_frame();
                        if let Some(prompt) = followup.filter(|_| !mid_turn) {
                            return Box::pin(handle_submit(
                                host,
                                prompt,
                                composer,
                                nav,
                                session,
                                transcript,
                                surface,
                                interjections,
                                pending_echoes,
                                viewed_agent,
                                paging,
                                turn,
                                requester,
                            ))
                            .await;
                        }
                        return false;
                    }
                    if name == "/diff" {
                        match host.workspace_diff().await {
                            Ok(diff) => {
                                session.inspection =
                                    Some(crate::tui_inspection::InspectionState::diff(&diff));
                                surface.enter_modal(ModalKind::DiffViewer);
                            }
                            Err(reason) => session.notice(transcript, reason),
                        }
                        requester.schedule_immediate();
                        return false;
                    }
                    if name == "/logs" {
                        if !rest.is_empty() {
                            session.notice(transcript, "usage: /logs");
                        } else {
                            let session_id = host
                                .current_session()
                                .unwrap_or_else(|| session.active_session.clone());
                            match host.session_log(&session_id).await {
                                Ok(logs) => {
                                    session.inspection =
                                        Some(crate::tui_inspection::InspectionState::diff(&logs));
                                    surface.enter_modal(ModalKind::DiffViewer);
                                }
                                Err(reason) => session.notice(transcript, reason),
                            }
                        }
                        requester.schedule_immediate();
                        return false;
                    }
                    if name == "/hooks" {
                        if !rest.is_empty() {
                            session.notice(transcript, "usage: /hooks");
                        } else {
                            match host.hooks_status().await {
                                Ok(status) => session.notice(
                                    transcript,
                                    format!("Hook readiness (redacted):\n{status}"),
                                ),
                                Err(reason) => session.notice(transcript, reason),
                            }
                        }
                        requester.schedule_frame();
                        return false;
                    }
                    if name == "/insights" {
                        match host.insights().await {
                            Ok(text) if !text.trim().is_empty() => session.notice(transcript, text),
                            Ok(_) => {
                                session.notice(transcript, "No session insights are available.")
                            }
                            Err(reason) => session.notice(transcript, reason),
                        }
                        requester.schedule_frame();
                        return false;
                    }
                    if name == "/init" {
                        let result = if rest.trim().is_empty() {
                            host.init_workspace().await
                        } else {
                            Err("usage: /init".to_string())
                        };
                        match result {
                            Ok(output) | Err(output) => session.notice(transcript, output),
                        }
                        requester.schedule_frame();
                        return false;
                    }
                    if name == "/cd" && !rest.trim().is_empty() {
                        match host.select_repository(rest.trim()).await {
                            Ok(()) => {
                                rebind_view_to_active(
                                    host,
                                    composer,
                                    nav,
                                    session,
                                    transcript,
                                    interjections,
                                    viewed_agent,
                                    paging,
                                    format!("Switched workspace to {}", rest.trim()),
                                )
                                .await;
                            }
                            Err(error) => session.notice(transcript, error),
                        }
                        requester.schedule_frame();
                        return false;
                    }
                    if name == "/session" && !rest.trim().is_empty() && rest.trim() != "list" {
                        session.notice(
                            transcript,
                            format!(
                                "Session {} · workspace {}",
                                host.current_session()
                                    .unwrap_or_else(|| session.active_session.clone()),
                                host.active_directory().unwrap_or_else(|| "unknown".into())
                            ),
                        );
                        requester.schedule_frame();
                        return false;
                    }
                    if name == "/worktree" {
                        let mut args = rest.split_whitespace();
                        let action = args.next().unwrap_or("list");
                        let values: Vec<&str> = args.collect();
                        // A088 worktree menu: list renders a numbered menu;
                        // switch/remove accept a menu number, path or branch.
                        use rightkit_terminal_ui::nav::worktree_menu as wtm;
                        let entries = match host.worktree_action("list", &[]).await {
                            Ok(raw) => wtm::parse_porcelain(&wtm::porcelain_from_result(&raw)),
                            Err(_) => Vec::new(),
                        };
                        match wtm::parse_request(action, &values, &entries) {
                            Ok(wtm::WorktreeRequest::List) => {
                                let active = host.active_directory();
                                session.notice(
                                    transcript,
                                    wtm::render_menu(&entries, active.as_deref()),
                                );
                            }
                            Ok(wtm::WorktreeRequest::Create { branch, base, path }) => {
                                let mut a: Vec<&str> = vec![branch.as_str()];
                                if let Some(b) = base.as_deref() {
                                    a.push(b);
                                }
                                if let Some(p) = path.as_deref() {
                                    a.push(p);
                                }
                                match host.worktree_action("add", &a).await {
                                    Ok(output) | Err(output) => session.notice(transcript, output),
                                }
                            }
                            Ok(wtm::WorktreeRequest::Remove { path }) => {
                                match host.worktree_action("remove", &[path.as_str()]).await {
                                    Ok(output) | Err(output) => session.notice(transcript, output),
                                }
                            }
                            Ok(wtm::WorktreeRequest::Switch { path }) => {
                                match host.select_repository(&path).await {
                                    Ok(()) => {
                                        rebind_view_to_active(
                                            host,
                                            composer,
                                            nav,
                                            session,
                                            transcript,
                                            interjections,
                                            viewed_agent,
                                            paging,
                                            format!("Switched workspace to worktree {path}"),
                                        )
                                        .await;
                                    }
                                    Err(error) => session.notice(transcript, error),
                                }
                            }
                            Err(reason) => session.notice(transcript, reason),
                        }
                        requester.schedule_frame();
                        return false;
                    }
                    if matches!(
                        name.as_str(),
                        "/copy"
                            | "/rename"
                            | "/clone"
                            | "/export"
                            | "export"
                            | "/archive"
                            | "/delete"
                    ) {
                        run_session_command(
                            host,
                            &name,
                            &rest,
                            composer,
                            nav,
                            session,
                            transcript,
                            surface,
                            interjections,
                            viewed_agent,
                            paging,
                            turn,
                        )
                        .await;
                        requester.schedule_frame();
                        return false;
                    }
                    let should_stop = host.execute_command(&name, &rest).await;
                    if name == "/theme" || name == "/vim" {
                        session.proj.settings_dirty = true;
                    }
                    if should_stop {
                        if let Err(error) = cancel_active_turn(host, turn).await {
                            session.notice(transcript, error);
                            requester.schedule_frame();
                            return false;
                        }
                    }
                    requester.schedule_frame();
                    return should_stop;
                }
                Err(reason) => session.notice(transcript, reason),
            }
            requester.schedule_frame();
            return false;
        }
    }

    if mid_turn {
        // P1-7: typed FIFO directives with immediate-interrupt semantics.
        // Steer stays queued and runs after the current turn ends; Cancel and
        // Reprompt take effect IMMEDIATELY on the in-flight turn — aborting
        // it at enqueue time instead of waiting for a drain that would only
        // run once the turn finished naturally (the old defect: a queued
        // cancel did nothing until the turn completed on its own, and the
        // abort then operated on an already-finished turn).
        let lower = text.trim().to_ascii_lowercase();
        let kind = if lower == "/cancel" || lower == "/stop" || lower == "/abort" {
            InterjectKind::Cancel
        } else if lower.starts_with("/reprompt ") || lower.starts_with("/replace ") {
            InterjectKind::Reprompt
        } else {
            InterjectKind::Steer
        };
        // Queue text is visible now; only an admitted follow-up has a durable
        // framed echo. Cancelled/discarded entries must not poison echo FIFO.
        if kind != InterjectKind::Steer {
            session.record(transcript, DaemonEvent::UserMessage { text: text.clone() });
        }
        match plan_immediate_interrupt(kind, &rest, interjections.len()) {
            ImmediateInterrupt::None => {
                // Queued messages stay out of the transcript until sent; the
                // live queue list above the composer shows them.
                interjections.push(text.clone(), InterjectKind::Steer);
            }
            ImmediateInterrupt::AbortAndDiscardAll { notice } => {
                // A stop discards everything queued around it: after "stop",
                // nothing else in the FIFO is a request the user still wants
                // carried out. The stop itself never enters the buffer.
                interjections.clear();
                if let Err(error) = cancel_active_turn(host, turn).await {
                    session.notice(transcript, error);
                    return false;
                }
                composer.set_streaming(false);
                session.working_started = None;
                session.working_signal = None;
                session.notice(transcript, notice);
            }
            ImmediateInterrupt::AbortAndReplace { framed, notice } => {
                // Discard everything queued, abort the in-flight turn, and
                // spawn the replacement immediately. The payload carries the
                // re-prompt envelope (note + <user_query> + unfinished-work
                // reminder) with UTF-8-boundary truncation already applied by
                // `frame_reprompt`.
                interjections.clear();
                if let Err(error) = cancel_active_turn(host, turn).await {
                    session.notice(transcript, error);
                    return false;
                }
                composer.set_streaming(true);
                session.working_started = Some(Instant::now());
                session.working_signal = None;
                session.begin_turn_status();
                pending_echoes.push_back(framed.clone());
                *turn = match spawn_target_turn(host, session, framed) {
                    Ok(turn) => Some(turn),
                    Err(error) => {
                        composer.set_streaming(false);
                        session.notice(transcript, error);
                        None
                    }
                };
                session.notice(transcript, notice);
            }
        }
        return false;
    }

    // A turn that ended through Ctrl-C/cancel_active_turn never ran the
    // `wait_turn` branch's tail drain, so its already-queued canonical
    // events (final deltas, tool results, the interrupted notice) can still
    // sit behind the host's cursor with no turn in flight to tick the
    // maintenance poll. Fold them through the same reducer + echo dedup the
    // tick uses BEFORE the local echo lands — otherwise the cancelled
    // turn's tail paints after this prompt's `You` block.
    for event in host.poll_timeline_events().await {
        record_polled_event(session, transcript, pending_echoes, event);
    }
    flush_history(transcript, surface, requester);

    // S:I3 — the submitted prompt is the session's next typed event.
    record_local_submission(session, transcript, pending_echoes, &text);
    // `!cmd` output queued since the last prompt rides in front of it; the
    // durable echo then carries the framed text, so suppress that instead.
    let prompt = rightkit_terminal_ui::user_shell::frame_with_pending_context(&text);
    if prompt != text {
        if let Some(echo) = pending_echoes.back_mut() {
            *echo = prompt.clone();
        }
    }
    composer.set_streaming(true);
    session.working_started = Some(Instant::now());
    session.working_signal = None;
    session.begin_turn_status();
    *turn = match spawn_target_turn(host, session, prompt) {
        Ok(turn) => Some(turn),
        Err(error) => {
            composer.set_streaming(false);
            session.notice(transcript, error);
            None
        }
    };
    false
}

/// Process-wide handle to the TUI input reader's pause, set once by `drive`.
/// Ctrl+G needs exclusive use of the tty while the editor runs.
static EDITOR_INPUT_PAUSE: std::sync::OnceLock<Arc<InputPause>> = std::sync::OnceLock::new();

/// Ctrl+G: hand the tty to `$VISUAL`/`$EDITOR` on the draft and load the
/// edited text back. The editor must not race the crossterm reader.
fn run_external_editor(
    composer: &mut Composer,
    session: &mut SessionState,
    transcript: &mut Transcript,
    surface: &mut Surface,
    requester: &FrameRequester,
    text: &str,
) {
    let Some(pause) = EDITOR_INPUT_PAUSE.get() else {
        session.notice(
            transcript,
            "External editor is unavailable: no input reader.",
        );
        return;
    };
    pause.request();
    let _resume = InputPauseGuard::new(|| pause.resume());
    match rightkit_terminal_ui::terminal::external_editor::edit_text(text) {
        Ok(edited) => composer.set_text(&edited),
        Err(error) => session.notice(transcript, format!("External editor failed: {error}")),
    }
    // The editor painted over the viewport; keystrokes typed meanwhile went to
    // the editor, not the composer.
    if let Ok((w, h)) = ct_terminal::size() {
        surface.resize(w, h);
    }
    requester.record_reflow();
    requester.schedule_immediate();
}

/// A070/P-16: `/pty attach <id>`. Detach and kill act on the attached process
/// (Ctrl+] detaches); outside an attach they explain themselves.
async fn run_pty_command(
    host: &impl TurnHost,
    command: rightkit_terminal_ui::pty_attach::PtyCommand,
    composer: &mut Composer,
    session: &mut SessionState,
    transcript: &mut Transcript,
    surface: &Surface,
    requester: &FrameRequester,
) {
    use rightkit_terminal_ui::pty_attach::{PtyAttach, PtyCommand};
    match command {
        PtyCommand::Attach(id) => match host.pty_attach(&id).await {
            Ok(lease) => {
                let (width, height) = surface.screen_size();
                let mut pty = PtyAttach::new(id.clone(), lease, width, height);
                // Replay the retained tail so the user sees where it is.
                // The retained tail feeds the terminal-grid pane, not the transcript.
                if let Ok(snapshot) = host.running_output(&id).await {
                    let _ = pty.ingest_snapshot(&snapshot);
                }
                session.notice(
                    transcript,
                    format!("Attached to {id}. Keys go to the process; Ctrl+] detaches."),
                );
                session.pty = Some(pty);
                pty_apply_intents(host, session, transcript).await;
                pty_show_partial(composer, session);
            }
            Err(error) => session.notice(transcript, format!("Cannot attach to {id}: {error}")),
        },
        PtyCommand::Detach => session.notice(transcript, "Not attached to a PTY process."),
        PtyCommand::Kill => session.notice(
            transcript,
            "Not attached to a PTY process. Stop a shell from the Running panel.",
        ),
    }
    requester.schedule_immediate();
}

/// Route one event to the attached PTY and execute what it asks for.
async fn pty_attached_event(
    host: &impl TurnHost,
    event: &TuiEvent,
    composer: &mut Composer,
    session: &mut SessionState,
    transcript: &mut Transcript,
    requester: &FrameRequester,
) {
    if let Some(pty) = session.pty.as_mut() {
        pty.handle(event);
    }
    pty_apply_intents(host, session, transcript).await;
    pty_poll(host, session, transcript).await;
    pty_show_partial(composer, session);
    requester.schedule_immediate();
}

/// Execute queued PTY intents in order; a failed write ends the attach so
/// input is never silently dropped or reordered.
async fn pty_apply_intents(
    host: &impl TurnHost,
    session: &mut SessionState,
    transcript: &mut Transcript,
) {
    use rightkit_terminal_ui::pty_attach::PtyIntent;
    let Some(pty) = session.pty.as_mut() else {
        return;
    };
    let id = pty.id().to_string();
    let mut detach = false;
    let mut failure: Option<String> = None;
    for intent in pty.drain_intents() {
        match intent {
            PtyIntent::Write {
                bytes,
                epoch,
                sequence,
            } => match host.pty_write(&id, &bytes, epoch, sequence).await {
                Ok(()) => pty.write_accepted(),
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            },
            PtyIntent::Resize { rows, cols } => {
                if let Err(error) = host.pty_resize(&id, rows, cols).await {
                    failure = Some(error);
                    break;
                }
            }
            PtyIntent::Detach => {
                detach = true;
                break;
            }
        }
    }
    if detach || failure.is_some() {
        if let Some(mut pty) = session.pty.take() {
            if let Some(line) = pty.flush_partial() {
                session.notice(transcript, line);
            }
        }
        let _ = host.pty_detach(&id).await;
        session.notice(
            transcript,
            match failure {
                Some(error) => format!("Detached from {id}: {error}"),
                None => format!("Detached from {id}."),
            },
        );
    }
}

/// Pull new output for the attached process into the transcript.
async fn pty_poll(host: &impl TurnHost, session: &mut SessionState, transcript: &mut Transcript) {
    let Some(pty) = session.pty.as_mut() else {
        return;
    };
    let id = pty.id().to_string();
    match host.running_output(&id).await {
        Ok(snapshot) => {
            // Output renders in the terminal-grid pane, not the transcript.
            let _ = pty.ingest_snapshot(&snapshot);
        }
        Err(error) => {
            if let Some(mut pty) = session.pty.take() {
                if let Some(line) = pty.flush_partial() {
                    session.notice(transcript, line);
                }
            }
            session.notice(transcript, format!("PTY process {id} ended: {error}"));
        }
    }
}

/// Show the child's unfinished line (its prompt) in the composer.
fn pty_show_partial(composer: &mut Composer, session: &SessionState) {
    composer.set_placeholder_override(
        session
            .pty
            .as_ref()
            .map(|pty| format!("[{}] {}", pty.id(), pty.partial_line())),
    );
}

/// `!cmd` / `!!cmd`: run in the session directory, show the output in the
/// transcript, and (for `!cmd`) queue it as context for the model's next
/// prompt. Runs asynchronously: output streams to the transcript from
/// `drain_user_shell` on the UI tick, and Esc / Ctrl+C kills it.
async fn run_user_shell(
    host: &impl TurnHost,
    shell: rightkit_terminal_ui::user_shell::UserShell,
    session: &mut SessionState,
    transcript: &mut Transcript,
    surface: &mut Surface,
    requester: &FrameRequester,
) {
    let cwd = host
        .active_directory()
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    session.record(
        transcript,
        DaemonEvent::UserMessage {
            text: format!(
                "!{}{}",
                if shell.exclude_from_context { "!" } else { "" },
                shell.command
            ),
        },
    );
    flush_history(transcript, surface, requester);
    requester.schedule_immediate();
    if rightkit_terminal_ui::user_shell::is_active() {
        session.notice(
            transcript,
            "A shell command is still running (Esc or Ctrl+C stops it).",
        );
    } else {
        let job = rightkit_terminal_ui::user_shell::spawn(&shell.command, &cwd);
        rightkit_terminal_ui::user_shell::set_active(job, shell, cwd);
    }
    requester.schedule_frame();
}

/// Stream the active `!cmd`'s new output into the transcript and settle it
/// when it ends. Returns true while a shell is still running.
fn drain_user_shell(
    composer: &mut Composer,
    session: &mut SessionState,
    transcript: &mut Transcript,
    surface: &mut Surface,
    requester: &FrameRequester,
) -> bool {
    let Some(poll) = rightkit_terminal_ui::user_shell::poll_active() else {
        composer.set_shell_running(false);
        return false;
    };
    if !poll.new_output.trim().is_empty() {
        session.notice(transcript, poll.new_output.trim_end().to_string());
    }
    let running = poll.finished.is_none();
    if let Some((command, cwd, exclude, run)) = poll.finished {
        session.notice(transcript, run.display(&command));
        if !exclude {
            rightkit_terminal_ui::user_shell::push_context(run.context_block(&command, &cwd));
        }
    }
    composer.set_shell_running(running);
    flush_history(transcript, surface, requester);
    requester.schedule_frame();
    running
}

/// What a mid-turn directive must do to the IN-FLIGHT turn right now (P1-7
/// immediate-interrupt semantics). Pure so the semantics are unit-testable
/// without a running loop.
///
/// - `Steer` interrupts nothing; it stays queued (`None`).
/// - `Cancel` aborts the turn and discards the whole queue.
/// - `Reprompt` aborts the turn and spawns its framed replacement now,
///   discarding the queue behind it. `queued` is how many messages were
///   waiting when this directive arrived (for the dropped-count notice).
enum ImmediateInterrupt {
    None,
    AbortAndDiscardAll { notice: String },
    AbortAndReplace { framed: String, notice: String },
}

fn dropped_suffix(queued: usize) -> String {
    match queued {
        0 => String::new(),
        1 => " 1 queued message was discarded.".to_string(),
        n => format!(" {n} queued messages were discarded."),
    }
}

fn plan_immediate_interrupt(
    kind: InterjectKind,
    reprompt_payload: &str,
    queued: usize,
) -> ImmediateInterrupt {
    match kind {
        InterjectKind::Steer => ImmediateInterrupt::None,
        InterjectKind::Cancel => ImmediateInterrupt::AbortAndDiscardAll {
            notice: format!("Turn cancelled.{}", dropped_suffix(queued)),
        },
        InterjectKind::Reprompt => ImmediateInterrupt::AbortAndReplace {
            framed: InterjectionBuffer::frame_reprompt(reprompt_payload),
            notice: format!(
                "Re-prompted -- the previous turn was stopped and its replacement started.{}",
                dropped_suffix(queued)
            ),
        },
    }
}

fn flush_history(transcript: &mut Transcript, surface: &mut Surface, requester: &FrameRequester) {
    if !surface.can_insert_history() {
        requester.schedule_frame();
        return;
    }
    let (width, _) = surface.screen_size();
    let lines = transcript.drain_finalized(width);
    if !lines.is_empty() {
        // Codex queues history until the next draw has settled its viewport.
        // Inserting while finalized cells still reserve rows leaves blank
        // bands when that oversized viewport then shrinks.
        surface.queue_history(lines);
    }
    requester.schedule_frame();
}

/// Insert display-ready lines into the terminal's own scrollback above the
/// composer (`Surface::insert_history`). Shared by transcript flushing,
/// P2-5 eviction salvage, and P2-5 durable-history paging. `&mut` because a
/// slide-down insertion moves the viewport's top and records the released
/// band for the next frame's erase pass.
fn insert_scrollback_lines(surface: &mut Surface, lines: &[HyperlinkLine]) {
    if lines.is_empty() || !surface.can_insert_history() {
        return;
    }
    let mut out = io::stdout();
    // The exact pre-call cursor position is whatever the composer last
    // drew it at; any transient mismatch self-heals on the very next
    // `render`, which repositions the real cursor unconditionally.
    let cursor = (0, surface.viewport().top);
    let _ = surface.insert_history(&mut out, lines, HistoryLineWrapPolicy::PreWrap, cursor);
}

/// Rules above/below input plus the one persistent footer row.
const COMPOSER_CHROME_ROWS: u16 = 3;

/// Row geometry of the input frame inside the inline viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChromeLayout {
    status: Rect,
    queue: Rect,
    working: Rect,
    rule_top: Rect,
    composer: Rect,
    subagents: Rect,
    rule_bottom: Rect,
    footer: Rect,
}

/// Pack the input frame from the bottom up so the footer is always the last
/// row of `area`; the transcript tail takes whatever is left above. Rows
/// degrade (footer, rule, rule, working) when `area` is too short.
fn chrome_layout(
    area: Rect,
    composer_rows: u16,
    subagent_rows: u16,
    working: bool,
    queue_rows: u16,
) -> ChromeLayout {
    let mut bottom = area.bottom();
    let mut take = |want: u16, floor: u16| -> Rect {
        let rows = want.min(bottom.saturating_sub(floor.max(area.y)));
        bottom -= rows;
        Rect {
            x: area.x,
            y: bottom,
            width: area.width,
            height: rows,
        }
    };
    let footer = take(1, area.y);
    let rule_bottom = take(1, area.y);
    let subagents = take(subagent_rows, area.y);
    let composer = take(composer_rows, area.y);
    let rule_top = take(1, area.y);
    let working = take(u16::from(working), area.y);
    let queue = take(queue_rows, area.y);
    let status = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: bottom - area.y,
    };
    ChromeLayout {
        status,
        queue,
        working,
        rule_top,
        composer,
        subagents,
        rule_bottom,
        footer,
    }
}

fn agent_count(node: &AgentNode) -> usize {
    node.children
        .iter()
        .map(|child| 1 + agent_count(child))
        .sum()
}

/// A dim full-width horizontal rule framing the composer (Claude Code's
/// input box).
fn composer_rule(width: u16, theme: &Theme) -> rightkit_terminal_ui::ratatui::text::Line<'static> {
    use rightkit_terminal_ui::ratatui::style::Style;
    rightkit_terminal_ui::ratatui::text::Line::from(rightkit_terminal_ui::ratatui::text::Span::styled(
        "\u{2500}".repeat(width as usize),
        Style::default().fg(theme.color(rightkit_terminal_ui::component::Role::Dim)),
    ))
}

/// One dim line for a reasoning block: its bold `**headings**` joined with
/// ` · `, else its first sentence. The full working notes are the model's, not
/// the transcript's; the summary says what it was thinking about.
#[allow(dead_code)]
fn reasoning_summary(text: &str) -> String {
    let mut headings: Vec<String> = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("**") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("**") else { break };
        let heading = after[..end].trim();
        if !heading.is_empty() && !heading.contains('\n') {
            headings.push(heading.chars().take(80).collect());
        }
        rest = &after[end + 2..];
        if headings.len() == 3 {
            break;
        }
    }
    let summary = if headings.is_empty() {
        text.lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("")
            .to_string()
    } else {
        headings.join(" \u{b7} ")
    };
    if summary.chars().count() > 160 {
        let mut cut: String = summary.chars().take(159).collect();
        cut.push('…');
        cut
    } else {
        summary
    }
}

/// First complete bold `**heading**` in a reasoning summary (Codex status
/// header). Single-line, bounded.
fn reasoning_heading_of(text: &str) -> Option<String> {
    let start = text.find("**")? + 2;
    let end = text[start..].find("**")? + start;
    let heading = text[start..end].trim();
    if heading.is_empty() || heading.contains('\n') {
        return None;
    }
    Some(heading.chars().take(80).collect())
}

/// Working-line activity for a running tool call: "Running <cmd>",
/// "Reading <file>", "Editing <file>", else "Running <tool>".
fn tool_activity_label(name: &str, args_json: &str) -> String {
    let args: serde_json::Value = serde_json::from_str(args_json).unwrap_or_default();
    let field = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| args.get(*k).and_then(serde_json::Value::as_str))
            .map(|v| {
                // First line only, clipped with a visible ellipsis.
                let line = v.lines().next().unwrap_or("").trim();
                if line.chars().count() > 60 {
                    let mut cut: String = line.chars().take(59).collect();
                    cut.push('…');
                    cut
                } else {
                    line.to_string()
                }
            })
    };
    let lower = name.to_ascii_lowercase();
    let label = if lower.contains("read") || lower == "cat" || lower.contains("view") {
        field(&["path", "file_path", "file"]).map(|f| format!("Reading {f}"))
    } else if lower.contains("edit") || lower.contains("write") || lower.contains("patch") {
        field(&["path", "file_path", "file"]).map(|f| format!("Editing {f}"))
    } else if lower.contains("shell")
        || lower.contains("bash")
        || lower.contains("exec")
        || lower == "run"
    {
        field(&["command", "cmd"]).map(|c| format!("Running {c}"))
    } else {
        None
    };
    label.unwrap_or_else(|| format!("Running {name}"))
}

/// Row the inline viewport starts at: never above the launch cursor row, and
/// never so high that the viewport would not end on the screen's last row.
fn bottom_anchored_launch_row(launch_row: u16, rows: u16, viewport_height: u16) -> u16 {
    launch_row.max(rows.saturating_sub(viewport_height.max(1)))
}

/// Composer placeholder after an approval Esc: the user is expected to
/// redirect the model.
fn reject_placeholder(model: &str) -> String {
    let model = model.split(" \u{b7} ").next().unwrap_or("").trim();
    if model.is_empty() {
        "Tell the model what to do instead".to_string()
    } else {
        format!("Tell {model} what to do instead")
    }
}

/// The model label the chrome shows: the committed route's model (with its
/// effort), else the identity projection's model.
fn footer_model(host: &impl TurnHost, session: &SessionState) -> String {
    let committed = host.committed_route();
    let model = committed
        .model
        .filter(|m| !m.trim().is_empty())
        .or_else(|| session.identity.attribution().map(|a| a.model.clone()))
        .unwrap_or_default();
    match committed.effort.filter(|e| !e.trim().is_empty()) {
        Some(effort) if !model.is_empty() => format!("{model} \u{b7} {effort}"),
        _ => model,
    }
}

/// The right-hand footer text: the model label plus the context gauge once the
/// runtime has reported usage (`gpt-6.1-sol · high · 14% ctx`).
fn footer_right(host: &impl TurnHost, session: &SessionState) -> String {
    let settings = host.tui_settings();
    let show_model = settings.shows("model");
    let label = footer_model(host, session);
    let mut parts = Vec::new();
    if show_model && !label.is_empty() {
        parts.push(label);
    }
    if settings.shows("ctx") {
        if let Some(fraction) = session.context_fraction {
            // Never show a hollow 0%; round up so a started window reads >= 1%.
            let percent = ((fraction * 100.0).ceil() as u32).clamp(1, 100);
            parts.push(format!("{percent}% ctx"));
        }
        // Usage arrived but the model's window is unknown.
        else if session.last_prompt.is_some() {
            parts.push("? ctx".to_string());
        }
    }
    if settings.shows("cost") {
        parts.push(
            session
                .cost_usd
                .map(|cost| format!("${cost:.4}"))
                .unwrap_or_else(|| "cost unavailable".to_string()),
        );
    }
    if settings.shows("cache") {
        if let Some(part) = cache_footer_part(session.last_prompt) {
            parts.push(part);
        }
    }
    if let Some(tools) = session.running_tools.chip() {
        parts.push(tools);
    }
    if let Some(running) = session.subagents.footer_summary() {
        parts.push(running);
    }
    if let Some(chip) = session.panes.footer_chip() {
        parts.push(chip);
    }
    parts.join(" \u{b7} ")
}

/// `cache 92%`: the provider cache hit rate of the latest usage report.
/// `prompt` is every input bucket (uncached + cache read + cache write), so
/// the ratio is the hit rate. A visible 0% on a later turn is the cheapest
/// signal that the cache broke.
fn cache_footer_part(last_prompt: Option<(i64, i64)>) -> Option<String> {
    let (prompt, cached) = last_prompt?;
    if prompt <= 0 {
        return None;
    }
    let hit = (cached.max(0).saturating_mul(100) / prompt).clamp(0, 100);
    Some(format!("cache {hit}%"))
}

/// One footer row: hints (or startup progress, or the agent breadcrumb once
/// descended) on the left, the model on the right.
/// The footer's left-edge permission-mode marker. `None` for `Ask` (the
/// default shows nothing), like Claude Code's mode line.
fn permission_mode_footer_label(mode: rightkit_agent_policy::PermissionMode) -> &'static str {
    use rightkit_agent_policy::PermissionMode;
    match mode {
        PermissionMode::Ask => "Ask",
        PermissionMode::AcceptEdits => "⏵⏵ Accept edits",
        PermissionMode::AutoSafe => "⏵⏵ Auto",
        PermissionMode::Bypass => "⚠ Yolo",
        PermissionMode::Plan => "⏸ Plan",
    }
}

/// Current git branch (or short detached sha) for `dir`, read from `.git/HEAD`.
fn git_branch(dir: &str) -> Option<String> {
    let mut path = std::path::PathBuf::from(dir);
    loop {
        let head = path.join(".git").join("HEAD");
        if let Ok(text) = std::fs::read_to_string(&head) {
            let text = text.trim();
            return Some(match text.strip_prefix("ref: refs/heads/") {
                Some(branch) => branch.to_string(),
                None => text.chars().take(7).collect(),
            });
        }
        if !path.pop() {
            return None;
        }
    }
}

/// Keep the right-hand end of `text` within `max` display columns, marking
/// the cut with a leading `…`.
fn left_truncate(text: &str, max: usize) -> String {
    use rightkit_terminal_ui::transcript::wrap::display_width;
    if display_width(text) <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut tail: Vec<char> = Vec::new();
    let mut used = 1;
    for ch in text.chars().rev() {
        let w = display_width(&ch.to_string());
        if used + w > max {
            break;
        }
        used += w;
        tail.push(ch);
    }
    tail.reverse();
    format!("…{}", tail.into_iter().collect::<String>())
}

/// Static marker for startup progress in the footer.
const STARTUP_GLYPH: &str = "•";
/// Seconds a startup phase must run before the footer shows a clock for it.
const STARTUP_CLOCK_AFTER_SECS: u64 = 5;

/// The one persistent footer row: `<cwd> · <branch> · <Mode>` on the left,
/// `<model> · <effort>` on the right. The cwd truncates from the left at
/// narrow widths. Startup progress, a vendor warning, or the agent
/// breadcrumb appends after the mode; `shortcuts` (F1) swaps in the key list.
#[allow(clippy::too_many_arguments)]
fn footer_line(
    width: u16,
    cwd: &str,
    branch: Option<&str>,
    model: &str,
    mode: rightkit_agent_policy::PermissionMode,
    readiness: Option<&HostReadiness>,
    crumb: Option<&str>,
    warning: Option<&str>,
    goal: Option<&str>,
    shortcuts: bool,
    startup_secs: u64,
    theme: &Theme,
) -> rightkit_terminal_ui::ratatui::text::Line<'static> {
    use rightkit_agent_policy::PermissionMode;
    use rightkit_terminal_ui::component::Role;
    use rightkit_terminal_ui::ratatui::style::{Modifier, Style};
    use rightkit_terminal_ui::ratatui::text::{Line, Span};
    use rightkit_terminal_ui::transcript::wrap::display_width;
    let dim = Style::default().fg(theme.color(Role::Dim));
    let bright = Style::default().fg(theme.color(Role::Text));
    // Startup progress is a static glyph in the brand accent and dim text,
    // never an animated or blue spinner; a clock joins it once the wait is
    // long enough to matter.
    let progress = |label: &str| {
        let clock = if startup_secs >= STARTUP_CLOCK_AFTER_SECS {
            format!(" {}:{:02}", startup_secs / 60, startup_secs % 60)
        } else {
            String::new()
        };
        format!("{STARTUP_GLYPH} {label}{clock}")
    };
    let mut extra = match (warning.or(crumb), readiness) {
        (Some(text), _) => Some((text.to_string(), Role::Dim)),
        (None, Some(HostReadiness::Configuring)) => {
            Some((progress("Resolving provider…"), Role::Dim))
        }
        (None, Some(HostReadiness::StartingContext)) => {
            Some((progress("Starting context…"), Role::Dim))
        }
        (None, Some(HostReadiness::Degraded { reason })) => Some((
            format!("⚠ context off · {}", context_off_code(reason)),
            Role::Attention,
        )),
        (None, Some(HostReadiness::Failed { .. })) => Some((
            "✗ Context unavailable · submit to retry".to_string(),
            Role::Attention,
        )),
        _ => None,
    };
    let mode_label = permission_mode_footer_label(mode);
    let mode_style = match mode {
        PermissionMode::Ask | PermissionMode::Plan => bright,
        PermissionMode::AcceptEdits | PermissionMode::AutoSafe => {
            Style::default().fg(theme.color(Role::Success))
        }
        PermissionMode::Bypass => Style::default()
            .fg(theme.color(Role::Failure))
            .add_modifier(Modifier::BOLD),
    };
    let (model_name, effort) = match model.split_once(" · ") {
        Some((m, e)) => (m.to_string(), Some(e.to_string())),
        None => (model.to_string(), None),
    };
    let mut right = vec![Span::styled(model_name, bright)];
    if let Some(effort) = effort {
        right.push(Span::styled(format!(" · {effort}"), dim));
    }
    right.push(Span::raw(" "));
    let right_width: usize = right
        .iter()
        .map(|s| display_width(s.content.as_ref()))
        .sum();
    let mut left: Vec<Span<'static>>;
    if shortcuts {
        left = vec![Span::styled(
            " Enter queue/send · Tab push · Shift+Tab mode · Esc cancel · ctrl+x unqueue · /help"
                .to_string(),
            dim,
        )];
    } else {
        let branch_text = branch.map(|b| format!(" · {b}")).unwrap_or_default();
        // The route/model/usage block outranks the status note: when the row
        // cannot hold both, clip the note (keeping a short cwd) instead of
        // dropping the right block.
        const MIN_CWD: usize = 8;
        let reserved = 1
            + MIN_CWD
            + display_width(&branch_text)
            + 3
            + display_width(mode_label)
            + 3
            + right_width
            + 1;
        let room = usize::from(width).saturating_sub(reserved).max(4);
        // The goal outranks the status note (e.g. "context off"): clip the
        // note first so the goal stays readable, dropping it when no room
        // is left for even a short fragment.
        if let Some(goal) = goal {
            let status_room = room.saturating_sub(display_width(goal) + 3);
            let drop_status = extra
                .as_ref()
                .is_some_and(|(text, _)| display_width(text) > status_room && status_room < 4);
            if drop_status {
                extra = None;
            } else if let Some((text, _)) = extra
                .as_mut()
                .filter(|(text, _)| display_width(text) > status_room)
            {
                let mut clipped = String::new();
                let mut used = 0usize;
                for ch in text.chars() {
                    let w = display_width(&ch.to_string());
                    if used + w + 1 > status_room {
                        break;
                    }
                    used += w;
                    clipped.push(ch);
                }
                clipped.push('…');
                *text = clipped;
            }
            match extra.as_mut() {
                Some((text, _)) => {
                    text.push_str(" · ");
                    text.push_str(goal);
                }
                None => extra = Some((goal.to_string(), Role::Dim)),
            }
        }
        if let Some((text, _)) = extra.as_mut() {
            if display_width(text) > room {
                let mut clipped = String::new();
                let mut used = 0usize;
                for ch in text.chars() {
                    let w = display_width(&ch.to_string());
                    if used + w + 1 > room {
                        break;
                    }
                    used += w;
                    clipped.push(ch);
                }
                clipped.push('…');
                *text = clipped;
            }
        }
        let extra_text = extra
            .as_ref()
            .map(|(t, _)| format!(" · {t}"))
            .unwrap_or_default();
        let fixed = 1
            + display_width(&branch_text)
            + 3
            + display_width(mode_label)
            + display_width(&extra_text)
            + right_width
            + 1;
        let cwd_budget = usize::from(width).saturating_sub(fixed).max(4);
        let cwd = left_truncate(&rightkit_terminal_ui::render::sanitize(cwd), cwd_budget);
        left = vec![
            Span::raw(" "),
            Span::styled(format!("{cwd}{branch_text} · "), dim),
            Span::styled(mode_label.to_string(), mode_style),
        ];
        if let Some((text, role)) = extra {
            // Startup progress leads with its glyph in the brand accent.
            match text.strip_prefix(STARTUP_GLYPH) {
                Some(rest) => {
                    left.push(Span::styled(" · ".to_string(), dim));
                    left.push(Span::styled(
                        STARTUP_GLYPH.to_string(),
                        Style::default().fg(theme.accent()),
                    ));
                    left.push(Span::styled(
                        rest.to_string(),
                        Style::default().fg(theme.color(role)),
                    ));
                }
                None => left.push(Span::styled(
                    format!(" · {text}"),
                    Style::default().fg(theme.color(role)),
                )),
            }
        }
    }
    let left_width: usize = left.iter().map(|s| display_width(s.content.as_ref())).sum();
    let gap = usize::from(width).saturating_sub(left_width + right_width);
    let mut spans = left;
    if gap > 0 && right_width > 1 {
        spans.push(Span::raw(" ".repeat(gap)));
        spans.extend(right);
    }
    rightkit_terminal_ui::render::cell::clipped_line(Line::from(spans), width)
}

/// Pi footer's cwd projection, clipped through the absorbed Codex helper.
#[cfg_attr(not(test), allow(dead_code))]
fn directory_footer_line(
    width: u16,
    directory: Option<&str>,
    theme: &Theme,
) -> rightkit_terminal_ui::ratatui::text::Line<'static> {
    use rightkit_terminal_ui::ratatui::style::Style;
    use rightkit_terminal_ui::ratatui::text::{Line, Span};
    let directory = directory.unwrap_or("no directory selected");
    let directory = rightkit_terminal_ui::render::display_path_str(directory);
    let directory = rightkit_terminal_ui::render::sanitize(&directory);
    let spans = vec![Span::styled(
        format!("  {directory}"),
        Style::default().fg(theme.color(rightkit_terminal_ui::component::Role::Text)),
    )];
    rightkit_terminal_ui::render::cell::clipped_line(Line::from(spans), width)
}

/// Startup lockup: the wordmark with the accent on "Right" plus the version,
/// then one readiness line. No logo mark, no tagline.
fn welcome_lines(
    width: u16,
    model: &str,
    theme: &Theme,
) -> Vec<rightkit_terminal_ui::ratatui::text::Line<'static>> {
    use rightkit_terminal_ui::component::Role;
    use rightkit_terminal_ui::ratatui::style::{Modifier, Style};
    use rightkit_terminal_ui::ratatui::text::{Line, Span};
    let dim = Style::default().fg(theme.color(Role::Dim));
    let bold = Modifier::BOLD;
    let mut lines = vec![Line::from(vec![
        Span::raw("  "),
        Span::styled(
            "Code",
            Style::default()
                .fg(theme.color(Role::Text))
                .add_modifier(bold),
        ),
        Span::styled(
            "Right",
            Style::default().fg(theme.accent()).add_modifier(bold),
        ),
        Span::styled(format!("  {}", env!("CARGO_PKG_VERSION")), dim),
    ])];
    if model.trim().is_empty() {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("!", Style::default().fg(theme.color(Role::Attention))),
            Span::raw(" no model yet"),
        ]));
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled("/providers", Style::default().add_modifier(bold)),
            Span::styled(" — sign in with a vendor CLI, or paste an API key", dim),
        ]));
    } else {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("✓", Style::default().fg(theme.color(Role::Success))),
            Span::raw(" ready"),
            Span::styled(format!(" · {model}"), dim),
        ]));
    }
    lines.push(Line::from(""));
    lines
        .into_iter()
        .map(|line| rightkit_terminal_ui::render::cell::clipped_line(line, width))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn render(
    host: &impl TurnHost,
    term: &mut Terminal<CrosstermBackend<io::Stdout>>,
    surface: &mut Surface,
    composer: &mut Composer,
    nav: &mut NavView,
    transcript: &mut Transcript,
    theme: &Theme,
    session: &mut SessionState,
    layout: LayoutMode,
    requester: &FrameRequester,
    state: &mut RenderState,
) {
    // S:I6 — the strip is a projection of the typed timeline plus the
    // host's committed/pending route state, refreshed once per frame so
    // attribution can never lag the authority it reads. Effort and pending
    // selection come from committed route state, never the draft catalog.
    session.identity.set_effort(host.committed_route().effort);
    // The directory shell calls run in, for the approval card.
    session.approval_cwd = host
        .active_directory()
        .map(|dir| rightkit_terminal_ui::render::display_path_str(&dir));
    session.identity.set_pending(host.pending_route_label());
    session.identity.sync(&session.timeline);
    if session.settle_stale_live_tools(transcript) {
        flush_history(transcript, surface, requester);
    }

    // ── geometry lifecycle ───────────────────────────────────────────────
    // One place owns the transition, in this order:
    //   1. mode changes swap the real alternate screen;
    //   2. a viewport that grew past the terminal bottom scrolls the screen
    //      so the rows it displaces land in real scrollback (donor
    //      `grow_viewport`) — this also covers the launch overflow, where
    //      the launch cursor left fewer free rows than the viewport needs;
    //   3. the released bands of the old footprint are erased, sparing the
    //      band a history slide just filled;
    //   4. ratatui is resized once per actual change — never per frame;
    //   5. the draw repaints the new footprint and lands the cursor.
    // Real transcript/scrollback is terminal-owned and never cleared here.
    // #126: the attach pane owns the alternate screen while attached.
    if session.pty.is_some() && surface.modal().is_none() {
        surface.enter_modal(ModalKind::Transcript);
        session.pty_modal = true;
    } else if session.pty.is_none() && session.pty_modal {
        surface.leave_modal();
        session.pty_modal = false;
    }
    let screen = surface.screen_size();
    let mode = surface.mode();

    if std::mem::take(&mut session.clear_screen_requested) && mode == ScreenMode::Inline {
        // Ctrl+L: wipe the visible screen, then repaint the live footprint whole.
        // The session, its history and its scrollback are untouched.
        if let Err(error) = term.clear_visible_screen() {
            note_render_fault(state, session, transcript, format!("clear screen: {error}"));
            return;
        }
        term.invalidate_viewport();
    }

    // Persisted fullscreen must enter its terminal modes on the first frame too.
    if state.mode != Some(mode) && (state.mode.is_some() || mode == ScreenMode::Alternate) {
        let mut out = io::stdout().lock();
        let transition = match mode {
            ScreenMode::Alternate if surface.modal() == Some(ModalKind::Fullscreen) => {
                rightkit_terminal_ui::terminal::session::enter_fullscreen_screen(&mut out)
            }
            ScreenMode::Alternate => {
                rightkit_terminal_ui::terminal::session::enter_alternate_screen(&mut out)
            }
            ScreenMode::Inline => {
                rightkit_terminal_ui::terminal::session::leave_alternate_screen(&mut out)
            }
        };
        if let Err(error) = transition {
            note_render_fault(
                state,
                session,
                transcript,
                format!("screen transition: {error}"),
            );
            return;
        }
        term.invalidate_viewport();
    }

    if mode == ScreenMode::Inline {
        if state.welcome_shown && !layout.is_linear() {
            surface.anchor_to_bottom();
        }
        let want = inline_viewport_height(composer, session, transcript, theme, screen.0, screen.1);
        // Unconditional: a no-change call is cheap, and the launch anchor's
        // pending scroll (or a drained state) must still be reconciled.
        surface.set_viewport_height(want);
        // Emit the owed growth scroll only on a same-screen inline frame.
        // On the first frame `state` is empty, which is exactly the launch
        // case that must scroll; after a modal leave the alternate-buffer
        // restore re-created the rows, and after a resize the terminal
        // reflowed them — both drop the owed scroll instead.
        let same_screen =
            state.mode != Some(ScreenMode::Alternate) && state.screen.is_none_or(|s| s == screen);
        let mut out = io::stdout().lock();
        if same_screen {
            let _ = surface.emit_pending_scroll(&mut out, (0, surface.viewport().top));
        } else {
            surface.drop_pending_scroll();
        }
        let _ = out.flush();
    }

    let rect = inline_rect(surface);
    if state.rect != Some(rect) || state.mode != Some(mode) {
        if state.mode == Some(ScreenMode::Inline) && mode == ScreenMode::Inline {
            if let Some(old) = state.rect {
                // Bands released above the new footprint hold stale paint —
                // except the tail a history slide just filled, which is real
                // scrollback-bound content.
                let slide = surface.take_history_slide();
                if state.screen != Some(screen) {
                    // Codex clears from the earlier viewport top on resize:
                    // terminal reflow has already moved old paint, so row-band
                    // subtraction using pre-resize coordinates leaves ghosts.
                    // Codex's pending_viewport_area adjusts that old top by
                    // the cursor displacement caused by reflow. Windows can
                    // query console position without racing the input reader.
                    #[cfg(windows)]
                    let old_top = term.get_cursor_position().map_or(old.y, |cursor| {
                        // Cursor displacement locates the old top only if the
                        // viewport moved rigidly. A narrower window also wraps
                        // the wide rows above the cursor (rules, the working
                        // line, queue hints), so the true top sits that many
                        // rows higher than displacement alone says.
                        let cursor_row =
                            usize::from(term.last_known_cursor_pos.y.saturating_sub(old.y));
                        let reflow = reflow_rows_above(&state.drawn_rows, cursor_row, screen.0);
                        let estimate = (i32::from(old.y) + i32::from(cursor.y)
                            - i32::from(term.last_known_cursor_pos.y)
                            - i32::from(reflow))
                        .clamp(0, i32::from(screen.1.saturating_sub(1)))
                            as u16;
                        // The estimate can land below the real top; the
                        // screen itself says where the old frame's rows are.
                        stale_top_above(
                            estimate,
                            &state.drawn_rows,
                            screen.0,
                            &state.recent_history,
                            |row| rightkit_terminal_ui::terminal::visible_row_text(row, screen.0),
                        )
                    });
                    #[cfg(not(windows))]
                    let old_top = old.y;
                    let position =
                        rightkit_terminal_ui::ratatui::layout::Position::new(0, old_top.min(rect.y));
                    if let Err(error) = term.clear_after_position(position) {
                        note_render_fault(
                            state,
                            session,
                            transcript,
                            format!("resize clear: {error}"),
                        );
                        return;
                    }
                    // The terminal kept the history top-anchored while the
                    // viewport moved: close the gap so the prompt stays
                    // attached to the transcript instead of floating below
                    // a band of blank rows.
                    #[cfg(windows)]
                    {
                        let gap = blank_gap_above(rect.y, |row| {
                            rightkit_terminal_ui::terminal::visible_row_text(row, screen.0)
                        });
                        if gap > 0 {
                            let mut out = io::stdout().lock();
                            let _ = rightkit_terminal_ui::terminal::history::slide_history_down(
                                &mut out, rect.y, gap,
                            );
                            let _ = out.flush();
                        }
                    }
                } else {
                    let mut out = io::stdout().lock();
                    let _ = erase_released_rows(&mut out, old, rect, slide);
                    if slide == 0 {
                        let _ = rightkit_terminal_ui::terminal::history::close_released_band(
                            &mut out,
                            screen.1,
                            old.y,
                            old.height,
                            rect.y,
                            rect.height,
                        );
                    }
                    let _ = out.flush();
                }
            } else {
                surface.take_history_slide();
            }
        } else {
            surface.take_history_slide();
        }
        if let Err(e) = term.resize(rightkit_terminal_ui::ratatui::layout::Size::new(
            screen.0, screen.1,
        )) {
            note_render_fault(state, session, transcript, format!("resize: {e}"));
            return;
        }
        term.set_viewport_area(rect);
        term.invalidate_viewport();
        state.rect = Some(rect);
        state.mode = Some(mode);
        state.screen = Some(screen);
    }

    // History belongs immediately above the new footprint, after growth,
    // shrink & stale-paint erasure, before the frame paints its live content.
    if mode == ScreenMode::Inline {
        let history = surface.take_pending_history();
        if !history.is_empty() {
            for line in &history {
                let text: String = line
                    .line()
                    .spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect();
                let text = text.trim();
                if !text.is_empty() {
                    state.recent_history.push_back(text.to_string());
                }
            }
            while state.recent_history.len() > 48 {
                state.recent_history.pop_front();
            }
            insert_scrollback_lines(surface, &history);
            term.invalidate_viewport();
        }
        let rect = inline_rect(surface);
        if state.rect != Some(rect) {
            term.set_viewport_area(rect);
            term.invalidate_viewport();
            state.rect = Some(rect);
        }
    }

    session.selection_lines = transcript.inspection_lines(screen.0);
    let started = Instant::now();
    let draw_result = term.draw_with_size(
        rightkit_terminal_ui::ratatui::layout::Size::new(screen.0, screen.1),
        |frame| {
            let area = frame.area();
            match surface.mode() {
                _ if session.pty.is_some() => {
                    if let Some(pty) = session.pty.as_ref() {
                        let body = Rect { height: area.height.saturating_sub(1), ..area };
                        if let Some((x, y)) =
                            rightkit_terminal_ui::pty_attach::render_grid(pty.screen(), frame.buffer_mut(), body)
                        {
                            frame.set_cursor_position((x, y));
                        }
                    }
                }
                ScreenMode::Alternate if surface.modal() != Some(ModalKind::Fullscreen) => {
                    let body = Rect {
                        height: area.height.saturating_sub(1),
                        ..area
                    };
                    if let Some(inspection) = session.inspection.as_mut() {
                        inspection.render(frame, body, theme);
                    } else if let Some(picker) = session.session_action_picker.as_mut() {
                        picker.render(frame, body, theme);
                    } else {
                        nav.render(frame, body, theme);
                    }
                }
                ScreenMode::Alternate | ScreenMode::Inline => {
                    let composer_rows = composer.desired_height(area.width, area.height);
                    // No subagents → no rows: an empty panel must never
                    // steal the composer's top rule.
                    let subagent_rows = if !layout.is_linear() {
                        if let Some(root) = nav.viewed_agent() {
                            session.subagents.set_tree(root);
                        }
                        session.subagents.rows()
                    } else {
                        0
                    };
                    if area.height > composer_rows.saturating_add(subagent_rows) {
                        // Claude-style input frame, packed from the bottom:
                        // the footer owns the last row, then rule, composer,
                        // rule, working line, queue list; whatever is left
                        // above is the transcript tail. Spare rows therefore
                        // always sit above the input, never below the footer.
                        let layout_rects = chrome_layout(
                            area,
                            composer_rows,
                            subagent_rows,
                            session.working_started.is_some() || session.failed_hold.is_some(),
                            session.queue_list_lines(area.width, theme).len() as u16,
                        );
                        let status_height = layout_rects.status.height + layout_rects.queue.height;
                        let status_area = layout_rects.status;
        let queue_area = layout_rects.queue;
                        let working_area = layout_rects.working;
                        let rule_top = layout_rects.rule_top;
                        let composer_area = layout_rects.composer;
                        let rule_bottom = layout_rects.rule_bottom;
                        let transcript_height = status_area.height;
                        // Projection refresh only; attribution now renders in
                        // the footer, so the one-shot boundary flag is drained
                        // here instead of by a strip render.
                        let _ = session.identity.boundary_crossed();
                        // S:I7 — Plain mode is append-only: settled lines reach
                        // the user through `flush_history`'s scrollback inserts,
                        // so nothing above the composer is redrawn in place. The
                        // region is cleared rather than left to keep whatever the
                        // previous frame painted there.
                        if layout.is_linear() {
                            frame.render_widget(
                                rightkit_terminal_ui::ratatui::widgets::Clear,
                                Rect {
                                    x: area.x,
                                    y: area.y,
                                    width: area.width,
                                    height: status_height,
                                },
                            );
                            composer.render(frame, composer_area, theme);
                            if layout_rects.subagents.height > 0 {
                                session
                                    .subagents
                                    .render(frame, layout_rects.subagents, theme);
                            }
                        } else {
                            // The live tail is two projections: the typed
                            // still-moving cells (reasoning, running tool/exec)
                            // drawn on top, then the unflushed transcript
                            // messages (echoed prompt, notices) below. Cells
                            // render through their own `Cell::display_lines`;
                            // transcript through `Transcript`'s own Component —
                            // no ad-hoc re-implementation of either.
                            let cell_lines = session.live_cell_lines(area.width, theme);
                            let cells_height = (cell_lines.len() as u16).min(transcript_height);
                            let cells_area = Rect {
                                x: status_area.x,
                                y: status_area.y,
                                width: status_area.width,
                                height: cells_height,
                            };
                            let transcript_area = Rect {
                                x: status_area.x,
                                y: status_area.y + cells_height,
                                width: status_area.width,
                                height: transcript_height - cells_height,
                            };
                            session.selection_area = Some(transcript_area);
                            if cells_height > 0 {
                                frame.render_widget(
                                    rightkit_terminal_ui::ratatui::widgets::Paragraph::new(cell_lines),
                                    cells_area,
                                );
                            }
                            if transcript_area.height > 0 {
                                transcript.render(frame, transcript_area, theme);
                                if let Some(((start_line, start_col), (end_line, end_col))) =
                                    (surface.mode() == ScreenMode::Alternate
                                        && surface.modal() == Some(ModalKind::Fullscreen))
                                    .then(|| selection_bounds(session))
                                    .flatten()
                                {
                                    let visible_start = session.selection_lines.len().saturating_sub(transcript_area.height as usize);
                                    let buffer = frame.buffer_mut();
                                    for line_index in start_line..=end_line {
                                        if line_index < visible_start || line_index >= visible_start + transcript_area.height as usize { continue; }
                                        let row = transcript_area.y + (line_index - visible_start) as u16;
                                        let line = &session.selection_lines[line_index];
                                        let chars: Vec<char> = line.chars().collect();
                                        let from = if line_index == start_line { start_col } else { 0 };
                                        let to = if line_index == end_line { end_col } else { chars.len() };
                                        for column in from..to.min(chars.len()) {
                                            let x = transcript_area.x + column as u16;
                                            if x < transcript_area.x + transcript_area.width {
                                                buffer.get_mut(x, row).set_style(rightkit_terminal_ui::ratatui::style::Style::default().add_modifier(rightkit_terminal_ui::ratatui::style::Modifier::REVERSED));
                                            }
                                        }
                                    }
                                }
                            }
                            let rule = composer_rule(area.width, theme);
                            if queue_area.height > 0 {
                                let mut queue_lines = session.queue_list_lines(area.width, theme);
                                let skip = queue_lines
                                    .len()
                                    .saturating_sub(usize::from(queue_area.height));
                                queue_lines.drain(..skip);
                                frame.render_widget(
                                    rightkit_terminal_ui::ratatui::widgets::Paragraph::new(queue_lines),
                                    queue_area,
                                );
                            }
                            if working_area.height > 0 {
                                let model_line = footer_model(host, session);
                                // The actor follows the latest route decision
                                // when the stream supplied one.
                                let actor = match session.identity.attribution() {
                                    Some(a)
                                        if session.authoritative_route && !a.model.is_empty() =>
                                    {
                                        a.model.clone()
                                    }
                                    _ => model_line.split(" · ").next().unwrap_or("").to_string(),
                                };
                                let waiting = session.approval_overlay.current_request().is_some();
                                let running = session.working_started.is_some();
                                let held = if running {
                                    None
                                } else {
                                    session.failed_hold.as_ref()
                                };
                                let held_secs = held.map_or(0, |(_, secs)| *secs);
                                let view = held
                                    .map(|(signal, _)| signal)
                                    .or(session.working_signal.as_ref())
                                    .map(rightkit_terminal_ui::render::status::signal_view);
                                let default_op = session.default_operation();
                                let status = rightkit_terminal_ui::render::status::WorkingStatus {
                                    state: if waiting {
                                        rightkit_terminal_ui::render::status::WorkState::Waiting
                                    } else {
                                        view.as_ref().map_or(
                                            rightkit_terminal_ui::render::status::WorkState::Working,
                                            |v| v.state,
                                        )
                                    },
                                    actor: &actor,
                                    operation: if waiting {
                                        "Waiting for you · approve command"
                                    } else {
                                        view.as_ref()
                                            .map_or(default_op.as_str(), |v| v.operation.as_str())
                                    },
                                    short: if waiting {
                                        None
                                    } else {
                                        view.as_ref().map(|v| v.short.as_str())
                                    },
                                    verb: if waiting {
                                        None
                                    } else {
                                        view.as_ref().map(|v| v.verb.as_str())
                                    },
                                    elapsed_secs: session
                                        .working_started
                                        .map_or(held_secs, |start| start.elapsed().as_secs()),
                                    stages: None,
                                    queued: composer.queued_count(),
                                    dismissable: held.is_some(),
                                };
                                frame.render_widget(
                                    rightkit_terminal_ui::ratatui::widgets::Paragraph::new(
                                        rightkit_terminal_ui::render::status::working_status_line(
                                            &status,
                                            working_area.width,
                                            theme,
                                        ),
                                    ),
                                    working_area,
                                );
                            }
                            if rule_top.height > 0 {
                                frame.render_widget(
                                    rightkit_terminal_ui::ratatui::widgets::Paragraph::new(rule.clone()),
                                    rule_top,
                                );
                            }
                            composer.render(frame, composer_area, theme);
                            if layout_rects.subagents.height > 0 {
                                session.subagents.render(frame, layout_rects.subagents, theme);
                            }
                            if rule_bottom.height > 0 {
                                frame.render_widget(
                                    rightkit_terminal_ui::ratatui::widgets::Paragraph::new(rule),
                                    rule_bottom,
                                );
                            }
                        }
                        // Stage-3 overlays draw last, over the live region:
                        // the route picker fills the status band; the
                        // approval modal anchors its own box to the bottom
                        // of the area it is given (donor bottom-pane style).
                        if let Some(picker) = session.model_picker.as_mut() {
                            frame
                                .render_widget(rightkit_terminal_ui::ratatui::widgets::Clear, status_area);
                            picker.render(frame, status_area, theme);
                        }
                        if let Some(picker) = session.providers_picker.as_mut() {
                            frame
                                .render_widget(rightkit_terminal_ui::ratatui::widgets::Clear, status_area);
                            picker.render(frame, status_area, theme);
                        }
                        if let Some(picker) = session.repository_picker.as_mut() {
                            frame
                                .render_widget(rightkit_terminal_ui::ratatui::widgets::Clear, status_area);
                            picker.render(frame, status_area, theme);
                        }
                        if let Some(flow) = session.rewind.as_mut() {
                            frame
                                .render_widget(rightkit_terminal_ui::ratatui::widgets::Clear, status_area);
                            flow.picker_mut().render(frame, status_area, theme);
                        }
                        if let Some(active) = session.proj.picker.as_mut() {
                            let target = if active.kind == crate::tui_projects::PickKind::Sidebar {
                                // Left overlay: the transcript stays visible
                                // to its right.
                                Rect {
                                    width: crate::tui_projects::sidebar_width(status_area.width),
                                    ..status_area
                                }
                            } else {
                                status_area
                            };
                            frame.render_widget(rightkit_terminal_ui::ratatui::widgets::Clear, target);
                            active.picker.render(frame, target, theme);
                        }
                        if session.approval_overlay.current_request().is_some() {
                            session
                                .approval_overlay
                                .set_model_label(Some(footer_model(host, session)));
                            session.approval_overlay.render(
                                frame,
                                Rect {
                                    height: area.height.saturating_sub(1),
                                    ..area
                                },
                                theme,
                            );
                        }
                    } else {
                        composer.render(frame, area, theme);
                    }
                }
            }
            if let Some(text) = session.interaction_text() {
                use rightkit_terminal_ui::ratatui::widgets::{Clear, Paragraph};
                frame.render_widget(Clear, area);
                frame.render_widget(
                    Paragraph::new(text),
                    Rect {
                        height: area.height.saturating_sub(1),
                        ..area
                    },
                );
            }
            // Directory follows active admission scope, including overlays.
            if area.height > 0 && !layout.is_linear() {
                let directory = host.active_directory();
                let settings = &session.proj.settings;
                let directory_area = Rect {
                    x: area.x,
                    y: area.bottom() - 1,
                    width: area.width,
                    height: 1,
                };
                frame.render_widget(rightkit_terminal_ui::ratatui::widgets::Clear, directory_area);
                frame.render_widget(
                    rightkit_terminal_ui::ratatui::widgets::Paragraph::new({
                        let crumb = (!nav.viewing_root()).then(|| nav.breadcrumb());
                        let cwd = if settings.shows("cwd") {
                            directory
                                .clone()
                                .unwrap_or_else(|| "no directory selected".to_string())
                        } else {
                            String::new()
                        };
                        let cwd = rightkit_terminal_ui::render::display_path_str(&cwd);
                        let branch = settings.shows("branch").then(|| git_branch(&cwd)).flatten();
                        let goal_chip = settings.shows("goal").then(|| host.goal_chip()).flatten();
                        footer_line(
                            area.width,
                            &cwd,
                            branch.as_deref(),
                            &footer_right(host, session),
                            host.permission_mode(),
                            session.readiness.as_ref(),
                            crumb.as_deref(),
                            host.context_degraded_reason()
                                .map(|reason| format!("⚠ context off · {}", context_off_code(&reason)))
                                .or_else(|| host.permission_warning().map(str::to_string))
                                .as_deref(),
                            goal_chip.as_deref(),
                            composer.shortcut_overlay_visible(),
                            session.started_at.elapsed().as_secs(),
                            theme,
                        )
                    }),
                    directory_area,
                );
            }
        },
    );
    match draw_result {
        Ok(_) => {
            state.last_error = None;
            state.drawn_rows = term.drawn_row_text();
        }
        Err(e) => note_render_fault(state, session, transcript, format!("draw: {e}")),
    }
    // The welcome card goes to real scrollback once, above the live UI, as
    // soon as the surface can accept history inserts.
    if !state.welcome_shown
        && surface.mode() == ScreenMode::Inline
        && !layout.is_linear()
        && surface.can_insert_history()
    {
        state.welcome_shown = true;
        let lines: Vec<HyperlinkLine> =
            welcome_lines(screen.0, &footer_model(host, session), theme)
                .into_iter()
                .map(HyperlinkLine::from)
                .collect();
        insert_scrollback_lines(surface, &lines);
        // The insertion moved terminal rows under the live viewport; repaint
        // it whole so the composer's top rule is never left displaced.
        term.invalidate_viewport();
        requester.schedule_frame();
    }
    requester.record_frame(started.elapsed(), Duration::from_millis(16));
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("tui_host_interaction_tests.rs");

    #[cfg(windows)]
    thread_local! {
        static RESTORE_BYTES: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    // Preserve actual native writes while recording bytes before ConPTY
    // normalizes mouse, cursor & screen control sequences. Test binaries only.
    #[cfg(windows)]
    pub(super) struct RestoreOutput<W: io::Write>(pub(super) W);

    #[cfg(windows)]
    impl<W: io::Write> io::Write for RestoreOutput<W> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let written = self.0.write(bytes)?;
            RESTORE_BYTES.with(|record| record.borrow_mut().extend_from_slice(&bytes[..written]));
            Ok(written)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }

    // Runs only as a child of the ConPTY qualification test below. No fault
    // injection or environment switch is compiled into the shipped CLI.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn terminal_guard_fault_child() {
        let kind = std::env::var("RIGHTKIT_TERMINAL_TEST_CHILD").expect("owned test child");
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetStdHandle(which: u32) -> *mut std::ffi::c_void;
            fn GetConsoleMode(handle: *mut std::ffi::c_void, mode: *mut u32) -> i32;
        }
        fn input_mode() -> u32 {
            let mut mode = 0;
            // SAFETY: this child owns a live ConPTY console & a valid out pointer.
            assert_ne!(
                unsafe { GetConsoleMode(GetStdHandle((-10_i32) as u32), &mut mode) },
                0
            );
            mode
        }
        fn disturb_terminal() {
            io::stdout()
                .write_all(b"\x1b[?1049h\x1b[?1000h\x1b[?1006h\x1b[?25l\x1b[2;10r")
                .unwrap();
            io::stdout().flush().unwrap();
        }
        let before = input_mode();
        if kind == "init-error" {
            let result = TerminalGuard::engage_with(|| {
                rightkit_terminal_ui::terminal::session::set_modes()?;
                assert_ne!(
                    input_mode(),
                    before,
                    "initialization must change real console mode"
                );
                disturb_terminal();
                Err(io::Error::other("owned partial-initialization fault"))
            });
            assert!(result.is_err());
        } else {
            let result = std::panic::catch_unwind(|| {
                let _guard = TerminalGuard::engage().unwrap();
                assert_ne!(input_mode(), before, "guard must change real console mode");
                disturb_terminal();
                if kind == "panic" {
                    panic!("owned recoverable terminal fault");
                }
                assert_eq!(kind, "normal");
            });
            assert_eq!(result.is_err(), kind == "panic");
        }
        assert_eq!(
            input_mode(),
            before,
            "original console input mode must be restored"
        );
        RESTORE_BYTES.with(|record| {
            let bytes = record.borrow();
            let text = String::from_utf8_lossy(&bytes);
            for reset in [
                "\x1b[?1000l",
                "\x1b[?1006l",
                "\x1b[?1049l",
                "\x1b[?25h",
                "\x1b[r",
            ] {
                assert!(
                    text.contains(reset),
                    "{kind}: missing emitted reset {reset:?}: {text:?}"
                );
            }
            println!(
                "TERMINAL_RESET_BYTES:{}",
                serde_json::to_string(&*bytes).unwrap()
            );
        });
        println!("TERMINAL_RESTORED:{kind}");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn terminal_guard_native_restores_after_normal_init_error_and_panic() {
        use rightkit_sandbox::execution::{
            CommandLaunchSpec, ExecutionChannel, ProcessLaunchAdapter, ProcessLaunchContext,
            PtySize,
        };
        use tokio::io::AsyncReadExt;
        let exe = std::env::current_exe().unwrap();
        let cwd = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        for kind in ["normal", "init-error", "panic"] {
            let spec = CommandLaunchSpec {
                program: exe.to_string_lossy().into_owned(),
                argv: vec![
                    "--exact".into(),
                    "tui_host::tests::terminal_guard_fault_child".into(),
                    "--ignored".into(),
                    "--nocapture".into(),
                    "--test-threads=1".into(),
                ],
                cwd: cwd.clone(),
                context: ProcessLaunchContext {
                    workspace_root: cwd.clone(),
                    sandbox_profile: Some("owned-terminal-qualification".into()),
                    sandbox_required: false,
                    network_required: false,
                },
                env: vec![("RIGHTKIT_TERMINAL_TEST_CHILD".into(), kind.into())],
            };
            let mut child = rightkit_agent_tools::coding_tools::process_launch::UnconfinedProcessLaunchAdapter
                .spawn_channel(
                    spec,
                    ExecutionChannel::Pty(PtySize {
                        cols: 100,
                        rows: 30,
                        ..PtySize::default()
                    }),
                )
                .await
                .unwrap();
            let mut reader = child.take_stdout().unwrap();
            let output = tokio::spawn(async move {
                let mut output = Vec::new();
                reader.read_to_end(&mut output).await.unwrap();
                output
            });
            let exit = tokio::time::timeout(std::time::Duration::from_secs(30), child.wait()).await;
            if exit.is_err() {
                let _ = child.cancel().await;
            }
            let output = tokio::time::timeout(std::time::Duration::from_secs(5), output)
                .await
                .unwrap()
                .unwrap();
            let text = String::from_utf8_lossy(&output);
            assert_eq!(
                exit.expect("bounded terminal child").unwrap(),
                0,
                "{kind}: {text:?}"
            );
            assert!(
                text.contains(&format!("TERMINAL_RESTORED:{kind}")),
                "{kind}: {text}"
            );
            assert!(
                text.contains("TERMINAL_RESET_BYTES:"),
                "{kind}: no pre-ConPTY reset evidence"
            );
            assert!(
                text.contains("\x1b[?2004l"),
                "{kind}: bracketed paste was not disabled: {text:?}"
            );
        }
    }

    fn key(ch: char) -> io::Result<ct_event::Event> {
        Ok(ct_event::Event::Key(KeyEvent::new(
            KeyCode::Char(ch),
            KeyModifiers::NONE,
        )))
    }

    fn queued_key(index: usize) -> char {
        char::from_u32('a' as u32 + index as u32).unwrap()
    }

    #[test]
    fn input_queue_has_fixed_capacity_and_rejects_growth() {
        let (tx, mut rx) = mpsc::channel(INPUT_QUEUE_CAPACITY);
        assert_eq!(tx.max_capacity(), INPUT_QUEUE_CAPACITY);
        for index in 0..INPUT_QUEUE_CAPACITY {
            assert!(tx.try_send(key(queued_key(index))).is_ok());
        }
        assert!(matches!(
            tx.try_send(key('0')),
            Err(mpsc::error::TrySendError::Full(_))
        ));
        assert_eq!(
            rx.try_recv().unwrap().unwrap(),
            ct_event::Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE,))
        );
    }

    #[test]
    fn draw_notifications_use_a_bounded_drop_oldest_ring() {
        let (tx, mut rx) = broadcast::channel::<()>(DRAW_QUEUE_CAPACITY);
        for _ in 0..=DRAW_QUEUE_CAPACITY {
            tx.send(()).expect("draw receiver remains subscribed");
        }

        assert!(matches!(
            rx.try_recv(),
            Err(broadcast::error::TryRecvError::Lagged(skipped))
                if skipped >= 1
        ));
    }

    #[test]
    fn freeing_one_slot_delivers_held_event_after_fifo_prefix() {
        let pause = InputPause::new();
        let (tx, mut rx) = mpsc::channel(INPUT_QUEUE_CAPACITY);
        for index in 0..INPUT_QUEUE_CAPACITY {
            assert!(tx.try_send(key(queued_key(index))).is_ok());
        }
        assert_eq!(
            rx.try_recv().unwrap().unwrap(),
            ct_event::Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE,))
        );
        assert_eq!(
            send_input_event(&pause, &tx, key('z')),
            InputSendOutcome::Delivered
        );
        for index in 1..INPUT_QUEUE_CAPACITY {
            let event = rx.try_recv().unwrap().unwrap();
            assert_eq!(
                event,
                ct_event::Event::Key(KeyEvent::new(
                    KeyCode::Char(queued_key(index)),
                    KeyModifiers::NONE,
                ))
            );
        }
        assert_eq!(
            rx.try_recv().unwrap().unwrap(),
            ct_event::Event::Key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE,))
        );
    }

    #[test]
    fn pause_interrupts_full_queue_and_discards_only_held_event() {
        let pause = InputPause::new();
        let (tx, mut rx) = mpsc::channel(INPUT_QUEUE_CAPACITY);
        for index in 0..INPUT_QUEUE_CAPACITY {
            assert!(tx.try_send(key(queued_key(index))).is_ok());
        }
        let parker = pause.clone();
        let reader = std::thread::spawn(move || loop {
            if parker.reader_park() {
                return;
            }
            std::thread::yield_now();
        });
        let sender_pause = pause.clone();
        let sender = std::thread::spawn(move || send_input_event(&sender_pause, &tx, key('!')));
        assert!(pause.request_with_deadline(Duration::from_secs(1)));
        assert_eq!(sender.join().unwrap(), InputSendOutcome::DiscardedForPause);
        pause.resume();
        reader.join().unwrap();
        assert_eq!(
            rx.try_recv().unwrap().unwrap(),
            ct_event::Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE,))
        );
        while rx.try_recv().is_ok() {}
        assert!(
            rx.try_recv().is_err(),
            "held stale event must not enter queue"
        );
    }

    #[test]
    fn closed_receiver_terminates_producer_path() {
        let pause = InputPause::new();
        let (tx, rx) = mpsc::channel(INPUT_QUEUE_CAPACITY);
        drop(rx);
        assert_eq!(
            send_input_event(&pause, &tx, key('x')),
            InputSendOutcome::ReceiverClosed
        );
    }

    #[test]
    fn translate_preserves_mouse_and_focus_events() {
        let mouse = ct_event::MouseEvent {
            kind: ct_event::MouseEventKind::ScrollDown,
            column: 4,
            row: 2,
            modifiers: ct_event::KeyModifiers::NONE,
        };
        assert!(matches!(
            translate(ct_event::Event::Mouse(mouse)),
            Some(TuiEvent::Mouse(event)) if event.kind == ct_event::MouseEventKind::ScrollDown
        ));
        assert!(matches!(
            translate(ct_event::Event::FocusGained),
            Some(TuiEvent::Focus(true))
        ));
        assert!(matches!(
            translate(ct_event::Event::FocusLost),
            Some(TuiEvent::Focus(false))
        ));
    }

    struct QuitHost;

    #[test]
    fn todo_overlay_tracks_todo_write_and_seeds_from_durable_tasks() {
        let mut session = SessionState::new("first");
        let mut transcript = Transcript::new();
        session.record(
            &mut transcript,
            DaemonEvent::ToolStarted {
                call_id: "c1".into(),
                name: "todo_write".into(),
                args_preview: r#"{"todos":[{"content":"A","status":"completed"},{"content":"B","status":"pending"}]}"#.into(),
            },
        );
        assert_eq!(session.todo_overlay.items().len(), 2);
        assert_eq!(session.todo_overlay.done_count(), 1);

        // After compaction/resume the durable task graph reseeds the list.
        let mut resumed = SessionState::new("first");
        let node = |i: usize, title: &str, state| rightkit_agent_engine_protocol::TaskNode {
            id: format!("todo-{i}"),
            title: title.into(),
            deps: vec![],
            done_when: String::new(),
            done_when_predicates: vec![],
            state,
            category: "todo".into(),
            estimated_duration: String::new(),
            assigned_role: String::new(),
            escalation_path: String::new(),
            open_questions: vec![],
            created_at: "t".into(),
            updated_at: "t".into(),
        };
        resumed.seed_todo_from_tasks(&[
            node(0, "A", rightkit_agent_engine_protocol::TaskState::Done),
            node(1, "B", rightkit_agent_engine_protocol::TaskState::InProgress),
        ]);
        assert_eq!(resumed.todo_overlay.items().len(), 2);
        assert_eq!(resumed.todo_overlay.done_count(), 1);
    }

    #[test]
    fn task_graph_event_rebuilds_plan_after_resume() {
        let mut session = SessionState::new("first");
        let mut transcript = Transcript::new();
        session.record(
            &mut transcript,
            DaemonEvent::TaskGraph {
                todos: vec![
                    ("A".into(), "completed".into()),
                    ("B".into(), "in_progress".into()),
                ],
            },
        );
        assert_eq!(session.todo_overlay.items().len(), 2);
        assert_eq!(session.todo_overlay.done_count(), 1);
    }

    #[test]
    fn approval_keys_are_explicit_and_scoped_to_current_session() {
        let mut session = SessionState::new("first");
        let mut transcript = Transcript::new();
        session.record(
            &mut transcript,
            DaemonEvent::ApprovalRequested {
                request_id: "one".into(),
                summary: "first tool".into(),
            },
        );
        session.active_session = "second".into();
        let allow = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL);
        assert_eq!(session.approval_key(&allow), None);
        session.record(
            &mut transcript,
            DaemonEvent::ApprovalRequested {
                request_id: "two".into(),
                summary: "second tool".into(),
            },
        );
        assert_eq!(session.approval_key(&allow), Some((1, true)));
        assert_eq!(
            session.approval_key(&KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)),
            Some((1, false))
        );
        let mut repeat = allow;
        repeat.kind = KeyEventKind::Repeat;
        assert_eq!(session.approval_key(&repeat), None);
        assert_eq!(
            session.approval_key(&KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)),
            None
        );
        session.record(
            &mut transcript,
            DaemonEvent::ApprovalResolved {
                request_id: "two".into(),
                approved: false,
            },
        );
        assert_eq!(session.approval_key(&allow), None);
        session.active_session = "first".into();
        assert_eq!(session.approval_key(&allow), Some((0, true)));
    }

    #[test]
    fn chat_chrome_shifted_release_never_inserts_unshifted_slash() {
        for (press, release) in [('?', '/'), ('A', 'a'), ('!', '1'), ('"', '\'')] {
            let mut pressed = std::collections::HashSet::new();
            let first = ct_event::Event::Key(KeyEvent::new_with_kind(
                KeyCode::Char(press),
                KeyModifiers::SHIFT,
                KeyEventKind::Press,
            ));
            normalize_alt_code_release(&mut pressed, first);
            let last = ct_event::Event::Key(KeyEvent::new_with_kind(
                KeyCode::Char(release),
                KeyModifiers::NONE,
                KeyEventKind::Release,
            ));
            match normalize_alt_code_release(&mut pressed, last) {
                ct_event::Event::Key(key) => assert_eq!(key.kind, KeyEventKind::Release),
                other => panic!("expected release, got {other:?}"),
            }
            assert!(pressed.is_empty());
        }
    }

    #[test]
    fn alt_code_release_char_is_promoted_to_press() {
        let mut pressed = std::collections::HashSet::new();
        // ConPTY delivers '·' (OEM-mappable, absent from the layout) as an
        // Alt-numpad sequence whose char rides the Alt-release record --
        // crossterm surfaces a Release-kind Char with no prior Press.
        let orphan_release = ct_event::Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('·'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));
        match normalize_alt_code_release(&mut pressed, orphan_release) {
            ct_event::Event::Key(key) => assert_eq!(key.kind, KeyEventKind::Press),
            other => panic!("expected key event, got {other:?}"),
        }
        // Ordinary press-then-release pairs keep their real kinds so a held
        // char never double-inserts.
        for kind in [KeyEventKind::Press, KeyEventKind::Release] {
            match normalize_alt_code_release(
                &mut pressed,
                ct_event::Event::Key(KeyEvent::new_with_kind(
                    KeyCode::Char('a'),
                    KeyModifiers::NONE,
                    kind,
                )),
            ) {
                ct_event::Event::Key(key) => assert_eq!(key.kind, kind),
                other => panic!("expected key event, got {other:?}"),
            }
        }
        // A modified release stays a release even without a known press.
        match normalize_alt_code_release(
            &mut pressed,
            ct_event::Event::Key(KeyEvent::new_with_kind(
                KeyCode::Char('z'),
                KeyModifiers::CONTROL,
                KeyEventKind::Release,
            )),
        ) {
            ct_event::Event::Key(key) => assert_eq!(key.kind, KeyEventKind::Release),
            other => panic!("expected key event, got {other:?}"),
        }
    }

    struct RefusingCancelHost;

    impl TurnHost for RefusingCancelHost {
        fn spawn_turn(&self, _: String) -> JoinHandle<i32> {
            tokio::spawn(std::future::pending())
        }
        fn resolve_command(&self, _: &str) -> Option<String> {
            None
        }
        fn check_command(&self, _: &str, _: bool) -> Result<(), String> {
            Ok(())
        }
        async fn execute_command(&self, _: &str, _: &str) -> bool {
            false
        }
        fn completions(&self, _: &str) -> Vec<String> {
            vec![]
        }
        async fn cancel_turn(&self) -> Result<(), String> {
            Err("engine still active".into())
        }
    }

    #[tokio::test]
    async fn cancellation_refusal_preserves_presentation_handle() {
        let mut turn = Some(RefusingCancelHost.spawn_turn("old".into()));
        assert_eq!(
            cancel_active_turn(&RefusingCancelHost, &mut turn)
                .await
                .unwrap_err(),
            "engine still active"
        );
        assert!(!turn.as_ref().unwrap().is_finished());
        cancel_active_turn(&QuitHost, &mut turn).await.unwrap();
        assert!(turn.is_none());
    }

    impl TurnHost for QuitHost {
        async fn cancel_turn(&self) -> Result<(), String> {
            Ok(())
        }
        fn spawn_turn(&self, _prompt: String) -> JoinHandle<i32> {
            tokio::spawn(async { 0 })
        }

        fn resolve_command(&self, input: &str) -> Option<String> {
            (input == "/quit" || input == "quit").then(|| "/quit".to_string())
        }

        fn check_command(&self, _name: &str, _mid_turn: bool) -> Result<(), String> {
            Ok(())
        }

        async fn execute_command(&self, name: &str, rest: &str) -> bool {
            assert_eq!(name, "/quit");
            assert!(rest.is_empty());
            true
        }

        fn completions(&self, _prefix: &str) -> Vec<String> {
            Vec::new()
        }
    }

    #[tokio::test]
    async fn quit_command_signals_event_loop_exit() {
        let mut composer = Composer::new();
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        let mut surface = Surface::new(
            80,
            24,
            17,
            7,
            rightkit_terminal_ui::terminal::InsertMode::ScrollRegion,
        );
        let interjections = InterjectionBuffer::new();
        let mut pending_echoes = std::collections::VecDeque::new();
        let mut turn = None;
        let (draw_tx, _) = broadcast::channel(1);
        let requester = rightkit_terminal_ui::scheduler::spawn(draw_tx);

        assert!(
            handle_submit(
                &QuitHost,
                "/quit".to_string(),
                &mut composer,
                &mut NavView::new(),
                &mut session,
                &mut transcript,
                &mut surface,
                &interjections,
                &mut pending_echoes,
                &mut None,
                &mut HistoryPaging::default(),
                &mut turn,
                &requester,
            )
            .await
        );
        assert!(turn.is_none());
    }

    #[tokio::test]
    async fn bare_registered_words_are_submitted_as_prompts() {
        for text in ["quit", "help", "run ordinary prompt"] {
            let mut composer = Composer::new();
            let mut session = SessionState::new(LOCAL_SESSION_ID);
            let mut transcript = Transcript::new();
            let mut surface = Surface::new(
                80,
                24,
                17,
                7,
                rightkit_terminal_ui::terminal::InsertMode::ScrollRegion,
            );
            let interjections = InterjectionBuffer::new();
            let mut pending_echoes = std::collections::VecDeque::new();
            let mut turn = None;
            let (draw_tx, _) = broadcast::channel(1);
            let requester = rightkit_terminal_ui::scheduler::spawn(draw_tx);

            assert!(
                !handle_submit(
                    &QuitHost,
                    text.to_string(),
                    &mut composer,
                    &mut NavView::new(),
                    &mut session,
                    &mut transcript,
                    &mut surface,
                    &interjections,
                    &mut pending_echoes,
                    &mut None,
                    &mut HistoryPaging::default(),
                    &mut turn,
                    &requester,
                )
                .await
            );
            assert!(turn.is_some(), "bare {text:?} must start a prompt turn");
            assert!(
                session.timeline.entries().iter().any(|entry| matches!(
                    entry,
                    rightkit_terminal_ui::transcript::timeline::TimelineEntry::User { text: value }
                        if value == text
                )),
                "bare {text:?} must be echoed as user text"
            );
            if let Some(handle) = turn.take() {
                handle.abort();
            }
        }
    }

    #[tokio::test]
    async fn reset_settles_turn_before_clearing_state() {
        let mut composer = Composer::new();
        composer.set_streaming(true);
        let mut nav = NavView::new();
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        let interjections = InterjectionBuffer::new();
        let (draw_tx, _) = broadcast::channel(1);
        let requester = rightkit_terminal_ui::scheduler::spawn(draw_tx);
        let mut turn = Some(QuitHost.spawn_turn("old".into()));
        let mut viewed_agent = None;
        let mut paging = HistoryPaging::default();

        apply_command_effect(
            &QuitHost,
            CommandEffect::Reset,
            &mut composer,
            &mut nav,
            &mut session,
            &mut transcript,
            &interjections,
            &mut turn,
            &requester,
            &mut viewed_agent,
            &mut paging,
        )
        .await;

        assert!(
            turn.is_none(),
            "successful reset must settle and drop handle"
        );
        assert!(settled_transcript_text(&mut transcript).contains("cancelled the in-flight turn"));
    }

    #[tokio::test]
    async fn reset_refusal_preserves_busy_turn_handle() {
        let mut composer = Composer::new();
        composer.set_streaming(true);
        let mut nav = NavView::new();
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        let interjections = InterjectionBuffer::new();
        let (draw_tx, _) = broadcast::channel(1);
        let requester = rightkit_terminal_ui::scheduler::spawn(draw_tx);
        let mut turn = Some(RefusingCancelHost.spawn_turn("old".into()));
        let mut viewed_agent = None;
        let mut paging = HistoryPaging::default();

        apply_command_effect(
            &RefusingCancelHost,
            CommandEffect::Reset,
            &mut composer,
            &mut nav,
            &mut session,
            &mut transcript,
            &interjections,
            &mut turn,
            &requester,
            &mut viewed_agent,
            &mut paging,
        )
        .await;

        assert!(
            turn.is_some(),
            "failed reset must retain presentation handle"
        );
        assert!(settled_transcript_text(&mut transcript).contains("engine still active"));
        turn.unwrap().abort();
    }

    /// A fake host whose daemon feed still holds the canonical tail of a
    /// cancelled turn: the events `wait_turn` would have drained had the
    /// presentation handle not been aborted by Ctrl-C.
    struct TailHost {
        queued: std::sync::Mutex<std::collections::VecDeque<DaemonEvent>>,
    }

    impl TurnHost for TailHost {
        fn spawn_turn(&self, _: String) -> JoinHandle<i32> {
            tokio::spawn(async { 0 })
        }
        async fn cancel_turn(&self) -> Result<(), String> {
            Ok(())
        }
        fn resolve_command(&self, _: &str) -> Option<String> {
            None
        }
        fn check_command(&self, _: &str, _: bool) -> Result<(), String> {
            Ok(())
        }
        async fn execute_command(&self, _: &str, _: &str) -> bool {
            false
        }
        fn completions(&self, _: &str) -> Vec<String> {
            vec![]
        }
        async fn poll_timeline_events(&self) -> Vec<DaemonEvent> {
            self.queued.lock().unwrap().drain(..).collect()
        }
    }

    /// Regression for the cancel-tail ordering defect: Ctrl-C aborts the
    /// presentation handle, so `wait_turn` never drains the turn's queued
    /// canonical tail. The next idle submission must fold that tail through
    /// `record_polled_event` BEFORE recording its local echo — otherwise the
    /// old assistant tail and interrupted notice paint after the new `You`
    /// block — and the tail's durable `userMessageSubmitted` echo of the
    /// earlier prompt must still be consumed by `pending_echoes`, not
    /// rendered as a duplicate.
    #[tokio::test]
    async fn idle_submit_drains_cancelled_turn_tail_before_local_echo() {
        let host = TailHost {
            queued: std::sync::Mutex::new(
                vec![
                    DaemonEvent::UserMessage {
                        text: "old prompt".into(),
                    },
                    DaemonEvent::AssistantDelta {
                        token: "tail".into(),
                    },
                    DaemonEvent::AssistantSettled,
                    DaemonEvent::System {
                        text: "Turn interrupted.".into(),
                    },
                ]
                .into(),
            ),
        };
        let mut composer = Composer::new();
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        let mut surface = Surface::new(
            80,
            24,
            17,
            7,
            rightkit_terminal_ui::terminal::InsertMode::ScrollRegion,
        );
        let interjections = InterjectionBuffer::new();
        // The earlier prompt was echoed locally; its canonical
        // userMessageSubmitted is still queued in the tail.
        let mut pending_echoes = std::collections::VecDeque::from(["old prompt".to_string()]);
        let mut turn = None;
        let (draw_tx, _) = broadcast::channel(1);
        let requester = rightkit_terminal_ui::scheduler::spawn(draw_tx);

        assert!(
            !handle_submit(
                &host,
                "fresh prompt".to_string(),
                &mut composer,
                &mut NavView::new(),
                &mut session,
                &mut transcript,
                &mut surface,
                &interjections,
                &mut pending_echoes,
                &mut None,
                &mut HistoryPaging::default(),
                &mut turn,
                &requester,
            )
            .await
        );
        assert!(turn.is_some());

        use rightkit_terminal_ui::transcript::timeline::TimelineEntry;
        let entries = session.timeline.entries();
        assert_eq!(
            entries,
            &[
                TimelineEntry::Assistant {
                    text: "tail".into(),
                    finalized: true,
                },
                TimelineEntry::System {
                    text: "Turn interrupted.".into(),
                },
                TimelineEntry::User {
                    text: "fresh prompt".into(),
                },
            ],
            "the cancelled turn's tail must settle before the new user entry"
        );
        assert_eq!(
            pending_echoes.as_slices().0,
            &["fresh prompt".to_string()],
            "the old prompt's canonical echo is consumed, not rendered twice"
        );
        // The timeline assert above also proves the echo dedup held: a
        // missed suppression would have recorded a second `User` entry for
        // "old prompt" before the tail deltas.
    }

    // ── S:I10 completion catalog ──────────────────────────────────────

    /// The popup's catalog is derived from the command-effects registry, so
    /// it must cover exactly that registry and nothing else: one row per
    /// effect, named and described by the registry itself.
    #[test]
    fn completion_catalog_covers_every_command_effect() {
        let catalog = command_effect_catalog(false);
        assert_eq!(catalog.len(), CommandEffect::ALL.len());
        for (candidate, effect) in catalog.iter().zip(CommandEffect::ALL.iter()) {
            assert_eq!(candidate.name, effect.name());
            assert_eq!(candidate.description, effect.description());
        }
    }

    /// A turn in flight disables `/new` (the popup refuses it too, via
    /// `apply_command_effect`); rewind carries its reason always.
    #[test]
    fn a_busy_session_disables_new_and_always_explains_rewind() {
        let idle = command_effect_catalog(false);
        let busy = command_effect_catalog(true);
        let reason = |c: &[CompletionCandidate], name: &str| {
            c.iter()
                .find(|c| c.name == name)
                .and_then(|c| c.disabled_reason.clone())
        };
        assert_eq!(reason(&idle, "new"), None);
        assert_eq!(reason(&busy, "new").as_deref(), Some("a turn is in flight"));
        assert!(reason(&idle, "rewind").is_some());
        assert!(reason(&busy, "rewind").is_some());
        // Nothing else is disabled: the other three always have real work.
        for name in ["reset", "undo", "redo"] {
            assert_eq!(reason(&busy, name), None, "{name} must stay enabled");
        }
    }

    /// S:P10 production-bound acceptance: the host's live catalog builder
    /// reaches the composer popup, including disabled command rows.
    #[test]
    fn host_refresh_catalog_reaches_composer_acceptance_guard() {
        let mut composer = Composer::new();
        let session = SessionState::new(LOCAL_SESSION_ID);
        refresh_completion_catalog(&mut composer, &session, true);
        composer.set_text("/new");

        let row = composer
            .completion()
            .selected_candidate()
            .expect("live host catalog must expose /new");
        assert_eq!(row.name, "new");
        assert!(row.disabled_reason.is_some());
        assert!(!composer.accept_completion());
        assert_eq!(composer.cursor_line_text(), "/new");
    }

    // ── S:I3 typed timeline / S:I6 identity strip ─────────────────────

    /// A recorded event lands in BOTH structures: the transcript keeps its
    /// scrollback line, the timeline keeps the typed entry.
    #[test]
    fn recording_a_session_event_feeds_the_transcript_and_the_timeline() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.record(
            &mut transcript,
            DaemonEvent::UserMessage {
                text: "hello".into(),
            },
        );
        session.notice(&mut transcript, "a notice");

        assert_eq!(session.timeline.len(), 2);
        assert!(matches!(
            session.timeline.entries()[0],
            rightkit_terminal_ui::transcript::timeline::TimelineEntry::User { .. }
        ));
        let live: Vec<&Message> = transcript.live_messages().collect();
        assert_eq!(live.len(), 2);
        assert_eq!(live[1].content, "a notice");
    }

    #[test]
    fn production_assistant_stream_releases_stable_prefix_and_settles_once() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        let chunks = ["stable **tail", "** plus [link", "](url)"];
        let expected: String = chunks.concat();

        for chunk in chunks {
            session.record(
                &mut transcript,
                DaemonEvent::AssistantDelta {
                    token: chunk.into(),
                },
            );
        }
        let live = transcript.live_messages().next().unwrap();
        // The link closes with the final chunk (`](url)`), so by REN-003's
        // own rule — release stable prefixes, withhold only syntactically
        // incomplete tails — it is safe to release before settle: nothing
        // later in the stream can un-close an already-closed `]( ... )`.
        // Only the bold span and the link's open bracket were ever
        // incomplete, and both closed within these three chunks.
        assert_eq!(live.content, "stable **tail** plus [link](url)");
        assert!(!live.finalized);

        session.record(&mut transcript, DaemonEvent::AssistantSettled);
        let settled = transcript.live_messages().next().unwrap();
        assert_eq!(settled.content, expected);
        assert!(settled.finalized);

        let drained = transcript.drain_finalized(80);
        assert!(!drained.is_empty());
        assert!(transcript.drain_finalized(80).is_empty());
    }

    #[test]
    fn native_harness_completion_without_deltas_renders_finalized_text() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.record(
            &mut transcript,
            DaemonEvent::AssistantCompleted {
                text: "No model provider is active.".into(),
            },
        );
        let messages: Vec<_> = transcript.live_messages().collect();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "No model provider is active.");
        assert!(messages[0].finalized);
        assert!(matches!(
            &session.timeline.entries()[0],
            rightkit_terminal_ui::transcript::timeline::TimelineEntry::Assistant { text, finalized: true }
                if text == "No model provider is active."
        ));
    }

    #[test]
    fn native_harness_completion_reconciles_stream_without_duplicate_prose() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.record(
            &mut transcript,
            DaemonEvent::AssistantDelta {
                token: "Finished **pending".into(),
            },
        );
        session.record(
            &mut transcript,
            DaemonEvent::AssistantCompleted {
                text: "Finished **edit**.".into(),
            },
        );
        let messages: Vec<_> = transcript.live_messages().collect();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "Finished **edit**.");
        assert!(messages[0].finalized);
        assert_eq!(session.timeline.len(), 1);
        assert!(matches!(
            &session.timeline.entries()[0],
            rightkit_terminal_ui::transcript::timeline::TimelineEntry::Assistant { text, finalized: true }
                if text == "Finished **edit**."
        ));
    }

    /// S:I6 end to end without a terminal: a route decision in the timeline
    /// is what the strip attributes, and a model change marks a boundary.
    #[test]
    fn a_changed_route_decision_is_attributed_and_marks_a_boundary() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        let mut nav = NavView::new();
        let tree = |model: &str| AgentNode {
            id: AgentId(LOCAL_AGENT_ID.to_string()),
            name: "root agent".to_string(),
            model: model.to_string(),
            elapsed_cost: String::new(),
            state: AgentState::Running,
            children: Vec::new(),
        };

        // A nav model that does not know its model is not an attribution.
        nav.set_tree(tree("-"));
        sync_route(&nav, &mut session, &mut transcript);
        assert!(session.timeline.latest_route().is_none());

        nav.set_tree(tree("opus"));
        sync_route(&nav, &mut session, &mut transcript);
        session.identity.sync(&session.timeline);
        assert_eq!(
            session.identity.attribution().map(|a| a.model.as_str()),
            Some("opus")
        );
        assert!(
            !session.identity.boundary_crossed(),
            "the first attribution is a start, not a boundary"
        );
        assert_eq!(
            session.identity.line_at_width(80),
            format!("{} · root agent/opus", LOCAL_SESSION_ID)
        );

        nav.set_tree(tree("sonnet"));
        sync_route(&nav, &mut session, &mut transcript);
        session.identity.sync(&session.timeline);
        assert!(
            session.identity.boundary_crossed(),
            "a model change is a boundary"
        );
    }

    // ── S:I7 layout mode ──────────────────────────────────────────────

    /// The environment half of `resolve_layout_mode`, kept pure so it needs
    /// no process-env mutation, combined with the flag half.
    #[test]
    fn layout_mode_resolves_from_flags_then_environment() {
        assert_eq!(env_layout_signals(Some("1"), Some("dumb")), (true, true));
        assert_eq!(env_layout_signals(Some(""), Some("xterm")), (true, false));
        assert_eq!(env_layout_signals(None, Some("DUMB")), (false, true));
        assert_eq!(env_layout_signals(None, None), (false, false));

        assert_eq!(
            LayoutMode::resolve(false, false, false, false),
            LayoutMode::Normal
        );
        assert_eq!(
            LayoutMode::resolve(false, false, true, false),
            LayoutMode::ReducedMotion
        );
        assert_eq!(
            LayoutMode::resolve(true, false, true, true),
            LayoutMode::Plain
        );
        // Only Plain is linear and only Normal animates, which is exactly
        // what the render loop and the maintenance tick branch on.
        assert!(LayoutMode::Plain.is_linear());
        assert!(!LayoutMode::ReducedMotion.is_linear());
        assert!(LayoutMode::Normal.animates());
        assert!(!LayoutMode::ReducedMotion.animates());
    }

    /// The resolved mode reaches the composer, so both the composer's own
    /// rendering and the host's frame layout agree on linear vs animated.
    #[test]
    fn the_resolved_layout_mode_reaches_the_composer() {
        let mut composer = Composer::new();
        assert_eq!(composer.layout_mode(), LayoutMode::Normal);

        composer.set_layout_mode(LayoutMode::resolve(false, false, true, false));
        assert_eq!(composer.layout_mode(), LayoutMode::ReducedMotion);
        assert!(
            !composer.layout_mode().animates(),
            "reduced motion must stop the composer animating too"
        );
    }

    // ── S:I7 per-view drafts ──────────────────────────────────────────

    /// The view key a draft is filed under is the session, or the session
    /// plus the agent being viewed, and switching views moves the draft.
    #[test]
    fn switching_views_stashes_and_restores_the_per_view_draft() {
        use rightkit_terminal_ui::composer::draft_store::DraftStore;

        // Save/take, the primitive the composer's draft store gives the host.
        let mut store = DraftStore::new();
        store.save("local", "unsent");
        assert!(store.has_draft("local"));
        assert_eq!(store.take("local").as_deref(), Some("unsent"));
        assert_eq!(store.take("local"), None, "take must not serve it twice");

        let session = SessionState::new(LOCAL_SESSION_ID);
        let agent = AgentId("worker-1".to_string());
        assert_eq!(view_key(&session, None), LOCAL_SESSION_ID);
        assert_eq!(
            view_key(&session, Some(&agent)),
            format!("{LOCAL_SESSION_ID}/worker-1")
        );

        let mut composer = Composer::new();
        composer.set_text("half-typed prompt");
        switch_view(
            &mut composer,
            &view_key(&session, None),
            &view_key(&session, Some(&agent)),
        );
        assert!(
            composer.is_empty(),
            "the draft left the composer with the view it was typed in"
        );
        composer.set_text("text typed while viewing the agent");
        switch_view(
            &mut composer,
            &view_key(&session, Some(&agent)),
            &view_key(&session, None),
        );
        assert_eq!(
            composer.cursor_line_text(),
            "half-typed prompt",
            "returning to a view restores exactly what was typed in it"
        );
    }

    /// Restoring a draft happens after normal event dispatch, so the host
    /// switch seam must refresh completion against the restored buffer.
    #[test]
    fn switching_views_resynchronizes_a_restored_command_popup() {
        let mut composer = Composer::new();
        composer.set_text("/new");
        assert!(composer.completion_open());
        switch_view(&mut composer, "local", "local/worker-1");
        assert!(!composer.completion_open());
        switch_view(&mut composer, "local/worker-1", "local");
        assert!(composer.completion_open());
        assert_eq!(composer.cursor_line_text(), "/new");
    }

    /// Drift guard, binary side: every tool name the production registry
    /// builds must have an intentional entry in [`PRODUCTION_RENDER_CLASSES`].
    /// A new built-in tool added to the default tool catalog without a mapping here
    /// fails this test instead of silently rendering as an unclassified
    /// Generic forever. This is possible without a `coderight-tui` ->
    /// `coderight-tools` dependency because the mapping table lives in this
    /// binary, which already depends on both crates.
    #[test]
    fn every_production_tool_name_has_an_intentional_render_class() {
        let capabilities = rightkit_agent_tools::coding_tools::default_tool_capabilities();
        let mut unmapped: Vec<String> = Vec::new();
        for tool in capabilities {
            if !PRODUCTION_RENDER_CLASSES
                .iter()
                .any(|(name, _)| *name == tool.name)
            {
                unmapped.push(tool.name);
            }
        }
        assert!(
            unmapped.is_empty(),
            "production tools missing from PRODUCTION_RENDER_CLASSES: {unmapped:?}"
        );
    }

    /// Reverse drift: every mapped name must still be a real production tool,
    /// so renaming a tool surfaces here as an entry to update rather than a
    /// stale registration that never matches.
    #[test]
    fn every_mapped_render_class_name_is_a_real_production_tool() {
        let capabilities = rightkit_agent_tools::coding_tools::default_tool_capabilities();
        // `powershell` is conditionally present when an interpreter exists;
        // `wasm_compute` is gated on `wasm_runtime_configured()`, which is
        // false while no WASM runtime is packaged. Their absence here is
        // environmental, not drift.
        const CONDITIONALLY_REGISTERED: &[&str] = &["powershell", "wasm_compute"];
        let stale: Vec<&str> = PRODUCTION_RENDER_CLASSES
            .iter()
            .filter(|(name, _)| {
                !CONDITIONALLY_REGISTERED.contains(name)
                    && !capabilities.iter().any(|tool| tool.name == *name)
            })
            .map(|(name, _)| *name)
            .collect();
        assert!(
            stale.is_empty(),
            "mapped names that no longer exist in the default tool catalog: {stale:?}"
        );
    }

    /// End to end, P1-4: the registration drive() performs in production —
    /// derived live from `coderight-tools default metadata projection` and mapped through
    /// `tool_render_class_to_tui`, with NO static-table re-registration on
    /// top — must make `classify` resolve every production tool to exactly
    /// the intentional class recorded in [`PRODUCTION_RENDER_CLASSES`]. The
    /// static table is the expected-value fixture; the live registry is the
    /// source of what gets registered.
    #[test]
    fn live_registry_derived_registration_resolves_through_classify() {
        let classes = rightkit_agent_tools::coding_tools::default_tool_render_classes();
        let registered: std::collections::HashSet<String> = classes
            .iter()
            .map(|(name, _)| name.to_ascii_lowercase())
            .collect();
        // Mirror drive()'s metadata-only production registration exactly.
        let owned: Vec<(&'static str, RenderClass)> = classes
            .into_iter()
            .map(|(name, class)| {
                let leaked: &'static str = Box::leak(name.into_boxed_str());
                (leaked, tool_render_class_to_tui(class))
            })
            .collect();
        rightkit_terminal_ui::register_render_classes(owned);

        for (name, expected) in PRODUCTION_RENDER_CLASSES {
            // Conditionally registered by the default tool catalog itself:
            // `powershell` only when a PowerShell interpreter exists on the
            // host, `wasm_compute` only when a WASM runtime is packaged.
            // Their absence is environmental, not a drift signal.
            if matches!(*name, "powershell" | "wasm_compute") && !registered.contains(*name) {
                continue;
            }
            assert!(
                registered.contains(*name),
                "{name} has an intentional class but the default tool catalog did not produce it"
            );
            assert_eq!(
                rightkit_terminal_ui::render::classify(name),
                *expected,
                "classify({name:?}) did not resolve to its registered class"
            );
        }
    }

    /// The local ids the nav intents are matched against must be exactly the
    /// ids `refresh_nav` feeds into the nav model -- otherwise Stop and
    /// SwitchSession would refuse their own host.
    #[test]
    fn refresh_nav_feeds_the_ids_the_intent_handlers_match() {
        let mut nav = NavView::new();
        refresh_nav(&mut nav, true, "test-model");

        let tree = nav.viewed_agent().expect("refresh_nav must load a tree");
        assert_eq!(tree.id, AgentId(LOCAL_AGENT_ID.to_string()));
        assert_eq!(
            tree.state,
            AgentState::Running,
            "turn in flight must show running"
        );
        assert!(
            tree.children.is_empty(),
            "the local tree has no subagents yet"
        );

        // The sessions list contains exactly the current session, under the id
        // SwitchSession compares against. (Read back through stop_scope's
        // sibling accessor; NavView exposes no direct session getter.)
        assert_eq!(
            nav.stop_scope(&AgentId(LOCAL_AGENT_ID.to_string())),
            vec![AgentId(LOCAL_AGENT_ID.to_string())],
            "the local root's whole scope is itself"
        );
    }

    /// P1-7 immediate-interrupt semantics, as pure decisions:
    /// steer interrupts nothing; cancel and re-prompt abort the in-flight
    /// turn NOW; a stop discards everything queued around it; the re-prompt
    /// replacement carries the full framing envelope with truncation.
    #[test]
    fn mid_turn_directives_plan_immediate_interruption() {
        use rightkit_terminal_ui::interject::Kind;

        // Steer stays queued — nothing interrupts the in-flight turn.
        assert!(matches!(
            plan_immediate_interrupt(Kind::Steer, "ignored", 3),
            ImmediateInterrupt::None
        ));

        // Cancel aborts now and discards the whole queue.
        let ImmediateInterrupt::AbortAndDiscardAll { notice } =
            plan_immediate_interrupt(Kind::Cancel, "", 2)
        else {
            panic!("cancel must interrupt immediately");
        };
        assert!(notice.contains("Turn cancelled."));
        assert!(notice.contains("2 queued messages were discarded."));

        // Re-prompt aborts now and spawns the framed replacement immediately.
        let framed = InterjectionBuffer::frame_reprompt("do this instead");
        assert!(framed.contains("<user_query>"));
        let ImmediateInterrupt::AbortAndReplace {
            framed: got,
            notice,
        } = plan_immediate_interrupt(Kind::Reprompt, "do this instead", 1)
        else {
            panic!("reprompt must interrupt immediately");
        };
        assert_eq!(got, framed);
        assert!(notice.contains("1 queued message was discarded."));
    }

    /// P1-7: the replacement-turn envelope keeps the unfinished-work
    /// reminder and never splits a multi-byte character when truncating.
    #[test]
    fn reprompt_envelope_keeps_reminder_and_utf8_boundaries() {
        let reminder = "Make sure to complete any unfinished tasks from previous turns.";
        let plain = InterjectionBuffer::frame_reprompt("replace this");
        assert!(plain.contains("<user_query>"));
        assert!(plain.ends_with(reminder));

        let huge = "é".repeat(30_000);
        let bounded = InterjectionBuffer::frame_reprompt(&huge);
        assert!(bounded.contains("... [truncated]"));
        assert!(std::str::from_utf8(bounded.as_bytes()).is_ok());
    }

    /// P1-10 handshake: a pause request completes only AFTER the reader has
    /// acknowledged from outside poll/read, the parked reader stops
    /// consuming while acknowledged, and resume releases it again.
    #[test]
    fn pause_request_is_acknowledged_and_excludes_the_reader() {
        let pause = InputPause::new();

        // Simulated reader: counts consumed "keystrokes" between cycles.
        let reader_pause = pause.clone();
        let consumed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let consumed_handle = consumed.clone();
        let stop_flag = stop.clone();
        let reader = std::thread::spawn(move || {
            while !stop_flag.load(Ordering::SeqCst) {
                if reader_pause.reader_park() {
                    continue; // parked: never touches the "tty"
                }
                consumed_handle.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(5));
            }
        });

        // Let the reader run free for a moment, then request the exclusive
        // pause. The request must COMPLETE (acknowledged), not time out.
        std::thread::sleep(Duration::from_millis(60));
        assert!(
            pause.request_with_deadline(Duration::from_secs(2)),
            "pause request must be acknowledged by the reader"
        );
        assert!(pause.is_paused());

        // While acknowledged, the reader must not consume anything further:
        // it has exited poll/read and will not touch the tty until resume.
        let before = consumed.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(80));
        let after = consumed.load(Ordering::SeqCst);
        assert_eq!(
            after, before,
            "an acknowledged pause must exclude the reader from poll/read"
        );

        // Resume releases the parked reader.
        pause.resume();
        assert!(!pause.is_paused());
        std::thread::sleep(Duration::from_millis(40));
        assert!(
            consumed.load(Ordering::SeqCst) > after,
            "resume must let the reader continue"
        );
        stop.store(true, Ordering::SeqCst);
        reader.join().expect("reader thread exits cleanly");
    }

    /// P1-10: requesting BEFORE any reader exists cannot hang forever — the
    /// deadline variant reports the missing acknowledgement, and a later
    /// reader parks on its very first cycle boundary.
    #[test]
    fn pause_without_a_reader_reports_no_ack_within_deadline() {
        let pause = InputPause::new();
        // No reader at all: the acknowledgement cannot arrive.
        assert!(!pause.request_with_deadline(Duration::from_millis(50)));
        assert!(pause.is_paused(), "the request itself is still pending");
        pause.resume();

        // A live reader acknowledges the request at its next cycle boundary.
        let reader_pause = pause.clone();
        let acked = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let acked_setter = acked.clone();
        let stop_flag = stop.clone();
        let reader = std::thread::spawn(move || {
            while !stop_flag.load(Ordering::SeqCst) {
                if reader_pause.reader_park() {
                    continue;
                }
                std::thread::sleep(Duration::from_millis(5));
                acked_setter.store(true, Ordering::SeqCst);
            }
        });
        let started = std::time::Instant::now();
        while !acked.load(Ordering::SeqCst) {
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "the reader must complete its first read cycle"
            );
            std::thread::yield_now();
        }
        assert!(
            pause.request_with_deadline(Duration::from_secs(2)),
            "a live reader must acknowledge the request"
        );
        assert!(
            acked.load(Ordering::SeqCst),
            "the acknowledgement comes from a completed read cycle"
        );
        pause.resume();
        stop.store(true, Ordering::SeqCst);
        reader.join().expect("reader thread exits cleanly");
    }

    /// P2-5: the paging cursor advances to the smallest sequence of the page
    /// just inserted, so consecutive pages never overlap or skip.
    #[test]
    fn paging_cursor_advances_to_smallest_seq_in_page() {
        let page = vec![
            HistoryPageEntry {
                seq: 41,
                line: Some("c".into()),
            },
            HistoryPageEntry {
                seq: 12,
                line: None,
            },
            HistoryPageEntry {
                seq: 27,
                line: Some("b".into()),
            },
        ];
        assert_eq!(next_paging_cursor(&page), Some(12));
        assert_eq!(next_paging_cursor(&[]), None);
    }

    /// A host that accepts session switches and `/new`, so tests can drive
    /// the real SwitchSession / CommandEffect::New arms in `handle_event`
    /// and `apply_command_effect`.
    struct SwitchingHost;

    impl TurnHost for SwitchingHost {
        fn spawn_turn(&self, _: String) -> JoinHandle<i32> {
            tokio::spawn(async { 0 })
        }
        async fn cancel_turn(&self) -> Result<(), String> {
            Ok(())
        }
        fn resolve_command(&self, _: &str) -> Option<String> {
            None
        }
        fn check_command(&self, _: &str, _: bool) -> Result<(), String> {
            Ok(())
        }
        async fn execute_command(&self, _: &str, _: &str) -> bool {
            false
        }
        fn completions(&self, _: &str) -> Vec<String> {
            vec![]
        }
        async fn switch_session(&self, _id: &SessionId) -> Result<(), String> {
            Ok(())
        }
        async fn new_session(&self) -> Result<SessionId, String> {
            Ok(SessionId("fresh".to_string()))
        }
    }

    struct PagingFixture {
        composer: Composer,
        nav: NavView,
        session: SessionState,
        transcript: Transcript,
        surface: Surface,
        interjections: InterjectionBuffer,
        pending_echoes: std::collections::VecDeque<String>,
        turn: Option<JoinHandle<i32>>,
        requester: FrameRequester,
        exit_code: i32,
        should_exit: bool,
        viewed_agent: Option<AgentId>,
        paging: HistoryPaging,
    }

    impl PagingFixture {
        /// Two sessions ("a" current, "b" other) and paging state that has
        /// already advanced on session "a"'s ledger.
        fn new() -> (SwitchingHost, Self) {
            let (draw_tx, _) = broadcast::channel(1);
            let summary = |id: &str| SessionSummary {
                id: SessionId(id.to_string()),
                name: id.to_string(),
            };
            let mut nav = NavView::new();
            nav.set_sessions(vec![summary("a"), summary("b")], Some(summary("a")));
            (
                SwitchingHost,
                Self {
                    composer: Composer::new(),
                    nav,
                    session: SessionState::new("a"),
                    transcript: Transcript::new(),
                    surface: Surface::new(
                        80,
                        24,
                        17,
                        7,
                        rightkit_terminal_ui::terminal::InsertMode::ScrollRegion,
                    ),
                    interjections: InterjectionBuffer::new(),
                    pending_echoes: std::collections::VecDeque::new(),
                    turn: None,
                    requester: rightkit_terminal_ui::scheduler::spawn(draw_tx),
                    exit_code: 0,
                    should_exit: false,
                    viewed_agent: None,
                    paging: HistoryPaging {
                        cursor: Some(7),
                        exhausted: true,
                        paged_through_evictions: 42,
                    },
                },
            )
        }

        async fn send_key(&mut self, host: &SwitchingHost, code: KeyCode) {
            handle_event(
                host,
                &TuiEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)),
                &mut self.composer,
                &mut self.nav,
                &mut self.session,
                &mut self.transcript,
                &mut self.surface,
                &self.interjections,
                &mut self.pending_echoes,
                &mut self.turn,
                &self.requester,
                &mut self.exit_code,
                &mut self.should_exit,
                &mut self.viewed_agent,
                &mut self.paging,
            )
            .await;
        }

        /// Open the session picker and accept the second row — the exact
        /// key path a user takes to produce `NavIntent::SwitchSession`.
        async fn pick_second_session(&mut self, host: &SwitchingHost) {
            self.nav.open_sessions();
            self.nav.drain_intents();
            self.send_key(host, KeyCode::Down).await;
            self.send_key(host, KeyCode::Enter).await;
        }
    }

    /// Regression: an accepted switch to another session rebinds all paging
    /// state — cursor, exhaustion flag, and eviction baseline — so the new
    /// session's first page fetch cannot reuse the old session's cursor.
    #[tokio::test]
    async fn switching_sessions_rebinds_scrollback_paging_state() {
        let (host, mut fx) = PagingFixture::new();
        fx.pick_second_session(&host).await;
        assert_eq!(fx.session.active_session, "b");
        assert_eq!(fx.paging, HistoryPaging::default());
        assert!(fx.viewed_agent.is_none());
    }

    /// Regression: `/new` lands on a different durable ledger too, so it
    /// rebinds paging exactly like a session switch.
    #[tokio::test]
    async fn new_session_rebinds_scrollback_paging_state() {
        let (host, mut fx) = PagingFixture::new();
        apply_command_effect(
            &host,
            CommandEffect::New,
            &mut fx.composer,
            &mut fx.nav,
            &mut fx.session,
            &mut fx.transcript,
            &fx.interjections,
            &mut fx.turn,
            &fx.requester,
            &mut fx.viewed_agent,
            &mut fx.paging,
        )
        .await;
        assert_eq!(fx.session.active_session, "fresh");
        assert_eq!(fx.paging, HistoryPaging::default());
    }

    /// B-5: a host that owns a current session and really re-points it.
    struct OwningHost {
        current: std::sync::Mutex<String>,
        actions: std::sync::Mutex<Vec<(String, String, Option<String>)>>,
    }

    impl OwningHost {
        fn new(current: &str) -> Self {
            Self {
                current: std::sync::Mutex::new(current.to_string()),
                actions: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl TurnHost for OwningHost {
        fn spawn_turn(&self, _: String) -> JoinHandle<i32> {
            tokio::spawn(async { 0 })
        }
        async fn cancel_turn(&self) -> Result<(), String> {
            Ok(())
        }
        fn resolve_command(&self, _: &str) -> Option<String> {
            None
        }
        fn check_command(&self, _: &str, _: bool) -> Result<(), String> {
            Ok(())
        }
        async fn execute_command(&self, _: &str, _: &str) -> bool {
            false
        }
        fn completions(&self, _: &str) -> Vec<String> {
            vec![]
        }
        fn current_session(&self) -> Option<String> {
            Some(self.current.lock().unwrap().clone())
        }
        async fn switch_session(&self, id: &SessionId) -> Result<(), String> {
            *self.current.lock().unwrap() = id.0.clone();
            Ok(())
        }
        async fn new_session(&self) -> Result<SessionId, String> {
            *self.current.lock().unwrap() = "fresh".to_string();
            Ok(SessionId("fresh".to_string()))
        }
        async fn session_action(
            &self,
            action: &str,
            session_id: &str,
            value: Option<&str>,
        ) -> Result<String, String> {
            self.actions.lock().unwrap().push((
                action.to_string(),
                session_id.to_string(),
                value.map(str::to_string),
            ));
            Ok(match action {
                "clone" => "Cloned session clone-1".to_string(),
                "export" => "# exported".to_string(),
                other => format!("{other} ok"),
            })
        }
    }

    fn notice_text(fx: &PagingFixture) -> String {
        fx.transcript.inspection_lines(200).join("\n")
    }

    /// B-5: `/resume <id>` on a host that owns a different current session
    /// switches to the past session instead of "Already on this session".
    #[tokio::test]
    async fn resume_by_id_switches_to_a_past_session() {
        let (_, mut fx) = PagingFixture::new();
        let host = OwningHost::new("a");
        switch_to_session(
            &host,
            SessionId("b".into()),
            &mut fx.composer,
            &mut fx.nav,
            &mut fx.session,
            &mut fx.transcript,
            &mut fx.surface,
            false,
            &mut fx.viewed_agent,
            &mut fx.paging,
        )
        .await;
        assert_eq!(host.current_session().as_deref(), Some("b"));
        assert_eq!(fx.session.active_session, "b");
        assert!(!notice_text(&fx).contains("Already on this session"));
    }

    /// B-5: resuming the session already being viewed stays a no-op notice.
    #[tokio::test]
    async fn resume_of_the_current_session_is_a_noop_notice() {
        let (_, mut fx) = PagingFixture::new();
        let host = OwningHost::new("a");
        switch_to_session(
            &host,
            SessionId("a".into()),
            &mut fx.composer,
            &mut fx.nav,
            &mut fx.session,
            &mut fx.transcript,
            &mut fx.surface,
            false,
            &mut fx.viewed_agent,
            &mut fx.paging,
        )
        .await;
        assert!(notice_text(&fx).contains("Already on this session"));
        assert_eq!(fx.paging.cursor, Some(7));
    }

    async fn run_session(host: &OwningHost, fx: &mut PagingFixture, name: &str, rest: &str) {
        run_session_command(
            host,
            name,
            rest,
            &mut fx.composer,
            &mut fx.nav,
            &mut fx.session,
            &mut fx.transcript,
            &mut fx.surface,
            &fx.interjections,
            &mut fx.viewed_agent,
            &mut fx.paging,
            &fx.turn,
        )
        .await;
    }

    #[tokio::test]
    async fn rename_targets_the_current_session_and_requires_a_title() {
        let (_, mut fx) = PagingFixture::new();
        let host = OwningHost::new("a");
        run_session(&host, &mut fx, "/rename", "").await;
        assert!(host.actions.lock().unwrap().is_empty());
        assert!(notice_text(&fx).contains("usage: /rename"));
        run_session(&host, &mut fx, "/rename", "  New title ").await;
        assert_eq!(
            host.actions.lock().unwrap().as_slice(),
            &[(
                "rename".to_string(),
                "a".to_string(),
                Some("New title".to_string())
            )]
        );
    }

    #[tokio::test]
    async fn clone_switches_the_view_to_the_clone() {
        let (_, mut fx) = PagingFixture::new();
        let host = OwningHost::new("a");
        run_session(&host, &mut fx, "/clone", "").await;
        assert_eq!(host.current_session().as_deref(), Some("clone-1"));
        assert_eq!(fx.session.active_session, "clone-1");
    }

    #[tokio::test]
    async fn delete_needs_confirmation_then_lands_on_a_new_session() {
        let (_, mut fx) = PagingFixture::new();
        let host = OwningHost::new("a");
        run_session(&host, &mut fx, "/delete", "").await;
        assert!(host.actions.lock().unwrap().is_empty());
        assert!(notice_text(&fx).contains("--yes"));
        run_session(&host, &mut fx, "/delete", "--yes").await;
        assert_eq!(host.actions.lock().unwrap()[0].0, "delete");
        assert_eq!(fx.session.active_session, "fresh");
    }

    #[tokio::test]
    async fn archive_of_another_session_keeps_the_current_view() {
        let (_, mut fx) = PagingFixture::new();
        let host = OwningHost::new("a");
        run_session(&host, &mut fx, "/archive", "b").await;
        assert_eq!(host.actions.lock().unwrap()[0].1, "b");
        assert_eq!(fx.session.active_session, "a");
    }

    #[test]
    fn export_arguments_parse_id_and_format() {
        assert_eq!(parse_export_args(""), Ok(ExportTarget::Current("md")));
        assert_eq!(parse_export_args("html"), Ok(ExportTarget::Current("html")));
        assert_eq!(
            parse_export_args("abc"),
            Ok(ExportTarget::Other("abc".into(), "md"))
        );
        assert_eq!(
            parse_export_args("abc HTML"),
            Ok(ExportTarget::Other("abc".into(), "html"))
        );
        assert!(parse_export_args("abc pdf").is_err());
        assert!(parse_export_args("a b c").is_err());
    }

    #[test]
    fn export_writes_a_named_file_into_the_directory() {
        let dir = std::env::temp_dir().join(format!("cr-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = write_session_export(&dir, "ab/cd-1234567890", "html", "<p>x</p>").unwrap();
        assert_eq!(
            path.file_name().unwrap().to_str().unwrap(),
            "coderight-session-abcd1234.html"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "<p>x</p>");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clone_and_delete_argument_helpers() {
        assert_eq!(
            parse_cloned_session_id("Cloned session s-2"),
            Some("s-2".into())
        );
        assert_eq!(parse_cloned_session_id("Cloned session "), None);
        assert_eq!(parse_cloned_session_id("boom"), None);
        assert_eq!(parse_delete_args("--yes"), (None, true));
        assert_eq!(parse_delete_args("s-1 -y"), (Some("s-1".into()), true));
        assert_eq!(parse_delete_args("s-1"), (Some("s-1".into()), false));
    }

    /// Same-session operation retains progress: a refused switch (the
    /// default host impl has no daemon feed) leaves the view on session "a",
    /// so its cursor, exhaustion flag, and eviction baseline must survive.
    #[tokio::test]
    async fn refused_switch_retains_scrollback_paging_state() {
        let (_, mut fx) = PagingFixture::new();
        // A host that cannot switch sessions: the default TurnHost impl.
        struct RefusingSwitchHost;
        impl TurnHost for RefusingSwitchHost {
            fn spawn_turn(&self, _: String) -> JoinHandle<i32> {
                tokio::spawn(async { 0 })
            }
            async fn cancel_turn(&self) -> Result<(), String> {
                Ok(())
            }
            fn resolve_command(&self, _: &str) -> Option<String> {
                None
            }
            fn check_command(&self, _: &str, _: bool) -> Result<(), String> {
                Ok(())
            }
            async fn execute_command(&self, _: &str, _: &str) -> bool {
                false
            }
            fn completions(&self, _: &str) -> Vec<String> {
                vec![]
            }
        }
        let host = RefusingSwitchHost;
        fx.nav.open_sessions();
        fx.nav.drain_intents();
        for code in [KeyCode::Down, KeyCode::Enter] {
            handle_event(
                &host,
                &TuiEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)),
                &mut fx.composer,
                &mut fx.nav,
                &mut fx.session,
                &mut fx.transcript,
                &mut fx.surface,
                &fx.interjections,
                &mut fx.pending_echoes,
                &mut fx.turn,
                &fx.requester,
                &mut fx.exit_code,
                &mut fx.should_exit,
                &mut fx.viewed_agent,
                &mut fx.paging,
            )
            .await;
        }
        assert_eq!(fx.session.active_session, "a");
        assert_eq!(
            fx.paging,
            HistoryPaging {
                cursor: Some(7),
                exhausted: true,
                paged_through_evictions: 42,
            }
        );
    }

    /// Host that records the approval decision and turn cancellation.
    struct RejectHost {
        decisions: std::sync::Mutex<Vec<(String, rightkit_agent_engine_protocol::ReviewDecision)>>,
        cancels: std::sync::atomic::AtomicUsize,
    }
    impl TurnHost for RejectHost {
        fn spawn_turn(&self, _: String) -> JoinHandle<i32> {
            tokio::spawn(async { 0 })
        }
        async fn cancel_turn(&self) -> Result<(), String> {
            self.cancels
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        fn resolve_command(&self, _: &str) -> Option<String> {
            None
        }
        fn check_command(&self, _: &str, _: bool) -> Result<(), String> {
            Ok(())
        }
        async fn execute_command(&self, _: &str, _: &str) -> bool {
            false
        }
        fn completions(&self, _: &str) -> Vec<String> {
            vec![]
        }
        async fn resolve_approval(
            &self,
            _session_id: &str,
            tool_call_id: &str,
            decision: rightkit_agent_engine_protocol::ReviewDecision,
        ) -> Result<(), String> {
            self.decisions
                .lock()
                .unwrap()
                .push((tool_call_id.to_string(), decision));
            Ok(())
        }
    }

    /// Esc on the approval modal rejects the request through the approval
    /// authority, stops the turn, keeps queued messages queued, and hands
    /// the composer a "tell the model" placeholder.
    #[tokio::test]
    async fn approval_escape_rejects_stops_turn_and_keeps_queue() {
        let (draw_tx, _) = broadcast::channel(1);
        let host = RejectHost {
            decisions: Default::default(),
            cancels: Default::default(),
        };
        let mut composer = Composer::new();
        let mut nav = NavView::new();
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        let mut surface = Surface::new(
            80,
            24,
            17,
            7,
            rightkit_terminal_ui::terminal::InsertMode::ScrollRegion,
        );
        let interjections = InterjectionBuffer::new();
        let mut pending_echoes = std::collections::VecDeque::new();
        let mut turn: Option<JoinHandle<i32>> =
            Some(tokio::spawn(async { std::future::pending::<i32>().await }));
        let requester = rightkit_terminal_ui::scheduler::spawn(draw_tx);
        let (mut exit_code, mut should_exit, mut viewed, mut paging) =
            (0, false, None, HistoryPaging::default());
        interjections.push("later".into(), rightkit_terminal_ui::interject::Kind::Steer);
        composer.set_streaming(true);
        session.working_started = Some(Instant::now());
        session.record(
            &mut transcript,
            DaemonEvent::ApprovalRequested {
                request_id: "req-1".into(),
                summary: "run cargo test".into(),
            },
        );
        session.approval_overlay.arm_for_tests();
        session.approval_input_safe = true;
        handle_event(
            &host,
            &TuiEvent::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            &mut composer,
            &mut nav,
            &mut session,
            &mut transcript,
            &mut surface,
            &interjections,
            &mut pending_echoes,
            &mut turn,
            &requester,
            &mut exit_code,
            &mut should_exit,
            &mut viewed,
            &mut paging,
        )
        .await;
        let decisions = host.decisions.lock().unwrap();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].0, "req-1");
        assert!(!decisions[0].1.is_approved(), "Esc must never approve");
        assert!(turn.is_none(), "the running turn is stopped");
        assert_eq!(host.cancels.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(session.working_started.is_none());
        assert!(session.approval_overlay.current_request().is_none());
        assert!(session.pending_approvals.is_empty());
        assert!(composer
            .placeholder_override()
            .is_some_and(|p| p.starts_with("Tell ") && p.ends_with("what to do instead")));
        assert_eq!(
            interjections.texts(),
            vec!["later".to_string()],
            "Esc keeps the queue"
        );
    }

    #[test]
    fn status_header_prefers_reasoning_heading_then_tool_then_thinking() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        assert_eq!(session.default_operation(), "Thinking");
        session.record(
            &mut transcript,
            DaemonEvent::ReasoningDelta {
                token: "**Planning the fix**\nmore".into(),
            },
        );
        assert_eq!(session.default_operation(), "Planning the fix");
        session.record(
            &mut transcript,
            DaemonEvent::ToolStarted {
                call_id: "c1".into(),
                name: "read_file".into(),
                args_preview: r#"{"path":"src/main.rs"}"#.into(),
            },
        );
        assert_eq!(session.default_operation(), "Reading src/main.rs");
        session.record(
            &mut transcript,
            DaemonEvent::ToolUpdated {
                call_id: "c1".into(),
                status: rightkit_terminal_ui::transcript::timeline::ToolStatus::Succeeded,
                output_preview: String::new(),
            },
        );
        assert_eq!(session.default_operation(), "Thinking");
        assert_eq!(
            tool_activity_label("shell", r#"{"command":"cargo test"}"#),
            "Running cargo test"
        );
        assert_eq!(
            tool_activity_label("apply_patch", r#"{"path":"a.rs"}"#),
            "Editing a.rs"
        );
        assert_eq!(
            tool_activity_label("web_search", "{}"),
            "Running web_search"
        );
        let long = tool_activity_label("shell", &format!(r#"{{"command":"{}"}}"#, "x".repeat(90)));
        assert!(
            long.ends_with('…') && long.chars().count() <= "Running ".len() + 60,
            "{long}"
        );
    }

    #[test]
    fn failure_is_held_on_the_row_until_the_next_turn() {
        use rightkit_terminal_ui::transcript::timeline::RunSignal;
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.working_started = Some(Instant::now());
        session.record(
            &mut transcript,
            DaemonEvent::Status(RunSignal::Failed {
                reason: "boom".into(),
                hint: "network".into(),
            }),
        );
        assert!(matches!(
            session.failed_hold,
            Some((RunSignal::Failed { .. }, _))
        ));
        // Turn settles: the hold survives.
        session.working_started = None;
        session.working_signal = None;
        assert!(session.failed_hold.is_some());
        // Next turn clears it.
        session.begin_turn_status();
        assert!(session.failed_hold.is_none());
    }

    #[test]
    fn route_change_emits_a_boundary_divider_and_actor_follows() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        let route = |model: &str| DaemonEvent::RouteDecided {
            project: "p".into(),
            model: model.into(),
            actor: "a".into(),
        };
        session.record(&mut transcript, route("model-a"));
        session.record(&mut transcript, route("model-b"));
        assert_eq!(
            session.identity.attribution().map(|a| a.model.as_str()),
            Some("model-b")
        );
        let text = settled_transcript_text(&mut transcript);
        assert!(
            text.contains("route changed: model-a \u{2192} model-b"),
            "{text}"
        );
    }

    /// Regression: the maintenance tick once awaited `poll_timeline_events()`
    /// inside its select arm. InlineHost can spend seconds rebuilding its
    /// state there, which froze input, timers and redraws (typed text only
    /// appeared on the next key; Enter arrived bunched with the text and read
    /// as a paste newline). The poll must be a pinned future with its own arm.
    #[test]
    fn maintenance_tick_never_awaits_the_timeline_poll_inline() {
        let source = include_str!("tui_host.rs").replace("\r\n", "\n");
        let production = source
            .split("#[cfg(test)]\nmod tests")
            .next()
            .unwrap_or(&source)
            .to_string();
        let awaited = production
            .matches("host.poll_timeline_events().await")
            .count();
        assert_eq!(
            awaited, 2,
            "only the turn-settle drain and the pre-submit drain may await the poll directly; the maintenance tick must use `timeline_poll`"
        );
        assert!(production.contains("timeline_poll = Some(Box::pin(host.poll_timeline_events()))"));
    }

    #[test]
    fn a_decision_recorded_locally_and_by_the_feed_paints_one_cell() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.record(
            &mut transcript,
            DaemonEvent::ApprovalRequested {
                request_id: "r1".into(),
                summary: "run cargo test".into(),
            },
        );
        for _ in 0..2 {
            session.record(
                &mut transcript,
                DaemonEvent::ApprovalResolved {
                    request_id: "r1".into(),
                    approved: false,
                },
            );
        }
        let text = settled_transcript_text(&mut transcript);
        assert_eq!(text.matches("rejected run cargo test").count(), 1, "{text}");
    }

    /// Regression: navigation refresh runs on the terminal loop (startup and
    /// every turn start). It must read host state only when it is already
    /// built; building it there froze the UI for seconds (Enter appeared to
    /// do nothing).
    #[test]
    fn nav_refresh_never_builds_the_inline_host() {
        let source = include_str!("main_runtime_interactive_session.rs").replace("\r\n", "\n");
        let start = source
            .find("async fn refresh_nav(")
            .expect("refresh_nav exists");
        let tail = &source[start..];
        let end = tail[10..]
            .find("\n        async fn ")
            .map(|n| n + 10)
            .unwrap_or(tail.len());
        let body = &tail[..end];
        assert!(
            !body.contains("SessionExecutionHost::select("),
            "refresh_nav must use select_if_ready"
        );
        let host = include_str!("main_runtime_session_host.rs").replace("\r\n", "\n");
        let list = host
            .find("async fn list_sessions_for_workspace(")
            .expect("fn exists");
        let list_body = &host[list..list + 900];
        assert!(list_body.contains(".peek()") && !list_body.contains(".ensure("));
    }

    #[test]
    fn startup_viewport_is_anchored_to_the_screen_bottom() {
        // Short viewport launched near the top moves down to the bottom.
        assert_eq!(bottom_anchored_launch_row(4, 34, 4), 30);
        // A launch already below the anchor row is kept (scroll is owed).
        assert_eq!(bottom_anchored_launch_row(33, 34, 4), 33);
        // A viewport taller than the screen starts at the top.
        assert_eq!(bottom_anchored_launch_row(0, 10, 40), 0);
    }

    #[test]
    fn working_signals_reach_the_session_and_clear_on_new_turn() {
        use rightkit_terminal_ui::transcript::timeline::RunSignal;
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.record(
            &mut transcript,
            DaemonEvent::Status(RunSignal::Retrying {
                provider: "openai".into(),
                reason: "overloaded".into(),
                attempt: Some((1, 3)),
                delay_secs: Some(4),
            }),
        );
        assert!(matches!(
            session.working_signal,
            Some(RunSignal::Retrying { .. })
        ));
        let text = settled_transcript_text(&mut transcript);
        assert!(
            text.contains("Retrying · openai · overloaded · attempt 1/3 in 4s"),
            "{text}"
        );
        session.record(
            &mut transcript,
            DaemonEvent::Status(RunSignal::Failed {
                reason: "boom".into(),
                hint: "network".into(),
            }),
        );
        assert!(matches!(
            session.working_signal,
            Some(RunSignal::Failed { .. })
        ));
    }

    // ── Stage-3 typed projection & approval overlay ─────────────────

    pub(super) fn settled_transcript_text(transcript: &mut Transcript) -> String {
        transcript
            .drain_finalized(80)
            .into_iter()
            .map(|line| line.line().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn mono_theme() -> Theme {
        Theme::new(
            rightkit_terminal_ui::terminal::Background::Dark,
            rightkit_terminal_ui::terminal::ColorDepth::Monochrome,
        )
    }

    #[test]
    fn chat_chrome_queued_framed_echo_is_hidden_without_hiding_remote_input() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        let mut pending = std::collections::VecDeque::new();
        let raw = "what about this?";
        session.record(
            &mut transcript,
            DaemonEvent::UserMessage { text: raw.into() },
        );
        let queue = InterjectionBuffer::new();
        queue.push(raw.into(), InterjectKind::Steer);
        let Directive::Steer(framed) = queue.drain().remove(0) else {
            panic!("steer");
        };
        pending.push_back(framed.clone());
        record_polled_event(
            &mut session,
            &mut transcript,
            &mut pending,
            DaemonEvent::UserMessage {
                text: "remote message".into(),
            },
        );
        assert_eq!(pending.len(), 1);
        record_polled_event(
            &mut session,
            &mut transcript,
            &mut pending,
            DaemonEvent::UserMessage { text: framed },
        );
        assert!(pending.is_empty());
        let visible = settled_transcript_text(&mut transcript);
        assert_eq!(visible.matches(raw).count(), 1, "{visible}");
        assert!(visible.contains("remote message"));
        assert!(!visible.contains("<user_query>"));
    }

    #[test]
    fn chat_chrome_footer_keeps_directory_and_clips_by_display_width() {
        use rightkit_terminal_ui::transcript::wrap::display_width;
        let text =
            |width, path| directory_footer_line(width, Some(path), &mono_theme()).to_string();
        assert_eq!(
            text(80, r"\\?\D:\Claude\workright"),
            r"  D:\Claude\workright"
        );
        assert_eq!(text(80, r"\\?\UNC\server\share"), r"  \\server\share");
        let clipped = text(16, "D:\\Claude\\長いディレクトリ");
        assert!(clipped.starts_with("  D:\\Claude\\"), "{clipped}");
        assert!(clipped.ends_with('…'));
        assert!(display_width(&clipped) <= 16);
        assert!(!text(40, "bad\u{1b}[2Jpath").contains('\u{1b}'));
    }

    fn footer_text(line: rightkit_terminal_ui::ratatui::text::Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.to_string())
            .collect()
    }

    #[test]
    fn footer_shows_cwd_branch_mode_left_and_model_effort_right() {
        use rightkit_agent_policy::PermissionMode;
        let text = |mode| {
            footer_text(footer_line(
                100,
                r"D:\Claude\coderight",
                Some("main"),
                "model-x · high",
                mode,
                None,
                None,
                None,
                None,
                false,
                0,
                &mono_theme(),
            ))
        };
        let ask = text(PermissionMode::Ask);
        assert!(
            ask.trim_start()
                .starts_with(r"D:\Claude\coderight · main · Ask"),
            "{ask}"
        );
        assert!(ask.trim_end().ends_with("model-x · high"), "{ask}");
        assert!(!ask.contains("? /help"), "{ask}");
        assert!(text(PermissionMode::AcceptEdits).contains("⏵⏵ Accept edits"));
        assert!(text(PermissionMode::AutoSafe).contains("⏵⏵ Auto"));
        assert!(text(PermissionMode::Bypass).contains("⚠ Yolo"));
        assert!(text(PermissionMode::Plan).contains("⏸ Plan"));
    }

    #[test]
    fn footer_left_truncates_the_cwd_at_narrow_widths_and_keeps_mode_and_model() {
        use rightkit_terminal_ui::transcript::wrap::display_width;
        let line = footer_text(footer_line(
            44,
            r"D:\Claude\very\long\path\coderight",
            Some("main"),
            "gpt-6.1-sol · high",
            rightkit_agent_policy::PermissionMode::Ask,
            None,
            None,
            None,
            None,
            false,
            0,
            &mono_theme(),
        ));
        assert!(line.contains("…"), "{line}");
        assert!(line.contains("coderight"), "{line}");
        assert!(
            line.contains("Ask") && line.contains("gpt-6.1-sol"),
            "{line}"
        );
        assert!(display_width(&line) <= 44, "{line}");
    }

    #[test]
    fn startup_is_the_wordmark_and_a_ready_line_only() {
        let text = |lines: Vec<rightkit_terminal_ui::ratatui::text::Line<'static>>| {
            lines.into_iter().map(footer_text).collect::<Vec<_>>()
        };
        let ready = text(welcome_lines(80, "gpt-6.1-sol · high", &mono_theme()));
        assert!(ready[0].contains("RightKit"), "{ready:?}");
        assert!(
            ready[1].contains("✓ ready · gpt-6.1-sol · high"),
            "{ready:?}"
        );
        assert!(!ready.iter().any(|l| l.contains('✻') || l.contains('╭')));
        let none = text(welcome_lines(80, "", &mono_theme()));
        assert!(none[1].contains("no model yet"), "{none:?}");
        assert!(none[2].contains("/providers"), "{none:?}");
    }

    #[test]
    fn short_session_keeps_footer_on_the_last_row_with_spare_rows_above() {
        // 100x22 terminal, short transcript: the viewport is anchored to the
        // bottom, and the footer owns the very last row.
        let mut surface = Surface::new(
            100,
            22,
            5,
            6,
            rightkit_terminal_ui::terminal::InsertMode::ScrollRegion,
        );
        surface.anchor_to_bottom();
        surface.set_viewport_height(7);
        let viewport = surface.viewport();
        assert_eq!(
            viewport.top + viewport.height,
            22,
            "no empty rows below the viewport"
        );
        let area = Rect {
            x: 0,
            y: viewport.top,
            width: 100,
            height: viewport.height,
        };
        let layout = chrome_layout(area, 1, 0, false, 0);
        assert_eq!(
            layout.footer.bottom(),
            22,
            "footer sits on the last terminal row"
        );
        assert_eq!(layout.footer.height, 1);
        assert_eq!(layout.rule_bottom.bottom(), layout.footer.y);
        assert_eq!(layout.composer.bottom(), layout.rule_bottom.y);
        assert_eq!(layout.rule_top.bottom(), layout.composer.y);
        // Spare space is the transcript region above the input.
        assert_eq!(layout.status.height, 7 - 4);
        assert!(layout.status.bottom() <= layout.rule_top.y);
        // Working line and queue list stack directly above the top rule.
        let busy = chrome_layout(area, 1, 0, true, 2);
        assert_eq!(busy.footer.bottom(), 22);
        assert_eq!(busy.working.bottom(), busy.rule_top.y);
        assert_eq!(busy.queue.bottom(), busy.working.y);
    }

    #[test]
    fn exact_substring_edits_preview_as_a_numbered_diff_not_raw_json() {
        let dir = std::env::temp_dir().join(format!("cr-edit-preview-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("calc.py"), "def divide(a, b):\n    return a / b\n").unwrap();
        let args = r#"{"path":"calc.py","edits":[{"old_string":"    return a / b","new_string":"    if b == 0:\n        return None\n    return a / b"}]}"#;
        let kind = approval_kind_for_call("multi_edit", args, "multi_edit {...}", dir.to_str());
        let ApprovalKind::Patch { changes, .. } = kind else {
            panic!("an edit with old/new strings must be a Patch preview");
        };
        let rightkit_terminal_ui::render::diff::FileChange::Update { unified_diff } =
            changes.values().next().unwrap().clone()
        else {
            panic!("an exact-substring edit is an Update");
        };
        assert!(unified_diff.contains("+    if b == 0:"), "{unified_diff}");
        assert!(
            unified_diff.contains(" def divide(a, b):"),
            "real context from the file: {unified_diff}"
        );
        // A file that cannot be read still previews exactly the replacement.
        let kind = approval_kind_for_call(
            "edit_file",
            r#"{"path":"missing.py","old_string":"a","new_string":"b"}"#,
            "",
            dir.to_str(),
        );
        assert!(matches!(kind, ApprovalKind::Patch { .. }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prose_before_a_tool_call_settles_so_it_stays_above_the_live_tool_row() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.record(
            &mut transcript,
            DaemonEvent::AssistantDelta {
                token: "Running the tests now.".into(),
            },
        );
        assert!(!transcript.live_messages().last().unwrap().finalized);
        session.record(
            &mut transcript,
            DaemonEvent::ToolStarted {
                call_id: "c1".into(),
                name: "shell".into(),
                args_preview: r#"{"command":"pytest"}"#.into(),
            },
        );
        assert!(transcript.live_messages().last().unwrap().finalized);
    }

    #[test]
    fn running_tools_registry_tracks_tool_start_and_finish_and_feeds_the_chip() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.record(
            &mut transcript,
            DaemonEvent::ToolStarted {
                call_id: "c1".into(),
                name: "grep".into(),
                args_preview: "{}".into(),
            },
        );
        assert_eq!(session.running_tools.chip().as_deref(), Some("1 tool"));
        session.record(
            &mut transcript,
            DaemonEvent::ToolUpdated {
                call_id: "c1".into(),
                status: rightkit_terminal_ui::transcript::timeline::ToolStatus::Running,
                output_preview: String::new(),
            },
        );
        assert_eq!(session.running_tools.chip().as_deref(), Some("1 tool"));
        session.reset_view("other");
        assert_eq!(session.running_tools.chip(), None);
    }

    #[test]
    fn a_stale_pending_echo_never_blocks_later_echo_suppression() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        let mut pending: std::collections::VecDeque<String> =
            ["never-echoed".to_string(), "real prompt".to_string()].into();
        record_polled_event(
            &mut session,
            &mut transcript,
            &mut pending,
            DaemonEvent::UserMessage {
                text: "real prompt".into(),
            },
        );
        assert!(
            pending.is_empty(),
            "the stale head and the matched echo are both consumed"
        );
        assert!(
            session.timeline.entries().is_empty(),
            "the durable echo was suppressed, not painted twice"
        );
        // A message from elsewhere is still shown.
        record_polled_event(
            &mut session,
            &mut transcript,
            &mut pending,
            DaemonEvent::UserMessage {
                text: "from another client".into(),
            },
        );
        assert_eq!(session.timeline.entries().len(), 1);
    }

    #[test]
    fn an_interrupt_settles_the_partial_answer() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.record(
            &mut transcript,
            DaemonEvent::AssistantDelta {
                token: "half an ans".into(),
            },
        );
        session.record(
            &mut transcript,
            DaemonEvent::System {
                text: "Turn interrupted: interrupted by user".into(),
            },
        );
        assert!(transcript
            .live_messages()
            .all(|m| m.speaker != Speaker::Assistant || m.finalized));
    }

    #[test]
    fn a_resize_gap_is_the_blank_run_between_transcript_and_viewport() {
        let rows = ["", "", "history", "last line", "", "", "", "", ""];
        let read = |row: u16| rows.get(row as usize).map(|r| r.to_string());
        // Viewport top at 9: five blank rows above it, one is the spacer.
        assert_eq!(blank_gap_above(9, read), 4);
        // Directly attached (one spacer row, or none): nothing to close.
        assert_eq!(blank_gap_above(5, read), 0);
        assert_eq!(blank_gap_above(4, read), 0);
        // A short session with nothing above the blank run is left alone.
        assert_eq!(blank_gap_above(2, read), 0);
        // Unreadable rows never trigger a slide.
        assert_eq!(blank_gap_above(9, |_| None), 0);
    }

    #[test]
    fn a_stopped_turn_does_not_leave_a_running_row_behind() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.working_started = Some(Instant::now());
        session.record(
            &mut transcript,
            DaemonEvent::ToolStarted {
                call_id: "c1".into(),
                name: "shell".into(),
                args_preview: r#"{"command":"sleep 30"}"#.into(),
            },
        );
        assert!(!session.live_cells.is_empty());
        // While the turn runs the row stays live.
        assert!(!session.settle_stale_live_tools(&mut transcript));
        // The stop clears the working state; the next frame settles the row.
        session.working_started = None;
        assert!(session.settle_stale_live_tools(&mut transcript));
        assert!(session.live_cells.is_empty());
    }

    #[test]
    fn usage_events_feed_the_context_gauge_without_a_transcript_line() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.record(
            &mut transcript,
            DaemonEvent::Usage {
                context_size: 12_000,
                context_permille: 45,
                cache_read: 9_000,
                prompt: 12_000,
                cost_micros: None,
                cost_reported: false,
            },
        );
        assert_eq!(session.context_fraction, Some(0.045));
        assert_eq!(session.last_prompt, Some((12_000, 9_000)));
        assert!(
            settled_transcript_text(&mut transcript).trim().is_empty(),
            "no raw usage line"
        );
        let line = footer_text(footer_line(
            100,
            r"D:\w",
            None,
            "gpt-6.1-sol · high · 5% ctx",
            rightkit_agent_policy::PermissionMode::Ask,
            None,
            None,
            None,
            None,
            false,
            0,
            &mono_theme(),
        ));
        assert!(
            line.trim_end().ends_with("gpt-6.1-sol · high · 5% ctx"),
            "{line}"
        );
    }

    #[test]
    fn cache_footer_part_reports_the_hit_rate_and_hides_without_usage() {
        assert_eq!(cache_footer_part(None), None);
        assert_eq!(cache_footer_part(Some((0, 0))), None);
        assert_eq!(
            cache_footer_part(Some((12_000, 11_040))).as_deref(),
            Some("cache 92%")
        );
        assert_eq!(
            cache_footer_part(Some((12_000, 0))).as_deref(),
            Some("cache 0%")
        );
        assert_eq!(
            cache_footer_part(Some((100, 500))).as_deref(),
            Some("cache 100%")
        );
    }

    #[test]
    fn generic_approval_summaries_never_lead_with_raw_json() {
        assert_eq!(
            humanize_generic_summary("tool_search", r#"tool_search {"query":"invoke_skill"}"#),
            "tool_search · query: invoke_skill"
        );
        assert_eq!(humanize_generic_summary("sleep", "sleep {}"), "sleep");
        assert_eq!(
            humanize_generic_summary("x", "allow network access"),
            "allow network access"
        );
        let policy = |kind| approval_policy_reason(Some("mode:ask"), &kind).unwrap();
        assert_eq!(
            policy(ApprovalKind::Generic {
                description: "x".into()
            }),
            "this action needs approval in Ask mode"
        );
    }

    #[test]
    fn queued_messages_show_in_the_queue_list_with_key_hints() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        assert!(session.queue_list_lines(80, &mono_theme()).is_empty());
        session.queued_preview = vec!["then bump the changelog".into()];
        let text: Vec<String> = session
            .queue_list_lines(80, &mono_theme())
            .into_iter()
            .map(footer_text)
            .collect();
        assert!(
            text[0].contains("then bump the changelog") && text[0].contains("queued"),
            "{text:?}"
        );
        // Idle (no turn running): the hint says what really happens, and
        // offers no push.
        assert!(
            text[1].contains("↑ edit")
                && text[1].contains("ctrl+x remove")
                && text[1].contains("next message"),
            "{text:?}"
        );
        assert!(!text[1].contains("⇥"), "{text:?}");
        session.working_started = Some(Instant::now());
        let running: Vec<String> = session
            .queue_list_lines(80, &mono_theme())
            .into_iter()
            .map(footer_text)
            .collect();
        assert!(
            running[1].contains("sends when this turn ends") && running[1].contains("⇥ push now"),
            "{running:?}"
        );
        session.working_started = None;
        // Pending, not sent: dashed rail + right-aligned `queued · N` tag, and
        // never the solid sent-message rail.
        assert!(
            text[0].starts_with("┆ ") && !text[0].contains('▌'),
            "{text:?}"
        );
        assert!(text[0].trim_end().ends_with("queued · 1"), "{text:?}");
        use rightkit_terminal_ui::transcript::wrap::display_width;
        assert_eq!(
            display_width(&text[0]),
            80,
            "tag is right-aligned: {text:?}"
        );
    }

    #[test]
    fn the_queue_block_has_one_hint_and_degrades_at_narrow_widths() {
        use rightkit_terminal_ui::transcript::wrap::display_width;
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        session.queued_preview = vec!["first".into(), "second message that is rather long".into()];
        session.working_started = Some(Instant::now());
        for width in [110u16, 60, 40, 24] {
            let lines: Vec<String> = session
                .queue_list_lines(width, &mono_theme())
                .into_iter()
                .map(footer_text)
                .collect();
            assert_eq!(lines.len(), 3, "two rows + exactly one hint: {lines:?}");
            assert!(
                lines.iter().all(|l| display_width(l) <= usize::from(width)),
                "{lines:?}"
            );
            assert!(
                !lines[2].contains('…'),
                "hint is rewritten, not clipped: {lines:?}"
            );
            assert!(lines[1].contains("queued · 2") || width < 40, "{lines:?}");
        }
    }

    #[test]
    fn queue_pops_oldest_first_for_send_and_newest_for_edit() {
        let queue = InterjectionBuffer::new();
        queue.push("one".into(), InterjectKind::Steer);
        queue.push("two".into(), InterjectKind::Steer);
        queue.push("three".into(), InterjectKind::Steer);
        assert_eq!(queue.pop_last().as_deref(), Some("three"));
        assert_eq!(queue.pop_first().as_deref(), Some("one"));
        assert_eq!(queue.texts(), vec!["two".to_string()]);
    }

    #[test]
    fn footer_surfaces_vendor_permission_warning() {
        let line = footer_line(
            110,
            "/repo",
            None,
            "gemini",
            rightkit_agent_policy::PermissionMode::Ask,
            Some(&HostReadiness::Ready),
            None,
            Some("Antigravity uses its own permission rules"),
            None,
            false,
            0,
            &mono_theme(),
        );
        assert!(footer_text(line).contains("Antigravity uses its own permission rules"));
    }

    /// Tool events produce a live exec cell, then settle into the
    /// transcript as text naming the real tool — never dropped, never a
    /// raw dispatch label.
    #[test]
    fn tool_events_project_through_exec_cell_and_settle() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.record(
            &mut transcript,
            DaemonEvent::ToolStarted {
                call_id: "call-1".into(),
                name: "read_file".into(),
                args_preview: "src/lib.rs".into(),
            },
        );
        assert_eq!(session.live_cells.len(), 1);
        assert!(transcript.live_messages().next().is_none());
        let lines = session.live_cell_lines(80, &mono_theme());
        assert!(!lines.is_empty(), "a running call must draw something");

        session.record(
            &mut transcript,
            DaemonEvent::ToolUpdated {
                call_id: "call-1".into(),
                status: rightkit_terminal_ui::transcript::timeline::ToolStatus::Succeeded,
                output_preview: "file body".into(),
            },
        );
        assert!(
            session.live_cells.is_empty(),
            "settled calls leave the rail"
        );
        let settled = settled_transcript_text(&mut transcript);
        assert!(
            settled.contains("✓ read") && settled.contains("src/lib.rs"),
            "settled cell must name the tool and its target on one ledger row: {settled:?}"
        );
        assert!(
            settled.contains("file body"),
            "settled cell must retain output: {settled:?}"
        );
        assert!(
            transcript.drain_finalized(80).is_empty(),
            "settled cell flushes exactly once"
        );
    }

    /// A cumulative `output_preview` feeds `append_output` as a delta;
    /// the settled transcript shows the preview once, not repeated.
    #[test]
    fn cumulative_output_preview_is_fed_as_a_delta() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.record(
            &mut transcript,
            DaemonEvent::ToolStarted {
                call_id: "c".into(),
                name: "shell".into(),
                args_preview: "echo hi".into(),
            },
        );
        for preview in ["partial out", "partial output"] {
            session.record(
                &mut transcript,
                DaemonEvent::ToolUpdated {
                    call_id: "c".into(),
                    status: rightkit_terminal_ui::transcript::timeline::ToolStatus::Running,
                    output_preview: preview.into(),
                },
            );
        }
        session.record(
            &mut transcript,
            DaemonEvent::ToolUpdated {
                call_id: "c".into(),
                status: rightkit_terminal_ui::transcript::timeline::ToolStatus::Succeeded,
                output_preview: "partial output".into(),
            },
        );
        let text = settled_transcript_text(&mut transcript);
        assert!(text.contains("partial output"), "{text:?}");
        assert_eq!(
            text.matches("partial output").count(),
            1,
            "cumulative preview must not duplicate: {text:?}"
        );
    }

    /// Reasoning streams into a live cell and settles as retained text.
    #[test]
    fn reasoning_deltas_settle_into_transcript() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        for token in ["thinking", " out loud"] {
            session.record(
                &mut transcript,
                DaemonEvent::ReasoningDelta {
                    token: token.into(),
                },
            );
        }
        assert_eq!(session.live_cells.len(), 1, "one open reasoning block");
        session.record(&mut transcript, DaemonEvent::ReasoningSettled);
        assert!(session.live_cells.is_empty());
        let text = settled_transcript_text(&mut transcript);
        assert!(text.contains("thinking out loud"), "{text:?}");
    }

    /// The runtime never marks the end of reasoning: the answer starting is it.
    /// What settles is one dim line of headings, never the raw `**` notes.
    #[test]
    fn reasoning_settles_as_one_summary_line_when_the_answer_starts() {
        assert_eq!(
            reasoning_summary("**Weighing options**\nlong notes\n**Picking one**\nmore notes"),
            "Weighing options \u{b7} Picking one"
        );
        assert_eq!(
            reasoning_summary("just some notes\nsecond line"),
            "just some notes"
        );
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.record(
            &mut transcript,
            DaemonEvent::ReasoningDelta {
                token: "**Reading the file**\nnotes".into(),
            },
        );
        assert_eq!(session.live_cells.len(), 1);
        session.record(
            &mut transcript,
            DaemonEvent::AssistantDelta {
                token: "Here.".into(),
            },
        );
        assert!(
            session.live_cells.is_empty(),
            "prose settles the reasoning block"
        );
        let text = settled_transcript_text(&mut transcript);
        assert!(
            text.contains("Reading the file") && !text.contains("**"),
            "{text:?}"
        );
    }

    /// Task, subagent, and check events are visible typed notices, not
    /// silently dropped raw labels.
    #[test]
    fn task_subagent_and_check_events_surface_as_typed_notices() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.record(
            &mut transcript,
            DaemonEvent::TaskUpdated {
                task_id: "t1".into(),
                label: "build".into(),
                status: rightkit_terminal_ui::transcript::timeline::TaskStatus::Running,
            },
        );
        session.record(
            &mut transcript,
            DaemonEvent::SubagentUpdated {
                subagent_id: "s1".into(),
                label: "explorer".into(),
                status: rightkit_terminal_ui::transcript::timeline::TaskStatus::Succeeded,
            },
        );
        session.record(
            &mut transcript,
            DaemonEvent::CheckReported {
                check_id: "c1".into(),
                label: "tests".into(),
                passed: false,
                detail: "1 failure".into(),
            },
        );
        let text = settled_transcript_text(&mut transcript);
        assert!(text.contains("Task build"), "{text:?}");
        assert!(text.contains("Subagent explorer"), "{text:?}");
        assert!(text.contains("Check tests: failed"), "{text:?}");
        assert!(text.contains("1 failure"), "{text:?}");
    }

    /// A new approval request must quarantine buffered input AND arm its
    /// modal only after a render: keys forwarded before the first draw
    /// emit no decision intent.
    #[test]
    fn approval_request_quarantines_input_and_gates_keys_until_drawn() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.record(
            &mut transcript,
            DaemonEvent::ApprovalRequested {
                request_id: "req-1".into(),
                summary: "run cargo test".into(),
            },
        );
        assert!(
            session.take_input_quarantine(),
            "a fresh request must demand a pre-modal input quarantine"
        );
        assert!(!session.take_input_quarantine(), "the flag is one-shot");
        assert!(session.approval_overlay.current_request().is_some());

        // A key reaching the overlay before its first render is swallowed,
        // never resolved — translated or not.
        let enter = TuiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let outcome = session.approval_overlay.handle(&enter);
        assert!(outcome.is_terminal());
        assert!(
            session.approval_overlay.drain_intents().is_empty(),
            "pre-draw input must not decide the request"
        );
    }

    /// Ctrl-Y/Ctrl-N translate onto the overlay's option digits so the
    /// documented bindings stay live through the arming gate.
    #[test]
    fn ctrl_approval_bindings_translate_to_option_digits() {
        let request = ApprovalRequest::new(
            LOCAL_SESSION_ID,
            "r",
            ApprovalKind::Generic {
                description: "do it".into(),
            },
            vec![ApprovalDecision::Approved, ApprovalDecision::Denied],
        );
        let yes = approval_overlay_key(
            &KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
            &request,
        );
        assert!(matches!(
            yes,
            TuiEvent::Key(k) if k.code == KeyCode::Char('1')
        ));
        let no = approval_overlay_key(
            &KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL),
            &request,
        );
        assert!(matches!(
            no,
            TuiEvent::Key(k) if k.code == KeyCode::Char('2')
        ));
        // An unrelated key passes through untouched.
        let other = approval_overlay_key(
            &KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
            &request,
        );
        assert!(matches!(
            other,
            TuiEvent::Key(k) if k.code == KeyCode::Char('x')
        ));
    }

    fn shell_approval_event(id: &str, command: &str) -> DaemonEvent {
        DaemonEvent::ToolApprovalRequested {
            request_id: id.into(),
            summary: format!("shell {{\"command\":\"{command}\"}}"),
            call_name: "shell".into(),
            arguments_json: format!("{{\"command\":\"{command}\"}}"),
            policy_source: Some("mode:ask".into()),
        }
    }

    /// Spec 05: a shell request offers allow once / allow for session / deny /
    /// redirect, names where it runs from the session directory when the call
    /// names none, and states the policy's own reason.
    #[test]
    fn a_shell_approval_offers_the_session_grant_and_the_policy_reason() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        session.approval_cwd = Some("D:/repo".into());
        session.deny_rules = Some(vec![]);
        let mut transcript = Transcript::new();
        session.record(&mut transcript, shell_approval_event("c1", "echo hi"));
        let request = session.approval_overlay.current_request().expect("card");
        assert_eq!(
            request.options,
            vec![
                ApprovalDecision::Approved,
                ApprovalDecision::ApprovedForPrefix,
                ApprovalDecision::ApprovedForSession,
                ApprovalDecision::Denied,
                ApprovalDecision::Redirect,
            ]
        );
        assert_eq!(
            request.policy_reason.as_deref(),
            Some("shell commands need approval in Ask mode")
        );
        match &request.kind {
            ApprovalKind::Exec {
                cwd,
                writes,
                network,
                ..
            } => {
                assert_eq!(cwd.as_deref(), Some("D:/repo"));
                // Nothing in the call declares these, so they are not shown.
                assert_eq!((writes, network), (&None, &None));
            }
            other => panic!("expected Exec, got {other:?}"),
        }
        // No transcript echo: the card is the only approval prompt.
        assert!(settled_transcript_text(&mut transcript).is_empty());
    }

    fn deny(pattern: &str) -> PermissionRuleView {
        PermissionRuleView {
            pattern: pattern.into(),
            effect: "deny".into(),
        }
    }

    #[test]
    fn always_allow_is_withheld_while_the_rules_are_unknown() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.record(&mut transcript, shell_approval_event("c1", "echo hi"));
        let request = session.approval_overlay.current_request().expect("card");
        assert!(!request
            .options
            .contains(&ApprovalDecision::ApprovedForPrefix));
        assert!(session.approval_allow_rules.is_empty());
    }

    #[test]
    fn always_allow_is_never_offered_when_a_deny_rule_blocks_the_request() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        session.deny_rules = Some(vec![deny("shell(echo *)")]);
        let mut transcript = Transcript::new();
        session.record(&mut transcript, shell_approval_event("c1", "echo hi"));
        let request = session.approval_overlay.current_request().expect("card");
        assert!(!request
            .options
            .contains(&ApprovalDecision::ApprovedForPrefix));
        assert!(session.approval_allow_rules.is_empty());
    }

    #[test]
    fn always_allow_shows_and_records_the_exact_rule_for_a_clear_request() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        session.deny_rules = Some(vec![deny("shell(rm *)")]);
        let mut transcript = Transcript::new();
        session.record(&mut transcript, shell_approval_event("c1", "git status"));
        let request = session.approval_overlay.current_request().expect("card");
        assert!(request
            .options
            .contains(&ApprovalDecision::ApprovedForPrefix));
        let shown = request.allow_rule().expect("rule shown on the card");
        assert_eq!(
            shown.patterns,
            vec!["shell(git status)", "shell(git status *)"]
        );
        let (stored, _) = session.approval_allow_rules.get("c1").expect("stored");
        assert_eq!(stored, shown);
    }

    #[test]
    fn deny_matching_covers_commands_and_paths() {
        let exec = ApprovalKind::Exec {
            command: "git push --force".into(),
            cwd: None,
            reason: None,
            writes: None,
            network: None,
        };
        assert!(request_denied_by_rules(&exec, &[deny("shell(git push*)")]));
        assert!(!request_denied_by_rules(&exec, &[deny("shell(rm *)")]));
        let allow = PermissionRuleView {
            pattern: "shell(git push*)".into(),
            effect: "allow".into(),
        };
        assert!(!request_denied_by_rules(&exec, &[allow]));
        let mut changes = std::collections::BTreeMap::new();
        changes.insert(
            std::path::PathBuf::from("secrets/key.pem"),
            rightkit_terminal_ui::render::diff::FileChange::add("x".to_string()),
        );
        let patch = ApprovalKind::Patch {
            changes,
            cwd: std::path::PathBuf::new(),
            reason: None,
        };
        assert!(request_denied_by_rules(
            &patch,
            &[deny("file_edit(secrets/*)")]
        ));
    }

    #[derive(Default)]
    struct RulesHost {
        rules: Vec<PermissionRuleView>,
        installed: std::sync::Mutex<Vec<String>>,
    }

    impl TurnHost for RulesHost {
        async fn cancel_turn(&self) -> Result<(), String> {
            Ok(())
        }
        fn spawn_turn(&self, _prompt: String) -> JoinHandle<i32> {
            tokio::spawn(async { 0 })
        }
        fn resolve_command(&self, _input: &str) -> Option<String> {
            None
        }
        fn check_command(&self, _name: &str, _mid_turn: bool) -> Result<(), String> {
            Ok(())
        }
        async fn execute_command(&self, _name: &str, _rest: &str) -> bool {
            false
        }
        fn completions(&self, _prefix: &str) -> Vec<String> {
            Vec::new()
        }
        fn active_directory(&self) -> Option<String> {
            Some("D:/repo".into())
        }
        async fn permission_rules(&self, _s: &str) -> Result<Vec<PermissionRuleView>, String> {
            Ok(self.rules.clone())
        }
        async fn install_permission_rule(
            &self,
            _s: &str,
            pattern: &str,
            root: &str,
        ) -> Result<(), String> {
            self.installed
                .lock()
                .unwrap()
                .push(format!("{root}|{pattern}"));
            Ok(())
        }
    }

    fn git_status_entry() -> Option<(rightkit_terminal_ui::render::allow_rule::AllowRule, ApprovalKind)> {
        let kind = ApprovalKind::Exec {
            command: "git status".into(),
            cwd: None,
            reason: None,
            writes: None,
            network: None,
        };
        rightkit_terminal_ui::render::allow_rule::derive_allow_rule(&kind, Some("mode:ask"))
            .map(|rule| (rule, kind))
    }

    #[tokio::test]
    async fn installing_the_allow_rule_posts_exactly_the_shown_patterns() {
        let host = RulesHost::default();
        install_allow_rule(&host, git_status_entry(), "s1")
            .await
            .expect("installed");
        assert_eq!(
            *host.installed.lock().unwrap(),
            vec!["D:/repo|shell(git status)", "D:/repo|shell(git status *)"]
        );
    }

    #[tokio::test]
    async fn a_deny_that_appeared_after_the_card_stops_the_install() {
        let host = RulesHost {
            rules: vec![deny("shell(git *)")],
            ..Default::default()
        };
        let error = install_allow_rule(&host, git_status_entry(), "s1")
            .await
            .unwrap_err();
        assert!(error.contains("deny"), "{error}");
        assert!(host.installed.lock().unwrap().is_empty());
        assert!(install_allow_rule(&host, None, "s1").await.is_err());
    }

    fn elicitation_event(schema: &str) -> DaemonEvent {
        DaemonEvent::McpElicitationRequested {
            server_name: "files".into(),
            request_id: "7".into(),
            mode: "form".into(),
            message: "Which project?".into(),
            requested_schema: Some(schema.into()),
            url: None,
            elicitation_id: None,
        }
    }

    #[test]
    fn an_mcp_form_request_renders_one_row_per_field() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.record(
            &mut transcript,
            elicitation_event(
                r#"{"type":"object","properties":{"name":{"type":"string","title":"Name"},"deep":{"type":"boolean"}},"required":["name"]}"#,
            ),
        );
        let pending = session.pending_elicitation.as_ref().expect("pending");
        assert!(pending.form.is_some());
        assert_eq!(pending.request_id, "files:7");
        let text = session.interaction_text().expect("card text");
        assert!(text.contains("MCP request · files"), "{text}");
        assert!(text.contains("Which project?"), "{text}");
        assert!(text.contains("1/2 Name *"), "{text}");
        assert!(text.contains("2/2 deep"), "{text}");
    }

    #[test]
    fn an_unrepresentable_schema_falls_back_to_raw_json_entry() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.record(
            &mut transcript,
            elicitation_event(r#"{"type":"object","properties":{"o":{"type":"object"}}}"#),
        );
        let pending = session.pending_elicitation.as_ref().expect("pending");
        assert!(pending.form.is_none());
        assert!(session.interaction_text().expect("text").contains("JSON:"));
    }

    #[test]
    fn only_commands_can_be_granted_for_the_session() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.record(
            &mut transcript,
            DaemonEvent::ApprovalRequested {
                request_id: "g1".into(),
                summary: "allow network access".into(),
            },
        );
        let request = session.approval_overlay.current_request().expect("card");
        assert_eq!(
            request.options,
            vec![
                ApprovalDecision::Approved,
                ApprovalDecision::Denied,
                ApprovalDecision::Redirect,
            ]
        );
        assert_eq!(request.policy_reason, None, "no policy source, no reason");
    }

    #[test]
    fn policy_reasons_are_worded_only_for_known_sources() {
        let exec = ApprovalKind::Exec {
            command: "x".into(),
            cwd: None,
            reason: None,
            writes: None,
            network: None,
        };
        assert_eq!(
            approval_policy_reason(Some("mode:accept-edits"), &exec).as_deref(),
            Some("shell commands need approval in Accept edits mode")
        );
        assert_eq!(
            approval_policy_reason(Some("abac:deny-prod"), &exec).as_deref(),
            Some("policy abac:deny-prod requires approval")
        );
        assert_eq!(approval_policy_reason(None, &exec), None);
        assert_eq!(approval_policy_reason(Some("  "), &exec), None);
    }

    /// Item 2: a rejected shell call is one readable decision line, never
    /// the raw `shell {json}` summary.
    #[test]
    fn a_rejected_shell_call_reads_as_one_human_line_without_json() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.record(&mut transcript, shell_approval_event("c1", "echo hi"));
        session.record(
            &mut transcript,
            DaemonEvent::ApprovalResolved {
                request_id: "c1".into(),
                approved: false,
            },
        );
        let text = settled_transcript_text(&mut transcript);
        assert_eq!(text.trim_end(), "  ✗ rejected shell · echo hi");
        assert!(!text.contains('{') && !text.contains("command\""), "{text}");
    }

    #[test]
    fn a_session_grant_is_recorded_as_such() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.record(&mut transcript, shell_approval_event("c1", "echo hi"));
        session.session_grants.insert("c1".into());
        session.record(
            &mut transcript,
            DaemonEvent::ApprovalResolved {
                request_id: "c1".into(),
                approved: true,
            },
        );
        let text = settled_transcript_text(&mut transcript);
        assert_eq!(
            text.trim_end(),
            "  ✓ allowed shell · echo hi · this session"
        );
        assert!(session.session_grants.is_empty());
    }

    #[test]
    fn a_plain_allow_is_recorded_without_the_session_marker() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.record(&mut transcript, shell_approval_event("c1", "echo hi"));
        session.record(
            &mut transcript,
            DaemonEvent::ApprovalResolved {
                request_id: "c1".into(),
                approved: true,
            },
        );
        let text = settled_transcript_text(&mut transcript);
        assert_eq!(text.trim_end(), "  ✓ allowed shell · echo hi");
    }

    /// Item 2: Esc-rejecting prints one stop notice; the runtime's own
    /// `Turn interrupted` for the same stop is not shown a second time, and a
    /// later, unrelated interruption still is.
    #[test]
    fn a_rejection_stop_shows_one_notice_not_two() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.swallow_interrupt_notice = true;
        session.notice(&mut transcript, "Turn stopped · request rejected");
        session.notice(&mut transcript, "Turn interrupted: interrupted by user");
        let text = settled_transcript_text(&mut transcript);
        assert_eq!(text.trim_end(), "  Turn stopped · request rejected");
        assert!(!session.swallow_interrupt_notice);
        session.notice(&mut transcript, "Turn interrupted: user pressed Esc");
        let later = settled_transcript_text(&mut transcript);
        assert_eq!(later.trim_end(), "  Turn interrupted: user pressed Esc");
    }

    #[test]
    fn a_new_user_message_clears_a_stale_interrupt_suppression() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.swallow_interrupt_notice = true;
        session.record(
            &mut transcript,
            DaemonEvent::UserMessage {
                text: "again".into(),
            },
        );
        session.notice(&mut transcript, "Turn interrupted");
        let text = settled_transcript_text(&mut transcript);
        assert!(text.contains("Turn interrupted"), "{text}");
    }

    /// Item 1: the user's message is a red-rail panel line, not a `You`
    /// header; a system notice is one dim line, not a `System` header.
    #[test]
    fn user_messages_and_notices_have_no_speaker_headers() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.record(
            &mut transcript,
            DaemonEvent::UserMessage {
                text: "run the shell command echo hi".into(),
            },
        );
        session.notice(&mut transcript, "Model set to gpt-6.1-sol.");
        let text = settled_transcript_text(&mut transcript);
        let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
        assert_eq!(lines[0], "▌ run the shell command echo hi", "{text:?}");
        assert!(
            !text.contains("You") && !text.contains("System"),
            "{text:?}"
        );
        assert!(lines.contains(&"  Model set to gpt-6.1-sol."), "{text:?}");
    }

    /// Item 1: tool calls are the aligned verb ledger; a successful shell
    /// collapses to one row, a failure opens to its first error.
    #[test]
    fn tool_calls_render_as_the_ledger_and_failures_open_to_the_first_error() {
        use rightkit_terminal_ui::transcript::timeline::ToolStatus;
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        let start = |id: &str, name: &str, args: &str| DaemonEvent::ToolStarted {
            call_id: id.into(),
            name: name.into(),
            args_preview: args.into(),
        };
        session.record(
            &mut transcript,
            start("a", "shell", r#"{"command":"echo hi"}"#),
        );
        let running: Vec<String> = session
            .live_cell_lines(80, &mono_theme())
            .into_iter()
            .map(footer_text)
            .collect();
        assert!(running[0].starts_with("  ● shell   echo hi"), "{running:?}");
        session.record(
            &mut transcript,
            DaemonEvent::ToolUpdated {
                call_id: "a".into(),
                status: ToolStatus::Succeeded,
                output_preview: "hi".into(),
            },
        );
        session.record(
            &mut transcript,
            start("b", "shell", r#"{"command":"cargo test"}"#),
        );
        session.record(
            &mut transcript,
            DaemonEvent::ToolUpdated {
                call_id: "b".into(),
                status: ToolStatus::Failed,
                output_preview: "running 2 tests\ntest x ... FAILED\nassertion failed".into(),
            },
        );
        let text = settled_transcript_text(&mut transcript);
        let rows: Vec<&str> = text.lines().map(str::trim_end).collect();
        assert!(rows[0].starts_with("  ✓ shell   echo hi"), "{text}");
        assert!(
            rows[0].ends_with("hi") || rows[0].contains("hi  "),
            "{text}"
        );
        assert!(rows[1].starts_with("  ✗ shell   cargo test"), "{text}");
        assert_eq!(rows[2], "     │ test x ... FAILED", "{text}");
        assert_eq!(
            rows[4], "     ╰ first error · exit 1 · ctrl+o full output",
            "{text}"
        );
        assert!(!text.contains("{\"command\""), "{text}");
    }

    /// Item 6: startup progress is a static accent glyph with dim text — no
    /// blue Running colour, no `◌` spinner glyph — and a clock only once it
    /// has run long enough to matter.
    #[test]
    fn starting_context_is_a_static_dim_marker_with_a_clock_only_when_long() {
        use rightkit_terminal_ui::component::Role;
        let theme = Theme::new(
            rightkit_terminal_ui::terminal::Background::Dark,
            rightkit_terminal_ui::terminal::ColorDepth::TrueColor,
        );
        let line = |secs| {
            footer_line(
                110,
                "/repo",
                Some("main"),
                "m",
                rightkit_agent_policy::PermissionMode::Ask,
                Some(&HostReadiness::StartingContext),
                None,
                None,
                None,
                false,
                secs,
                &theme,
            )
        };
        let quick = line(2);
        let text = footer_text(quick.clone());
        assert!(text.contains("• Starting context…"), "{text}");
        assert!(!text.contains('◌') && !text.contains("0:0"), "{text}");
        let running_blue = theme.color(Role::Running);
        for span in &quick.spans {
            assert_ne!(span.style.fg, Some(running_blue), "{:?}", span.content);
        }
        let glyph = quick
            .spans
            .iter()
            .find(|s| s.content == "•")
            .expect("glyph span");
        assert_eq!(glyph.style.fg, Some(theme.accent()));
        let label = quick
            .spans
            .iter()
            .find(|s| s.content.contains("Starting context"))
            .expect("label span");
        assert_eq!(label.style.fg, Some(theme.color(Role::Dim)));
        assert!(footer_text(line(7)).contains("Starting context… 0:07"));
        assert!(footer_text(line(75)).contains("Starting context… 1:15"));
        let ready = footer_text(footer_line(
            110,
            "/repo",
            None,
            "m",
            rightkit_agent_policy::PermissionMode::Ask,
            Some(&HostReadiness::Ready),
            None,
            None,
            None,
            false,
            99,
            &theme,
        ));
        assert!(!ready.contains("Starting"), "{ready}");
    }

    /// An external settle removes the pending request and records the
    /// donor decision cell in the transcript.
    #[test]
    fn externally_resolved_approvals_leave_a_decision_record() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.record(
            &mut transcript,
            DaemonEvent::ApprovalRequested {
                request_id: "req-9".into(),
                summary: "run rm".into(),
            },
        );
        session.record(
            &mut transcript,
            DaemonEvent::ApprovalResolved {
                request_id: "req-9".into(),
                approved: false,
            },
        );
        assert!(session.pending_approvals.is_empty());
        assert!(session.approval_overlay.is_complete());
        let text = settled_transcript_text(&mut transcript);
        assert!(text.contains("rejected run rm"), "{text:?}");
    }

    /// Switching sessions rebinds the overlay: a request keyed to the old
    /// session can never be answered from the new session's view.
    #[test]
    fn session_reset_rebinds_the_approval_overlay() {
        let mut session = SessionState::new("first");
        let mut transcript = Transcript::new();
        session.record(
            &mut transcript,
            DaemonEvent::ApprovalRequested {
                request_id: "r1".into(),
                summary: "old".into(),
            },
        );
        session.reset_view("second");
        session.active_session = "second".into();
        assert!(session.pending_approvals.is_empty());
        assert!(session.approval_overlay.pending_ids().is_empty());
        // A request stamped for the session we left is rejected outright.
        assert!(!session.approval_overlay.enqueue(ApprovalRequest::new(
            "first",
            "r1",
            ApprovalKind::Generic {
                description: "stale".into(),
            },
            vec![ApprovalDecision::Approved],
        )));
    }

    /// A canonical `toolApprovalRequested` carrying a shell command reaches
    /// the overlay as the donor Exec preview — command verbatim — while the
    /// dedup, quarantine and session-binding invariants are unchanged.
    #[test]
    fn tool_approval_request_builds_an_exec_preview() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.record(
            &mut transcript,
            DaemonEvent::ToolApprovalRequested {
                request_id: "req-exec".into(),
                summary: "shell {\"command\":\"cargo test\"}".into(),
                call_name: "shell".into(),
                arguments_json: "{\"command\":\"cargo test\",\"cwd\":\"D:/work\"}".into(),
                policy_source: Some("mode:ask".into()),
            },
        );
        let request = session
            .approval_overlay
            .current_request()
            .expect("typed request must reach the overlay");
        match &request.kind {
            ApprovalKind::Exec { command, cwd, .. } => {
                assert_eq!(command, "cargo test");
                assert_eq!(cwd.as_deref(), Some("D:/work"));
            }
            other => panic!("expected Exec preview, got {other:?}"),
        }
        assert_eq!(request.request_id, "req-exec");
        assert_eq!(request.session_id, LOCAL_SESSION_ID);
        assert!(
            session.take_input_quarantine(),
            "typed requests quarantine pre-modal input exactly like free-text ones"
        );
        assert!(!session.approval_input_safe);
    }

    /// `path`+`content` calls render the donor Patch preview as a whole-file
    /// add; a unified-diff argument renders as an update. Calls whose
    /// arguments cannot be parsed degrade to the Generic description rather
    /// than fabricating a preview.
    #[test]
    fn tool_approval_request_builds_a_patch_preview_and_degrades() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.record(
            &mut transcript,
            DaemonEvent::ToolApprovalRequested {
                request_id: "req-write".into(),
                summary: "write_file src/new.rs".into(),
                call_name: "write_file".into(),
                arguments_json: "{\"path\":\"src/new.rs\",\"content\":\"fn f() {}\"}".into(),
                policy_source: None,
            },
        );
        let request = session.approval_overlay.current_request().unwrap();
        match &request.kind {
            ApprovalKind::Patch { changes, .. } => {
                assert!(
                    changes.contains_key(std::path::Path::new("src/new.rs")),
                    "patch preview must name the wire path, got {changes:?}"
                );
            }
            other => panic!("expected Patch preview, got {other:?}"),
        }
        session.record(
            &mut transcript,
            DaemonEvent::ToolApprovalRequested {
                request_id: "req-diff".into(),
                summary: "edit src/lib.rs".into(),
                call_name: "edit".into(),
                arguments_json:
                    "{\"path\":\"src/lib.rs\",\"diff\":\"@@ -1 +1 @@\\n-old\\n+new\\n\"}".into(),
                policy_source: None,
            },
        );
        session.record(
            &mut transcript,
            DaemonEvent::ToolApprovalRequested {
                request_id: "req-bad".into(),
                summary: "opaque tool".into(),
                call_name: "mystery".into(),
                arguments_json: "not json".into(),
                policy_source: None,
            },
        );
        assert_eq!(session.pending_approvals.len(), 3);
        // The malformed request queued behind the two typed ones; its kind
        // is Generic, so the overlay shows the description, not a preview.
        assert!(session
            .pending_approvals
            .iter()
            .any(|(_, id)| id == "req-bad"));
    }

    /// A typed request for a different session is rejected by the overlay's
    /// binding exactly like a free-text one — the richer payload never
    /// widens what this view can answer.
    #[test]
    fn typed_approval_requests_keep_session_binding_and_dedup() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        let event = |id: &str| DaemonEvent::ToolApprovalRequested {
            request_id: id.into(),
            summary: "shell pwd".into(),
            call_name: "shell".into(),
            arguments_json: "{\"command\":\"pwd\"}".into(),
            policy_source: None,
        };
        session.record(&mut transcript, event("req-1"));
        session.record(&mut transcript, event("req-1"));
        assert_eq!(session.pending_approvals.len(), 1, "duplicate ids dedup");
        assert!(!session.approval_overlay.enqueue(ApprovalRequest::new(
            "other-session",
            "req-9",
            ApprovalKind::Exec {
                command: "ls".into(),
                cwd: None,
                reason: None,
                writes: None,
                network: None,
            },
            vec![ApprovalDecision::Approved],
        )));
    }

    // Installed-fixture 106 signature: a turn's cells finalize, the scrollback
    // insertion slides the footprint down, and the settle frame shrinks the
    // viewport in the same pass. The slide physically moved the old paint, so
    // the released tail sits `slide` rows below `old.bottom()` — erasing to
    // `old.bottom()` left the stale streamed cell and the second composer
    // prompt on screen (cancel/retry rows 17-18, resize rows 9-10).
    /// Item 4: shrinking to 60 columns wraps the wide rows above the cursor
    /// (rules, working line, the queue hint), so the old viewport's top sits
    /// that many rows higher than cursor displacement says. Clearing from
    /// the displacement alone left the queue block painted twice.
    #[test]
    fn a_narrower_window_wraps_the_wide_rows_above_the_cursor() {
        // queue row, queue hint, working line, rule, composer | rule, footer
        let drawn: Vec<String> = [44usize, 70, 110, 110, 30, 110, 110]
            .iter()
            .map(|n| "x".repeat(*n))
            .collect();
        // Cursor on the composer (row 4): hint, working line and rule wrap.
        assert_eq!(reflow_rows_above(&drawn, 4, 60), 3);
        // Rows below the cursor never move its top.
        assert_eq!(reflow_rows_above(&drawn, 2, 60), 1);
        // A window as wide as the frame wraps nothing; blank rows add none.
        assert_eq!(reflow_rows_above(&drawn, 4, 110), 0);
        assert_eq!(reflow_rows_above(&[String::new(), String::new()], 2, 40), 0);
        // Very narrow: ceil(width / cols) - 1 extra rows per wide row.
        assert_eq!(reflow_rows_above(&drawn, 4, 30), 1 + 2 + 3 + 3);
        assert_eq!(reflow_rows_above(&drawn, 4, 0), 0);
    }

    fn screen_reader(rows: Vec<(u16, &'static str)>) -> impl Fn(u16) -> Option<String> {
        move |row| {
            Some(
                rows.iter()
                    .find(|(index, _)| *index == row)
                    .map_or(String::new(), |(_, text)| (*text).to_string()),
            )
        }
    }

    /// Item 4 (the draft-composer case): the displacement estimate landed one
    /// row below the real top, leaving the first wrapped half of the old top
    /// rule painted above the redrawn frame. The screen scan extends the top
    /// over every row that is still a piece of the old frame.
    #[test]
    fn the_clear_extends_over_leftover_rows_of_the_old_frame() {
        let rule = "─".repeat(110);
        let old: Vec<String> = vec![
            rule.clone(),
            "▌ first line of a draft".into(),
            "  second line of a draft".into(),
            rule.clone(),
            r" D:\Claude\coderight · main · Ask".into(),
        ];
        let rule60: &'static str = Box::leak("─".repeat(60).into_boxed_str());
        // Real top is row 13 (the rule's first 60 columns); the estimate says 14.
        let screen = screen_reader(vec![
            (10, "  RightKit  0.1.116"),
            (11, "  ✓ ready · gpt-6.1-sol · high"),
            (13, rule60),
            (14, rule60),
        ]);
        assert_eq!(
            stale_top_above(14, &old, 60, &Default::default(), &screen),
            13
        );
        // Both halves of the wrapped rule are recognised; the history lines
        // above are not, so the walk stops there.
        let screen = screen_reader(vec![
            (11, "  ✓ ready · gpt-6.1-sol · high"),
            (12, rule60),
            (13, rule60),
        ]);
        assert_eq!(
            stale_top_above(14, &old, 60, &Default::default(), &screen),
            12
        );
    }

    #[test]
    fn the_clear_never_reaches_into_unrelated_history() {
        let old: Vec<String> = vec!["▌ type to queue".into(), "─".repeat(110)];
        // Nothing above the estimate belongs to the old frame.
        let screen = screen_reader(vec![
            (8, "Turn stopped · request rejected"),
            (9, "  ✗ rejected shell · echo hi"),
        ]);
        assert_eq!(
            stale_top_above(10, &old, 60, &Default::default(), &screen),
            10
        );
        // An unrelated row stops the walk, even with old-frame rows above it.
        let screen = screen_reader(vec![(6, "▌ type to queue"), (9, "unrelated")]);
        assert_eq!(
            stale_top_above(10, &old, 60, &Default::default(), &screen),
            10
        );
        let screen = screen_reader(vec![(7, "▌ type to queue")]);
        assert_eq!(
            stale_top_above(10, &old, 60, &Default::default(), &screen),
            7
        );
        // An unreadable screen (non-Windows) leaves the estimate alone.
        assert_eq!(
            stale_top_above(10, &old, 60, &Default::default(), |_| None),
            10
        );
        assert_eq!(
            stale_top_above(0, &old, 60, &Default::default(), &screen),
            0
        );
    }

    #[test]
    fn released_rows_below_track_the_history_slide() {
        // Old footprint {12,5} slid down 2 rows (paint now at rows 14..=18);
        // the frame then shrank to {14,3}. Stale tail: 0-based rows 17,18.
        let mut out = Vec::new();
        erase_released_rows(
            &mut out,
            Rect::new(0, 12, 80, 5),
            Rect::new(0, 14, 80, 3),
            2,
        )
        .unwrap();
        let bytes = String::from_utf8(out).unwrap();
        assert_eq!(
            bytes, "\x1b[18;1H\x1b[K\x1b[19;1H\x1b[K",
            "the slid tail past the pre-slide bottom must be erased: {bytes:?}"
        );
    }

    #[test]
    fn released_rows_below_without_a_slide_stop_at_the_old_bottom() {
        // No insertion between frames: a plain shrink erases exactly the
        // released rows of the old footprint — nothing more.
        let mut out = Vec::new();
        erase_released_rows(
            &mut out,
            Rect::new(0, 14, 80, 5),
            Rect::new(0, 14, 80, 3),
            0,
        )
        .unwrap();
        let bytes = String::from_utf8(out).unwrap();
        assert_eq!(
            bytes, "\x1b[18;1H\x1b[K\x1b[19;1H\x1b[K",
            "plain shrink releases the old tail rows: {bytes:?}"
        );
    }

    #[test]
    fn a_history_slide_without_shrink_releases_nothing() {
        // Same-height slide: the slid paint is fully re-covered by the new
        // footprint and the vacated head rows hold inserted history — no row
        // is stale, so no erase may be emitted (sparing inserted scrollback).
        let mut out = Vec::new();
        erase_released_rows(
            &mut out,
            Rect::new(0, 12, 80, 5),
            Rect::new(0, 14, 80, 5),
            2,
        )
        .unwrap();
        assert!(
            out.is_empty(),
            "a same-height slide releases no stale rows: {:?}",
            String::from_utf8_lossy(&out)
        );
    }
}

#[cfg(test)]
mod protected_input_tests {
    use super::tests::settled_transcript_text;
    use super::*;

    #[test]
    fn discard_failure_is_acknowledged_before_resume() {
        let pause = InputPause::new();
        let reader_pause = pause.clone();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            while !reader_pause.is_paused() {
                std::thread::yield_now();
            }
            assert!(reader_pause.reader_park_with(|| {
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(1)).unwrap();
                Err(io::Error::other("partial input boundary"))
            }));
        });
        let requester_pause = pause.clone();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let requester = std::thread::spawn(move || {
            result_tx
                .send(requester_pause.quarantine_and_park())
                .unwrap();
        });
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(result_rx.try_recv().is_err());
        release_tx.send(()).unwrap();
        assert_eq!(
            result_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            Err("partial input boundary".into())
        );
        assert!(pause.is_paused());
        pause.resume();
        reader.join().unwrap();
        requester.join().unwrap();
    }

    #[test]
    fn abandoned_quarantine_cannot_discard_next_interactions_input() {
        let pause = InputPause::new();
        *pause.lock() = PauseState::Requested;
        pause.quarantine.store(true, Ordering::Release);
        pause.resume();
        assert!(!pause.quarantine.load(Ordering::Acquire));
        assert!(!pause.reader_park_with(|| panic!("stale discard")));
    }

    #[test]
    fn internal_wire_labels_are_recognized_and_human_notices_are_not() {
        assert!(is_internal_wire_label("[selfReviewProposalReceived]"));
        assert!(is_internal_wire_label(
            "[selfReviewProposalReceived] clipped payload"
        ));
        assert!(!is_internal_wire_label("[exit 3]"));
        assert!(!is_internal_wire_label("[post-edit verification] ok"));
        assert!(!is_internal_wire_label("[truncated]"));
        assert!(!is_internal_wire_label("plain text"));
    }

    #[test]
    fn internal_wire_event_name_never_renders_in_the_transcript() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        // `SessionState::new` starts with the outcome already seen; begin a
        // turn so the final assertion proves the internal event did not count
        // as a visible outcome.
        session.begin_turn_status();
        assert!(!session.turn_outcome_seen);
        session.record(
            &mut transcript,
            DaemonEvent::System {
                text: "[selfReviewProposalReceived]".into(),
            },
        );
        let text = settled_transcript_text(&mut transcript);
        assert!(!text.contains("selfReviewProposalReceived"), "{text}");
        assert!(!session.turn_outcome_seen);
    }

    #[test]
    fn persistence_failed_notice_renders_in_the_transcript() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.record(
            &mut transcript,
            DaemonEvent::System {
                text: "Session persistence failed: disk full. In-flight tools were cancelled and new work is not being saved.".into(),
            },
        );
        let text = settled_transcript_text(&mut transcript);
        assert!(
            text.contains("Session persistence failed: disk full"),
            "{text}"
        );
    }

    #[test]
    fn failed_tool_reports_the_real_exit_code() {
        assert_eq!(tool_exit_code("[exit 3]\nboom"), 3);
        // A marker the command printed itself must not override the header.
        assert_eq!(tool_exit_code("[exit 3]\nlog: [exit 1]\n"), 3);
        // Header lost to truncation: the first line-start marker, else 1.
        assert_eq!(tool_exit_code("tail\n[exit 7]\n"), 7);
        assert_eq!(tool_exit_code("no marker"), 1);
    }

    #[test]
    fn failed_tool_row_footer_shows_exit_three() {
        use rightkit_terminal_ui::transcript::timeline::ToolStatus;
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.record(
            &mut transcript,
            DaemonEvent::ToolStarted {
                call_id: "c1".into(),
                name: "shell".into(),
                args_preview: "{\"command\":\"false\"}".into(),
            },
        );
        session.record(
            &mut transcript,
            DaemonEvent::ToolUpdated {
                call_id: "c1".into(),
                status: ToolStatus::Failed,
                output_preview: "[exit 3]\nboom\nlog: [exit 1]".into(),
            },
        );
        let text = settled_transcript_text(&mut transcript);
        assert!(text.contains("exit 3"), "{text}");
        assert!(!text.contains("exit 1 "), "{text}");
    }

    #[test]
    fn reasoning_settles_into_a_thought_row_even_behind_a_later_cell() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.record(
            &mut transcript,
            DaemonEvent::ReasoningDelta {
                token: "**Exploring volcanic hazards** details".into(),
            },
        );
        // A tool row that landed after the open block must not strand it.
        session.push_live_cell(
            LiveCell::Exec(ExecCell::new(ExecCall::tool("c9", "shell", "{}"))),
            vec!["c9".into()],
        );
        session.settle_reasoning(&mut transcript);
        let text = settled_transcript_text(&mut transcript);
        assert!(
            text.contains("Thought: Exploring volcanic hazards"),
            "{text}"
        );
        assert!(session.exec_cell_for("c9").is_some());
    }

    #[test]
    fn a_result_without_a_live_row_still_shows_one_tool_row_once() {
        use rightkit_terminal_ui::transcript::timeline::ToolStatus;
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        let done = || DaemonEvent::ToolUpdated {
            call_id: "granted-1".into(),
            status: ToolStatus::Succeeded,
            output_preview: "ok".into(),
        };
        session.record(&mut transcript, done());
        let first = settled_transcript_text(&mut transcript);
        assert!(!first.trim().is_empty(), "session-granted run drew no row");
        // The same result delivered again (live then durable) adds nothing.
        session.record(&mut transcript, done());
        assert!(settled_transcript_text(&mut transcript).trim().is_empty());
    }

    #[test]
    fn ctrl_o_target_never_outlives_its_turn() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.record(
            &mut transcript,
            DaemonEvent::ReasoningDelta {
                token: "**Checking the tree** first".into(),
            },
        );
        session.settle_reasoning(&mut transcript);
        assert!(session.expandable.is_some(), "settled cell is expandable");
        // A new user turn drops it, so Ctrl+O cannot re-append an earlier
        // turn's cell as a stray row.
        session.record(
            &mut transcript,
            DaemonEvent::UserMessage {
                text: "next".into(),
            },
        );
        assert!(session.expandable.is_none());
    }

    #[test]
    fn replayed_answer_settles_the_thought_row_before_it() {
        let mut session = SessionState::new(LOCAL_SESSION_ID);
        let mut transcript = Transcript::new();
        session.record(
            &mut transcript,
            DaemonEvent::ReasoningDelta {
                token: "**Weighing options** carefully".into(),
            },
        );
        session.record(
            &mut transcript,
            DaemonEvent::AssistantCompleted {
                text: "Final answer".into(),
            },
        );
        let text = settled_transcript_text(&mut transcript);
        let thought = text.find("Thought: Weighing options").expect(&text);
        let answer = text.find("Final answer").expect(&text);
        assert!(thought < answer, "Thought landed after the answer: {text}");
    }
}
