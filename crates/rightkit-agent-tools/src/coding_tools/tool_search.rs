//! `tool_search` — let the agent discover available tools.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::registry::CapabilityDescriptor;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

pub const TOOL_SEARCH_DESCRIPTION: &str =
    "Discover available tools with a ranked shortlist from case-insensitive substring matching over tool names and descriptions.";

const SHORTLIST_MIN: usize = 5;
const SHORTLIST_DEFAULT: usize = 7;
const SHORTLIST_MAX: usize = 9;
/// Byte cap on the no-query catalog index so an uncapped catalog can never
/// dump itself wholesale into the prompt.
const CATALOG_DUMP_MAX_BYTES: usize = 2048;
const QUERY_RESULT_MAX_BYTES: usize = 8192;
const MAX_QUERY_BYTES: usize = 1024;

pub struct ToolSearchTool {
    descriptors: Vec<CapabilityDescriptor>,
    detailed: bool,
}

fn placeholder_descriptor(name: String, description: String) -> CapabilityDescriptor {
    let input_schema = json!({"type": "object"});
    CapabilityDescriptor {
        id: format!("tool:{name}"),
        name,
        description,
        schema_digest: crate::coding_tools::registry::capability_schema_digest(&input_schema),
        input_schema,
        read_only: false,
        availability: "available".to_string(),
    }
}

impl ToolSearchTool {
    pub fn new(tools: Vec<(String, String)>) -> Self {
        Self::from_descriptors(
            tools
                .into_iter()
                .map(|(name, description)| placeholder_descriptor(name, description))
                .collect(),
            false,
        )
    }

    pub(crate) fn from_descriptors(descriptors: Vec<CapabilityDescriptor>, detailed: bool) -> Self {
        Self {
            descriptors,
            detailed,
        }
    }

    pub(crate) fn self_descriptor() -> CapabilityDescriptor {
        let input_schema = tool_search_input_schema();
        CapabilityDescriptor {
            id: "tool:tool_search".to_string(),
            name: "tool_search".to_string(),
            description: TOOL_SEARCH_DESCRIPTION.to_string(),
            schema_digest: crate::coding_tools::registry::capability_schema_digest(&input_schema),
            input_schema,
            read_only: true,
            availability: "available".to_string(),
        }
    }

    pub fn with_defaults() -> Self {
        let defaults = vec![
            ("read_file", "Read the contents of a file."),
            ("write_file", "Write content to a file."),
            ("apply_patch", "Apply a custom patch to files."),
            ("shell", "Execute a shell command."),
            ("search", "Search for a pattern in files."),
            ("git_read", "Read git repository status and log."),
            ("git_diff", "Show a unified diff of working tree changes."),
            (
                "browser_fetch",
                "Fetch a URL and return readable text content.",
            ),
            ("web_search", "Search the web for a query."),
            (
                "tool_search",
                "Discover available tools by name or description.",
            ),
            ("attach_verdict", "Attach a trust verdict to an artifact."),
            (
                "finalize_artifact",
                "Mark an artifact as finalized/shippable.",
            ),
            ("github", "Interact with GitHub (PRs, files, comments)."),
        ];
        Self::new(
            defaults
                .into_iter()
                .map(|(name, description)| (name.to_string(), description.to_string()))
                .collect(),
        )
    }

