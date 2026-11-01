//! Journeys: subagent delegation, session messaging, team definitions and the
//! durable peer-team board/mailbox/mission-log (VG-007), all through a real
//! `coderight serve` daemon and a scripted provider.
//! Retires the orchestration `spawn`/`supervision`/`teams`/`registry` unit tests.

use crate::support::e2e_orch;
use e2e_orch::*;

use serde_json::{json, Value};

fn children_of(env: &Env, parent: &str) -> Vec<Value> {
    // /sessions intentionally hides child workers from the recents view
    // unless callers opt in; this journey asserts daemon child lineage.
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

#[test]
fn subagent_runs_to_completion_and_its_answer_reaches_the_parent_model() {
    let provider = Provider::start(|body, _| {
        let task = first_user_text(body);
        if task.contains("CHILD-TASK-ALPHA") {
            return Reply::text("child-final-ALPHA");
        }
        if tool_results(body).is_empty() {
            Reply::tool(
                "call-spawn-1",
                "spawn_subagent",
                json!({"role":"planner","task":"CHILD-TASK-ALPHA: plan the thing"}),
            )
        } else {
            Reply::text("parent finished")
        }
    });
    let env = Env::start(provider, &[], |_, _| {});
    assert_eq!(
        env.turn("parent", "PARENT: delegate the planning"),
        "completed"
    );

    // The parent model was shown the child's final answer as the tool result.
    let seen: Vec<String> = env
        .provider
        .requests()
        .iter()
        .flat_map(tool_results)
        .collect();
    assert!(
        seen.iter().any(|r| r.contains("child-final-ALPHA")),
        "parent never received the child result: {seen:?}"
    );
    // Child ran as its own session with parent lineage, and was given only its task.
    let kids = children_of(&env, "parent");
    assert_eq!(
        kids.len(),
        1,
        "exactly one child session expected: {kids:?}"
    );
    let child_id = kids[0]["id"].as_str().unwrap().to_string();
    assert_eq!(env.events(&child_id, "sessionTurnFinished").len(), 1);
    // child request + parent request before + parent request after
    assert_eq!(env.provider.count(), 3);
}

#[test]
fn two_sequential_spawns_get_distinct_child_sessions_and_isolated_tasks() {
    let provider = Provider::start(|body, _| {
        let task = first_user_text(body);
        if task.contains("CHILD-A") {
            return Reply::text("answer-A");
        }
        if task.contains("CHILD-B") {
            return Reply::text("answer-B");
        }
        match tool_results(body).len() {
            0 => Reply::tool(
                "c1",
                "spawn_subagent",
                json!({"role":"planner","task":"CHILD-A"}),
            ),
            1 => Reply::tool(
                "c2",
                "spawn_subagent",
                json!({"role":"planner","task":"CHILD-B"}),
            ),
            _ => Reply::text("both done"),
        }
    });
    let env = Env::start(provider, &[], |_, _| {});
    assert_eq!(env.turn("p2", "go"), "completed");
    let kids = children_of(&env, "p2");
    assert_eq!(kids.len(), 2, "{kids:?}");
    // Child B never saw child A's task or answer (context isolation).
    for r in env.provider.requests() {
        if first_user_text(&r).contains("CHILD-B") {
            let all = whole_prompt(&r);
            assert!(!all.contains("answer-A"), "child B leaked A: {all}");
        }
    }
}

#[test]
fn peer_team_board_mailbox_and_mission_log_are_durable_across_daemon_restart() {
    let provider = Provider::start(|_, _| Reply::text("ok"));
    let mut env = Env::start(provider, &[], |_, _| {});
    let t = "/peer-teams/alpha";

    // Board: post two, claim in order, claims are exclusive and status moves forward.
    let (s, a) = env.post(
        &format!("{t}/board"),
        json!({"title":"first","payload":{"n":1}}),
    );
    assert_eq!(s, 200, "{a}");
    let first_id = a["item"]["item_id"].as_str().unwrap().to_string();
    env.post(&format!("{t}/board"), json!({"title":"second"}));
    let (_, c1) = env.post(&format!("{t}/board/claim"), json!({"peerId":"peer-1"}));
    assert_eq!(c1["item"]["item_id"], first_id.as_str());
    let (_, c2) = env.post(&format!("{t}/board/claim"), json!({"peerId":"peer-2"}));
    assert_eq!(
        c2["item"]["title"], "second",
        "same item must not be claimed twice"
    );
    let (_, c3) = env.post(&format!("{t}/board/claim"), json!({"peerId":"peer-3"}));
    assert!(c3["item"].is_null(), "no open work left: {c3}");
    let (s, _) = env.post(
        &format!("{t}/board/{first_id}/status"),
        json!({"status":"done"}),
    );
    assert_eq!(s, 204);
    let (s, _) = env.post(
        &format!("{t}/board/{first_id}/status"),
        json!({"status":"bogus"}),
    );
    assert_eq!(s, 400);

    // Mailbox: direct + broadcast; ack hides it only for the acking peer.
    env.post(
        &format!("{t}/mailbox"),
        json!({"fromPeer":"peer-1","toPeer":"peer-2","body":{"hi":"direct"}}),
    );
    env.post(
        &format!("{t}/mailbox"),
        json!({"fromPeer":"peer-1","body":{"hi":"all"}}),
    );
    let (_, m2) = env.get(&format!("{t}/mailbox?peerId=peer-2"));
    assert_eq!(m2["messages"].as_array().unwrap().len(), 2, "{m2}");
    let (_, m3) = env.get(&format!("{t}/mailbox?peerId=peer-3"));
    assert_eq!(
        m3["messages"].as_array().unwrap().len(),
        1,
        "only broadcast: {m3}"
    );
    let direct = m2["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["body"]["hi"] == "direct")
        .unwrap()["message_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (s, _) = env.post(
        &format!("{t}/mailbox/{direct}/ack?peerId=peer-2"),
        json!({}),
    );
    assert_eq!(s, 204);
    let (_, m2) = env.get(&format!("{t}/mailbox?peerId=peer-2"));
    assert_eq!(m2["messages"].as_array().unwrap().len(), 1);

    // Mission log is ordered and cursor-readable.
    for kind in ["start", "progress", "done"] {
        env.post(
            &format!("{t}/mission-log"),
            json!({"peerId":"peer-1","kind":kind}),
        );
    }
    let (_, all) = env.get(&format!("{t}/mission-log?sinceSeq=0"));
    let entries = all["entries"].as_array().unwrap().clone();
    assert_eq!(entries.len(), 3);
    let cursor = entries[0]["seq"].as_i64().unwrap();
    let (_, tail) = env.get(&format!("{t}/mission-log?sinceSeq={cursor}"));
    assert_eq!(tail["entries"].as_array().unwrap().len(), 2);

    // Unsafe team id is rejected (no path traversal into the store root).
    let (s, _) = env.get("/peer-teams/..%2Fescape/board");
    assert!(s == 400 || s == 404, "unsafe team id accepted: {s}");

    // Restart the daemon on the same home: state is durable, not process memory.
    let home_path = env.home.path().to_path_buf();
    let peer_root = home_path.join("peer-teams");
    drop(std::mem::replace(
        &mut env.daemon,
        daemon_process::start_daemon(
            &home_path,
            &[("RIGHTKIT_PEER_TEAM_STORE_ROOT", peer_root.to_str().unwrap())],
        ),
    ));
    let (_, board) = env.get(&format!("{t}/board"));
    assert_eq!(board["items"].as_array().unwrap().len(), 2);
    let (_, m2) = env.get(&format!("{t}/mailbox?peerId=peer-2"));
    assert_eq!(
        m2["messages"].as_array().unwrap().len(),
        1,
        "ack survived restart"
    );
    let (_, log) = env.get(&format!("{t}/mission-log?sinceSeq=0"));
    assert_eq!(log["entries"].as_array().unwrap().len(), 3);
}
