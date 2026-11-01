//! Native FederationEngine composition used by Membrane runtime.
//!
//! The engine is built once per runtime composition from typed owner source
//! handles.  It returns candidates only; planner admission remains the next
//! step in [`super::super::federation`].

use super::federation_sources::{NativeSourceBindings, RuntimeDeliveryLedger, RuntimeRuleSource};
use crate::pull::metrics::{FederationMetricStatus, FederationMetrics};
use rightkit_context_federation::providers::{
    anchors::AnchorsProvider, blueprint::BlueprintProvider, cortex::CortexProvider,
    git::GitProvider, live_files::LiveFilesProvider, rules::RulesProvider,
};
use rightkit_context_federation::{
    FederationConfig, FederationEngine, FusionStrategy, ProviderConfig, ProviderRegistry,
};
use rightkit_context_protocol::{
    FederationProviderStatusV1, FederationRequestV1, FederationResponseV1, ProviderId,
    SourceResolutionReceiptV1, SourceResolutionStatusV1,
};
use rightkit_context_provider_sdk::{
    FreshnessSource, Provider, ProviderContext, ProviderError, ProviderOutput,
    ProviderRegistration, SourceQuery,
};
use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

// Provider lanes are admitted concurrently (ten lanes in V1), while their
// owner-backed work is moved to Tokio's bounded blocking pool. Keep a small
// async scheduling pool for timers, cancellation, and result assembly.
const NATIVE_FEDERATION_ASYNC_WORKERS: usize = 2;
const NATIVE_FEDERATION_BLOCKING_WORKERS: usize = 10;
static PROVIDER_BLOCKING_CAPACITY: OnceLock<Arc<Semaphore>> = OnceLock::new();

pub(super) fn provider_capacity() -> Arc<Semaphore> {
    PROVIDER_BLOCKING_CAPACITY
        .get_or_init(|| Arc::new(Semaphore::new(NATIVE_FEDERATION_BLOCKING_WORKERS)))
        .clone()
}

pub(crate) fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(NATIVE_FEDERATION_ASYNC_WORKERS)
        .max_blocking_threads(NATIVE_FEDERATION_BLOCKING_WORKERS)
        .enable_all()
        .build()
        .map_err(|error| format!("create native federation runtime: {error}"))
}

/// Resident callers are already on a blocking dispatch thread. Borrow their
/// engine runtime; only standalone compatibility callers need a local bridge.
pub(crate) fn run_on_runtime<T: Send>(
    handle: Option<&tokio::runtime::Handle>,
    work: impl Future<Output = Result<T, String>> + Send,
) -> Result<T, String> {
    if let Some(handle) = handle {
        return handle.block_on(work);
    }
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let runtime = runtime()?;
                let result = runtime.block_on(work);
                runtime.shutdown_timeout(std::time::Duration::from_secs(5));
                result
            })
            .join()
            .map_err(|_| "native federation thread panicked".to_owned())?
    })
}

/// Native engine plus immutable source handles.  No Python gateway, worker,
/// stdio framing, or dynamic provider lookup is reachable from this type.
pub struct NativeFederation {
    engine: FederationEngine,
    metrics: Arc<FederationMetrics>,
    freshness: Arc<dyn FreshnessSource>,
    last_freshness: Arc<Mutex<Option<rightkit_context_protocol::FreshnessSnapshotV1>>>,
    cancellations: Arc<Mutex<HashMap<String, CancellationToken>>>,
    temporal_queries: Arc<Mutex<HashMap<String, rightkit_memory_store::TemporalFactQuery>>>,
}

impl std::fmt::Debug for NativeFederation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeFederation")
            .field("engine", &self.engine)
            .field("freshness", &"injected")
            .finish()
    }
}

impl NativeFederation {
    pub fn new(bindings: NativeSourceBindings) -> Result<Self, String> {
        Self::with_config(bindings, FederationConfig::all_enabled())
    }

