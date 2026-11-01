fn observation_fields_json(fields: &HashMap<String, String>) -> serde_json::Value {
    serde_json::Value::Object(
        fields
            .iter()
            .map(|(key, value)| (key.clone(), serde_json::Value::String(value.clone())))
            .collect::<Map<String, serde_json::Value>>(),
    )
}

fn single_verification_task_id(tasks: &[FeatureTask]) -> Option<String> {
    let mut ids = tasks
        .iter()
        .filter(|task| task.foundation_phase.as_deref() == Some("verification"))
        .map(|task| task.id.clone());
    let first = ids.next()?;
    if ids.next().is_none() {
        Some(first)
    } else {
        None
    }
}

fn matching_task_id(meta: &serde_json::Value, tasks: &[FeatureTask]) -> Option<String> {
    if let Some(task_id) = string_field(meta, "taskId") {
        if tasks.iter().any(|task| task.id == task_id) {
            return Some(task_id);
        }
    }

    let task_ids = string_array(meta, "taskIds");
    if let Some(task) = tasks
        .iter()
        .find(|task| task_ids.iter().any(|id| id == &task.id))
    {
        return Some(task.id.clone());
    }

    if let Some(evidence_id) = string_field(meta, "evidenceId") {
        if let Some(task) = tasks
            .iter()
            .find(|task| task.evidence_ids.iter().any(|id| id == &evidence_id))
        {
            return Some(task.id.clone());
        }
    }

    let requirement_ids = string_array(meta, "requirementIds");
    if let Some(task) = tasks.iter().find(|task| {
        task.requirement_ids
            .iter()
            .any(|id| requirement_ids.iter().any(|candidate| candidate == id))
    }) {
        return Some(task.id.clone());
    }

    let success_criterion_ids = string_array(meta, "successCriterionIds");
    tasks
        .iter()
        .find(|task| {
            task.success_criterion_ids.iter().any(|id| {
                success_criterion_ids
                    .iter()
                    .any(|candidate| candidate == id)
            })
        })
        .map(|task| task.id.clone())
}

fn string_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|field| field.as_str())
        .map(str::trim)
        .filter(|field| !field.is_empty())
        .map(str::to_string)
}

fn string_array(value: &serde_json::Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(|field| field.as_array())
        .into_iter()
        .flatten()
        .filter_map(|field| field.as_str())
        .map(str::trim)
        .filter(|field| !field.is_empty())
        .map(str::to_string)
        .collect()
}

fn materialize_user_stories(body: &CreateSpecRequest) -> Vec<UserStory> {
    if !body.user_stories.is_empty() {
        return body
            .user_stories
            .iter()
            .enumerate()
            .map(|(index, story)| UserStory {
                id: story
                    .id
                    .clone()
                    .unwrap_or_else(|| format!("US{}", index + 1)),
                priority: story.priority.unwrap_or(2),
                title: story.title.clone(),
                narrative: story.narrative.clone(),
                independent_test: story.independent_test.clone(),
                acceptance_scenarios: story.acceptance_scenarios.clone(),
            })
            .collect();
    }

    vec![UserStory {
        id: "US1".to_string(),
        priority: 1,
        title: first_sentence(&body.description),
        narrative: body.description.clone(),
        independent_test: format!("Verify that {}", body.description.trim()),
        acceptance_scenarios: Vec::new(),
    }]
}

fn materialize_requirements(body: &CreateSpecRequest) -> Vec<Requirement> {
    if !body.functional_requirements.is_empty() {
        return body
            .functional_requirements
            .iter()
            .enumerate()
            .map(|(index, req)| Requirement {
                id: req
                    .id
                    .clone()
                    .unwrap_or_else(|| format!("FR-{:03}", index + 1)),
                text: req.text.clone(),
                source_story_id: req.source_story_id.clone(),
                needs_clarification: req.needs_clarification,
            })
            .collect();
    }

    vec![Requirement {
        id: "FR-001".to_string(),
        text: body.description.clone(),
        source_story_id: Some("US1".to_string()),
        needs_clarification: false,
    }]
}

fn materialize_success_criteria(body: &CreateSpecRequest) -> Vec<SuccessCriterion> {
    if !body.success_criteria.is_empty() {
        return body
            .success_criteria
            .iter()
            .enumerate()
            .map(|(index, criterion)| SuccessCriterion {
                id: criterion
                    .id
                    .clone()
                    .unwrap_or_else(|| format!("SC-{:03}", index + 1)),
                text: criterion.text.clone(),
                buildable: criterion.buildable,
            })
            .collect();
    }

    vec![SuccessCriterion {
        id: "SC-001".to_string(),
        text: "A focused verification run passes and is attached as evidence.".to_string(),
        buildable: true,
    }]
}

fn load_constitution_sources(workspace_root: &str) -> Vec<ConstitutionSource> {
    let mut candidates = Vec::new();
    if let Some(state) = state_dir() {
        candidates.push(("user".to_string(), state.join("constitution.md")));
    }
    // Normalize through the host filesystem before joining. Windows test and
    // API inputs can carry mixed slash styles; canonicalizing the existing
    // workspace root makes the constitution lookup independent of that syntax.
    let workspace_path = std::fs::canonicalize(FsPath::new(workspace_root))
        .unwrap_or_else(|_| PathBuf::from(workspace_root));
    candidates.push((
        "workspace".to_string(),
        workspace_path.join(".coderight").join("constitution.md"),
    ));

    candidates
        .into_iter()
        .filter_map(|(scope, path)| {
            let bytes = std::fs::read(&path).ok()?;
            Some(ConstitutionSource {
                scope,
                path: path.to_string_lossy().replace('\\', "/"),
                body_sha256: rightkit_fs::digest::sha256_bytes(&bytes),
            })
        })
        .collect()
}

fn resolve_constitution_source(
    spec: &FeatureSpec,
    source_path: Option<&str>,
    source_sha256: Option<&str>,
) -> Result<ConstitutionSource, SpecError> {
    let trimmed_path = source_path.map(str::trim).filter(|value| !value.is_empty());
    let trimmed_sha = source_sha256
        .map(str::trim)
        .filter(|value| !value.is_empty());

    if let Some(path) = trimmed_path {
        return spec
            .constitution_sources
            .iter()
            .find(|source| {
                source.path == path
                    && trimmed_sha
                        .map(|sha| source.body_sha256 == sha)
                        .unwrap_or(true)
            })
            .cloned()
            .ok_or_else(|| {
                SpecError::bad_request(
                    "constitution decision source does not match an attached constitution source",
                )
            });
    }

    if let Some(sha) = trimmed_sha {
        return spec
            .constitution_sources
            .iter()
            .find(|source| source.body_sha256 == sha)
            .cloned()
            .ok_or_else(|| {
                SpecError::bad_request(
                    "constitution decision source hash does not match an attached source",
                )
            });
    }

    match spec.constitution_sources.as_slice() {
        [single] => Ok(single.clone()),
        [] => Err(SpecError::bad_request(
            "spec has no attached constitution source to decide",
        )),
        _ => Err(SpecError::bad_request(
            "sourcePath or sourceSha256 is required when multiple constitution sources are attached",
        )),
    }
}

fn state_dir() -> Option<PathBuf> {
    rightkit_app_settings::agent_config::state_root()
}

fn first_sentence(value: &str) -> String {
    value
        .split(['.', '\n'])
        .map(str::trim)
        .find(|part| !part.is_empty())
        .unwrap_or("Untitled spec")
        .chars()
        .take(96)
        .collect()
}

fn default_true() -> bool {
    true
}
