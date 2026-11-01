// Daemon seam for resolving versioned role packs from the live model catalog.
//
// Resolution happens before a spawn is admitted.  The returned assignment is
// descriptive; permission/action decisions remain owned by policy and are not
// recomputed here.
//
// Two resolution paths are exposed:
// - [`RolePackResolver`] wraps `rightkit_agent_policy::role_pack_resolver`
//   (model-assignment validation only, no persona).
// - [`SpawnRolePackResolver`] wraps `rightkit_agent_orchestration::role_pack`
//   (FM-088/FM-104/S:F8): it additionally binds the role's `PersonaContract`
//   from a `PersonaRegistry`, so a spawn gets its system prompt from the
//   same versioned pack instead of the registry's single fixed prompt
//   (`AgentRegistry::with_prompt`). `DaemonState::spawn` consumes this full
//   resolved binding for built-in role-pack children.

use rightkit_agent_orchestration::personas::PersonaRegistry;
use rightkit_agent_orchestration::role_pack::{
    self, RolePackCatalogEntry, RolePackError as SpawnRolePackError,
};
use rightkit_agent_policy::role_pack_resolver::{
    resolve_role_pack, LiveModel, ResolvedRolePack, RolePackResolutionError,
};

pub struct RolePackResolver<'a> {
    live_catalog: &'a [LiveModel],
}

impl<'a> RolePackResolver<'a> {
    pub fn new(live_catalog: &'a [LiveModel]) -> Self {
        Self { live_catalog }
    }

    pub fn resolve(&self, raw_pack: &str) -> Result<ResolvedRolePack, RolePackResolutionError> {
        resolve_role_pack(raw_pack, self.live_catalog)
    }

    pub fn resolve_builtin(&self, role: &str) -> Result<ResolvedRolePack, RolePackResolutionError> {
        let raw =
            builtin_role_pack_source(role).ok_or(RolePackResolutionError::InvalidField("role"))?;
        self.resolve(raw)
    }
}

/// Resolves a role pack against a live catalog *and* binds the role's
/// registered persona in one call, so a caller building a spawn decision
/// gets model assignment and system-prompt identity from a single source of
/// truth. Fails visibly (no default persona, no partial spawn) when either
/// the pack or the persona lookup fails.
pub struct SpawnRolePackResolver<'a> {
    live_catalog: &'a [RolePackCatalogEntry],
    personas: &'a PersonaRegistry,
}

impl<'a> SpawnRolePackResolver<'a> {
    pub fn new(live_catalog: &'a [RolePackCatalogEntry], personas: &'a PersonaRegistry) -> Self {
        Self {
            live_catalog,
            personas,
        }
    }

    pub fn resolve(
        &self,
        raw_pack: &str,
    ) -> Result<role_pack::ResolvedRolePack, SpawnRolePackError> {
        role_pack::resolve_role_pack(raw_pack, self.live_catalog, Some(self.personas))
    }

    pub fn resolve_builtin(
        &self,
        role: &str,
    ) -> Result<role_pack::ResolvedRolePack, SpawnRolePackError> {
        let raw = builtin_role_pack_source(role).ok_or(SpawnRolePackError::InvalidField("role"))?;
        self.resolve(raw)
    }
}

/// The live model catalog adapted for role-pack resolution. `ModelCapability`
/// (`rightkit_llm::agent_providers::model_metadata::table()`, the same static roster
/// `router_seam.rs` already routes against) carries no explicit provider
/// field, so `provider_family` is derived from the catalog's `vendor/model`
/// slug convention — the same convention the built-in role-pack TOML
/// fixtures (`engine/fixtures/role-packs/*.v1.toml`) use for every model id.
pub fn live_role_pack_catalog() -> Vec<RolePackCatalogEntry> {
    // Consume the provider-layer metadata projection so role-pack admission
    // sees the same embedded source, auth/eligibility, availability, and
    // capability catalogue exposed to provider consumers.
    rightkit_llm::agent_providers::registry_meta::RegistryMeta::from_builtin_catalogue()
        .iter()
        .map(|m| RolePackCatalogEntry {
            id: m.slug.clone(),
            context_window_tokens: m.context_window_tokens,
            cheap: matches!(
                m.tier,
                rightkit_llm::agent_providers::model_metadata::ModelTier::Cheap
            ),
            provider_family: m.provider.clone(),
        })
        .collect()
}

/// Resolve the role's built-in pack against `live_catalog` at live-spawn time
/// (FM-088/FM-104/S:F8), including its registered persona. The caller applies
/// every runtime-supported field or refuses child execution.
///
/// Returns `Ok(None)` for a role with no built-in pack, leaving the caller's
/// existing (legacy) model-resolution path completely unaffected. Returns
/// `Err` for a role that DOES have a built-in pack but fails to resolve
/// (unknown/unqualified model reference, malformed pack) — the caller must
/// treat this as fatal and abort the spawn before any execution, never fall
/// back silently to the legacy path.
pub fn resolve_builtin_role_pack_for_spawn(
    role: &str,
    live_catalog: &[RolePackCatalogEntry],
) -> Result<Option<role_pack::ResolvedRolePack>, SpawnRolePackError> {
    match builtin_role_pack_source(role) {
        Some(raw) => {
            // FM-088: bind the role's persona as well as its model. The
            // registry is built from the same builtin role packs, so a pack
            // whose persona is missing fails visibly here rather than
            // silently spawning with the registry's fixed prompt.
            let personas = PersonaRegistry::from_builtin_role_packs()
                .map_err(|_| SpawnRolePackError::InvalidField("persona"))?;
            role_pack::resolve_role_pack(raw, live_catalog, Some(&personas)).map(Some)
        }
        None => Ok(None),
    }
}

