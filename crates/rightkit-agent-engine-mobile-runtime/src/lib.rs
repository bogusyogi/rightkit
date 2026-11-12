#![allow(clippy::collapsible_match)]

//! Embedded the host app agent runtime for hosts without `fork`/`exec`.
//!
//! This is the crate an iOS app links. It owns one session — engine, tool
//! registry, conduit — inside the host process, and exposes a deliberately
//! coarse surface: start a session, send a message, drain events. The host
//! never sees engine internals.
//!
//! The contract that makes this worth building is the event stream: the
//! events drained here are the *same* [`rightkit_agent_engine_protocol::ProtocolEvent`]
//! values the daemon publishes over SSE. A client that can render a daemon
//! session can render a local one with no second projection.
//!
//! See `docs/pending/2026-08-28-ios-standalone-local-runtime.md` §5–§6.

pub mod audit_log;
pub mod background_sync;
pub mod branching;
pub mod deferred_inbox;
pub mod ffi;
pub mod git;
pub mod journal;
pub mod journal_repair;
pub mod llm;
pub mod small_model;
pub mod team_fleet;
pub mod wasm;

use rightkit_agent_coding_tools::registry::ToolRegistry;
use rightkit_agent_engine_core::conduit::Conduit;
use rightkit_agent_engine_core::engine::{SessionEngine, SessionEngineConfig};
use rightkit_agent_engine_core::llm::LlmClient;
use rightkit_agent_engine_protocol::ProtocolEvent;
use rightkit_agent_policy::PermissionMode;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

/// Explicit, host-owned HTTPS connectors. Tokens stay in process memory.
#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileMcpServer {
    pub name: String,
    pub endpoint: String,
    pub access_token: Option<String>,
}

impl std::fmt::Debug for MobileMcpServer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MobileMcpServer")
            .field("name", &self.name)
            .field("endpoint", &self.endpoint)
            .field("access_token", &"[redacted]")
            .finish()
    }
}

/// How the runtime should reach a model. Credentials are supplied by the host
/// per session and are never persisted by this crate — on iOS they live in
/// the Keychain and cross the bridge only as a value here.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    /// `"openai"` for any OpenAI-compatible endpoint, `"anthropic"` for the
    /// Anthropic Messages API.
    pub protocol: String,
    /// Runtime-owned conversation identity for provider routing & caching.
    #[serde(default)]
    pub session_id: Option<String>,
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    /// Optional provider-side reasoning hint: none, low, medium, high, or
    /// xhigh. Absent leaves provider/model default untouched.
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSearchConfig {
    pub provider: String,
    pub api_key: String,
}

/// Everything the host must decide before a session can start.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeConfig {
    pub session_id: String,
    /// Absolute path to the working tree, inside the host's own container.
    /// The runtime never discovers this; a host that cannot name its
    /// workspace has no business starting a session.
    pub workspace_root: PathBuf,
    pub provider: ProviderConfig,
    #[serde(default)]
    pub mcp_servers: Vec<MobileMcpServer>,
    /// Absent preserves the tools crate's keyless DuckDuckGo default.
    #[serde(default)]
    pub web_search: Option<WebSearchConfig>,
    /// Optional GitHub OAuth/PAT token supplied from host Keychain.
    #[serde(default)]
    pub github_token: Option<String>,
    /// Optional Cloudflare OAuth/API-token credential supplied from host Keychain.
    #[serde(default)]
    pub cloudflare: Option<rightkit_agent_coding_tools::CloudflareConfig>,
    #[serde(default)]
    pub max_iterations: Option<usize>,
    /// `"plan"` | `"ask"` | `"acceptEdits"` | `"autoSafe"`.
    ///
    /// Absent means `acceptEdits`: file edits apply without interrupting, but
    /// anything destructive still stops for approval. `SessionEngineConfig`'s
    /// own default is `Bypass`, which would let an agent rewrite a phone's
    /// working tree with no prompt at all — never inherit it here.
    #[serde(default)]
    pub permission_mode: Option<String>,
    /// Opaque value returned by [`MobileRuntime::checkpoint_json`]. Hosts
    /// persist it verbatim; Rust alone owns its schema and validation.
    #[serde(default)]
    pub checkpoint_json: Option<String>,
    /// Absolute path to a host-owned, app-private directory this runtime may
    /// write durable per-session files into (the journal, its crash-repair
    /// metadata sidecar, and the redacted audit log). Absent falls back to
    /// `<workspace_root>/<state-dir>/runtime`, which is adequate for tests and
    /// a host that has not yet been updated to supply real app-container
    /// storage, but a production host should always pass its own value here
    /// so this state survives a workspace being re-cloned or wiped.
    #[serde(default)]
    pub storage_dir: Option<PathBuf>,
    /// Hard per-session cost ceiling in USD (ENG-30). `None` or `Some(0.0)`
    /// means no ceiling, matching `SessionEngineConfig`'s own default — but
    /// unlike that default, an explicit `None` here is a deliberate mobile
    /// choice recorded once, not an accidental inheritance, because a host
    /// that never sets this is choosing to let a runaway turn spend without
    /// limit on someone's phone.
    #[serde(default)]
    pub budget_ceiling_usd: Option<f64>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct MobileCheckpoint {
    version: u32,
    session_id: String,
    high_water: i64,
    transcript: Vec<rightkit_agent_engine_protocol::TranscriptMessage>,
    /// ENG-32: the session's compaction lineage (which prefixes of the
    /// transcript have already been folded into a summary, and what they
    /// contained), so a relaunched host can still reconstruct full visible
    /// history via `materialize_compaction_lineage` instead of losing that
    /// ability the moment the process restarts. Absent on a checkpoint taken
    /// before this field existed, or on a session that has never compacted.
    #[serde(default)]
    compaction_lineage:
        Option<rightkit_agent_engine_core::compaction_archive::CompactionRolloutLineage>,
    /// User-created goal state. This is deliberately host-independent: a
    /// Swift view being disconnected must not silently discard its goal.
    #[serde(default)]
    goal: Option<MobileGoalStatus>,
}

