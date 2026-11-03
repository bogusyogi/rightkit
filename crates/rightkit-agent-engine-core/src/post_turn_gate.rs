//! Post-turn loop-safety gate.
//!
//! This module is deliberately pure. It does not run hooks, diagnostics, or
//! LLMs; it only turns existing outcomes into engine-loop decisions.

use rightkit_agent_engine_protocol::ToolCall;
use rightkit_hooks::agent_hooks::HookOutcome;

use crate::post_edit_diagnostics::{DiagnosticSeverity, DiagnosticSource, PostEditReport};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostTurnGateDecision {
    Proceed,
    Reenter { instruction: String, reason: String },
    ForceFinish { warning: String },
}

/// True if `tool_call` is for a tool whose successful execution reports
/// workspace mutations. Stale name-match is acceptable here because the
/// engine is going to apply the post-edit guard to whatever mutations the
/// tool returns; the name matches the four write-class tools we ship.
fn is_legacy_edit_tool_name(name: &str) -> bool {
    matches!(
        name,
        "edit_file" | "write_file" | "multi_edit" | "apply_patch"
    )
}

/// Decide whether a turn's mutation-bearing tool calls left any "unbacked
/// mutation" — a WriteFile-class success whose mutations list is empty.
/// Returns the offending tool call ID, if any. Read-only turns can never
/// produce an unbacked mutation by construction.
pub fn detect_unbacked_mutations(
    tool_calls: &[ToolCall],
    tool_results: &std::collections::HashMap<
        String,
        rightkit_agent_tools::coding_tools::ToolResult,
    >,
) -> Vec<String> {
    let mut offenders = Vec::new();
    for call in tool_calls {
        if !is_legacy_edit_tool_name(&call.name) {
            continue;
        }
        let Some(result) = tool_results.get(&call.id) else {
            continue;
        };
        if result.is_error {
            continue;
        }
        if result.mutations.is_empty() {
            offenders.push(call.id.clone());
        }
    }
    offenders
}

#[derive(Debug, Clone)]
pub struct PostTurnGateState {
    consecutive_stop_blocks: usize,
    max_consecutive_stop_blocks: usize,
    /// Diagnostic infrastructure failures observed this run. When the engine
    /// accumulates more than the cap, the next gate call returns `ForceFinish`
    /// with an explicit "we have lost the ability to verify edits" warning.
    /// The count is informational; gating logic reads the latest diagnostic
    /// decision directly.
    #[allow(dead_code)]
    diagnostic_infrastructure_failures: usize,
}

impl Default for PostTurnGateState {
    fn default() -> Self {
        Self {
            consecutive_stop_blocks: 0,
            max_consecutive_stop_blocks: Self::MAX_CONSECUTIVE_STOP_BLOCKS,
            diagnostic_infrastructure_failures: 0,
        }
    }
}

impl PostTurnGateState {
    pub const MAX_CONSECUTIVE_STOP_BLOCKS: usize = 8;

    pub fn consecutive_stop_blocks(&self) -> usize {
        self.consecutive_stop_blocks
    }

