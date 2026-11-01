//! Focused tests for the PL-007 verification fan-out.
//!
//! Test shapes follow the donors: the receipt-binding cases mirror the
//! `post_edit.rs` FM-179 test shapes (valid bind / missing receipt /
//! wrong-call receipt / read-only bypass); the no-language-server case
//! mirrors `post_edit_diagnostics::build_report_marks_no_server_as_
//! unverified_not_clean`; the concurrency/budget cases exercise the Pi
//! `executeToolCallsParallel` fan-out shape (bounded units, ordered
//! reassembly) with stub futures; the compiler-output cases exercise the
//! standard `path:line:col: severity: message` diagnostic contract.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
use rightkit_agent_tools::coding_tools::{ToolResult, WorkspaceMutation, WorkspaceMutationKind};

use super::collection::ReceiptBinding;
use super::injection::aggregate_status;
use super::verifiers::{
    expand_command_args, fan_out_bounded, parse_compiler_line, parse_lsp_finding_line,
    split_location_head, CommandVerifierSpec, VerifierId, VerifierOutcome, VerifierRequirement,
    VerifierStatus, VerifierTask,
};
use super::{run_verify_fanout, FanoutRequest, FindingCategory, FindingSeverity, VerifyBudgets};

fn budgets() -> VerifyBudgets {
    VerifyBudgets::default()
}

fn mutated_result(path: &Path) -> ToolResult {
    ToolResult::ok("ok").with_mutations(vec![WorkspaceMutation {
        path: path.to_path_buf(),
        kind: WorkspaceMutationKind::Modified,
        previous_content: Some("old".into()),
    }])
}

fn turn_results_from(results: Vec<(&str, ToolResult)>) -> HashMap<String, ToolResult> {
    results
        .into_iter()
        .map(|(id, result)| (id.to_string(), result))
        .collect()
}

fn request<'a>(
    workspace_root: &'a Path,
    turn_results: &'a HashMap<String, ToolResult>,
    command_verifiers: Vec<CommandVerifierSpec>,
) -> FanoutRequest<'a> {
    FanoutRequest {
        session_id: "session-1",
        workspace_root,
        permission_mode: PermissionMode::Bypass,
        workspace_trust: WorkspaceTrust::Trusted,
        turn_results,
        receipt_binding: ReceiptBinding::Unchecked,
        command_verifiers,
        budgets: budgets(),
    }
}

// ─── End-to-end: structured-config leg + staleness binding ─────────────────

#[tokio::test]
async fn broken_json_after_an_edit_is_reported_bound_and_actionable() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("package.json");
    std::fs::write(
        &config,
        "{\n  \"scripts\": {\n    \"audit\": \"broken\n  }\n}",
    )
    .unwrap();
    let turn_results = turn_results_from(vec![("call-1", mutated_result(&config))]);
    let report = run_verify_fanout(request(dir.path(), &turn_results, vec![])).await;

    assert!(!report.is_fenced());
    assert_eq!(report.mutated_paths, vec![config.clone()]);
    assert_eq!(report.verified_paths, vec![config.clone()]);
    // One config unit; its outcome is Failed with exactly one normalized
    // finding.
    let config_outcomes: Vec<_> = report
        .outcomes
        .iter()
        .filter(|o| o.verifier == VerifierId::config())
        .collect();
    assert_eq!(config_outcomes.len(), 1);
    assert!(matches!(
        config_outcomes[0].status,
        VerifierStatus::Failed { .. }
    ));
    assert_eq!(report.findings.len(), 1);
    let finding = &report.findings[0];
    assert_eq!(finding.severity, FindingSeverity::Error);
    assert_eq!(finding.category, FindingCategory::Config);
    assert!(
        finding.content_hash.is_some(),
        "finding must bind content hash"
    );
    assert!(matches!(
        finding.fixability,
        super::FindingFixability::FixAvailable { .. }
    ));
    // Tri-state roll-up: a real failure is Failed, never Unavailable.
    assert!(matches!(
        aggregate_status(&report),
        super::AggregateStatus::Failed { .. }
    ));
    // Actionable + injected back to the agent pre-completion.
    assert_eq!(super::actionable_findings(&report).len(), 1);
    let block = super::render_followup_injection(&report).unwrap();
    assert!(block.contains("package.json"));
    assert!(block.contains("Fix the errors"));
}

