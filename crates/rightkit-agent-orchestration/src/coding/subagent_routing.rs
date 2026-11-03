//! Per-turn and per-subagent model routing (Lane 13, Book A / P1-2).
//!
//! This module gives the host app the thing a process-wide `ANTHROPIC_BASE_URL`
//! structurally cannot: a subagent that routes to a different provider/model
//! than its parent, expressed as an explicit parameter rather than an
//! environment variable, plus a session-level "current model" that can move
//! between turns without ending the session.
//!
//! ## The five-way decomposition
//!
//! Every field describing a subagent call lives in exactly one of five
//! places, decided by *which one determines its value*:
//!
//! - [`Profile`] — how a worker thinks. **Ceiling values only** — the
//!   maximum this worker may ever be given, never what one call asked for.
//!   Nothing in this module ever writes a per-call value into a `Profile`;
//!   see [`resolve_invocation`] and the `profile_ceiling_boundary` tests.
//!
//!   Book A cites `repos/cli/deepseek-reasonix/internal/agent/profile_spec.go`
//!   and `profile_boundary_test.go` as prior art for this decomposition.
//!   That path does not exist in this workspace (`repos/cli/**` is absent) —
//!   this module is implemented from the book's prose description of the
//!   five-way split, not from reading that source.
//! - [`TaskSpec`] — what this call wants (payload, requested model/provider).
//! - [`CapabilityGrant`] — what it may touch (tools, read-only-ness).
//! - [`ContextRequest`] — what it starts from (whether project instruction
//!   files are included, inherited cache-prefix key).
//! - [`SchedulerPolicy`] — when it runs (priority, earliest-start).

use std::fmt;

// ── profile — ceiling values only ──────────────────────────────────────────

/// How a worker thinks, as **ceilings**, never as a per-call choice.
///
/// A `Profile` never stores "the model this call used" — only the maximum
/// this worker is ever allowed to reach for. Deliberately has no field named
/// `model_id`, `provider_id`, `payload`, or anything else a single call
/// decides; if you find yourself wanting to add one, it belongs in
/// [`TaskSpec`] instead.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    /// Highest escalation tier this worker may ever be routed to (1 = cheap
    /// leaf, 3 = architect/boss). A ceiling, not a request.
    pub max_tier_ceiling: u8,
    /// Hard USD ceiling for any single call this worker makes. `None` = no
    /// profile-level cap (falls through to account/global limits elsewhere).
    pub max_cost_usd_ceiling: Option<f64>,
    /// Maximum retries this worker's calls may ever accumulate.
    pub max_retries_ceiling: u32,
}

impl Profile {
    pub fn new(max_tier_ceiling: u8, max_retries_ceiling: u32) -> Self {
        Self {
            max_tier_ceiling,
            max_cost_usd_ceiling: None,
            max_retries_ceiling,
        }
    }

    pub fn with_cost_ceiling(mut self, max_cost_usd_ceiling: f64) -> Self {
        self.max_cost_usd_ceiling = Some(max_cost_usd_ceiling);
        self
    }
}

// ── TaskSpec — what this call wants ────────────────────────────────────────

/// What a single call wants. Distinct from [`Profile`]: a `TaskSpec` value
/// exists for exactly one call and is never persisted onto the worker that
/// serves it.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskSpec {
    pub task_id: String,
    pub payload: String,
    /// Explicit provider override for this call, independent of whatever
    /// provider its parent turn or parent agent is using. `None` = use the
    /// registry-assigned default for the routed agent.
    pub requested_provider_id: Option<String>,
    /// Explicit model override for this call, paired with
    /// `requested_provider_id`. `None` = use the registry default.
    pub requested_model_id: Option<String>,
    /// Escalation tier this call is asking to run at. Checked against
    /// `Profile::max_tier_ceiling` at resolution time, never written back.
    pub requested_tier: u8,
}

impl TaskSpec {
    pub fn new(task_id: impl Into<String>, payload: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            payload: payload.into(),
            requested_provider_id: None,
            requested_model_id: None,
            requested_tier: 1,
        }
    }

    pub fn with_model_override(
        mut self,
        provider_id: impl Into<String>,
        model_id: impl Into<String>,
    ) -> Self {
        self.requested_provider_id = Some(provider_id.into());
        self.requested_model_id = Some(model_id.into());
        self
    }

    pub fn with_tier(mut self, requested_tier: u8) -> Self {
        self.requested_tier = requested_tier;
        self
    }
}

// ── CapabilityGrant — what it may touch ────────────────────────────────────

/// What a call may touch. Governs tool access and whether the call is
/// treated as read-only for context-slimming purposes (13d).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityGrant {
    pub allowed_tools: Vec<String>,
    /// True when this call cannot mutate the workspace (e.g. an Explore-style
    /// search subagent). Drives [`derive_context_request`]'s decision to omit
    /// project instruction files.
    pub read_only: bool,
}

