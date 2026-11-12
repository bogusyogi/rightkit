pub use rightkit_agent_daemon::{DaemonSettingsStore, EngineConfig, LiveTurnHandle};

/// Bounded engine mailbox capacity for `LiveSession::msg_tx`. A route update
/// treats `capacity() < SESSION_MSG_CHANNEL_CAPACITY` as queued user work.
pub(crate) const SESSION_MSG_CHANNEL_CAPACITY: usize = 32;

pub struct LiveSession {
    pub compaction_snapshot_feed:
        Arc<std::sync::RwLock<rightkit_agent_engine_core::CompactionSnapshotV1>>,
    pub id: String,
    pub title: Arc<Mutex<String>>,
    pub model: Arc<Mutex<String>>,
    pub model_route: Option<crate::provider_client::SharedProviderRoute>,
    /// Identity only: resolved route secrets and headers never enter session state.
    pub inference_selection: Arc<Mutex<Option<crate::inference_runtime::InferenceSelection>>>,
    pub inference_fingerprint: Arc<Mutex<Option<String>>>,
    /// Opaque update seam into the live provider client. Sender state never
    /// exposes queued credentials to session serialization or debug output.
    pub inference_route_tx:
        Option<tokio::sync::mpsc::Sender<crate::inference_runtime::ResolvedInferenceRoute>>,
    pub inference_active_requests: Arc<std::sync::atomic::AtomicUsize>,
    /// Resolved provider identity for the session route. For qualified
    /// sessions this mirrors `inference_selection.connection_id`; for
    /// dynamic sessions it is the provider `ProviderRoute` was resolved
    /// against. Persisted as `providerId` so resume/fork reproduce the
    /// same provider instead of re-deriving it from defaults.
    /// `None` while a label cannot be determined (e.g. no route yet).
    pub provider_id: Arc<Mutex<Option<String>>>,
    /// Serializes route updates against prompt admission. A route update
    /// holding this gate cannot interleave with a `msg_tx` try_send, so a
    /// committed switch applies before the next turn can be admitted.
    /// `LockFailure` events do not take the gate: they never reach the
    /// engine's provider stream and need no route ordering.
    pub route_update_gate: Arc<tokio::sync::Mutex<()>>,
    /// User turns handed to the engine channel but not yet begun. Route
    /// updates treat queued work as busy so a switch never lands between
    /// admission and the first provider request of that turn.
    pub pending_admissions: Arc<std::sync::atomic::AtomicUsize>,
    pub workspace_root: String,
    pub role: Option<String>,
    pub parent_session_id: Option<String>,
    pub forked_from_session_id: Option<String>,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub toolset: Option<String>,
    pub active_skills: Vec<String>,
    pub execution_backend: Option<String>,
    pub execution_workspace_id: Option<String>,
    pub execution_workspace_path: Option<PathBuf>,
    pub execution_network_profile: Option<String>,
    /// Mutable because the composer can repoint a session's conductor while it
    /// is live. Held like `title`/`model` rather than as a plain String so a
    /// switch takes effect on the next send instead of only after a restart —
    /// a stale value here makes the capability lease 409 against the kind the
    /// user just chose.
    pub driver_kind: Arc<Mutex<String>>,
    pub mcp_channel: String,
    pub spec_id: Arc<Mutex<Option<String>>>,
    pub workspace_trust: rightkit_agent_policy::WorkspaceTrust,
    /// Requested reasoning effort. Mutable because PATCH can re-route a live
    /// session's effort; projections and forks must read the updated value,
    /// not the create-time request.
    pub effort: Arc<Mutex<Option<String>>>,
    pub thinking_budget_tokens: Option<u32>,
    pub disable_thinking: bool,
    pub safe_mode: bool,
    /// Explicit per-session cost ceiling supplied by the creating authority.
    /// Child sessions inherit this as a ceiling so delegation cannot expand a
    /// trusted orchestrator's budget.
    pub budget_ceiling_usd: Option<f64>,
    /// Atomic remaining-budget ledger shared by the complete delegation tree.
    pub lineage_budget: Arc<rightkit_agent_engine_core::LineageBudget>,
    /// Per-run sub-agent budget ledger (Lane C3, task 2). Tracks concurrent
    /// and cumulative sub-agent counts and refuses spawns that would exceed
    /// either cap. Wrapped in a `tokio::sync::Mutex` so the daemon's spawn
    /// gate can `try_admit` synchronously without holding the daemon-wide
    /// sessions lock.
    pub run_budget: tokio::sync::Mutex<RunBudgetLedger>,
    /// Per-run governed completion arbiter (Lane C3, task 3). Holds the
    /// unbacked-work attempt counter for this run so a victory claim
    /// without typed mutations is refused after the policy's
    /// `max_unbacked_reenter` cap. Reset between goals so a new attempt
    /// starts fresh.
    pub governed_arbiter:
        tokio::sync::Mutex<rightkit_agent_orchestration::coding::run_governance::GovernedArbiter>,
    pub created_at: String,
    pub updated_at: Arc<Mutex<String>>,
    pub conduit: Arc<Conduit>,
    /// Send a user message to the engine task. `None` when no real provider is configured.
    pub msg_tx: Option<tokio::sync::mpsc::Sender<String>>,
    /// Live turn ownership. This is the shutdown gate's authoritative busy
    /// signal; persisted status is repaired from it after crashes.
    pub live_turn_handle: LiveTurnHandle,
    /// Submit a vendor-driver tool call to the live engine's governed dispatch
    /// path. This is the only supported execution seam for session capability
    /// gateways; callers receive the typed post-effect result.
    pub driver_tool_tx: Option<tokio::sync::mpsc::Sender<DriverToolRequest>>,
    /// Request authoritative compaction from the live engine. Manual requests
    /// use the same operation as the automatic capacity trigger.
    pub compact_tx:
        Option<tokio::sync::mpsc::Sender<rightkit_agent_engine_core::CompactionCommandV1>>,
    /// Request a structural, recoverable fold from the live engine task.
    pub context_fold_tx:
        Option<tokio::sync::mpsc::Sender<crate::api_context_folds::ContextFoldCommand>>,
    /// Live active-model compaction capability sent to the engine on model changes.
    /// Unknown models remain explicit and disable deterministic auto-compaction.
    pub compaction_policy_tx:
        Option<tokio::sync::watch::Sender<rightkit_agent_engine_core::CompactionModelPolicy>>,
    /// Shared compaction revision wake callback; execution remains serialized
    /// by the engine task's existing select loop.
    pub compaction_revision_notifier: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Set to `true` to interrupt the engine at its next iteration boundary.
    pub interrupt_tx: Option<tokio::sync::watch::Sender<bool>>,
    /// Live permission-mode control; the engine adopts the latest value each turn.
    pub permission_mode_tx:
        Option<tokio::sync::watch::Sender<rightkit_agent_policy::PermissionMode>>,
    /// Live session-scoped permission rules shared with the engine.
    pub permission_rules: Arc<Mutex<rightkit_agent_policy::RuleStore>>,
    /// Pending tool-approval oneshots keyed by tool_call_id.
    pub pending_approvals: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>>,
    pub pending_approval_contexts:
        Arc<Mutex<HashMap<String, rightkit_agent_engine_protocol::PendingApprovalContext>>>,
    /// Pending provider-credential responses, single-use and keyed by exact tool call.
    pub pending_credential_requests:
        Arc<Mutex<rightkit_agent_engine_core::credential_request::PendingCredentialRequests>>,
    /// In-flight tool cancellation tokens keyed by tool_call_id.
    /// Setting the sender to `true` signals the tool to abort.
    pub cancel_tokens: Arc<Mutex<HashMap<String, tokio::sync::watch::Sender<bool>>>>,
    /// Pending `ask_user` question oneshots keyed by question_id; the answer
    /// payload is delivered when the operator responds.
    pub pending_questions:
        Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<serde_json::Value>>>>,
    /// Current caller's sandbox grant UI; headless turns keep typed denials.
    pub sandbox_grant_prompts: Arc<std::sync::atomic::AtomicBool>,
    /// Pending MCP elicitation responses, keyed by server + JSON-RPC request id.
    pub pending_elicitations: Arc<Mutex<HashMap<String, PendingElicitation>>>,
    /// Pending `exit_plan_mode` approval oneshots keyed by plan_id.
    pub pending_plans: crate::plan_exit::PendingPlans,
    /// Running transcript built from conduit events + submitted user messages.
    pub transcript: Arc<Mutex<Vec<TranscriptMessage>>>,
    /// Durable event-log acknowledgement observed by request handlers.
    pub persistence_state: rightkit_agent_engine_storage::EventPersistenceState,
    /// Latest task graph snapshot from TaskGraphUpdated events.
    pub tasks: Arc<Mutex<Vec<rightkit_agent_engine_protocol::TaskNode>>>,
    /// Live background shells owned by this daemon session.
    pub background_shells: rightkit_agent_tools::coding_tools::background_shell::BgStore,
    /// Host-composed process adapter exposed to execution handlers. Process
    /// lifecycle/schema ownership remains with the process subsystem.
    pub process_launch_adapter: Option<Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter>>,
    /// Session-bound policy context paired with `process_launch_adapter`.
    pub process_launch_context: rightkit_sandbox::execution::ProcessLaunchContext,
    /// Live browser sessions shared by agent tools and the visible Browser pane.
    pub browser_sessions: rightkit_agent_tools::coding_tools::browser::BrowserStore,
    /// Artifacts produced during a conductor run (worker handoffs, the
    /// goal_verification record). Surfaced by GET /runtime/artifacts.
    pub artifacts: Arc<Mutex<Vec<rightkit_agent_engine_protocol::ArtifactRow>>>,
    /// Optional durable session store used to persist artifact trust labels for
    /// replay after the live session is gone.
    pub storage: Option<SharedStorage>,
    /// Inject a message into the running engine at the next iteration boundary.
    pub steer_tx: Option<tokio::sync::mpsc::Sender<String>>,
    /// Queue already-rendered host state for the next provider-turn boundary.
    /// Producers may send while a provider stream is active; the engine never
    /// drains this channel mid-request.
    pub provider_boundary_tx:
        Option<rightkit_agent_engine_core::provider_turn_boundary::ProviderBoundarySender>,
    pub tool_catalog: Arc<crate::session_tool_catalog::SessionToolCatalog>,
    /// Persistent goal text shared with the engine (injected before every LLM call).
    pub goal: Arc<Mutex<Option<String>>>,
    /// Budget-bounded goal state (F2). Shared with the engine; the daemon writes
    /// `tokens_used` from the rollup and sets/clears the inner goal.
    pub goal_state: Arc<Mutex<Option<rightkit_agent_engine_core::GoalState>>>,
    /// Per-goal normalized unresolved-verifier-gap streak. This is session
    /// runtime state, reset whenever the operator replaces or clears the goal.
    pub goal_gap_tracker: Arc<Mutex<rightkit_agent_engine_core::GoalGapTracker>>,
    /// Send a rewind command; the engine task responds with the removed message text.
    pub(crate) rewind_tx: tokio::sync::mpsc::Sender<crate::api_sessions::rewind::RewindRequest>,
    /// Hook bus for this session's lifecycle events.
    pub hook_bus: rightkit_hooks::agent_hooks::HookBus,
    /// Per-sub-agent roster (F1 agent rail) accumulated from child session
    /// events; also the source of the `SessionUsageRollup` budget meter.
    pub agent_roster: Mutex<crate::agent_roster::AgentRoster>,
    /// This session's own cumulative cost, updated from its `UsageUpdated`
    /// events. `None` until the first usage event arrives, or permanently
    /// when the active model's pricing is unknown — unknown cost must never
    /// be conflated with zero. Surfaced as `costUsd` on the session row and
    /// `{app} agents --json`.
    pub cost_usd: Arc<Mutex<Option<f64>>>,
    /// Shared counter the engine bumps on every successful typed
    /// `WorkspaceMutation`. Read by `finalize_goal` so the unbacked-victory
    /// gate sees the real count from the engine task, not a hardcoded zero.
    /// Reset on session creation; persists across turns for the session's
    /// lifetime.
    pub typed_mutations: Arc<std::sync::atomic::AtomicU32>,
    /// FM-188 itemized per-task evidence keyed by stable task/job identity.
    /// Typed mutation counts are attributed per `TaskNode.id` (or job id).
    /// `finalize_goal` builds `GovernedCompletionInput::task_evidence` from
    /// this map + the live task graph, so the gate refuses only tasks
    /// lacking evidence.
    pub task_typed_evidence: Arc<Mutex<std::collections::HashMap<String, u32>>>,
    /// Optional mapping from stable task id to stable job id. When a task is
    /// dispatched as a job, this records the job identity so the gate can
    /// key on the durable job id across retries.
    pub task_job_ids: Arc<Mutex<std::collections::HashMap<String, String>>>,
    /// Cached completion-evidence flags written by the conductor's jury
    /// arbitration before `finalize_goal` reads them. `verification_present`
    /// is true iff a goal_verification artifact exists; `verification_approved`
    /// is true iff the auto-jury approved it. `finalize_goal` builds the
    /// `CompletionInput` from these instead of a hardcoded `true`.
    pub completion_evidence: Arc<Mutex<CompletionEvidence>>,
}

