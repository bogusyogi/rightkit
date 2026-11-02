//! Pure local-plugin discovery & middleware projection.

use std::path::{Path, PathBuf};

pub fn discover_plugins(dir: &Path) -> Result<Vec<rightkit_agent_plugins::PluginManifest>, String> {
    let mut loader = rightkit_agent_plugins::PluginLoader::new(dir.to_path_buf());
    loader.discover().map_err(|error| error.to_string())?;
    let mut plugins = loader
        .enabled_plugins()
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    for entry in std::fs::read_dir(dir).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let manifest_path = entry.path().join("plugin.json");
        if !manifest_path.exists() {
            continue;
        }
        let raw = std::fs::read_to_string(&manifest_path).map_err(|error| error.to_string())?;
        let manifest = rightkit_agent_plugins::PluginManifest::from_json(&raw)
            .map_err(|error| error.to_string())?;
        if plugins.iter().all(|plugin| plugin.id != manifest.id) {
            plugins.push(manifest);
        }
    }
    plugins.sort_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then_with(|| left.version.cmp(&right.version))
    });
    Ok(plugins)
}

pub fn plugin_manifests_with_roots(
    dir: &Path,
) -> Vec<(rightkit_agent_plugins::PluginManifest, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut manifests = Vec::new();
    for entry in entries.flatten() {
        let root = entry.path();
        if !root.is_dir() {
            continue;
        }
        let manifest_path = root.join("plugin.json");
        let Ok(raw) = std::fs::read_to_string(&manifest_path) else {
            continue;
        };
        let Ok(manifest) = rightkit_agent_plugins::PluginManifest::from_json(&raw) else {
            continue;
        };
        manifests.push((manifest, root));
    }
    manifests.sort_by(|(left, _), (right, _)| {
        left.id
            .cmp(&right.id)
            .then_with(|| left.version.cmp(&right.version))
    });
    manifests
}

pub fn provider_extension_documents(dir: &Path) -> Result<Vec<String>, String> {
    let mut documents = Vec::new();
    for (manifest, root) in plugin_manifests_with_roots(dir) {
        if !manifest.enabled {
            continue;
        }
        let path = root.join("provider.json");
        if path.exists() {
            documents.push(std::fs::read_to_string(path).map_err(|error| error.to_string())?);
        }
    }
    Ok(documents)
}

pub fn local_plugin_middleware_definitions(
    dir: &Path,
) -> Vec<rightkit_agent_engine_runtime_middleware::MiddlewareDefinition> {
    let mut definitions = Vec::new();
    for (manifest, _) in plugin_manifests_with_roots(dir) {
        if !manifest.enabled {
            continue;
        }
        for contribution in manifest.middleware {
            if let Some(definition) = plugin_middleware_definition(&manifest.id, contribution) {
                definitions.push(definition);
            }
        }
    }
    definitions.sort_by(|left, right| {
        left.order
            .cmp(&right.order)
            .then_with(|| left.id.cmp(&right.id))
    });
    definitions
}

fn plugin_middleware_definition(
    plugin_id: &str,
    contribution: rightkit_agent_plugins::PluginMiddlewareContribution,
) -> Option<rightkit_agent_engine_runtime_middleware::MiddlewareDefinition> {
    use rightkit_agent_engine_runtime_middleware::{
        MiddlewareCondition, MiddlewareDefinition, MiddlewareSource,
    };
    let phase = parse_middleware_phase(&contribution.phase)?;
    let effect = parse_middleware_effect(&contribution.effect)?;
    let id = contribution.id.trim();
    if id.is_empty() {
        return None;
    }
    Some(MiddlewareDefinition {
        id: format!("plugin.{plugin_id}.{id}"),
        title: if contribution.title.trim().is_empty() {
            format!("{plugin_id}: {id}")
        } else {
            contribution.title
        },
        source: MiddlewareSource::LocalPlugin,
        phase,
        effect,
        tool: contribution
            .tool
            .map(|tool| tool.trim().to_string())
            .filter(|tool| !tool.is_empty()),
        requires_toolset: contribution
            .requires_toolset
            .map(|toolset| toolset.trim().to_string())
            .filter(|toolset| !toolset.is_empty()),
        condition: contribution.condition.map(|condition| MiddlewareCondition {
            argument: condition
                .argument
                .map(|argument| argument.trim().to_string())
                .filter(|argument| !argument.is_empty()),
            contains: condition
                .contains
                .map(|contains| contains.trim().to_string())
                .filter(|contains| !contains.is_empty()),
            equals: condition.equals,
        }),
        mutate_args: contribution.mutate_args,
        reason: contribution
            .reason
            .map(|reason| reason.trim().to_string())
            .filter(|reason| !reason.is_empty()),
        enabled: contribution.enabled.unwrap_or(true),
        order: contribution.order.unwrap_or(100),
        description: contribution.description,
    })
}

fn parse_middleware_phase(
    value: &str,
) -> Option<rightkit_agent_engine_runtime_middleware::MiddlewarePhase> {
    use rightkit_agent_engine_runtime_middleware::MiddlewarePhase;
    match value {
        "sessionStart" => Some(MiddlewarePhase::SessionStart),
        "preLlmCall" => Some(MiddlewarePhase::PreLlmCall),
        "postLlmCall" => Some(MiddlewarePhase::PostLlmCall),
        "preToolCall" => Some(MiddlewarePhase::PreToolCall),
        "postToolCall" => Some(MiddlewarePhase::PostToolCall),
        "approvalRequested" => Some(MiddlewarePhase::ApprovalRequested),
        "subagentStart" => Some(MiddlewarePhase::SubagentStart),
        "subagentStop" => Some(MiddlewarePhase::SubagentStop),
        "scheduledJobStart" => Some(MiddlewarePhase::ScheduledJobStart),
        "scheduledJobEnd" => Some(MiddlewarePhase::ScheduledJobEnd),
        "sessionEnd" => Some(MiddlewarePhase::SessionEnd),
        _ => None,
    }
}