#[tokio::test]
async fn valid_json_after_an_edit_runs_clean() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    std::fs::write(&config, "{\"k\":1}\n").unwrap();
    let turn_results = turn_results_from(vec![("call-1", mutated_result(&config))]);
    let report = run_verify_fanout(request(dir.path(), &turn_results, vec![])).await;
    assert!(report.findings.is_empty());
    assert!(matches!(
        aggregate_status(&report),
        super::AggregateStatus::Clean
    ));
    assert!(super::render_followup_injection(&report).is_none());
}

#[tokio::test]
async fn broken_toml_after_an_edit_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = dir.path().join("Cargo.toml");
    std::fs::write(&manifest, "[package\nname = \"x\"\n").unwrap();
    let turn_results = turn_results_from(vec![("call-1", mutated_result(&manifest))]);
    let report = run_verify_fanout(request(dir.path(), &turn_results, vec![])).await;
    assert_eq!(report.findings.len(), 1);
    assert!(report.findings[0].message.contains("no longer valid TOML"));
}

#[tokio::test]
async fn no_applicable_verifier_is_unavailable_never_clean() {
    let dir = tempfile::tempdir().unwrap();
    let notes = dir.path().join("README.md");
    std::fs::write(&notes, "hello").unwrap();
    let turn_results = turn_results_from(vec![("call-1", mutated_result(&notes))]);
    let report = run_verify_fanout(request(dir.path(), &turn_results, vec![])).await;
    assert!(report.outcomes.is_empty());
    assert!(report.findings.is_empty());
    // Nothing ran: the honest roll-up is Unavailable, not Clean.
    assert!(matches!(
        aggregate_status(&report),
        super::AggregateStatus::Unavailable { .. }
    ));
}

#[tokio::test]
async fn deleted_files_are_not_verified() {
    let dir = tempfile::tempdir().unwrap();
    let gone = dir.path().join("gone.json");
    let turn_results = turn_results_from(vec![(
        "call-1",
        ToolResult::ok("removed").with_mutations(vec![WorkspaceMutation {
            path: gone.clone(),
            kind: WorkspaceMutationKind::Deleted,
            previous_content: None,
        }]),
    )]);
    let report = run_verify_fanout(request(dir.path(), &turn_results, vec![])).await;
    assert_eq!(report.mutated_paths, vec![gone]);
    assert!(report.verified_paths.is_empty());
    assert!(report.outcomes.is_empty());
}

// ─── FM-179 receipt-binding fence (end-to-end) ─────────────────────────────

#[tokio::test]
async fn unbound_mutations_fence_the_whole_run() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("broken.json");
    std::fs::write(&config, "{ not json").unwrap();
    let turn_results = turn_results_from(vec![("call-1", mutated_result(&config))]);
    let receipts: HashMap<String, crate::assurance_broker::ToolReceipt> = HashMap::new();
    let mut req = request(dir.path(), &turn_results, vec![]);
    req.receipt_binding = ReceiptBinding::Enforced {
        receipts: &receipts,
    };
    let report = run_verify_fanout(req).await;

    assert!(report.is_fenced());
    assert_eq!(report.binding_errors.len(), 1);
    assert_eq!(report.binding_errors[0].tool_call_id, "call-1");
    // Fenced means nothing ran and nothing is claimed.
    assert!(report.outcomes.is_empty());
    assert!(report.findings.is_empty());
    let block = super::render_followup_injection(&report).unwrap();
    assert!(block.contains("fenced"));
}

// ─── Command verifiers ─────────────────────────────────────────────────────

