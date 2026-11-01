//! Runtime middleware helper types and pure transformations.
//!
//! `SessionEngine` owns when middleware spans and decisions are emitted; this
//! module owns the small matching, mutation, and label helpers used by that path.

use rightkit_agent_engine_protocol::ToolCall;
use rightkit_agent_engine_runtime_middleware::{
    MiddlewareDefinition, MiddlewareEffect, MiddlewarePhase,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppliedMiddlewareDecision {
    pub(crate) middleware_id: String,
    pub(crate) title: String,
    pub(crate) effect: String,
    pub(crate) action: AppliedMiddlewareAction,
    pub(crate) reason: String,
    pub(crate) mutated_arg_keys: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppliedMiddlewareAction {
    Continue,
    MutateArgs,
    RequireApproval,
    Deny,
}

impl AppliedMiddlewareAction {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            AppliedMiddlewareAction::Continue => "continue",
            AppliedMiddlewareAction::MutateArgs => "mutateArgs",
            AppliedMiddlewareAction::RequireApproval => "requireApproval",
            AppliedMiddlewareAction::Deny => "deny",
        }
    }
}

pub(crate) fn middleware_matches_call(
    definition: &MiddlewareDefinition,
    call: &ToolCall,
    args: &serde_json::Value,
) -> bool {
    if let Some(tool) = definition.tool.as_deref() {
        if tool != call.name {
            return false;
        }
    }
    if definition.requires_toolset.is_some() {
        return false;
    }
    let Some(condition) = &definition.condition else {
        return true;
    };
    middleware_condition_matches(condition, args)
}

fn middleware_condition_matches(
    condition: &rightkit_agent_engine_runtime_middleware::MiddlewareCondition,
    args: &serde_json::Value,
) -> bool {
    let target = condition
        .argument
        .as_deref()
        .and_then(|key| args.get(key))
        .unwrap_or(args);
    if let Some(expected) = &condition.equals {
        if target != expected {
            return false;
        }
    }
    if let Some(needle) = condition.contains.as_deref() {
        let haystack = target
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| target.to_string());
        if !haystack.contains(needle) {
            return false;
        }
    }
    true
}

pub(crate) fn apply_middleware_arg_patch(
    args: &mut serde_json::Value,
    definition: &MiddlewareDefinition,
) -> Vec<String> {
    let Some(map) = args.as_object_mut() else {
        return Vec::new();
    };
    let mut keys = Vec::new();
    for (key, value) in &definition.mutate_args {
        if key.trim().is_empty() || key.contains('.') || key.contains('/') {
            continue;
        }
        map.insert(key.clone(), value.clone());
        keys.push(key.clone());
    }
    keys.sort();
    keys
}

pub(crate) fn merge_runtime_attributes(
    mut attributes: serde_json::Value,
    extra: serde_json::Value,
) -> serde_json::Value {
    let Some(extra) = extra.as_object() else {
        return attributes;
    };
    if !attributes.is_object() {
        attributes = serde_json::json!({});
    }
    if let Some(map) = attributes.as_object_mut() {
        for (key, value) in extra {
            map.insert(key.clone(), value.clone());
        }
    }
    attributes
}

pub(crate) fn middleware_phase_key(phase: MiddlewarePhase) -> &'static str {
    match phase {
        MiddlewarePhase::SessionStart => "sessionStart",
        MiddlewarePhase::PreLlmCall => "preLlmCall",
        MiddlewarePhase::PostLlmCall => "postLlmCall",
        MiddlewarePhase::PreToolCall => "preToolCall",
        MiddlewarePhase::PostToolCall => "postToolCall",
        MiddlewarePhase::ApprovalRequested => "approvalRequested",
        MiddlewarePhase::SubagentStart => "subagentStart",
        MiddlewarePhase::SubagentStop => "subagentStop",
        MiddlewarePhase::ScheduledJobStart => "scheduledJobStart",
        MiddlewarePhase::ScheduledJobEnd => "scheduledJobEnd",
        MiddlewarePhase::SessionEnd => "sessionEnd",
    }
}

pub(crate) fn middleware_effect_key(effect: MiddlewareEffect) -> &'static str {
    match effect {
        MiddlewareEffect::Observe => "observe",
        MiddlewareEffect::MutateArgs => "mutateArgs",
        MiddlewareEffect::RequireApproval => "requireApproval",
        MiddlewareEffect::Deny => "deny",
    }
}

pub(crate) fn synthetic_runtime_middleware(
    id: &str,
    title: &str,
    phase: MiddlewarePhase,
    effect: MiddlewareEffect,
) -> MiddlewareDefinition {
    MiddlewareDefinition {
        id: id.to_string(),
        title: title.to_string(),
        source: rightkit_agent_engine_runtime_middleware::MiddlewareSource::BuiltIn,
        phase,
        effect,
        tool: None,
        requires_toolset: None,
        condition: None,
        mutate_args: std::collections::BTreeMap::new(),
        reason: None,
        enabled: true,
        order: 0,
        description: String::new(),
    }
}
