//! Filesystem-watch and execution-process (terminal attach/detach/stdin/logs/
//! kill) HTTP families of a REAL `coderight serve` daemon, driven against real
//! files and a real shell child process.
//!
//! Mirrors `api_fs_watch.rs` (client-scoped watches, debounced buffered poll)
//! and `api_execution/{process_handlers,process_rows,routes_and_types}.rs`
//! (lease-fenced stdin, attach/detach/ack, tail logs). Routes come from
//! `ops004_route_coverage.toml` (`/fs/watch/*`, `/execution/processes*`).
//!
//! Retires `api_fs_watch.rs` tests and `api_execution/tests_process_*`.

use crate::support::daemon_process;
use crate::support::e2e_api;
use crate::support::scripted_provider;

use e2e_api::*;
use serde_json::{json, Value};
use std::time::Duration;

fn poll_events(rig: &Rig, client: &str, watch: &str) -> Vec<Value> {
    let (status, body) = rig.get(&format!("/fs/watch/{client}/{watch}/poll"));
    assert_eq!(status, 200, "{body}");
    body["events"].as_array().cloned().unwrap_or_default()
}

fn wait_for_change(rig: &Rig, client: &str, watch: &str, file_name: &str) -> Value {
    rig.wait(&format!("fs change for {file_name}"), 20, || {
        poll_events(rig, client, watch).into_iter().find(|event| {
            event["changedPaths"].as_array().is_some_and(|paths| {
                paths
                    .iter()
                    .any(|p| p.as_str().is_some_and(|p| p.ends_with(file_name)))
            })
        })
    })
}

#[test]
fn fs_watch_reports_debounced_changes_per_client_and_unwatch_is_owner_scoped() {
    let rig = Rig::start(vec![]);
    let dir = rig.ws().join("watched");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.to_string_lossy().to_string();

    let (status, watched) = rig.post(
        "/fs/watch/client-a",
        json!({"watchId": "w1", "path": path, "recursive": true}),
    );
    assert_eq!(status, 200, "{watched}");
    assert_eq!(
        watched["path"],
        path.as_str(),
        "the registered path is echoed back"
    );

    // The same watch id for the same client is a conflict; another client may reuse it.
    let (status, dup) = rig.post("/fs/watch/client-a", json!({"watchId": "w1", "path": path}));
    assert_eq!(status, 409, "{dup}");
    assert_eq!(dup["message"], "watchId already exists: w1");
    let (status, _) = rig.post(
        "/fs/watch/client-b",
        json!({"watchId": "w1", "path": path, "recursive": true}),
    );
    assert_eq!(status, 200, "watch ids are scoped per client");

    std::fs::write(dir.join("created.txt"), "hello").unwrap();
    let event = wait_for_change(&rig, "client-a", "w1", "created.txt");
    assert_eq!(event["watchId"], "w1");
    let paths: Vec<&str> = event["changedPaths"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p.as_str())
        .collect();
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(paths, sorted, "changed paths are reported sorted");
    assert!(
        paths.iter().all(|p| std::path::Path::new(p).is_absolute()),
        "paths are absolute: {paths:?}"
    );

    // Polling drains the buffer; an unknown watch or foreign client sees nothing.
    assert!(
        poll_events(&rig, "client-a", "w1")
            .iter()
            .all(|e| !e.to_string().contains("created.txt")),
        "already drained"
    );
    assert!(poll_events(&rig, "client-a", "no-such-watch").is_empty());
    assert!(poll_events(&rig, "client-z", "w1").is_empty());

    // A foreign client cannot unwatch someone else's watch; the owner can.
    assert_eq!(rig.delete("/fs/watch/client-z/w1").1, json!({}));
    std::fs::write(dir.join("second.txt"), "again").unwrap();
    wait_for_change(&rig, "client-a", "w1", "second.txt");
    let (status, body) = rig.delete("/fs/watch/client-a/w1");
    assert_eq!((status, body), (200, json!({})));
    std::fs::write(dir.join("after-unwatch.txt"), "late").unwrap();
    std::thread::sleep(Duration::from_millis(900));
    assert!(
        poll_events(&rig, "client-a", "w1").is_empty(),
        "no events are buffered after unwatch"
    );
    // client-b's independent watch on the same path is untouched.
    wait_for_change(&rig, "client-b", "w1", "after-unwatch.txt");
    assert_eq!(
        rig.delete("/fs/watch/client-a/w1").0,
        200,
        "unwatching twice is a harmless no-op"
    );
}

/// Kills the terminal process at the end of a test, even when an assertion fails first.
struct ProcessGuard<'a> {
    rig: &'a Rig,
    id: String,
}

impl Drop for ProcessGuard<'_> {
    fn drop(&mut self) {
        let _ = self
            .rig
            .post(&format!("/execution/processes/{}/kill", self.id), json!({}));
    }
}

fn process_row(rig: &Rig, id: &str) -> Value {
    let (status, body) = rig.get(&format!("/execution/processes/{id}"));
    assert_eq!(status, 200, "{body}");
    body["process"].clone()
}

