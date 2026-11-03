#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;

use rightkit_agent_orchestration::{
    ContextRequest, EffectPolicy, EngineOutcome, Invocation, OrchestrationEngine, RouteCandidate,
    RuntimeError,
};
use rightkit_code_audit_engine::{
    verify_binding, verify_execution, AuditError, AuditPlan, AuditProvider, ExecutionReport,
    FrozenPlan, InventoryEnvelope, InventorySource, ProviderExecutor,
};
use rightkit_code_audit_provider_sdk::{
    EffectInterface, ExternalProjectTool, ImplementationRegistry, Provider, ProviderContext,
    ProviderDefinition, ProviderError, ProviderErrorKind, ProviderRegistry,
    ProviderRegistryDocument, SourceInterface,
};
use rightkit_evidence_contracts::task::RequestEnvelope;
use rightkit_evidence_contracts::{
    AgentDefinition, AgentId, BudgetCeiling, Coverage, EffectRequest, HookActorContext,
    InvocationGrant, InvocationId, Latitude, PolicyPack, ProviderId, ProviderResult, ProviderSpec,
    ProviderStatus, ReportId, ReportStatus, ReportV1, RequestId, RoutingCeiling, TaskSpec,
    TaskStatus, ToolCeiling,
};
use rightkit_evidence_report::ReportError;
use thiserror::Error;

/// Host-owned capability catalog failure.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct CatalogHostError(pub String);

/// Host-owned capability catalog. The host application supplies its own
/// catalog type; this crate only validates it, counts entries and returns it.
pub trait CapabilityCatalog: Send + Sync {
    /// Validate the catalog as a whole.
    fn validate(&self) -> Result<(), CatalogHostError>;
    /// Number of catalog entries.
    fn entry_count(&self) -> usize;
    /// JSON projection of the catalog.
    fn to_json(&self) -> serde_json::Value;
}

/// A catalog with no entries.
#[derive(Clone, Copy, Debug, Default)]
pub struct EmptyCatalog;

impl CapabilityCatalog for EmptyCatalog {
    fn validate(&self) -> Result<(), CatalogHostError> {
        Ok(())
    }
    fn entry_count(&self) -> usize {
        0
    }
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({ "entries": [] })
    }
}

/// A catalog held as a JSON document with an `entries` array.
#[derive(Clone, Debug)]
pub struct JsonCatalog(pub serde_json::Value);

impl CapabilityCatalog for JsonCatalog {
    fn validate(&self) -> Result<(), CatalogHostError> {
        if self
            .0
            .get("entries")
            .is_some_and(serde_json::Value::is_array)
        {
            Ok(())
        } else {
            Err(CatalogHostError(
                "catalog must be an object with an entries array".into(),
            ))
        }
    }
    fn entry_count(&self) -> usize {
        self.0
            .get("entries")
            .and_then(serde_json::Value::as_array)
            .map_or(0, Vec::len)
    }
    fn to_json(&self) -> serde_json::Value {
        self.0.clone()
    }
}

/// Read-only catalog access supplied by the composition owner.
pub trait CatalogSource: Send + Sync {
    fn catalog(&self) -> Result<Arc<dyn CapabilityCatalog>, CatalogHostError>;
}

/// Identity of the host application's default agent. Declared once with
/// [`set_application_identity`]; otherwise the neutral default applies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationIdentity {
    /// Agent id of the default profile and run source.
    pub agent_id: String,
    /// Display name of the default profile.
    pub name: String,
    /// Description of the default profile.
    pub description: String,
}

impl Default for ApplicationIdentity {
    fn default() -> Self {
        Self {
            agent_id: "rightkit".to_string(),
            name: "RightKit".to_string(),
            description: "native standalone application".to_string(),
        }
    }
}

static APPLICATION_IDENTITY: std::sync::OnceLock<ApplicationIdentity> = std::sync::OnceLock::new();

/// Declare the host application's identity. The first call wins; it returns
/// `false` when an identity was already set.
pub fn set_application_identity(identity: ApplicationIdentity) -> bool {
    APPLICATION_IDENTITY.set(identity).is_ok()
}

/// The application identity in effect for this process.
pub fn application_identity() -> &'static ApplicationIdentity {
    APPLICATION_IDENTITY.get_or_init(ApplicationIdentity::default)
}

/// Read-only report access supplied by the composition owner.
pub trait ReportSource: Send + Sync {
    fn report(&self) -> Result<ReportV1, ReportError>;
}

/// Run state access supplied by the composition owner.
pub trait RunSource: Send + Sync {
    fn invocation(&self) -> Result<Invocation, RuntimeError>;

    /// Build one invocation from parsed lifecycle request data. Existing
    /// sources retain their legacy default while native CLI composition can
    /// pass the exact typed request through the same runtime context.
    fn invocation_for(&self, _request: &serde_json::Value) -> Result<Invocation, RuntimeError> {
        self.invocation()
    }
}

/// Native renderer selected by a report operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReportFormat {
    Json,
    Sarif,
    Markdown,
    Html,
}

/// Bounded work exposed by the native application seam.
#[allow(clippy::large_enum_variant)]
pub enum NativeOperation {
    /// Execute one already-authorized runtime invocation.
    Invoke(Invocation),
    /// Obtain one invocation from the injected run source, then execute it.
    Run,
    /// Obtain one invocation from the injected run source using parsed CLI
    /// lifecycle request data, then execute it.
    RunRequest { request: serde_json::Value },
    /// Validate one repository's configured inventory/catalog without running providers.
    Doctor { repository_id: String },
    /// Compile and freeze one plan from explicit provider specifications.
    Plan {
        repository_id: String,
        providers: Vec<ProviderSpec>,
        signing_key: Option<Vec<u8>>,
    },
    /// Compile, freeze, bind, execute, and verify one audit plan.
    Audit {
        repository_id: String,
        providers: Vec<ProviderSpec>,
        signing_key: Option<Vec<u8>>,
    },
    /// Execute and verify an audit over a plan the caller already compiled and
    /// froze (once per run) from the inventory it was bound to. Execution
    /// still walks a fresh inventory and refuses source drift against the plan.
    AuditFrozen {
        repository_id: String,
        plan: FrozenPlan,
        signing_key: Option<Vec<u8>>,
    },
    /// Rebuild one plan and verify its binding against current inventory.
    Verify {
        repository_id: String,
        providers: Vec<ProviderSpec>,
        signing_key: Option<Vec<u8>>,
    },
    /// Verify one CLI-supplied facts/plan pair against the configured native
    /// plan path after validating both artifacts as canonical objects.
    VerifyRequest {
        repository_id: String,
        providers: Vec<ProviderSpec>,
        signing_key: Option<Vec<u8>>,
        facts: serde_json::Value,
        plan: serde_json::Value,
    },
    /// Return the immutable catalog supplied by the catalog source.
    Catalog,
    /// Render the immutable report supplied by the report source.
    Report(ReportFormat),
}

