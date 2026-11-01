// Test-only stylistic lints: fixture `Default::default()` field-assignment and
// nested `if let` matching are clearer as written in test code.
#![allow(clippy::field_reassign_with_default, clippy::collapsible_match)]
use super::*;
use crate::engine::engine_helpers::STABLE_SYSTEM_KERNEL_MAX_BYTES;
use crate::llm::{
    LlmClient, LlmRequestMetadata, LlmResponse, LlmResponseStream, LlmStreamEvent, ScriptedLlm,
};
use crate::self_review::{RestrictedSelfReviewFork, SelfReviewInput, SELF_REVIEW_ALLOWED_TOOLS};
use rightkit_agent_policy::{Rule, RuleSource, RuleStore};
use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, RwLock,
};

fn compaction_federation_decision(
    identity: &str,
) -> rightkit_agent_engine_protocol::CompactionFederationDecisionV1 {
    rightkit_agent_engine_protocol::CompactionFederationDecisionV1 {
        schema: rightkit_agent_engine_protocol::event::COMPACTION_FEDERATION_SCHEMA_V1.to_owned(),
        identity: identity.to_owned(),
        fence: "fence-1".to_owned(),
        scope: "session-1".to_owned(),
        decision: "approved".to_owned(),
        selection_receipt: rightkit_agent_engine_protocol::CompactionFederationSelectionReceiptV1 {
            selected_item_ids: vec!["item-1".to_owned()],
            selection_hash: "selection-hash-1".to_owned(),
        },
        issued_monotonic_elapsed_ms: 10,
        expires_after_elapsed_ms: 1_000,
        restart_epoch: 1,
    }
}

struct RecordingReviewFork {
    tx: tokio::sync::mpsc::UnboundedSender<SelfReviewInput>,
}

impl RestrictedSelfReviewFork for RecordingReviewFork {
    fn review_and_propose(&self, input: SelfReviewInput) {
        let _ = self.tx.send(input);
    }
}

#[tokio::test]
async fn restricted_self_review_fork_runs_after_configured_visible_turns() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let fork = Arc::new(RecordingReviewFork { tx });
    let llm = Arc::new(ScriptedLlm::new(vec![LlmResponse::text(
        "Remember: prefer quarantine proposals before live memory writes.",
    )]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        ..Default::default()
    };
    let mut engine = SessionEngine::new(llm, tools, conduit, cfg);
    engine.set_self_review_fork(fork);
    engine.set_self_review_turn_interval(1);
    engine.set_system_prompt("stable parent system prompt");

    let out = engine.submit_user_message("finish").await.unwrap();
    assert_eq!(
        out,
        "Remember: prefer quarantine proposals before live memory writes."
    );

    let input = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(input.model, "haiku");
    assert_eq!(
        input.allowed_tools,
        SELF_REVIEW_ALLOWED_TOOLS
            .iter()
            .map(|tool| (*tool).to_string())
            .collect::<Vec<_>>()
    );
    assert!(!input.shell_allowed);
    assert!(input.is_governed());
    assert!(input.stable_system_cache_key.is_some());
    assert_eq!(
        String::from_utf8(input.stable_system_prompt_bytes).unwrap(),
        "stable parent system prompt\n"
    );
}

#[tokio::test]
async fn restricted_self_review_fork_waits_for_default_turn_gate() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let fork = Arc::new(RecordingReviewFork { tx });
    let llm = Arc::new(ScriptedLlm::new(vec![LlmResponse::text("done")]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        ..Default::default()
    };
    let mut engine = SessionEngine::new(llm, tools, conduit, cfg);
    engine.set_self_review_fork(fork);

    assert_eq!(engine.submit_user_message("finish").await.unwrap(), "done");
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn governed_engine_path_denies_shell_guardrail_before_execution() {
    let call = ToolCall {
        id: "self-review-shell".into(),
        name: "shell".into(),
        arguments_json: serde_json::json!({"command": ":(){ :|:& };:"}).to_string(),
    };
    let (mut engine, _conduit) = engine_with(
        vec![
            LlmResponse::default().with_tool_call(call),
            LlmResponse::text("shell denied"),
        ],
        PermissionMode::Bypass,
    );

    assert_eq!(
        engine
            .submit_user_message("review this result")
            .await
            .unwrap(),
        "shell denied"
    );
    assert!(engine.transcript().messages().iter().any(|message| {
        matches!(
            message,
            TranscriptMessage::Tool {
                summary,
                is_error: true,
                ..
            } if summary.contains("guardrail.shell.fork_bomb")
        )
    }));
}

fn engine_with(responses: Vec<LlmResponse>, mode: PermissionMode) -> (SessionEngine, Arc<Conduit>) {
    let _ = rightkit_agent_tools::coding_tools::process_resource_authority::install_process_resource_profile(
        rightkit_agent_tools::coding_tools::process_resource_authority::ResourceProfile::Expanded,
    );
    let llm = Arc::new(ScriptedLlm::new(responses));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: mode,
        ..Default::default()
    };
    let engine = SessionEngine::new(llm, tools, conduit.clone(), cfg);
    (engine, conduit)
}