fn builtin_role_pack_source(role: &str) -> Option<&'static str> {
    match role {
        "orchestrator" => Some(include_str!(
            "../../../../fixtures/role-packs/orchestrator.v1.toml"
        )),
        "coder" => Some(include_str!(
            "../../../../fixtures/role-packs/coder.v1.toml"
        )),
        "verifier" => Some(include_str!(
            "../../../../fixtures/role-packs/verifier.v1.toml"
        )),
        _ => None,
    }
}

#[cfg(test)]
mod router_tests {
    use super::*;
    use rightkit_agent_orchestration::personas::PersonaContract;

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
                id: "model-verifier".into(),
                context_window_tokens: 100_000,
                cheap: false,
                provider_family: "openai".into(),
            },
        ]
    }

    fn raw_pack() -> String {
        r#"
schema_version = 1
role = "reviewer"
primary = "model-strong"
cheap_candidate = "model-cheap"
fallbacks = []
verifier = "model-verifier"
reasoning = "high"
context_budget = 50000
toolset = ["Read"]
permissions = ["read"]
isolation = "sandboxed"
max_turns = 10
max_cost_usd = 2.5
retry_limit = 2
escalate_on = ["timeout"]
"#
        .replace("fallbacks = []", "fallbacks = [\"model-strong\"]")
    }

    #[test]
    fn spawn_resolver_binds_persona_alongside_model_assignment() {
        let catalog = catalog();
        let mut personas = PersonaRegistry::new();
        personas
            .register(PersonaContract::new("p1", "reviewer", "review carefully").unwrap())
            .unwrap();

        let resolver = SpawnRolePackResolver::new(&catalog, &personas);
        let resolved = resolver.resolve(&raw_pack()).unwrap();
        assert_eq!(resolved.primary.id, "model-strong");
        assert_eq!(resolved.persona.expect("persona should resolve").id, "p1");
    }

    #[test]
    fn spawn_resolver_fails_visibly_without_registered_persona() {
        let catalog = catalog();
        let personas = PersonaRegistry::new();
        let resolver = SpawnRolePackResolver::new(&catalog, &personas);
        let error = resolver.resolve(&raw_pack()).unwrap_err();
        assert_eq!(
            error,
            SpawnRolePackError::NoPersonaForRole("reviewer".to_string())
        );
    }

    /// S:F8 acceptance shape (resolution layer): `coder.v1.toml` resolves
    /// against the REAL live catalog (`live_role_pack_catalog()`, i.e.
    /// `rightkit_llm::agent_providers::model_metadata::table()`), and its `primary`
    /// field ("xiaomi/mimo-v2.5-pro") is the model that must drive the
    /// spawn. See `daemon_state.rs::live_spawn_uses_role_packs_primary_model_for_a_builtin_role`
    /// for the same assertion made through the live spawn path itself.
    #[test]
    fn coder_role_pack_resolves_against_the_real_live_catalog() {
        let catalog = live_role_pack_catalog();
        let resolved = resolve_builtin_role_pack_for_spawn("coder", &catalog)
            .expect("coder pack must resolve against the real live catalog")
            .expect("coder has a built-in role pack");
        assert_eq!(resolved.primary.id, "xiaomi/mimo-v2.5-pro");
        assert_eq!(resolved.pack.role, "coder");
    }

    /// S:F8: "invalid pack fails visibly before execution." An empty catalog
    /// makes every model reference in the built-in "coder" pack unresolvable
    /// — the resolver must return a typed error, not a silent fallback.
    #[test]
    fn invalid_role_pack_fails_visibly_instead_of_falling_back_silently() {
        let empty_catalog: Vec<RolePackCatalogEntry> = vec![];
        let error = resolve_builtin_role_pack_for_spawn("coder", &empty_catalog)
            .expect_err("expected a resolution error against an empty catalog");
        assert_eq!(
            error,
            SpawnRolePackError::UnknownModel("xiaomi/mimo-v2.5-pro".to_string())
        );
    }

    /// A role with no built-in pack must leave the caller's legacy
    /// model-resolution path completely untouched — `Ok(None)`, not an error.
    #[test]
    fn role_without_a_builtin_pack_resolves_to_none() {
        let catalog = live_role_pack_catalog();
        assert_eq!(
            resolve_builtin_role_pack_for_spawn("researcher", &catalog).unwrap(),
            None
        );
    }

    #[test]
    fn resolve_builtin_rejects_unknown_role_for_both_paths() {
        let live_models: Vec<LiveModel> = vec![];
        let resolver = RolePackResolver::new(&live_models);
        assert_eq!(
            resolver.resolve_builtin("not-a-role").unwrap_err(),
            RolePackResolutionError::InvalidField("role")
        );

        let catalog = catalog();
        let personas = PersonaRegistry::new();
        let spawn_resolver = SpawnRolePackResolver::new(&catalog, &personas);
        assert_eq!(
            spawn_resolver.resolve_builtin("not-a-role").unwrap_err(),
            SpawnRolePackError::InvalidField("role")
        );
    }
}
