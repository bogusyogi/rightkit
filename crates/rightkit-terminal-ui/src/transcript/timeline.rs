//! S:I3 — typed timeline state.
//!
//! [`Transcript`](super::Transcript) collapses everything the daemon sends
//! into `Speaker{User,Assistant,System}` text. That is sufficient for plain
//! chat but cannot represent a reasoning block, a tool call with its own
//! status, an approval request, a route decision, or a task/subagent/check
//! event as anything other than a flattened string -- so nothing downstream
//! (detail-row copy of a single tool call, an identity/attribution strip
//! keyed to a route decision, a completion popup that needs to know "is a
//! turn open") can address these as distinct things.
//!
//! This module adds an *additive* typed model next to [`super::Transcript`]:
//! a [`TimelineEntry`] enum with one variant per daemon event family, and a
//! [`TimelineReducer`] that consumes [`DaemonEvent`]s and folds them into an
//! ordered [`Timeline`]. It does not replace `Transcript` or touch its
//! rendering -- `Transcript` keeps owning the scrollback-drain contract
//! (`entries[..flushed]` already in terminal scrollback) documented in
//! `super`. `Timeline` is the shape the rest of this lane's atoms (S:I5's
//! command-effect entries, S:I6's route attribution, S:I10's popup reading
//! "is a turn in flight") compose on top of.
//!
//! The live `tui_host` owns this reducer alongside its scrollback
//! [`super::Transcript`] projection, so typed event history survives while
//! existing terminal rendering keeps its established drain contract.

use std::fmt;

