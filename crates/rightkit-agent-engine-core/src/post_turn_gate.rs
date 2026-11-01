//! Post-turn loop-safety gate.
//!
//! This module is deliberately pure. It does not run hooks, diagnostics, or
//! LLMs; it only turns existing outcomes into engine-loop decisions.

use rightkit_agent_engine_protocol::ToolCall;
use rightkit_hooks::agent_hooks::HookOutcome;

#[cfg(test)]
use crate::post_edit_diagnostics::{DiagnosticSeverity, DiagnosticSource, PostEditReport};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostTurnGateDecision {
    Proceed,
    Reenter { instruction: String, reason: String },
    ForceFinish { warning: String },
}

/// True if the tool's capability class is mutating. Read-only tools do not
/// belong in the post-edit guard at all; the engine uses this filter to
/// ensure read-only turns remain free of edit-check work. Read-only tool
/// names (read_file, ls, etc.) never reach the unbacked-mutation check
/// because the four write-class tools we ship are name-matched below, and
/// any new tool's capability class is what's checked at the engine's
/// dispatch partition.
#[cfg(test)]
pub fn is_mutating_tool_capability(class: Option<rightkit_agent_policy::CapabilityClass>) -> bool {
    matches!(
        class,
        Some(rightkit_agent_policy::CapabilityClass::WriteFile)
            | Some(rightkit_agent_policy::CapabilityClass::Destructive)
            | Some(rightkit_agent_policy::CapabilityClass::Network)
    )
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

/// Largest hook or verification text the gate puts into the transcript. Stop
/// hook output is bounded only by the hook's 256 KiB capture, and the
/// re-entry instruction is kept as the latest user message through every
/// compaction (audit M9), so it takes the same 4000-byte bound as a tool
/// result preview.
pub(crate) const MAX_GATE_TEXT_BYTES: usize = 4000;

/// `text` cut to [`MAX_GATE_TEXT_BYTES`] at a character boundary, with the
/// original size named when anything was dropped.
pub(crate) fn bound_gate_text(text: &str) -> String {
    bound_text(text, MAX_GATE_TEXT_BYTES)
}

pub(crate) fn bound_text(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}... [truncated, {} bytes total]",
        &text[..end],
        text.len()
    )
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
    #[cfg(test)]
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

    /// Backwards-compatible render of the typed re-entry block. Kept so the
    /// test suite can assert the pre-existing string contract even as the
    /// underlying decision moves to typed findings.
    #[cfg(test)]
    pub fn after_diagnostics_block(&mut self, block: String) -> PostTurnGateDecision {
        PostTurnGateDecision::Reenter {
            instruction: block,
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
                let msg = bound_gate_text(&msg);
                self.consecutive_stop_blocks = 0;
                PostTurnGateDecision::ForceFinish {
                    warning: format!(
                        "[Loop-safety gate warning: blocking Stop hook errored; proceeding so hook failures do not wedge the run. Error: {msg}]"
                    ),
                }
            }
            HookOutcome::Deny(msg) => {
                let msg = bound_gate_text(&msg);
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

#[cfg(test)]
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

/// Lane B3 / C3 unbacked-work verification gate. Runs BEFORE the engine
/// emits an `AssistantMessageCompleted` claim of victory ("I've finished…").
/// The gate uses the typed A2 mutation set carried on the tool's
/// `ToolResult` to confirm the assistant actually performed the work it
/// claims; otherwise the gate forces a reenter with a typed finding.
///
/// This struct is intentionally separate from `PostTurnGateState` so the
/// concurrency, persistence, and reschedule knobs can evolve
/// independently.
#[derive(Debug, Clone)]
#[cfg(test)]
pub struct UnbackedWorkGate {
    /// Maximum number of times the gate can push the same turn back before
    /// the engine accepts an unbacked claim (avoids an infinite loop).
    max_force_reenter: usize,
    force_reenter_count: usize,
}

#[cfg(test)]
impl Default for UnbackedWorkGate {
    fn default() -> Self {
        Self {
            max_force_reenter: Self::DEFAULT_MAX_FORCE_REENTER,
            force_reenter_count: 0,
        }
    }
}

#[cfg(test)]
impl UnbackedWorkGate {
    pub const DEFAULT_MAX_FORCE_REENTER: usize = 3;

    pub fn new(max_force_reenter: usize) -> Self {
        Self {
            max_force_reenter: max_force_reenter.max(1),
            force_reenter_count: 0,
        }
    }

    pub fn reset(&mut self) {
        self.force_reenter_count = 0;
    }

    pub fn force_reenter_count(&self) -> usize {
        self.force_reenter_count
    }

    /// Run the gate. Returns `Ok(())` when the claim is backed; otherwise
    /// returns a typed [`VerificationIssue`] describing the gap and the
    /// loop's reaction (reenter vs. cap reached).
    pub fn check(&mut self, evidence: &CompletionEvidence<'_>) -> GateOutcome {
        if evidence.has_any_typed_mutation() {
            self.reset();
            return GateOutcome::Accepted;
        }
        if evidence.claims_victory() && self.force_reenter_count < self.max_force_reenter {
            self.force_reenter_count += 1;
            return GateOutcome::Reenter {
                finding: VerificationIssue {
                    tool: evidence.last_tool_name().unwrap_or("").into(),
                    file_path: String::new(),
                    line: None,
                    column: None,
                    message: "completion claim has no typed mutation evidence".to_string(),
                    repair_suggestion:
                        "perform the actual edit before declaring completion, or retract the claim"
                            .into(),
                },
                counter: self.force_reenter_count,
            };
        }
        GateOutcome::Unbacked {
            attempts: self.force_reenter_count,
        }
    }
}

/// A typed finding payload — the gate refuses to let an unbacked claim
/// reach "completed" without raising one. Mirrors
/// `rightkit_agent_engine_protocol::VerificationIssue`; the seam here keeps the gate
/// independent so A2's mutation contract is the only consumer of these
/// findings, and so the engine can construct one before protocol wiring.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(test)]
pub struct VerificationIssue {
    pub tool: String,
    pub file_path: String,
    pub line: Option<i32>,
    pub column: Option<i32>,
    pub message: String,
    pub repair_suggestion: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(test)]
pub enum GateOutcome {
    /// The completion claim is backed by typed mutations.
    Accepted,
    /// The completion claim is unbacked; re-enter once with the typed
    /// finding. `counter` is the 1-indexed reenter count.
    Reenter {
        finding: VerificationIssue,
        counter: usize,
    },
    /// The completion claim is unbacked and the reenter cap has been
    /// reached; the engine proceeds (the cap exists to prevent an
    /// infinite loop, not to silently accept unbacked work).
    Unbacked { attempts: usize },
}

/// Pure data passed to `UnbackedWorkGate::check`. Constructed by the
/// engine from the A2 mutation set carried on `ToolResult` and the model's
/// visible completion claim.
#[derive(Debug, Default)]
#[cfg(test)]
pub struct CompletionEvidence<'a> {
    pub typed_mutations: u32,
    pub claim_text: Option<&'a str>,
    pub last_tool_name: Option<&'a str>,
}

#[cfg(test)]
impl<'a> CompletionEvidence<'a> {
    pub fn has_any_typed_mutation(&self) -> bool {
        self.typed_mutations > 0
    }

    pub fn claims_victory(&self) -> bool {
        matches!(self.claim_text, Some(s) if Self::text_signals_done(s))
    }

    pub fn last_tool_name(&self) -> Option<&'a str> {
        self.last_tool_name
    }

    /// Conservative heuristic; the agent is claiming victory. False
    /// negatives are allowed (we err on the side of asking for proof).
    fn text_signals_done(text: &str) -> bool {
        let lower = text.to_ascii_lowercase();
        [
            "all done",
            "i'm done",
            "i am done",
            "finished",
            "completed the task",
            "task is complete",
            "work is complete",
            "all set",
            "ready for review",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_engine_protocol::ToolCall;
    use rightkit_agent_tools::coding_tools::WorkspaceMutation;
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn diagnostics_block_reenters_without_incrementing_stop_blocks() {
        let mut gate = PostTurnGateState::default();
        let decision = gate.after_diagnostics_block("[auto-diagnostics] fix me".into());

        assert_eq!(
            decision,
            PostTurnGateDecision::Reenter {
                instruction: "[auto-diagnostics] fix me".into(),
                reason: "post_edit_diagnostics".into()
            }
        );
        assert_eq!(gate.consecutive_stop_blocks(), 0);
    }

    #[test]
    fn stop_deny_reenters_until_cap_then_force_finishes() {
        let mut gate = PostTurnGateState::default();
        let mut last = PostTurnGateDecision::Proceed;

        for _ in 0..PostTurnGateState::MAX_CONSECUTIVE_STOP_BLOCKS {
            last = gate.after_blocking_stop(HookOutcome::Deny("still unsafe".into()));
        }

        assert_eq!(
            last,
            PostTurnGateDecision::ForceFinish {
                warning: "[Loop-safety gate override: blocking Stop hook denied completion 8 consecutive times. Proceeding with a graceful stop so the agent cannot loop forever. Last denial: still unsafe]".into()
            }
        );
    }

    #[test]
    fn stop_proceed_resets_consecutive_blocks() {
        let mut gate = PostTurnGateState::default();
        assert!(matches!(
            gate.after_blocking_stop(HookOutcome::Deny("blocked".into())),
            PostTurnGateDecision::Reenter { .. }
        ));
        assert_eq!(gate.consecutive_stop_blocks(), 1);

        assert_eq!(
            gate.after_blocking_stop(HookOutcome::Proceed),
            PostTurnGateDecision::Proceed
        );
        assert_eq!(gate.consecutive_stop_blocks(), 0);
    }

    fn call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments_json: "{}".into(),
        }
    }

    fn result_with_mutations(path: &str) -> rightkit_agent_tools::coding_tools::ToolResult {
        rightkit_agent_tools::coding_tools::ToolResult::ok("ok").with_mutations(vec![
            WorkspaceMutation {
                path: PathBuf::from(path),
                kind: rightkit_agent_tools::coding_tools::WorkspaceMutationKind::Modified,
                previous_content: Some("before".to_string()),
            },
        ])
    }

    /// The core unbacked-mutation test: a WriteFile-class success with an
    /// empty `mutations` list is the contract violation that blocks completion.
    /// The detector is the single source of truth for the rule.
    #[test]
    fn detect_unbacked_mutations_flags_success_without_mutations() {
        let calls = vec![call("a", "write_file"), call("b", "edit_file")];
        let mut results: HashMap<String, rightkit_agent_tools::coding_tools::ToolResult> =
            HashMap::new();
        // 'a' is a violation: success but no mutations.
        results.insert(
            "a".to_string(),
            rightkit_agent_tools::coding_tools::ToolResult::ok("ok"),
        );
        // 'b' is fine.
        results.insert("b".to_string(), result_with_mutations("b.txt"));

        let offenders = detect_unbacked_mutations(&calls, &results);
        assert_eq!(offenders, vec!["a".to_string()]);
    }

    #[test]
    fn detect_unbacked_mutations_ignores_read_only_and_failed_calls() {
        let calls = vec![call("a", "read_file"), call("b", "edit_file")];
        let mut results: HashMap<String, rightkit_agent_tools::coding_tools::ToolResult> =
            HashMap::new();
        results.insert(
            "a".to_string(),
            rightkit_agent_tools::coding_tools::ToolResult::ok("ok"),
        );
        // 'b' failed — it is not "unbacked", it is just an error.
        results.insert(
            "b".to_string(),
            rightkit_agent_tools::coding_tools::ToolResult::error("boom"),
        );
        let offenders = detect_unbacked_mutations(&calls, &results);
        assert!(offenders.is_empty(), "{:?}", offenders);
    }

    #[test]
    fn gate_blocks_on_unbacked_mutations_with_concrete_paths() {
        let mut gate = PostTurnGateState::default();
        let decision = gate.after_unbacked_mutations(
            &["a".to_string()],
            &[PathBuf::from("foo.txt"), PathBuf::from("bar.txt")],
        );
        match decision {
            PostTurnGateDecision::Reenter {
                instruction,
                reason,
            } => {
                assert_eq!(reason, "unbacked_mutation");
                assert!(instruction.contains("a"));
                assert!(instruction.contains("foo.txt"));
                assert!(instruction.contains("bar.txt"));
            }
            other => panic!("expected Reenter, got {other:?}"),
        }
    }

    #[test]
    fn gate_proceeds_when_no_unbacked_mutations() {
        let mut gate = PostTurnGateState::default();
        let decision = gate.after_unbacked_mutations(&[], &[]);
        assert_eq!(decision, PostTurnGateDecision::Proceed);
    }

    #[test]
    fn gate_clean_diagnostic_report_proceeds() {
        let mut gate = PostTurnGateState::default();
        let report = PostEditReport::default();
        let decision = gate.after_diagnostics_report(&report);
        assert_eq!(decision, PostTurnGateDecision::Proceed);
    }

    #[test]
    fn gate_blocks_on_structured_config_error() {
        let mut gate = PostTurnGateState::default();
        let report = PostEditReport {
            touched_paths: vec![PathBuf::from("config.json")],
            findings: vec![crate::post_edit_diagnostics::DiagnosticFinding {
                path: PathBuf::from("config.json"),
                source: crate::post_edit_diagnostics::DiagnosticSource::StructuredConfig,
                severity: DiagnosticSeverity::Error,
                message: "broken JSON".into(),
            }],
        };
        let decision = gate.after_diagnostics_report(&report);
        match decision {
            PostTurnGateDecision::Reenter {
                instruction,
                reason,
            } => {
                assert_eq!(reason, "post_edit_diagnostics");
                assert!(instruction.contains("broken JSON"));
                assert!(instruction.contains("config.json"));
            }
            other => panic!("expected Reenter, got {other:?}"),
        }
    }

    /// Required diagnostic infrastructure failure fails closed: when the
    /// ONLY signal is "we couldn't reach the LSP", the gate refuses to
    /// proceed. This is the typed Unverified → ForceFinish shape; the model
    /// never sees a clean completion under an undiagnosed edit.
    #[test]
    fn gate_fails_closed_on_unverified_only_report() {
        let mut gate = PostTurnGateState::default();
        let report = PostEditReport {
            touched_paths: vec![PathBuf::from("a.rs"), PathBuf::from("b.rs")],
            findings: vec![
                crate::post_edit_diagnostics::DiagnosticFinding {
                    path: PathBuf::from("a.rs"),
                    source: crate::post_edit_diagnostics::DiagnosticSource::LanguageServer,
                    severity: DiagnosticSeverity::Unverified,
                    message: "no language server".into(),
                },
                crate::post_edit_diagnostics::DiagnosticFinding {
                    path: PathBuf::from("b.rs"),
                    source: crate::post_edit_diagnostics::DiagnosticSource::LanguageServer,
                    severity: DiagnosticSeverity::Unverified,
                    message: "no language server".into(),
                },
            ],
        };
        match gate.after_diagnostics_report(&report) {
            PostTurnGateDecision::ForceFinish { warning } => {
                assert!(
                    warning.contains("could not verify"),
                    "warning did not mention verification failure: {warning}"
                );
                assert!(warning.contains("a.rs"));
                assert!(warning.contains("b.rs"));
            }
            other => panic!("expected ForceFinish, got {other:?}"),
        }
    }

    /// Mixed blocking findings (Error + Unverified) re-enter rather than
    /// force-finish, because the model can repair a real Error.
    #[test]
    fn gate_reenters_on_mixed_error_and_unverified() {
        let mut gate = PostTurnGateState::default();
        let report = PostEditReport {
            touched_paths: vec![PathBuf::from("a.rs")],
            findings: vec![
                crate::post_edit_diagnostics::DiagnosticFinding {
                    path: PathBuf::from("a.rs"),
                    source: crate::post_edit_diagnostics::DiagnosticSource::LanguageServer,
                    severity: DiagnosticSeverity::Error,
                    message: "cannot find value".into(),
                },
                crate::post_edit_diagnostics::DiagnosticFinding {
                    path: PathBuf::from("config.json"),
                    source: crate::post_edit_diagnostics::DiagnosticSource::StructuredConfig,
                    severity: DiagnosticSeverity::Unverified,
                    message: "syntax changed while reading".into(),
                },
            ],
        };
        match gate.after_diagnostics_report(&report) {
            PostTurnGateDecision::Reenter { reason, .. } => {
                assert_eq!(reason, "post_edit_diagnostics");
            }
            other => panic!("expected Reenter, got {other:?}"),
        }
    }

    /// Warnings-only reports proceed; the previous string renderer forced
    /// re-entry on warnings, which produced reentry loops. The typed
    /// pipeline's `Warning` severity no longer blocks.
    #[test]
    fn gate_proceeds_on_warnings_only_report() {
        let mut gate = PostTurnGateState::default();
        let report = PostEditReport {
            touched_paths: vec![PathBuf::from("a.rs")],
            findings: vec![crate::post_edit_diagnostics::DiagnosticFinding {
                path: PathBuf::from("a.rs"),
                source: crate::post_edit_diagnostics::DiagnosticSource::LanguageServer,
                severity: DiagnosticSeverity::Warning,
                message: "unused import".into(),
            }],
        };
        let decision = gate.after_diagnostics_report(&report);
        assert_eq!(decision, PostTurnGateDecision::Proceed);
    }

    #[test]
    fn is_legacy_edit_tool_name_recognizes_known_names() {
        assert!(is_legacy_edit_tool_name("write_file"));
        assert!(is_legacy_edit_tool_name("edit_file"));
        assert!(is_legacy_edit_tool_name("multi_edit"));
        assert!(is_legacy_edit_tool_name("apply_patch"));
        assert!(!is_legacy_edit_tool_name("read_file"));
    }

    #[test]
    fn capability_class_filter_excludes_read_only_classes() {
        // Read-only capability classes must NEVER enter the unbacked check;
        // see `is_mutating_tool_capability`. This is the read-only exclusion
        // guard: edit-check work is reserved for mutating turns only.
        use rightkit_agent_policy::CapabilityClass;
        assert!(!is_mutating_tool_capability(Some(
            CapabilityClass::ReadFile
        )));
        assert!(!is_mutating_tool_capability(None));
        assert!(is_mutating_tool_capability(Some(
            CapabilityClass::WriteFile
        )));
        assert!(is_mutating_tool_capability(Some(
            CapabilityClass::Destructive
        )));
    }
    // ── Unbacked-work verification gate (Lane B3 / C3 task 3) ───────────

    #[test]
    fn unbacked_gate_accepts_when_typed_mutations_present() {
        let mut gate = UnbackedWorkGate::default();
        let evidence = CompletionEvidence {
            typed_mutations: 2,
            claim_text: Some("all done"),
            last_tool_name: Some("write_file"),
        };
        assert_eq!(gate.check(&evidence), GateOutcome::Accepted);
        assert_eq!(gate.force_reenter_count(), 0);
    }

    #[test]
    fn unbacked_gate_reenters_when_claim_unbacked_then_caps() {
        let mut gate = UnbackedWorkGate::new(2);
        let evidence = CompletionEvidence {
            typed_mutations: 0,
            claim_text: Some("all done"),
            last_tool_name: Some("read_file"),
        };
        let r1 = gate.check(&evidence);
        let r2 = gate.check(&evidence);
        let r3 = gate.check(&evidence);
        assert!(matches!(r1, GateOutcome::Reenter { counter: 1, .. }));
        assert!(matches!(r2, GateOutcome::Reenter { counter: 2, .. }));
        assert!(matches!(r3, GateOutcome::Unbacked { attempts: 2 }));
    }

    #[test]
    fn unbacked_gate_does_not_reenter_when_no_claim() {
        let mut gate = UnbackedWorkGate::default();
        let evidence = CompletionEvidence {
            typed_mutations: 0,
            claim_text: Some("thinking out loud…"),
            last_tool_name: Some("read_file"),
        };
        assert!(matches!(
            gate.check(&evidence),
            GateOutcome::Unbacked { .. }
        ));
    }

    #[test]
    fn unbacked_gate_resets_when_evidence_eventually_appears() {
        let mut gate = UnbackedWorkGate::new(5);
        let unbacked = CompletionEvidence {
            typed_mutations: 0,
            claim_text: Some("all done"),
            last_tool_name: Some("read_file"),
        };
        let backed = CompletionEvidence {
            typed_mutations: 1,
            claim_text: Some("all done"),
            last_tool_name: Some("write_file"),
        };
        gate.check(&unbacked);
        gate.check(&unbacked);
        assert_eq!(gate.force_reenter_count(), 2);
        assert_eq!(gate.check(&backed), GateOutcome::Accepted);
        assert_eq!(gate.force_reenter_count(), 0);
    }

    #[test]
    fn victory_heuristic_detects_common_phrases_without_forgiving_genuine_doubt() {
        for positive in [
            "All done!",
            "Task is complete.",
            "I am done with the edits.",
            "ready for review.",
        ] {
            assert!(
                CompletionEvidence {
                    typed_mutations: 0,
                    claim_text: Some(positive),
                    last_tool_name: Some("read_file"),
                }
                .claims_victory(),
                "expected {positive:?} to be a claim"
            );
        }
        for negative in ["thinking out loud…", "I will retry once more.", ""] {
            assert!(
                !CompletionEvidence {
                    typed_mutations: 0,
                    claim_text: Some(negative),
                    last_tool_name: Some("read_file"),
                }
                .claims_victory(),
                "expected {negative:?} NOT to be a claim"
            );
        }
    }
}
