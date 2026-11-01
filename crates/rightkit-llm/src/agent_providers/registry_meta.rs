//! PL-003 — Provider + model registry metadata.
//!
//! Adds the eligibility/capability/freshness metadata layer that sits above
//! the existing route registry ([`crate::agent_providers::registry::ModelRegistry`]) and
//! capability/pricing table ([`crate::agent_providers::model_metadata`]). This module is a
//! **delta**, not a fork: it does not duplicate route storage or pricing,
//! it composes one `RegistryMetaEntry` per model out of data those two
//! modules and [`crate::agent_providers::subscription_auth::eligibility`] already own, plus
//! the fields canon PL-003 requires that neither owns today — supported
//! auth modes, the third-party-subscription eligibility flag (kept
//! deliberately distinct from plain API availability), streaming/prompt
//! caching/usage-reporting capability flags, an availability/disabled
//! reason, and catalog source + freshness.
//!
//! Donor lineage (Port-first `ADAPT_TO_RUST`, canon PL-003, delta over
//! closed `S:M1`/`S:M2`/`S:M3`/`S:M6`):
//!
//! - Pi `packages/ai/src/models.ts` (`Provider<TApi>` — `id`/`name`/
//!   `baseUrl`/`auth`/`getModels()`) and `packages/ai/src/types.ts`
//!   (`Model<TApi>` — `reasoning`, `input` ("text"/"image" -> vision),
//!   `contextWindow`, `maxTokens`) supply the provider+model shape this
//!   entry composes over.
//! - Pi `packages/ai/src/models-store.ts` (`ModelsStoreEntry` —
//!   `lastModified` / `checkedAt` / `etag`) supplies the freshness triple
//!   ported here as [`CatalogFreshness`].
//! - `ProviderAuth` (`packages/ai/src/auth/types.ts`; its the host app port
//!   was retired with the P-3 move to RightKit's OAuth resolver) is the
//!   source for the supported-auth-mode enumeration ([`AuthMode`]).
//! - the host app-original: [`SubscriptionEligibility`] re-export (PL-001,
//!   `crate::agent_providers::subscription_auth::eligibility`) is layered in as its own
//!   field, never conflated with `available`/`disabled_reason` — canon is
//!   explicit that "eligibility flag is separate from API availability".
//!
//! No second registry: [`RegistryMeta`] does not replace
//! [`crate::agent_providers::registry::ModelRegistry`] (routing) or
//! [`crate::agent_providers::model_metadata`] (pricing + `ModelCapability` bundled table);
//! it is composed from them (`from_route_and_capability`) plus the fields
//! neither owns.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::agent_providers::model_metadata::{ModelCapability, ModelTier};
use crate::agent_providers::registry::ModelRoute;
use crate::agent_providers::subscription_auth::eligibility::SubscriptionEligibility;

/// Supported authentication modes for a provider, mirroring the donor
/// `ProviderAuth` union (`apiKey` / `oauth`) plus the ambient-credential
/// case Pi documents on `Provider.auth` ("even providers with only ambient
/// credentials ... provide `apiKey` auth whose `resolve()` reports whether
/// the provider is configured").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    /// Static or environment-sourced API key.
    ApiKey,
    /// OAuth / device-code / PKCE flow (see `subscription_auth`).
    OAuth,
    /// Ambient credential resolution (env var, cloud profile, ADC file,
    /// local keyless server) exposed through the `apiKey` auth surface per
    /// donor comment above.
    Ambient,
}

/// Why a model/provider entry is not currently available, kept distinct
/// from `SubscriptionEligibility` (canon: "eligibility flag is separate
/// from API availability" — a model can be subscription-eligible yet
/// unavailable for an unrelated reason, e.g. no credential configured).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum AvailabilityReason {
    /// Fully available: credential resolved, route reachable, no override.
    Available,
    /// No credential is currently resolvable for any of the model's
    /// supported auth modes.
    NoCredential,
    /// A credential is configured but access was refused (revoked, plan
    /// mismatch, region-blocked, etc). Carries a redacted, human-readable
    /// detail — never raw credential material.
    Refused { detail: String },
    /// Disabled by explicit local configuration/policy, not a provider-side
    /// fact.
    DisabledByPolicy { detail: String },
    /// Not yet checked (no resolution attempt has run).
    Unchecked,
}

impl AvailabilityReason {
    pub fn is_available(&self) -> bool {
        matches!(self, AvailabilityReason::Available)
    }

