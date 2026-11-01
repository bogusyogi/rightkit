//! Skill trigger/effect helpers used by the session loop.
//!
//! `SessionEngine` owns when hints, denials, and observations are emitted; this
//! module turns the skills the host reports as triggered into hints, denials and
//! observations. Skill discovery, manifests and trigger evaluation belong to the
//! host and come through [`rightkit_agent_host_traits::SkillCatalog`]; with no
//! catalog installed no skill is ever triggered.

use crate::tool_policy::{extract_command_arg, extract_path_arg};
use rightkit_agent_engine_protocol::ToolCall;
use rightkit_agent_host_traits::{skill_catalog, SkillEffects, TriggerContext, TriggeredSkill};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Most skills hinted or enforced for a single event.
const MAX_TRIGGERED_SKILLS: usize = 5;

pub(crate) struct SkillToolsetCeilingObservation {
    pub(crate) skill: String,
    pub(crate) tool_call_id: String,
    pub(crate) attempted_tool: String,
    pub(crate) allowed_tools: Vec<String>,
    pub(crate) requested_tools_outside_ceiling: Vec<String>,
}

impl SkillToolsetCeilingObservation {
    pub(crate) fn fields(&self) -> HashMap<String, String> {
        HashMap::from([
            ("skill".to_string(), self.skill.clone()),
            ("toolCallId".to_string(), self.tool_call_id.clone()),
            ("attemptedTool".to_string(), self.attempted_tool.clone()),
            ("allowedTools".to_string(), self.allowed_tools.join(",")),
            (
                "requestedToolsOutsideCeiling".to_string(),
                self.requested_tools_outside_ceiling.join(","),
            ),
        ])
    }
}

pub(crate) fn triggered_skill_hint(
    skill_dirs: &[PathBuf],
    workspace_root: &Path,
    role: Option<&str>,
    event: &str,
    enrich: impl FnOnce(&mut TriggerContext),
) -> Option<String> {
    let matched = triggered_skills_for_context(skill_dirs, workspace_root, role, event, enrich);
    if matched.is_empty() {
        return None;
    }

    let mut out = format!(
        "Triggered local skills for {event}. If relevant, call `invoke_skill` with one of these slugs before proceeding:\n"
    );
    for skill in matched {
        let reason = if !skill.trigger.trim().is_empty() {
            format!("trigger: {}", skill.trigger)
        } else {
            format!("path affinity: {}", skill.path_affinity.join(", "))
        };
        out.push_str(&format!(
            "- {}: {} ({reason})\n",
            skill.slug, skill.description
        ));
        if let Some(effects) = skill_effects_summary(&skill.effects) {
            out.push_str(&format!("  effects: {effects}\n"));
        }
    }
    Some(out)
}

fn triggered_skills_for_context(
    skill_dirs: &[PathBuf],
    workspace_root: &Path,
    role: Option<&str>,
    event: &str,
    enrich: impl FnOnce(&mut TriggerContext),
) -> Vec<TriggeredSkill> {
    let mut ctx = TriggerContext::new();
    ctx.set("event", event);
    ctx.set("workspace_root", workspace_root.display().to_string());
    if let Some(role) = role {
        ctx.set("role", role);
    }
    enrich(&mut ctx);

    let Some(catalog) = skill_catalog() else {
        return Vec::new();
    };
    let mut matched = catalog.triggered_skills(skill_dirs, &ctx);
    matched.truncate(MAX_TRIGGERED_SKILLS);
    matched
}

pub(crate) fn skill_effect_denial_for_tool_call(
    skill_dirs: &[PathBuf],
    workspace_root: &Path,
    call: &ToolCall,
) -> Option<String> {
    let matched = triggered_skills_for_tool_call(skill_dirs, workspace_root, call);
    skill_effect_denial_for_matched_skills(&matched, call)
}

