//! Pass / warn / block checks and the verdict they roll up to.
//!
//! Every [`Check`] and every [`Dimension`] yields a status. The report rolls up to one
//! [`Status`], renders as text or JSON, and the process exit code encodes it: 0 ready, 1 ready
//! with warnings, 2 blocked.

use std::collections::BTreeMap;

use serde::Serialize;

/// The outcome of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Ready.
    Pass,
    /// Works, with a caveat.
    Warn,
    /// Cannot work until fixed.
    Block,
}

impl Status {
    /// The glyph the text report puts before a check.
    pub fn symbol(self) -> &'static str {
        match self {
            Status::Pass => "\u{2713}",
            Status::Warn => "\u{26a0}",
            Status::Block => "\u{2717}",
        }
    }

    /// 0 pass, 1 warn, 2 block; also the process exit code of a report with that verdict.
    pub fn rank(self) -> u8 {
        match self {
            Status::Pass => 0,
            Status::Warn => 1,
            Status::Block => 2,
        }
    }

    /// The headline of the text report.
    pub fn headline(self) -> &'static str {
        match self {
            Status::Pass => "READY",
            Status::Warn => "READY WITH WARNINGS",
            Status::Block => "BLOCKED",
        }
    }
}

/// One check of a verdict report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    /// Stable machine id.
    pub id: String,
    /// Label shown in the text report.
    pub label: String,
    /// Outcome.
    pub status: Status,
    /// What was found.
    pub detail: String,
    /// What to do about it (empty for a clean pass).
    pub fix: String,
}

impl Check {
    /// Build a check.
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        status: Status,
        detail: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            status,
            detail: detail.into(),
            fix: fix.into(),
        }
    }

    /// A passing check with no fix.
    pub fn pass(id: &str, label: &str, detail: impl Into<String>) -> Self {
        Self::new(id, label, Status::Pass, detail, "")
    }
}

/// The checks of one doctor run and their rolled-up verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerdictReport {
    /// The worst status among the checks (pass when there are none).
    pub verdict: Status,
    /// Every check, in the order they ran.
    pub checks: Vec<Check>,
}

impl VerdictReport {
    /// Roll `checks` up to a verdict.
    pub fn new(checks: Vec<Check>) -> Self {
        let verdict = checks
            .iter()
            .map(|c| c.status)
            .max_by_key(|s| s.rank())
            .unwrap_or(Status::Pass);
        Self { verdict, checks }
    }

    /// Process exit code: 0 ready, 1 warnings, 2 blocked.
    pub fn exit_code(&self) -> i32 {
        i32::from(self.verdict.rank())
    }

    /// The text report, headed `<title> doctor`.
    pub fn render_text(&self, title: &str) -> String {
        let mut out = format!("{title} doctor\n\n");
        for c in &self.checks {
            out.push_str(&format!(
                "  {} {} \u{2014} {}\n",
                c.status.symbol(),
                c.label,
                c.detail
            ));
            if !c.fix.is_empty() && c.status != Status::Pass {
                out.push_str(&format!("      \u{2192} {}\n", c.fix));
            }
        }
        out.push_str(&format!("\nVerdict: {}\n", self.verdict.headline()));
        out
    }

    /// The report with `schema_version` and extra top-level members merged in, as pretty JSON.
    pub fn to_json_with(
        &self,
        schema_version: &str,
        extra: serde_json::Map<String, serde_json::Value>,
    ) -> String {
        let mut value = serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({}));
        if let Some(object) = value.as_object_mut() {
            object.insert("schema_version".into(), schema_version.into());
            object.extend(extra);
        }
        serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into())
    }
}

/// One security or runtime dimension probe result (`pass`, `warn`, `block` or `unknown`; unknown
/// is for dimensions with no accessor yet, never a fake pass).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Dimension {
    /// `pass` | `warn` | `block` | `unknown`.
    pub status: &'static str,
    /// What was found.
    pub detail: String,
    /// Sub-checks, present only in verbose mode and when non-empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Vec<Evidence>>,
}

/// One piece of evidence inside a [`Dimension`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Evidence {
    /// Stable machine id.
    pub id: String,
    /// Label.
    pub label: String,
    /// `pass` | `warn` | `block` | `unknown`.
    pub status: &'static str,
    /// What was found.
    pub detail: String,
}

impl Dimension {
    /// A dimension without evidence.
    pub fn new(status: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status,
            detail: detail.into(),
            evidence: None,
        }
    }

    /// A dimension with evidence; an empty list is omitted rather than serialised as `[]`.
    pub fn with_evidence(
        status: &'static str,
        detail: impl Into<String>,
        evidence: Vec<Evidence>,
    ) -> Self {
        Self {
            status,
            detail: detail.into(),
            evidence: if evidence.is_empty() {
                None
            } else {
                Some(evidence)
            },
        }
    }
}

/// Dimensions keyed by name; the alphabetical key order is part of the JSON contract.
pub type Dimensions = BTreeMap<&'static str, Dimension>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_is_the_worst_status_and_sets_the_exit_code() {
        let ready = VerdictReport::new(vec![Check::pass("a", "A", "ok")]);
        assert_eq!(ready.exit_code(), 0);
        let warn = VerdictReport::new(vec![
            Check::pass("a", "A", "ok"),
            Check::new("b", "B", Status::Warn, "slow", "wait"),
        ]);
        assert_eq!(warn.exit_code(), 1);
        let blocked = VerdictReport::new(vec![
            Check::new("b", "B", Status::Warn, "slow", "wait"),
            Check::new("c", "C", Status::Block, "gone", "install it"),
        ]);
        assert_eq!(blocked.exit_code(), 2);
        let text = blocked.render_text("Demo");
        assert!(text.starts_with("Demo doctor"));
        assert!(text.contains("install it"));
        assert!(text.contains("Verdict: BLOCKED"));
        assert_eq!(VerdictReport::new(vec![]).verdict, Status::Pass);
    }

    #[test]
    fn json_carries_schema_and_extras_and_skips_empty_evidence() {
        let report = VerdictReport::new(vec![Check::pass("a", "A", "ok")]);
        let mut extra = serde_json::Map::new();
        extra.insert("dimensions".into(), serde_json::json!({}));
        let value: serde_json::Value =
            serde_json::from_str(&report.to_json_with("demo.doctor.v1", extra)).unwrap();
        assert_eq!(value["schema_version"], "demo.doctor.v1");
        assert_eq!(value["verdict"], "pass");
        let dim = Dimension::with_evidence("unknown", "none", vec![]);
        assert!(dim.evidence.is_none());
        let json = serde_json::to_value(&dim).unwrap();
        assert!(json.get("evidence").is_none());
    }
}