    /// Build refusal metadata without allowing provider credentials or bearer
    /// material to cross the registry boundary into status/UI payloads.
    pub fn refused(detail: impl AsRef<str>) -> Self {
        let detail = detail.as_ref();
        let redacted = detail
            .split_whitespace()
            .map(|part| {
                if part.len() >= 16
                    && (part.starts_with("sk-")
                        || part.starts_with("ghp_")
                        || part.starts_with("glpat-")
                        || part.to_ascii_lowercase().starts_with("bearer"))
                {
                    "[redacted]"
                } else {
                    part
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        Self::Refused { detail: redacted }
    }
}

/// Where a catalog row's capability/pricing data came from, plus how fresh
/// it is. Deliberately reuses [`crate::agent_providers::model_metadata::CatalogSource`]'s
/// three-way vocabulary (embedded default / signed refresh / last-known-good
/// fallback) rather than inventing a second one; freshness fields are the
/// PL-003 delta ported from Pi's `ModelsStoreEntry`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogSourceKind {
    /// Bundled compile-time default (`crate::agent_providers::model_metadata::CatalogSource::Embedded`).
    Embedded,
    /// A verified signed refresh was applied
    /// (`crate::agent_providers::model_metadata::CatalogSource::SignedRefresh`).
    SignedRefresh,
    /// A signed refresh failed verification/parsing and the last
    /// known-good snapshot was retained
    /// (`crate::agent_providers::model_metadata::CatalogSource::LastKnownGood`).
    LastKnownGood,
}

impl From<crate::agent_providers::model_metadata::CatalogSource> for CatalogSourceKind {
    fn from(value: crate::agent_providers::model_metadata::CatalogSource) -> Self {
        match value {
            crate::agent_providers::model_metadata::CatalogSource::Embedded => {
                CatalogSourceKind::Embedded
            }
            crate::agent_providers::model_metadata::CatalogSource::SignedRefresh => {
                CatalogSourceKind::SignedRefresh
            }
            crate::agent_providers::model_metadata::CatalogSource::LastKnownGood => {
                CatalogSourceKind::LastKnownGood
            }
        }
    }
}

/// Freshness of a catalog row, ported from Pi's `ModelsStoreEntry`
/// (`lastModified` / `checkedAt` / `etag`), all Unix-ms timestamps here to
/// match this crate's existing `now_ms()` convention
/// (`crate::agent_providers::subscription_auth::types::now_ms`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogFreshness {
    /// Unix-ms timestamp from the remote catalog's `Last-Modified` header,
    /// when known.
    pub last_modified_ms: Option<i64>,
    /// Unix-ms timestamp of the last completed remote freshness check,
    /// regardless of whether it changed anything.
    pub checked_at_ms: Option<i64>,
    /// Opaque validator from the remote catalog's `ETag` header, stored
    /// verbatim (quotes included) and echoed back as `If-None-Match`, per
    /// donor contract.
    pub etag: Option<String>,
}

/// One model's registry metadata row: everything PL-003 requires that is
/// not already owned by [`crate::agent_providers::registry::ModelRegistry`] (routing) or
/// [`crate::agent_providers::model_metadata::ModelCapability`] (pricing + coarse capability
/// bucket), composed together into one lookup unit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegistryMetaEntry {
    /// `provider/model` slug, matching `ModelCapability::slug` and usable
    /// as a `ModelRegistry` route name.
    pub slug: String,
    pub provider: String,
    pub model_id: String,

    /// Auth modes this provider actually supports, in donor-declared
    /// preference order (e.g. `[OAuth, ApiKey]` when both are offered).
    pub supported_auth_modes: Vec<AuthMode>,

    /// Third-party-subscription eligibility. Separate field from
    /// `availability` by design — see module docs.
    pub subscription_eligibility: SubscriptionEligibility,

    /// Current availability/disabled state, independent of eligibility.
    pub availability: AvailabilityReason,

    /// Capability metadata. `tools`/`streaming`/`prompt_caching`/
    /// `usage_reporting` are the PL-003 delta fields with no home in
    /// `ModelCapability`; the rest project from the existing capability
    /// row so this stays one composed view, not a duplicate store.
    pub context_window_tokens: u64,
    pub reasoning: u8,
    pub vision: bool,
    pub tool_schema_class: String,
    pub tools: bool,
    pub streaming: bool,
    pub prompt_caching: bool,
    pub usage_reporting: bool,
    pub tier: ModelTier,

    pub catalog_source: CatalogSourceKind,
    pub freshness: CatalogFreshness,
}

/// In-memory registry of [`RegistryMetaEntry`] rows keyed by slug.
/// Extension registration (canon: "extension registration") is `register`
/// itself — an extension (PL-004) inserts/overwrites its own entries
/// through the same call a built-in catalog load uses; there is no
/// separate built-in-only path.
#[derive(Debug, Default)]
pub struct RegistryMeta {
    entries: HashMap<String, RegistryMetaEntry>,
}