    pub fn hook(bindings: NativeSourceBindings) -> Result<Self, String> {
        // SessionStart orientation must surface Ledger, Blueprint and Cortex
        // evidence when fixtures make each relevant — not a Cortex-only
        // packet. All four lanes are bounded indexed reads under the request
        // deadline now that per-request full-root sync and mandatory
        // whole-worktree fingerprints are gone; a lane that cannot answer in
        // budget emits its typed omission rather than consuming the window.
        let providers = ProviderId::ALL
            .into_iter()
            .map(|id| {
                if matches!(
                    id,
                    ProviderId::Cortex
                        | ProviderId::Skills
                        | ProviderId::Ledger
                        | ProviderId::Blueprint
                ) {
                    ProviderConfig::enabled(id)
                } else {
                    ProviderConfig::disabled(id)
                }
            })
            .collect();
        let config = FederationConfig::new(providers).map_err(|error| error.to_string())?;
        Self::with_config(bindings, config)
    }

    fn with_config(
        bindings: NativeSourceBindings,
        config: FederationConfig,
    ) -> Result<Self, String> {
        let cancellations = bindings.cancellations.clone();
        let temporal_queries = bindings.temporal_queries.clone();
        let blueprint = bindings
            .blueprint
            .clone()
            .ok_or_else(|| "native Blueprint source unavailable".to_owned())?;
        let contextual = bindings
            .blueprint_contextual
            .clone()
            .ok_or_else(|| "native Blueprint contextual source unavailable".to_owned())?;
        let freshness = bindings
            .freshness
            .clone()
            .ok_or_else(|| "native freshness source unavailable".to_owned())?;
        let release = bindings
            .release
            .ok_or_else(|| "native release source unavailable".to_owned())?;
        let blueprint_provider: Arc<dyn rightkit_context_provider_sdk::Provider> =
            Arc::new(BlueprintSourceOwnerProvider {
                inner: Arc::new(BlueprintProvider::with_contextual_source_pair(
                    blueprint,
                    contextual.clone(),
                )),
                ledger: bindings.ledger.clone(),
            });
        let anchors_provider = AnchorsProvider::default().with_blueprint_source(contextual);
        let rules = Arc::new(RuntimeRuleSource);
        let ledger = Arc::new(RuntimeDeliveryLedger::default());
        let providers = vec![
            registration(
                ProviderId::Anchors,
                "native.anchors",
                Arc::new(anchors_provider),
                vec![ProviderId::Blueprint],
            ),
            registration(
                ProviderId::Blueprint,
                "native.blueprint",
                blueprint_provider,
                vec![],
            ),
            registration(
                ProviderId::Rules,
                "native.rules",
                Arc::new(RulesProvider::new(rules, ledger)),
                vec![],
            ),
            registration(
                ProviderId::LiveFiles,
                "native.live_files",
                Arc::new(LiveFilesProvider::default()),
                vec![],
            ),
            registration(ProviderId::Git, "native.git", Arc::new(GitProvider), vec![]),
            registration(
                ProviderId::Audit,
                "native.audit",
                Arc::new(RetiredLaneProvider(ProviderId::Audit)),
                vec![],
            ),
            registration(
                ProviderId::Architect,
                "native.architect",
                Arc::new(RetiredLaneProvider(ProviderId::Architect)),
                vec![],
            ),
            registration(
                ProviderId::Skills,
                "native.skills",
                // LDG-032: Ledger owns the skill-document index lane — source/
                // revision/span/ticket binding identical to the document lane.
                Arc::new(crate::ledger::provider::LedgerSkillProvider::new(
                    bindings.ledger.clone(),
                )),
                vec![],
            ),
            registration(
                ProviderId::Cortex,
                "native.cortex",
                Arc::new(CortexProvider::new()),
                vec![],
            ),
            registration(
                ProviderId::Ledger,
                "native.ledger",
                Arc::new(crate::ledger::provider::LedgerProvider::new(
                    bindings.ledger.clone(),
                )),
                vec![],
            ),
        ];
        let registry = ProviderRegistry::new(providers).map_err(|e| e.to_string())?;
        let engine =
            FederationEngine::with_release_source(registry, config, bindings.source_set(), release)
                .map_err(|e| e.to_string())?
                .with_fusion_strategy(FusionStrategy::FixedOrder);
        Ok(Self {
            engine,
            metrics: Arc::new(FederationMetrics::new()),
            freshness,
            last_freshness: Arc::new(Mutex::new(None)),
            cancellations,
            temporal_queries,
        })
    }

