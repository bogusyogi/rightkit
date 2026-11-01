//! Session-family HTTP contract of a REAL `coderight serve` daemon: create,
//! list/filter/patch, search, fork, delete tree, interrupt/kill, rewind,
//! resume across a hard restart, durable event history paging, transcript
//! derivatives, idempotent message receipts and the machine error envelope.
//!
//! Route families come from `ops004_route_coverage.toml` (`/sessions*`). The
//! assertions mirror `api_sessions/{lifecycle,messages,sse,router}.rs` and
//! `session_row`; nothing here reaches into the process.
//!
//! Retires the `api_tests_part_*`, `api_session_*` and `api_sessions/*` unit
//! tests that exercised these handlers with in-memory routers.

use crate::support::daemon_process;
use crate::support::e2e_api;
use crate::support::scripted_provider;

use e2e_api::*;
use scripted_provider::Step;
use serde_json::{json, Value};
use std::time::Duration;

fn find<'a>(rows: &'a Value, id: &str) -> Option<&'a Value> {
    rows.as_array()?.iter().find(|r| r["id"] == id)
}

fn assert_error_envelope(body: &Value, code: &str, retryable: bool, effect_state: &str) {
    assert_eq!(body["schemaVersion"], "coderight.error.v1", "{body}");
    assert_eq!(body["code"], code, "{body}");
    assert_eq!(body["retryable"], retryable, "{body}");
    assert_eq!(body["effectState"], effect_state, "{body}");
    assert!(
        body["correlationId"]
            .as_str()
            .is_some_and(|c| c.len() >= 32),
        "every machine error carries a correlation id: {body}"
    );
}

#[test]
fn create_session_returns_the_full_row_and_rejects_duplicates_and_invalid_options() {
    let rig = Rig::start(vec![]);
    let row = rig.session("crt1", json!({"title": "Created via API"}));
    assert_eq!(row["id"], "crt1");
    assert_eq!(row["title"], "Created via API");
    assert_eq!(row["model"], MODEL);
    assert_eq!(row["status"], "running");
    assert_eq!(row["turnActive"], false);
    assert_eq!(row["driverKind"], "native");
    assert_eq!(row["mcpChannel"], "desktop");
    assert_eq!(row["safeMode"], false);
    assert_eq!(row["tags"], json!([]));
    assert!(row["parentSessionId"].is_null() && row["forkedFromSessionId"].is_null());
    assert!(row["createdAt"].as_str().is_some_and(|t| t.len() > 10));

    // Persisted too: the list route sees it.
    let (_, rows) = rig.get("/sessions");
    assert_eq!(find(&rows, "crt1").unwrap()["title"], "Created via API");

    let (status, body) = rig.post(
        "/sessions",
        json!({"id": "crt1", "workspaceRoot": rig.ws().to_string_lossy(), "model": MODEL}),
    );
    assert_eq!(status, 409, "{body}");
    assert_error_envelope(&body, "conflict", false, "none");

    let (status, body) = rig.post("/sessions", json!({"id": "crt-bad-mode", "workspaceRoot": rig.ws().to_string_lossy(), "model": MODEL, "permissionMode": "yolo-ish"}));
    assert_eq!(
        status, 400,
        "a misspelled mode is refused, not downgraded: {body}"
    );
    assert_error_envelope(&body, "invalid_request", false, "none");
    assert!(
        find(&rig.get("/sessions").1, "crt-bad-mode").is_none(),
        "a refused create leaves nothing behind"
    );

    let safe = rig.session("crt-safe", json!({"safeMode": true}));
    assert_eq!(safe["safeMode"], true);
    assert_eq!(
        safe["toolset"], "read-only",
        "safe mode forces the read-only toolset"
    );
}