#[tokio::test]
async fn missing_command_binary_is_unavailable_not_clean() {
    // A non-code extension so no LSP unit is assembled (a real language
    // server installed on the host would make the LSP outcome
    // environment-dependent); the command unit is the only verifier here.
    let dir = tempfile::tempdir().unwrap();
    let notes = dir.path().join("notes.md");
    std::fs::write(&notes, "# notes\n").unwrap();
    let turn_results = turn_results_from(vec![("call-1", mutated_result(&notes))]);
    let spec = CommandVerifierSpec {
        id: "spellcheck".into(),
        category: FindingCategory::Lint,
        argv: vec![
            "definitely-not-a-real-verifier-xyz".into(),
            "--files".into(),
            "{paths}".into(),
        ],
        applicable_extensions: vec!["md".into()],
        path_prefixes: vec![],
        required: true,
        fixable: false,
    };
    let report = run_verify_fanout(request(dir.path(), &turn_results, vec![spec])).await;
    let command_outcomes: Vec<_> = report
        .outcomes
        .iter()
        .filter(|o| o.verifier.0 == "command:spellcheck")
        .collect();
    assert_eq!(command_outcomes.len(), 1);
    assert!(matches!(
        command_outcomes[0].status,
        VerifierStatus::Unavailable { .. }
    ));
    // Nothing produced a verdict: the honest roll-up is Unavailable, not
    // Clean — an absent verifier never makes the run clean.
    assert!(matches!(
        aggregate_status(&report),
        super::AggregateStatus::Unavailable { .. }
    ));
}

#[test]
fn expand_command_args_requires_explicit_path_placeholder() {
    let paths = vec![PathBuf::from("a.rs"), PathBuf::from("b.rs")];
    let with_placeholder: Vec<String> = vec!["check".into(), "{paths}".into(), "--quiet".into()];
    assert_eq!(
        expand_command_args(&with_placeholder, &paths),
        vec!["check", "a.rs", "b.rs", "--quiet"]
    );
    let without_placeholder: Vec<String> = vec!["check".into(), "--all".into()];
    assert_eq!(
        expand_command_args(&without_placeholder, &paths),
        vec!["check", "--all"]
    );
}

#[tokio::test]
async fn plain_text_edit_in_rust_workspace_does_not_launch_cargo_verifiers() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname='probe'\nversion='0.1.0'\n",
    )
    .unwrap();
    let path = dir.path().join("notes.txt");
    std::fs::write(&path, "edited\n").unwrap();
    let results = turn_results_from(vec![("call-1", mutated_result(&path))]);
    let specs = super::verifiers::default_command_verifiers(dir.path());
    assert!(!specs.is_empty());
    let report = run_verify_fanout(request(dir.path(), &results, specs)).await;
    assert_eq!(report.mutated_paths, vec![path]);
    assert!(report.outcomes.is_empty());
    assert!(!super::is_infrastructure_unverified(&report));
    assert!(super::render_followup_injection(&report).is_none());
    // No applicable verifier is not a claim that diagnostics ran clean.
    assert!(matches!(
        aggregate_status(&report),
        super::AggregateStatus::Unavailable { .. }
    ));
}

#[test]
fn rust_workspace_commands_keep_manifest_arguments_without_source_paths() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").unwrap();
    for spec in super::verifiers::default_command_verifiers(dir.path()) {
        assert!(spec.applies_to(&dir.path().join("src/main.rs")));
        assert!(spec.applies_to(&dir.path().join("Cargo.toml")));
        assert!(spec.applies_to(&dir.path().join("Cargo.lock")));
        assert!(!spec.applies_to(&dir.path().join("notes.md")));
        assert!(!spec.applies_to(&dir.path().join("input.txt")));
        let expanded = expand_command_args(&spec.argv, &[dir.path().join("src/main.rs")]);
        assert_eq!(expanded, spec.argv);
        let manifest_pos = expanded
            .iter()
            .position(|arg| arg == "--manifest-path")
            .unwrap();
        if let Some(separator) = expanded.iter().position(|arg| arg == "--") {
            assert!(manifest_pos + 1 < separator);
        }
    }
}

