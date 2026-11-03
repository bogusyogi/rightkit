//! Skill manifest loading from TOML frontmatter in markdown files.

#![allow(clippy::empty_line_after_doc_comments)]
mod github_url;
mod parser;

pub use github_url::normalize_skill_pack_github_url;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

use crate::trigger::{TriggerBinder, TriggerError};
use parser::{
    default_skill_priority, extract_frontmatter, parse_gate_mode, parse_inline_list,
    parse_skill_category, parse_skill_lifecycle, split_yaml, unquote,
};

/// Errors that can occur while loading or validating a skill manifest.
#[derive(Debug, Error)]
pub enum SkillError {
    /// The frontmatter block was missing or malformed.
    #[error("frontmatter error: {0}")]
    Frontmatter(String),
    /// The frontmatter TOML failed to parse.
    #[error("toml error: {0}")]
    Toml(String),
    /// The trigger expression was malformed.
    #[error("trigger error: {0}")]
    Trigger(#[from] TriggerError),
    /// Installation or provenance error for external skill packs.
    #[error("install error: {0}")]
    Install(String),
}

/// A skill manifest. Parsed from either Anthropic-style YAML frontmatter
/// (`---` delimited — the `SKILL.md` standard) or the host app's native TOML
/// frontmatter (`+++` delimited, which adds the trigger DSL).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
#[derive(Default)]
pub struct SkillEffects {
    /// Guidance execution mode the skill wants the agent to adopt when it
    /// fires, e.g. "review", "test-first", "plan-first", or "careful".
    #[serde(default, alias = "execution_mode")]
    pub execution_mode: String,
    /// Runtime-enforced allow-list for matching PreToolUse events.
    #[serde(default, alias = "allowed_tools")]
    pub allowed_tools: Vec<String>,
    /// Runtime-enforced deny-list for matching PreToolUse events.
    #[serde(default, alias = "disallowed_tools", alias = "denied_tools")]
    pub disallowed_tools: Vec<String>,
    /// Short guidance prompt fragment to include in triggered-skill hints.
    #[serde(default)]
    pub prompt: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum SkillCategory {
    #[default]
    Tool,
    Behavioral,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum SkillLifecycle {
    Proposed,
    Quarantined,
    Benchmarked,
    Approved,
    #[default]
    Active,
    Retired,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SkillGateMode {
    Hard,
    Soft,
    Mixed,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SkillPackageMetadata {
    #[serde(default)]
    pub host: HostSkillMetadata,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HostSkillMetadata {
    #[serde(default)]
    pub category: Option<SkillCategory>,
    #[serde(default, alias = "requires_toolsets")]
    pub requires_toolsets: Vec<String>,
    #[serde(default, alias = "fallback_for_toolsets")]
    pub fallback_for_toolsets: Vec<String>,
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(default, alias = "gate_mode")]
    pub gate_mode: Option<SkillGateMode>,
    #[serde(default, alias = "benchmark_ids")]
    pub benchmark_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillManifest {
    /// The skill's unique name.
    pub name: String,
    /// A human-readable description. For Anthropic skills this is how the model
    /// decides *when* to use the skill, so it must be present and specific.
    pub description: String,
    /// The trigger DSL expression that controls when the skill fires. Empty for
    /// Anthropic `SKILL.md` skills (the model invokes them by description).
    #[serde(default)]
    pub trigger: String,
    /// Whether the skill is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Governance lifecycle. Only active skills are loaded into model context
    /// or returned by `invoke_skill`; other states remain visible to inventory
    /// and review surfaces.
    #[serde(default, alias = "status")]
    pub lifecycle: SkillLifecycle,
    /// Optional package/version provenance fields accepted from external
    /// `SKILL.md` formats.
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub compatibility: Vec<String>,
    #[serde(default)]
    pub platforms: Vec<String>,
    #[serde(
        default,
        alias = "requiredEnvironmentVariables",
        alias = "required_env",
        alias = "requiredEnv"
    )]
    pub required_environment_variables: Vec<String>,
    #[serde(default)]
    pub metadata: SkillPackageMetadata,
    /// Skill category. Tool skills are discoverable/invocable; behavioral
    /// skills compose into the session instruction layer.
    #[serde(default)]
    pub category: SkillCategory,
    /// Lower numbers compose earlier when multiple behavioral skills are active.
    #[serde(default = "default_skill_priority")]
    pub priority: i32,
    /// Prevent automatic model invocation hints/catalog entries for
    /// side-effectful skills. Direct user-requested `invoke_skill` calls can
    /// still load the skill by slug.
    #[serde(
        default,
        alias = "disableModelInvocation",
        alias = "disable-model-invocation"
    )]
    pub disable_model_invocation: bool,
    /// Free-form tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Glob patterns for file-path affinity. Runtime file-path events can use
    /// these patterns to suggest the skill without duplicating that condition
    /// in the trigger DSL.
    #[serde(default)]
    pub path_affinity: Vec<String>,
    /// Runtime effects. Tool allow/deny lists are enforced for matching
    /// PreToolUse events; execution mode and prompt remain model guidance.
    #[serde(default)]
    pub effects: SkillEffects,
    /// The markdown body — the skill's actual instructions. Populated for
    /// `SKILL.md` skills (where the body *is* the skill); empty for trigger-only
    /// native manifests.
    #[serde(default)]
    pub body: String,
}

impl SkillManifest {
    /// Parse a skill from a markdown file, auto-detecting the frontmatter style:
    /// `---` → Anthropic `SKILL.md` (YAML), `+++` → the host app native (TOML).
    pub fn from_markdown(text: &str) -> Result<SkillManifest, SkillError> {
        let trimmed = text.trim_start_matches(['\n', '\r', ' ']);
        if trimmed.starts_with("---") {
            Self::from_yaml_frontmatter(text)
        } else if trimmed.starts_with("+++") {
            Self::from_frontmatter(text)
        } else {
            Err(SkillError::Frontmatter(
                "missing frontmatter ('---' for SKILL.md, '+++' for native)".to_string(),
            ))
        }
    }

