//! Daemon-backed session verbs of the `coderight` CLI, run as real child
//! processes against a REAL `coderight serve` daemon: `ps`/`sessions`, `session
//! set|archive|restore|delete|list`, `fork`, `export`/`transcript`, `kill`,
//! `agents`, `permission *`. Assertions are the exact stdout confirmations,
//! stderr diagnostics and exit codes written by `main_runtime_daemon_sessions`,
//! `main_runtime_daemon_resolve`, `main_runtime_daemon_actions`,
//! `main_runtime_daemon_exports` and `main_runtime_cli_controls`.
//!
//! Retires the unit tests in `main_runtime_daemon_resolve_tests`,
//! `main_runtime_cli_management_tests` and the session/permission arms of
//! `cli_definitions_tests_*`.

use crate::support::daemon_process;
use crate::support::e2e_api;
use crate::support::scripted_provider;

use e2e_api::*;
use scripted_provider::Step;
use serde_json::{json, Value};

fn text(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn err(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn diag(out: &std::process::Output) -> String {
    format!(
        "code={:?}\nstdout={}\nstderr={}",
        out.status.code(),
        text(out),
        err(out)
    )
}

fn row<'a>(rows: &'a Value, id: &str) -> Option<&'a Value> {
    rows.as_array()?.iter().find(|r| r["id"] == id)
}

#[test]
fn ps_and_sessions_print_the_same_session_rows_with_live_metadata() {
    let rig = Rig::start(vec![]);
    rig.session("cps1", json!({"title": "Listing probe"}));

    let ps = rig.cli(&["ps"]);
    assert_eq!(ps.status.code(), Some(0), "{}", diag(&ps));
    let sessions = rig.cli(&["sessions", "--json"]);
    assert_eq!(sessions.status.code(), Some(0), "{}", diag(&sessions));

    let a = stdout_json(&ps);
    let b = stdout_json(&sessions);
    let r = row(&a, "cps1").expect("created session is listed");
    assert_eq!(r["title"], "Listing probe");
    assert_eq!(r["model"], MODEL);
    assert_eq!(
        r["status"], "running",
        "a live session reports runtime status"
    );
    assert!(r["parentSessionId"].is_null());
    assert!(r["workspaceRoot"].as_str().is_some_and(|w| !w.is_empty()));
    assert_eq!(
        row(&b, "cps1").map(|r| r["title"].clone()),
        Some(json!("Listing probe"))
    );
}

#[test]
fn session_set_archive_restore_delete_print_exact_confirmations_and_change_rows() {
    let rig = Rig::start(vec![]);
    rig.session("cl1", json!({}));

    let set = rig.cli(&[
        "session",
        "set",
        "cl1",
        "--title",
        "Renamed by CLI",
        "--tag",
        "alpha",
        "--tag",
        "beta",
    ]);
    assert_eq!(set.status.code(), Some(0), "{}", diag(&set));
    assert_eq!(text(&set).trim(), "Session cl1 updated.");

    let tagged = stdout_json(&rig.cli(&["session", "list", "--tag", "beta"]));
    let r = row(&tagged, "cl1").expect("tag filter finds the session");
    assert_eq!(r["title"], "Renamed by CLI");
    assert_eq!(r["tags"], json!(["alpha", "beta"]));
    let other = stdout_json(&rig.cli(&["session", "list", "--tag", "gamma"]));
    assert!(
        row(&other, "cl1").is_none(),
        "non-matching tag filters it out"
    );

    let archive = rig.cli(&["session", "archive", "cl1"]);
    assert_eq!(
        text(&archive).trim(),
        "Session cl1 status set to archived.",
        "{}",
        diag(&archive)
    );
    let archived = stdout_json(&rig.cli(&["session", "list", "--status", "archived"]));
    assert!(row(&archived, "cl1").is_some(), "{archived}");

    let restore = rig.cli(&["session", "restore", "cl1"]);
    assert_eq!(
        text(&restore).trim(),
        "Session cl1 status set to dormant.",
        "{}",
        diag(&restore)
    );
    let dormant = stdout_json(&rig.cli(&["session", "list", "--status", "dormant"]));
    assert!(row(&dormant, "cl1").is_some(), "{dormant}");

    let clear = rig.cli(&["session", "set", "cl1", "--clear-tags"]);
    assert_eq!(clear.status.code(), Some(0), "{}", diag(&clear));
    let cleared = stdout_json(&rig.cli(&["session", "list"]));
    assert_eq!(row(&cleared, "cl1").unwrap()["tags"], json!([]));

    // Empty or contradictory updates are refused client-side, before any request.

    let nothing = rig.cli(&["session", "set", "cl1"]);
    assert_eq!(nothing.status.code(), Some(1), "{}", diag(&nothing));
    assert!(
        err(&nothing).contains("Nothing to update"),
        "{}",
        diag(&nothing)
    );

    let both = rig.cli(&["session", "set", "cl1", "--tag", "x", "--clear-tags"]);
    assert_eq!(both.status.code(), Some(1), "{}", diag(&both));
    assert!(
        err(&both).contains("Use either --tag or --clear-tags"),
        "{}",
        diag(&both)
    );

    let missing = rig.cli(&["session", "set", "no-such", "--title", "T"]);
    assert_eq!(missing.status.code(), Some(1), "{}", diag(&missing));
    assert!(
        err(&missing).contains("Session not found: no-such"),
        "{}",
        diag(&missing)
    );

    let del = rig.cli(&["session", "delete", "cl1"]);
    assert_eq!(text(&del).trim(), "Session cl1 deleted.", "{}", diag(&del));
    let gone = rig.cli(&["session", "delete", "cl1"]);
    assert_eq!(gone.status.code(), Some(1), "{}", diag(&gone));
    assert!(
        err(&gone).contains("Session not found: cl1"),
        "{}",
        diag(&gone)
    );
    assert!(row(&stdout_json(&rig.cli(&["session", "list"])), "cl1").is_none());
}