    /// Build a reentry instruction from a typed diagnostic report. Errors and
    /// unverified items always fail closed: the engine is told to fix the
    /// files (or rule out the unverifiable claim) before continuing. Warnings
    /// are surfaced but do NOT block the turn — that is a deliberate
    /// difference from the prior string-prefix renderer.
    pub fn after_diagnostics_report(&mut self, report: &PostEditReport) -> PostTurnGateDecision {
        let blocking: Vec<_> = report
            .findings
            .iter()
            .filter(|f| {
                matches!(
                    f.severity,
                    DiagnosticSeverity::Error | DiagnosticSeverity::Unverified
                )
            })
            .collect();

        if blocking.is_empty() {
            return PostTurnGateDecision::Proceed;
        }

        // Required diagnostic uncertainty or infrastructure failure fails
        // closed: any Unverified finding means we lack the ability to certify
        // the edit, so the turn does NOT proceed.
        let has_unverified = blocking
            .iter()
            .any(|f| f.severity == DiagnosticSeverity::Unverified);
        if has_unverified
            && blocking
                .iter()
                .all(|f| f.severity == DiagnosticSeverity::Unverified)
        {
            self.diagnostic_infrastructure_failures += 1;
            return PostTurnGateDecision::ForceFinish {
                warning: format!(
                    "[Loop-safety gate: required diagnostic infrastructure could not verify {} edited file(s); failing closed. Files: {}]",
                    blocking.len(),
                    blocking
                        .iter()
                        .map(|f| f.path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            };
        }

        PostTurnGateDecision::Reenter {
            instruction: render_blocking_instruction(blocking.iter().copied()),
            reason: "post_edit_diagnostics".into(),
        }
    }

    /// Decide what to do after the engine has aggregated one turn's
    /// mutations AND the post-edit diagnostics failed closed. Returns
    /// `ForceFinish` if the mutation list includes a successful tool whose
    /// mutations is empty AND the unbacked contract would otherwise let a
    /// clean response slip out.
    pub fn after_unbacked_mutations(
        &mut self,
        offenders: &[String],
        touched_paths: &[std::path::PathBuf],
    ) -> PostTurnGateDecision {
        if offenders.is_empty() {
            return PostTurnGateDecision::Proceed;
        }
        let path_summary = touched_paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        PostTurnGateDecision::Reenter {
            instruction: format!(
                "[unbacked mutation block] tool call{} {} returned success but reported no workspace mutation. The post-edit guard cannot verify what landed on disk; re-run the edit (or restate the affected files explicitly) before continuing. Touched paths observed this turn: {}",
                if offenders.len() == 1 { "" } else { "s" },
                offenders.join(", "),
                if path_summary.is_empty() { "<none>".to_string() } else { path_summary }
            ),
            reason: "unbacked_mutation".into(),
        }
    }

    pub fn after_blocking_stop(&mut self, outcome: HookOutcome) -> PostTurnGateDecision {
        match outcome {
            HookOutcome::Proceed => {
                self.consecutive_stop_blocks = 0;
                PostTurnGateDecision::Proceed
            }
            HookOutcome::Error(msg) => {
                self.consecutive_stop_blocks = 0;
                PostTurnGateDecision::ForceFinish {
                    warning: format!(
                        "[Loop-safety gate warning: blocking Stop hook errored; proceeding so hook failures do not wedge the run. Error: {msg}]"
                    ),
                }
            }
            HookOutcome::Deny(msg) => {
                self.consecutive_stop_blocks += 1;
                if self.consecutive_stop_blocks >= self.max_consecutive_stop_blocks {
                    PostTurnGateDecision::ForceFinish {
                        warning: format!(
                            "[Loop-safety gate override: blocking Stop hook denied completion {} consecutive times. Proceeding with a graceful stop so the agent cannot loop forever. Last denial: {msg}]",
                            self.max_consecutive_stop_blocks
                        ),
                    }
                } else {
                    PostTurnGateDecision::Reenter {
                        instruction: format!(
                            "[Stop gate blocked completion] {msg}\n\nAddress the blocker with a different, minimal next step. Do not repeat the same action. If it cannot be addressed, explain why and stop."
                        ),
                        reason: "blocking_stop_hook".into(),
                    }
                }
            }
        }
    }
}

fn render_blocking_instruction<'a, I>(findings: I) -> String
where
    I: Iterator<Item = &'a crate::post_edit_diagnostics::DiagnosticFinding>,
{
    let mut sections: Vec<String> = Vec::new();
    for finding in findings {
        let label = match finding.severity {
            DiagnosticSeverity::Error => "Error",
            DiagnosticSeverity::Unverified => "Unverified",
            DiagnosticSeverity::Warning => "Warning",
        };
        let source = match finding.source {
            DiagnosticSource::StructuredConfig => "structured-config",
            DiagnosticSource::LanguageServer => "language-server",
        };
        sections.push(format!(
            "{} ({} via {}): {}",
            finding.path.display(),
            label,
            source,
            finding.message
        ));
    }
    format!(
        "[auto-diagnostics] Typed findings require fixing before this turn can proceed:\n\n{}",
        sections.join("\n\n")
    )
}