impl RegistryMeta {
    pub fn new() -> Self {
        Self::default()
    }

    /// Compose the offline-safe built-in provider catalogue.  This is the
    /// canonical seed used before any signed refresh or extension overlay;
    /// availability remains `NoCredential` until auth resolution proves it.
    pub fn from_builtin_catalogue() -> Self {
        let mut registry = Self::new();
        for capability in crate::agent_providers::model_metadata::table() {
            let provider = capability
                .slug
                .split('/')
                .next()
                .unwrap_or("unknown")
                .to_string();
            let route = ModelRoute::new(
                capability.slug.clone(),
                provider,
                String::new(),
                capability.slug.clone(),
                0,
            );
            registry.register(from_route_and_capability(
                &route,
                capability,
                vec![AuthMode::ApiKey, AuthMode::OAuth, AuthMode::Ambient],
                SubscriptionEligibility::Unknown,
                AvailabilityReason::NoCredential,
                true,
                true,
                false,
                true,
                CatalogSourceKind::Embedded,
                CatalogFreshness::default(),
            ));
        }
        registry
    }

    /// Register (insert or overwrite) one entry. Used both for built-in
    /// catalog composition and for extension (PL-004) registration.
    pub fn register(&mut self, entry: RegistryMetaEntry) {
        self.entries.insert(entry.slug.clone(), entry);
    }

    pub fn get(&self, slug: &str) -> Option<&RegistryMetaEntry> {
        self.entries.get(slug)
    }

    pub fn remove(&mut self, slug: &str) -> Option<RegistryMetaEntry> {
        self.entries.remove(slug)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// All entries whose `subscription_eligibility` gate is actually
    /// enabled in core (canon: "only `Supported` may be enabled in core").
    pub fn subscription_enabled(&self) -> Vec<&RegistryMetaEntry> {
        self.entries
            .values()
            .filter(|e| e.subscription_eligibility.enabled_in_core())
            .collect()
    }

    /// All entries currently available for use, independent of
    /// eligibility.
    pub fn available(&self) -> Vec<&RegistryMetaEntry> {
        self.entries
            .values()
            .filter(|e| e.availability.is_available())
            .collect()
    }

    pub fn iter(&self) -> impl Iterator<Item = &RegistryMetaEntry> {
        self.entries.values()
    }
}

/// Compose one [`RegistryMetaEntry`] from an existing [`ModelRoute`]
/// (routing identity) and [`ModelCapability`] (pricing/coarse capability
/// row), filling the PL-003 delta fields explicitly rather than guessing.
/// `tools`/`streaming`/`prompt_caching`/`usage_reporting` have no source in
/// either input today, so callers must state them; this composer never
/// invents a "probably true" default for a capability flag it cannot
/// observe.
#[allow(clippy::too_many_arguments)]
pub fn from_route_and_capability(
    route: &ModelRoute,
    capability: &ModelCapability,
    supported_auth_modes: Vec<AuthMode>,
    subscription_eligibility: SubscriptionEligibility,
    availability: AvailabilityReason,
    tools: bool,
    streaming: bool,
    prompt_caching: bool,
    usage_reporting: bool,
    catalog_source: CatalogSourceKind,
    freshness: CatalogFreshness,
) -> RegistryMetaEntry {
    RegistryMetaEntry {
        slug: capability.slug.clone(),
        provider: route.provider.clone(),
        model_id: route.model_id.clone(),
        supported_auth_modes,
        subscription_eligibility,
        availability,
        context_window_tokens: capability.context_window_tokens,
        reasoning: capability.reasoning,
        vision: capability.vision,
        tool_schema_class: capability.tool_schema_class.clone(),
        tools,
        streaming,
        prompt_caching,
        usage_reporting,
        tier: capability.tier,
        catalog_source,
        freshness,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_route() -> ModelRoute {
        ModelRoute::new(
            "anthropic/claude-opus-4-8",
            "anthropic",
            "https://api.anthropic.com/v1",
            "claude-opus-4-8-20251101",
            0,
        )
    }

    fn sample_capability() -> ModelCapability {
        ModelCapability {
            slug: "anthropic/claude-opus-4-8".to_string(),
            prompt_per_m: 15.0,
            completion_per_m: 75.0,
            context_window_tokens: 200_000,
            compaction_trigger_percent: 70,
            compaction_reserve_tokens: 32_768,
            compaction_max_output_tokens: 20_000,
            tier: ModelTier::Capable,
            reasoning: 3,
            supported_efforts: None,
            tool_schema_class: "anthropic".to_string(),
            vision: true,
        }
    }

    fn sample_entry() -> RegistryMetaEntry {
        from_route_and_capability(
            &sample_route(),
            &sample_capability(),
            vec![AuthMode::OAuth, AuthMode::ApiKey],
            SubscriptionEligibility::Supported,
            AvailabilityReason::Available,
            true,
            true,
            true,
            true,
            CatalogSourceKind::Embedded,
            CatalogFreshness::default(),
        )
    }

    #[test]
    fn composes_routing_and_capability_fields_without_duplicating_storage() {
        let entry = sample_entry();
        assert_eq!(entry.slug, "anthropic/claude-opus-4-8");
        assert_eq!(entry.provider, "anthropic");
        assert_eq!(entry.model_id, "claude-opus-4-8-20251101");
        assert_eq!(entry.context_window_tokens, 200_000);
        assert_eq!(entry.reasoning, 3);
        assert!(entry.vision);
        assert_eq!(entry.tier, ModelTier::Capable);
    }

    #[test]
    fn eligibility_and_availability_stay_independent_fields() {
        // Eligible for subscription billing but unavailable for an
        // unrelated reason (canon: "eligibility flag is separate from
        // API availability").
        let mut entry = sample_entry();
        entry.availability = AvailabilityReason::NoCredential;
        assert_eq!(
            entry.subscription_eligibility,
            SubscriptionEligibility::Supported
        );
        assert!(!entry.availability.is_available());
    }

    #[test]
    fn registry_register_and_get_round_trip() {
        let mut registry = RegistryMeta::new();
        assert!(registry.is_empty());
        registry.register(sample_entry());
        assert_eq!(registry.len(), 1);
        let got = registry.get("anthropic/claude-opus-4-8").expect("entry");
        assert_eq!(got.provider, "anthropic");
    }

    #[test]
    fn register_overwrites_existing_slug_extension_registration() {
        // Extension (PL-004) registration path: re-registering the same
        // slug overwrites rather than duplicating.
        let mut registry = RegistryMeta::new();
        registry.register(sample_entry());
        let mut updated = sample_entry();
        updated.availability = AvailabilityReason::DisabledByPolicy {
            detail: "org policy".to_string(),
        };
        registry.register(updated);
        assert_eq!(registry.len(), 1);
        assert!(!registry
            .get("anthropic/claude-opus-4-8")
            .unwrap()
            .availability
            .is_available());
    }

    #[test]
    fn subscription_enabled_filters_to_only_the_supported_gate() {
        let mut registry = RegistryMeta::new();
        registry.register(sample_entry());

        let mut unsupported = sample_entry();
        unsupported.slug = "other/unsupported-model".to_string();
        unsupported.subscription_eligibility = SubscriptionEligibility::Unsupported;
        registry.register(unsupported);

        let mut unknown = sample_entry();
        unknown.slug = "other/unknown-model".to_string();
        unknown.subscription_eligibility = SubscriptionEligibility::Unknown;
        registry.register(unknown);

        let enabled = registry.subscription_enabled();
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].slug, "anthropic/claude-opus-4-8");
    }