/// One raw event as the daemon feed delivers it. Named after the daemon
/// event families this lane's dispatch packet lists: AssistantDelta,
/// reasoning, tool, approval, route, task, subagent, check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonEvent {
    /// A user turn started.
    UserMessage {
        text: String,
    },
    /// A streamed token of assistant prose.
    AssistantDelta {
        token: String,
    },
    /// The assistant turn is fully settled; no more deltas will arrive for
    /// the entry currently open.
    AssistantSettled,
    /// Canonical completed prose, including turns with no streamed deltas.
    AssistantCompleted {
        text: String,
    },
    /// A reasoning/thinking delta, kept separate from assistant prose so a
    /// detail row can show or hide it independently (S:I4's copy affordance
    /// already assumes reasoning is addressable on its own).
    ReasoningDelta {
        token: String,
    },
    ReasoningSettled,
    /// Durable task-graph snapshot (`TaskGraphUpdated`): `(title, status)` per
    /// step, status one of `pending | in_progress | completed`. Carries plan
    /// state only; it adds no transcript row.
    TaskGraph {
        todos: Vec<(String, String)>,
    },
    /// A tool call started.
    ToolStarted {
        call_id: String,
        name: String,
        args_preview: String,
    },
    /// A tool call still being generated: the top-level string fields of its arguments read so
    /// far (a JSON object). Live-only; the call itself starts with `ToolStarted`.
    ToolArgumentsPartial {
        call_id: String,
        name: String,
        arguments_json: String,
    },
    /// A tool call's status changed (streaming output, or terminal state).
    ToolUpdated {
        call_id: String,
        status: ToolStatus,
        output_preview: String,
    },
    /// An approval gate opened, needing a user decision before continuing.
    ApprovalRequested {
        request_id: String,
        summary: String,
    },
    /// An approval gate opened for a concrete daemon tool call. Carries the
    /// exact nested `call{name, argumentsJson}` payload the
    /// `toolApprovalRequested` wire event delivers so presentation can build
    /// a typed Exec/Patch preview; `ApprovalRequested` remains for producers
    /// that only have a free-text description.
    ToolApprovalRequested {
        request_id: String,
        summary: String,
        call_name: String,
        arguments_json: String,
        /// The policy source that raised the prompt (`mode:ask`,
        /// `abac:<rule>`, ...), when the runtime reported one.
        policy_source: Option<String>,
    },
    McpElicitationRequested {
        server_name: String,
        request_id: String,
        mode: String,
        message: String,
        requested_schema: Option<String>,
        url: Option<String>,
        elicitation_id: Option<String>,
    },
    CredentialRequested {
        tool_call_id: String,
        provider: String,
    },
    CredentialResolved {
        tool_call_id: String,
        outcome: String,
    },
    ApprovalResolved {
        request_id: String,
        approved: bool,
    },
    UserQuestionAsked {
        question_id: String,
        questions: Vec<UserQuestionPrompt>,
    },
    UserQuestionAnswered {
        question_id: String,
    },
    PlanPresented {
        plan_id: String,
        plan: String,
    },
    PlanResolved {
        plan_id: String,
        approved: bool,
    },
    /// A routing decision was made (which model/provider/actor handled this
    /// turn). Carries the attribution fields S:I6's identity strip reads.
    RouteDecided {
        project: String,
        model: String,
        actor: String,
    },
    /// A background task changed state.
    TaskUpdated {
        task_id: String,
        label: String,
        status: TaskStatus,
    },
    /// A subagent was spawned or changed state.
    SubagentUpdated {
        subagent_id: String,
        label: String,
        status: TaskStatus,
    },
    /// A verification/check gate reported a result.
    CheckReported {
        check_id: String,
        label: String,
        passed: bool,
        detail: String,
    },
    /// A plain system notice (unattributable to any of the above).
    System {
        text: String,
    },
    /// A runtime state change that drives the working line (failure, retry
    /// backoff, compaction, verification). Never a timeline entry itself:
    /// the host shows it on the working line and as one dim notice.
    Status(RunSignal),
    /// Vendor account rate-limit telemetry (`accountRateLimited`). Never a
    /// timeline entry: the host keeps the latest state for the footer and
    /// prints one notice when the status changes.
    RateLimit {
        /// `allowed`, `allowed_warning` or `rejected`.
        status: String,
        /// The account label the limit applies to, when reported.
        account: Option<String>,
        /// The limit window (`five_hour`, `weekly`, ...), when reported.
        window: Option<String>,
        /// Unix seconds at which the window resets.
        resets_at: Option<u64>,
        /// Window utilization in permille (`0..=1000`), when reported.
        utilization_permille: Option<u32>,
    },
    /// Usage accounting after a provider response. Never a timeline entry:
    /// the host keeps the latest figures for the context gauge.
    Usage {
        /// Size of the context the next request carries.
        context_size: i64,
        /// Context window in use, in permille (`0..=1000`).
        context_permille: i64,
        /// Prompt size served from the provider's prompt cache.
        cache_read: i64,
        /// Prompt size of the latest request.
        prompt: i64,
        /// Cumulative output tokens when the event is a cumulative one, else the
        /// latest response's; the observability pane reads it only for cumulative events.
        completion: i64,
        /// Cumulative session cost in micro-USD when canonical pricing is known.
        cost_micros: Option<i64>,
        /// True only for cumulative usage events, which can explicitly mark cost unknown.
        cost_reported: bool,
        /// True when `cost_micros` is the provider's own charge rather than a price-table estimate.
        cost_billed: bool,
        /// The rate-limit budget the provider reported on this response's headers.
        rate_limit: Option<crate::observability_pane::ProviderRateLimit>,
        /// Milliseconds the prompt cache keeps the latest request's prefix, when the route has a lifetime.
        cache_ttl_ms: Option<u64>,
    },
}

