//! A197 PR/CI status band: one line above the prompt for the current
//! branch's PR and check state. Presentation only; the host fetches GitHub
//! check-runs JSON through the forge client and hands it to
//! [`parse_check_runs`].

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    Pending,
    Passing,
    Failing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckSummary {
    pub passed: u32,
    pub failed: u32,
    pub pending: u32,
}

impl CheckSummary {
    pub fn state(&self) -> CheckState {
        if self.failed > 0 {
            CheckState::Failing
        } else if self.pending > 0 {
            CheckState::Pending
        } else {
            CheckState::Passing
        }
    }
}

/// Parse a GitHub `check-runs` response body. Unparseable bodies yield None.
pub fn parse_check_runs(body: &str) -> Option<CheckSummary> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let runs = v.get("check_runs")?.as_array()?;
    let mut s = CheckSummary {
        passed: 0,
        failed: 0,
        pending: 0,
    };
    for r in runs {
        if r.get("status").and_then(|x| x.as_str()) != Some("completed") {
            s.pending += 1;
            continue;
        }
        match r.get("conclusion").and_then(|x| x.as_str()) {
            Some("success" | "neutral" | "skipped") => s.passed += 1,
            _ => s.failed += 1,
        }
    }
    Some(s)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CiBand {
    pub branch: String,
    pub pr_number: Option<u64>,
    pub pr_title: Option<String>,
    pub checks: Option<CheckSummary>,
}

impl CiBand {
    /// None when there is nothing to show (no PR and no checks).
    pub fn line(&self) -> Option<Line<'static>> {
        if self.pr_number.is_none() && self.checks.is_none() {
            return None;
        }
        let mut spans = vec![Span::styled(
            self.branch.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )];
        if let Some(n) = self.pr_number {
            let title = self.pr_title.as_deref().unwrap_or("");
            spans.push(Span::raw(
                format!("  PR #{n} {title}").trim_end().to_string(),
            ));
        } else {
            spans.push(Span::raw("  no PR"));
        }
        if let Some(c) = &self.checks {
            let (label, color) = match c.state() {
                CheckState::Passing => ("checks passing", Color::Green),
                CheckState::Failing => ("checks failing", Color::Red),
                CheckState::Pending => ("checks running", Color::Yellow),
            };
            spans.push(Span::styled(
                format!(
                    "  {label} ({} ok, {} failed, {} pending)",
                    c.passed, c.failed, c.pending
                ),
                Style::default().fg(color),
            ));
        }
        Some(Line::from(spans))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(l: &Line<'static>) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    const BODY: &str = r#"{"check_runs":[
        {"status":"completed","conclusion":"success"},
        {"status":"completed","conclusion":"skipped"},
        {"status":"in_progress","conclusion":null}]}"#;

    #[test]
    fn parses_check_runs() {
        let s = parse_check_runs(BODY).unwrap();
        assert_eq!((s.passed, s.failed, s.pending), (2, 0, 1));
        assert_eq!(s.state(), CheckState::Pending);
    }

    #[test]
    fn failure_dominates_and_bad_body_is_none() {
        let s = parse_check_runs(
            r#"{"check_runs":[{"status":"completed","conclusion":"failure"},{"status":"queued"}]}"#,
        )
        .unwrap();
        assert_eq!(s.state(), CheckState::Failing);
        assert!(parse_check_runs("not json").is_none());
        assert!(parse_check_runs("{}").is_none());
    }

    #[test]
    fn empty_band_renders_nothing() {
        assert!(CiBand::default().line().is_none());
    }

    #[test]
    fn band_line_shows_pr_and_checks() {
        let band = CiBand {
            branch: "main".into(),
            pr_number: Some(7),
            pr_title: Some("Fix".into()),
            checks: parse_check_runs(BODY),
        };
        assert_eq!(
            flat(&band.line().unwrap()),
            "main  PR #7 Fix  checks running (2 ok, 0 failed, 1 pending)"
        );
    }

    #[test]
    fn checks_without_pr_say_no_pr() {
        let band = CiBand {
            branch: "dev".into(),
            checks: Some(CheckSummary {
                passed: 1,
                failed: 0,
                pending: 0,
            }),
            ..Default::default()
        };
        assert!(flat(&band.line().unwrap()).starts_with("dev  no PR  checks passing"));
    }
}
