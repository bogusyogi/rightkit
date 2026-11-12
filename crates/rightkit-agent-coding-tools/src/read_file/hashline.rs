// `read_file` — read a file from the workspace, with optional line offset/limit.

use crate::context::ToolContext;
use crate::tool_trait::{Tool, ToolResult};
use rightkit_agent_policy::{CapabilityClass, PermissionDecision, PermissionEngine};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
use std::path::{Component, Path, PathBuf};

fn line_hash(s: &str) -> u16 {
    let mut h: u32 = 0x5811_4519;
    for b in s.bytes() {
        h = h.wrapping_mul(0x0135_7bf5).wrapping_add(b as u32);
    }
    (h & 0xFFF) as u16
}

fn format_hashline(seq: usize, content: &str) -> String {
    format!("{:>6}:{:03x}: {}", seq, line_hash(content), content)
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HashlineLineMatch {
    pub line_number: usize,
    pub start_byte: usize,
    pub content_end_byte: usize,
    pub line_end_byte: usize,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HashlineBlockMatch {
    pub start_byte: usize,
    pub end_byte: usize,
}

#[derive(Debug, Clone, Copy)]
struct HashlineAnchor<'a> {
    line_number: usize,
    hash: &'a str,
    content: &'a str,
}

fn parse_hashline_prefix(s: &str) -> Option<HashlineAnchor<'_>> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i] == b' ' {
        i += 1;
    }

    let digit_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == digit_start || i >= b.len() || b[i] != b':' {
        return None;
    }
    let line_number = s[digit_start..i].parse::<usize>().ok()?;
    i += 1;

    let hash_start = i;
    while i < b.len() && b[i].is_ascii_hexdigit() {
        i += 1;
    }
    if i.saturating_sub(hash_start) != 3 {
        return None;
    }
    if i + 1 >= b.len() || b[i] != b':' || b[i + 1] != b' ' {
        return None;
    }

    Some(HashlineAnchor {
        line_number,
        hash: &s[hash_start..i],
        content: &s[i + 2..],
    })
}

fn text_line_ranges(content: &str) -> Vec<HashlineLineMatch> {
    let mut ranges = Vec::new();
    let mut start = 0usize;

    for (idx, line_with_ending) in content.split_inclusive('\n').enumerate() {
        let line_end_byte = start + line_with_ending.len();
        let mut content_end_byte = if line_with_ending.ends_with('\n') {
            line_end_byte - 1
        } else {
            line_end_byte
        };
        if content_end_byte > start && content.as_bytes()[content_end_byte - 1] == b'\r' {
            content_end_byte -= 1;
        }
        ranges.push(HashlineLineMatch {
            line_number: idx + 1,
            start_byte: start,
            content_end_byte,
            line_end_byte,
            content: content[start..content_end_byte].to_string(),
        });
        start = line_end_byte;
    }

    if start < content.len() {
        ranges.push(HashlineLineMatch {
            line_number: ranges.len() + 1,
            start_byte: start,
            content_end_byte: content.len(),
            line_end_byte: content.len(),
            content: content[start..].to_string(),
        });
    }

    ranges
}

fn fresh_hashline_context(current: &str, focus_line: usize, context_radius: usize) -> String {
    let ranges = text_line_ranges(current);
    if ranges.is_empty() {
        return "[file is empty]".to_string();
    }

    let focus_index = focus_line.saturating_sub(1).min(ranges.len() - 1);
    let start = focus_index.saturating_sub(context_radius);
    let end = (focus_index + context_radius + 1).min(ranges.len());
    ranges[start..end]
        .iter()
        .map(|line| format_hashline(line.line_number, &line.content))
        .collect::<Vec<_>>()
        .join("\n")
}

fn stale_hashline_error(anchor: HashlineAnchor<'_>, current: &str, detail: String) -> String {
    let hint =
        crate::edit_helpers::nearest_content_diagnostic(current, anchor.content)
            .map(|hint| format!("\nNearest-content suggestion: {hint}."))
            .unwrap_or_default();
    format!(
        "Stale hashline anchor at line {}: {}. The file changed since read; re-read it and retry with fresh anchors.\nFresh anchors:\n{}{}",
        anchor.line_number,
        detail,
        fresh_hashline_context(current, anchor.line_number, 2),
        hint,
    )
}

pub(crate) fn resolve_hashline_line(
    raw_line: &str,
    current: &str,
) -> Result<Option<HashlineLineMatch>, String> {
    let Some(anchor) = parse_hashline_prefix(raw_line) else {
        return Ok(None);
    };

    let ranges = text_line_ranges(current);
    let Some(actual) = anchor
        .line_number
        .checked_sub(1)
        .and_then(|idx| ranges.get(idx))
    else {
        return Err(stale_hashline_error(
            anchor,
            current,
            format!("current file has {} line(s)", ranges.len()),
        ));
    };

    let actual_hash = format!("{:03x}", line_hash(&actual.content));
    if !anchor.hash.eq_ignore_ascii_case(&actual_hash) || anchor.content != actual.content {
        return Err(stale_hashline_error(
            anchor,
            current,
            format!(
                "expected hash {} and content {:?}, found hash {} and content {:?}",
                anchor.hash, anchor.content, actual_hash, actual.content
            ),
        ));
    }

    Ok(Some(actual.clone()))
}

pub(crate) fn resolve_hashline_block(
    block: &str,
    current: &str,
) -> Result<Option<HashlineBlockMatch>, String> {
    let lines = block.lines().collect::<Vec<_>>();
    let hashline_count = lines
        .iter()
        .filter(|line| parse_hashline_prefix(line).is_some())
        .count();
    if hashline_count == 0 {
        return Ok(None);
    }
    if hashline_count != lines.len() {
        return Err(
            "Hashline old_string mixes hashline-prefixed and plain lines; re-read the file and use either fresh hashline anchors for every line or plain exact text."
                .to_string(),
        );
    }

    let mut matches = Vec::with_capacity(lines.len());
    for line in &lines {
        let matched = resolve_hashline_line(line, current)?
            .expect("hashline_count already proved every line has a hashline prefix");
        matches.push(matched);
    }

    let first_line = matches
        .first()
        .expect("hashline_count > 0 requires at least one match")
        .line_number;
    for (offset, matched) in matches.iter().enumerate() {
        let expected_line = first_line + offset;
        if matched.line_number != expected_line {
            return Err(format!(
                "Hashline old_string spans non-contiguous lines (expected line {}, got line {}). Re-read the file and copy one contiguous block.",
                expected_line, matched.line_number
            ));
        }
    }

    let start_byte = matches.first().unwrap().start_byte;
    let end_byte = if block.ends_with('\n') {
        matches.last().unwrap().line_end_byte
    } else {
        matches.last().unwrap().content_end_byte
    };
    Ok(Some(HashlineBlockMatch {
        start_byte,
        end_byte,
    }))
}

pub(crate) fn replace_hashline_block(
    current: &str,
    matched: &HashlineBlockMatch,
    replacement: &str,
) -> String {
    let mut updated = String::with_capacity(
        current.len() - (matched.end_byte - matched.start_byte) + replacement.len(),
    );
    updated.push_str(&current[..matched.start_byte]);
    updated.push_str(replacement);
    updated.push_str(&current[matched.end_byte..]);
    updated
}
