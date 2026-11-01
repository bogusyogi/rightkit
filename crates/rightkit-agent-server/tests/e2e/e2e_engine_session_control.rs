//! Engine session-control journeys over the real daemon HTTP API: approval
//! allow / deny / allow-for-session, steer, queue, cancel, permission modes.
//! A REAL `coderight serve` child talks to an independent scripted provider;
//! everything asserted is observed from outside (HTTP, durable event history,
//! files on disk, the requests the provider received).
//!
//! Evidence map: EFF-* effect/approval authority, EVT-* durable event truth,
//! KRN-001/002 session execution boundary, FM approvals/steer/queue atoms.

use crate::support::daemon_process;
use crate::support::e2e_api;
use crate::support::scripted_provider;

use e2e_api::*;
use scripted_provider::Step;
use serde_json::{json, Value};

fn approval_request(rig: &Rig, id: &str, n: usize) -> Value {
    rig.wait(&format!("approval request #{n}"), 30, || {
        rig.history(id)
            .into_iter()
            .filter(|e| kind_of(e) == "toolApprovalRequested")
            .nth(n)
    })
}

fn call_id(ev: &Value) -> String {
    let c = if ev.get("call").is_some() {
        &ev["call"]
    } else {
        &ev["data"]["call"]
    };
    c["id"].as_str().expect("call id").to_string()
}

fn worktree_file(rig: &Rig, session: &Value, rel: &str) -> Option<String> {
    let root = session["workspaceRoot"].as_str()?;
    let _ = rig;
    std::fs::read_to_string(std::path::Path::new(root).join(rel)).ok()
}

fn trust_workspace(rig: &Rig) {
    let (status, body) = rig.patch(
        "/workspace/trust",
        json!({
            "workspaceRoot": rig.ws().to_string_lossy(),
            "trust": "trusted",
            "approvedBy": "e2e",
        }),
    );
    assert_eq!(status, 200, "{body}");
}

#[test]
fn approval_allow_then_session_allow_stops_prompting_for_the_same_tool() {
    let rig = Rig::start(vec![
        Step::tool("a1", "write_file", json!({"path": "a.txt", "content": "A"})),
        Step::text("wrote a"),
        Step::tool("b1", "write_file", json!({"path": "a.txt", "content": "A"})),
        Step::text("wrote a again"),
        Step::tool("c1", "write_file", json!({"path": "c.txt", "content": "C"})),
        Step::text("wrote c"),
    ]);
    let s = rig.session("ap1", json!({}));
    assert_eq!(rig.send("ap1", "write a").0, 200);

    // Turn 1: the mutating tool is gated; nothing is written until a human approves.
    let req = approval_request(&rig, "ap1", 0);
    assert_eq!(call_id(&req), "a1");
    assert!(
        worktree_file(&rig, &s, "a.txt").is_none(),
        "effect must not land before approval"
    );
    let (status, body) = rig.post(
        "/sessions/ap1/tool-approval",
        json!({"toolCallId": "a1", "approved": true, "decision": "approved_for_session"}),
    );
    assert_eq!(status, 200, "{body}");
    rig.wait_assistant("ap1", "wrote a");
    assert_eq!(worktree_file(&rig, &s, "a.txt").as_deref(), Some("A"));
    assert!(
        rig.history("ap1")
            .iter()
            .any(|e| kind_of(e) == "toolApprovalResolved"),
        "resolution is durable"
    );

    // Turn 2: the identical payload is covered by the session grant (non-command
    // calls get an exact-payload digest grant, decision #181) -> no second prompt.
    assert_eq!(rig.send("ap1", "write a again").0, 200);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    rig.wait("assistant text wrote a again after session approval", 60, || {
        let (_, transcript) = rig.get("/sessions/ap1/transcript");
        let reply = transcript.as_array().and_then(|messages| {
            messages.iter().find(|message| {
                message["$kind"] == "assistant" && message.to_string().contains("wrote a again")
            }).cloned()
        });
        if reply.is_some() {
            return reply;
        }
        // Distinguish an execution stall from post-effect capture/settlement.
        // Keep the original 60s bound and every approval/effect assertion.
        assert!(std::time::Instant::now() < deadline,
            "second approved write stalled: a.txt={:?}; provider_calls={}; history={:?}; runtime={}",
            worktree_file(&rig, &s, "a.txt"), rig.provider.request_count(), rig.history("ap1"),
            std::fs::read_to_string(crate::common::app_root(rig.home.path()).join("runtime-middleware.json"))
                .unwrap_or_else(|error| error.to_string()));
        None
    });
    assert_eq!(worktree_file(&rig, &s, "a.txt").as_deref(), Some("A"));
    let prompts = rig
        .history("ap1")
        .iter()
        .filter(|e| kind_of(e) == "toolApprovalRequested")
        .count();
    assert_eq!(
        prompts, 1,
        "approved_for_session must suppress the second prompt for the identical payload"
    );

    // Turn 3: a DIFFERENT payload is not covered by the exact-payload grant and prompts.
    assert_eq!(rig.send("ap1", "write c").0, 200);
    let req = approval_request(&rig, "ap1", 1);
    assert_eq!(call_id(&req), "c1");
    assert!(
        worktree_file(&rig, &s, "c.txt").is_none(),
        "a different payload must not land without a fresh approval"
    );
}

