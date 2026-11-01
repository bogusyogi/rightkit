use rightkit_agent_host_traits::{
    clear_skill_catalog, install_skill_catalog, skill_catalog, SkillCatalog, SkillCatalogRoot,
    SkillManifestSummary, TriggerContext, TriggeredSkill,
};
use std::path::PathBuf;
use std::sync::Arc;

struct Fixed;

impl SkillCatalog for Fixed {
    fn parse_manifest(&self, markdown: &str) -> Option<SkillManifestSummary> {
        (!markdown.is_empty()).then(|| SkillManifestSummary {
            name: markdown.to_string(),
            active: true,
            ..SkillManifestSummary::default()
        })
    }

    fn triggered_skills(&self, _dirs: &[PathBuf], context: &TriggerContext) -> Vec<TriggeredSkill> {
        if context.get("event") == Some("go") {
            vec![TriggeredSkill {
                slug: "one".into(),
                ..TriggeredSkill::default()
            }]
        } else {
            Vec::new()
        }
    }

    fn list_catalog(
        &self,
        roots: &[SkillCatalogRoot],
        _query: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!(roots.len()))
    }
}

#[test]
fn context_stores_and_replaces_values() {
    let mut ctx = TriggerContext::new();
    ctx.set("event", "a");
    ctx.set("event", String::from("b"));
    assert_eq!(ctx.get("event"), Some("b"));
    assert_eq!(ctx.get("missing"), None);
    assert_eq!(ctx.values().len(), 1);
}

#[test]
fn install_replace_and_clear() {
    clear_skill_catalog();
    assert!(skill_catalog().is_none());
    install_skill_catalog(Arc::new(Fixed));
    let catalog = skill_catalog().expect("installed");
    let mut ctx = TriggerContext::new();
    ctx.set("event", "go");
    assert_eq!(catalog.triggered_skills(&[], &ctx).len(), 1);
    assert!(catalog.parse_manifest("").is_none());
    assert_eq!(catalog.list_catalog(&[], None), Ok(serde_json::json!(0)));
    clear_skill_catalog();
    assert!(skill_catalog().is_none());
}
