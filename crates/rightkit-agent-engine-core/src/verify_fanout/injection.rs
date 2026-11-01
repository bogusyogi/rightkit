//! Turn-boundary injection of actionable findings (Pi "turn-boundary
//! injection" leg).
//!
//! Donor shape: Pi extensions feed findings back to the originating agent
//! with `sendMessage(..., { triggerTurn, deliverAs: "followUp" })`
//! (`packages/coding-agent/src/core/extensions/types.ts:397-400,1343-1346`),
//! i.e. a follow-up message injected at the turn boundary so the agent sees
//! it before the turn completes. The the host app equivalent is a rendered
//! instruction block the engine pushes as a user turn before completion
//! (`PostTurnGateDecision::Reenter` in the existing post-edit pipeline).
//!
//! "Actionable" is the canon's word: error-severity findings from verifiers
//! that actually ran. Warnings ride along under a non-blocking section;
//! verifiers that could not run are reported honestly at the tail when
//! nothing else is actionable — verification that could not confirm the
//! edits must never read as clean.

use std::path::PathBuf;

use super::finding::{FindingSeverity, VerifiedFinding};
use super::verifiers::{
    AggregateStatus, VerifierId, VerifierOutcome, VerifierRequirement, VerifierStatus,
};

/// Maximum rendered lines before truncation, mirroring the bound the
/// post-edit diagnostics renderer applies.
pub const DEFAULT_MAX_INJECTION_LINES: usize = 40;

pub const FOLLOWUP_SOURCE: &str = "verify_fanout";

/// Findings the originating agent must act on pre-completion: error-severity
/// findings from verifiers that ran. (Stale findings are already discarded
/// by the report; this only filters severity.)
pub fn actionable_findings(report: &super::FanoutReport) -> Vec<&VerifiedFinding> {
    if report.is_fenced() {
        return Vec::new();
    }
    report
        .findings
        .iter()
        .filter(|f| f.severity.is_actionable())
        .collect()
}

fn render_finding_line(finding: &VerifiedFinding) -> String {
    let location = match finding.range {
        Some(range) => format!(
            "{}:{}:{} ",
            finding.path.display(),
            range.start_line + 1,
            range.start_column + 1
        ),
        None => format!("{} ", finding.path.display()),
    };
    let fix_hint = match &finding.fixability {
        super::finding::FindingFixability::FixAvailable { hint } => format!(" [fix: {hint}]"),
        super::finding::FindingFixability::Manual => String::new(),
    };
    format!(
        "{location}({} via {}, {}) {}{fix_hint}",
        finding.category.as_str(),
        finding.source,
        finding.severity.as_str(),
        crate::post_turn_gate::bound_text(&finding.message, MAX_FINDING_MESSAGE_BYTES)
    )
}

/// Longest finding message one rendered line carries. The renderer caps the
/// number of lines; this caps each line too (audit M9), so a verifier that
/// emits one enormous message cannot defeat compaction.
const MAX_FINDING_MESSAGE_BYTES: usize = 400;

