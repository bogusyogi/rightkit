//! Runtime-owned source bindings for native Pull federation.
//!
//! Providers receive typed owner handles. They do not open Cortex, catalog
//! storage, or Blueprint transport themselves.

use rightkit_context_federation::blueprint_client::{BlueprintClient, ContextualBlueprintSource};
use rightkit_context_federation::providers::rules::{
    DeliveryKey, DeliveryLedger, DeliveryMode, DeliveryReceipt, LedgerError, RuleDocument,
    RuleFuture, RuleSource, RuleSourceError, RuleSourceResponse,
};
use rightkit_context_federation::release::{ReleaseError, ReleaseIdentity, ReleaseSource};
use rightkit_context_protocol::{CandidateV1, FreshnessSnapshotV1};
use rightkit_context_provider_sdk::{
    BlueprintSource, FreshnessSource, MemoryCandidate, MemoryCandidateSource, ScopeGrantSource,
    SkillCatalogEntry, SkillCatalogSource, SourceQuery, SourceResponse, SourceResult, SourceSet,
    SourceWarning,
};
use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};

use tokio_util::sync::CancellationToken;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The native Blueprint operation is stateless and its client cache is keyed
/// by (repository, worktree, task, anchors, bounds, expected generation), so
/// one process-wide client lets repeat queries at the same generation answer
/// from cache instead of cold-loading the graph on every request.  A
/// republished generation misses the cache by construction, so freshness can
/// never be served stale.
fn shared_blueprint_client() -> Arc<BlueprintClient> {
    static CLIENT: OnceLock<Arc<BlueprintClient>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            Arc::new(BlueprintClient::from_operation(
                rightkit_repo_graph::native_blueprint_operation(),
            ))
        })
        .clone()
}

/// Runtime-owned source handles captured by one native federation
/// composition. Unbound owners remain absent so providers report an omission.
#[derive(Clone)]
pub struct NativeSourceBindings {
    pub(crate) ledger: Option<Arc<crate::ledger::service::LedgerService>>,
    pub skills: Option<Arc<dyn SkillCatalogSource>>,
    pub memory: Option<Arc<dyn MemoryCandidateSource>>,
    pub scope_grant: Option<Arc<dyn ScopeGrantSource>>,
    pub freshness: Option<Arc<dyn FreshnessSource>>,
    pub blueprint: Option<Arc<dyn BlueprintSource>>,
    pub blueprint_contextual: Option<Arc<dyn ContextualBlueprintSource>>,
    pub release: Option<RuntimeReleaseSource>,
    pub(crate) cancellations: Arc<Mutex<HashMap<String, CancellationToken>>>,
    pub(crate) temporal_queries:
        Arc<Mutex<HashMap<String, rightkit_memory_store::TemporalFactQuery>>>,
    /// How this request's Cortex lexical lane was answered (`scan_fallback:...` once the registry
    /// scan answered, else the index mode observed), recorded by the memory source for the receipt.
    pub(crate) cortex_recall: Arc<Mutex<Option<String>>>,
}

impl std::fmt::Debug for NativeSourceBindings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeSourceBindings")
            .field("skills", &self.skills.is_some())
            .field("memory", &self.memory.is_some())
            .field("scope_grant", &self.scope_grant.is_some())
            .field("freshness", &self.freshness.is_some())
            .field("blueprint", &self.blueprint.is_some())
            .field("blueprint_contextual", &self.blueprint_contextual.is_some())
            .field("release", &self.release.is_some())
            .field("ledger", &self.ledger.is_some())
            .finish()
    }
}

