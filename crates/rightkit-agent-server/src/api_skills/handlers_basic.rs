async fn list_skills_handler(State(state): State<DaemonState>) -> Json<serde_json::Value> {
    let skills = discover_skill_items(&state.skill_dirs);
    Json(serde_json::json!({
        "skillDirs": state.skill_dirs.iter().map(|dir| dir.display().to_string()).collect::<Vec<_>>(),
        "skills": skills.iter().map(skill_item_json).collect::<Vec<_>>(),
    }))
}

async fn read_skill_handler(
    State(state): State<DaemonState>,
    Path(slug): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let slug = normalize_skill_slug(&slug).ok_or(StatusCode::BAD_REQUEST)?;
    let (item, content) = find_skill_item(&state.skill_dirs, &slug).ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(serde_json::json!({
        "skill": skill_item_json(&item),
        "content": content,
    })))
}

async fn save_skill_handler(
    State(state): State<DaemonState>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    Json(body): Json<SaveSkillRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let slug = normalize_skill_slug(&slug).ok_or(StatusCode::BAD_REQUEST)?;
    if body.content.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let (item, _) = find_skill_item(&state.skill_dirs, &slug).ok_or(StatusCode::NOT_FOUND)?;
    ensure_skill_path_is_managed(&state.skill_dirs, &item.path)?;
    let content = if let Some(enabled) = body.enabled {
        set_skill_enabled_in_content(&body.content, enabled)
    } else {
        body.content
    };
    std::fs::write(&item.path, content).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let (item, content) = load_skill_item(&slug, &item.path, &item.directory, &item.kind)
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(serde_json::json!({
        "saved": true,
        "skill": skill_item_json(&item),
        "content": content,
    })))
}

async fn delete_skill_handler(
    State(state): State<DaemonState>,
    Path(slug): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let slug = normalize_skill_slug(&slug).ok_or(StatusCode::BAD_REQUEST)?;
    let (item, _) = find_skill_item(&state.skill_dirs, &slug).ok_or(StatusCode::NOT_FOUND)?;
    ensure_skill_path_is_managed(&state.skill_dirs, &item.path)?;
    if item.kind == "directory" {
        let skill_dir = item.path.parent().ok_or(StatusCode::BAD_REQUEST)?;
        ensure_skill_path_is_managed(&state.skill_dirs, skill_dir)?;
        std::fs::remove_dir_all(skill_dir).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    } else {
        std::fs::remove_file(&item.path).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    Ok(Json(serde_json::json!({
        "deleted": true,
        "slug": slug,
        "path": item.path.display().to_string(),
    })))
}

async fn install_skill_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(body): Json<InstallSkillRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let slug = normalize_skill_slug(&body.slug).ok_or(StatusCode::BAD_REQUEST)?;
    if body.content.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let dir = state
        .skill_dirs
        .first()
        .cloned()
        .or_else(|| default_skills_dirs().into_iter().next())
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    std::fs::create_dir_all(&dir).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let path = dir.join(format!("{slug}.md"));
    let content = render_installed_skill(&slug, body);
    std::fs::write(&path, content).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let (item, _) = load_skill_item(&slug, &path, &dir, "file").ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(serde_json::json!({
        "installed": true,
        "skillDir": dir.display().to_string(),
        "skill": skill_item_json(&item),
    })))
}

async fn review_skill_provenance_handler(
    Json(body): Json<SkillProvenanceReviewRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    let response = review_skill_provenance(&body.source_url, &body.content);
    let status = if response.ok {
        StatusCode::OK
    } else {
        StatusCode::BAD_REQUEST
    };
    (status, Json(serde_json::to_value(response).unwrap()))
}
