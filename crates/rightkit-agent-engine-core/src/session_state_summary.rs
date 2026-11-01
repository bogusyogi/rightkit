//! Structured session-state summary.
//!
//! Tier 2 compaction (and, later, agent handoff) summarizes older history into
//! one fixed schema instead of free prose, so a successor context -- the same
//! session after compaction, or a different agent receiving a handoff -- can
//! rely on the same sections being present and in the same order.
//!
//! The model is prompted to emit the markdown form produced by
//! [`SessionStateSummary::render_markdown`]; [`SessionStateSummary::parse_markdown`]
//! reads it back leniently (unknown or reordered headings never discard a
//! summary). The type is deliberately free of compactor/engine dependencies so
//! handoff code can depend on it alone.

use serde::{Deserialize, Serialize};

/// Section headings, in canonical order. The compaction prompt and the
/// renderer both derive from this list so they cannot drift apart.
pub const SESSION_STATE_SECTIONS: [&str; 7] = [
    "Goal",
    "Decisions",
    "Constraints",
    "Files touched",
    "Current state",
    "Open questions",
    "Next steps",
];

/// One file the session touched and where it stands.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileStatus {
    pub path: String,
    /// Free-form status, e.g. `edited`, `created`, `read only`, `tests failing`.
    #[serde(default)]
    pub status: String,
}

/// The model-authored state of a session at a compaction (or handoff) boundary.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStateSummary {
    /// The user's active goal.
    #[serde(default)]
    pub goal: String,
    /// Decisions made, with their reasoning.
    #[serde(default)]
    pub decisions: Vec<String>,
    /// Constraints that must keep holding (user instructions, safety rules).
    #[serde(default)]
    pub constraints: Vec<String>,
    /// Files touched and their status.
    #[serde(default)]
    pub files_touched: Vec<FileStatus>,
    /// Where the work stands right now, including verification state.
    #[serde(default)]
    pub current_state: String,
    /// Questions still unresolved.
    #[serde(default)]
    pub open_questions: Vec<String>,
    /// The exact next steps, in order.
    #[serde(default)]
    pub next_steps: Vec<String>,
}

impl SessionStateSummary {
    /// `true` when no section carries content.
    pub fn is_empty(&self) -> bool {
        self.goal.trim().is_empty()
            && self.decisions.is_empty()
            && self.constraints.is_empty()
            && self.files_touched.is_empty()
            && self.current_state.trim().is_empty()
            && self.open_questions.is_empty()
            && self.next_steps.is_empty()
    }

    /// Render the canonical markdown form. Every section is emitted (empty
    /// ones as `(none)`) so a reader can tell "nothing to report" from "section
    /// lost".
    pub fn render_markdown(&self) -> String {
        fn list(items: &[String]) -> String {
            if items.is_empty() {
                "(none)\n".to_string()
            } else {
                items.iter().map(|item| format!("- {item}\n")).collect()
            }
        }
        fn text(value: &str) -> String {
            if value.trim().is_empty() {
                "(none)\n".to_string()
            } else {
                format!("{}\n", value.trim())
            }
        }
        let files = if self.files_touched.is_empty() {
            "(none)\n".to_string()
        } else {
            self.files_touched
                .iter()
                .map(|file| {
                    if file.status.trim().is_empty() {
                        format!("- {}\n", file.path)
                    } else {
                        format!("- {} -- {}\n", file.path, file.status)
                    }
                })
                .collect()
        };
        format!(
            "## Goal\n{}\n## Decisions\n{}\n## Constraints\n{}\n## Files touched\n{}\n## Current state\n{}\n## Open questions\n{}\n## Next steps\n{}",
            text(&self.goal),
            list(&self.decisions),
            list(&self.constraints),
            files,
            text(&self.current_state),
            list(&self.open_questions),
            list(&self.next_steps),
        )
    }

    /// Parse markdown shaped like [`Self::render_markdown`]. Headings match
    /// case-insensitively at any `#` level; unknown headings and preamble are
    /// ignored. Returns `None` when no known heading is present, so callers
    /// keep the raw model text instead of an empty structure.
    pub fn parse_markdown(markdown: &str) -> Option<Self> {
        let mut summary = Self::default();
        let mut section: Option<usize> = None;
        let mut buffers: [Vec<String>; 7] = Default::default();
        let mut matched_any = false;
        for line in markdown.lines() {
            let trimmed = line.trim();
            if let Some(heading) = trimmed.strip_prefix('#') {
                let heading = heading.trim_start_matches('#').trim().trim_end_matches(':');
                section = SESSION_STATE_SECTIONS
                    .iter()
                    .position(|known| known.eq_ignore_ascii_case(heading));
                matched_any |= section.is_some();
                continue;
            }
            if let Some(index) = section {
                if !trimmed.is_empty() && trimmed != "(none)" {
                    buffers[index].push(trimmed.to_string());
                }
            }
        }
        if !matched_any {
            return None;
        }
        let bullets = |lines: &[String]| -> Vec<String> {
            lines
                .iter()
                .map(|line| {
                    line.trim_start_matches(|c: char| c == '-' || c == '*' || c == '+')
                        .trim()
                        .to_string()
                })
                .filter(|line| !line.is_empty())
                .collect()
        };
        summary.goal = buffers[0].join(" ");
        summary.decisions = bullets(&buffers[1]);
        summary.constraints = bullets(&buffers[2]);
        summary.files_touched = bullets(&buffers[3])
            .into_iter()
            .map(|entry| match entry.split_once(" -- ") {
                Some((path, status)) => FileStatus {
                    path: path.trim().to_string(),
                    status: status.trim().to_string(),
                },
                None => FileStatus {
                    path: entry,
                    status: String::new(),
                },
            })
            .collect();
        summary.current_state = buffers[4].join(" ");
        summary.open_questions = bullets(&buffers[5]);
        summary.next_steps = bullets(&buffers[6]);
        Some(summary)
    }