#[test]
fn list_filters_and_patch_update_the_merged_session_rows() {
    let rig = Rig::start(vec![]);
    rig.session("ls-old", json!({}));
    std::thread::sleep(Duration::from_millis(1100));
    rig.session("ls-new", json!({}));
    rig.session(
        "ls-child",
        json!({"parentSessionId": "ls-old", "role": "coder"}),
    );

    let (status, rows) = rig.get("/sessions");
    assert_eq!(status, 200);
    let ids: Vec<&str> = rows
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["id"].as_str())
        .collect();
    let (old_at, new_at) = (
        ids.iter().position(|i| *i == "ls-old").unwrap(),
        ids.iter().position(|i| *i == "ls-new").unwrap(),
    );
    assert!(new_at < old_at, "most recently created first: {ids:?}");
    assert!(
        !ids.contains(&"ls-child"),
        "child worker sessions stay out of Recents: {ids:?}"
    );

    let (_, with_children) = rig.get("/sessions?includeSubagents=true");
    assert_eq!(
        find(&with_children, "ls-child").unwrap()["parentSessionId"],
        "ls-old"
    );

    // Tag + status filters operate on the merged persisted/live rows.
    let (status, body) = rig.patch(
        "/sessions/ls-new",
        json!({"tags": ["keep", "bug"], "title": "Tagged"}),
    );
    assert_eq!(status, 200, "{body}");
    let (_, tagged) = rig.get("/sessions?tag=bug");
    assert_eq!(tagged.as_array().unwrap().len(), 1, "{tagged}");
    assert_eq!(tagged[0]["id"], "ls-new");
    assert_eq!(tagged[0]["title"], "Tagged");
    let (_, none) = rig.get("/sessions?tag=absent");
    assert_eq!(none, json!([]));
    let (_, running) = rig.get("/sessions?status=running");
    assert!(find(&running, "ls-old").is_some() && find(&running, "ls-new").is_some());
    let (_, done) = rig.get("/sessions?status=done");
    assert_eq!(done, json!([]));

    // PATCH reports exactly what changed and refuses bad input with the machine envelope.
    rig.session("pt1", json!({}));

    let (status, body) = rig.patch(
        "/sessions/pt1",
        json!({"title": "Patched", "driverKind": "native"}),
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["updated"], true);
    assert_eq!(body["id"], "pt1");
    assert_eq!(body["modelUpdated"], false);
    assert_eq!(body["driverKindUpdated"], true);
    assert_eq!(body["driverKind"], "native");
    assert_eq!(body["inferenceSelectionUpdated"], false);
    assert_eq!(
        find(&rig.get("/sessions").1, "pt1").unwrap()["title"],
        "Patched"
    );

    // A live engine owns runtime status ("running"), so a lifecycle status that removes the engine
    // (archived) is what the status filter can observe.
    let (status, body) = rig.patch("/sessions/pt1", json!({"status": "archived"}));
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        find(&rig.get("/sessions?status=archived").1, "pt1").unwrap()["id"],
        "pt1"
    );

    let (status, body) = rig.patch("/sessions/pt1", json!({"status": "bogus"}));
    assert_eq!(status, 400, "{body}");
    assert_error_envelope(&body, "invalid_request", false, "none");

    let (status, body) = rig.patch("/sessions/pt-missing", json!({"title": "x"}));
    assert_eq!(status, 404, "{body}");
    assert_error_envelope(&body, "resource_not_found", false, "none");
}

#[test]
fn messages_return_a_machine_receipt_and_the_nonce_makes_retries_idempotent() {
    let rig = Rig::start(vec![Step::text("RECEIPT-ANSWER-1")]);
    rig.session("rc1", json!({"permissionMode": "bypass"}));

    let (status, first) = rig.post(
        "/sessions/rc1/messages",
        json!({"text": "NONCE-TEXT-1", "nonce": "retry-key-1"}),
    );
    assert_eq!(status, 200, "{first}");
    assert_eq!(first["schemaVersion"], "coderight.machine.v1");
    assert_eq!(first["sessionId"], "rc1");
    assert_eq!(first["requestId"], "retry-key-1");
    assert_eq!(first["effectState"], "committed");
    assert_eq!(first["accepted"], true);
    assert!(first["seq"].as_i64().unwrap() > 0);
    rig.wait_assistant("rc1", "RECEIPT-ANSWER-1");

    // Same key + same body replays the stored receipt verbatim and does no new work.
    let (status, replay) = rig.post(
        "/sessions/rc1/messages",
        json!({"text": "NONCE-TEXT-1", "nonce": "retry-key-1"}),
    );
    assert_eq!(status, 200, "{replay}");
    assert_eq!(
        replay, first,
        "a replay returns the original receipt, correlation id included"
    );
    let submitted = rig
        .history("rc1")
        .iter()
        .filter(|e| kind_of(e) == "userMessageSubmitted")
        .count();
    assert_eq!(submitted, 1, "the retry must not create a second user turn");
    assert_eq!(
        rig.provider.request_count(),
        1,
        "the retry must not reach the provider"
    );

    // Same key with a different body is a hard conflict.
    let (status, conflict) = rig.post(
        "/sessions/rc1/messages",
        json!({"text": "DIFFERENT TEXT", "nonce": "retry-key-1"}),
    );
    assert_eq!(status, 409, "{conflict}");
    assert_error_envelope(&conflict, "idempotency_key_conflict", false, "none");

    let long = "k".repeat(257);
    let (status, bad) = rig.post(
        "/sessions/rc1/messages",
        json!({"text": "x", "nonce": long}),
    );
    assert_eq!(status, 400, "{bad}");
    assert_error_envelope(&bad, "idempotency_key_invalid", false, "none");
}

