//! VG-001 prompt-cache break detection: end-to-end proofs.
//!
//! Evidence map (canon atom `VG-001`, donor
//! `claude-code/src/services/api/promptCacheBreakDetection.ts`):
//! - healthy multi-turn session through the real binary: hit rate on
//!   `turnUsage`, zero `promptCacheBreak`:
//!   `healthy_multi_turn_session_*`
//! - an unexplained cache collapse through the real binary surfaces exactly one
//!   unexpected, durable `promptCacheBreak`: `cache_collapse_*`
//! - a real engine session against a provider that simulates prefix caching:
//!   deliberate system-prompt change => exactly one unexpected break attributed
//!   to `system`; compaction => expected; unexplained history rewrite =>
//!   unexpected: `engine_*`

use crate::support::e2e_cli;
use e2e_cli::*;

use async_trait::async_trait;
use rightkit_agent_engine_core::compaction_archive::WireProjection;
use rightkit_agent_engine_core::engine::SessionEngineConfig;
use rightkit_agent_engine_core::llm::{LlmResponseStream, LlmStreamEvent};
use rightkit_agent_engine_core::{
    Conduit, LlmClient, LlmRequestMetadata, LlmResponse, SessionEngine,
};
use rightkit_agent_engine_protocol::{ProtocolEvent, TranscriptMessage};
use rightkit_agent_policy::PermissionMode;
use rightkit_agent_tools::coding_tools::ToolRegistry;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Through the real binary (scripted OpenAI-compatible provider)
// ---------------------------------------------------------------------------

/// Total prompt size per provider request, growing by one tool round trip.
const PROMPTS: [u64; 5] = [12_000, 12_800, 13_600, 14_400, 15_200];

/// A five-request session whose provider reuses the whole previous prompt from
/// cache, except at request index `collapse_at` (cache read reported as 0).
fn cached_session(collapse_at: Option<usize>) -> Vec<Reply> {
    PROMPTS
        .iter()
        .enumerate()
        .map(|(index, prompt)| {
            let cached = if index == 0 || Some(index) == collapse_at {
                0
            } else {
                PROMPTS[index - 1]
            };
            let chat = if index + 1 < PROMPTS.len() {
                Chat::tool(&format!("c{index}"), "list_files", json!({"path": "."}))
            } else {
                Chat::text("done")
            };
            Reply::Chat(chat.usage(*prompt, 20, cached))
        })
        .collect()
}

fn turn_hit_rates(events: &[Value]) -> Vec<f64> {
    of_kind(events, "turnUsage")
        .iter()
        .map(|event| {
            data(event)["cacheHitPct"]
                .as_f64()
                .expect("turnUsage carries cacheHitPct")
        })
        .collect()
}

#[test]
fn healthy_multi_turn_session_reports_hit_rate_and_zero_cache_breaks() {
    let provider = Provider::start(cached_session(None));
    let cli = Cli::new(&provider);
    let out = cli.run(&["--stream-json", "--bypass-approvals"], "cache health");
    assert_eq!(out.code, Some(0), "{}", out.diag());
    assert_eq!(provider.count(), PROMPTS.len(), "{}", out.diag());

    let events = out.events();
    let rates = turn_hit_rates(&events);
    assert_eq!(
        rates.len(),
        PROMPTS.len(),
        "one turnUsage per request: {}",
        out.stdout
    );
    assert_eq!(rates[0], 0.0, "the first request writes the cache");
    for (turn, rate) in rates.iter().enumerate().skip(1) {
        assert!(
            *rate >= 90.0,
            "turn {} hit rate {rate:.1}% is below the 90% floor: {rates:?}",
            turn + 1
        );
    }

    // Session aggregate: reads / (uncached + reads + writes) across all turns.
    let session_rate = data(of_kind(&events, "turnUsage").last().unwrap())["sessionCacheHitPct"]
        .as_f64()
        .expect("turnUsage carries sessionCacheHitPct");
    let reads: u64 = PROMPTS[..PROMPTS.len() - 1].iter().sum();
    let expected_rate = reads as f64 / (PROMPTS[PROMPTS.len() - 1] + reads) as f64 * 100.0;
    assert!(
        (session_rate - expected_rate).abs() < 0.5,
        "session aggregate {session_rate:.2}% vs expected {expected_rate:.2}%"
    );

    assert!(
        of_kind(&events, "promptCacheBreak").is_empty(),
        "a healthy cache must raise no break at all, expected or not: {}",
        out.stdout
    );
}

