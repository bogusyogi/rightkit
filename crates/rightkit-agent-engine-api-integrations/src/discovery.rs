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