#[test]
fn event_history_pages_forward_backward_and_by_kind_and_unknown_sessions_are_404() {
    let rig = Rig::start(vec![Step::text("HISTORY-ANSWER-1")]);
    rig.session("hs1", json!({"permissionMode": "bypass"}));
    assert_eq!(rig.send("hs1", "HISTORY-QUESTION-1").0, 200);
    rig.wait_assistant("hs1", "HISTORY-ANSWER-1");

    let all = rig.history("hs1");
    let seqs: Vec<i64> = all.iter().filter_map(|e| e["seq"].as_i64()).collect();
    assert_eq!(seqs.len(), all.len(), "every durable event carries a seq");
    assert!(
        seqs.windows(2).all(|w| w[0] < w[1]),
        "strictly increasing: {seqs:?}"
    );
    let kinds: Vec<String> = all.iter().map(kind_of).collect();
    let user_at = kinds
        .iter()
        .position(|k| k == "userMessageSubmitted")
        .expect("user turn is durable");
    let done_at = kinds
        .iter()
        .position(|k| k == "assistantMessageCompleted")
        .expect("answer is durable");
    assert!(user_at < done_at, "{kinds:?}");

    // Forward cursor: only newer events.
    let cursor = seqs[user_at];
    let (status, newer) = rig.get(&format!("/sessions/hs1/events/history?after={cursor}"));
    assert_eq!(status, 200);
    let newer = newer.as_array().unwrap();
    assert!(
        !newer.is_empty() && newer.iter().all(|e| e["seq"].as_i64().unwrap() > cursor),
        "{newer:?}"
    );
    assert_eq!(newer.len(), seqs.iter().filter(|s| **s > cursor).count());

    // Bounded forward page.
    let (_, page) = rig.get("/sessions/hs1/events/history?after=0&limit=1");
    assert_eq!(page.as_array().unwrap().len(), 1);
    assert_eq!(page[0]["seq"], seqs[0]);

    // Backward page: the events immediately before a seq, oldest first.
    let last = *seqs.last().unwrap();
    let (_, back) = rig.get(&format!(
        "/sessions/hs1/events/history?before={last}&limit=2"
    ));
    let back_seqs: Vec<i64> = back
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["seq"].as_i64().unwrap())
        .collect();
    assert_eq!(back_seqs.len(), 2.min(seqs.len() - 1), "{back_seqs:?}");
    assert!(
        back_seqs.iter().all(|s| *s < last) && back_seqs.windows(2).all(|w| w[0] < w[1]),
        "{back_seqs:?}"
    );
    // Paging back past the start is an empty page, not a missing session.
    let (status, start) = rig.get(&format!(
        "/sessions/hs1/events/history?before={}&limit=5",
        seqs[0]
    ));
    assert_eq!((status, start), (200, json!([])));

    let (_, only) = rig.get("/sessions/hs1/events/history?kind=assistantMessageCompleted");
    assert!(
        only.as_array()
            .unwrap()
            .iter()
            .all(|e| kind_of(e) == "assistantMessageCompleted"),
        "{only}"
    );
    assert!(!only.as_array().unwrap().is_empty());

    let (status, missing) = rig.get("/sessions/never-existed/events/history");
    assert_eq!(status, 404);
    assert_eq!(missing["error"], "session_not_found");
    assert_eq!(missing["sessionId"], "never-existed");
}

