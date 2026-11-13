//! Canonical in-process Blueprint operation.
//!
//! This is the single owner of repository graph construction, publication,
//! status, and generation-bound query dispatch.  It deliberately has no
//! resident-service or process dependency.

use crate::api::{BlueprintError, BlueprintOperation, BlueprintRequest, RequestContext};
use crate::delta_store::{self, ApplyOptions, EventKind, FactBatch, FileDelta};
use crate::freshness::{stable_read_with_limit, StableReadError, MAX_SOURCE_FILE_BYTES};
use crate::graph::{self, GraphGeneration, GraphOptions};
use crate::model::Operation;
use crate::query;
use crate::refresh_view::{RefreshView, StoredEnvelope};
use crate::security::{canonical_root, is_confined_path};
use crate::store::{self, Generation, StoreError};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Native graph/store operation used by one-shot callers and resident hosts.
#[derive(Debug, Default)]
pub struct NativeBlueprintOperation;

/// Return the canonical Blueprint operation.  The returned trait object is
/// intentionally stateless; all durable state lives at the request root.
pub fn native_blueprint_operation() -> Arc<dyn BlueprintOperation> {
    Arc::new(NativeBlueprintOperation)
}

impl BlueprintOperation for NativeBlueprintOperation {
    fn execute(
        &self,
        request: &BlueprintRequest,
        context: &RequestContext,
    ) -> Result<Value, BlueprintError> {
        let mut result = self.execute_unfitted(request, context)?;
        // #554: a result that no operation fitted itself (doctor, findings, snapshots, document
        // truth, and any future one) is cut to the request's byte bound with `byte_ceiling`
        // omissions, never rejected as `blueprint_oversized`. The map export answers with a path
        // and counts and is not cut.
        if request.method != Operation::Export {
            query::fit_any_result(&mut result, request, context);
        }
        Ok(result)
    }
}

impl NativeBlueprintOperation {
    fn execute_unfitted(
        &self,
        request: &BlueprintRequest,
        context: &RequestContext,
    ) -> Result<Value, BlueprintError> {
        context.check()?;
        let root = confined_root(request, context)?;
        check_paths(request, &root)?;
        crate::graph_dir::retire_legacy_graph(&root);
        let db_path = store_path(&root);
        match request.method {
            Operation::Doctor => doctor(request, &root),
            Operation::Repair => repair(request, &root),
            Operation::Build | Operation::Refresh => {
                // A rebuilt or refreshed store leaves any cached copy of the old generation
                // unreachable; drop it on every exit so it is never held until the next query.
                let _evict = EvictCachedGeneration(&db_path);
                if request.generation.is_some()
                    || request
                        .input
                        .get("generation")
                        .and_then(Value::as_str)
                        .is_some()
                {
                    // The stored identity is in the envelope; never load the graph for it (#635).
                    let stored = RefreshView::open(&db_path)?.envelope()?;
                    ensure_generation_id(request, &stored.generation_id)?;
                }
                match verified_construction_reason(&db_path)? {
                    Some(verified) => {
                        defer_oversized_rebuild(&db_path)?;
                        build_and_publish(request, context, &root, &db_path, verified)
                    }
                    None => {
                        if let Some(unchanged) =
                            unchanged_generation_response(request, &root, &db_path)?
                        {
                            return Ok(unchanged);
                        }
                        incremental_refresh_with_repair(request, context, &root, &db_path)
                    }
                }
            }
            Operation::Status | Operation::DbStatus => status(request, context, &root, &db_path)
                .and_then(|value| bounded_generation_response(request, value)),
            Operation::Embed => {
                let (generation, _) = load_current_shared(&db_path)?;
                ensure_generation(request, &generation)?;
                let dense =
                    crate::hybrid_task::request_dense(Arc::clone(&generation), None, 0, context);
                Ok(
                    json!({"schemaVersion":1,"generationId":generation.generation_id,
                    "state":if dense.omissions.is_empty(){"complete"}else{"partial"},
                    "units":dense.units,"embedded":dense.embedded,"omissions":dense.omissions}),
                )
            }
            Operation::FindingsGet
            | Operation::FindingsExplain
            | Operation::FindingsEvidencePack
            | Operation::FindingsBaselineCapture
            | Operation::FindingsBaselineList
            | Operation::FindingsSarif => {
                // Findings are a projection over the same persisted native
                // generation used by query operations. Keep this dispatch in
                // the native owner so installed CLI callers cannot fall back
                // to a second JS detector or an unpinned live scan.
                let connection = store::open_store_read_only(&db_path).map_err(store_error)?;
                let (stored_nodes, stored_edges) =
                    store::stored_graph_rows(&connection).map_err(store_error)?;
                refuse_oversized_query_load(stored_nodes.saturating_add(stored_edges))?;
                let generation = store::load_generation(&connection)
                    .map_err(store_error)?
                    .ok_or_else(|| {
                        BlueprintError::new(
                            "blueprint_store_missing",
                            "no persisted Blueprint generation exists",
                        )
                    })?;
                let state_dir = crate::graph_dir::findings_baselines_dir(&root);
                crate::findings::execute_findings(&generation, request, context, &state_dir)
            }
            Operation::DocumentTruth => {
                // Ported from blueprint/src/lib/application/service.mjs
                // `documentTruth` (lane LIB5). Claims/edges/supersession are
                // relational store rows the GraphGeneration query path does
                // not carry, so this reads store::Generation directly, the
                // same pattern FindingsGet uses above.
                let connection = store::open_store_read_only(&db_path).map_err(store_error)?;
                let (stored_nodes, stored_edges) =
                    store::stored_graph_rows(&connection).map_err(store_error)?;
                refuse_oversized_query_load(stored_nodes.saturating_add(stored_edges))?;
                let generation = store::load_generation(&connection)
                    .map_err(store_error)?
                    .ok_or_else(|| {
                        BlueprintError::new(
                            "blueprint_store_missing",
                            "no persisted Blueprint generation exists",
                        )
                    })?;
                crate::lib_application_document_truth::execute_document_truth(&generation, request)
            }
            Operation::SnapshotGet => {
                let connection = store::open_store_read_only(&db_path).map_err(store_error)?;
                let name = request
                    .input
                    .get("snapshot")
                    .or_else(|| request.input.get("node"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| BlueprintError::missing("input.snapshot"))?;
                let (generation_id, snapshot) = {
                    // Only the generation id is needed: read the envelope, never the rows (#635).
                    let generation = store::read_generation_envelope(&connection)
                        .map_err(store_error)?
                        .ok_or_else(|| {
                            BlueprintError::new(
                                "blueprint_store_missing",
                                "no persisted Blueprint generation exists",
                            )
                        })?;
                    (
                        generation.generation_id().unwrap_or("").to_owned(),
                        crate::lib_application_snapshots::get_snapshot(&connection, name)?,
                    )
                };
                Ok(json!({"schemaVersion": 1, "generationId": generation_id, "snapshot": snapshot}))
            }
            Operation::SnapshotList => {
                let connection = store::open_store_read_only(&db_path).map_err(store_error)?;
                // Only the generation id is needed: read the envelope, never the rows (#635).
                let generation = store::read_generation_envelope(&connection)
                    .map_err(store_error)?
                    .ok_or_else(|| {
                        BlueprintError::new(
                            "blueprint_store_missing",
                            "no persisted Blueprint generation exists",
                        )
                    })?;
                let generation_id = generation.generation_id().unwrap_or("").to_owned();
                let snapshots = crate::lib_application_snapshots::list_snapshots(&connection)?;
                Ok(
                    json!({"schemaVersion": 1, "generationId": generation_id, "snapshots": snapshots}),
                )
            }
            Operation::Changes => {
                let connection = store::open_store_read_only(&db_path).map_err(store_error)?;
                // Only the generation id is needed: read the envelope, never the rows (#635).
                let generation = store::read_generation_envelope(&connection)
                    .map_err(store_error)?
                    .ok_or_else(|| {
                        BlueprintError::new(
                            "blueprint_store_missing",
                            "no persisted Blueprint generation exists",
                        )
                    })?;
                let generation_id = generation.generation_id().unwrap_or("").to_owned();
                let limit = request
                    .input
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(100);
                let snapshot = request.input.get("snapshot").and_then(Value::as_str);
                let since_generation = request.input.get("sinceGeneration").and_then(Value::as_str);
                let treeish = request.input.get("treeish");
                let head = request
                    .input
                    .get("head")
                    .and_then(Value::as_str)
                    .unwrap_or("HEAD");
                let treeish_pair = match treeish {
                    Some(Value::String(base)) => Some((base.as_str(), head)),
                    Some(Value::Object(map)) => {
                        let base = map
                            .get("base")
                            .or_else(|| map.get("from"))
                            .and_then(Value::as_str);
                        let treeish_head = map
                            .get("head")
                            .or_else(|| map.get("to"))
                            .and_then(Value::as_str)
                            .unwrap_or(head);
                        base.map(|b| (b, treeish_head))
                    }
                    _ => None,
                };
                let mut result = crate::lib_application_snapshots::changes_since_reference(
                    &connection,
                    &root,
                    snapshot,
                    since_generation,
                    treeish_pair,
                    limit,
                )?;
                if let Value::Object(object) = &mut result {
                    object.insert("generationId".into(), json!(generation_id));
                }
                Ok(result)
            }
            Operation::Federate => {
                crate::lib_application_federate::execute_federate(request, &context.cancellation)
            }
            Operation::Architecture
                if request.input.get("view").and_then(Value::as_str) == Some("changes") =>
            {
                architecture_changes(request, context, &root, &db_path)
            }
            Operation::Search
            | Operation::Resolve
            | Operation::Recall
            | Operation::Expand
            | Operation::Impact
            | Operation::Path
            | Operation::Architecture => {
                let (generation, source_observation) = load_current_shared(&db_path)?;
                ensure_generation(request, &generation)?;
                // #554: a search answers with the page its scan reaches inside its deadline, so it
                // does not fail here on an expired deadline (see `query::search`).
                if request.method != Operation::Search {
                    context.check()?;
                }
                // The freshness receipt compares the sealed basis against a
                // bounded live VCS observation; stale paths are confined to
                // files this generation actually indexed.
                let indexed_paths: HashSet<String> = generation
                    .files
                    .iter()
                    .map(|file| file.path.clone())
                    .collect();
                let receipt = generation_freshness_receipt(
                    &root,
                    &generation.generation_id,
                    source_observation.as_ref(),
                    Some(&indexed_paths),
                    FreshnessObservation::LiveBounded(context.deadline.remaining()),
                );
                let mut effective_request = request.clone();
                apply_freshness_suppression(&mut effective_request, &receipt);
                let dense = if request.method == Operation::Recall {
                    Some(crate::hybrid_task::request_dense(
                        Arc::clone(&generation),
                        Some(query::recall_task(&effective_request).to_owned()),
                        context.bounds.max_candidates,
                        context,
                    ))
                } else {
                    None
                };
                let mut result = query::execute_query_with_dense(
                    &generation,
                    &effective_request,
                    context,
                    dense,
                )?;
                if let Some(observation) = source_observation {
                    if let Value::Object(object) = &mut result {
                        object.insert("sourceObservation".into(), observation);
                    }
                }
                if let Value::Object(object) = &mut result {
                    object.insert("freshnessReceipt".into(), receipt);
                }
                // The query fitted its body before these receipts were added;
                // without a re-fit the wrapped response exceeds its bound and
                // the whole answer is rejected as `blueprint_oversized`.
                query::fit_to_request_bounds(&mut result, request, context);
                if request.method != Operation::Search {
                    context.check()?;
                }
                Ok(result)
            }
            Operation::Export => export_html_map(request, context, &db_path),
            operation => Err(BlueprintError::new(
                "unsupported_operation",
                format!(
                    "native Blueprint operation {} is unsupported",
                    operation.as_str()
                ),
            )),
        }
    }
}

fn confined_root(
    request: &BlueprintRequest,
    context: &RequestContext,
) -> Result<PathBuf, BlueprintError> {
    let requested = request
        .input
        .get("repoRoot")
        .and_then(Value::as_str)
        .ok_or_else(|| BlueprintError::missing("input.repoRoot"))?;
    let scoped = canonical_root(&context.scope.repo_root).map_err(|_| {
        BlueprintError::new(
            "root_escape",
            "repository root is unavailable or escapes its scope",
        )
    })?;
    let requested_root = canonical_root(Path::new(requested)).map_err(|_| {
        BlueprintError::new(
            "root_escape",
            "repository root is unavailable or escapes its scope",
        )
    })?;
    if scoped != requested_root {
        return Err(BlueprintError::new(
            "root_escape",
            "request repository root does not match canonical request scope",
        ));
    }
    Ok(scoped)
}

fn check_paths(request: &BlueprintRequest, root: &Path) -> Result<(), BlueprintError> {
    if let Some(paths) = request.input.get("paths").and_then(Value::as_array) {
        for raw in paths.iter().filter_map(Value::as_str) {
            let path = Path::new(raw);
            let candidate = if path.is_absolute() {
                path.to_path_buf()
            } else {
                root.join(path)
            };
            is_confined_path(root, &candidate, true).map_err(|_| {
                BlueprintError::new(
                    "root_escape",
                    "request path escapes canonical repository root",
                )
            })?;
        }
    }
    Ok(())
}

fn store_path(root: &Path) -> PathBuf {
    crate::graph_dir::graph_db_path(root)
}

/// What a refresh was asked to apply. A watcher or explicit caller names the paths; a startup or
/// Build/Refresh with none discovers them by comparing a digest-only scan with the stored hashes.
enum Change {
    Explicit(Vec<Value>),
    Discovered {
        paths: Vec<String>,
        files: Vec<graph::FileRecord>,
    },
}

/// Phase progress lines for a refresh, one per completed phase.
struct PhaseLog<'a> {
    request: &'a BlueprintRequest,
    started: Instant,
}

