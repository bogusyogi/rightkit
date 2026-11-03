//! Membrane resident: the process `membrane serve` (stdin held open by the host app) runs for as
//! long as the host app runs.
//!
//! `run_resident` is the entry and owns the lifecycle. It starts the stdin-closed parent watch and
//! stop-signal watchers, takes the owner lock through `service::start_resident_engine`
//! (before any store opens; losing it is `ResidentExit::AlreadyServing`), then `run` opens
//! the stores, starts background review, binds the owner-only endpoint through
//! `service::start_endpoint`, publishes `resident.json` and serves until asked to stop.
//! Watchers, Blueprint refresh and indexers run for as long as the process does.
//!
//! The `(method, url, body) -> (status, body)` table in `route` is the memory, planner,
//! push and freshness logic behind `explicit_memory_response`; the
//! endpoint's ops (`pull`, `push`, `blueprint`, `status`, `hook`) are answered in `service`.

use crate::catalog::{self, ContextCatalog, CATALOG_SCHEMA_VERSION};
use crate::memdb::MemDb;
use crate::pull::planner::{ContextCandidateSetV1, PlannerError, PlannerInput};
use crate::scope::{normalize_scope, scope_chain};
use crate::store::{
    ExternalLifecycleStage, MemoryBatchError, MemoryBatchRequest, MemoryEventContext,
    MemoryLifecycleInputV1, MemoryLifecycleOperationV1, MemoryStore, VerifiedMemoryActor,
};
use rightkit_context_federation::deadline::Deadline;
use rightkit_fs::digest::sha256_bytes;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

const MAX_BODY_BYTES: usize = 1 << 20;
const MAX_PUSH_BODY_BYTES: usize = 8 << 20;

/// Existing request-owned control passed into Push by the transport integrator.
/// Serve owns request lifetime; Push only observes this control.
#[derive(Clone)]
pub struct PushRequestControl {
    pub deadline: Deadline,
    pub cancellation: CancellationToken,
}

pub fn push_request_control(
    deadline: Deadline,
    cancellation: CancellationToken,
) -> PushRequestControl {
    PushRequestControl {
        deadline,
        cancellation,
    }
}

thread_local! {
    static INHERITED_PUSH_CONTROL: std::cell::RefCell<Option<PushRequestControl>> =
        const { std::cell::RefCell::new(None) };
}

/// Run one synchronous dispatch with transport-owned Push control.
/// The dispatcher is synchronous, so this request-local binding cannot leak
/// across tasks or survive beyond `dispatch`.
pub fn with_inherited_push_control<T>(
    control: PushRequestControl,
    dispatch: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<PushRequestControl>);
    impl Drop for Restore {
        fn drop(&mut self) {
            INHERITED_PUSH_CONTROL.with(|slot| {
                *slot.borrow_mut() = self.0.take();
            });
        }
    }

    let previous = INHERITED_PUSH_CONTROL.with(|slot| slot.replace(Some(control)));
    let _restore = Restore(previous);
    dispatch()
}

pub(crate) fn inherited_push_control() -> Option<PushRequestControl> {
    INHERITED_PUSH_CONTROL.with(|slot| slot.borrow().clone())
}

const MAX_QUERY_CHARS: usize = 8 * 1024;
const MAX_CONTENT_CHARS: usize = 256 * 1024;
const MAX_RECALL_K: u64 = 50;
static TASTE_REQUEST_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
const DEFAULT_ANCHOR_RETRIEVE_BYTES: usize = 64 * 1024;
const MAX_ANCHOR_RETRIEVE_BYTES: usize = 256 * 1024;
const RESERVED_ADAPT_ADMISSION_REASON: &str = "reserved_adapt_authority";
struct PromptTelemetryWorker {
    running: crate::wake::Wake,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl PromptTelemetryWorker {
    fn stop(mut self) -> Result<(), String> {
        self.running.close();
        self.thread
            .take()
            .expect("prompt telemetry worker handle present")
            .join()
            .map_err(|_| "cortex prompt telemetry drain panicked".to_string())
    }
}

impl Drop for PromptTelemetryWorker {
    fn drop(&mut self) {
        self.running.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn str_list(v: &serde_json::Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Body of a memory with any YAML (`---`) or TOML (`+++`) frontmatter block removed. An unclosed
/// fence falls through to the raw content rather than eating the whole memory.
fn strip_frontmatter(content: &str) -> &str {
    for fence in ["---", "+++"] {
        if let Some(rest) = content.strip_prefix(&format!("{fence}\n")) {
            if let Some(end) = rest.find(&format!("\n{fence}")) {
                return rest[end + fence.len() + 1..].trim_start_matches('\n');
            }
        }
    }
    content
}

/// Injection preview: first ~`max` chars of real prose. Skips frontmatter, HTML comments, blank
/// lines, code-fence markers, and short heading-only lines (titles), then joins what remains.
/// This is what the recall hook shows — for short rules the preview IS the delivered value, so
/// its quality directly drives injection usefulness (E6, 2026-07-05). The fallback ladder never
/// returns raw frontmatter: filtered body → post-frontmatter text.
fn preview(content: &str, max: usize) -> String {
    let after_fm = strip_frontmatter(content);
    let body: Vec<&str> = after_fm
        .lines()
        .filter(|l| {
            let t = l.trim();
            if t.is_empty() || t.starts_with("<!--") || t.starts_with("```") {
                return false;
            }
            if t.starts_with('#') {
                // Heading: a short one is a title (drop); a long one carries content (keep).
                return t.trim_start_matches('#').trim().len() >= 40;
            }
            true
        })
        .collect();
    let joined = body.join(" ");
    let src = if joined.trim().is_empty() {
        after_fm
    } else {
        &joined
    };
    src.chars().take(max).collect()
}

fn latest_analysis_path(directory: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let mut reports = std::fs::read_dir(directory)
        .map_err(|error| format!("read analysis directory {}: {error}", directory.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    reports.sort();
    reports
        .pop()
        .ok_or_else(|| format!("no analysis reports in {}", directory.display()))
}

fn latest_analysis_json(directory: &std::path::Path) -> Result<serde_json::Value, String> {
    let latest = latest_analysis_path(directory)?;
    let raw = std::fs::read_to_string(&latest)
        .map_err(|error| format!("read analysis report {}: {error}", latest.display()))?;
    let report: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|error| format!("parse analysis report {}: {error}", latest.display()))?;
    let generated = report
        .get("generated_at")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            format!(
                "analysis stale: missing generated_at in {}",
                latest.display()
            )
        })?;
    let today = crate::time::now_iso();
    if generated.get(..10) != today.get(..10) {
        return Err(format!(
            "analysis stale: latest report {} is not from the current UTC date",
            latest.display()
        ));
    }
    Ok(report)
}

fn analysis_response(directory: &std::path::Path) -> (u16, String) {
    match latest_analysis_json(directory) {
        Ok(report) => (200, report.to_string()),
        Err(error) => (
            503,
            serde_json::json!({ "error": "analysis unavailable", "detail": error }).to_string(),
        ),
    }
}

fn configured_analysis_directory() -> std::path::PathBuf {
    crate::qa_env::daily_analysis_dir()
        .map(std::path::PathBuf::from)
        .or_else(|| {
            crate::qa_env::workspace_root()
                .map(std::path::PathBuf::from)
                .map(|root| root.join("tools/.cache/metrics/daily-analysis"))
        })
        .unwrap_or_else(|| std::path::PathBuf::from("tools/.cache/metrics/daily-analysis"))
}

fn configured_workspace_root() -> std::path::PathBuf {
    crate::qa_env::repo_root()
        .or_else(crate::qa_env::workspace_root)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        })
}

fn anchor_retrieve_response(body: &str) -> (u16, String) {
    let value = match json_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let Some(repo) = value
        .get("repo")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return (400, json!({"error":"repo required"}).to_string());
    };
    let Some(anchor) = value
        .get("anchor")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return (400, json!({"error":"anchor required"}).to_string());
    };
    let max_bytes = match value.get("maxBytes") {
        None => DEFAULT_ANCHOR_RETRIEVE_BYTES,
        Some(value) => match value.as_u64().and_then(|value| usize::try_from(value).ok()) {
            Some(value) if value > 0 => value.min(MAX_ANCHOR_RETRIEVE_BYTES),
            _ => {
                return (
                    400,
                    json!({"error":"maxBytes must be a positive integer"}).to_string(),
                );
            }
        },
    };
    let workspace = match configured_workspace_root().canonicalize() {
        Ok(path) => path,
        Err(_) => {
            return (
                503,
                json!({"error":"workspace root unavailable"}).to_string(),
            );
        }
    };
    let repo = match std::path::Path::new(repo).canonicalize() {
        Ok(path) if path.starts_with(&workspace) => path,
        _ => {
            return (
                403,
                json!({"error":"repo is outside configured workspace"}).to_string(),
            );
        }
    };
    let requested = std::path::Path::new(anchor);
    let file = match (if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        repo.join(requested)
    })
    .canonicalize()
    {
        Ok(path) if path.starts_with(&repo) => path,
        _ => return (403, json!({"error":"anchor is outside repo"}).to_string()),
    };
    let metadata = match std::fs::metadata(&file) {
        Ok(metadata) if metadata.is_file() => metadata,
        _ => {
            return (
                400,
                json!({"error":"anchor must resolve to a regular file"}).to_string(),
            );
        }
    };
    let read_limit = max_bytes.saturating_add(4);
    let mut bytes = Vec::with_capacity(read_limit);
    let read_result = (|| -> std::io::Result<()> {
        use std::io::Read;
        std::fs::File::open(&file)?
            .take(read_limit as u64)
            .read_to_end(&mut bytes)?;
        Ok(())
    })();
    if read_result.is_err() {
        return (
            400,
            json!({"error":"anchor file could not be read"}).to_string(),
        );
    }
    let complete = metadata.len() <= read_limit as u64;
    let valid_end = match std::str::from_utf8(&bytes) {
        Ok(_) => bytes.len(),
        Err(error) if !complete && error.error_len().is_none() => error.valid_up_to(),
        Err(_) => {
            return (
                400,
                json!({"error":"anchor file must be valid UTF-8"}).to_string(),
            );
        }
    };
    let content_end = valid_end.min(max_bytes);
    let content_end = std::str::from_utf8(&bytes[..content_end])
        .map_or_else(|error| error.valid_up_to(), |_| content_end);
    let content = match std::str::from_utf8(&bytes[..content_end]) {
        Ok(content) => content,
        Err(_) => {
            return (
                400,
                json!({"error":"anchor file must be valid UTF-8"}).to_string(),
            );
        }
    };
    let truncated = metadata.len() > content_end as u64;
    (
        200,
        json!({
            "path": file.strip_prefix(&repo).unwrap_or(&file).to_string_lossy(),
            "sha256": sha256_bytes(content.as_bytes()),
            "content": content,
            "truncated": truncated,
        })
        .to_string(),
    )
}

/// Shared `/freshness` response assembly for the resident and the test-routed dispatcher.
/// F19 fix: `sessionId` and `worktreePath` are the caller's
/// declared identity on the source-barrier receipt. Silently defaulting either one
/// (`"freshness-http"` / the raw `repoRoot`) let every caller that omitted its identity collapse
/// onto the same receipt identity, which defeats the receipt's purpose — both are now required,
/// with no fallback value invented on the caller's behalf.
fn freshness_response_body(
    v: &Value,
    verdict: &crate::freshness::FreshnessVerdict,
    repo_root: &std::path::Path,
) -> (u16, String) {
    let Some(session_id) = v
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return (
            400,
            serde_json::json!({ "error": "sessionId required" }).to_string(),
        );
    };
    let Some(worktree_path) = v
        .get("worktreePath")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return (
            400,
            serde_json::json!({ "error": "worktreePath required" }).to_string(),
        );
    };
    let requested_worktree = match crate::freshness::canonical_repo_root(
        std::path::Path::new(worktree_path),
        &configured_workspace_root(),
    ) {
        Ok(root) => root,
        Err(_) => {
            return (
                400,
                serde_json::json!({ "error": "worktreePath invalid" }).to_string(),
            );
        }
    };
    if requested_worktree != repo_root {
        return (
            409,
            serde_json::json!({ "error": "worktreePath does not match repoRoot" }).to_string(),
        );
    }
    let repository_id = v
        .get("repositoryId")
        .and_then(Value::as_str)
        .unwrap_or("workspace-root");
    let mut payload_value = match serde_json::to_value(verdict) {
        Ok(value) => value,
        Err(_) => {
            return (
                500,
                serde_json::json!({ "error": "freshness serialization failed" }).to_string(),
            );
        }
    };
    payload_value["sourceBarrierReceipt"] = crate::freshness::source_barrier_receipt(
        verdict,
        repository_id,
        session_id,
        &repo_root.to_string_lossy(),
    );
    match serde_json::to_string(&payload_value) {
        Ok(payload) => (200, payload),
        Err(_) => (
            500,
            serde_json::json!({ "error": "freshness serialization failed" }).to_string(),
        ),
    }
}

