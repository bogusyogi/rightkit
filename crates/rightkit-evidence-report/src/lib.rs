#![forbid(unsafe_code)]

use rightkit_evidence_contracts::{Finding, ReportStatus, ReportV1, ReportV2};

pub mod error;
pub mod escape;
pub mod html;
pub mod json;
pub mod markdown;
pub mod ponytail;
pub mod review;
pub mod sarif;
pub mod trend;

pub use error::ReportError;

/// Application-owned names shown in rendered reports: the SARIF tool driver
/// and the HTML and Markdown headings. Declared once with
/// [`set_report_branding`]; otherwise the neutral default applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportBranding {
    /// SARIF `tool.driver.name`.
    pub tool_name: String,
    /// SARIF `tool.driver.informationUri`.
    pub information_uri: String,
    /// Heading and title of the HTML and Markdown renderings.
    pub title: String,
}

impl Default for ReportBranding {
    fn default() -> Self {
        Self {
            tool_name: "RightKit".to_string(),
            information_uri: "https://github.com/bogusyogi/rightkit".to_string(),
            title: "Evidence Report".to_string(),
        }
    }
}

static BRANDING: std::sync::OnceLock<ReportBranding> = std::sync::OnceLock::new();

/// Declare the branding used by every renderer. The first call wins; it
/// returns `false` when branding was already set.
pub fn set_report_branding(branding: ReportBranding) -> bool {
    BRANDING.set(branding).is_ok()
}

/// The branding in effect for this process.
pub fn report_branding() -> &'static ReportBranding {
    BRANDING.get_or_init(ReportBranding::default)
}

pub fn render_json(report: &ReportV1) -> Result<String, ReportError> {
    json::render(report)
}
/// Renders a v2 report as schema 2 (native) or schema 1 (projection).
pub fn render_json_schema(report: &ReportV2, schema: u32) -> Result<String, ReportError> {
    json::render_schema(report, schema)
}
pub fn render_sarif(report: &ReportV1) -> Result<String, ReportError> {
    sarif::render(report)
}
pub fn render_markdown(report: &ReportV1) -> Result<String, ReportError> {
    markdown::render(report)
}
pub fn render_html(report: &ReportV1) -> Result<String, ReportError> {
    html::render(report)
}

/// Renders an evidence-preserving source review from a schema-v2 report.
pub fn render_review(report: &ReportV2) -> Result<String, ReportError> {
    report
        .validate()
        .map_err(|error| ReportError::Contract(error.to_string()))?;
    Ok(review::render(report))
}

/// Renders the canonical Ponytail five-tag finding projection from a v2 report.
pub fn render_ponytail(report: &ReportV2) -> Result<String, ReportError> {
    report
        .validate()
        .map_err(|error| ReportError::Contract(error.to_string()))?;
    Ok(ponytail::render(report))
}

/// Renders GitHub's review-file JSON shape. This only creates a local payload;
/// it never posts a review or comment.
pub fn render_github_review(report: &ReportV2) -> Result<String, ReportError> {
    report
        .validate()
        .map_err(|error| ReportError::Contract(error.to_string()))?;
    review::render_github_review(report)
}

/// JSON-only view of the `auditAssuranceV1` report extension. This crate
/// never links Audit types: it reads the versioned extension as data, shows
/// completion, finding outcome and release eligibility as three independent
/// facts, and never upgrades a malformed, future-versioned or inconsistent
/// extension into an eligibility claim.
pub(crate) mod assurance_view {
    use std::collections::BTreeMap;

    use serde_json::Value;

    use crate::escape;

    pub(crate) const KEY: &str = rightkit_evidence_contracts::report::AUDIT_ASSURANCE_EXTENSION_KEY;

    pub(crate) struct Row {
        pub concern: String,
        pub target: String,
        pub kind: String,
        pub status: String,
        pub reason: String,
        /// (granted_by, rationale, expires_at)
        pub waiver: Option<(String, String, String)>,
    }

    pub(crate) struct Facts {
        pub profile: String,
        pub runtime_constraint: String,
        pub product: String,
        pub target: String,
        pub completion: String,
        pub finding_outcome: String,
        pub release_eligibility: String,
        pub counts: Vec<(String, u64)>,
        pub rows: Vec<Row>,
    }

    pub(crate) enum View {
        Absent,
        Invalid(String),
        Present(Facts),
    }

    fn text(value: &Value, key: &str) -> Option<String> {
        value.get(key).and_then(Value::as_str).map(str::to_owned)
    }