impl<'a> PhaseLog<'a> {
    fn new(request: &'a BlueprintRequest) -> Self {
        Self {
            request,
            started: Instant::now(),
        }
    }

    fn mark(&self, name: &str, detail: u64) {
        stderr_line!(
            "{}",
            json!({"event":"blueprint_incremental_phase","requestId":self.request.request_id,"phase":name,"elapsedMs":self.started.elapsed().as_millis(),"detail":detail})
        );
    }
}

fn parse_event_kind(request: &BlueprintRequest) -> Option<EventKind> {
    let name = request
        .input
        .get("eventKind")
        .and_then(Value::as_str)
        .unwrap_or("modify")
        .to_ascii_lowercase();
    match name.as_str() {
        "create" => Some(EventKind::Create),
        "modify" | "changed" => Some(EventKind::Modify),
        "delete" => Some(EventKind::Delete),
        "repair" => Some(EventKind::Repair),
        "rename" => Some(EventKind::Rename),
        _ => None,
    }
}

fn request_source_clock(request: &BlueprintRequest) -> i64 {
    request
        .input
        .get("sourceClock")
        .and_then(Value::as_u64)
        .and_then(|clock| i64::try_from(clock).ok())
        .unwrap_or(0)
}

/// Paths whose stored content hash differs from the scanned one, or that exist on one side only.
fn digest_delta(stored: &[(String, String)], files: &[graph::FileRecord]) -> Vec<String> {
    let strip = |hash: &str| -> String { hash.strip_prefix("xxh128:").unwrap_or(hash).to_owned() };
    let before: HashMap<&str, String> = stored
        .iter()
        .map(|(path, hash)| (path.as_str(), strip(hash)))
        .collect();
    let after: HashMap<&str, String> = files
        .iter()
        .map(|file| (file.path.as_str(), strip(&file.content_hash)))
        .collect();
    let mut paths = before
        .keys()
        .chain(after.keys())
        .copied()
        .collect::<Vec<_>>();
    paths.sort_unstable();
    paths.dedup();
    paths
        .into_iter()
        .filter(|path| before.get(path) != after.get(path))
        .map(str::to_owned)
        .collect()
}

/// A file record that carries identity only: path and stored hash, no bytes.
fn identity_file_record(root: &Path, path: &str, digest: &str) -> graph::FileRecord {
    graph::FileRecord {
        absolute_path: root.join(path),
        path: path.to_owned(),
        bytes: Vec::new(),
        text: None,
        size: 0,
        semantic_content_hash: digest.to_owned(),
        content_hash: digest.to_owned(),
    }
}

fn identity_symbol_stub(id: String, name: String, path: String) -> crate::model::GraphNode {
    crate::model::GraphNode {
        id,
        kind: "symbol".into(),
        path: Some(path),
        name: Some(name),
        generation_id: String::new(),
        evidence: Vec::new(),
    }
}

fn identity_file_stub(path: &str) -> crate::model::GraphNode {
    crate::model::GraphNode {
        id: format!("file:{path}"),
        kind: "file".into(),
        path: Some(path.to_owned()),
        name: None,
        generation_id: String::new(),
        evidence: Vec::new(),
    }
}

/// A refresh that cannot be answered from a partial view, or whose delta is too large for one,
/// either rebuilds the graph from scratch (only when the whole graph fits `FULL_GRAPH_BYTE_BUDGET`
/// and only through `full_graph_guard`) or is deferred with a typed error and the stored
/// generation kept. It never loads the stored graph (#635).
#[allow(clippy::too_many_arguments)] // the refresh request context, passed through as the caller holds it
fn full_build_or_defer(
    request: &BlueprintRequest,
    context: &RequestContext,
    root: &Path,
    db_path: &Path,
    stored: &StoredEnvelope,
    stored_rows: u64,
    trigger: &str,
    detail: Value,
) -> Result<Value, BlueprintError> {
    if !crate::memory::full_graph_fits(stored_rows) {
        let estimated = crate::memory::estimated_graph_bytes(stored_rows);
        let mut error = BlueprintError::new(
            "blueprint_incremental_deferred_memory_budget",
            format!(
                "{trigger}: a full build of {stored_rows} stored rows needs about {} MiB, above the {} MiB budget; refresh deferred, stored generation preserved",
                estimated / (1024 * 1024),
                crate::memory::FULL_GRAPH_BYTE_BUDGET / (1024 * 1024)
            ),
        );
        error.details = Some(json!({
            "preservedGeneration": true, "trigger": trigger, "detail": detail,
            "generationId": stored.generation_id, "complete": stored.complete,
            "storedRows": stored_rows, "estimatedBytes": estimated,
            "byteBudget": crate::memory::FULL_GRAPH_BYTE_BUDGET,
        }));
        return Err(error);
    }
    let result = build_and_publish(
        request,
        context,
        root,
        db_path,
        VerifiedConstructionReason::IncrementalFallback,
    );
    crate::memory::release_unused_memory();
    result
}

/// Apply one change set by rebuilding only the changed files and the files that reference them.
/// The stored graph is read through path and edge indexes (`RefreshView`) and the delta is
/// written in one transaction; the whole generation is never materialised (#635). Untouched
/// source rows keep their content hashes and never pass through a provider.
fn incremental_refresh_with_repair(
    request: &BlueprintRequest,
    context: &RequestContext,
    root: &Path,
    db_path: &Path,
) -> Result<Value, BlueprintError> {
    let mut pending = request
        .input
        .get("paths")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let head_marker = pending
        .iter()
        .any(|path| path.as_str() == Some(".git/HEAD"));
    if head_marker {
        pending.clear();
    }
    let unsupported = |reason: &str| {
        let mut error = BlueprintError::new("blueprint_incremental_unsupported", reason);
        error.details =
            Some(json!({"preservedGeneration": true, "pendingChanges": pending.clone()}));
        error
    };
    let log = PhaseLog::new(request);
    // One refresh holds one bounded working set at a time, whichever repository it is for.
    let _refresh = crate::memory::refresh_guard();
    context.check()?;
    let view = RefreshView::open(db_path)?;
    let stored = view.envelope()?;
    if !stored.complete {
        return Err(unsupported("existing generation is incomplete"));
    }
    let (stored_nodes, stored_edges, stored_files) = view.counts()?;
    log.mark("load", stored_nodes.saturating_add(stored_edges));
    context.check()?;
    // Watcher and explicit callers already identify the changed paths. Keep
    // this path-scoped branch ahead of discovery so a known event never walks
    // the repository just to rediscover the event it supplied.
    if !pending.is_empty() {
        return incremental_partial(
            request,
            context,
            root,
            db_path,
            view,
            stored,
            Change::Explicit(pending.clone()),
            false,
        );
    }
    let scan = graph::scan_repository_digests_with_cancellation(
        root,
        &graph::ScanOptions::default(),
        &context.cancellation,
    )
    .map_err(|error| match error {
        graph::GraphError::Cancelled if context.cancellation.deadline_expired() => {
            BlueprintError::deadline()
        }
        graph::GraphError::Cancelled => BlueprintError::cancelled(),
        error => BlueprintError::new("blueprint_refresh_scan_failed", error.to_string()),
    })?;
    log.mark("scan", scan.files.len() as u64);
    if scan.traversal_truncated || scan.file_limit_reached {
        return Err(unsupported("change discovery is incomplete"));
    }
    let changed = digest_delta(&view.file_hashes()?, &scan.files);
    if parse_event_kind(request).is_none() {
        return Err(unsupported("event kind lacks incremental implementation"));
    }
    if !changed.is_empty() {
        return incremental_partial(
            request,
            context,
            root,
            db_path,
            view,
            stored,
            Change::Discovered {
                paths: changed,
                files: scan.files,
            },
            false,
        );
    }
    let observation = crate::git_source_observation::git_source_observation(
        &root.to_string_lossy(),
    )
    .map(|value| {
        json!({
            "sourceClock": request.input.get("sourceClock"),
            "eventKind": request.input.get("eventKind"),
            "paths": request.input.get("paths"),
            "head": value.head,
            "dirty": value.dirty,
            "statusDigest": value.status_digest,
            "sealedAtUnixMs": now_unix_ms(),
        })
    });
    let metadata_changed = observation.as_ref().is_some_and(|live| {
        stored
            .observation
            .as_ref()
            .map(|sealed| {
                sealed.get("head") != live.get("head")
                    || sealed.get("dirty") != live.get("dirty")
                    || sealed.get("statusDigest") != live.get("statusDigest")
            })
            .unwrap_or(true)
    });
    if !metadata_changed {
        return bounded_generation_response(
            request,
            json!({
                "schemaVersion":1,"operation":request.method.as_str(),"state":"fresh","refreshMode":"incremental_noop",
                "generationId":stored.generation_id,"repoRoot":root.to_string_lossy(),"storePath":db_path.to_string_lossy(),
                "sourceHash":stored.source_hash,"complete":stored.complete,"truncationReasons":stored.truncation_reasons,
                "counts":{"nodes":stored_nodes,"edges":stored_edges,"files":stored_files},
                "sourceObservation":stored.observation.clone().unwrap_or(Value::Null)
            }),
        );
    }
    let observation =
        observation.ok_or_else(|| unsupported("git source observation unavailable"))?;
    drop(view);
    let mut connection = open_store(db_path)?;
    delta_store::apply_source_observation(
        &mut connection,
        &observation,
        request_source_clock(request),
    )
    .map_err(|error| delta_error(context, error))?;
    let status = post_apply_status(db_path)?;
    bounded_generation_response(
        request,
        json!({
            "schemaVersion": 1, "operation": request.method.as_str(), "state": "fresh",
            "refreshMode": "metadata", "generationId": status.generation_id,
            "repoRoot": root.to_string_lossy(), "storePath": db_path.to_string_lossy(),
            "sourceHash": status.source_hash, "complete": status.complete,
            "truncationReasons": status.truncation_reasons,
            "counts": {"nodes": status.nodes, "edges": status.edges, "files": status.files},
            "invalidatedPaths": [], "reusedFiles": status.files,
            "sourceObservation": status.source_observation,
        }),
    )
}

/// Apply a discovered delta that is too large for one partial refresh, over a graph too large for
/// a full build, as bounded batches of changed files. Each batch is a complete partial refresh
/// against the post-change file set (its own referrers, vanished-target reset and transaction), so
/// progress persists batch by batch and an interrupted run resumes from the stored file hashes.
/// Deleted files go first so referrers of a deleted file are reset before later batches read them.
/// A batch whose own rows and referrers still exceed the budget is deferred with the typed error.
fn incremental_batches(
    request: &BlueprintRequest,
    context: &RequestContext,
    root: &Path,
    db_path: &Path,
    files: Vec<graph::FileRecord>,
    changed: Vec<String>,
) -> Result<Value, BlueprintError> {
    let after: HashSet<&str> = files.iter().map(|file| file.path.as_str()).collect();
    let mut changed = changed;
    changed.sort_by_key(|path| after.contains(path.as_str()));
    let mut batches: Vec<Vec<String>> = Vec::new();
    {
        let view = RefreshView::open(db_path)?;
        let budget_rows =
            crate::memory::PARTIAL_REFRESH_BYTE_BUDGET / crate::memory::ESTIMATED_BYTES_PER_ROW / 2;
        let mut current: Vec<String> = Vec::new();
        let mut rows = 0u64;
        for path in changed {
            context.check()?;
            let cost = view.rows_for_path(&path)?.max(1);
            if !current.is_empty() && rows.saturating_add(cost) > budget_rows {
                batches.push(std::mem::take(&mut current));
                rows = 0;
            }
            rows = rows.saturating_add(cost);
            current.push(path);
        }
        if !current.is_empty() {
            batches.push(current);
        }
    }
    let total = batches.len();
    let log = PhaseLog::new(request);
    let mut last = Value::Null;
    for (index, batch) in batches.into_iter().enumerate() {
        context.check()?;
        let view = RefreshView::open(db_path)?;
        let stored = view.envelope()?;
        last = incremental_partial(
            request,
            context,
            root,
            db_path,
            view,
            stored,
            Change::Discovered {
                paths: batch,
                files: files.clone(),
            },
            true,
        )?;
        log.mark("batch", (index + 1) as u64);
    }
    if let Some(object) = last.as_object_mut() {
        object.insert("batches".into(), json!(total));
    }
    Ok(last)
}