    pub fn metrics_snapshot(&self) -> std::collections::BTreeMap<String, u64> {
        self.metrics.snapshot()
    }

    pub fn freshness_snapshot(&self) -> Option<rightkit_context_protocol::FreshnessSnapshotV1> {
        self.last_freshness
            .lock()
            .ok()
            .and_then(|snapshot| snapshot.clone())
    }

    pub async fn federate_until(
        &self,
        request: &FederationRequestV1,
        cancellation: CancellationToken,
        deadline: rightkit_context_federation::deadline::Deadline,
    ) -> Result<FederationResponseV1, String> {
        if deadline.is_exhausted_at(std::time::Instant::now()) {
            return Err("federation deadline exhausted during owner binding".to_owned());
        }
        let cancelled = cancellation.is_cancelled();
        let work_cancellation = cancellation.child_token();
        let _cancel_work = work_cancellation.clone().drop_guard();
        let temporal_query = request
            .extensions
            .get("cortexTemporalQuery")
            .cloned()
            .map(|value| {
                serde_json::from_value::<rightkit_memory_store::TemporalFactQuery>(value)
                    .map_err(|error| format!("invalid cortexTemporalQuery: {error}"))
            })
            .transpose()?;
        let query = SourceQuery {
            request_id: request.request_id.clone(),
            repository_id: rightkit_context_federation::root::canonical_repository_id(Path::new(
                &request.repository_root,
            )),
            repository_root: request.repository_root.clone(),
            task: request.task.clone(),
            session_id: request.session_id.clone(),
            generation: request
                .release_generation
                .clone()
                .or_else(|| request.blueprint_generation.clone()),
            cortex_scope_descriptor: request.extensions.get("cortexScopeDescriptor").cloned(),
            anchors: request.anchors.clone(),
        };
        let freshness = tokio::select! {
            result = tokio::time::timeout_at(deadline.instant().into(), self.freshness.freshness(&query)) =>
                result.map_err(|_| "federation deadline exhausted during owner binding".to_owned())?,
            _ = cancellation.cancelled() =>
                return Err("federation request cancelled during owner binding".to_owned()),
        };
        if deadline.is_exhausted_at(std::time::Instant::now()) {
            return Err("federation deadline exhausted during owner binding".to_owned());
        }
        match freshness {
            Ok(snapshot) => {
                let stale = snapshot.value.stale;
                if let Ok(mut current) = self.last_freshness.lock() {
                    *current = Some(snapshot.value);
                }
                if stale {
                    self.metrics.record(FederationMetricStatus::Stale);
                }
            }
            Err(error) => {
                let status = error_status(&error.to_string(), cancelled);
                self.metrics.record(status);
                match error {
                    rightkit_context_provider_sdk::ProviderError::Unavailable(_)
                    | rightkit_context_provider_sdk::ProviderError::Uninitialized(_)
                    | rightkit_context_provider_sdk::ProviderError::MissingSource(_)
                    | rightkit_context_provider_sdk::ProviderError::Incomplete(_) => {
                        if let Ok(mut current) = self.last_freshness.lock() {
                            *current = Some(rightkit_context_protocol::FreshnessSnapshotV1 {
                                graph_state: "unavailable".to_owned(),
                                generation: None,
                                snapshot_id: None,
                                base_commit: None,
                                overlay_digest: None,
                                stale: false,
                            });
                        }
                    }
                    other => return Err(other.to_string()),
                }
            }
        }
        if let Ok(mut tokens) = self.cancellations.lock() {
            tokens.insert(request.request_id.clone(), work_cancellation.clone());
        }
        if let Some(temporal_query) = temporal_query {
            if let Ok(mut queries) = self.temporal_queries.lock() {
                queries.insert(request.request_id.clone(), temporal_query);
            }
        }
        let response = tokio::select! {
            response = self.engine.federate_until(request, work_cancellation.clone(), deadline) => response,
            _ = cancellation.cancelled() => return Err("federation request cancelled".to_owned()),
        };
        // Provider adapters may own synchronous work behind the future. Signal
        // descendants on every exit so no child keeps running after Pull has
        // released its request scope.
        work_cancellation.cancel();
        if let Ok(mut tokens) = self.cancellations.lock() {
            tokens.remove(&request.request_id);
        }
        if let Ok(mut queries) = self.temporal_queries.lock() {
            queries.remove(&request.request_id);
        }
        match &response {
            Ok(value) => self.metrics.record(metric_status(value, cancelled)),
            Err(error) => self
                .metrics
                .record(error_status(&error.to_string(), cancelled)),
        }
        response.map_err(|e| e.to_string())
    }
}