    pub(crate) fn view(extensions: &BTreeMap<String, Value>) -> View {
        let Some(value) = extensions.get(KEY) else {
            return View::Absent;
        };
        match parse(value) {
            Ok(facts) => View::Present(facts),
            Err(reason) => View::Invalid(reason),
        }
    }

    fn parse(value: &Value) -> Result<Facts, String> {
        match value.get("schemaVersion").and_then(Value::as_u64) {
            Some(1) => {}
            Some(other) => return Err(format!("unsupported schemaVersion {other}")),
            None => return Err("missing schemaVersion".into()),
        }
        let summary = value.get("summary").ok_or("missing summary")?;
        let need = |v: &Value, k: &str| text(v, k).ok_or_else(|| format!("missing {k}"));
        let profile = value
            .get("profile")
            .and_then(|p| text(p, "profile"))
            .ok_or("missing profile")?;
        let product_target = value.get("productTarget").ok_or("missing productTarget")?;
        let mut counts = Vec::new();
        for key in [
            "total",
            "satisfied",
            "findings",
            "unmet",
            "deferred",
            "waived",
            "inapplicable",
        ] {
            let n = summary
                .get(key)
                .and_then(Value::as_u64)
                .ok_or_else(|| format!("missing summary.{key}"))?;
            counts.push((key.to_owned(), n));
        }
        let mut rows = Vec::new();
        for item in value
            .get("obligations")
            .and_then(Value::as_array)
            .ok_or("missing obligations")?
        {
            let id = item.get("id").ok_or("obligation missing id")?;
            rows.push(Row {
                concern: need(id, "concern")?,
                target: need(id, "target")?,
                kind: need(id, "evidenceKind")?,
                status: need(item, "status")?,
                reason: text(item, "reason").unwrap_or_default(),
                waiver: item.get("waiver").filter(|w| w.is_object()).map(|w| {
                    (
                        text(w, "grantedBy").unwrap_or_default(),
                        text(w, "rationale").unwrap_or_default(),
                        text(w, "expiresAt").unwrap_or_default(),
                    )
                }),
            });
        }
        let completion = need(summary, "completion")?;
        let finding_outcome = need(summary, "findingOutcome")?;
        let claimed = need(summary, "releaseEligibility")?;
        let open = rows
            .iter()
            .any(|r| matches!(r.status.as_str(), "unmet" | "deferred" | "waived"));
        let has_findings = rows.iter().any(|r| r.status == "findings");
        // Display-time guard: an eligibility claim is shown only when the
        // rows themselves support it.
        let release_eligibility = if claimed == "eligible"
            && !(profile == "release"
                && completion == "complete"
                && finding_outcome == "clean"
                && !open
                && !has_findings)
        {
            "not-eligible (extension claims eligible but its obligations do not support it)"
                .to_owned()
        } else {
            claimed
        };
        Ok(Facts {
            profile,
            runtime_constraint: text(value, "runtimeConstraint").unwrap_or_default(),
            product: need(product_target, "product")?,
            target: need(product_target, "target")?,
            completion,
            finding_outcome,
            release_eligibility,
            counts,
            rows,
        })
    }

    impl Facts {
        pub(crate) fn uncertain(&self) -> Vec<&Row> {
            self.rows
                .iter()
                .filter(|r| matches!(r.status.as_str(), "unmet" | "deferred"))
                .collect()
        }
        pub(crate) fn waived(&self) -> Vec<&Row> {
            self.rows.iter().filter(|r| r.status == "waived").collect()
        }
        pub(crate) fn examined(&self) -> Vec<&Row> {
            self.rows
                .iter()
                .filter(|r| matches!(r.status.as_str(), "satisfied" | "findings"))
                .collect()
        }
        pub(crate) fn weaker_than_release(&self) -> bool {
            self.profile != "release"
        }
    }

    fn row_line(row: &Row) -> String {
        let mut line = format!(
            "{} / {} / {} [{}]",
            escape::markdown_code(&row.target),
            escape::markdown_code(&row.concern),
            escape::markdown_code(&row.kind),
            escape::markdown_code(&row.status),
        );
        if !row.reason.is_empty() {
            line.push_str(": ");
            line.push_str(&escape::markdown(&row.reason));
        }
        line
    }

