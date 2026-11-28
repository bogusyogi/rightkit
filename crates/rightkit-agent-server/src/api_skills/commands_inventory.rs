pub(crate) fn render_skills_command(skill_dirs: &[PathBuf]) -> String {
    let skills = discover_skill_items(skill_dirs);
    let rows = skills
        .iter()
        .map(|skill| {
            let status = if skill.valid { "valid" } else { "invalid" };
            let mut row = format!(
                "- {} [{}] {} ({})",
                skill.slug,
                status,
                skill.description,
                skill.path.display()
            );
            if let Some(error) = &skill.error {
                row.push_str(&format!(" - {error}"));
            }
            row
        })
        .collect::<Vec<_>>();
    let mut output = format!(
        "**Skills:** {} discovered\nDirectories:\n{}",
        skills.len(),
        skill_dirs
            .iter()
            .map(|dir| format!("- {}", dir.display()))
            .collect::<Vec<_>>()
            .join("\n")
    );
    if rows.is_empty() {
        output.push_str("\nNo local skills found.");
    } else {
        output.push('\n');
        output.push_str(&rows.join("\n"));
    }
    output
}

/// Each skill the model's catalogue offers (enabled, active, valid, model-invocable) with the tokens it costs
/// context when it is read, estimated as characters / 4 of its catalogue line plus its file. `/skill-doctor`.
pub(crate) fn skill_context_costs(skill_dirs: &[PathBuf]) -> Vec<(String, usize)> {
    discover_skill_items(skill_dirs)
        .into_iter()
        .filter(|skill| {
            skill.enabled
                && skill.lifecycle == SkillLifecycle::Active
                && skill.valid
                && !skill.disable_model_invocation
        })
        .map(|skill| {
            let catalogue = skill.slug.chars().count() + skill.description.chars().count();
            let body = std::fs::read_to_string(&skill.path)
                .map(|text| text.chars().count())
                .unwrap_or(0);
            (skill.slug.clone(), (catalogue + body).div_ceil(4))
        })
        .collect()
}

pub(crate) fn skill_inventory_counts(skill_dirs: &[PathBuf]) -> (usize, usize) {
    let skills = discover_skill_items(skill_dirs);
    let enabled_valid = skills
        .iter()
        .filter(|skill| skill.enabled && skill.lifecycle == SkillLifecycle::Active && skill.valid)
        .count();
    (skills.len(), enabled_valid)
}
