//! Real ServiceHost PTY detach/reattach and restart qualification.
//!
//! The child is the production terminal shell launched behind the real
//! `/execution/processes` API. The shared harness launches daemon and child
//! processes without creating visible Windows consoles.

use crate::support::daemon_process;
use crate::support::e2e_api;
use crate::support::scripted_provider;

use e2e_api::*;
use serde_json::{json, Value};

struct ProcessGuard {
    base_url: String,
    token: String,
    id: String,
}

impl ProcessGuard {
    fn new(rig: &Rig, id: String) -> Self {
        Self {
            base_url: rig.url(),
            token: rig.token().to_string(),
            id,
        }
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        // Restart changes port/token; teardown must never panic on a stale
        // endpoint. The test explicitly waits for its shell to exit first.
        daemon_process::post_ignoring_transport_failure(
            &self.base_url,
            &format!("/execution/processes/{}/kill", self.id),
            &self.token,
            &json!({}),
        );
        // Kill is asynchronous: wait for terminal state before Rig drops its
        // workspace. A stale restart endpoint means this shell already exited.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            match daemon_process::get_json(
                &self.base_url,
                &format!("/execution/processes/{}", self.id),
                &self.token,
            ) {
                Some(body) if body["process"]["status"] == "running" => {
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                _ => break,
            }
        }
    }
}

fn process_row(rig: &Rig, id: &str) -> Value {
    let (status, body) = rig.get(&format!("/execution/processes/{id}"));
    assert_eq!(status, 200, "{body}");
    body["process"].clone()
}

fn pid_command(marker: &str) -> String {
    if cfg!(windows) {
        format!("Write-Output \"{marker}:$PID\"")
    } else {
        format!("echo {marker}:$$")
    }
}

