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

pub(super) fn parse_known_lifecycle(value: &str) -> Option<SkillLifecycle> {
    match value.trim().replace('-', "_").to_ascii_lowercase().as_str() {
        "active" => Some(SkillLifecycle::Active),
        "proposed" => Some(SkillLifecycle::Proposed),
        "quarantined" | "quarantine" => Some(SkillLifecycle::Quarantined),
        "benchmarked" => Some(SkillLifecycle::Benchmarked),
        "approved" => Some(SkillLifecycle::Approved),
        "retired" => Some(SkillLifecycle::Retired),
        _ => None,
    }
}

/// Fail closed: an unrecognised lifecycle must never load into model context.
pub(super) fn parse_skill_lifecycle(value: &str) -> SkillLifecycle {
    parse_known_lifecycle(value).unwrap_or(SkillLifecycle::Quarantined)
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

/// Join YAML block scalars (`>`, `|`, with chomp/indent markers) and quoted values that span
/// lines into one logical line each (`key: text`; a literal block keeps its newlines), so the
/// line-based frontmatter reader sees the value instead of its continuation lines.
pub(super) fn fold_yaml_multiline(front: &str) -> Vec<String> {
    fn indent_of(line: &str) -> usize {
        line.chars().take_while(|ch| ch.is_whitespace()).count()
    }
    let lines: Vec<&str> = front.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        let trimmed = line.trim();
        let Some((key, value)) = line.split_once(':') else {
            out.push(line.to_string());
            continue;
        };
        if trimmed.is_empty() || trimmed.starts_with('#') {
            out.push(line.to_string());
            continue;
        }
        let value = value.trim();
        let key_indent = indent_of(line);
        let is_block = (value.starts_with('>') || value.starts_with('|'))
            && value.len() <= 3
            && value[1..]
                .chars()
                .all(|ch| matches!(ch, '+' | '-' | '0'..='9'));
        if is_block {
            let literal = value.starts_with('|');
            let mut body: Vec<&str> = Vec::new();
            while i < lines.len() {
                let next = lines[i];
                if !next.trim().is_empty() && indent_of(next) <= key_indent {
                    break;
                }
                body.push(next);
                i += 1;
            }
            let min_indent = body
                .iter()
                .filter(|l| !l.trim().is_empty())
                .map(|l| indent_of(l))
                .min()
                .unwrap_or(0);
            let stripped: Vec<String> = body
                .iter()
                .map(|l| {
                    l.chars()
                        .skip(min_indent)
                        .collect::<String>()
                        .trim_end()
                        .to_string()
                })
                .collect();
            let text = if literal {
                stripped.join("\n")
            } else {
                let mut folded = String::new();
                for l in &stripped {
                    if l.is_empty() {
                        folded.push('\n');
                    } else {
                        if !folded.is_empty() && !folded.ends_with('\n') {
                            folded.push(' ');
                        }
                        folded.push_str(l);
                    }
                }
                folded
            };
            out.push(format!("{key}: {}", text.trim()));
            continue;
        }
        if let Some(quote) = value.chars().next().filter(|ch| matches!(*ch, '"' | '\'')) {
            if !(value.len() >= 2 && value.ends_with(quote)) {
                let mut joined = value.to_string();
                let mut j = i;
                let mut closed = false;
                while j < lines.len() {
                    let piece = lines[j].trim();
                    j += 1;
                    joined.push(' ');
                    joined.push_str(piece);
                    if piece.ends_with(quote) {
                        closed = true;
                        break;
                    }
                }
                if closed {
                    i = j;
                    out.push(format!("{key}: {joined}"));
                    continue;
                }
            }
        }
        out.push(line.to_string());
    }
    out
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