#[test]
fn parse_compiler_line_reads_the_standard_diagnostic_shape() {
    let parsed =
        parse_compiler_line("src/main.rs:3:5: error[E0382]: use of moved value `x`").unwrap();
    assert_eq!(parsed.path, "src/main.rs");
    assert_eq!(parsed.line, 3);
    assert_eq!(parsed.column, Some(5));
    assert_eq!(parsed.severity, FindingSeverity::Error);
    assert_eq!(parsed.message, "use of moved value `x`");

    let warning = parse_compiler_line("src/a.rs:10: warning: unused import").unwrap();
    assert_eq!(warning.severity, FindingSeverity::Warning);
    assert_eq!(warning.column, None);
    assert_eq!(warning.message, "unused import");

    let note = parse_compiler_line("src/a.rs:12:4: note: this frame has 1 field").unwrap();
    assert_eq!(note.severity, FindingSeverity::Information);

    // No recognized severity word: kept as a warning, message preserved.
    let plain = parse_compiler_line("src/a.rs:14:3: unexpected token in verifier output").unwrap();
    assert_eq!(plain.severity, FindingSeverity::Warning);
    assert_eq!(plain.message, "unexpected token in verifier output");

    // Non-diagnostic lines yield nothing.
    assert!(parse_compiler_line("Compiling rightkit-agent-engine v0.1.0").is_none());
    assert!(parse_compiler_line("").is_none());
}

#[test]
fn split_location_head_handles_windows_drive_paths() {
    let (path, line, column) = split_location_head("C:\\src\\a.rs:3:5").unwrap();
    assert_eq!(path, "C:\\src\\a.rs");
    assert_eq!(line, 3);
    assert_eq!(column, Some(5));

    let (path, line, column) = split_location_head("/abs/path/b.rs:10").unwrap();
    assert_eq!(path, "/abs/path/b.rs");
    assert_eq!(line, 10);
    assert_eq!(column, None);

    assert!(split_location_head("just-a-word").is_none());
}

// ─── LSP line parsing (the CG-B01 wire shape) ──────────────────────────────

#[test]
fn lsp_wire_line_parses_range_severity_and_message() {
    let rendered = "file:///w/a.rs:3:5 severity=1 cannot find value x";
    let (range, severity, message) = parse_lsp_finding_line(rendered).unwrap();
    // Stored in LSP wire convention: 0-based line, 0-based character.
    assert_eq!(range.start_line, 2);
    assert_eq!(range.start_column, 5);
    assert_eq!(severity, FindingSeverity::Error);
    assert_eq!(message, "cannot find value x");

    let warning = "file:///w/a.rs:9:1 severity=2 unused import";
    let (range, severity, _) = parse_lsp_finding_line(warning).unwrap();
    assert_eq!(range.start_line, 8);
    assert_eq!(severity, FindingSeverity::Warning);

    // Unknown severities have no honest mapping and are dropped.
    assert!(parse_lsp_finding_line("file:///w/a.rs:1:1 severity=5 weird").is_none());
    // Non-diagnostic lines (the no-server / no-diagnostics responses) are
    // not findings.
    assert!(parse_lsp_finding_line("No diagnostics reported.").is_none());
}

// ─── Bounded concurrency machinery (Pi executeToolCallsParallel shape) ─────

fn stub_task_with(
    id: &str,
    budget: Duration,
    delay: Duration,
    findings: usize,
    requirement: VerifierRequirement,
    covered_paths: Vec<PathBuf>,
) -> VerifierTask {
    let verifier = VerifierId(format!("stub:{id}"));
    VerifierTask {
        verifier: verifier.clone(),
        subject: None,
        requirement,
        covered_paths,
        budget,
        run: Box::pin(async move {
            tokio::time::sleep(delay).await;
            let verifier = verifier.clone();
            VerifierOutcome::with_findings(
                verifier,
                None,
                VerifierStatus::Clean,
                (0..findings)
                    .map(|i| super::VerifiedFinding {
                        source: "stub".into(),
                        range: None,
                        severity: FindingSeverity::Warning,
                        category: FindingCategory::Test,
                        message: format!("finding {i}"),
                        content_hash: None,
                        fixability: super::FindingFixability::Manual,
                        path: PathBuf::from("stub.txt"),
                    })
                    .collect(),
            )
        }),
    }
}

