//! `invoke_skill` — look up a named skill by slug and return its content.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub struct SkillTool;

#[async_trait::async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "invoke_skill"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        #[cfg(feature = "coding_tools-host")]
        {
            "Invoke a named skill by slug. Reads the skill definition from \
             $HOST_SKILLS_DIR/<slug>.md or ~/.host/skills/<slug>.md and returns \
             its content for the agent to act on. Returns 'Skill not found: {slug}' if missing."
        }
        // The mobile build has no env override, so its description names none.
        #[cfg(not(feature = "coding_tools-host"))]
        {
            "Invoke a named skill by slug. Reads the skill definition from ~/.rightkit/skills/<slug>.md and returns its content for the agent to act on. Returns 'Skill not found: {slug}' if missing."
        }
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["skill"],
            "properties": {
                "skill": {
                    "type": "string",
                    "description": "Slug of the skill to invoke (e.g. 'deep-research')."
                },
                "args": {
                    "description": "Optional arguments to pass to the skill."
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let slug = match args["skill"].as_str() {
            Some(s) => s,
            None => return ToolResult::error("invoke_skill: 'skill' is required"),
        };
        let Some(slug) = normalize_skill_slug(slug) else {
            return ToolResult::error("invoke_skill: invalid skill slug");
        };

        let dirs = skill_dirs(ctx);

        for path in candidate_paths(&dirs, &slug) {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(manifest) = rightkit_agent_skills::SkillManifest::from_markdown(&content)
                {
                    if !manifest.is_active() {
                        record_skill_asset_observation(&slug, &content, false);
                        return ToolResult::error(format!(
                            "invoke_skill: skill '{slug}' is not active (lifecycle: {:?})",
                            manifest.lifecycle
                        ));
                    }
                }
                record_skill_asset_observation(&slug, &content, true);
                let skill_args = &args["args"];
                return if skill_args.is_null() {
                    ToolResult::ok(format!(
                        "Skill: {slug}\nPath: {}\n\n{content}",
                        path.display()
                    ))
                } else {
                    ToolResult::ok(format!(
                        "Skill: {slug}\nPath: {}\n\n{content}\n\n---\nArgs: {skill_args}",
                        path.display()
                    ))
                };
            }
        }
        ToolResult::ok(format!("Skill not found: {slug}"))
    }
}

fn record_skill_asset_observation(slug: &str, content: &str, applied: bool) {
    rightkit_logs::agent_telemetry::record_procedural_asset_observation(
        rightkit_logs::agent_telemetry::ProceduralAssetObservationV1::from_bytes(
            slug,
            rightkit_logs::agent_telemetry::ProceduralAssetKind::Skill,
            content.as_bytes(),
            false,
            Vec::new(),
            rightkit_logs::agent_telemetry::CountObservationV1::unavailable(
                rightkit_logs::agent_telemetry::UnavailableReason::NotInstrumented,
            ),
            rightkit_logs::agent_telemetry::CountObservationV1::observed(1),
            rightkit_logs::agent_telemetry::CountObservationV1::observed(if applied {
                1
            } else {
                0
            }),
            rightkit_logs::agent_telemetry::CountObservationV1::unavailable(
                rightkit_logs::agent_telemetry::UnavailableReason::NotInstrumented,
            ),
            None,
            rightkit_logs::agent_telemetry::ProvenanceV1::from_evidence(
                "skill_tool",
                vec![format!("skill:{slug}")],
            ),
        ),
    );
}

fn normalize_skill_slug(slug: &str) -> Option<String> {
    let slug = slug.trim();
    if slug.is_empty()
        || slug.contains("..")
        || slug.contains('/')
        || slug.contains('\\')
        || !slug
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return None;
    }
    Some(slug.to_string())
}

fn skill_dirs(ctx: &ToolContext) -> Vec<PathBuf> {
    if !ctx.skill_dirs.is_empty() {
        return ctx.skill_dirs.clone();
    }
    default_skill_dirs()
}

fn default_skill_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    #[cfg(feature = "coding_tools-host")]
    {
        if let Ok(dir) = std::env::var("RIGHTKIT_SKILLS_DIR") {
            let trimmed = dir.trim();
            if !trimmed.is_empty() {
                dirs.push(PathBuf::from(trimmed));
            }
        }
    }
    if let Some(root) = rightkit_app_settings::agent_config::state_root() {
        dirs.push(root.join("skills"));
    }
    dirs
}

fn candidate_paths(dirs: &[PathBuf], slug: &str) -> Vec<PathBuf> {
    dirs.iter()
        .flat_map(|dir| skill_candidate_paths(dir, slug))
        .collect()
}

fn skill_candidate_paths(dir: &Path, slug: &str) -> Vec<PathBuf> {
    vec![
        dir.join(format!("{slug}.md")),
        dir.join(slug).join("SKILL.md"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coding_tools::tool_trait::Tool;
    use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};

    fn ctx(root: &std::path::Path, skills_dir: std::path::PathBuf) -> ToolContext {
        ToolContext::new("s1", root, PermissionMode::Bypass, WorkspaceTrust::Trusted)
            .with_skill_dirs(vec![skills_dir])
    }

    #[tokio::test]
    async fn invoke_skill_reads_context_skill_dir_file() {
        let tmp = tempfile::tempdir().unwrap();
        let skills = tmp.path().join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::write(skills.join("reviewer.md"), "# Reviewer\n\nReview code.").unwrap();

        let result = SkillTool
            .execute(
                json!({"skill": "reviewer", "args": {"focus": "correctness"}}),
                &ctx(tmp.path(), skills),
            )
            .await;

        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("# Reviewer"));
        assert!(result.content.contains("\"focus\":\"correctness\""));
    }

    #[tokio::test]
    async fn invoke_skill_reads_context_folder_skill_md() {
        let tmp = tempfile::tempdir().unwrap();
        let skills = tmp.path().join("skills");
        let skill_dir = skills.join("security");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: security\ndescription: Review security-sensitive code.\n---\n\n# Security\n\nCheck auth.",
        )
        .unwrap();

        let result = SkillTool
            .execute(json!({"skill": "security"}), &ctx(tmp.path(), skills))
            .await;

        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("# Security"));
        assert!(result.content.contains("Check auth."));
    }

    #[tokio::test]
    async fn invoke_skill_blocks_quarantined_manifest_skill() {
        let tmp = tempfile::tempdir().unwrap();
        let skills = tmp.path().join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::write(
            skills.join("candidate.md"),
            "---\nname: candidate\ndescription: Candidate skill.\nlifecycle: quarantined\n---\n\n# Candidate\n\nDo not load.",
        )
        .unwrap();

        let result = SkillTool
            .execute(json!({"skill": "candidate"}), &ctx(tmp.path(), skills))
            .await;

        assert!(result.is_error);
        assert!(result.content.contains("not active"));
        assert!(!result.content.contains("Do not load."));
    }

    #[tokio::test]
    async fn invoke_skill_rejects_path_traversal_slug() {
        let tmp = tempfile::tempdir().unwrap();
        let result = SkillTool
            .execute(
                json!({"skill": "../escape"}),
                &ctx(tmp.path(), tmp.path().join("skills")),
            )
            .await;

        assert!(result.is_error);
        assert!(result.content.contains("invalid skill slug"));
    }
}
