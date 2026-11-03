//! Journeys for the model-facing orchestration tools, driven through a real `coderight serve`
//! daemon and a scripted provider: `send_message`, `team_create` / `team_delete`,
//! `exit_plan_mode` (approve + reject), `wait_for_subagents`, `followup_subagent`,
//! `interrupt_subagent`, and a `schedule_cron`-created schedule that really fires.
//! Each test asserts the daemon-side effect AND the exact text the model received.

use crate::support::e2e_orch;
use e2e_orch::*;

use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

fn children_of(env: &Env, parent: &str) -> Vec<Value> {
    let (_, sessions) = env.get("/sessions?includeSubagents=true");
    sessions
        .as_array()
        .or_else(|| sessions["sessions"].as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s["parentSessionId"] == parent || s["parent_session_id"] == parent)
        .collect()
}

/// Tool results the model of the session whose first user message contains `marker` saw in its
/// LAST request (that request carries every earlier result).
fn final_tool_results(env: &Env, marker: &str) -> Vec<String> {
    env.provider
        .requests()
        .iter()
        .filter(|r| first_user_text(r).contains(marker))
        .map(tool_results)
        .max_by_key(|r| r.len())
        .unwrap_or_default()
}

fn session_workspace(env: &Env, id: &str) -> std::path::PathBuf {
    // The daemon exposes session rows through the list route (there is no GET /sessions/{id}).
    let (_, sessions) = env.get("/sessions?includeSubagents=true");
    let s = sessions
        .as_array()
        .or_else(|| sessions["sessions"].as_array())
        .and_then(|rows| rows.iter().find(|row| row["id"] == id))
        .cloned()
        .unwrap_or_else(|| panic!("session {id} is not in the daemon session list: {sessions}"));
    std::path::PathBuf::from(
        s["workspaceRoot"]
            .as_str()
            .unwrap_or_else(|| panic!("session {id} has no workspaceRoot: {s}")),
    )
}

// ───────────────────────── team_* and send_message ────────────────────────

#[test]
fn team_and_message_tools_reach_the_live_daemon_directory_and_peer_sessions() {
    let provider = Provider::start(|body, _| {
        let first = first_user_text(body);
        if first.contains("hello peer") {
            return Reply::text("peer-got-it");
        }
        match tool_results(body).len() {
            0 => Reply::tool(
                "t0",
                "team_create",
                json!({"name":"review-squad","members":["architect","tester"],"description":"review changes","max_workers":2}),
            ),
            1 => Reply::tool(
                "t1",
                "team_create",
                json!({"name":"ops-squad","members":["sre"]}),
            ),
            2 => Reply::tool(
                "t2",
                "team_create",
                json!({"name":"empty-squad","members":[]}),
            ),
            3 => Reply::tool("t3", "team_delete", json!({"name":"review-squad"})),
            4 => Reply::tool("t4", "team_delete", json!({"name":"review-squad"})),
            5 => Reply::tool(
                "t5",
                "send_message",
                json!({"session_id":"tm-peer","message":"hello peer"}),
            ),
            6 => Reply::tool(
                "t6",
                "send_message",
                json!({"session_id":"tm-nobody","message":"anyone there"}),
            ),
            _ => Reply::text("lead finished"),
        }
    });
    let env = Env::start(provider, &[], |_, _| {});
    env.create_session("tm-peer");
    assert_eq!(env.turn("tm-lead", "TEAM-LEAD: manage teams"), "completed");

    let r = final_tool_results(&env, "TEAM-LEAD");
    assert_eq!(r.len(), 7, "{r:?}");
    assert_eq!(
        r[0],
        "Team created: review-squad (review-squad registered with 2 member(s))"
    );
    assert_eq!(
        r[1],
        "Team created: ops-squad (ops-squad registered with 1 member(s))"
    );
    assert!(
        r[2].contains("team must include at least one member"),
        "{}",
        r[2]
    );
    assert_eq!(
        r[3],
        "Team deleted: review-squad (review-squad removed from live directory)"
    );
    assert!(r[4].contains("team not found: review-squad"), "{}", r[4]);
    assert_eq!(r[5], "Message dispatched: delivered to tm-peer");
    assert!(
        r[6].contains("send_message: live dispatch failed"),
        "{}",
        r[6]
    );

    // The daemon's live directory holds exactly the surviving team, with its fields.
    let (s, teams) = env.get("/teams");
    assert_eq!(s, 200, "{teams}");
    let rows = teams["teams"].as_array().expect("teams array");
    assert_eq!(rows.len(), 1, "{teams}");
    assert_eq!(rows[0]["name"], "ops-squad");
    assert_eq!(rows[0]["members"], json!(["sre"]));

    // The peer session really received the relayed message and ran a turn on it.
    assert_eq!(env.wait_turns("tm-peer", 1), "completed");
    let peer_prompt = env
        .provider
        .requests()
        .iter()
        .map(whole_prompt)
        .find(|p| p.contains("hello peer"))
        .expect("peer's model request must carry the relayed message");
    assert!(
        peer_prompt.contains("[Message from tm-lead via notification]"),
        "{peer_prompt}"
    );
    let (_, history) = env.get("/messages");
    let recorded = history["messages"].to_string();
    assert!(
        recorded.contains("hello peer") && recorded.contains("delivered"),
        "message history must record the delivery: {recorded}"
    );
}