#[test]
fn transcript_away_summary_untrusted_content_and_search_describe_the_same_turn() {
    let rig = Rig::start(vec![Step::text("SUMMARY-ANSWER-9")]);
    rig.session(
        "tr1",
        json!({"title": "Transcript probe", "permissionMode": "bypass"}),
    );
    assert_eq!(rig.send("tr1", "SEARCHABLE-MARKER-9 please").0, 200);
    rig.wait_assistant("tr1", "SUMMARY-ANSWER-9");

    let (status, transcript) = rig.get("/sessions/tr1/transcript");
    assert_eq!(status, 200);
    let rows = transcript.as_array().unwrap();
    let user = rows
        .iter()
        .find(|m| m["$kind"] == "user")
        .expect("user message");
    assert!(user["content"].to_string().contains("SEARCHABLE-MARKER-9"));
    assert!(rows.iter().any(
        |m| m["$kind"] == "assistant" && m["content"].to_string().contains("SUMMARY-ANSWER-9")
    ));

    let (status, summary) = rig.get("/sessions/tr1/away-summary");
    assert_eq!(status, 200);
    assert_eq!(summary["sessionId"], "tr1");
    assert_eq!(summary["sinceIndex"], 0);
    assert_eq!(summary["nextIndex"], rows.len());
    assert_eq!(summary["userMessages"], 1);
    assert!(summary["assistantMessages"].as_u64().unwrap() >= 1);
    assert_eq!(summary["hasActivity"], true);
    assert!(!summary["bullets"].as_array().unwrap().is_empty());
    let (_, beyond) = rig.get("/sessions/tr1/away-summary?sinceIndex=9999&limit=1");
    assert_eq!(
        beyond["sinceIndex"],
        rows.len(),
        "the cursor clamps to the transcript length"
    );
    assert_eq!(beyond["messageCount"], 0);
    assert_eq!(beyond["hasActivity"], false);
    let (_, limited) = rig.get("/sessions/tr1/away-summary?limit=1");
    assert_eq!(
        limited["bullets"].as_array().unwrap().len(),
        1,
        "limit caps the bullets"
    );

    let (status, untrusted) = rig.get("/sessions/tr1/untrusted-content");
    assert_eq!(status, 200);
    assert_eq!(untrusted["sessionId"], "tr1");
    assert_eq!(
        untrusted["untrustedCount"].as_u64().unwrap() as usize,
        untrusted["items"].as_array().unwrap().len()
    );

    let hit = rig.wait("session search to index the turn", 20, || {
        let (_, hits) = rig.get("/sessions/search?q=SEARCHABLE-MARKER-9&limit=5");
        hits.as_array()?
            .iter()
            .find(|h| h["sessionId"] == "tr1")
            .cloned()
    });
    assert_eq!(hit["title"], "Transcript probe");
    assert!(hit["snippet"].as_str().is_some_and(|s| !s.is_empty()));

    assert_eq!(rig.get("/sessions/no-such-transcript/transcript").0, 404);
    assert_eq!(rig.get("/sessions/no-such-transcript/away-summary").0, 404);
}

#[test]
fn fork_creates_a_linked_live_session_and_rejects_duplicate_and_unknown_parents() {
    let rig = Rig::start(vec![]);
    rig.session("fk-parent", json!({"title": "Origin"}));

    let (status, forked) = rig.post(
        "/sessions/fk-parent/fork",
        json!({"id": "fk-child", "title": "  Branch A  "}),
    );
    assert_eq!(status, 200, "{forked}");
    assert_eq!(forked["id"], "fk-child");
    assert_eq!(forked["parentSessionId"], "fk-parent");
    assert_eq!(forked["title"], "Branch A");
    assert_eq!(forked["session"]["forkedFromSessionId"], "fk-parent");
    assert_eq!(
        forked["session"]["model"], MODEL,
        "the fork inherits the parent's route"
    );

    let (status, default_title) = rig.post("/sessions/fk-parent/fork", json!({}));
    assert_eq!(status, 200, "{default_title}");
    assert_eq!(default_title["title"], "Fork of Origin");
    assert!(
        default_title["id"].as_str().unwrap().starts_with("s-"),
        "server mints an id when none is given"
    );

    let (status, dup) = rig.post("/sessions/fk-parent/fork", json!({"id": "fk-child"}));
    assert_eq!(status, 409, "{dup}");
    assert_error_envelope(&dup, "conflict", false, "none");
    let (status, missing) = rig.post("/sessions/ghost-parent/fork", json!({}));
    assert_eq!(status, 404, "{missing}");
    assert_error_envelope(&missing, "resource_not_found", false, "none");
}