    /// Bounded substring search. Matches are scored by lexical tiers so the
    /// adaptive shortlist applies, and the (score, name) ordering is
    /// deterministic across calls.
    fn substring_indices(&self, query: &str) -> Vec<usize> {
        let normalized_query = normalize_text(query);
        let query_lower = query.to_lowercase();
        let mut matched: Vec<(usize, f32)> = Vec::new();
        // Multi-word queries ("git history diff") match per term: a tool
        // scores by the share of terms its name or description contains,
        // plus the whole-phrase bonus.
        let terms: Vec<String> = query_lower
            .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
            .filter(|term| term.len() >= 2)
            .map(str::to_string)
            .collect();
        for (index, descriptor) in self.descriptors.iter().enumerate() {
            let name = descriptor.name.to_lowercase();
            let description = descriptor.description.to_lowercase();
            let phrase = name.contains(&query_lower) || description.contains(&query_lower);
            let hits = terms
                .iter()
                .filter(|term| name.contains(term.as_str()) || description.contains(term.as_str()))
                .count();
            if phrase || hits > 0 {
                let coverage = if terms.is_empty() {
                    0.0
                } else {
                    hits as f32 / terms.len() as f32
                };
                let score = coverage
                    + lexical_bonus(&normalized_query, &descriptor.name, &descriptor.description);
                matched.push((index, score));
            }
        }
        matched.sort_by(|left, right| {
            right
                .1
                .partial_cmp(&left.1)
                .unwrap_or(Ordering::Equal)
                .then_with(|| {
                    self.descriptors[left.0]
                        .name
                        .cmp(&self.descriptors[right.0].name)
                })
        });
        let scores: Vec<f32> = matched.iter().map(|(_, score)| *score).collect();
        matched.truncate(adaptive_shortlist_len(&scores));
        matched.into_iter().map(|(index, _)| index).collect()
    }

    fn result_values(&self, indices: impl IntoIterator<Item = usize>) -> Vec<Value> {
        indices
            .into_iter()
            .map(|index| {
                let descriptor = &self.descriptors[index];
                if self.detailed {
                    json!({
                        "name": descriptor.name,
                        "description": descriptor.description,
                        "capability": descriptor.id,
                        "schema_digest": descriptor.schema_digest,
                        "input_schema": descriptor.input_schema,
                        "read_only": descriptor.read_only,
                        "availability": descriptor.availability,
                    })
                } else {
                    // Discovery must retain the tool name even when its live
                    // description contains a large generated API (run_program).
                    // The complete description ships with the promoted schema.
                    let description = if descriptor.description.len() > 1024 {
                        format!(
                            "{}\n[description truncated; full description in promoted tool schema]",
                            truncate_str_utf8_safe(&descriptor.description, 1024)
                        )
                    } else {
                        descriptor.description.clone()
                    };
                    json!({
                        "name": descriptor.name,
                        "description": description,
                    })
                }
            })
            .collect()
    }
}

fn tool_search_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": { "type": "string", "description": "Optional natural-language description of the capability needed" }
        }
    })
}

fn normalize_text(text: &str) -> String {
    text.to_lowercase().replace(['_', '-'], " ")
}

fn lexical_bonus(normalized_query: &str, name: &str, description: &str) -> f32 {
    let normalized_name = normalize_text(name);
    let normalized_description = description.to_lowercase();
    if normalized_query == normalized_name {
        0.30
    } else if normalized_query.contains(&normalized_name)
        || normalized_name.contains(normalized_query)
    {
        0.15
    } else if normalized_description.contains(normalized_query) {
        0.08
    } else {
        0.0
    }
}

fn adaptive_shortlist_len(sorted_scores: &[f32]) -> usize {
    if sorted_scores.len() <= SHORTLIST_DEFAULT {
        return sorted_scores.len();
    }
    let top = sorted_scores[0];
    let fifth = sorted_scores[SHORTLIST_MIN - 1];
    let seventh = sorted_scores[SHORTLIST_DEFAULT - 1];
    if top - fifth >= 0.12 {
        SHORTLIST_MIN
    } else if top - seventh <= 0.035 {
        SHORTLIST_MAX.min(sorted_scores.len())
    } else {
        SHORTLIST_DEFAULT
    }
}

#[async_trait::async_trait]
impl Tool for ToolSearchTool {
    fn name(&self) -> &str {
        "tool_search"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        TOOL_SEARCH_DESCRIPTION
    }

