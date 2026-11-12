use super::*;

impl SessionEngine {}

// ---------------------------------------------------------------------------
// CR-18 (mechanism 3-1): the post-call envelope meter.
//
// `UsageAccount::context_tokens` is a running local estimate that keeps only
// prompt + completion of the last call; the provider bills against -- and
// reports context occupancy across -- all four token kinds (prompt,
// completion, cache_read, cache_creation; see
// `PostCallEnvelope::context_tokens`). Triggering compaction off that
// estimate therefore drifts from the very meter that produces the pressure.
// This ledger reconstructs the just-completed call's logged envelope by
// differencing the cumulative UsageAccount totals between consecutive step
// boundaries: `auto_compaction_due` runs exactly once per engine-loop
// iteration, immediately after the response's usage is recorded, so each
// nonzero delta IS the just-completed call's logged envelope. Every
// compaction decision (trigger, prefire, escalation target) reads this one
// meter.
//
// Keyed by session id because this fix's file allowlist excludes `engine.rs`
// (same constraint CR-17 solved with the `compaction_coordinator` registry).
#[derive(Default)]
struct EnvelopeLedgerEntry {
    last: crate::compaction_policy::PostCallEnvelope,
    seen_prompt: u64,
    seen_completion: u64,
    seen_reasoning: u64,
    seen_cache_read: u64,
    seen_cache_creation: u64,
}

/// Tier label reported on `sessionCompacted` / failure events. `summary` is
/// tier 2 (pressure-driven structured session-state summary), `overflow` is
/// tier 3 (reactive, after a provider context rejection), `manual` is the
/// user-requested floor. Tier 1 (wire-side tool-output decay) never compacts
/// the transcript and so never emits this event.
pub(crate) fn compaction_tier_label(
    kind: crate::compactor::CompactionCommandKindV1,
) -> &'static str {
    match kind {
        crate::compactor::CompactionCommandKindV1::Manual => "manual",
        crate::compactor::CompactionCommandKindV1::Overflow => "overflow",
        crate::compactor::CompactionCommandKindV1::Automatic
        | crate::compactor::CompactionCommandKindV1::AutomaticRetry => "summary",
    }
}

/// Typed fields of the `compaction_lineage` observation.
pub(crate) fn compaction_lineage_fields(
    session_id: &str,
    command_kind: crate::compactor::CompactionCommandKindV1,
    command_id: &str,
    epoch: u64,
    predecessor_epoch: u64,
    archive_id: Option<&str>,
    segment_count: usize,
) -> HashMap<String, String> {
    HashMap::from([
        ("sessionId".to_string(), session_id.to_string()),
        ("commandId".to_string(), command_id.to_string()),
        (
            "tier".to_string(),
            compaction_tier_label(command_kind).to_string(),
        ),
        ("epoch".to_string(), epoch.to_string()),
        (
            "predecessorEpoch".to_string(),
            predecessor_epoch.to_string(),
        ),
        (
            "archiveId".to_string(),
            archive_id.unwrap_or("").to_string(),
        ),
        ("lineageSegments".to_string(), segment_count.to_string()),
    ])
}

type PrefireTask = (String, tokio::task::JoinHandle<()>);
fn prefire_tasks() -> &'static std::sync::Mutex<HashMap<String, PrefireTask>> {
    static TASKS: std::sync::OnceLock<std::sync::Mutex<HashMap<String, PrefireTask>>> =
        std::sync::OnceLock::new();
    TASKS.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

fn envelope_ledger() -> &'static std::sync::Mutex<HashMap<String, EnvelopeLedgerEntry>> {
    static LEDGER: std::sync::OnceLock<std::sync::Mutex<HashMap<String, EnvelopeLedgerEntry>>> =
        std::sync::OnceLock::new();
    LEDGER.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// Free every piece of per-session compaction state a long-running daemon
/// would otherwise hold forever.
///
/// Three process-wide, session-keyed registries back compaction: the
/// coordinator (`compaction_coordinator`, holding an `Arc<Compactor>` so
/// breaker and prefire state survive across attempts), the CR-18 envelope
/// meter above, and the CR-18 pending-retry ledger in `compactor_retry`.
/// Clearing one and not the others still leaks, so **this is the only
/// retirement point** — callers must not reach into the registries
/// individually.
///
/// Takes a bare `session_id` rather than `&SessionEngine` because the caller
/// that matters most does not have an engine: the daemon deletes sessions by
/// id (`api_sessions::lifecycle::delete_session_handler`) long after any
/// engine for them is gone. Before that call site existed, the only callers
/// of the `SessionEngine` method wrapping this were inside the engine crate's
/// own tests, so in production these registries only ever grew — including
/// for sessions the user had explicitly deleted.
pub fn retire_session_compaction_state(session_id: &str) {
    rightkit_agent_coding_tools::context::retire_cloudflare_credential(session_id);
    rightkit_agent_coding_tools::context::retire_flyio_credential(session_id);
    rightkit_agent_coding_tools::async_inbox::retire_session(session_id);
    crate::engine::tool_dispatch::retire_session_async_state(session_id);
    // A tunnel stop that fails leaves a public exposure possibly live (#376): record it at error
    // level with the leaked resources instead of discarding the failures.
    let leaked_exposures =
        rightkit_agent_coding_tools::tunnel::retire_tunnel_session(session_id);
    if !leaked_exposures.is_empty() {
        tracing::error!(
            session_id,
            leaked = ?leaked_exposures,
            "session ended with tunnel exposures that could not be stopped; they may still be live"
        );
    }
    crate::compaction_coordinator::retire_session(session_id);
    if let Some((_, task)) = prefire_tasks()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(session_id)
    {
        task.abort();
    }
    envelope_ledger()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(session_id);
    crate::compactor::clear_pending_retry(session_id);
}

/// Typed meaning of a message arriving while a turn is active. The daemon's
/// current HTTP seam still carries a string, so `parse` is the single boundary
/// that preserves intent instead of allowing control text to become an
/// ordinary user turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InterjectionDirective {
    Steer(String),
    Cancel { reason: Option<String> },
    Reprompt(String),
}

impl InterjectionDirective {
    pub(crate) fn parse(raw: String) -> Self {
        let trimmed = raw.trim();
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(object) = value.as_object() {
                let kind = object
                    .get("kind")
                    .or_else(|| object.get("type"))
                    .or_else(|| object.get("$kind"))
                    .and_then(serde_json::Value::as_str)
                    .map(|kind| kind.to_ascii_lowercase());
                let text = object
                    .get("text")
                    .or_else(|| object.get("content"))
                    .or_else(|| object.get("query"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                match kind.as_deref() {
                    Some("cancel") | Some("stop") | Some("abort") => {
                        return Self::Cancel {
                            reason: object
                                .get("reason")
                                .and_then(serde_json::Value::as_str)
                                .map(ToString::to_string),
                        };
                    }
                    Some("reprompt") | Some("replace") => return Self::Reprompt(text),
                    Some("steer") => return Self::Steer(text),
                    _ => {}
                }
            }
        }

        let lower = trimmed.to_ascii_lowercase();
        if lower == "/cancel" || lower == "/stop" || lower == "/abort" {
            return Self::Cancel { reason: None };
        }
        if lower.starts_with("/reprompt ") || lower.starts_with("/replace ") {
            let payload = trimmed
                .split_once(char::is_whitespace)
                .map_or("", |(_, rest)| rest);
            return Self::Reprompt(payload.trim().to_string());
        }
        Self::Steer(raw)
    }
}

impl SessionEngine {
    /// Register a call that will resolve outside this engine's streaming loop.
    pub fn defer_tool_call(
        &self,
        tool_call_id: impl Into<String>,
    ) -> Result<
        rightkit_agent_coding_tools::deferred_resolution::DeferredToolCall,
        rightkit_agent_coding_tools::deferred_resolution::RegisterError,
    > {
        self.deferred_tool_calls.try_register(tool_call_id)
    }

    /// Resolve a deferred call once, then make its terminal payload available
    /// to a later turn through the same session's async inbox.
    pub fn resolve_deferred_tool_call(
        &self,
        tool_call_id: &str,
        resolution: rightkit_agent_coding_tools::deferred_resolution::DeferredResolution,
    ) -> Result<(), rightkit_agent_coding_tools::deferred_resolution::ResolveError> {
        let payload = serde_json::json!({
            "toolCallId": tool_call_id,
            "isError": resolution.is_error,
            "content": resolution.content.clone(),
            "data": resolution.data.clone(),
        });
        // A receiver may have gone away with an earlier streaming turn while
        // this first, out-of-band completion still needs to reach the inbox.
        // The registry removed its entry either way, so only `NotFound`
        // represents a duplicate or unknown resolver here.
        match self.deferred_tool_calls.resolve(tool_call_id, resolution) {
            Ok(())
            | Err(rightkit_agent_coding_tools::deferred_resolution::ResolveError::AlreadyResolvedOrAbandoned) =>
                {}
            Err(error @ rightkit_agent_coding_tools::deferred_resolution::ResolveError::NotFound) => {
                return Err(error);
            }
        }
        let inbox_message_id = self.async_inbox_producer.push(
            "deferred_tool_resolution",
            format!("Deferred tool call '{tool_call_id}' resolved."),
            Some(payload),
        );
        if let Some(message_id) = inbox_message_id {
            self.emit(|seq| ProtocolEvent::OrchestrationObservationRecorded {
                session_id: self.session_id.clone(),
                seq,
                created_at: now_iso(),
                observation: rightkit_agent_engine_protocol::OrchestrationObservation {
                    id: format!("deferred-tool-resolution-{tool_call_id}"),
                    kind: "deferred_tool_resolution".to_string(),
                    subject: self.session_id.clone(),
                    summary: format!(
                        "Deferred tool call '{tool_call_id}' resolved outside the streaming loop."
                    ),
                    fields: std::collections::HashMap::from([
                        ("toolCallId".to_string(), tool_call_id.to_string()),
                        ("inboxMessageId".to_string(), message_id.to_string()),
                    ]),
                    created_at: now_iso(),
                },
            });
        }
        Ok(())
    }