#[test]
fn observe_host_revision_clears_snapshot_slot_when_value_becomes_unavailable() {
    let (mut engine, _) = engine_with(Vec::new(), PermissionMode::Bypass);
    let feed = Arc::new(RwLock::new(engine.compaction_snapshot()));
    engine.set_compaction_snapshot_feed(feed.clone());

    engine.observe_host_revision("pin", Some("pin-a"));
    assert_eq!(feed.read().unwrap().pin_revision, Some(1));

    engine.observe_host_revision("pin", None);
    assert_eq!(feed.read().unwrap().pin_revision, None);
}

#[test]
fn observe_host_revision_advances_after_a_to_b_to_a_without_reusing_revision() {
    let (mut engine, _) = engine_with(Vec::new(), PermissionMode::Bypass);
    let feed = Arc::new(RwLock::new(engine.compaction_snapshot()));
    engine.set_compaction_snapshot_feed(feed.clone());

    engine.observe_host_revision("pin", Some("pin-a"));
    let first = feed.read().unwrap().pin_revision.unwrap();
    engine.observe_host_revision("pin", Some("pin-b"));
    let second = feed.read().unwrap().pin_revision.unwrap();
    engine.observe_host_revision("pin", Some("pin-a"));
    let third = feed.read().unwrap().pin_revision.unwrap();

    assert!(first < second && second < third);
}

#[test]
fn observe_host_revision_notifies_only_for_actual_changes() {
    let (mut engine, _) = engine_with(Vec::new(), PermissionMode::Bypass);
    let feed = Arc::new(RwLock::new(engine.compaction_snapshot()));
    let notifications = Arc::new(AtomicUsize::new(0));
    let observed = notifications.clone();
    engine.set_compaction_snapshot_feed(feed.clone());
    engine.set_compaction_revision_notifier(Arc::new(move || {
        let snapshot = feed
            .try_read()
            .expect("notifier must run after feed lock release");
        let index = observed.fetch_add(1, Ordering::SeqCst);
        assert_eq!(snapshot.accounting_revision.is_some(), index == 0);
    }));

    engine.observe_host_revision("accounting", Some("usage-a"));
    engine.observe_host_revision("accounting", Some("usage-a"));
    engine.observe_host_revision("accounting", None);
    engine.observe_host_revision("accounting", None);

    assert_eq!(notifications.load(Ordering::SeqCst), 2);
}

#[test]
fn stale_federation_binding_refuses_without_mutating_snapshot() {
    let (mut engine, _) = engine_with(Vec::new(), PermissionMode::Bypass);
    let feed = Arc::new(RwLock::new(engine.compaction_snapshot()));
    engine.set_compaction_snapshot_feed(feed.clone());
    let expected = feed.read().unwrap().clone();
    engine.observe_host_revision("pin", Some("pin-a"));
    let before = feed.read().unwrap().clone();

    let result = engine.bind_compaction_federation_revision(
        &expected,
        &compaction_federation_decision("decision-stale"),
    );

    assert!(result.is_err());
    assert_eq!(*feed.read().unwrap(), before);
}

#[test]
fn matching_federation_binding_sets_canonical_federation_revision() {
    let (mut engine, _) = engine_with(Vec::new(), PermissionMode::Bypass);
    let feed = Arc::new(RwLock::new(engine.compaction_snapshot()));
    engine.set_compaction_snapshot_feed(feed.clone());
    let expected = feed.read().unwrap().clone();

    let bound = engine
        .bind_compaction_federation_revision(
            &expected,
            &compaction_federation_decision("decision-matching"),
        )
        .unwrap();

    assert_eq!(bound.federation_revision, Some(1));
    assert_eq!(feed.read().unwrap().federation_revision, Some(1));
}