    fn input_schema(&self) -> Value {
        tool_search_input_schema()
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let query = match args.get("query") {
            None | Some(Value::Null) => None,
            Some(Value::String(q)) => Some(q.trim()),
            Some(_) => {
                return ToolResult::error("invalid_argument: 'query' must be a string");
            }
        };
        if query.is_some_and(|q| q.len() > MAX_QUERY_BYTES) {
            return ToolResult::error(format!(
                "invalid_argument: 'query' exceeds {MAX_QUERY_BYTES} bytes"
            ));
        }
        match query {
            None | Some("") => ToolResult::ok(compact_catalog_index(&self.descriptors)),
            Some(query) => {
                let indices = self.substring_indices(query);
                let results = self.result_values(indices);
                let cap = if self.detailed {
                    QUERY_RESULT_MAX_BYTES
                } else {
                    CATALOG_DUMP_MAX_BYTES
                };
                ToolResult::ok(serialize_bounded(results, cap))
            }
        }
    }
}

fn truncation_marker(omitted: usize, original_bytes: usize, cap_bytes: usize) -> Value {
    json!({
        "truncated": true,
        "omitted": omitted,
        "original_bytes": original_bytes,
        "cap_bytes": cap_bytes
    })
}

/// Serialize a JSON array of entries under `cap_bytes` with a
/// typed truncation marker. Both queried and no-query paths share this
/// exact byte-limit semantics: the cap bounds the *final* serialized string
/// including the marker. Truncation drops whole entries so the byte cut is
/// UTF-8 safe (no slicing inside a code point). Ordering is caller-owned
/// and preserved.
fn serialize_bounded(entries: Vec<Value>, cap_bytes: usize) -> String {
    // Original size before any truncation — stated in the marker on overrun.
    let original_bytes = serde_json::to_string(&entries).unwrap_or_default().len();
    if original_bytes <= cap_bytes {
        return serde_json::to_string(&entries).unwrap_or_default();
    }
    // Overrun: reserve marker bytes before filling. We hard-truncate by
    // popping whole entries and re-measuring with the marker included each
    // iteration (marker size varies with digit width of `omitted` and
    // `original_bytes`), so the final string never exceeds the cap.
    let mut truncated = entries;
    let mut omitted = 0usize;
    loop {
        // Pop one entry per iteration; omitted counts total dropped.
        if truncated.is_empty() {
            let marker = truncation_marker(omitted, original_bytes, cap_bytes);
            let candidate = serde_json::to_string(&vec![marker]).unwrap_or_default();
            // If even the marker alone exceeds cap (only possible with tiny cap),
            // hard-truncate the serialized marker at a UTF-8-safe boundary.
            if candidate.len() <= cap_bytes {
                return candidate;
            }
            return truncate_str_utf8_safe(&candidate, cap_bytes);
        }
        truncated.pop();
        omitted += 1;
        let marker = truncation_marker(omitted, original_bytes, cap_bytes);
        let mut with_marker = truncated.clone();
        with_marker.push(marker);
        let candidate = serde_json::to_string(&with_marker).unwrap_or_default();
        if candidate.len() <= cap_bytes {
            return candidate;
        }
        // Continue popping; loop re-measures next iteration.
    }
}