#[test]
fn cache_collapse_with_an_unchanged_prompt_raises_exactly_one_unexpected_break() {
    // Request index 2 (the 3rd call) reads nothing from cache although the
    // prompt is byte-identical to what request 2 cached: the silent money burn.
    let provider = Provider::start(cached_session(Some(2)));
    let cli = Cli::new(&provider);
    let out = cli.run(&["--stream-json", "--bypass-approvals"], "cache collapse");
    assert_eq!(out.code, Some(0), "{}", out.diag());

    let events = out.events();
    let breaks = of_kind(&events, "promptCacheBreak");
    assert_eq!(breaks.len(), 1, "exactly one break: {}", out.stdout);
    let found = data(breaks[0]);
    assert_eq!(found["expected"], false, "{found}");
    assert_eq!(found["reason"], "serverSide", "{found}");
    assert_eq!(found["turn"], 3, "{found}");
    assert_eq!(
        found["tokensLost"].as_i64(),
        Some(PROMPTS[0] as i64),
        "baseline is what request 2 had read: {found}"
    );
    assert_eq!(
        found["previousCacheReadTokens"].as_i64(),
        Some(PROMPTS[0] as i64)
    );
    assert_eq!(found["cacheReadTokens"], 0);

    // The break is ordered after the turn's usage and the recovery turns are clean.
    let kinds = out.kinds();
    let break_at = first_index(&kinds, "promptCacheBreak");
    assert!(
        kinds[..break_at]
            .iter()
            .filter(|k| *k == "turnUsage")
            .count()
            >= 3,
        "break must follow the third turnUsage: {kinds:?}"
    );
    let rates = turn_hit_rates(&events);
    assert_eq!(rates[2], 0.0, "{rates:?}");
    assert!(
        rates[3] >= 90.0 && rates[4] >= 90.0,
        "recovery turns: {rates:?}"
    );
}

#[test]
fn sustained_zero_cache_reads_remain_visible_with_cumulative_loss() {
    let replies = PROMPTS
        .iter()
        .enumerate()
        .map(|(index, prompt)| {
            let chat = if index + 1 < PROMPTS.len() {
                Chat::tool(&format!("zero{index}"), "list_files", json!({"path": "."}))
            } else {
                Chat::text("done")
            };
            Reply::Chat(chat.usage(*prompt, 20, 0))
        })
        .collect();
    let provider = Provider::start(replies);
    let cli = Cli::new(&provider);
    let out = cli.run(&["--stream-json", "--bypass-approvals"], "cache stays cold");
    assert_eq!(out.code, Some(0), "{}", out.diag());

    let events = out.events();
    let breaks = of_kind(&events, "promptCacheBreak");
    assert_eq!(
        breaks.len(),
        PROMPTS.len() - 1,
        "every eligible zero-read request stays visible: {}",
        out.stdout
    );
    let cumulative: Vec<_> = breaks
        .iter()
        .map(|event| data(event)["detail"].as_str().unwrap().to_owned())
        .collect();
    assert!(cumulative.windows(2).all(|pair| pair[0] != pair[1]));
    assert!(cumulative
        .last()
        .unwrap()
        .contains("cumulative estimated reusable-prefix loss"));
    assert!(breaks.iter().all(|event| data(event)["expected"] == false));
    assert!(breaks
        .iter()
        .all(|event| data(event)["estimatedCostLost"].is_null()));
}

// ---------------------------------------------------------------------------
// Through a real SessionEngine against a prefix-caching provider simulation
// ---------------------------------------------------------------------------

/// Simulates a provider prompt cache: whatever leading messages are byte-equal
/// to the previous request are served from cache, the rest is written. This is
/// independent of the detector, so a break only appears when the wire prefix
/// really diverged.
struct PrefixCacheProvider {
    previous: Mutex<Vec<String>>,
}

impl PrefixCacheProvider {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            previous: Mutex::new(Vec::new()),
        })
    }
}

fn approx_tokens(messages: &[String]) -> u32 {
    messages
        .iter()
        .map(|message| message.len() / 4)
        .sum::<usize>() as u32
}

#[async_trait]
impl LlmClient for PrefixCacheProvider {
    async fn complete(&self, _messages: &[TranscriptMessage]) -> Result<LlmResponse, String> {
        Ok(LlmResponse::text("ok"))
    }

