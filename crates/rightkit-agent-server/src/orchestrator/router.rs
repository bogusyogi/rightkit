// Daemon seam for resolving versioned role packs from the live model catalog.
//
// Resolution happens before a spawn is admitted.  The returned assignment is
// descriptive; permission/action decisions remain owned by policy and are not
// recomputed here.
//
// Two resolution paths are exposed:
// - [`RolePackResolver`] wraps `rightkit_agent_policy::role_pack_resolver`
//   (model-assignment validation only, no persona).
// - [`SpawnRolePackResolver`] wraps `rightkit_agent_orchestration::coding::role_pack`
//   (FM-088/FM-104/S:F8): it additionally binds the role's `PersonaContract`
//   from a `PersonaRegistry`, so a spawn gets its system prompt from the
//   same versioned pack instead of the registry's single fixed prompt
//   (`AgentRegistry::with_prompt`). `DaemonState::spawn` consumes this full
//   resolved binding for built-in role-pack children.

use rightkit_agent_orchestration::coding::personas::PersonaRegistry;
use rightkit_agent_orchestration::coding::role_pack::{
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
        "orchestrator" => Some(include_str!("../../data/role-packs/orchestrator.v1.toml")),
        "coder" => Some(include_str!("../../data/role-packs/coder.v1.toml")),
        "verifier" => Some(include_str!("../../data/role-packs/verifier.v1.toml")),
        _ => None,
    }
}