fn process_id_from_logs(rig: &Rig, id: &str, marker: &str) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut last = String::new();
    loop {
        let (status, body) = rig.get(&format!("/execution/processes/{id}/logs"));
        if status == 200 {
            last = body["logs"]["output"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let found = last.lines().find_map(|line| {
                // ConPTY may paint the echoed command and its output on one line
                // via cursor moves; the output is the last occurrence.
                let suffix = line.rsplit_once(&format!("{marker}:"))?.1.trim();
                (!suffix.is_empty() && suffix.chars().all(|ch| ch.is_ascii_digit()))
                    .then(|| suffix.to_string())
            });
            if let Some(pid) = found {
                return pid;
            }
        } else {
            last = format!("logs status {status}: {body}");
        }
        if std::time::Instant::now() >= deadline {
            let tail: String = last
                .chars()
                .rev()
                .take(1500)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            panic!(
                "timed out waiting for {marker} from PTY child; log tail:
{tail:?}"
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

fn send_line(rig: &Rig, id: &str, line: &str, epoch: u64, sequence: u64) -> Value {
    let (status, body) = rig.post(
        &format!("/execution/processes/{id}/stdin"),
        json!({
            "input": line,
            "appendNewline": true,
            "leaseEpoch": epoch,
            "inputSequence": sequence,
        }),
    );
    assert_eq!(status, 200, "{body}");
    body
}

#[test]
fn service_pty_survives_detach_and_reattach_with_fenced_input_lease() {
    let rig = Rig::start(vec![]);
    rig.session("pty-lease", json!({}));

    let (status, inline) = rig.post(
        "/execution/processes",
        json!({"sessionId": "pty-lease", "executionChannel": "pty"}),
    );
    assert_eq!(status, 200, "{inline}");
    let inline_id = inline["process"]["id"].as_str().unwrap().to_string();
    let _inline_guard = ProcessGuard::new(&rig, inline_id.clone());
    assert_eq!(inline["process"]["executionChannel"], "pty");
    assert_eq!(inline["process"]["persistence"], "inline");
    let (status, refused) = rig.post(
        &format!("/execution/processes/{inline_id}/detach"),
        json!({}),
    );
    assert_eq!(status, 409, "{refused}");
    assert_eq!(
        refused["error"],
        "inline process cannot be detached; use Service persistence"
    );
    let (status, stopped_inline) =
        rig.post(&format!("/execution/processes/{inline_id}/kill"), json!({}));
    assert_eq!(status, 200, "{stopped_inline}");

    let (status, created) = rig.post(
        "/execution/processes",
        json!({
            "sessionId": "pty-lease",
            "executionChannel": "pty",
            "ptySize": {"rows": 24, "cols": 80, "pixelWidth": 0, "pixelHeight": 0},
            "persistence": "service",
        }),
    );
    assert_eq!(status, 200, "{created}");
    let initial = created["process"].clone();
    let id = initial["id"].as_str().unwrap().to_string();
    let _guard = ProcessGuard::new(&rig, id.clone());
    assert_eq!(initial["executionChannel"], "pty");
    assert_eq!(initial["persistence"], "service");
    assert_eq!(initial["status"], "running");
    let initial_epoch = initial["leaseEpoch"].as_u64().unwrap();
    let initial_sequence = initial["nextInputSequence"].as_u64().unwrap();

    send_line(
        &rig,
        &id,
        &pid_command("PTY_SAME_CHILD"),
        initial_epoch,
        initial_sequence,
    );
    let child_pid = process_id_from_logs(&rig, &id, "PTY_SAME_CHILD");
    let (status, ack) = rig.post(
        &format!("/execution/processes/{id}/ack"),
        json!({"leaseEpoch": initial_epoch, "sequence": initial_sequence}),
    );
    assert_eq!(status, 200, "{ack}");
    assert_eq!(ack["lastAckedInputSequence"], initial_sequence);

    let row = process_row(&rig, &id);
    let delayed_probe = if cfg!(windows) {
        "Start-Sleep -Seconds 1; Write-Output PTY_DETACHED_CONTINUED:$PID".to_string()
    } else {
        "sleep 1; echo PTY_DETACHED_CONTINUED:$$".to_string()
    };
    send_line(
        &rig,
        &id,
        &delayed_probe,
        initial_epoch,
        row["nextInputSequence"].as_u64().unwrap(),
    );
    let (status, detached) = rig.post(&format!("/execution/processes/{id}/detach"), json!({}));
    assert_eq!(status, 200, "{detached}");
    assert_eq!(detached["attached"], false);
    let continued_pid = process_id_from_logs(&rig, &id, "PTY_DETACHED_CONTINUED");
    assert_eq!(
        continued_pid, child_pid,
        "detached PTY shell remains the same child"
    );
    assert_eq!(process_row(&rig, &id)["status"], "running");

    let (status, detached_write) = rig.post(
        &format!("/execution/processes/{id}/stdin"),
        json!({
            "input": "echo MUST_NOT_BE_WRITTEN",
            "appendNewline": true,
            "leaseEpoch": initial_epoch,
            "inputSequence": detached["nextInputSequence"],
        }),
    );
    assert_eq!(status, 409, "{detached_write}");

    // PTY-002 forbids ambiguous replay: this outstanding sequence was
    // observed executing while detached, so reconcile it as delivered.
    let (status, unreconciled) = rig.post(&format!("/execution/processes/{id}/attach"), json!({}));
    assert_eq!(status, 409, "{unreconciled}");
    let (status, attached) = rig.post(
        &format!("/execution/processes/{id}/attach"),
        json!({"reconcileUnacknowledgedInput": "delivered"}),
    );
    assert_eq!(status, 200, "{attached}");
    assert_eq!(attached["process"]["id"], id.as_str());
    assert_eq!(attached["process"]["attachment"], "attached");
    let epoch = attached["leaseEpoch"].as_u64().unwrap();
    assert!(epoch > initial_epoch, "reattach fences lease epoch forward");

    let stale_sequence = attached["nextInputSequence"].as_u64().unwrap();
    let (status, stale) = rig.post(
        &format!("/execution/processes/{id}/stdin"),
        json!({
            "input": "echo MUST_NOT_BE_WRITTEN",
            "appendNewline": true,
            "leaseEpoch": initial_epoch,
            "inputSequence": stale_sequence,
        }),
    );
    assert_eq!(status, 409, "{stale}");
    assert!(stale["error"].is_string(), "{stale}");

    let (status, resumed) = rig.post(
        &format!("/execution/processes/{id}/stdin"),
        json!({
            "input": &pid_command("PTY_REATTACHED_CHILD"),
            "appendNewline": true,
            "leaseEpoch": epoch,
            "inputSequence": stale_sequence,
        }),
    );
    assert_eq!(status, 200, "{resumed}");
    let resumed_pid = process_id_from_logs(&rig, &id, "PTY_REATTACHED_CHILD");
    assert_eq!(
        resumed_pid, child_pid,
        "reattach resumes the original shell"
    );
    let (status, exit) = rig.post(
        &format!("/execution/processes/{id}/stdin"),
        json!({
            "input": "exit",
            "appendNewline": true,
            "leaseEpoch": epoch,
            "inputSequence": resumed["process"]["nextInputSequence"],
        }),
    );
    assert_eq!(status, 200, "{exit}");
    rig.wait("PTY child exit", 20, || {
        (process_row(&rig, &id)["status"] == "exited").then_some(())
    });
}

#[test]
fn service_pty_restart_reports_unavailable_history_without_respawning_child() {
    let mut rig = Rig::start(vec![]);
    rig.session("pty-restart", json!({}));
    let (status, created) = rig.post(
        "/execution/processes",
        json!({"sessionId": "pty-restart", "executionChannel": "pty", "persistence": "service"}),
    );
    assert_eq!(status, 200, "{created}");
    let row = created["process"].clone();
    let id = row["id"].as_str().unwrap().to_string();
    let _guard = ProcessGuard::new(&rig, id.clone());
    let epoch = row["leaseEpoch"].as_u64().unwrap();
    let seq = row["nextInputSequence"].as_u64().unwrap();
    send_line(&rig, &id, &pid_command("PTY_CRASH_CHILD"), epoch, seq);
    let _child_pid = process_id_from_logs(&rig, &id, "PTY_CRASH_CHILD");

    // Close the real shell before the hard host restart, so the fixture is
    // process-bounded while still qualifying persisted PTY crash history.
    let current = process_row(&rig, &id);
    let (status, exit) = rig.post(
        &format!("/execution/processes/{id}/stdin"),
        json!({
            "input": "exit",
            "appendNewline": true,
            "leaseEpoch": epoch,
            "inputSequence": current["nextInputSequence"],
        }),
    );
    assert_eq!(status, 200, "{exit}");
    rig.wait("PTY child exit before host restart", 20, || {
        (process_row(&rig, &id)["status"] == "exited").then_some(())
    });

    let workspace_root = rig.ws().to_string_lossy().to_string();
    rig.restart();
    let encoded_root = workspace_root
        .replace('\\', "/")
        .replace(':', "%3A")
        .replace('/', "%2F");
    let (status, listed) = rig.get(&format!(
        "/execution/processes?workspaceRoot={encoded_root}"
    ));
    assert_eq!(status, 200, "{listed}");
    let rows = listed["processes"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "restart must not create a second execution");
    let historical = rows
        .iter()
        .find(|row| row["id"] == id)
        .expect("same process identity");
    assert_eq!(historical["id"], id.as_str());
    assert_eq!(historical["executionChannel"], "pty");
    assert_eq!(historical["persistence"], "service");
    assert_eq!(historical["live"], false);
    assert_eq!(historical["status"], "unknown");
    assert_eq!(historical["controlMode"], "historicalUnavailable");

    let (status, denied) = rig.post(
        &format!("/execution/processes/{id}/kill?workspaceRoot={encoded_root}"),
        json!({}),
    );
    assert_eq!(status, 409, "{denied}");
    assert!(denied["error"]
        .as_str()
        .is_some_and(|error| error.contains("cannot be killed")));
}