/// Results preserve the underlying typed crate result; no empty success is synthesized.
pub enum NativeOperationResult {
    Invocation(EngineOutcome),
    Doctor {
        repository_id: String,
        inventory_digest: String,
        catalog_entries: usize,
        provider_count: usize,
    },
    Plan {
        repository_id: String,
        plan_digest: String,
        plan_signature: Option<String>,
        providers: Vec<String>,
    },
    Audit(ExecutionReport),
    Verification {
        repository_id: String,
        plan_digest: String,
        inventory_digest: String,
    },
    Catalog(Arc<dyn CapabilityCatalog>),
    Report(String),
}

#[derive(Debug, Error)]
pub enum NativeApplicationError {
    #[error("required application component is missing: {component}")]
    MissingComponent { component: &'static str },
    #[error("runtime operation failed: {0}")]
    Runtime(#[from] RuntimeError),
    #[error("audit operation failed: {0}")]
    Audit(#[from] AuditError),
    #[error("catalog operation failed: {0}")]
    Catalog(#[from] CatalogHostError),
    #[error("report operation failed: {0}")]
    Report(#[from] ReportError),
    #[error("invalid versioned application configuration: {0}")]
    Configuration(String),
    #[error("provider composition failed: {0}")]
    Provider(String),
}

/// Explicit dependency set for one in-process native application.
#[derive(Default)]
pub struct NativeApplicationConfig {
    profile: Option<rightkit_agent_orchestration::AgentProfile>,
    registry: Option<Arc<ProviderRegistry>>,
    policy: Option<Arc<dyn EffectPolicy>>,
    inventory_source: Option<Arc<dyn InventorySource>>,
    /// Repository root, when known, threaded into `AuditPlan::compile_with_root`
    /// so conditional-lens providers can be gated on their deterministic
    /// trigger. `None` falls back to `AuditPlan::compile`'s no-gating behavior.
    root: Option<PathBuf>,
    provider_executor: Option<Arc<dyn ProviderExecutor>>,
    catalog_source: Option<Arc<dyn CatalogSource>>,
    report_source: Option<Arc<dyn ReportSource>>,
    run_source: Option<Arc<dyn RunSource>>,
    external_project_tool: Option<Arc<dyn ExternalProjectTool>>,
    provider_specs: Vec<ProviderSpec>,
}

impl NativeApplicationConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// Compose native audit, policy, catalog, and report dependencies from one
    /// schema-versioned JSON document. RunSource remains optional and is
    /// required only by the separate `Run` operation.
    pub fn from_versioned_json(input: &str) -> Result<Self, NativeApplicationError> {
        let document: VersionedApplicationConfig = serde_json::from_str(input)
            .map_err(|error| NativeApplicationError::Configuration(error.to_string()))?;
        document.into_runtime_config()
    }

    /// Load a versioned configuration from inline JSON or an explicit file
    /// path. Installed adapters use the file form so packaging never depends
    /// on shell-sized environment payloads or source-tree symlinks.
    pub fn from_versioned_source(source: &str) -> Result<Self, NativeApplicationError> {
        if source.trim_start().starts_with('{') {
            return Self::from_versioned_json(source);
        }
        let input = std::fs::read_to_string(source).map_err(|error| {
            NativeApplicationError::Configuration(format!(
                "could not read native application config {source}: {error}"
            ))
        })?;
        Self::from_versioned_json(&input)
    }

    /// Build the installed CLI's safe default composition. The default run is
    /// a real bounded native invocation backed by one deterministic provider;
    /// no external process, Node runtime, or Python runtime is required.
    pub fn default_for_repository(
        repository_id: impl Into<String>,
    ) -> Result<NativeApplication, NativeApplicationError> {
        let repository_id = repository_id.into();
        let profile = rightkit_agent_orchestration::AgentProfile::new(
            AgentDefinition::new(
                AgentId::new(application_identity().agent_id.as_str())
                    .map_err(|error| NativeApplicationError::Configuration(error.to_string()))?,
                application_identity().name.as_str(),
                application_identity().description.as_str(),
                BudgetCeiling {
                    max_active_time_ms: 30_000,
                    max_cost_micros: 1,
                    max_output_bytes: 1_024,
                },
                ToolCeiling::default(),
                RoutingCeiling::default(),
            )
            .map_err(|error| NativeApplicationError::Configuration(error.to_string()))?,
        )
        .map_err(|error| NativeApplicationError::Configuration(error.to_string()))?;
        let provider_id = ProviderId::new("native-default")
            .map_err(|error| NativeApplicationError::Configuration(error.to_string()))?;
        let default_result = ProviderResult {
            schema_version: 1,
            provider: provider_id.clone(),
            applicable: true,
            required: true,
            status: ProviderStatus::Complete,
            complete: true,
            coverage: Some(Coverage {
                denominator_digest: "native-default".into(),
                expected: 1,
                examined: 1,
                gaps: Vec::new(),
            }),
            findings: Vec::new(),
            coverage_gaps: Vec::new(),
            degradation: Vec::new(),
            details: BTreeMap::new(),
        };
        let provider_definition = ProviderDefinition {
            schema_version: 1,
            id: provider_id.clone(),
            provider_version: "1".into(),
            implementation_key: "native-default".into(),
            capabilities: Vec::new(),
            depends_on: Vec::new(),
            required: true,
            permissions: Vec::new(),
            source_provenance: BTreeMap::new(),
        };
        let mut implementations = ImplementationRegistry::new();
        let configured_result = default_result.clone();
        implementations
            .register("native-default", "1", move |definition| {
                Ok(Arc::new(ConfiguredProvider {
                    definition: definition.clone(),
                    result: configured_result.clone(),
                }) as Arc<dyn Provider>)
            })
            .map_err(|error| NativeApplicationError::Provider(error.to_string()))?;
        let registry = ProviderRegistry::load(
            ProviderRegistryDocument {
                schema_version: 1,
                providers: vec![provider_definition],
            },
            &implementations,
        )
        .map_err(|error| NativeApplicationError::Provider(error.to_string()))?;
        let inventory =
            InventoryEnvelope::new(repository_id.clone(), "native-default", Vec::new())?;
        let catalog: Arc<dyn CapabilityCatalog> = Arc::new(EmptyCatalog);
        let report = ReportV1 {
            schema_version: 1,
            report_id: ReportId::new("native-default")
                .map_err(|error| NativeApplicationError::Configuration(error.to_string()))?,
            status: ReportStatus::Incomplete,
            findings: Vec::new(),
            gaps: Vec::new(),
            claims: BTreeMap::new(),
            targets: Vec::new(),
            extensions: BTreeMap::new(),
        };
        NativeApplicationConfig::new()
            .with_profile(profile)
            .with_registry(Arc::new(registry))
            .with_policy(Arc::new(CanonicalEffectPolicy {
                pack: {
                    let pack = rightkit_evidence_contracts::canonical_default_policy_pack();
                    pack.validate().map_err(|error| {
                        NativeApplicationError::Configuration(format!(
                            "canonical default policy pack failed validation: {error}"
                        ))
                    })?;
                    pack
                },
            }))
            .with_inventory_source(Arc::new(StaticInventorySource {
                snapshots: vec![inventory],
            }))
            .with_provider_executor(Arc::new(StaticProviderExecutor {
                results: [(provider_id.to_string(), default_result.clone())]
                    .into_iter()
                    .collect(),
            }))
            .with_catalog_source(Arc::new(StaticCatalogSource { catalog }))
            .with_report_source(Arc::new(StaticReportSource { report }))
            .with_run_source(Arc::new(DefaultRunSource {
                repository_id,
                agent_id: AgentId::new(application_identity().agent_id.as_str())
                    .map_err(|error| NativeApplicationError::Configuration(error.to_string()))?,
                provider_id,
                budget: BudgetCeiling {
                    max_active_time_ms: 30_000,
                    max_cost_micros: 1,
                    max_output_bytes: 1_024,
                },
            }))
            .build()
    }

    /// Compose one standalone Audit from an inventory source, an exact selected
    /// provider plan, and typed host-injected results. Inventory comes solely
    /// from Audit's own read-only filesystem walk (`FilesystemInventorySource`)
    /// or an injected fixture; there is no external context-engine dependency.
    /// `root`, when supplied, gates conditional-lens providers on their
    /// deterministic trigger (see `AuditPlan::compile_with_root`).
    pub fn for_audit_artifacts(
        repository_id: impl Into<String>,
        inventory_source: Arc<dyn InventorySource>,
        provider_specs: Vec<ProviderSpec>,
        provider_results: Vec<ProviderResult>,
        root: Option<PathBuf>,
    ) -> Result<NativeApplication, NativeApplicationError> {
        let repository_id = repository_id.into();
        let inventory = inventory_source.inventory(&repository_id)?;
        if provider_specs.is_empty() {
            return Err(NativeApplicationError::Configuration(
                "standalone Audit requires selected provider specifications".into(),
            ));
        }
        let frozen_plan =
            AuditPlan::compile_with_root(root.as_deref(), &inventory, &provider_specs)
                .map_err(|error| NativeApplicationError::Configuration(error.to_string()))?;
        let mut results = BTreeMap::new();
        for result in provider_results {
            result
                .validate()
                .map_err(|error| NativeApplicationError::Configuration(error.to_string()))?;
            if result.complete {
                let provider = frozen_plan
                    .providers
                    .iter()
                    .find(|provider| provider.id == result.provider.to_string())
                    .ok_or_else(|| {
                        NativeApplicationError::Configuration(format!(
                            "complete provider result {} is not in frozen plan",
                            result.provider
                        ))
                    })?;
                let expected_digest = provider
                    .configuration
                    .get("denominatorDigest")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        NativeApplicationError::Configuration(format!(
                            "provider {} is missing frozen denominator",
                            provider.id
                        ))
                    })?;
                let expected_count = provider
                    .configuration
                    .get("denominatorCount")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| {
                        NativeApplicationError::Configuration(format!(
                            "provider {} is missing frozen denominator count",
                            provider.id
                        ))
                    })?;
                let coverage = result.coverage.as_ref().ok_or_else(|| {
                    NativeApplicationError::Configuration(format!(
                        "complete provider result {} is missing coverage",
                        result.provider
                    ))
                })?;
                if coverage.denominator_digest != expected_digest
                    || coverage.expected != expected_count
                    || coverage.examined != coverage.expected
                {
                    return Err(NativeApplicationError::Configuration(format!(
                        "complete provider result {} is not bound to frozen selector denominator",
                        result.provider
                    )));
                }
            }
            let provider_id = result.provider.to_string();
            if results.insert(provider_id.clone(), result).is_some() {
                return Err(NativeApplicationError::Configuration(format!(
                    "duplicate provider result: {provider_id}"
                )));
            }
        }
        let planned = provider_specs
            .iter()
            .map(|specification| specification.id.to_string())
            .collect::<BTreeSet<_>>();
        let supplied = results.keys().cloned().collect::<BTreeSet<_>>();
        if planned != supplied {
            return Err(NativeApplicationError::Configuration(
                "selected provider specifications and supplied results do not reconcile".into(),
            ));
        }
        for specification in &provider_specs {
            specification
                .validate()
                .map_err(|error| NativeApplicationError::Configuration(error.to_string()))?;
        }

        let profile_definition = AgentDefinition::new(
            AgentId::new(application_identity().agent_id.as_str())
                .map_err(|error| NativeApplicationError::Configuration(error.to_string()))?,
            application_identity().name.as_str(),
            "native standalone Audit",
            BudgetCeiling {
                max_active_time_ms: 300_000,
                max_cost_micros: 1,
                max_output_bytes: 64 * 1024 * 1024,
            },
            ToolCeiling::default(),
            RoutingCeiling::default(),
        )
        .map_err(|error| NativeApplicationError::Configuration(error.to_string()))?;
        let profile = rightkit_agent_orchestration::AgentProfile::new(profile_definition)
            .map_err(|error| NativeApplicationError::Configuration(error.to_string()))?;
        let definitions = provider_specs
            .iter()
            .map(|specification| ProviderDefinition {
                schema_version: 1,
                id: specification.id.clone(),
                provider_version: specification.provider_version.clone(),
                implementation_key: "host-injected-audit-result".into(),
                capabilities: specification.control_ids.clone(),
                depends_on: specification.depends_on.clone(),
                required: true,
                permissions: Vec::new(),
                source_provenance: BTreeMap::from([(
                    "kind".into(),
                    "host-injected-audit-result".into(),
                )]),
            })
            .collect::<Vec<_>>();
        let mut implementations = ImplementationRegistry::new();
        let configured_results = results.clone();
        implementations
            .register("host-injected-audit-result", "*", move |definition| {
                let result = configured_results
                    .get(&definition.id.to_string())
                    .cloned()
                    .ok_or_else(|| {
                        ProviderError::new(
                            ProviderErrorKind::MissingTool,
                            format!("no host-injected result for provider {}", definition.id),
                        )
                    })?;
                Ok(Arc::new(ConfiguredProvider {
                    definition: definition.clone(),
                    result,
                }) as Arc<dyn Provider>)
            })
            .map_err(|error| NativeApplicationError::Provider(error.to_string()))?;
        let registry = ProviderRegistry::load(
            ProviderRegistryDocument {
                schema_version: 1,
                providers: definitions,
            },
            &implementations,
        )
        .map_err(|error| NativeApplicationError::Provider(error.to_string()))?;
        let catalog: Arc<dyn CapabilityCatalog> = Arc::new(EmptyCatalog);
        let report = ReportV1 {
            schema_version: 1,
            report_id: ReportId::new("native-audit")
                .map_err(|error| NativeApplicationError::Configuration(error.to_string()))?,
            status: ReportStatus::Incomplete,
            findings: Vec::new(),
            gaps: vec!["not-executed".into()],
            claims: BTreeMap::new(),
            targets: vec![repository_id],
            extensions: BTreeMap::new(),
        };
        let mut config = NativeApplicationConfig::new()
            .with_profile(profile)
            .with_registry(Arc::new(registry))
            .with_policy(Arc::new(CanonicalEffectPolicy {
                pack: PolicyPack {
                    schema_version: 1,
                    id: "native-audit".into(),
                    version: 1,
                    rules: Vec::new(),
                    extensions: BTreeMap::new(),
                },
            }))
            .with_inventory_source(inventory_source)
            .with_provider_executor(Arc::new(StaticProviderExecutor { results }))
            .with_catalog_source(Arc::new(StaticCatalogSource { catalog }))
            .with_report_source(Arc::new(StaticReportSource { report }))
            .with_provider_specs(provider_specs);
        if let Some(root) = root {
            config = config.with_root(root);
        }
        config.build()
    }

    /// Compose standalone Audit from a real in-process ProviderExecutor.
    /// Provider specifications remain frozen inputs; execution is delegated to
    /// one concrete native implementation rather than precomputed results.
    pub fn for_audit_executor(
        repository_id: impl Into<String>,
        inventory_source: Arc<dyn InventorySource>,
        provider_specs: Vec<ProviderSpec>,
        provider_executor: Arc<dyn ProviderExecutor>,
        root: Option<PathBuf>,
    ) -> Result<NativeApplication, NativeApplicationError> {
        let repository_id = repository_id.into();
        let inventory = inventory_source.inventory(&repository_id)?;
        if provider_specs.is_empty() {
            return Err(NativeApplicationError::Configuration(
                "standalone Audit requires selected provider specifications".into(),
            ));
        }
        AuditPlan::compile_with_root(root.as_deref(), &inventory, &provider_specs)
            .map_err(|error| NativeApplicationError::Configuration(error.to_string()))?;
        for specification in &provider_specs {
            specification
                .validate()
                .map_err(|error| NativeApplicationError::Configuration(error.to_string()))?;
        }

        let profile = rightkit_agent_orchestration::AgentProfile::new(
            AgentDefinition::new(
                AgentId::new(application_identity().agent_id.as_str())
                    .map_err(|error| NativeApplicationError::Configuration(error.to_string()))?,
                application_identity().name.as_str(),
                "native standalone Audit",
                BudgetCeiling {
                    max_active_time_ms: 300_000,
                    max_cost_micros: 1,
                    max_output_bytes: 64 * 1024 * 1024,
                },
                ToolCeiling::default(),
                RoutingCeiling::default(),
            )
            .map_err(|error| NativeApplicationError::Configuration(error.to_string()))?,
        )
        .map_err(|error| NativeApplicationError::Configuration(error.to_string()))?;

        let definitions = provider_specs
            .iter()
            .map(|specification| ProviderDefinition {
                schema_version: 1,
                id: specification.id.clone(),
                provider_version: specification.provider_version.clone(),
                implementation_key: "native-audit-executor".into(),
                capabilities: specification.control_ids.clone(),
                depends_on: specification.depends_on.clone(),
                required: specification
                    .execution
                    .get("required")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true),
                permissions: Vec::new(),
                source_provenance: BTreeMap::from([(
                    "kind".into(),
                    "native-audit-executor".into(),
                )]),
            })
            .collect::<Vec<_>>();
        let mut implementations = ImplementationRegistry::new();
        implementations
            .register("native-audit-executor", "*", move |definition| {
                let result = ProviderResult {
                    schema_version: 1,
                    provider: definition.id.clone(),
                    applicable: true,
                    required: definition.required,
                    status: ProviderStatus::Partial,
                    complete: false,
                    coverage: None,
                    findings: Vec::new(),
                    coverage_gaps: vec!["provider-executes-through-audit-boundary".into()],
                    degradation: Vec::new(),
                    details: BTreeMap::new(),
                };
                Ok(Arc::new(ConfiguredProvider {
                    definition: definition.clone(),
                    result,
                }) as Arc<dyn Provider>)
            })
            .map_err(|error| NativeApplicationError::Provider(error.to_string()))?;
        let registry = ProviderRegistry::load(
            ProviderRegistryDocument {
                schema_version: 1,
                providers: definitions,
            },
            &implementations,
        )
        .map_err(|error| NativeApplicationError::Provider(error.to_string()))?;
        let catalog: Arc<dyn CapabilityCatalog> = Arc::new(EmptyCatalog);
        let report = ReportV1 {
            schema_version: 1,
            report_id: ReportId::new("native-audit")
                .map_err(|error| NativeApplicationError::Configuration(error.to_string()))?,
            status: ReportStatus::Incomplete,
            findings: Vec::new(),
            gaps: vec!["not-executed".into()],
            claims: BTreeMap::new(),
            targets: vec![repository_id],
            extensions: BTreeMap::new(),
        };
        let mut config = NativeApplicationConfig::new()
            .with_profile(profile)
            .with_registry(Arc::new(registry))
            .with_policy(Arc::new(CanonicalEffectPolicy {
                pack: PolicyPack {
                    schema_version: 1,
                    id: "native-audit".into(),
                    version: 1,
                    rules: Vec::new(),
                    extensions: BTreeMap::new(),
                },
            }))
            .with_inventory_source(inventory_source)
            .with_provider_executor(provider_executor)
            .with_catalog_source(Arc::new(StaticCatalogSource { catalog }))
            .with_report_source(Arc::new(StaticReportSource { report }))
            .with_provider_specs(provider_specs);
        if let Some(root) = root {
            config = config.with_root(root);
        }
        config.build()
    }