/// Which origin scope an observable-event read runs under. Chosen by the route the caller hit,
/// never by a field in the request body — Taste must not be able to widen itself to
/// assistant/model/tool origin simply by asking for it, which is why the two routes exist
/// separately instead of one route with a scope parameter.
enum ObservableQueryRoute {
    Taste,
    Insights,
    ForgeTimeAccounting,
}

fn observable_query_response(
    store: &MemoryStore,
    body: &str,
    route: ObservableQueryRoute,
) -> (u16, String) {
    let Ok(request) = serde_json::from_str::<Value>(body) else {
        return (
            400,
            serde_json::json!({ "error": "invalid observable event query" }).to_string(),
        );
    };
    // `limit` is required with no default: a caller that omits it would otherwise silently receive
    // whatever bound we picked and read a partial ledger as though it were the whole one.
    let Some(limit) = request.get("limit").and_then(Value::as_u64) else {
        return (
            400,
            serde_json::json!({ "error": "limit required" }).to_string(),
        );
    };
    let text = |key: &str| {
        request
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let filter = crate::context_telemetry::ObservableEventQuery {
        since: text("since"),
        until: text("until"),
        event_type: text("eventType"),
        session_id: text("sessionId"),
        task_id: text("taskId"),
        trace_id: text("traceId"),
        installation_id: text("installationId"),
        after_sequence: request.get("afterSequence").and_then(Value::as_i64),
        limit: limit as usize,
    };
    let queried = match route {
        ObservableQueryRoute::Taste => store.db().query_observable_events_for_taste(&filter),
        ObservableQueryRoute::Insights => store.db().query_observable_events_for_insights(&filter),
        ObservableQueryRoute::ForgeTimeAccounting => {
            store.db().query_observable_events_for_forge(&filter)
        }
    };
    match queried {
        Ok(result) => match serde_json::to_string(&result) {
            Ok(payload) => (200, payload),
            Err(_) => (
                500,
                serde_json::json!({ "error": "observable query serialization failed" }).to_string(),
            ),
        },
        Err(crate::context_telemetry::ContextTelemetryError::Invalid(error)) => {
            (400, serde_json::json!({ "error": error }).to_string())
        }
        Err(_) => (
            500,
            serde_json::json!({ "error": "observable event storage unavailable" }).to_string(),
        ),
    }
}

fn route_path(url: &str) -> &str {
    url.split('?').next().unwrap_or(url)
}

fn json_body(body: &str) -> Result<serde_json::Value, (u16, String)> {
    if body.trim().is_empty() {
        return Err((400, "{\"error\":\"empty json body\"}".to_string()));
    }
    serde_json::from_str(body).map_err(|_| (400, "{\"error\":\"malformed json body\"}".to_string()))
}

fn lifecycle_input_from_json(value: &Value) -> Result<MemoryLifecycleInputV1, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "request body must be an object".to_string())?;
    let lifecycle = [
        "effectiveFromMs",
        "effectiveUntilMs",
        "expiresAtMs",
        "reviewAfterMs",
        "priorityClass",
        "confidence",
        "confidenceBasis",
        "supersedes",
        "authority",
        "influenceClass",
    ]
    .into_iter()
    .filter_map(|key| {
        object
            .get(key)
            .map(|value| (key.to_string(), value.clone()))
    })
    .collect::<serde_json::Map<_, _>>();
    serde_json::from_value(Value::Object(lifecycle))
        .map_err(|error| format!("invalid lifecycle input: {error}"))
}

fn public_write_guard_defaults(lifecycle: &mut MemoryLifecycleInputV1) {
    if lifecycle.authority.is_none() {
        lifecycle.authority = Some("A0".into());
    }
    if lifecycle.influence_class.is_none() {
        lifecycle.influence_class = Some("data_only".into());
    }
}

fn event_context(v: &serde_json::Value, fallback_surface: &str) -> MemoryEventContext {
    let surface = v
        .get("client")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(fallback_surface);
    let mut context = MemoryEventContext::new(surface);
    if let Some(session) = v.get("session").and_then(|value| value.as_str()) {
        context = context.with_session(session);
    }
    if let Some(turn_id) = v
        .get("turn_id")
        .or_else(|| v.get("decision_id"))
        .and_then(|value| value.as_str())
    {
        context = context.with_turn(turn_id);
    }
    if let Some(trace_id) = v.get("trace_id").and_then(|value| value.as_str()) {
        context = context.with_trace(trace_id);
    }
    context
}

fn best_effort_event_context(body: &str, fallback_surface: &str) -> MemoryEventContext {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .map(|value| event_context(&value, fallback_surface))
        .unwrap_or_else(|| MemoryEventContext::new(fallback_surface))
}

fn external_route_stage(path: &str) -> Option<(&'static str, ExternalLifecycleStage)> {
    match path {
        "/put" | "/v1/memories:batch" => Some(("write", ExternalLifecycleStage::Validation)),
        "/delete" => Some(("delete", ExternalLifecycleStage::Validation)),
        "/get" | "/list" => Some(("read", ExternalLifecycleStage::Provider)),
        _ => None,
    }
}

fn record_http_rejection(
    store: &MemoryStore,
    path: &str,
    status: &str,
    reason_code: &str,
) -> Result<(), String> {
    let Some((operation, stage)) = external_route_stage(path) else {
        return Ok(());
    };
    store.record_external_lifecycle(
        &MemoryEventContext::new("http"),
        operation,
        stage,
        status,
        reason_code,
        None,
        None,
        "memory",
        "http",
        1,
    )
}

#[allow(clippy::too_many_arguments)]
fn record_external_or_500(
    store: &MemoryStore,
    context: &MemoryEventContext,
    operation: &str,
    stage: ExternalLifecycleStage,
    status: &str,
    reason_code: &str,
    memory_id: Option<&str>,
    scope_id: Option<&str>,
    artifact_family: &str,
    producer: &str,
    quantity: usize,
) -> Option<(u16, String)> {
    store
        .record_external_lifecycle(
            context,
            operation,
            stage,
            status,
            reason_code,
            memory_id,
            scope_id,
            artifact_family,
            producer,
            quantity,
        )
        .err()
        .map(|_| {
            (
                500,
                serde_json::json!({ "error": "external lifecycle accounting unavailable" })
                    .to_string(),
            )
        })
}

fn route(store: &MemoryStore, method: &str, url: &str, body: &str) -> (u16, String) {
    route_with_context_ingest_lease(store, None, None, method, url, body, None)
}

/// Closed SDK operation selection delegates to the same canonical handlers as HTTP.
pub(crate) fn explicit_memory_response(
    store: &MemoryStore,
    operation: rightkit_context_protocol::explicit::ExplicitOperation,
    request: &serde_json::Map<String, Value>,
) -> (u16, Value) {
    use rightkit_context_protocol::explicit::ExplicitOperation as Op;
    if operation == Op::AdaptCompare {
        let request: rightkit_adapt_taste::comparison::CandidateComparisonV1 =
            match serde_json::from_value(Value::Object(request.clone())) {
                Ok(value) => value,
                Err(error) => {
                    return (
                        400,
                        json!({"code":"invalid_request","error":format!("invalid ADP-076 comparison: {error}")}),
                    );
                }
            };
        let decision = match rightkit_adapt_taste::comparison::compare(&request) {
            Ok(value) => value,
            Err(error) => return (400, json!({"code":"invalid_request","error":error})),
        };
        let decision_value = match serde_json::to_value(&decision) {
            Ok(value) => value,
            Err(error) => {
                return (
                    500,
                    json!({"code":"internal_error","error":error.to_string()}),
                );
            }
        };
        let receipt = match crate::adapt_service::journal(
            store,
            &request.scope,
            "adapt.comparison",
            &request.comparison_id,
            decision_value.clone(),
        ) {
            Ok(value) => value,
            Err(error) => return (500, json!({"code":"store_error","error":error})),
        };
        return (200, json!({"decision":decision_value,"receipt":receipt}));
    }
    if operation == Op::ActivityRead {
        let limit = request
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(20)
            .min(10_000) as usize;
        return (200, store.activity_json(limit));
    }
    let (method, path) = match operation {
        Op::Activity => ("POST", "/activity"),
        Op::Delete => ("POST", "/delete"),
        Op::Federate => ("POST", "/federate"),
        Op::Get => ("POST", "/get"),
        Op::List => ("POST", "/list"),
        Op::Metrics => ("GET", "/metrics"),
        Op::Put => ("POST", "/put"),
        Op::Recall => ("POST", "/recall"),
        Op::Remember => ("POST", "/remember"),
        Op::RememberConsolidated => ("POST", "/remember_consolidated"),
        Op::Scopes => ("POST", "/scopes"),
        Op::Search => ("POST", "/search"),
        Op::Use => ("POST", "/use"),
        _ => {
            return (
                400,
                json!({"code":"invalid_request","error":"unsupported memory operation"}),
            );
        }
    };
    let (status, body) = route(
        store,
        method,
        path,
        &Value::Object(request.clone()).to_string(),
    );
    match serde_json::from_str(&body) {
        Ok(value) => (status, value),
        Err(_) => (
            500,
            json!({"code":"commit_unknown","error":"owner response serialization failed"}),
        ),
    }
}

fn claims_reserved_adapt_authority(item: &crate::store::MemoryBatchItem) -> bool {
    item.artifact_family == "adapt"
        || item.producer == "adapt_native"
        || item.record_type == "taste_preference"
        || item.client == "membrane_adapt"
        || (item.lifecycle.authority.as_deref() == Some("A2")
            && item.lifecycle.influence_class.as_deref() == Some("behavioral_directive"))
}

