fn skill_item_json(item: &SkillItem) -> serde_json::Value {
    serde_json::json!({
        "slug": item.slug,
        "name": item.name,
        "description": item.description,
        "trigger": item.trigger,
        "enabled": item.enabled,
        "lifecycle": item.lifecycle,
        "active": item.enabled && item.lifecycle == SkillLifecycle::Active,
        "version": item.version,
        "author": item.author,
        "compatibility": item.compatibility,
        "platforms": item.platforms,
        "requiredEnvironmentVariables": item.required_environment_variables,
        "metadata": item.metadata,
        "category": item.category,
        "priority": item.priority,
        "disableModelInvocation": item.disable_model_invocation,
        "tags": item.tags,
        "pathAffinity": item.path_affinity,
        "effects": {
            "executionMode": &item.effects.execution_mode,
            "allowedTools": &item.effects.allowed_tools,
            "disallowedTools": &item.effects.disallowed_tools,
            "prompt": &item.effects.prompt,
        },
        "path": item.path.display().to_string(),
        "directory": item.directory.display().to_string(),
        "kind": item.kind,
        "valid": item.valid,
        "error": item.error,
    })
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

fn has_frontmatter_marker(content: &str) -> bool {
    let trimmed = content.trim_start_matches(['\n', '\r', ' ']);
    trimmed.starts_with("---") || trimmed.starts_with("+++")
}

fn fallback_skill_description(content: &str) -> String {
    content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.trim_start_matches('#').trim().to_string())
        .filter(|line| !line.is_empty())
        .unwrap_or_else(|| "Local RightKit skill".to_string())
}

fn skill_prompt_body(content: &str) -> String {
    let trimmed = content.trim_start_matches(['\n', '\r', ' ']);
    for delimiter in ["---", "+++"] {
        let Some(after_open) = trimmed.strip_prefix(delimiter) else {
            continue;
        };
        let Some(open_line_end) = after_open.find('\n') else {
            continue;
        };
        let after_open_line = &after_open[open_line_end + 1..];
        let mut offset = 0;
        for line in after_open_line.split_inclusive('\n') {
            if line.trim_end_matches(['\n', '\r']).trim() == delimiter {
                return after_open_line[offset + line.len()..].trim().to_string();
            }
            offset += line.len();
        }
    }
    content.trim().to_string()
}

fn limit_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out = text.chars().take(max_chars).collect::<String>();
    out.push_str("\n[truncated]");
    out
}

fn review_skill_provenance(source_url: &str, content: &str) -> SkillProvenanceReviewResponse {
    let mut findings = Vec::new();
    let normalized_source_url = match coderight_skills::normalize_skill_pack_github_url(source_url)
    {
        Ok(url) => {
            findings.push(SkillProvenanceFinding {
                severity: "info",
                code: "github-source-normalized",
                message: "Source URL is a supported GitHub skill-pack URL.".to_string(),
            });
            Some(url)
        }
        Err(error) => {
            findings.push(SkillProvenanceFinding {
                severity: "error",
                code: "unsupported-source-url",
                message: error.to_string(),
            });
            None
        }
    };
    let content_hash = format!(
        "sha256:{}",
        rightkit_fs::digest::sha256_bytes(content.as_bytes())
    );
    let mut manifest_name = None;
    let mut manifest_description = None;
    match SkillManifest::from_markdown(content).and_then(|manifest| {
        manifest.validate()?;
        Ok(manifest)
    }) {
        Ok(manifest) => {
            manifest_name = Some(manifest.name.clone());
            manifest_description = Some(manifest.description.clone());
            if manifest.lifecycle == SkillLifecycle::Active || manifest.enabled {
                findings.push(SkillProvenanceFinding {
                    severity: "warning",
                    code: "external-pack-should-quarantine",
                    message: "External skill packs should be installed as quarantined until reviewed and benchmarked.".to_string(),
                });
            }
            if manifest.metadata.coderight.benchmark_ids.is_empty() {
                findings.push(SkillProvenanceFinding {
                    severity: "warning",
                    code: "missing-benchmark-ids",
                    message: "No RightKit benchmark ids are declared for this skill pack."
                        .to_string(),
                });
            }
            if !manifest.required_environment_variables.is_empty() {
                findings.push(SkillProvenanceFinding {
                    severity: "info",
                    code: "requires-environment",
                    message: format!(
                        "Requires environment variables: {}. Values must not be injected into model context.",
                        manifest.required_environment_variables.join(", ")
                    ),
                });
            }
            if manifest.category == SkillCategory::Behavioral {
                findings.push(SkillProvenanceFinding {
                    severity: "info",
                    code: "behavioral-skill",
                    message: "Behavioral skill packs alter instruction composition and should be reviewed for prompt-policy conflicts.".to_string(),
                });
            }
        }
        Err(error) => findings.push(SkillProvenanceFinding {
            severity: "error",
            code: "invalid-skill-manifest",
            message: error.to_string(),
        }),
    };
    let ok = normalized_source_url.is_some()
        && !findings.iter().any(|finding| finding.severity == "error");
    SkillProvenanceReviewResponse {
        ok,
        source_url: source_url.trim().to_string(),
        normalized_source_url,
        content_hash,
        manifest_name,
        manifest_description,
        recommended_lifecycle: "quarantined".to_string(),
        findings,
    }
}

fn render_installed_skill(slug: &str, body: InstallSkillRequest) -> String {
    let name = body
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(slug);
    let description = body
        .description
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Local RightKit skill");
    let trigger = body.trigger.as_deref().map(str::trim).unwrap_or_default();
    let enabled = body.enabled.unwrap_or(true);
    let category = body.category.unwrap_or(SkillCategory::Tool);
    let priority = body.priority.unwrap_or(100);
    let tags = body.tags.unwrap_or_default();
    let tag_expr = tags
        .iter()
        .map(|tag| format!("\"{}\"", toml_escape(tag)))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "+++\nname = \"{}\"\ndescription = \"{}\"\ntrigger = \"{}\"\nenabled = {}\ncategory = \"{}\"\npriority = {}\ntags = [{}]\n+++\n\n{}",
        toml_escape(name),
        toml_escape(description),
        toml_escape(trigger),
        enabled,
        match category {
            SkillCategory::Tool => "tool",
            SkillCategory::Behavioral => "behavioral",
        },
        priority,
        tag_expr,
        body.content.trim()
    )
}

fn toml_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}