fn stub_task(id: &str, budget: Duration, delay: Duration, findings: usize) -> VerifierTask {
    stub_task_with(
        id,
        budget,
        delay,
        findings,
        VerifierRequirement::Required,
        Vec::new(),
    )
}

#[tokio::test]
async fn per_verifier_budget_turns_an_overrun_into_unavailable() {
    let tasks = vec![stub_task(
        "slow",
        Duration::from_millis(50),
        Duration::from_secs(5),
        0,
    )];
    let outcomes = fan_out_bounded(tasks, &budgets()).await;
    assert_eq!(outcomes.len(), 1);
    assert!(matches!(
        outcomes[0].status,
        VerifierStatus::Unavailable { ref reason }
            if reason.contains("per-verifier budget exhausted")
    ));
}

#[tokio::test]
async fn aggregate_budget_cancels_the_rest_and_records_it_honestly() {
    let budgets = VerifyBudgets {
        aggregate: Duration::from_millis(300),
        ..budgets()
    };
    let tasks = vec![
        stub_task("fast", Duration::from_secs(10), Duration::from_millis(1), 1),
        stub_task("slow", Duration::from_secs(30), Duration::from_secs(5), 0),
    ];
    let outcomes = fan_out_bounded(tasks, &budgets).await;
    assert_eq!(outcomes.len(), 2);
    // Fast one completed and is reported.
    assert!(matches!(outcomes[0].status, VerifierStatus::Clean));
    // Slow one was cancelled by the aggregate budget, not reported clean.
    assert!(matches!(
        outcomes[1].status,
        VerifierStatus::Unavailable { ref reason }
            if reason.contains("aggregate verification budget exhausted")
    ));
}

#[tokio::test]
async fn results_reassemble_in_submission_order_not_completion_order() {
    let tasks = vec![
        stub_task(
            "first",
            Duration::from_secs(5),
            Duration::from_millis(120),
            0,
        ),
        stub_task(
            "second",
            Duration::from_secs(5),
            Duration::from_millis(40),
            0,
        ),
        stub_task("third", Duration::from_secs(5), Duration::from_millis(1), 0),
    ];
    let outcomes = fan_out_bounded(tasks, &budgets()).await;
    let ids: Vec<String> = outcomes.iter().map(|o| o.verifier.0.clone()).collect();
    assert_eq!(
        ids,
        vec![
            "stub:first".to_string(),
            "stub:second".to_string(),
            "stub:third".to_string()
        ]
    );
    assert!(outcomes.iter().all(|o| o.status == VerifierStatus::Clean));
}

#[tokio::test]
async fn unit_budget_caps_admission_and_records_the_overflow() {
    let budgets = VerifyBudgets {
        max_verifiers: 2,
        ..budgets()
    };
    let tasks = vec![
        stub_task("one", Duration::from_secs(5), Duration::from_millis(1), 0),
        stub_task("two", Duration::from_secs(5), Duration::from_millis(1), 0),
        stub_task("three", Duration::from_secs(5), Duration::from_millis(1), 0),
    ];
    let outcomes = fan_out_bounded(tasks, &budgets).await;
    assert_eq!(outcomes.len(), 4);
    let overflow: Vec<_> = outcomes
        .iter()
        .filter(|o| o.verifier.0 == VerifierId::BUDGET)
        .collect();
    assert_eq!(overflow.len(), 1);
    let skipped = outcomes
        .iter()
        .find(|outcome| outcome.verifier.0 == "stub:three")
        .unwrap();
    assert!(skipped.requirement.is_required());
    assert!(matches!(skipped.status, VerifierStatus::Unavailable { .. }));
    assert!(matches!(
        overflow[0].status,
        VerifierStatus::Unavailable { .. }
    ));
}

