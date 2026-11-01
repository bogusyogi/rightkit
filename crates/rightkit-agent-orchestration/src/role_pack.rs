//! Production role-pack resolution: catalog validation + persona binding (FM-088, FM-104, S:F8).
//!
//! Prior state (D05/R3): `router.rs::builtin_role_pack`/`RolePackV1` parses and
//! validates a role pack but is called only from `router_tests.rs`; nothing in
//! the live spawn path constructs one. Separately, `personas.rs` defines a
//! typed persona contract that nothing consults. This module is the single
//! production seam that closes both gaps at once: it resolves a role pack
//! against a caller-supplied live model catalog, then binds the matching
//! `PersonaContract` from a `PersonaRegistry` so a spawn decision gets model
//! assignment and system-prompt identity from the same versioned pack instead
//! of the registry's one-fixed-prompt-per-agent bake
//! (`registry.rs::with_prompt`).
//!
//! `router.rs::role_pack_values` also has a live defect this module does not
//! repeat: it comments-strips with `line.split('#').next()`, which truncates
//! any field value that legitimately contains `#` inside quotes (e.g. a
//! system-prompt fragment or model alias). This module parses with the real
//! `toml` crate, which already tracks quote state correctly, so a `#` inside
//! a quoted value is never mistaken for a comment marker.
//!
//! This module deliberately returns assignment data only; it never evaluates
//! a permission/action decision.

use crate::personas::{PersonaContract, PersonaRegistry};