/// Production federation route is native and same-process. Its request-time
/// H8 ceiling is validated before fan-out and its Push selection is attached
/// to the same response.
fn federate_route_response(store: &MemoryStore, body: &str) -> (u16, String) {
    crate::pull::federation::native_route_response_with_store(body, Some(store))
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Observation windows currently being served. A window that completes while
/// another is still in flight is honestly reported as foreground-active, which
/// makes the background scheduler defer instead of competing with the turn.
static OBSERVATION_WINDOWS_IN_FLIGHT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// CTX-023/CTX-024 producer trigger.
///
/// A completed Adapt observation window is the runtime's real turn boundary:
/// it is the point at which new session events have just been appended to
/// Cortex's append-only stream. Publishing here (rather than on a timer) means
/// the background-review input is written from recorded state, inside the
/// tray-owned serve process, and never when the Hub is not serving.
///
/// Publication never changes the host's response: a producer refusal is a
/// stderr diagnostic, not an HTTP failure.
/// The observation route for a caller with no catalog: the resident's `adapt_observation` op and
/// the in-process `membrane adapt observe` / `transcript append`. Same answer and publication as
/// `POST /v1/adapt/observations`.
pub(crate) fn observation_body_response(store: &MemoryStore, body: &str) -> (u16, String) {
    observation_response_and_publish(store, None, body)
}

fn observation_response_and_publish(
    store: &MemoryStore,
    catalog: Option<&ContextCatalog>,
    body: &str,
) -> (u16, String) {
    use std::sync::atomic::Ordering;
    OBSERVATION_WINDOWS_IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
    // Session-transcript append/forget is a typed operation on this same
    // route: it gets 200/409/410 semantics rather than the generic 400
    // collapse, and it never produces an observation window for the
    // background-review publisher.
    let response = match crate::adapt_observations::transcript_response(store, body) {
        Some(transcript) => transcript,
        None => crate::adapt_observations::response_with_catalog(store, catalog, body),
    };
    let concurrent = OBSERVATION_WINDOWS_IN_FLIGHT
        .fetch_sub(1, Ordering::SeqCst)
        .saturating_sub(1);
    if response.0 == 200 {
        publish_background_review_input(store, body, concurrent > 0);
    }
    response
}

fn publish_background_review_input(store: &MemoryStore, body: &str, foreground_active: bool) {
    let Ok(crate::adapt_observations::AdaptObservationRequestV1::Analyze {
        scope,
        window_id,
        session_id,
        task_id,
        ..
    }) = serde_json::from_str::<crate::adapt_observations::AdaptObservationRequestV1>(body)
    else {
        // Acknowledge/Outcome operations append no session window.
        return;
    };
    let root = configured_workspace_root();
    match crate::background_review_input::publish_observation_window(
        store,
        &root,
        &scope,
        &session_id,
        &task_id,
        &window_id,
        foreground_active,
        crate::time::now_millis() as u64,
    ) {
        Ok(Some(window)) => stderr_line!(
            "membrane background-review-input published session={} seq={}..={}",
            window.session_id,
            window.from_seq,
            window.to_seq
        ),
        Ok(None) => {}
        Err(error) => {
            stderr_line!("membrane background-review-input refused: {error}")
        }
    }
}

fn route_with_context_ingest_lease(
    store: &MemoryStore,
    catalog: Option<&ContextCatalog>,
    context_ingest_lease: Option<&crate::context_telemetry::ContextIngestLease>,
    method: &str,
    url: &str,
    body: &str,
    push_control: Option<&PushRequestControl>,
) -> (u16, String) {
    let path = route_path(url);
    if body.len() > MAX_BODY_BYTES {
        if record_http_rejection(store, path, "failed", "request_too_large").is_err() {
            return (
                500,
                serde_json::json!({ "error": "external lifecycle accounting unavailable" })
                    .to_string(),
            );
        }
        return (413, "{\"error\":\"request body too large\"}".to_string());
    }
    if method == "POST" && path == crate::adapt_service::OPERATOR_PATH {
        return crate::adapt_service::operator_response(store, body);
    }
    if method == "POST" && path == crate::adapt_service::OBSERVATION_PATH {
        return observation_response_and_publish(store, catalog, body);
    }
    if method == "GET" && path == "/metrics" {
        return (200, store.metrics_json().to_string());
    }
    if method == "GET" && path == "/activity" {
        return (200, store.activity_json(20).to_string());
    }
    if method == "GET" && path == "/graph" {
        return (
            200,
            store.relationship_graph_json(3, 0.55, false).to_string(),
        );
    }
    if method == "GET" && path == "/analysis" {
        return analysis_response(&configured_analysis_directory());
    }
    if method == "POST" && path == "/anchor/retrieve" {
        return anchor_retrieve_response(body);
    }
    if method == "POST" && path == "/memory-lifecycle" {
        let Some(_lease) = context_ingest_lease else {
            return (503, r#"{"error":"verified startup lease required"}"#.into());
        };
        let value = match json_body(body) {
            Ok(value) => value,
            Err(response) => return response,
        };
        let request: MemoryLifecycleOperationV1 =
            match serde_json::from_value(value.clone()) {
                Ok(request) => request,
                Err(error) => return (
                    400,
                    serde_json::json!({"error": format!("invalid lifecycle operation: {error}")})
                        .to_string(),
                ),
            };
        let request_digest = sha256_bytes(body.as_bytes());
        let actor = match VerifiedMemoryActor::from_execution_context(
            "membrane-runtime",
            "A1",
            "startup_lease",
            &format!("lifecycle-{}", &request_digest[..16]),
            &format!("lifecycle-{}", &request_digest[16..32]),
        ) {
            Ok(actor) => actor,
            Err(error) => return (403, serde_json::json!({"error": error}).to_string()),
        };
        return match store.execute_lifecycle_operation(&request, &actor) {
            Ok(receipt) => (
                200,
                serde_json::to_string(&receipt)
                    .unwrap_or_else(|_| "{\"error\":\"serialization failed\"}".into()),
            ),
            Err(error) => (409, serde_json::json!({"error": error}).to_string()),
        };
    }
    if method == "POST" && matches!(path, "/expand" | "/push/resolve") {
        return match push_control {
            Some(control) => crate::push::api::http_response_with_control(
                "membrane_push_resolve",
                body,
                control.deadline,
                &control.cancellation,
            ),
            None => crate::push::api::http_response("membrane_push_resolve", body),
        };
    }
    if method == "POST" && path == "/push/prepare" {
        return match push_control {
            Some(control) => crate::push::api::http_response_with_control(
                "membrane_push_prepare",
                body,
                control.deadline,
                &control.cancellation,
            ),
            None => crate::push::api::http_response("membrane_push_prepare", body),
        };
    }
    if method == "POST" && path == "/federate" {
        return match push_control {
            Some(control) => with_inherited_push_control(control.clone(), || {
                federate_route_response(store, body)
            }),
            None => federate_route_response(store, body),
        };
    }
    if method == "POST" && path == "/freshness" {
        let v = match json_body(body) {
            Ok(v) => v,
            Err(resp) => return resp,
        };
        let Some(requested) = v
            .get("repoRoot")
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
        else {
            return (
                400,
                serde_json::json!({ "error": "repoRoot required" }).to_string(),
            );
        };
        let repo_root = match crate::freshness::canonical_repo_root(
            std::path::Path::new(requested),
            &configured_workspace_root(),
        ) {
            Ok(root) => root,
            Err(error) if error == "repoRoot is outside the configured workspace" => {
                return (403, serde_json::json!({ "error": error }).to_string());
            }
            Err(error) => return (400, serde_json::json!({ "error": error }).to_string()),
        };
        let verdict = crate::freshness::evaluate_repository_freshness(store, repo_root.clone());
        return freshness_response_body(&v, &verdict, &repo_root);
    }
    if method == "POST" && path == "/skills-snapshot" {
        return match store.skills_snapshot() {
            Ok(snapshot) => match serde_json::to_string(&snapshot) {
                Ok(payload) => (200, payload),
                Err(_) => (
                    500,
                    serde_json::json!({ "error": "skills snapshot serialization failed" })
                        .to_string(),
                ),
            },
            Err(_) => (
                500,
                serde_json::json!({ "error": "skills snapshot unavailable" }).to_string(),
            ),
        };
    }
    if method == "POST" && path == "/v1/telemetry/events:batch" {
        let Some(context_ingest_lease) = context_ingest_lease else {
            return (
                503,
                serde_json::json!({ "error": "active telemetry lease unavailable" }).to_string(),
            );
        };
        let batch = match serde_json::from_str::<crate::context_telemetry::ContextEventBatch>(body)
        {
            Ok(batch) => batch,
            Err(_) => {
                return (
                    400,
                    serde_json::json!({ "error": "invalid context telemetry batch" }).to_string(),
                );
            }
        };
        return match store
            .db()
            .ingest_local_context_events(&batch, context_ingest_lease)
        {
            Ok(receipt) => {
                let status = if receipt.inserted == 0 { 200 } else { 201 };
                match serde_json::to_string(&receipt) {
                    Ok(payload) => (status, payload),
                    Err(_) => (
                        500,
                        serde_json::json!({ "error": "telemetry receipt serialization failed" })
                            .to_string(),
                    ),
                }
            }
            Err(crate::context_telemetry::ContextTelemetryError::Invalid(error)) => {
                (400, serde_json::json!({ "error": error }).to_string())
            }
            Err(crate::context_telemetry::ContextTelemetryError::Conflict { event_id }) => (
                409,
                serde_json::json!({
                    "error": "event_id conflicts with an existing canonical event",
                    "event_id": event_id,
                })
                .to_string(),
            ),
            Err(crate::context_telemetry::ContextTelemetryError::AttributionMismatch) => (
                403,
                serde_json::json!({
                    "error": "context telemetry attribution does not match active installation"
                })
                .to_string(),
            ),
            Err(crate::context_telemetry::ContextTelemetryError::Database(_)) => (
                500,
                serde_json::json!({ "error": "context telemetry storage unavailable" }).to_string(),
            ),
        };
    }
    if method == "POST" && path == "/v1/telemetry/observable-events:batch" {
        let Some(context_ingest_lease) = context_ingest_lease else {
            return (
                503,
                serde_json::json!({ "error": "active telemetry lease unavailable" }).to_string(),
            );
        };
        let batch =
            match serde_json::from_str::<crate::context_telemetry::ObservableEventBatchV1>(body) {
                Ok(batch) => batch,
                Err(_) => {
                    return (
                        400,
                        serde_json::json!({ "error": "invalid observable event batch" })
                            .to_string(),
                    );
                }
            };
        return match store
            .db()
            .ingest_observable_events(&batch, context_ingest_lease)
        {
            Ok(receipt) => {
                let status = if receipt.inserted == 0 { 200 } else { 201 };
                match serde_json::to_string(&receipt) {
                    Ok(payload) => (status, payload),
                    Err(_) => (
                        500,
                        serde_json::json!({ "error": "observable receipt serialization failed" })
                            .to_string(),
                    ),
                }
            }
            Err(crate::context_telemetry::ContextTelemetryError::Invalid(error)) => {
                (400, serde_json::json!({ "error": error }).to_string())
            }
            Err(crate::context_telemetry::ContextTelemetryError::Conflict { event_id }) => (
                409,
                serde_json::json!({
                    "error": "event_id conflicts with an existing canonical event",
                    "event_id": event_id,
                })
                .to_string(),
            ),
            Err(crate::context_telemetry::ContextTelemetryError::AttributionMismatch) => (
                403,
                serde_json::json!({
                    "error": "observable event attribution does not match active installation"
                })
                .to_string(),
            ),
            Err(crate::context_telemetry::ContextTelemetryError::Database(_)) => (
                500,
                serde_json::json!({ "error": "observable event storage unavailable" }).to_string(),
            ),
        };
    }
    if method == "POST" && path == "/v1/telemetry/observable-events:query-taste" {
        return observable_query_response(store, body, ObservableQueryRoute::Taste);
    }
    if method == "POST" && path == "/v1/telemetry/observable-events:query-insights" {
        return observable_query_response(store, body, ObservableQueryRoute::Insights);
    }
    if method == "POST" && path == "/v1/telemetry/observable-events:query-forge-time" {
        return observable_query_response(store, body, ObservableQueryRoute::ForgeTimeAccounting);
    }
    if method == "POST" && path == "/v1/memories:batch" {
        let request = match serde_json::from_str::<MemoryBatchRequest>(body) {
            Ok(request) => request,
            Err(_) => {
                let context = best_effort_event_context(body, "http");
                if let Some(response) = record_external_or_500(
                    store,
                    &context,
                    "write",
                    ExternalLifecycleStage::Validation,
                    "failed",
                    "malformed_json",
                    None,
                    None,
                    "memory",
                    "http",
                    1,
                ) {
                    return response;
                }
                return (
                    400,
                    serde_json::json!({ "error": "invalid memory batch" }).to_string(),
                );
            }
        };
        if let Some(item) = request
            .items
            .iter()
            .find(|item| claims_reserved_adapt_authority(item))
        {
            let context = MemoryEventContext::new(&item.client)
                .with_session(&item.session_id)
                .with_turn(&item.turn_id)
                .with_trace(&item.trace_id);
            if let Some(response) = record_external_or_500(
                store,
                &context,
                "write",
                ExternalLifecycleStage::Validation,
                "failed",
                RESERVED_ADAPT_ADMISSION_REASON,
                Some(&format!("batch/{}", request.batch_id)),
                Some(&item.scope),
                "memory",
                "http",
                1,
            ) {
                return response;
            }
            return (
                403,
                serde_json::json!({
                    "error": "reserved Adapt authority requires verified native apply",
                    "code": RESERVED_ADAPT_ADMISSION_REASON,
                })
                .to_string(),
            );
        }
        let context = request
            .items
            .first()
            .map(|item| {
                MemoryEventContext::new(&item.client)
                    .with_session(&item.session_id)
                    .with_turn(&item.turn_id)
                    .with_trace(&item.trace_id)
            })
            .unwrap_or_else(|| MemoryEventContext::new("http"));
        return match store.try_put_batch(&request) {
            Ok(receipt) => {
                if receipt.inserted == 0 {
                    for item in &request.items {
                        let item_context = MemoryEventContext::new(&item.client)
                            .with_session(&item.session_id)
                            .with_turn(&item.turn_id)
                            .with_trace(&item.trace_id);
                        let memory_id = format!(
                            "{}/{}",
                            crate::scope::normalize_scope(&item.scope),
                            item.name
                        );
                        if let Some(response) = record_external_or_500(
                            store,
                            &item_context,
                            "write",
                            ExternalLifecycleStage::Commit,
                            "skipped",
                            "idempotent_duplicate",
                            Some(&memory_id),
                            Some(&item.scope),
                            &item.artifact_family,
                            &item.producer,
                            1,
                        ) {
                            return response;
                        }
                    }
                }
                let status = if receipt.inserted == 0 { 200 } else { 201 };
                match serde_json::to_string(&receipt) {
                    Ok(payload) => (status, payload),
                    Err(_) => (
                        500,
                        serde_json::json!({ "error": "memory batch receipt serialization failed" })
                            .to_string(),
                    ),
                }
            }
            Err(error) => {
                let (http_status, stage, status, reason) = match error {
                    MemoryBatchError::Invalid(_) => (
                        400,
                        ExternalLifecycleStage::Validation,
                        "failed",
                        "invalid_batch",
                    ),
                    MemoryBatchError::Conflict => (
                        409,
                        ExternalLifecycleStage::Validation,
                        "failed",
                        "batch_conflict",
                    ),
                    MemoryBatchError::Persist(ref message)
                        if message.contains("embed") || message.contains("writes disabled") =>
                    {
                        (
                            500,
                            ExternalLifecycleStage::Embedding,
                            "unavailable",
                            "embedding_unavailable",
                        )
                    }
                    MemoryBatchError::Persist(_) => (
                        500,
                        ExternalLifecycleStage::Commit,
                        "failed",
                        "commit_failed",
                    ),
                };
                if let Some(response) = record_external_or_500(
                    store,
                    &context,
                    "write",
                    stage,
                    status,
                    reason,
                    Some(&format!("batch/{}", request.batch_id)),
                    None,
                    "memory",
                    "http",
                    request.items.len().max(1),
                ) {
                    return response;
                }
                let message = if http_status == 409 {
                    "batch_id conflicts with an existing canonical request"
                } else if http_status == 400 {
                    "invalid memory batch"
                } else {
                    "memory batch storage unavailable"
                };
                (
                    http_status,
                    serde_json::json!({ "error": message }).to_string(),
                )
            }
        };
    }
    if method == "POST" && path == "/put" {
        let v = match json_body(body) {
            Ok(v) => v,
            Err(resp) => {
                let context = best_effort_event_context(body, "http");
                if let Some(response) = record_external_or_500(
                    store,
                    &context,
                    "write",
                    ExternalLifecycleStage::Validation,
                    "failed",
                    "malformed_json",
                    None,
                    None,
                    "memory",
                    "http",
                    1,
                ) {
                    return response;
                }
                return resp;
            }
        };
        let context = event_context(&v, "http");
        let lifecycle = match lifecycle_input_from_json(&v) {
            Ok(mut lifecycle) => {
                public_write_guard_defaults(&mut lifecycle);
                lifecycle
            }
            Err(error) => return (400, serde_json::json!({"error": error}).to_string()),
        };
        if let Err(error) = lifecycle.validate() {
            return (400, serde_json::json!({"error": error}).to_string());
        }
        let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("").trim();
        let content = v
            .get("content")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim();
        if name.is_empty() || content.is_empty() {
            if let Some(response) = record_external_or_500(
                store,
                &context,
                "write",
                ExternalLifecycleStage::Validation,
                "failed",
                "invalid_request",
                None,
                None,
                "memory",
                "http",
                1,
            ) {
                return response;
            }
            return (400, "{\"error\":\"name and content required\"}".into());
        }
        if content.chars().count() > MAX_CONTENT_CHARS {
            if let Some(response) = record_external_or_500(
                store,
                &context,
                "write",
                ExternalLifecycleStage::Validation,
                "failed",
                "content_too_large",
                None,
                None,
                "memory",
                "http",
                1,
            ) {
                return response;
            }
            return (413, "{\"error\":\"content too large\"}".into());
        }
        let scope = v
            .get("scope")
            .and_then(|x| x.as_str())
            .unwrap_or("proposed");
        if let Err(message) = crate::scope::validate_write_scope(scope) {
            if let Some(response) = record_external_or_500(
                store,
                &context,
                "write",
                ExternalLifecycleStage::Validation,
                "failed",
                "invalid_scope",
                None,
                Some(scope),
                "memory",
                "http",
                1,
            ) {
                return response;
            }
            return (400, serde_json::json!({ "error": message }).to_string());
        }
        let tier = match v.get("tier").and_then(|x| x.as_str()).unwrap_or("Working") {
            t if t.eq_ignore_ascii_case("working") => rightkit_memory_core::MemoryTier::Working,
            t if t.eq_ignore_ascii_case("episodic") => rightkit_memory_core::MemoryTier::Episodic,
            _ => rightkit_memory_core::MemoryTier::Semantic,
        };
        let artifact_family = v
            .get("artifactFamily")
            .and_then(|x| x.as_str())
            .unwrap_or("memory")
            .trim();
        let producer = v
            .get("producer")
            .and_then(|x| x.as_str())
            .unwrap_or("manual")
            .trim();
        let record_type = v
            .get("recordType")
            .and_then(|x| x.as_str())
            .unwrap_or("memory")
            .trim();
        let memory_id = format!("{}/{}", crate::scope::normalize_scope(scope), name);
        return match store.try_put_attributed_lifecycle_observed(
            name,
            content,
            scope,
            tier,
            artifact_family,
            producer,
            record_type,
            &context,
            &lifecycle,
        ) {
            Ok(id) => {
                // Recall admits A1..A5 only, and an unspecified authority
                // defaults to A0 — so the default write is invisible to the
                // default read, and a bare `{"put": id}` gave no sign of it.
                // Say it at the point of writing rather than leaving the
                // caller to discover an empty recall later.
                let recall_eligible = matches!(
                    lifecycle.authority.as_deref(),
                    Some("A1") | Some("A2") | Some("A3") | Some("A4") | Some("A5")
                );
                let mut body = serde_json::json!({ "put": id, "recallEligible": recall_eligible });
                if !recall_eligible {
                    body["note"] = serde_json::json!(format!(
                        "stored at authority {}; recall admits A1..A5, so this row will not be recalled",
                        lifecycle.authority.as_deref().unwrap_or("A0")
                    ));
                }
                (200, body.to_string())
            }
            Err(e) if e.starts_with("memory write attribution") => {
                if let Some(response) = record_external_or_500(
                    store,
                    &context,
                    "write",
                    ExternalLifecycleStage::Validation,
                    "failed",
                    "invalid_attribution",
                    Some(&memory_id),
                    Some(scope),
                    "memory",
                    "http",
                    1,
                ) {
                    return response;
                }
                (400, serde_json::json!({ "error": e }).to_string())
            }
            Err(e) => {
                let (stage, status, reason) =
                    if e.contains("embed") || e.contains("writes disabled") {
                        (
                            ExternalLifecycleStage::Embedding,
                            "unavailable",
                            "embedding_unavailable",
                        )
                    } else {
                        (ExternalLifecycleStage::Commit, "failed", "commit_failed")
                    };
                if let Some(response) = record_external_or_500(
                    store,
                    &context,
                    "write",
                    stage,
                    status,
                    reason,
                    Some(&memory_id),
                    Some(scope),
                    if crate::context_telemetry::registered_artifact_family(artifact_family) {
                        artifact_family
                    } else {
                        "memory"
                    },
                    if producer.is_empty() {
                        "http"
                    } else {
                        producer
                    },
                    1,
                ) {
                    return response;
                }
                (500, serde_json::json!({ "error": e }).to_string())
            }
        };
    }
    if method == "POST" && path == "/remember" {
        let v = match json_body(body) {
            Ok(v) => v,
            Err(resp) => return resp,
        };
        let content = v
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if content.is_empty() {
            return (400, "{\"error\":\"content required\"}".into());
        }
        if content.chars().count() > MAX_CONTENT_CHARS {
            return (413, "{\"error\":\"content too large\"}".into());
        }
        let keywords = v
            .get("keywords")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        // `name`, `scope` and `tier` place the memory (`membrane memory push --name/--scope/
        // --tier`). A body without any of them is the original `remember`, unchanged.
        if ["name", "scope", "tier"]
            .iter()
            .any(|key| v.get(*key).is_some())
        {
            let text = |key: &str| v.get(key).and_then(Value::as_str);
            let scope = match text("scope") {
                Some(scope) => match crate::cli::canonical_write_scope(scope) {
                    Ok(scope) => Some(scope),
                    Err(error) => return (400, serde_json::json!({ "error": error }).to_string()),
                },
                None => None,
            };
            let tier = match text("tier") {
                Some(tier) => match crate::cli::memory_tier_from_name(&tier.to_lowercase()) {
                    Some(tier) => Some(tier),
                    None => {
                        return (
                            400,
                            serde_json::json!({ "error": format!("unknown tier `{tier}`") })
                                .to_string(),
                        );
                    }
                },
                None => None,
            };
            let name = text("name").map(str::trim);
            if let Some(name) = name {
                if let Err(error) = crate::cli::validate_memory_name(name) {
                    return (400, serde_json::json!({ "error": error }).to_string());
                }
            }
            return match store.remember_with(content, keywords, name, scope.as_deref(), tier) {
                Ok(entry) => (
                    200,
                    serde_json::to_string(&entry).unwrap_or_else(|_| "{}".into()),
                ),
                Err(error) => (500, serde_json::json!({ "error": error }).to_string()),
            };
        }
        let entry = store.remember(content, keywords);
        return (
            200,
            serde_json::to_string(&entry).unwrap_or_else(|_| "{}".into()),
        );
    }
    if method == "POST" && path == "/remember_consolidated" {
        let v = match json_body(body) {
            Ok(v) => v,
            Err(resp) => return resp,
        };
        let name = v.get("name").and_then(Value::as_str).unwrap_or("").trim();
        let content = v
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if name.is_empty() || content.is_empty() {
            return (400, "{\"error\":\"name and content required\"}".into());
        }
        if content.chars().count() > MAX_CONTENT_CHARS {
            return (413, "{\"error\":\"content too large\"}".into());
        }
        let keywords = v
            .get("keywords")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let threshold = v.get("threshold").and_then(Value::as_f64).unwrap_or(0.0);
        let entry = store.remember_consolidated(name, content, keywords, threshold);
        let response = match entry {
            Some(entry) => {
                let id = entry.id.clone();
                serde_json::json!({"id": id, "consolidated": true, "entry": entry})
            }
            None => serde_json::json!({"id": null, "consolidated": false}),
        };
        return (200, response.to_string());
    }
    if method == "POST" && path == "/delete" {
        let v = match json_body(body) {
            Ok(v) => v,
            Err(resp) => {
                let context = best_effort_event_context(body, "http");
                if let Some(response) = record_external_or_500(
                    store,
                    &context,
                    "delete",
                    ExternalLifecycleStage::Validation,
                    "failed",
                    "malformed_json",
                    None,
                    None,
                    "memory",
                    "http",
                    1,
                ) {
                    return response;
                }
                return resp;
            }
        };
        let context = event_context(&v, "http");
        let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("").trim();
        if id.is_empty() {
            if let Some(response) = record_external_or_500(
                store,
                &context,
                "delete",
                ExternalLifecycleStage::Validation,
                "failed",
                "missing_id",
                None,
                None,
                "memory",
                "http",
                1,
            ) {
                return response;
            }
            return (400, "{\"error\":\"missing id\"}".into());
        }
        return match store.try_delete_observed(id, &context) {
            Ok(true) => (200, serde_json::json!({ "deleted": true }).to_string()),
            Ok(false) => {
                if let Some(response) = record_external_or_500(
                    store,
                    &context,
                    "delete",
                    ExternalLifecycleStage::Commit,
                    "empty",
                    "target_not_found",
                    Some(id),
                    None,
                    "memory",
                    "http",
                    0,
                ) {
                    return response;
                }
                (404, serde_json::json!({ "deleted": false }).to_string())
            }
            Err(e) => {
                if let Some(response) = record_external_or_500(
                    store,
                    &context,
                    "delete",
                    ExternalLifecycleStage::Commit,
                    "failed",
                    "commit_failed",
                    Some(id),
                    None,
                    "memory",
                    "http",
                    1,
                ) {
                    return response;
                }
                (500, serde_json::json!({ "error": e }).to_string())
            }
        };
    }
    if method == "POST" && path == "/list" {
        let v = match json_body(body) {
            Ok(v) => v,
            Err(resp) => {
                let context = best_effort_event_context(body, "http");
                if let Some(response) = record_external_or_500(
                    store,
                    &context,
                    "read",
                    ExternalLifecycleStage::Provider,
                    "failed",
                    "malformed_json",
                    None,
                    None,
                    "memory",
                    "http",
                    1,
                ) {
                    return response;
                }
                return resp;
            }
        };
        let context = event_context(&v, "http");
        let scope = v
            .get("scope")
            .and_then(|x| x.as_str())
            .filter(|s| !s.trim().is_empty());
        let response_version = v
            .get("responseVersion")
            .or_else(|| v.get("response_version"))
            .and_then(Value::as_u64)
            .unwrap_or(1);
        let limit = v
            .get("limit")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(256);
        if response_version == 2 && (limit == 0 || limit > 1_000) {
            return (
                400,
                serde_json::json!({"error":"limit must be between 1 and 1000"}).to_string(),
            );
        }
        let (listed, completeness) = match if response_version == 2 {
            store
                .try_list_bounded(scope, limit)
                .map(|page| (page.items, Some(page.completeness)))
        } else {
            store.try_list(scope).map(|items| (items, None))
        } {
            Ok(result) => result,
            Err(error) => {
                if let Some(response) = record_external_or_500(
                    store,
                    &context,
                    "read",
                    ExternalLifecycleStage::Provider,
                    "failed",
                    "provider_failed",
                    None,
                    scope,
                    "memory",
                    "http",
                    1,
                ) {
                    return response;
                }
                return (500, serde_json::json!({ "error": error }).to_string());
            }
        };
        let rows: Vec<serde_json::Value> = listed
            .into_iter()
            .map(|(id, tier, chars, access, inject)| {
                serde_json::json!({"id": id, "tier": tier, "chars": chars,
                                   "access": access, "inject": inject})
            })
            .collect();
        let (status, reason) = if rows.is_empty() {
            ("empty", "no_results")
        } else {
            ("success", "results_returned")
        };
        if let Some(response) = record_external_or_500(
            store,
            &context,
            "read",
            ExternalLifecycleStage::Provider,
            status,
            reason,
            None,
            scope,
            "memory",
            "http",
            rows.len(),
        ) {
            return response;
        }
        return if response_version == 2 {
            (
                200,
                serde_json::json!({
                    "schemaVersion": 2,
                    "items": rows,
                    "completeness": completeness.expect("V2 list always has completeness"),
                })
                .to_string(),
            )
        } else {
            (200, serde_json::Value::Array(rows).to_string())
        };
    }
    if method == "POST" && path == "/scopes" {
        return (
            200,
            serde_json::to_string(&store.scopes()).unwrap_or_else(|_| "[]".into()),
        );
    }
    if method == "POST" && path == "/get" {
        let v = match json_body(body) {
            Ok(v) => v,
            Err(resp) => {
                let context = best_effort_event_context(body, "http");
                if let Some(response) = record_external_or_500(
                    store,
                    &context,
                    "read",
                    ExternalLifecycleStage::Provider,
                    "failed",
                    "malformed_json",
                    None,
                    None,
                    "memory",
                    "http",
                    1,
                ) {
                    return response;
                }
                return resp;
            }
        };
        let context = event_context(&v, "http");
        let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("").trim();
        if id.is_empty() {
            if let Some(response) = record_external_or_500(
                store,
                &context,
                "read",
                ExternalLifecycleStage::Provider,
                "failed",
                "missing_id",
                None,
                None,
                "memory",
                "http",
                1,
            ) {
                return response;
            }
            return (400, "{\"error\":\"missing id\"}".into());
        }
        return match store.get_full_observed(id, &context) {
            Ok((content, access_count)) => match store.lifecycle_json_for(id) {
                Ok(lifecycle) => (
                    200,
                    serde_json::json!({"id": id, "content": content, "access_count": access_count, "lifecycle": lifecycle})
                        .to_string(),
                ),
                Err(error) => (500, serde_json::json!({"error": error}).to_string()),
            },
            Err(e) => {
                let (http_status, status, reason) = if e.starts_with("no memory with id ") {
                    (404, "empty", "target_not_found")
                } else {
                    (500, "failed", "provider_failed")
                };
                if let Some(response) = record_external_or_500(
                    store,
                    &context,
                    "read",
                    ExternalLifecycleStage::Provider,
                    status,
                    reason,
                    Some(id),
                    None,
                    "memory",
                    "http",
                    usize::from(http_status != 404),
                ) {
                    return response;
                }
                (http_status, serde_json::json!({ "error": e }).to_string())
            }
        };
    }
    if method == "GET" && path == "/health" {
        return (200, store.health_json().to_string());
    }
    let request_parse_started = std::time::Instant::now();
    let v = if method == "POST" {
        match json_body(body) {
            Ok(v) => v,
            Err(resp) => return resp,
        }
    } else {
        serde_json::Value::Null
    };
    if method == "POST" && path == "/scratchpad" {
        return (200, crate::scratchpad::handle(&v).to_string());
    }
    if method == "POST" && path == "/scratchpad/session-close" {
        let session = v
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let (status, body) = crate::scratchpad::close_response(session);
        return (status, body.to_string());
    }
    let request_parse_ms = request_parse_started.elapsed().as_secs_f64() * 1000.0;
    if method == "POST" && path == "/recall" {
        let query = v.get("query").and_then(|x| x.as_str()).unwrap_or("").trim();
        if query.is_empty() {
            return (400, "{\"error\":\"query required\"}".to_string());
        }
        if query.chars().count() > MAX_QUERY_CHARS {
            return (413, "{\"error\":\"query too large\"}".to_string());
        }
        let client = v
            .get("client")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim();
        if client.is_empty() || client.eq_ignore_ascii_case("unknown") {
            return (
                400,
                "{\"error\":\"client attribution required\"}".to_string(),
            );
        }
        let k = v.get("k").and_then(|x| x.as_u64()).unwrap_or(6);
        if k == 0 || k > MAX_RECALL_K {
            return (
                400,
                "{\"error\":\"k must be between 1 and 50\"}".to_string(),
            );
        }
        let k = k as usize;
        let bounded_budget = |key: &str, default: usize, max: usize| {
            let value = v
                .get(key)
                .and_then(|x| x.as_u64())
                .map(|x| x as usize)
                .unwrap_or(default);
            if !(80..=max).contains(&value) {
                Err((
                    400,
                    serde_json::json!({"error": format!("{key} must be between 80 and {max}")})
                        .to_string(),
                ))
            } else {
                Ok(value)
            }
        };
        let preview_chars = match bounded_budget("preview_chars", 240, 400) {
            Ok(value) => value,
            Err(response) => return response,
        };
        let top_preview_chars = match bounded_budget("top_preview_chars", 400, 600) {
            Ok(value) => value,
            Err(response) => return response,
        };
        if top_preview_chars < preview_chars {
            return (
                400,
                "{\"error\":\"top_preview_chars must be >= preview_chars\"}".to_string(),
            );
        }
        let default_total = top_preview_chars + k.saturating_sub(1) * preview_chars;
        let total_preview_chars =
            match bounded_budget("total_preview_chars", default_total.min(4000), 4000) {
                Ok(value) => value,
                Err(response) => return response,
            };
        let observe = v.get("observe").and_then(|x| x.as_bool()).unwrap_or(true);
        let traffic_class = v
            .get("traffic_class")
            .and_then(|x| x.as_str())
            .unwrap_or("production")
            .trim();
        if traffic_class.is_empty() || traffic_class.chars().count() > 64 {
            return (400, "{\"error\":\"invalid traffic_class\"}".to_string());
        }
        let production_traffic = traffic_class.eq_ignore_ascii_case("production");
        let explicit_smoke = match v.get("context_source") {
            None => false,
            Some(value) => match value.as_str() {
                Some(marker) if marker.trim().eq_ignore_ascii_case("smoke") => true,
                _ => {
                    return (
                        400,
                        "{\"error\":\"context_source must be smoke when present\"}".to_string(),
                    );
                }
            },
        };
        let normalized_client = client.to_ascii_lowercase();
        let smoke_client = matches!(
            normalized_client.as_str(),
            "smoke" | "spotcheck" | "smoke-spotcheck"
        );
        let smoke_sink = explicit_smoke || smoke_client;
        if smoke_sink && (observe || production_traffic) {
            return (
                400,
                "{\"error\":\"smoke recall requires observe=false and nonproduction traffic_class\"}"
                    .to_string(),
            );
        }
        if !observe
            && (v.get("traffic_class").and_then(|x| x.as_str()).is_none() || production_traffic)
        {
            return (
                400,
                "{\"error\":\"observe=false requires explicit nonproduction traffic_class\"}"
                    .to_string(),
            );
        }
        let scope = v.get("scope").and_then(|x| x.as_str()).map(normalize_scope);
        let mut chain = match &scope {
            Some(s) => scope_chain(s, &store.scopes()),
            None => Vec::new(),
        };
        for c in str_list(&v, "cross") {
            if !chain.contains(&c) {
                chain.push(c);
            }
        }
        let bounded_taste_count = |key: &str, default: usize, max: usize| {
            let value = v
                .get(key)
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or(default);
            (value <= max).then_some(value).ok_or_else(|| {
                (
                    400,
                    serde_json::json!({"error": format!("{key} must be between 0 and {max}")})
                        .to_string(),
                )
            })
        };
        let taste_core_records = match bounded_taste_count("taste_max_core", 2, 4) {
            Ok(value) => value,
            Err(response) => return response,
        };
        let taste_scoped_records = match bounded_taste_count("taste_max_scoped", 4, 16) {
            Ok(value) => value,
            Err(response) => return response,
        };
        let model = match v.get("model") {
            None => None,
            Some(Value::String(value))
                if !value.trim().is_empty() && value.chars().count() <= 200 =>
            {
                Some(value.trim().to_string())
            }
            Some(_) => return (400, "{\"error\":\"invalid model dimension\"}".into()),
        };
        let machine = match v.get("machine") {
            None => None,
            Some(Value::String(value))
                if !value.trim().is_empty() && value.chars().count() <= 200 =>
            {
                Some(value.trim().to_string())
            }
            Some(_) => return (400, "{\"error\":\"invalid machine identity\"}".into()),
        };
        let mut raw_dimensions = BTreeMap::new();
        if let Some(dimensions) = v.get("dimensions") {
            let Some(dimensions) = dimensions.as_object() else {
                return (400, "{\"error\":\"dimensions must be an object\"}".into());
            };
            for (key, value) in dimensions {
                let Some(value) = value.as_str() else {
                    return (
                        400,
                        "{\"error\":\"dimension values must be strings\"}".into(),
                    );
                };
                let normalized_key = key.trim().to_ascii_lowercase();
                if normalized_key.is_empty() || raw_dimensions.contains_key(&normalized_key) {
                    return (
                        400,
                        serde_json::json!({"error": format!("duplicate normalized Taste dimension: {normalized_key}")})
                            .to_string(),
                    );
                }
                raw_dimensions.insert(normalized_key, value.trim().to_string());
            }
        }
        for (key, value) in [("client", Some(client)), ("model", model.as_deref())] {
            let Some(value) = value else { continue };
            if raw_dimensions
                .get(key)
                .is_some_and(|declared| !declared.eq_ignore_ascii_case(value))
            {
                return (
                    400,
                    serde_json::json!({"error": format!("{key} dimension conflicts with request attribution")})
                        .to_string(),
                );
            }
            raw_dimensions.insert(key.into(), value.into());
        }
        let taste_dimensions = match rightkit_adapt_taste::scope::ScopeDimensions::normalize(
            &raw_dimensions,
        ) {
            Ok(dimensions) => dimensions,
            Err(error) => return (
                400,
                serde_json::json!({"error": format!("invalid Taste scope dimensions: {error}")})
                    .to_string(),
            ),
        };
        let mut allowed_taste_scopes = if chain.is_empty() {
            vec!["global".to_string()]
        } else {
            chain.clone()
        };
        allowed_taste_scopes.sort();
        allowed_taste_scopes.dedup();
        let delivery_timestamp = crate::time::now_iso();
        let taste_request_id = format!(
            "taste-request-{}-{}",
            crate::time::now_millis(),
            TASTE_REQUEST_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let (inventory, delivery_plan) = match crate::adapt_service::select(
            store,
            &rightkit_adapt_taste::delivery::PreferenceDeliveryContextV1 {
                allowed_scopes: allowed_taste_scopes,
                dimensions: taste_dimensions,
                machine,
                max_core_records: taste_core_records,
                max_scoped_records: taste_scoped_records,
                max_total_records: k,
                max_rendered_chars: total_preview_chars,
                timestamp: delivery_timestamp,
                session_id: v
                    .get("session")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .into(),
                trace_id: v
                    .get("trace_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .into(),
                request_id: taste_request_id,
                client: client.into(),
                model,
            },
        ) {
            Ok(value) => value,
            Err(error) => {
                return (
                    503,
                    json!({"error":"adapt_delivery_unavailable","detail":error}).to_string(),
                );
            }
        };
        let mut full_chars = 0usize;
        let mut injected_chars = 0usize;
        let mut remaining_preview_chars = total_preview_chars;
        let mut hits = Vec::with_capacity(k);
        let mut seen_ids = HashSet::new();
        for delivered in delivery_plan.delivered {
            let Some(memory_id) = inventory.memory_id_for_record(&delivered.record_id) else {
                return (
                    500,
                    "{\"error\":\"selected Taste record lost its Cortex binding\"}".into(),
                );
            };
            let chars = delivered.rule.chars().count();
            full_chars += chars;
            injected_chars += chars;
            remaining_preview_chars = remaining_preview_chars.saturating_sub(chars);
            seen_ids.insert(memory_id.to_string());
            hits.push(serde_json::json!({
                "id": memory_id,
                "skel": delivered.rule,
                "type": "taste_preference",
                "scope": inventory.scope_for_record(&delivered.record_id).unwrap_or("global"),
                "kind": "taste_preference",
                "score": 1.0,
                "cos": 1.0,
                "origin": "taste",
                "delivery_receipt": delivered.receipt,
            }));
        }
        let generic_slots = k.saturating_sub(hits.len());
        let generic_candidates = if generic_slots == 0 {
            Vec::new()
        } else {
            store.recall_scored_detailed(
                query,
                generic_slots
                    .saturating_add(inventory.memory_ids.len())
                    .saturating_add(1),
                &chain,
            )
        };
        let generic_considered = generic_candidates.len();
        let mut generic_rank = 0usize;
        for hit in generic_candidates {
            if hits.len() >= k || inventory.memory_ids.contains(&hit.entry.id) {
                continue;
            }
            let e = hit.entry;
            let cos = hit.score;
            // Graduated top-1 (2026-07-05, E6): the top hit gets a bigger inline budget
            // ONLY when strongly relevant (cos >= 0.55, above the hook's 0.40 floor) —
            // usefulness delivered inline beats the fetch loop's friction, but an
            // out-of-domain top-1 must not inject a large irrelevant block.
            if remaining_preview_chars == 0 {
                break;
            }
            let desired = if generic_rank == 0 && cos >= 0.55 {
                top_preview_chars
            } else {
                preview_chars
            };
            let budget = desired.min(remaining_preview_chars);
            let skel = preview(&e.content, budget);
            full_chars += e.content.chars().count();
            let actual = skel.chars().count();
            injected_chars += actual;
            remaining_preview_chars = remaining_preview_chars.saturating_sub(actual);
            if !seen_ids.insert(e.id.clone()) {
                continue;
            }
            hits.push(serde_json::json!({
                "id": e.id, "skel": skel, "type": "memory",
                "scope": e.scope_id, "kind": "memory", "score": cos, "cos": cos,
                "origin": hit.origin,
            }));
            generic_rank += 1;
        }
        let ids: Vec<String> = hits
            .iter()
            .filter_map(|h| h.get("id").and_then(|x| x.as_str()).map(String::from))
            .collect();
        if observe {
            let context = event_context(&v, client);
            if let Err(e) = store.record_injections_observed(&ids, &context) {
                return (500, serde_json::json!({ "error": e }).to_string());
            }
        }
        let replay_hits = serde_json::Value::Array(
            hits.iter()
                .map(|hit| {
                    serde_json::json!({
                        "id": hit.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                        "score": hit.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0),
                        "origin": hit.get("origin").and_then(|v| v.as_str()).unwrap_or("semantic"),
                    })
                })
                .collect(),
        )
        .to_string();
        let log_result = store.log_recall_with_replay_to(
            if smoke_sink {
                crate::memdb::RecallLogSink::Smoke
            } else {
                crate::memdb::RecallLogSink::Production
            },
            &crate::time::now_iso(),
            scope.as_deref(),
            query.chars().count(),
            hits.len(),
            full_chars,
            injected_chars,
            "serve",
            Some(&query.chars().take(120).collect::<String>()),
            Some(client),
            v.get("session").and_then(|x| x.as_str()),
            v.get("cwd").and_then(|x| x.as_str()),
            v.get("hook_event").and_then(|x| x.as_str()),
            v.get("trace_id").and_then(|x| x.as_str()),
            v.get("client_visibility").and_then(|x| x.as_str()),
            traffic_class,
            &replay_hits,
            &replay_hits,
        );
        if smoke_sink {
            if let Err(error) = log_result {
                return (
                    500,
                    serde_json::json!({
                        "error": "smoke recall telemetry persistence failed",
                        "detail": error,
                    })
                    .to_string(),
                );
            }
        }
        // Delivery is only true once every other fallible pre-response step
        // has succeeded. Persist the complete decision immediately before the
        // response; a receipt failure still fails the request.
        if let Err(error) = store.persist_taste_delivery_receipts(
            &inventory,
            &delivery_plan.receipts,
            &crate::store::TasteDeliveryAttributionV1 {
                client: client.into(),
                session_id: v
                    .get("session")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .into(),
                turn_id: v.get("turn_id").and_then(Value::as_str).map(str::to_string),
                trace_id: v
                    .get("trace_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .into(),
                traffic_class: traffic_class.into(),
            },
        ) {
            return (
                500,
                serde_json::json!({"error": "Taste receipt persistence failed", "detail": error})
                    .to_string(),
            );
        }
        let response_version = v
            .get("responseVersion")
            .or_else(|| v.get("response_version"))
            .and_then(Value::as_u64)
            .unwrap_or(1);
        let mut completeness = if hits.len() >= k || generic_considered > generic_rank {
            crate::store::CortexCompletenessV1::lower_bound(
                "ceiling_truncated",
                hits.len().saturating_add(1),
                hits.len(),
                1,
            )
        } else {
            crate::store::CortexCompletenessV1::exact(hits.len(), hits.len(), 0)
        };
        if remaining_preview_chars == 0 && generic_considered > generic_rank {
            completeness.state = crate::store::CortexCompletenessState::LowerBound;
            completeness.counts_exact = false;
            if !completeness
                .causes
                .iter()
                .any(|cause| cause == "preview_budget_exhausted")
            {
                completeness
                    .causes
                    .push("preview_budget_exhausted".to_owned());
            }
        }
        if hits.is_empty()
            && store.last_recall_status().as_deref() == Some("insufficient_confidence")
        {
            (
                200,
                serde_json::json!({
                    "status": "insufficient_confidence",
                    "hits": [],
                    "completeness": completeness,
                })
                .to_string(),
            )
        } else if response_version == 2 {
            (
                200,
                serde_json::json!({
                    "schemaVersion": 2,
                    "hits": hits,
                    "completeness": completeness,
                })
                .to_string(),
            )
        } else {
            (
                200,
                serde_json::to_string(&hits).unwrap_or_else(|_| "[]".into()),
            )
        }
    } else if method == "POST" && path == "/search" {
        let query = v.get("query").and_then(Value::as_str).unwrap_or("").trim();
        if query.is_empty() {
            return (400, "{\"error\":\"query required\"}".to_string());
        }
        if query.chars().count() > MAX_QUERY_CHARS {
            return (413, "{\"error\":\"query too large\"}".to_string());
        }
        let limit = v
            .get("limit")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(6);
        let rows = store.search(query, limit);
        (
            200,
            serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string()),
        )
    } else if method == "POST" && path == "/activity" {
        let timestamp = v
            .get("timestamp")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(crate::time::now_iso);
        let scope = v.get("scope").and_then(Value::as_str);
        let query_chars = v
            .get("query_chars")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(0);
        let hit_count = v
            .get("hit_count")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(0);
        let full_chars = v
            .get("full_chars")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(0);
        let injected_chars = v
            .get("injected_chars")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(0);
        let source = v.get("source").and_then(Value::as_str).unwrap_or("http");
        let query_excerpt = v.get("query_excerpt").and_then(Value::as_str);
        let client = v.get("client").and_then(Value::as_str);
        let model = v.get("model").and_then(Value::as_str);
        let session = v.get("session").and_then(Value::as_str);
        let task = v.get("task").and_then(Value::as_str);
        let turn = v.get("turn").and_then(Value::as_str);
        let actor = v.get("actor").and_then(Value::as_str);
        let traffic_class = v
            .get("mode")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("production");
        let replay_hits = v.get("replay_hits").and_then(Value::as_str).unwrap_or("[]");
        let replay_context = v
            .get("replay_context")
            .and_then(Value::as_str)
            .unwrap_or(replay_hits);
        match store.log_recall_with_replay_to(
            crate::memdb::RecallLogSink::Production,
            &timestamp,
            scope,
            query_chars,
            hit_count,
            full_chars,
            injected_chars,
            source,
            query_excerpt,
            client,
            session,
            task,
            turn,
            actor,
            model.or(actor),
            traffic_class,
            replay_hits,
            replay_context,
        ) {
            Ok(()) => (200, "{\"ok\":true}".to_string()),
            Err(error) => (500, serde_json::json!({"error": error}).to_string()),
        }
    } else if method == "POST" && path == "/policy/assign" {
        let session = v
            .get("session")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let client = v
            .get("client")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let policy_version = v
            .get("policy_version")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let control_pct = v
            .get("control_pct")
            .and_then(|value| value.as_u64())
            .unwrap_or(10);
        let task_class = v
            .get("task_class")
            .and_then(|value| value.as_str())
            .filter(|value| {
                matches!(
                    *value,
                    "user_prompt_recall" | "code" | "research" | "writing" | "other"
                )
            })
            .unwrap_or("unknown");
        if control_pct > 100 {
            return (
                400,
                "{\"error\":\"control_pct must be between 0 and 100\"}".into(),
            );
        }
        match store.assign_context_policy(
            session,
            client,
            policy_version,
            control_pct as u8,
            task_class,
        ) {
            Ok(cohort) => (
                200,
                serde_json::json!({
                    "cohort": cohort,
                    "policy_version": policy_version,
                    "control_pct": control_pct,
                    "task_class": task_class,
                })
                .to_string(),
            ),
            Err(error) => (400, serde_json::json!({ "error": error }).to_string()),
        }
    } else if method == "POST" && path == "/curate" {
        let today = v
            .get("today")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| crate::time::now_iso().chars().take(10).collect());
        let context = event_context(&v, "curate");
        match store.dream_now_observed(&today, &context) {
            Ok(status) => (
                200,
                serde_json::json!({
                    "agent_id": status.agent_id,
                    "status": status.status,
                    "model": status.model,
                    "shell_allowed": status.shell_allowed,
                    "read_count": status.read_count,
                    "consolidated_count": status.consolidated_count,
                    "pruned_count": status.pruned_count,
                    "quarantined_count": status.quarantined_count,
                    "duplicate_quarantined_count": status.duplicate_quarantined_count,
                })
                .to_string(),
            ),
            Err(error) => (500, serde_json::json!({ "error": error }).to_string()),
        }
    } else if method == "POST" && path == "/quarantine/list" {
        (
            200,
            serde_json::json!({ "ids": store.quarantined_ids() }).to_string(),
        )
    } else if method == "POST" && path == "/quarantine/restore" {
        let id = match v.get("id").and_then(|value| value.as_str()) {
            Some(id) if !id.trim().is_empty() => id.trim(),
            _ => return (400, "{\"error\":\"missing id\"}".to_string()),
        };
        match store.restore_quarantined(id) {
            Ok(restored) => (
                200,
                serde_json::json!({ "id": id, "restored": restored }).to_string(),
            ),
            Err(error) => (500, serde_json::json!({ "error": error }).to_string()),
        }
    } else if method == "POST" && path == "/add" {
        (
            410,
            "{\"error\":\"/add disabled; use the cortex CLI for file ingestion\"}".to_string(),
        )
    } else if method == "POST" && path == "/use" {
        let id = match v.get("id").and_then(|x| x.as_str()) {
            Some(id) if !id.trim().is_empty() => id.trim(),
            _ => return (400, "{\"error\":\"missing id\"}".to_string()),
        };
        let context = event_context(&v, "http");
        match store.record_use_observed(id, &context) {
            Ok(n) => (200, format!("{{\"ok\":true,\"access_count\":{n}}}")),
            Err(e) => (500, format!("{{\"error\":{:?}}}", e)),
        }
    } else if method == "POST" && path == "/get" {
        let id = match v.get("id").and_then(|x| x.as_str()) {
            Some(id) if !id.trim().is_empty() => id.trim(),
            _ => return (400, "{\"error\":\"missing id\"}".to_string()),
        };
        let context = event_context(&v, "http");
        match store.get_full_observed(id, &context) {
            Ok((content, access_count)) => (
                200,
                serde_json::json!({ "id": id, "content": content, "access_count": access_count })
                    .to_string(),
            ),
            Err(e) => (404, format!("{{\"error\":{:?}}}", e)),
        }
    } else if method == "POST" && path == "/feedback" {
        let trace_id = v
            .get("trace_id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let candidate_id = v
            .get("candidate_id")
            .or_else(|| v.get("id"))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let content_sha256 = v
            .get("content_sha256")
            .or_else(|| v.get("sha"))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let outcome = match v
            .get("outcome")
            .and_then(|x| x.as_str())
            .map(crate::feedback::parse_outcome)
        {
            Some(Ok(o)) => o,
            _ => {
                return (
                    400,
                    "{\"error\":\"missing or invalid outcome\"}".to_string(),
                );
            }
        };
        let source = crate::feedback::parse_source(
            v.get("source")
                .and_then(|x| x.as_str())
                .unwrap_or("advisory"),
        );
        let verdict_ref = v
            .get("verdict_ref")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string());
        let scope_id = v
            .get("scope")
            .or_else(|| v.get("scope_id"))
            .and_then(|x| x.as_str())
            .unwrap_or("global")
            .to_string();
        let rec = crate::feedback::FeedbackRecord {
            trace_id,
            candidate_id,
            content_sha256,
            outcome,
            source,
            verdict_ref,
            scope_id,
        };
        match store.record_feedback(&rec) {
            Ok(()) => (
                200,
                format!("{{\"ok\":true,\"verified\":{}}}", rec.verified()),
            ),
            Err(e) => (400, format!("{{\"error\":{:?}}}", e)),
        }
    } else if method == "POST" && path == "/context/close-unknown" {
        let observed_since = v
            .get("observed_since")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim();
        let observed_through = v
            .get("observed_through")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim();
        let max_deliveries = v
            .get("max_deliveries")
            .and_then(|x| x.as_u64())
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(0);
        match store.close_unresolved_deliveries(observed_since, observed_through, max_deliveries) {
            Ok(closed) => (
                200,
                serde_json::json!({ "ok": true, "closed": closed }).to_string(),
            ),
            Err(error) => (400, serde_json::json!({ "error": error }).to_string()),
        }
    } else if method == "POST" && path == "/memory-candidates" {
        // Warm federation memory-candidate generation IN-PROCESS (the resident serve's embedder is
        // already loaded). The federation gateway's memory provider POSTs here instead of spawning
        // a cold `cortex memory-candidates` CLI, which reloads ONNX/fastembed (~3.6s) on every
        // prompt and blew the on-mode hook timeout. Same payload as the CLI verb.
        let task = v.get("task").and_then(|x| x.as_str()).unwrap_or("").trim();
        if task.is_empty() {
            return (400, "{\"error\":\"missing task\"}".to_string());
        }
        let descriptor = match v.get("scopeDescriptor") {
            Some(value) => {
                match serde_json::from_value::<crate::scope::ScopeDescriptorV1>(value.clone()) {
                    Ok(descriptor) => descriptor,
                    Err(error) => return (
                        400,
                        serde_json::json!({"error": format!("invalid scopeDescriptor: {error}")})
                            .to_string(),
                    ),
                }
            }
            None => crate::scope::ScopeDescriptorV1::filesystem(
                v.get("scope")
                    .and_then(|x| x.as_str())
                    .unwrap_or("D--Claude")
                    .trim(),
            ),
        };
        let max = v
            .get("max_candidates")
            .and_then(|x| x.as_u64())
            .unwrap_or(64) as usize;
        // F11: no `repoRoot` travels on this hot path (the federation gateway's `cortex.py`
        // provider never sends one — verified against `providers/cortex.py`), and this route's
        // whole reason to exist is the sub-350ms warm-serve budget the cold CLI blew (see the
        // comment above) — spending a real `git status` + graph-db read on every call to compute
        // a genuine freshness verdict would reintroduce that latency. Passing `None` here is the
        // honest choice per F11's contract: the candidate set's own freshness is unverifiable at
        // this call site, so `stale` reports that honestly instead of the old hardcoded `false`.
        let mut payload = match crate::pull::federation::memory_candidates_payload_for_descriptor(
            store,
            task,
            &descriptor,
            max,
            None,
        ) {
            Ok(payload) => payload,
            Err(error) => return (400, serde_json::json!({"error": error}).to_string()),
        };
        if let Some(stages) = payload
            .get_mut("_membrane")
            .and_then(|value| value.get_mut("stageElapsedMs"))
            .and_then(serde_json::Value::as_object_mut)
        {
            stages.insert("request_parse".to_string(), request_parse_ms.into());
        }
        (200, payload.to_string())
    } else if method == "POST" && path == "/verify-memory" {
        // Delivery provenance seal, client-agnostic: does `id` resolve to a real memory row whose
        // current content sha256 equals `sha`? Fail-closed. Both the Claude and Codex delivery
        // carve-outs call this before a memory preview reaches the model, so an unattested/forged
        // `agent_verified` label with content not in the trusted store is dropped. Warm, no `get`.
        let raw_id = v.get("id").and_then(|x| x.as_str()).unwrap_or("").trim();
        let id = raw_id.strip_prefix("memory:role:").unwrap_or(raw_id);
        let sha = v
            .get("sha")
            .or_else(|| v.get("sourceHash"))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .trim();
        let ok =
            !id.is_empty() && sha.len() == 64 && store.content_sha256(id).as_deref() == Some(sha);
        (200, format!("{{\"ok\":{ok}}}"))
    } else {
        (404, "{\"error\":\"unknown\"}".to_string())
    }
}

// ---- G3B planner surface --------------------------------------------------
//
// Planner routes are failure-isolated from memory routes. Each planner handler
// catches panics, timeouts, and SQLite failures and returns a bounded error
// payload WITHOUT mutating the `store`. The metrics + latency trackers are
// independent of the catalog — a planner crash leaves `get`/`put` entirely
// unaffected.

// ============================================================================
// resident.json: who is serving, and proof they are alive.
// ============================================================================

const RESIDENT_REFRESH: Duration = Duration::from_secs(10);

/// Path of this process's `resident.json`, set only by the instance that owns the
/// lock, so a second instance's exit never removes the owner's file.
static RESIDENT_JSON: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

fn write_resident_json(
    path: &std::path::Path,
    endpoint: &str,
    generation: u64,
    started: u64,
) -> std::io::Result<()> {
    let body = json!({
        "pid": std::process::id(),
        "endpoint": endpoint,
        "generation": generation,
        "started": started,
        "ts": now_unix_ms(),
        // Absolute path of the running `membrane` binary, so a caller without `host` on PATH
        // finds the CLI from this file (README "Finding the binary").
        "binary": std::env::current_exe()
            .ok()
            .map(|exe| exe.to_string_lossy().into_owned()),
    });
    rightkit_fs::atomic_write(path, format!("{body}\n").as_bytes()).map_err(std::io::Error::other)
}

fn remove_resident_json() {
    let Some(path) = RESIDENT_JSON.get() else {
        return;
    };
    let ours = std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| value.get("pid").and_then(Value::as_u64))
        == Some(u64::from(std::process::id()));
    if ours {
        let _ = std::fs::remove_file(path);
    }
}

/// Publishes `resident.json` and refreshes its `ts` every 10 s while alive.
/// Dropping it stops the refresh and removes the file.
struct ResidentFile {
    stop: crate::wake::Wake,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ResidentFile {
    fn publish(endpoint: &str, generation: u64) -> Result<Self, String> {
        let path = crate::paths::resident_json_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("create {}: {error}", parent.display()))?;
        }
        let started = now_unix_ms();
        write_resident_json(&path, endpoint, generation, started)
            .map_err(|error| format!("write {}: {error}", path.display()))?;
        let _ = RESIDENT_JSON.set(path.clone());
        let stop = crate::wake::Wake::new();
        let thread_stop = stop.clone();
        let endpoint = endpoint.to_owned();
        let thread = std::thread::Builder::new()
            .name("membrane-resident-json".into())
            .spawn(move || {
                // Blocks for the refresh period; Drop closes the wake so stop is immediate.
                while !thread_stop.is_closed() {
                    thread_stop.wait(RESIDENT_REFRESH);
                    if thread_stop.is_closed() {
                        break;
                    }
                    let _ = write_resident_json(&path, &endpoint, generation, started);
                }
            })
            .map_err(|error| format!("start resident.json refresh: {error}"))?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for ResidentFile {
    fn drop(&mut self) {
        self.stop.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        remove_resident_json();
    }
}

// ============================================================================
// Lifetime: the resident exits within 2 s of its parent (stdin pipe closed), and on SIGTERM or
// CTRL_CLOSE. Drain is cooperative; the watchdog makes the 2 s bound hold.
// ============================================================================

const FORCED_EXIT_AFTER: Duration = Duration::from_millis(1000);
static EXIT_WATCHDOG_STARTED: AtomicBool = AtomicBool::new(false);
static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

fn begin_shutdown(lifecycle: &crate::service::LifecycleControl, reason: &str) {
    // The watchdog starts first: whatever the drain request does (it can take a lock a busy store
    // open holds), the forced exit is already counting.
    if !EXIT_WATCHDOG_STARTED.swap(true, Ordering::AcqRel) {
        let _ = std::thread::Builder::new()
            .name("membrane-exit-watchdog".into())
            .spawn(|| {
                std::thread::sleep(FORCED_EXIT_AFTER);
                remove_resident_json();
                std::process::exit(0);
            });
    }
    lifecycle.request_drain(Some(reason));
}

/// Why the parent watch ended. The parent holds the write end of this process's stdin pipe for
/// as long as it lives, so the pipe closing is the parent going (even by SIGKILL).
const PARENT_STDIN_CLOSED: &str = "parent_exited";

fn log_parent_end() {
    crate::lifecycle_log::emit(serde_json::json!({
        "event": "membrane_parent_watch_ended",
        "reason": PARENT_STDIN_CLOSED,
    }));
}

fn start_watchers(lifecycle: crate::service::LifecycleControl) -> Result<(), String> {
    sys::install_stop_handlers();
    // The launcher gives this process a stdin pipe and holds it for the child's life
    // (`rightkit_daemon::private::watch_stdin_eof`, rightkit-daemon 0.4.0): the pipe closing
    // means the parent exited or let go, and the resident drains.
    let for_parent = lifecycle.clone();
    rightkit_daemon::private::watch_stdin_eof(move || {
        log_parent_end();
        begin_shutdown(&for_parent, PARENT_STDIN_CLOSED);
    });
    std::thread::Builder::new()
        .name("membrane-stop-signal".into())
        .spawn(move || {
            loop {
                if STOP_REQUESTED.load(Ordering::Acquire) {
                    begin_shutdown(&lifecycle, "stop_signal");
                    return;
                }
                // Blocks until the signal handler wakes it (self-pipe on Unix, a wake on Windows).
                sys::wait_stop();
            }
        })
        .map_err(|error| format!("start stop-signal watch: {error}"))?;
    Ok(())
}

#[cfg(unix)]
mod sys {
    use super::STOP_REQUESTED;
    use std::os::raw::c_int;
    use std::sync::atomic::{AtomicI32, Ordering};

    const SIGINT: c_int = 2;
    const SIGTERM: c_int = 15;

    /// Self-pipe ends (-1 until installed). The handler may only call async-signal-safe
    /// functions, so it writes one byte instead of touching a lock or condvar.
    static STOP_PIPE_READ: AtomicI32 = AtomicI32::new(-1);
    static STOP_PIPE_WRITE: AtomicI32 = AtomicI32::new(-1);

    unsafe extern "C" {
        fn signal(signum: c_int, handler: usize) -> usize;
        fn pipe(fds: *mut c_int) -> c_int;
        fn read(fd: c_int, buf: *mut u8, count: usize) -> isize;
        fn write(fd: c_int, buf: *const u8, count: usize) -> isize;
    }

    extern "C" fn on_stop(_signal: c_int) {
        STOP_REQUESTED.store(true, Ordering::SeqCst);
        let fd = STOP_PIPE_WRITE.load(Ordering::SeqCst);
        if fd >= 0 {
            let byte = 1u8;
            unsafe {
                write(fd, &byte, 1);
            }
        }
    }

    /// Blocks until a stop signal arrives (or a read is interrupted); the caller re-checks
    /// `STOP_REQUESTED`. Without a pipe it falls back to a 1 s sleep.
    pub(super) fn wait_stop() {
        let fd = STOP_PIPE_READ.load(Ordering::SeqCst);
        if fd < 0 {
            std::thread::sleep(std::time::Duration::from_secs(1));
            return;
        }
        let mut byte = 0u8;
        unsafe {
            read(fd, &mut byte, 1);
        }
    }

    pub(super) fn install_stop_handlers() {
        let mut fds = [-1 as c_int; 2];
        if unsafe { pipe(fds.as_mut_ptr()) } == 0 {
            STOP_PIPE_READ.store(fds[0], Ordering::SeqCst);
            STOP_PIPE_WRITE.store(fds[1], Ordering::SeqCst);
        }
        let handler = on_stop as extern "C" fn(c_int) as usize;
        unsafe {
            signal(SIGTERM, handler);
            signal(SIGINT, handler);
        }
    }
}

#[cfg(windows)]
mod sys {
    use super::STOP_REQUESTED;
    use std::sync::atomic::Ordering;

    /// Signalled by the console control handler so the stop-signal thread wakes at once.
    fn stop_wake() -> &'static crate::wake::Wake {
        static WAKE: std::sync::OnceLock<crate::wake::Wake> = std::sync::OnceLock::new();
        WAKE.get_or_init(crate::wake::Wake::new)
    }

    /// Blocks until a stop is requested or the backstop passes; the caller re-checks
    /// `STOP_REQUESTED`.
    pub(super) fn wait_stop() {
        stop_wake().wait(crate::wake::BACKSTOP);
    }

    const CTRL_C_EVENT: u32 = 0;
    const CTRL_BREAK_EVENT: u32 = 1;
    const CTRL_CLOSE_EVENT: u32 = 2;
    const CTRL_LOGOFF_EVENT: u32 = 5;
    const CTRL_SHUTDOWN_EVENT: u32 = 6;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetConsoleCtrlHandler(
            handler: Option<unsafe extern "system" fn(u32) -> i32>,
            add: i32,
        ) -> i32;
    }

    unsafe extern "system" fn on_ctrl(kind: u32) -> i32 {
        match kind {
            CTRL_C_EVENT | CTRL_BREAK_EVENT => {
                STOP_REQUESTED.store(true, Ordering::SeqCst);
                stop_wake().notify();
                1
            }
            CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => {
                STOP_REQUESTED.store(true, Ordering::SeqCst);
                stop_wake().notify();
                // Windows ends the process when this returns; hold it open so the
                // drain (and the watchdog's cleanup) run first.
                std::thread::sleep(std::time::Duration::from_secs(2));
                1
            }
            _ => 0,
        }
    }

    pub(super) fn install_stop_handlers() {
        unsafe {
            SetConsoleCtrlHandler(Some(on_ctrl), 1);
        }
    }
}