/// A runtime signal the engine actually emitted that changes what the
/// working line says. Built only from real wire events; nothing here is
/// synthesised by the TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunSignal {
    /// The run failed (`runFailed`): reason plus the failure class.
    Failed { reason: String, hint: String },
    /// A retry is pending: a reason, an optional `attempt/max`, and an
    /// optional delay in whole seconds.
    Retrying {
        provider: String,
        reason: String,
        attempt: Option<(u32, u32)>,
        delay_secs: Option<u64>,
    },
    /// Context compaction began (`sessionCompactionStarted`).
    Compacting,
    /// Context was compacted (`sessionCompacted`).
    Compacted { dropped: i32 },
    /// A verification gate finished (`verificationFinished`).
    Verification {
        passed: bool,
        tool: String,
        issues: i32,
    },
    /// A deterministic pre-gate check finished (`checkRunCompleted`).
    Check { passed: bool, jury_skipped: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserQuestionPrompt {
    pub header: String,
    pub question: String,
    pub options: Vec<String>,
    /// One description per option (same order; may be shorter or empty).
    pub descriptions: Vec<String>,
    pub multi_select: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

/// One typed entry in the execution rail. Unlike `transcript::Message`,
/// each variant carries the fields specific to what actually happened,
/// rather than flattening everything to `(Speaker, String)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineEntry {
    User {
        text: String,
    },
    Assistant {
        text: String,
        finalized: bool,
    },
    Reasoning {
        text: String,
        finalized: bool,
    },
    Tool {
        call_id: String,
        name: String,
        args_preview: String,
        status: ToolStatus,
        output_preview: String,
    },
    Approval {
        request_id: String,
        summary: String,
        resolution: Option<bool>,
    },
    UserQuestion {
        question_id: String,
        summary: String,
        resolved: bool,
        cancelled: bool,
    },
    Plan {
        plan_id: String,
        plan: String,
        resolution: Option<bool>,
        cancelled: bool,
    },
    Route {
        project: String,
        model: String,
        actor: String,
    },
    Task {
        task_id: String,
        label: String,
        status: TaskStatus,
    },
    Subagent {
        subagent_id: String,
        label: String,
        status: TaskStatus,
    },
    Check {
        check_id: String,
        label: String,
        passed: bool,
        detail: String,
    },
    System {
        text: String,
    },
}

impl TimelineEntry {
    /// Whether this entry can still change (a streaming assistant/reasoning
    /// block, a running tool/task/subagent, an unresolved approval). Used by
    /// S:I10's completion popup ("is a turn in flight?") and by any future
    /// drain-to-scrollback pass that must not flush something still moving.
    pub fn is_open(&self) -> bool {
        match self {
            TimelineEntry::Assistant { finalized, .. } => !finalized,
            TimelineEntry::Reasoning { finalized, .. } => !finalized,
            TimelineEntry::Tool { status, .. } => *status == ToolStatus::Running,
            TimelineEntry::Approval { resolution, .. } => resolution.is_none(),
            TimelineEntry::UserQuestion {
                resolved,
                cancelled,
                ..
            } => !resolved && !cancelled,
            TimelineEntry::Plan {
                resolution,
                cancelled,
                ..
            } => resolution.is_none() && !cancelled,
            TimelineEntry::Task { status, .. } | TimelineEntry::Subagent { status, .. } => {
                *status == TaskStatus::Running
            }
            TimelineEntry::User { .. }
            | TimelineEntry::Route { .. }
            | TimelineEntry::Check { .. }
            | TimelineEntry::System { .. } => false,
        }
    }

    /// A short, single-line label for scrollback/detail-row rendering
    /// (feeds S:I4's `copy_line`-style addressing once wired at the host).
    pub fn label(&self) -> String {
        match self {
            TimelineEntry::User { .. } => "You".to_string(),
            TimelineEntry::Assistant { finalized, .. } => {
                if *finalized {
                    "Assistant".to_string()
                } else {
                    "Assistant …".to_string()
                }
            }
            TimelineEntry::Reasoning { .. } => "Reasoning".to_string(),
            TimelineEntry::Tool { name, status, .. } => format!("Tool {name} ({status:?})"),
            TimelineEntry::Approval { resolution, .. } => match resolution {
                None => "Approval (pending)".to_string(),
                Some(true) => "Approval (approved)".to_string(),
                Some(false) => "Approval (denied)".to_string(),
            },
            TimelineEntry::UserQuestion {
                resolved,
                cancelled,
                ..
            } => if *cancelled {
                "Question (cancelled)"
            } else if *resolved {
                "Question (answered)"
            } else {
                "Question (pending)"
            }
            .to_string(),
            TimelineEntry::Plan {
                cancelled: true, ..
            } => "Plan (cancelled)".to_string(),
            TimelineEntry::Plan { resolution, .. } => match resolution {
                None => "Plan (pending)".to_string(),
                Some(true) => "Plan (approved)".to_string(),
                Some(false) => "Plan (rejected)".to_string(),
            },
            TimelineEntry::Route { model, actor, .. } => format!("Route → {actor}/{model}"),
            TimelineEntry::Task { label, status, .. } => format!("Task {label} ({status:?})"),
            TimelineEntry::Subagent { label, status, .. } => {
                format!("Subagent {label} ({status:?})")
            }
            TimelineEntry::Check { label, passed, .. } => {
                format!("Check {label} ({})", if *passed { "pass" } else { "fail" })
            }
            TimelineEntry::System { .. } => "System".to_string(),
        }
    }
}

impl fmt::Display for TimelineEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label())
    }
}

