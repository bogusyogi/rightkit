#[cfg(test)]
pub(crate) fn skill_runtime_context(
    dirs: &[PathBuf],
    workspace_root: &str,
    role: Option<&str>,
) -> Option<String> {
    skill_runtime_context_with_behavioral(dirs, workspace_root, role, None)
}

pub(crate) fn skill_runtime_context_with_behavioral(
    dirs: &[PathBuf],
    workspace_root: &str,
    role: Option<&str>,
    active_skills: Option<&[String]>,
) -> Option<String> {
    let skills = discover_skill_items(dirs)
        .into_iter()
        .filter(|skill| skill.enabled && skill.lifecycle == SkillLifecycle::Active && skill.valid)
        .collect::<Vec<_>>();
    if skills.is_empty() {
        return None;
    }

    let behavioral = skills
        .iter()
        .filter(|skill| {
            if skill.category != SkillCategory::Behavioral {
                return false;
            }
            active_skills
                .map(|active| active.iter().any(|slug| slug == &skill.slug))
                .unwrap_or(true)
        })
        .collect::<Vec<_>>();
    let invocable = skills
        .iter()
        .filter(|skill| skill.category == SkillCategory::Tool && !skill.disable_model_invocation)
        .collect::<Vec<_>>();

    let mut sections = Vec::new();
    if !behavioral.is_empty() {
        let mut ordered = behavioral;
        ordered.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| a.slug.cmp(&b.slug))
        });
        let mut section = String::from(
            "Active behavioral skill instructions. Apply these prompt-level guidelines additively, in the order shown; project-specific instructions may add stricter rules but should not erase them.",
        );
        for skill in ordered.into_iter().take(8) {
            let Ok(raw) = std::fs::read_to_string(&skill.path) else {
                continue;
            };
            let body = skill_prompt_body(&raw);
            let prompt = if body.trim().is_empty() {
                skill.effects.prompt.trim()
            } else {
                body.trim()
            };
            if prompt.is_empty() {
                continue;
            }
            section.push_str(&format!(
                "\n\n## {}\n{}",
                skill.slug,
                limit_chars(prompt, 2000)
            ));
        }
        sections.push(section);
    }

    if !invocable.is_empty() {
        let mut section = String::from(
            "Enabled local skills are available in this session. Use `invoke_skill` with the listed slug when a skill is relevant instead of guessing its full instructions.",
        );
        section.push_str("\n\nEnabled local skills:");
        for skill in invocable.iter().take(40) {
            let trigger = if skill.trigger.trim().is_empty() {
                "model-invoked".to_string()
            } else {
                format!("trigger: {}", skill.trigger)
            };
            section.push_str(&format!(
                "\n- {}: {} ({})",
                skill.slug, skill.description, trigger
            ));
            if let Some(effects) = skill_effects_summary(&skill.effects) {
                section.push_str(&format!("; effects: {effects}"));
            }
        }
        if invocable.len() > 40 {
            section.push_str(&format!(
                "\n- ... {} more skill(s) omitted from the catalog.",
                invocable.len() - 40
            ));
        }
        sections.push(section);
    }

    let mut auto_loaded = Vec::new();
    for skill in invocable
        .iter()
        .filter(|skill| {
            skill_fires_at_session_start(skill, workspace_root, role)
                || active_skills
                    .map(|active| active.iter().any(|slug| slug == &skill.slug))
                    .unwrap_or(false)
        })
        .take(8)
    {
        let Ok(raw) = std::fs::read_to_string(&skill.path) else {
            continue;
        };
        let body = skill_prompt_body(&raw);
        if body.trim().is_empty() {
            continue;
        }
        auto_loaded.push(format!(
            "## {}\n{}",
            skill.slug,
            limit_chars(body.trim(), 2000)
        ));
    }
    if !auto_loaded.is_empty() {
        let mut section = String::from("Auto-loaded skill instructions:");
        section.push('\n');
        section.push_str(&auto_loaded.join("\n\n"));
        sections.push(section);
    }

    if sections.is_empty() {
        return None;
    }
    Some(sections.join("\n\n"))
}
