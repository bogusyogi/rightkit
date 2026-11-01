use crate::MemoryEntry;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DreamAgentPolicy {
    pub model: String,
    pub allowed_tools: Vec<String>,
    pub shell_allowed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DreamConsolidatedMemory {
    /// The PRIMARY entry's own id — consolidation updates it in place. (The old
    /// generated `dream-<slug>` ids churned stable ids on every date rewrite,
    /// could collide across distinct memories sharing a 12-word prefix, and
    /// dropped the primary's scope. Removed 2026-07-04.)
    pub id: String,
    pub content: String,
    pub keywords: Vec<String>,
    pub source_ids: Vec<String>,
    /// Preserved from the primary so an in-place write never re-scopes the row.
    pub scope_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DreamPlan {
    pub consolidated: Vec<DreamConsolidatedMemory>,
    /// Exact-duplicate secondaries absorbed into a primary during consolidation.
    /// These are reversibly quarantined (reason `duplicate_consolidated`), never
    /// deleted outright — quarantine-before-destructive is a locked invariant.
    /// The name intentionally no longer says "pruned": nothing here is destroyed
    /// in this pass.
    pub duplicate_quarantine_ids: Vec<String>,
    /// Low-score / never-exposed / expired entries, reversibly quarantined with
    /// reason `low_effectiveness`.
    pub quarantined_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DreamStatus {
    pub agent_id: String,
    pub status: String,
    pub model: String,
    pub shell_allowed: bool,
    pub read_count: usize,
    pub consolidated_count: usize,
    /// Rows permanently destroyed in this pass. Duplicate consolidation no
    /// longer deletes anything, so this is 0 unless a future destructive path
    /// is added — never repurpose it to count quarantines.
    pub pruned_count: usize,
    /// Low-score / expired quarantines (reason `low_effectiveness`).
    pub quarantined_count: usize,
    /// Exact-duplicate quarantines (reason `duplicate_consolidated`), counted
    /// separately from `quarantined_count` so receipts distinguish why a row
    /// left active recall.
    pub duplicate_quarantined_count: usize,
}

impl DreamAgentPolicy {
    pub fn restricted() -> Self {
        Self {
            // Consolidation is deterministic local Rust code. Do not attribute it to an LLM.
            model: String::new(),
            allowed_tools: vec![
                "memory.read".to_string(),
                "memory.write".to_string(),
                "remember_consolidated".to_string(),
            ],
            shell_allowed: false,
        }
    }
}

pub fn consolidate_dream_memories(entries: &[MemoryEntry], today: &str) -> DreamPlan {
    let mut groups: BTreeMap<String, Vec<&MemoryEntry>> = BTreeMap::new();
    for entry in entries {
        let source_date = entry.created_at.get(..10).unwrap_or(today);
        let normalized_content = normalize_relative_dates(&entry.content, source_date);
        groups
            .entry(format!(
                "{}\u{0}{}",
                entry.scope_id,
                dedup_key(&normalized_content)
            ))
            .or_default()
            .push(entry);
    }

    let mut consolidated = Vec::new();
    let mut duplicate_quarantine_ids = BTreeSet::new();
    let mut quarantined_ids = BTreeSet::new();
    for (_key, mut group) in groups {
        group.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.access_count.cmp(&a.access_count))
                .then_with(|| a.id.cmp(&b.id))
        });

        let primary = group[0];
        let source_date = primary.created_at.get(..10).unwrap_or(today);
        let normalized_content = normalize_relative_dates(&primary.content, source_date);
        let has_relative_dates = normalized_content != primary.content;
        let is_duplicate = group.len() > 1;
        if is_duplicate || has_relative_dates {
            let source_ids = group
                .iter()
                .map(|entry| entry.id.clone())
                .collect::<Vec<_>>();
            consolidated.push(DreamConsolidatedMemory {
                id: primary.id.clone(),
                content: normalized_content.trim().to_string(),
                keywords: merged_keywords(&group),
                source_ids,
                scope_id: primary.scope_id.clone(),
            });
            // Only the non-primary duplicates are absorbed; the primary is updated
            // in place under its own id. The absorbed secondaries are reversibly
            // quarantined by the caller, never deleted here.
            for source in group.iter().skip(1) {
                duplicate_quarantine_ids.insert(source.id.clone());
            }
        }
    }

    // Low-value quarantine — but never quarantine an id that consolidation just
    // chose as a write target (a low-score primary is being refreshed, not
    // discarded).
    let targets: BTreeSet<&str> = consolidated.iter().map(|c| c.id.as_str()).collect();
    for entry in entries {
        if entry.score < 0.2 && entry.access_count == 0 && !targets.contains(entry.id.as_str()) {
            quarantined_ids.insert(entry.id.clone());
        }
    }

    DreamPlan {
        consolidated,
        duplicate_quarantine_ids: duplicate_quarantine_ids.into_iter().collect(),
        quarantined_ids: quarantined_ids.into_iter().collect(),
    }
}

fn merged_keywords(entries: &[&MemoryEntry]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    for entry in entries {
        for keyword in &entry.keywords {
            let keyword = keyword.trim().to_lowercase();
            if !keyword.is_empty() {
                seen.insert(keyword);
            }
        }
    }
    seen.into_iter().take(12).collect()
}

/// Stage 0's single notion of "same content": lowercase, strip every
/// non-alphanumeric character to whitespace, collapse runs.  Stage 1's
/// deterministic analyzer ([`crate::review`]) reuses this exact function so
/// there is one duplicate notion in Cortex, not two divergent ones.
pub(crate) fn dedup_key(content: &str) -> String {
    content
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_relative_dates(content: &str, today: &str) -> String {
    let Some(today) = parse_date(today) else {
        return content.to_string();
    };
    let replacements = [
        ("yesterday", format_date(today.add_days(-1))),
        ("today", format_date(today)),
        ("tomorrow", format_date(today.add_days(1))),
        ("last week", format_date(today.add_days(-7))),
        ("next week", format_date(today.add_days(7))),
    ];
    replace_case_insensitive(content, &replacements)
}

fn replace_case_insensitive(input: &str, replacements: &[(&str, String)]) -> String {
    let mut out = input.to_string();
    for (needle, replacement) in replacements {
        let mut cursor = 0usize;
        loop {
            let haystack = out[cursor..].to_lowercase();
            let Some(pos) = haystack.find(needle) else {
                break;
            };
            let start = cursor + pos;
            let end = start + needle.len();
            if is_word_boundary(&out, start, end) {
                out.replace_range(start..end, replacement);
                cursor = start + replacement.len();
            } else {
                cursor = end;
            }
            if cursor >= out.len() {
                break;
            }
        }
    }
    out
}

fn is_word_boundary(s: &str, start: usize, end: usize) -> bool {
    let before = s[..start].chars().next_back();
    let after = s[end..].chars().next();
    !before.map(|c| c.is_ascii_alphanumeric()).unwrap_or(false)
        && !after.map(|c| c.is_ascii_alphanumeric()).unwrap_or(false)
}

fn parse_date(s: &str) -> Option<rightkit_time::Date> {
    let mut parts = s.split('-');
    let year = parts.next()?.parse().ok()?;
    let month = parts.next()?.parse().ok()?;
    let day = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    rightkit_time::Date::new(year, month, day)
}

fn format_date(date: rightkit_time::Date) -> String {
    format!("{:04}-{:02}-{:02}", date.year, date.month, date.day)
}
