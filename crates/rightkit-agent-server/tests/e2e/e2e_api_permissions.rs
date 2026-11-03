//! Permission family of a REAL `coderight serve` daemon: live session mode and
//! rules (and that they actually gate a tool call), approval-resolution error
//! contracts, persistent user/project rule store (list/add/delete/export/
//! import, durable across a hard restart) and workspace trust.
//!
//! Mirrors `api_permissions/{handlers,serialization}.rs` and
//! `api_sessions/approvals.rs`; routes picked from `ops004_route_coverage.toml`
//! (`/permission-rules*`, `/sessions/{id}/permission-*`, `/workspace/trust`,
//! `/permission-denials`, `/sessions/{id}/tool-approval`).
//!
//! Retires the `api_permissions/tests.rs` and approval unit tests.

use crate::support::daemon_process;
use crate::support::e2e_api;
use crate::support::scripted_provider;

use e2e_api::*;
use scripted_provider::Step;
use serde_json::{json, Value};

fn rule_with<'a>(rules: &'a Value, pattern: &str) -> Option<&'a Value> {
    rules.as_array()?.iter().find(|r| r["pattern"] == pattern)
}

fn session_file(session: &Value, rel: &str) -> Option<String> {
    let root = session["workspaceRoot"].as_str()?;
    std::fs::read_to_string(std::path::Path::new(root).join(rel)).ok()
}

#[test]
fn session_permission_mode_and_rules_have_a_stable_wire_contract() {
    let rig = Rig::start(vec![]);
    rig.session("pm1", json!({}));

    let (status, body) = rig.post("/sessions/pm1/permission-mode", json!({"mode": "plan"}));
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, json!({"mode": "Plan"}));
    assert_eq!(
        rig.post(
            "/sessions/pm1/permission-mode",
            json!({"mode": "accept-edits"})
        )
        .1,
        json!({"mode": "AcceptEdits"})
    );
    assert_eq!(
        rig.post(
            "/sessions/pm1/permission-mode",
            json!({"mode": "auto-safe"})
        )
        .1,
        json!({"mode": "AutoSafe"})
    );
    assert_eq!(
        rig.post("/sessions/pm1/permission-mode", json!({"mode": "bypass"}))
            .1,
        json!({"mode": "Bypass"})
    );
    assert_eq!(
        rig.post("/sessions/pm-ghost/permission-mode", json!({"mode": "ask"}))
            .0,
        404
    );

    let (status, before) = rig.get("/sessions/pm1/permission-rules");
    assert_eq!(status, 200);
    let baseline = before["rules"].as_array().unwrap().len();
    let (status, added) = rig.post(
        "/sessions/pm1/permission-rules",
        json!({"pattern": "  shell(git push*)  ", "effect": "DENY"}),
    );
    assert_eq!(status, 200, "{added}");
    assert_eq!(
        added["rule"]["pattern"], "shell(git push*)",
        "patterns are trimmed"
    );
    assert_eq!(
        added["rule"]["effect"], "deny",
        "effects are case-insensitive and normalised"
    );
    assert_eq!(added["rule"]["source"], "session");
    let index = added["rule"]["index"].as_u64().unwrap();
    assert_eq!(index as usize, baseline, "the new rule is appended");

    let (_, listed) = rig.get("/sessions/pm1/permission-rules");
    assert_eq!(
        listed["rules"][index as usize]["pattern"],
        "shell(git push*)"
    );

    assert_eq!(
        rig.post(
            "/sessions/pm1/permission-rules",
            json!({"pattern": "   ", "effect": "allow"})
        )
        .0,
        400,
        "blank pattern"
    );
    assert_eq!(
        rig.post(
            "/sessions/pm1/permission-rules",
            json!({"pattern": "x", "effect": "sometimes"})
        )
        .0,
        400,
        "unknown effect"
    );
    assert_eq!(
        rig.post(
            "/sessions/pm-ghost/permission-rules",
            json!({"pattern": "x", "effect": "allow"})
        )
        .0,
        404
    );

    let (status, removed) = rig.delete(&format!("/sessions/pm1/permission-rules/{index}"));
    assert_eq!(status, 200, "{removed}");
    assert_eq!(removed["removed"], true);
    assert_eq!(removed["rule"]["pattern"], "shell(git push*)");
    assert!(rule_with(
        &rig.get("/sessions/pm1/permission-rules").1["rules"],
        "shell(git push*)"
    )
    .is_none());
    assert_eq!(rig.delete("/sessions/pm1/permission-rules/9999").0, 404);
    assert_eq!(rig.get("/sessions/pm-ghost/permission-rules").0, 404);
}