#[test]
fn delete_removes_the_whole_session_tree_and_is_not_repeatable() {
    let rig = Rig::start(vec![]);
    rig.session("del-root", json!({}));
    rig.session(
        "del-child",
        json!({"parentSessionId": "del-root", "role": "coder"}),
    );
    rig.session("del-other", json!({}));

    let (status, body) = rig.delete("/sessions/del-root");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["deleted"], true);
    assert_eq!(body["id"], "del-root");
    let mut deleted: Vec<String> = body["deletedSessionIds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    deleted.sort();
    assert_eq!(
        deleted,
        vec!["del-child".to_string(), "del-root".to_string()]
    );

    let (_, rows) = rig.get("/sessions?includeSubagents=true");
    assert!(
        find(&rows, "del-root").is_none() && find(&rows, "del-child").is_none(),
        "{rows}"
    );
    assert!(
        find(&rows, "del-other").is_some(),
        "unrelated sessions survive"
    );
    assert_eq!(rig.get("/sessions/del-root/transcript").0, 404);
    assert_eq!(rig.get("/sessions/del-root/events/history").0, 404);

    let (status, again) = rig.delete("/sessions/del-root");
    assert_eq!(status, 404, "{again}");
    assert_error_envelope(&again, "resource_not_found", false, "none");
}

#[test]
fn interrupt_kill_and_rewind_act_on_live_sessions_and_404_otherwise() {
    let rig = Rig::start(vec![Step::text("REWIND-ANSWER-1")]);
    rig.session("ctl1", json!({"permissionMode": "bypass"}));

    assert_eq!(rig.send("ctl1", "REWIND-ME").0, 200);
    rig.wait_assistant("ctl1", "REWIND-ANSWER-1");
    let (status, body) = rig.post("/sessions/ctl1/interrupt", json!({}));
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["interrupted"], true);
    assert_eq!(rig.post("/sessions/ctl-ghost/interrupt", json!({})).0, 404);

    // D3 selects a prior prompt, then rewinds its conversation context.
    let (points_status, points) = rig.get("/sessions/ctl1/rewind/points");
    assert_eq!(points_status, 200, "{points}");
    let point = points.as_array().unwrap().last().unwrap().clone();
    let (status, rewound) = rig.post(
        "/sessions/ctl1/rewind",
        json!({"point": point, "mode": "conversation"}),
    );
    assert_eq!(status, 200, "{rewound}");
    assert_eq!(rewound["rewound"], true);
    assert!(
        rewound["text"].as_str().unwrap().starts_with("REWIND-ME"),
        "{rewound}"
    );
    let (_, transcript) = rig.get("/sessions/ctl1/transcript");
    assert!(
        !transcript.to_string().contains("REWIND-ME"),
        "the rewound turn left the live transcript: {transcript}"
    );
    assert_eq!(rig.get("/sessions/ctl-ghost/rewind/points").0, 404);

    let (status, killed) = rig.post("/sessions/ctl1/kill", json!({}));
    assert_eq!(status, 200, "{killed}");
    assert_eq!(killed["killed"], true);
    let row = rig.wait("killed session to persist as dormant", 15, || {
        find(&rig.get("/sessions").1, "ctl1")
            .filter(|r| r["status"] == "dormant")
            .cloned()
    });
    assert_eq!(row["id"], "ctl1");
    assert_eq!(
        rig.post("/sessions/ctl1/kill", json!({})).0,
        404,
        "a killed session is no longer live"
    );
}