/// Entries kept resident; older settled entries are dropped from the front (the durable store still holds them).
pub const TIMELINE_MAX_ENTRIES: usize = 20_000;
/// Eviction runs in chunks, not on every push.
const TIMELINE_EVICT_SLACK: usize = 1_024;

/// The resident cap and the eviction chunk. A journey lowers the cap with
/// `<PREFIX>_TIMELINE_MAX_ENTRIES` (the host's prefix from `terminal::notify::env_name`, `RIGHTKIT_`
/// by default; read once; unset, empty or not a number above 50 leaves the default), because
/// 20,000 entries cannot be produced through a real stream in a test run.
fn timeline_limits() -> (usize, usize) {
    static LIMITS: std::sync::OnceLock<(usize, usize)> = std::sync::OnceLock::new();
    *LIMITS.get_or_init(|| {
        match std::env::var(crate::terminal::notify::env_name("TIMELINE_MAX_ENTRIES"))
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .filter(|cap| *cap >= 50 && *cap < TIMELINE_MAX_ENTRIES)
        {
            Some(cap) => (cap, (cap / 4).min(TIMELINE_EVICT_SLACK)),
            None => (TIMELINE_MAX_ENTRIES, TIMELINE_EVICT_SLACK),
        }
    })
}

/// Ordered typed history. Appended to by the reducer and bounded: once more than [`TIMELINE_MAX_ENTRIES`] are
/// resident, the oldest settled entries are dropped from the front. `base` counts what was dropped, so an absolute
/// index (the position the entry had when it was pushed) stays valid: resident position = absolute - `base`.
#[derive(Debug, Default)]
pub struct Timeline {
    entries: Vec<TimelineEntry>,
    /// Number of entries evicted from the front so far.
    base: usize,
    /// The newest route entry, kept as it is pushed so the strip never scans the whole history.
    latest_route: Option<(String, String, String)>,
}

impl Timeline {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            base: 0,
            latest_route: None,
        }
    }

    pub fn push(&mut self, entry: TimelineEntry) {
        if let TimelineEntry::Route {
            project,
            model,
            actor,
        } = &entry
        {
            self.latest_route = Some((project.clone(), model.clone(), actor.clone()));
        }
        self.entries.push(entry);
        let (cap, slack) = timeline_limits();
        if self.entries.len() > cap + slack {
            self.evict_oldest_settled();
        }
    }

    /// Drop settled entries from the front down to the cap. Stops at the first entry that can still change, so
    /// every kept entry keeps its absolute index. A streaming entry that something newer was pushed after is
    /// superseded, not open (the turn-end settle closes it).
    fn evict_oldest_settled(&mut self) {
        let excess = self.entries.len().saturating_sub(timeline_limits().0);
        let last = self.entries.len().saturating_sub(1);
        let drop = self
            .entries
            .iter()
            .enumerate()
            .take(excess)
            .take_while(|(index, entry)| {
                !entry.is_open()
                    || (*index < last
                        && matches!(
                            entry,
                            TimelineEntry::Assistant { .. } | TimelineEntry::Reasoning { .. }
                        ))
            })
            .count();
        self.entries.drain(..drop);
        self.base += drop;
    }

    /// The resident entries (the newest window); the first one has absolute index [`Self::evicted`].
    pub fn entries(&self) -> &[TimelineEntry] {
        &self.entries
    }

    /// How many entries were evicted from the front.
    pub fn evicted(&self) -> usize {
        self.base
    }

    /// The entry at an absolute index, or `None` when it was evicted or never existed.
    pub fn get(&self, absolute: usize) -> Option<&TimelineEntry> {
        absolute
            .checked_sub(self.base)
            .and_then(|index| self.entries.get(index))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn last(&self) -> Option<&TimelineEntry> {
        self.entries.last()
    }

    pub fn last_mut(&mut self) -> Option<&mut TimelineEntry> {
        self.entries.last_mut()
    }

    /// True if any entry is still open (S:I10 needs this to decide whether
    /// mid-turn-only commands should show as disabled in the popup).
    pub fn has_open_entry(&self) -> bool {
        self.entries.iter().any(TimelineEntry::is_open)
    }

    /// The turn has ended: nothing in it can still be moving. Close whatever
    /// the stream left dangling (a call whose result never arrived, a prompt
    /// the user already answered through the card) so "a turn is in flight"
    /// cannot outlive the turn and keep session commands disabled.
    pub fn settle_open_entries(&mut self) {
        for entry in &mut self.entries {
            match entry {
                TimelineEntry::Tool { status, .. } if *status == ToolStatus::Running => {
                    *status = ToolStatus::Failed;
                }
                TimelineEntry::Approval { resolution, .. } if resolution.is_none() => {
                    *resolution = Some(false);
                }
                TimelineEntry::Assistant { finalized, .. }
                | TimelineEntry::Reasoning { finalized, .. } => *finalized = true,
                _ => {}
            }
        }
    }

    /// A settled turn can no longer receive question or plan responses.
    pub fn cancel_pending_interactions(&mut self) {
        for entry in &mut self.entries {
            match entry {
                TimelineEntry::UserQuestion {
                    resolved: false,
                    cancelled,
                    ..
                }
                | TimelineEntry::Plan {
                    resolution: None,
                    cancelled,
                    ..
                } => *cancelled = true,
                _ => {}
            }
        }
    }

    /// Most recent route decision, if any (S:I6's identity strip reads
    /// this for its attribution fields).
    pub fn latest_route(&self) -> Option<(&str, &str, &str)> {
        self.latest_route
            .as_ref()
            .map(|(project, model, actor)| (project.as_str(), model.as_str(), actor.as_str()))
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.base = 0;
        self.latest_route = None;
    }
}

