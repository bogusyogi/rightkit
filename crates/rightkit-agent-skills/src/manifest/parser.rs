use super::{SkillCategory, SkillError, SkillGateMode, SkillLifecycle};

pub(super) fn default_skill_priority() -> i32 {
    100
}

pub(super) fn parse_skill_category(value: &str) -> SkillCategory {
    if value.eq_ignore_ascii_case("behavioral") {
        SkillCategory::Behavioral
    } else {
        SkillCategory::Tool
    }
}

pub(super) fn parse_skill_lifecycle(value: &str) -> SkillLifecycle {
    match value.trim().replace('-', "_").to_ascii_lowercase().as_str() {
        "proposed" => SkillLifecycle::Proposed,
        "quarantined" | "quarantine" => SkillLifecycle::Quarantined,
        "benchmarked" => SkillLifecycle::Benchmarked,
        "approved" => SkillLifecycle::Approved,
        "retired" => SkillLifecycle::Retired,
        _ => SkillLifecycle::Active,
    }
}

pub(super) fn parse_gate_mode(value: &str) -> Option<SkillGateMode> {
    match value.trim().replace('-', "_").to_ascii_lowercase().as_str() {
        "hard" => Some(SkillGateMode::Hard),
        "soft" => Some(SkillGateMode::Soft),
        "mixed" => Some(SkillGateMode::Mixed),
        _ => None,
    }
}

/// Strip one layer of matching surrounding quotes from a YAML scalar.
pub(super) fn unquote(s: &str) -> String {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
        || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Parse a simple inline YAML list `[a, b, c]` into its elements.
pub(super) fn parse_inline_list(s: &str) -> Vec<String> {
    s.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|t| unquote(t.trim()))
        .filter(|t| !t.is_empty())
        .collect()
}

/// Split a `---`-delimited YAML frontmatter block from the markdown body.
pub(super) fn split_yaml(text: &str) -> Result<(&str, &str), SkillError> {
    let trimmed = text.trim_start_matches(['\n', '\r', ' ']);
    let after_open = trimmed.strip_prefix("---").ok_or_else(|| {
        SkillError::Frontmatter("missing opening '---' frontmatter delimiter".to_string())
    })?;
    let after_open_line = match after_open.find('\n') {
        Some(idx) => &after_open[idx + 1..],
        None => {
            return Err(SkillError::Frontmatter(
                "missing closing '---' frontmatter delimiter".to_string(),
            ))
        }
    };
    // Find a line that is exactly `---`.
    let mut offset = 0;
    for line in after_open_line.split_inclusive('\n') {
        if line.trim_end_matches(['\n', '\r']).trim() == "---" {
            let front = &after_open_line[..offset];
            let body = &after_open_line[offset + line.len()..];
            return Ok((front, body));
        }
        offset += line.len();
    }
    Err(SkillError::Frontmatter(
        "missing closing '---' frontmatter delimiter".to_string(),
    ))
}

/// Extract the TOML frontmatter delimited by `+++` lines from a markdown file.
pub(super) fn extract_frontmatter(text: &str) -> Result<&str, SkillError> {
    // Find the opening delimiter (allowing leading whitespace/blank lines).
    let trimmed_start = text.trim_start_matches(['\n', '\r', ' ']);
    let after_open = trimmed_start.strip_prefix("+++").ok_or_else(|| {
        SkillError::Frontmatter("missing opening '+++' frontmatter delimiter".to_string())
    })?;
    // The opening +++ should be on its own line; skip to the next newline.
    let after_open_line = match after_open.find('\n') {
        Some(idx) => &after_open[idx + 1..],
        None => {
            return Err(SkillError::Frontmatter(
                "missing closing '+++' frontmatter delimiter".to_string(),
            ))
        }
    };

    // Find the closing delimiter line.
    let close_rel = find_closing_delim(after_open_line).ok_or_else(|| {
        SkillError::Frontmatter("missing closing '+++' frontmatter delimiter".to_string())
    })?;

    Ok(&after_open_line[..close_rel])
}

/// Find the byte offset of a line that consists solely of `+++` (ignoring
/// surrounding whitespace). Returns the offset where the frontmatter content
/// ends.
pub(super) fn find_closing_delim(body: &str) -> Option<usize> {
    let mut offset = 0;
    for line in body.split_inclusive('\n') {
        if line.trim_end_matches(['\n', '\r']).trim() == "+++" {
            return Some(offset);
        }
        offset += line.len();
    }
    None
}