/// The Cortex scope an explicit pull (one-shot or resident) is authorized for, decided once per
/// request before any candidate is selected. A root with a binding is read-authorized through the
/// canonical gate and reads that binding's full descriptor; a malformed, unreadable, mismatched,
/// cross-root or revoked state is a typed refusal and never falls back to another scope. A root
/// the healthy registry does not hold keeps the unenrolled behavior (`None`: the scope derived
/// from the working directory, never labelled authorized). Hooks do not call this.
pub(crate) fn authorized_read_scope(
    repository_root: &Path,
) -> Result<Option<serde_json::Value>, String> {
    use crate::authorization::{authorize_context_read, ContextReadScope, PULL_REFUSAL_PREFIX};
    use crate::scope::ScopeDescriptorV1;
    match authorize_context_read(&repository_root.to_string_lossy()) {
        Ok(ContextReadScope::Unenrolled) => Ok(None),
        Ok(ContextReadScope::Enrolled {
            scope_descriptor, ..
        }) => {
            let shape = serde_json::from_value::<ScopeDescriptorV1>(scope_descriptor.clone());
            if let Err(error) = shape {
                return Err(format!(
                    "{PULL_REFUSAL_PREFIX}caller_scope_binding_denied:invalid descriptor: {error}"
                ));
            }
            Ok(Some(scope_descriptor))
        }
        Err(denial) => Err(denial.refusal()),
    }
}

impl NativeSourceBindings {
    pub(crate) fn with_ambient_store_and_deadline(
        _repository_root: &Path,
        scope_grant_id: Option<&str>,
        store: crate::MemoryStore,
        deadline: rightkit_context_federation::deadline::Deadline,
        explicit_pull: bool,
    ) -> Result<Self, String> {
        Self::with_store_and_deadline_mode(
            scope_grant_id,
            store,
            Some(deadline),
            true,
            explicit_pull,
        )
    }

    pub(crate) fn with_store_and_deadline(
        _repository_root: &Path,
        scope_grant_id: Option<&str>,
        store: crate::MemoryStore,
        deadline: Option<rightkit_context_federation::deadline::Deadline>,
    ) -> Result<Self, String> {
        Self::with_store_and_deadline_mode(scope_grant_id, store, deadline, false, false)
    }

    fn with_store_and_deadline_mode(
        scope_grant_id: Option<&str>,
        store: crate::MemoryStore,
        deadline: Option<rightkit_context_federation::deadline::Deadline>,
        persisted_freshness: bool,
        explicit_pull: bool,
    ) -> Result<Self, String> {
        let catalog_path = crate::catalog::default_catalog_path()
            .map_err(|error| format!("resolve context catalog: {error}"))?;
        let catalog = if deadline.is_some() {
            None
        } else {
            Some(
                crate::catalog::ContextCatalog::open(&catalog_path)
                    .map_err(|error| format!("open context catalog: {error}"))?,
            )
        };
        let blueprint = shared_blueprint_client();
        let cancellations = Arc::new(Mutex::new(HashMap::new()));
        let temporal_queries = Arc::new(Mutex::new(HashMap::new()));
        let cortex_recall = Arc::new(Mutex::new(None));

        Ok(Self {
            ledger: crate::ledger::service::active_owner()
                .or_else(|_| crate::ledger::service::open_explicit_owner())
                .ok(),
            skills: Some(Arc::new(RuntimeSkillsSource {
                store: store.clone(),
            })),
            memory: Some(Arc::new(RuntimeMemorySource {
                store: store.clone(),
                cancellations: cancellations.clone(),
                temporal_queries: temporal_queries.clone(),
                lexical_only: persisted_freshness,
                explicit_pull,
                cortex_recall: cortex_recall.clone(),
            })),
            scope_grant: Some(Arc::new(RuntimeScopeGrantSource {
                catalog,
                catalog_path,
                deadline,
                grant_id: scope_grant_id.map(str::to_owned),
            })),
            freshness: Some(Arc::new(RuntimeFreshnessSource {
                store,
                deadline,
                persisted_freshness,
            })),
            blueprint: Some(blueprint.clone()),
            blueprint_contextual: Some(blueprint),
            release: Some(RuntimeReleaseSource),
            cancellations,
            temporal_queries,
            cortex_recall,
        })
    }

    pub fn source_set(&self) -> SourceSet {
        SourceSet {
            skills: self.skills.clone(),
            memory: self.memory.clone(),
            scope_grant: self.scope_grant.clone(),
            freshness: self.freshness.clone(),
            blueprint: self.blueprint.clone(),
        }
    }
}