fn render_outcome_line(outcome: &VerifierOutcome, reason: &str) -> String {
    let subject = match &outcome.subject {
        Some(subject) => format!("{} ", subject.display()),
        None if !outcome.covered_paths.is_empty() => format!(
            "{} ",
            outcome
                .covered_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        None => String::new(),
    };
    format!(
        "{subject}({} via {}): {reason}",
        outcome.requirement.as_str(),
        outcome.verifier
    )
}

fn unavailable_summary(
    outcomes: &[VerifierOutcome],
    requirement: VerifierRequirement,
) -> Vec<String> {
    outcomes
        .iter()
        .filter_map(|outcome| match &outcome.status {
            VerifierStatus::Unavailable { reason } if outcome.requirement == requirement => {
                Some(render_outcome_line(outcome, reason))
            }
            _ => None,
        })
        .collect()
}

fn failed_summary(outcomes: &[VerifierOutcome]) -> Vec<String> {
    outcomes
        .iter()
        .filter_map(|outcome| match &outcome.status {
            VerifierStatus::Failed { reason } => Some(render_outcome_line(outcome, reason)),
            _ => None,
        })
        .collect()
}

/// Verified paths whose required verifier coverage produced no verdict.
/// A path counts as uncovered only when at least one `Required` unit claimed
/// it in `covered_paths` — file types with no required verifier at all are
/// not gaps (nothing could have verified them), while a file whose required
/// checker timed out, overflowed the unit budget, or could not run is.
/// A clean verdict on file A therefore never conceals a required verifier
/// that produced no verdict on file B.
pub fn uncovered_required_paths(report: &super::FanoutReport) -> Vec<PathBuf> {
    report
        .verified_paths
        .iter()
        .filter(|path| {
            let coverers: Vec<&VerifierOutcome> = report
                .outcomes
                .iter()
                .filter(|outcome| {
                    outcome.requirement.is_required()
                        && outcome.covered_paths.iter().any(|c| c == *path)
                })
                .collect();
            !coverers.is_empty()
                && coverers
                    .iter()
                    .all(|outcome| matches!(outcome.status, VerifierStatus::Unavailable { .. }))
        })
        .cloned()
        .collect()
}

/// Required verifier units that produced no verdict or a failing verdict.
/// A `Failed` required unit with no parsed diagnostics still appears here —
/// the failure is visible and blocks, never dropped because `findings` is
/// empty.
pub fn required_gap_lines(report: &super::FanoutReport) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    if report.is_fenced() {
        lines.push(format!(
            "{} mutation receipt(s) could not be bound",
            report.binding_errors.len()
        ));
    }
    lines.extend(
        actionable_findings(report)
            .into_iter()
            .map(render_finding_line),
    );
    for outcome in &report.outcomes {
        if !outcome.requirement.is_required() {
            continue;
        }
        match &outcome.status {
            VerifierStatus::Unavailable { reason } | VerifierStatus::Failed { reason } => {
                lines.push(render_outcome_line(outcome, reason));
            }
            VerifierStatus::Clean => {}
        }
    }
    for path in uncovered_required_paths(report) {
        lines.push(format!(
            "{} (required): every verifier covering this file produced no verdict",
            path.display()
        ));
    }
    lines.sort();
    lines.dedup();
    lines
}

/// True when a required verifier failed, could not run, or never produced a
/// verdict for a path it covers. Required gaps block verified completion;
/// the agent may keep using tools to diagnose and repair meanwhile.
pub fn has_required_gap(report: &super::FanoutReport) -> bool {
    if report.is_fenced() {
        return true;
    }
    report.outcomes.iter().any(|outcome| {
        outcome.requirement.is_required() && !matches!(outcome.status, VerifierStatus::Clean)
    }) || !uncovered_required_paths(report).is_empty()
}

/// What the dispatch gate does with a fan-out report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostDispatchAction {
    /// Nothing to feed back (all clean, or no applicable verifier at all).
    Proceed,
    /// Only optional verifiers failed to produce a verdict (and/or
    /// warnings). Reported visibly in the transcript and the observation —
    /// but the mutation turn neither finishes early nor force-reenters; a
    /// missing optional language server must not loop the agent.
    ReportOnly(String),
    /// The agent must act: actionable findings, a required verification gap,
    /// or a receipt-binding fence. `required_gap` tells the caller to also
    /// arm pending verification so the completion path re-verifies.
    Reenter {
        instruction: String,
        required_gap: bool,
    },
}

/// The dispatch-side decision for one fan-out report. Pure: engine wiring
/// maps it to `ToolDispatchOutcome` and pending-state bookkeeping.
pub fn post_dispatch_action(report: &super::FanoutReport) -> PostDispatchAction {
    let Some(instruction) = render_followup_injection(report) else {
        return PostDispatchAction::Proceed;
    };
    let required_gap = has_required_gap(report);
    if report.is_fenced() || !actionable_findings(report).is_empty() || required_gap {
        PostDispatchAction::Reenter {
            instruction,
            required_gap,
        }
    } else {
        PostDispatchAction::ReportOnly(instruction)
    }
}

