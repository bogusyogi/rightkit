//! Registry consumer for rightkit-agent-host-traits 0.1.0: builds against the published API only.
use rightkit_agent_host_traits::{
    clear_skill_catalog, install_skill_catalog, skill_catalog, SkillCatalog, SkillCatalogRoot,
    SkillManifestSummary, TriggerContext, TriggeredSkill,
};
use std::path::PathBuf;
use std::sync::Arc;

struct Empty;

impl SkillCatalog for Empty {
    fn parse_manifest(&self, _markdown: &str) -> Option<SkillManifestSummary> {
        None
    }

    fn triggered_skills(&self, _dirs: &[PathBuf], _context: &TriggerContext) -> Vec<TriggeredSkill> {
        Vec::new()
    }

    fn list_catalog(
        &self,
        _roots: &[SkillCatalogRoot],
        _query: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!([]))
    }
}

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(skill_catalog().is_none(), "no catalog is installed by default");
    install_skill_catalog(Arc::new(Empty));
    check(skill_catalog().is_some(), "an installed catalog is returned");
    let mut ctx = TriggerContext::new();
    ctx.set("event", "PreToolUse");
    check(
        ctx.get("event") == Some("PreToolUse"),
        "trigger context round-trips a value",
    );
    clear_skill_catalog();
    check(skill_catalog().is_none(), "the catalog can be cleared");
}