fn engine_with_rules(
    responses: Vec<LlmResponse>,
    mode: PermissionMode,
    rules: Arc<Mutex<RuleStore>>,
) -> (SessionEngine, Arc<Conduit>) {
    let llm = Arc::new(ScriptedLlm::new(responses));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: mode,
        permission_rules: Some(rules),
        ..Default::default()
    };
    let engine = SessionEngine::new(llm, tools, conduit.clone(), cfg);
    (engine, conduit)
}

struct CapturingLlm {
    responses: Mutex<VecDeque<LlmResponse>>,
    calls: Mutex<Vec<Vec<TranscriptMessage>>>,
}

impl CapturingLlm {
    fn new(responses: Vec<LlmResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().collect()),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<Vec<TranscriptMessage>> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl LlmClient for CapturingLlm {
    async fn complete(&self, messages: &[TranscriptMessage]) -> Result<LlmResponse, String> {
        self.calls.lock().unwrap().push(messages.to_vec());
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| "CapturingLlm: no more scripted responses".to_string())
    }
}

struct BoundaryFixtureTool(&'static str);

#[async_trait::async_trait]
impl rightkit_agent_tools::coding_tools::Tool for BoundaryFixtureTool {
    fn name(&self) -> &str {
        self.0
    }
    fn description(&self) -> &str {
        "HCTX boundary fixture tool"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    async fn execute(
        &self,
        _args: serde_json::Value,
        _ctx: &rightkit_agent_tools::coding_tools::ToolContext,
    ) -> rightkit_agent_tools::coding_tools::ToolResult {
        rightkit_agent_tools::coding_tools::ToolResult::ok("ok")
    }
}

#[derive(Clone)]
struct BoundaryRequestCapture {
    messages: Vec<TranscriptMessage>,
    metadata: LlmRequestMetadata,
    tool_names: Vec<String>,
}

struct BoundaryStreamingLlm {
    calls: std::sync::atomic::AtomicUsize,
    captures: Mutex<Vec<BoundaryRequestCapture>>,
    active_tools: Mutex<Arc<ToolRegistry>>,
    first_started: Arc<tokio::sync::Notify>,
    release_first: Arc<tokio::sync::Notify>,
}

impl BoundaryStreamingLlm {
    fn new(tools: Arc<ToolRegistry>) -> Self {
        Self {
            calls: std::sync::atomic::AtomicUsize::new(0),
            captures: Mutex::new(Vec::new()),
            active_tools: Mutex::new(tools),
            first_started: Arc::new(tokio::sync::Notify::new()),
            release_first: Arc::new(tokio::sync::Notify::new()),
        }
    }

    fn captures(&self) -> Vec<BoundaryRequestCapture> {
        self.captures.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl LlmClient for BoundaryStreamingLlm {
    async fn complete(&self, _messages: &[TranscriptMessage]) -> Result<LlmResponse, String> {
        Err("boundary fixture requires streaming".into())
    }

    async fn stream_with_metadata(
        &self,
        messages: &[TranscriptMessage],
        metadata: &LlmRequestMetadata,
    ) -> Result<LlmResponseStream, String> {
        use futures::StreamExt;
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut tool_names = self
            .active_tools
            .lock()
            .unwrap()
            .all()
            .iter()
            .map(|tool| tool.name().to_string())
            .collect::<Vec<_>>();
        tool_names.sort();
        self.captures.lock().unwrap().push(BoundaryRequestCapture {
            messages: messages.to_vec(),
            metadata: metadata.clone(),
            tool_names,
        });
        if call == 0 {
            let started = self.first_started.clone();
            let release = self.release_first.clone();
            let first = futures::stream::once(async move {
                started.notify_one();
                release.notified().await;
                Ok(LlmStreamEvent::ToolCall(ToolCall {
                    id: "boundary-call-1".into(),
                    name: "boundary_noop".into(),
                    arguments_json: "{}".into(),
                }))
            });
            let finished = futures::stream::iter(vec![Ok(LlmStreamEvent::Finished(Some(
                "tool_calls".into(),
            )))]);
            Ok(Box::pin(first.chain(finished)))
        } else {
            Ok(Box::pin(futures::stream::iter(vec![
                Ok(LlmStreamEvent::Text("done".into())),
                Ok(LlmStreamEvent::Finished(Some("stop".into()))),
            ])))
        }
    }

    fn replace_tool_registry(&self, tools: Arc<ToolRegistry>) {
        *self.active_tools.lock().unwrap() = tools;
    }
}

struct NeverCompletesLlm;

#[async_trait::async_trait]
impl LlmClient for NeverCompletesLlm {
    async fn complete(&self, _messages: &[TranscriptMessage]) -> Result<LlmResponse, String> {
        std::future::pending().await
    }
}

#[tokio::test]
#[ignore = "HCTX-002 final qualification: run the active-stream host-boundary driver"]
async fn qualification_hctx002_active_stream_promotes_host_state_only_on_next_request() {
    use crate::provider_turn_boundary::{BoundaryTextUpdate, ProviderBoundaryUpdate};

    let mut initial_registry = ToolRegistry::new();
    initial_registry.register(Box::new(BoundaryFixtureTool("boundary_noop")));
    let initial_registry = Arc::new(initial_registry);
    let llm = Arc::new(BoundaryStreamingLlm::new(initial_registry.clone()));
    let conduit = Arc::new(Conduit::new(64));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        ..Default::default()
    };
    let mut engine = SessionEngine::new(llm.clone(), initial_registry, conduit, cfg);
    engine.set_system_prompt("stable-system");
    let (boundary_tx, boundary_rx) = crate::provider_turn_boundary::provider_boundary_channel(16);
    engine.set_provider_boundary_receiver(boundary_rx);
    let (steer_tx, steer_rx) = tokio::sync::mpsc::channel(8);
    engine.set_steer_receiver(steer_rx);

    let task = tokio::spawn(async move { engine.submit_user_message("start").await });
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        llm.first_started.notified(),
    )
    .await
    .expect("first stream becomes active");

    let prompt = BoundaryTextUpdate::new(1, "prompt-next");
    let context = BoundaryTextUpdate::new(1, "context-next");
    let world = BoundaryTextUpdate::new(1, "world-next");
    let expected_context_fingerprint = {
        let lineage = format!(
            "prompt:1:{}\ncontext:1:{}\nworld_state:1:{}",
            prompt.fingerprint, context.fingerprint, world.fingerprint,
        );
        let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
        hasher.update(lineage.as_bytes());
        format!("sha256:{}", hasher.finalize())
    };

    boundary_tx.send_latest(ProviderBoundaryUpdate::WorldState(world));
    boundary_tx.send_latest(ProviderBoundaryUpdate::Prompt(prompt));
    boundary_tx.send_latest(ProviderBoundaryUpdate::Context(context));
    let mut next_registry = ToolRegistry::new();
    next_registry.register(Box::new(BoundaryFixtureTool("boundary_noop")));
    next_registry.register(Box::new(BoundaryFixtureTool("boundary_beta")));
    boundary_tx.send_latest(ProviderBoundaryUpdate::ToolCatalog {
        revision: 1,
        tools: Arc::new(next_registry),
    });
    steer_tx.send("steer-next".into()).await.unwrap();

    let first = llm
        .captures()
        .into_iter()
        .next()
        .expect("first request captured");
    assert_eq!(first.metadata.provider_boundary_generation, Some(0));
    assert!(first.metadata.context_fingerprint.is_none());
    assert_eq!(first.tool_names, vec!["boundary_noop"]);
    assert!(!first
        .messages
        .iter()
        .any(|message| format!("{message:?}").contains("-next")));

    llm.release_first.notify_one();
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .expect("qualification fixture finishes")
        .unwrap()
        .unwrap();
    assert_eq!(result, "done");
    let captures = llm.captures();
    assert_eq!(captures.len(), 2);
    let next = &captures[1];
    assert_eq!(next.metadata.provider_boundary_generation, Some(1));
    assert_eq!(
        next.metadata.context_fingerprint.as_deref(),
        Some(expected_context_fingerprint.as_str())
    );
    assert_eq!(next.tool_names, vec!["boundary_beta", "boundary_noop"]);

    let system = next
        .messages
        .iter()
        .filter_map(|message| match message {
            TranscriptMessage::System { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        &system[..4],
        &["stable-system", "prompt-next", "context-next", "world-next"]
    );
    for expected in ["prompt-next", "context-next", "world-next"] {
        assert_eq!(
            system
                .iter()
                .filter(|content| **content == expected)
                .count(),
            1
        );
    }
    assert_eq!(
        next.messages
            .iter()
            .filter(|message| match message {
                TranscriptMessage::User { content, .. } => content.contains("steer-next"),
                _ => false,
            })
            .count(),
        1
    );
}

#[tokio::test]
async fn interrupt_aborts_pending_stream_headers() {
    struct PendingHeaders(tokio::sync::Notify);

    #[async_trait::async_trait]
    impl LlmClient for PendingHeaders {
        async fn complete(&self, _: &[TranscriptMessage]) -> Result<LlmResponse, String> {
            panic!("streaming request must not fall back to completion");
        }

        async fn stream_with_metadata(
            &self,
            _: &[TranscriptMessage],
            _: &LlmRequestMetadata,
        ) -> Result<LlmResponseStream, String> {
            self.0.notify_one();
            std::future::pending().await
        }
    }

    let llm = Arc::new(PendingHeaders(tokio::sync::Notify::new()));
    let mut engine = SessionEngine::new(
        llm.clone(),
        Arc::new(ToolRegistry::build_default()),
        Arc::new(Conduit::new(32)),
        SessionEngineConfig::default(),
    );
    let (interrupt_tx, interrupt_rx) = tokio::sync::watch::channel(false);
    engine.set_interrupt_receiver(interrupt_rx);
    let task = tokio::spawn(async move { engine.submit_user_message("work").await });
    tokio::time::timeout(std::time::Duration::from_secs(5), llm.0.notified())
        .await
        .expect("provider must reach stream opening before cancellation");
    interrupt_tx.send(true).unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), task)
        .await
        .expect("pending response headers must be interruptible")
        .unwrap();
    assert!(matches!(result, Err(EngineError::Interrupted)));
}

#[tokio::test]
async fn interrupt_aborts_an_inflight_llm_request() {
    let llm = Arc::new(NeverCompletesLlm);
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(32));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        ..Default::default()
    };
    let mut engine = SessionEngine::new(llm, tools, conduit, cfg);
    let (interrupt_tx, interrupt_rx) = tokio::sync::watch::channel(false);
    engine.set_interrupt_receiver(interrupt_rx);

    let task = tokio::spawn(async move { engine.submit_user_message("work").await });
    tokio::task::yield_now().await;
    interrupt_tx.send(true).unwrap();

    let result = tokio::time::timeout(std::time::Duration::from_secs(1), task)
        .await
        .expect("in-flight provider call should be interruptible")
        .unwrap();
    assert!(matches!(result, Err(EngineError::Interrupted)));
}