#[allow(clippy::too_many_arguments)] // the refresh request context, passed through as the caller holds it
fn incremental_partial(
    request: &BlueprintRequest,
    context: &RequestContext,
    root: &Path,
    db_path: &Path,
    view: RefreshView,
    stored: StoredEnvelope,
    change: Change,
    batched: bool,
) -> Result<Value, BlueprintError> {
    let pending_report: Vec<Value> = match &change {
        Change::Explicit(pending) => pending.clone(),
        Change::Discovered { paths, .. } => paths.iter().cloned().map(Value::String).collect(),
    };
    let unsupported = |reason: &str| {
        let mut error = BlueprintError::new("blueprint_incremental_unsupported", reason);
        error.details =
            Some(json!({"preservedGeneration": true, "pendingChanges": pending_report.clone()}));
        error
    };
    let log = PhaseLog::new(request);
    let Some(event_kind) = parse_event_kind(request) else {
        return Err(unsupported("event kind lacks incremental implementation"));
    };
    let (mut paths, discovered_files) = match change {
        Change::Explicit(pending) => (
            pending
                .iter()
                .filter_map(Value::as_str)
                .map(graph::normalize_path)
                .collect::<Vec<_>>(),
            None,
        ),
        Change::Discovered { paths, files } => (paths, Some(files)),
    };
    if paths.iter().any(|path| {
        path.is_empty()
            || path.starts_with('/')
            || path.contains("..")
            || path.starts_with(".agent/")
    }) {
        return Err(unsupported("pending path is outside incremental scope"));
    }
    paths.dedup();
    let discovered = discovered_files.is_some();
    let rename_to = if event_kind == EventKind::Rename {
        if discovered {
            return Err(unsupported("rename requires old & new paths"));
        }
        let destination = request
            .input
            .get("renameTo")
            .and_then(Value::as_str)
            .map(graph::normalize_path);
        match (paths.len(), destination) {
            (1, Some(destination)) if !destination.is_empty() => {
                paths.push(destination);
                Some(paths[1].clone())
            }
            (2, None) => Some(paths[1].clone()),
            (2, Some(destination)) if destination == paths[1] => Some(destination),
            _ => return Err(unsupported("rename requires old & new paths")),
        }
    } else {
        paths.sort();
        None
    };
    if paths.is_empty() {
        return Err(unsupported("explicit event set is empty"));
    }
    context.check()?;

    let stored_hashes = view.file_hashes()?;
    let before_paths = stored_hashes
        .iter()
        .map(|(path, _)| path.clone())
        .collect::<HashSet<_>>();
    let stored_hash_by_path: HashMap<&str, &str> = stored_hashes
        .iter()
        .map(|(path, hash)| (path.as_str(), hash.as_str()))
        .collect();
    let mut observed = HashMap::<String, graph::FileRecord>::new();
    let mut changed_paths = Vec::new();
    let mut noop_paths = Vec::new();
    let after_paths: HashSet<String>;
    let mut source_files: Vec<graph::FileRecord>;
    if let Some(files) = discovered_files {
        // A discovered change set is already a difference against the stored hashes, and the
        // digest-only scan is the post-change identity of every file.
        after_paths = files.iter().map(|file| file.path.clone()).collect();
        source_files = files;
        changed_paths = paths.clone();
    } else {
        for path in &paths {
            let is_deleted = event_kind == EventKind::Delete
                || (event_kind == EventKind::Rename && Some(path.as_str()) != rename_to.as_deref());
            if is_deleted {
                continue;
            }
            context.check()?;
            match stable_explicit_file(root, path) {
                Ok(Some(file)) => {
                    observed.insert(path.clone(), file);
                }
                Ok(None) => return Err(unsupported(&format!("source path is absent: {path}"))),
                Err(error) => return Err(error),
            }
        }
        // The event set is explicit, so derive membership from the stored graph
        // and supplied observations only; no untouched path is probed.
        after_paths = {
            let mut result = before_paths.clone();
            match event_kind {
                EventKind::Delete => {
                    for path in &paths {
                        result.remove(path);
                    }
                }
                EventKind::Rename => {
                    result.remove(&paths[0]);
                    result.insert(rename_to.clone().unwrap());
                }
                _ => {
                    for path in &paths {
                        result.insert(path.clone());
                    }
                }
            }
            result
        };
        for path in &paths {
            let previous = stored_hash_by_path.get(path.as_str()).copied();
            let current_digest = observed.get(path).map(|file| file.content_hash.as_str());
            let same_digest = previous
                .zip(current_digest)
                .is_some_and(|(before, after)| normalize_digest(before) == normalize_digest(after));
            if event_kind != EventKind::Repair && event_kind != EventKind::Rename && same_digest {
                noop_paths.push(path.clone());
            } else {
                changed_paths.push(path.clone());
            }
        }
        if event_kind == EventKind::Delete && paths.iter().any(|path| !before_paths.contains(path))
        {
            return Err(unsupported("delete path is absent from stored graph"));
        }
        if event_kind == EventKind::Modify && paths.iter().any(|path| !observed.contains_key(path))
        {
            return Err(unsupported("modify path is absent from repository"));
        }
        source_files = stored_hashes
            .iter()
            .filter(|(path, _)| !paths.contains(path))
            .map(|(path, hash)| identity_file_record(root, path, hash))
            .collect();
        source_files.extend(observed.values().cloned());
        source_files.sort_by(|a, b| a.path.cmp(&b.path));
    }
    log.mark("observe", observed.len() as u64);

    let source_hash = graph::source_hash_for_files(&source_files);
    let config_digest = crate::static_provider::build_config_digest_for_files(&source_files);
    let observation =
        crate::git_source_observation::git_source_observation(&root.to_string_lossy());
    let source_clock = request_source_clock(request);
    let provider_file_map: std::collections::BTreeMap<String, &graph::FileRecord> = source_files
        .iter()
        .map(|file| (file.path.clone(), file))
        .collect();
    if changed_paths.is_empty() {
        let (stored_nodes, stored_edges, _) = view.counts()?;
        drop(view);
        let mut deltas = Vec::new();
        for path in &noop_paths {
            deltas.push(file_delta_from_graph(
                path,
                event_kind,
                None,
                observed.get(path),
                Vec::new(),
                source_clock,
                &source_hash,
                config_digest.clone(),
                observation.clone(),
                request,
            )?);
        }
        if !deltas.is_empty() {
            let mut connection = open_store(db_path)?;
            delta_store::apply_file_deltas(
                &mut connection,
                &deltas,
                ApplyOptions {
                    cancellation: Some(context.cancellation.clone()),
                    ..ApplyOptions::default()
                },
            )
            .map_err(|error| delta_error(context, error))?;
        }
        return bounded_generation_response(
            request,
            json!({
                "schemaVersion": 1, "operation": request.method.as_str(), "state": "fresh", "refreshMode": "incremental_noop",
                "generationId": stored.generation_id, "repoRoot": root.to_string_lossy(), "storePath": db_path.to_string_lossy(),
                "sourceHash": stored.source_hash, "complete": stored.complete, "truncationReasons": stored.truncation_reasons,
                "counts": {"nodes": stored_nodes, "edges": stored_edges, "files": before_paths.len()},
                "invalidatedPaths": [], "reusedFiles": source_files.len(),
                "sourceObservation": {"sourceClock": request.input.get("sourceClock"), "eventKind": request.input.get("eventKind"),
                    "paths": request.input.get("paths"), "head": observation.as_ref().map(|value| value.head.clone()),
                    "dirty": observation.as_ref().map(|value| value.dirty), "statusDigest": observation.as_ref().map(|value| value.status_digest.clone()),
                    "sealedAtUnixMs": now_unix_ms()}
            }),
        );
    }

    log.mark("detect", changed_paths.len() as u64);
    // Rebuild set: the changed files plus the files whose edges point at their nodes. A caller
    // whose edge targets a symbol that moved or vanished is the only file a change can leave
    // inconsistent; files that merely sit near it in the graph are untouched.
    let (stored_nodes, stored_edges, _) = view.counts()?;
    let stored_rows = stored_nodes.saturating_add(stored_edges);
    let mut ordered = changed_paths
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    for path in view.referencing_paths(&changed_paths)? {
        if before_paths.contains(&path) && after_paths.contains(&path) {
            ordered.insert(path);
        }
    }
    let mut ordered = ordered.into_iter().collect::<Vec<_>>();
    let mut affected_rows = 0u64;
    for path in &ordered {
        affected_rows = affected_rows.saturating_add(view.rows_for_path(path)?);
    }
    log.mark("closure", ordered.len() as u64);
    context.check()?;
    // The percentage rule only matters for repositories large enough that a partial rewrite of a
    // quarter of the files costs more than one build.
    let share_exceeded = ordered.len() >= crate::memory::PARTIAL_REFRESH_PERCENT_MIN_FILES
        && (ordered.len() as u64).saturating_mul(100)
            > (after_paths.len() as u64)
                .saturating_mul(crate::memory::PARTIAL_REFRESH_MAX_FILE_PERCENT);
    let over_budget = if batched {
        // A batch has no share rule; only its own row budget bounds it.
        !crate::memory::partial_refresh_fits(affected_rows)
    } else {
        share_exceeded || !crate::memory::partial_refresh_fits(affected_rows)
    };
    if over_budget && discovered && !batched && !crate::memory::full_graph_fits(stored_rows) {
        // The stored graph is too large for a full build, so deferring would repeat forever.
        // Apply the delta in bounded batches, each its own transaction (#635).
        drop(view);
        return incremental_batches(request, context, root, db_path, source_files, changed_paths);
    }
    if over_budget {
        drop(view);
        return full_build_or_defer(
            request,
            context,
            root,
            db_path,
            &stored,
            stored_rows,
            "delta too large for a partial refresh",
            json!({"affectedFiles": ordered.len(), "affectedRows": affected_rows, "totalFiles": after_paths.len()}),
        );
    }

    let source_files_by_path: HashMap<&str, &graph::FileRecord> = source_files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();
    let changed_set: HashSet<&str> = changed_paths.iter().map(String::as_str).collect();
    let mut target_events = HashMap::new();
    for target in &ordered {
        context.check()?;
        let target_file = source_files_by_path.get(target.as_str()).copied();
        let is_old_rename = event_kind == EventKind::Rename && target == &paths[0];
        let target_event = if is_old_rename || !after_paths.contains(target) {
            EventKind::Delete
        } else if !before_paths.contains(target) {
            EventKind::Create
        } else if changed_set.contains(target.as_str()) {
            event_kind
        } else {
            EventKind::Repair
        };
        if target_event != EventKind::Delete && target_file.is_none() {
            return Err(unsupported(&format!(
                "source facts unavailable for {target}"
            )));
        }
        target_events.insert(target.clone(), target_event);
    }
    log.mark("targets", ordered.len() as u64);

    let mut facts_by_path: HashMap<String, graph::FileFacts> = HashMap::new();
    for target in &ordered {
        context.check()?;
        if target_events[target] == EventKind::Delete {
            continue;
        }
        let facts = if changed_set.contains(target.as_str()) {
            // Cross-file calls are resolved once below against the post-change symbol set.
            graph::build_file_facts_from_scan(
                root,
                target,
                &source_files,
                &[],
                &context.cancellation,
            )
            .map_err(|error| match error {
                graph::GraphError::Cancelled if context.cancellation.deadline_expired() => {
                    BlueprintError::deadline()
                }
                graph::GraphError::Cancelled => BlueprintError::cancelled(),
                error => BlueprintError::new("blueprint_incremental_failed", error.to_string()),
            })?
            .ok_or_else(|| unsupported(&format!("source facts unavailable for {target}")))?
        } else {
            view.facts(target)?.ok_or_else(|| {
                unsupported(&format!("stored source facts unavailable for {target}"))
            })?
        };
        facts_by_path.insert(target.clone(), facts);
    }
    log.mark("facts", facts_by_path.len() as u64);

    // A call resolves only when exactly one symbol carries its name, so a change matters to other
    // files only where it moved a name across that boundary (none to one, one to two, two to one,
    // one to none). Count each changed name before and after, over the symbols the change did not
    // touch, and find the files whose stored edges name those transitions.
    let mut in_changed_before: HashMap<String, usize> = HashMap::new();
    for path in &changed_paths {
        for name in view.symbol_names(path)? {
            *in_changed_before.entry(name).or_default() += 1;
        }
    }
    let mut in_changed_after: HashMap<String, usize> = HashMap::new();
    for (path, facts) in &facts_by_path {
        if !changed_set.contains(path.as_str()) {
            continue;
        }
        for node in &facts.nodes {
            if let (true, Some(name)) = (node.kind == "symbol", node.name.as_ref()) {
                *in_changed_after.entry(name.clone()).or_default() += 1;
            }
        }
    }
    let moved_names: HashSet<String> = in_changed_before
        .keys()
        .chain(in_changed_after.keys())
        .filter(|name| in_changed_before.get(*name) != in_changed_after.get(*name))
        .cloned()
        .collect();
    let mut transition_names = HashSet::new();
    if !moved_names.is_empty() {
        let others = view.symbols_named(&moved_names)?;
        for name in &moved_names {
            let untouched = others.get(name).map_or(0, |entries| {
                entries
                    .iter()
                    .filter(|(_, path)| !changed_set.contains(path.as_str()))
                    .count()
            });
            let before = untouched + in_changed_before.get(name).copied().unwrap_or(0);
            let after = untouched + in_changed_after.get(name).copied().unwrap_or(0);
            if before != after && !(before >= 2 && after >= 2) {
                transition_names.insert(name.clone());
            }
        }
    }
    let created: HashSet<String> = target_events
        .iter()
        .filter(|(path, event)| **event == EventKind::Create && facts_by_path.contains_key(*path))
        .map(|(path, _)| path.clone())
        .collect();
    let mut late_dependents = Vec::new();
    for dependent in view.reference_dependents(&transition_names, &created, &after_paths)? {
        if target_events.contains_key(&dependent)
            || !before_paths.contains(&dependent)
            || !after_paths.contains(&dependent)
        {
            continue;
        }
        affected_rows = affected_rows.saturating_add(view.rows_for_path(&dependent)?);
        late_dependents.push(dependent);
    }
    if !crate::memory::partial_refresh_fits(affected_rows) {
        drop(view);
        return full_build_or_defer(
            request,
            context,
            root,
            db_path,
            &stored,
            stored_rows,
            "dependents too large for a partial refresh",
            json!({"affectedFiles": ordered.len() + late_dependents.len(), "affectedRows": affected_rows, "totalFiles": after_paths.len()}),
        );
    }
    for dependent in late_dependents {
        context.check()?;
        target_events.insert(dependent.clone(), EventKind::Repair);
        let facts = view.facts(&dependent)?.ok_or_else(|| {
            unsupported(&format!("stored source facts unavailable for {dependent}"))
        })?;
        facts_by_path.insert(dependent.clone(), facts);
        ordered.push(dependent);
    }
    ordered.sort();
    log.mark("dependents", ordered.len() as u64);

    // Resolution index: only the symbols the rebuilt files call by name, the rebuilt files' own
    // symbols, and file identity. Stand-ins carry id, kind, name and path; nothing else reads them.
    let mut call_names = HashSet::new();
    for facts in facts_by_path.values() {
        for edge in &facts.edges {
            if edge.kind != "CALLS" {
                continue;
            }
            if let Some(name) = edge
                .evidence
                .first()
                .and_then(|evidence| evidence.get("callName"))
                .and_then(Value::as_str)
            {
                call_names.insert(name.to_owned());
            }
        }
    }
    let mut stand_ins: Vec<crate::model::GraphNode> = Vec::new();
    for (name, entries) in view.symbols_named(&call_names)? {
        for (id, path) in entries {
            if !target_events.contains_key(&path) {
                stand_ins.push(identity_symbol_stub(id, name.clone(), path));
            }
        }
    }
    for facts in facts_by_path.values() {
        for node in &facts.nodes {
            if let (true, Some(name), Some(path)) = (
                node.kind == "symbol",
                node.name.as_ref(),
                node.path.as_ref(),
            ) {
                stand_ins.push(identity_symbol_stub(
                    node.id.clone(),
                    name.clone(),
                    path.clone(),
                ));
            }
        }
    }
    for path in &after_paths {
        stand_ins.push(identity_file_stub(path));
    }
    // Stored facts of an unchanged referrer still name nodes the delta deletes, renames or
    // replaces away. A source rebuild would leave those edges unresolved, so reset them here;
    // otherwise the rewrite re-inserts a resolved edge to a missing node, whatever the write order.
    let mut vanished: HashSet<String> = HashSet::new();
    for path in &changed_paths {
        let kept: HashSet<&str> = facts_by_path
            .get(path)
            .map(|facts| {
                std::iter::once(facts.file.id.as_str())
                    .chain(facts.nodes.iter().map(|node| node.id.as_str()))
                    .collect()
            })
            .unwrap_or_default();
        for id in view.node_ids(path)? {
            if !kept.contains(id.as_str()) {
                vanished.insert(id);
            }
        }
    }
    if !vanished.is_empty() {
        for (path, facts) in facts_by_path.iter_mut() {
            if changed_set.contains(path.as_str()) {
                continue;
            }
            for edge in facts.edges.iter_mut() {
                if edge.kind != "CALLS"
                    && edge
                        .target
                        .as_deref()
                        .is_some_and(|target| vanished.contains(target))
                {
                    graph::unresolve_edge(edge);
                }
            }
        }
    }
    let resolution_index = graph::symbol_index(stand_ins.iter());
    for facts in facts_by_path.values_mut() {
        graph::resolve_file_facts_edges(facts, &resolution_index);
    }
    log.mark("resolve", facts_by_path.len() as u64);
    drop(view);

    let mut deltas = Vec::with_capacity(ordered.len() + noop_paths.len());
    for target in &ordered {
        context.check()?;
        let target_file = source_files_by_path.get(target.as_str()).copied();
        let provider_batches = if target_events[target] == EventKind::Delete
            || !changed_set.contains(target.as_str())
        {
            Vec::new()
        } else {
            // Providers read the file itself; a digest-only scan record carries no bytes.
            let hydrated;
            let provider_file: &graph::FileRecord = match observed.get(target) {
                Some(file) => file,
                None => {
                    hydrated = stable_explicit_file(root, target)?.ok_or_else(|| {
                        unsupported(&format!("source facts unavailable for {target}"))
                    })?;
                    &hydrated
                }
            };
            provider_batches_for_path(root, provider_file, &provider_file_map)?
        };
        deltas.push(file_delta_from_graph(
            target,
            target_events[target],
            facts_by_path.remove(target),
            target_file,
            provider_batches,
            source_clock,
            &source_hash,
            config_digest.clone(),
            observation.clone(),
            request,
        )?);
    }
    for path in &noop_paths {
        deltas.push(file_delta_from_graph(
            path,
            event_kind,
            None,
            observed.get(path),
            Vec::new(),
            source_clock,
            &source_hash,
            config_digest.clone(),
            observation.clone(),
            request,
        )?);
    }
    // Deletes first, then upserts, so the outcome does not depend on path order.
    deltas.sort_by_key(|delta| !matches!(delta.event_kind, EventKind::Delete));
    log.mark("deltas", deltas.len() as u64);
    context.check()?;
    let mut connection = open_store(db_path)?;
    delta_store::apply_file_deltas(
        &mut connection,
        &deltas,
        ApplyOptions {
            cancellation: Some(context.cancellation.clone()),
            ..ApplyOptions::default()
        },
    )
    .map_err(|error| delta_error(context, error))?;
    log.mark("apply", 0);
    if event_kind == EventKind::Rename {
        let old = &paths[0];
        if after_paths.contains(old) || !before_paths.contains(old) {
            return Err(unsupported("rename source/destination is inconsistent"));
        }
    }
    let status = post_apply_status(db_path)?;
    log.mark("status", 0);
    bounded_generation_response(
        request,
        json!({
            "schemaVersion": 1, "operation": request.method.as_str(), "state": "fresh", "refreshMode": "incremental",
            "generationId": status.generation_id, "repoRoot": root.to_string_lossy(), "storePath": db_path.to_string_lossy(),
            "sourceHash": status.source_hash, "complete": status.complete, "truncationReasons": status.truncation_reasons,
            "counts": {"nodes": status.nodes, "edges": status.edges, "files": status.files},
            "sourceClock": request.input.get("sourceClock").cloned().unwrap_or(Value::Null), "eventKind": request.input.get("eventKind").cloned().unwrap_or(Value::Null),
            "invalidatedPaths": ordered, "reusedFiles": source_files.len().saturating_sub(ordered.len()), "sourceObservation": status.source_observation
        }),
    )
}