    /// Markdown `## Assurance` section (also embedded by Ponytail).
    pub(crate) fn markdown(view: &View, always: bool) -> String {
        let mut out = String::new();
        match view {
            View::Absent => {
                if always {
                    out.push_str("## Assurance\n\nNot recorded (legacy report without `auditAssuranceV1`). Completion, finding outcome and release eligibility were not assessed; this report cannot carry release eligibility.\n\n");
                }
            }
            View::Invalid(reason) => {
                out.push_str("## Assurance\n\n");
                out.push_str(&format!(
                    "Extension `{KEY}` is malformed or unsupported ({}). It is not interpreted; release eligibility: not assessed.\n\n",
                    escape::markdown(reason)
                ));
            }
            View::Present(f) => {
                out.push_str("## Assurance\n\n");
                out.push_str(&format!(
                    "- Profile: {}; runtime constraint: {}\n- Product target: {} / {}\n- Completion: {}\n- Finding outcome: {}\n- Release eligibility: {}\n- Counts: {}\n\n",
                    escape::markdown_code(&f.profile),
                    escape::markdown_code(&f.runtime_constraint),
                    escape::markdown_code(&f.product),
                    escape::markdown_code(&f.target),
                    escape::markdown_code(&f.completion),
                    escape::markdown_code(&f.finding_outcome),
                    escape::markdown(&f.release_eligibility),
                    f.counts
                        .iter()
                        .map(|(k, n)| format!("{k}={n}"))
                        .collect::<Vec<_>>()
                        .join(", "),
                ));
                if f.finding_outcome == "findings" {
                    out.push_str(
                        "Obligations were satisfied with findings: this is not a clean result.\n\n",
                    );
                }
                out.push_str("### Examined obligations\n\n");
                let examined = f.examined();
                if examined.is_empty() {
                    out.push_str("None examined.\n\n");
                }
                for row in examined {
                    out.push_str(&format!("- {}\n", row_line(row)));
                }
                out.push('\n');
                out.push_str("### Inherited uncertainty\n\n");
                let uncertain = f.uncertain();
                if uncertain.is_empty() && !f.weaker_than_release() {
                    out.push_str("None recorded.\n\n");
                }
                for row in uncertain {
                    out.push_str(&format!("- {}\n", row_line(row)));
                }
                if f.weaker_than_release() {
                    out.push_str(&format!(
                        "- Profile {} does not project to release: release-only obligations were not part of this run.\n",
                        escape::markdown_code(&f.profile)
                    ));
                }
                out.push('\n');
                out.push_str("### Waivers and debt\n\n");
                let waived = f.waived();
                if waived.is_empty() {
                    out.push_str(
                        "No waivers. Waived obligations are never counted as executed.\n\n",
                    );
                }
                for row in waived {
                    out.push_str(&format!("- {}", row_line(row)));
                    if let Some((by, why, until)) = &row.waiver {
                        out.push_str(&format!(
                            " (waived by {}, expires {}: {}; not executed)",
                            escape::markdown_code(by),
                            escape::markdown_code(until),
                            escape::markdown(why)
                        ));
                    }
                    out.push('\n');
                }
                if !f.waived().is_empty() {
                    out.push('\n');
                }
            }
        }
        out
    }

    /// HTML `<h2>Assurance</h2>` section.
    pub(crate) fn html(view: &View) -> String {
        let h = escape::html;
        let li = |row: &Row| {
            let mut s = format!(
                "<li><code>{}</code> / <code>{}</code> / <code>{}</code> [<code>{}</code>]",
                h(&row.target),
                h(&row.concern),
                h(&row.kind),
                h(&row.status)
            );
            if !row.reason.is_empty() {
                s.push_str(": ");
                s.push_str(&h(&row.reason));
            }
            if let Some((by, why, until)) = &row.waiver {
                s.push_str(&format!(
                    " (waived by <code>{}</code>, expires <code>{}</code>: {}; not executed)",
                    h(by),
                    h(until),
                    h(why)
                ));
            }
            s.push_str("</li>\n");
            s
        };
        match view {
            View::Absent => "<h2>Assurance</h2>\n<p>Not recorded (legacy report without <code>auditAssuranceV1</code>). Release eligibility: not assessed; this report cannot carry release eligibility.</p>\n".to_owned(),
            View::Invalid(reason) => format!(
                "<h2>Assurance</h2>\n<p>Extension <code>{KEY}</code> is malformed or unsupported ({}). It is not interpreted; release eligibility: not assessed.</p>\n",
                h(reason)
            ),
            View::Present(f) => {
                let mut out = format!(
                    "<h2>Assurance</h2>\n<ul>\n<li>Profile: <code>{}</code>; runtime constraint: <code>{}</code></li>\n<li>Product target: <code>{}</code> / <code>{}</code></li>\n<li>Completion: <code>{}</code></li>\n<li>Finding outcome: <code>{}</code></li>\n<li>Release eligibility: {}</li>\n<li>Counts: {}</li>\n</ul>\n",
                    h(&f.profile),
                    h(&f.runtime_constraint),
                    h(&f.product),
                    h(&f.target),
                    h(&f.completion),
                    h(&f.finding_outcome),
                    h(&f.release_eligibility),
                    h(&f.counts.iter().map(|(k, n)| format!("{k}={n}")).collect::<Vec<_>>().join(", ")),
                );
                if f.finding_outcome == "findings" {
                    out.push_str("<p>Obligations were satisfied with findings: this is not a clean result.</p>\n");
                }
                out.push_str("<h3>Examined obligations</h3>\n<ul>\n");
                let examined = f.examined();
                if examined.is_empty() {
                    out.push_str("<li>None examined.</li>\n");
                }
                for row in examined {
                    out.push_str(&li(row));
                }
                out.push_str("</ul>\n<h3>Inherited uncertainty</h3>\n<ul>\n");
                let uncertain = f.uncertain();
                if uncertain.is_empty() && !f.weaker_than_release() {
                    out.push_str("<li>None recorded.</li>\n");
                }
                for row in uncertain {
                    out.push_str(&li(row));
                }
                if f.weaker_than_release() {
                    out.push_str(&format!(
                        "<li>Profile <code>{}</code> does not project to release: release-only obligations were not part of this run.</li>\n",
                        h(&f.profile)
                    ));
                }
                out.push_str("</ul>\n<h3>Waivers and debt</h3>\n<ul>\n");
                let waived = f.waived();
                if waived.is_empty() {
                    out.push_str("<li>No waivers. Waived obligations are never counted as executed.</li>\n");
                }
                for row in waived {
                    out.push_str(&li(row));
                }
                out.push_str("</ul>\n");
                out
            }
        }
    }