    /// Queue a session-owned event from a non-streaming platform producer.
    /// Mobile hosts use this for local completion callbacks; the producer
    /// lease rejects callbacks from a retired session lifetime.
    pub fn post_async_inbox_message(
        &self,
        channel: impl Into<String>,
        body: impl Into<String>,
        data: Option<serde_json::Value>,
    ) -> Option<u64> {
        let channel = channel.into();
        let body = body.into();
        let message_id = self
            .async_inbox_producer
            .push(channel.clone(), body.clone(), data);
        if let Some(message_id) = message_id {
            self.emit(|seq| ProtocolEvent::OrchestrationObservationRecorded {
                session_id: self.session_id.clone(),
                seq,
                created_at: now_iso(),
                observation: rightkit_agent_engine_protocol::OrchestrationObservation {
                    id: format!("async-inbox-{message_id}"),
                    kind: "async_inbox_message".to_string(),
                    subject: self.session_id.clone(),
                    summary: body,
                    fields: std::collections::HashMap::from([
                        ("channel".to_string(), channel),
                        ("inboxMessageId".to_string(), message_id.to_string()),
                    ]),
                    created_at: now_iso(),
                },
            });
        }
        message_id
    }

    /// End unresolved deferred calls before dropping this engine instance.
    pub fn retire_runtime_collaboration(&self) {
        self.deferred_tool_calls.abandon_all();
        retire_session_compaction_state(&self.session_id);
    }

    pub fn set_web_search_config(&mut self, config: WebSearchConfig) {
        self.web_search_config = Some(config);
    }

    /// Give this session a key for one exact image-provider route. Callers
    /// must resolve the provider from the bundled image catalog first.
    pub fn set_image_provider_key(
        &mut self,
        provider: impl Into<String>,
        api_key: impl Into<String>,
    ) {
        let provider = provider.into();
        let api_key = api_key.into();
        if !provider.trim().is_empty() && !api_key.trim().is_empty() {
            self.image_provider_keys.insert(provider, api_key);
        }
    }

    pub fn set_cloudflare_config(
        &mut self,
        config: rightkit_agent_coding_tools::CloudflareConfig,
    ) {
        self.cloudflare_config = Some(config);
    }

    /// Mid-session Cloudflare credential refresh.
    ///
    /// `set_cloudflare_config` above is session-start only: it needs `&mut
    /// self`, which in practice means holding the engine's per-turn lock —
    /// exactly the lock a host wants to avoid blocking on when it refreshes a
    /// near-expiry token mid-turn. This associated function instead writes
    /// straight into the tools crate's session-keyed credential cell
    /// (`rightkit_agent_coding_tools::context::update_cloudflare_credential`), so the
    /// refreshed bearer reaches the NEXT Cloudflare tool call in this session
    /// without waiting for any in-flight call to finish, and without
    /// disturbing that in-flight call's own already-captured credential.
    ///
    /// Takes `session_id` directly (not `&self`/`&mut self`) since the
    /// credential cell it forwards to is keyed by session id and lives
    /// independently of any particular `SessionEngine` instance.
    pub fn update_cloudflare_credential(
        session_id: &str,
        config: rightkit_agent_coding_tools::CloudflareConfig,
    ) {
        rightkit_agent_coding_tools::context::update_cloudflare_credential(
            session_id, config,
        );
    }

    pub fn set_github_token(&mut self, token: impl Into<String>) {
        self.github_token = Some(token.into());
    }

    /// Override the summarizer with an infallible implementation. Production
    /// compaction uses the fail-closed default; this adapter primarily keeps
    /// deterministic test/custom summarizers source-compatible.
    pub fn with_summarizer(mut self, summarizer: Arc<dyn crate::compactor::Summarizer>) -> Self {
        self.summarizer = Arc::new(crate::compactor::InfallibleAdapter::new(summarizer));
        self
    }

    /// Override the authoritative compaction summarizer with a fallible path.
    pub fn with_compaction_summarizer(
        mut self,
        summarizer: Arc<dyn crate::compactor::FallibleSummarizer>,
    ) -> Self {
        self.summarizer = summarizer;
        self
    }

    pub fn with_compaction_archive_sink(
        mut self,
        sink: Arc<dyn crate::compaction_archive::CompactionArchiveSink>,
    ) -> Self {
        self.compaction_archive_sink = Some(sink);
        self
    }

    pub fn set_compaction_archive_sink(
        &mut self,
        sink: Arc<dyn crate::compaction_archive::CompactionArchiveSink>,
    ) {
        self.compaction_archive_sink = Some(sink);
    }

    pub fn set_compaction_decision_provider(
        &mut self,
        provider: Arc<dyn crate::compaction_archive::CompactionDecisionProvider>,
    ) {
        self.compaction_decision_provider = Some(provider);
    }

    /// FM-178: live session compaction lineage (ported codex `RolloutLineage`).
    pub fn compaction_lineage(&self) -> &crate::compaction_archive::CompactionRolloutLineage {
        &self.compaction_lineage
    }

    /// ENG-32: restore a previously-persisted compaction lineage into a
    /// freshly constructed session. A host that checkpoints
    /// `compaction_lineage()` (it derives `Serialize`/`Deserialize`) calls
    /// this before resuming a turn so `materialize_compaction_lineage`
    /// continues to reconstruct full visible history across a process
    /// restart, not just within one process lifetime. Only meaningful before
    /// any turn has run on this instance; calling it later replaces whatever
    /// lineage this instance had already accumulated, which is never what a
    /// resuming host wants mid-session.
    pub fn set_compaction_lineage(
        &mut self,
        lineage: crate::compaction_archive::CompactionRolloutLineage,
    ) {
        self.compaction_lineage = lineage;
    }

    /// FM-178: reconstruct the full visible history this session has compacted
    /// away, independently of the currently compacted context.
    pub fn materialize_compaction_lineage(
        &self,
    ) -> Vec<rightkit_agent_engine_protocol::TranscriptMessage> {
        self.compaction_lineage.materialize()
    }

    /// Wire a post-compaction memory-block provider. When set, every
    /// authoritative compaction invokes the provider fresh (no caching) and
    /// injects the returned block immediately after the post-compaction
    /// continuation marker, replacing any prior sentinel-scoped block for
    /// this session. The bin wires its `session_memory_block` builder here
    /// so live compaction re-injects the same durable-memory view that
    /// turn-0 already established.
    pub fn set_memory_block_provider(
        &mut self,
        provider: Arc<dyn crate::compactor::MemoryBlockProvider>,
    ) {
        self.memory_block_provider = Some(provider);
    }

    /// Attach an operator-question callback used by the `ask_user` tool.
    pub fn set_questioner(
        &mut self,
        questioner: Arc<dyn rightkit_agent_coding_tools::UserQuestioner>,
    ) {
        self.questioner = Some(questioner);
    }

    /// Attach the existing authenticated Membrane adapter used by run_program.
    #[cfg(feature = "host")]
    pub fn set_context_tool_dispatcher(
        &mut self,
        dispatcher: Arc<dyn rightkit_agent_coding_tools::code_mode::ContextToolDispatcher>,
    ) {
        self.context_tool_dispatcher = Some(dispatcher);
    }

    /// Attach a plan-exit callback used by the `exit_plan_mode` tool.
    pub fn set_plan_exit(
        &mut self,
        plan_exit: Arc<dyn rightkit_agent_coding_tools::PlanExit>,
    ) {
        self.plan_exit = Some(plan_exit);
    }

    /// Attach a live message dispatcher used by the `send_message` tool.
    pub fn set_message_dispatcher(
        &mut self,
        dispatcher: Arc<dyn rightkit_agent_coding_tools::MessageDispatcher>,
    ) {
        self.message_dispatcher = Some(dispatcher);
    }

    /// Attach a live team directory used by team tools.
    pub fn set_team_directory(
        &mut self,
        directory: Arc<dyn rightkit_agent_coding_tools::TeamDirectory>,
    ) {
        self.team_directory = Some(directory);
    }

    /// Attach an MCP manager used by the dynamic MCP tools.
    pub fn set_mcp(&mut self, mcp: Arc<rightkit_mcp::agent_mcp::McpManager>) {
        self.mcp = Some(mcp);
    }

    pub fn set_interrupt_receiver(&mut self, rx: tokio::sync::watch::Receiver<bool>) {
        self.interrupt_rx = Some(rx);
        // Keep one session-lifetime watcher in addition to per-call watchers.
        // A completed call drops its cancel sender; this watcher ensures a
        // later turn interrupt still reaches shells started by that turn.
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        drop(cancel_tx);
        self.propagate_cancel_to_background_shells("session-interrupt", cancel_rx);
    }