    async fn stream_with_metadata(
        &self,
        messages: &[TranscriptMessage],
        _metadata: &LlmRequestMetadata,
    ) -> Result<LlmResponseStream, String> {
        let current: Vec<String> = messages
            .iter()
            .filter_map(rightkit_llm::agent_providers::transcript_to_wire)
            .map(|message| serde_json::to_string(&message).expect("serialize wire message"))
            .collect();
        let mut previous = self.previous.lock().unwrap();
        let common = previous
            .iter()
            .zip(current.iter())
            .take_while(|(a, b)| a == b)
            .count();
        let cache_read = approx_tokens(&current[..common]);
        let cache_creation = approx_tokens(&current[common..]);
        *previous = current;
        Ok(Box::pin(futures_util::stream::iter([
            Ok(LlmStreamEvent::Text("ok".into())),
            Ok(LlmStreamEvent::Usage {
                // Provider-normalised: excludes cache reads and writes.
                prompt_tokens: 40,
                completion_tokens: 5,
                cache_read_tokens: cache_read,
                cache_creation_tokens: cache_creation,
            }),
            Ok(LlmStreamEvent::Finished(Some("stop".into()))),
        ])))
    }
}

/// A wire projection the test drives between turns.
struct Mutations {
    change_system: AtomicBool,
    drop_history: AtomicBool,
}

impl WireProjection for Mutations {
    fn project(&self, canonical: &[TranscriptMessage]) -> Vec<TranscriptMessage> {
        let mut wire = canonical.to_vec();
        if self.change_system.load(Ordering::SeqCst) {
            if let Some(TranscriptMessage::System { content, .. }) = wire.first_mut() {
                content.push_str("\nPOLICY CHANGED MID-SESSION");
            }
        }
        if self.drop_history.load(Ordering::SeqCst) {
            // Keep the system tier and the newest message only.
            let system = wire
                .iter()
                .take_while(|message| matches!(message, TranscriptMessage::System { .. }))
                .count();
            if wire.len() > system + 1 {
                wire.drain(system..wire.len() - 1);
            }
        }
        wire
    }
}

struct Rig {
    engine: SessionEngine,
    conduit: Arc<Conduit>,
    mutations: Arc<Mutations>,
    _workspace: tempfile::TempDir,
}

fn rig() -> Rig {
    let workspace = tempfile::tempdir().expect("workspace");
    let conduit = Arc::new(Conduit::new(1024));
    let mut engine = SessionEngine::new(
        PrefixCacheProvider::new(),
        Arc::new(ToolRegistry::build_default()),
        conduit.clone(),
        SessionEngineConfig {
            session_id: "cache-break-e2e".into(),
            permission_mode: PermissionMode::Bypass,
            workspace_root: workspace.path().to_path_buf(),
            day_key: "2026-01-01".into(),
            ..Default::default()
        },
    );
    // ~2.7k tokens of stable system prompt, kept under the 12 kB stable-kernel
    // ceiling so the session stays on the normal (non-degraded) cache path.
    engine.set_system_prompt("S".repeat(11_000));
    engine.set_run_changes_sink(Arc::new(
        rightkit_agent_engine_core::InMemoryRunChangesSink::default(),
    ));
    let mutations = Arc::new(Mutations {
        change_system: AtomicBool::new(false),
        drop_history: AtomicBool::new(false),
    });
    engine.set_wire_projection(mutations.clone());
    Rig {
        engine,
        conduit,
        mutations,
        _workspace: workspace,
    }
}

/// Warm a substantial prefix once; subsequent turns append a small suffix.
/// VG-001 measures reusable provider bytes, not newly submitted input.
fn big_message(label: &str) -> String {
    let bytes = if label == "turn 1" { 60_000 } else { 200 };
    format!("{label} {}", "u".repeat(bytes))
}

struct Break {
    reason: String,
    expected: bool,
    tokens_lost: i64,
    turn: i64,
    detail: String,
}

fn breaks(conduit: &Conduit) -> Vec<Break> {
    conduit
        .events_after(0)
        .into_iter()
        .filter_map(|event| match event {
            ProtocolEvent::PromptCacheBreak {
                reason,
                expected,
                tokens_lost,
                turn,
                detail,
                ..
            } => Some(Break {
                reason,
                expected,
                tokens_lost,
                turn,
                detail,
            }),
            _ => None,
        })
        .collect()
}