pub struct PendingElicitation {
    pub response: tokio::sync::oneshot::Sender<serde_json::Value>,
    pub requested_schema: Option<serde_json::Value>,
}

pub struct PendingElicitationCleanup {
    pending: Arc<Mutex<HashMap<String, PendingElicitation>>>,
    key: String,
}

impl PendingElicitationCleanup {
    pub fn new(pending: Arc<Mutex<HashMap<String, PendingElicitation>>>, key: String) -> Self {
        Self { pending, key }
    }
}

impl Drop for PendingElicitationCleanup {
    fn drop(&mut self) {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.key);
    }
}

impl LiveSession {
    /// Internal producers serialize with the same route authority as user admission.
    pub(crate) async fn send_route_bound_text(&self, text: String) -> Result<(), String> {
        let tx = self.msg_tx.as_ref().ok_or("session has no live engine")?;
        // Reserve before taking the gate: a full queue must not block a running
        // engine's next provider request while this sender waits for capacity.
        let permit = tx
            .reserve()
            .await
            .map_err(|_| "session provider channel is closed")?;
        let _gate = self.route_update_gate.lock().await;
        self.pending_admissions
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        permit.send(text);
        Ok(())
    }

    pub(crate) async fn try_admit_route_bound_text(&self, text: String) -> Result<(), String> {
        let _gate = self.route_update_gate.lock().await;
        let tx = self.msg_tx.as_ref().ok_or("session has no live engine")?;
        self.pending_admissions
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        if let Err(error) = tx.try_send(text) {
            self.pending_admissions
                .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
            return Err(error.to_string());
        }
        Ok(())
    }