fn png_1x1_bytes() -> &'static [u8] {
    &[
        0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, b'I', b'H', b'D',
        b'R', 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89,
    ]
}

fn is_structured_view_image_user_message(message: &TranscriptMessage) -> bool {
    let TranscriptMessage::User { content, .. } = message else {
        return false;
    };
    let Ok(parts) = serde_json::from_str::<serde_json::Value>(content) else {
        return false;
    };
    let Some(parts) = parts.as_array() else {
        return false;
    };
    parts.iter().any(|part| {
        part.get("type").and_then(|value| value.as_str()) == Some("image_url")
            && part
                .get("image_url")
                .and_then(|image| image.get("url"))
                .and_then(|url| url.as_str())
                .is_some_and(|url| url.starts_with("data:image/png;base64,"))
    })
}

#[path = "engine_tests_basic.rs"]
mod engine_tests_basic;
#[path = "engine_tests_execution.rs"]
mod engine_tests_execution;
#[path = "engine_tests_guardian.rs"]
mod engine_tests_guardian;
#[path = "engine_tests_iteration_budget.rs"]
mod engine_tests_iteration_budget;
#[path = "engine_tests_permissions_a.rs"]
mod engine_tests_permissions_a;
#[path = "engine_tests_permissions_b.rs"]
mod engine_tests_permissions_b;
#[path = "engine_tests_skills_a.rs"]
mod engine_tests_skills_a;
#[path = "engine_tests_skills_b.rs"]
mod engine_tests_skills_b;
#[path = "engine_tests_transcript.rs"]
mod engine_tests_transcript;
#[path = "engine_tests_workers_goals.rs"]
mod engine_tests_workers_goals;