#[test]
fn fork_prints_the_new_session_with_lineage_and_honours_title_and_last() {
    let rig = Rig::start(vec![]);
    rig.session("cf-parent", json!({"title": "Parent chat"}));

    let fork = rig.cli(&["fork", "cf-parent"]);
    assert_eq!(fork.status.code(), Some(0), "{}", diag(&fork));
    let v = stdout_json(&fork);
    assert_eq!(v["parentSessionId"], "cf-parent");
    assert_eq!(v["title"], "Fork of Parent chat");
    assert_ne!(v["id"], "cf-parent");
    assert_eq!(v["session"]["forkedFromSessionId"], "cf-parent");
    assert_eq!(v["session"]["id"], v["id"]);

    let titled = stdout_json(&rig.cli(&["fork", "cf-parent", "--title", "  Spike branch  "]));
    assert_eq!(titled["title"], "Spike branch", "title is trimmed");

    // `--last` picks the most recently updated top-level session of this workspace.
    std::thread::sleep(std::time::Duration::from_millis(1300));
    rig.session("cf-newest", json!({"title": "Newest"}));
    let last = stdout_json(&rig.cli(&["fork", "--last"]));
    assert_eq!(last["parentSessionId"], "cf-newest", "{last}");

    let missing = rig.cli(&["fork", "no-such-session"]);
    assert_eq!(missing.status.code(), Some(1), "{}", diag(&missing));
    assert!(err(&missing).contains("404"), "{}", diag(&missing));

    let usage = rig.cli(&["fork"]);
    assert_eq!(
        usage.status.code(),
        Some(2),
        "a session id or --last is required: {}",
        diag(&usage)
    );
}

#[test]
fn export_includes_both_sides_of_a_real_turn_as_json_and_markdown() {
    let rig = Rig::start(vec![Step::text("EXPORT-ANSWER-31")]);
    rig.session("cx1", json!({"permissionMode": "bypass"}));
    assert_eq!(rig.send("cx1", "EXPORT-QUESTION-31").0, 200);
    rig.wait_assistant("cx1", "EXPORT-ANSWER-31");

    let json_out = rig.cli(&["export", "cx1", "--json"]);
    assert_eq!(json_out.status.code(), Some(0), "{}", diag(&json_out));
    let v = stdout_json(&json_out);
    assert_eq!(v["sessionId"], "cx1");
    let msgs = v["messages"].as_array().expect("messages");
    assert_eq!(v["messageCount"].as_u64().unwrap() as usize, msgs.len());
    assert!(
        msgs.iter().any(
            |m| m["$kind"] == "user" && m["content"].to_string().contains("EXPORT-QUESTION-31")
        ),
        "{v}"
    );
    assert!(
        msgs.iter()
            .any(|m| m["$kind"] == "assistant"
                && m["content"].to_string().contains("EXPORT-ANSWER-31")),
        "{v}"
    );

    let md = rig.cli(&["session", "export", "cx1"]);
    assert_eq!(md.status.code(), Some(0), "{}", diag(&md));
    let body = text(&md);
    assert!(
        body.starts_with("**Session export:** cx1\nFormat: markdown\nMessages: "),
        "{body}"
    );
    let user_at = body.find("## User").expect("user heading");
    let assistant_at = body.find("## Assistant").expect("assistant heading");
    assert!(user_at < assistant_at, "turns render in conversation order");
    assert!(body.contains("EXPORT-QUESTION-31") && body.contains("EXPORT-ANSWER-31"));

    let missing = rig.cli(&["export", "nope"]);
    assert_eq!(missing.status.code(), Some(1), "{}", diag(&missing));
    assert!(
        err(&missing).contains("Session not found: nope"),
        "{}",
        diag(&missing)
    );
}