#[tokio::test]
async fn a_panicking_unit_degrades_to_unavailable_without_poisoning_the_fanout() {
    let verifier = VerifierId("stub:panic".into());
    let tasks = vec![
        stub_task("fine", Duration::from_secs(5), Duration::from_millis(1), 0),
        VerifierTask {
            verifier: verifier.clone(),
            subject: None,
            requirement: VerifierRequirement::Required,
            covered_paths: Vec::new(),
            budget: Duration::from_secs(5),
            run: Box::pin(async move { panic!("verifier exploded") }),
        },
    ];
    let outcomes = fan_out_bounded(tasks, &budgets()).await;
    assert_eq!(outcomes.len(), 2);
    assert_eq!(outcomes[1].verifier, verifier);
    assert!(outcomes[1].requirement.is_required());
    assert!(matches!(outcomes[0].status, VerifierStatus::Clean));
    assert!(matches!(
        outcomes[1].status,
        VerifierStatus::Unavailable { ref reason }
            if reason.contains("panicked")
    ));
}

#[tokio::test]
async fn finding_cap_truncates_and_marks_the_outcome() {
    let budgets = VerifyBudgets {
        max_findings_per_verifier: 2,
        ..budgets()
    };
    let tasks = vec![stub_task(
        "chatty",
        Duration::from_secs(5),
        Duration::from_millis(1),
        5,
    )];
    let outcomes = fan_out_bounded(tasks, &budgets).await;
    assert_eq!(outcomes.len(), 1);
    assert!(outcomes[0].truncated);
    assert_eq!(outcomes[0].findings.len(), 2);
}

#[tokio::test]
async fn tri_state_distinctions_hold_for_every_outcome_shape() {
    // The canon requires unavailable ≠ clean ≠ failed; this pins the three
    // constructors against each other.
    let clean = VerifierOutcome::clean(VerifierId::lsp(), None);
    let unavailable = VerifierOutcome::unavailable(VerifierId::lsp(), None, "no server".into());
    let failed = VerifierOutcome::failed(VerifierId::lsp(), None, "errors".into(), vec![]);
    assert_eq!(clean.status, VerifierStatus::Clean);
    assert_ne!(clean.status, unavailable.status);
    assert_ne!(clean.status, failed.status);
    assert_ne!(unavailable.status, failed.status);
    match unavailable.status {
        VerifierStatus::Unavailable { reason } => assert!(!reason.is_empty()),
        other => panic!("expected Unavailable, got {other:?}"),
    }
    match failed.status {
        VerifierStatus::Failed { reason } => assert!(!reason.is_empty()),
        other => panic!("expected Failed, got {other:?}"),
    }
}

// ─── LSP unit over the CG-B01 seam (host builds only) ──────────────────────