impl CapabilityGrant {
    pub fn read_only(allowed_tools: Vec<String>) -> Self {
        Self {
            allowed_tools,
            read_only: true,
        }
    }

    pub fn read_write(allowed_tools: Vec<String>) -> Self {
        Self {
            allowed_tools,
            read_only: false,
        }
    }
}

// ── ContextRequest — what it starts from ───────────────────────────────────

/// What a call starts from. Whether project instruction files (CLAUDE.md and
/// friends) are loaded is derived, not chosen ad hoc — see
/// [`derive_context_request`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextRequest {
    /// False for read-only subagents: they omit project instruction files
    /// entirely (13d). Prior art: `runAgent.ts:383-397`, "saves ~5-15
    /// Gtok/week across 34M+ Explore spawns" at Claude Code's scale.
    pub include_project_instructions: bool,
    /// Stable-prefix cache key inherited from the parent run, if any. Shared
    /// with [`crate::coding::conductor_cache_key`] — sibling workers under the same
    /// conductor run carry the same `Some(key)` so they cache-read against
    /// the parent's prefix slot.
    pub conductor_stable_cache_key: Option<String>,
}

/// Derive the context a call starts from, from what it may touch.
///
/// Read-only capability grants omit project instruction files by default
/// (13d) — but read-only does not imply "doesn't need project
/// conventions" (a read-only code-review subagent still needs CLAUDE.md
/// style rules). `include_project_instructions_override` is an explicit
/// per-call escape hatch: when the caller sets it, it wins outright over
/// the read-only-derived default in either direction. Mirrors
/// `runAgent.ts`'s `omitClaudeMd` gate, whose explicit
/// `override.userContext` from callers is "preserved untouched" rather
/// than overridden by the agent-type default.
pub fn derive_context_request(
    grant: &CapabilityGrant,
    conductor_stable_cache_key: Option<String>,
    include_project_instructions_override: Option<bool>,
) -> ContextRequest {
    ContextRequest {
        include_project_instructions: include_project_instructions_override
            .unwrap_or(!grant.read_only),
        conductor_stable_cache_key,
    }
}

// ── SchedulerPolicy — when it runs ─────────────────────────────────────────

/// When a call runs, relative to its siblings. Never determines what model
/// it uses or what it may touch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct SchedulerPolicy {
    /// Higher runs first among ready siblings.
    pub priority: i32,
    /// Concurrency-group slot; two calls in the same group never run
    /// simultaneously. `0` = unconstrained.
    pub concurrency_group: u32,
}

// ── Resolution: TaskSpec x Profile -> effective invocation ─────────────────

/// Why a resolution was clamped or rejected against the profile ceiling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileCeilingViolation {
    /// `requested_tier` exceeded `Profile::max_tier_ceiling`.
    TierExceedsCeiling { requested: u8, ceiling: u8 },
}

impl fmt::Display for ProfileCeilingViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProfileCeilingViolation::TierExceedsCeiling { requested, ceiling } => write!(
                f,
                "requested tier {requested} exceeds profile ceiling {ceiling}"
            ),
        }
    }
}

/// The model/provider a call actually runs with, after checking the
/// per-call request against the worker's profile ceiling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedInvocation {
    pub task_id: String,
    pub effective_tier: u8,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
}

/// Resolve a [`TaskSpec`] against a [`Profile`] ceiling.
///
/// Takes `profile` by shared reference and returns a new value — this
/// function has no way to write `task`'s per-call fields back into
/// `profile`, because `Profile` exposes no mutable setter for any of them
/// (there is nothing to set: it has no `model_id`/`provider_id`/`payload`
/// field at all). That absence is the enforcement mechanism the
/// `profile_ceiling_boundary` tests below assert.
pub fn resolve_invocation(
    task: &TaskSpec,
    profile: &Profile,
) -> Result<ResolvedInvocation, ProfileCeilingViolation> {
    if task.requested_tier > profile.max_tier_ceiling {
        return Err(ProfileCeilingViolation::TierExceedsCeiling {
            requested: task.requested_tier,
            ceiling: profile.max_tier_ceiling,
        });
    }
    Ok(ResolvedInvocation {
        task_id: task.task_id.clone(),
        effective_tier: task.requested_tier,
        provider_id: task.requested_provider_id.clone(),
        model_id: task.requested_model_id.clone(),
    })
}

// ── 13e — model switch is a cache event ────────────────────────────────────

/// Why a prefix change happened. Kept open-ended with `Other` since the
/// enumerable causes will grow as more callers adopt this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrefixChangeReason {
    /// The user (or an interactive `/model` style command) asked for a
    /// different model for the next turn.
    UserRequestedSwitch,
    /// A subagent was routed to a provider/model different from its parent
    /// (13b) — carries the parent's model id for attribution.
    SubagentRouting {
        parent_model_id: String,
    },
    /// An escalation path moved the call to a higher-tier model.
    EscalationRoute,
    Other(String),
}

