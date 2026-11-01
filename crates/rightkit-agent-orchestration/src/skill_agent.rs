//! Promote a `SKILL.md` into a routable agent, no code, no new format.
//!
//! A `SKILL.md` placed in `<state-dir>/agents/` (vs `<state-dir>/skills/`) is
//! loaded as a first-class agent: its `description` becomes the trigger surface
//! the conductor routes against (via [`crate::catalog`]), its body becomes the
//! agent's charter. Two optional frontmatter fields the skills format already
//! tolerates as unknown keys drive the runtime: `model:` (the tier that runs it)
//! and `tools:` (its allowed set). The inbuilt signed roster is unchanged and
//! authoritative; promoted agents are the local, unsigned user-extension tier.

use crate::registry::AgentDescriptor;
use rightkit_agent_skills::SkillManifest;

/// Cheapest sufficient coder — the default tier a promoted skill runs on when
/// its frontmatter doesn't name a `model:`.
pub const DEFAULT_PROMOTED_MODEL: &str = "xiaomi/mimo-v2.5";

/// Default allowed tools for a promoted agent when frontmatter omits `tools:`.
pub const DEFAULT_PROMOTED_TOOLS: &[&str] = &["read_file", "write_file", "search", "shell"];

/// Build an [`AgentDescriptor`] from a parsed skill manifest.
///
/// `model`/`tools` override the manifest defaults (the caller reads them from
/// the optional frontmatter fields). The descriptor is tier-2 with a `conductor`
/// escalation path, so [`crate::catalog::path_hint_for`] derives `ArchitectPlan`
/// — methodologies tend to plan; a promoted skill that is plainly a coding leaf
/// gets `DirectEdit` from the same derivation once it carries a coding capability.
pub fn agent_from_skill_manifest(
    m: &SkillManifest,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{path_hint_for, PathHint};

    fn manifest(name: &str, desc: &str, body: &str) -> SkillManifest {
        // A minimal SKILL.md round-trips through the real parser.
        let md = format!("---\nname: {name}\ndescription: {desc}\n---\n{body}\n");
        SkillManifest::from_markdown(&md).expect("parse SKILL.md")
    }

    #[test]
    fn minimal_skill_round_trips_to_an_agent() {
        let m = manifest(
            "seo-tune",
            "Use to improve a site's SEO and propose fixes.",
            "You are an SEO specialist. Inspect the page and report gaps.",
        );
        let a = agent_from_skill_manifest(&m, None, None);
        assert_eq!(a.name, "seo-tune");
        assert_eq!(
            a.tool_description,
            "Use to improve a site's SEO and propose fixes."
        );
        assert!(a.system_prompt.contains("SEO specialist"));
        assert_eq!(a.model_id, DEFAULT_PROMOTED_MODEL);
        assert_eq!(a.allowed_tools, DEFAULT_PROMOTED_TOOLS);
        // No coding/audit capability tag and a neutral name → gated path by default.
        assert_eq!(path_hint_for(&a), PathHint::ArchitectPlan);
    }

    #[test]
    fn an_audit_named_skill_promotes_to_the_audit_lane() {
        let m = manifest(
            "security-audit",
            "Use to audit a repo for vulns.",
            "Audit it.",
        );
        let a = agent_from_skill_manifest(&m, None, None);
        assert_eq!(path_hint_for(&a), PathHint::Audit);
    }

    #[test]
    fn model_and_tools_overrides_apply() {
        let m = manifest("x", "Use for X.", "Do X.");
        let a = agent_from_skill_manifest(
            &m,
            Some("moonshotai/kimi-k2.6"),
            Some(vec!["read_file".to_string()]),
        );
        assert_eq!(a.model_id, "moonshotai/kimi-k2.6");
        assert_eq!(a.allowed_tools, vec!["read_file".to_string()]);
    }
}