#[test]
fn a_hard_restart_leaves_sessions_dormant_and_resume_restores_history_for_the_next_turn() {
    let mut rig = Rig::start(vec![
        Step::text("BEFORE-RESTART-ANSWER"),
        Step::text("AFTER-RESTART-ANSWER"),
    ]);
    rig.session(
        "rs1",
        json!({"title": "Restart probe", "permissionMode": "bypass"}),
    );
    assert_eq!(rig.send("rs1", "BEFORE-RESTART-QUESTION").0, 200);
    rig.wait_assistant("rs1", "BEFORE-RESTART-ANSWER");
    let before = rig.history("rs1").len();

    rig.restart();

    let (_, rows) = rig.get("/sessions");
    let row = find(&rows, "rs1").expect("session survives a kill -9 of the daemon");
    assert!(
        row["status"].is_string(),
        "persisted rows carry a status: {row}"
    );
    assert_eq!(row["title"], "Restart probe");
    assert_eq!(row["hasHistory"], true);
    assert_eq!(
        rig.history("rs1").len(),
        before,
        "durable history is intact and not duplicated"
    );
    let (status, transcript) = rig.get("/sessions/rs1/transcript");
    assert_eq!(
        status, 200,
        "a dormant transcript is rehydrated from the durable log"
    );
    assert!(transcript.to_string().contains("BEFORE-RESTART-ANSWER"));

    let (status, resumed) = rig.post("/sessions/rs1/resume", json!({}));
    assert_eq!(status, 200, "{resumed}");
    assert_eq!(resumed, json!({"resumed": true, "id": "rs1"}));
    assert_eq!(
        find(&rig.get("/sessions").1, "rs1").unwrap()["status"],
        "running"
    );
    let (_, again) = rig.post("/sessions/rs1/resume", json!({}));
    assert_eq!(
        again["resumed"], true,
        "resume is idempotent for a live session"
    );
    assert_eq!(rig.post("/sessions/rs-ghost/resume", json!({})).0, 404);

    assert_eq!(rig.send("rs1", "AFTER-RESTART-QUESTION").0, 200);
    rig.wait_assistant("rs1", "AFTER-RESTART-ANSWER");
    let sent = rig.provider.requests().last().unwrap().to_string();
    assert!(
        sent.contains("BEFORE-RESTART-QUESTION") && sent.contains("BEFORE-RESTART-ANSWER"),
        "the resumed engine was seeded with the pre-restart conversation: {sent}"
    );
}

#[test]
fn auth_is_required_and_session_errors_use_the_versioned_machine_envelope() {
    let rig = Rig::start(vec![]);
    assert_eq!(rig.anon("GET", "/sessions"), 401, "no token");
    assert_eq!(rig.anon("POST", "/sessions"), 401, "no token on a mutation");
    let wrong = crate::support::daemon_process::send(
        &rightkit_http::Request::get(format!("{}/sessions", rig.url())).bearer("not-the-token"),
    )
    .unwrap();
    assert_eq!(wrong.status, 401, "wrong token");
    assert_eq!(
        rig.anon("GET", "/health"),
        200,
        "liveness probe needs no credential"
    );

    // A bare 404 from a session handler is normalised into the shared envelope.
    let (status, body) = rig.get("/sessions/missing-goal-session/goal");
    assert_eq!(status, 404);
    assert_error_envelope(&body, "resource_not_found", false, "none");
    let (status, body) = rig.post("/sessions/missing-goal-session/steer", json!({"text": "x"}));
    assert_eq!(status, 404, "{body}");
    assert_error_envelope(&body, "resource_not_found", false, "none");
}

#[test]
fn rewind_returns_only_what_the_user_typed() {
    let rig = Rig::start(vec![Step::text("ok")]);
    rig.session("rw2", json!({"permissionMode": "bypass"}));
    assert_eq!(rig.send("rw2", "REWIND-ME").0, 200);
    rig.wait_assistant("rw2", "ok");
    let (points_status, points) = rig.get("/sessions/rw2/rewind/points");
    assert_eq!(points_status, 200, "{points}");
    let point = points.as_array().unwrap().last().unwrap().clone();
    let (status, rewound) = rig.post(
        "/sessions/rw2/rewind",
        json!({"point": point, "mode": "conversation"}),
    );
    assert_eq!(status, 200, "{rewound}");
    assert_eq!(rewound["text"], "REWIND-ME");
}