fn normalize_digest(value: &str) -> String {
    if value.starts_with("xxh128:") {
        value.to_owned()
    } else {
        format!("xxh128:{value}")
    }
}

fn stable_explicit_file(
    root: &Path,
    path: &str,
) -> Result<Option<graph::FileRecord>, BlueprintError> {
    let absolute = root.join(path);
    let read = match stable_read_with_limit(&absolute, MAX_SOURCE_FILE_BYTES) {
        Ok(read) => read,
        Err(StableReadError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None)
        }
        Err(error) => {
            return Err(BlueprintError::new(
                "blueprint_incremental_read_failed",
                format!("{path}: {error}"),
            ))
        }
    };
    if read.unstable {
        return Err(BlueprintError::new(
            "blueprint_incremental_unstable",
            format!("source changed during stable read: {path}"),
        ));
    }
    let text = String::from_utf8_lossy(&read.bytes).into_owned();
    Ok(Some(graph::FileRecord {
        path: path.to_owned(),
        absolute_path: absolute,
        size: read.bytes.len() as u64,
        bytes: read.bytes,
        content_hash: read.content_digest.clone(),
        semantic_content_hash: read.content_digest,
        text: Some(text),
    }))
}

fn provider_batches_for_path(
    root: &Path,
    file: &graph::FileRecord,
    file_map: &std::collections::BTreeMap<String, &graph::FileRecord>,
) -> Result<Vec<FactBatch>, BlueprintError> {
    let path = file.path.as_str();
    let one_file = [file.clone()];
    let context = crate::providers::ProviderContext {
        repo_root: root,
        files: &one_file,
        file_map,
    };
    let mut batches = Vec::new();
    for descriptor in crate::providers::registry() {
        let output = (descriptor.run)(&context);
        let nodes = output
            .nodes
            .into_iter()
            .filter(|node| node.path.as_deref() == Some(path))
            .collect::<Vec<_>>();
        let edges = output
            .edges
            .into_iter()
            .filter(|edge| {
                edge.evidence
                    .iter()
                    .any(|evidence| evidence.get("path").and_then(Value::as_str) == Some(path))
            })
            .collect::<Vec<_>>();
        if nodes.is_empty() && edges.is_empty() {
            continue;
        }
        let provider_version = nodes
            .iter()
            .flat_map(|node| node.evidence.iter())
            .chain(edges.iter().flat_map(|edge| edge.evidence.iter()))
            .find_map(|evidence| evidence.get("providerVersion").and_then(Value::as_str))
            .unwrap_or(graph::PROVIDER_VERSION);
        batches.push(FactBatch {
            provider_id: descriptor.id.to_owned(),
            provider_version: provider_version.to_owned(),
            nodes: nodes
                .into_iter()
                .filter_map(|node| serde_json::to_value(node).ok())
                .collect(),
            edges: edges
                .into_iter()
                .filter_map(|edge| serde_json::to_value(edge).ok())
                .collect(),
            dependencies: Vec::new(),
        });
    }
    Ok(batches)
}

#[allow(clippy::too_many_arguments)] // mirrors the delta record fields; a wrapper struct would not simplify callers
fn file_delta_from_graph(
    path: &str,
    event_kind: EventKind,
    facts: Option<graph::FileFacts>,
    file: Option<&graph::FileRecord>,
    provider_batches: Vec<FactBatch>,
    source_clock: i64,
    source_hash: &str,
    config_digest: Option<String>,
    observation: Option<crate::git_source_observation::GitSourceObservation>,
    request: &BlueprintRequest,
) -> Result<FileDelta, BlueprintError> {
    let mut delta = FileDelta {
        path: path.to_owned(),
        event_kind,
        source_clock: Some(source_clock),
        content_digest: file.map(|file| file.content_hash.clone()),
        size: file.map(|file| file.size as i64),
        source_hash: Some(source_hash.to_owned()),
        config_digest,
        source_observation: Some(
            json!({"sourceClock": request.input.get("sourceClock"), "eventKind": request.input.get("eventKind"), "paths": request.input.get("paths"), "head": observation.as_ref().map(|value| value.head.clone()), "dirty": observation.as_ref().map(|value| value.dirty), "statusDigest": observation.as_ref().map(|value| value.status_digest.clone()), "sealedAtUnixMs": now_unix_ms()}),
        ),
        ..FileDelta::default()
    };
    if let Some(facts) = facts {
        let mut nodes = Vec::with_capacity(facts.nodes.len() + 1);
        nodes.push(
            serde_json::to_value(facts.file).map_err(|error| {
                BlueprintError::new("blueprint_delta_failed", error.to_string())
            })?,
        );
        nodes.extend(
            facts
                .nodes
                .into_iter()
                .filter_map(|node| serde_json::to_value(node).ok()),
        );
        let dependencies = facts
            .edges
            .iter()
            .filter_map(|edge| {
                let target = edge.target.as_deref()?.strip_prefix("file:")?;
                let reason = match edge.kind.as_str() {
                    "IMPORTS" | "IMPORT" => "import",
                    "CALLS" | "CALL" => "call",
                    "ROUTES" | "ROUTE" => "route",
                    "SCHEMA" | "SCHEMAS" => "schema",
                    "CONFIG" | "CONFIGURES" => "config",
                    "MANIFEST" | "MANIFESTS" => "manifest",
                    _ => "schema",
                };
                Some((target.to_owned(), path.to_owned(), reason.to_owned()))
            })
            .collect();
        delta.content_digest = Some(facts.content_digest);
        delta.file_report =
            Some(serde_json::to_value(facts.report).map_err(|error| {
                BlueprintError::new("blueprint_delta_failed", error.to_string())
            })?);
        // This batch is the merged replacement for one source path. An empty
        // provider id deliberately selects all prior owners, including
        // lexical/tree-sitter rows created by complete construction, so a
        // repair cannot leave stale facts from another provider behind.
        delta.fact_batches.push(FactBatch {
            provider_id: String::new(),
            provider_version: graph::PROVIDER_VERSION.into(),
            nodes,
            edges: facts
                .edges
                .into_iter()
                .filter_map(|edge| serde_json::to_value(edge).ok())
                .collect(),
            dependencies,
        });
        delta.fact_batches.extend(provider_batches);
    }
    Ok(delta)
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum VerifiedConstructionReason {
    GraphMissing,
    UnrecoverableCorruption,
    /// The store was written by a newer schema than this build supports. It is
    /// set aside (never deleted) and rebuilt fresh (#482).
    NewerSchema,
    /// The persisted graph was built by an older graph schema or a different
    /// provider than this build supports. The graph is a rebuildable index, so
    /// it is set aside (never deleted) and rebuilt fresh (#482).
    IncompatibleGeneration,
    /// A refresh whose delta is too large for a partial refresh rebuilds the graph in place,
    /// within `memory::FULL_GRAPH_BYTE_BUDGET` (#635). The store is replaced in one transaction,
    /// so nothing is set aside.
    IncrementalFallback,
}

impl VerifiedConstructionReason {
    fn code(self) -> &'static str {
        match self {
            Self::GraphMissing => "graph_missing",
            Self::UnrecoverableCorruption => "unrecoverable_corruption",
            Self::NewerSchema => "newer_schema",
            Self::IncompatibleGeneration => "incompatible_generation",
            Self::IncrementalFallback => "incremental_fallback",
        }
    }
}