    /// Publish host-rendered state for promotion at next provider boundary.
    /// A single monotonic sequence keeps each category deterministic while
    /// provider requests already in flight retain their captured inputs.
    pub fn publish_boundary_text(&self, kind: &str, content: impl Into<String>) {
        static REVISION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let Some(sender) = self.provider_boundary_tx.as_ref() else {
            return;
        };
        let update = rightkit_agent_engine_core::provider_turn_boundary::BoundaryTextUpdate::new(
            REVISION.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1,
            content,
        );
        let update = match kind {
            "prompt" => {
                rightkit_agent_engine_core::provider_turn_boundary::ProviderBoundaryUpdate::Prompt(update)
            }
            "world" => {
                rightkit_agent_engine_core::provider_turn_boundary::ProviderBoundaryUpdate::WorldState(update)
            }
            _ => rightkit_agent_engine_core::provider_turn_boundary::ProviderBoundaryUpdate::Context(update),
        };
        let _ = sender.send_latest(update);
    }

    /// Atomically publish a replacement MCP manager with its visible tool
    /// catalog. The engine adopts both only at its next provider boundary.
    pub fn publish_mcp_manager(
        &self,
        manager: Arc<rightkit_mcp::agent_mcp::McpManager>,
        tools: Arc<rightkit_agent_tools::coding_tools::ToolRegistry>,
    ) {
        static REVISION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let Some(sender) = self.provider_boundary_tx.as_ref() else {
            return;
        };
        let revision = REVISION.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let _ = sender.send_latest(
            rightkit_agent_engine_core::provider_turn_boundary::ProviderBoundaryUpdate::McpManager {
                revision,
                manager,
                tools,
            },
        );
    }
}