    /// Parse an Anthropic-style `SKILL.md`: YAML frontmatter between `---` lines
    /// (`name` + `description` required), with the markdown body kept as the
    /// skill's instructions. No `trigger` — the model invokes by description.
    pub fn from_yaml_frontmatter(text: &str) -> Result<SkillManifest, SkillError> {
        let (front, body) = split_yaml(text)?;
        let mut name = None;
        let mut description = None;
        let mut trigger = String::new();
        let mut enabled = true; // an authored SKILL.md is active by default
        let mut lifecycle = SkillLifecycle::Active;
        let mut version = None;
        let mut author = None;
        let mut compatibility = Vec::new();
        let mut platforms = Vec::new();
        let mut required_environment_variables = Vec::new();
        let mut metadata = SkillPackageMetadata::default();
        let mut category = SkillCategory::Tool;
        let mut priority = default_skill_priority();
        let mut disable_model_invocation = false;
        let mut tags = Vec::new();
        let mut path_affinity = Vec::new();
        let mut effects = SkillEffects::default();
        let mut section = String::new();
        for line in front.lines() {
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                continue;
            }
            let indent = line.chars().take_while(|ch| ch.is_whitespace()).count();
            let line = line.trim();
            let Some((k, v)) = line.split_once(':') else {
                continue;
            };
            let key = k.trim();
            let v = unquote(v.trim());
            if v.is_empty() {
                if section == "metadata" && key == "rightkit-agent" {
                    section = "metadata.rightkit".to_string();
                } else if indent == 0 {
                    section = key.to_string();
                }
                continue;
            }
            if section == "metadata.rightkit"
                && matches!(
                    key,
                    "category"
                        | "requires_toolsets"
                        | "requiresToolsets"
                        | "fallback_for_toolsets"
                        | "fallbackForToolsets"
                        | "gate_mode"
                        | "gateMode"
                        | "benchmark_ids"
                        | "benchmarkIds"
                )
            {
                match key {
                    "category" => metadata.host.category = Some(parse_skill_category(&v)),
                    "requires_toolsets" | "requiresToolsets" => {
                        metadata.host.requires_toolsets = parse_inline_list(&v)
                    }
                    "fallback_for_toolsets" | "fallbackForToolsets" => {
                        metadata.host.fallback_for_toolsets = parse_inline_list(&v)
                    }
                    "gate_mode" | "gateMode" => metadata.host.gate_mode = parse_gate_mode(&v),
                    "benchmark_ids" | "benchmarkIds" => {
                        metadata.host.benchmark_ids = parse_inline_list(&v)
                    }
                    _ => {}
                }
                continue;
            }
            match key {
                "name" => name = Some(v),
                "description" => description = Some(v),
                "trigger" => trigger = v,
                "enabled" => enabled = v != "false",
                "lifecycle" | "status" => lifecycle = parse_skill_lifecycle(&v),
                "version" => version = Some(v),
                "author" => author = Some(v),
                "compatibility" => compatibility = parse_inline_list(&v),
                "platforms" => platforms = parse_inline_list(&v),
                "required_environment_variables"
                | "requiredEnvironmentVariables"
                | "required_env"
                | "requiredEnv" => required_environment_variables = parse_inline_list(&v),
                "category" => category = parse_skill_category(&v),
                "priority" | "merge_priority" | "mergePriority" => {
                    priority = v
                        .parse::<i32>()
                        .unwrap_or_else(|_| default_skill_priority())
                }
                "disable_model_invocation"
                | "disableModelInvocation"
                | "disable-model-invocation" => disable_model_invocation = v == "true",
                "tags" => tags = parse_inline_list(&v),
                "path_affinity" | "pathAffinity" | "file_patterns" | "filePatterns" => {
                    path_affinity = parse_inline_list(&v)
                }
                "execution_mode" | "executionMode" => effects.execution_mode = v,
                "allowed_tools" | "allowedTools" => effects.allowed_tools = parse_inline_list(&v),
                "disallowed_tools" | "disallowedTools" | "denied_tools" | "deniedTools" => {
                    effects.disallowed_tools = parse_inline_list(&v)
                }
                "effect_prompt" | "effectPrompt" => effects.prompt = v,
                _ => {}
            }
        }
        let name = name.filter(|s| !s.is_empty()).ok_or_else(|| {
            SkillError::Frontmatter("SKILL.md frontmatter missing 'name'".to_string())
        })?;
        let description = description.filter(|s| !s.is_empty()).ok_or_else(|| {
            SkillError::Frontmatter("SKILL.md frontmatter missing 'description'".to_string())
        })?;
        Ok(SkillManifest {
            name,
            description,
            trigger,
            enabled,
            lifecycle,
            version,
            author,
            compatibility,
            platforms,
            required_environment_variables,
            metadata,
            category,
            priority,
            disable_model_invocation,
            tags,
            path_affinity,
            effects,
            body: body.trim().to_string(),
        })
    }