/// Runtime-owned release identity.  Its generation is compiled into this
/// binary by `release_identity`; it never falls back to repository contents.
#[derive(Clone, Copy, Debug, Default)]
pub struct RuntimeReleaseSource;

impl RuntimeReleaseSource {
    pub fn generation() -> Result<String, String> {
        RuntimeReleaseSource
            .current_release()
            .map(|identity| identity.generation)
            .map_err(|error| error.to_string())
    }
}

impl ReleaseSource for RuntimeReleaseSource {
    fn current_release(&self) -> Result<ReleaseIdentity, ReleaseError> {
        ReleaseIdentity::new(
            crate::release_identity::release_generation(),
            "membrane-runtime.release_identity",
            Some("release_identity::release_generation".to_owned()),
        )
    }
}

#[derive(Clone)]
struct RuntimeMemorySource {
    store: crate::MemoryStore,
    cancellations: Arc<Mutex<HashMap<String, CancellationToken>>>,
    temporal_queries: Arc<Mutex<HashMap<String, rightkit_memory_store::TemporalFactQuery>>>,
    lexical_only: bool,
    /// An explicit pull: Cortex recall uses the explicit-pull entry (hooks never do).
    explicit_pull: bool,
    cortex_recall: Arc<Mutex<Option<String>>>,
}

impl MemoryCandidateSource for RuntimeMemorySource {
    fn candidates<'a, 'b, 'c>(
        &'a self,
        query: &'b SourceQuery,
    ) -> BoxFuture<'c, SourceResult<Vec<MemoryCandidate>>>
    where
        'a: 'c,
        'b: 'c,
        Self: 'c,
    {
        let store = self.store.clone();
        let cancellations = self.cancellations.clone();
        let temporal_queries = self.temporal_queries.clone();
        let lexical_only = self.lexical_only;
        let explicit_pull = self.explicit_pull;
        let cortex_recall = self.cortex_recall.clone();
        let query = query.clone();
        Box::pin(async move {
            let cancellation = cancellations
                .lock()
                .ok()
                .and_then(|tokens| tokens.get(&query.request_id).cloned())
                .unwrap_or_default();
            let descriptor = match query.cortex_scope_descriptor {
                Some(value) => serde_json::from_value::<crate::scope::ScopeDescriptorV1>(value)
                    .map_err(|error| {
                        rightkit_context_provider_sdk::ProviderError::InvalidRequest {
                            operation: "memory.candidates".to_owned(),
                            message: format!("invalid authorized Cortex scope descriptor: {error}"),
                        }
                    })?,
                None => crate::scope::ScopeDescriptorV1::filesystem(&query.repository_root),
            };
            let temporal = temporal_queries
                .lock()
                .ok()
                .and_then(|queries| queries.get(&query.request_id).cloned());
            let payload = if lexical_only {
                crate::pull::federation::memory_candidates_payload_for_descriptor_lexical(
                    &store, &query.task, &descriptor, 64, Some(Path::new(&query.repository_root)),
                    explicit_pull,
                )
            } else {
                crate::pull::federation::memory_candidates_payload_for_descriptor_cancellable_with_temporal(
                    &store, &query.task, &descriptor, 64,
                    Some(Path::new(&query.repository_root)), &cancellation, temporal,
                )
            }
            .map_err(rightkit_context_provider_sdk::ProviderError::Unavailable)?;
            let generation = query.generation.clone().ok_or_else(|| {
                rightkit_context_provider_sdk::ProviderError::Uninitialized(
                    "Cortex generation binding is unavailable".to_owned(),
                )
            })?;
            let values = payload
                .get("candidates")
                .and_then(serde_json::Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut candidates = Vec::with_capacity(values.len());
            for value in values {
                let candidate: CandidateV1 = serde_json::from_value(value).map_err(|error| {
                    rightkit_context_provider_sdk::ProviderError::MalformedOutput(error.to_string())
                })?;
                candidates.push(MemoryCandidate {
                    id: candidate.id.clone(),
                    repository_id: query.repository_id.clone(),
                    generation: generation.clone(),
                    source_hash: candidate.source_hash.clone(),
                    candidate,
                });
            }
            let completeness = payload
                .get("completeness")
                .cloned()
                .and_then(|value| {
                    serde_json::from_value::<crate::store::CortexCompletenessV1>(value).ok()
                })
                .unwrap_or_else(|| {
                    crate::store::CortexCompletenessV1::lower_bound(
                        "completeness_unavailable",
                        candidates.len(),
                        candidates.len(),
                        0,
                    )
                });
            if lexical_only {
                // Record which index answered; a scan fallback, once seen, is not overwritten by a
                // later (corrective) pass of the same request.
                let mode = store.last_recall_lexical_mode();
                let scanned = completeness
                    .causes
                    .iter()
                    .any(|cause| cause == crate::store::LEXICAL_SCAN_FALLBACK_CAUSE);
                if let Ok(mut recorded) = cortex_recall.lock() {
                    let keeps = recorded
                        .as_deref()
                        .is_some_and(|value| value.starts_with("scan_fallback:"));
                    if !keeps && (scanned || recorded.is_none()) {
                        *recorded = mode;
                    }
                }
            }
            // The scan fallback is an informational cause, not a source failure: it stays in the
            // completeness and reaches the receipt as `cortexRecall`, never as a provider warning.
            let warnings = completeness
                .causes
                .iter()
                .filter(|cause| cause.as_str() != crate::store::LEXICAL_SCAN_FALLBACK_CAUSE)
                .map(|cause| cortex_completeness_warning(cause, &query.request_id))
                .collect();
            Ok(SourceResponse {
                value: candidates,
                generation: Some(generation),
                complete: completeness.is_exact(),
                warnings,
            })
        })
    }
}