/// The subset of a live catalog entry this resolver needs to validate a role
/// pack. Callers adapt their own catalog type into this rather than this
/// module depending on a specific catalog crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolePackCatalogEntry {
    pub id: String,
    pub context_window_tokens: u64,
    pub cheap: bool,
    pub provider_family: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RolePackV2 {
    pub schema_version: u32,
    pub role: String,
    pub primary: String,
    pub cheap_candidate: String,
    pub fallbacks: Vec<String>,
    pub verifier: String,
    pub reasoning: String,
    pub context_budget: u64,
    pub toolset: Vec<String>,
    pub permissions: Vec<String>,
    pub isolation: String,
    pub max_turns: u32,
    pub max_cost_usd: f64,
    pub retry_limit: u8,
    pub escalate_on: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolePackVerifierBinding {
    pub model_id: String,
    pub independent: bool,
    pub degradation_reason: Option<String>,
}

/// A role pack resolved against a live catalog and bound to its persona.
/// `persona` is `None` when no `PersonaContract` is registered for the
/// role — the caller must decide whether that is fatal (a role pack without
/// an identity should not silently spawn with a stale baked-in prompt).
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedRolePack {
    pub pack: RolePackV2,
    pub primary: RolePackCatalogEntry,
    pub cheap_candidate: RolePackCatalogEntry,
    pub fallbacks: Vec<RolePackCatalogEntry>,
    pub verifier: RolePackCatalogEntry,
    pub verifier_binding: RolePackVerifierBinding,
    pub persona: Option<PersonaContract>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RolePackError {
    #[error("role pack field `{0}` is missing or invalid")]
    InvalidField(&'static str),
    #[error("role pack references unknown model `{0}`")]
    UnknownModel(String),
    #[error("role pack model `{model}` is unqualified: {reason}")]
    UnqualifiedModel { model: String, reason: String },
    #[error("role pack for role `{0}` has no registered persona")]
    NoPersonaForRole(String),
}

/// Resolve a raw role-pack document against a live catalog and, if
/// `persona_registry` is supplied, bind the persona registered for the
/// pack's role. Fails visibly (no fallback, no silent default) whenever the
/// pack is malformed, references an unknown/unqualified model, or (when a
/// registry is supplied) has no matching persona.
pub fn resolve_role_pack(
    raw: &str,
    live_catalog: &[RolePackCatalogEntry],
    persona_registry: Option<&PersonaRegistry>,
) -> Result<ResolvedRolePack, RolePackError> {
    let pack = parse_role_pack(raw)?;
    validate_pack(&pack)?;

    let lookup = |id: &str| -> Result<RolePackCatalogEntry, RolePackError> {
        live_catalog
            .iter()
            .find(|model| model.id == id)
            .cloned()
            .ok_or_else(|| RolePackError::UnknownModel(id.to_string()))
    };
    let primary = lookup(&pack.primary)?;
    let cheap_candidate = lookup(&pack.cheap_candidate)?;
    let fallbacks = pack
        .fallbacks
        .iter()
        .map(|id| lookup(id))
        .collect::<Result<Vec<_>, _>>()?;
    let verifier = lookup(&pack.verifier)?;

    for model in std::iter::once(&primary)
        .chain(std::iter::once(&cheap_candidate))
        .chain(std::iter::once(&verifier))
        .chain(fallbacks.iter())
    {
        if model.context_window_tokens < pack.context_budget {
            return Err(RolePackError::UnqualifiedModel {
                model: model.id.clone(),
                reason: format!(
                    "context window {} is below role budget {}",
                    model.context_window_tokens, pack.context_budget
                ),
            });
        }
    }
    if !cheap_candidate.cheap {
        return Err(RolePackError::UnqualifiedModel {
            model: cheap_candidate.id.clone(),
            reason: "cheap_candidate is not catalogued as cheap".into(),
        });
    }

    let independent =
        primary.id != verifier.id && primary.provider_family != verifier.provider_family;
    let verifier_binding = RolePackVerifierBinding {
        model_id: verifier.id.clone(),
        independent,
        degradation_reason: (!independent)
            .then(|| "role-pack verifier is not from a different provider family".to_string()),
    };

    let persona = match persona_registry {
        Some(registry) => {
            let persona = registry
                .persona_for_role(&pack.role)
                .cloned()
                .ok_or_else(|| RolePackError::NoPersonaForRole(pack.role.clone()))?;
            Some(persona)
        }
        None => None,
    };

    Ok(ResolvedRolePack {
        pack,
        primary,
        cheap_candidate,
        fallbacks,
        verifier,
        verifier_binding,
        persona,
    })
}

fn validate_pack(pack: &RolePackV2) -> Result<(), RolePackError> {
    if pack.schema_version != 1 {
        return Err(RolePackError::InvalidField("schema_version"));
    }
    if pack.role.trim().is_empty()
        || pack.reasoning.trim().is_empty()
        || pack.toolset.is_empty()
        || pack.permissions.is_empty()
        || pack.isolation.trim().is_empty()
        || pack.context_budget == 0
        || pack.max_turns == 0
        || pack.escalate_on.is_empty()
    {
        return Err(RolePackError::InvalidField("required v1 assignment"));
    }
    Ok(())
}

/// Parse a role pack from real TOML. Field-level defects (missing, wrong
/// type, empty, non-positive) map onto `RolePackError::InvalidField(key)`
/// exactly as the hand-rolled parser did; a document that is not valid TOML
/// at all has no matching field, so it also maps onto `InvalidField`, using
/// a fixed `"<toml>"` marker rather than inventing a new error variant.
fn parse_role_pack(raw: &str) -> Result<RolePackV2, RolePackError> {
    let table: toml::Table = raw
        .parse()
        .map_err(|_| RolePackError::InvalidField("<toml>"))?;
    Ok(RolePackV2 {
        schema_version: parse_u32(&table, "schema_version")?,
        role: parse_string(&table, "role")?,
        primary: parse_string(&table, "primary")?,
        cheap_candidate: parse_string(&table, "cheap_candidate")?,
        fallbacks: parse_array(&table, "fallbacks")?,
        verifier: parse_string(&table, "verifier")?,
        reasoning: parse_string(&table, "reasoning")?,
        context_budget: parse_u64(&table, "context_budget")?,
        toolset: parse_array(&table, "toolset")?,
        permissions: parse_array(&table, "permissions")?,
        isolation: parse_string(&table, "isolation")?,
        max_turns: parse_u32(&table, "max_turns")?,
        max_cost_usd: table
            .get("max_cost_usd")
            .and_then(|v| v.as_float())
            .filter(|v: &f64| v.is_finite() && *v > 0.0)
            .ok_or(RolePackError::InvalidField("max_cost_usd"))?,
        retry_limit: parse_u32(&table, "retry_limit")?
            .try_into()
            .map_err(|_| RolePackError::InvalidField("retry_limit"))?,
        escalate_on: parse_array(&table, "escalate_on")?,
    })
}

fn parse_string(table: &toml::Table, key: &'static str) -> Result<String, RolePackError> {
    table
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .ok_or(RolePackError::InvalidField(key))
}

fn parse_array(table: &toml::Table, key: &'static str) -> Result<Vec<String>, RolePackError> {
    let array = table
        .get(key)
        .and_then(|v| v.as_array())
        .ok_or(RolePackError::InvalidField(key))?;
    let items = array
        .iter()
        .map(|v| v.as_str().map(str::trim).filter(|v| !v.is_empty()))
        .collect::<Option<Vec<_>>>()
        .ok_or(RolePackError::InvalidField(key))?
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    if items.is_empty() {
        Err(RolePackError::InvalidField(key))
    } else {
        Ok(items)
    }
}

fn parse_u32(table: &toml::Table, key: &'static str) -> Result<u32, RolePackError> {
    table
        .get(key)
        .and_then(|v| v.as_integer())
        .and_then(|v| u32::try_from(v).ok())
        .filter(|v| *v > 0)
        .ok_or(RolePackError::InvalidField(key))
}

fn parse_u64(table: &toml::Table, key: &'static str) -> Result<u64, RolePackError> {
    table
        .get(key)
        .and_then(|v| v.as_integer())
        .and_then(|v| u64::try_from(v).ok())
        .filter(|v| *v > 0)
        .ok_or(RolePackError::InvalidField(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_raw() -> String {
        r#"
schema_version = 1
role = "reviewer"
primary = "model-strong"
cheap_candidate = "model-cheap"
fallbacks = ["model-fallback"]
verifier = "model-verifier"
reasoning = "high"
context_budget = 50000
toolset = ["Read", "Grep"]
permissions = ["read"]
isolation = "sandboxed"
max_turns = 10
max_cost_usd = 2.5
retry_limit = 2
escalate_on = ["timeout"]
"#
        .to_string()
    }

    fn catalog() -> Vec<RolePackCatalogEntry> {
        vec![
            RolePackCatalogEntry {
                id: "model-strong".into(),
                context_window_tokens: 200_000,
                cheap: false,
                provider_family: "anthropic".into(),
            },
            RolePackCatalogEntry {
                id: "model-cheap".into(),
                context_window_tokens: 100_000,
                cheap: true,
                provider_family: "anthropic".into(),
            },
            RolePackCatalogEntry {
                id: "model-fallback".into(),
                context_window_tokens: 100_000,
                cheap: false,
                provider_family: "anthropic".into(),
            },
            RolePackCatalogEntry {
                id: "model-verifier".into(),
                context_window_tokens: 100_000,
                cheap: false,
                provider_family: "openai".into(),
            },
        ]
    }

    fn registry_with_reviewer() -> PersonaRegistry {
        let mut registry = PersonaRegistry::new();
        registry
            .register(PersonaContract::new("p1", "reviewer", "review carefully").unwrap())
            .unwrap();
        registry
    }

    #[test]
    fn resolves_pack_and_binds_registered_persona() {
        let registry = registry_with_reviewer();
        let resolved = resolve_role_pack(&valid_raw(), &catalog(), Some(&registry)).unwrap();
        assert_eq!(resolved.primary.id, "model-strong");
        assert_eq!(resolved.cheap_candidate.id, "model-cheap");
        assert_eq!(resolved.fallbacks.len(), 1);
        assert_eq!(resolved.verifier.id, "model-verifier");
        assert!(resolved.verifier_binding.independent);
        let persona = resolved.persona.expect("persona must resolve for role");
        assert_eq!(persona.id, "p1");
        assert_eq!(persona.system_prompt, "review carefully");
    }

    #[test]
    fn fails_visibly_when_role_has_no_registered_persona() {
        let empty_registry = PersonaRegistry::new();
        let err = resolve_role_pack(&valid_raw(), &catalog(), Some(&empty_registry)).unwrap_err();
        assert_eq!(err, RolePackError::NoPersonaForRole("reviewer".to_string()));
    }

    #[test]
    fn omitting_registry_skips_persona_binding_without_failing() {
        let resolved = resolve_role_pack(&valid_raw(), &catalog(), None).unwrap();
        assert_eq!(resolved.persona, None);
    }

    #[test]
    fn rejects_unsupported_schema_version() {
        let raw = valid_raw().replace("schema_version = 1", "schema_version = 2");
        let error = resolve_role_pack(&raw, &catalog(), None).unwrap_err();
        assert_eq!(error, RolePackError::InvalidField("schema_version"));
    }

    #[test]
    fn rejects_reference_to_unknown_model() {
        let raw = valid_raw().replace("model-verifier", "model-ghost");
        let error = resolve_role_pack(&raw, &catalog(), None).unwrap_err();
        assert_eq!(
            error,
            RolePackError::UnknownModel("model-ghost".to_string())
        );
    }

    #[test]
    fn rejects_model_with_context_window_below_role_budget() {
        let mut small_catalog = catalog();
        small_catalog[0].context_window_tokens = 1_000; // primary, below 50000 budget
        let error = resolve_role_pack(&valid_raw(), &small_catalog, None).unwrap_err();
        match error {
            RolePackError::UnqualifiedModel { model, reason } => {
                assert_eq!(model, "model-strong");
                assert!(reason.contains("context window"));
            }
            other => panic!("expected UnqualifiedModel, got {other:?}"),
        }
    }

    #[test]
    fn rejects_cheap_candidate_not_flagged_cheap_in_catalog() {
        let mut not_cheap_catalog = catalog();
        not_cheap_catalog[1].cheap = false; // model-cheap
        let error = resolve_role_pack(&valid_raw(), &not_cheap_catalog, None).unwrap_err();
        match error {
            RolePackError::UnqualifiedModel { model, reason } => {
                assert_eq!(model, "model-cheap");
                assert!(reason.contains("not catalogued as cheap"));
            }
            other => panic!("expected UnqualifiedModel, got {other:?}"),
        }
    }

    #[test]
    fn flags_degradation_when_verifier_shares_provider_family_with_primary() {
        let mut same_family_catalog = catalog();
        same_family_catalog[3].provider_family = "anthropic".into();
        let resolved = resolve_role_pack(&valid_raw(), &same_family_catalog, None).unwrap();
        assert!(!resolved.verifier_binding.independent);
        assert_eq!(
            resolved.verifier_binding.degradation_reason.as_deref(),
            Some("role-pack verifier is not from a different provider family")
        );
    }

    #[test]
    fn parse_role_pack_strips_unquoted_comments_but_preserves_hash_in_quotes() {
        // This is the exact defect router.rs::role_pack_values carries:
        // `line.split('#').next()` truncates a quoted value containing `#`.
        // This parser (now real TOML) must not repeat it: a `#` inside a
        // quoted string is preserved, while an unquoted trailing comment is
        // stripped.
        let raw = valid_raw().replace(
            "reasoning = \"high\"",
            "reasoning = \"val#ue\" # a trailing comment",
        );
        let pack = parse_role_pack(&raw).unwrap();
        assert_eq!(pack.reasoning, "val#ue");
    }

    #[test]
    fn quoted_hash_in_system_prompt_style_field_survives_full_parse() {
        let raw = valid_raw().replace(
            "reasoning = \"high\"",
            "reasoning = \"use #priority tagging\"",
        );
        let pack = parse_role_pack(&raw).unwrap();
        assert_eq!(pack.reasoning, "use #priority tagging");
    }
}