/// Owner-only construction authorization. Caller-provided reason/flags are
/// ignored; filesystem, SQLite & generation evidence decide eligibility.
pub(crate) fn verified_construction_reason(
    db_path: &Path,
) -> Result<Option<VerifiedConstructionReason>, BlueprintError> {
    if !db_path.is_file() {
        return Ok(Some(VerifiedConstructionReason::GraphMissing));
    }
    match store::open_store_read_only(db_path).and_then(|connection| {
        let version = store::current_schema_version(&connection)?;
        if version > crate::migrations::SCHEMA_VERSION {
            return Err(StoreError::Migration(
                crate::migrations::MigrationError::UnsupportedVersion(version as i64),
            ));
        }
        drop(connection);
        let connection = store::open_store_read_only(db_path)?;
        // Schema and provider identity live in the sealed envelope; reading the
        // whole graph here held a full second copy per root (#635).
        store::read_generation_envelope(&connection)
    }) {
        Ok(Some(generation)) => {
            // Missing or unparseable schema/provider metadata is never defaulted
            // to the current values: it is incompatible and rebuilt (#482).
            let graph_schema = generation.schema_version;
            let provider_meta = generation.provider.as_ref().and_then(|value| {
                Some((value.get("id")?.as_str()?, value.get("version")?.as_str()?))
            });
            if let Some(persisted) = graph_schema.filter(|v| *v > graph::GRAPH_SCHEMA_VERSION) {
                stderr_line!(
                    "blueprint store.newer_schema: persisted graph schema {persisted} is newer than supported {}; setting the store aside and rebuilding",
                    graph::GRAPH_SCHEMA_VERSION
                );
                return Ok(Some(VerifiedConstructionReason::NewerSchema));
            }
            let compatible = matches!(
                (graph_schema, provider_meta),
                (Some(schema), Some((provider, provider_version)))
                    if schema == graph::GRAPH_SCHEMA_VERSION
                        && provider == "native-rust"
                        && provider_version == graph::PROVIDER_VERSION
            );
            if !compatible {
                stderr_line!(
                    "blueprint store.incompatible_generation: persisted graph schema/provider {graph_schema:?}/{provider_meta:?} is missing, unreadable or differs from supported {}/native-rust@{}; setting the store aside and rebuilding",
                    graph::GRAPH_SCHEMA_VERSION, graph::PROVIDER_VERSION
                );
                return Ok(Some(VerifiedConstructionReason::IncompatibleGeneration));
            }
            Ok(None)
        }
        Ok(None) => Ok(Some(VerifiedConstructionReason::UnrecoverableCorruption)),
        Err(StoreError::Migration(crate::migrations::MigrationError::UnsupportedVersion(
            version,
        ))) if version > crate::migrations::SCHEMA_VERSION as i64 => {
            stderr_line!(
                "blueprint store.newer_schema: persisted schema version {version} is newer than supported {}; setting the store aside and rebuilding",
                crate::migrations::SCHEMA_VERSION
            );
            Ok(Some(VerifiedConstructionReason::NewerSchema))
        }
        Err(StoreError::Sqlite(error)) if error.to_string().contains("file is not a database") => {
            Ok(Some(VerifiedConstructionReason::UnrecoverableCorruption))
        }
        Err(error) => Err(store_error(error)),
    }
}

/// A Build/Refresh with no explicit change set whose sealed source observation
/// equals the live HEAD and worktree fingerprint has nothing to apply: answer
/// from the envelope and row counts without materialising the graph (#635).
fn unchanged_generation_response(
    request: &BlueprintRequest,
    root: &Path,
    db_path: &Path,
) -> Result<Option<Value>, BlueprintError> {
    let explicit_paths = request
        .input
        .get("paths")
        .and_then(Value::as_array)
        .is_some_and(|paths| paths.iter().any(|path| path.as_str() != Some(".git/HEAD")));
    if explicit_paths {
        return Ok(None);
    }
    let Some(live) = crate::git_source_observation::git_source_observation(&root.to_string_lossy())
    else {
        return Ok(None);
    };
    let status = post_apply_status(db_path)?;
    let sealed_matches = status.complete
        && status
            .source_observation
            .get("head")
            .and_then(Value::as_str)
            == Some(live.head.as_str())
        && status
            .source_observation
            .get("dirty")
            .and_then(Value::as_bool)
            == Some(live.dirty)
        && status
            .source_observation
            .get("statusDigest")
            .and_then(Value::as_str)
            == Some(live.status_digest.as_str());
    if !sealed_matches {
        return Ok(None);
    }
    let connection = store::open_store_read_only(db_path).map_err(store_error)?;
    let (nodes, edges) = store::stored_graph_rows(&connection).map_err(store_error)?;
    drop(connection);
    bounded_generation_response(
        request,
        json!({
            "schemaVersion":1,"operation":request.method.as_str(),"state":"fresh","refreshMode":"incremental_noop",
            "generationId":status.generation_id,"repoRoot":root.to_string_lossy(),"storePath":db_path.to_string_lossy(),
            "sourceHash":status.source_hash,"complete":status.complete,"truncationReasons":status.truncation_reasons,
            "counts":{"nodes":nodes,"edges":edges,"files":status.files},
            "sourceObservation":status.source_observation
        }),
    )
    .map(Some)
}

/// A missing, incompatible or corrupt store is rebuilt in process. When the old store is still
/// countable and its rows exceed the full-build budget, the rebuild is deferred with the typed
/// error and the store left in place. A missing store has no row count and builds under
/// `full_graph_guard`.
fn defer_oversized_rebuild(db_path: &Path) -> Result<(), BlueprintError> {
    if !db_path.is_file() {
        return Ok(());
    }
    let Ok(connection) = store::open_store_read_only(db_path) else {
        return Ok(());
    };
    let Ok((nodes, edges)) = store::stored_graph_rows(&connection) else {
        return Ok(());
    };
    let rows = nodes.saturating_add(edges);
    if crate::memory::full_graph_fits(rows) {
        return Ok(());
    }
    let mut error = BlueprintError::new(
        "blueprint_incremental_deferred_memory_budget",
        format!(
            "rebuild of an unusable store of {rows} rows needs about {} MiB, above the {} MiB budget; deferred, store kept",
            crate::memory::estimated_graph_bytes(rows) / (1024 * 1024),
            crate::memory::FULL_GRAPH_BYTE_BUDGET / (1024 * 1024)
        ),
    );
    error.details = Some(json!({
        "preservedGeneration": true, "trigger": "store unusable", "storedRows": rows,
        "estimatedBytes": crate::memory::estimated_graph_bytes(rows),
        "byteBudget": crate::memory::FULL_GRAPH_BYTE_BUDGET,
    }));
    Err(error)
}

fn build_and_publish(
    request: &BlueprintRequest,
    context: &RequestContext,
    root: &Path,
    db_path: &Path,
    verified_reason: VerifiedConstructionReason,
) -> Result<Value, BlueprintError> {
    context.check()?;
    // One whole-graph materialisation at a time (#635). Taken after any refresh guard, never
    // before one.
    let _full_graph = crate::memory::full_graph_guard();
    record_construction_event(
        root,
        request,
        verified_reason,
        "started",
        None,
        db_path,
        None,
    )?;
    // Keep graph construction synchronous so no detached worker can outlive
    // this call; its checkpoints observe request cancellation & deadline.
    let graph = match graph::build_generation_with_cancellation(
        root,
        &GraphOptions::default(),
        &context.cancellation,
    ) {
        Ok(graph) => graph,
        Err(error) => {
            let mapped = match error {
                graph::GraphError::Cancelled if context.cancellation.deadline_expired() => {
                    BlueprintError::deadline()
                }
                graph::GraphError::Cancelled => BlueprintError::cancelled(),
                error => BlueprintError::new("blueprint_build_failed", error.to_string()),
            };
            let _ = record_construction_event(
                root,
                request,
                verified_reason,
                "failed",
                None,
                db_path,
                None,
            );
            return Err(mapped);
        }
    };
    context.check()?;
    let generation_id = graph.generation_id.clone();
    let quarantined_to = quarantine_unusable_store(db_path, verified_reason)?;
    let mut connection = open_store(db_path)?;
    context.check()?;
    // Gap 3 (lane STORE2): persist head/dirty with the generation at build
    // time, matching legacy `identityFromStore`'s `envelope.sourceObservation`
    // (`{ head, dirty }`), rather than leaving snapshot/changes callers to
    // recompute git state live at query time.
    //
    // Lane WIRE2 (NCL-02, git-source-observation.mjs row): this used to call
    // `lib_application_snapshots::current_git_identity`, a second,
    // independently drifted git observer (different porcelain exclusion set,
    // no statusDigest) -- exactly the split the legacy module's header
    // comment warns against. Build time and freshness-receipt time must run
    // the identical observer, so this now calls the canonical
    // `git_source_observation` port directly and also persists its
    // `statusDigest`, the bounded worktree fingerprint freshness comparisons
    // are built on.
    let git_identity =
        crate::git_source_observation::git_source_observation(&root.to_string_lossy());
    // Nothing between the graph build and the store write observed
    // cancellation; a shutdown firing during git observation or the SQLite
    // write otherwise stays invisible until the next checkpoint.
    context.check()?;
    let observation = json!({
        "sourceClock": request.input.get("sourceClock").cloned().unwrap_or(Value::Null),
        "eventKind": request.input.get("eventKind").cloned().unwrap_or(Value::Null),
        "paths": request.input.get("paths").cloned().unwrap_or_else(|| json!([])),
        "head": git_identity.as_ref().map(|observation| observation.head.clone()),
        "dirty": git_identity.as_ref().map(|observation| observation.dirty),
        "statusDigest": git_identity.as_ref().map(|observation| observation.status_digest.clone()),
        "sealedAtUnixMs": now_unix_ms(),
    });
    store::save_generation(
        &mut connection,
        &to_store_generation(&graph, observation.clone()),
    )
    .map_err(store_error)?;
    // Gap 2 (lane STORE2): populate `generation_leaf` from the just-written
    // `files` table so snapshot/changes leaf identity comes from the same
    // table legacy's `identityFromStore` reads (`generation_leaf WHERE
    // kind='file'`), instead of native callers reading `files` directly.
    {
        let mut statement = connection
            .prepare(
                "SELECT path, content_hash FROM files WHERE content_hash IS NOT NULL ORDER BY path",
            )
            .map_err(|error| BlueprintError::new("blueprint_build_failed", error.to_string()))?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| BlueprintError::new("blueprint_build_failed", error.to_string()))?;
        let mut leaf_files = Vec::new();
        for row in rows {
            let (path, content_hash) = row.map_err(|error| {
                BlueprintError::new("blueprint_build_failed", error.to_string())
            })?;
            leaf_files.push(crate::merkle_ledger::LedgerFile {
                path,
                content_digest: content_hash,
            });
        }
        drop(statement);
        crate::merkle_ledger::compute_full_ledger(&connection, &leaf_files)
            .map_err(|error| BlueprintError::new("blueprint_build_failed", error.to_string()))?;
    }
    // The legacy build publishes generated human docs after its graph/store
    // commit. Native builds have the same side effect through the Rust port;
    // `lib_generated_docs` reads the just-published generation directly when
    // retired JSON projection files are absent. A docs conflict is a typed
    // successful result with fallback output, matching the legacy contract.
    drop(connection);
    record_construction_event(
        root,
        request,
        verified_reason,
        "succeeded",
        Some(&generation_id),
        db_path,
        quarantined_to.as_deref(),
    )?;
    context.check()?;
    // Generated human docs (`docs/` + README pointer) mutate the user's working
    // tree, so they are opt-in (`generateDocs: true`); a default build only
    // writes Blueprint's own store under `.agent/`.
    let generate_docs_requested = request
        .input
        .get("generateDocs")
        .or_else(|| request.input.get("generate-docs"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let docs_value = if generate_docs_requested {
        let docs_options = crate::lib_generated_docs::GenerateDocsOptions {
            no_readme_link: request
                .input
                .get("noReadmeLink")
                .or_else(|| request.input.get("no-readme-link"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
        };
        // Docs generation is a bounded but uncancellable phase; at least refuse
        // to enter it once shutdown has been requested.
        context.check()?;
        let docs_result = crate::lib_generated_docs::generate_docs(root, docs_options)
            .map_err(|error| BlueprintError::new("blueprint_docs_failed", error.to_string()))?;
        json!({
            "mode": docs_result.mode,
            "conflicts": docs_result.conflicts,
            "wrote": docs_result.wrote,
            "fallback": docs_result.fallback,
            "readme": docs_result.readme.map(|mode| json!({"mode": match mode {
                crate::lib_generated_docs::ReadmePointerMode::Absent => "absent",
                crate::lib_generated_docs::ReadmePointerMode::Created => "created",
                crate::lib_generated_docs::ReadmePointerMode::Updated => "updated",
            }})),
            "retired": docs_result.retired,
        })
    } else {
        json!({"mode": "skipped", "conflicts": [], "wrote": [], "fallback": [], "readme": null, "retired": []})
    };
    context.check()?;
    bounded_generation_response(
        request,
        json!({
            "schemaVersion": 1,
            "operation": request.method.as_str(),
            "state": "fresh",
            "generationId": generation_id,
            "repoRoot": root.to_string_lossy(),
            "storePath": db_path.to_string_lossy(),
            "sourceHash": graph.source_hash,
            "complete": graph.complete,
            "truncationReasons": graph.truncation_reasons,
            "counts": {"nodes": graph.nodes.len(), "edges": graph.edges.len(), "files": graph.files.len()},
            "sourceClock": request.input.get("sourceClock").cloned().unwrap_or(Value::Null),
            "eventKind": request.input.get("eventKind").cloned().unwrap_or(Value::Null),
            "sourceObservation": observation,
            "docsResult": docs_value,
        }),
    )
}

/// Sets an unusable store aside as `<db>.corrupt-<unix-ms>` (unreadable store) or
/// `<db>.replaced-<unix-ms>` (newer or incompatible schema, which is never labelled corrupt,
/// MDP-011/MDP-047) (main file and sidecars; nothing is deleted) and returns where it went. The caller's request
/// id and the verified reason are recorded in `full-constructions.jsonl`.
fn quarantine_unusable_store(
    db_path: &Path,
    reason: VerifiedConstructionReason,
) -> Result<Option<PathBuf>, BlueprintError> {
    if !matches!(
        reason,
        VerifiedConstructionReason::UnrecoverableCorruption
            | VerifiedConstructionReason::NewerSchema
            | VerifiedConstructionReason::IncompatibleGeneration
    ) || !db_path.exists()
    {
        return Ok(None);
    }
    let options = match reason {
        VerifiedConstructionReason::UnrecoverableCorruption => {
            rightkit_db::QuarantineOptions::default()
        }
        _ => rightkit_db::QuarantineOptions {
            name: rightkit_db::QuarantineStamp::Custom(|at| {
                let millis = at
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|elapsed| elapsed.as_millis())
                    .unwrap_or_default();
                format!(".replaced-{millis}")
            }),
            ..rightkit_db::QuarantineOptions::default()
        },
    };
    rightkit_db::quarantine_database_opts(db_path, &options)
        .map(Some)
        .map_err(|error| {
            BlueprintError::new("blueprint_store_quarantine_failed", error.to_string())
        })
}

fn record_construction_event(
    root: &Path,
    request: &BlueprintRequest,
    reason: VerifiedConstructionReason,
    phase: &str,
    generation: Option<&str>,
    db_path: &Path,
    quarantined_to: Option<&Path>,
) -> Result<(), BlueprintError> {
    let path = crate::graph_dir::graph_dir(root).join("full-constructions.jsonl");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            BlueprintError::new("blueprint_construction_receipt_failed", error.to_string())
        })?;
    }
    let evidence = match reason {
        VerifiedConstructionReason::GraphMissing => {
            json!({"storePath":db_path,"storeExisted":false})
        }
        VerifiedConstructionReason::UnrecoverableCorruption => {
            json!({"storePath":db_path,"storeExisted":true,"generationReadable":false,"quarantinedTo":quarantined_to})
        }
        VerifiedConstructionReason::IncompatibleGeneration => {
            json!({"storePath":db_path,"storeExisted":true,"incompatibleGeneration":true,"supportedGraphSchema":graph::GRAPH_SCHEMA_VERSION,"quarantinedTo":quarantined_to})
        }
        VerifiedConstructionReason::NewerSchema => {
            json!({"storePath":db_path,"storeExisted":true,"newerSchema":true,"supportedSchema":crate::migrations::SCHEMA_VERSION,"quarantinedTo":quarantined_to})
        }
        VerifiedConstructionReason::IncrementalFallback => {
            json!({"storePath":db_path,"storeExisted":true,"deltaTooLargeForPartialRefresh":true})
        }
    };
    let record = json!({"schemaVersion":1,"phase":phase,"caller":{"requestId":request.request_id,"operation":request.method.as_str()},"verifiedReason":reason.code(),"evidence":evidence,"resultingGeneration":generation});
    let mut line = record.to_string().into_bytes();
    line.push(b'\n');
    rightkit_fs::append_record(
        &path,
        &line,
        rightkit_fs::AppendOptions::default().durable(false),
    )
    .map(|_| ())
    .map_err(|error| {
        BlueprintError::new("blueprint_construction_receipt_failed", error.to_string())
    })
}