/// Observation fields for the `post_edit_verification` orchestration event:
/// every verifier's status, requirement, covered paths, and reason, so a
/// zero-finding run never reads as fully verified when a required verifier
/// produced no verdict.
pub fn observation_fields(
    report: &super::FanoutReport,
) -> std::collections::HashMap<String, String> {
    let mut fields = std::collections::HashMap::new();
    fields.insert(
        "aggregate".to_string(),
        match aggregate_status(report) {
            AggregateStatus::Clean => "clean".to_string(),
            AggregateStatus::Unavailable { .. } => "unavailable".to_string(),
            AggregateStatus::Failed { .. } => "failed".to_string(),
        },
    );
    fields.insert(
        "requiredGap".to_string(),
        has_required_gap(report).to_string(),
    );
    fields.insert(
        "infrastructureUnverified".to_string(),
        is_infrastructure_unverified(report).to_string(),
    );
    let uncovered = uncovered_required_paths(report);
    if !uncovered.is_empty() {
        fields.insert(
            "uncoveredPaths".to_string(),
            uncovered
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    let verifiers: Vec<serde_json::Value> = report
        .outcomes
        .iter()
        .map(|outcome| {
            serde_json::json!({
                "id": outcome.verifier.to_string(),
                "subject": outcome.subject.as_ref().map(|p| p.display().to_string()),
                "status": match &outcome.status {
                    VerifierStatus::Clean => "clean".to_string(),
                    VerifierStatus::Unavailable { reason } => format!("unavailable: {reason}"),
                    VerifierStatus::Failed { reason } => format!("failed: {reason}"),
                },
                "required": outcome.requirement.is_required(),
                "coveredPaths": outcome
                    .covered_paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>(),
                "findings": outcome.findings.len(),
                "truncated": outcome.truncated,
            })
        })
        .collect();
    fields.insert(
        "verifiers".to_string(),
        serde_json::to_string(&verifiers).unwrap_or_else(|_| "[]".to_string()),
    );
    // Counts cannot project the actionable, content-bound finding schema.
    // Keep the durable observation bounded like the originating-agent note.
    let findings: Vec<serde_json::Value> = report
        .findings
        .iter()
        .take(DEFAULT_MAX_INJECTION_LINES)
        .map(|finding| serde_json::json!({
            "source": finding.source,
            "path": finding.path,
            "range": finding.range.map(|range| serde_json::json!({
                "startLine": range.start_line, "startColumn": range.start_column,
                "endLine": range.end_line, "endColumn": range.end_column,
            })),
            "severity": finding.severity.as_str(),
            "category": finding.category.as_str(),
            "message": finding.message.chars().take(2000).collect::<String>(),
            "contentHash": finding.content_hash,
            "fixability": match &finding.fixability {
                super::finding::FindingFixability::Manual => serde_json::json!({"kind":"manual"}),
                super::finding::FindingFixability::FixAvailable { hint } => serde_json::json!({
                    "kind":"fixAvailable", "hint": hint.chars().take(2000).collect::<String>(),
                }),
            },
        }))
        .collect();
    if !findings.is_empty() {
        fields.insert(
            "verificationFindings".into(),
            serde_json::Value::Array(findings).to_string(),
        );
    }
    if report.budget_exhausted {
        fields.insert("budgetExhausted".to_string(), "true".to_string());
    }
    if report.discarded_stale > 0 {
        fields.insert(
            "discardedStale".to_string(),
            report.discarded_stale.to_string(),
        );
    }
    fields
}

/// `true` when the fan-out actually ran verifiers and could not confirm the
/// edits at all -- no actionable (error-severity) findings, nothing actually
/// failed, but every verifier that would have covered these files came back
/// `Unavailable`. Retained as a report predicate for observers; it no longer
/// gates the turn. The dispatch gate distinguishes requirement instead: an
/// *optional* infrastructure gap is reported without reentry or early finish,
/// while a *required* gap arms pending verification and is rechecked at
/// attempted completion (see `has_required_gap` / `post_dispatch_action`).
///
/// A fenced report (unbound receipts) is deliberately excluded: that is a
/// re-entry case like an actionable finding, not force-finish -- the model
/// gets a chance to reproduce the edit with valid receipts. It is also
/// excluded when the mutation genuinely has no applicable verifier for its
/// path(s) at all (`report.outcomes` empty), which is not an infrastructure
/// failure.
pub fn is_infrastructure_unverified(report: &super::FanoutReport) -> bool {
    if report.is_fenced() || report.outcomes.is_empty() {
        return false;
    }
    if !actionable_findings(report).is_empty() {
        return false;
    }
    let mut saw_unavailable = false;
    for outcome in &report.outcomes {
        match &outcome.status {
            VerifierStatus::Clean => {}
            VerifierStatus::Unavailable { .. } => saw_unavailable = true,
            VerifierStatus::Failed { .. } => return false,
        }
    }
    saw_unavailable
}

/// Render the follow-up injection block for the turn boundary.
///
/// * Some actionable findings → the block lists them (bounded), plus any
///   warnings, plus an honest unavailable-verifier tail.
/// * No actionable findings but some verifier could not run → a short block
///   saying verification could not fully confirm the edits (never silence —
///   unavailable must not read as clean).
/// * Everything ran clean (or there is nothing to say) → `None`.
pub fn render_followup_injection(report: &super::FanoutReport) -> Option<String> {
    render_followup_injection_bounded(report, DEFAULT_MAX_INJECTION_LINES)
}

pub fn render_followup_injection_bounded(
    report: &super::FanoutReport,
    max_lines: usize,
) -> Option<String> {
    if report.is_fenced() {
        return Some(format!(
            "[post-edit verification] Mutation receipts could not be bound ({} tool call(s) unbound); \
             verification was fenced and nothing was run. Re-produce the edits so they carry valid \
             receipts before continuing.",
            report.binding_errors.len()
        ));
    }
    let actionable = actionable_findings(report);
    if actionable.is_empty()
        && report
            .outcomes
            .iter()
            .all(|o| o.status == VerifierStatus::Clean)
    {
        return None;
    }
    let mut lines: Vec<String> = Vec::new();
    if actionable.is_empty() {
        lines.push(
            "[post-edit verification] No actionable findings, but verification could not fully \
             confirm the edits:"
                .to_string(),
        );
    } else {
        lines.push(
            "[post-edit verification] Findings for files you just edited. Fix the errors before \
             continuing:"
                .to_string(),
        );
    }

    let mut truncated_flag = false;
    let push_bounded = |lines: &mut Vec<String>, line: String, truncated: &mut bool| {
        if lines.len() - 1 >= max_lines {
            *truncated = true;
            return false;
        }
        lines.push(line);
        true
    };
    for finding in &actionable {
        if !push_bounded(
            &mut lines,
            format!("- {}", render_finding_line(finding)),
            &mut truncated_flag,
        ) {
            break;
        }
    }
    // Failed verifier verdicts are rendered even when they produced no
    // parsed diagnostic lines — a required check that failed with empty
    // `findings` must still block visibly instead of disappearing.
    let failed = failed_summary(&report.outcomes);
    if !failed.is_empty() && !truncated_flag {
        lines.push("Verifiers that failed:".to_string());
        for entry in failed {
            if !push_bounded(&mut lines, format!("- {entry}"), &mut truncated_flag) {
                break;
            }
        }
    }
    let warnings: Vec<&VerifiedFinding> = report
        .findings
        .iter()
        .filter(|f| matches!(f.severity, FindingSeverity::Warning))
        .collect();
    if !warnings.is_empty() && !truncated_flag {
        lines.push("Warnings (non-blocking):".to_string());
        for finding in warnings {
            if !push_bounded(
                &mut lines,
                format!("- {}", render_finding_line(finding)),
                &mut truncated_flag,
            ) {
                break;
            }
        }
    }
    let required_unavailable = unavailable_summary(&report.outcomes, VerifierRequirement::Required);
    let uncovered = uncovered_required_paths(report);
    if (!required_unavailable.is_empty() || !uncovered.is_empty()) && !truncated_flag {
        lines.push(
            "Required verification that produced no verdict (verified completion is blocked, \
             but other tools still work — diagnose or supply the missing verifier):"
                .to_string(),
        );
        for entry in required_unavailable {
            if !push_bounded(&mut lines, format!("- {entry}"), &mut truncated_flag) {
                break;
            }
        }
        for path in uncovered {
            if !push_bounded(
                &mut lines,
                format!(
                    "- {} (required): every verifier covering this file produced no verdict",
                    path.display()
                ),
                &mut truncated_flag,
            ) {
                break;
            }
        }
    }
    let optional_unavailable = unavailable_summary(&report.outcomes, VerifierRequirement::Optional);
    if !optional_unavailable.is_empty() && !truncated_flag {
        lines.push(
            "Optional verifiers that could not run (no clean claim is made for them):".to_string(),
        );
        for entry in optional_unavailable {
            if !push_bounded(&mut lines, format!("- {entry}"), &mut truncated_flag) {
                break;
            }
        }
    }
    if truncated_flag {
        lines.push("(verification findings truncated)".to_string());
    }
    Some(lines.join("\n"))
}

/// The aggregate tri-state for the whole fan-out, for gate logic one level
/// up. `Failed` dominates; "nothing ran or nothing produced a verdict" is
/// `Unavailable`, never `Clean`.
pub fn aggregate_status(report: &super::FanoutReport) -> AggregateStatus {
    if report.is_fenced() {
        return AggregateStatus::Failed {
            reason: "mutation receipts could not be bound; fan-out fenced".into(),
        };
    }
    let mut ran_any = false;
    let mut clean_any = false;
    let mut unavailable: Vec<String> = Vec::new();
    for outcome in &report.outcomes {
        match &outcome.status {
            VerifierStatus::Clean => {
                ran_any = true;
                clean_any = true;
            }
            VerifierStatus::Failed { reason } => {
                return AggregateStatus::Failed {
                    reason: format!("{}: {reason}", outcome.verifier),
                };
            }
            VerifierStatus::Unavailable { reason } => {
                unavailable.push(format!("{}: {reason}", outcome.verifier));
            }
        }
    }
    if ran_any && (clean_any || !report.findings.is_empty()) {
        return AggregateStatus::Clean;
    }
    if ran_any {
        // Ran but nothing produced a verdict worth counting as clean.
        return AggregateStatus::Unavailable {
            reason: format!(
                "no verifier produced a verdict ({})",
                unavailable.join("; ")
            ),
        };
    }
    AggregateStatus::Unavailable {
        reason: if unavailable.is_empty() {
            "no applicable verifier ran".to_string()
        } else {
            unavailable.join("; ")
        },
    }
}

/// Which verifier ids produced no verdict — used by tests and by the
/// orchestrator's diagnostics when a run degrades.
pub fn unavailable_verifier_ids(report: &super::FanoutReport) -> Vec<VerifierId> {
    report
        .outcomes
        .iter()
        .filter_map(|outcome| match outcome.status {
            VerifierStatus::Unavailable { .. } => Some(outcome.verifier.clone()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify_fanout::finding::{
        FindingCategory, FindingFixability, FindingRange, FindingSeverity, VerifiedFinding,
    };
    use crate::verify_fanout::verifiers::VerifierOutcome;
    use std::path::PathBuf;

    fn error_finding(path: &str, message: &str) -> VerifiedFinding {
        VerifiedFinding {
            source: "lsp".into(),
            range: Some(FindingRange::point(2, 4)),
            severity: FindingSeverity::Error,
            category: FindingCategory::Lsp,
            message: message.into(),
            content_hash: None,
            fixability: FindingFixability::Manual,
            path: PathBuf::from(path),
        }
    }

    fn clean_report() -> super::super::FanoutReport {
        super::super::FanoutReport {
            mutated_paths: vec![],
            verified_paths: vec![],
            outcomes: vec![VerifierOutcome::clean(
                VerifierId::lsp(),
                Some(PathBuf::from("a.rs")),
            )],
            findings: vec![],
            discarded_stale: 0,
            binding_errors: vec![],
            budget_exhausted: false,
        }
    }

    #[test]
    fn clean_report_renders_no_injection() {
        assert!(render_followup_injection(&clean_report()).is_none());
    }

    #[test]
    fn actionable_errors_render_with_location_category_and_source() {
        let mut report = clean_report();
        report
            .findings
            .push(error_finding("a.rs", "cannot find value x"));
        let block = render_followup_injection(&report).unwrap();
        assert!(block.contains("Fix the errors"));
        assert!(block.contains("a.rs:3:5"));
        assert!(block.contains("lsp via lsp"));
        assert!(block.contains("cannot find value x"));
    }

    #[test]
    fn unavailable_only_report_is_not_silent() {
        let mut report = clean_report();
        report.outcomes = vec![VerifierOutcome::unavailable(
            VerifierId::lsp(),
            Some(PathBuf::from("a.rs")),
            "no language server".into(),
        )];
        let block = render_followup_injection(&report).unwrap();
        assert!(block.contains("could not fully confirm"));
        assert!(block.contains("no language server"));
    }

    #[test]
    fn fenced_report_renders_the_fence() {
        let mut report = clean_report();
        report
            .binding_errors
            .push(crate::verify_fanout::ReceiptBindingError {
                tool_call_id: "call-1".into(),
                reason: "no receipt".into(),
            });
        let block = render_followup_injection(&report).unwrap();
        assert!(block.contains("fenced"));
    }

    #[test]
    fn aggregate_status_failed_dominates_and_nothing_ran_is_unavailable() {
        let mut report = clean_report();
        assert_eq!(aggregate_status(&report), AggregateStatus::Clean);
        report.outcomes.clear();
        assert!(matches!(
            aggregate_status(&report),
            AggregateStatus::Unavailable { .. }
        ));
        report.outcomes.push(VerifierOutcome::failed(
            VerifierId::config(),
            Some(PathBuf::from("a.json")),
            "config no longer parses".into(),
            vec![],
        ));
        assert!(matches!(
            aggregate_status(&report),
            AggregateStatus::Failed { .. }
        ));
    }

    #[test]
    fn injection_is_bounded_and_marks_truncation() {
        let mut report = clean_report();
        for i in 0..(DEFAULT_MAX_INJECTION_LINES + 10) {
            report
                .findings
                .push(error_finding(&format!("a{i}.rs"), "boom"));
        }
        let block = render_followup_injection(&report).unwrap();
        assert!(block.contains("truncated"));
        assert!(block.lines().count() <= DEFAULT_MAX_INJECTION_LINES + 3);
    }
}
