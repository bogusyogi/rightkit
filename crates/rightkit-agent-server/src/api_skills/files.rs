fn skill_fires_at_session_start(
    skill: &SkillItem,
    workspace_root: &str,
    role: Option<&str>,
) -> bool {
    let trigger = skill.trigger.trim();
    if trigger.is_empty() {
        return false;
    }
    let mut ctx = rightkit_agent_host_traits::skill_host::trigger::TriggerContext::new();
    ctx.trigger_type = Some("session".to_string());
    ctx.trigger_payload = Some(workspace_root.to_string());
    ctx.set("event", "SessionStart")
        .set("trigger_type", "session")
        .set("workspace_root", workspace_root)
        .set("role", role.unwrap_or(""))
        .set("skill", &skill.slug);
    rightkit_agent_host_traits::skill_host::trigger::evaluate_trigger(trigger, &ctx)
        .unwrap_or(false)
}

fn find_skill_item(dirs: &[PathBuf], slug: &str) -> Option<(SkillItem, String)> {
    let slug = normalize_skill_slug(slug)?;
    for dir in dirs {
        let file = dir.join(format!("{slug}.md"));
        if file.is_file() {
            if let Some(found) = load_skill_item(&slug, &file, dir, "file") {
                return Some(found);
            }
        }
        let skill_md = dir.join(&slug).join("SKILL.md");
        if skill_md.is_file() {
            if let Some(found) = load_skill_item(&slug, &skill_md, dir, "directory") {
                return Some(found);
            }
        }
    }
    None
}

fn ensure_skill_path_is_managed(dirs: &[PathBuf], path: &FsPath) -> Result<(), StatusCode> {
    let target = path
        .canonicalize()
        .or_else(|_| {
            path.parent()
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))
                .and_then(|parent| parent.canonicalize())
        })
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    for dir in dirs {
        let Ok(root) = dir.canonicalize() else {
            continue;
        };
        if target.starts_with(root) {
            return Ok(());
        }
    }
    Err(StatusCode::BAD_REQUEST)
}

fn set_skill_enabled_in_content(content: &str, enabled: bool) -> String {
    set_skill_frontmatter_bool(content, "+++", "enabled", " = ", enabled)
        .or_else(|| set_skill_frontmatter_bool(content, "---", "enabled", ": ", enabled))
        .unwrap_or_else(|| {
            format!(
                "+++\nenabled = {}\n+++\n\n{}",
                enabled,
                content.trim_start()
            )
        })
}

fn set_skill_lifecycle_in_content(content: &str, lifecycle: SkillLifecycle) -> String {
    let value = skill_lifecycle_value(lifecycle);
    set_skill_frontmatter_scalar(content, "+++", "lifecycle", " = ", &format!("\"{value}\""))
        .or_else(|| set_skill_frontmatter_scalar(content, "---", "lifecycle", ": ", value))
        .or_else(|| {
            set_skill_frontmatter_scalar(content, "+++", "status", " = ", &format!("\"{value}\""))
        })
        .or_else(|| set_skill_frontmatter_scalar(content, "---", "status", ": ", value))
        .unwrap_or_else(|| {
            format!(
                "+++\nlifecycle = \"{}\"\n+++\n\n{}",
                value,
                content.trim_start()
            )
        })
}

fn skill_lifecycle_value(lifecycle: SkillLifecycle) -> &'static str {
    match lifecycle {
        SkillLifecycle::Proposed => "proposed",
        SkillLifecycle::Quarantined => "quarantined",
        SkillLifecycle::Benchmarked => "benchmarked",
        SkillLifecycle::Approved => "approved",
        SkillLifecycle::Active => "active",
        SkillLifecycle::Retired => "retired",
    }
}