// Detailed omissions remain in the durable generation. Project only as many
// as fit on wire, accounting for the actual response envelope & UTF-8 bytes.
fn bounded_generation_response(
    request: &BlueprintRequest,
    mut value: Value,
) -> Result<Value, BlueprintError> {
    let count = value["truncationReasons"].as_array().map_or(0, Vec::len);
    value["truncationReasonCount"] = json!(count);
    value["truncationReasonsOmitted"] = json!(0);
    // Receipt listings are not data channels. A large enrolled repository can invalidate thousands
    // of paths in one refresh, and the freshness receipt of `status` carries every changed path
    // (`freshnessReceipt.staleSources.paths`, #601). Without projection the listing alone overflows
    // the response bound and the answer is refused. Record authoritative counts up front so each
    // listing can shrink without losing the truth.
    const RECEIPT_LISTS: [&str; 4] = [
        "invalidatedPaths",
        "changedPaths",
        "pendingChanges",
        "freshnessReceipt.staleSources.paths",
    ];
    for field in RECEIPT_LISTS {
        if let Some(length) = receipt_list(&value, field).map(Vec::len) {
            set_receipt_counter(&mut value, field, "Count", json!(length));
            set_receipt_counter(&mut value, field, "Omitted", json!(0));
        }
    }
    // `omissions` is keyed by field: each shrink rewrites that field's single
    // entry with its cumulative count. Appending per iteration left several
    // cumulative entries for a re-shrunk field (double-counting on sum), and
    // the truncationReasons branch previously overwrote the array entirely,
    // dropping receipt-field omissions recorded in earlier iterations.
    fn record_omission(value: &mut Value, field: &str, omitted: u64) {
        let mut omissions = value
            .get("omissions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        omissions.retain(|entry| entry.get("field").and_then(Value::as_str) != Some(field));
        if omitted > 0 {
            omissions
                .push(json!({"reason": "response_projection", "field": field, "count": omitted}));
        }
        value["omissions"] = json!(omissions);
    }
    loop {
        let response = crate::api::BlueprintResponse::success(
            request.request_id.clone(),
            request.generation.clone(),
            value.clone(),
        );
        match response.validate(crate::api::Bounds::default()) {
            Ok(()) => return Ok(value),
            Err(error) => {
                if let Some(reasons) = value["truncationReasons"].as_array_mut() {
                    if reasons.pop().is_some() {
                        let omitted = count - reasons.len();
                        value["truncationReasonsOmitted"] = json!(omitted);
                        record_omission(&mut value, "truncationReasons", omitted as u64);
                        continue;
                    }
                }
                // Shrink the largest receipt listing geometrically so the
                // loop re-validates O(log n) times rather than once per path.
                let largest = RECEIPT_LISTS
                    .iter()
                    .filter_map(|field| {
                        receipt_list(&value, field).map(|list| (*field, list.len()))
                    })
                    .filter(|(_, length)| *length > 0)
                    .max_by_key(|(_, length)| *length)
                    .map(|(field, _)| field);
                let Some(field) = largest else {
                    return Err(error);
                };
                let kept = {
                    let list = receipt_list_mut(&mut value, field)
                        .expect("largest receipt field is an array");
                    list.truncate(list.len() / 2);
                    list.len()
                };
                let total = receipt_counter(&value, field, "Count")
                    .and_then(Value::as_u64)
                    .unwrap_or(kept as u64);
                let omitted = total.saturating_sub(kept as u64);
                set_receipt_counter(&mut value, field, "Omitted", json!(omitted));
                record_omission(&mut value, field, omitted);
            }
        }
    }
}

/// A receipt listing at a dotted path below the response root, such as
/// `freshnessReceipt.staleSources.paths`.
fn receipt_list<'a>(value: &'a Value, field: &str) -> Option<&'a Vec<Value>> {
    field
        .split('.')
        .try_fold(value, |node, part| node.get(part))
        .and_then(Value::as_array)
}

fn receipt_list_mut<'a>(value: &'a mut Value, field: &str) -> Option<&'a mut Vec<Value>> {
    field
        .split('.')
        .try_fold(value, |node, part| node.get_mut(part))
        .and_then(Value::as_array_mut)
}

/// Counter sitting beside a receipt listing: `<leaf><suffix>` in the listing's parent object,
/// so `invalidatedPaths` keeps `invalidatedPathsCount` at the root and `paths` gets `pathsCount`
/// beside it in `staleSources`.
fn receipt_counter<'a>(value: &'a Value, field: &str, suffix: &str) -> Option<&'a Value> {
    let (parent, leaf) = field.rsplit_once('.').unwrap_or(("", field));
    let parent_value = if parent.is_empty() {
        value
    } else {
        parent
            .split('.')
            .try_fold(value, |node, part| node.get(part))?
    };
    parent_value.get(format!("{leaf}{suffix}"))
}

fn set_receipt_counter(value: &mut Value, field: &str, suffix: &str, counter: Value) {
    let (parent, leaf) = field.rsplit_once('.').unwrap_or(("", field));
    let target = if parent.is_empty() {
        Some(value)
    } else {
        parent
            .split('.')
            .try_fold(value, |node, part| node.get_mut(part))
    };
    if let Some(target) = target {
        target[format!("{leaf}{suffix}")] = counter;
    }
}

fn status(
    request: &BlueprintRequest,
    context: &RequestContext,
    root: &Path,
    db_path: &Path,
) -> Result<Value, BlueprintError> {
    context.check()?;
    if !db_path.exists() {
        return Ok(status_value(
            request, root, db_path, "missing", None, None, None, None,
        ));
    }
    // Status is admission metadata, not a graph query. Reading every node,
    // edge, and file row made Pull freshness proportional to repository size.
    let connection = match store::open_store_read_only(db_path).map_err(store_error) {
        Ok(connection) => connection,
        Err(error) if error.code == "blueprint_store_corrupt" => {
            return Ok(status_value(
                request,
                root,
                db_path,
                "corrupt",
                None,
                None,
                Some(error.message),
                None,
            ));
        }
        Err(error) => return Err(error),
    };
    let envelope = match store::read_generation_envelope(&connection).map_err(store_error) {
        Ok(Some(envelope)) => envelope,
        Ok(None) => {
            return Ok(status_value(
                request, root, db_path, "missing", None, None, None, None,
            ))
        }
        Err(error) if error.code == "blueprint_store_corrupt" => {
            return Ok(status_value(
                request,
                root,
                db_path,
                "corrupt",
                None,
                None,
                Some(error.message),
                None,
            ));
        }
        Err(error) => return Err(error),
    };
    let (generation, source_observation) = match graph_metadata_from_envelope(envelope) {
        Ok(value) => value,
        Err(error) if error.code == "blueprint_store_corrupt" => {
            return Ok(status_value(
                request,
                root,
                db_path,
                "corrupt",
                None,
                None,
                Some(error.message),
                None,
            ));
        }
        Err(error) => return Err(error),
    };
    // `liveObservation:false` keeps the read strictly on the sealed
    // generation basis — used by resident callers that hold their own
    // watcher/maintenance observation and must not pay a live fingerprint.
    // Default stays a staged live observation bounded by this request's
    // remaining deadline rather than a mandatory whole-worktree scan.
    let observation = if request
        .input
        .get("liveObservation")
        .and_then(Value::as_bool)
        == Some(false)
    {
        FreshnessObservation::Sealed
    } else {
        FreshnessObservation::LiveBounded(context.deadline.remaining())
    };
    let freshness = generation_freshness_receipt(
        root,
        &generation.generation_id,
        source_observation.as_ref(),
        None,
        observation,
    );
    context.check()?;
    let state = match freshness.get("freshness").and_then(Value::as_str) {
        Some("fresh") => "fresh",
        Some("changed_since_generation") => "stale",
        Some("unknown") => "unknown",
        _ => "unavailable",
    };
    Ok(status_value(
        request,
        root,
        db_path,
        state,
        Some(&generation),
        source_observation.as_ref(),
        None,
        Some(freshness),
    ))
}