// ───────────────────────────── exit_plan_mode ─────────────────────────────

fn plan_flow_provider() -> Provider {
    Provider::start(|body, _| match tool_results(body).len() {
        0 => Reply::tool(
            "w0",
            "write_file",
            json!({"path":"plan-gate.txt","content":"first-attempt"}),
        ),
        1 => Reply::tool(
            "p1",
            "exit_plan_mode",
            json!({"plan":"1. create plan-gate.txt\n2. verify it"}),
        ),
        2 => Reply::tool(
            "w2",
            "write_file",
            json!({"path":"plan-gate.txt","content":"second-attempt"}),
        ),
        _ => Reply::text("plan flow finished"),
    })
}

fn present_plan(env: &Env, id: &str) -> (String, String) {
    env.create_session_with(id, json!({"permissionMode":"plan"}));
    env.say(id, "PLAN-FLOW: make the file");
    let row = env.eventually("planPresented", 60, || {
        env.events(id, "planPresented").first().cloned()
    });
    let plan_id = row["planId"]
        .as_str()
        .or_else(|| row["data"]["planId"].as_str())
        .unwrap_or_else(|| panic!("no planId in {row}"))
        .to_string();
    let plan = row["plan"]
        .as_str()
        .or_else(|| row["data"]["plan"].as_str())
        .unwrap_or_default()
        .to_string();
    (plan_id, plan)
}

#[test]
fn exit_plan_mode_blocks_for_approval_and_approval_unlocks_writes() {
    let env = Env::start(plan_flow_provider(), &[], |_, _| {});
    // EFF policy preserves the workspace trust clamp after Plan -> AcceptEdits.
    // Qualify plan approval in a trusted workspace, as the accept-edits journey does.
    let (status, body) = env.send(
        "PATCH",
        "/workspace/trust",
        json!({
            "workspaceRoot": env.workspace, "trust": "trusted", "approvedBy": "e2e",
        }),
    );
    assert_eq!(status, 200, "{body}");
    let (plan_id, plan) = present_plan(&env, "plan-ok");
    assert_eq!(plan, "1. create plan-gate.txt\n2. verify it");

    let root = session_workspace(&env, "plan-ok");
    assert!(
        !root.join("plan-gate.txt").exists(),
        "plan mode must have refused the first write"
    );
    // Single-use, unknown ids are refused.
    assert_eq!(
        env.post(
            "/sessions/plan-ok/resolve-plan",
            json!({"planId":"plan-unknown","approved":true})
        )
        .0,
        404
    );
    let (s, b) = env.post(
        "/sessions/plan-ok/resolve-plan",
        json!({"planId": plan_id, "approved": true}),
    );
    assert_eq!(s, 200, "{b}");
    assert_eq!(b, json!({"resolved": true, "approved": true}));
    assert_eq!(env.wait_turns("plan-ok", 1), "completed");

    let r = final_tool_results(&env, "PLAN-FLOW");
    assert_eq!(r.len(), 3, "{r:?}");
    assert!(
        r[0].to_lowercase().contains("plan") || r[0].to_lowercase().contains("denied"),
        "the first write must be refused in plan mode, got: {}",
        r[0]
    );
    assert_eq!(
        r[1],
        "Plan approved — leaving Plan mode. You may now make changes."
    );
    assert!(!r[2].to_lowercase().contains("denied"), "{}", r[2]);
    assert_eq!(
        std::fs::read_to_string(root.join("plan-gate.txt")).expect("write after approval"),
        "second-attempt"
    );
    // A resolved plan cannot be resolved twice.
    assert_eq!(
        env.post(
            "/sessions/plan-ok/resolve-plan",
            json!({"planId": plan_id, "approved": true})
        )
        .0,
        404
    );
}