/// Emitted whenever the active provider/model for a session or a spawned
/// call changes. A model switch mutates the prompt-cache prefix, so this
/// event exists to make that mutation attributable instead of silent.
///
/// This is a **local interface**: Lane 2 owns the actual telemetry sink and
/// event shape reconciliation happens there. Report this shape to the lead
/// for reconciliation rather than assuming it matches Lane 2's wire format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefixChangeEvent {
    pub session_id: String,
    /// Monotonic turn counter at the moment of the switch. For a subagent
    /// spawn this is the parent turn's index.
    pub turn_index: u64,
    pub previous_provider_id: Option<String>,
    pub previous_model_id: Option<String>,
    pub new_provider_id: String,
    pub new_model_id: String,
    pub reason: PrefixChangeReason,
}

/// Anything that wants to observe prefix-change events. Lane 2's telemetry
/// sink implements this; tests use an in-memory recorder.
pub trait PrefixChangeSink: Send + Sync {
    fn emit(&self, event: PrefixChangeEvent);
}

/// In-memory sink for tests and for callers that haven't wired Lane 2's
/// telemetry sink yet.
#[derive(Debug, Default)]
pub struct RecordingPrefixChangeSink {
    events: std::sync::Mutex<Vec<PrefixChangeEvent>>,
}

impl RecordingPrefixChangeSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> Vec<PrefixChangeEvent> {
        self.events.lock().expect("recording sink poisoned").clone()
    }
}

impl PrefixChangeSink for RecordingPrefixChangeSink {
    fn emit(&self, event: PrefixChangeEvent) {
        self.events
            .lock()
            .expect("recording sink poisoned")
            .push(event);
    }
}

// ── 13a — mutable per-turn model (session-level primitive) ────────────────

/// The current provider/model for a session, mutable **at turn boundaries**
/// only: a change made mid-turn takes effect starting the *next* turn, never
/// retroactively on the turn in flight.
///
/// This is the reusable primitive Lane 13 owns. Wiring it into the
/// interactive REPL loop's `turn_args` call site is a change to
/// `main_runtime_interactive_session.rs`, which is out of this lane's
/// allowlist (explicitly marked read-only reference) — that wiring is
/// reported to the lead rather than performed here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionTurnModel {
    session_id: String,
    turn_index: u64,
    provider_id: String,
    model_id: String,
    /// Set by `request_switch_for_next_turn`, applied by `begin_turn`.
    pending: Option<(String, String, PrefixChangeReason)>,
}

impl SessionTurnModel {
    pub fn new(
        session_id: impl Into<String>,
        provider_id: impl Into<String>,
        model_id: impl Into<String>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            turn_index: 0,
            provider_id: provider_id.into(),
            model_id: model_id.into(),
            pending: None,
        }
    }

    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    pub fn turn_index(&self) -> u64 {
        self.turn_index
    }

    /// Rebind model-boundary attribution to a newly selected durable
    /// session. Provider/model selection survives navigation, while turn
    /// numbering and a queued change belong only to the prior session.
    pub fn rebind_session(&mut self, session_id: impl Into<String>) {
        self.session_id = session_id.into();
        self.turn_index = 0;
        self.pending = None;
    }

    /// Queue a provider/model change for the *next* turn boundary. Does not
    /// affect the turn currently in flight. Returns nothing yet — the
    /// [`PrefixChangeEvent`] is produced by `begin_turn` once the switch
    /// actually takes effect, so the event's `turn_index` always reflects
    /// the turn the new model actually served.
    pub fn request_switch_for_next_turn(
        &mut self,
        provider_id: impl Into<String>,
        model_id: impl Into<String>,
        reason: PrefixChangeReason,
    ) {
        self.pending = Some((provider_id.into(), model_id.into(), reason));
    }

    /// Advance to the next turn boundary, applying any pending switch.
    /// Returns the emitted [`PrefixChangeEvent`] if a switch took effect on
    /// this boundary, or `None` if the model stayed the same. The session
    /// itself is untouched otherwise — this never ends or resets it.
    pub fn begin_turn(&mut self) -> Option<PrefixChangeEvent> {
        self.turn_index += 1;
        let (new_provider_id, new_model_id, reason) = self.pending.take()?;
        let event = PrefixChangeEvent {
            session_id: self.session_id.clone(),
            turn_index: self.turn_index,
            previous_provider_id: Some(self.provider_id.clone()),
            previous_model_id: Some(self.model_id.clone()),
            new_provider_id: new_provider_id.clone(),
            new_model_id: new_model_id.clone(),
            reason,
        };
        self.provider_id = new_provider_id;
        self.model_id = new_model_id;
        Some(event)
    }
}
