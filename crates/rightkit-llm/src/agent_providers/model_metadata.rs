//! Loadable model pricing + capability registry.
//!
//! Replaces the previously compile-in-only `PRICES`/`WINDOWS` tables in
//! [`crate::agent_providers::pricing`] with a bundled TOML data file
//! (`assets/model-metadata.toml`), embedded via `include_str!` as the
//! offline-safe compile-time default and overridable wholesale by a matching
//! file at `<config-dir>/model-metadata.toml` if present.
//!
//! [`ModelCapability`] is the per-model row: pricing AND capability metadata
//! live in one place so the (future) capability/cost router (ADR
//! `docs/canon/daemon-master-canon-atoms.md`) has a single
//! roster to consult. This module only loads and looks up the table; it does
//! not make routing decisions.
//!
//! RIGHTKIT-SERIOUS: no RightKit model-capability catalog (tier, reasoning depth, compaction trigger/reserve/max-output, efforts, signed refresh with rollback); compaction and routing cannot run without it (report P-5)

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// Coarse cost/capability bucket a router can consult. Distinct from
/// `AgentDescriptor::escalation_tier` (1-3 agent seniority) — this is a
/// per-model bucket, not a per-agent one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelTier {
    Cheap,
    Capable,
}

/// One model's pricing + capability row, as parsed from `model-metadata.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelCapability {
    pub slug: String,
    pub prompt_per_m: f64,
    pub completion_per_m: f64,
    pub context_window_tokens: u64,
    /// Percentage of the model context at which automatic compaction becomes
    /// eligible. The engine also preserves `compaction_reserve_tokens`, so the
    /// earlier of the two boundaries wins.
    #[serde(default = "default_compaction_trigger_percent")]
    pub compaction_trigger_percent: u8,
    /// Context headroom reserved for the next turn, tool results, and the
    /// compaction request itself.
    #[serde(default = "default_compaction_reserve_tokens")]
    pub compaction_reserve_tokens: u64,
    /// Provider output ceiling applied specifically to the model-authored
    /// compaction summary request.
    #[serde(default = "default_compaction_max_output_tokens")]
    pub compaction_max_output_tokens: u32,
    pub tier: ModelTier,
    /// Coarse reasoning-depth rating, 0-3 (higher = deeper).
    pub reasoning: u8,
    /// Explicit provider-supported effort ids. None means unqualified/unknown;
    /// the independent reasoning-depth score never implies an API setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supported_efforts: Option<Vec<String>>,
    pub tool_schema_class: String,
    pub vision: bool,
}

impl ModelCapability {
    /// CRA-10 wiring point: builds the H8 [`crate::agent_providers::client::CapacityProfileV1`]
    /// for this row. `context_window_tokens` is the sole effective limit and
    /// `compaction_reserve_tokens` the sole heuristic headroom margin — this
    /// deliberately does not fold in `compaction_trigger_percent` or
    /// `compaction_max_output_tokens`, per semantic-contracts.md
    /// "Model-aware capacity & final insertion": those never substitute for
    /// `context_window_tokens` as the effective limit.
    pub fn capacity_profile(
        &self,
        route_id: impl Into<String>,
        catalog_revision: Option<String>,
    ) -> crate::agent_providers::client::CapacityProfileV1 {
        crate::agent_providers::client::CapacityProfileV1::new(
            self.context_window_tokens,
            self.compaction_reserve_tokens,
            self.slug.clone(),
            route_id,
            catalog_revision,
        )
    }
}

const fn default_compaction_trigger_percent() -> u8 {
    85
}

const fn default_compaction_reserve_tokens() -> u64 {
    32_768
}

const fn default_compaction_max_output_tokens() -> u32 {
    20_000
}

/// Top-level shape of `model-metadata.toml`: a TOML array of tables under the
/// `[[model]]` key.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ModelMetadataFile {
    #[serde(default)]
    model: Vec<ModelCapability>,
}

/// Versioned refresh artifact for the authoritative live catalog. Signature
/// verification is delegated to a caller-supplied implementation so this
/// crate never hand-rolls cryptography or embeds an update-channel key.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalogSnapshot {
    pub schema_version: u32,
    pub version: String,
    pub issued_at: String,
    pub models: Vec<ModelCapability>,
    pub payload_sha256: String,
    pub signature: String,
}