fn cortex_completeness_warning(cause: &str, request_id: &str) -> SourceWarning {
    let code = match cause {
        "cancelled" => "cancelled",
        "temporal_scope_rejected" => "scope_grant_invalid",
        "temporal_unavailable" | "completeness_unavailable" => "provider_unavailable",
        _ => "provider_failed",
    };
    SourceWarning {
        code: code.to_owned(),
        detail_id: Some(format!("{cause}:{request_id}")),
    }
}

#[derive(Clone)]
struct RuntimeSkillsSource {
    store: crate::MemoryStore,
}

impl SkillCatalogSource for RuntimeSkillsSource {
    fn skills<'a, 'b, 'c>(
        &'a self,
        query: &'b SourceQuery,
    ) -> BoxFuture<'c, SourceResult<Vec<SkillCatalogEntry>>>
    where
        'a: 'c,
        'b: 'c,
        Self: 'c,
    {
        let store = self.store.clone();
        let repository_id = query.repository_id.clone();
        Box::pin(async move {
            let snapshot = store
                .skills_snapshot()
                .map_err(rightkit_context_provider_sdk::ProviderError::Unavailable)?;
            let generation = snapshot.generation.clone();
            let results = store
                .search_skills(&query.task, 64)
                .map_err(rightkit_context_provider_sdk::ProviderError::Unavailable)?;
            let value = results
                .items
                .into_iter()
                .map(|entry| SkillCatalogEntry {
                    id: entry.name,
                    repository_id: repository_id.clone(),
                    generation: generation.clone(),
                    source_hash: entry.body_hash,
                    title: entry.description,
                    keywords: Vec::new(),
                })
                .collect();
            Ok(SourceResponse {
                value,
                generation: Some(generation),
                complete: results.completeness.is_exact(),
                warnings: results
                    .completeness
                    .causes
                    .into_iter()
                    .map(|code| SourceWarning {
                        code,
                        detail_id: Some(query.request_id.clone()),
                    })
                    .collect(),
            })
        })
    }
}