pub struct DriverToolRequest {
    pub call: rightkit_agent_engine_protocol::ToolCall,
    pub permission_only: bool,
    pub response: tokio::sync::oneshot::Sender<rightkit_agent_tools::coding_tools::ToolResult>,
}

/// Cached completion-evidence flags consumed by `finalize_goal` to build
/// the `CompletionInput`. Set by the conductor's jury arbitration.
#[derive(Debug, Clone, Copy, Default)]
pub struct CompletionEvidence {
    pub verification_present: bool,
    pub verification_approved: bool,
}

// ── ask_user questioner ─────────────────────────────────────────────────────

static QUESTION_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Bridges the `ask_user` tool to the operator: emits `UserQuestionAsked` on the
/// session conduit, then blocks on a oneshot until the answer route resolves it.
struct DaemonQuestioner {
    session_id: String,
    conduit: Arc<Conduit>,
    store: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<serde_json::Value>>>>,
    sandbox_grant_prompts: Arc<std::sync::atomic::AtomicBool>,
}

impl rightkit_agent_tools::coding_tools::UserQuestioner for DaemonQuestioner {
    fn supports_sandbox_grants(&self) -> bool {
        self.sandbox_grant_prompts
            .load(std::sync::atomic::Ordering::Acquire)
    }

    fn ask(
        &self,
        questions: serde_json::Value,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<serde_json::Value, String>> + Send>,
    > {
        let session_id = self.session_id.clone();
        let conduit = self.conduit.clone();
        let store = self.store.clone();
        Box::pin(async move {
            let parsed: Vec<rightkit_agent_engine_protocol::UserQuestion> =
                serde_json::from_value(questions).map_err(|e| format!("invalid questions: {e}"))?;
            let qid = format!(
                "q-{}-{}",
                now_millis(),
                QUESTION_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            );
            let (tx, rx) = tokio::sync::oneshot::channel::<serde_json::Value>();
            lock_recover(&store, "session.pending_questions").insert(qid.clone(), tx);

            let seq = conduit.next_seq();
            conduit.publish(ProtocolEvent::UserQuestionAsked {
                session_id: session_id.clone(),
                seq,
                created_at: now_iso(),
                question_id: qid.clone(),
                questions: parsed,
            });

            // Human interaction is live, but never unbounded: a disconnected
            // client must release the pending request instead of pinning a
            // worker forever. The keyed answer route remains the sole way to
            // resolve a question before this deadline.
            let answer =
                match tokio::time::timeout(std::time::Duration::from_secs(30 * 60), rx).await {
                    Ok(result) => result.map_err(|_| "question was cancelled".to_string())?,
                    Err(_) => {
                        lock_recover(&store, "session.pending_questions").remove(&qid);
                        return Err("question timed out awaiting operator response".to_string());
                    }
                };

            let seq2 = conduit.next_seq();
            conduit.publish(ProtocolEvent::UserQuestionAnswered {
                session_id,
                seq: seq2,
                created_at: now_iso(),
                question_id: qid,
            });
            Ok(answer)
        })
    }
}

// ── Daemon state ──────────────────────────────────────────────────────────────

/// Per session id: the plugins unloaded for it and how many registrations that removed.
pub type SessionPluginUnloads = Arc<Mutex<std::collections::HashMap<String, (Vec<String>, usize)>>>;

#[derive(Clone)]
pub struct DaemonState {
    /// Explicit context admission policy. Production defaults to Membrane.
    pub context_mode: crate::cli::ContextMode,
    /// Set when Membrane context was unavailable and turns run visibly degraded.
    pub context_degraded: Option<String>,
    /// PL-009 durable observation loop shared by daemon callers.
    pub external_observations:
        Arc<Mutex<crate::daemon::external_observations::ExternalObservationLoop>>,
    /// VG-003: daemon-wide `fs/watch` manager. One debounced `FileWatcher`
    /// core backs every client's watches, matching the donor's
    /// one-watcher-per-process shape.
    pub fs_watch: crate::api_fs_watch::FsWatchManager,
    /// FM-074/FM-091 dream-cycle status projection (A11): last consolidation
    /// run's outcome, surfaced via `GET /memory/consolidation-status`.
    pub memory_consolidation_status:
        rightkit_memory_client::consolidation_status::ConsolidationStatusState,
    pub sessions: Arc<Mutex<HashMap<String, Arc<LiveSession>>>>,
    /// Shared authority for root-controller handoffs. Intake paths consult
    /// this controller before accepting work for a session being transferred.
    pub handoff_controller: Arc<rightkit_agent_tools::coding_tools::handoff::HandoffController>,
    /// Daemon-lifetime canonical MCP transport pool (master-change 3.4).
    /// Sessions loading the same server share one transport (one process +
    /// N leases) instead of spawning per-session copies.
    pub mcp_pool: Arc<rightkit_mcp::agent_mcp::McpTransportPool>,
    /// Vendor reasoning-process supervisor. Execution authority remains in
    /// the live session gateway; this owns only process lifecycle and wire I/O.
    pub(crate) driver_host: crate::driver_host_runtime::DriverHostRuntime,
    /// Live adapter/transport registry. This is retained alongside the executor
    /// so discovery, login status, attestation, & runtime dispatch consult one
    /// source of truth rather than a copied provider list.
    pub cli_registry: Arc<rightkit_agents::driver_host::cli_runtime::CliAdapterRegistry>,
    /// Shared generic-inference admission gate. Vendor CLI transports remain
    /// outside this gate until post-parity cleanup; Direct/Omni callers use it.
    pub(crate) inference_scheduler: crate::inference_runtime::InferenceScheduler,
    /// Authenticated-CLI child agent executor. PR 5: when a
    /// `SubagentSpawner::spawn` call arrives with
    /// `SubagentSpawnOptions::runtime = Some(AuthenticatedCli { ... })`,
    /// the spawn helper drives this executor instead of the legacy
    /// native path. `None` while the daemon is still booting or in
    /// tests that do not wire the bridge.
    pub cli_executor:
        Option<Arc<rightkit_agents::driver_host::cli_runtime::AuthenticatedCliExecutor>>,
    pub(crate) cli_bridge: Option<Arc<crate::driver_host_runtime::DaemonCliDriverBridge>>,
    pub(crate) qualification_bindings: Arc<std::sync::RwLock<HashSet<String>>>,
    /// Binding-scoped plaintext launch material. Secrets live only in memory;
    /// storage retains lease hashes and non-secret binding metadata.
    pub(crate) cli_launch_contexts: Option<Arc<crate::api::api_state::StoredCliLaunchContexts>>,
    /// Serializes binding + lease + process authority transitions. This closes
    /// start/open/resume races where a losing request could invalidate a live
    /// process instance or rotate its lease underneath it.
    pub(crate) driver_authority_lock: Arc<tokio::sync::Mutex<()>>,
    /// Snapshot of the provider ids the `CliAdapterRegistry` knows
    /// about. Re-read on every spawn so adapters registered after the
    /// daemon came up (hot reload, PR 3 attestation updates) take
    /// effect without a restart.
    pub cli_registry_provider_ids:
        std::collections::BTreeSet<rightkit_agents::driver_host::cli_runtime::CliProviderId>,
    /// Session-scoped delegation policy that decides whether a
    /// requested CLI runtime is honoured. Stored on the daemon
    /// rather than the session because PR 5 ships a single global
    /// default; per-session overrides land in PR 12 (desktop UI).
    pub cli_delegation_policy: rightkit_agents::driver_host::cli_runtime::CliDelegationPolicy,
    pub cli_delegation_policies:
        Arc<Mutex<HashMap<String, rightkit_agents::driver_host::cli_runtime::CliDelegationPolicy>>>,
    /// Root-session id to live shared budget ledger. Weak entries prevent this
    /// runtime index from retaining dormant lineages; resume reconstructs spent
    /// state from persisted usage events when no live member remains.
    pub lineage_budgets:
        Arc<Mutex<HashMap<String, std::sync::Weak<rightkit_agent_engine_core::LineageBudget>>>>,
    /// Session ids explicitly deleted by the operator. Event/session persisters
    /// consult this set while holding the same mutex through their storage write,
    /// preventing an interrupted background agent from resurrecting deleted rows.
    pub deleted_session_ids: Arc<Mutex<HashSet<String>>>,
    #[allow(dead_code)]
    pub token: String,
    pub engine_cfg: Option<EngineConfig>,
    /// Fixed Pro feature set. Empty; the licence gate below decides.
    pub licensed_features: Arc<Mutex<HashSet<String>>>,
    /// The daemon's own verification of the stored licence record (`crate::license`). When set it
    /// is the only source of Pro features; absent means no licence locators were given (Free).
    pub license_gate: Option<Arc<crate::license::LicenseGate>>,
    pub agent_registry: Arc<AgentRegistry>,
    pub settings: Arc<Mutex<DaemonSettingsStore>>,
    /// The actual TCP port the daemon bound to (0 until `with_runtime_info`).
    pub bound_port: u16,
    /// Absolute path to the SQLite database backing this daemon.
    pub database_path: String,
    /// Self-evaluating harness: per-role performance + learning proposals.
    pub learning: crate::learning::LearningHarness,
    /// Approved taste/memory instincts injected into session system prompts.
    pub instincts: Arc<Mutex<rightkit_agent_learning::InstinctStore>>,
    /// Named SkillOpt-style optimization runs and candidate skill bundles.
    pub skillopt_runs: Arc<Mutex<rightkit_agent_learning::SkillOptimizationStore>>,
    /// Durable SkillOpt run ledger path.
    pub skillopt_store_path: Option<PathBuf>,
    /// One sticky, startup-selected memory authority shared by routes, tools,
    /// session retrieval, & compaction.
    pub memory_backend: Arc<dyn rightkit_memory_client::backend::MemoryBackend>,
    /// Pulse Membrane CLI adapter (`{app}-membrane-cli`); `Unavailable` when Pulse is not
    /// installed. Every call is bounded by the adapter timeout.
    pub membrane: Arc<dyn rightkit_memory_client::membrane::MembraneBackend + Send + Sync>,
    /// Lane-C native session-transcript exporter on the same authenticated
    /// binding. `None` when the installation identity recorded no usable
    /// service claim at bind time — durable events simply remain RightKit-
    /// local rather than reaching any fallback transport.
    pub transcript_exporter:
        Option<Arc<crate::native_transcript_exporter::NativeTranscriptExporter>>,
    /// Optional only while Hub Diagnostics binding is unavailable; engine
    /// fails closed at its pre-effect mutation seam when this is absent.
    pub mutation_diagnostics: Option<Arc<dyn rightkit_agent_engine_core::MutationDiagnostics>>,
    /// RightKit-owned causal labels paired with the pinned MemRight content store.
    pub(crate) memory_provenance: rightkit_memory_client::memory_provenance::MemoryProvenanceStore,
    /// Write-once, hash-chained governance audit log.
    pub audit: crate::audit::AuditLog,
    /// Observation layer: per-agent execution records feeding TasteScore.
    pub observations: crate::observation::ObservationStore,
    /// Active staged model rollouts (change governance).
    pub rollouts: crate::rollout_store::RolloutStore,
    /// Session/event store, when persistence is available. Used to purge a
    /// session's persisted rows on delete.
    pub storage: Option<SharedStorage>,
    /// Live process-generation table for the CLI runtime recovery flow
    /// (PR 13). One entry per binding; advanced when the supervisor
    /// restarts a vendor process.
    pub cli_recovery_live_generations: Arc<crate::cli_recovery_runtime::LiveGenerationTable>,
    /// Live RightKit context epoch for the CLI runtime recovery flow
    /// (PR 13). Bumped when workspace trust changes, the user
    /// re-authenticates, the model changes, or the policy changes.
    pub cli_recovery_live_epoch: Arc<crate::cli_recovery_runtime::LiveContextEpochState>,
    /// Deferred active-binding update queue for the CLI runtime recovery
    /// flow (PR 13). The applier drains this only at the turn boundary
    /// so the `idx_one_active_binding_per_agent` UNIQUE invariant is
    /// preserved.
    pub cli_recovery_deferred_updates:
        Arc<rightkit_agents::driver_host::cli_runtime::DeferredBindingUpdates>,
    /// Pending blind model comparisons, keyed by comparison id.
    pub comparisons:
        Arc<Mutex<HashMap<String, rightkit_agent_orchestration::coding::BlindComparison>>>,
    /// Captured high-quality run trajectories for fine-tuning export.
    pub trajectories: crate::trajectory::TrajectoryStore,
    /// Directory containing installed plugin folders with `plugin.json` manifests.
    pub plugin_dir: PathBuf,
    /// Live provider-extension registry owned by daemon state. Extensions
    /// remain provenance-bearing and untrusted until host policy grants trust.
    pub provider_extensions: Arc<Mutex<rightkit_llm::agent_providers::ExtensionProviderRegistry>>,
    /// File-backed marketplace catalog used for plugin discovery/install.
    pub plugin_marketplace_path: PathBuf,
    /// Local skill directories scanned by `/skills` and aligned with `invoke_skill`.
    pub skill_dirs: Vec<PathBuf>,
    /// User-level MCP config path managed by `/mcp` routes.
    pub mcp_user_config_path: PathBuf,
    /// Live team definitions created by agent/team tools.
    pub live_teams: Arc<Mutex<HashMap<String, rightkit_agent_tools::coding_tools::TeamDefinition>>>,
    /// Durable user-level team-definition index.
    pub team_store_path: PathBuf,
    /// Durable inter-session message ledger.
    pub message_history: Arc<Mutex<Vec<MessageHistoryRow>>>,
    pub message_store_path: PathBuf,
    /// Durable user/project permission rules inherited by new live sessions.
    pub permission_rule_store: Arc<Mutex<PersistentPermissionRuleStore>>,
    pub permission_rule_store_path: PathBuf,
    /// Durable recent permission-denial ledger.
    pub permission_denials: Arc<Mutex<Vec<PermissionDenialRow>>>,
    pub permission_denial_store_path: PathBuf,
    /// In-memory denial ledger (FM-030) — every `PermissionVerdict::Deny`
    /// records tool/reason/session/timestamp synchronously. Queried via
    /// `GET /diagnostics/denials` and included redacted in support bundles.
    pub denial_tracker: Arc<Mutex<rightkit_agent_policy::DenialTracker>>,
    /// Durable per-project trust choices used by new session policy.
    pub workspace_trust_store: Arc<Mutex<WorkspaceTrustStore>>,
    pub workspace_trust_store_path: PathBuf,
    /// Durable execution-journal directory used to detect interrupted turns.
    pub journal_dir: Option<PathBuf>,
    /// Operator-visible runtime middleware registry and local span store.
    pub runtime_middleware:
        Arc<Mutex<rightkit_agent_engine_runtime_middleware::RuntimeMiddlewareRegistry>>,
    /// Operator-requested cancellation facts for tool invocations. This remains
    /// separate from approval resolution so cancellation is immediately auditable.
    pub tool_invocation_cancellations:
        Arc<Mutex<Vec<crate::api_tool_invocations::ToolInvocationCancellation>>>,
    /// Secret-redacted remote/Telegram startup configuration for status UI.
    pub telegram_status: TelegramRemoteStatus,
    /// Explicit spec-mode artifact store. Normal sessions do not consult it
    /// unless the operator chooses to create or bind a spec.
    pub specs: Arc<Mutex<rightkit_evidence::SpecStore>>,
    /// Durable file backing specs, tasks, and spec analysis runs when configured.
    pub spec_store_path: Option<PathBuf>,
    /// Named execution backend registry. Tool routing still uses the existing
    /// local execution path until backend-aware execution is enabled.
    pub execution_backends: Arc<Mutex<rightkit_sandbox::execution::ExecutionBackendRegistry>>,
    /// Installed recipes and local run records.
    pub recipes: Arc<Mutex<RecipeStore>>,
    /// Durable file backing recipes and local run records when configured.
    pub recipe_store_path: Option<PathBuf>,
    /// Client-presence lifecycle (U-F): attached app/CLI leases, the 409 rule and the exit rule,
    /// owned by `rightkit_daemon::loopback::Presence`. A daemon no client ever attached to (bare
    /// dev/test) never self-exits.
    pub presence: Arc<rightkit_daemon::loopback::Presence>,
    /// Resolved workflow feature gate (ADR-0021 / WP-09). Mirrors
    /// `HostConfig.features.workflows_enabled`. The router mount in
    /// [`api_router::build_router`] consults this to decide whether to merge
    /// the workflow surface into the daemon's main router.
    pub workflows_feature: rightkit_workflow::control_plane::prelude::FeatureGate,
    /// Per-router state for the workflow control plane REST surface. Built
    /// only when storage is available; `None` lets the daemon serve without
    /// the workflow surface even if the feature flag is on (e.g. a dev
    /// daemon with an in-memory storage that's deliberately disabled).
    pub workflow_api_state: Option<Arc<crate::api_workflows::WorkflowApiState>>,
    /// Per-process routing decision cache (R1a-L3 SPEC 4). Keyed by
    /// `(conductor_run_id, role)`; sibling workers spawned from the same
    /// conductor read the cached routed model verbatim so they share the
    /// prefix-cache slot the first spawn established.
    pub routing_cache: crate::router_seam::RoutingCache,
    /// Per-session plugin activation overrides (FM-156). Global enable/disable
    /// is persisted in `plugin.json`; per-session overrides let a session
    /// enable a globally-disabled plugin or disable a globally-enabled one
    /// without touching the manifest. Stored in-memory per daemon lifetime;
    /// clearing on session delete prevents leak.
    pub session_plugin_activations: Arc<Mutex<rightkit_agent_plugins::SessionPluginActivations>>,
    /// EXT-001: which plugin scopes were unloaded from a session's tool
    /// registry because they are disabled for that session, and how many
    /// registrations that removed.
    ///
    /// Observable so the unload is provable at the call site rather than only
    /// logged: a test that merely composes the same calls would pass with the
    /// production wiring deleted.
    pub session_plugin_unloads: SessionPluginUnloads,
    /// EFF-006: reconciliation of mutation batches that never settled,
    /// recorded per session when that session opened its workspace.
    ///
    /// Observable so the reconciliation is provable rather than only logged:
    /// a `prepared` journal is not evidence that nothing ran, and a test must
    /// be able to assert the sweep actually happened at session creation.
    pub mutation_journal_reports: Arc<
        Mutex<
            std::collections::HashMap<
                String,
                Vec<rightkit_agent_tools::coding_tools::file_service::JournalReconciliation>,
            >,
        >,
    >,
    /// Live executed-task evaluator. Production uses the daemon's real
    /// conductor/sub-agent seams; tests may replace it with a deterministic
    /// executor to exercise the authenticated HTTP contract without spending
    /// provider credits.
    #[cfg(feature = "benchmarks")]
    pub(crate) executed_bakeoff_runtime: Arc<dyn crate::api_benchmarks::ExecutedBakeoffRuntime>,
    /// Broadcast one-shot: closed/sent when the client-presence reaper decides
    /// the daemon should shut down. `main_runtime_server::serve` races this
    /// against Ctrl-C/SIGTERM inside `axum::serve(...).with_graceful_shutdown`.
    pub shutdown_tx: Arc<tokio::sync::watch::Sender<bool>>,
}

// Sub-state extraction for the extracted `{app}-attention` crate (crate-
// extraction pilot, docs/pending/rust-build-surgery.md). The attention domain
// never reads any `DaemonState` field, so its sub-state is empty; this impl
// lets axum mount `rightkit_agent_engine_attention::attention_routes()` — a
// `Router<rightkit_agent_engine_attention::AttentionRouterState>` — directly into the
// `Router<DaemonState>` built in `api_router.rs` via `.merge()`, without the
// attention crate ever naming `DaemonState`.
impl axum::extract::FromRef<DaemonState> for rightkit_agent_engine_attention::AttentionRouterState {
    fn from_ref(_state: &DaemonState) -> Self {
        rightkit_agent_engine_attention::AttentionRouterState
    }
}

#[derive(Clone)]
pub struct SharedStorage {
    writer: Arc<tokio::sync::Mutex<rightkit_agent_engine_storage::Storage>>,
    read_pool: Option<rightkit_agent_engine_storage::StoragePool>,
}

impl SharedStorage {
    pub fn new(storage: rightkit_agent_engine_storage::Storage) -> Self {
        let read_pool = storage.read_pool();
        Self {
            writer: Arc::new(tokio::sync::Mutex::new(storage)),
            read_pool,
        }
    }

