//! Trait implementations that express DaemonState's identity as a runtime —
//! sub-agent spawning, message dispatch, and team directory.
//!
//! Relocated verbatim from api.rs (2026-06-20) to shrink the daemon's monolith
//! and isolate the trait surface (SECURITY F11 auditability). Behaviour is
//! unchanged: the impl blocks moved as-is; api.rs still owns the
//! DaemonState struct, the router, and the free helpers these impls call.
//!
//! ponytail: the plan's optional decomposition of `spawn` into five named phase
//! helpers is deferred — its provided code dropped the timeout `interrupt_tx`
//! signal (a behaviour regression) and rebuilt the fork twice. Relocation alone
//! achieves the file-size/auditability goal with zero behaviour change; revisit
//! the decomposition as its own slice if spawn grows further.

use rightkit_agent_engine_core::conduit::ConduitReceive;
use rightkit_agent_engine_protocol::ProtocolEvent;
use rightkit_agent_engine_runtime_middleware::MiddlewarePhase;
use rightkit_agent_orchestration::coding::subagent_routing::{
    resolve_invocation, Profile, TaskSpec,
};

use crate::api::DaemonSubagentSupervisor;
use crate::api::{
    build_subagent_fork, cleanup_subagent_worktree, decorate_user_payload_with_system_reminder,
    dynamic_user_turn_reminder, lock_recover, native_spawn_route, now_iso, now_millis,
    prepare_subagent_workspace, publish_agent_roster, record_subagent_runtime_span,
    record_subagent_transcript_artifact, BudgetDecision, DaemonState, LiveSession, RunBudgetLedger,
    RunSubagentBudget, SubagentIdentity, SubagentSpan, SubagentSpanExecution, SubagentSpanSubject,
};
use crate::api_stores::update_team_store;

/// Sessions carrying the config-layer grant of a `POST /sessions` `trustWorkspace` (their own, or
/// inherited by a sub-agent from its parent). In memory only: never written to the trust store,
/// `PROCESS_TRUSTED` or the trust file, so a daemon restart or a dormant rehydrate drops it. Keyed
/// by session id because `LiveSession` has no field for it; every session construction rewrites
/// its entry, so an id that is reused never keeps an old grant.
static SESSION_CONFIG_GRANTS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashSet<String>>,
> = std::sync::LazyLock::new(Default::default);

/// Record (or clear) the config-layer grant of `session_id`.
pub(crate) fn set_session_config_grant(session_id: &str, granted: bool) {
    let mut grants = SESSION_CONFIG_GRANTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if granted {
        grants.insert(session_id.to_string());
    } else {
        grants.remove(session_id);
    }
}

/// Whether `session_id` carries the config-layer grant (read by sub-agent spawn).
pub(crate) fn session_config_granted(session_id: &str) -> bool {
    SESSION_CONFIG_GRANTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains(session_id)
}

/// Whether a per-session `trustWorkspace` grant may apply to `workspace_root`: the project must
/// bind (`Inside`, as `session_workspace_trust` requires) and its canonical root may be neither the
/// filesystem root nor the home directory, so one request cannot trust every project under it.
/// Both spellings are compared as `workspace_trust_key` normalises them (the key every trust
/// reader uses), the raw root and its canonical form.
pub(crate) fn trust_grant_allowed(workspace_root: &str) -> bool {
    let binding = crate::project_binding::ProjectBinding::bind(workspace_root);
    if !matches!(
        binding.evaluate(workspace_root),
        crate::project_binding::NavigationOutcome::Inside
    ) {
        return false;
    }
    let Some(canonical) = binding.canonical_root() else {
        return false;
    };
    if canonical.parent().is_none() {
        return false;
    }
    let mut home_keys: Vec<String> = Vec::new();
    if let Some(home) = rightkit_agent_config::os_home_dir() {
        home_keys.push(rightkit_agent_config::workspace_trust_key(
            &home.to_string_lossy(),
        ));
        if let Ok(canonical_home) = std::fs::canonicalize(&home) {
            home_keys.push(rightkit_agent_config::workspace_trust_key(
                &canonical_home.to_string_lossy(),
            ));
        }
    }
    let root_keys = [
        rightkit_agent_config::workspace_trust_key(binding.raw_root()),
        rightkit_agent_config::workspace_trust_key(&canonical.to_string_lossy()),
    ];
    !root_keys.iter().any(|key| home_keys.contains(key))
}

/// `load_workspace_config_with_env` for a session that may carry the config-layer grant: the
/// workspace layer loads whole when `granted`, otherwise exactly as the ungranted loader does.
pub(crate) fn load_workspace_config_granted(
    workspace: &std::path::Path,
    granted: bool,
) -> rightkit_agent_config::HostConfig {
    let loader = rightkit_agent_config::ConfigLoader::new();
    let base = loader.load_files_for_workspace_granted(workspace, granted);
    let env_vars: std::collections::HashMap<String, String> = std::env::vars().collect();
    loader.apply_env(base, &env_vars)
}

/// Anything that owns a sub-agent [`RunBudgetLedger`] (a live parent session
/// in production, a bare ledger in tests).
pub(crate) trait HoldsRunBudget: Send + Sync + 'static {
    fn run_budget(&self) -> &tokio::sync::Mutex<RunBudgetLedger>;
}

impl HoldsRunBudget for LiveSession {
    fn run_budget(&self) -> &tokio::sync::Mutex<RunBudgetLedger> {
        &self.run_budget
    }
}

impl HoldsRunBudget for tokio::sync::Mutex<RunBudgetLedger> {
    fn run_budget(&self) -> &tokio::sync::Mutex<RunBudgetLedger> {
        self
    }
}

enum BudgetSlotState {
    /// Admitted, child not registered yet: dropping rolls the admission back
    /// (both counters), as if `try_admit` never ran.
    Admitted,
    /// Child session registered: dropping releases the concurrent slot once
    /// (the cumulative count holds) and interrupts the child so a caller's
    /// shorter `tokio::time::timeout` cannot leave it running.
    Registered {
        child_id: String,
        interrupt: Option<tokio::sync::watch::Sender<bool>>,
    },
    Released,
}

/// RAII owner of one admitted sub-agent budget slot (audit 2026-11-05
/// orchestration H1). Every exit from the spawn future after `try_admit` —
/// an early `?`, the `!sent` branch, or the future being dropped mid-wait by a
/// caller's timeout — releases the slot through `Drop`. Explicit paths that
/// already rolled back under the ledger lock call [`Self::disarm`].
pub(crate) struct SubagentBudgetSlot<P: HoldsRunBudget> {
    owner: std::sync::Arc<P>,
    state: BudgetSlotState,
    /// Writes the ledger's cumulative count back to the persisted
    /// `run_budget` row after a drop-path rollback (PR 459 follow-up), so a
    /// restart does not restore a count that includes a spawn that never ran.
    persist: Option<RunBudgetPersist>,
}

/// Persists a parent's cumulative sub-agent count (`upsert_run_budget`).
pub(crate) type RunBudgetPersist = std::sync::Arc<
    dyn Fn(u32) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync,
>;

/// The production persister: `upsert_run_budget(parent_id, cumulative)`.
pub(crate) fn storage_run_budget_persist(
    storage: crate::api::SharedStorage,
    parent_id: String,
) -> RunBudgetPersist {
    std::sync::Arc::new(move |cumulative| {
        let storage = storage.clone();
        let parent_id = parent_id.clone();
        Box::pin(async move {
            if let Err(error) = storage
                .lock()
                .await
                .upsert_run_budget(&parent_id, cumulative)
            {
                tracing::warn!(
                    session_id = %parent_id,
                    %error,
                    "failed to persist sub-agent budget rollback"
                );
            }
        })
    })
}

impl<P: HoldsRunBudget> SubagentBudgetSlot<P> {
    /// Wrap a slot that `try_admit` just returned `Admit` for.
    pub(crate) fn admitted(owner: std::sync::Arc<P>) -> Self {
        Self {
            owner,
            state: BudgetSlotState::Admitted,
            persist: None,
        }
    }

    /// Persist the cumulative count whenever a drop or `complete` rolls an
    /// admission back.
    pub(crate) fn with_persistence(mut self, persist: RunBudgetPersist) -> Self {
        self.persist = Some(persist);
        self
    }

    /// The caller released the slot itself (under the ledger lock).
    pub(crate) fn disarm(&mut self) {
        self.state = BudgetSlotState::Released;
    }

    /// The child session now exists; from here a drop releases its slot
    /// exactly once and interrupts it.
    pub(crate) fn registered(
        &mut self,
        child_id: &str,
        interrupt: Option<tokio::sync::watch::Sender<bool>>,
    ) {
        if matches!(self.state, BudgetSlotState::Released) {
            return;
        }
        self.state = BudgetSlotState::Registered {
            child_id: child_id.to_string(),
            interrupt,
        };
    }

    /// Normal completion: release the slot (cumulative holds) without
    /// interrupting the child, which has already settled.
    pub(crate) async fn complete(&mut self) {
        // Take the state only once the lock is held, so a drop while waiting
        // for the lock still releases through `Drop`.
        let owner = self.owner.clone();
        let mut ledger = owner.run_budget().lock().await;
        let state = std::mem::replace(&mut self.state, BudgetSlotState::Released);
        if apply_slot_release(&mut ledger, state) {
            if let Some(persist) = self.persist.as_ref() {
                // Persist under the ledger lock, as the explicit rollback
                // paths do, so a racing admission cannot be overwritten.
                persist(ledger.cumulative_used()).await;
            }
        }
    }
}

/// Apply a release; returns `true` when it rolled back the cumulative count
/// (an admission that never registered a child), which must be persisted.
/// The slot's `mem::replace` to `Released` is the only guard against a double
/// release (the ledger's per-child `released` set was removed in wave 2).
fn apply_slot_release(ledger: &mut RunBudgetLedger, state: BudgetSlotState) -> bool {
    match state {
        BudgetSlotState::Admitted => {
            ledger.rollback_after_admit();
            SUBAGENT_SLOT_FREED.notify_waiters();
            true
        }
        BudgetSlotState::Registered { .. } => {
            ledger.mark(crate::api::SubagentLifecycleMark::Completed);
            SUBAGENT_SLOT_FREED.notify_waiters();
            false
        }
        BudgetSlotState::Released => false,
    }
}

impl<P: HoldsRunBudget> Drop for SubagentBudgetSlot<P> {
    fn drop(&mut self) {
        let state = std::mem::replace(&mut self.state, BudgetSlotState::Released);
        if matches!(state, BudgetSlotState::Released) {
            return;
        }
        // Interrupt first, independent of the ledger lock.
        let state = match state {
            BudgetSlotState::Registered {
                child_id,
                interrupt,
            } => {
                if let Some(tx) = interrupt.as_ref() {
                    let _ = tx.send(true);
                }
                BudgetSlotState::Registered {
                    child_id,
                    interrupt: None,
                }
            }
            other => other,
        };
        // A rollback that must be persisted cannot finish synchronously (the
        // storage write is async), so it takes the deferred path below.
        let persist = match state {
            BudgetSlotState::Admitted => self.persist.take(),
            _ => None,
        };
        if persist.is_none() {
            if let Ok(mut ledger) = self.owner.run_budget().try_lock() {
                apply_slot_release(&mut ledger, state);
                return;
            }
        }
        let owner = self.owner.clone();
        let release = async move {
            let mut ledger = owner.run_budget().lock().await;
            if apply_slot_release(&mut ledger, state) {
                if let Some(persist) = persist {
                    persist(ledger.cumulative_used()).await;
                }
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(release);
            }
            Err(_) => match tokio::runtime::Builder::new_current_thread().build() {
                Ok(runtime) => runtime.block_on(release),
                Err(error) => {
                    tracing::warn!(%error, "sub-agent budget slot release could not run");
                }
            },
        }
    }
}

/// How long a sub-agent gets to stop after an interrupt before its budget slot
/// is released regardless (round 2 M5). Bounded so a child that ignores the
/// interrupt cannot hold the parent's slot forever.
const SUBAGENT_STOP_GRACE: std::time::Duration = std::time::Duration::from_secs(30);

