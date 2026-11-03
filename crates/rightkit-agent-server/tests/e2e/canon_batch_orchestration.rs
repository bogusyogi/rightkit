//! Runtime orchestration contracts for provider verification, handoff & resolution.

use crate::support::{e2e_api, e2e_cli};
use serde_json::json;

#[test]
fn pl007_successful_edit_returns_bound_verification_findings_before_completion() {
    let p = e2e_cli::Provider::start(vec![
        e2e_cli::Reply::Chat(e2e_cli::Chat::tool(
            "w",
            "write_file",
            json!({"path":"src/main.rs","content":"fn main(){ let x: u8 = 999; }"}),
        )),
        // EFF-003/005: FileService requires current bytes before replacing an existing file.
        e2e_cli::Reply::Chat(e2e_cli::Chat::tool(
            "read-repair",
            "read_file",
            json!({"path":"src/main.rs"}),
        )),
        e2e_cli::Reply::Chat(e2e_cli::Chat::tool(
            "repair",
            "write_file",
            json!({"path":"src/main.rs","content":"fn main(){ let x: u8 = 99; }"}),
        )),
        e2e_cli::Reply::Chat(e2e_cli::Chat::text("finished")),
    ]);
    // PL-007 runs applicable provisioned verifiers; a loose .rs file does
    // not provision rust-analyzer or a Cargo project. Supply a bounded
    // static verifier through the shipped project verification profile.
    let cli = e2e_cli::Cli::with_config(
        &p,
        "",
        &[
            (
                ".coderight/verification.json",
                r#"{"version":1,"commands":[{"id":"u8-bound","category":"static-analysis","argv":["node","verify-u8.js","{paths}"],"extensions":["rs"],"required":true}]}"#,
            ),
            (
                "verify-u8.js",
                r#"const fs = require('fs');
let failed = false;
for (const path of process.argv.slice(2)) {
  const source = fs.readFileSync(path, 'utf8');
  const match = /:\s*u8\s*=\s*(\d+)/.exec(source);
  if (match && Number(match[1]) > 255) {
    process.stdout.write(path + ':1:1: error: u8 literal exceeds 255\n');
    failed = true;
  }
}
process.exitCode = failed ? 1 : 0;
"#,
            ),
        ],
    );
    // PL-007/TrustPolicy: bypass permits tool effects, but never grants trust
    // to workspace-supplied verifier commands. Provision the fixture's trust
    // through the same shipped command an operator uses before execution.
    let trusted = cli.exec(&["trust"], &[]);
    assert_eq!(
        trusted.code,
        Some(0),
        "verifier workspace trust: {}",
        trusted.diag()
    );
    let out = cli.run(
        &["--stream-json", "--bypass-approvals"],
        "write invalid Rust",
    );
    assert_eq!(out.code, Some(0), "{}", out.diag());
    let events = out.events();
    let findings = events
        .iter()
        .position(|e| e.to_string().contains("verificationFinding"));
    let completed = events
        .iter()
        .position(|e| e.to_string().contains("assistantMessageCompleted"));
    assert!(
        findings.is_some(),
        "fan-out must surface verifier findings: {}",
        out.diag()
    );
    assert!(completed.is_some() && findings < completed,
        "actionable findings reach originating agent before completion; findings={findings:?}; completed={completed:?}; requests={:?}; {}",
        p.requests(), out.diag());
    assert!(
        p.requests()[1]["messages"]
            .to_string()
            .contains("u8 literal exceeds 255"),
        "finding reaches originating model before repair"
    );
    assert_eq!(
        p.count(),
        4,
        "edit, read current bytes, verification-guided repair, then completion"
    );
    assert_eq!(
        cli.written("src/main.rs").as_deref(),
        Some("fn main(){ let x: u8 = 99; }"),
        "verified repair landed"
    );
}

