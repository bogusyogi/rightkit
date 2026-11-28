// Workspace skill activation (mcp R13): skills found in a trusted workspace's `.agents/skills`
// and `.claude/skills` are candidates until a user activates them. The record lives under the
// state root (`workspace-skills.json`), keyed by workspace trust key + slug + the `SKILL.md`
// content digest, so a changed file needs re-activation. Included into `surface.rs`.

/// The activation store: `<state root>/workspace-skills.json`.
pub(crate) fn workspace_skill_store_path() -> Option<PathBuf> {
    rightkit_agent_config::state_root().map(|root| root.join("workspace-skills.json"))
}

/// Stage the activated project skills of `workspace_root` for a session and return the staging
/// folder to scan like an installed one, or `None` (untrusted workspace, no store, or nothing
/// activated). Staged copies hold exactly the activated content.
pub(crate) fn stage_activated_project_skills(
    workspace_root: &FsPath,
    trusted: bool,
) -> Option<PathBuf> {
    if !trusted {
        return None;
    }
    let store_path = workspace_skill_store_path()?;
    let key = rightkit_agent_config::workspace_trust_key(&workspace_root.to_string_lossy());
    let store = rightkit_agent_skills::ActivationStore::load(&store_path);
    let stage = store_path
        .parent()?
        .join("workspace-skills-active")
        .join(rightkit_agent_skills::skill_digest(key.as_bytes()));
    match rightkit_agent_skills::materialize_activated_project_skills(
        workspace_root,
        trusted,
        &key,
        &store,
        &stage,
    ) {
        Ok(staged) => staged,
        Err(error) => {
            // Nothing is staged: a stale copy of a de-activated skill must not reach a session.
            tracing::warn!(
                stage = %stage.display(),
                error = %error,
                "could not stage the activated project skills"
            );
            None
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceSkillQuery {
    workspace_root: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceSkillAction {
    workspace_root: String,
    slug: String,
}

fn workspace_skills_trusted(state: &DaemonState, root: &str) -> bool {
    state.workspace_trust_for(root) == rightkit_agent_policy::WorkspaceTrust::Trusted
}

async fn list_workspace_skills_handler(
    State(state): State<DaemonState>,
    axum::extract::Query(query): axum::extract::Query<WorkspaceSkillQuery>,
) -> Result<Json<Value>, StatusCode> {
    let root = query.workspace_root.trim().to_string();
    if root.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let trusted = workspace_skills_trusted(&state, &root);
    let key = rightkit_agent_config::workspace_trust_key(&root);
    let store = workspace_skill_store_path()
        .map(|path| rightkit_agent_skills::ActivationStore::load(&path))
        .unwrap_or_default();
    let candidates: Vec<Value> = rightkit_agent_skills::discover_project_skill_candidates(
        FsPath::new(&root),
        trusted,
        &key,
        &store,
    )
    .into_iter()
    .map(|candidate| {
        json!({
            "slug": candidate.slug,
            "path": candidate.path.display().to_string(),
            "digest": candidate.digest,
            "activated": candidate.activated,
        })
    })
    .collect();
    Ok(Json(json!({
        "workspaceRoot": root,
        "trusted": trusted,
        "candidates": candidates,
    })))
}

async fn set_workspace_skill_activation(
    state: DaemonState,
    headers: HeaderMap,
    body: WorkspaceSkillAction,
    activate: bool,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let root = body.workspace_root.trim().to_string();
    let slug = body.slug.trim().to_string();
    if root.is_empty() || slug.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    // Project skills are repository-controlled: nothing is activated in an untrusted workspace.
    if !workspace_skills_trusted(&state, &root) {
        return Err(StatusCode::FORBIDDEN);
    }
    let path = workspace_skill_store_path().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let key = rightkit_agent_config::workspace_trust_key(&root);
    let mut store = rightkit_agent_skills::ActivationStore::load(&path);
    if activate {
        let candidates = rightkit_agent_skills::discover_project_skill_candidates(
            FsPath::new(&root),
            true,
            &key,
            &store,
        );
        let candidate = candidates
            .into_iter()
            .find(|candidate| candidate.slug == slug)
            .ok_or(StatusCode::NOT_FOUND)?;
        store.activate(&key, &slug, &candidate.digest);
        store
            .save(&path)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        return Ok(Json(json!({
            "slug": slug,
            "activated": true,
            "digest": candidate.digest,
            "appliesTo": "sessions created after this call",
        })));
    }
    let was_active = store.deactivate(&key, &slug);
    store
        .save(&path)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(
        json!({ "slug": slug, "activated": false, "wasActivated": was_active }),
    ))
}

async fn activate_workspace_skill_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(body): Json<WorkspaceSkillAction>,
) -> Result<Json<Value>, StatusCode> {
    set_workspace_skill_activation(state, headers, body, true).await
}

async fn deactivate_workspace_skill_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(body): Json<WorkspaceSkillAction>,
) -> Result<Json<Value>, StatusCode> {
    set_workspace_skill_activation(state, headers, body, false).await
}