/// Blueprint's graph identity names a path/hash but does not read source.
/// Bind graph candidates through Ledger's source owner before issuing the
/// only resolution receipt that can reach native admission.
struct BlueprintSourceOwnerProvider {
    inner: Arc<dyn Provider>,
    ledger: Option<Arc<crate::ledger::service::LedgerService>>,
}

impl Provider for BlueprintSourceOwnerProvider {
    fn provide<'a, 'b, 'async_trait>(
        &'a self,
        context: &'b ProviderContext,
    ) -> Pin<Box<dyn Future<Output = Result<ProviderOutput, ProviderError>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        let inner = Arc::clone(&self.inner);
        let ledger = self.ledger.clone();
        let context = context.clone();
        Box::pin(async move {
            let mut output = inner.provide(&context).await?;
            output.extensions.remove("sourceResolutions");
            let mut receipts = Vec::new();
            let mut bind_omissions = Vec::new();
            let generation = output.generation.clone();
            let budget = crate::ledger::limits::WorkBudget::new(
                context.deadline,
                context.cancellation.clone(),
            );
            let pinned_generation = context
                .scope_grant
                .as_ref()
                .map(|grant| grant.blueprint_generation.clone())
                .filter(|value| !value.is_empty())
                .or_else(|| {
                    context
                        .freshness
                        .generation
                        .clone()
                        .filter(|value| !value.is_empty())
                });
            let generation_coherent = pinned_generation
                .as_ref()
                .is_none_or(|expected| generation.as_ref() == Some(expected));
            let overlay = context.freshness.overlay_digest.as_ref().map(|digest| {
                rightkit_context_protocol::OverlayIdentityV1 {
                    session_id: context.session_id.clone(),
                    worktree_path: context.repository_root.clone(),
                    generation_id: generation.clone().unwrap_or_default(),
                    overlay_digest: digest.clone(),
                }
            });
            for candidate in &mut output.candidates {
                if !matches!(
                    candidate.source_kind.to_ascii_lowercase().as_str(),
                    "graph" | "vector"
                ) {
                    continue;
                }
                let relative = candidate
                    .source_ref
                    .strip_prefix("doc://repo/worktree/")
                    .unwrap_or(&candidate.source_ref);
                let binding = if !generation_coherent {
                    Err("blueprint_generation_mismatch".to_owned())
                } else if let Some(owner) = ledger.as_ref() {
                    owner.bind_blueprint_source(&context, relative, &candidate.source_hash, &budget)
                } else {
                    Err("ledger_owner_unavailable".into())
                };
                match binding {
                    Ok((resolver, sha256)) => {
                        candidate.resolver = resolver.clone();
                        candidate.source_hash = sha256;
                        candidate.estimated_tokens = rightkit_memory_core::estimate_tokens(
                            &format!("{}\n{}", candidate.text, resolver),
                        )
                        .min(u32::MAX as usize)
                            as u32;
                        receipts.push(SourceResolutionReceiptV1 {
                            schema_version: 1,
                            candidate_id: candidate.id.clone(),
                            provider: "blueprint".into(),
                            status: SourceResolutionStatusV1::Resolved,
                            expected_hash: candidate.source_hash.clone(),
                            resolved_hash: Some(candidate.source_hash.clone()),
                            expected_generation: generation.clone().unwrap_or_default(),
                            resolved_generation: generation.clone(),
                            expected_path: candidate.source_ref.clone(),
                            resolved_path: Some(candidate.source_ref.clone()),
                            resolver,
                            overlay_identity: overlay.clone(),
                        });
                    }
                    Err(reason) => {
                        let detail = blueprint_source_bind_detail(&reason);
                        candidate.resolver.clear();
                        candidate.recoverable = false;
                        bind_omissions.push(rightkit_context_protocol::ProviderOmissionV1 {
                            provider: ProviderId::Blueprint,
                            reason: rightkit_context_protocol::ReasonCode::ProviderUnavailable,
                            candidate_id: Some(candidate.id.clone()),
                            detail_id: Some(detail.into()),
                            stage: Some("source_resolution".into()),
                        });
                    }
                }
            }
            output.omissions.extend(bind_omissions);
            if !receipts.is_empty() {
                output.extensions.insert(
                    "sourceResolutions".into(),
                    serde_json::to_value(receipts)
                        .map_err(|error| ProviderError::Internal(error.to_string()))?,
                );
            }
            Ok(output)
        })
    }

    fn list_capabilities(&self) -> Vec<rightkit_context_provider_sdk::CapabilityV1> {
        self.inner.list_capabilities()
    }
}

