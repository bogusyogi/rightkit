use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{get, post},
    Router,
};
use rightkit_agent_skills::{SkillCategory, SkillEffects, SkillLifecycle, SkillManifest};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::path::{Path as FsPath, PathBuf};

use crate::api::DaemonState;
use crate::api_permissions::enforce_trusted_source_authority;

/// Write the host's bundled skills ([`crate::host_options::bundled_skills`]) under the state root
/// once per process and return that folder, or `None` when the host bundles none. A new build
/// rewrites them (its digest differs from the one recorded); a restart of the same build leaves
/// them alone, so a lifecycle change a user made to one survives.
fn bundled_skills_dir() -> Option<PathBuf> {
    static DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let bundled = crate::host_options::bundled_skills();
        if bundled.is_empty() {
            return None;
        }
        let root = rightkit_agent_config::state_root()?.join("bundled-skills");
        let digest = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            bundled.hash(&mut hasher);
            format!("{:016x}", hasher.finish())
        };
        let marker = root.join(".digest");
        let current = std::fs::read_to_string(&marker).ok();
        let complete = bundled
            .iter()
            .all(|(slug, _)| root.join(slug).join("SKILL.md").is_file());
        if current.as_deref().map(str::trim) != Some(digest.as_str()) || !complete {
            for (slug, text) in bundled {
                let dir = root.join(slug);
                std::fs::create_dir_all(&dir).ok()?;
                std::fs::write(dir.join("SKILL.md"), text).ok()?;
            }
            std::fs::write(&marker, &digest).ok()?;
        }
        Some(root)
    })
    .clone()
}

pub(crate) fn default_skills_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(root) = rightkit_agent_config::state_root() {
        dirs.push(root.join("skills"));
    }
    // The shared user folders other agents also read (mcp D32): the user's own, so they are
    // scanned like the host's. A project's `.agents/skills` is never a root here (R13): see
    // `rightkit_agent_skills::discover_project_skill_candidates`.
    if crate::host_options::shared_user_skill_dirs() {
        if let Some(home) = rightkit_agent_config::os_home_dir() {
            dirs.push(rightkit_agent_skills::user_shared_skill_dir(&home));
            // The user's own Claude Code skills folder, read the same way.
            dirs.push(rightkit_agent_skills::user_claude_skill_dir(&home));
        }
    }
    if let Some(bundled) = bundled_skills_dir() {
        dirs.push(bundled);
    }
    dirs
}

pub(crate) fn skills_routes() -> Router<DaemonState> {
    Router::new()
        .route("/skills", get(list_skills_handler))
        .route("/skills/install", post(install_skill_handler))
        .route(
            "/skills/provenance-review",
            post(review_skill_provenance_handler),
        )
        .route(
            "/skills/proposals",
            get(list_skill_proposals_handler).post(create_skill_proposal_handler),
        )
        .route(
            "/skills/proposals/{id}/quarantine",
            post(quarantine_skill_proposal_handler),
        )
        .route(
            "/skills/proposals/{id}/benchmark",
            post(record_skill_proposal_benchmark_handler),
        )
        .route(
            "/skills/proposals/{id}/run-benchmark",
            post(run_skill_proposal_benchmark_handler),
        )
        .route("/workspace/skills", get(list_workspace_skills_handler))
        .route(
            "/workspace/skills/activate",
            post(activate_workspace_skill_handler),
        )
        .route(
            "/workspace/skills/deactivate",
            post(deactivate_workspace_skill_handler),
        )
        .route("/skills/{slug}/quarantine", post(quarantine_skill_handler))
        .route("/skills/{slug}/activate", post(activate_skill_handler))
        .route("/skills/{slug}/retire", post(retire_skill_handler))
        .route(
            "/skills/{slug}",
            get(read_skill_handler)
                .put(save_skill_handler)
                .delete(delete_skill_handler),
        )
}