#[test]
fn exit_plan_mode_rejection_keeps_the_session_in_plan_mode() {
    let env = Env::start(plan_flow_provider(), &[], |_, _| {});
    let (plan_id, _) = present_plan(&env, "plan-no");
    let (s, b) = env.post(
        "/sessions/plan-no/resolve-plan",
        json!({"planId": plan_id, "approved": false}),
    );
    assert_eq!(s, 200, "{b}");
    assert_eq!(b, json!({"resolved": true, "approved": false}));
    assert_eq!(env.wait_turns("plan-no", 1), "completed");

    let r = final_tool_results(&env, "PLAN-FLOW");
    assert_eq!(r.len(), 3, "{r:?}");
    assert_eq!(
        r[1],
        "Plan rejected — staying in Plan mode. Revise and re-present."
    );
    let root = session_workspace(&env, "plan-no");
    assert!(
        !root.join("plan-gate.txt").exists(),
        "a rejected plan must leave writes blocked"
    );
}

// ─────────── wait_for_subagents / followup_subagent / interrupt_subagent ───

#[test]
fn subagent_supervision_tools_wait_follow_up_and_interrupt_a_real_child() {
    let child_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let hold_seen = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let (cid, seen, rel) = (child_id.clone(), hold_seen.clone(), release.clone());

    let provider = Provider::start(move |body, _| {
        let first = first_user_text(body);
        let last = last_user_text(body);
        if first.contains("CHILD-ONE") {
            if last.contains("HOLD-ME") {
                seen.store(true, Ordering::SeqCst);
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
                while !rel.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                return Reply::text("held-child-done");
            }
            if last.contains("FOLLOWUP-INSTRUCTION") {
                return Reply::text("child-followup-ack");
            }
            return Reply::text("child-one-done");
        }
        let id = || -> String {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            loop {
                if let Some(id) = cid.lock().unwrap().clone() {
                    return id;
                }
                assert!(std::time::Instant::now() < deadline, "child id never found");
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        };
        match tool_results(body).len() {
            0 => Reply::tool(
                "s0",
                "spawn_subagent",
                json!({"role":"planner","task":"CHILD-ONE: do the thing"}),
            ),
            1 => Reply::tool(
                "s1",
                "wait_for_subagents",
                json!({"agentPaths":[id()],"timeoutMs":30000}),
            ),
            2 => Reply::tool(
                "s2",
                "followup_subagent",
                json!({"agentPath": id(), "message":"FOLLOWUP-INSTRUCTION: refine it"}),
            ),
            3 => Reply::tool(
                "s3",
                "spawn_subagent",
                json!({"role":"planner","task":"HOLD-ME: keep working",
                       "continuity":{"kind":"continue_agent","continueAgentId": id()}}),
            ),
            4 => {
                // Interrupt only once the child is genuinely mid-turn.
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
                while !seen.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Reply::tool(
                    "s4",
                    "interrupt_subagent",
                    json!({"agentPath": id(), "reason":"operator asked to stop"}),
                )
            }
            5 => Reply::tool(
                "s5",
                "interrupt_subagent",
                json!({"agentPath":"not-a-child-of-this-session"}),
            ),
            6 => Reply::tool(
                "s6",
                "wait_for_subagents",
                json!({"agentPaths":["not-a-child-of-this-session"],"timeoutMs":2000}),
            ),
            7 => Reply::tool(
                "s7",
                "followup_subagent",
                json!({"agentPath":"  ","message":"x"}),
            ),
            _ => {
                rel.store(true, Ordering::SeqCst);
                Reply::text("supervision finished")
            }
        }
    });
    let env = Env::start(provider, &[], |_, _| {});
    // Assert the live admission budget before creating this lineage. A
    // metered parent holds its entire balance while its provider waits for
    // HOLD-ME, so a child cannot start under that reservation.
    let (status, saved) = env.send(
        "PUT",
        "/settings/orchestration",
        json!({"budget":{"perSessionMaxUsd":0.0,"perAgentCallMaxUsd":0.0}}),
    );
    assert_eq!(status, 200, "{saved}");
    let (status, settings) = env.get("/settings/orchestration");
    assert_eq!(status, 200, "{settings}");
    assert_eq!(settings["budget"]["perSessionMaxUsd"], json!(0.0));
    assert_eq!(settings["budget"]["perAgentCallMaxUsd"], json!(0.0));
    env.create_session("sup1");
    env.say("sup1", "SUPERVISE: run the child");
    let kid = env.eventually("child session", 60, || {
        children_of(&env, "sup1")
            .first()
            .and_then(|c| c["id"].as_str().map(str::to_string))
    });
    *child_id.lock().unwrap() = Some(kid.clone());
    assert_eq!(env.wait_turns("sup1", 1), "completed");
    release.store(true, Ordering::SeqCst);

    let r = final_tool_results(&env, "SUPERVISE");
    assert_eq!(r.len(), 8, "{r:?}");
    assert_eq!(r[0], "child-one-done");

    let waited: Value = serde_json::from_str(&r[1]).expect("wait returns JSON");
    assert_eq!(waited["status"], "completed", "{waited}");
    assert_eq!(waited["outcomes"][0]["agent_path"], kid.as_str());
    assert_eq!(waited["outcomes"][0]["status"], "completed");

    assert_eq!(r[2], format!("Follow-up delivered: delivered to {kid}"));
    assert_eq!(r[3], format!("continued sub-agent {kid}"));
    assert_eq!(r[4], "subagent subtree interrupted");
    assert!(
        r[5].contains("interrupt_subagent: supervisor interrupt failed"),
        "{}",
        r[5]
    );
    assert!(
        r[6].contains("wait_for_subagents: supervisor wait failed"),
        "{}",
        r[6]
    );
    assert!(r[7].contains("followup_subagent:"), "{}", r[7]);

    // The child really received the steering message, framed as a follow-up.
    let child_saw_followup = env.provider.requests().iter().any(|req| {
        first_user_text(req).contains("CHILD-ONE")
            && last_user_text(req).contains("[Message from sup1 via followup_task]")
            && last_user_text(req).contains("FOLLOWUP-INSTRUCTION: refine it")
    });
    assert!(child_saw_followup, "the child never saw the follow-up");
    // And the continued child really started its second task.
    assert!(hold_seen.load(Ordering::SeqCst));
}

// ───────────────────────────── schedule_cron fires ────────────────────────

#[test]
fn schedule_cron_tool_creates_a_schedule_the_daemon_can_fire() {
    let provider = Provider::start(|body, _| {
        if first_user_text(body).contains("TOOL-SCHEDULED-PROMPT") {
            return Reply::text("scheduled-run-answer");
        }
        match tool_results(body).len() {
            0 => Reply::tool(
                "c0",
                "schedule_cron",
                json!({"operation":"register","cron_expr":"0 3 1 1 *",
                       "prompt":"TOOL-SCHEDULED-PROMPT: nightly sweep","time_zone":"UTC"}),
            ),
            _ => Reply::text("registered"),
        }
    });
    let env = Env::start(provider, &[], |_, _| {});
    assert_eq!(
        env.turn("sched-author", "SCHEDULE-AUTHOR: register the sweep"),
        "completed"
    );
    let r = final_tool_results(&env, "SCHEDULE-AUTHOR");
    assert_eq!(r.len(), 1, "{r:?}");
    let id = r[0].split('\'').nth(1).expect("schedule id").to_string();
    assert!(id.starts_with("schedule_"), "{}", r[0]);

    // The schedule the tool wrote is visible through the daemon's schedule API...
    let root = session_workspace(&env, "sched-author");
    let q = format!(
        "?workspaceRoot={}",
        slash_path(&root).replace(':', "%3A").replace('/', "%2F")
    );
    let (s, list) = env.get(&format!("/schedules{q}"));
    assert_eq!(s, 200, "{list}");
    let row = list["schedules"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == id.as_str()))
        .unwrap_or_else(|| panic!("tool-created schedule {id} not listed: {list}"))
        .clone();
    assert_eq!(row["paused"], false, "{row}");

    // ...and it fires: running it submits its prompt to the model.
    let before = env.provider.count();
    let (s, b) = env.post(&format!("/schedules/{id}/run{q}"), json!({}));
    assert_eq!(s, 200, "run now: {b}");
    assert_eq!(b["submitted"], true, "{b}");
    env.eventually("scheduled prompt reaches the model", 60, || {
        env.provider
            .requests()
            .iter()
            .skip(before)
            .any(|req| whole_prompt(req).contains("TOOL-SCHEDULED-PROMPT: nightly sweep"))
            .then_some(())
    });
    let runs = env.eventually("terminal schedule run", 60, || {
        let (_, r) = env.get(&format!("/schedules/{id}/runs{q}"));
        let list = r["runs"].as_array().cloned().unwrap_or_default();
        list.iter()
            .any(|e| e["status"] != "submitted")
            .then_some(list)
    });
    assert!(!runs.is_empty());
}