#[cfg(all(feature = "host", windows))]
#[tokio::test]
async fn real_python_project_checks_survive_optional_lsp_and_bulk_edits() {
    // The Windows Store `python.exe` alias is not an interpreter; skip when no
    // real one is provisioned rather than failing on the stub.
    if super::profile::resolve_python_interpreter(Path::new("."), Path::new(".")).is_none() {
        eprintln!("skipped: no real Python interpreter (Windows Store alias ignored)");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".rightkit")).unwrap();
    std::fs::write(dir.path().join(".rightkit/lsp.json"), r#"{"servers":[{"language":"python","extensions":["py"],"serverPath":"host-missing-lsp-fixture","serverArgs":["--stdio"]}]}"#).unwrap();
    std::fs::write(dir.path().join(".rightkit/verification.json"), r#"{"version":1,"commands":[{"id":"project-tests","category":"test","argv":["{python}","-B","-m","unittest","test_calc"],"extensions":["py"],"required":true}]}"#).unwrap();
    std::fs::write(dir.path().join("test_calc.py"), "import unittest\nfrom calc import value\nclass Check(unittest.TestCase):\n    def test_value(self):\n        self.assertEqual(value(), 2)\n").unwrap();
    let source = dir.path().join("calc.py");
    std::fs::write(&source, "def value():\n    return 2\n").unwrap();
    let mut results = turn_results_from(vec![("edit", mutated_result(&source))]);
    for index in 0..6 {
        let path = dir.path().join(format!("bulk{index}.py"));
        std::fs::write(&path, "answer = 42\n").unwrap();
        results.insert(format!("bulk{index}"), mutated_result(&path));
    }
    let clean = run_verify_fanout(request(dir.path(), &results, vec![])).await;
    assert!(!has_required_gap(&clean), "{:?}", clean.outcomes);
    let syntax = clean
        .outcomes
        .iter()
        .find(|outcome| outcome.verifier.0 == "command:python-syntax")
        .unwrap();
    assert_eq!(syntax.covered_paths.len(), 7);
    assert_eq!(syntax.status, VerifierStatus::Clean);
    assert!(matches!(
        post_dispatch_action(&clean),
        PostDispatchAction::ReportOnly(_)
    ));
    std::fs::write(&source, "def value(\n").unwrap();
    let syntax_failed = run_verify_fanout(request(dir.path(), &results, vec![])).await;
    assert!(has_required_gap(&syntax_failed));
    assert!(syntax_failed
        .findings
        .iter()
        .any(|finding| finding.category == FindingCategory::Syntax && finding.path == source));
    std::fs::write(&source, "def value():\n    return 9\n").unwrap();
    let test_failed = run_verify_fanout(request(dir.path(), &results, vec![])).await;
    assert!(has_required_gap(&test_failed));
    let test = test_failed
        .outcomes
        .iter()
        .find(|outcome| outcome.verifier.0 == "command:project-tests")
        .unwrap();
    assert!(
        matches!(&test.status, VerifierStatus::Failed {reason} if reason.contains("AssertionError"))
    );
    std::fs::write(&source, "def value():\n    return 2\n").unwrap();
    let repaired = run_verify_fanout(request(dir.path(), &results, vec![])).await;
    assert!(!has_required_gap(&repaired), "{:?}", repaired.outcomes);
    assert!(!dir.path().join("__pycache__").exists());
}

#[cfg(all(feature = "host", windows))]
#[tokio::test]
async fn untrusted_project_commands_cannot_execute_during_verification() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".rightkit")).unwrap();
    std::fs::write(dir.path().join(".rightkit/verification.json"), r#"{"version":1,"commands":[{"id":"must-not-run","category":"test","argv":["{python}","-c","open('unexpected-effect','w').write('bad')"],"extensions":["py"]}]}"#).unwrap();
    let source = dir.path().join("x.py");
    std::fs::write(&source, "value = 1\n").unwrap();
    let results = turn_results_from(vec![("edit", mutated_result(&source))]);
    let mut req = request(dir.path(), &results, vec![]);
    req.workspace_trust = WorkspaceTrust::Untrusted;
    let report = run_verify_fanout(req).await;
    assert!(has_required_gap(&report));
    assert!(!dir.path().join("unexpected-effect").exists());
    assert!(report.outcomes.iter().any(|outcome| matches!(&outcome.status, VerifierStatus::Unavailable {reason} if reason.contains("trusted workspace"))));
}

#[cfg(feature = "host")]
mod host_lsp {
    use super::*;
    use rightkit_agent_tools::coding_tools::lsp_tool::{
        write_lsp_workspace_config, LspWorkspaceConfig, LspWorkspaceServer,
    };

    #[tokio::test]
    async fn no_language_server_is_unavailable_not_clean() {
        let dir = tempfile::tempdir().unwrap();
        write_lsp_workspace_config(
            dir.path(),
            &LspWorkspaceConfig {
                servers: vec![LspWorkspaceServer {
                    language: "rust".into(),
                    extensions: vec!["rs".into()],
                    server_path: "definitely-not-a-real-langserver-xyz".into(),
                    server_args: vec![],
                }],
            },
        )
        .unwrap();
        let code = dir.path().join("a.rs");
        std::fs::write(&code, "fn main() { let x = 1; }\n").unwrap();
        let turn_results = turn_results_from(vec![("call-1", mutated_result(&code))]);
        let report = run_verify_fanout(request(dir.path(), &turn_results, vec![])).await;

        let lsp_outcomes: Vec<_> = report
            .outcomes
            .iter()
            .filter(|o| o.verifier == VerifierId::lsp())
            .collect();
        assert_eq!(lsp_outcomes.len(), 1);
        // A server that cannot run produced no verdict: Unavailable — the
        // same fail-closed discipline the post-edit diagnostics seam uses.
        assert!(
            matches!(lsp_outcomes[0].status, VerifierStatus::Unavailable { .. }),
            "no-server run must be Unavailable, got {:?}",
            lsp_outcomes[0].status
        );
        assert!(report.findings.is_empty());
        // And the honest aggregate is not Clean.
        assert!(!matches!(
            aggregate_status(&report),
            super::super::AggregateStatus::Clean
        ));
    }