fn graph_metadata_from_envelope(
    envelope: store::GenerationEnvelope,
) -> Result<(GraphGeneration, Option<Value>), BlueprintError> {
    let manifest = envelope.manifest.as_ref().ok_or_else(|| {
        BlueprintError::new(
            "blueprint_store_corrupt",
            "persisted generation manifest is missing",
        )
    })?;
    let generation_id = manifest
        .get("generationId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            BlueprintError::new(
                "blueprint_store_corrupt",
                "persisted generation identity is missing",
            )
        })?
        .to_owned();
    let provider = envelope
        .provider
        .as_ref()
        .and_then(|value| value.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("native-rust")
        .to_owned();
    let provider_version = envelope
        .provider
        .as_ref()
        .and_then(|value| value.get("version"))
        .and_then(Value::as_str)
        .unwrap_or("native-rust-1")
        .to_owned();
    let source_observation = envelope.source_observation.clone();
    Ok((
        GraphGeneration {
            schema_version: envelope
                .schema_version
                .unwrap_or(graph::GRAPH_SCHEMA_VERSION),
            provider,
            provider_version,
            generation_id,
            source_hash: manifest
                .get("sourceHash")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            repo_root: envelope
                .repo_root
                .as_ref()
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            complete: manifest
                .get("complete")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            nodes: Vec::new(),
            edges: Vec::new(),
            files: Vec::new(),
            truncation_reasons: manifest
                .get("truncationReasons")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
        },
        source_observation,
    ))
}

#[allow(clippy::too_many_arguments)] // flat status fields feed one JSON object; a wrapper struct would not simplify callers
fn status_value(
    request: &BlueprintRequest,
    root: &Path,
    db_path: &Path,
    state: &str,
    generation: Option<&GraphGeneration>,
    source_observation: Option<&Value>,
    detail: Option<String>,
    freshness: Option<Value>,
) -> Value {
    json!({
        "schemaVersion": 1,
        "operation": request.method.as_str(),
        "state": state,
        "fresh": state == "fresh",
        "generationId": generation.map(|g| g.generation_id.clone()),
        "sourceHash": generation.map(|g| g.source_hash.clone()),
        "repoRoot": root.to_string_lossy(),
        "storePath": db_path.to_string_lossy(),
        "detail": detail,
        "complete": generation.map(|g| g.complete),
        "truncationReasons": generation.map(|g| g.truncation_reasons.clone()).unwrap_or_default(),
        "sourceObservation": source_observation.cloned(),
        "freshnessReceipt": freshness,
        // What brings the graph current, so a stale answer says how to fix it (#601).
        "refreshCommand": match state {
            "fresh" => Value::Null,
            "missing" | "corrupt" => json!("pulse blueprint build"),
            _ => json!("pulse blueprint refresh"),
        },
        // Last write to the graph store (the database and its WAL), the closest wall-clock
        // evidence of the last build or refresh: the store records no build time of its own.
        // Null when the store is absent or its time unreadable; never guessed (#555).
        "storeModifiedAtMs": store_modified_ms(db_path),
        "storeAgeSecs": store_modified_ms(db_path).map(|modified| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_millis())
                .unwrap_or(0);
            (now.saturating_sub(modified) / 1000) as u64
        }),
    })
}

/// Newest modification time, in milliseconds since the epoch, of the graph store file and its
/// write-ahead log. `None` when neither can be read.
fn store_modified_ms(db_path: &Path) -> Option<u128> {
    let wal = {
        let mut name = db_path.as_os_str().to_owned();
        name.push("-wal");
        PathBuf::from(name)
    };
    [db_path, wal.as_path()]
        .into_iter()
        .filter_map(|path| {
            std::fs::metadata(path)
                .and_then(|meta| meta.modified())
                .ok()
        })
        .filter_map(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_millis())
        .max()
}

fn doctor(request: &BlueprintRequest, root: &Path) -> Result<Value, BlueprintError> {
    let out_dir = request
        .input
        .get("outDir")
        .and_then(Value::as_str)
        .unwrap_or(".agent");
    let diagnostics = crate::lib_operations_doctor::collect_store_diagnostics(&store_path(root));
    Ok(json!({
        "schemaVersion": diagnostics.schema_version,
        "operation": request.method.as_str(),
        "state": diagnostics.state,
        "errors": diagnostics.errors,
        "warnings": diagnostics.warnings,
        "reasons": diagnostics.reasons.iter().map(|reason| json!({"code": reason.code, "severity": reason.severity, "message": reason.message})).collect::<Vec<_>>(),
        "repoRoot": root.to_string_lossy(),
        "outDir": out_dir,
    }))
}

fn repair(request: &BlueprintRequest, root: &Path) -> Result<Value, BlueprintError> {
    let out_dir = request
        .input
        .get("outDir")
        .and_then(Value::as_str)
        .unwrap_or(".agent");
    let diagnostics = crate::lib_operations_doctor::collect_store_diagnostics(&store_path(root));
    let reasons = diagnostics
        .reasons
        .iter()
        .map(|reason| crate::lib_operations_repair::DoctorReason {
            code: reason.code.clone(),
            severity: reason.severity.clone(),
        })
        .collect::<Vec<_>>();
    let plan = crate::lib_operations_repair::build_repair_plan(
        &root.to_string_lossy(),
        out_dir,
        &diagnostics.state,
        &reasons,
    );
    Ok(json!({
        "schemaVersion": plan.schema_version,
        "operation": request.method.as_str(),
        "root": plan.root,
        "outDir": out_dir,
        "graphState": plan.graph_state,
        "actions": plan.actions.iter().map(|action| json!({"id": action.id, "kind": action.kind, "command": action.command, "reversible": action.reversible, "reason": action.reason})).collect::<Vec<_>>(),
    }))
}

fn architecture_changes(
    request: &BlueprintRequest,
    context: &RequestContext,
    root: &Path,
    db_path: &Path,
) -> Result<Value, BlueprintError> {
    context.check()?;
    let connection = store::open_store_read_only(db_path).map_err(store_error)?;
    let (generation, source_observation) = load_current_with_observation(db_path)?;
    ensure_generation(request, &generation)?;
    let limit = request
        .input
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(100);
    let snapshot = request.input.get("snapshot").and_then(Value::as_str);
    let since_generation = request.input.get("sinceGeneration").and_then(Value::as_str);
    let head = request
        .input
        .get("head")
        .and_then(Value::as_str)
        .unwrap_or("HEAD");
    let treeish_pair = match request.input.get("treeish") {
        Some(Value::String(base)) => Some((base.as_str(), head)),
        Some(Value::Object(map)) => {
            let base = map
                .get("base")
                .or_else(|| map.get("from"))
                .and_then(Value::as_str);
            let treeish_head = map
                .get("head")
                .or_else(|| map.get("to"))
                .and_then(Value::as_str)
                .unwrap_or(head);
            base.map(|value| (value, treeish_head))
        }
        _ => None,
    };
    let mut value = crate::lib_application_snapshots::changes_since_reference(
        &connection,
        root,
        snapshot,
        since_generation,
        treeish_pair,
        limit,
    )?;
    if let Value::Object(object) = &mut value {
        object.insert("view".into(), json!("changes"));
        object.insert(
            "generationId".into(),
            json!(generation.generation_id.clone()),
        );
        let indexed_paths: HashSet<String> = generation
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect();
        object.insert(
            "freshnessReceipt".into(),
            generation_freshness_receipt(
                root,
                &generation.generation_id,
                source_observation.as_ref(),
                Some(&indexed_paths),
                FreshnessObservation::LiveBounded(context.deadline.remaining()),
            ),
        );
    }
    context.check()?;
    Ok(value)
}

/// How a freshness read observes current VCS state. `Sealed` reads the
/// persisted generation basis only — no git spawn at all; the verdict
/// resolves `unavailable` and an authoritative caller (e.g. a resident
/// watcher) must overlay its own observation state. `LiveBounded` runs the
/// staged observation under the caller's remaining request budget: a cheap
/// HEAD read first, the worktree fingerprint only under what remains, and
/// changed-path enumeration only inside the same deadline. Incomplete
/// observation produces honest `unavailable`/whole-generation suppression,
/// never a guessed `fresh`.
enum FreshnessObservation {
    Sealed,
    LiveBounded(Duration),
}

/// Build the freshness receipt for a sealed generation. The receipt compares
/// the sealed-at basis against a bounded live VCS observation (`git rev-parse
/// HEAD` plus `git status --porcelain=v1 -z`, each time- and output-bounded),
/// so a repository edit after the last refresh surfaces as
/// `changed_since_generation` instead of an old-fresh result. Reads stay
/// non-mutating: the probe is read-only, and changed-path enumeration runs
/// lazily only when drift is detected. `indexed_paths`, when supplied,
/// confines `staleSources` to paths this generation actually indexed.
fn generation_freshness_receipt(
    root: &Path,
    generation_id: &str,
    source_observation: Option<&Value>,
    indexed_paths: Option<&HashSet<String>>,
    observation: FreshnessObservation,
) -> Value {
    let indexed_revision = source_observation
        .and_then(|value| value.get("head").or_else(|| value.get("baseCommit")))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let indexed_fingerprint = source_observation
        .and_then(|value| {
            value
                .get("statusDigest")
                .or_else(|| value.get("worktreeFingerprint"))
        })
        .and_then(Value::as_str)
        .map(str::to_owned);
    let basis = crate::freshness::GenerationFreshnessBasis {
        indexed_revision,
        indexed_worktree_fingerprint: indexed_fingerprint,
    };
    let observation_started = Instant::now();
    let (current, observation_incomplete) = match observation {
        FreshnessObservation::Sealed => (crate::freshness::CurrentSourceState::default(), false),
        FreshnessObservation::LiveBounded(budget) => {
            let state = crate::freshness_observation::observe_current_vcs_state_bounded(
                root,
                basis.indexed_revision.as_deref(),
                budget,
            );
            // HEAD was observed but the worktree fingerprint never completed:
            // incomplete evidence, distinguishable from "git absent".
            let incomplete = !state.available && state.vcs_revision.is_some();
            (state, incomplete)
        }
    };
    let live_observed = current.available;
    let enumeration_budget = match observation {
        FreshnessObservation::LiveBounded(budget) => {
            budget.saturating_sub(observation_started.elapsed())
        }
        FreshnessObservation::Sealed => Duration::ZERO,
    };
    let changed_basis = basis.clone();
    let changed_current = current.clone();
    let receipt = crate::freshness_receipt::build_freshness_receipt(
        Some(generation_id.to_owned()),
        None,
        basis,
        current,
        move || {
            crate::freshness_observation::changed_paths_for_freshness_bounded(
                root,
                &changed_basis,
                &changed_current,
                enumeration_budget,
            )
        },
        |path| indexed_paths.is_none_or(|paths| paths.contains(path)),
    );
    let mut value = serde_json::to_value(receipt).unwrap_or_else(|_| json!({"schema":"BlueprintFreshnessReceiptV1","generationId":generation_id,"freshness":"unavailable"}));
    if let Value::Object(object) = &mut value {
        let sealed_at = source_observation
            .and_then(|observation| observation.get("sealedAtUnixMs"))
            .and_then(Value::as_u64);
        object.insert(
            "observationMode".into(),
            json!(if live_observed {
                "live_observation"
            } else {
                "sealed_generation"
            }),
        );
        object.insert("liveSourceObserved".into(), json!(live_observed));
        if observation_incomplete {
            object.insert("observationIncomplete".into(), json!(true));
        }
        object.insert(
            "sealedAtUnixMs".into(),
            sealed_at.map_or(Value::Null, Value::from),
        );
        object.insert(
            "ageMs".into(),
            sealed_at
                .map(|sealed| now_unix_ms().saturating_sub(sealed))
                .map_or(Value::Null, Value::from),
        );
    }
    value
}

/// Drive query-time stale-source suppression from a freshness receipt. A
/// `changed_since_generation` verdict marks every changed path stale for the
/// served generation; an incomplete enumeration suppresses the whole
/// generation rather than guessing which rows remain trustworthy. Other
/// verdicts leave the request untouched.
fn apply_freshness_suppression(request: &mut BlueprintRequest, receipt: &Value) {
    if receipt.get("freshness").and_then(Value::as_str) != Some("changed_since_generation") {
        return;
    }
    let stale = receipt.get("staleSources");
    let paths = stale
        .and_then(|value| value.get("paths"))
        .cloned()
        .unwrap_or_else(|| json!([]));
    let whole_generation = stale
        .and_then(|value| value.get("complete"))
        .and_then(Value::as_bool)
        == Some(false);
    if !request.input.is_object() {
        request.input = json!({});
    }
    request.input["staleSourcePaths"] = paths;
    request.input["staleWholeGeneration"] = json!(whole_generation);
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn open_store(path: &Path) -> Result<rightkit_db::rusqlite::Connection, BlueprintError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            BlueprintError::new("blueprint_store_unavailable", error.to_string())
        })?;
    }
    store::open_store(Some(path)).map_err(store_error)
}

/// Post-delta response state without re-materialising graph rows. The delta
/// apply already reseals `generation.manifest` (identity, counts, digests),
/// so the bounded response only needs the envelope plus a files count.
struct PostApplyStatus {
    generation_id: String,
    source_hash: String,
    complete: bool,
    truncation_reasons: Value,
    nodes: u64,
    edges: u64,
    files: u64,
    source_observation: Value,
}

fn delta_error(
    context: &RequestContext,
    error: delta_store::ApplyFileDeltaError,
) -> BlueprintError {
    match error {
        delta_store::ApplyFileDeltaError::Cancelled if context.cancellation.deadline_expired() => {
            BlueprintError::deadline()
        }
        delta_store::ApplyFileDeltaError::Cancelled => BlueprintError::cancelled(),
        error => BlueprintError::new("blueprint_delta_failed", error.to_string()),
    }
}