    /// Attach a live permission-mode channel; the current value is adopted at the
    /// start of each turn.
    pub fn set_permission_mode_receiver(
        &mut self,
        rx: tokio::sync::watch::Receiver<PermissionMode>,
    ) {
        self.permission_mode_rx = Some(rx);
    }

    /// Attach the live active-model compaction-capability channel.
    pub fn set_compaction_policy_receiver(
        &mut self,
        rx: tokio::sync::watch::Receiver<crate::compaction_policy::CompactionModelPolicy>,
    ) {
        self.compaction_policy_rx = Some(rx);
        self.refresh_compaction_policy();
    }

    /// Refresh the permission mode from the live channel, if one is attached.
    pub(super) fn refresh_permission_mode(&mut self) {
        if let Some(rx) = &self.permission_mode_rx {
            self.permission_mode = *rx.borrow();
        }
    }

    pub(super) fn refresh_compaction_policy(&mut self) {
        if let Some(rx) = &mut self.compaction_policy_rx {
            self.compaction_policy = rx.borrow_and_update().clone();
            self.usage
                .set_window(self.compaction_policy.context_window_tokens.unwrap_or(0));
            self.summarizer
                .set_max_output_tokens(self.compaction_policy.summary_max_output_tokens);
        }
    }

    /// Refresh host-owned policy inputs before retry-wake eligibility is
    /// evaluated. This is also used by the host while a session is idle.
    pub fn refresh_compaction_revision_inputs(&mut self) {
        self.refresh_permission_mode();
        self.refresh_compaction_policy();
        if let Some(capacity) = self.compaction_policy.context_window_tokens {
            self.observe_host_revision("capacity", Some(&capacity.to_string()));
        } else {
            self.observe_host_revision("capacity", None);
        }
        if let Some(reserve) = self.compaction_policy.reserve_tokens {
            self.observe_host_revision("reserve", Some(&reserve.to_string()));
        } else {
            self.observe_host_revision("reserve", None);
        }
        let authorization_policy = if self.abac_pdp.is_some() {
            None
        } else {
            let rules = match &self.permission_rules {
                Some(rules) => rules
                    .lock()
                    .ok()
                    .map(|rules| canonical_rule_revision(&rules)),
                None => Some(canonical_json(serde_json::json!(["rules-v1", "none"]))),
            };
            rules.map(|rules| {
                canonical_json(serde_json::json!([
                    "compaction-authorization-v1",
                    format!("{:?}", self.permission_mode),
                    format!("{:?}", self.workspace_trust),
                    rules,
                ]))
            })
        };
        self.observe_host_revision("policy", authorization_policy.as_deref());
    }

    pub fn set_approval_store(
        &mut self,
        store: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>>,
    ) {
        self.pending_approvals = store;
    }

    pub fn set_approval_context_store(
        &mut self,
        store: Arc<Mutex<HashMap<String, rightkit_agent_engine_protocol::PendingApprovalContext>>>,
    ) {
        self.pending_approval_contexts = store;
    }

    /// Attach an external credential-request registry, mirroring
    /// `set_approval_store` (ENG-94): the daemon/FFI layer constructs and
    /// retains its own `Arc<Mutex<PendingCredentialRequests>>` and passes it
    /// in here so it can list/resolve requests from outside the engine using
    /// the same shared handle the engine inserts into mid-turn.
    pub fn set_credential_request_store(
        &mut self,
        store: Arc<Mutex<crate::credential_request::PendingCredentialRequests>>,
    ) {
        self.pending_credential_requests = store;
    }

    /// Non-consuming snapshot of every credential request currently awaiting
    /// an operator response for this session. Safe to call repeatedly and
    /// concurrently with a resolve: listing never removes or invalidates a
    /// pending entry (see `PendingCredentialRequests::pending`). Carries only
    /// the credential's identity/reason — never a secret value, since none is
    /// ever stored in the registry.
    pub fn list_pending_credential_requests(
        &self,
    ) -> Vec<crate::credential_request::CredentialRequest> {
        self.pending_credential_requests.lock().unwrap().pending()
    }

    /// Resolve one pending credential request by `tool_call_id`, delivering
    /// `resume` to the paused tool call awaiting it. Returns `true` if a
    /// pending request was found and resolved; single-use, mirroring
    /// approval resolution — a second call for the same id (e.g. a duplicate
    /// UI submission) finds nothing left to resolve and returns `false`
    /// rather than redelivering or replaying a credential.
    ///
    /// The secret carried inside `CredentialResume::Supplied` travels only
    /// through this oneshot channel to the paused tool call; this accessor
    /// does not log it, place it in the transcript, or otherwise persist it.
    pub fn resolve_credential_request(
        &mut self,
        tool_call_id: &str,
        resume: crate::credential_request::CredentialResume,
    ) -> bool {
        self.pending_credential_requests
            .lock()
            .unwrap()
            .take_and_resolve(tool_call_id, resume)
    }

    pub fn set_cancel_token_store(
        &mut self,
        store: Arc<Mutex<HashMap<String, tokio::sync::watch::Sender<bool>>>>,
    ) {
        self.cancel_tokens = store;
    }

    /// Attach a steering channel. Wire strings are parsed into typed
    /// interjection directives at the loop boundary; callers that predate the
    /// typed contract remain source-compatible.
    pub fn set_steer_receiver(&mut self, rx: tokio::sync::mpsc::Receiver<String>) {
        self.steer_rx = Some(rx);
    }

    /// Attach asynchronous host-state observations. They are drained only at
    /// the next provider-turn boundary, before live steering is consumed.
    pub fn set_provider_boundary_receiver(
        &mut self,
        rx: crate::provider_turn_boundary::ProviderBoundaryReceiver,
    ) {
        self.provider_boundary_rx = Some(rx);
    }

    /// Attach a sub-agent spawner.
    /// Attach a lifecycle hook bus. The engine will publish UserPromptSubmit and
    /// Pre/PostToolUse events for subscribers.
    pub fn set_hook_bus(&mut self, bus: rightkit_hooks::agent_hooks::HookBus) {
        self.hook_bus = Some(bus);
    }

    /// Attach an optional per-tool guardian LLM reviewer (FEATURE-MATRIX
    /// §#87). Unset by default — leaving this uncalled keeps the guardian
    /// path a total no-op. See `guardian.rs` for the trait contract.
    pub fn set_guardian_reviewer(&mut self, guardian: Arc<dyn GuardianReviewer>) {
        self.guardian = Some(guardian);
    }

    pub(super) fn publish_hook(&self, event: rightkit_hooks::agent_hooks::HookEvent) {
        if let Some(bus) = &self.hook_bus {
            bus.publish(event);
        }
    }

    /// Mark this engine as a sub-agent so its completion gate runs
    /// `SubagentStop` hooks instead of `Stop`.
    pub fn set_subagent_role(&mut self, role: impl Into<String>) {
        self.subagent_role = Some(role.into());
    }

    /// Feed hook-provided `additionalContext` back to the model as a system
    /// note (same channel as the skill hints).
    pub(super) fn push_hook_context(&mut self, source: &str, context: &str) {
        let context = context.trim();
        if context.is_empty() {
            return;
        }
        self.transcript
            .push_system(format!("[Hook context: {source}] {context}"));
    }