#[test]
fn kill_stops_a_live_session_then_reports_unknown_on_the_second_call() {
    let rig = Rig::start(vec![]);
    rig.session("ck1", json!({}));

    let kill = rig.cli(&["kill", "ck1"]);
    assert_eq!(kill.status.code(), Some(0), "{}", diag(&kill));
    assert_eq!(text(&kill).trim(), "Session ck1 killed.");

    // The engine is gone from memory; the durable row remains and goes dormant.
    rig.wait("killed session to be dormant", 15, || {
        let (_, rows) = rig.get("/sessions");
        row(&rows, "ck1")
            .filter(|r| r["status"] == "dormant")
            .cloned()
    });

    let again = rig.cli(&["kill", "ck1"]);
    assert_eq!(again.status.code(), Some(1), "{}", diag(&again));
    assert!(err(&again).contains("404"), "{}", diag(&again));
}

#[test]
fn agents_lists_sessions_as_json_with_last_activity_and_as_tab_separated_text() {
    let rig = Rig::start(vec![]);
    rig.session("cag1", json!({"title": "Agents probe"}));

    let json_out = rig.cli(&["agents", "--json"]);
    assert_eq!(json_out.status.code(), Some(0), "{}", diag(&json_out));
    let rows = stdout_json(&json_out);
    let r = row(&rows, "cag1").expect("session listed");
    assert_eq!(
        r["lastActivity"], r["updatedAt"],
        "lastActivity mirrors updatedAt"
    );

    let table = rig.cli(&["agents"]);
    assert_eq!(table.status.code(), Some(0), "{}", diag(&table));
    let out = text(&table);
    let line = out
        .lines()
        .find(|l| l.starts_with("cag1\t"))
        .unwrap_or_else(|| panic!("no row in: {out}"));
    let cols: Vec<&str> = line.split('\t').collect();
    assert_eq!(
        cols.len(),
        7,
        "id, status, model, cost, last activity, title, workspace: {line}"
    );
    assert_eq!(cols[1], "running");
    assert_eq!(cols[2], MODEL);
    assert_eq!(cols[5], "Agents probe");

    // Archived sessions are hidden unless --all is given.
    assert_eq!(
        rig.cli(&["session", "archive", "cag1"]).status.code(),
        Some(0)
    );
    let hidden = text(&rig.cli(&["agents"]));
    assert!(!hidden.lines().any(|l| l.starts_with("cag1\t")), "{hidden}");
    let shown = text(&rig.cli(&["agents", "--all"]));
    assert!(shown.lines().any(|l| l.starts_with("cag1\t")), "{shown}");
}

#[test]
fn permission_verbs_drive_live_mode_and_session_rules_through_the_daemon() {
    let rig = Rig::start(vec![]);
    rig.session("cpm1", json!({}));

    let mode = rig.cli(&["permission", "mode", "cpm1", "plan"]);
    assert_eq!(mode.status.code(), Some(0), "{}", diag(&mode));
    assert_eq!(stdout_json(&mode)["mode"], "Plan");
    assert_eq!(
        stdout_json(&rig.cli(&["permission", "mode", "cpm1", "bypass"]))["mode"],
        "Bypass"
    );

    let add = rig.cli(&["permission", "rule-add", "cpm1", "deny", "shell(git push*)"]);
    assert_eq!(add.status.code(), Some(0), "{}", diag(&add));
    let rule = stdout_json(&add)["rule"].clone();
    assert_eq!(rule["pattern"], "shell(git push*)");
    assert_eq!(rule["effect"], "deny");
    assert_eq!(rule["source"], "session");
    let index = rule["index"].as_u64().unwrap().to_string();

    let rules = stdout_json(&rig.cli(&["permissions", "rules", "cpm1"]));
    assert!(
        rules["rules"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["pattern"] == "shell(git push*)" && r["effect"] == "deny"),
        "{rules}"
    );

    let bad = rig.cli(&["permission", "rule-add", "cpm1", "maybe", "x"]);
    assert_eq!(bad.status.code(), Some(1), "{}", diag(&bad));
    assert!(
        err(&bad).contains("400") && err(&bad).contains("permission rule add"),
        "{}",
        diag(&bad)
    );

    let removed = rig.cli(&["permission", "rule-remove", "cpm1", &index]);
    assert_eq!(removed.status.code(), Some(0), "{}", diag(&removed));
    assert_eq!(stdout_json(&removed)["removed"], true);
    let after = stdout_json(&rig.cli(&["permission", "rules", "cpm1"]));
    assert!(
        !after["rules"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["pattern"] == "shell(git push*)"),
        "{after}"
    );

    let gone = rig.cli(&["permission", "rule-remove", "cpm1", "9999"]);
    assert_eq!(gone.status.code(), Some(1), "{}", diag(&gone));
    assert!(err(&gone).contains("404"), "{}", diag(&gone));
}