/// Rebind a validated snapshot to an independent conversation. Truncation uses
/// complete user turns, never UI row IDs or partial tool-call/result pairs.
pub(crate) fn fork_checkpoint_json(
    raw: &str,
    parent_id: &str,
    destination_id: &str,
    keep_user_turns: Option<usize>,
) -> Result<String, String> {
    let mut checkpoint: MobileCheckpoint =
        serde_json::from_str(raw).map_err(|e| format!("Invalid conversation checkpoint: {e}"))?;
    if checkpoint.version != 1 || checkpoint.session_id != parent_id {
        return Err(
            "Checkpoint does not belong to this conversation or uses an unsupported version."
                .into(),
        );
    }
    if let Some(keep) = keep_user_turns {
        if checkpoint.compaction_lineage.is_some() {
            return Err(
                "This earlier turn has been compacted. Fork the current conversation instead."
                    .into(),
            );
        }
        let starts: Vec<usize> = checkpoint
            .transcript
            .iter()
            .enumerate()
            .filter_map(|(index, message)| {
                matches!(
                    message,
                    rightkit_agent_engine_protocol::TranscriptMessage::User { .. }
                )
                .then_some(index)
            })
            .collect();
        if keep > starts.len() {
            return Err("The selected turn is no longer in this checkpoint.".into());
        }
        if let Some(end) = starts.get(keep) {
            checkpoint.transcript.truncate(*end);
        }
    }
    checkpoint.session_id = destination_id.to_owned();
    checkpoint.high_water = 0;
    checkpoint.goal = None;
    serde_json::to_string(&checkpoint).map_err(|e| e.to_string())
}

/// Serializable, host-facing state for one iOS autonomous progress-audit goal.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MobileGoalStatus {
    pub goal: String,
    pub state: String,
    pub latest_summary: Option<String>,
    pub blockers: Vec<String>,
    pub audit_count: u32,
    pub stop_reason: Option<String>,
}

#[derive(Debug)]
struct MobileGoalState {
    status: MobileGoalStatus,
    watchdog: rightkit_agent_orchestration::coding::watchdog::RunWatchdog,
}

/// Complete user-visible briefing. Pinned knowledge is copied from this
/// session's transcript; model output never invents it.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MobileSessionBriefing {
    pub recap: String,
    pub suggested_follow_up: String,
    pub knowledge_briefing: String,
}

fn permission_mode(raw: Option<&str>) -> Result<PermissionMode, RuntimeError> {
    match raw.unwrap_or("acceptEdits") {
        "plan" => Ok(PermissionMode::Plan),
        "ask" => Ok(PermissionMode::Ask),
        "acceptEdits" => Ok(PermissionMode::AcceptEdits),
        "autoSafe" => Ok(PermissionMode::AutoSafe),
        other => Err(RuntimeError::Config(format!(
            "unknown permissionMode '{other}'; expected plan, ask, acceptEdits, or autoSafe"
        ))),
    }
}

#[derive(Debug)]
pub enum RuntimeError {
    Config(String),
    Engine(String),
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(message) => write!(f, "config: {message}"),
            Self::Engine(message) => write!(f, "engine: {message}"),
        }
    }
}

impl std::error::Error for RuntimeError {}

/// One in-process agent session.
///
/// Deliberately single-session: a phone runs one conversation at a time, and
/// a multiplexing layer here would duplicate what the host's session list
/// already does.
///
/// # Concurrency (IOS-AUD-01)
///
/// Every method here takes `&self`. The FFI layer (`ffi.rs`) hands out a
/// bare `&RuntimeHandle` to every entry point rather than exclusive access to
/// the whole struct, because the actual usage pattern needs it: Swift calls
/// `submit` on a background queue for the length of a turn, while its own
/// actor concurrently polls `drain_events`/`resolve_approval` on a DIFFERENT
/// thread so the transcript fills in as the turn runs rather than only at
/// the end. An earlier version took `&mut self` for `submit` and
/// `drain_events` alike, so both a background thread inside the blocking
/// Rust call and the Swift actor's own thread could hold a mutable
/// reference to the same object at once — undefined behaviour, not merely a
/// bad API. Each field here now owns exactly the synchronization it needs:
/// `engine` (mutated only by `submit`) is a `tokio::sync::Mutex` so it is
/// safe to hold across an `.await`; `cursor` is an atomic; `approvals` was
/// already `Arc<Mutex<_>>`. No field requires exclusive access to the
/// *struct* — only to itself, for the moment it is touched.
pub struct MobileRuntime {
    session_id: String,
    github_configured: bool,
    cloudflare_configured: bool,
    engine: tokio::sync::Mutex<SessionEngine>,
    /// Session-scoped, tool-free assist transport used by recap, goal audit,
    /// and user-requested diff narration. It shares this session's provider
    /// credentials but never receives its tool registry or engine handle.
    small_model:
        Arc<dyn rightkit_agent_orchestration::coding::small_model_utility::SmallModelClient>,
    /// Active autonomous audit survives Swift view detachment & is carried in
    /// checkpoints across relaunch. The watchdog lets an explicit Stop reach
    /// an in-flight bounded model loop.
    goal: Mutex<Option<MobileGoalState>>,
    conduit: Arc<Conduit>,
    /// IOS-AUD-03: "End" must actually stop an in-flight turn, not just walk
    /// away from it — the engine keeps running (and, worse, the FFI handle's
    /// `tokio::runtime::Runtime` blocks on drop until it does) unless told
    /// to stop. `false` at the start of every `submit`, `true` from
    /// `interrupt`; see `wait_for_interrupt_signal` in the engine crate,
    /// which fires immediately if the flag is already set when a turn
    /// starts watching it — so this must be reset per turn, not left `true`
    /// after one interrupted call.
    interrupt_tx: tokio::sync::watch::Sender<bool>,
    /// Shared with the engine so the host can answer an approval the agent is
    /// blocked on. Without this the session would simply hang the first time a
    /// tool asked — which is why `Bypass` is tempting and wrong.
    approvals: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>>,
    /// Sequence of the last event handed to the host. Draining is resumable
    /// across app suspension, which is the whole reason it is a cursor rather
    /// than a channel the host must stay attached to. Atomic so `drain_events`
    /// needs no lock and can genuinely run concurrently with `submit`.
    cursor: AtomicI64,
    /// Durable append-only session journal (ENG-33/34/35): every user
    /// message, tool-call outcome, and effect this session commits is
    /// mirrored here as it happens, in addition to living in the in-memory
    /// transcript/conduit. A plain `std::sync::Mutex` is enough — every use
    /// here is a single `append` call with no `.await` held across the lock.
    journal: Mutex<crate::journal::Journal>,
    /// Redacted runtime observability log (ENG-41). Best-effort: a write
    /// failure here is logged nowhere further and never fails the caller's
    /// turn (see `AuditLog::append`'s doc comment).
    audit_log: crate::audit_log::AuditLog,
    /// High-water mark already mirrored into the journal, independent of
    /// `cursor` (which tracks what the HOST has drained). Kept separate so
    /// the durable journal stays current even when the host is slow to call
    /// `drain_events`, and so a host that never drains at all still gets a
    /// durable record of everything that happened.
    journal_cursor: AtomicI64,
}