    /// Run the `RIGHTKIT_HOOKS_JSON` chain for a lifecycle event, forward any
    /// `additionalContext` to the model, and log non-blocking errors. Returns
    /// the aggregated report so callers can honor a deny.
    pub(super) async fn run_lifecycle_hooks(
        &mut self,
        event: &str,
        extra_envs: &[(&'static str, String)],
    ) -> rightkit_hooks::agent_hooks::ChainReport {
        let Some(bus) = self.hook_bus.clone() else {
            return rightkit_hooks::agent_hooks::ChainReport::default();
        };
        let session_id = self.session_id.clone();
        let report = bus
            .run_event_hooks(event, &session_id, None, None, extra_envs)
            .await;
        for context in &report.additional_context {
            self.push_hook_context(event, context);
        }
        for error in &report.errors {
            eprintln!("[hook] {event} error (non-blocking): {error}");
        }
        report
    }

    pub fn set_spawner(&mut self, spawner: Arc<dyn SubagentSpawner>) {
        self.spawner = Some(spawner);
    }

    /// Attach host-owned wait/interrupt control for spawned child identities.
    pub fn set_subagent_supervisor(
        &mut self,
        supervisor: Arc<dyn rightkit_agent_coding_tools::SubagentSupervisor>,
    ) {
        self.subagent_supervisor = Some(supervisor);
    }

    pub fn transcript(&self) -> &Transcript {
        &self.transcript
    }

    pub fn usage(&self) -> &UsageAccount {
        &self.usage
    }

    /// Set the per-1k-token prices used to compute this session's cost. Call at
    /// setup, before any usage has accrued. Assumes the caller resolved real
    /// pricing for the model; use [`Self::set_pricing_with_known`] when the
    /// model's pricing may be an unresolved fallback.
    pub fn set_pricing(&mut self, prompt_per_1k: f64, completion_per_1k: f64) {
        self.usage = UsageAccount::new(prompt_per_1k, completion_per_1k);
    }

    /// Same as `set_pricing`, but records whether the prices came from a
    /// curated table entry (`priced = true`) or a conservative fallback
    /// (`priced = false`). Downstream cost reporting (`UsageAccount::
    /// cost_usd_or_unknown`) uses this to report `null` instead of a guessed
    /// number for unpriced models.
    pub fn set_pricing_with_known(
        &mut self,
        prompt_per_1k: f64,
        completion_per_1k: f64,
        priced: bool,
    ) {
        self.usage = UsageAccount::with_known_pricing(prompt_per_1k, completion_per_1k, priced);
    }

    /// Set the context window capacity (in tokens) for the active model so that
    /// `UsageUpdated.contextPercent` reports a meaningful fraction.
    pub fn set_window(&mut self, window_tokens: u64) {
        self.usage.set_window(window_tokens);
        self.compaction_policy = crate::compaction_policy::CompactionModelPolicy::known(
            "legacy-explicit-window",
            window_tokens,
            70,
            1,
            crate::compactor::MIN_SUMMARY_TOKENS,
        );
        self.summarizer
            .set_max_output_tokens(self.compaction_policy.summary_max_output_tokens);
    }

    pub fn set_compaction_policy(
        &mut self,
        policy: crate::compaction_policy::CompactionModelPolicy,
    ) {
        self.compaction_policy = policy;
        self.usage
            .set_window(self.compaction_policy.context_window_tokens.unwrap_or(0));
        self.summarizer
            .set_max_output_tokens(self.compaction_policy.summary_max_output_tokens);
    }

    pub fn compaction_policy(&self) -> &crate::compaction_policy::CompactionModelPolicy {
        &self.compaction_policy
    }

    /// CR-17: snapshot of THIS session's persistent breaker state, drawn
    /// from the same per-session coordinator `compact_context` uses. Exposed
    /// so operator surfaces (and tests) can observe consecutive-failure
    /// counts and trip state that now actually accumulate across turns.
    pub fn compaction_breaker_status(&self) -> crate::compactor::CompactorBreakerStatus {
        let provider = self.memory_block_provider.clone();
        crate::compaction_coordinator::coordinator_for(&self.session_id, || {
            let built = crate::compactor::Compactor::new(0, 8);
            if let Some(provider) = provider.clone() {
                built.with_memory_block_provider(provider)
            } else {
                built
            }
        })
        .breaker()
        .status()
    }

    /// CR-17: release this session's persistent compaction coordinator
    /// (prefire cache + breaker history). Call when a session is deleted or
    /// otherwise permanently retired so a long-running daemon does not
    /// accumulate one `Compactor` per session forever. Also drops this
    /// session's CR-18 envelope-meter and pending-retry state so a recycled
    /// session id starts from a clean meter.
    pub fn retire_compaction_coordinator(&self) {
        self.retire_runtime_collaboration();
    }

    /// CR-18 (3-1): difference the cumulative UsageAccount totals against
    /// the last step boundary; a nonzero delta is the just-completed call's
    /// logged envelope and becomes the session's pressure reading. When no
    /// new call completed since the last boundary the previous envelope is
    /// reused (pressure did not change); before the first completed call the
    /// zero envelope is returned.
    fn observe_post_call_envelope(&self) -> crate::compaction_policy::PostCallEnvelope {
        let mut guard = envelope_ledger()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = guard.entry(self.session_id.clone()).or_default();
        // A fresh SessionEngine instance can reuse a session id (tests do);
        // totals that went backwards mean this ledger entry belongs to a dead
        // instance, so reset it rather than difference across instances.
        if self.usage.prompt_tokens < entry.seen_prompt
            || self.usage.completion_tokens < entry.seen_completion
            || self.usage.reasoning_tokens < entry.seen_reasoning
            || self.usage.cache_read_tokens < entry.seen_cache_read
            || self.usage.cache_creation_tokens < entry.seen_cache_creation
        {
            *entry = EnvelopeLedgerEntry::default();
        }
        let delta = crate::compaction_policy::PostCallEnvelope {
            prompt_tokens: self.usage.prompt_tokens - entry.seen_prompt,
            // Audit C8: reasoning tokens are billed as completion but never
            // resent, so they must not inflate context pressure.
            completion_tokens: (self.usage.completion_tokens - entry.seen_completion)
                .saturating_sub(self.usage.reasoning_tokens - entry.seen_reasoning),
            cache_read_tokens: self.usage.cache_read_tokens - entry.seen_cache_read,
            cache_creation_tokens: self.usage.cache_creation_tokens - entry.seen_cache_creation,
        };
        entry.seen_prompt = self.usage.prompt_tokens;
        entry.seen_completion = self.usage.completion_tokens;
        entry.seen_reasoning = self.usage.reasoning_tokens;
        entry.seen_cache_read = self.usage.cache_read_tokens;
        entry.seen_cache_creation = self.usage.cache_creation_tokens;
        if delta.context_tokens() > 0 {
            entry.last = delta;
        }
        entry.last
    }

    /// CR-18 invariant 3: ONE pressure meter for every compaction decision.
    /// Reads the last logged post-call envelope; falls back to the
    /// UsageAccount estimate only before the first completed call of the
    /// session.
    fn pressure_meter_tokens(&self) -> u64 {
        let guard = envelope_ledger()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match guard.get(&self.session_id) {
            Some(entry) if entry.last.context_tokens() > 0 => entry.last.context_tokens(),
            _ => self.usage.context_tokens,
        }
    }

    fn exclude_summary_usage_from_pressure(&self) {
        let mut guard = envelope_ledger()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = guard.entry(self.session_id.clone()).or_default();
        entry.seen_prompt = self.usage.prompt_tokens;
        entry.seen_completion = self.usage.completion_tokens;
        entry.seen_reasoning = self.usage.reasoning_tokens;
        entry.seen_cache_read = self.usage.cache_read_tokens;
        entry.seen_cache_creation = self.usage.cache_creation_tokens;
    }

    fn reset_pressure_from_rebuilt_context(&self, context_tokens: u64) {
        let mut guard = envelope_ledger()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = guard.entry(self.session_id.clone()).or_default();
        entry.last = crate::compaction_policy::PostCallEnvelope {
            prompt_tokens: context_tokens,
            ..Default::default()
        };
        entry.seen_prompt = self.usage.prompt_tokens;
        entry.seen_completion = self.usage.completion_tokens;
        entry.seen_reasoning = self.usage.reasoning_tokens;
        entry.seen_cache_read = self.usage.cache_read_tokens;
        entry.seen_cache_creation = self.usage.cache_creation_tokens;
    }

    /// CR-18 (mechanism 3-3): the cheap-first escalation target, derived
    /// from the SAME logged-envelope meter that triggers compaction: how far
    /// the session sits past its automatic boundary. Pruning alone may finish
    /// the job only when the surviving fold-away set measures at or under
    /// this overage; anything larger still needs a real summary.
    fn escalation_trigger_tokens(&self) -> u32 {
        let boundary = self.compaction_policy.auto_trigger_tokens().unwrap_or(0);
        (self.pressure_meter_tokens().saturating_sub(boundary)).min(u32::MAX as u64) as u32
    }

    /// User-triggered compaction. This intentionally bypasses the automatic
    /// threshold while using the exact same authoritative engine operation.
    pub async fn compact_context_now(&mut self) -> Result<CompactionOutcome, EngineError> {
        self.refresh_compaction_policy();
        self.compact_context_with_kind(
            crate::compactor::CompactionCommandKindV1::Manual,
            crate::compactor::next_compaction_command_id().to_string(),
        )
        .await
    }

    pub async fn compact_context_automatic(&mut self) -> Result<CompactionOutcome, EngineError> {
        self.refresh_compaction_policy();
        self.compact_context().await
    }

    /// VG-002 tier 3: reactive compaction after the provider rejected the
    /// request for context size. Same authoritative operation as the automatic
    /// path, but reported under its own `overflow` tier/trigger.
    pub async fn compact_context_overflow(&mut self) -> Result<CompactionOutcome, EngineError> {
        self.refresh_compaction_policy();
        self.compact_context_with_kind(
            crate::compactor::CompactionCommandKindV1::Overflow,
            crate::compactor::next_compaction_command_id().to_string(),
        )
        .await
    }

    pub async fn compact_context_automatic_retry(
        &mut self,
    ) -> Result<CompactionOutcome, EngineError> {
        self.refresh_compaction_policy();
        self.compact_context_with_kind(
            crate::compactor::CompactionCommandKindV1::AutomaticRetry,
            crate::compactor::next_compaction_command_id().to_string(),
        )
        .await
    }

    /// Serialized command seam shared by API delivery and engine-internal
    /// automatic triggers. The caller owns FIFO admission; this method owns
    /// the typed command meaning while the engine's `&mut self` is held.
    pub async fn execute_compaction_command(
        &mut self,
        command: crate::compactor::CompactionCommandV1,
    ) -> Result<CompactionOutcome, EngineError> {
        tracing::debug!(
            session_id = %self.session_id,
            command_id = command.id,
            command_kind = ?command.kind,
            "execute serialized compaction command"
        );
        if matches!(
            command.kind,
            crate::compactor::CompactionCommandKindV1::AutomaticRetry
        ) && !crate::compactor::pending_retry_exists(&self.session_id)
        {
            return Err(EngineError::Compaction(
                "automatic compaction retry skipped: pending decision no longer exists".into(),
            ));
        }
        let trigger = match command.kind {
            crate::compactor::CompactionCommandKindV1::Manual => "manual",
            crate::compactor::CompactionCommandKindV1::Overflow => "overflow",
            _ => "automatic",
        };
        let sid = self.session_id.clone();
        let command_id = command.id.to_string();
        // PreCompact: publish for subscribers, then run configured hooks. Not
        // a gate: compaction protects the context window, so a hook cannot
        // veto it (additionalContext is still forwarded).
        self.publish_hook(rightkit_hooks::agent_hooks::HookEvent::PreCompact {
            session_id: sid.clone(),
        });
        self.run_lifecycle_hooks(
            "PreCompact",
            &[("RIGHTKIT_HOOK_TRIGGER", trigger.to_string())],
        )
        .await;
        self.emit(|seq| ProtocolEvent::SessionCompactionStarted {
            session_id: sid,
            seq,
            created_at: now_iso(),
            trigger: trigger.into(),
            command_id: command_id.clone(),
        });
        self.refresh_compaction_policy();
        let result = self
            .compact_context_with_kind(command.kind, command_id.clone())
            .await;
        match &result {
            Ok(outcome) if !outcome.compacted => self.emit_compaction_failed(
                command.kind,
                &command_id,
                "nothing eligible to compact",
            ),
            Err(error) => {
                self.emit_compaction_failed(command.kind, &command_id, &error.to_string())
            }
            // Tools rule 58 `PostCompact`: fires after a compaction that changed
            // the transcript. Like `PreCompact` it cannot veto anything.
            Ok(_) => {
                self.run_lifecycle_hooks(
                    "PostCompact",
                    &[("RIGHTKIT_HOOK_TRIGGER", trigger.to_string())],
                )
                .await;
            }
        }
        result
    }

    pub fn compaction_retry_wake_eligible(
        &self,
        snapshot: &crate::compactor::CompactionSnapshotV1,
    ) -> bool {
        crate::compactor::pending_retry_wake_eligible(&self.session_id, snapshot)
    }

    pub(super) fn auto_compaction_due(&mut self) -> bool {
        self.refresh_compaction_policy();
        // FM-065: the live, per-turn compaction checkpoint. Project the
        // image-payload cost into the context-fit decision via
        // `ensure_context_fits` so an image-heavy session trips
        // `AutoCompact`/`CriticalSync` on projected cost, not just the
        // text-only envelope count. This is the single production dispatch
        // point for the retained four-tier policy (was test-only before).
        let envelope = self.observe_post_call_envelope();
        let context_tokens =
            envelope
                .context_tokens()
                .max(u64::from(crate::compactor::estimate_transcript_tokens(
                    self.transcript.messages(),
                )));
        let inline_images = crate::image_eviction::inline_image_count(self.transcript.messages());
        // Audit C8: the provider-reported envelope already counts the images
        // it saw, and the transcript estimate is text-only. Project image cost
        // only on top of the estimate, never on top of a measured envelope.
        let measured_includes_images = envelope.context_tokens()
            >= u64::from(crate::compactor::estimate_transcript_tokens(
                self.transcript.messages(),
            ));
        let projected_images = if measured_includes_images {
            0
        } else {
            inline_images
        };
        let action = crate::compaction_policy::ensure_context_fits(
            &self.compaction_policy,
            context_tokens,
            projected_images,
            crate::compaction_policy::DEFAULT_PREFIRE_LEAD_PERCENT,
            crate::compaction_policy::DEFAULT_CRITICAL_LEAD_PERCENT,
        );
        match action {
            // Below the prefire window: nothing to do.
            crate::compaction_policy::CompactionAction::None => false,
            // Inside the prefire window: start a background summary (well
            // before the real trigger) so a valid result is cached by the
            // time `compact_context` fires for real.
            crate::compaction_policy::CompactionAction::Prefire => {
                self.maybe_spawn_prefire(inline_images);
                false
            }
            // At/over the automatic trigger: block the hot path and compact now.
            crate::compaction_policy::CompactionAction::AutoCompact => true,
            // At/over the critical boundary: the session is about to be
            // rejected by the provider. Surface the observation and compact
            // synchronously now (jcode `ensure_context_fits` hard-compacts at
            // its critical threshold; refusing here left the session to hit a
            // provider overflow instead). Manual `/compact` stays available
            // at every tier as the floor.
            crate::compaction_policy::CompactionAction::CriticalSync => {
                self.flag_critical_compaction_required(inline_images);
                true
            }
        }
    }

    /// FM-065: emit an orchestration observation recording that the session
    /// has crossed the critical-sync boundary and is being compacted
    /// synchronously on the hot path.
    fn flag_critical_compaction_required(&self, inline_images: u64) {
        self.emit(|seq| ProtocolEvent::OrchestrationObservationRecorded {
            session_id: self.session_id.clone(),
            seq,
            created_at: now_iso(),
            observation: rightkit_agent_engine_protocol::OrchestrationObservation {
                id: format!("critical-compaction-{}", self.session_id),
                kind: "compaction_critical_sync".to_string(),
                subject: self.session_id.to_string(),
                summary: "context exceeds critical-sync boundary; compacting synchronously before the next provider call".to_string(),
                fields: std::collections::HashMap::from([
                    ("sessionId".to_string(), self.session_id.to_string()),
                    ("inlineImages".to_string(), inline_images.to_string()),
                ]),
                created_at: now_iso(),
            },
        });
    }

    /// CR-17 (mechanism 2, background half, wired to the live path): if the
    /// session has crossed the prefire boundary but not yet the real
    /// compaction trigger, and no valid prefire is already cached for the
    /// CURRENT transcript prefix, spawn a background task that prepares one
    /// via `Compactor::prefire_prepare` and stores it on this session's
    /// persistent coordinator. The hot path (`compact_context`) then finds
    /// it via `prefire_still_valid` inside `prepare_authoritative` with zero
    /// additional model calls, PROVIDED the prefix has not mutated in the
    /// meantime (a rewind/edit invalidates it, exactly as designed).
    fn maybe_spawn_prefire(&mut self, inline_images: u64) {
        // FM-065: gate on the same projected policy used by the trigger so
        // the prefire window is decided on image-projected cost.
        let action = crate::compaction_policy::ensure_context_fits(
            &self.compaction_policy,
            self.pressure_meter_tokens(),
            inline_images,
            crate::compaction_policy::DEFAULT_PREFIRE_LEAD_PERCENT,
            crate::compaction_policy::DEFAULT_CRITICAL_LEAD_PERCENT,
        );
        if !matches!(action, crate::compaction_policy::CompactionAction::Prefire) {
            return;
        }
        let provider = self.memory_block_provider.clone();
        let session_id = self.session_id.clone();
        let compactor = crate::compaction_coordinator::coordinator_for(&session_id, || {
            let built = crate::compactor::Compactor::new(0, 8);
            if let Some(provider) = provider.clone() {
                built.with_memory_block_provider(provider)
            } else {
                built
            }
        });
        let current_fingerprint =
            crate::compactor::prefire_fingerprint(&self.transcript, compactor.keep_recent);
        if compactor.prefired_fingerprint().as_deref() == Some(current_fingerprint.as_str()) {
            return;
        }
        {
            let mut tasks = prefire_tasks().lock().unwrap_or_else(|p| p.into_inner());
            if let Some((fingerprint, task)) = tasks.get(&session_id) {
                if fingerprint == &current_fingerprint && !task.is_finished() {
                    return;
                }
            }
            if let Some((_, old)) = tasks.remove(&session_id) {
                old.abort();
            }
        }
        let summarizer = self.summarizer.clone();
        let snapshot = self.transcript.messages().to_vec();
        let task_session = session_id.clone();
        let task_fingerprint = current_fingerprint.clone();
        let task = tokio::spawn(async move {
            let mut transcript = Transcript::new();
            transcript.replace(snapshot);
            if let Ok(Some(prefired)) = compactor.prefire_prepare(&transcript, summarizer).await {
                let current = prefire_tasks()
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .get(&task_session)
                    .is_some_and(|(generation, _)| generation == &task_fingerprint);
                if current {
                    compactor.store_prefired(prefired);
                }
            }
        });
        prefire_tasks()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(session_id, (current_fingerprint, task));
    }

    pub(super) async fn compact_context(&mut self) -> Result<CompactionOutcome, EngineError> {
        self.compact_context_with_kind(
            crate::compactor::CompactionCommandKindV1::Automatic,
            crate::compactor::next_compaction_command_id().to_string(),
        )
        .await
    }

    async fn compact_context_with_kind(
        &mut self,
        command_kind: crate::compactor::CompactionCommandKindV1,
        command_id: String,
    ) -> Result<CompactionOutcome, EngineError> {
        let pending_prefire = prefire_tasks()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.session_id);
        if let Some((_, task)) = pending_prefire {
            task.abort();
            let _ = task.await;
        }
        if self.usage.exceeds_budget(self.budget_ceiling_usd) {
            let (used, ceiling) = (self.usage.cost_usd, self.budget_ceiling_usd);
            self.emit_budget_exceeded();
            return Err(EngineError::Compaction(format!(
                "budget ceiling reached before compaction (${:.4} / ${:.4})",
                used, ceiling
            )));
        }

        // ENG-104: stamp user pins onto the live transcript before the
        // compactor reads it, so `compactor_preservation` sees the `pinned`
        // bit it already honours. Both compaction entry points funnel through
        // here (`compact_context_now` delegates), so this is the one place a
        // pin can be applied without racing a concurrent compaction.
        crate::compaction_coordinator::apply_pinned_context_for(
            &self.session_id,
            &mut self.transcript,
        );
        let pinned_messages = self
            .transcript
            .messages()
            .iter()
            .filter(|message| {
                serde_json::to_value(message)
                    .ok()
                    .and_then(|value| value.get("pinned").and_then(|value| value.as_bool()))
                    .unwrap_or(false)
            })
            .cloned()
            .collect::<Vec<_>>();
        let pinned_digest = crate::compaction_archive::transcript_digest(&pinned_messages);
        self.observe_host_revision("pin", Some(&pinned_digest));
        if let Some(capacity) = self.compaction_policy.context_window_tokens {
            self.observe_host_revision("capacity", Some(&capacity.to_string()));
        } else {
            self.observe_host_revision("capacity", None);
        }
        if let Some(reserve) = self.compaction_policy.reserve_tokens {
            self.observe_host_revision("reserve", Some(&reserve.to_string()));
        } else {
            self.observe_host_revision("reserve", None);
        }
        self.observe_compaction_accounting();
        self.observe_host_revision(
            "promotion",
            Some(&self.provider_boundary_projection.generation().to_string()),
        );
        let authorization_policy = if self.abac_pdp.is_some() {
            None
        } else {
            let rules = match &self.permission_rules {
                Some(rules) => rules
                    .lock()
                    .ok()
                    .map(|rules| canonical_rule_revision(&rules)),
                None => Some(canonical_json(serde_json::json!(["rules-v1", "none"]))),
            };
            rules.map(|rules| {
                canonical_json(serde_json::json!([
                    "compaction-authorization-v1",
                    format!("{:?}", self.permission_mode),
                    format!("{:?}", self.workspace_trust),
                    rules,
                ]))
            })
        };
        self.observe_host_revision("policy", authorization_policy.as_deref());

        // CR-18: sourced from the same logged-envelope meter that decides
        // trigger and escalation, so a failed attempt restores the pressure
        // reading the decision was made against.
        let previous_context_tokens = self.pressure_meter_tokens();
        let compaction_snapshot = self.compaction_snapshot();
        if let Some(feed) = &self.compaction_snapshot_feed {
            if let Ok(mut current) = feed.write() {
                // Engine owns transcript accounting; host owns every external
                // authority revision. Preserve host fields across refresh.
                current.request_generation = compaction_snapshot.request_generation;
                current.transcript_high_water = compaction_snapshot.transcript_high_water;
                current.transcript_digest = compaction_snapshot.transcript_digest.clone();
                current.tool_catalog_revision = compaction_snapshot.tool_catalog_revision;
                current.decision_expires_at_unix_ms =
                    compaction_snapshot.decision_expires_at_unix_ms;
            }
        }
        // Preserve the authority state used to prepare this summary. Refreshing
        // it after the await would silently admit a result prepared under stale
        // host policy, binding, or retained-context authority.
        let compaction_snapshot = self
            .compaction_snapshot_feed
            .as_ref()
            .and_then(|feed| feed.read().ok().map(|snapshot| snapshot.clone()))
            .unwrap_or(compaction_snapshot);
        let summarizer = self.summarizer.clone();
        // CR-17: pull the ONE persistent coordinator for this live session
        // rather than constructing a fresh `Compactor` per attempt. A fresh
        // instance on every call meant `prefired` and `breaker` -- both
        // per-instance fields -- could never accumulate state across turns:
        // no background prefire result would ever be found valid, and no
        // sequence of real failures could ever trip the breaker. See
        // `compaction_coordinator` for the registry this draws from.
        let provider = self.memory_block_provider.clone();
        let compactor = crate::compaction_coordinator::coordinator_for(&self.session_id, || {
            let built = crate::compactor::Compactor::new(0, 8);
            if let Some(provider) = provider.clone() {
                built.with_memory_block_provider(provider)
            } else {
                built
            }
        });
        // P2-3 / CR-16: rolling observation decay is a READ-TIME projection
        // (`DecayingObservationProjection::project`) over canonical history, not
        // a live-transcript mutation. The `ObservationMasker::mask(&mut
        // Transcript)` call that lived here mutated `self.transcript` in place
        // before compaction -- a direct violation of the append-only canonical
        // invariant (AB / mechanism 10). The wire path in `engine.rs` now
        // serves decay via `WireProjection::project` at request time, leaving
        // `self.transcript` (and the SQLite event ledger it seeds) untouched.
        // No compaction code in this file mutates the live transcript.
        // r5.32: refuse unchanged failure-relevant state before work & consume
        // at most one automatic retry under the pending-decision ledger lock.
        let pending_retry = crate::compactor::admit_pending_retry(
            &self.session_id,
            &compaction_snapshot,
            command_kind,
        )
        .map_err(EngineError::Compaction)?;
        let prior_attempts = pending_retry.as_ref().map(|p| p.attempts).unwrap_or(0);
        let escalation_target = pending_retry
            .as_ref()
            .map(|p| p.escalation_trigger_tokens)
            .unwrap_or_else(|| {
                if matches!(
                    command_kind,
                    crate::compactor::CompactionCommandKindV1::Automatic
                        | crate::compactor::CompactionCommandKindV1::Overflow
                ) {
                    // Tiers 2 and 3 exist to write the structured
                    // session-state summary with the model. Cheap non-model
                    // relief is tier 1 (wire-side tool-output decay) and runs
                    // before pressure builds; once a pressure-driven
                    // compaction is dispatched, a deterministic count digest
                    // would silently replace the summary. Target 0 means
                    // pruning short-circuits only when it eliminates the
                    // entire fold-away set.
                    0
                } else {
                    self.escalation_trigger_tokens()
                }
            });
        // The composed snapshot check above is the live retry authority.
        // A binding/accounting/policy transition can authorize a retry with
        // unchanged transcript; the legacy transcript-only retry helper would
        // incorrectly refuse that already-admitted attempt.
        let mut attempt = compactor
            .prepare_authoritative_with_trigger(
                &self.transcript,
                summarizer.clone(),
                &self.session_id,
                &self.workspace_root,
                escalation_target,
            )
            .await;
        // A short history dominated by large tool output sits inside the
        // recent-message guard, so nothing is foldable. When pressure demanded
        // the compaction (automatic trigger or provider overflow), retry
        // keeping only the structural anchors so the window can recover.
        if matches!(attempt, Ok(None))
            && matches!(
                command_kind,
                crate::compactor::CompactionCommandKindV1::Automatic
                    | crate::compactor::CompactionCommandKindV1::Overflow
                    | crate::compactor::CompactionCommandKindV1::Manual
            )
        {
            attempt = compactor
                .prepare_authoritative_under_pressure(
                    &self.transcript,
                    summarizer.clone(),
                    &self.session_id,
                    &self.workspace_root,
                    escalation_target,
                )
                .await;
        }
        let compaction_usage = summarizer.take_usage();
        let tokens_before = u64::from(crate::compactor::estimate_transcript_tokens(
            self.transcript.messages(),
        ));
        if let Some(usage) = compaction_usage {
            self.usage.record(
                usage.prompt_tokens,
                usage.completion_tokens,
                usage.cache_read_tokens,
                usage.cache_creation_tokens,
            );
            self.exclude_summary_usage_from_pressure();
        }
        let result = match attempt {
            Ok(result) => {
                // Mechanism 6 (breaker): a completed authoritative prepare is
                // a real success for the live session's breaker, whether or
                // not a compaction was actually due (`Ok(None)` still means
                // the summarizer path -- if it ran -- did not error).
                compactor.breaker().record_success();
                result
            }
            Err(error) => {
                // The failed summary request is still billable, but it did not
                // replace the full model-facing context.
                self.usage.context_tokens = previous_context_tokens;
                if let Some(usage) = compaction_usage {
                    self.emit_compaction_usage(usage);
                }
                // Mechanism 6 (breaker), CR-17: route the failure through the
                // SAME persistent breaker every attempt for this session
                // shares, so three REAL failures in a row actually trip it.
                // `record_outcome` excludes user-cancels from the count (see
                // `compactor_breaker.rs`) -- pass the real `CompactError`
                // through unwrapped so a `Cancelled` variant is honored
                // rather than counted as a failure.
                compactor.breaker().record_outcome(&error);
                // Mechanism 7: schedule a bounded monotonic retry with a
                // strictly smaller escalation target; after
                // `MAX_COMPACTION_RETRIES` scheduled retries the ledger gives
                // up until a fresh attempt runs.
                crate::compactor::note_attempt_failed(
                    &self.session_id,
                    crate::compactor::SurfaceGeneration::observe(&self.transcript),
                    escalation_target,
                    prior_attempts,
                    Some(compaction_snapshot.clone()),
                );
                return Err(EngineError::Compaction(error.to_string()));
            }
        };
        let Some(result) = result else {
            // Close the started/compacted pair: a started event with no
            // terminal event would leave observers waiting forever.
            return Ok(CompactionOutcome {
                compacted: false,
                summary: String::new(),
                dropped_count: 0,
                archive: None,
            });
        };

        // Typed no-gain stop: a replacement that does not shrink the context
        // is refused before any archive or mutation, and recorded with its own
        // failure class so it neither trips the breaker nor loops.
        {
            let tokens_after_estimate = u64::from(crate::compactor::estimate_transcript_tokens(
                result.replacement_messages(),
            ));
            if !compaction_reduces_tokens(tokens_before, tokens_after_estimate) {
                self.usage.context_tokens = previous_context_tokens;
                if let Some(usage) = compaction_usage {
                    self.emit_compaction_usage(usage);
                }
                crate::compactor::note_attempt_failed_with_class(
                    &self.session_id,
                    crate::compactor::SurfaceGeneration::observe(&self.transcript),
                    escalation_target,
                    prior_attempts,
                    Some(compaction_snapshot.clone()),
                    crate::compactor::CompactionFailureClassV1::NoGain,
                );
                return Err(EngineError::CompactionNoGain {
                    tokens_before,
                    tokens_after: tokens_after_estimate,
                });
            }
        }
        // CR-16: capture the exact pre-compaction surface being archived plus
        // the canonical boundary it corresponds to, and build the immutable
        // projection record inside the SAME fail-closed call that archives the
        // transcript, so a sink that supports persistence stores the projection
        // atomically with the archive it describes. `current_high_water` is the
        // last canonical seq allocated before compaction began, so the
        // summarized range is [0, high_water] and the SessionCompacted marker
        // this pass emits lands strictly above it. A concurrent publisher that
        // allocates a seq after this read is excluded from the recorded range,
        // which can only UNDER-cover (a range verified against fewer rows than
        // exist) -- never over-delete.
        let pre_compaction_messages = self.transcript.messages().to_vec();
        let mut snapshot = compaction_snapshot;
        let summarized_through_seq =
            u32::try_from(self.conduit.current_high_water()).unwrap_or(u32::MAX);
        let projection = crate::compaction_archive::CompactionProjectionRecord::build(
            self.session_id.clone(),
            0,
            summarized_through_seq,
            &pre_compaction_messages,
            result.summary.clone(),
        );
        // FM-178 / CRA-11 (Z24 residual fix): build the lineage segment this
        // pass will record BEFORE calling the sink, using `result.dropped_count`
        // (the exact count of messages this compaction summarizes away) rather
        // than diffing transcript lengths after `commit_authoritative` -- so the
        // segment is ready to ride inside the SAME fail-closed `archive()` call
        // as the transcript and projection, instead of a second best-effort
        // call after the fact. That second call (`persist_lineage`, discarding
        // its own errors) was the hidden-partial-commit defect: a crash between
        // the two independent commits could leave a durable archive with no
        // lineage. One call, one transaction, all-or-nothing.
        let removed_prefix = result.removed_messages.clone();
        let start_ordinal = self
            .compaction_lineage
            .segments
            .iter()
            .map(|segment| segment.canonical_range.len() as u64)
            .sum::<u64>()
            // `CompactionHistoryPosition` uses 1-based exclusive ordinals:
            // its cutoff validator reserves zero for source metadata. Keep
            // live appends aligned with the Codex lineage contract.
            .saturating_add(1);
        let byte_length = serde_json::to_vec(&removed_prefix)
            .map(|encoded| encoded.len() as u64)
            .unwrap_or(0);
        let mut lineage_after_this_pass = self.compaction_lineage.clone();
        lineage_after_this_pass.segments.push(
            crate::compaction_archive::CompactionLineageSegment {
                session_id: self.session_id.clone(),
                canonical_range: removed_prefix,
                start_ordinal,
                end: None,
                byte_length,
            },
        );

        let failure_session = self.session_id.clone();
        let failure_generation = crate::compactor::SurfaceGeneration::observe(&self.transcript);
        let record_external_failure = |class, snapshot: &crate::compactor::CompactionSnapshotV1| {
            crate::compactor::note_attempt_failed_with_class(
                &failure_session,
                failure_generation,
                escalation_target,
                prior_attempts,
                Some(snapshot.clone()),
                class,
            );
        };
        let (federation_decision, federation_request_started_at) =
            if let Some(provider) = self.compaction_decision_provider.clone() {
                let (decision, started) = provider
                    .issue(&self.session_id, &snapshot)
                    .await
                    .map_err(|error| {
                        record_external_failure(
                            crate::compactor::CompactionFailureClassV1::FederationUnavailable,
                            &snapshot,
                        );
                        EngineError::Compaction(format!(
                            "federation admission unavailable: {error}"
                        ))
                    })?;
                snapshot = self
                    .bind_compaction_federation_revision(&snapshot, &decision)
                    .map_err(|error| {
                        record_external_failure(
                            crate::compactor::CompactionFailureClassV1::StaleSnapshot,
                            &snapshot,
                        );
                        EngineError::Compaction(error)
                    })?;
                (Some(decision), Some(started))
            } else {
                (None, None)
            };
        let archive = if let Some(sink) = self.compaction_archive_sink.clone() {
            match sink
                .archive(crate::compaction_archive::CompactionArchiveInput {
                    session_id: self.session_id.clone(),
                    created_at: now_iso(),
                    messages: pre_compaction_messages.clone(),
                    projection: Some(projection),
                    lineage: Some(lineage_after_this_pass.clone()),
                    snapshot: Some(snapshot.clone()),
                    federation_decision,
                    federation_request_started_at,
                })
                .await
            {
                Ok(receipt) => Some(receipt),
                Err(error) => {
                    record_external_failure(error.class, &snapshot);
                    self.usage.context_tokens = previous_context_tokens;
                    if let Some(usage) = compaction_usage {
                        self.emit_compaction_usage(usage);
                    }
                    return Err(EngineError::Compaction(format!(
                        "pre-compaction transcript archive failed; context retained: {error}"
                    )));
                }
            }
        } else {
            record_external_failure(
                crate::compactor::CompactionFailureClassV1::StorageUnavailable,
                &snapshot,
            );
            self.usage.context_tokens = previous_context_tokens;
            if let Some(usage) = compaction_usage {
                self.emit_compaction_usage(usage);
            }
            return Err(EngineError::Compaction(
                "pre-compaction transcript archive unavailable; context retained".into(),
            ));
        };
        // The sink committed the archive, projection, and lineage atomically;
        // only now does the in-memory lineage advance to match what is durable.
        if let Some(receipt) = archive.as_ref() {
            self.conduit
                .advance_committed_high_water(receipt.durable_high_water);
        }
        self.compaction_lineage = lineage_after_this_pass;
        let summary = result.summary.clone();
        let dropped_count = result.dropped_count;
        let retained_turns = u32::try_from(result.retained_turns).unwrap_or(u32::MAX);
        compactor.commit_authoritative(&mut self.transcript, result);
        // Compaction rewrote model-visible context: earlier "already sent"
        // file reads are gone from it, so the next read must resend contents.
        self.read_cache.clear();
        // Pending eligibility clears only after the archive sink succeeded and
        // the in-memory transcript crosses the durable commit boundary.
        crate::compactor::note_attempt_succeeded(&self.session_id);

        // HCTX-004: a committed compaction is an expected prompt-cache break
        // and the point at which host-loaded context identities from earlier
        // epochs stop being trustworthy. Advance the epoch exactly once per
        // successful compaction and report it with the event.
        let epoch = archive
            .as_ref()
            .map(|receipt| receipt.compaction_epoch)
            .filter(|epoch| *epoch > 0)
            .unwrap_or_else(|| self.compaction_epoch.saturating_add(1));
        let predecessor_epoch = self.compaction_epoch;
        self.compaction_epoch = epoch;
        let tokens_after = u64::from(crate::compactor::estimate_transcript_tokens(
            self.transcript.messages(),
        ));
        let tier = compaction_tier_label(command_kind);
        tracing::info!(
            session_id = %self.session_id,
            tier,
            tokens_before,
            tokens_after,
            epoch,
            dropped_count,
            "context compacted; expected prompt-cache break"
        );
        let sid = self.session_id.clone();
        self.emit(|seq| ProtocolEvent::SessionCompacted {
            session_id: sid,
            seq,
            created_at: now_iso(),
            summary: summary.clone(),
            dropped_count: dropped_count as i32,
            tier: tier.into(),
            tokens_before,
            tokens_after,
            epoch,
            command_id: command_id.clone(),
            retained_turns,
        });
        self.emit_compaction_lineage(
            command_kind,
            &command_id,
            epoch,
            predecessor_epoch,
            archive.as_ref().map(|receipt| receipt.archive_id.as_str()),
        );
        // The next provider response will replace this estimate with measured
        // usage. Until then, avoid immediately retriggering the same summary.
        self.usage.context_tokens = tokens_after;
        self.reset_pressure_from_rebuilt_context(tokens_after);
        if let Some(usage) = compaction_usage {
            self.emit_compaction_usage(usage);
        }
        Ok(CompactionOutcome {
            compacted: true,
            summary,
            dropped_count,
            archive,
        })
    }

    /// Current compaction epoch (HCTX-004). Zero until the first successful
    /// compaction; strictly increases by one per compaction.
    pub fn compaction_epoch(&self) -> u64 {
        self.compaction_epoch
    }

    /// Restore the epoch when a session is resumed from durable storage so the
    /// counter never moves backwards across restarts.
    pub fn restore_compaction_epoch(&mut self, epoch: u64) {
        self.compaction_epoch = self.compaction_epoch.max(epoch);
    }

    /// Typed failure signal for a compaction that did not replace the context.
    /// Carried as an orchestration observation (`compaction_failed`) rather
    /// than `runFailed`: the run itself continues with the full transcript.
    /// A042: every committed compaction links to its predecessor (previous
    /// epoch, `0` for the first) and to the archive holding the pre-compaction
    /// transcript, as a typed observation next to `SessionCompacted`.
    fn emit_compaction_lineage(
        &self,
        command_kind: crate::compactor::CompactionCommandKindV1,
        command_id: &str,
        epoch: u64,
        predecessor_epoch: u64,
        archive_id: Option<&str>,
    ) {
        let sid = self.session_id.clone();
        let fields = compaction_lineage_fields(
            &sid,
            command_kind,
            command_id,
            epoch,
            predecessor_epoch,
            archive_id,
            self.compaction_lineage.segments.len(),
        );
        self.emit(|seq| ProtocolEvent::OrchestrationObservationRecorded {
            session_id: sid.clone(),
            seq,
            created_at: now_iso(),
            observation: rightkit_agent_engine_protocol::OrchestrationObservation {
                id: format!("compaction-lineage-{command_id}"),
                kind: "compaction_lineage".to_string(),
                subject: sid.clone(),
                summary: format!("compaction epoch {epoch} follows epoch {predecessor_epoch}"),
                fields,
                created_at: now_iso(),
            },
        });
    }

    /// A043: typed outcome of a reactive provider-overflow recovery cycle.
    pub(crate) fn emit_overflow_recovery_outcome(
        &self,
        outcome: crate::overflow_recovery::OverflowRecoveryOutcome,
        error: &str,
    ) {
        let sid = self.session_id.clone();
        self.emit(|seq| ProtocolEvent::OrchestrationObservationRecorded {
            session_id: sid.clone(),
            seq,
            created_at: now_iso(),
            observation: rightkit_agent_engine_protocol::OrchestrationObservation {
                id: format!("overflow-recovery-{seq}"),
                kind: "overflow_recovery".to_string(),
                subject: sid.clone(),
                summary: format!("context overflow recovery: {}", outcome.as_str()),
                fields: std::collections::HashMap::from([
                    ("sessionId".to_string(), sid.clone()),
                    ("outcome".to_string(), outcome.as_str().to_string()),
                    ("epoch".to_string(), self.compaction_epoch.to_string()),
                    ("error".to_string(), error.to_string()),
                ]),
                created_at: now_iso(),
            },
        });
    }

    fn emit_compaction_failed(
        &self,
        command_kind: crate::compactor::CompactionCommandKindV1,
        command_id: &str,
        reason: &str,
    ) {
        tracing::warn!(
            session_id = %self.session_id,
            tier = compaction_tier_label(command_kind),
            reason,
            "compaction failed; full transcript retained"
        );
        let sid = self.session_id.clone();
        self.emit(|seq| ProtocolEvent::OrchestrationObservationRecorded {
            session_id: sid.clone(),
            seq,
            created_at: now_iso(),
            observation: rightkit_agent_engine_protocol::OrchestrationObservation {
                id: format!("compaction-failed-{command_id}"),
                kind: "compaction_failed".to_string(),
                subject: sid.clone(),
                summary: format!("compaction failed; context retained: {reason}"),
                fields: std::collections::HashMap::from([
                    ("sessionId".to_string(), sid.clone()),
                    ("commandId".to_string(), command_id.to_string()),
                    (
                        "tier".to_string(),
                        compaction_tier_label(command_kind).to_string(),
                    ),
                    ("epoch".to_string(), self.compaction_epoch.to_string()),
                    ("reason".to_string(), reason.to_string()),
                ]),
                created_at: now_iso(),
            },
        });
    }

    fn emit_compaction_usage(&mut self, usage: crate::compactor::CompactionUsage) {
        let sid = self.session_id.clone();
        self.emit(|seq| ProtocolEvent::UsageUpdated {
            session_id: sid,
            seq,
            created_at: now_iso(),
            prompt_tokens: usage.prompt_tokens as i32,
            completion_tokens: usage.completion_tokens as i32,
            cost_usd: self.usage.cost_usd,
            context_tokens: self.usage.context_tokens as i32,
            context_percent: self.usage.context_percent(0),
            cache_read_tokens: self.usage.cache_read_tokens as i32,
            cache_creation_tokens: self.usage.cache_creation_tokens as i32,
            cost_known: self.usage.is_priced(),
        });
        // The summarizer is a real model call. If it pushes this session over
        // its ceiling, surface that immediately; the updated UsageAccount then
        // prevents every subsequent model or compaction call.
        if self.usage.exceeds_budget(self.budget_ceiling_usd) {
            self.emit_budget_exceeded();
        }
    }

    fn emit_budget_exceeded(&self) {
        let sid = self.session_id.clone();
        self.emit(|seq| ProtocolEvent::BudgetExceeded {
            session_id: sid,
            seq,
            created_at: now_iso(),
            scope: "session".into(),
            current_usd: self.usage.cost_usd,
            ceiling_usd: self.budget_ceiling_usd,
            hint: "raise the ceiling to continue".into(),
        });
    }

    pub fn set_system_prompt(&mut self, prompt: impl Into<String>) {
        self.transcript.push_system(prompt);
    }

    pub(super) fn add_triggered_skill_hint(
        &mut self,
        event: &str,
        enrich: impl FnOnce(&mut rightkit_agent_host_traits::TriggerContext),
    ) {
        let Some(hint) =
            triggered_skill_hint(&self.skill_dirs, &self.workspace_root, None, event, enrich)
        else {
            return;
        };
        // Emit each distinct hint at most once per transcript. This ran on
        // every UserPromptSubmit and every PreToolUse, appending a new pinned
        // System message each time: the model was told to `invoke_skill` (a
        // deferred tool, so a `tool_search` call first) on every turn, and
        // each appended system message mutated the provider's cached system
        // prefix.
        let already_hinted = self.transcript.messages().iter().any(|message| {
            matches!(
                message,
                rightkit_agent_engine_protocol::TranscriptMessage::System { content, .. } if *content == hint
            )
        });
        if already_hinted {
            return;
        }
        self.transcript.push_system(hint);
    }

    /// Seed prior conversation (rehydrated from the event log) as context so a
    /// resumed session continues where it left off. Appends after any system
    /// prompt that's already been set.
    pub fn seed_transcript(
        &mut self,
        messages: Vec<rightkit_agent_engine_protocol::TranscriptMessage>,
    ) {
        if messages.is_empty() {
            return;
        }
        for message in &messages {
            if let rightkit_agent_engine_protocol::TranscriptMessage::Tool {
                trust: Some(trust),
                ..
            } = message
            {
                if tool_dispatch::tool_result_carries_content(&trust.source) {
                    self.content_provenance
                        .absorb(tool_dispatch::tool_source_provenance(&trust.source));
                }
            }
        }
        let mut all = self.transcript.messages().to_vec();
        all.extend(messages);
        self.transcript.replace(all);
    }

    /// Remove the last user message and everything after it from the transcript.
    /// Returns the text of the removed message, or None if there was nothing to rewind.
    pub fn trim_before_last_user(&mut self) -> Option<String> {
        self.transcript.trim_before_last_user()
    }

    /// Rewind to an earlier user prompt: drop it and everything after it from
    /// the active context (see [`Transcript::trim_before_user_matching`]).
    pub fn trim_before_user_matching(
        &mut self,
        matches: impl Fn(&str) -> bool,
        nth_from_end: usize,
    ) -> Option<String> {
        self.transcript
            .trim_before_user_matching(matches, nth_from_end)
    }

    /// Rewind that keeps what it removed: the chosen prompt and everything
    /// after it, for a later [`Self::restore_messages`].
    pub fn take_tail_before_user_matching(
        &mut self,
        matches: impl Fn(&str) -> bool,
        nth_from_end: usize,
    ) -> Option<Vec<TranscriptMessage>> {
        self.transcript
            .take_tail_from_user_matching(matches, nth_from_end)
    }

    /// Put back messages a rewind removed (redo).
    pub fn restore_messages(&mut self, messages: Vec<TranscriptMessage>) {
        self.transcript.extend_messages(messages);
    }

    pub(super) fn emit(&self, make: impl FnOnce(i64) -> ProtocolEvent) {
        self.conduit.emit(make);
    }

    /// Emit the per-response `turnUsage` event. Provider adapters normalize
    /// `prompt` to EXCLUDE cached input, so the full prompt is
    /// `prompt + cache_read + cache_creation`. The context occupied after the
    /// call is that plus the output, measured against the active model window.
    pub(super) fn emit_turn_usage(
        &self,
        sid: &str,
        prompt: u32,
        cache_read: u32,
        cache_creation: u32,
        output: u32,
        reasoning: u32,
    ) {
        let input_total = prompt as i64 + cache_read as i64 + cache_creation as i64;
        let window = self.usage.window_tokens as i64;
        let used = input_total + output as i64;
        let pct = if window > 0 {
            ((used as f64 / window as f64) * 100.0).min(100.0)
        } else {
            0.0
        };
        let session_id = sid.to_string();
        self.emit(|seq| ProtocolEvent::TurnUsage {
            session_id,
            seq,
            created_at: now_iso(),
            input_tokens: input_total,
            cached_input_tokens: cache_read as i64,
            cache_creation_tokens: cache_creation as i64,
            output_tokens: output as i64,
            reasoning_tokens: reasoning as i64,
            context_window: window,
            context_used_tokens: used,
            context_used_pct: pct,
            cache_hit_pct: crate::usage::cache_hit_rate_pct(
                u64::from(prompt),
                u64::from(cache_read),
                u64::from(cache_creation),
            ),
            session_cache_hit_pct: self.usage.cache_hit_rate_pct(),
        });
    }
}

/// `true` when a replacement of `after` tokens is a strict reduction of the
/// `before` context. Equal or larger replacements are no-gain.
fn compaction_reduces_tokens(before: u64, after: u64) -> bool {
    after < before
}