    /// Parse a manifest from a markdown file whose top is a TOML frontmatter
    /// block delimited by `+++` lines. The remaining body is ignored for now.
    pub fn from_frontmatter(text: &str) -> Result<SkillManifest, SkillError> {
        let frontmatter = extract_frontmatter(text)?;
        let manifest: SkillManifest =
            toml::from_str(frontmatter).map_err(|e| SkillError::Toml(e.to_string()))?;
        Ok(manifest)
    }

    /// Validate the manifest. A trigger expression, if present, must compile;
    /// an empty trigger (Anthropic `SKILL.md`) is valid (model-invoked).
    pub fn validate(&self) -> Result<(), SkillError> {
        if self.trigger.trim().is_empty() {
            return Ok(());
        }
        let binder = TriggerBinder::new();
        binder.compile(&self.trigger)?;
        Ok(())
    }

    pub fn is_active(&self) -> bool {
        self.enabled && self.lifecycle == SkillLifecycle::Active
    }
}

/// Precedence class attached to every automatically discovered skill source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillSourceScope {
    Bundled,
    User,
    WorkspaceCompatibility,
    Workspace,
}

/// One normalized skill declaration used by the unified capability index.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredSkill {
    pub manifest: SkillManifest,
    pub path: PathBuf,
    pub scope: SkillSourceScope,
}

/// Discover skill manifests from ordered roots, deduplicating by manifest
/// name. Higher-precedence scopes replace lower-precedence declarations.
/// Symlinks and paths escaping their canonical root are ignored.
pub fn discover_skill_roots(roots: &[(SkillSourceScope, PathBuf)]) -> Vec<DiscoveredSkill> {
    let mut by_name = BTreeMap::<String, DiscoveredSkill>::new();
    let mut ordered = roots.to_vec();
    ordered.sort_by_key(|(scope, _)| *scope);

    for (scope, root) in ordered {
        let Ok(canonical_root) = root.canonicalize() else {
            continue;
        };
        let mut candidates = Vec::new();
        collect_skill_candidates(&canonical_root, &canonical_root, 0, &mut candidates);
        candidates.sort();
        for path in candidates {
            let Ok(raw) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(manifest) = SkillManifest::from_markdown(&raw) else {
                continue;
            };
            if manifest.validate().is_err() {
                continue;
            }
            by_name.insert(
                manifest.name.clone(),
                DiscoveredSkill {
                    manifest,
                    path,
                    scope,
                },
            );
        }
    }

    by_name.into_values().collect()
}

fn collect_skill_candidates(
    canonical_root: &Path,
    directory: &Path,
    depth: usize,
    out: &mut Vec<PathBuf>,
) {
    if depth > 3 || out.len() >= 256 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        if out.len() >= 256 {
            break;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        let Ok(canonical) = path.canonicalize() else {
            continue;
        };
        if !canonical.starts_with(canonical_root) {
            continue;
        }
        if file_type.is_dir() {
            collect_skill_candidates(canonical_root, &canonical, depth + 1, out);
        } else if file_type.is_file()
            && (path.file_name().is_some_and(|name| name == "SKILL.md")
                || path.extension().is_some_and(|extension| extension == "md"))
        {
            out.push(canonical);
        }
    }
}

/// Validate and normalize a GitHub-hosted skill pack URL into a fetchable
/// raw URL. Row 17 follow-up: a real install flow would download the
/// `SKILL.md`, run provenance review, and stage the pack into a per-user
/// skill directory; this helper only validates the shape so the planned
/// `POST /skills/install` endpoint can short-circuit malformed URLs early.
///
/// Accepted shapes:
/// - `https://github.com/<owner>/<repo>/blob/<ref>/<path-to-skill-md>`
/// - `https://raw.githubusercontent.com/<owner>/<repo>/<ref>/<path-to-skill-md>`
///
/// Returns the raw URL on success.

#[cfg(test)]
#[cfg(test)]
#[path = "manifest_tests.rs"]
mod manifest_tests;