fn truncate_str_utf8_safe(s: &str, cap: usize) -> String {
    if s.len() <= cap {
        return s.to_string();
    }
    let mut end = cap;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Compact, categorized replacement for the old pretty-printed catalog dump:
/// one `{category, name}` entry per tool, descriptions omitted (a follow-up
/// query retrieves them), ordered by category then name, serialized compact
/// and truncated under `CATALOG_DUMP_MAX_BYTES` with an explicit marker.
fn compact_catalog_index(descriptors: &[CapabilityDescriptor]) -> String {
    let mut categories: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for descriptor in descriptors {
        let category = descriptor
            .name
            .split('_')
            .next()
            .unwrap_or(&descriptor.name)
            .to_string();
        categories
            .entry(category)
            .or_default()
            .insert(descriptor.name.clone());
    }
    let mut entries: Vec<Value> = Vec::new();
    for (category, names) in &categories {
        for name in names {
            entries.push(json!({"category": category, "name": name}));
        }
    }
    serialize_bounded(entries, CATALOG_DUMP_MAX_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
    use tempfile::tempdir;

    fn ctx() -> ToolContext {
        let dir = tempdir().unwrap();
        ToolContext::new(
            "s1",
            dir.path(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        )
    }

    fn descriptors_of(tools: Vec<(String, String)>) -> Vec<CapabilityDescriptor> {
        tools
            .into_iter()
            .map(|(name, description)| placeholder_descriptor(name, description))
            .collect()
    }

    fn fixture_tools() -> Vec<(String, String)> {
        [
            ("view_image", "Inspect an image file."),
            ("read_file", "Read file contents."),
            ("write_file", "Write a file."),
            ("shell", "Execute a command."),
            ("search", "Find a pattern in files."),
            ("git_diff", "Show source changes."),
            ("web_search", "Find information online."),
            ("ask_user", "Request clarification."),
            ("repl", "Run code interactively."),
            ("schedule_cron", "Schedule recurring work."),
        ]
        .into_iter()
        .map(|(name, description)| (name.to_string(), description.to_string()))
        .collect()
    }

    #[tokio::test]
    async fn tool_search_returns_all_tools_without_query() {
        let tool = ToolSearchTool::with_defaults();
        let result = tool.execute(json!({}), &ctx()).await;
        assert!(!result.is_error);
        let tools: Value = serde_json::from_str(&result.content).unwrap();
        assert!(tools.as_array().unwrap().len() >= 13);
    }

    #[tokio::test]
    async fn finds_tools_by_name_or_description_keyword() {
        let tool = ToolSearchTool::new(fixture_tools());
        for query in ["schedule_cron", "recurring"] {
            let result = tool.execute(json!({"query": query}), &ctx()).await;
            assert!(!result.is_error);
            let tools: Value = serde_json::from_str(&result.content).unwrap();
            assert_eq!(tools[0]["name"], "schedule_cron", "query {query}");
        }
    }

    #[tokio::test]
    async fn invalid_query_is_typed_error() {
        let tool = ToolSearchTool::new(fixture_tools());
        let result = tool.execute(json!({"query": 42}), &ctx()).await;
        assert!(result.is_error);
        assert!(result.content.starts_with("invalid_argument:"));
        let long = "a".repeat(MAX_QUERY_BYTES + 1);
        let result = tool.execute(json!({"query": long}), &ctx()).await;
        assert!(result.is_error && result.content.starts_with("invalid_argument:"));
    }

    #[tokio::test]
    async fn fallback_no_match_returns_empty_array() {
        let tool = ToolSearchTool::new(fixture_tools());
        let result = tool.execute(json!({"query": "zzznomatch"}), &ctx()).await;
        assert!(!result.is_error);
        let tools: Value = serde_json::from_str(&result.content).unwrap();
        assert!(tools.as_array().unwrap().is_empty());
    }

    #[test]
    fn shortlist_adapts_to_score_separation() {
        assert_eq!(adaptive_shortlist_len(&[0.9, 0.7, 0.7]), 3);
        assert_eq!(
            adaptive_shortlist_len(&[0.9, 0.85, 0.84, 0.82, 0.70, 0.69, 0.68, 0.67]),
            SHORTLIST_MIN
        );
        assert_eq!(
            adaptive_shortlist_len(&[0.9, 0.88, 0.86, 0.84, 0.83, 0.82, 0.80, 0.7]),
            SHORTLIST_DEFAULT
        );
        assert_eq!(
            adaptive_shortlist_len(&[0.9, 0.89, 0.88, 0.88, 0.87, 0.87, 0.87, 0.86]),
            8
        );
    }

    #[tokio::test]
    async fn substring_fallback_is_bounded_by_shortlist_max() {
        // 30 tools whose descriptions all contain the query word: the old
        // fallback returned every one of them into the prompt.
        let tools: Vec<(String, String)> = (0..30)
            .map(|i| {
                (
                    format!("tool_{i:02}"),
                    "Handles widget operations.".to_string(),
                )
            })
            .collect();
        let tool = ToolSearchTool::new(tools);
        let result = tool.execute(json!({"query": "widget"}), &ctx()).await;
        assert!(!result.is_error);
        let tools: Value = serde_json::from_str(&result.content).unwrap();
        assert!(tools.as_array().unwrap().len() <= SHORTLIST_MAX);
    }

    #[tokio::test]
    async fn substring_fallback_order_is_deterministic() {
        let tools: Vec<(String, String)> = (0..12)
            .map(|i| {
                (
                    format!("tool_{i:02}"),
                    "Handles widget operations.".to_string(),
                )
            })
            .collect();
        let tool = ToolSearchTool::new(tools);
        let first = tool.execute(json!({"query": "widget"}), &ctx()).await;
        let second = tool.execute(json!({"query": "widget"}), &ctx()).await;
        assert_eq!(first.content, second.content);
    }

    #[tokio::test]
    async fn no_query_returns_compact_categorized_index() {
        let tool = ToolSearchTool::with_defaults();
        let result = tool.execute(json!({}), &ctx()).await;
        assert!(!result.is_error);
        let entries: Value = serde_json::from_str(&result.content).unwrap();
        let entries = entries.as_array().unwrap();
        assert!(entries.len() >= 13);
        // Every entry is categorized; descriptions never appear in the index.
        for entry in entries {
            assert!(entry["category"].is_string());
            assert!(entry["name"].is_string());
            assert!(entry.get("description").is_none());
        }
        // git_read and git_diff share the `git` category.
        let git_names: Vec<&str> = entries
            .iter()
            .filter(|entry| entry["category"] == "git")
            .filter_map(|entry| entry["name"].as_str())
            .collect();
        assert_eq!(git_names, vec!["git_diff", "git_read"]);
    }

    #[test]
    fn no_query_index_stays_under_byte_cap_with_truncation_marker() {
        let tools: Vec<(String, String)> = (0..500)
            .map(|i| (format!("category_{i:04}_tool_name_{i:04}"), "d".to_string()))
            .collect();
        let serialized = compact_catalog_index(&descriptors_of(tools));
        // CR-36: the cap is a hard ceiling on the fully serialized output
        // (marker included), not a soft one with slack for the marker.
        assert!(serialized.len() <= CATALOG_DUMP_MAX_BYTES);
        let entries: Value = serde_json::from_str(&serialized).unwrap();
        let entries = entries.as_array().unwrap();
        let marker = entries.last().expect("truncation marker");
        assert_eq!(marker["truncated"], true);
        assert!(marker["omitted"].as_u64().unwrap() > 0);
        // Marker must state original size and cap (P0-D source acceptance).
        assert!(marker["original_bytes"].as_u64().unwrap() > CATALOG_DUMP_MAX_BYTES as u64);
        assert_eq!(
            marker["cap_bytes"].as_u64().unwrap(),
            CATALOG_DUMP_MAX_BYTES as u64
        );
        // UTF-8 safe: serialized string is valid UTF-8 and byte length == char boundary.
        assert!(serialized.is_char_boundary(serialized.len()));
    }

    // P0-D: exact-ceiling — serialized length equals cap exactly, no marker.
    #[test]
    fn exact_ceiling_has_no_marker_and_equals_cap() {
        // Brute-force single entry pad to hit exactly cap: each 'a' adds one byte.
        let mut found = None;
        for pad in 0..5000 {
            let e = vec![json!({"name": "x", "description": "a".repeat(pad)})];
            let cur = serde_json::to_string(&e).unwrap();
            if cur.len() == CATALOG_DUMP_MAX_BYTES {
                found = Some((e, cur));
                break;
            }
        }
        let (entries, candidate) = found.expect("must find exact-ceiling payload");
        assert_eq!(candidate.len(), CATALOG_DUMP_MAX_BYTES);
        let out = serialize_bounded(entries, CATALOG_DUMP_MAX_BYTES);
        assert_eq!(out.len(), CATALOG_DUMP_MAX_BYTES);
        let parsed: Value = serde_json::from_str(&out).unwrap();
        let arr = parsed.as_array().unwrap();
        // No truncation marker when exactly at cap.
        assert!(arr.iter().all(|v| v.get("truncated").is_none()));
    }

    // P0-D: one-byte-over — original is cap+1, final is <= cap with marker.
    #[test]
    fn one_byte_over_truncates_with_marker_stating_original_and_cap() {
        // Find single entry that serializes to exactly cap+1.
        let mut found = None;
        for pad in 0..5000 {
            let e = vec![json!({"name": "x", "description": "a".repeat(pad)})];
            let cur = serde_json::to_string(&e).unwrap();
            if cur.len() == CATALOG_DUMP_MAX_BYTES + 1 {
                found = Some(e);
                break;
            }
            if cur.len() > CATALOG_DUMP_MAX_BYTES + 1 && pad > 0 {
                // Since len grows by 1 per 'a', we will hit every value; continue
            }
        }
        let entries = found.expect("must find cap+1 payload");
        let original = serde_json::to_string(&entries).unwrap();
        assert_eq!(original.len(), CATALOG_DUMP_MAX_BYTES + 1);
        let out = serialize_bounded(entries, CATALOG_DUMP_MAX_BYTES);
        assert!(out.len() <= CATALOG_DUMP_MAX_BYTES);
        let parsed: Value = serde_json::from_str(&out).unwrap();
        let arr = parsed.as_array().unwrap();
        let marker = arr.last().unwrap();
        assert_eq!(marker["truncated"], true);
        assert_eq!(
            marker["original_bytes"].as_u64().unwrap(),
            (CATALOG_DUMP_MAX_BYTES + 1) as u64
        );
        assert_eq!(
            marker["cap_bytes"].as_u64().unwrap(),
            CATALOG_DUMP_MAX_BYTES as u64
        );
        assert!(marker["omitted"].as_u64().unwrap() >= 1);
    }

    // P0-D: multibyte UTF-8 — truncation never splits a code point.
    #[test]
    fn multibyte_utf8_truncation_is_utf8_safe_and_within_cap() {
        // Use 4-byte emoji (🔬) and 2-byte é to ensure byte vs char len diverge.
        let emoji = "🔬"; // 4 bytes
        let tools: Vec<(String, String)> = (0..200)
            .map(|i| {
                (
                    format!("cat_{i:03}_tool"),
                    format!("desc {emoji} café {emoji} {i}"),
                )
            })
            .collect();
        let out = compact_catalog_index(&descriptors_of(tools));
        assert!(out.len() <= CATALOG_DUMP_MAX_BYTES);
        // Valid UTF-8 and valid JSON
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert!(parsed.is_array());
        // str boundary check: last byte is char boundary
        assert!(out.is_char_boundary(out.len()));
        // If truncated, marker still present and UTF-8 intact
        let arr = parsed.as_array().unwrap();
        if arr.iter().any(|v| v.get("truncated").is_some()) {
            let marker = arr.last().unwrap();
            assert_eq!(marker["truncated"], true);
        }
        // Direct serialize_bounded with emoji payloads
        let entries: Vec<Value> = (0..30)
            .map(|i| json!({"name": format!("tool_{i}"), "description": emoji.repeat(50)}))
            .collect();
        let out2 = serialize_bounded(entries, CATALOG_DUMP_MAX_BYTES);
        assert!(out2.len() <= CATALOG_DUMP_MAX_BYTES);
        assert!(out2.is_char_boundary(out2.len()));
        serde_json::from_str::<Value>(&out2).expect("valid JSON after emoji truncation");
    }

    // P0-D: queried path respects byte cap.
    #[tokio::test]
    async fn queried_path_respects_byte_cap() {
        // 30 tools with large descriptions, all matching "widget".
        let tools: Vec<(String, String)> = (0..30)
            .map(|i| (format!("widget_tool_{i:02}"), "🔬 ".repeat(200) + "widget"))
            .collect();
        let tool = ToolSearchTool::new(tools);
        let result = tool.execute(json!({"query": "widget"}), &ctx()).await;
        assert!(!result.is_error);
        assert!(result.content.len() <= QUERY_RESULT_MAX_BYTES);
        // Valid JSON array
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        let arr = parsed.as_array().unwrap();
        // Bounded by shortlist max or by byte cap truncation
        assert!(arr.len() <= SHORTLIST_MAX || arr.iter().any(|v| v.get("truncated").is_some()));
        if arr.iter().any(|v| v.get("truncated").is_some()) {
            let marker = arr.last().unwrap();
            assert_eq!(marker["truncated"], true);
            assert!(marker["original_bytes"].as_u64().unwrap() > CATALOG_DUMP_MAX_BYTES as u64);
            assert_eq!(
                marker["cap_bytes"].as_u64().unwrap(),
                CATALOG_DUMP_MAX_BYTES as u64
            );
        }
    }

    // P0-D: deterministic ordering for both paths.
    #[tokio::test]
    async fn deterministic_ordering_both_paths() {
        // Queried path: same query yields identical content across calls.
        let tools: Vec<(String, String)> = (0..15)
            .map(|i| (format!("tool_{i:02}"), "widget ops".to_string()))
            .collect();
        let tool = ToolSearchTool::new(tools.clone());
        let first = tool.execute(json!({"query": "widget"}), &ctx()).await;
        let second = tool.execute(json!({"query": "widget"}), &ctx()).await;
        assert_eq!(first.content, second.content);
        // Catalog path: same tool set in shuffled input order yields same output (category+name sorted).
        let mut shuffled = tools.clone();
        shuffled.reverse();
        let out_original = compact_catalog_index(&descriptors_of(tools));
        let out_shuffled = compact_catalog_index(&descriptors_of(shuffled));
        assert_eq!(out_original, out_shuffled);
        // Also verify catalog is sorted by category then name
        let parsed: Value = serde_json::from_str(&out_original).unwrap();
        let arr = parsed.as_array().unwrap();
        let names: Vec<String> = arr
            .iter()
            .filter(|v| v.get("truncated").is_none())
            .filter_map(|v| v["name"].as_str().map(|s| s.to_string()))
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        // For this fixture categories are derived from prefix before '_' — "tool" single category so names sorted.
        assert_eq!(names, sorted);
    }

    // P0-D: queried path respects byte cap including marker.
    #[tokio::test]
    async fn queried_results_byte_capped_with_typed_marker() {
        // Force byte overrun via large descriptions, queried path must truncate.
        let tools: Vec<(String, String)> = (0..20)
            .map(|i| (format!("search_tool_{i:02}"), "a".repeat(600) + " search"))
            .collect();
        let tool = ToolSearchTool::new(tools);
        let result = tool.execute(json!({"query": "search"}), &ctx()).await;
        assert!(!result.is_error);
        assert!(result.content.len() <= QUERY_RESULT_MAX_BYTES);
        let parsed: Value = serde_json::from_str(&result.content).unwrap();
        let arr = parsed.as_array().unwrap();
        // If original exceeded cap, marker present with typed fields
        if arr.iter().any(|v| v.get("truncated").is_some()) {
            let marker = arr.last().unwrap();
            assert_eq!(marker["truncated"], true);
            assert!(marker["omitted"].as_u64().unwrap() >= 1);
            assert!(marker["original_bytes"].as_u64().unwrap() > CATALOG_DUMP_MAX_BYTES as u64);
            assert_eq!(
                marker["cap_bytes"].as_u64().unwrap(),
                CATALOG_DUMP_MAX_BYTES as u64
            );
        }
        // Deterministic: second call identical
        let second = tool.execute(json!({"query": "search"}), &ctx()).await;
        assert_eq!(result.content, second.content);
    }

    // R14 CR-30: retrieval of an externalized result through the normal file-read
    // path. The product externalizes large tool results to the content-addressed
    // virtual path `<state-dir>/tool-results/<ref_id>.txt` under the workspace
    // root and hands the model exactly that workspace-relative path in the
    // preview (engine/src/tool_result_store.rs: `virtual_path` +
    // `large_output_preview`). v5 removed the extra `read_tool_result` schema
    // in favor of ordinary file reads, so this test proves the exact route the
    // model must take: `read_file` with the virtual path from the preview,
    // including the hidden `<state-dir>` directory and the workspace boundary
    // check in resolve_in_workspace. The actual schema deregistration
    // (`registry_impl.rs`) is outside this lane's allowlist (R5 owns
    // read_tool_result.rs); this test validates the replacement path works.
    #[tokio::test]
    async fn externalized_result_retrievable_via_normal_file_read() {
        let ref_id = "abc123def456abc123def456abc123def456abc1";
        let payload = "externalized large output that would have needed read_tool_result";
        let context = ctx();
        let virtual_rel = format!(".rightkit/tool-results/{ref_id}.txt");
        let on_disk = context.workspace_root.join(&virtual_rel);
        std::fs::create_dir_all(on_disk.parent().unwrap()).unwrap();
        std::fs::write(&on_disk, payload).unwrap();

        // The full bytes are durable at the advertised virtual path.
        assert_eq!(std::fs::read_to_string(&on_disk).unwrap(), payload);

        // The normal read path reaches them through the same workspace-relative
        // path the preview advertises — no dedicated retrieval schema involved.
        let rf = crate::coding_tools::read_file::ReadFileTool;
        let result = rf
            .execute(serde_json::json!({"path": virtual_rel}), &context)
            .await;
        assert!(!result.is_error, "read_file failed: {}", result.content);
        assert!(result.content.contains(payload));
    }

    // R14 CR-36 preserved: cap holds marker included (already tested above),
    // this second assertion guards against regression when editing this file.
    #[test]
    fn catalog_cap_still_hard_with_marker() {
        let tools: Vec<(String, String)> = (0..500)
            .map(|i| (format!("cat_{i:04}_tool_{i:04}"), "d".into()))
            .collect();
        let s = compact_catalog_index(&descriptors_of(tools));
        assert!(s.len() <= CATALOG_DUMP_MAX_BYTES);
    }

    #[tokio::test]
    async fn registry_built_query_result_carries_capability_descriptor() {
        let registry = crate::coding_tools::registry::ToolRegistry::build_default()
            .with_model_interface(crate::coding_tools::registry::ModelToolInterface::FixedV1);
        let tool = registry.get("tool_search").expect("tool_search registered");
        let result = tool.execute(json!({"query": "read_file"}), &ctx()).await;
        assert!(!result.is_error, "query failed: {}", result.content);
        assert!(result.content.len() <= QUERY_RESULT_MAX_BYTES);
        let entries: Value = serde_json::from_str(&result.content).unwrap();
        let entry = entries
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"] == "read_file")
            .expect("read_file in results");

        let descriptor = registry.capability_descriptor("read_file").unwrap();
        assert_eq!(entry["capability"], descriptor.id);
        assert_eq!(entry["capability"], "tool:read_file");
        assert_eq!(entry["schema_digest"], descriptor.schema_digest);
        assert_eq!(entry["input_schema"], descriptor.input_schema);
        assert_eq!(entry["read_only"], true);
        assert_eq!(entry["availability"], "available");
        assert!(entry["description"].is_string());
    }

    #[tokio::test]
    async fn oversized_description_keeps_tool_discoverable() {
        let tool = ToolSearchTool::new(vec![(
            "run_program".into(),
            "generated API: 🔬 ".repeat(1000),
        )]);
        let result = tool.execute(json!({"query": "run_program"}), &ctx()).await;
        assert!(!result.is_error);
        assert!(result.content.len() <= CATALOG_DUMP_MAX_BYTES);
        let entries: Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(entries[0]["name"], "run_program");
        assert!(entries[0]["description"]
            .as_str()
            .unwrap()
            .contains("description truncated"));
    }

    #[tokio::test]
    async fn tool_search_default_registry_keeps_legacy_queried_shape() {
        let registry = crate::coding_tools::registry::ToolRegistry::build_default();
        let tool = registry.get("tool_search").expect("tool_search registered");
        let result = tool.execute(json!({"query": "read_file"}), &ctx()).await;
        assert!(!result.is_error, "query failed: {}", result.content);
        assert!(result.content.len() <= CATALOG_DUMP_MAX_BYTES);
        let entries: Value = serde_json::from_str(&result.content).unwrap();
        for entry in entries.as_array().unwrap() {
            if entry["name"].is_string() {
                let mut keys: Vec<&str> = entry
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect();
                keys.sort_unstable();
                assert_eq!(keys, ["description", "name"]);
            }
        }
        assert!(entries
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["name"] == "read_file"));
    }
}