#[test]
#[ignore = "controller handoff routes not implemented yet (PL-008)"]
fn pl008_controller_handoff_is_epoch_fenced_and_preflight_failure_preserves_source() {
    let rig = e2e_api::Rig::start(vec![]);
    rig.session("handoff", json!({}));
    let (status, result) = rig.post(
        "/sessions/handoff/controller-handoff",
        json!({"target":"claude","preflightOnly":true}),
    );
    assert_eq!(
        status, 409,
        "unavailable target preflight is typed: {result}"
    );
    let (_, state) = rig.get("/sessions/handoff/controller");
    assert_eq!(
        state["authoritativeController"], "coderight",
        "failed preflight cannot tear down source"
    );
    assert!(
        state["controllerEpoch"].as_u64().unwrap_or(0) > 0,
        "controller epoch is explicit"
    );
}

#[test]
fn pl009_external_observation_is_deduplicated_durable_and_creates_one_causal_task() {
    let mut rig = e2e_api::Rig::start(vec![]);
    let (status, trusted) = rig.patch(
        "/workspace/trust",
        json!({
            "workspaceRoot":rig.ws().to_string_lossy(), "trust":"trusted", "approvedBy":"user"
        }),
    );
    assert_eq!(
        status, 200,
        "provision trusted observation workspace: {trusted}"
    );
    rig.session("observe", json!({}));
    let observation = json!({"source":"ci","externalId":"run-42","workspaceRoot":rig.ws().to_string_lossy(),"subject":"main","kind":"ciFailure","summary":"tests failed","evidence":"job=test","evidenceDigest":"abc123","actionable":true,"trusted":true,"observedAt":"2026-10-07T00:00:00Z"});
    let body = json!({"observation":observation,"approvalGranted":true});
    let (status, first) = rig.post("/sessions/observe/external-observations", body.clone());
    assert_eq!(status, 200, "{first}");
    assert_eq!(
        first["work"].as_array().map(Vec::len),
        Some(1),
        "one corrective task emitted"
    );
    let task_id = first["work"][0]["taskId"].as_str().expect("task ID");
    let (status, duplicate) = rig.post("/sessions/observe/external-observations", body.clone());
    assert_eq!(status, 200, "{duplicate}");
    assert!(
        duplicate["work"]
            .as_array()
            .is_some_and(|rows| rows.is_empty()),
        "replay cannot duplicate task: {duplicate}"
    );
    let (_, taskgraph) = rig.get("/sessions/observe/runtime/taskgraph");
    assert!(
        taskgraph.to_string().contains(task_id),
        "durable canonical task is linked to observation"
    );
    let db = rightkit_db::rusqlite::Connection::open(&rig.daemon.startup.database_path)
        .expect("canonical task store");
    let stored: String = db
        .query_row(
            "SELECT node_json FROM tasks WHERE session_id = ?1 AND id = ?2",
            ("observe", task_id),
            |row| row.get(0),
        )
        .expect("corrective task persisted with reaction marker");
    assert!(
        stored.contains(first["receipt"].as_str().expect("observation identity")),
        "canonical task retains reverse causal link"
    );
    drop(db);
    rig.restart();
    rig.session("observe", json!({}));
    let (status, replay) = rig.post("/sessions/observe/external-observations", body);
    assert_eq!(status, 200, "restart replay: {replay}");
    assert!(
        replay["work"]
            .as_array()
            .is_some_and(|rows| rows.is_empty()),
        "restart cannot duplicate corrective work: {replay}"
    );
    let (status, restored) = rig.get("/sessions/observe/runtime/taskgraph");
    assert_eq!(status, 200, "restored task graph: {restored}");
    assert_eq!(
        restored.to_string().matches(task_id).count(),
        1,
        "one canonical causal task survives restart: {restored}"
    );
}

#[test]
#[ignore = "mechanism routing not wired into sessions yet (PL-010)"]
fn pl010_non_model_mechanism_resolves_without_provider_call() {
    let provider = e2e_cli::Provider::start(vec![]);
    let cli = e2e_cli::Cli::new(&provider);
    let out = cli.run(
        &["--stream-json"],
        "format this JSON: { \"b\": 2, \"a\": 1 }",
    );
    assert_eq!(
        provider.count(),
        0,
        "deterministic structured operation resolves without inference: {}",
        out.diag()
    );
    assert_eq!(out.code, Some(0), "{}", out.diag());
    assert!(
        out.stdout.contains("a") && out.stdout.contains("b"),
        "resolved result is returned: {}",
        out.stdout
    );
}