    #[tokio::test]
    async fn config_and_lsp_and_command_units_run_concurrently_over_one_turn() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("broken.json"), "{ nope").unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn main() {}\n").unwrap();
        let mut turn_results = turn_results_from(vec![(
            "call-1",
            mutated_result(&dir.path().join("broken.json")),
        )]);
        turn_results.insert(
            "call-2".to_string(),
            mutated_result(&dir.path().join("a.rs")),
        );
        let spec = CommandVerifierSpec {
            id: "lint".into(),
            category: FindingCategory::Lint,
            argv: vec![
                "definitely-not-a-real-verifier-xyz".into(),
                "{paths}".into(),
            ],
            applicable_extensions: vec!["rs".into()],
            path_prefixes: vec![],
            required: true,
            fixable: false,
        };
        let report = run_verify_fanout(request(dir.path(), &turn_results, vec![spec])).await;

        assert_eq!(report.verified_paths.len(), 2);
        // One config unit for the JSON + one LSP unit for the code file +
        // one command unit that applies to the .rs file: three units total.
        assert_eq!(report.outcomes.len(), 3);
        let has_config_failure = report.outcomes.iter().any(|o| {
            o.verifier == VerifierId::config() && matches!(o.status, VerifierStatus::Failed { .. })
        });
        assert!(has_config_failure, "{:?}", report.outcomes);
        let has_command_unavailable = report.outcomes.iter().any(|o| {
            o.verifier.0 == "command:lint" && matches!(o.status, VerifierStatus::Unavailable { .. })
        });
        assert!(has_command_unavailable, "{:?}", report.outcomes);
        // The broken JSON finding survived normalization, dedupe, and
        // staleness (the file did not change during the run).
        assert!(report
            .findings
            .iter()
            .any(|f| f.path.ends_with("broken.json")));
    }
}

#[cfg(feature = "host")]
#[test]
fn interpreter_launch_env_carries_exactly_install_dir_and_venv_home() {
    let dir = tempfile::tempdir().unwrap();
    let install = dir.path().join("Python311");
    let scripts = dir.path().join("proj/.venv/Scripts");
    std::fs::create_dir_all(&install).unwrap();
    std::fs::create_dir_all(&scripts).unwrap();
    // Roots are honoured only where an interpreter actually lives.
    std::fs::write(install.join("python.exe"), b"x").unwrap();
    std::fs::write(scripts.join("python.exe"), b"x").unwrap();
    std::fs::write(
        dir.path().join("proj/.venv/pyvenv.cfg"),
        format!("home = {}\n", install.display()),
    )
    .unwrap();

    let plain = super::verifiers::launch_read_roots_env(&super::profile::interpreter_read_roots(
        &install.join("python.exe"),
    ));
    assert_eq!(
        rightkit_agent_tools::coding_tools::sandbox::parse_sandbox_read_roots(&plain),
        vec![install.clone()]
    );

    let venv = super::verifiers::launch_read_roots_env(&super::profile::interpreter_read_roots(
        &scripts.join("python.exe"),
    ));
    assert_eq!(
        rightkit_agent_tools::coding_tools::sandbox::parse_sandbox_read_roots(&venv),
        vec![scripts, install]
    );

    // A non-interpreter program requests nothing.
    assert!(
        super::verifiers::launch_read_roots_env(&super::profile::interpreter_read_roots(
            &dir.path().join("pyright.exe")
        ))
        .is_empty()
    );
}