/// Folds [`DaemonEvent`]s into a [`Timeline`], the S:I3 event-reducer layer.
///
/// One rule governs every delta event: a delta appends to the last entry
/// only if that entry is the matching open kind; otherwise (no entry yet,
/// or the last entry already settled/is a different kind) it opens a new
/// one. This mirrors `Transcript::append_stream`'s no-op-if-mismatched
/// discipline, extended to every streamable kind instead of only
/// `Assistant`.
#[derive(Debug, Default)]
pub struct TimelineReducer;

impl TimelineReducer {
    pub fn new() -> Self {
        Self
    }

    pub fn apply(&self, timeline: &mut Timeline, event: DaemonEvent) {
        match event {
            DaemonEvent::UserMessage { text } => {
                timeline.push(TimelineEntry::User { text });
            }
            DaemonEvent::AssistantDelta { token } => {
                if let Some(TimelineEntry::Assistant { text, finalized: false }) =
                    timeline.last_mut()
                {
                    text.push_str(&token);
                } else {
                    timeline.push(TimelineEntry::Assistant { text: token, finalized: false });
                }
            }
            DaemonEvent::AssistantSettled => {
                if let Some(TimelineEntry::Assistant { finalized, .. }) = timeline.last_mut() {
                    *finalized = true;
                }
            }
            DaemonEvent::AssistantCompleted { text } => {
                if let Some(TimelineEntry::Assistant { text: current, finalized: false }) =
                    timeline.last_mut()
                {
                    *current = text;
                    if let Some(TimelineEntry::Assistant { finalized, .. }) = timeline.last_mut() {
                        *finalized = true;
                    }
                } else if !text.is_empty() {
                    timeline.push(TimelineEntry::Assistant { text, finalized: true });
                }
            }
            DaemonEvent::ReasoningDelta { token } => {
                if let Some(TimelineEntry::Reasoning { text, finalized: false }) =
                    timeline.last_mut()
                {
                    text.push_str(&token);
                } else {
                    timeline.push(TimelineEntry::Reasoning { text: token, finalized: false });
                }
            }
            DaemonEvent::ReasoningSettled => {
                if let Some(TimelineEntry::Reasoning { finalized, .. }) = timeline.last_mut() {
                    *finalized = true;
                }
            }
            DaemonEvent::ToolStarted { call_id, name, args_preview } => {
                timeline.push(TimelineEntry::Tool {
                    call_id,
                    name,
                    args_preview,
                    status: ToolStatus::Running,
                    output_preview: String::new(),
                });
            }
            // A live view of a call still streaming: it adds no row, the call's own start does.
            DaemonEvent::ToolArgumentsPartial { .. } => {}
            DaemonEvent::ToolUpdated { call_id, status, output_preview } => {
                if let Some(TimelineEntry::Tool { status: s, output_preview: o, .. }) = timeline
                    .entries
                    .iter_mut()
                    .rev()
                    .find(|e| matches!(e, TimelineEntry::Tool { call_id: c, .. } if *c == call_id))
                {
                    *s = status;
                    *o = output_preview;
                }
            }
            DaemonEvent::ApprovalRequested { request_id, summary }
            | DaemonEvent::ToolApprovalRequested { request_id, summary, .. } => {
                timeline.push(TimelineEntry::Approval { request_id, summary, resolution: None });
            }
            DaemonEvent::ApprovalResolved { request_id, approved } => {
                if let Some(TimelineEntry::Approval { resolution, .. }) = timeline
                    .entries
                    .iter_mut()
                    .rev()
                    .find(|e| matches!(e, TimelineEntry::Approval { request_id: r, .. } if *r == request_id))
                {
                    *resolution = Some(approved);
                }
            }
            DaemonEvent::UserQuestionAsked { question_id, questions } => {
                let summary = questions.iter().map(|q| q.question.as_str()).collect::<Vec<_>>().join("\n");
                timeline.push(TimelineEntry::UserQuestion { question_id, summary, resolved: false, cancelled: false });
            }
            DaemonEvent::UserQuestionAnswered { question_id } => {
                if let Some(TimelineEntry::UserQuestion { resolved, .. }) = timeline.entries.iter_mut().rev().find(|e| matches!(e, TimelineEntry::UserQuestion { question_id: id, .. } if *id == question_id)) {
                    *resolved = true;
                }
            }
            DaemonEvent::PlanPresented { plan_id, plan } => {
                timeline.push(TimelineEntry::Plan { plan_id, plan, resolution: None, cancelled: false });
            }
            DaemonEvent::PlanResolved { plan_id, approved } => {
                if let Some(TimelineEntry::Plan { resolution, .. }) = timeline.entries.iter_mut().rev().find(|e| matches!(e, TimelineEntry::Plan { plan_id: id, .. } if *id == plan_id)) {
                    *resolution = Some(approved);
                }
            }
            DaemonEvent::RouteDecided { project, model, actor } => {
                timeline.push(TimelineEntry::Route { project, model, actor });
            }
            DaemonEvent::TaskUpdated { task_id, label, status } => {
                if let Some(TimelineEntry::Task { status: s, label: l, .. }) = timeline
                    .entries
                    .iter_mut()
                    .rev()
                    .find(|e| matches!(e, TimelineEntry::Task { task_id: t, .. } if *t == task_id))
                {
                    *s = status;
                    *l = label;
                } else {
                    timeline.push(TimelineEntry::Task { task_id, label, status });
                }
            }
            DaemonEvent::SubagentUpdated { subagent_id, label, status } => {
                if let Some(TimelineEntry::Subagent { status: s, label: l, .. }) = timeline
                    .entries
                    .iter_mut()
                    .rev()
                    .find(|e| matches!(e, TimelineEntry::Subagent { subagent_id: i, .. } if *i == subagent_id))
                {
                    *s = status;
                    *l = label;
                } else {
                    timeline.push(TimelineEntry::Subagent { subagent_id, label, status });
                }
            }
            DaemonEvent::CheckReported { check_id, label, passed, detail } => {
                timeline.push(TimelineEntry::Check { check_id, label, passed, detail });
            }
            DaemonEvent::System { text } => {
                timeline.push(TimelineEntry::System { text });
            }
            // Working-line state only; carries no transcript content.
            DaemonEvent::Status(_)
            | DaemonEvent::RateLimit { .. }
            | DaemonEvent::Usage { .. }
            | DaemonEvent::McpElicitationRequested { .. }
            | DaemonEvent::TaskGraph { .. }
            | DaemonEvent::CredentialRequested { .. } => {}
            DaemonEvent::CredentialResolved { .. } => {}
        }
    }
}