fn blueprint_source_bind_detail(reason: &str) -> &'static str {
    if reason.contains("stale") {
        "blueprint_source_stale"
    } else if reason.contains("scope")
        || reason.contains("denied")
        || reason.contains("ineligible")
        || reason.contains("erased")
    {
        "blueprint_source_scope_denied"
    } else if reason.contains("cancelled") {
        "blueprint_source_cancelled"
    } else if reason.contains("deadline") {
        "blueprint_source_deadline_exhausted"
    } else if reason.contains("generation") {
        "blueprint_source_generation_mismatch"
    } else {
        "blueprint_source_unavailable"
    }
}

fn metric_status(response: &FederationResponseV1, cancelled: bool) -> FederationMetricStatus {
    if cancelled || response.status == rightkit_context_protocol::FederationStatus::Cancelled {
        return FederationMetricStatus::Cancellation;
    }
    if response
        .diagnostics
        .as_ref()
        .and_then(|d| d.attributes.get("deadline_exhausted"))
        == Some(&"true".to_owned())
    {
        return FederationMetricStatus::Timeout;
    }
    if response.candidates.is_empty()
        && response.status == rightkit_context_protocol::FederationStatus::Complete
    {
        return FederationMetricStatus::EmptyComplete;
    }
    if response.warnings.is_empty()
        && response.omissions.is_empty()
        && !response.candidates.is_empty()
    {
        return FederationMetricStatus::Success;
    }
    if !response.candidates.is_empty() {
        return FederationMetricStatus::Partial;
    }
    response
        .warnings
        .first()
        .map(|warning| match warning.reason {
            rightkit_context_protocol::ReasonCode::ProviderUnavailable => {
                FederationMetricStatus::Unavailable
            }
            rightkit_context_protocol::ReasonCode::ProviderTimeout
            | rightkit_context_protocol::ReasonCode::DeadlineExhausted => {
                FederationMetricStatus::Timeout
            }
            rightkit_context_protocol::ReasonCode::GenerationIncoherent
            | rightkit_context_protocol::ReasonCode::ReleaseGenerationMismatch => {
                FederationMetricStatus::Incoherent
            }
            rightkit_context_protocol::ReasonCode::ScopeGrantInvalid
            | rightkit_context_protocol::ReasonCode::ScopeGrantMissing => {
                FederationMetricStatus::Unauthorized
            }
            rightkit_context_protocol::ReasonCode::ProviderMalformed => {
                FederationMetricStatus::Malformed
            }
            _ => FederationMetricStatus::Unavailable,
        })
        .unwrap_or(FederationMetricStatus::Unavailable)
}

fn error_status(error: &str, cancelled: bool) -> FederationMetricStatus {
    if cancelled || error.contains("cancel") {
        FederationMetricStatus::Cancellation
    } else if error.contains("deadline") || error.contains("timeout") {
        FederationMetricStatus::Timeout
    } else if error.contains("scope") || error.contains("unauthor") {
        FederationMetricStatus::Unauthorized
    } else if error.contains("generation") || error.contains("freshness") {
        FederationMetricStatus::Stale
    } else if error.contains("malformed") || error.contains("invalid") {
        FederationMetricStatus::Malformed
    } else {
        FederationMetricStatus::Unavailable
    }
}