fn hit_rates(conduit: &Conduit) -> Vec<f64> {
    conduit
        .events_after(0)
        .into_iter()
        .filter_map(|event| match event {
            ProtocolEvent::TurnUsage { cache_hit_pct, .. } => Some(cache_hit_pct),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn engine_steady_session_has_high_hit_rate_and_no_breaks() {
    let mut rig = rig();
    for turn in 1..=5 {
        rig.engine
            .submit_user_message(&big_message(&format!("turn {turn}")))
            .await
            .expect("turn completes");
    }
    let rates = hit_rates(&rig.conduit);
    assert_eq!(rates.len(), 5, "{rates:?}");
    assert_eq!(rates[0], 0.0);
    for (index, rate) in rates.iter().enumerate().skip(1) {
        assert!(*rate >= 90.0, "turn {} at {rate:.1}%: {rates:?}", index + 1);
    }
    assert!(
        breaks(&rig.conduit).is_empty(),
        "no break expected in a steady session"
    );
}

#[tokio::test]
async fn engine_system_prompt_change_mid_session_is_exactly_one_unexpected_system_break() {
    let mut rig = rig();
    for turn in 1..=3 {
        rig.engine
            .submit_user_message(&big_message(&format!("turn {turn}")))
            .await
            .expect("turn completes");
    }
    assert!(breaks(&rig.conduit).is_empty(), "warm-up must be clean");

    // The deliberate mid-session system prompt change.
    rig.mutations.change_system.store(true, Ordering::SeqCst);
    for turn in 4..=6 {
        rig.engine
            .submit_user_message(&big_message(&format!("turn {turn}")))
            .await
            .expect("turn completes");
    }

    let found = breaks(&rig.conduit);
    assert_eq!(found.len(), 1, "exactly one break: {} events", found.len());
    let found = &found[0];
    assert_eq!(found.reason, "system", "{}", found.detail);
    assert!(!found.expected, "a system prompt change is not deliberate");
    assert_eq!(found.turn, 4, "the first request after the change");
    assert!(found.tokens_lost > 9_000, "{}", found.tokens_lost);
    assert!(
        found.detail.contains("system prompt changed"),
        "{}",
        found.detail
    );

    // The cache re-warms on the very next turn: the new prefix is stable.
    let rates = hit_rates(&rig.conduit);
    assert!(rates[3] < 5.0, "{rates:?}");
    assert!(rates[4] >= 90.0 && rates[5] >= 90.0, "{rates:?}");
}

#[tokio::test]
async fn engine_compaction_is_an_expected_break_not_an_alarm() {
    let mut rig = rig();
    for turn in 1..=3 {
        rig.engine
            .submit_user_message(&big_message(&format!("turn {turn}")))
            .await
            .expect("turn completes");
    }
    // What a compaction does on the wire: history collapses and the epoch moves.
    rig.engine.restore_compaction_epoch(1);
    rig.mutations.drop_history.store(true, Ordering::SeqCst);
    rig.engine
        .submit_user_message(&big_message("turn 4"))
        .await
        .expect("turn completes");

    let found = breaks(&rig.conduit);
    assert_eq!(found.len(), 1, "one break for one compaction");
    assert_eq!(found[0].reason, "compaction", "{}", found[0].detail);
    assert!(found[0].expected, "compaction is a deliberate cache break");
    assert!(found[0].tokens_lost > 10_000, "{}", found[0].tokens_lost);
}

#[tokio::test]
async fn engine_history_rewrite_without_compaction_is_an_unexpected_messages_break() {
    let mut rig = rig();
    for turn in 1..=3 {
        rig.engine
            .submit_user_message(&big_message(&format!("turn {turn}")))
            .await
            .expect("turn completes");
    }
    // Same history collapse, but the compaction epoch did NOT move: nothing
    // sanctioned this, so it must be flagged.
    rig.mutations.drop_history.store(true, Ordering::SeqCst);
    rig.engine
        .submit_user_message(&big_message("turn 4"))
        .await
        .expect("turn completes");

    let found = breaks(&rig.conduit);
    assert_eq!(found.len(), 1, "one break");
    assert_eq!(found[0].reason, "messagesEdited", "{}", found[0].detail);
    assert!(!found[0].expected);
    assert!(
        found[0].detail.contains("message 0 of 1 edited"),
        "{}",
        found[0].detail
    );
}