fn post_apply_status(db_path: &Path) -> Result<PostApplyStatus, BlueprintError> {
    let connection = store::open_store_read_only(db_path).map_err(store_error)?;
    let envelope = store::read_generation_envelope(&connection)
        .map_err(store_error)?
        .ok_or_else(|| {
            BlueprintError::new(
                "blueprint_store_missing",
                "no persisted Blueprint generation exists",
            )
        })?;
    let manifest = envelope.manifest.unwrap_or(Value::Null);
    let files: i64 = connection
        .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
        .map_err(|error| store_error(store::StoreError::Sqlite(error)))?;
    Ok(PostApplyStatus {
        generation_id: manifest
            .get("generationId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        source_hash: manifest
            .get("sourceHash")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        complete: manifest
            .get("complete")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        truncation_reasons: manifest
            .get("truncationReasons")
            .cloned()
            .unwrap_or_else(|| json!([])),
        nodes: manifest
            .pointer("/counts/nodes")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        edges: manifest
            .pointer("/counts/edges")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        files: files as u64,
        source_observation: envelope.source_observation.unwrap_or(Value::Null),
    })
}

/// Most stored rows a read-only query (search, impact, findings, document truth) may load into
/// memory. Queries load once under `full_graph_guard` and the cache keeps one generation, so the
/// ceiling is above `FULL_GRAPH_BYTE_BUDGET` (rightkit, ~1.5M rows, must stay searchable); a
/// refresh and a full build keep the byte budget (#635).
const MAX_QUERY_LOAD_ROWS: u64 = 2_500_000;

fn refuse_oversized_query_load(rows: u64) -> Result<(), BlueprintError> {
    if rows <= MAX_QUERY_LOAD_ROWS {
        return Ok(());
    }
    let mut error = BlueprintError::new(
        "blueprint_generation_too_large",
        format!(
            "stored generation of {rows} rows is above the {MAX_QUERY_LOAD_ROWS}-row query limit; answered without loading it"
        ),
    );
    error.details = Some(json!({"rows": rows, "limitRows": MAX_QUERY_LOAD_ROWS}));
    Err(error)
}

/// The whole stored generation, materialised. Only the read-only query cache and the
/// full-build fallback reach it; a refresh reads through `refresh_view` instead (#635).
fn load_current_with_observation(
    path: &Path,
) -> Result<(GraphGeneration, Option<Value>), BlueprintError> {
    if !path.exists() {
        return Err(BlueprintError::new(
            "blueprint_store_missing",
            "no persisted Blueprint generation exists",
        ));
    }
    let connection = store::open_store_read_only(path).map_err(store_error)?;
    // A whole-generation load costs the same bytes per row as a full build; above the same budget
    // it answers a typed refusal instead of passing the resident memory ceiling (#635).
    let (stored_nodes, stored_edges) =
        store::stored_graph_rows(&connection).map_err(store_error)?;
    refuse_oversized_query_load(stored_nodes.saturating_add(stored_edges))?;
    let generation = store::load_generation(&connection)
        .map_err(store_error)?
        .ok_or_else(|| {
            BlueprintError::new(
                "blueprint_store_missing",
                "no persisted Blueprint generation exists",
            )
        })?;
    let observation = generation.source_observation.clone();
    from_store_generation(generation).map(|graph| (graph, observation))
}

/// Read-only queries reuse the materialised generation while the sealed
/// envelope is unchanged. Loading every node and edge from SQLite costs
/// seconds on a large repository; the envelope read that validates the cache
/// costs microseconds. Envelope and rows are written in one transaction, so an
/// equal envelope means equal rows.
type CachedGeneration = (
    store::GenerationEnvelope,
    Arc<GraphGeneration>,
    Option<Value>,
);
const GENERATION_CACHE_REPOSITORIES: usize = 4;
/// Total graph rows (nodes + edges) the cache may hold across repositories (#635). A generation
/// larger than this is answered from one transient load and never retained: the resident engine
/// sat at 3.4 GB steady with a 1.5M-row rightkit generation eligible to live here. The small
/// repositories that make up most queries stay cached.
const GENERATION_CACHE_MAX_ROWS: usize = 300_000;

/// Drops the cached generation of one store when a build or refresh exits, however it exits, and
/// returns the freed pages to the OS.
struct EvictCachedGeneration<'a>(&'a Path);

impl Drop for EvictCachedGeneration<'_> {
    fn drop(&mut self) {
        let evicted = match generation_cache().lock() {
            Ok(mut cache) => {
                let before = cache.len();
                cache.retain(|(cached_path, _)| cached_path != self.0);
                cache.len() != before
            }
            Err(_) => false,
        };
        if evicted {
            crate::memory::release_unused_memory();
        }
    }
}

fn generation_cache() -> &'static Mutex<Vec<(PathBuf, CachedGeneration)>> {
    static CACHE: OnceLock<Mutex<Vec<(PathBuf, CachedGeneration)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

fn load_current_shared(
    path: &Path,
) -> Result<(Arc<GraphGeneration>, Option<Value>), BlueprintError> {
    if !path.exists() {
        return Err(BlueprintError::new(
            "blueprint_store_missing",
            "no persisted Blueprint generation exists",
        ));
    }
    let connection = store::open_store_read_only(path).map_err(store_error)?;
    let envelope = store::read_generation_envelope(&connection)
        .map_err(store_error)?
        .ok_or_else(|| {
            BlueprintError::new(
                "blueprint_store_missing",
                "no persisted Blueprint generation exists",
            )
        })?;
    drop(connection);
    if let Ok(cache) = generation_cache().lock() {
        if let Some((_, (cached, generation, observation))) =
            cache.iter().find(|(cached_path, _)| cached_path == path)
        {
            if *cached == envelope {
                return Ok((Arc::clone(generation), observation.clone()));
            }
        }
    }
    let (generation, observation) = load_current_with_observation(path)?;
    let generation = Arc::new(generation);
    if generation.nodes.len() + generation.edges.len() > GENERATION_CACHE_MAX_ROWS {
        // Too large to retain: the caller's Arc is the only owner and frees it with the request.
        if let Ok(mut cache) = generation_cache().lock() {
            cache.retain(|(cached_path, _)| cached_path != path);
        }
        return Ok((generation, observation));
    }
    if let Ok(mut cache) = generation_cache().lock() {
        cache.retain(|(cached_path, _)| cached_path != path);
        if cache.len() >= GENERATION_CACHE_REPOSITORIES {
            cache.remove(0);
        }
        cache.push((
            path.to_path_buf(),
            (envelope, Arc::clone(&generation), observation.clone()),
        ));
        let rows =
            |entry: &(PathBuf, CachedGeneration)| entry.1 .1.nodes.len() + entry.1 .1.edges.len();
        while cache.len() > 1 && cache.iter().map(rows).sum::<usize>() > GENERATION_CACHE_MAX_ROWS {
            cache.remove(0);
        }
        drop(cache);
        crate::memory::release_unused_memory();
    }
    Ok((generation, observation))
}

fn ensure_generation(
    request: &BlueprintRequest,
    generation: &GraphGeneration,
) -> Result<(), BlueprintError> {
    ensure_generation_id(request, &generation.generation_id)
}

fn ensure_generation_id(
    request: &BlueprintRequest,
    generation_id: &str,
) -> Result<(), BlueprintError> {
    if let Some(expected) = request
        .generation
        .as_deref()
        .or_else(|| request.input.get("generation").and_then(Value::as_str))
    {
        if expected != generation_id {
            return Err(BlueprintError::generation_mismatch(
                expected,
                generation_id.to_owned(),
            ));
        }
    }
    Ok(())
}

/// `export --format html` (#556): write the self-contained Blueprint map of the loaded generation
/// to `map.html` in the graph directory beside the store (`graph_dir`), and answer with where it went. The page is a
/// derived file; the answer carries its path and size, never the page, so the response stays
/// inside its byte bound.
fn export_html_map(
    request: &BlueprintRequest,
    context: &RequestContext,
    db_path: &Path,
) -> Result<Value, BlueprintError> {
    let format = request
        .input
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or("html");
    if format != "html" {
        return Err(BlueprintError::invalid(format!(
            "unsupported export format: {format}"
        )));
    }
    let (generation, _) = load_current_shared(db_path)?;
    ensure_generation(request, &generation)?;
    context.check()?;
    let page = crate::html_map::render(
        &generation.generation_id,
        &generation.provider,
        generation.complete,
        &generation.nodes,
        &generation.edges,
    );
    context.check()?;
    let out = db_path.with_file_name("map.html");
    let tmp = db_path.with_file_name("map.html.tmp");
    fs::write(&tmp, page.as_bytes())
        .and_then(|()| fs::rename(&tmp, &out))
        .map_err(|error| BlueprintError::new("blueprint_map_write_failed", error.to_string()))?;
    Ok(json!({
        "kind": "html-map",
        "generationId": generation.generation_id,
        "complete": generation.complete,
        "path": out.to_string_lossy(),
        "bytes": page.len(),
        "nodeCount": generation.nodes.len(),
        "edgeCount": generation.edges.len(),
    }))
}

fn to_store_generation(graph: &GraphGeneration, source_observation: Value) -> Generation {
    Generation {
        schema_version: Some(graph.schema_version),
        provider: Some(json!({"id": graph.provider, "version": graph.provider_version})),
        manifest: Some(
            json!({"generationId": graph.generation_id, "sourceHash": graph.source_hash, "configDigest": crate::graph::config_digest_for_generation(graph), "complete": graph.complete, "truncationReasons": graph.truncation_reasons}),
        ),
        repo_root: Some(json!(graph.repo_root)),
        augmentation: None,
        source_observation: Some(source_observation),
        nodes: graph
            .nodes
            .iter()
            .filter_map(|value| serde_json::to_value(value).ok())
            .collect(),
        edges: graph
            .edges
            .iter()
            .filter_map(|value| serde_json::to_value(value).ok())
            .collect(),
        file_reports: graph
            .files
            .iter()
            .filter_map(|value| serde_json::to_value(value).ok())
            .collect(),
        documents: None,
        claims: None,
        claim_code_edges: None,
        document_supersession: None,
        extra: Default::default(),
    }
}

fn from_store_generation(value: Generation) -> Result<GraphGeneration, BlueprintError> {
    let manifest = value.manifest.as_ref().ok_or_else(|| {
        BlueprintError::new(
            "blueprint_store_corrupt",
            "persisted generation manifest is missing",
        )
    })?;
    let generation_id = manifest
        .get("generationId")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            BlueprintError::new(
                "blueprint_store_corrupt",
                "persisted generation identity is missing",
            )
        })?
        .to_owned();
    let provider = value
        .provider
        .as_ref()
        .and_then(|v| v.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("native-rust")
        .to_owned();
    let provider_version = value
        .provider
        .as_ref()
        .and_then(|v| v.get("version"))
        .and_then(Value::as_str)
        .unwrap_or("native-rust-1")
        .to_owned();
    Ok(GraphGeneration {
        schema_version: value.schema_version.unwrap_or(graph::GRAPH_SCHEMA_VERSION),
        provider,
        provider_version,
        generation_id,
        source_hash: manifest
            .get("sourceHash")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        repo_root: value
            .repo_root
            .as_ref()
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        complete: manifest
            .get("complete")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        nodes: value
            .nodes
            .into_iter()
            .map(graph_node_from_store)
            .collect::<Result<_, _>>()?,
        edges: value
            .edges
            .into_iter()
            .map(graph_edge_from_store)
            .collect::<Result<_, _>>()?,
        files: value
            .file_reports
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<_, _>>()
            .map_err(|e| BlueprintError::new("blueprint_store_corrupt", e.to_string()))?,
        truncation_reasons: manifest
            .get("truncationReasons")
            .and_then(Value::as_array)
            .map(|v| {
                v.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
    })
}

pub(crate) fn graph_node_from_store(
    value: Value,
) -> Result<crate::model::GraphNode, BlueprintError> {
    let Value::Object(mut object) = value else {
        return Err(BlueprintError::new(
            "blueprint_store_corrupt",
            "persisted graph node is not an object",
        ));
    };
    object.retain(|key, _| {
        matches!(
            key.as_str(),
            "id" | "kind" | "path" | "name" | "generationId" | "evidence"
        )
    });
    serde_json::from_value(Value::Object(object))
        .map_err(|error| BlueprintError::new("blueprint_store_corrupt", error.to_string()))
}

pub(crate) fn graph_edge_from_store(
    value: Value,
) -> Result<crate::model::GraphEdge, BlueprintError> {
    let Value::Object(mut object) = value else {
        return Err(BlueprintError::new(
            "blueprint_store_corrupt",
            "persisted graph edge is not an object",
        ));
    };
    object.retain(|key, _| {
        matches!(
            key.as_str(),
            "id" | "kind" | "source" | "target" | "generationId" | "evidence"
        )
    });
    serde_json::from_value(Value::Object(object))
        .map_err(|error| BlueprintError::new("blueprint_store_corrupt", error.to_string()))
}

pub(crate) fn store_error(error: StoreError) -> BlueprintError {
    let code = match &error {
        StoreError::GenerationNotFound | StoreError::GenerationMismatch { .. } => {
            "blueprint_store_missing"
        }
        StoreError::Sqlite(_)
        | StoreError::Migration(_)
        | StoreError::Json(_)
        | StoreError::InvalidGeneration(_)
        | StoreError::Path(_) => "blueprint_store_corrupt",
    };
    BlueprintError::new(code, error.to_string())
}
