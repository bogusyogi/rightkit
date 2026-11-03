//! Harness-radar intake primitives.
//!
//! The radar turns external agent-harness watchlists into normalized entries
//! and deterministic the host app backlog proposals. Network fetching and durable
//! scheduling live outside this pure module.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HarnessRadarSource {
    pub id: String,
    pub kind: String,
    pub url: String,
    pub cadence: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_entries: Option<usize>,
    #[serde(default)]
    pub required_capabilities: Vec<String>,
    #[serde(default)]
    pub required_categories: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HarnessRadarEntry {
    pub source_id: String,
    pub project_name: String,
    pub repo_url: String,
    pub category: String,
    pub tags: Vec<String>,
    pub summary: String,
    pub updated_at: Option<String>,
    pub stars_snapshot: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HarnessRadarProposal {
    pub id: String,
    pub title: String,
    pub capability: String,
    pub source_id: String,
    pub project_names: Vec<String>,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HarnessRadarValidationIssue {
    pub severity: String,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HarnessRadarValidation {
    pub source_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_updated_at: Option<String>,
    pub entry_count: usize,
    pub capability_count: usize,
    pub missing_capabilities: Vec<String>,
    pub missing_categories: Vec<String>,
    pub issues: Vec<HarnessRadarValidationIssue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HarnessRadarChange {
    pub source_id: String,
    pub project_name: String,
    pub repo_url: String,
    pub status: String,
    pub changed_fields: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_updated_at: Option<String>,
}

pub const HARNESS_RADAR_CAPABILITIES: &[&str] = &[
    "orchestration",
    "context-state",
    "execution-sandbox",
    "protocol-contracts",
    "evaluation",
    "observability",
    "guardrails",
    "reference-implementation",
];

pub fn parse_harness_radar_sources(raw: &str) -> Vec<HarnessRadarSource> {
    let mut sources = Vec::new();
    let mut current = BTreeMap::<String, String>::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "[[source]]" {
            push_source(&mut sources, &mut current);
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        current.insert(key.trim().to_string(), unquote(value.trim()));
    }
    push_source(&mut sources, &mut current);
    sources
}

pub fn import_harness_radar_yaml(source_id: &str, raw: &str) -> Vec<HarnessRadarEntry> {
    let mut rows = Vec::new();
    let mut current = BTreeMap::<String, String>::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("- ") {
            push_entry(source_id, &mut rows, &mut current);
            parse_yaml_pair(rest, &mut current);
        } else {
            parse_yaml_pair(trimmed, &mut current);
        }
    }
    push_entry(source_id, &mut rows, &mut current);
    rows
}

pub fn generate_harness_radar_proposals(
    entries: &[HarnessRadarEntry],
) -> Vec<HarnessRadarProposal> {
    let mut by_capability = BTreeMap::<String, Vec<&HarnessRadarEntry>>::new();
    for entry in entries {
        by_capability
            .entry(harness_radar_capability_for_entry(entry))
            .or_default()
            .push(entry);
    }

    let mut proposals = Vec::new();
    for (capability, mut rows) in by_capability {
        rows.sort_by(|a, b| {
            b.stars_snapshot
                .cmp(&a.stars_snapshot)
                .then_with(|| a.project_name.cmp(&b.project_name))
        });
        let source_id = rows
            .first()
            .map(|entry| entry.source_id.clone())
            .unwrap_or_default();
        let project_names = rows
            .iter()
            .take(5)
            .map(|entry| entry.project_name.clone())
            .collect::<Vec<_>>();
        let (title, rationale) = if capability == "protocol-contracts"
            && rows.iter().any(|entry| mentions_protocol(entry))
        {
            (
                "Open protocol projects increased; review host interop.".to_string(),
                "Protocol-oriented harness entries changed, so the host should re-check external gateway compatibility.".to_string(),
            )
        } else if rows.len() > 1 {
            (
                format!(
                    "Multiple reference harnesses now map to {capability}; consider a gap audit."
                ),
                format!(
                    "{} projects in this capability changed the watchlist signal.",
                    rows.len()
                ),
            )
        } else {
            (
                format!("New top project in {capability} changed since the last radar run."),
                format!(
                    "{} is the current highest-priority watchlist entry for this capability.",
                    project_names
                        .first()
                        .map(String::as_str)
                        .unwrap_or("A watched project")
                ),
            )
        };
        proposals.push(HarnessRadarProposal {
            id: format!("harness-radar-{capability}"),
            title,
            capability,
            source_id,
            project_names,
            rationale,
        });
    }
    proposals
}

pub fn validate_harness_radar_snapshot(
    source: &HarnessRadarSource,
    entries: &[HarnessRadarEntry],
) -> HarnessRadarValidation {
    let mut issues = Vec::new();
    if let Some(min_entries) = source.min_entries {
        if entries.len() < min_entries {
            issues.push(HarnessRadarValidationIssue {
                severity: "error".to_string(),
                code: "min_entries".to_string(),
                message: format!(
                    "Snapshot has {} entries but source requires at least {min_entries}.",
                    entries.len()
                ),
            });
        }
    }

    let capabilities = entries
        .iter()
        .map(harness_radar_capability_for_entry)
        .collect::<BTreeSet<_>>();
    let categories = entries
        .iter()
        .map(|entry| entry.category.clone())
        .collect::<BTreeSet<_>>();

    let missing_capabilities = source
        .required_capabilities
        .iter()
        .filter(|capability| !capabilities.contains(*capability))
        .cloned()
        .collect::<Vec<_>>();
    if !missing_capabilities.is_empty() {
        issues.push(HarnessRadarValidationIssue {
            severity: "error".to_string(),
            code: "required_capabilities".to_string(),
            message: format!(
                "Snapshot is missing required capabilities: {}.",
                missing_capabilities.join(", ")
            ),
        });
    }

    let missing_categories = source
        .required_categories
        .iter()
        .filter(|category| !categories.contains(*category))
        .cloned()
        .collect::<Vec<_>>();
    if !missing_categories.is_empty() {
        issues.push(HarnessRadarValidationIssue {
            severity: "error".to_string(),
            code: "required_categories".to_string(),
            message: format!(
                "Snapshot is missing required categories: {}.",
                missing_categories.join(", ")
            ),
        });
    }

    for entry in entries {
        if entry.repo_url.trim().is_empty() {
            issues.push(HarnessRadarValidationIssue {
                severity: "warning".to_string(),
                code: "entry_repo_url".to_string(),
                message: format!("{} is missing a repo_url.", entry.project_name),
            });
        }
        if entry.summary.trim().is_empty() {
            issues.push(HarnessRadarValidationIssue {
                severity: "warning".to_string(),
                code: "entry_summary".to_string(),
                message: format!("{} is missing a summary.", entry.project_name),
            });
        }
        if entry.updated_at.as_deref().unwrap_or("").trim().is_empty() {
            issues.push(HarnessRadarValidationIssue {
                severity: "warning".to_string(),
                code: "entry_updated_at".to_string(),
                message: format!("{} is missing updated_at.", entry.project_name),
            });
        }
    }

    let status = if issues.iter().any(|issue| issue.severity == "error") {
        "fail"
    } else if issues.is_empty() {
        "pass"
    } else {
        "warn"
    };

    HarnessRadarValidation {
        source_id: source.id.clone(),
        status: status.to_string(),
        snapshot_updated_at: source.snapshot_updated_at.clone(),
        entry_count: entries.len(),
        capability_count: capabilities.len(),
        missing_capabilities,
        missing_categories,
        issues,
    }
}

pub fn diff_harness_radar_entries(
    source_id: &str,
    previous: &[HarnessRadarEntry],
    current: &[HarnessRadarEntry],
) -> Vec<HarnessRadarChange> {
    let previous_by_key = previous
        .iter()
        .map(|entry| (entry_key(entry), entry))
        .collect::<BTreeMap<_, _>>();
    let mut changes = Vec::new();
    for entry in current {
        let key = entry_key(entry);
        let Some(previous_entry) = previous_by_key.get(&key) else {
            changes.push(HarnessRadarChange {
                source_id: source_id.to_string(),
                project_name: entry.project_name.clone(),
                repo_url: entry.repo_url.clone(),
                status: "new".to_string(),
                changed_fields: vec![
                    "projectName".to_string(),
                    "repoUrl".to_string(),
                    "category".to_string(),
                    "tags".to_string(),
                    "summary".to_string(),
                    "updatedAt".to_string(),
                    "starsSnapshot".to_string(),
                ],
                previous_updated_at: None,
                current_updated_at: entry.updated_at.clone(),
            });
            continue;
        };

        let changed_fields = changed_fields(previous_entry, entry);
        changes.push(HarnessRadarChange {
            source_id: source_id.to_string(),
            project_name: entry.project_name.clone(),
            repo_url: entry.repo_url.clone(),
            status: if changed_fields.is_empty() {
                "unchanged".to_string()
            } else {
                "changed".to_string()
            },
            changed_fields,
            previous_updated_at: previous_entry.updated_at.clone(),
            current_updated_at: entry.updated_at.clone(),
        });
    }
    changes.sort_by(|a, b| {
        change_rank(&a.status)
            .cmp(&change_rank(&b.status))
            .then_with(|| a.project_name.cmp(&b.project_name))
    });
    changes
}

fn entry_key(entry: &HarnessRadarEntry) -> String {
    if entry.repo_url.trim().is_empty() {
        entry.project_name.to_ascii_lowercase()
    } else {
        entry.repo_url.to_ascii_lowercase()
    }
}

fn changed_fields(previous: &HarnessRadarEntry, current: &HarnessRadarEntry) -> Vec<String> {
    let mut fields = Vec::new();
    if previous.project_name != current.project_name {
        fields.push("projectName".to_string());
    }
    if previous.repo_url != current.repo_url {
        fields.push("repoUrl".to_string());
    }
    if previous.category != current.category {
        fields.push("category".to_string());
    }
    if previous.summary != current.summary {
        fields.push("summary".to_string());
    }
    if previous.tags != current.tags {
        fields.push("tags".to_string());
    }
    if previous.updated_at != current.updated_at {
        fields.push("updatedAt".to_string());
    }
    if previous.stars_snapshot != current.stars_snapshot {
        fields.push("starsSnapshot".to_string());
    }
    fields
}

fn change_rank(status: &str) -> u8 {
    match status {
        "new" => 0,
        "changed" => 1,
        "unchanged" => 2,
        _ => 3,
    }
}

fn push_source(sources: &mut Vec<HarnessRadarSource>, current: &mut BTreeMap<String, String>) {
    if current.is_empty() {
        return;
    }
    let min_entries = take_optional(current, "min_entries").and_then(|value| value.parse().ok());
    sources.push(HarnessRadarSource {
        id: take_or_default(current, "id", "unknown-source"),
        kind: take_or_default(current, "kind", "unknown"),
        url: take_or_default(current, "url", ""),
        cadence: take_or_default(current, "cadence", "manual"),
        snapshot_updated_at: take_optional(current, "snapshot_updated_at"),
        min_entries,
        required_capabilities: parse_inline_list(&take_or_default(
            current,
            "required_capabilities",
            "",
        )),
        required_categories: parse_inline_list(&take_or_default(
            current,
            "required_categories",
            "",
        )),
    });
    current.clear();
}

fn push_entry(
    source_id: &str,
    rows: &mut Vec<HarnessRadarEntry>,
    current: &mut BTreeMap<String, String>,
) {
    if current.is_empty() {
        return;
    }
    let project_name = take_or_default(current, "name", "Unnamed project");
    rows.push(HarnessRadarEntry {
        source_id: source_id.to_string(),
        project_name,
        repo_url: take_or_default(current, "repo_url", ""),
        category: take_or_default(current, "category", "uncategorized"),
        tags: parse_inline_list(&take_or_default(current, "tags", "")),
        summary: take_or_default(current, "summary", ""),
        updated_at: take_optional(current, "updated_at"),
        stars_snapshot: take_optional(current, "stars_snapshot").and_then(|v| v.parse().ok()),
    });
    current.clear();
}

fn parse_yaml_pair(line: &str, current: &mut BTreeMap<String, String>) {
    let Some((key, value)) = line.split_once(':') else {
        return;
    };
    current.insert(key.trim().to_string(), unquote(value.trim()));
}

fn take_or_default(map: &mut BTreeMap<String, String>, key: &str, default: &str) -> String {
    take_optional(map, key).unwrap_or_else(|| default.to_string())
}

fn take_optional(map: &mut BTreeMap<String, String>, key: &str) -> Option<String> {
    map.remove(key).filter(|value| !value.trim().is_empty())
}

fn parse_inline_list(value: &str) -> Vec<String> {
    value
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|part| unquote(part.trim()))
        .filter(|part| !part.is_empty())
        .collect()
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    if (value.starts_with('"') && value.ends_with('"') && value.len() >= 2)
        || (value.starts_with('\'') && value.ends_with('\'') && value.len() >= 2)
    {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

pub fn harness_radar_capability_for_entry(entry: &HarnessRadarEntry) -> String {
    let haystack = format!(
        "{} {} {}",
        entry.category,
        entry.tags.join(" "),
        entry.summary
    )
    .to_ascii_lowercase();
    let capability = if haystack.contains("sandbox")
        || haystack.contains("execution")
        || haystack.contains("isolation")
    {
        "execution-sandbox"
    } else if haystack.contains("protocol") || haystack.contains("contract") {
        "protocol-contracts"
    } else if haystack.contains("eval") || haystack.contains("benchmark") {
        "evaluation"
    } else if haystack.contains("observability") || haystack.contains("trace") {
        "observability"
    } else if haystack.contains("guard") || haystack.contains("safety") {
        "guardrails"
    } else if haystack.contains("context")
        || haystack.contains("state")
        || haystack.contains("memory")
    {
        "context-state"
    } else if haystack.contains("orchestration") || haystack.contains("multi-agent") {
        "orchestration"
    } else {
        "reference-implementation"
    };
    capability.to_string()
}

fn mentions_protocol(entry: &HarnessRadarEntry) -> bool {
    let haystack = format!("{} {}", entry.tags.join(" "), entry.summary).to_ascii_lowercase();
    haystack.contains("protocol")
}