async fn owner_work_permit(
    deadline: Option<rightkit_context_federation::deadline::Deadline>,
) -> Result<tokio::sync::OwnedSemaphorePermit, rightkit_context_provider_sdk::ProviderError> {
    check_owner_deadline(deadline)?;
    let acquire = super::native_federation::provider_capacity().acquire_owned();
    let permit = match deadline {
        Some(deadline) => tokio::time::timeout_at(deadline.instant().into(), acquire)
            .await
            .map_err(|_| rightkit_context_provider_sdk::ProviderError::DeadlineExceeded)?,
        None => acquire.await,
    };
    permit.map_err(|_| {
        rightkit_context_provider_sdk::ProviderError::Unavailable("provider capacity closed".into())
    })
}

fn check_owner_deadline(
    deadline: Option<rightkit_context_federation::deadline::Deadline>,
) -> Result<(), rightkit_context_provider_sdk::ProviderError> {
    if deadline.is_some_and(|deadline| deadline.is_exhausted_at(std::time::Instant::now())) {
        return Err(rightkit_context_provider_sdk::ProviderError::DeadlineExceeded);
    }
    Ok(())
}

#[derive(Clone)]
struct RuntimeFreshnessSource {
    store: crate::MemoryStore,
    deadline: Option<rightkit_context_federation::deadline::Deadline>,
    persisted_freshness: bool,
}

/// Latency-bound freshness read for foreground callers (hook path). This is
/// the same observation machinery as the resident path: the resident
/// watcher's barrier state plus sealed generation basis when a resident
/// service covers the repo, else a staged bounded status dispatch. The
/// verdict is read honestly — `stale` reports `stale:true`, and any
/// incomplete observation returns a typed `freshness_unavailable` error the
/// engine degrades, never a fabricated `clean`.
fn persisted_blueprint_freshness(
    query: &SourceQuery,
    deadline: Option<rightkit_context_federation::deadline::Deadline>,
) -> Result<SourceResponse<FreshnessSnapshotV1>, String> {
    let root = PathBuf::from(&query.repository_root);
    let status = crate::freshness::read_blueprint_status_resident(&root, deadline)?;
    let result = status
        .get("result")
        .ok_or_else(|| "blueprint_freshness_result_missing".to_owned())?;
    let state = result
        .get("state")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "blueprint_freshness_state_missing".to_owned())?;
    let generation = result
        .pointer("/manifest/generationId")
        .or_else(|| status.get("generation"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let base_commit = result
        .pointer("/manifest/baseCommit")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    if generation.is_none() {
        return Err("blueprint_freshness_generation_missing".to_owned());
    }
    let snapshot_id = format!(
        "blueprint:{}:{}",
        generation.as_deref().unwrap_or("none"),
        base_commit.as_deref().unwrap_or("unknown"),
    );
    match state {
        "fresh" => Ok(SourceResponse {
            value: FreshnessSnapshotV1 {
                graph_state: "clean".to_owned(),
                generation: generation.clone(),
                snapshot_id: Some(snapshot_id),
                base_commit,
                overlay_digest: None,
                stale: false,
            },
            generation,
            complete: true,
            warnings: Vec::new(),
        }),
        "stale" => Ok(SourceResponse {
            value: FreshnessSnapshotV1 {
                graph_state: "stale_snapshot".to_owned(),
                generation: generation.clone(),
                snapshot_id: Some(snapshot_id),
                base_commit,
                overlay_digest: None,
                stale: true,
            },
            generation,
            complete: true,
            warnings: vec![SourceWarning {
                code: "freshness_stale".to_owned(),
                detail_id: Some("snapshot_base_differs_from_head".to_owned()),
            }],
        }),
        other => Err(format!("freshness_unavailable:observation_{other}")),
    }
}

impl FreshnessSource for RuntimeFreshnessSource {
    fn freshness<'a, 'b, 'c>(
        &'a self,
        query: &'b SourceQuery,
    ) -> BoxFuture<'c, SourceResult<FreshnessSnapshotV1>>
    where
        'a: 'c,
        'b: 'c,
        Self: 'c,
    {
        let store = self.store.clone();
        let root = PathBuf::from(&query.repository_root);
        let deadline = self.deadline;
        let persisted_freshness = self.persisted_freshness;
        let query = query.clone();
        Box::pin(async move {
            let permit = owner_work_permit(deadline).await?;
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                check_owner_deadline(deadline)?;
                if persisted_freshness {
                    // The latency-bound profile reads resident watcher state
                    // plus the sealed generation basis; the status dispatch
                    // underneath is deadline-bounded end to end, so a short
                    // budget degrades to a typed `freshness_unavailable`
                    // instead of spending the window on a live fingerprint.
                    return persisted_blueprint_freshness(&query, deadline)
                        .map_err(rightkit_context_provider_sdk::ProviderError::Unavailable);
                }
                let verdict =
                    crate::freshness::evaluate_repository_freshness_until(&store, root, deadline);
                let graph_state = serde_json::to_string(&verdict.graph_state)
                    .unwrap_or_else(|_| "\"indeterminate\"".to_owned())
                    .trim_matches('"')
                    .to_owned();
                let stale = !verdict.stable
                    || matches!(
                        verdict.graph_state,
                        crate::freshness::GraphState::StaleSnapshot
                    );
                let warning = (!verdict.stable).then(|| SourceWarning {
                    code: "freshness_unavailable".to_owned(),
                    detail_id: verdict.reasons.first().cloned(),
                });
                Ok(SourceResponse {
                    value: FreshnessSnapshotV1 {
                        graph_state,
                        generation: verdict.blueprint_generation.clone(),
                        snapshot_id: Some(verdict.snapshot_id),
                        // An overlay is a diff against a base commit; a root
                        // without one (no git) has no overlay basis, and the
                        // protocol rejects the pair as malformed. Reporting it
                        // anyway split Pull's freshness from the engine's
                        // degraded binding and failed every source receipt
                        // as a generation mismatch.
                        overlay_digest: verdict
                            .base_commit
                            .is_some()
                            .then_some(verdict.overlay_digest),
                        base_commit: verdict.base_commit,
                        stale,
                    },
                    generation: verdict.blueprint_generation,
                    complete: verdict.stable,
                    warnings: warning.into_iter().collect(),
                })
            })
            .await
            .map_err(|error| {
                rightkit_context_provider_sdk::ProviderError::Unavailable(format!(
                    "freshness blocking worker: {error}"
                ))
            })?
        })
    }
}