    /// SARIF `run.properties.assurance` value.
    pub(crate) fn sarif(view: &View) -> Value {
        use serde_json::json;
        match view {
            View::Absent => json!({
                "recorded": false,
                "releaseEligibility": "not-assessed",
                "note": "legacy report without auditAssuranceV1; cannot carry release eligibility"
            }),
            View::Invalid(reason) => json!({
                "recorded": false,
                "invalid": reason,
                "releaseEligibility": "not-assessed"
            }),
            View::Present(f) => {
                let rows = |rows: Vec<&Row>| {
                    rows.into_iter()
                        .map(|r| json!({"target": r.target, "concern": r.concern, "evidenceKind": r.kind, "status": r.status, "reason": r.reason}))
                        .collect::<Vec<_>>()
                };
                json!({
                    "recorded": true,
                    "profile": f.profile,
                    "runtimeConstraint": f.runtime_constraint,
                    "completion": f.completion,
                    "findingOutcome": f.finding_outcome,
                    "releaseEligibility": f.release_eligibility,
                    "counts": f.counts.iter().cloned().collect::<BTreeMap<_, _>>(),
                    "inheritedUncertainty": rows(f.uncertain()),
                    "waived": f.waived().into_iter().map(|r| json!({
                        "target": r.target, "concern": r.concern, "evidenceKind": r.kind,
                        "grantedBy": r.waiver.as_ref().map(|w| w.0.clone()),
                        "expiresAt": r.waiver.as_ref().map(|w| w.2.clone()),
                        "executed": false
                    })).collect::<Vec<_>>()
                })
            }
        }
    }
}

pub(crate) fn status_label(status: ReportStatus) -> &'static str {
    match status {
        ReportStatus::Clean => "clean",
        ReportStatus::Findings => "findings",
        ReportStatus::Incomplete => "incomplete",
        ReportStatus::Failed => "failed",
        ReportStatus::Blocked => "blocked",
    }
}

pub(crate) fn ordered_findings(report: &ReportV1) -> Vec<&Finding> {
    let mut findings: Vec<_> = report.findings.iter().collect();
    findings.sort_by(|left, right| {
        left.id
            .as_str()
            .cmp(right.id.as_str())
            .then_with(|| first_location(left).cmp(&first_location(right)))
            .then_with(|| left.message.cmp(&right.message))
    });
    findings
}

fn first_location(finding: &Finding) -> (String, u64, u64) {
    finding
        .locations
        .iter()
        .map(|location| {
            let normalized = location.replace('\\', "/");
            let (path, span) = normalized
                .rsplit_once(':')
                .unwrap_or((normalized.as_str(), ""));
            let (start, end) = span.split_once('-').unwrap_or((span, span));
            let start = start.parse::<u64>().unwrap_or(0);
            let end = end.parse::<u64>().unwrap_or(start);
            (path.to_string(), start, end)
        })
        .min()
        .unwrap_or_else(|| (String::new(), 0, 0))
}