    pub async fn lock(
        &self,
    ) -> tokio::sync::MutexGuard<'_, rightkit_agent_engine_storage::Storage> {
        self.writer.lock().await
    }

    pub fn try_lock(
        &self,
    ) -> Result<
        tokio::sync::MutexGuard<'_, rightkit_agent_engine_storage::Storage>,
        tokio::sync::TryLockError,
    > {
        self.writer.try_lock()
    }

    pub fn blocking_lock(
        &self,
    ) -> tokio::sync::MutexGuard<'_, rightkit_agent_engine_storage::Storage> {
        self.writer.blocking_lock()
    }

    pub fn read_pool(&self) -> Option<rightkit_agent_engine_storage::StoragePool> {
        self.read_pool.clone()
    }
}

impl From<Arc<tokio::sync::Mutex<rightkit_agent_engine_storage::Storage>>> for SharedStorage {
    fn from(writer: Arc<tokio::sync::Mutex<rightkit_agent_engine_storage::Storage>>) -> Self {
        let read_pool = writer
            .try_lock()
            .ok()
            .and_then(|storage| storage.read_pool());
        Self { writer, read_pool }
    }
}

pub(crate) fn lock_recover<'a, T>(mutex: &'a Mutex<T>, label: &str) -> MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::error!(mutex = label, "recovering from poisoned daemon mutex");
            poisoned.into_inner()
        }
    }
}