/// Interrupt a child and wait until its engine turn is no longer active, or
/// `grace` passes. Returns whether the turn ended in time.
async fn interrupt_and_await_idle(
    interrupt: Option<&tokio::sync::watch::Sender<bool>>,
    turn: &rightkit_agent_daemon::LiveTurnHandle,
    grace: std::time::Duration,
) -> bool {
    if let Some(tx) = interrupt {
        let _ = tx.send(true);
    }
    let deadline = tokio::time::Instant::now() + grace;
    while turn.is_active() {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    true
}

static SUBAGENT_ID_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Eight random hex characters from a fresh v7 UUID (its last, random, group).
pub(crate) fn unique_id_tail() -> String {
    let id = rightkit_agent_engine_protocol::uuid_v7();
    id[id.len().saturating_sub(8)..].to_string()
}

/// An artifact id: sortable by time, unique across processes (the row is a global primary key
/// written with INSERT OR REPLACE).
pub(crate) fn next_artifact_id() -> String {
    format!("art-{}-{}", now_millis(), unique_id_tail())
}

pub(crate) fn next_subagent_id() -> String {
    let sequence = SUBAGENT_ID_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // The random tail keeps two processes on one account, and this generator and the tool's own
    // (`child_session_id_for_visible_spawn`), from ever minting the same id: the session row is
    // written with INSERT OR REPLACE, so a collision would overwrite another agent's row.
    format!("sub-{}-{sequence}-{}", now_millis(), unique_id_tail())
}

/// Maximum sub-agent nesting depth. A sub-agent spawned directly by a root
/// session counts as one live ancestor; spawning is refused once the parent's
/// chain already reaches this many live ancestors — a backstop against runaway
/// recursive fan-out (a sub-agent that re-spawns itself without bound). Mirrors
/// Claude Code's 5-level background cap.
/// ponytail: const, not a setting — promote to DaemonSettings only if tuning is needed.
pub(crate) const MAX_SUBAGENT_DEPTH: usize = 5;

/// Count live ancestors of `session_id` inclusive by walking parent links.
/// Stops at `MAX_SUBAGENT_DEPTH + 1` so a corrupt parent cycle can't spin forever.
pub(crate) fn subagent_chain_depth(state: &DaemonState, session_id: &str) -> usize {
    let mut depth = 0;
    let mut current = Some(session_id.to_string());
    while let Some(id) = current {
        let Some(session) = state.get_session(&id) else {
            break;
        };
        depth += 1;
        if depth > MAX_SUBAGENT_DEPTH {
            break;
        }
        current = session.parent_session_id.clone();
    }
    depth
}

/// Depth backstop: refuse to spawn deeper than [`MAX_SUBAGENT_DEPTH`] so a
/// sub-agent that re-spawns itself cannot build an unbounded chain. The
/// per-agent wall-clock ceiling bounds runtime; this bounds nesting.
fn check_subagent_depth(state: &DaemonState, parent_id: &str) -> Result<(), String> {
    if subagent_chain_depth(state, parent_id) >= MAX_SUBAGENT_DEPTH {
        return Err(format!(
            "sub-agent nesting limit reached ({MAX_SUBAGENT_DEPTH} levels) — refusing to spawn deeper from {parent_id}"
        ));
    }
    Ok(())
}

/// Per-role spawn limit: a parent whose role has `spawnable-roles` in
/// `[orchestration.agents.<role>]` may spawn only those roles. A root session (no role) and a role
/// with no list are unrestricted here.
fn check_spawnable_roles(parent: &LiveSession, role: &str) -> Result<(), String> {
    let Some(parent_role) = parent.role.as_deref() else {
        return Ok(());
    };
    let config = load_workspace_config_granted(
        std::path::Path::new(parent.workspace_root.trim()),
        session_config_granted(&parent.id),
    );
    let Some(allowed) = config
        .orchestration
        .agents
        .get(parent_role)
        .map(|agent| &agent.spawnable_roles)
        .filter(|roles| !roles.is_empty())
    else {
        return Ok(());
    };
    if allowed
        .iter()
        .any(|allowed_role| allowed_role.trim() == role)
    {
        return Ok(());
    }
    Err(format!(
        "role '{parent_role}' may spawn only {} (spawnable-roles); '{role}' is not one of them",
        allowed.join(", ")
    ))
}

/// R13 (CR-42/43) tier ceiling: a bounded parent (one whose role is
/// registered) may not spawn a child registered at a higher escalation tier,
/// and an unregistered child role fails closed. A root or custom-role parent
/// is not tier-restricted.
fn check_subagent_tier(
    state: &DaemonState,
    parent: &LiveSession,
    role: &str,
    task_id: &str,
    task_text: &str,
    options: &rightkit_agent_coding_tools::SubagentSpawnOptions,
) -> Result<(), String> {
    let Some(parent_role) = parent.role.clone() else {
        return Ok(());
    };
    let Some(parent_descriptor) = state.agent_registry.get(&parent_role) else {
        return Ok(());
    };
    let Some(child_descriptor) = state.agent_registry.get(role) else {
        return Err(format!(
            "sub-agent role '{role}' is not registered, so its escalation tier \
             cannot be checked against spawning agent '{parent_role}'s ceiling \
             — refusing to spawn"
        ));
    };
    let profile = Profile::new(parent_descriptor.escalation_tier, 3);
    let task = native_spawn_task_spec(
        task_id.to_string(),
        task_text.to_string(),
        options,
        child_descriptor.escalation_tier,
    );
    if let Err(violation) = resolve_invocation(&task, &profile) {
        return Err(format!(
            "sub-agent role '{role}' (tier {}) exceeds spawning agent \
             '{parent_role}'s tier ceiling: {violation}",
            child_descriptor.escalation_tier
        ));
    }
    Ok(())
}

/// Serialises sub-agent admissions daemon-wide so the run-tree concurrency cap
/// is exact (round 2 L2). Held only for the admission itself, never while a
/// child runs.
static SUBAGENT_ADMISSION_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Woken (under the ledger lock) whenever a sub-agent budget slot is released
/// or an admission is rolled back, so queued over-cap spawns re-run admission.
static SUBAGENT_SLOT_FREED: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Longest a spawn waits for a concurrent slot before the refusal returns.
/// Only a spawn with `SubagentSpawnOptions::queue_when_full` (the spawn tool's
/// background path, which runs in a detached task and so parks no turn) waits;
/// a blocking spawn is refused at once (zero wait) because its caller's turn
/// would otherwise sit parked behind the cap (issue 481).
const SUBAGENT_BACKGROUND_QUEUE_MAX_WAIT: std::time::Duration =
    std::time::Duration::from_secs(30 * 60);

/// Resolves when the parent session is interrupted (its turn cancelled),
/// including one already interrupted when the wait starts (the same reading
/// `DaemonState::session_cancelling` uses). Pends forever for a parent without
/// an interrupt channel.
async fn wait_parent_interrupt(parent: &LiveSession) {
    let Some(tx) = parent.interrupt_tx.as_ref() else {
        return std::future::pending().await;
    };
    let mut rx = tx.subscribe();
    if *rx.borrow_and_update() {
        return;
    }
    loop {
        if rx.changed().await.is_err() {
            return std::future::pending().await;
        }
        let interrupted = *rx.borrow();
        if interrupted {
            return;
        }
    }
}

/// Lane C3 (task 2): per-run sub-agent budget gate. Consults the parent's
/// `RunBudgetLedger` after the depth backstop and refuses concurrent
/// overloads and cumulative exhaustion. Admission advances both counters;
/// from `Admit` on the returned slot owns them, so every exit (an early `?`,
/// a failed send, or the spawn future being dropped by a caller's shorter
/// timeout) releases it through `Drop`, persisting any rollback.
async fn admit_subagent_budget(
    state: &DaemonState,
    parent: &std::sync::Arc<LiveSession>,
    parent_id: &str,
    role: &str,
    queue_when_full: bool,
) -> Result<SubagentBudgetSlot<LiveSession>, String> {
    // A background spawn (`queue_when_full`) over the per-session concurrent
    // cap waits for a free slot (up to `SUBAGENT_BACKGROUND_QUEUE_MAX_WAIT`)
    // instead of being refused; each wake re-runs the whole admission.
    // Blocking spawns, and the cumulative and run-tree caps, fail at once.
    let wait = if queue_when_full {
        SUBAGENT_BACKGROUND_QUEUE_MAX_WAIT
    } else {
        std::time::Duration::ZERO
    };
    let deadline = tokio::time::Instant::now() + wait;
    let mut queued_logged = false;
    loop {
        if let Some(slot) =
            admit_subagent_budget_once(state, parent, parent_id, role, deadline, &mut queued_logged)
                .await?
        {
            return Ok(slot);
        }
    }
}

/// One admission attempt. `Ok(None)` means the concurrent cap was full and a
/// slot has since been released (or the wait was spuriously woken): retry.
async fn admit_subagent_budget_once(
    state: &DaemonState,
    parent: &std::sync::Arc<LiveSession>,
    parent_id: &str,
    role: &str,
    deadline: tokio::time::Instant,
    queued_logged: &mut bool,
) -> Result<Option<SubagentBudgetSlot<LiveSession>>, String> {
    let effective_budget = effective_run_budget(state, parent_id);
    // Each session keeps its own ledger, so without this a spawn-capable
    // child (e.g. `auditor`) could fan out geometrically: every level gets a
    // fresh `concurrent_max`. Bound the whole run tree's concurrency too
    // (audit 2026-11-05 orchestration M6). Checked before the parent's ledger
    // is locked, so no two ledger locks are ever held at once.
    //
    // Round 2 L2: the tree sum and the parent's admission below are one
    // check-then-act, so concurrent spawns from sibling parents could each see
    // `cap - 1` and all admit. Admissions are serialised through one gate held
    // until `try_admit` has counted this child. Only admission takes the gate,
    // and it is taken before any ledger or storage lock, so the existing
    // ledger-then-storage order is unchanged and no cycle is possible.
    let _admission_gate = SUBAGENT_ADMISSION_GATE.lock().await;
    check_run_tree_capacity(state, parent_id, role).await?;
    let persisted_cumulative = if let Some(storage) = state.storage.as_ref() {
        storage
            .lock()
            .await
            .restore_run_budget(parent_id)
            .map_err(|error| {
                format!("failed to restore sub-agent budget for {parent_id}: {error}")
            })?
    } else {
        None
    };
    let mut ledger = parent.run_budget.lock().await;
    ledger.configure(effective_budget, persisted_cumulative);
    match ledger.try_admit() {
        BudgetDecision::Admit => {}
        BudgetDecision::RefuseConcurrent {
            concurrent_limit,
            concurrent_in_flight,
            cumulative_limit,
            cumulative_used,
        } => {
            let refusal = format!(
                "sub-agent concurrent budget exhausted (limit {concurrent_limit}, in-flight {concurrent_in_flight}, cumulative {cumulative_used}/{cumulative_limit}) — refusing to spawn {role} from {parent_id}"
            );
            if tokio::time::Instant::now() >= deadline {
                return Err(refusal);
            }
            // Queue instead of refusing: register for the next slot release
            // while the ledger lock is still held (every release notifies under
            // that lock, so no wakeup can be lost), then free the gate and the
            // ledger and wait. The caller re-runs the full admission.
            let freed = SUBAGENT_SLOT_FREED.notified();
            tokio::pin!(freed);
            let _ = freed.as_mut().enable();
            drop(ledger);
            drop(_admission_gate);
            if !*queued_logged {
                *queued_logged = true;
                tracing::info!(
                    parent_id,
                    role,
                    concurrent_limit,
                    concurrent_in_flight,
                    "sub-agent spawn queued until a concurrent slot frees"
                );
            }
            tokio::select! {
                _ = &mut freed => {}
                _ = tokio::time::sleep_until(deadline) => return Err(refusal),
                _ = wait_parent_interrupt(parent) => {
                    // Deleting, archiving, killing or rebinding a session removes it from
                    // the live map and also sets its interrupt flag, so the flag is the one
                    // wake-up for both. Only the map tells the two apart.
                    let ended = state
                        .get_session(parent_id)
                        .is_none_or(|live| !std::sync::Arc::ptr_eq(&live, parent));
                    return Err(if ended {
                        "sub-agent spawn cancelled while queued: parent session ended".to_string()
                    } else {
                        format!(
                            "sub-agent spawn cancelled while queued: parent {parent_id} was interrupted (role {role})"
                        )
                    })
                }
            }
            return Ok(None);
        }
        BudgetDecision::RefuseCumulative {
            concurrent_limit,
            concurrent_in_flight,
            cumulative_limit,
            cumulative_used,
        } => {
            return Err(format!(
                "sub-agent cumulative budget exhausted (used {cumulative_used}/{cumulative_limit}, running {concurrent_in_flight}/{concurrent_limit}) — no more sub-agents can start in this session; refusing to spawn {role} from {parent_id}"
            ));
        }
    }
    let mut slot = SubagentBudgetSlot::admitted(parent.clone());
    if let Some(storage) = state.storage.as_ref() {
        slot = slot.with_persistence(storage_run_budget_persist(
            storage.clone(),
            parent_id.to_string(),
        ));
        if let Err(error) = storage
            .lock()
            .await
            .upsert_run_budget(parent_id, ledger.cumulative_used())
        {
            ledger.rollback_after_admit();
            slot.disarm();
            SUBAGENT_SLOT_FREED.notify_waiters();
            return Err(format!(
                "failed to persist sub-agent budget admission for {parent_id}: {error}"
            ));
        }
    }
    Ok(Some(slot))
}

/// The root session of `session_id`'s sub-agent tree, found through
/// `parent_of`. Bounded like [`subagent_chain_depth`] so a corrupt parent
/// cycle cannot spin.
fn subagent_tree_root_with(session_id: &str, parent_of: impl Fn(&str) -> Option<String>) -> String {
    let mut current = session_id.to_string();
    for _ in 0..=MAX_SUBAGENT_DEPTH {
        match parent_of(&current) {
            Some(parent) => current = parent,
            None => break,
        }
    }
    current
}

/// Concurrency cap for a whole run tree: the root's per-session cap times the
/// nesting limit. Linear in depth, where per-session ledgers alone allow
/// `concurrent_max ^ depth`.
fn run_tree_concurrent_cap(root_concurrent_max: u32) -> u32 {
    root_concurrent_max.saturating_mul(MAX_SUBAGENT_DEPTH as u32)
}

fn run_tree_capacity_verdict(
    tree_in_flight: u32,
    cap: u32,
    root_id: &str,
    role: &str,
    parent_id: &str,
) -> Result<(), String> {
    if tree_in_flight >= cap {
        return Err(format!(
            "sub-agent run-tree concurrent budget exhausted (limit {cap}, in-flight {tree_in_flight} under root {root_id}) — refusing to spawn {role} from {parent_id}"
        ));
    }
    Ok(())
}

async fn check_run_tree_capacity(
    state: &DaemonState,
    parent_id: &str,
    role: &str,
) -> Result<(), String> {
    let parent_of = |id: &str| {
        state
            .get_session(id)
            .and_then(|session| session.parent_session_id.clone())
    };
    let root_id = subagent_tree_root_with(parent_id, parent_of);
    let sessions: Vec<std::sync::Arc<LiveSession>> = lock_recover(&state.sessions, "sessions")
        .values()
        .cloned()
        .collect();
    let mut tree_in_flight: u32 = 0;
    for session in sessions {
        if subagent_tree_root_with(&session.id, parent_of) != root_id {
            continue;
        }
        let in_flight = session.run_budget.lock().await.concurrent_in_flight();
        tree_in_flight = tree_in_flight.saturating_add(in_flight);
    }
    let cap = run_tree_concurrent_cap(effective_run_budget(state, &root_id).concurrent_max);
    run_tree_capacity_verdict(tree_in_flight, cap, &root_id, role, parent_id)
}

/// A short, human-readable title for a sub-agent's task.
///
/// Handoff briefs are markdown, so taking the first 60 raw characters produced
/// agent cards reading `## Handoff Brief\n\n### Goal\nRun the wh` — heading
/// syntax, a mid-word cut, and none of the actual task. Skip heading and label
/// lines, take the first real sentence, and cut on a word boundary.
fn handoff_task_title(task_text: &str) -> String {
    const MAX: usize = 60;
    let first_line = task_text
        .lines()
        .map(str::trim)
        .find(|line| {
            !line.is_empty()
                && !line.starts_with('#')
                && !line.starts_with("**")
                && !line.starts_with('-')
                && !line.starts_with('>')
        })
        .unwrap_or_else(|| task_text.trim());

    if first_line.chars().count() <= MAX {
        return first_line.to_string();
    }
    // Cut on the last word boundary that fits, so titles never end mid-word.
    let head: String = first_line.chars().take(MAX).collect();
    let trimmed = match head.rsplit_once(' ') {
        Some((left, _)) if left.chars().count() >= MAX / 2 => left,
        _ => head.as_str(),
    };
    format!("{}…", trimmed.trim_end_matches(['.', ',', ';', ':']))
}

fn ensure_parent_agent_roster(
    parent: &LiveSession,
    sub_session_id: &str,
    role: &str,
    model: &str,
    provider: Option<&str>,
) {
    lock_recover(&parent.agent_roster, "session.agent_roster").ensure(
        sub_session_id,
        role,
        model,
        provider,
    );
}

fn record_parent_agent_usage(
    parent: &LiveSession,
    sub_session_id: &str,
    prompt_tokens: i32,
    completion_tokens: i32,
    cache_read_tokens: i32,
    cost_usd: Option<f64>,
) {
    lock_recover(&parent.agent_roster, "session.agent_roster").record_usage(
        sub_session_id,
        prompt_tokens.max(0) as u64,
        completion_tokens.max(0) as u64,
        cache_read_tokens.max(0) as u64,
        cost_usd,
    );
}

fn record_parent_agent_tool(parent: &LiveSession, sub_session_id: &str, tool_name: &str) {
    lock_recover(&parent.agent_roster, "session.agent_roster")
        .record_tool(sub_session_id, tool_name);
}

fn set_parent_agent_status(parent: &LiveSession, sub_session_id: &str, status: &str) {
    lock_recover(&parent.agent_roster, "session.agent_roster").set_status(sub_session_id, status);
}

/// Paths a sub-agent's file-writing tool call names (`write_file`, `edit_file`, `multi_edit`
/// by `path`; `apply_patch` by the `*** Update File:` / `*** Add File:` / `*** Delete File:`
/// headers of its `patch`), for the typed sub-agent result.
fn collect_subagent_touched_paths(
    tool_name: &str,
    arguments_json: &str,
    out: &mut std::collections::BTreeSet<String>,
) {
    if !matches!(
        tool_name,
        "write_file" | "edit_file" | "multi_edit" | "apply_patch"
    ) {
        return;
    }
    let Ok(args) = serde_json::from_str::<serde_json::Value>(arguments_json) else {
        return;
    };
    if tool_name == "apply_patch" {
        let Some(patch) = args["patch"].as_str().or_else(|| args["input"].as_str()) else {
            return;
        };
        for line in patch.lines() {
            for header in ["*** Update File: ", "*** Add File: ", "*** Delete File: "] {
                if let Some(path) = line.strip_prefix(header) {
                    let path = path.trim();
                    if !path.is_empty() {
                        out.insert(path.to_string());
                    }
                }
            }
        }
    } else if let Some(path) = args["path"].as_str() {
        if !path.trim().is_empty() {
            out.insert(path.to_string());
        }
    }
}

/// Whether a child may write to the workspace: false when its own toolset (role configuration
/// or role pack) is a read-only profile, or its allowed tools hold no mutating tool.
fn subagent_may_write(
    role: &str,
    options: &rightkit_agent_coding_tools::SubagentSpawnOptions,
    config: &rightkit_agent_config::HostConfig,
    pack_toolset: Option<String>,
) -> bool {
    let configured = config
        .orchestration
        .agents
        .get(role)
        .map(|agent| agent.toolset.trim().to_string())
        .filter(|toolset| !toolset.is_empty());
    let from_pack = pack_toolset.map(|toolset| {
        crate::api_toolsets::role_pack_toolset_profile(&toolset)
            .map(str::to_string)
            .unwrap_or(toolset)
    });
    for name in [configured, from_pack].into_iter().flatten() {
        if let Ok(profile) = crate::api_toolsets::resolve_toolset_profile_with_config(&name, config)
        {
            if profile.read_only {
                return false;
            }
        }
    }
    if !options.allowed_tools.is_empty() {
        let mutating = crate::api_toolsets::builtin_toolset_profiles()
            .into_iter()
            .find(|profile| profile.id == "reader")
            .map(|profile| profile.disallowed_tools);
        if let Some(mutating) = mutating {
            if !options
                .allowed_tools
                .iter()
                .any(|tool| mutating.contains(tool))
            {
                return false;
            }
        }
    }
    true
}

/// Leads the result of a sub-agent that ran out of turns before it finished.
pub(crate) const SUBAGENT_TURN_LIMIT_NOTE: &str = "[This sub-agent ran out of turns before it finished. What follows is its own summary of the work so far, not a completed result.]";

/// Fold one child event into its completion state. An assistant message only
/// records the latest content (it is interim: steer, tool turns and summaries
/// also emit them); only the typed terminal `SessionTurnFinished` status
/// settles the child. Returns `None` while the child is still running.
fn settle_subagent_event(
    event: &ProtocolEvent,
    last_content: &mut String,
) -> Option<Result<String, String>> {
    match event {
        ProtocolEvent::AssistantMessageCompleted {
            message: rightkit_agent_engine_protocol::TranscriptMessage::Assistant { content, .. },
            ..
        } => {
            *last_content = content.clone();
            None
        }
        // ORCH-04: a child that spent its turn budget ends `completed` with a summary. Mark the
        // text so the parent reads it as cut off, never as a finished result.
        ProtocolEvent::OrchestrationObservationRecorded { observation, .. }
            if observation.kind == rightkit_agent_engine_core::TURN_LIMIT_OBSERVATION_KIND =>
        {
            *last_content = format!("{SUBAGENT_TURN_LIMIT_NOTE}\n\n{last_content}");
            None
        }
        ProtocolEvent::SessionTurnFinished { status, .. } => Some(match status.as_str() {
            "completed" => Ok(std::mem::take(last_content)),
            "cancelled" => Err("sub-agent interrupted".to_string()),
            other => Err(format!("sub-agent turn {other}")),
        }),
        _ => None,
    }
}

/// Text published when a session's engine task dies by panic. The prefix is
/// the typed marker `subagent_error_kind` classifies as a failed child (never
/// a resource-limit cutoff).
pub(crate) fn engine_task_panic_failure(
    session_id: &str,
    seq: i64,
    join_error: &tokio::task::JoinError,
) -> Option<ProtocolEvent> {
    if !join_error.is_panic() {
        return None;
    }
    Some(ProtocolEvent::RunFailed {
        session_id: session_id.to_string(),
        seq,
        created_at: now_iso(),
        error: format!(
            "{}: {join_error}",
            crate::orchestrator::SUBAGENT_PANIC_PREFIX
        ),
        kind: "other".to_string(),
        hint: String::new(),
    })
}

/// Persist session status `failed` (so `terminal_status` and restart-safe
/// readers see the child as failed, never pending or completed).
pub(crate) async fn persist_session_failed(storage: &crate::api::SharedStorage, session_id: &str) {
    if let Err(error) = storage
        .lock()
        .await
        .update_session_status(session_id, "failed", &now_iso())
    {
        tracing::error!(session_id = %session_id, error = %error, "failed to persist failed status after engine task panic");
    }
}

/// Like [`supervise_session_engine_task`], and when `storage` is supplied also
/// persists session status `failed` on panic, before the typed `RunFailed` is
/// published, so no observer can see the failure event with a stale status.
pub(crate) fn supervise_session_engine_task_persisting(
    handle: tokio::task::JoinHandle<()>,
    conduit: std::sync::Arc<rightkit_agent_engine_core::conduit::Conduit>,
    session_id: String,
    storage: Option<crate::api::SharedStorage>,
) {
    tokio::spawn(async move {
        if let Err(error) = handle.await {
            let seq = conduit.next_seq();
            if let Some(event) = engine_task_panic_failure(&session_id, seq, &error) {
                tracing::error!(session_id = %session_id, error = %error, "session engine task panicked");
                if let Some(storage) = storage.as_ref() {
                    persist_session_failed(storage, &session_id).await;
                }
                conduit.publish(event);
            }
        }
    });
}

/// Run the configured `SubagentStart` hooks for a child session that is about
/// to receive its task (tools rule 58). Returns the hook errors, which are
/// logged and never block the child.
pub(crate) async fn run_subagent_start_hooks(
    bus: &rightkit_hooks::agent_hooks::HookBus,
    sub_id: &str,
    role: &str,
) -> Vec<String> {
    let report = bus
        .run_event_hooks(
            "SubagentStart",
            sub_id,
            Some(role),
            None,
            &[("RIGHTKIT_HOOK_ROLE", role.to_string())],
        )
        .await;
    for error in &report.errors {
        tracing::warn!(session_id = %sub_id, %error, "SubagentStart hook error (non-blocking)");
    }
    report.errors
}

fn subagent_wall_clock_ceiling(state: &DaemonState) -> std::time::Duration {
    std::time::Duration::from_secs(
        (lock_recover(&state.settings, "settings").max_wall_clock_minutes as u64).max(1) * 60,
    )
}

/// Compute the effective run sub-agent budget for a parent session.
///
/// Source of truth:
/// - `max_workers` from `DaemonSettingsStore` (config.toml `budget.max_workers`)
/// - The team preset's `max_workers` if the goal mentions the team name.
///
/// Returns a [`RunSubagentBudget`] whose `concurrent_max` is `effective_max_workers`
/// and whose `cumulative_max` is 4× that value (the conservative lifetime
/// multiplier the bin has used since the per-run budget was introduced).
pub(crate) fn effective_run_budget(state: &DaemonState, parent_id: &str) -> RunSubagentBudget {
    let configured = lock_recover(&state.settings, "settings").max_workers as usize;
    // Look up the parent session's goal text to see if a team preset applies.
    let team_preset = state.get_session(parent_id).and_then(|parent| {
        let goal = lock_recover(&parent.goal, "session.goal").clone();
        goal.and_then(|goal_text| {
            // Also picks up teams another process saved since this one read the file.
            crate::api_stores::refresh_team_store(&state.team_store_path, &state.live_teams);
            let teams = lock_recover(&state.live_teams, "live_teams").clone();
            let snapshot: Vec<rightkit_agent_coding_tools::TeamDefinition> = teams
                .values()
                .filter_map(|team| team.max_workers.map(|_| team.clone()))
                .collect();
            crate::orchestrator::find_team_preset_for_goal(&goal_text, &snapshot)
        })
    });
    let concurrent = effective_max_workers(configured, team_preset.as_ref()).max(1) as u32;
    run_budget_from_concurrent(concurrent)
}

/// The run budget for a concurrent cap: cumulative is 4x concurrent.
fn run_budget_from_concurrent(concurrent: u32) -> RunSubagentBudget {
    let cumulative = concurrent.saturating_mul(4).max(concurrent);
    RunSubagentBudget::default()
        .with_concurrent(concurrent)
        .with_cumulative(cumulative)
}

/// Per-LLM-response ceiling on worker-spawning tool calls for the engine
/// (`SessionEngineConfig::max_worker_tool_calls`; 0 = unlimited): the run's
/// cumulative sub-agent cap from the same budget the daemon ledger uses, not
/// the concurrent cap. The ledger (`admit_subagent_budget`) is the one
/// concurrency authority, so over-cap background spawns reach it and queue
/// instead of being dropped by the engine (issue 481).
pub(crate) fn engine_worker_call_cap(configured_max_workers: usize) -> usize {
    let concurrent = effective_max_workers(configured_max_workers, None).max(1) as u32;
    match run_budget_from_concurrent(concurrent).cumulative_max {
        u32::MAX => 0,
        cumulative => cumulative as usize,
    }
}

/// Expose the underlying `effective_max_workers` so the budget builder can
/// reuse the same `configured vs team.min(configured)` rule the conductor
/// task graph already uses.
fn effective_max_workers(
    configured: usize,
    team: Option<&rightkit_agent_coding_tools::TeamDefinition>,
) -> usize {
    match (
        configured,
        team.and_then(|t| t.max_workers).map(|n| n as usize),
    ) {
        (0, Some(team_max)) => team_max,
        (configured, Some(team_max)) => configured.min(team_max).max(1),
        _ => configured,
    }
}

/// Provider for a split-mode model with no explicit provider-id: the one
/// `[provider-routes.*]` whose declared model matches, else the provider
/// derived from the model slug. When several routes declare the model, each
/// is a different account or key that would pay: the parent's own route wins
/// if it is one of them, and otherwise the spawn is refused with the routes
/// named, never settled by picking the first.
fn split_provider_for_model(
    config: &rightkit_agent_config::HostConfig,
    model: &str,
    parent_provider: Option<&str>,
) -> Result<String, String> {
    let mut routes: Vec<&String> = config
        .provider_routes
        .iter()
        .filter(|(_, route)| route.model.trim().eq_ignore_ascii_case(model.trim()))
        .map(|(key, _)| key)
        .collect();
    routes.sort();
    match routes.as_slice() {
        [] => Ok(crate::model_policy::provider_id_for_model_slug(model).to_string()),
        [only] => Ok((*only).clone()),
        several => {
            if let Some(parent) = parent_provider
                .map(str::trim)
                .filter(|parent| several.iter().any(|key| key.as_str() == *parent))
            {
                return Ok(parent.to_string());
            }
            let names = several
                .iter()
                .map(|key| format!("'{key}'"))
                .collect::<Vec<_>>()
                .join(", ");
            Err(format!(
                "model '{}' is declared by more than one provider route ({names}), and each is a different \
                 account that would pay for this sub-agent; name the provider (provider-id, or \
                 `<provider>/<model>`) — refusing to spawn",
                model.trim()
            ))
        }
    }
}

fn native_spawn_task_spec(
    task_id: String,
    payload: String,
    options: &rightkit_agent_coding_tools::SubagentSpawnOptions,
    requested_tier: u8,
) -> TaskSpec {
    let task = TaskSpec::new(task_id, payload).with_tier(requested_tier);
    match native_spawn_route(options) {
        Some((provider_id, model_id)) => task.with_model_override(provider_id, model_id),
        None => task,
    }
}

impl DaemonState {
    /// Root-controller handoffs fence new daemon intake until ownership has
    /// moved to a destination runtime.
    pub(crate) fn handoff_intake_is_fenced(&self, session_id: &str) -> bool {
        let _controller = &self.handoff_controller;
        rightkit_agent_coding_tools::handoff::HandoffController::is_in_flight(session_id)
    }
}

/// Out-slot for the typed `RunFailed.kind` a sub-agent failed with. The wire
/// kind travels beside the failure text so outcome classification uses the
/// type; it stays `None` for failures that carried no kind (spawn refusals,
/// wall-clock ceiling, conduit closure), which classify by their text.
pub(crate) type SubagentFailureKindSlot = std::sync::Arc<std::sync::Mutex<Option<String>>>;

impl rightkit_agent_coding_tools::SubagentSpawner for DaemonState {
    fn spawn(
        &self,
        role: &str,
        task: &str,
        parent_session_id: &str,
        options: rightkit_agent_coding_tools::SubagentSpawnOptions,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> {
        self.spawn_with_failure_kind(
            role,
            task,
            parent_session_id,
            options,
            SubagentFailureKindSlot::default(),
        )
    }

    fn concurrent_cap(&self, parent_session_id: &str) -> Option<usize> {
        match effective_run_budget(self, parent_session_id).concurrent_max {
            u32::MAX => None,
            cap => Some(cap as usize),
        }
    }
}

impl DaemonState {
    pub(crate) fn spawn_with_failure_kind(
        &self,
        role: &str,
        task: &str,
        parent_session_id: &str,
        options: rightkit_agent_coding_tools::SubagentSpawnOptions,
        failure_kind_slot: SubagentFailureKindSlot,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> {
        let state = self.clone();
        let role = role.to_string();
        let task_text = task.to_string();
        let parent_id = parent_session_id.to_string();
        Box::pin(async move {
            // PR 5 dispatch: when the caller asks for an authenticated
            // CLI runtime, route the spawn through the CLI helper
            // instead of the legacy native path. The helper honours
            // the session's `CliDelegationPolicy`, claims an
            // idempotency key in `driver_event_inbox`, persists the
            // child `driver_bindings` row with `parent_agent_id` set,
            // and returns the structured `AgentResult` projected onto
            // its `final_text`.
            if let Some(rightkit_agent_coding_tools::SpawnRuntimeKind::AuthenticatedCli {
                ..
            }) = &options.runtime
            {
                // A CLI child is still a sub-agent: the depth backstop, the
                // tier ceiling and the run budget apply exactly as for a
                // native spawn (audit 2026-11-05 orchestration M1).
                let parent = state
                    .get_session(&parent_id)
                    .ok_or_else(|| format!("parent session not found: {parent_id}"))?;
                check_subagent_depth(&state, &parent_id)?;
                // A vendor-CLI child builds its own context outside this daemon's session
                // loader, so a zero-context request cannot be honoured: refuse it visibly.
                if options.bare {
                    return Err(
                        "spawn_subagent: 'bare' is not supported with runtime 'authenticated_cli' (the vendor CLI loads its own context); omit 'bare' or use the native runtime"
                            .to_string(),
                    );
                }
                let cli_slot_id = format!("cli-{}", next_subagent_id());
                check_subagent_tier(&state, &parent, &role, &cli_slot_id, &task_text, &options)?;
                check_spawnable_roles(&parent, &role)?;
                // An exit before dispatch (no executor) rolls the admission
                // back through the slot's `Drop`.
                let mut budget_slot = admit_subagent_budget(
                    &state,
                    &parent,
                    &parent_id,
                    &role,
                    options.queue_when_full,
                )
                .await?;
                if state.cli_executor.is_none() {
                    return Err(
                        "authenticated CLI runtime is unavailable; refusing legacy-provider fallback"
                            .to_string(),
                    );
                }
                // The admission is spent once dispatch starts (it may launch a
                // vendor process before failing), so the cumulative count
                // holds; completion or a dropped future frees the concurrent slot.
                budget_slot.registered(&cli_slot_id, None);
                // Round 2 M6: the dispatch runs as its own task that owns the
                // slot. A caller that drops this future (a shorter timeout)
                // no longer cancels the dispatch mid-way, which skipped
                // `cleanup_child_runtime` and freed the slot while the vendor
                // child could still be running; the slot is released only
                // when the dispatch itself has finished and cleaned up.
                let dispatch = tokio::spawn(async move {
                    let result = crate::cli_delegation::dispatch_cli_spawn(
                        state, role, task_text, parent_id, options,
                    )
                    .await;
                    budget_slot.complete().await;
                    result
                });
                return dispatch.await.unwrap_or_else(|error| {
                    Err(format!("CLI sub-agent dispatch stopped: {error}"))
                });
            }
            let parent_session = state.get_session(&parent_id);
            if parent_session.is_none() {
                return Err(format!("parent session not found: {parent_id}"));
            }
            // Codex's continuation form re-attaches to an existing child
            // instead of creating a second transcript. Validate ownership
            // before enqueueing so a parent cannot steer a foreign child.
            if let Some(rightkit_agent_coding_tools::SubagentContinuity::ContinueAgent {
                continue_agent_id,
            }) = &options.continuity
            {
                let child = state
                    .get_session(continue_agent_id)
                    .ok_or_else(|| format!("continuation child not found: {continue_agent_id}"))?;
                if child.parent_session_id.as_deref() != Some(parent_id.as_str()) {
                    return Err(format!(
                        "continuation child {continue_agent_id} is not owned by parent {parent_id}"
                    ));
                }
                if child.msg_tx.is_none() {
                    return Err(format!(
                        "continuation child {continue_agent_id} is not accepting input"
                    ));
                }
                child.send_route_bound_text(task_text).await.map_err(|_| {
                    format!("continuation child {continue_agent_id} rejected follow-up")
                })?;
                return Ok(format!("continued sub-agent {continue_agent_id}"));
            }
            // A caller that must know the child's identity up front (a
            // `visible` spawn, whose watch window attaches to the child's
            // session) allocates the id itself; otherwise mint one.
            let sub_id = options
                .child_session_id
                .clone()
                .unwrap_or_else(next_subagent_id);
            // The spawning call's id rides on the child's lifecycle events (`toolCallId`).
            let spawn_tool_call_id = options.tool_call_id.clone();
            // Depth backstop: refuse to spawn deeper than MAX_SUBAGENT_DEPTH so a
            // sub-agent that re-spawns itself can't build an unbounded chain. The
            // per-agent wall-clock ceiling bounds runtime; this bounds nesting.
            check_subagent_depth(&state, &parent_id)?;
            // FM-088/FM-104/S:F8: resolve the role's built-in role pack
            // against the live model catalog BEFORE any admission or
            // workspace work, so an invalid pack (unknown/unqualified model,
            // malformed TOML) fails the spawn visibly with no partial
            // admission and no worktree ever created. A role with no
            // built-in pack (anything other than coder/orchestrator/
            // verifier) is unaffected — `resolved_role_pack` stays `None`
            // and the legacy per-agent-TOML/inherited-model
            // resolution below runs exactly as before.
            // In single-model mode (the default) every role, including a
            // role-pack role such as `coder`, runs on the one model the parent
            // session uses, and a pack model missing from the live catalog does
            // not refuse the spawn (orchestration rules 1 and 42, D3, D31). In
            // split mode the pack's own model assignment drives the spawn.
            let parent_root = parent_session
                .as_ref()
                .map(|parent| parent.workspace_root.as_str())
                .unwrap_or_default();
            // Per-role models need the split entitlement unless the host opens them to everyone
            // (`host_options::role_model_split_open_to_all`, applied inside `model_policy`).
            let split_available =
                crate::model_policy::split_entitled(|feature| state.has_license_feature(feature));
            // D1: a sub-agent of a `trustWorkspace` session reads the workspace layer whole too
            // (its route must resolve); policy trust is not inherited.
            let parent_config_granted = parent_session
                .as_ref()
                .is_some_and(|parent| session_config_granted(&parent.id));
            let pre_workspace_config = rightkit_agent_config::ConfigLoader::new()
                .load_files_for_workspace_granted(
                    std::path::Path::new(parent_root),
                    parent_config_granted,
                );
            // Per-agent TOML scoping (`.{app}/agents/<role>.toml`): role,
            // optional model override, and agent-scoped MCP servers. Loaded
            // once, before pack resolution, so the same file drives the
            // split-mode route and the fork's model override. A malformed
            // file refuses the spawn.
            let agent_toml = if parent_root.is_empty() {
                None
            } else {
                rightkit_agent_config::AgentMcpConfig::load_or_err(
                    &rightkit_agent_config::find_agents_dir(std::path::Path::new(parent_root)),
                    &role,
                )
                .map_err(|error| {
                    format!("agent TOML config failed for role '{role}': {error}; refusing spawn")
                })?
            };
            // Split mode only: the one model + provider this role runs on.
            // Model precedence: per-spawn native route > `.{app}/agents/<role>.toml`
            // model > `[orchestration.agents.<role>]` > role-pack default.
            // Provider precedence: explicit provider-id > route declaring the model > slug.
            let split_parent_provider: Option<String> =
                parent_session.as_ref().and_then(|parent| {
                    lock_recover(&parent.provider_id, "session.provider_id").clone()
                });
            let split_route: Option<(String, String)> =
                if crate::model_policy::uses_single_role_model(
                    &pre_workspace_config,
                    split_available,
                ) {
                    None
                } else {
                    let entry = pre_workspace_config.orchestration.agents.get(&role);
                    let entry_model = entry.map(|e| e.model_id.trim()).unwrap_or_default();
                    let entry_provider = entry.map(|e| e.provider_id.trim()).unwrap_or_default();
                    let toml_model = agent_toml
                        .as_ref()
                        .and_then(|cfg| cfg.model.as_deref())
                        .map(str::trim)
                        .filter(|model| !model.is_empty());
                    if let Some((provider, model)) = native_spawn_route(&options) {
                        // A bare per-spawn model finds its route like any other split model;
                        // only an explicit provider/model pair keeps its own provider.
                        let explicit = options
                            .runtime
                            .as_ref()
                            .and_then(|runtime| runtime.native_provider_model())
                            .is_some();
                        let provider = if explicit {
                            provider
                        } else {
                            split_provider_for_model(
                                &pre_workspace_config,
                                &model,
                                split_parent_provider.as_deref(),
                            )?
                        };
                        Some((model, provider))
                    } else if let Some(model) = toml_model {
                        let provider = if !entry_provider.is_empty()
                            && entry_model.eq_ignore_ascii_case(model)
                        {
                            entry_provider.to_string()
                        } else {
                            split_provider_for_model(
                                &pre_workspace_config,
                                model,
                                split_parent_provider.as_deref(),
                            )?
                        };
                        Some((model.to_string(), provider))
                    } else if entry_model.is_empty() && entry_provider.is_empty() {
                        None
                    } else {
                        let routes = &pre_workspace_config.provider_routes;
                        let model = if entry_model.is_empty() {
                            routes
                                .get(entry_provider)
                                .map(|route| route.model.trim())
                                .unwrap_or_default()
                        } else {
                            entry_model
                        };
                        if !entry_provider.is_empty() {
                            if !routes.contains_key(entry_provider)
                                && rightkit_llm::agent_providers::find_provider(entry_provider)
                                    .is_none()
                            {
                                return Err(format!(
                                    "[orchestration.agents.{role}] provider-id '{entry_provider}' names no \
                                     [provider-routes.{entry_provider}] and no catalog provider — refusing to spawn"
                                ));
                            }
                            if model.is_empty() {
                                return Err(format!(
                                    "[orchestration.agents.{role}] sets provider-id '{entry_provider}' but no model-id, \
                                     and no [provider-routes.{entry_provider}] declares a model for it; set model-id — refusing to spawn"
                                ));
                            }
                        }
                        let provider = if entry_provider.is_empty() {
                            split_provider_for_model(
                                &pre_workspace_config,
                                model,
                                split_parent_provider.as_deref(),
                            )?
                        } else {
                            entry_provider.to_string()
                        };
                        Some((model.to_string(), provider))
                    }
                };
            let resolved_role_pack = if crate::model_policy::uses_single_role_model(
                &pre_workspace_config,
                split_available,
            ) {
                let single_model = parent_session
                    .as_ref()
                    .map(|parent| lock_recover(&parent.model, "session.model").clone())
                    .filter(|model| !model.trim().is_empty());
                match single_model {
                    Some(model) => {
                        match crate::orchestrator::resolve_builtin_role_pack_for_single_model_spawn(
                            &role, &model,
                        ) {
                            Ok(resolved) => resolved,
                            Err(error) => {
                                return Err(format!(
                                "role pack resolution failed for role '{role}': {error} — refusing to spawn"
                            ));
                            }
                        }
                    }
                    None => None,
                }
            } else if let Some((override_model, _)) = split_route.as_ref() {
                // The split-mode route (per-spawn native route > agent TOML model >
                // `[orchestration.agents.<role>]`) is the user's own choice of model for this role and
                // outranks the pack's built-in default: the pack keeps its budgets, toolset and persona
                // but runs on the user's model and route, instead of on a pack model the user has no
                // route for ("no engine for sub-agent", D31).
                match crate::orchestrator::resolve_builtin_role_pack_for_single_model_spawn(
                    &role,
                    override_model,
                ) {
                    Ok(resolved) => resolved,
                    Err(error) => {
                        return Err(format!(
                            "role pack resolution failed for role '{role}' on its configured model \
                             '{override_model}': {error} — refusing to spawn"
                        ));
                    }
                }
            } else {
                let role_pack_catalog = crate::orchestrator::live_role_pack_catalog();
                match crate::orchestrator::resolve_builtin_role_pack_for_spawn(
                    &role,
                    &role_pack_catalog,
                ) {
                    Ok(resolved) => resolved,
                    Err(error) => {
                        return Err(format!(
                            "role pack resolution failed for role '{role}': {error} — refusing to spawn"
                        ));
                    }
                }
            };
            if crate::model_policy::uses_single_role_model(&pre_workspace_config, split_available) {
                if let (Some((_, native_model)), Some(resolved)) =
                    (native_spawn_route(&options), resolved_role_pack.as_ref())
                {
                    tracing::warn!(
                        role = %role,
                        ignored_model = %native_model,
                        single_model = %resolved.primary.id,
                        "single-model mode: per-spawn model ignored for role-pack role"
                    );
                }
            }
            // R13 (CR-42/43): tier-ceiling enforcement on the LIVE spawn path.
            //
            // `spawn_with_capabilities` (orchestration::spawn) checks a
            // requested tier against `Profile::max_tier_ceiling` via
            // `resolve_invocation`, but it is only ever called by its own
            // test — this daemon fork is the actual production spawn and
            // never consulted it, so the ceiling was open in production.
            //
            // This is a role-routed spawn, not a capability search, so we
            // call `resolve_invocation` directly (the one enforcement
            // primitive both callers share) rather than going through
            // `spawn_with_capabilities`'s capability-matching wrapper, which
            // has nothing to match here — the role is already fixed.
            //
            // The requesting agent's OWN registered escalation tier is its
            // ceiling: a bounded worker must not be able to spawn a child
            // registered at a higher tier than itself. A parent with no
            // role (a root/human-driven session) or a custom role absent
            // from the registry is not itself a bounded subagent, so it is
            // not tier-restricted here — there is no ceiling to check it
            // against.
            // Once the parent is known to be a bounded subagent, every spawn it
            // makes is checked -- including one naming a role the registry does
            // not know. An unknown child role has no tier to compare, and a
            // security boundary that skips the case it cannot evaluate is open
            // by exactly the input an attacker controls: the role string. Fail
            // closed instead.
            if let Some(parent) = parent_session.as_ref() {
                check_subagent_tier(&state, parent, &role, &sub_id, &task_text, &options)?;
                check_spawnable_roles(parent, &role)?;
            }
            let mut budget_slot = admit_subagent_budget(
                &state,
                parent_session
                    .as_ref()
                    .expect("parent session existence checked above"),
                &parent_id,
                &role,
                options.queue_when_full,
            )
            .await?;
            // Inherit the parent's workspace so the worker operates on the same
            // project the conductor was given, not an empty root.
            let parent_workspace = parent_session
                .as_ref()
                .map(|p| p.workspace_root.clone())
                .unwrap_or_default();
            let parent_execution_backend = parent_session
                .as_ref()
                .and_then(|p| p.execution_backend.clone());
            let parent_execution_workspace_id = parent_session
                .as_ref()
                .and_then(|p| p.execution_workspace_id.clone());
            let parent_execution_network_profile = parent_session
                .as_ref()
                .and_then(|p| p.execution_network_profile.clone());
            let parent_mcp_channel = parent_session
                .as_ref()
                .map(|p| p.mcp_channel.clone())
                .unwrap_or_else(|| "desktop".to_string());
            let subagent_start_span_id = record_subagent_runtime_span(
                &state,
                SubagentSpanSubject {
                    parent_session_id: &parent_id,
                    sub_session_id: &sub_id,
                    role: &role,
                    task: &task_text,
                    workspace: &parent_workspace,
                },
                SubagentSpanExecution {
                    execution_backend: parent_execution_backend.as_deref(),
                    execution_workspace_id: parent_execution_workspace_id.as_deref(),
                    execution_network_profile: parent_execution_network_profile.as_deref(),
                    mcp_channel: Some(&parent_mcp_channel),
                },
                SubagentSpan {
                    phase: MiddlewarePhase::SubagentStart,
                    status: "started",
                    parent_span_id: None,
                    message: None,
                    extra: serde_json::json!({
                        "worktreeRequested": options.worktree.is_some(),
                        "maxIterations": options.max_iterations,
                    }),
                },
            );
            // ORCH-07: a child that may write and runs beside another child of the same parent
            // gets its own worktree by default. Cleanup stays off: the worktree holds the
            // child's work and is reported in the result trailer.
            let mut options = options;
            if options.worktree.is_none()
                && subagent_may_write(
                    &role,
                    &options,
                    &pre_workspace_config,
                    resolved_role_pack
                        .as_ref()
                        .and_then(|resolved| resolved.pack.toolset.first().cloned()),
                )
            {
                let in_flight = match parent_session.as_ref() {
                    Some(parent) => parent.run_budget.lock().await.concurrent_in_flight(),
                    None => 0,
                };
                // This child's own slot is already admitted, so more than one means another
                // child of the same parent is running.
                if in_flight > 1 {
                    options.worktree = Some(rightkit_agent_coding_tools::SubagentWorktreeOptions {
                        branch: None,
                        path: None,
                        cleanup: false,
                    });
                }
            }
            let (workspace, cleanup_worktree) = match prepare_subagent_workspace(
                &parent_workspace,
                &role,
                &sub_id,
                &options,
            )
            .await
            {
                Ok(prepared) => prepared,
                Err(e) => {
                    if let Some(parent) = parent_session.as_ref() {
                        let mut ledger = parent.run_budget.lock().await;
                        ledger.rollback_after_admit();
                        budget_slot.disarm();
                        SUBAGENT_SLOT_FREED.notify_waiters();
                        if let Some(storage) = state.storage.as_ref() {
                            storage
                                    .lock()
                                    .await
                                    .upsert_run_budget(&parent_id, ledger.cumulative_used())
                                    .map_err(|error| {
                                        format!(
                                            "{e}; additionally failed to persist budget rollback: {error}"
                                        )
                                    })?;
                        }
                    }
                    record_subagent_runtime_span(
                        &state,
                        SubagentSpanSubject {
                            parent_session_id: &parent_id,
                            sub_session_id: &sub_id,
                            role: &role,
                            task: &task_text,
                            workspace: &parent_workspace,
                        },
                        SubagentSpanExecution {
                            execution_backend: parent_execution_backend.as_deref(),
                            execution_workspace_id: parent_execution_workspace_id.as_deref(),
                            execution_network_profile: parent_execution_network_profile.as_deref(),
                            mcp_channel: Some(&parent_mcp_channel),
                        },
                        SubagentSpan {
                            phase: MiddlewarePhase::SubagentStop,
                            status: "failed",
                            parent_span_id: subagent_start_span_id.as_deref(),
                            message: Some(&e),
                            extra: serde_json::json!({ "failureStage": "prepareWorkspace" }),
                        },
                    );
                    return Err(e);
                }
            };
            // The sub-agent's file changes are offered to the chat that started it (review:
            // `proposal` on each change, kept or discarded there).
            crate::api_run_changes::register_subagent_parent(
                &sub_id,
                &parent_id,
                &role,
                &parent_workspace,
            );
            let fork_workspace_config = rightkit_agent_config::ConfigLoader::new()
                .load_files_for_workspace_granted(
                    std::path::Path::new(&workspace),
                    parent_config_granted,
                );
            // The per-agent TOML (`.{app}/agents/<role>.toml`) was loaded once
            // above; its model wins over the inherited model when present.
            let agent_model_override = agent_toml
                .and_then(|cfg| cfg.model)
                .filter(|m| !m.trim().is_empty());
            let inherited_model = if crate::model_policy::uses_single_role_model(
                &fork_workspace_config,
                split_available,
            ) {
                parent_session
                    .as_ref()
                    .map(|parent| lock_recover(&parent.model, "session.model").clone())
            } else {
                None
            };
            // Single resolution order: per-spawn native route > per-agent TOML
            // model > `[orchestration.agents.<role>]` > role-pack default
            // (S:F8: "coder.v1.toml drives role→model assignment through live
            // spawn"). In split mode `split_route` (applied below via
            // `use_route`) already carries the first three and wins; the pack
            // default only applies when no split route exists.
            let inherited_model = agent_model_override.or(inherited_model);
            let inherited_model = resolved_role_pack
                .as_ref()
                .map(|resolved| resolved.primary.id.clone())
                .or(inherited_model);
            let mut fork = build_subagent_fork(
                SubagentIdentity {
                    sub_session_id: sub_id.clone(),
                    parent_session_id: parent_id.clone(),
                    role: role.clone(),
                    task: task_text.clone(),
                    workspace_root: workspace.clone(),
                },
                parent_session.as_ref(),
                &options,
                fork_workspace_config.prompt.brief_default(),
                inherited_model,
            );
            if let Some(resolved) = resolved_role_pack.as_ref() {
                if let Err(error) = fork.apply_resolved_role_pack(resolved) {
                    if let Some(parent) = parent_session.as_ref() {
                        let mut ledger = parent.run_budget.lock().await;
                        ledger.rollback_after_admit();
                        budget_slot.disarm();
                        SUBAGENT_SLOT_FREED.notify_waiters();
                        if let Some(storage) = state.storage.as_ref() {
                            storage
                                .lock()
                                .await
                                .upsert_run_budget(&parent_id, ledger.cumulative_used())
                                .map_err(|storage_error| format!(
                                    "{error}; additionally failed to persist budget rollback: {storage_error}"
                                ))?;
                        }
                    }
                    if let Some(path) = cleanup_worktree.as_deref() {
                        cleanup_subagent_worktree(&parent_workspace, path).await;
                    }
                    return Err(format!(
                        "role pack binding failed for role '{role}': {error} — refusing to spawn"
                    ));
                }
            }
            // Pack binding and fork construction re-derive the model and provider from the model
            // slug; the split-mode route (per-spawn native route > agent TOML model >
            // `[orchestration.agents.<role>]`; provider: explicit > route declaring the model > slug)
            // is the one the child must use, for pack and non-pack roles alike.
            if let Some((model, provider)) = split_route.as_ref() {
                fork.use_route(model, provider);
            }
            // Single-model mode means every role runs on the orchestrator's model, and that has
            // to include its route. A role with a built-in descriptor re-derived the provider
            // from the model's name, so a parent on a custom-named route (say `anthropic-direct`)
            // got a child bound to a provider with no route or key: "no engine for sub-agent".
            if split_route.is_none()
                && crate::model_policy::uses_single_role_model(
                    &fork_workspace_config,
                    split_available,
                )
            {
                if let Some(parent) = parent_session.as_ref() {
                    let parent_model = lock_recover(&parent.model, "session.model").clone();
                    let parent_provider =
                        lock_recover(&parent.provider_id, "session.provider_id").clone();
                    if let Some(provider) = parent_provider.filter(|p| !p.trim().is_empty()) {
                        if !parent_model.trim().is_empty() {
                            fork.use_route(&parent_model, &provider);
                        }
                    }
                }
            }
            // Parent cancellation has begun: admit no new children (CR2).
            let parent_cancelling = state.session_cancelling(&parent_id);
            if state.get_session(&parent_id).is_none() || parent_cancelling {
                if let Some(parent) = parent_session.as_ref() {
                    let mut ledger = parent.run_budget.lock().await;
                    ledger.rollback_after_admit();
                    budget_slot.disarm();
                    SUBAGENT_SLOT_FREED.notify_waiters();
                    if let Some(storage) = state.storage.as_ref() {
                        storage
                            .lock()
                            .await
                            .upsert_run_budget(&parent_id, ledger.cumulative_used())
                            .map_err(|error| {
                                format!(
                                    "failed to persist budget rollback for {parent_id}: {error}"
                                )
                            })?;
                    }
                }
                if let Some(path) = cleanup_worktree.as_deref() {
                    cleanup_subagent_worktree(&parent_workspace, path).await;
                }
                if parent_cancelling {
                    return Err(format!(
                        "parent session is cancelling; no new sub-agents: {parent_id}"
                    ));
                }
                return Err(format!(
                    "parent session was deleted before worker start: {parent_id}"
                ));
            }
            // The owner edge (`parent_session_id`) and the child's interrupt
            // channel are registered here, and the completion observation is
            // subscribed below, all BEFORE the task is dispatched (CR2).
            let live = state.create_session_with_options(
                &fork.sub_session_id,
                Some(&role),
                &fork.workspace_root,
                &format!("{role} subtask"),
                Some(&parent_id),
                fork.runtime_options(),
            );
            let mut sub = live.conduit.subscribe();
            // The child exists now: a drop from here on releases its slot once
            // and interrupts it (the `!sent` branch and a caller's timeout).
            budget_slot.registered(&sub_id, live.interrupt_tx.clone());
            // Close the race with a cancel that landed after the check above.
            if state.session_cancelling(&parent_id) {
                if let Some(tx) = &live.interrupt_tx {
                    let _ = tx.send(true);
                }
            }

            // Notify parent: sub-agent spawned.
            if let Some(parent) = state.get_session(&parent_id) {
                let seq = parent.conduit.next_seq();
                parent.conduit.publish(ProtocolEvent::SubAgentSpawned {
                    session_id: parent_id.clone(),
                    seq,
                    created_at: now_iso(),
                    sub_session_id: sub_id.clone(),
                    role: role.clone(),
                    task_title: handoff_task_title(&task_text),
                    tool_call_id: spawn_tool_call_id.clone(),
                });
                // Carry the child's resolved model onto the rail so the user can
                // see which model each agent ran on — the difference Pro's
                // multi-model routing actually buys.
                let child_model = lock_recover(&live.model, "session.model").clone();
                let child_provider = lock_recover(&live.provider_id, "session.provider_id").clone();
                ensure_parent_agent_roster(
                    &parent,
                    &sub_id,
                    &role,
                    &child_model,
                    child_provider.as_deref(),
                );
                publish_agent_roster(&parent);
            }

            // A delegated task is admitted input too: keep its original text
            // in the canonical child transcript before execution can publish
            // later facts. Direct channel delivery alone made live child chat
            // empty until an assistant response completed.
            lock_recover(&live.transcript, "session.transcript").push(
                rightkit_agent_engine_protocol::TranscriptMessage::User {
                    id: format!("user-{}", now_millis()),
                    created_at: now_iso(),
                    content: task_text.clone(),
                    trust: None,
                    pinned: false,
                    attachments: Vec::new(),
                },
            );
            let seq = live.conduit.next_seq();
            live.conduit.publish(ProtocolEvent::UserMessageSubmitted {
                session_id: live.id.clone(),
                seq,
                created_at: now_iso(),
                text: task_text.clone(),
                nonce: None,
                trust: None,
                attachments: Vec::new(),
            });
            // Tools rule 58 `SubagentStart`: the child session exists and is about
            // to receive its task. Observing only; a failing hook never blocks it.
            run_subagent_start_hooks(&live.hook_bus, &sub_id, &role).await;
            let memory_injection = crate::api::retrieve_session_memory(
                &state,
                &live.id,
                &live.workspace_root,
                &task_text,
                5,
                "subagent",
            )
            .await;
            let reminder = dynamic_user_turn_reminder(
                memory_injection
                    .as_ref()
                    .map(|injection| injection.context.as_str()),
                &[],
            );
            let briefing = decorate_user_payload_with_system_reminder(&task_text, &reminder);
            // A224: siblings that share a prompt prefix start one after the other, so the later
            // ones read the cache the first wrote. The first starts now; the others wait for its
            // first streamed byte or `[orchestration] sibling-start-hold-ms` (0 = off).
            let sibling_hold_ms = pre_workspace_config.orchestration.sibling_start_hold_ms();
            let mut sibling_leader = None;
            if sibling_hold_ms > 0 {
                let prefix = format!(
                    "{role}|{:?}",
                    (
                        options.bare,
                        options.fork,
                        options.handoff_summary,
                        options.worktree.is_some(),
                        &options.runtime,
                        &options.allowed_tools,
                        &options.disallowed_tools,
                        options.permission_mode,
                    )
                );
                match rightkit_agent_orchestration::coding::sibling_hold::claim(&parent_id, &prefix)
                {
                    rightkit_agent_orchestration::coding::sibling_hold::SiblingStart::Leader(
                        guard,
                    ) => {
                        sibling_leader = Some(guard);
                    }
                    rightkit_agent_orchestration::coding::sibling_hold::SiblingStart::Follower(
                        wait,
                    ) => {
                        wait.wait(std::time::Duration::from_millis(sibling_hold_ms))
                            .await;
                    }
                }
            }
            // Send the task.
            let sent = if live.msg_tx.is_some() {
                live.send_route_bound_text(briefing).await.is_ok()
            } else {
                false
            };
            if !sent {
                if let Some(path) = cleanup_worktree.as_deref() {
                    cleanup_subagent_worktree(&parent_workspace, path).await;
                }
                record_subagent_runtime_span(
                    &state,
                    SubagentSpanSubject {
                        parent_session_id: &parent_id,
                        sub_session_id: &sub_id,
                        role: &role,
                        task: &task_text,
                        workspace: &workspace,
                    },
                    SubagentSpanExecution {
                        execution_backend: parent_execution_backend.as_deref(),
                        execution_workspace_id: parent_execution_workspace_id.as_deref(),
                        execution_network_profile: parent_execution_network_profile.as_deref(),
                        mcp_channel: Some(&parent_mcp_channel),
                    },
                    SubagentSpan {
                        phase: MiddlewarePhase::SubagentStop,
                        status: "failed",
                        parent_span_id: subagent_start_span_id.as_deref(),
                        message: Some("no engine for sub-agent"),
                        extra: serde_json::json!({ "failureStage": "sendTask" }),
                    },
                );
                record_subagent_transcript_artifact(&state, &fork, false);
                return Err("no engine for sub-agent".into());
            }

            // Wait for the sub-agent to complete by listening to its conduit.
            // Bounded by a wall-clock ceiling so the conductor can never deadlock
            // on a wedged worker (defense in depth alongside the engine's
            // terminal-event guarantees and the provider request timeout).
            // (`sub` was subscribed before dispatch so an immediately-finishing
            // child cannot be missed.) Only the typed terminal turn event
            // settles the child; interim assistant messages just update the
            // latest content.
            // Round 2 M5: set when the wait ends without a terminal event (the
            // conduit lagged or closed), so the child may still be running.
            let mut child_may_still_run = false;
            // Kept outside the wait so a stop or timeout does not lose the text
            // the child had already produced.
            let mut last_content = String::new();
            // Text streamed since the last completed message: a reply that carries tool calls is never
            // "completed" as a message, so its text is only seen as deltas.
            let mut streamed_text = String::new();
            // For the typed result trailer: files the child's writing tools named, and the last
            // known cost it reported.
            let mut touched_files = std::collections::BTreeSet::<String>::new();
            let mut child_cost_usd: Option<f64> = None;
            let wait = async {
                loop {
                    match sub.recv().await {
                        ConduitReceive::Event(
                            event @ (ProtocolEvent::AssistantMessageCompleted { .. }
                            | ProtocolEvent::OrchestrationObservationRecorded { .. }
                            | ProtocolEvent::SessionTurnFinished { .. }),
                        ) => {
                            if matches!(event, ProtocolEvent::AssistantMessageCompleted { .. }) {
                                streamed_text.clear();
                            }
                            if let Some(settled) = settle_subagent_event(&event, &mut last_content)
                            {
                                break settled;
                            }
                        }
                        ConduitReceive::Event(ProtocolEvent::RunFailed { error, kind, .. }) => {
                            // Carry the typed failure kind to the outcome
                            // classifier; text matching is only the fallback.
                            if !kind.trim().is_empty() {
                                *lock_recover(&failure_kind_slot, "failure_kind_slot") = Some(kind);
                            }
                            if error.starts_with(crate::orchestrator::SUBAGENT_PANIC_PREFIX) {
                                if let Some(storage) = state.storage.as_ref() {
                                    persist_session_failed(storage, &sub_id).await;
                                }
                            }
                            break Err(error);
                        }
                        ConduitReceive::Event(ProtocolEvent::RunInterrupted { .. }) => {
                            break Err("sub-agent interrupted".to_string())
                        }
                        ConduitReceive::Event(ProtocolEvent::BudgetExceeded {
                            current_usd,
                            ceiling_usd,
                            ..
                        }) => {
                            break Err(format!(
                                "session budget exceeded (${current_usd:.4} / ${ceiling_usd:.4})"
                            ))
                        }
                        ConduitReceive::Lagged { missed } => {
                            child_may_still_run = true;
                            break Err(format!(
                                "sub-agent conduit lagged ({missed} events missed); resync required"
                            ));
                        }
                        ConduitReceive::Closed => {
                            child_may_still_run = true;
                            break Err("sub-agent conduit closed".to_string());
                        }
                        ConduitReceive::Event(other) => {
                            // The first streamed byte (text, reasoning, a tool call or usage)
                            // releases the siblings held behind this child (A224).
                            if let Some(leader) = sibling_leader.as_ref() {
                                if matches!(
                                    other,
                                    ProtocolEvent::AssistantDelta { .. }
                                        | ProtocolEvent::ToolCallStarted { .. }
                                        | ProtocolEvent::UsageUpdated { .. }
                                ) {
                                    leader.first_byte();
                                }
                            }
                            // Accumulate per-agent metrics for the parent's agent rail (F1).
                            // The child's usage/tool events flow on its own conduit, which
                            // this loop is subscribed to.
                            // What the child says and does goes to the parent's stream too,
                            // tagged with the call that spawned it (a display event only).
                            if let Some(parent) = state.get_session(&parent_id) {
                                let forwarded = match &other {
                                    ProtocolEvent::AssistantDelta {
                                        text,
                                        is_reasoning: false,
                                        ..
                                    } if !text.is_empty() => {
                                        Some(("text", text.clone(), None, None))
                                    }
                                    ProtocolEvent::ToolCallStarted { call, .. } => Some((
                                        "toolCall",
                                        String::new(),
                                        Some(call.name.clone()),
                                        Some(call.id.clone()),
                                    )),
                                    ProtocolEvent::ToolResultAdded {
                                        result:
                                            rightkit_agent_engine_protocol::TranscriptMessage::Tool {
                                                tool_call_id,
                                                ..
                                            },
                                        ..
                                    } => Some((
                                        "toolResult",
                                        String::new(),
                                        None,
                                        Some(tool_call_id.clone()),
                                    )),
                                    _ => None,
                                };
                                if let Some((output_kind, text, tool_name, child_call_id)) =
                                    forwarded
                                {
                                    let seq = parent.conduit.next_seq();
                                    parent.conduit.publish(ProtocolEvent::SubAgentOutput {
                                        session_id: parent_id.clone(),
                                        seq,
                                        created_at: now_iso(),
                                        sub_session_id: sub_id.clone(),
                                        role: role.clone(),
                                        tool_call_id: spawn_tool_call_id.clone(),
                                        output_kind: output_kind.to_string(),
                                        text,
                                        tool_name,
                                        child_call_id,
                                    });
                                }
                            }
                            let changed = match &other {
                                ProtocolEvent::AssistantDelta {
                                    text,
                                    is_reasoning: false,
                                    ..
                                } => {
                                    streamed_text.push_str(text);
                                    false
                                }
                                ProtocolEvent::UsageUpdated {
                                    prompt_tokens,
                                    completion_tokens,
                                    cache_read_tokens,
                                    cost_usd,
                                    cost_known,
                                    ..
                                } => {
                                    if let Some(parent) = state.get_session(&parent_id) {
                                        record_parent_agent_usage(
                                            &parent,
                                            &sub_id,
                                            *prompt_tokens,
                                            *completion_tokens,
                                            *cache_read_tokens,
                                            cost_known.then_some(*cost_usd),
                                        );
                                    }
                                    if *cost_known {
                                        child_cost_usd = Some(*cost_usd);
                                    }
                                    true
                                }
                                ProtocolEvent::ToolCallStarted { call, .. } => {
                                    collect_subagent_touched_paths(
                                        &call.name,
                                        &call.arguments_json,
                                        &mut touched_files,
                                    );
                                    if let Some(parent) = state.get_session(&parent_id) {
                                        record_parent_agent_tool(&parent, &sub_id, &call.name);
                                    }
                                    true
                                }
                                _ => false,
                            };
                            if changed {
                                if let Some(parent) = state.get_session(&parent_id) {
                                    publish_agent_roster(&parent);
                                }
                            }
                        }
                    }
                }
            };
            // The limit is the configured `max-wall-clock-minutes` and nothing
            // else (orchestration rule 43): one number, the one the settings
            // screen shows. Unit tests shorten it so a hung worker test ends.
            let ceiling = subagent_wall_clock_ceiling(&state);
            let waited = tokio::time::timeout(ceiling, wait).await;
            let result: Result<String, String> = match waited {
                Ok(r) => r,
                Err(_) => {
                    // Stop the wedged worker and report a clean failure.
                    child_may_still_run = true;
                    Err(format!(
                        "sub-agent exceeded the {}-minute wall-clock ceiling",
                        ceiling.as_secs() / 60
                    ))
                }
            };
            // Round 2 M5: a child whose stream lagged, closed or timed out may
            // still be executing. Interrupt it and wait (bounded) for its turn
            // to end before the budget slot below is released, so the
            // conductor cannot schedule a replacement alongside it.
            if child_may_still_run
                && !interrupt_and_await_idle(
                    live.interrupt_tx.as_ref(),
                    &live.live_turn_handle,
                    SUBAGENT_STOP_GRACE.min(ceiling),
                )
                .await
            {
                tracing::warn!(sub_session_id = %sub_id, "sub-agent did not stop within the grace period after an interrupt; releasing its budget slot");
            }

            // Notify parent: sub-agent completed. `outcome`/`outcome_detail`
            // carry the sub-agent's TRUE completion state (parity P-2): a
            // rate-limit/wall-clock/budget cutoff is `partial_with_limit`
            // (never masqueraded as plain success/failure), and an
            // unrecovered provider/API error is `api_error` — reusing the
            // same classification the conductor's `WorkerResult::outcome`
            // uses, so the UI and the conductor agree on one taxonomy.
            let typed_failure_kind: Option<String> =
                lock_recover(&failure_kind_slot, "failure_kind_slot").clone();
            let (outcome_label, outcome_detail) = match &result {
                Ok(text) if text.starts_with(SUBAGENT_TURN_LIMIT_NOTE) => {
                    ("partial_with_limit".to_string(), "max_turns".to_string())
                }
                Ok(_) => ("complete".to_string(), String::new()),
                Err(e) => match crate::orchestrator::subagent_error_kind_typed(
                    typed_failure_kind.as_deref(),
                    e,
                ) {
                    crate::orchestrator::SubagentErrorKind::ResourceLimit(limit_kind) => {
                        ("partial_with_limit".to_string(), limit_kind)
                    }
                    crate::orchestrator::SubagentErrorKind::ApiError => {
                        ("api_error".to_string(), e.clone())
                    }
                },
            };
            let result_trailer = format!(
                "\n\n[sub-agent result] {}",
                serde_json::json!({
                    "status": outcome_label,
                    "outcomeDetail": outcome_detail,
                    "exhausted": outcome_label == "partial_with_limit",
                    "touchedFiles": touched_files.iter().take(100).collect::<Vec<_>>(),
                    "costUsd": child_cost_usd,
                    "worktree": options.worktree.is_some().then(|| workspace.clone()),
                })
            );
            if let Some(parent) = state.get_session(&parent_id) {
                let seq = parent.conduit.next_seq();
                parent.conduit.publish(ProtocolEvent::SubAgentCompleted {
                    session_id: parent_id.clone(),
                    seq,
                    created_at: now_iso(),
                    sub_session_id: sub_id.clone(),
                    role: role.clone(),
                    success: result.is_ok(),
                    summary: result
                        .as_deref()
                        .unwrap_or("task failed")
                        .chars()
                        .take(200)
                        .collect(),
                    outcome: outcome_label,
                    outcome_detail,
                    tool_call_id: spawn_tool_call_id.clone(),
                });
                set_parent_agent_status(
                    &parent,
                    &sub_id,
                    if result.is_ok() { "done" } else { "failed" },
                );
                publish_agent_roster(&parent);
            }
            // Lane C3 (task 2): the concurrent count drops and the cumulative
            // count holds, for Ok and Err outcomes alike. Release the slot exactly once per child, even if cancel and
            // completion both reach this point. The slot holds the parent it
            // was admitted on, so it releases even when the parent session was
            // removed from the live map meanwhile.
            budget_slot.complete().await;

            // Fire SubagentStop hook on the sub-agent's own bus.
            if let Some(sub_live) = state.get_session(&sub_id) {
                sub_live
                    .hook_bus
                    .publish(rightkit_hooks::agent_hooks::HookEvent::SubagentStop {
                        session_id: sub_id.clone(),
                        role: role.clone(),
                        success: result.is_ok(),
                    });
            }

            record_subagent_transcript_artifact(&state, &fork, result.is_ok());

            // The sub-agent ran: its checkout is removed only when that loses nothing.
            let worktree_kept = match cleanup_worktree.as_deref() {
                Some(path) => {
                    crate::api::cleanup_finished_subagent_worktree(&parent_workspace, path).await
                }
                None => None,
            };

            record_subagent_runtime_span(
                &state,
                SubagentSpanSubject {
                    parent_session_id: &parent_id,
                    sub_session_id: &sub_id,
                    role: &role,
                    task: &task_text,
                    workspace: &workspace,
                },
                SubagentSpanExecution {
                    execution_backend: parent_execution_backend.as_deref(),
                    execution_workspace_id: parent_execution_workspace_id.as_deref(),
                    execution_network_profile: parent_execution_network_profile.as_deref(),
                    mcp_channel: Some(&parent_mcp_channel),
                },
                SubagentSpan {
                    phase: MiddlewarePhase::SubagentStop,
                    status: if result.is_ok() {
                        "completed"
                    } else {
                        "failed"
                    },
                    parent_span_id: subagent_start_span_id.as_deref(),
                    message: result.as_ref().err().map(String::as_str),
                    extra: serde_json::json!({ "success": result.is_ok() }),
                },
            );

            // A failed child hands back what it had already written.
            match result {
                Err(error) => {
                    let partial = if streamed_text.trim().is_empty() {
                        last_content
                    } else {
                        streamed_text
                    };
                    let kept = worktree_kept
                        .map(|note| format!("\n\n{note}"))
                        .unwrap_or_default();
                    if partial.trim().is_empty() {
                        Err(format!("{error}{kept}{result_trailer}"))
                    } else {
                        Err(format!(
                            "{error}\n\nPartial output from the sub-agent before it stopped:\n{partial}{kept}{result_trailer}"
                        ))
                    }
                }
                Ok(text) => Ok(match worktree_kept {
                    Some(note) => format!("{text}\n\n{note}{result_trailer}"),
                    None => format!("{text}{result_trailer}"),
                }),
            }
        })
    }
}

/// True when `peer` shares the sender's workspace or sits in its parent chain or its subtree.
fn sessions_are_related(
    state: &DaemonState,
    sender: &std::sync::Arc<LiveSession>,
    peer: &std::sync::Arc<LiveSession>,
) -> bool {
    if sender.workspace_root == peer.workspace_root {
        return true;
    }
    let is_ancestor = |descendant: &std::sync::Arc<LiveSession>, ancestor_id: &str| {
        let mut next = descendant.parent_session_id.clone();
        let mut hops = 0;
        while let Some(id) = next {
            if id == ancestor_id || hops > 16 {
                return id == ancestor_id;
            }
            next = state
                .get_session(&id)
                .and_then(|s| s.parent_session_id.clone());
            hops += 1;
        }
        false
    };
    is_ancestor(sender, &peer.id) || is_ancestor(peer, &sender.id)
}

impl rightkit_agent_coding_tools::MessageDispatcher for DaemonState {
    fn dispatch(
        &self,
        message: rightkit_agent_coding_tools::MessageDispatch,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> {
        let state = self.clone();
        Box::pin(async move {
            let body = format!(
                "[Message from {} via {}]\n{}",
                message.from_session_id, message.channel, message.message
            );
            if let Some(target) = message.target_session_id.clone() {
                // `followup_task` carries a canonical AgentPath, unlike
                // ordinary send_message which addresses a raw session id.
                // Resolve it against sender lineage before touching the live
                // session, preserving exact subtree targeting for nested and
                // foreign-harness roots.
                let target = if message.channel == "followup_task" {
                    DaemonSubagentSupervisor::new(state.clone())
                        .resolve_path(&message.from_session_id, &target)
                        .await?
                } else {
                    target
                };
                // A model's message reaches only sessions of its own workspace or lineage; a
                // schedule delivery is not a model and names its own target.
                if message.channel != "schedule" && message.channel != "followup_task" {
                    let sender = state.get_session(&message.from_session_id);
                    if let (Some(sender), Some(peer)) =
                        (sender.as_ref(), state.get_session(&target))
                    {
                        if !sessions_are_related(&state, sender, &peer) {
                            let error = format!(
                                "session '{target}' is in another workspace; a message reaches only sessions of this workspace or its own lineage"
                            );
                            let _ = state.record_message_history(
                                message,
                                "failed",
                                Vec::new(),
                                Some(error.clone()),
                            );
                            return Err(error);
                        }
                    }
                }
                match state.submit_relayed_message(&target, &body).await {
                    Ok(_) => {
                        crate::async_inbox_feed::feed_session_message(
                            &target,
                            &message.from_session_id,
                            &message.channel,
                            &message.message,
                        );
                        let _ = state.record_message_history(
                            message,
                            "delivered",
                            vec![target.clone()],
                            None,
                        );
                    }
                    Err(e) => {
                        let _ = state.record_message_history(
                            message,
                            "failed",
                            Vec::new(),
                            Some(e.clone()),
                        );
                        return Err(e);
                    }
                }
                return Ok(format!("delivered to {target}"));
            }

            // A broadcast from a model reaches only its own live children; a schedule's explicit
            // broadcast keeps reaching every live session.
            let sessions = if message.channel == "schedule" {
                state.list_sessions()
            } else {
                state
                    .list_sessions()
                    .into_iter()
                    .filter(|peer| {
                        peer.parent_session_id.as_deref() == Some(message.from_session_id.as_str())
                    })
                    .collect()
            };
            if sessions.is_empty() {
                let error = "no live sessions available for broadcast".to_string();
                let _ = state.record_message_history(
                    message,
                    "failed",
                    Vec::new(),
                    Some(error.clone()),
                );
                return Err(error);
            }
            let mut delivered = Vec::new();
            for session in sessions {
                if let Err(e) = state.submit_relayed_message(&session.id, &body).await {
                    let _ =
                        state.record_message_history(message, "failed", delivered, Some(e.clone()));
                    return Err(e);
                }
                crate::async_inbox_feed::feed_session_message(
                    &session.id,
                    &message.from_session_id,
                    &message.channel,
                    &message.message,
                );
                delivered.push(session.id.clone());
            }
            let delivered_count = delivered.len();
            let delivered_list = delivered.join(", ");
            let _ = state.record_message_history(message, "broadcast", delivered, None);
            Ok(format!(
                "broadcast to {} session(s): {}",
                delivered_count, delivered_list
            ))
        })
    }
}

impl rightkit_agent_coding_tools::TeamDirectory for DaemonState {
    fn create_team(
        &self,
        team: rightkit_agent_coding_tools::TeamDefinition,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> {
        let teams = self.live_teams.clone();
        let store_path = self.team_store_path.clone();
        Box::pin(async move {
            if team.name.trim().is_empty() {
                return Err("team name cannot be empty".to_string());
            }
            if team.members.is_empty() {
                return Err("team must include at least one member".to_string());
            }
            let name = team.name.clone();
            let member_count = team.members.len();
            // Several processes share the file: this one team is added to the map on disk under
            // the cross-process lock, and the map as written becomes the held one.
            let written = update_team_store(&store_path, &teams, |on_disk| {
                on_disk.insert(name.clone(), team.clone());
                true
            });
            if let Err(error) = written {
                // Not written: the team is still registered in this process, as before.
                lock_recover(&teams, "live_teams").insert(name.clone(), team);
                return Err(error);
            }
            Ok(format!("{name} registered with {member_count} member(s)"))
        })
    }

    fn delete_team(
        &self,
        name: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> {
        let teams = self.live_teams.clone();
        let store_path = self.team_store_path.clone();
        Box::pin(async move {
            // Several processes share the file: this one team is removed from the map on disk
            // under the cross-process lock, and the map as written becomes the held one. A team
            // only this process holds (an earlier save failed) is removed here all the same.
            let held_here = lock_recover(&teams, "live_teams").contains_key(&name);
            let mut on_disk_too = false;
            let written = update_team_store(&store_path, &teams, |on_disk| {
                on_disk_too = on_disk.remove(&name).is_some();
                on_disk_too
            });
            if let Err(error) = written {
                // Not written: the team is still removed in this process, as before.
                if lock_recover(&teams, "live_teams").remove(&name).is_none() {
                    return Err(format!("team not found: {name}"));
                }
                return Err(error);
            }
            if !held_here && !on_disk_too {
                return Err(format!("team not found: {name}"));
            }
            Ok(format!("{name} removed from live directory"))
        })
    }
}

#[cfg(test)]
mod budget_slot_tests {
    //! Audit 2026-11-05 orchestration H1: an admitted slot must come back on
    //! every exit path, including a caller's timeout dropping the spawn future.
    use super::*;
    use std::sync::Arc;

    /// Round 2 M5: a child whose stream was lost is interrupted, and the
    /// caller waits for its turn to end before the slot is released.
    #[tokio::test]
    async fn lost_child_is_interrupted_and_awaited_before_release() {
        let turn = rightkit_agent_daemon::LiveTurnHandle::default();
        let guard = turn.begin();
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        let child = tokio::spawn(async move {
            // The child stops only once it sees the interrupt.
            rx.wait_for(|interrupted| *interrupted).await.unwrap();
            drop(guard);
        });
        assert!(
            interrupt_and_await_idle(Some(&tx), &turn, std::time::Duration::from_secs(10)).await
        );
        assert!(*tx.borrow(), "the interrupt was not sent");
        assert!(!turn.is_active());
        child.await.unwrap();
    }

    #[tokio::test]
    async fn a_child_ignoring_the_interrupt_is_given_up_on_after_the_grace() {
        let turn = rightkit_agent_daemon::LiveTurnHandle::default();
        let _guard = turn.begin();
        let (tx, _rx) = tokio::sync::watch::channel(false);
        assert!(
            !interrupt_and_await_idle(Some(&tx), &turn, std::time::Duration::from_millis(120))
                .await
        );
        assert!(*tx.borrow());
    }

    fn ledger(concurrent: u32) -> Arc<tokio::sync::Mutex<RunBudgetLedger>> {
        Arc::new(tokio::sync::Mutex::new(RunBudgetLedger::new(
            RunSubagentBudget {
                concurrent_max: concurrent,
                cumulative_max: 100,
            },
        )))
    }

    type Owner = tokio::sync::Mutex<RunBudgetLedger>;

    async fn admit(owner: &Arc<Owner>) -> SubagentBudgetSlot<Owner> {
        assert_eq!(owner.lock().await.try_admit(), BudgetDecision::Admit);
        SubagentBudgetSlot::admitted(owner.clone())
    }

    #[tokio::test]
    async fn drop_before_registration_rolls_back_both_counters() {
        let owner = ledger(1);
        drop(admit(&owner).await);
        let ledger = owner.lock().await;
        assert_eq!(ledger.concurrent_in_flight(), 0);
        assert_eq!(ledger.cumulative_used(), 0);
    }

    #[tokio::test]
    async fn drop_after_registration_releases_slot_and_interrupts_child() {
        let owner = ledger(1);
        let (tx, rx) = tokio::sync::watch::channel(false);
        let mut slot = admit(&owner).await;
        slot.registered("sub-1", Some(tx));
        drop(slot);
        assert!(*rx.borrow(), "dropped spawn must interrupt its child");
        let mut ledger = owner.lock().await;
        assert_eq!(ledger.concurrent_in_flight(), 0);
        assert_eq!(
            ledger.cumulative_used(),
            1,
            "cumulative holds after a spawn"
        );
        assert_eq!(ledger.try_admit(), BudgetDecision::Admit);
    }

    #[tokio::test]
    async fn complete_releases_once_without_interrupt() {
        let owner = ledger(2);
        let (tx, rx) = tokio::sync::watch::channel(false);
        let mut slot = admit(&owner).await;
        slot.registered("sub-1", Some(tx));
        slot.complete().await;
        drop(slot);
        assert!(!*rx.borrow(), "a settled child is not interrupted");
        assert_eq!(owner.lock().await.concurrent_in_flight(), 0);
    }

    #[tokio::test]
    async fn disarmed_slot_does_not_release_twice() {
        let owner = ledger(2);
        let _other = admit(&owner).await;
        let mut slot = admit(&owner).await;
        owner.lock().await.rollback_after_admit();
        slot.disarm();
        drop(slot);
        assert_eq!(owner.lock().await.concurrent_in_flight(), 1);
    }

    #[tokio::test]
    async fn drop_while_ledger_locked_releases_after_unlock() {
        let owner = ledger(1);
        let mut slot = admit(&owner).await;
        slot.registered("sub-1", None);
        {
            let _held = owner.lock().await;
            drop(slot);
        }
        // The deferred release runs on the runtime once the lock is free.
        for _ in 0..100 {
            if owner.lock().await.concurrent_in_flight() == 0 {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("deferred release never ran");
    }

    fn recording_persist() -> (RunBudgetPersist, Arc<std::sync::Mutex<Vec<u32>>>) {
        let writes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = writes.clone();
        let persist: RunBudgetPersist = Arc::new(move |cumulative| {
            let sink = sink.clone();
            Box::pin(async move {
                sink.lock().unwrap().push(cumulative);
            })
        });
        (persist, writes)
    }

    async fn wait_for_writes(writes: &Arc<std::sync::Mutex<Vec<u32>>>, len: usize) {
        for _ in 0..100 {
            if writes.lock().unwrap().len() >= len {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("persisted rollback never ran");
    }

    #[tokio::test]
    async fn drop_rollback_persists_cumulative_count() {
        // PR 459 follow-up: a drop-path rollback must also rewrite the
        // persisted `run_budget` row, or a restart restores the phantom spawn.
        let owner = ledger(2);
        let _earlier = admit(&owner).await;
        let (persist, writes) = recording_persist();
        let slot = admit(&owner).await.with_persistence(persist);
        drop(slot);
        wait_for_writes(&writes, 1).await;
        assert_eq!(*writes.lock().unwrap(), vec![1]);
        assert_eq!(owner.lock().await.cumulative_used(), 1);
    }

    #[tokio::test]
    async fn timed_out_admission_persists_rollback() {
        let owner = ledger(1);
        let (persist, writes) = recording_persist();
        let fut = {
            let owner = owner.clone();
            async move {
                let _slot = admit(&owner).await.with_persistence(persist);
                std::future::pending::<()>().await;
            }
        };
        let timed = tokio::time::timeout(std::time::Duration::from_millis(10), fut).await;
        assert!(timed.is_err());
        wait_for_writes(&writes, 1).await;
        assert_eq!(*writes.lock().unwrap(), vec![0]);
    }

    #[tokio::test]
    async fn registered_release_and_disarm_do_not_persist() {
        let owner = ledger(2);
        let (persist, writes) = recording_persist();
        let mut slot = admit(&owner).await.with_persistence(persist.clone());
        slot.registered("sub-1", None);
        drop(slot);
        let mut disarmed = admit(&owner).await.with_persistence(persist);
        owner.lock().await.rollback_after_admit();
        disarmed.disarm();
        drop(disarmed);
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert!(writes.lock().unwrap().is_empty());
        assert_eq!(owner.lock().await.cumulative_used(), 1);
    }

    fn cli_options() -> rightkit_agent_coding_tools::SubagentSpawnOptions {
        rightkit_agent_coding_tools::SubagentSpawnOptions {
            runtime: Some(
                rightkit_agent_coding_tools::SpawnRuntimeKind::AuthenticatedCli {
                    provider_id: "claude".to_string(),
                    model_id: None,
                    account_profile_id: "claude/default".to_string(),
                },
            ),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn cli_spawn_respects_the_depth_backstop() {
        // Audit 2026-11-05 orchestration M1: an authenticated-CLI spawn used
        // to skip the depth, tier and budget checks.
        let _ = rightkit_agent_coding_tools::process_resource_authority::install_process_resource_profile(
            rightkit_agent_coding_tools::process_resource_authority::ResourceProfile::Expanded,
        );
        let state = DaemonState::new("test-token");
        let workspace = std::env::temp_dir().to_string_lossy().to_string();
        let mut parent: Option<String> = None;
        for level in 0..MAX_SUBAGENT_DEPTH {
            let id = format!("m1-depth-{level}");
            state.create_session(&id, None, &workspace, "depth", parent.as_deref());
            parent = Some(id);
        }
        let error = state
            .spawn_with_failure_kind(
                "coder",
                "task",
                parent.as_deref().unwrap(),
                cli_options(),
                SubagentFailureKindSlot::default(),
            )
            .await
            .unwrap_err();
        assert!(error.contains("nesting limit"), "{error}");
    }

    #[tokio::test]
    async fn cli_spawn_respects_the_run_budget_and_rolls_back() {
        let _ = rightkit_agent_coding_tools::process_resource_authority::install_process_resource_profile(
            rightkit_agent_coding_tools::process_resource_authority::ResourceProfile::Expanded,
        );
        let state = DaemonState::new("test-token");
        let workspace = std::env::temp_dir().to_string_lossy().to_string();
        let parent = state.create_session("m1-budget", None, &workspace, "budget", None);
        // No CLI executor is wired: an admitted spawn fails after admission
        // and must hand both counters back.
        let error = state
            .spawn_with_failure_kind(
                "coder",
                "task",
                "m1-budget",
                cli_options(),
                SubagentFailureKindSlot::default(),
            )
            .await
            .unwrap_err();
        assert!(error.contains("unavailable"), "{error}");
        {
            let ledger = parent.run_budget.lock().await;
            assert_eq!(ledger.concurrent_in_flight(), 0);
            assert_eq!(ledger.cumulative_used(), 0);
        }
        // Fill the concurrent limit; the next CLI spawn is refused by budget.
        {
            let budget = effective_run_budget(&state, "m1-budget");
            let mut ledger = parent.run_budget.lock().await;
            ledger.configure(budget, None);
            while ledger.try_admit() == BudgetDecision::Admit {}
        }
        let error = state
            .spawn_with_failure_kind(
                "coder",
                "task",
                "m1-budget",
                cli_options(),
                SubagentFailureKindSlot::default(),
            )
            .await
            .unwrap_err();
        assert!(error.contains("budget exhausted"), "{error}");
    }

    #[test]
    fn drop_outside_runtime_still_persists_rollback() {
        let owner = ledger(1);
        let (persist, writes) = recording_persist();
        assert_eq!(owner.blocking_lock().try_admit(), BudgetDecision::Admit);
        drop(SubagentBudgetSlot::admitted(owner.clone()).with_persistence(persist));
        assert_eq!(*writes.lock().unwrap(), vec![0]);
        assert_eq!(owner.blocking_lock().concurrent_in_flight(), 0);
    }

    #[tokio::test]
    async fn timed_out_spawn_future_frees_its_slot() {
        // Mirrors `tokio::time::timeout(short, spawn(..))` in the conductor:
        // the future is dropped mid-wait and the slot must still come back.
        let owner = ledger(1);
        let (tx, rx) = tokio::sync::watch::channel(false);
        let fut = {
            let owner = owner.clone();
            async move {
                let mut slot = admit(&owner).await;
                slot.registered("sub-1", Some(tx));
                std::future::pending::<()>().await;
                slot.complete().await;
            }
        };
        let timed = tokio::time::timeout(std::time::Duration::from_millis(10), fut).await;
        assert!(timed.is_err());
        assert!(*rx.borrow());
        assert_eq!(owner.lock().await.try_admit(), BudgetDecision::Admit);
    }
}

#[cfg(test)]
mod run_tree_budget_tests {
    use super::*;
    use std::collections::HashMap;

    fn parents(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(child, parent)| (child.to_string(), parent.to_string()))
            .collect()
    }

    #[test]
    fn tree_root_walks_to_the_top_ancestor() {
        let map = parents(&[("c", "b"), ("b", "a")]);
        let parent_of = |id: &str| map.get(id).cloned();
        assert_eq!(subagent_tree_root_with("c", parent_of), "a");
        assert_eq!(subagent_tree_root_with("a", parent_of), "a");
    }

    #[test]
    fn tree_root_terminates_on_a_parent_cycle() {
        let map = parents(&[("a", "b"), ("b", "a")]);
        let root = subagent_tree_root_with("a", |id: &str| map.get(id).cloned());
        assert!(root == "a" || root == "b");
    }

    #[test]
    fn run_tree_cap_is_linear_in_depth() {
        assert_eq!(run_tree_concurrent_cap(4), 4 * MAX_SUBAGENT_DEPTH as u32);
        assert_eq!(run_tree_concurrent_cap(u32::MAX), u32::MAX);
    }

    #[test]
    fn run_tree_verdict_refuses_at_the_cap() {
        let cap = run_tree_concurrent_cap(4);
        assert!(run_tree_capacity_verdict(cap - 1, cap, "root", "auditor", "p").is_ok());
        let error = run_tree_capacity_verdict(cap, cap, "root", "auditor", "p").unwrap_err();
        assert!(
            error.contains("run-tree concurrent budget exhausted"),
            "{error}"
        );
        assert!(error.contains("auditor"), "{error}");
    }
}