#[test]
fn approval_denial_blocks_the_effect_and_tells_the_model() {
    let rig = Rig::start(vec![
        Step::tool(
            "d1",
            "write_file",
            json!({"path": "denied.txt", "content": "nope"}),
        ),
        Step::text("understood, not writing"),
    ]);
    let s = rig.session("ap2", json!({}));
    assert_eq!(rig.send("ap2", "write it").0, 200);
    let req = approval_request(&rig, "ap2", 0);
    assert_eq!(call_id(&req), "d1");
    let (status, body) = rig.post(
        "/sessions/ap2/tool-approval",
        json!({"toolCallId": "d1", "approved": false}),
    );
    assert_eq!(status, 200, "{body}");
    rig.wait_assistant("ap2", "understood, not writing");

    assert!(
        worktree_file(&rig, &s, "denied.txt").is_none(),
        "denied tool must not execute"
    );
    let resolved = rig
        .find_event("ap2", "toolApprovalResolved")
        .expect("resolution event");
    let approved = resolved
        .get("approved")
        .or_else(|| resolved["data"].get("approved"))
        .cloned();
    assert_eq!(approved, Some(json!(false)));
    // The model's follow-up request carries a tool message for the denied call.
    let reqs = rig.provider.requests();
    let tool = reqs[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("denial replayed as tool message");
    assert_eq!(tool["tool_call_id"], "d1");
}

#[test]
fn bypass_permission_mode_runs_mutations_unprompted_and_mode_changes_apply_live() {
    let rig = Rig::start(vec![
        Step::tool(
            "w1",
            "write_file",
            json!({"path": "free.txt", "content": "F"}),
        ),
        Step::text("free write ok"),
        Step::tool(
            "w2",
            "write_file",
            json!({"path": "gated.txt", "content": "G"}),
        ),
        Step::text("gated write ok"),
    ]);
    let s = rig.session("pm1", json!({"permissionMode": "bypass"}));
    assert_eq!(rig.send("pm1", "write freely").0, 200);
    rig.wait_assistant("pm1", "free write ok");
    assert_eq!(worktree_file(&rig, &s, "free.txt").as_deref(), Some("F"));
    assert!(
        !rig.kinds("pm1")
            .iter()
            .any(|k| k == "toolApprovalRequested"),
        "bypass mode must never prompt"
    );

    // Switching the live mode back to ask re-arms the gate for the next turn.
    let (status, body) = rig.post("/sessions/pm1/permission-mode", json!({"mode": "ask"}));
    assert_eq!(status, 200, "{body}");
    assert_eq!(rig.send("pm1", "write again").0, 200);
    let req = approval_request(&rig, "pm1", 0);
    assert_eq!(call_id(&req), "w2");
    assert!(
        worktree_file(&rig, &s, "gated.txt").is_none(),
        "gated write must wait for approval"
    );
    let (status, body) = rig.post(
        "/sessions/pm1/tool-approval",
        json!({"toolCallId": "w2", "approved": true}),
    );
    assert_eq!(status, 200, "{body}");
    rig.wait_assistant("pm1", "gated write ok");
    assert_eq!(worktree_file(&rig, &s, "gated.txt").as_deref(), Some("G"));
}

#[test]
fn accept_edits_mode_applies_file_writes_but_still_gates_shell() {
    let rig = Rig::start(vec![
        // This two-action prompt invokes automatic dispatch classification
        // before the turn. Answer that request without consuming its edit.
        Step::text(r#"{"complexity":"simple","reason":"one edit then one shell call"}"#),
        Step::tool(
            "e1",
            "write_file",
            json!({"path": "edit.txt", "content": "E"}),
        ),
        Step::tool(
            "e2",
            "shell",
            json!({"command": "echo ran > shell-ran.txt"}),
        ),
        Step::text("shell was denied"),
    ]);
    // Accept-edits auto-applies edits only in a TRUSTED workspace; an untrusted
    // one clamps every effectful auto-allow back to a prompt (see the untrusted
    // test below). The rig workspace starts untrusted, so record the trust first.
    trust_workspace(&rig);
    let s = rig.session("ae1", json!({"permissionMode": "accept-edits"}));
    assert_eq!(rig.send("ae1", "edit then run").0, 200);
    // The FIRST prompt is the shell call: the write was auto-applied.
    let req = approval_request(&rig, "ae1", 0);
    assert_eq!(
        call_id(&req),
        "e2",
        "file edit must not prompt under accept-edits"
    );
    assert_eq!(
        worktree_file(&rig, &s, "edit.txt").as_deref(),
        Some("E"),
        "session: {s}; history: {:?}; provider: {:?}",
        rig.history("ae1"),
        rig.provider.requests()
    );
    assert!(
        worktree_file(&rig, &s, "shell-ran.txt").is_none(),
        "shell must not run before approval"
    );
    let (status, body) = rig.post(
        "/sessions/ae1/tool-approval",
        json!({"toolCallId": "e2", "approved": false}),
    );
    assert_eq!(status, 200, "{body}");
    rig.wait_assistant("ae1", "shell was denied");
    assert!(
        worktree_file(&rig, &s, "shell-ran.txt").is_none(),
        "denied shell must never run"
    );
    let prompts = rig
        .history("ae1")
        .iter()
        .filter(|e| kind_of(e) == "toolApprovalRequested")
        .count();
    assert_eq!(prompts, 1);
}

#[test]
fn accept_edits_in_an_untrusted_workspace_still_prompts_for_file_writes() {
    let rig = Rig::start(vec![
        Step::tool(
            "u1",
            "write_file",
            json!({"path": "untrusted.txt", "content": "U"}),
        ),
        Step::text("write was denied"),
    ]);
    let s = rig.session("ae2", json!({"permissionMode": "accept-edits"}));
    assert_eq!(rig.send("ae2", "edit in untrusted workspace").0, 200);
    let req = approval_request(&rig, "ae2", 0);
    assert_eq!(
        call_id(&req),
        "u1",
        "trust clamp: untrusted workspaces never auto-apply edits"
    );
    assert!(
        worktree_file(&rig, &s, "untrusted.txt").is_none(),
        "edit must wait for approval"
    );
    let (status, body) = rig.post(
        "/sessions/ae2/tool-approval",
        json!({"toolCallId": "u1", "approved": false}),
    );
    assert_eq!(status, 200, "{body}");
    rig.wait_assistant("ae2", "write was denied");
    assert!(worktree_file(&rig, &s, "untrusted.txt").is_none());
}

#[test]
fn plan_mode_refuses_mutations_and_the_refusal_is_visible_to_the_model() {
    let rig = Rig::start(vec![
        Step::tool(
            "pl1",
            "write_file",
            json!({"path": "plan.txt", "content": "P"}),
        ),
        Step::text("plan mode noted"),
    ]);
    let s = rig.session("pl1s", json!({"permissionMode": "plan"}));
    assert_eq!(rig.send("pl1s", "try writing").0, 200);
    rig.wait_assistant("pl1s", "plan mode noted");
    assert!(
        worktree_file(&rig, &s, "plan.txt").is_none(),
        "plan mode must not mutate"
    );
    let tool = rig.provider.requests()[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .cloned()
        .expect("refusal replayed");
    assert_eq!(tool["tool_call_id"], "pl1");
    assert!(!tool["content"].to_string().is_empty());
}

#[test]
fn steer_during_a_running_turn_reaches_the_model_on_the_next_call() {
    let rig = Rig::start(vec![
        Step::tool("s1", "list_files", json!({"path": "."})).held(),
        Step::text("steered answer"),
    ]);
    rig.session("st1", json!({"permissionMode": "bypass"}));
    assert_eq!(rig.send("st1", "start working").0, 200);
    rig.wait_provider_calls(1);

    let (status, body) = rig.post(
        "/sessions/st1/steer",
        json!({"text": "STEER-MARK-31 prefer the small fix"}),
    );
    assert!(
        status == 200 || status == 202,
        "steer rejected: {status} {body}"
    );
    rig.provider.release(0);
    rig.wait_assistant("st1", "steered answer");

    assert!(
        rig.kinds("st1")
            .iter()
            .any(|k| k == "humanSteeringInjected"),
        "{:?}",
        rig.kinds("st1")
    );
    let second = rig.provider.requests()[1].to_string();
    assert!(
        second.contains("STEER-MARK-31"),
        "steering text must be in the model's next request"
    );
}

#[test]
fn messages_sent_while_busy_queue_and_run_in_order_after_the_current_turn() {
    let rig = Rig::start(vec![
        Step::text("first reply").held(),
        Step::text("second reply"),
    ]);
    rig.session("q1", json!({"permissionMode": "bypass"}));
    assert_eq!(rig.send("q1", "FIRST-MSG").0, 200);
    rig.wait_provider_calls(1);
    let (status, body) = rig.send("q1", "SECOND-MSG");
    assert!(
        status == 200 || status == 202,
        "queueing must not be an error: {status} {body}"
    );
    assert_eq!(
        rig.provider.request_count(),
        1,
        "queued message must wait for the running turn"
    );

    rig.provider.release(0);
    rig.wait_assistant("q1", "second reply");
    let reqs = rig.provider.requests();
    assert!(
        reqs[0].to_string().contains("FIRST-MSG") && !reqs[0].to_string().contains("SECOND-MSG")
    );
    assert!(reqs[1].to_string().contains("SECOND-MSG"));
    let order: Vec<String> = rig
        .history("q1")
        .iter()
        .filter(|e| kind_of(e) == "assistantMessageCompleted")
        .map(|e| e.to_string())
        .collect();
    assert!(
        order[0].contains("first reply") && order[1].contains("second reply"),
        "FIFO violated: {order:?}"
    );
}

#[test]
fn interrupt_cancels_the_inflight_turn_and_the_session_stays_usable() {
    let rig = Rig::start(vec![
        Step::text("never completes").held(),
        Step::text("fresh turn works"),
    ]);
    rig.session("ix1", json!({"permissionMode": "bypass"}));
    assert_eq!(rig.send("ix1", "long job").0, 200);
    rig.wait_provider_calls(1);

    let (status, body) = rig.post("/sessions/ix1/interrupt", json!({}));
    assert_eq!(status, 200, "{body}");
    rig.wait_event("ix1", "runInterrupted");
    rig.provider.release(0);

    assert_eq!(rig.send("ix1", "try again").0, 200);
    rig.wait_assistant("ix1", "fresh turn works");
    let finals: Vec<String> = rig
        .history("ix1")
        .iter()
        .filter(|e| kind_of(e) == "assistantMessageCompleted")
        .map(|e| e.to_string())
        .collect();
    assert!(
        !finals.iter().any(|f| f.contains("never completes")),
        "interrupted turn must not commit an assistant message"
    );
}
