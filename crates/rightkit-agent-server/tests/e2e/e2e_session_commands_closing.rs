//! Persisted backend paths needed by the closing-pass session commands.

use crate::support::daemon_process;
use crate::support::e2e_api;
use crate::support::scripted_provider;

use e2e_api::*;
use scripted_provider::Step;
use serde_json::json;

#[test]
fn rename_archive_fork_and_delete_survive_session_storage_boundaries() {
    let mut rig = Rig::start(vec![]);
    rig.session("closing-parent", json!({"title": "Before rename"}));

    let (status, renamed) = rig.patch("/sessions/closing-parent", json!({"title": "After rename"}));
    assert_eq!(status, 200, "{renamed}");
    assert_eq!(renamed["updated"], true);
    assert_eq!(
        find(&rig.get("/sessions").1, "closing-parent").unwrap()["title"],
        "After rename"
    );

    let (status, archived) = rig.patch("/sessions/closing-parent", json!({"status": "archived"}));
    assert_eq!(status, 200, "{archived}");
    assert_eq!(
        find(&rig.get("/sessions?status=archived").1, "closing-parent").unwrap()["status"],
        "archived"
    );

    // The daemon restart forces the following list to come from durable storage.
    rig.restart();
    let persisted = find(&rig.get("/sessions").1, "closing-parent")
        .unwrap()
        .clone();
    assert_eq!(persisted["title"], "After rename");
    assert_eq!(persisted["status"], "archived");

    let (status, fork) = rig.post(
        "/sessions/closing-parent/fork",
        json!({"id": "closing-copy"}),
    );
    assert_eq!(status, 200, "{fork}");
    assert_eq!(fork["parentSessionId"], "closing-parent");
    assert_eq!(fork["title"], "Fork of After rename");

    let (status, deleted) = rig.delete("/sessions/closing-copy");
    assert_eq!(status, 200, "{deleted}");
    assert_eq!(deleted["deleted"], true);
    assert!(find(&rig.get("/sessions").1, "closing-copy").is_none());
}

#[test]
fn clone_search_sort_and_markdown_html_exports_use_persisted_conversation() {
    let rig = Rig::start(vec![Step::text("CLONE-ANSWER-42")]);
    rig.session(
        "closing-source",
        json!({"title": "Conversation source", "permissionMode": "bypass"}),
    );
    assert_eq!(rig.send("closing-source", "CLONE-QUESTION-42").0, 200);
    rig.wait_assistant("closing-source", "CLONE-ANSWER-42");

    let (status, clone) = rig.post(
        "/sessions/closing-source/clone",
        json!({"id": "closing-clone"}),
    );
    assert_eq!(status, 200, "{clone}");
    assert_eq!(clone["status"], "dormant");
    assert_eq!(clone["session"]["parentSessionId"], serde_json::Value::Null);
    assert_eq!(clone["session"]["forkedFromSessionId"], "closing-source");

    let (_, rows) = rig.get("/sessions?q=conversation%20source&sort=title&order=asc");
    assert!(find(&rows, "closing-source").is_some());
    assert!(find(&rows, "closing-clone").is_some());
    let (_, markdown) = rig.get("/sessions/closing-clone/export?format=markdown");
    assert_eq!(markdown["format"], "markdown");
    assert!(markdown["content"]
        .as_str()
        .unwrap()
        .contains("CLONE-QUESTION-42"));
    assert!(markdown["content"]
        .as_str()
        .unwrap()
        .contains("CLONE-ANSWER-42"));
    let (_, html) = rig.get("/sessions/closing-clone/export?format=html");
    assert_eq!(html["format"], "html");
    assert!(html["content"].as_str().unwrap().contains("<article>"));
    assert!(html["content"]
        .as_str()
        .unwrap()
        .contains("CLONE-ANSWER-42"));
}

fn find<'a>(rows: &'a serde_json::Value, id: &str) -> Option<&'a serde_json::Value> {
    rows.as_array()?.iter().find(|row| row["id"] == id)
}