    /// Parse only complete canonical summaries. Empty sections are valid when
    /// explicitly represented by `(none)`; omitted or duplicated headings fail.
    pub fn parse_and_validate(markdown: &str) -> Result<Self, String> {
        let mut seen = [false; 7];
        for line in markdown.lines() {
            let trimmed = line.trim();
            if let Some(heading) = trimmed.strip_prefix('#') {
                let heading = heading.trim_start_matches('#').trim().trim_end_matches(':');
                if let Some(index) = SESSION_STATE_SECTIONS
                    .iter()
                    .position(|known| known.eq_ignore_ascii_case(heading))
                {
                    if seen[index] {
                        return Err(format!("duplicate section: {heading}"));
                    }
                    seen[index] = true;
                }
            }
        }
        if let Some(missing) = SESSION_STATE_SECTIONS
            .iter()
            .enumerate()
            .find_map(|(i, section)| (!seen[i]).then_some(*section))
        {
            return Err(format!("missing section: {missing}"));
        }
        Self::parse_markdown(markdown)
            .ok_or_else(|| "summary contains no recognized sections".into())
    }

    /// Instruction block telling a summarizing model which sections to write.
    /// Shared by the compaction prompt and any future handoff prompt.
    pub fn prompt_schema() -> String {
        let mut out = String::from(
            "Write the summary as a session-state document with exactly these markdown \
             sections, in this order, each as a `## ` heading. Write `(none)` for an empty \
             section; never omit one:\n",
        );
        for section in SESSION_STATE_SECTIONS {
            out.push_str(&format!("## {section}\n"));
        }
        out.push_str(
            "Goal and Current state are short prose; every other section is a bullet list. \
             Files touched uses `- path -- status` bullets.",
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SessionStateSummary {
        SessionStateSummary {
            goal: "ship the compaction tiers".into(),
            decisions: vec!["use a reactive retry once".into()],
            constraints: vec!["never print to stdout".into()],
            files_touched: vec![
                FileStatus {
                    path: "src/a.rs".into(),
                    status: "edited".into(),
                },
                FileStatus {
                    path: "src/b.rs".into(),
                    status: String::new(),
                },
            ],
            current_state: "tier 2 implemented, tests pending".into(),
            open_questions: vec![],
            next_steps: vec!["run the e2e suite".into(), "fix failures".into()],
        }
    }

    #[test]
    fn render_then_parse_round_trips() {
        let original = sample();
        let parsed = SessionStateSummary::parse_markdown(&original.render_markdown())
            .expect("rendered markdown must parse");
        assert_eq!(parsed, original);
    }

    #[test]
    fn render_emits_every_section_in_canonical_order() {
        let rendered = SessionStateSummary::default().render_markdown();
        let mut cursor = 0;
        for section in SESSION_STATE_SECTIONS {
            let at = rendered[cursor..]
                .find(&format!("## {section}"))
                .unwrap_or_else(|| panic!("missing or misordered section {section}"));
            cursor += at;
        }
        assert_eq!(rendered.matches("(none)").count(), 7);
    }

    #[test]
    fn parse_is_lenient_about_heading_level_case_and_unknown_sections() {
        let parsed = SessionStateSummary::parse_markdown(
            "Preamble text\n# goal:\nfix it\n### Unknown extra\nignored\n## NEXT STEPS\n* one\n",
        )
        .expect("known headings present");
        assert_eq!(parsed.goal, "fix it");
        assert_eq!(parsed.next_steps, vec!["one".to_string()]);
        assert!(parsed.decisions.is_empty());
    }

    #[test]
    fn free_prose_without_known_headings_is_not_forced_into_structure() {
        assert!(SessionStateSummary::parse_markdown("just a paragraph of prose").is_none());
        assert!(SessionStateSummary::parse_and_validate("## Goal\ng").is_err());
    }

    #[test]
    fn prompt_schema_lists_every_section() {
        let schema = SessionStateSummary::prompt_schema();
        for section in SESSION_STATE_SECTIONS {
            assert!(schema.contains(&format!("## {section}")));
        }
    }

    #[test]
    fn serde_uses_camel_case_and_tolerates_missing_fields() {
        let json = serde_json::to_value(sample()).unwrap();
        assert!(json.get("filesTouched").is_some());
        let sparse: SessionStateSummary =
            serde_json::from_value(serde_json::json!({"goal": "g"})).unwrap();
        assert_eq!(sparse.goal, "g");
        assert!(!sparse.is_empty());
        assert!(SessionStateSummary::default().is_empty());
    }
}
