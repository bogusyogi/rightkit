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
/// (`rightkit_app_settings::agent_config::state_root()`).
fn default_config_dir() -> Option<PathBuf> {
    rightkit_app_settings::agent_config::state_root()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_table_parses_and_is_nonempty() {
        let rows = load_embedded();
        assert!(!rows.is_empty());
    }

    #[test]
    fn embedded_table_has_no_duplicate_slugs() {
        let rows = load_embedded();
        let mut seen = std::collections::HashSet::new();
        for row in &rows {
            assert!(
                seen.insert(row.slug.clone()),
                "duplicate slug: {}",
                row.slug
            );
        }
    }

    #[test]
    fn roundtrip_known_model_pricing() {
        let rows = load_embedded();
        let row = rows
            .iter()
            .find(|m| m.slug == "deepseek/deepseek-v4-pro")
            .expect("deepseek-v4-pro must be present");
        assert!((row.prompt_per_m - 0.435).abs() < 1e-9);
        assert!((row.completion_per_m - 0.87).abs() < 1e-9);
        assert_eq!(row.context_window_tokens, 1_048_576);
        assert_eq!(row.compaction_trigger_percent, 85);
        assert_eq!(row.compaction_reserve_tokens, 65_536);
        assert_eq!(row.compaction_max_output_tokens, 32_768);
    }

    #[test]
    fn legacy_override_rows_receive_conservative_compaction_defaults() {
        let rows = parse(
            r#"
            [[model]]
            slug = "acme/legacy"
            prompt_per_m = 1.0
            completion_per_m = 2.0
            context_window_tokens = 100000
            tier = "cheap"
            reasoning = 1
            tool_schema_class = "openai-tools"
            vision = false
            "#,
        )
        .unwrap();
        assert_eq!(rows[0].compaction_trigger_percent, 85);
        assert_eq!(rows[0].compaction_reserve_tokens, 32_768);
        assert_eq!(rows[0].compaction_max_output_tokens, 20_000);
    }

    #[test]
    fn override_file_wins_over_embedded() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("model-metadata.toml"),
            r#"
            [[model]]
            slug = "acme/override-test"
            prompt_per_m = 9.99
            completion_per_m = 19.99
            context_window_tokens = 4096
            tier = "capable"
            reasoning = 3
            tool_schema_class = "openai-tools"
            vision = false
            "#,
        )
        .unwrap();

        let rows = load_from_dir(dir.path());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].slug, "acme/override-test");
    }

    #[test]
    fn missing_override_falls_back_to_embedded() {
        let dir = tempfile::tempdir().unwrap();
        // No model-metadata.toml written in this dir.
        let rows = load_from_dir(dir.path());
        assert!(rows.iter().any(|m| m.slug == "deepseek/deepseek-v4-pro"));
    }

    #[test]
    fn malformed_override_falls_back_to_embedded_loudly_ignored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("model-metadata.toml"), "not = = toml").unwrap();

        let rows = load_from_dir(dir.path());
        assert!(rows.iter().any(|m| m.slug == "deepseek/deepseek-v4-pro"));
    }

    #[test]
    fn empty_override_falls_back_to_embedded() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("model-metadata.toml"), "").unwrap();

        let rows = load_from_dir(dir.path());
        assert!(rows.iter().any(|m| m.slug == "deepseek/deepseek-v4-pro"));
    }

    #[test]
    fn serde_rejects_malformed_entry_missing_required_field() {
        let bad = r#"
        [[model]]
        slug = "acme/missing-fields"
        prompt_per_m = 1.0
        "#;
        let result = parse(bad);
        assert!(result.is_err(), "expected parse error for incomplete row");
    }

    #[test]
    fn serde_rejects_invalid_tier_value() {
        let bad = r#"
        [[model]]
        slug = "acme/bad-tier"
        prompt_per_m = 1.0
        completion_per_m = 2.0
        context_window_tokens = 1000
        tier = "ultra-mega"
        reasoning = 1
        tool_schema_class = "openai-tools"
        vision = false
        "#;
        let result = parse(bad);
        assert!(
            result.is_err(),
            "expected parse error for invalid tier enum value"
        );
    }

    #[test]
    fn capability_of_finds_known_model() {
        // Uses the process-wide cached table (embedded, since no user catalog exists in
        // the default test env) — just exercise the lookup API shape.
        let found = table().iter().any(|m| m.slug == "moonshotai/kimi-k2.6");
        assert!(found);
    }

    #[test]
    fn native_openai_gpt_61_sol_alias_reuses_canonical_metadata_only() {
        let rows = load_embedded();
        let canonical = rows
            .iter()
            .find(|model| model.slug == "openai/gpt-6.1-sol")
            .expect("canonical OpenAI GPT-6.1 Sol metadata must exist");
        assert_eq!(canonical.context_window_tokens, 272_000);
        assert_eq!(
            canonical.supported_efforts.as_ref().unwrap(),
            &["low", "medium", "high", "xhigh", "max"]
        );
        let m2 = capability_from_rows(&rows, "minimax/minimax-m2").unwrap();
        assert!(
            m2.supported_efforts.is_none(),
            "depth rating must not invent API efforts"
        );
        assert!(std::ptr::eq(
            capability_from_rows(&rows, "gpt-6.1-sol").unwrap(),
            canonical
        ));
        assert!(capability_from_rows(&rows, "gpt-5.6").is_none());
        assert!(capability_from_rows(&rows, "gpt-6.1-mini").is_none());
        assert!(capability_from_rows(&rows, "other/gpt-6.1-sol").is_none());
    }

    #[test]
    fn signed_refresh_rolls_back_to_last_good_and_reports_degraded() {
        struct ExactVerifier;
        impl CatalogSignatureVerifier for ExactVerifier {
            fn verify(&self, _payload: &[u8], signature: &str) -> bool {
                signature == "valid"
            }
        }
        let dir = tempfile::tempdir().unwrap();
        // Signed snapshots require provider/model slugs. Embedded native aliases
        // are lookup conveniences, not valid signed refresh rows.
        let models: Vec<_> = load_embedded()
            .into_iter()
            .filter(|row| row.slug.contains('/'))
            .collect();
        let snapshot = |version: &str, signature: &str| {
            let issued_at = "2026-08-25T00:00:00Z";
            let payload = catalog_payload(&models, version, issued_at);
            ModelCatalogSnapshot {
                schema_version: 1,
                version: version.into(),
                issued_at: issued_at.into(),
                models: models.clone(),
                payload_sha256: sha256_bytes(&payload),
                signature: signature.into(),
            }
        };
        std::fs::write(
            dir.path().join("model-catalog.snapshot.json"),
            serde_json::to_vec(&snapshot("bad", "invalid")).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("model-catalog.last-good.json"),
            serde_json::to_vec(&snapshot("good", "valid")).unwrap(),
        )
        .unwrap();

        let loaded = load_signed_catalog(dir.path(), &ExactVerifier);
        assert_eq!(
            loaded.source,
            CatalogSource::LastKnownGood,
            "{:?}",
            loaded.diagnostic
        );
        assert_eq!(loaded.version, "good");
        assert!(loaded.degraded);
        assert!(loaded.diagnostic.unwrap().contains("signature"));
    }

    #[test]
    fn signed_refresh_rejects_unknown_shape_before_signature() {
        struct AcceptAll;
        impl CatalogSignatureVerifier for AcceptAll {
            fn verify(&self, _payload: &[u8], _signature: &str) -> bool {
                true
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let snapshot = ModelCatalogSnapshot {
            schema_version: 1,
            version: "bad".into(),
            issued_at: "now".into(),
            models: vec![],
            payload_sha256: "sha256:none".into(),
            signature: "valid".into(),
        };
        std::fs::write(
            dir.path().join("model-catalog.snapshot.json"),
            serde_json::to_vec(&snapshot).unwrap(),
        )
        .unwrap();
        let loaded = load_signed_catalog(dir.path(), &AcceptAll);
        assert_eq!(loaded.source, CatalogSource::Embedded);
        assert!(loaded.degraded);
    }
}