    pub fn with_profile(mut self, profile: rightkit_agent_orchestration::AgentProfile) -> Self {
        self.profile = Some(profile);
        self
    }

    pub fn with_registry(mut self, registry: Arc<ProviderRegistry>) -> Self {
        self.registry = Some(registry);
        self
    }

    pub fn with_policy(mut self, policy: Arc<dyn EffectPolicy>) -> Self {
        self.policy = Some(policy);
        self
    }

    pub fn with_inventory_source(mut self, source: Arc<dyn InventorySource>) -> Self {
        self.inventory_source = Some(source);
        self
    }

    pub fn with_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.root = Some(root.into());
        self
    }

    pub fn with_provider_executor(mut self, executor: Arc<dyn ProviderExecutor>) -> Self {
        self.provider_executor = Some(executor);
        self
    }

    pub fn with_catalog_source(mut self, source: Arc<dyn CatalogSource>) -> Self {
        self.catalog_source = Some(source);
        self
    }

    pub fn with_report_source(mut self, source: Arc<dyn ReportSource>) -> Self {
        self.report_source = Some(source);
        self
    }

    pub fn with_run_source(mut self, source: Arc<dyn RunSource>) -> Self {
        self.run_source = Some(source);
        self
    }

    pub fn with_external_project_tool(mut self, tool: Arc<dyn ExternalProjectTool>) -> Self {
        self.external_project_tool = Some(tool);
        self
    }

    pub fn with_provider_specs(mut self, providers: Vec<ProviderSpec>) -> Self {
        self.provider_specs = providers;
        self
    }

    pub fn build(self) -> Result<NativeApplication, NativeApplicationError> {
        let profile = self
            .profile
            .ok_or(NativeApplicationError::MissingComponent {
                component: "AgentProfile",
            })?;
        let registry = self
            .registry
            .ok_or(NativeApplicationError::MissingComponent {
                component: "ProviderRegistry",
            })?;
        let policy = self
            .policy
            .ok_or(NativeApplicationError::MissingComponent {
                component: "EffectPolicy",
            })?;
        let inventory_source =
            self.inventory_source
                .ok_or(NativeApplicationError::MissingComponent {
                    component: "InventorySource",
                })?;
        let provider_executor =
            self.provider_executor
                .ok_or(NativeApplicationError::MissingComponent {
                    component: "ProviderExecutor",
                })?;
        let catalog_source =
            self.catalog_source
                .ok_or(NativeApplicationError::MissingComponent {
                    component: "CatalogSource",
                })?;
        let report_source = self
            .report_source
            .ok_or(NativeApplicationError::MissingComponent {
                component: "ReportSource",
            })?;
        Ok(NativeApplication {
            engine: OrchestrationEngine::new(profile, registry).with_policy(policy),
            inventory_source,
            root: self.root,
            provider_executor,
            catalog_source,
            report_source,
            run_source: self.run_source,
            external_project_tool: self.external_project_tool,
            provider_specs: self.provider_specs,
        })
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VersionedApplicationConfig {
    schema_version: u32,
    profile: AgentDefinition,
    policy: PolicyPack,
    provider_specs: Vec<ProviderSpec>,
    providers: Vec<ConfiguredProviderDocument>,
    #[serde(default)]
    inventory: Vec<InventoryEnvelope>,
    /// Repository root, when known, threaded into `AuditPlan::compile_with_root`
    /// for conditional-lens trigger gating. Optional and additive.
    #[serde(default)]
    root: Option<String>,
    catalog: serde_json::Value,
    report: ReportV1,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConfiguredProviderDocument {
    definition: ProviderDefinition,
    result: ProviderResult,
}

impl VersionedApplicationConfig {
    fn into_runtime_config(self) -> Result<NativeApplicationConfig, NativeApplicationError> {
        // Bumped from schema 1: the Blueprint packet inventory source
        // (`blueprintPacketPath`/`blueprintExpectedGeneration`) was removed.
        // The application has no dependency on an external context engine;
        // `inventory` (the static fixture source) is now the only accepted
        // inventory input. A schema-1 document with those fields now fails
        // this version check rather than being silently reinterpreted.
        if self.schema_version != 2 {
            return Err(NativeApplicationError::Configuration(format!(
                "unsupported application schema version {}",
                self.schema_version
            )));
        }
        self.policy
            .validate()
            .map_err(|error| NativeApplicationError::Configuration(error.to_string()))?;
        let catalog = JsonCatalog(self.catalog);
        catalog
            .validate()
            .map_err(|error| NativeApplicationError::Configuration(error.to_string()))?;
        self.report
            .validate()
            .map_err(|error| NativeApplicationError::Configuration(error.to_string()))?;
        if self.inventory.is_empty() {
            return Err(NativeApplicationError::Configuration(
                "configure at least one static inventory snapshot".into(),
            ));
        }
        for inventory in &self.inventory {
            inventory
                .validate()
                .map_err(|error| NativeApplicationError::Configuration(error.to_string()))?;
        }
        for provider in &self.provider_specs {
            provider
                .validate()
                .map_err(|error| NativeApplicationError::Configuration(error.to_string()))?;
        }

        let mut implementations = ImplementationRegistry::new();
        let mut definitions = Vec::with_capacity(self.providers.len());
        let mut registered_keys = BTreeSet::new();
        let configured_results: BTreeMap<_, _> = self
            .providers
            .iter()
            .map(|provider| (provider.definition.id.clone(), provider.result.clone()))
            .collect();
        for configured in &self.providers {
            if configured.definition.id != configured.result.provider {
                return Err(NativeApplicationError::Configuration(format!(
                    "provider result identity does not match {}",
                    configured.definition.id
                )));
            }
            let key = configured.definition.implementation_key.clone();
            let version = configured.definition.provider_version.clone();
            if registered_keys.insert(key.clone()) {
                let configured_results = configured_results.clone();
                implementations
                    .register(key, version, move |definition| {
                        let result =
                            configured_results
                                .get(&definition.id)
                                .cloned()
                                .ok_or_else(|| {
                                    ProviderError::new(
                                        ProviderErrorKind::MissingTool,
                                        format!(
                                            "no configured result for provider {}",
                                            definition.id
                                        ),
                                    )
                                })?;
                        Ok(Arc::new(ConfiguredProvider {
                            definition: definition.clone(),
                            result,
                        }) as Arc<dyn Provider>)
                    })
                    .map_err(|error| NativeApplicationError::Provider(error.to_string()))?;
            }
            definitions.push(configured.definition.clone());
        }
        let registry = ProviderRegistry::load(
            ProviderRegistryDocument {
                schema_version: 1,
                providers: definitions,
            },
            &implementations,
        )
        .map_err(|error| NativeApplicationError::Provider(error.to_string()))?;
        let profile_id = self.profile.id.clone();
        let profile_budget = self.profile.budget.clone();
        let configured_repository = self
            .inventory
            .first()
            .map(|inventory| inventory.repository_id.clone())
            .unwrap_or_else(|| ".".into());
        let configured_provider = self
            .providers
            .first()
            .map(|provider| provider.definition.id.clone());
        let profile = rightkit_agent_orchestration::AgentProfile::new(self.profile)
            .map_err(|error| NativeApplicationError::Configuration(error.to_string()))?;
        let inventory_source: Arc<dyn InventorySource> = Arc::new(StaticInventorySource {
            snapshots: self.inventory,
        });
        let results = self
            .providers
            .iter()
            .map(|provider| (provider.definition.id.to_string(), provider.result.clone()))
            .collect();
        let executor = StaticProviderExecutor { results };
        let mut config = NativeApplicationConfig::new()
            .with_profile(profile)
            .with_registry(Arc::new(registry))
            .with_policy(Arc::new(CanonicalEffectPolicy { pack: self.policy }))
            .with_inventory_source(inventory_source)
            .with_provider_executor(Arc::new(executor))
            .with_catalog_source(Arc::new(StaticCatalogSource {
                catalog: Arc::new(catalog),
            }))
            .with_report_source(Arc::new(StaticReportSource {
                report: self.report,
            }))
            .with_provider_specs(self.provider_specs);
        if let Some(root) = self.root {
            config = config.with_root(root);
        }
        if let Some(provider_id) = configured_provider {
            config = config.with_run_source(Arc::new(DefaultRunSource {
                repository_id: configured_repository,
                agent_id: profile_id,
                provider_id,
                budget: profile_budget,
            }));
        }
        Ok(config)
    }
}

fn validate_verify_artifacts(
    facts: &serde_json::Value,
    plan: &serde_json::Value,
    providers: &[ProviderSpec],
) -> Result<(), NativeApplicationError> {
    if facts.get("kind").and_then(serde_json::Value::as_str) != Some("audit-facts") {
        return Err(NativeApplicationError::Configuration(
            "verify facts artifact kind must be audit-facts".into(),
        ));
    }
    if !facts.get("checks").is_some_and(serde_json::Value::is_array)
        || !facts
            .get("provider_reconciliation")
            .and_then(serde_json::Value::as_object)
            .and_then(|reconciliation| reconciliation.get("providerResults"))
            .is_some_and(serde_json::Value::is_array)
    {
        return Err(NativeApplicationError::Configuration(
            "verify facts must contain checks and provider reconciliation results".into(),
        ));
    }
    if plan
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64)
        != Some(1)
        || plan.get("kind").and_then(serde_json::Value::as_str) != Some("audit-provider-plan")
    {
        return Err(NativeApplicationError::Configuration(
            "verify plan artifact must be schemaVersion 1 audit-provider-plan".into(),
        ));
    }
    let plan_providers = plan
        .get("providers")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            NativeApplicationError::Configuration(
                "verify plan artifact providers must be an array".into(),
            )
        })?;
    let mut supplied_provider_ids = plan_providers
        .iter()
        .map(|provider| {
            provider.as_str().map(str::to_owned).ok_or_else(|| {
                NativeApplicationError::Configuration(
                    "verify plan provider entries must be strings".into(),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    supplied_provider_ids.sort();
    supplied_provider_ids.dedup();
    let mut configured_provider_ids = providers
        .iter()
        .map(|provider| provider.id.to_string())
        .collect::<Vec<_>>();
    configured_provider_ids.sort();
    configured_provider_ids.dedup();
    if supplied_provider_ids != configured_provider_ids {
        return Err(NativeApplicationError::Configuration(
            "verify plan providers do not match configured native providers".into(),
        ));
    }
    let plan_seal = plan
        .get("seal")
        .and_then(serde_json::Value::as_object)
        .and_then(|seal| seal.get("digest"))
        .and_then(serde_json::Value::as_str)
        .filter(|digest| digest.starts_with("sha256:") && digest.len() == 71)
        .ok_or_else(|| {
            NativeApplicationError::Configuration(
                "verify plan seal must contain a canonical sha256 digest".into(),
            )
        })?;
    let plan_binding = plan
        .get("binding")
        .and_then(serde_json::Value::as_object)
        .and_then(|binding| binding.get("repositoryRevision"))
        .and_then(serde_json::Value::as_str)
        .filter(|revision| !revision.trim().is_empty())
        .ok_or_else(|| {
            NativeApplicationError::Configuration(
                "verify plan binding must contain repositoryRevision".into(),
            )
        })?;
    let facts_plan = facts
        .get("plan")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            NativeApplicationError::Configuration(
                "verify facts must embed the supplied plan binding".into(),
            )
        })?;
    let facts_seal = facts_plan
        .get("seal")
        .and_then(serde_json::Value::as_object)
        .and_then(|seal| seal.get("digest"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            NativeApplicationError::Configuration(
                "verify facts embedded plan must contain seal digest".into(),
            )
        })?;
    let facts_revision = facts_plan
        .get("binding")
        .and_then(serde_json::Value::as_object)
        .and_then(|binding| binding.get("repositoryRevision"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            NativeApplicationError::Configuration(
                "verify facts embedded plan must contain repositoryRevision".into(),
            )
        })?;
    if facts_seal != plan_seal || facts_revision != plan_binding {
        return Err(NativeApplicationError::Configuration(
            "verify facts and plan bindings do not match".into(),
        ));
    }
    rightkit_evidence_contracts::canonical_digest_hex(facts)
        .map_err(|error| NativeApplicationError::Configuration(error.to_string()))?;
    rightkit_evidence_contracts::canonical_digest_hex(plan)
        .map_err(|error| NativeApplicationError::Configuration(error.to_string()))?;
    Ok(())
}

#[derive(Clone)]
struct StaticInventorySource {
    snapshots: Vec<InventoryEnvelope>,
}

impl InventorySource for StaticInventorySource {
    fn inventory(&self, repository_id: &str) -> Result<InventoryEnvelope, AuditError> {
        self.snapshots
            .iter()
            .find(|snapshot| snapshot.repository_id == repository_id)
            .cloned()
            .ok_or_else(|| {
                AuditError::SourceDrift(format!(
                    "no configured inventory for repository {repository_id}"
                ))
            })
    }
}

#[derive(Clone)]
struct StaticProviderExecutor {
    results: BTreeMap<String, ProviderResult>,
}

impl ProviderExecutor for StaticProviderExecutor {
    fn execute(
        &self,
        provider: &AuditProvider,
        _: &InventoryEnvelope,
    ) -> Result<ProviderResult, AuditError> {
        self.results.get(&provider.id).cloned().ok_or_else(|| {
            AuditError::Provider(format!("no configured result for provider {}", provider.id))
        })
    }
}

#[derive(Clone)]
struct StaticCatalogSource {
    catalog: Arc<dyn CapabilityCatalog>,
}
impl CatalogSource for StaticCatalogSource {
    fn catalog(&self) -> Result<Arc<dyn CapabilityCatalog>, CatalogHostError> {
        Ok(Arc::clone(&self.catalog))
    }
}

#[derive(Clone)]
struct StaticReportSource {
    report: ReportV1,
}
impl ReportSource for StaticReportSource {
    fn report(&self) -> Result<ReportV1, ReportError> {
        Ok(self.report.clone())
    }
}

#[derive(Clone)]
struct DefaultRunSource {
    repository_id: String,
    agent_id: AgentId,
    provider_id: ProviderId,
    budget: BudgetCeiling,
}

impl RunSource for DefaultRunSource {
    fn invocation(&self) -> Result<Invocation, RuntimeError> {
        self.invocation_for(&serde_json::json!({}))
    }

    fn invocation_for(&self, request: &serde_json::Value) -> Result<Invocation, RuntimeError> {
        let agent_id = self.agent_id.clone();
        let request_id = RequestId::new("native-run")
            .map_err(|error| RuntimeError::InvalidTask(error.to_string()))?;
        let task_id = "native-run"
            .parse::<rightkit_evidence_contracts::TaskId>()
            .map_err(|error| RuntimeError::InvalidTask(error.to_string()))?;
        let invocation_id = InvocationId::new("native-run")
            .map_err(|error| RuntimeError::InvalidTask(error.to_string()))?;
        let grant = InvocationGrant::new(agent_id.clone(), task_id.clone(), self.budget.clone())
            .map_err(|error| RuntimeError::GrantExceedsCeiling(error.to_string()))?;
        let task = TaskSpec {
            schema_version: 1,
            task_id: task_id.clone(),
            request_id: request_id.clone(),
            title: format!("native {} run", application_identity().name),
            description: Some("bounded native CLI run request".into()),
            own_scope: vec![self.repository_id.clone()],
            read_scope: vec![self.repository_id.clone()],
            depends_on: Vec::new(),
            implements_decisions: Vec::new(),
            latitude: Latitude::Bounded,
            declared_checks: vec!["native-provider".into()],
            evidence_requirements: vec!["provider-result".into()],
            status: TaskStatus::Complete,
            assigned_authority: agent_id,
        };
        let cli_fields: Vec<_> = std::env::args()
            .skip_while(|argument| argument != "run")
            .skip(1)
            .collect();
        let envelope = RequestEnvelope {
            schema_version: 1,
            request_id,
            task_id: Some(task_id),
            payload: serde_json::json!({
                "command": "run",
                "repository": &self.repository_id,
                "provider": &self.provider_id,
                "request": request,
                "cliFields": cli_fields,
            }),
            extensions: BTreeMap::new(),
        };
        let context = ContextRequest::new(
            envelope,
            self.repository_id.clone(),
            0,
            Instant::now() + Duration::from_millis(self.budget.max_active_time_ms),
            tokio_util::sync::CancellationToken::new(),
            Arc::new(DefaultSourceInterface),
            Arc::new(DefaultEffectInterface),
        )?;
        Ok(Invocation {
            invocation_id,
            task,
            grant,
            context,
            routes: vec![RouteCandidate {
                id: "native-default".into(),
                providers: vec![self.provider_id.clone()],
                required_capabilities: Vec::new(),
                worst_case_cost_micros: 1,
            }],
        })
    }
}

struct DefaultSourceInterface;

impl SourceInterface for DefaultSourceInterface {
    fn read(
        &self,
        _source: &str,
        _query: &serde_json::Value,
    ) -> Result<serde_json::Value, ProviderError> {
        Err(ProviderError::new(
            ProviderErrorKind::MissingTool,
            "default run has no external source access",
        ))
    }
}

struct DefaultEffectInterface;

impl EffectInterface for DefaultEffectInterface {
    fn request(&self, _effect: &EffectRequest) -> Result<serde_json::Value, ProviderError> {
        Err(ProviderError::new(
            ProviderErrorKind::PolicyDenied,
            "default run has no effect authority",
        ))
    }
}

struct CanonicalEffectPolicy {
    pack: PolicyPack,
}
impl EffectPolicy for CanonicalEffectPolicy {
    fn authorize(&self, request: &EffectRequest) -> Result<(), RuntimeError> {
        let matching: Vec<_> = self
            .pack
            .rules
            .iter()
            .filter(|rule| {
                rule.effect_class == request.effect_class
                    && rule
                        .targets
                        .iter()
                        .any(|target| target == "*" || target == &request.target)
                    && rule
                        .operations
                        .iter()
                        .any(|operation| operation == "*" || operation == &request.operation)
            })
            .collect();
        if matching.iter().any(|rule| !rule.allowed) {
            return Err(RuntimeError::Policy(
                "canonical policy denied effect".into(),
            ));
        }
        let Some(rule) = matching.first() else {
            return Err(RuntimeError::Policy(
                "canonical policy has no matching rule".into(),
            ));
        };
        if !matches!(
            rule.approval,
            rightkit_evidence_contracts::policy::ApprovalRequirement::None
        ) || request.approval_required
        {
            return Err(RuntimeError::Policy(
                "canonical policy requires an approval boundary".into(),
            ));
        }
        if rule.required_trust.is_some() || !rule.required_enforcement.is_empty() {
            return Err(RuntimeError::Policy(
                "canonical policy trust/enforcement inputs are not present".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone)]
struct ConfiguredProvider {
    definition: ProviderDefinition,
    result: ProviderResult,
}

#[async_trait]
impl Provider for ConfiguredProvider {
    fn definition(&self) -> &ProviderDefinition {
        &self.definition
    }
    async fn execute(&self, _: &ProviderContext) -> Result<ProviderResult, ProviderError> {
        Ok(self.result.clone())
    }
}

/// In-process composition root. Policy and provider state live in OrchestrationEngine only.
pub struct NativeApplication {
    engine: OrchestrationEngine,
    inventory_source: Arc<dyn InventorySource>,
    root: Option<PathBuf>,
    provider_executor: Arc<dyn ProviderExecutor>,
    catalog_source: Arc<dyn CatalogSource>,
    report_source: Arc<dyn ReportSource>,
    run_source: Option<Arc<dyn RunSource>>,
    external_project_tool: Option<Arc<dyn ExternalProjectTool>>,
    provider_specs: Vec<ProviderSpec>,
}

impl NativeApplication {
    /// Authorize one typed effect through the injected policy boundary.
    pub fn authorize_hook(
        &self,
        request: &EffectRequest,
        actor: HookActorContext,
    ) -> Result<(), NativeApplicationError> {
        if !actor.permits_effect(request.effect_class) {
            return Err(RuntimeError::Policy(
                "authenticated Sage/Oracle actor is restricted to positively identified read-only effects".into(),
            )
            .into());
        }
        self.engine
            .authorize_effect(request)
            .map_err(NativeApplicationError::from)
    }

    fn verify_operation(
        &self,
        repository_id: String,
        providers: Vec<ProviderSpec>,
        signing_key: Option<Vec<u8>>,
    ) -> Result<NativeOperationResult, NativeApplicationError> {
        let inventory = self.inventory_source.inventory(&repository_id)?;
        let plan = AuditPlan::compile_with_root(self.root.as_deref(), &inventory, &providers)?
            .freeze(signing_key.as_deref())?;
        verify_binding(&plan, &inventory, signing_key.as_deref())?;
        Ok(NativeOperationResult::Verification {
            repository_id,
            plan_digest: plan.digest().into(),
            inventory_digest: inventory.digest,
        })
    }

    fn bind_external_project_tool(&self, mut invocation: Invocation) -> Invocation {
        if let Some(tool) = self.external_project_tool.clone() {
            invocation.context = invocation.context.with_external_project_tool(tool);
        }
        invocation
    }

    async fn invoke_audit_with_cancellation(
        &self,
        repository_id: String,
        providers: Vec<ProviderSpec>,
        signing_key: Option<Vec<u8>>,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<NativeOperationResult, NativeApplicationError> {
        let plan_inventory = self.inventory_source.inventory(&repository_id)?;
        let pending =
            AuditPlan::compile_with_root(self.root.as_deref(), &plan_inventory, &providers)?;
        let plan = match signing_key.as_deref() {
            Some(key) => pending.freeze(Some(key))?,
            None => pending.freeze_source_diagnostic()?,
        };
        self.execute_frozen_audit(&repository_id, plan, signing_key, cancellation)
            .await
    }

    /// Bind a frozen plan to a fresh execution inventory, execute it, and
    /// verify the report.
    async fn execute_frozen_audit(
        &self,
        repository_id: &str,
        plan: FrozenPlan,
        signing_key: Option<Vec<u8>>,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<NativeOperationResult, NativeApplicationError> {
        let execution_inventory = self.inventory_source.inventory(repository_id)?;
        verify_binding(&plan, &execution_inventory, signing_key.as_deref())?;
        let report = rightkit_code_audit_engine::execute_with_cancellation(
            &plan,
            &execution_inventory,
            self.provider_executor.as_ref(),
            cancellation,
        )
        .await?;
        if signing_key.is_some() {
            verify_execution(&report)?;
        } else {
            rightkit_code_audit_engine::verify_source_diagnostic(&report, &plan)?;
        }
        Ok(NativeOperationResult::Audit(report))
    }

    pub async fn invoke(
        &self,
        operation: NativeOperation,
    ) -> Result<NativeOperationResult, NativeApplicationError> {
        match operation {
            NativeOperation::Invoke(invocation) => Ok(NativeOperationResult::Invocation(
                self.engine
                    .execute(self.bind_external_project_tool(invocation))
                    .await?,
            )),
            NativeOperation::Run => {
                let invocation = self
                    .run_source
                    .as_ref()
                    .ok_or(NativeApplicationError::MissingComponent {
                        component: "RunSource",
                    })?
                    .invocation()?;
                Ok(NativeOperationResult::Invocation(
                    self.engine
                        .execute(self.bind_external_project_tool(invocation))
                        .await?,
                ))
            }
            NativeOperation::RunRequest { request } => {
                let invocation = self
                    .run_source
                    .as_ref()
                    .ok_or(NativeApplicationError::MissingComponent {
                        component: "RunSource",
                    })?
                    .invocation_for(&request)?;
                Ok(NativeOperationResult::Invocation(
                    self.engine
                        .execute(self.bind_external_project_tool(invocation))
                        .await?,
                ))
            }
            NativeOperation::Doctor { repository_id } => {
                let inventory = self.inventory_source.inventory(&repository_id)?;
                let catalog = self.catalog_source.catalog()?;
                catalog.validate()?;
                Ok(NativeOperationResult::Doctor {
                    repository_id,
                    inventory_digest: inventory.digest,
                    catalog_entries: catalog.entry_count(),
                    provider_count: self.engine.registry().providers().count(),
                })
            }
            NativeOperation::Plan {
                repository_id,
                providers,
                signing_key,
            } => {
                let inventory = self.inventory_source.inventory(&repository_id)?;
                let plan =
                    AuditPlan::compile_with_root(self.root.as_deref(), &inventory, &providers)?
                        .freeze(signing_key.as_deref())?;
                Ok(NativeOperationResult::Plan {
                    repository_id,
                    plan_digest: plan.digest().into(),
                    plan_signature: plan.signature().map(ToOwned::to_owned),
                    providers: plan
                        .providers()
                        .iter()
                        .map(|provider| provider.id.clone())
                        .collect(),
                })
            }
            NativeOperation::Audit {
                repository_id,
                providers,
                signing_key,
            } => {
                let plan_inventory: InventoryEnvelope =
                    self.inventory_source.inventory(&repository_id)?;
                let pending = AuditPlan::compile_with_root(
                    self.root.as_deref(),
                    &plan_inventory,
                    &providers,
                )?;
                let plan = match signing_key.as_deref() {
                    Some(key) => pending.freeze(Some(key))?,
                    None => pending.freeze_source_diagnostic()?,
                };
                let execution_inventory = self.inventory_source.inventory(&repository_id)?;
                verify_binding(&plan, &execution_inventory, signing_key.as_deref())?;
                let report = rightkit_code_audit_engine::execute_with_cancellation(
                    &plan,
                    &execution_inventory,
                    self.provider_executor.as_ref(),
                    tokio_util::sync::CancellationToken::new(),
                )
                .await?;
                if signing_key.is_some() {
                    verify_execution(&report)?;
                } else {
                    rightkit_code_audit_engine::verify_source_diagnostic(&report, &plan)?;
                }
                Ok(NativeOperationResult::Audit(report))
            }
            NativeOperation::AuditFrozen {
                repository_id,
                plan,
                signing_key,
            } => {
                self.execute_frozen_audit(
                    &repository_id,
                    plan,
                    signing_key,
                    tokio_util::sync::CancellationToken::new(),
                )
                .await
            }
            NativeOperation::Verify {
                repository_id,
                providers,
                signing_key,
            } => self.verify_operation(repository_id, providers, signing_key),
            NativeOperation::VerifyRequest {
                repository_id,
                providers,
                signing_key,
                facts,
                plan,
            } => {
                validate_verify_artifacts(&facts, &plan, &providers)?;
                self.verify_operation(repository_id, providers, signing_key)
            }
            NativeOperation::Catalog => Ok(NativeOperationResult::Catalog(
                self.catalog_source.catalog()?,
            )),
            NativeOperation::Report(format) => {
                let report = self.report_source.report()?;
                let rendered = match format {
                    ReportFormat::Json => rightkit_evidence_report::render_json(&report)?,
                    ReportFormat::Sarif => rightkit_evidence_report::render_sarif(&report)?,
                    ReportFormat::Markdown => rightkit_evidence_report::render_markdown(&report)?,
                    ReportFormat::Html => rightkit_evidence_report::render_html(&report)?,
                };
                Ok(NativeOperationResult::Report(rendered))
            }
        }
    }

    /// Execute one operation under the caller-owned process cancellation.
    /// The token is not replaced or forked, preserving one request lifetime
    /// from CLI signal handling through runtime scheduling.
    pub async fn invoke_with_cancellation(
        &self,
        operation: NativeOperation,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<NativeOperationResult, NativeApplicationError> {
        let operation = match operation {
            NativeOperation::Invoke(mut invocation) => {
                invocation.context.cancellation = cancellation.clone();
                NativeOperation::Invoke(invocation)
            }
            NativeOperation::Run => {
                let mut invocation = self
                    .run_source
                    .as_ref()
                    .ok_or(NativeApplicationError::MissingComponent {
                        component: "RunSource",
                    })?
                    .invocation()?;
                invocation.context.cancellation = cancellation.clone();
                NativeOperation::Invoke(invocation)
            }
            NativeOperation::RunRequest { request } => {
                let mut invocation = self
                    .run_source
                    .as_ref()
                    .ok_or(NativeApplicationError::MissingComponent {
                        component: "RunSource",
                    })?
                    .invocation_for(&request)?;
                invocation.context.cancellation = cancellation.clone();
                NativeOperation::Invoke(invocation)
            }
            operation => operation,
        };
        if matches!(&operation, NativeOperation::Invoke(_)) {
            // Runtime owns provider cancellation and bounded cleanup. Keeping this future
            // awaited lets its scheduler retain terminal provider evidence.
            self.invoke(operation).await
        } else if let NativeOperation::Audit {
            repository_id,
            providers,
            signing_key,
        } = operation
        {
            self.invoke_audit_with_cancellation(repository_id, providers, signing_key, cancellation)
                .await
        } else if let NativeOperation::AuditFrozen {
            repository_id,
            plan,
            signing_key,
        } = operation
        {
            self.execute_frozen_audit(&repository_id, plan, signing_key, cancellation)
                .await
        } else if cancellation.is_cancelled() {
            Err(NativeApplicationError::Runtime(RuntimeError::Cancelled))
        } else {
            tokio::select! {
                _ = cancellation.cancelled() => Err(NativeApplicationError::Runtime(RuntimeError::Cancelled)),
                result = self.invoke(operation) => result,
            }
        }
    }

    pub fn profile(&self) -> &rightkit_agent_orchestration::AgentProfile {
        self.engine.profile()
    }
    pub fn registry(&self) -> &ProviderRegistry {
        self.engine.registry()
    }
    pub fn provider_specs(&self) -> Vec<ProviderSpec> {
        self.provider_specs.clone()
    }
}
