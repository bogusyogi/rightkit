//! Promote a `SKILL.md` into a routable agent, no code, no new format.
//!
//! A `SKILL.md` placed in `<state-dir>/agents/` (vs `<state-dir>/skills/`) is
//! loaded as a first-class agent: its `description` becomes the trigger surface
//! the conductor routes against (via [`crate::coding::catalog`]), its body becomes the
//! agent's charter. Two optional frontmatter fields the skills format already
//! tolerates as unknown keys drive the runtime: `model:` (the tier that runs it)
//! and `tools:` (its allowed set). The inbuilt signed roster is unchanged and
//! authoritative; promoted agents are the local, unsigned user-extension tier.
//!
//! Parsing a `SKILL.md` belongs to the host: it turns the file into a
//! [`SkillManifestSummary`] through its installed `SkillCatalog`, and this module
//! only maps the summary onto an agent descriptor.

use crate::coding::registry::AgentDescriptor;
use rightkit_agent_host_traits::SkillManifestSummary;

/// Cheapest sufficient coder — the default tier a promoted skill runs on when
/// its frontmatter doesn't name a `model:`.
pub const DEFAULT_PROMOTED_MODEL: &str = "xiaomi/mimo-v2.5";

/// Default allowed tools for a promoted agent when frontmatter omits `tools:`.
pub const DEFAULT_PROMOTED_TOOLS: &[&str] = &["read_file", "write_file", "search", "shell"];

/// Build an [`AgentDescriptor`] from a host-parsed skill manifest summary.
///
/// `model`/`tools` override the manifest defaults (the caller reads them from
/// the optional frontmatter fields). The descriptor is tier-2 with a `conductor`
/// escalation path, so [`crate::coding::catalog::path_hint_for`] derives `ArchitectPlan`
/// — methodologies tend to plan; a promoted skill that is plainly a coding leaf
/// gets `DirectEdit` from the same derivation once it carries a coding capability.
pub fn agent_from_skill_manifest(
    m: &SkillManifestSummary,
    model: Option<&str>,
    tools: Option<Vec<String>>,
) -> AgentDescriptor {
    let mut a = AgentDescriptor::new(
        m.name.clone(),
        m.name.clone(),
        model.unwrap_or(DEFAULT_PROMOTED_MODEL),
    );
    a.tool_description = m.description.clone();
    a.system_prompt = m.body.clone();
    a.allowed_tools = tools.unwrap_or_else(|| {
        DEFAULT_PROMOTED_TOOLS
            .iter()
            .map(|s| s.to_string())
            .collect()
    });
    a.capabilities = m.tags.clone();
    a.escalation_tier = 2;
    a.escalation_path = vec!["conductor".to_string()];
    a
}