fn parse_middleware_effect(
    value: &str,
) -> Option<rightkit_agent_engine_runtime_middleware::MiddlewareEffect> {
    use rightkit_agent_engine_runtime_middleware::MiddlewareEffect;
    match value {
        "observe" => Some(MiddlewareEffect::Observe),
        "mutateArgs" => Some(MiddlewareEffect::MutateArgs),
        "requireApproval" => Some(MiddlewareEffect::RequireApproval),
        "deny" => Some(MiddlewareEffect::Deny),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_engine_runtime_middleware::{
        MiddlewareEffect, MiddlewarePhase, MiddlewareSource,
    };

    fn contribution(
        value: serde_json::Value,
    ) -> rightkit_agent_plugins::PluginMiddlewareContribution {
        serde_json::from_value(value).expect("contribution parses")
    }

    #[test]
    fn every_known_phase_and_effect_parses_and_others_do_not() {
        for (text, phase) in [
            ("sessionStart", MiddlewarePhase::SessionStart),
            ("preLlmCall", MiddlewarePhase::PreLlmCall),
            ("postLlmCall", MiddlewarePhase::PostLlmCall),
            ("preToolCall", MiddlewarePhase::PreToolCall),
            ("postToolCall", MiddlewarePhase::PostToolCall),
            ("approvalRequested", MiddlewarePhase::ApprovalRequested),
            ("subagentStart", MiddlewarePhase::SubagentStart),
            ("subagentStop", MiddlewarePhase::SubagentStop),
            ("scheduledJobStart", MiddlewarePhase::ScheduledJobStart),
            ("scheduledJobEnd", MiddlewarePhase::ScheduledJobEnd),
            ("sessionEnd", MiddlewarePhase::SessionEnd),
        ] {
            assert_eq!(parse_middleware_phase(text), Some(phase), "{text}");
        }
        assert_eq!(parse_middleware_phase("PreToolCall"), None);
        assert_eq!(parse_middleware_phase(""), None);
        for (text, effect) in [
            ("observe", MiddlewareEffect::Observe),
            ("mutateArgs", MiddlewareEffect::MutateArgs),
            ("requireApproval", MiddlewareEffect::RequireApproval),
            ("deny", MiddlewareEffect::Deny),
        ] {
            assert_eq!(parse_middleware_effect(text), Some(effect), "{text}");
        }
        assert_eq!(parse_middleware_effect("allow"), None);
    }

    #[test]
    fn a_contribution_projects_with_trimmed_fields_and_defaults() {
        let definition = plugin_middleware_definition(
            "acme",
            contribution(serde_json::json!({
                "id": "  guard ",
                "title": "   ",
                "phase": "preToolCall",
                "effect": "deny",
                "tool": "  shell  ",
                "requiresToolset": "   ",
                "reason": "  no shell ",
                "description": "d"
            })),
        )
        .expect("valid contribution projects");
        assert_eq!(definition.id, "plugin.acme.guard");
        assert_eq!(definition.title, "acme: guard");
        assert_eq!(definition.source, MiddlewareSource::LocalPlugin);
        assert_eq!(definition.phase, MiddlewarePhase::PreToolCall);
        assert_eq!(definition.effect, MiddlewareEffect::Deny);
        assert_eq!(definition.tool.as_deref(), Some("shell"));
        assert_eq!(definition.requires_toolset, None);
        assert_eq!(definition.reason.as_deref(), Some("no shell"));
        assert!(definition.enabled);
        assert_eq!(definition.order, 100);
        assert_eq!(definition.description, "d");
    }

    #[test]
    fn a_contribution_with_an_unknown_phase_effect_or_blank_id_is_dropped() {
        for value in [
            serde_json::json!({"id": "x", "phase": "never", "effect": "deny"}),
            serde_json::json!({"id": "x", "phase": "preToolCall", "effect": "maybe"}),
            serde_json::json!({"id": "  ", "phase": "preToolCall", "effect": "deny"}),
        ] {
            assert!(
                plugin_middleware_definition("acme", contribution(value.clone())).is_none(),
                "{value}"
            );
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rk-api-integrations-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_missing_or_empty_plugin_directory_yields_nothing() {
        let missing = std::env::temp_dir().join("rk-api-integrations-does-not-exist-7f3c");
        assert!(plugin_manifests_with_roots(&missing).is_empty());
        assert!(local_plugin_middleware_definitions(&missing).is_empty());
        let empty = scratch("empty");
        assert!(plugin_manifests_with_roots(&empty).is_empty());
        assert_eq!(
            provider_extension_documents(&empty).unwrap(),
            Vec::<String>::new()
        );
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn unreadable_manifests_and_stray_files_are_skipped() {
        let dir = scratch("stray");
        std::fs::write(dir.join("README.md"), "not a plugin").unwrap();
        std::fs::create_dir_all(dir.join("broken")).unwrap();
        std::fs::write(dir.join("broken").join("plugin.json"), "{ not json").unwrap();
        std::fs::create_dir_all(dir.join("no-manifest")).unwrap();
        assert!(plugin_manifests_with_roots(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