pub trait CatalogSignatureVerifier: Send + Sync {
    fn verify(&self, payload: &[u8], signature: &str) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogSource {
    Embedded,
    SignedRefresh,
    LastKnownGood,
}

#[derive(Debug, Clone)]
pub struct CatalogLoad {
    pub models: Vec<ModelCapability>,
    pub source: CatalogSource,
    pub version: String,
    pub degraded: bool,
    pub diagnostic: Option<String>,
    pub payload_sha256: String,
}

/// Errors loading or parsing a model-metadata file.
#[derive(Debug, thiserror::Error)]
pub enum ModelMetadataError {
    #[error("failed to parse model-metadata TOML: {0}")]
    Toml(String),
    #[error("io error reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// The bundled default table, embedded at compile time so the app never
/// depends on a file existing on disk.
const EMBEDDED_METADATA_TOML: &str = include_str!("../../assets/model-metadata.toml");

/// Parse a `model-metadata.toml` document into a flat list of rows.
fn parse(toml_str: &str) -> Result<Vec<ModelCapability>, ModelMetadataError> {
    let parsed: ModelMetadataFile =
        toml::from_str(toml_str).map_err(|e| ModelMetadataError::Toml(e.to_string()))?;
    Ok(parsed.model)
}

/// Load the embedded default table. Panics only if the bundled asset itself
/// fails to parse — that is a build-time defect, never a runtime/user one.
fn load_embedded() -> Vec<ModelCapability> {
    parse(EMBEDDED_METADATA_TOML).expect("bundled model-metadata.toml must parse")
}

fn catalog_payload(models: &[ModelCapability], version: &str, issued_at: &str) -> Vec<u8> {
    serde_json::to_vec(&(version, issued_at, models)).expect("catalog payload serializes")
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    hasher.update(bytes);
    format!("sha256:{}", hasher.finalize())
}

fn validate_models(models: &[ModelCapability]) -> Result<(), String> {
    if models.is_empty() {
        return Err("catalog has no model rows".into());
    }
    let mut slugs = std::collections::HashSet::new();
    for model in models {
        if !model.slug.contains('/') || model.slug.split('/').any(str::is_empty) {
            return Err(format!("model slug `{}` is not provider/model", model.slug));
        }
        if !slugs.insert(model.slug.as_str()) {
            return Err(format!("duplicate model slug `{}`", model.slug));
        }
        if !model.prompt_per_m.is_finite()
            || !model.completion_per_m.is_finite()
            || model.prompt_per_m < 0.0
            || model.completion_per_m < 0.0
            || model.context_window_tokens == 0
            || model.reasoning > 3
            || model.tool_schema_class.trim().is_empty()
        {
            return Err(format!(
                "model `{}` has invalid capability metadata",
                model.slug
            ));
        }
    }
    Ok(())
}

fn verified_snapshot(
    path: &Path,
    verifier: &dyn CatalogSignatureVerifier,
) -> Result<CatalogLoad, String> {
    let raw =
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let snapshot: ModelCatalogSnapshot =
        serde_json::from_str(&raw).map_err(|error| format!("{}: {error}", path.display()))?;
    if snapshot.schema_version != 1 {
        return Err(format!(
            "unsupported catalog schema {}",
            snapshot.schema_version
        ));
    }
    validate_models(&snapshot.models)?;
    let payload = catalog_payload(&snapshot.models, &snapshot.version, &snapshot.issued_at);
    let digest = sha256_bytes(&payload);
    if digest != snapshot.payload_sha256 {
        return Err("catalog payload digest mismatch".into());
    }
    if !verifier.verify(&payload, &snapshot.signature) {
        return Err("catalog signature rejected".into());
    }
    Ok(CatalogLoad {
        models: snapshot.models,
        source: CatalogSource::SignedRefresh,
        version: snapshot.version,
        degraded: false,
        diagnostic: None,
        payload_sha256: digest,
    })
}

/// Load a signed refresh, rolling back to the separately preserved last-good
/// snapshot on validation/signature failure. If neither is valid, the bundled
/// catalog remains authoritative and the degraded reason is visible.
pub fn load_signed_catalog(
    config_dir: &Path,
    verifier: &dyn CatalogSignatureVerifier,
) -> CatalogLoad {
    let current = config_dir.join("model-catalog.snapshot.json");
    let last_good = config_dir.join("model-catalog.last-good.json");
    match verified_snapshot(&current, verifier) {
        Ok(load) => load,
        Err(current_error) => match verified_snapshot(&last_good, verifier) {
            Ok(mut load) => {
                load.source = CatalogSource::LastKnownGood;
                load.degraded = true;
                load.diagnostic = Some(format!("refresh rejected: {current_error}"));
                load
            }
            Err(last_good_error) => {
                let models = load_embedded();
                let payload = catalog_payload(&models, "embedded-v1", "compile-time");
                CatalogLoad {
                    models,
                    source: CatalogSource::Embedded,
                    version: "embedded-v1".into(),
                    degraded: current.exists() || last_good.exists(),
                    diagnostic: (current.exists() || last_good.exists()).then(|| {
                        format!(
                            "refresh rejected: {current_error}; last-good rejected: {last_good_error}"
                        )
                    }),
                    payload_sha256: sha256_bytes(&payload),
                }
            }
        },
    }
}

/// Load model metadata, preferring an on-disk override at
/// `<config_dir>/model-metadata.toml` when present and valid; falling back to
/// the embedded bundled table otherwise (missing file, unreadable file, or
/// malformed TOML all fall back — a broken override must never crash the
/// daemon, it should just not apply).
pub fn load_from_dir(config_dir: &Path) -> Vec<ModelCapability> {
    let override_path = config_dir.join("model-metadata.toml");
    if let Ok(content) = std::fs::read_to_string(&override_path) {
        match parse(&content) {
            Ok(rows) if !rows.is_empty() => return rows,
            Ok(_) => { /* empty override file: fall through to embedded */ }
            Err(_) => { /* malformed override: fall through to embedded */ }
        }
    }
    load_embedded()
}

/// Resolve the user config directory the same way `host_config` does
/// (`rightkit_agent_config::state_root()`).
fn default_config_dir() -> Option<PathBuf> {
    rightkit_agent_config::state_root()
}

static TABLE: OnceLock<CatalogLoad> = OnceLock::new();
static CATALOG_VERIFIER: OnceLock<Box<dyn CatalogSignatureVerifier>> = OnceLock::new();

/// Install the process verifier before the first catalog lookup. A second
/// verifier or a verifier installed after catalog initialization is rejected
/// visibly; refresh trust can never be replaced mid-process.
pub fn install_catalog_signature_verifier(
    verifier: Box<dyn CatalogSignatureVerifier>,
) -> Result<(), &'static str> {
    if TABLE.get().is_some() {
        return Err("model catalog was already initialized");
    }
    CATALOG_VERIFIER
        .set(verifier)
        .map_err(|_| "model catalog signature verifier was already installed")
}

struct RejectUnsignedRefresh;

impl CatalogSignatureVerifier for RejectUnsignedRefresh {
    fn verify(&self, _payload: &[u8], _signature: &str) -> bool {
        false
    }
}

fn catalog() -> &'static CatalogLoad {
    TABLE.get_or_init(|| match default_config_dir() {
        Some(dir) => load_signed_catalog(
            &dir,
            CATALOG_VERIFIER
                .get()
                .map(|verifier| verifier.as_ref())
                .unwrap_or(&RejectUnsignedRefresh),
        ),
        None => {
            let models = load_embedded();
            let payload = catalog_payload(&models, "embedded-v1", "compile-time");
            CatalogLoad {
                models,
                source: CatalogSource::Embedded,
                version: "embedded-v1".into(),
                degraded: false,
                diagnostic: None,
                payload_sha256: sha256_bytes(&payload),
            }
        }
    })
}

/// Process-wide model metadata table: on-disk override if present and valid,
/// else the embedded bundled default. Loaded once and cached.
pub fn table() -> &'static [ModelCapability] {
    catalog().models.as_slice()
}

fn capability_from_rows<'a>(
    models: &'a [ModelCapability],
    slug: &str,
) -> Option<&'a ModelCapability> {
    // The native ChatGPT OAuth transport sends provider-native IDs (without
    // the canonical provider prefix), while the shared catalog stores stable
    // provider/model slugs. Keep only this tested native model alias tied to
    // its canonical row; every other unknown ID remains unknown.
    let canonical_slug = match slug {
        "gpt-6.1-sol" => "openai/gpt-6.1-sol",
        _ => slug,
    };
    models.iter().find(|m| m.slug == canonical_slug)
}

pub fn catalog_status() -> &'static CatalogLoad {
    catalog()
}

/// Look up a model's full capability row by slug.
pub fn capability_of(slug: &str) -> Option<&'static ModelCapability> {
    capability_from_rows(table(), slug)
}
