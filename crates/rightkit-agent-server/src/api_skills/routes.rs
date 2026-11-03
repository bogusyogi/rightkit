use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{get, post},
    Router,
};
use rightkit_agent_host_traits::skill_host::{
    SkillCategory, SkillEffects, SkillLifecycle, SkillManifest,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::path::{Path as FsPath, PathBuf};

use crate::api::DaemonState;
use crate::api_permissions::enforce_trusted_source_authority;

pub(crate) fn default_skills_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(dir) = std::env::var("RIGHTKIT_SKILLS_DIR") {
        let trimmed = dir.trim();
        if !trimmed.is_empty() {
            dirs.push(PathBuf::from(trimmed));
        }
    }
    if let Some(root) = rightkit_app_settings::agent_config::state_root() {
        dirs.push(root.join("skills"));
    }
    let bundled = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("assets")
        .join("skills");
    if bundled.is_dir() {
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