fn logs(rig: &Rig, id: &str, query: &str) -> Value {
    let (status, body) = rig.get(&format!("/execution/processes/{id}/logs{query}"));
    assert_eq!(status, 200, "{body}");
    body
}

#[test]
fn terminal_process_create_validates_input_and_exposes_a_complete_row() {
    let rig = Rig::start(vec![]);
    rig.session("tp1", json!({}));

    let (status, body) = rig.post("/execution/processes", json!({"sessionId": "  "}));
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"], "sessionId is required");
    let (status, body) = rig.post(
        "/execution/processes",
        json!({"sessionId": "no-such-session"}),
    );
    assert_eq!(status, 404, "{body}");
    assert_eq!(body["error"], "session not found");
    let (_, body) = rig.post(
        "/execution/processes",
        json!({"sessionId": "tp1", "executionChannel": "tty"}),
    );
    assert_eq!(body["error"], "unsupported executionChannel 'tty'");
    let (_, body) = rig.post(
        "/execution/processes",
        json!({"sessionId": "tp1", "persistence": "forever"}),
    );
    assert_eq!(body["error"], "unsupported persistence 'forever'");
    let (status, body) = rig.post("/execution/processes", json!({"sessionId": "tp1", "executionChannel": "pty", "ptySize": {"rows": 0, "cols": 80, "pixelWidth": 0, "pixelHeight": 0}}));
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"], "ptySize rows/cols must be in 1..32767");
    assert_eq!(
        rig.get("/execution/processes").1["processes"],
        json!([]),
        "refused creates spawn nothing"
    );

    let (status, created) = rig.post("/execution/processes", json!({"sessionId": "tp1"}));
    assert_eq!(status, 200, "{created}");
    let row = created["process"].clone();
    let id = row["id"].as_str().expect("process id").to_string();
    let _guard = ProcessGuard {
        rig: &rig,
        id: id.clone(),
    };
    assert_eq!(row["kind"], "terminalSession");
    assert_eq!(row["sessionId"], "tp1");
    assert_eq!(row["backendId"], "local");
    assert_eq!(row["executionChannel"], "pipe");
    assert_eq!(row["persistence"], "inline");
    assert_eq!(row["status"], "running");
    assert_eq!(row["live"], true);
    assert!(
        row["attachment"] == "attached" || row["attachment"] == "detached",
        "{row}"
    );

    let (_, listed) = rig.get("/execution/processes");
    assert!(
        listed["processes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == id.as_str()),
        "{listed}"
    );
    assert_eq!(process_row(&rig, &id)["id"], id.as_str());
    let (status, missing) = rig.get("/execution/processes/proc-does-not-exist");
    assert_eq!(status, 404, "{missing}");
    assert_eq!(missing["error"], "execution process not found");
}

#[test]
fn terminal_stdin_is_lease_fenced_output_is_tailable_and_kill_stops_the_process() {
    let rig = Rig::start(vec![]);
    rig.session("tp2", json!({}));
    let (_, created) = rig.post("/execution/processes", json!({"sessionId": "tp2"}));
    let row = created["process"].clone();
    let id = row["id"].as_str().unwrap().to_string();
    let _guard = ProcessGuard {
        rig: &rig,
        id: id.clone(),
    };
    let stdin_path = format!("/execution/processes/{id}/stdin");

    // Every input must name the lease epoch and a sequence number.
    let (status, body) = rig.post(
        &stdin_path,
        json!({"input": "echo nope", "appendNewline": true}),
    );
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"], "leaseEpoch is required");
    let (status, body) = rig.post(
        &stdin_path,
        json!({"input": "echo nope", "leaseEpoch": row["leaseEpoch"]}),
    );
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"], "inputSequence is required");
    let (status, body) = rig.post(
        &stdin_path,
        json!({"input": "x", "inputBase64": "!!not base64!!", "leaseEpoch": 1, "inputSequence": 1}),
    );
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"], "invalid inputBase64");

    // A real command reaches the real shell and its output is readable from the log tail.
    let (status, written) = rig.post(
        &stdin_path,
        json!({"input": "echo TERMINAL-MARK-77", "appendNewline": true, "leaseEpoch": row["leaseEpoch"], "inputSequence": row["nextInputSequence"]}),
    );
    assert_eq!(status, 200, "{written}");
    assert_eq!(written["process"]["id"], id.as_str());
    let output = rig.wait("shell output", 30, || {
        let l = logs(&rig, &id, "");
        let hit = l["logs"]["output"]
            .as_str()
            .is_some_and(|o| o.contains("TERMINAL-MARK-77"));
        hit.then_some(l)
    });
    assert_eq!(output["logs"]["processId"], id.as_str());
    let full_len = output["logs"]["byteLength"].as_u64().unwrap();
    assert_eq!(
        full_len as usize,
        output["logs"]["output"].as_str().unwrap().len()
    );
    assert_eq!(output["logs"]["truncated"], false);

    // Pipe output (including the next shell prompt) can arrive between reads.
    // Compare the suffix only against a full snapshot with the same byte length.
    let (output, tail) = rig.wait("consistent full and tail snapshots", 30, || {
        let full = logs(&rig, &id, "");
        let tail = logs(&rig, &id, "?tailBytes=6");
        (full["logs"]["byteLength"] == tail["logs"]["byteLength"]).then_some((full, tail))
    });
    assert_eq!(tail["logs"]["tailBytes"], 6);
    assert!(
        tail["logs"]["output"].as_str().unwrap().len() <= 6,
        "{tail}"
    );
    assert_eq!(tail["logs"]["truncated"], true);
    assert!(
        output["logs"]["output"]
            .as_str()
            .unwrap()
            .ends_with(tail["logs"]["output"].as_str().unwrap()),
        "the tail is a suffix of the full log"
    );

    let (status, killed) = rig.post(&format!("/execution/processes/{id}/kill"), json!({}));
    assert_eq!(status, 200, "{killed}");
    rig.wait("process to stop", 20, || {
        (process_row(&rig, &id)["status"] != "running").then_some(())
    });
    assert_eq!(process_row(&rig, &id)["status"], "killed");
    let (status, gone) = rig.post("/execution/processes/proc-does-not-exist/kill", json!({}));
    assert_eq!(status, 404, "{gone}");
}

#[test]
fn terminal_detach_reattach_and_ack_advance_the_lease() {
    let rig = Rig::start(vec![]);
    rig.session("tp3", json!({}));
    // Inline processes cannot be detached (409); only Service-persistent ones keep a lease across detach.
    let (status, created) = rig.post(
        "/execution/processes",
        json!({"sessionId": "tp3", "persistence": "service"}),
    );
    assert_eq!(status, 200, "{created}");
    let row = created["process"].clone();
    let id = row["id"].as_str().unwrap().to_string();
    let _guard = ProcessGuard {
        rig: &rig,
        id: id.clone(),
    };
    let stdin_path = format!("/execution/processes/{id}/stdin");
    let first_epoch = row["leaseEpoch"].as_u64().unwrap();

    let (status, inline) = rig.post("/execution/processes", json!({"sessionId": "tp3"}));
    assert_eq!(status, 200, "{inline}");
    let inline_id = inline["process"]["id"].as_str().unwrap().to_string();
    let _inline_guard = ProcessGuard {
        rig: &rig,
        id: inline_id.clone(),
    };
    let (status, refused) = rig.post(
        &format!("/execution/processes/{inline_id}/detach"),
        json!({}),
    );
    assert_eq!(status, 409, "{refused}");
    assert_eq!(
        refused["error"],
        "inline process cannot be detached; use Service persistence"
    );

    let (status, detached) = rig.post(&format!("/execution/processes/{id}/detach"), json!({}));
    assert_eq!(status, 200, "{detached}");
    assert_eq!(detached["processId"], id.as_str());
    assert_eq!(detached["attached"], false);
    assert_eq!(process_row(&rig, &id)["attachment"], "detached");

    let (status, attached) = rig.post(&format!("/execution/processes/{id}/attach"), json!({}));
    assert_eq!(status, 200, "{attached}");
    assert_eq!(attached["process"]["id"], id.as_str());
    assert_eq!(attached["process"]["attachment"], "attached");
    let epoch = attached["leaseEpoch"].as_u64().unwrap();
    assert!(
        epoch > first_epoch,
        "a re-attach starts a new lease epoch: {first_epoch} -> {epoch}"
    );

    // Input written under the superseded epoch is fenced off.
    let (status, stale) = rig.post(&stdin_path, json!({"input": "echo stale", "appendNewline": true, "leaseEpoch": first_epoch, "inputSequence": attached["nextInputSequence"]}));
    assert_eq!(status, 409, "{stale}");
    assert!(stale["error"].is_string());

    // The current epoch works, and its input can be acknowledged.
    let seq = attached["nextInputSequence"].as_u64().unwrap();
    let (status, ok) = rig.post(&stdin_path, json!({"input": "echo LEASE-OK-5", "appendNewline": true, "leaseEpoch": epoch, "inputSequence": seq}));
    assert_eq!(status, 200, "{ok}");
    rig.wait("echoed output", 30, || {
        logs(&rig, &id, "")["logs"]["output"]
            .as_str()
            .filter(|o| o.contains("LEASE-OK-5"))
            .map(|_| ())
    });
    let (status, acked) = rig.post(
        &format!("/execution/processes/{id}/ack"),
        json!({"leaseEpoch": epoch, "sequence": seq}),
    );
    assert_eq!(status, 200, "{acked}");
    assert_eq!(acked["processId"], id.as_str());
    assert_eq!(acked["leaseEpoch"], epoch);
    assert!(
        acked["lastAckedInputSequence"].as_u64().unwrap() >= seq,
        "{acked}"
    );

    assert_eq!(
        rig.post("/execution/processes/proc-does-not-exist/attach", json!({}))
            .0,
        404
    );
    assert_eq!(
        rig.post("/execution/processes/proc-does-not-exist/detach", json!({}))
            .0,
        404
    );
}
