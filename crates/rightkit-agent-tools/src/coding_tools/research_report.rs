//! `research_report` — finalize a deep-research run into a clean, **cited**
//! markdown report.
//!
//! The deep-research pattern (gather → read → synthesize, à la Tongyi
//! DeepResearch) needs a structured output, not a wall of prose. The researcher
//! agent gathers with `web_search` / `browser_fetch`, then calls this tool with
//! its findings as structured JSON. The tool renders a consistent report:
//! titled sections whose claims cite numbered sources, plus a deduplicated
//! References list. Inline `[n]` markers in section bodies line up with the
//! numbered sources, so every claim is traceable.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use serde::Deserialize;
use serde_json::{json, Value};

pub struct ResearchReportTool;

#[derive(Deserialize)]
struct Section {
    heading: String,
    body: String,
}

#[derive(Deserialize)]
struct Source {
    title: String,
    url: String,
    #[serde(default)]
    note: String,
}

#[derive(Deserialize)]
struct Report {
    title: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    sections: Vec<Section>,
    #[serde(default)]
    sources: Vec<Source>,
}

#[async_trait::async_trait]
impl Tool for ResearchReportTool {
    fn name(&self) -> &str {
        "research_report"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Finalize a deep-research run into a cited markdown report. Provide title, an optional \
         summary, sections [{heading, body}] whose bodies cite sources inline as [1], [2]…, and \
         sources [{title, url, note?}]. Returns the rendered report; write it to a file or finalize \
         it as an artifact. Cite every non-obvious claim."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["title", "sections", "sources"],
            "properties": {
                "title": { "type": "string" },
                "summary": { "type": "string", "description": "1-3 sentence executive summary" },
                "sections": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["heading", "body"],
                        "properties": {
                            "heading": { "type": "string" },
                            "body": { "type": "string", "description": "may cite sources inline as [1], [2]" }
                        }
                    }
                },
                "sources": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["title", "url"],
                        "properties": {
                            "title": { "type": "string" },
                            "url": { "type": "string" },
                            "note": { "type": "string" }
                        }
                    }
                }
            }
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let report: Report = match serde_json::from_value(args) {
            Ok(r) => r,
            Err(e) => return ToolResult::error(format!("invalid research_report input: {e}")),
        };
        if report.title.trim().is_empty() {
            return ToolResult::error("research_report needs a non-empty title");
        }
        ToolResult::ok(render(&report))
    }
}

fn render(report: &Report) -> String {
    let mut out = format!("# {}\n", report.title.trim());
    if !report.summary.trim().is_empty() {
        out.push_str(&format!("\n{}\n", report.summary.trim()));
    }
    for s in &report.sections {
        out.push_str(&format!("\n## {}\n\n{}\n", s.heading.trim(), s.body.trim()));
    }
    if !report.sources.is_empty() {
        // Deduplicate sources by URL while preserving first-seen order, so the
        // numbering an agent used in [n] markers stays stable for unique URLs.
        let mut seen = std::collections::HashSet::new();
        out.push_str("\n## Sources\n\n");
        let mut n = 0;
        for src in &report.sources {
            if !seen.insert(src.url.trim().to_string()) {
                continue;
            }
            n += 1;
            let title = if src.title.trim().is_empty() {
                src.url.trim()
            } else {
                src.title.trim()
            };
            out.push_str(&format!("{n}. [{title}]({})", src.url.trim()));
            if !src.note.trim().is_empty() {
                out.push_str(&format!(" — {}", src.note.trim()));
            }
            out.push('\n');
        }
    }
    out
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

    #[tokio::test]
    async fn renders_titled_sections_with_numbered_sources() {
        let tool = ResearchReportTool;
        let r = tool
            .execute(
                json!({
                    "title": "State of Rust web frameworks",
                    "summary": "Axum leads for async services [1].",
                    "sections": [
                        { "heading": "Frameworks", "body": "Axum is built on tower [1]; Actix is fast [2]." }
                    ],
                    "sources": [
                        { "title": "Axum docs", "url": "https://docs.rs/axum", "note": "official" },
                        { "title": "Actix", "url": "https://actix.rs" }
                    ]
                }),
                &ctx(),
            )
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("# State of Rust web frameworks"));
        assert!(r.content.contains("## Frameworks"));
        assert!(r.content.contains("## Sources"));
        assert!(r
            .content
            .contains("1. [Axum docs](https://docs.rs/axum) — official"));
        assert!(r.content.contains("2. [Actix](https://actix.rs)"));
    }

    #[tokio::test]
    async fn deduplicates_sources_by_url() {
        let tool = ResearchReportTool;
        let r = tool
            .execute(
                json!({
                    "title": "T",
                    "sections": [],
                    "sources": [
                        { "title": "A", "url": "https://x.test" },
                        { "title": "A again", "url": "https://x.test" },
                        { "title": "B", "url": "https://y.test" }
                    ]
                }),
                &ctx(),
            )
            .await;
        // Only two unique URLs → numbered 1 and 2, no "3.".
        assert!(r.content.contains("1. [A](https://x.test)"));
        assert!(r.content.contains("2. [B](https://y.test)"));
        assert!(!r.content.contains("3."));
    }

    #[tokio::test]
    async fn rejects_empty_title() {
        let tool = ResearchReportTool;
        let r = tool
            .execute(
                json!({ "title": "  ", "sections": [], "sources": [] }),
                &ctx(),
            )
            .await;
        assert!(r.is_error);
    }
}
