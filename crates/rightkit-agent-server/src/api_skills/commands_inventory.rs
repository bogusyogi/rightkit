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

pub(crate) fn skill_inventory_counts(skill_dirs: &[PathBuf]) -> (usize, usize) {
    let skills = discover_skill_items(skill_dirs);
    let enabled_valid = skills
        .iter()
        .filter(|skill| skill.enabled && skill.lifecycle == SkillLifecycle::Active && skill.valid)
        .count();
    (skills.len(), enabled_valid)
}
