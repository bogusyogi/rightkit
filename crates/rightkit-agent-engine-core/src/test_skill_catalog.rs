//! A small in-memory-format `SkillCatalog` for tests.
//!
//! Real skill formats belong to the host. This stand-in reads the two front-matter shapes the
//! tests write (`+++` TOML-like and `---` YAML-like) and evaluates trigger clauses joined by
//! ` and ` using `==`, `contains` and `matches` (a leading `*` glob).

use rightkit_agent_host_traits::{
    install_skill_catalog, SkillCatalog, SkillCatalogRoot, SkillEffects, SkillManifestSummary,
    TriggerContext, TriggeredSkill,
};
use std::path::PathBuf;
use std::sync::{Arc, Once};

static INSTALL: Once = Once::new();

/// Install the stand-in catalog once for the test process.
pub(crate) fn install_test_skill_catalog() {
    INSTALL.call_once(|| install_skill_catalog(Arc::new(TestSkillCatalog)));
}

struct TestSkillCatalog;

#[derive(Default)]
struct Parsed {
    name: String,
    description: String,
    trigger: String,
    path_affinity: Vec<String>,
    model_invocation_disabled: bool,
    enabled: bool,
    effects: SkillEffects,
}

fn strings(value: &str) -> Vec<String> {
    value
        .split('"')
        .enumerate()
        .filter(|(i, _)| i % 2 == 1)
        .map(|(_, part)| part.to_string())
        .collect()
}

fn scalar(value: &str) -> String {
    let value = value.trim();
    match value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
        Some(inner) => inner.replace("\\\"", "\""),
        None => value.to_string(),
    }
}

fn parse(markdown: &str) -> Option<Parsed> {
    let fence = if markdown.starts_with("+++") {
        "+++"
    } else if markdown.starts_with("---") {
        "---"
    } else {
        return None;
    };
    let rest = &markdown[fence.len()..];
    let end = rest.find(fence)?;
    let mut parsed = Parsed {
        enabled: true,
        ..Parsed::default()
    };
    for line in rest[..end].lines() {
        let Some(split) = line.find(['=', ':']) else {
            continue;
        };
        let (key, value) = (line[..split].trim(), line[split + 1..].trim());
        match key {
            "name" => parsed.name = scalar(value),
            "description" => parsed.description = scalar(value),
            "trigger" => parsed.trigger = scalar(value),
            "pathAffinity" => parsed.path_affinity = strings(value),
            "disable-model-invocation" => parsed.model_invocation_disabled = value == "true",
            "enabled" => parsed.enabled = value != "false",
            "executionMode" => parsed.effects.execution_mode = scalar(value),
            "allowedTools" => parsed.effects.allowed_tools = strings(value),
            "disallowedTools" => parsed.effects.disallowed_tools = strings(value),
            "prompt" => parsed.effects.prompt = scalar(value),
            _ => {}
        }
    }
    Some(parsed)
}

fn glob_matches(pattern: &str, text: &str) -> bool {
    match pattern.strip_prefix('*') {
        Some(suffix) => text.ends_with(suffix),
        None => pattern == text,
    }
}

fn trigger_matches(trigger: &str, context: &TriggerContext) -> bool {
    trigger.split(" and ").all(|clause| {
        let clause = clause.trim();
        for (op, ops) in [(" == ", 0), (" contains ", 1), (" matches ", 2)] {
            if let Some((key, wanted)) = clause.split_once(op) {
                let wanted = scalar(wanted);
                let Some(actual) = context.get(key.trim()) else {
                    return false;
                };
                return match ops {
                    0 => actual == wanted,
                    1 => actual.contains(&wanted),
                    _ => glob_matches(&wanted, actual),
                };
            }
        }
        false
    })
}

impl SkillCatalog for TestSkillCatalog {
    fn parse_manifest(&self, markdown: &str) -> Option<SkillManifestSummary> {
        let parsed = parse(markdown)?;
        Some(SkillManifestSummary {
            name: parsed.name,
            description: parsed.description,
            body: String::new(),
            tags: Vec::new(),
            active: parsed.enabled,
            lifecycle: if parsed.enabled { "active" } else { "disabled" }.to_string(),
        })
    }

    fn triggered_skills(
        &self,
        skill_dirs: &[PathBuf],
        context: &TriggerContext,
    ) -> Vec<TriggeredSkill> {
        let mut out = Vec::new();
        for dir in skill_dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
                    continue;
                }
                let Some(slug) = path.file_stem().and_then(|name| name.to_str()) else {
                    continue;
                };
                let Ok(raw) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let Some(skill) = parse(&raw) else { continue };
                if !skill.enabled
                    || skill.model_invocation_disabled
                    || (skill.trigger.is_empty() && skill.path_affinity.is_empty())
                {
                    continue;
                }
                let by_trigger =
                    !skill.trigger.is_empty() && trigger_matches(&skill.trigger, context);
                let by_path = context.get("file_path").is_some_and(|file| {
                    skill
                        .path_affinity
                        .iter()
                        .any(|pattern| glob_matches(pattern, file))
                });
                if by_trigger || by_path {
                    out.push(TriggeredSkill {
                        slug: slug.to_string(),
                        description: skill.description,
                        trigger: skill.trigger,
                        path_affinity: skill.path_affinity,
                        effects: skill.effects,
                    });
                }
            }
        }
        out.sort_by(|a, b| a.slug.cmp(&b.slug));
        out
    }

    fn list_catalog(
        &self,
        _roots: &[SkillCatalogRoot],
        _query: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!([]))
    }
}