/// Three-tier prefix-cache discipline integration test (FEATURE-MATRIX row 152).
///
/// Verifies the Hermes-split invariants end-to-end through `SessionEngine`:
///   1. stable_system_cache_key is unchanged across two turns on the same day
///      with differing volatile content;
///   2. changing the day breaks the stable cache;
///   3. the self-review background fork inherits the parent's stable key verbatim.
///
/// Day is threaded via `SessionEngineConfig::day_key` (overridden here) so no
/// test path reads the wall clock.
#[tokio::test]
async fn stable_system_cache_key_three_tier_split_invariants() {
    use crate::cache::StableCacheKey;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let fork = Arc::new(RecordingReviewFork { tx });
    let llm = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::text("turn-1 done"),
        LlmResponse::text("turn-2 done"),
        LlmResponse::text("turn-3 done"),
    ]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));

    // Pre-set the day so the test never reads the clock. Lock to Monday.
    let day_a = "2026-06-30".to_string();
    let day_b = "2026-07-01".to_string();
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        day_key: day_a.clone(),
        ..Default::default()
    };
    let mut engine = SessionEngine::new(llm, tools, conduit, cfg);
    engine.set_self_review_fork(fork);
    engine.set_self_review_turn_interval(1);
    engine.set_system_prompt("stable agent identity v1");

    // ---- Turn 1 ----------------------------------------------------------
    assert_eq!(
        engine.submit_user_message("turn-1 user msg").await.unwrap(),
        "turn-1 done"
    );
    let input1 = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let key1 = input1
        .stable_system_cache_key
        .clone()
        .expect("pinned system prompt → key");

    // ---- Turn 2 (same day, DIFFERENT volatile content) -------------------
    // The volatile tier (memory + day timestamp) is built per turn but its
    // material MUST NOT enter the stable-tier cache key.
    assert_eq!(
        engine.submit_user_message("turn-2 user msg").await.unwrap(),
        "turn-2 done"
    );
    let input2 = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let key2 = input2
        .stable_system_cache_key
        .clone()
        .expect("pinned system prompt → key");

    // (1) Hermes invariant: stable key unchanged across turns on the same day
    //     with differing volatile/user content. The fork must carry the SAME key.
    assert_eq!(
        key1, key2,
        "stable_system_cache_key must be invariant across turns on the same day (volatile content differs)"
    );
    assert_ne!(
        input1.transcript.len(),
        input2.transcript.len(),
        "transcripts must grow across turns — otherwise no volatile churn actually happened"
    );

    // (3) Fork inheritance: fork's stable_system_cache_key equals a key
    //     derived from the parent's day plus its own stable-system-prompt
    //     bytes. That means the fork CAN cache-read against the parent's slot.
    let expected_key_on_day_a = StableCacheKey::new(
        String::from_utf8_lossy(&input2.stable_system_prompt_bytes).into_owned(),
        &day_a,
    )
    .compute();
    assert_eq!(
        key2, expected_key_on_day_a,
        "fork carries parent's stable key verbatim — key matches recomputation from transcript bytes"
    );

    // (4) PRODUCTION serialization contract (P2-6): the stable-system-prompt
    //     bytes the real engine fork serialized over the real post-turn
    //     transcript must equal the pinned-System prefix of that same
    //     transcript — never a synthetic reordering, and the volatile overlay
    //     (the per-turn goal reminder, unpinned user content) must not enter
    //     it. This asserts the stable → history → volatile ordering on the
    //     actual engine output, not on hand-built messages.
    let stable_prefix = pinned_system_prefix_as_stable(&input2.transcript, &day_a)
        .expect("the fork's real transcript carries pinned system messages");
    let serialized_prefix = stable_prefix.identity.as_str();
    assert_eq!(
        serialized_prefix,
        String::from_utf8_lossy(&input2.stable_system_prompt_bytes),
        "production serialization: stable_system_prompt_bytes must be exactly the pinned-System prefix of the real transcript"
    );

    // ---- Day boundary (turn 3) ------------------------------------------
    // Flip the day bucket. The same stable prompt now produces a DIFFERENT
    // key (cache breaks on day change).
    engine.day_key = day_b.clone();
    assert_eq!(
        engine.submit_user_message("turn-3 user msg").await.unwrap(),
        "turn-3 done"
    );
    let input3 = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let key3 = input3
        .stable_system_cache_key
        .clone()
        .expect("pinned system prompt → key");

    // (2) Day change breaks the cache even though the stable identity is
    //     identical.
    assert_ne!(
        key2, key3,
        "changing the UTC day must break the stable cache key on the new turn"
    );
    assert_eq!(
        key3,
        StableCacheKey::new(
            String::from_utf8_lossy(&input3.stable_system_prompt_bytes).into_owned(),
            &day_b,
        )
        .compute(),
        "the new-day key must be a re-derivation with the new day bucket, not a coincidental match"
    );
}