fn registration(
    id: ProviderId,
    key: &str,
    provider: Arc<dyn rightkit_context_provider_sdk::Provider>,
    dependencies: Vec<ProviderId>,
) -> ProviderRegistration {
    ProviderRegistration::new(
        id,
        key,
        dependencies,
        Arc::new(BlockingProvider {
            inner: provider,
            capacity: provider_capacity(),
        }),
    )
}

/// Run owner-backed provider work on Tokio's bounded blocking pool. Native
/// providers are async at their contract boundary, but their owner adapters
/// intentionally perform synchronous filesystem, SQLite, and Blueprint work.
/// Keeping that work off scheduler workers lets deadline timers and sibling
/// lanes continue to make progress; the provider/source handles remain the
/// same runtime-owned objects.
struct BlockingProvider {
    inner: Arc<dyn Provider>,
    capacity: Arc<Semaphore>,
}

impl Provider for BlockingProvider {
    fn provide<'life0, 'life1, 'async_trait>(
        &'life0 self,
        context: &'life1 ProviderContext,
    ) -> Pin<Box<dyn Future<Output = Result<ProviderOutput, ProviderError>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        let provider = Arc::clone(&self.inner);
        let capacity = Arc::clone(&self.capacity);
        let context = context.clone();
        Box::pin(async move {
            if context.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }
            if context.is_deadline_exhausted() {
                return Err(ProviderError::DeadlineExceeded);
            }
            let permit = tokio::select! {
                permit = capacity.acquire_owned() => permit
                    .map_err(|_| ProviderError::Unavailable("provider capacity closed".into()))?,
                _ = context.cancellation.cancelled() => return Err(ProviderError::Cancelled),
                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(context.deadline)) => return Err(ProviderError::DeadlineExceeded),
            };
            tokio::task::spawn_blocking(move || {
                // A blocking lane may have queued this job after admission;
                // re-check before starting owner work so expired/cancelled
                // requests never consume provider capacity.
                if context.is_cancelled() {
                    return Err(ProviderError::Cancelled);
                }
                if context.is_deadline_exhausted() {
                    return Err(ProviderError::DeadlineExceeded);
                }
                let _permit = permit;
                tokio::runtime::Handle::current().block_on(provider.provide(&context))
            })
            .await
            .map_err(|error| {
                ProviderError::Unavailable(format!("provider blocking worker: {error}"))
            })?
        })
    }

    fn list_capabilities(&self) -> Vec<rightkit_context_provider_sdk::CapabilityV1> {
        self.inner.list_capabilities()
    }

    fn readiness(&self) -> rightkit_context_provider_sdk::provider::ProviderReadinessV1 {
        self.inner.readiness()
    }

    fn handle_operation(
        &self,
        operation: &str,
        request: &serde_json::Value,
    ) -> Result<serde_json::Value, ProviderError> {
        self.inner.handle_operation(operation, request)
    }
}

/// Audit and Architect are Legion capabilities, not context-system lanes
/// (PUL-D014). The V1 wire shape still declares both provider ids, so the
/// frozen ten-lane registry keeps a registration that reports retired.
struct RetiredLaneProvider(ProviderId);

impl Provider for RetiredLaneProvider {
    fn provide<'a, 'b, 'async_trait>(
        &'a self,
        _context: &'b ProviderContext,
    ) -> Pin<Box<dyn Future<Output = Result<ProviderOutput, ProviderError>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        let provider = self.0;
        Box::pin(async move {
            use rightkit_context_protocol::{
                ProviderOmissionV1, ProviderWarningV1, ReasonCode, WarningSeverity,
            };
            let mut output = rightkit_context_provider_sdk::empty_output(
                provider,
                FederationProviderStatusV1::Failed,
            );
            output.warnings.push(ProviderWarningV1 {
                provider,
                reason: ReasonCode::ProviderUnavailable,
                severity: WarningSeverity::Warning,
                detail_id: Some("lane_retired".into()),
                stage: Some("source".into()),
                message: None,
            });
            output.omissions.push(ProviderOmissionV1 {
                provider,
                reason: ReasonCode::ProviderUnavailable,
                candidate_id: None,
                detail_id: Some("lane_retired".into()),
                stage: Some("source".into()),
            });
            Ok(output)
        })
    }

    fn list_capabilities(&self) -> Vec<rightkit_context_provider_sdk::CapabilityV1> {
        Vec::new()
    }
}