#[derive(Clone)]
struct RuntimeScopeGrantSource {
    catalog: Option<crate::catalog::ContextCatalog>,
    catalog_path: PathBuf,
    deadline: Option<rightkit_context_federation::deadline::Deadline>,
    grant_id: Option<String>,
}

impl ScopeGrantSource for RuntimeScopeGrantSource {
    fn grant<'a, 'b, 'c>(
        &'a self,
        query: &'b SourceQuery,
    ) -> BoxFuture<'c, SourceResult<rightkit_context_provider_sdk::ScopeGrantView>>
    where
        'a: 'c,
        'b: 'c,
        Self: 'c,
    {
        let catalog = self.catalog.clone();
        let catalog_path = self.catalog_path.clone();
        let deadline = self.deadline;
        let grant_id = self.grant_id.clone();
        let query = query.clone();
        Box::pin(async move {
            let permit = owner_work_permit(deadline).await?;
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                check_owner_deadline(deadline)?;
                let Some(id) = grant_id else {
                    return Err(rightkit_context_provider_sdk::ProviderError::Unavailable(
                        "scope_grant_missing".into(),
                    ));
                };
                let grant = match deadline {
                    Some(deadline) => {
                        crate::catalog::lookup_grant_until(&catalog_path, &id, deadline)
                    }
                    None => crate::catalog::lookup_grant(
                        catalog.as_ref().expect("unbounded catalog owner"),
                        &id,
                    )
                    .map_err(|error| error.to_string()),
                }
                .map_err(|error| {
                    rightkit_context_provider_sdk::ProviderError::Unavailable(error.to_string())
                })?
                .ok_or_else(|| {
                    rightkit_context_provider_sdk::ProviderError::Unavailable(
                        "scope_grant_missing".into(),
                    )
                })?;
                let complete = grant.permits();
                if !grant
                    .repository_ids
                    .iter()
                    .any(|repository_id| repository_id == &query.repository_id)
                {
                    return Err(rightkit_context_provider_sdk::ProviderError::Unavailable(
                        "scope_grant_repository_mismatch".into(),
                    ));
                }
                let value = rightkit_context_provider_sdk::ScopeGrantView {
                    id: grant.id,
                    repository_id: query.repository_id,
                    repository_root: query.repository_root,
                    task_id: grant.task_id,
                    session_id: grant.session_id,
                    manifest_digest: grant.manifest_digest,
                    blueprint_generation: query.generation.unwrap_or_else(|| "unknown".to_owned()),
                    permitted_edge_types: grant.permitted_edge_types,
                    read_paths: grant
                        .read_paths
                        .into_iter()
                        .map(|path| format!("{}:{}-{}", path.path, path.start_line, path.end_line))
                        .collect(),
                };
                Ok(SourceResponse {
                    value,
                    generation: None,
                    complete,
                    warnings: Vec::new(),
                })
            })
            .await
            .map_err(|error| {
                rightkit_context_provider_sdk::ProviderError::Unavailable(format!(
                    "scope grant blocking worker: {error}"
                ))
            })?
        })
    }
}