pub(crate) fn skill_toolset_ceiling_observations(
    skill_dirs: &[PathBuf],
    workspace_root: &Path,
    call: &ToolCall,
    available_tool_names: &HashSet<String>,
) -> Vec<SkillToolsetCeilingObservation> {
    triggered_skills_for_tool_call(skill_dirs, workspace_root, call)
        .into_iter()
        .filter_map(|skill| {
            if skill.effects.allowed_tools.is_empty() {
                return None;
            }
            let mut outside = skill
                .effects
                .allowed_tools
                .iter()
                .filter(|tool| {
                    let tool = tool.trim();
                    !tool.is_empty() && tool != "*" && !available_tool_names.contains(tool)
                })
                .cloned()
                .collect::<Vec<_>>();
            outside.sort();
            outside.dedup();
            if outside.is_empty() {
                return None;
            }
            Some(SkillToolsetCeilingObservation {
                skill: skill.slug,
                tool_call_id: call.id.clone(),
                attempted_tool: call.name.clone(),
                allowed_tools: skill.effects.allowed_tools,
                requested_tools_outside_ceiling: outside,
            })
        })
        .collect()
}

fn triggered_skills_for_tool_call(
    skill_dirs: &[PathBuf],
    workspace_root: &Path,
    call: &ToolCall,
) -> Vec<TriggeredSkill> {
    let path_arg = extract_path_arg(&call.arguments_json);
    let command_arg = extract_command_arg(&call.arguments_json);
    triggered_skills_for_context(skill_dirs, workspace_root, None, "PreToolUse", |ctx| {
        ctx.set("tool", &call.name);
        if let Some(path) = &path_arg {
            ctx.set("file_path", path);
        }
        if let Some(command) = &command_arg {
            ctx.set("command", command);
        }
        ctx.trigger_type = Some("tool_call".into());
        ctx.trigger_payload = Some(call.name.clone());
    })
}

fn skill_effect_denial_for_matched_skills(
    matched: &[TriggeredSkill],
    call: &ToolCall,
) -> Option<String> {
    if matched.is_empty() {
        return None;
    }

    for skill in matched {
        if skill
            .effects
            .disallowed_tools
            .iter()
            .any(|pattern| effect_tool_matches(pattern, &call.name))
        {
            return Some(format!(
                "skill effect denied tool '{}': matched skill '{}' disallows it",
                call.name, skill.slug
            ));
        }
    }

    if call.name == "invoke_skill" {
        return None;
    }

    let mut allowed_patterns = Vec::new();
    let mut allowed_skill_slugs = Vec::new();
    for skill in matched {
        if !skill.effects.allowed_tools.is_empty() {
            allowed_skill_slugs.push(skill.slug.clone());
            for tool in &skill.effects.allowed_tools {
                if !allowed_patterns.contains(tool) {
                    allowed_patterns.push(tool.clone());
                }
            }
        }
    }
    if allowed_patterns.is_empty()
        || allowed_patterns
            .iter()
            .any(|pattern| effect_tool_matches(pattern, &call.name))
    {
        return None;
    }

    Some(format!(
        "skill effect denied tool '{}': matched skill{} {} allow only {}",
        call.name,
        if allowed_skill_slugs.len() == 1 {
            ""
        } else {
            "s"
        },
        allowed_skill_slugs.join(","),
        allowed_patterns.join(",")
    ))
}

fn effect_tool_matches(pattern: &str, tool: &str) -> bool {
    pattern == "*" || pattern == tool
}

fn skill_effects_summary(effects: &SkillEffects) -> Option<String> {
    let mut parts = Vec::new();
    if !effects.execution_mode.trim().is_empty() {
        parts.push(format!("executionMode={}", effects.execution_mode.trim()));
    }
    if !effects.allowed_tools.is_empty() {
        parts.push(format!("allowedTools={}", effects.allowed_tools.join(",")));
    }
    if !effects.disallowed_tools.is_empty() {
        parts.push(format!(
            "disallowedTools={}",
            effects.disallowed_tools.join(",")
        ));
    }
    if !effects.prompt.trim().is_empty() {
        parts.push(format!(
            "prompt={}",
            effects.prompt.trim().chars().take(240).collect::<String>()
        ));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("; "))
    }
}