/// P2-6: the stable-prefix → history → volatile-overlay serialization is a
/// TESTED CONTRACT, independent of which channel-priority measurement wins on
/// a given provider/model route. It pins three structural properties:
///
/// 1. Only pinned System messages serialize into the stable prefix, and the
///    walk STOPS at the first non-System message — history begins exactly
///    there, so the stable tier is a true prefix of the wire list.
/// 2. The prefix bytes are identical no matter which serializer derives them
///    (`pinned_system_prefix_as_stable` and `stable_system_prompt_bytes`),
///    so a cache key computed from either matches.
/// 3. Volatile overlay (unpinned System, memory, per-turn reminders) serializes
///    AFTER every cache-stable field and never enters the key.
///
/// This is the contract P2-6's DoD names ("stable-prefix, history, and
/// volatile-overlay serialization is a tested contract"); the measurement
/// matrix that decides channel placement per route is a separate artifact.
#[test]
fn prompt_channel_serialization_contract_three_tiers() {
    use rightkit_agent_engine_protocol::TranscriptMessage;

    fn sys(id: &str, content: &str, pinned: bool) -> TranscriptMessage {
        TranscriptMessage::System {
            id: id.into(),
            created_at: "t".into(),
            content: content.into(),
            pinned,
        }
    }
    fn user(id: &str, content: &str) -> TranscriptMessage {
        TranscriptMessage::user(id, "t", content, None, false)
    }

    // Stable tier: two pinned system messages. History: a user turn. Volatile
    // overlay: an unpinned system reminder the engine prepends per turn.
    let stable_1 = sys("s1", "kernel: safety rules", true);
    let stable_2 = sys("s2", "kernel: deny list", true);
    let history = user("u1", "hello");
    let volatile = sys("vol", "memory: turn 7 reminder", false);

    let wire = vec![
        stable_1.clone(),
        stable_2.clone(),
        history.clone(),
        volatile.clone(),
    ];

    // (1) The stable prefix is exactly the leading pinned System messages and
    //     stops at the first non-System message (the user turn).
    let stable = pinned_system_prefix_as_stable(&wire, "2026-06-30")
        .expect("pinned system messages must produce a stable key");
    let expected = "kernel: safety rules\nkernel: deny list\n".to_string();
    assert_eq!(
        stable.identity,
        expected,
        "stable prefix must be exactly the leading pinned system messages, stopping at the first non-System message"
    );

    // (2) Both serializers agree byte-for-byte on the stable tier.
    let bytes = crate::self_review::stable_system_prompt_bytes(&wire);
    assert_eq!(
        String::from_utf8_lossy(&bytes),
        expected,
        "stable_system_prompt_bytes must serialize the same stable tier"
    );

    // (3) Volatile overlay never enters the key: drop the unpinned system
    //     reminder and the stable key is unchanged.
    let without_volatile = vec![stable_1.clone(), stable_2.clone(), history.clone()];
    let key_with = stable.compute();
    let key_without = pinned_system_prefix_as_stable(&without_volatile, "2026-06-30")
        .expect("same pinned prefix")
        .compute();
    assert_eq!(
        key_with, key_without,
        "volatile overlay must never change the stable cache key"
    );

    // The volatile overlay DOES serialize after the stable tier in the wire
    // list (volatile fields after every cache-stable field, invariant AF) —
    // it is present in the message list, just not in the key.
    assert!(
        wire.iter().any(|m| m.id() == "vol"),
        "volatile overlay must remain resident after the stable prefix"
    );
}

/// AE/P2-6: the stable system kernel has an explicit deterministic byte
/// ceiling equivalent to the engine's <=3k-token chars/4 budget. Oversized
/// prefixes disable the cache identity rather than being silently truncated.
#[test]
fn stable_system_kernel_budget_is_enforced_on_real_wire_prefix() {
    use rightkit_agent_engine_protocol::TranscriptMessage;

    let oversized = TranscriptMessage::System {
        id: "kernel".into(),
        created_at: "t".into(),
        content: "k".repeat(STABLE_SYSTEM_KERNEL_MAX_BYTES),
        pinned: true,
    };
    assert_eq!(
        pinned_system_prefix_as_stable(&[oversized], "2026-06-30"),
        None,
        "an oversized stable kernel must be observable as cache-disabled"
    );
}