// ============================================================================
// Entrypoints
// ============================================================================

/// How a `membrane serve` invocation ended without error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentExit {
    /// Served until the parent went, a stop signal arrived, or the endpoint ended.
    Stopped,
    /// Another resident already holds the owner lock; this one did nothing.
    AlreadyServing,
}

/// Run the resident for the host app process that holds this process's stdin pipe. The owner lock is taken inside
/// `service::start_resident_engine`, before any store opens; losing it is reported as
/// [`ResidentExit::AlreadyServing`], not an error.
pub fn run_resident() -> Result<ResidentExit, String> {
    let lifecycle = crate::service::LifecycleControl::default();
    start_watchers(lifecycle.clone())?;
    let engine = match crate::service::start_resident_engine(lifecycle) {
        Ok(engine) => engine,
        Err(error)
            if error.starts_with("membrane_engine_already_owned")
                || error == "membrane_resident_already_active" =>
        {
            return Ok(ResidentExit::AlreadyServing);
        }
        Err(error) => return Err(error),
    };
    let served = run(&engine);
    let stopped = engine.stop("resident_exit");
    served.and(stopped).map(|_| ResidentExit::Stopped)
}

/// Open the stores and serve until asked to stop. Called once by `run_resident` after the
/// owner lock, identity, manifest and Blueprint supervisor are up.
///
/// The Cortex store is the only database the resident opens here; `RIGHTKIT_CONTEXT_CATALOG` still
/// names the catalog the in-process pull path opens on demand.
pub(crate) fn run(engine: &crate::service::ResidentEngine) -> Result<(), String> {
    let startup_started = engine.started;
    let identity = &engine.identity;
    let claim = &engine.claim;
    let emit_startup_stage = |stage: &str, stage_started: Instant| {
        crate::lifecycle_log::emit(serde_json::json!({
            "event": "membrane_startup_stage",
            "stage": stage,
            "durationMs": stage_started.elapsed().as_millis() as u64,
            "elapsedMs": startup_started.elapsed().as_millis() as u64,
        }));
    };
    let stage_started = Instant::now();
    let db_path = engine.db.as_path();
    if !db_path.is_absolute() {
        return Err(format!(
            "the Cortex database path must be absolute: {}",
            db_path.display()
        ));
    }
    let catalog_path = crate::catalog::resolve_catalog_path_from(
        crate::qa_env::catalog(),
        crate::qa_env::context_home(),
        Some(db_path.as_os_str().to_os_string()),
        crate::qa_env::workspace_root(),
    )
    .map_err(|error| error.to_string())?;
    let workspace_root =
        crate::runtime_receipt::resolve_workspace_root(crate::qa_env::workspace_root(), db_path)
            .map_err(|error| error.to_string())?;
    let configured_event_db =
        crate::runtime_receipt::validate_telemetry_event_binding(crate::qa_env::event_db())
            .map_err(|error| error.to_string())?;
    emit_startup_stage("serve_configuration", stage_started);
    let cortex_started = Instant::now();
    let store = MemoryStore::try_open(MemDb::open(db_path).map_err(|e| e.to_string())?)?;
    emit_startup_stage("cortex_store", cortex_started);
    let stage_started = Instant::now();
    crate::service::install_resident_store(store.clone())?;
    let telemetry_event_db = store.db().event_db_path().ok_or_else(|| {
        "resident Cortex store has no physical telemetry event database".to_string()
    })?;
    if configured_event_db
        .as_deref()
        .is_some_and(|configured| configured != telemetry_event_db)
    {
        return Err("resident Cortex telemetry event binding changed during startup".to_string());
    }
    let runtime_receipt = crate::runtime_receipt::RuntimeReceiptV2::new(
        &workspace_root,
        db_path,
        &catalog_path,
        telemetry_event_db,
        identity,
        claim,
    )
    .map_err(|error| error.to_string())?;
    emit_startup_stage("runtime_receipt_prepare", stage_started);
    let stage_started = Instant::now();
    let context_ingest_lease =
        crate::context_telemetry::ContextIngestLease::from_startup(identity, claim)
            .map_err(|error| format!("prepare telemetry ingest lease: {error}"))?;
    let prompt_telemetry_db = store.db().clone();
    let prompt_telemetry_lease = context_ingest_lease.clone();
    let prompt_telemetry_ingress =
        crate::context_telemetry::default_prompt_telemetry_ingress(db_path);
    // The drain blocks on this wake: a journal event signals it, stop closes it, and the
    // backstop covers a missed event. Without a working directory watch it polls at 1 s.
    let prompt_telemetry_running = crate::wake::Wake::new();
    let prompt_telemetry_thread_running = prompt_telemetry_running.clone();
    let prompt_telemetry_target = prompt_telemetry_ingress
        .parent()
        .zip(
            prompt_telemetry_ingress
                .file_name()
                .and_then(|name| name.to_str()),
        )
        .map(|(dir, name)| (dir.to_path_buf(), name.to_owned()));
    let prompt_telemetry_watch_wake = prompt_telemetry_running.clone();
    // Starts (or restarts) the journal watch; None while it cannot run.
    let start_prompt_telemetry_watch = move || {
        let (dir, name) = prompt_telemetry_target.as_ref()?;
        let name = name.clone();
        crate::wake::watch_dir_into(
            dir,
            // Producers append to the active journal; the drain's own cursor and
            // rejection sidecars must not wake it again.
            move |file| file == name,
            &prompt_telemetry_watch_wake,
        )
    };
    let mut prompt_telemetry_watch = start_prompt_telemetry_watch();
    let prompt_telemetry_thread = std::thread::Builder::new()
        .name("cortex-prompt-telemetry".to_string())
        .spawn(move || {
            let mut failed = false;
            while !prompt_telemetry_thread_running.is_closed() {
                if !prompt_telemetry_watch
                    .as_ref()
                    .is_some_and(|watch| watch.is_live())
                {
                    prompt_telemetry_watch = start_prompt_telemetry_watch();
                }
                let mut had_records = false;
                match crate::context_telemetry::drain_prompt_telemetry_ingress(
                    &prompt_telemetry_db,
                    &prompt_telemetry_lease,
                    &prompt_telemetry_ingress,
                ) {
                    Ok(drained) => {
                        had_records = drained.records > 0;
                        if failed {
                            stderr_line!("cortex prompt telemetry drain recovered");
                        }
                        failed = false;
                    }
                    Err(_) => {
                        if !failed {
                            stderr_line!("cortex prompt telemetry drain unavailable");
                        }
                        failed = true;
                    }
                }
                // Records still flowing: drain again at once (an event raised during the drain
                // is still pending on the wake). Otherwise block until the journal changes or
                // stop closes the wake; a failed drain retries at 1 s, an unwatched journal
                // polls at 1 s, a healthy idle drain waits out the 30 s backstop.
                if had_records {
                    continue;
                }
                let watched = prompt_telemetry_watch
                    .as_ref()
                    .is_some_and(|watch| watch.is_live());
                let wait_for = if failed || !watched {
                    crate::wake::DEGRADED_POLL
                } else {
                    crate::wake::BACKSTOP
                };
                prompt_telemetry_thread_running.wait(wait_for);
            }
        })
        .map_err(|error| format!("start prompt telemetry drain: {error}"))?;
    let prompt_telemetry_worker = PromptTelemetryWorker {
        running: prompt_telemetry_running,
        thread: Some(prompt_telemetry_thread),
    };
    emit_startup_stage("telemetry_worker", stage_started);
    let stage_started = Instant::now();
    let runtime_receipt_path = runtime_receipt
        .persist()
        .map_err(|error| error.to_string())?;
    std::env::set_var("RIGHTKIT_CONTEXT_RUNTIME_RECEIPT", &runtime_receipt_path);
    crate::runtime_receipt::publish_current(runtime_receipt);
    emit_startup_stage("runtime_receipt_persist", stage_started);
    stderr_line!(
        "membrane resident db={} catalog={}",
        db_path.display(),
        catalog_path.display()
    );

    let stage_started = Instant::now();
    let _cortex_recovery = crate::cortex_lifecycle::AdmissionRecoveryWorker::start(store.clone())?;
    emit_startup_stage("cortex_recovery_dispatch", stage_started);

    // Watchers, Blueprint refresh and indexers run for as long as this process does: background
    // authority was granted with the engine and closes only on drain.
    // Background memory review does NOT arm itself (Adrian 2026-10-29, C-M3: "we run it on demand
    // and then we figure out a pattern for it"): no BackgroundReviewRunner is started, so the
    // daily Cortex dream/consolidation, AdaptBehavioralReview and the corpus-health tick never
    // run on a schedule. On-demand review is the explicit `membrane memory curate` / POST /curate.
    let lifecycle = engine.lifecycle();
    stderr_line!("membrane background memory review: on demand only (not scheduled)");
    let stage_started = Instant::now();
    let endpoint = Arc::new(crate::service::start_endpoint(
        crate::service::EndpointContext {
            state_root: engine.state_root.clone(),
            store,
        },
    )?);
    let _resident_file = ResidentFile::publish(&endpoint.endpoint(), claim.startup_generation)?;
    emit_startup_stage("endpoint", stage_started);

    // Serve until any drain: parent exit, stop signal, or a failure elsewhere in the service.
    // Block until the endpoint releases. A drain closes `drain_wake`, and this helper then shuts
    // the endpoint, which releases the wait below; the 30 s timeout is only a backstop.
    let drain_wake = lifecycle.new_wake();
    let drain_endpoint = Arc::clone(&endpoint);
    let _ = std::thread::Builder::new()
        .name("membrane-endpoint-drain".into())
        .spawn(move || {
            while !drain_wake.is_closed() {
                drain_wake.wait(crate::wake::BACKSTOP);
            }
            drain_endpoint.shutdown();
        });
    while !lifecycle.shutdown_requested() && !endpoint.wait_released(crate::wake::BACKSTOP) {}
    lifecycle.request_drain(Some("endpoint_stopped"));
    endpoint.shutdown();
    prompt_telemetry_worker.stop()
}