/// Filesystem owner adapter for grant-authorized rule paths.
#[derive(Clone, Copy, Debug, Default)]
pub struct RuntimeRuleSource;

impl RuleSource for RuntimeRuleSource {
    fn read_rules(
        &self,
        query: SourceQuery,
        authorized_paths: Vec<String>,
    ) -> RuleFuture<RuleSourceResponse> {
        Box::pin(async move {
            let root = PathBuf::from(&query.repository_root)
                .canonicalize()
                .map_err(|error| RuleSourceError::Unavailable(error.to_string()))?;
            let mut documents = Vec::new();
            for relative in authorized_paths {
                let candidate = root.join(&relative);
                let path = candidate
                    .canonicalize()
                    .map_err(|error| RuleSourceError::Unavailable(error.to_string()))?;
                if !path.starts_with(&root) {
                    return Err(RuleSourceError::Unauthorized(
                        "path_outside_repository".into(),
                    ));
                }
                let content = std::fs::read_to_string(&path)
                    .map_err(|error| RuleSourceError::Unavailable(error.to_string()))?;
                documents.push(RuleDocument {
                    repository_id: query.repository_id.clone(),
                    normalized_path: relative.clone(),
                    rule_identity: relative,
                    source_hash: None,
                    trust_class: "workspace_tracked".into(),
                    instruction_policy: "data_only".into(),
                    content,
                });
            }
            Ok(RuleSourceResponse {
                documents,
                generation: query.generation,
                complete: true,
            })
        })
    }
}

/// Process-local ledger for non-self-loading clients. It retains only
/// identity hashes, never rule bodies or prompt text.
#[derive(Clone, Default)]
pub struct RuntimeDeliveryLedger {
    claimed: Arc<Mutex<BTreeSet<String>>>,
}

impl DeliveryLedger for RuntimeDeliveryLedger {
    fn claim(
        &self,
        key: DeliveryKey,
    ) -> Pin<Box<dyn Future<Output = Result<DeliveryReceipt, LedgerError>> + Send>> {
        let claimed = self.claimed.clone();
        Box::pin(async move {
            let identity = format!(
                "{}\n{}\n{}\n{}\n{}",
                key.repository_id, key.client, key.session_id, key.candidate_id, key.source_hash
            );
            let receipt_id = format!(
                "native-delivery-{}",
                rightkit_fs::digest::sha256_bytes(identity.as_bytes())
            );
            let mut claimed = claimed
                .lock()
                .map_err(|_| LedgerError::Unavailable("ledger_poisoned".into()))?;
            let first = claimed.insert(receipt_id.clone());
            Ok(DeliveryReceipt {
                receipt_id,
                mode: if first {
                    DeliveryMode::Inline
                } else {
                    DeliveryMode::Reference
                },
            })
        })
    }
}