/// Deterministic idempotency nonce for a journal record minted inside this
/// crate (as opposed to one a caller supplies). Monotonic per process via
/// `sequence`, so two records minted in the same nanosecond within one
/// `MobileRuntime` never collide.
fn mint_nonce(session_id: &str, sequence: u64) -> String {
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{session_id}-{sequence}-{now_ns}")
}

/// Truncate a preview string to a bounded byte length on a `char` boundary,
/// matching the convention `journal.rs`'s module doc describes ("`preview` is
/// truncated by the caller before it ever reaches this module").
fn truncate_preview(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push_str("...");
    out
}

impl MobileRuntime {
    /// Build a runtime from a host-supplied config.
    ///
    /// The tool registry is built with `build_default()` compiled without the
    /// `host` feature, so the process-backed tools are absent from the model's
    /// schema rather than present and failing.
    pub fn new(mut config: RuntimeConfig) -> Result<Self, RuntimeError> {
        if !config.workspace_root.is_absolute() {
            return Err(RuntimeError::Config(
                "workspaceRoot must be an absolute path".into(),
            ));
        }
        if config.session_id.trim().is_empty() {
            return Err(RuntimeError::Config("sessionId must not be empty".into()));
        }

        let checkpoint = config
            .checkpoint_json
            .as_deref()
            .map(|raw| {
                serde_json::from_str::<MobileCheckpoint>(raw).map_err(|error| {
                    RuntimeError::Config(format!("checkpointJson is invalid: {error}"))
                })
            })
            .transpose()?;
        if let Some(checkpoint) = &checkpoint {
            if checkpoint.version != 1 {
                return Err(RuntimeError::Config(format!(
                    "checkpointJson version {} is unsupported",
                    checkpoint.version
                )));
            }
            if checkpoint.session_id != config.session_id {
                return Err(RuntimeError::Config(
                    "checkpointJson belongs to a different session".into(),
                ));
            }
        }

        config.provider.session_id = Some(config.session_id.clone());
        let github_configured = config.github_token.is_some();
        let cloudflare_configured = config.cloudflare.is_some();
        let llm: Arc<dyn LlmClient> =
            llm::client_for(&config.provider, github_configured, cloudflare_configured)?;
        let small_model = small_model::client_for(&config.provider)?;
        let mut tools = ToolRegistry::build_default();
        if wasm::is_wasm_executor_installed() {
            tools.replace(Box::new(
                rightkit_agent_coding_tools::wasm_compute::WasmComputeTool::with_runtime(Arc::new(
                    wasm::MobileWasmRuntime,
                )),
            ));
        } else {
            tools.remove("wasm_compute");
        }
        let tools = Arc::new(tools);
        let session_id = config.session_id.clone();

        // ENG-33/34/35: open the durable journal before anything else touches
        // storage. `storage_dir` defaults to a workspace-relative path so a
        // host that has not been updated to pass real app-container storage
        // still gets a working, if less durable-across-reinstall, journal
        // rather than a hard failure.
        let storage_dir = config
            .storage_dir
            .clone()
            .unwrap_or_else(|| config.workspace_root.join(".rightkit").join("runtime"));
        std::fs::create_dir_all(&storage_dir).map_err(|error| {
            RuntimeError::Config(format!(
                "could not create runtime storage directory {}: {error}",
                storage_dir.display()
            ))
        })?;
        // The journal's on-disk naming convention (`<session_id>.journal.jsonl`)
        // is a documented contract of `journal.rs` (see its tests and
        // `journal_repair`'s own fixtures), not a private implementation
        // detail — repair must run before `Journal::open` so a torn tail from
        // a prior crash is truncated before anything replays or appends to
        // the file.
        let journal_path = storage_dir.join(format!("{session_id}.journal.jsonl"));
        let repair_report = journal_repair::repair_tail(&journal_path)
            .map_err(|error| RuntimeError::Config(format!("journal repair failed: {error}")))?;
        match journal_repair::read_metadata(&journal_path) {
            Ok(Some(metadata)) if metadata.session_id != session_id => {
                return Err(RuntimeError::Config(format!(
                    "journal at {} belongs to session '{}', not '{session_id}'",
                    journal_path.display(),
                    metadata.session_id
                )));
            }
            Ok(_) => {}
            Err(error) => {
                return Err(RuntimeError::Config(format!(
                    "journal metadata at {} is unreadable: {error}",
                    journal_path.display()
                )));
            }
        }
        journal_repair::write_metadata_atomic(
            &journal_path,
            &journal_repair::JournalMetadata {
                format_version: journal::JOURNAL_FORMAT_VERSION,
                session_id: session_id.clone(),
            },
        )
        .map_err(|error| RuntimeError::Config(format!("journal metadata write failed: {error}")))?;
        let journal = journal::Journal::open(&storage_dir, &session_id)
            .map_err(|error| RuntimeError::Config(format!("could not open journal: {error}")))?;

        let audit_log = audit_log::AuditLog::open(&storage_dir, &session_id)
            .map_err(|error| RuntimeError::Config(format!("could not open audit log: {error}")))?;
        if repair_report.bytes_truncated > 0 {
            let _ = audit_log.append(audit_log::AuditEventKind::JournalRepaired {
                bytes_truncated: repair_report.bytes_truncated,
            });
        }
        let _ = audit_log.append(audit_log::AuditEventKind::SessionStarted {
            resumed: checkpoint.is_some(),
        });

        // The journal is a durable trail of every event this session has
        // published, independent of whatever cursor the host itself
        // persisted. On resume, take the higher of the two: the host's own
        // checkpoint high-water mark, and the last conduit sequence recorded
        // in the journal — the journal can only be equal or ahead, since it
        // is appended to synchronously as events are drained, so this can
        // never regress a host that already has a newer checkpoint but
        // strictly helps one whose checkpoint predates its last journal
        // flush.
        let checkpoint_high_water = checkpoint
            .as_ref()
            .map_or(0, |value| value.high_water.max(0));
        let journal_high_water = journal
            .replay(0)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|event| match event.kind {
                journal::JournalEventKind::ProtocolEvent { conduit_seq } => Some(conduit_seq),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        let high_water = checkpoint_high_water.max(journal_high_water);
        let conduit = Arc::new(Conduit::with_high_water_mark(1024, high_water));

        let mut engine_config = SessionEngineConfig {
            session_id: config.session_id,
            skill_dirs: vec![config.workspace_root.join(".rightkit").join("skills")],
            workspace_root: config.workspace_root,
            permission_mode: permission_mode(config.permission_mode.as_deref())?,
            // No language server to ask, so never pay for the attempt.
            auto_post_edit_diagnostics: false,
            // ENG-30: a hard per-session cost ceiling. `SessionEngineConfig`'s
            // own default (0.0 == unbounded) is inherited only when the host
            // explicitly declines to set one, mirroring the deliberate
            // (never-silent) pattern `permission_mode` already uses above.
            budget_ceiling_usd: config.budget_ceiling_usd.unwrap_or(0.0),
            ..SessionEngineConfig::default()
        };
        if let Some(max_iterations) = config.max_iterations {
            engine_config.max_iterations = max_iterations;
        }

        let mut engine = SessionEngine::new(llm, tools, conduit.clone(), engine_config);
        // Image generation owns a separate catalog/transport, but an exact
        // OpenAI image endpoint may reuse this account's Keychain-supplied
        // key. Custom/OpenAI-compatible text endpoints never receive it.
        if let Some(image_provider) =
            rightkit_agent_coding_tools::image_generate::image_provider_for_base_url(
                &config.provider.base_url,
            )
        {
            engine.set_image_provider_key(
                image_provider.to_string(),
                config.provider.api_key.clone(),
            );
        }
        let mut mcp = rightkit_mcp::agent_mcp::McpManager::new();
        for server in config.mcp_servers {
            let endpoint = url::Url::parse(&server.endpoint)
                .map_err(|_| RuntimeError::Config("MCP endpoint is invalid".into()))?;
            if endpoint.scheme() != "https"
                || endpoint.host_str().is_none()
                || !endpoint.username().is_empty()
                || endpoint.password().is_some()
                || server.name.trim().is_empty()
                || mcp.server_names().contains(&server.name)
            {
                return Err(RuntimeError::Config(
                    "MCP requires unique names & credential-free HTTPS endpoints".into(),
                ));
            }
            mcp.register(
                &server.name,
                Arc::new(rightkit_mcp::agent_mcp::HttpTransport::with_access_token(
                    server.endpoint,
                    server.access_token,
                )),
            );
        }
        engine.set_mcp(Arc::new(mcp));
        if let Some(search) = config.web_search {
            engine.set_web_search_config(rightkit_agent_coding_tools::WebSearchConfig {
                provider: search.provider,
                api_key: search.api_key,
            });
        }
        if let Some(token) = config.github_token {
            engine.set_github_token(token);
        }
        if let Some(cloudflare) = config.cloudflare {
            engine.set_cloudflare_config(cloudflare);
        }
        let checkpoint_has_system_prompt = checkpoint.as_ref().is_some_and(|value| {
            value.transcript.iter().any(|message| {
                matches!(
                    message,
                    rightkit_agent_engine_protocol::TranscriptMessage::System { .. }
                )
            })
        });
        if !checkpoint_has_system_prompt {
            engine.set_system_prompt(
                "You are Agent, a coding agent running locally on this iPhone. Complete user requests by inspecting and editing configured workspace with available tools. Use relative workspace paths. Never claim a file change unless a tool result confirms it. You cannot run shell commands, builds, tests, browsers, or desktop-only tools on this host. Treat tool results as authoritative and stop after requested work is complete.",
            );
        }
        if checkpoint.is_none() {
            if let Some(context) =
                ffi::project_memory_context(&storage_dir).map_err(RuntimeError::Config)?
            {
                engine.set_system_prompt(context);
            }
        }
        // Mobile has no daemon-owned SQLite review store, but engine safety
        // still requires pre-effect origin + post-effect mutation capture.
        // Keep both ledgers in-process for this session; checkpoints retain
        // transcript state while Git/file state remains source of truth.
        engine.set_run_origin_sink(Arc::new(
            rightkit_agent_engine_core::InMemoryOriginSink::always_ready(),
        ));
        engine.set_run_changes_sink(Arc::new(
            rightkit_agent_engine_core::InMemoryRunChangesSink::default(),
        ));
        if let Some(checkpoint) = checkpoint.as_ref() {
            engine.seed_transcript(checkpoint.transcript.clone());
            // ENG-32: restore compaction lineage before the first turn runs
            // so a relaunched host does not lose the ability to reconstruct
            // history folded away by a PREVIOUS process's compactions.
            if let Some(lineage) = checkpoint.compaction_lineage.clone() {
                engine.set_compaction_lineage(lineage);
            }
        }
        let approvals = Arc::new(Mutex::new(HashMap::new()));
        engine.set_approval_store(approvals.clone());
        let (interrupt_tx, interrupt_rx) = tokio::sync::watch::channel(false);
        engine.set_interrupt_receiver(interrupt_rx);
        let goal = checkpoint
            .as_ref()
            .and_then(|value| value.goal.clone())
            .map(|mut status| {
                // An FFI caller may have been disconnected while an audit
                // was in flight. Its goal remains durable, but no stale
                // "auditing" spinner may pretend that a relaunched process
                // still owns that future; Audit explicitly resumes it.
                if status.state == "auditing" {
                    status.state = "active".to_string();
                    status.stop_reason = Some("Audit interrupted by host restart".to_string());
                }
                MobileGoalState {
                    watchdog: rightkit_agent_orchestration::coding::watchdog::RunWatchdog::new(3),
                    status,
                }
            });

        Ok(Self {
            session_id,
            github_configured,
            cloudflare_configured,
            engine: tokio::sync::Mutex::new(engine),
            small_model,
            goal: Mutex::new(goal),
            conduit,
            approvals,
            cursor: AtomicI64::new(high_water),
            interrupt_tx,
            journal: Mutex::new(journal),
            audit_log,
            journal_cursor: AtomicI64::new(high_water),
        })
    }

    /// Append one journal record, tolerating a poisoned lock (a panic
    /// elsewhere in this process must not brick durability for the rest of
    /// the session) and treating a write failure as best-effort: a full disk
    /// or a permissions error here must not fail the caller's turn, since the
    /// in-memory conduit/transcript remain the authoritative state for the
    /// running process regardless of whether this durable mirror kept up.
    fn append_journal(&self, kind: crate::journal::JournalEventKind) {
        let mut journal = self
            .journal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let nonce = mint_nonce(&self.session_id, journal.last_sequence() + 1);
        let _ = journal.append(nonce, kind);
    }

    /// Run one turn to completion. Events land on the conduit as the turn
    /// proceeds; the host drains them with [`Self::drain_events`], which
    /// remains callable from another thread while this runs — see the type
    /// doc comment.
    pub async fn submit(&self, text: &str) -> Result<String, RuntimeError> {
        // Reset before each turn — see the field doc comment on why a stale
        // `true` from a PREVIOUS interrupted turn must not immediately fire
        // on this one.
        let _ = self.interrupt_tx.send(false);
        // ENG-33/41: mirror the submission itself, before the model ever
        // sees it, so a crash mid-turn still leaves a durable record that a
        // turn was requested.
        const PREVIEW_CHARS: usize = 200;
        let preview = truncate_preview(text, PREVIEW_CHARS);
        self.append_journal(crate::journal::JournalEventKind::UserMessage {
            preview: preview.clone(),
        });
        let _ = self
            .audit_log
            .append(crate::audit_log::AuditEventKind::TurnSubmitted { preview });

        let mut engine = self.engine.lock().await;
        let result = engine
            .submit_user_message(text)
            .await
            .map_err(|error| RuntimeError::Engine(error.to_string()));
        drop(engine);
        // ENG-18/33: mirror every event this turn published — including tool
        // outcomes, which is what makes an effect durable in the journal, not
        // just present in the process's in-memory conduit — regardless of
        // whether the host has drained them yet.
        self.journal_new_events();
        result
    }

    /// Mirror every conduit event published since the last time this was
    /// called into the durable journal and the redacted audit log, advancing
    /// `journal_cursor` independently of the host-facing `cursor`. Called
    /// after every turn; safe to call redundantly (idempotent: nothing new
    /// to mirror is simply a no-op).
    fn journal_new_events(&self) {
        let after = self.journal_cursor.load(Ordering::SeqCst);
        let events = self.conduit.events_after(after);
        let Some(last) = events.last() else {
            return;
        };
        let last_seq = rightkit_agent_engine_core::conduit::seq_of(last);
        for event in &events {
            let seq = rightkit_agent_engine_core::conduit::seq_of(event);
            self.append_journal(crate::journal::JournalEventKind::ProtocolEvent {
                conduit_seq: seq,
            });
            match event {
                ProtocolEvent::ToolResultAdded { result, .. } => {
                    if let rightkit_agent_engine_protocol::TranscriptMessage::Tool {
                        tool_call_id,
                        is_error,
                        ..
                    } = result
                    {
                        let ok = !is_error;
                        // ENG-18: this is the durable mirror of a committed
                        // effect's outcome. The transcript's `Tool` message
                        // carries `tool_call_id`, not the human tool name, at
                        // this layer — the journal's closed `ToolCall` kind
                        // still records that a call completed and whether it
                        // succeeded, which is what `undo`/audit consumers of
                        // the journal need; the tools crate's
                        // `effect_receipt_store` remains the authority for
                        // path-level conflict detection within one process.
                        self.append_journal(crate::journal::JournalEventKind::ToolCall {
                            tool_name: tool_call_id.clone(),
                            ok,
                        });
                        if ok {
                            self.append_journal(crate::journal::JournalEventKind::EffectApplied {
                                effect_id: tool_call_id.clone(),
                            });
                        }
                        let _ =
                            self.audit_log
                                .append(crate::audit_log::AuditEventKind::ToolInvoked {
                                    tool_name: tool_call_id.clone(),
                                    ok,
                                });
                    }
                }
                ProtocolEvent::SessionCompacted { dropped_count, .. } => {
                    let _ = self.audit_log.append(
                        crate::audit_log::AuditEventKind::CompactionOccurred {
                            dropped_count: *dropped_count,
                        },
                    );
                }
                ProtocolEvent::BudgetExceeded {
                    current_usd,
                    ceiling_usd,
                    ..
                } => {
                    let _ =
                        self.audit_log
                            .append(crate::audit_log::AuditEventKind::BudgetExceeded {
                                current_usd_cents: (current_usd * 100.0).round().max(0.0) as u64,
                                ceiling_usd_cents: (ceiling_usd * 100.0).round().max(0.0) as u64,
                            });
                }
                _ => {}
            }
        }
        self.journal_cursor.store(last_seq, Ordering::SeqCst);
    }

    /// Stop the in-flight turn, if any. Safe to call with no turn running —
    /// the next `submit` resets the flag regardless.
    pub fn interrupt(&self) {
        let _ = self.interrupt_tx.send(true);
        let _ = self
            .audit_log
            .append(crate::audit_log::AuditEventKind::TurnInterrupted);
    }

    /// Hand the host every event it has not yet seen, advancing the cursor.
    /// Safe to call while [`Self::submit`] is in flight on another thread.
    pub fn drain_events(&self) -> Vec<ProtocolEvent> {
        let after = self.cursor.load(Ordering::SeqCst);
        let events = self.conduit.events_after(after);
        if let Some(last) = events.last() {
            self.cursor.store(
                rightkit_agent_engine_core::conduit::seq_of(last),
                Ordering::SeqCst,
            );
        }
        events
    }

    /// Sequence of the last event handed to the host. Persisting this lets a
    /// relaunched host resume the stream instead of replaying it.
    pub fn cursor(&self) -> i64 {
        self.cursor.load(Ordering::SeqCst)
    }

    /// Resume draining from a cursor the host persisted before suspension.
    pub fn set_cursor(&self, cursor: i64) {
        self.cursor.store(cursor, Ordering::SeqCst);
    }

    /// Durable opaque snapshot for app suspension or relaunch. It contains
    /// model transcript plus event high-water mark, never provider secrets.
    pub async fn checkpoint_json(&self) -> Result<String, RuntimeError> {
        let engine = self.engine.lock().await;
        // ENG-32: persist compaction lineage alongside the transcript so a
        // relaunched host restores it via `set_compaction_lineage` and keeps
        // the ability to reconstruct history compacted away in a PRIOR
        // process, not just within this one.
        let lineage = engine.compaction_lineage().clone();
        let compaction_lineage = if lineage.segments.is_empty() {
            None
        } else {
            Some(lineage)
        };
        let goal = self
            .goal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(|state| state.status.clone());
        serde_json::to_string(&MobileCheckpoint {
            version: 1,
            session_id: self.session_id.clone(),
            high_water: self.conduit.current_high_water(),
            transcript: engine.transcript().messages().to_vec(),
            compaction_lineage,
            goal,
        })
        .map_err(|error| RuntimeError::Engine(format!("could not encode checkpoint: {error}")))
    }

    /// Generate a bounded recap from real session transcript. No unavailable
    /// route is represented as a recap: callers receive its typed error.
    pub async fn session_recap(&self) -> Result<Option<String>, RuntimeError> {
        let transcript = {
            let engine = self.engine.lock().await;
            serde_json::to_string(engine.transcript().messages()).map_err(|error| {
                RuntimeError::Engine(format!("could not encode transcript for recap: {error}"))
            })?
        };
        let budget = rightkit_agent_orchestration::coding::small_model_utility::SmallModelBudget::new(
            1,
            rightkit_agent_orchestration::coding::small_model_utility::DEFAULT_CALL_TOKEN_ESTIMATE,
        );
        rightkit_agent_orchestration::coding::session_recap::summarize_session(
            self.small_model.as_ref(),
            &transcript,
            &budget,
        )
        .await
        .map(|recap| recap.map(|value| value.summary))
        .map_err(|error| RuntimeError::Engine(error.to_string()))
    }

    /// Generate a recap, one user-controlled follow-up, & a verbatim pinned
    /// knowledge briefing from current local session state.
    pub async fn session_briefing(&self) -> Result<Option<MobileSessionBriefing>, RuntimeError> {
        let (session_content, knowledge_briefing) = {
            let engine = self.engine.lock().await;
            let messages = engine.transcript().messages();
            let session_content = serde_json::to_string(messages).map_err(|error| {
                RuntimeError::Engine(format!("could not encode transcript for briefing: {error}"))
            })?;
            let pinned = messages
                .iter()
                .filter(|message| {
                    matches!(
                        message,
                        rightkit_agent_engine_protocol::TranscriptMessage::System {
                            pinned: true,
                            ..
                        } | rightkit_agent_engine_protocol::TranscriptMessage::User {
                            pinned: true,
                            ..
                        } | rightkit_agent_engine_protocol::TranscriptMessage::Assistant {
                            pinned: true,
                            ..
                        } | rightkit_agent_engine_protocol::TranscriptMessage::Tool {
                            pinned: true,
                            ..
                        } | rightkit_agent_engine_protocol::TranscriptMessage::Compact {
                            pinned: true,
                            ..
                        } | rightkit_agent_engine_protocol::TranscriptMessage::Fold {
                            pinned: true,
                            ..
                        }
                    )
                })
                .map(|message| serde_json::to_string(message).unwrap_or_default())
                .collect::<Vec<_>>();
            let knowledge_briefing = if pinned.is_empty() {
                String::new()
            } else {
                format!(
                    "<pinned_transcript>\n{}\n</pinned_transcript>",
                    pinned.join("\n")
                )
            };
            (session_content, knowledge_briefing)
        };
        let mut budget = rightkit_agent_orchestration::coding::small_model_utility::SmallModelBudget::new(
            2,
            2 * rightkit_agent_orchestration::coding::small_model_utility::DEFAULT_CALL_TOKEN_ESTIMATE,
        );
        rightkit_agent_orchestration::coding::session_recap::brief_session(
            self.small_model.as_ref(),
            &session_content,
            &knowledge_briefing,
            &mut budget,
        )
        .await
        .map(|briefing| {
            briefing.map(|value| MobileSessionBriefing {
                recap: value.recap,
                suggested_follow_up: value.suggested_follow_up,
                knowledge_briefing: value.knowledge_briefing,
            })
        })
        .map_err(|error| RuntimeError::Engine(error.to_string()))
    }

    /// Create or replace an explicit local goal. Auditing is user-triggered;
    /// this never starts work just because a view appeared.
    pub fn create_goal(&self, goal: &str) -> Result<MobileGoalStatus, RuntimeError> {
        let goal = goal.trim();
        if goal.is_empty() {
            return Err(RuntimeError::Config("goal must not be empty".into()));
        }
        let status = MobileGoalStatus {
            goal: goal.to_string(),
            state: "active".to_string(),
            latest_summary: None,
            blockers: Vec::new(),
            audit_count: 0,
            stop_reason: None,
        };
        *self
            .goal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(MobileGoalState {
            status: status.clone(),
            watchdog: rightkit_agent_orchestration::coding::watchdog::RunWatchdog::new(3),
        });
        Ok(status)
    }

    /// Return current goal state without requiring a Swift view to remain mounted.
    pub fn goal_status(&self) -> Option<MobileGoalStatus> {
        self.goal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(|state| state.status.clone())
    }

    /// Run a bounded autonomous progress audit against live transcript state.
    pub async fn audit_goal(&self) -> Result<MobileGoalStatus, RuntimeError> {
        let (goal, watchdog) = {
            let mut state = self
                .goal
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let state = state
                .as_mut()
                .ok_or_else(|| RuntimeError::Config("no active goal".into()))?;
            if state.status.state == "stopped" {
                return Ok(state.status.clone());
            }
            if state.status.state == "auditing" {
                return Err(RuntimeError::Config(
                    "goal audit is already in progress".into(),
                ));
            }
            state.status.state = "auditing".to_string();
            (state.status.goal.clone(), state.watchdog.clone())
        };
        let report = async {
            let context = {
                let engine = self.engine.lock().await;
                serde_json::to_string(engine.transcript().messages()).map_err(|error| {
                    RuntimeError::Engine(format!(
                        "could not encode transcript for goal audit: {error}"
                    ))
                })?
            };
            let config = rightkit_agent_orchestration::coding::goal_loop::GoalLoopConfig {
                interval: std::time::Duration::from_millis(250),
                ..Default::default()
            };
            let budget = rightkit_agent_orchestration::coding::small_model_utility::SmallModelBudget::new(
                config.max_iterations,
                config.max_iterations
                    * rightkit_agent_orchestration::coding::small_model_utility::DEFAULT_CALL_TOKEN_ESTIMATE,
            );
            rightkit_agent_orchestration::coding::goal_loop::run_goal_loop(
                self.small_model.as_ref(),
                &goal,
                &context,
                budget,
                config,
                watchdog,
            )
            .await
            .map_err(|error| RuntimeError::Engine(error.to_string()))
        }
        .await;
        let mut state = self
            .goal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let state = state
            .as_mut()
            .ok_or_else(|| RuntimeError::Config("goal was removed while auditing".into()))?;
        let report = match report {
            Ok(report) => report,
            Err(error) => {
                if state.status.goal == goal && state.status.state == "auditing" {
                    state.status.state = "blocked".to_string();
                    state.status.stop_reason = Some("Audit could not complete".to_string());
                    state.status.blockers = vec![error.to_string()];
                }
                return Ok(state.status.clone());
            }
        };
        if state.status.goal == goal && state.status.state != "stopped" {
            state.status.audit_count = state
                .status
                .audit_count
                .saturating_add(report.audits.len() as u32);
            state.status.latest_summary = report.audits.last().map(|audit| audit.summary.clone());
            state.status.blockers = report.blockers;
            state.status.stop_reason = Some(format!("{:?}", report.stop_reason));
            state.status.state = match report.stop_reason {
                rightkit_agent_orchestration::coding::goal_loop::GoalLoopStopReason::WatchdogCancelled => {
                    "stopped".to_string()
                }
                rightkit_agent_orchestration::coding::goal_loop::GoalLoopStopReason::WatchdogStalled => {
                    "blocked".to_string()
                }
                _ => "active".to_string(),
            };
        }
        Ok(state.status.clone())
    }

    /// Cooperatively stop an in-flight audit & keep its final status durable.
    pub fn stop_goal(&self) -> Option<MobileGoalStatus> {
        let mut state = self
            .goal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let state = state.as_mut()?;
        state.watchdog.cancel();
        state.status.state = "stopped".to_string();
        state.status.stop_reason = Some("Stopped by user".to_string());
        Some(state.status.clone())
    }

    /// Narrate a caller-selected diff. This is deliberately explicit: normal
    /// session turns never spend a model call to narrate source changes.
    pub async fn diff_walkthrough(
        &self,
        diff: &str,
    ) -> Result<rightkit_agent_orchestration::coding::diff_walkthrough::DiffWalkthrough, RuntimeError>
    {
        let mut budget = rightkit_agent_orchestration::coding::small_model_utility::SmallModelBudget::new(
            8,
            8 * rightkit_agent_orchestration::coding::small_model_utility::DEFAULT_CALL_TOKEN_ESTIMATE,
        );
        Ok(
            rightkit_agent_orchestration::coding::diff_walkthrough::narrate_diff_walkthrough(
                self.small_model.as_ref(),
                diff,
                &mut budget,
            )
            .await,
        )
    }

    /// Answer an approval the agent is waiting on.
    ///
    /// Returns false when nothing was waiting under that id — a stale tap, or
    /// a turn that already moved on.
    pub fn resolve_approval(&self, tool_call_id: &str, approved: bool) -> bool {
        let sender = self
            .approvals
            .lock()
            .ok()
            .and_then(|mut pending| pending.remove(tool_call_id));
        let resolved = match sender {
            Some(sender) => sender.send(approved).is_ok(),
            None => false,
        };
        if resolved {
            let _ = self
                .audit_log
                .append(crate::audit_log::AuditEventKind::ApprovalResolved {
                    tool_call_id: tool_call_id.to_string(),
                    approved,
                });
        }
        resolved
    }

    /// Tool-call ids currently blocked on the user.
    pub fn pending_approvals(&self) -> Vec<String> {
        self.approvals
            .lock()
            .map(|pending| pending.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Mid-session Cloudflare credential refresh (frozen interface, ENG-74
    /// Lane A3): the host calls this whenever it refreshes a near-expiry
    /// OAuth access token from `configuration(gate:)`. This is deliberately
    /// synchronous and does not touch `self.engine`'s `tokio::sync::Mutex` —
    /// that mutex is held for an entire in-flight `submit`, and blocking on
    /// it here would defeat the point of a mid-turn refresh. Instead it
    /// forwards straight to the tools crate's session-keyed credential cell,
    /// which the `cloudflare` tool consults on every call ahead of its
    /// session-start value. The call already in flight keeps using the
    /// bearer it already captured; only the NEXT `cloudflare` tool call in
    /// this session observes the refreshed one.
    pub fn update_cloudflare_credential(
        &self,
        config: rightkit_agent_coding_tools::CloudflareConfig,
    ) {
        rightkit_agent_engine_core::engine::SessionEngine::update_cloudflare_credential(
            &self.session_id,
            config,
        );
        self.append_journal(crate::journal::JournalEventKind::CredentialRefreshed {
            provider: "cloudflare".to_string(),
        });
        let _ = self
            .audit_log
            .append(crate::audit_log::AuditEventKind::CredentialRefreshed {
                provider: "cloudflare".to_string(),
            });
    }

    /// Names of the tools this host actually offers the model. The host shows
    /// these so a user can see what the phone can and cannot do.
    pub fn tool_names(&self) -> Vec<String> {
        ToolRegistry::build_default()
            .all()
            .iter()
            .filter(|tool| {
                is_mobile_supported_tool(
                    tool.name(),
                    self.github_configured,
                    self.cloudflare_configured,
                ) && (tool.name() != "wasm_compute" || wasm::is_wasm_executor_installed())
            })
            .map(|tool| tool.name().to_string())
            .collect()
    }
}

/// Whether this tool is safe to expose to the model on this host.
///
/// This is deliberately an explicit denylist of every name that must never
/// reach a phone, not a filter that trusts whatever `ToolRegistry::build_default()`
/// happened to compile in. Historically `--no-default-features` (dropping
/// `host`) removed every process-backed tool from the registry entirely, so
/// this function only had to cover context-unwired tools. That structural
/// guarantee does NOT hold inside `cargo test --workspace`: Cargo's feature
/// unification can enable `host` for `host_tools` across the whole build
/// graph because some *other* workspace member depends on it with `host` on,
/// which silently re-adds `shell`, `powershell`, `lsp`, `browser_open`, and
/// the rest of `registry_impl.rs`'s `#[cfg(feature = "host")]` registrations
/// to `build_default()` even for this crate's own tests. The iOS product
/// build is unaffected (it builds this crate alone, so unification never
/// pulls `host` in), but a test run inside the workspace graph is a real
/// build of this exact binary shape, so the guarantee must hold there too.
/// Listing every process-backed tool name here — mirroring
/// `registry_impl.rs`'s `#[cfg(feature = "host")]` registrations and its
/// `"execution"`/`"browser"`/`"code-intel"` `tool_category` entries — makes
/// the mobile capability profile hold by construction regardless of which
/// features got unified into the build, not merely by what got compiled in.
///
/// The second half of this list is for tools that ARE always built (they
/// need no process), but need a context service `MobileRuntime` never wires
/// up: `ctx.questioner` for `ask_user`, `ctx.plan_exit` for
/// `exit_plan_mode`, `ctx.spawner` for `spawn_subagent`, `ctx.github_token`
/// for `github`, `ctx.team_directory` for `team_*`, `ctx.message_dispatcher`
/// for `send_message`, and any MCP server for `mcp_*` (no MCP config is ever
/// loaded here). Each returns a clean error without that context rather than
/// panicking, but the model was still choosing them from a schema promising
/// they would work, then burning a turn on a call that could not succeed.
///
/// docs/pending/2026-08-28-ios-standalone-local-runtime.md §7 (mobile
/// capability profile) is the source of truth for what launch actually
/// supports; this list implements it. Wiring these up for real — a mobile
/// questioner UI, a plan-review sheet, a subagent runtime, GitHub OAuth — is
/// M1/M2 scope, not something to half-wire under time pressure.
fn is_mobile_supported_tool(
    name: &str,
    github_configured: bool,
    cloudflare_configured: bool,
) -> bool {
    const UNSUPPORTED: &[&str] = &[
        // Process-backed tools: gated behind `#[cfg(feature = "host")]` in
        // `registry_impl.rs::build_default`, but listed here explicitly so a
        // unified `host` feature in a test/desktop build graph can never
        // leak them into a phone's schema.
        "shell",
        "powershell",
        "get_shell_output",
        "kill_shell",
        "process",
        "repl",
        "ocr",
        "ocr_onnx",
        "transcribe",
        "web_extract",
        "media_fetch",
        "ffmpeg_extract",
        "lsp",
        "browser_open",
        "browser_read",
        "browser_act",
        "browser_screenshot",
        "browser_console",
        "browser_close",
        "schedule_cron",
        "enter_worktree",
        "exit_worktree",
        // Context-unwired tools: registered unconditionally, but this host
        // never wires the service they need to actually succeed.
        "ask_user",
        "exit_plan_mode",
        "spawn_subagent",
        "mcp_auth",
        "team_create",
        "team_delete",
        "send_message",
        "followup_subagent",
        "wait_for_subagents",
        "interrupt_subagent",
        // ENG-102 remains an approved host-endpoint exposure path outside
        // iOS. No iOS connector exists, so never advertise generic port
        // forwarding from this runtime.
        "tunnel",
        "dev_tunnel_proxy",
    ];
    !UNSUPPORTED.contains(&name)
        && (name != "github" || github_configured)
        && (name != "cloudflare" || cloudflare_configured)
}