    #[test]
    fn available_filters_out_non_available_reasons() {
        let mut registry = RegistryMeta::new();
        registry.register(sample_entry());
        let mut unavailable = sample_entry();
        unavailable.slug = "anthropic/other-model".to_string();
        unavailable.availability = AvailabilityReason::Refused {
            detail: "plan mismatch".to_string(),
        };
        registry.register(unavailable);

        let available = registry.available();
        assert_eq!(available.len(), 1);
        assert_eq!(available[0].slug, "anthropic/claude-opus-4-8");
    }

    #[test]
    fn catalog_source_kind_maps_from_model_metadata_catalog_source() {
        assert_eq!(
            CatalogSourceKind::from(
                crate::agent_providers::model_metadata::CatalogSource::Embedded
            ),
            CatalogSourceKind::Embedded
        );
        assert_eq!(
            CatalogSourceKind::from(
                crate::agent_providers::model_metadata::CatalogSource::SignedRefresh
            ),
            CatalogSourceKind::SignedRefresh
        );
        assert_eq!(
            CatalogSourceKind::from(
                crate::agent_providers::model_metadata::CatalogSource::LastKnownGood
            ),
            CatalogSourceKind::LastKnownGood
        );
    }

    #[test]
    fn freshness_defaults_to_all_unknown() {
        let freshness = CatalogFreshness::default();
        assert!(freshness.last_modified_ms.is_none());
        assert!(freshness.checked_at_ms.is_none());
        assert!(freshness.etag.is_none());
    }

    #[test]
    fn remove_drops_an_entry() {
        let mut registry = RegistryMeta::new();
        registry.register(sample_entry());
        let removed = registry.remove("anthropic/claude-opus-4-8");
        assert!(removed.is_some());
        assert!(registry.is_empty());
    }
}