#[test]
fn a_session_deny_rule_blocks_the_tool_without_prompting_and_an_allow_rule_lets_it_run() {
    let rig = Rig::start(vec![
        Step::tool(
            "deny1",
            "write_file",
            json!({"path": "denied-by-rule.txt", "content": "no"}),
        ),
        Step::text("saw the refusal"),
        Step::tool(
            "allow1",
            "write_file",
            json!({"path": "allowed-by-rule.txt", "content": "yes"}),
        ),
        Step::text("wrote it"),
    ]);
    let session = rig.session("rl1", json!({"permissionMode": "ask"}));

    let (status, _) = rig.post(
        "/sessions/rl1/permission-rules",
        json!({"pattern": "write_file", "effect": "deny"}),
    );
    assert_eq!(status, 200);
    assert_eq!(rig.send("rl1", "write the denied file").0, 200);
    rig.wait_assistant("rl1", "saw the refusal");
    assert!(
        session_file(&session, "denied-by-rule.txt").is_none(),
        "a deny rule stops the effect"
    );
    assert!(
        !rig.kinds("rl1")
            .iter()
            .any(|k| k == "toolApprovalRequested"),
        "a rule decision never asks a human"
    );
    let tool = rig.provider.requests()[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .cloned()
        .expect("the refusal is replayed to the model");
    assert_eq!(tool["tool_call_id"], "deny1");

    // Replace the deny rule with an allow rule: the very next call runs unprompted.
    let (_, listed) = rig.get("/sessions/rl1/permission-rules");
    let deny_index = rule_with(&listed["rules"], "write_file").unwrap()["index"]
        .as_u64()
        .unwrap();
    assert_eq!(
        rig.delete(&format!("/sessions/rl1/permission-rules/{deny_index}"))
            .0,
        200
    );
    assert_eq!(
        rig.post(
            "/sessions/rl1/permission-rules",
            json!({"pattern": "write_file", "effect": "allow"})
        )
        .0,
        200
    );
    assert_eq!(rig.send("rl1", "write the allowed file").0, 200);
    rig.wait_assistant("rl1", "wrote it");
    assert_eq!(
        session_file(&session, "allowed-by-rule.txt").as_deref(),
        Some("yes")
    );
    assert!(!rig
        .kinds("rl1")
        .iter()
        .any(|k| k == "toolApprovalRequested"));
}

#[test]
fn approval_resolution_is_single_use_and_unknown_ids_are_not_found() {
    let rig = Rig::start(vec![
        Step::tool(
            "ap-w",
            "write_file",
            json!({"path": "approved.txt", "content": "A"}),
        ),
        Step::text("approved and done"),
    ]);
    let session = rig.session("apv1", json!({"permissionMode": "ask"}));
    assert_eq!(rig.send("apv1", "write approved.txt").0, 200);
    let requested = rig.wait("approval request", 60, || {
        rig.find_event("apv1", "toolApprovalRequested")
    });
    let call = if requested.get("call").is_some() {
        &requested["call"]
    } else {
        &requested["data"]["call"]
    };
    assert_eq!(call["id"], "ap-w");
    assert!(
        session_file(&session, "approved.txt").is_none(),
        "nothing lands before approval"
    );

    // A wrong id or session is 404 and leaves the real request pending.
    assert_eq!(
        rig.post(
            "/sessions/apv1/tool-approval",
            json!({"toolCallId": "someone-else", "approved": true})
        )
        .0,
        404
    );
    assert_eq!(
        rig.post(
            "/sessions/apv-ghost/tool-approval",
            json!({"toolCallId": "ap-w", "approved": true})
        )
        .0,
        404
    );
    assert_eq!(
        rig.post(
            "/sessions/apv1/tool-approval",
            json!({"toolCallId": "ap-w", "approved": true, "requestId": "stale-request"})
        )
        .0,
        404,
        "a mismatched request id is refused"
    );
    assert!(session_file(&session, "approved.txt").is_none());

    let (status, resolved) = rig.post(
        "/sessions/apv1/tool-approval",
        json!({"toolCallId": "ap-w", "approved": true}),
    );
    assert_eq!(status, 200, "{resolved}");
    assert_eq!(resolved, json!({"resolved": true, "approved": true}));
    rig.wait_assistant("apv1", "approved and done");
    assert_eq!(session_file(&session, "approved.txt").as_deref(), Some("A"));
    assert_eq!(
        rig.post(
            "/sessions/apv1/tool-approval",
            json!({"toolCallId": "ap-w", "approved": true})
        )
        .0,
        404,
        "an approval resolves exactly once"
    );

    // Questions and plans use the same pending-id discipline.
    assert_eq!(
        rig.post(
            "/sessions/apv1/answer-question",
            json!({"questionId": "q-none", "answers": {"x": "y"}})
        )
        .0,
        404
    );
    assert_eq!(
        rig.post(
            "/sessions/apv1/resolve-plan",
            json!({"planId": "p-none", "approved": true})
        )
        .0,
        404
    );
}

#[test]
fn approving_for_the_session_grants_the_identical_call_without_a_prompt() {
    let rig = Rig::start(vec![
        Step::tool(
            "fs-1",
            "write_file",
            json!({"path": "s.txt", "content": "S"}),
        ),
        Step::text("session scoped"),
        Step::tool(
            "fs-2",
            "write_file",
            json!({"path": "s.txt", "content": "S"}),
        ),
        Step::text("session scoped again"),
    ]);
    rig.session("sc1", json!({"permissionMode": "ask"}));
    assert_eq!(rig.send("sc1", "write s.txt").0, 200);
    rig.wait("approval request", 60, || {
        rig.find_event("sc1", "toolApprovalRequested")
    });
    let (status, body) = rig.post(
        "/sessions/sc1/tool-approval",
        json!({"toolCallId": "fs-1", "approved": true, "decision": "approved_for_session"}),
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["approved"], true);
    rig.wait_assistant("sc1", "session scoped");

    // Decision #181: a non-command call gets an exact-payload digest grant (no
    // pattern rule). The observable effect is that the identical call runs
    // without a second prompt.
    assert_eq!(rig.send("sc1", "write s.txt again").0, 200);
    rig.wait_assistant("sc1", "session scoped again");
    let prompts = rig
        .history("sc1")
        .iter()
        .filter(|e| kind_of(e) == "toolApprovalRequested")
        .count();
    assert_eq!(prompts, 1, "identical call must not prompt again");
}

#[test]
fn persistent_rules_cover_user_and_project_scopes_with_validation_and_survive_restart() {
    let mut rig = Rig::start(vec![]);
    let ws = rig.ws().to_string_lossy().to_string();

    let (status, added) = rig.post(
        "/permission-rules",
        json!({"scope": "user", "pattern": "shell(rm *)", "effect": "deny"}),
    );
    assert_eq!(status, 200, "{added}");
    assert_eq!(
        added["rule"],
        json!({"index": 0, "pattern": "shell(rm *)", "effect": "deny", "scope": "user", "workspaceRoot": null})
    );
    let (status, project) = rig.post("/permission-rules", json!({"scope": "project", "pattern": "write_file", "effect": "allow", "workspaceRoot": ws}));
    assert_eq!(status, 200, "{project}");
    assert_eq!(project["rule"]["scope"], "project");
    assert_eq!(project["rule"]["workspaceRoot"], ws.as_str());

    // Validation: project needs a workspace, scopes/effects are closed sets, blank patterns refused.
    assert_eq!(
        rig.post(
            "/permission-rules",
            json!({"scope": "project", "pattern": "x", "effect": "allow"})
        )
        .0,
        400
    );
    assert_eq!(
        rig.post(
            "/permission-rules",
            json!({"scope": "galaxy", "pattern": "x", "effect": "allow"})
        )
        .0,
        400
    );
    assert_eq!(
        rig.post(
            "/permission-rules",
            json!({"scope": "user", "pattern": "x", "effect": "perhaps"})
        )
        .0,
        400
    );
    assert_eq!(
        rig.post(
            "/permission-rules",
            json!({"scope": "user", "pattern": "  ", "effect": "allow"})
        )
        .0,
        400
    );

    let (_, all) = rig.get("/permission-rules");
    let store_path = all["permissionRuleStorePath"]
        .as_str()
        .expect("store path")
        .to_string();
    assert_eq!(all["user"][0]["pattern"], "shell(rm *)");
    assert_eq!(all["projects"][&ws][0]["pattern"], "write_file");
    assert!(
        std::fs::read_to_string(&store_path)
            .unwrap()
            .contains("shell(rm *)"),
        "rules are written through to disk"
    );

    let (_, user) = rig.get("/permission-rules?scope=user");
    assert_eq!(
        (
            user["scope"].clone(),
            user["rules"].as_array().unwrap().len()
        ),
        (json!("user"), 1)
    );
    let (_, proj) = rig.get(&format!(
        "/permission-rules?scope=project&workspaceRoot={}",
        url_encode(&ws)
    ));
    assert_eq!(proj["workspaceRoot"], ws.as_str());
    assert_eq!(proj["rules"][0]["effect"], "allow");
    assert_eq!(
        rig.get("/permission-rules?scope=project").0,
        400,
        "project listing needs a workspace"
    );
    assert_eq!(rig.get("/permission-rules?scope=nope").0, 400);

    // Export is the store itself; import replaces it wholesale and validates every rule first.
    let (status, exported) = rig.get("/permission-rules/export");
    assert_eq!(status, 200);
    assert_eq!(exported["user"][0]["pattern"], "shell(rm *)");
    assert_eq!(
        rig.post(
            "/permission-rules/import",
            json!({"user": [{"pattern": "", "effect": "allow"}], "projects": {}})
        )
        .0,
        400,
        "empty pattern"
    );
    assert_eq!(
        rig.post(
            "/permission-rules/import",
            json!({"user": [{"pattern": "x", "effect": "maybe"}], "projects": {}})
        )
        .0,
        400,
        "bad effect"
    );
    assert_eq!(
        rig.get("/permission-rules/export").1,
        exported,
        "a rejected import changes nothing"
    );
    let (status, imported) = rig.post(
        "/permission-rules/import",
        json!({"user": [{"pattern": "web_fetch", "effect": "ask"}, {"pattern": "shell(ls)", "effect": "allow"}], "projects": {}}),
    );
    assert_eq!(status, 200, "{imported}");
    assert_eq!(
        imported,
        json!({"imported": true, "userRules": 2, "projectScopes": 0})
    );

    // The store survives a hard restart of the daemon.
    rig.restart();
    let (_, after) = rig.get("/permission-rules?scope=user");
    let patterns: Vec<&str> = after["rules"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["pattern"].as_str())
        .collect();
    assert_eq!(patterns, vec!["web_fetch", "shell(ls)"]);

    // Delete by index within a scope.
    let (status, removed) = rig.delete("/permission-rules/0?scope=user");
    assert_eq!(status, 200, "{removed}");
    assert_eq!(removed["removed"], true);
    assert_eq!(removed["rule"]["pattern"], "web_fetch");
    assert_eq!(
        rig.get("/permission-rules?scope=user").1["rules"][0]["pattern"],
        "shell(ls)",
        "indices shift down"
    );
    assert_eq!(rig.delete("/permission-rules/5?scope=user").0, 404);
    assert_eq!(
        rig.delete("/permission-rules/0?scope=project").0,
        400,
        "project delete needs a workspace"
    );
    assert_eq!(rig.delete("/permission-rules/0?scope=bogus").0, 400);

    // The denial log has a stable envelope and honours the session/limit filters.
    let (status, body) = rig.get("/permission-denials");
    assert_eq!(status, 200, "{body}");
    assert!(body["permissionDenialStorePath"]
        .as_str()
        .is_some_and(|p| !p.is_empty()));
    assert_eq!(
        body["denials"],
        json!([]),
        "a fresh daemon has recorded no denials"
    );
    let (status, filtered) = rig.get("/permission-denials?sessionId=none&limit=3");
    assert_eq!(status, 200);
    assert_eq!(filtered["denials"], json!([]));
}

fn url_encode(raw: &str) -> String {
    raw.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

#[test]
fn workspace_trust_defaults_untrusted_updates_validates_and_persists() {
    let mut rig = Rig::start(vec![]);
    let ws = rig.ws().to_string_lossy().to_string();
    let query = format!("/workspace/trust?workspaceRoot={}", url_encode(&ws));

    let (status, initial) = rig.get(&query);
    assert_eq!(status, 200, "{initial}");
    assert_eq!(initial["trust"], "untrusted");
    assert_eq!(initial["trusted"], false);
    assert_eq!(initial["requiresTrust"], true, "no decision recorded yet");
    assert!(initial["approvedBy"].is_null());

    let (status, set) = rig.patch(
        "/workspace/trust",
        json!({"workspaceRoot": ws, "trust": "trusted", "approvedBy": "  qa-suite  "}),
    );
    assert_eq!(status, 200, "{set}");
    assert_eq!(set["trust"], "trusted");
    assert_eq!(set["trusted"], true);
    assert_eq!(set["requiresTrust"], false);
    assert_eq!(set["approvedBy"], "qa-suite");
    assert!(set["updatedAt"].is_string());

    assert_eq!(
        rig.patch(
            "/workspace/trust",
            json!({"workspaceRoot": ws, "trust": "perhaps"})
        )
        .0,
        400
    );
    assert_eq!(
        rig.patch(
            "/workspace/trust",
            json!({"workspaceRoot": "   ", "trust": "trusted"})
        )
        .0,
        400
    );
    assert_eq!(rig.get("/workspace/trust?workspaceRoot=").0, 400);

    rig.restart();
    let (_, after) = rig.get(&query);
    assert_eq!(
        after["trust"], "trusted",
        "the decision is durable across restart: {after}"
    );
    assert_eq!(after["approvedBy"], "qa-suite");

    let (_, revoked) = rig.patch(
        "/workspace/trust",
        json!({"workspaceRoot": ws, "trust": "untrusted"}),
    );
    assert_eq!(revoked["trusted"], false);
    assert_eq!(
        revoked["requiresTrust"], false,
        "an explicit untrusted decision is still a recorded decision"
    );
}