fn set_skill_frontmatter_bool(
    content: &str,
    delimiter: &str,
    key: &str,
    separator: &str,
    enabled: bool,
) -> Option<String> {
    let mut lines = content.lines().collect::<Vec<_>>();
    if lines.first().map(|line| line.trim()) != Some(delimiter) {
        return None;
    }
    let close_index = lines
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(index, line)| (line.trim() == delimiter).then_some(index))?;
    let mut replaced = false;
    for line in lines.iter_mut().take(close_index).skip(1) {
        let trimmed = line.trim_start();
        if trimmed.starts_with(key)
            && trimmed[key.len()..]
                .trim_start()
                .starts_with(separator.trim())
        {
            *line = if separator.trim() == "=" {
                if delimiter == "+++" {
                    if enabled {
                        "enabled = true"
                    } else {
                        "enabled = false"
                    }
                } else if enabled {
                    "enabled: true"
                } else {
                    "enabled: false"
                }
            } else if enabled {
                "enabled: true"
            } else {
                "enabled: false"
            };
            replaced = true;
            break;
        }
    }
    if !replaced {
        let line = if delimiter == "+++" {
            if enabled {
                "enabled = true"
            } else {
                "enabled = false"
            }
        } else if enabled {
            "enabled: true"
        } else {
            "enabled: false"
        };
        lines.insert(close_index, line);
    }
    let mut out = lines.join("\n");
    if content.ends_with('\n') {
        out.push('\n');
    }
    Some(out)
}

fn set_skill_frontmatter_scalar(
    content: &str,
    delimiter: &str,
    key: &str,
    separator: &str,
    rendered_value: &str,
) -> Option<String> {
    let mut lines = content.lines().map(str::to_string).collect::<Vec<_>>();
    if lines.first().map(|line| line.trim()) != Some(delimiter) {
        return None;
    }
    let close_index = lines
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(index, line)| (line.trim() == delimiter).then_some(index))?;
    let mut replaced = false;
    for line in lines.iter_mut().take(close_index).skip(1) {
        let trimmed = line.trim_start();
        if trimmed.starts_with(key)
            && trimmed[key.len()..]
                .trim_start()
                .starts_with(separator.trim())
        {
            *line = format!("{key}{separator}{rendered_value}");
            replaced = true;
            break;
        }
    }
    if !replaced {
        lines.insert(close_index, format!("{key}{separator}{rendered_value}"));
    }
    let mut out = lines.join("\n");
    if content.ends_with('\n') {
        out.push('\n');
    }
    Some(out)
}

fn load_skill_item(
    slug: &str,
    path: &FsPath,
    dir: &FsPath,
    kind: &str,
) -> Option<(SkillItem, String)> {
    let content = std::fs::read_to_string(path).ok()?;
    let (
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
        valid,
        error,
    ) = match SkillManifest::from_markdown(&content) {
        Ok(manifest) => {
            let validation_error = manifest.validate().err().map(|e| e.to_string());
            (
                manifest.name,
                manifest.description,
                manifest.trigger,
                manifest.enabled,
                manifest.lifecycle,
                manifest.version,
                manifest.author,
                manifest.compatibility,
                manifest.platforms,
                manifest.required_environment_variables,
                manifest.metadata,
                manifest.category,
                manifest.priority,
                manifest.disable_model_invocation,
                manifest.tags,
                manifest.path_affinity,
                manifest.effects,
                validation_error.is_none(),
                validation_error,
            )
        }
        Err(e) if has_frontmatter_marker(&content) => (
            slug.to_string(),
            fallback_skill_description(&content),
            String::new(),
            true,
            SkillLifecycle::Active,
            None,
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            rightkit_agent_host_traits::skill_host::manifest::SkillPackageMetadata::default(),
            SkillCategory::Tool,
            100,
            false,
            Vec::new(),
            Vec::new(),
            SkillEffects::default(),
            false,
            Some(e.to_string()),
        ),
        Err(_) => (
            slug.to_string(),
            fallback_skill_description(&content),
            String::new(),
            true,
            SkillLifecycle::Active,
            None,
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            rightkit_agent_host_traits::skill_host::manifest::SkillPackageMetadata::default(),
            SkillCategory::Tool,
            100,
            false,
            Vec::new(),
            Vec::new(),
            SkillEffects::default(),
            true,
            None,
        ),
    };
    Some((
        SkillItem {
            slug: slug.to_string(),
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
            path: path.to_path_buf(),
            directory: dir.to_path_buf(),
            kind: kind.to_string(),
            valid,
            error,
        },
        content,
    ))
}
