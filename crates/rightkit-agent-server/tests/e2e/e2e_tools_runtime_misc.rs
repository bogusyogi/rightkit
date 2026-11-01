//! Runtime journeys through a real daemon + CLI: interrupting a running shell command, the
//! execution-backend registry.
//! Retires unit tests of `execution`, `runtime_resources` admission for the
//! shell tool and the cancellation paths of `tools::shell`.

use crate::support::daemon_process;
use crate::support::e2e_api;
use crate::support::scripted_provider;

use e2e_api::*;
use scripted_provider::Step;
use serde_json::{json, Value};

#[test]
fn cancelling_a_tool_invocation_kills_the_running_shell_command_and_reports_it() {
    let rig = Rig::start(vec![
        Step::tool(
            "c1",
            "shell",
            json!({"command":"sleep 40; echo cancel-late > cancel-late.txt"}),
        ),
        Step::text("after cancel"),
    ]);
    let created = rig.session("k1", json!({"permissionMode":"bypass"}));
    let root = std::path::PathBuf::from(created["workspaceRoot"].as_str().unwrap());
    let (status, _) = rig.send("k1", "run something slow");
    assert_eq!(status, 200);
    rig.wait_event("k1", "toolCallStarted");
    // Let the command actually start.
    std::thread::sleep(std::time::Duration::from_millis(1500));

    // Per-call cancel (the session interrupt aborts the turn; this one signals the tool itself).
    let started = std::time::Instant::now();
    let (status, body) = rig.post("/tool-invocations/c1/cancel", json!({"reason":"e2e"}));
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["cancelled"], true, "{body}");

    let result = rig.wait("the cancelled shell result", 30, || {
        rig.history("k1")
            .into_iter()
            .find(|e| kind_of(e) == "toolResultAdded")
    });
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "cancel must not wait for the 40s command"
    );
    let text = result.to_string();
    assert!(
        text.contains("Command cancelled by operator (pid"),
        "{text}"
    );
    assert!(text.contains("\"isError\":true"), "{text}");
    assert!(!root.join("cancel-late.txt").exists());
}

#[test]
fn shell_output_streams_live_over_sse_before_the_command_finishes() {
    use std::io::BufRead;
    use std::time::{Duration, Instant};
    let rig = Rig::start(vec![
        Step::tool(
            "c1",
            "shell",
            json!({"command":"echo LIVE-1; sleep 5; echo LIVE-2"}),
        ),
        Step::text("done"),
    ]);
    rig.session("l1", json!({"permissionMode":"bypass"}));

    // Subscribe BEFORE the turn starts; live projections are never replayed.
    let (tx, rx) = std::sync::mpsc::channel::<(Instant, Value)>();
    let (url, token) = (
        format!("{}/sessions/l1/events", rig.url()),
        rig.token().to_string(),
    );
    std::thread::spawn(move || {
        let Some(resp) = crate::support::daemon_process::open_event_stream(&url, &token) else {
            return;
        };
        for line in std::io::BufReader::new(resp).lines() {
            let Ok(line) = line else { break };
            if let Some(data) = line.strip_prefix("data:") {
                if let Ok(v) = serde_json::from_str::<Value>(data.trim()) {
                    if tx.send((Instant::now(), v)).is_err() {
                        break;
                    }
                }
            }
        }
    });
    std::thread::sleep(Duration::from_millis(500));
    let (status, _) = rig.send("l1", "go");
    assert_eq!(status, 200);

    let (mut first, mut result): (Option<Instant>, Option<Instant>) = (None, None);
    let mut stream_name = String::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    while result.is_none() && Instant::now() < deadline {
        let Ok((t, v)) = rx.recv_timeout(Duration::from_secs(5)) else {
            continue;
        };
        match kind_of(&v).as_str() {
            "toolOutputDelta"
                if first.is_none() && v["text"].as_str().unwrap_or("").contains("LIVE-1") =>
            {
                first = Some(t);
                stream_name = v["stream"].as_str().unwrap_or("").to_string();
            }
            "toolResultAdded" => result = Some(t),
            _ => {}
        }
    }
    let first = first.expect("a toolOutputDelta carrying LIVE-1 over SSE");
    let result = result.expect("the final tool result");
    assert!(
        result.duration_since(first) >= Duration::from_secs(3),
        "LIVE-1 must arrive well before the 5s command finishes ({:?})",
        result.duration_since(first)
    );
    assert_eq!(stream_name, "stdout");
}

#[test]
fn execution_backend_registry_is_served_over_http_and_the_cli() {
    let rig = Rig::start(vec![]);
    let (status, registry) = rig.get("/execution/backends");
    assert_eq!(status, 200, "{registry}");
    assert_eq!(registry["defaultBackend"], "local");
    assert!(
        registry["backends"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["id"] == "local" && b["kind"] == "local"),
        "{registry}"
    );

    let o = rig.cli(&["execution", "backends", "--json"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        stdout_json(&o),
        registry,
        "CLI must print exactly what the daemon serves"
    );

    let o = rig.cli(&["execution", "doctor", "local", "--json"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(stdout_json(&o).is_object());

    // Unauthenticated callers are refused.
    assert_eq!(rig.anon("GET", "/execution/backends"), 401);
}

// A072: the resource profile caps process-backed execution. Practical admits one shell group;
// Strict refuses the same shell command with a typed admission error before any child exists.
#[test]
fn resource_profile_limits_cap_a_runaway_shell_command() {
    use crate::support::e2e_tools as t;

    let command = "echo profile-child-ran > profile-marker.txt";
    let args = &["--stream-json", "--bypass-approvals"];

    let practical_provider = t::sequence(
        vec![t::call("shell", json!({ "command": command }))],
        "done",
    );
    let practical = t::Rig::new(&practical_provider, &[]);
    let out = practical.run_args_with_resource_profile(args, "go", Some("practical"));
    let results = t::expect_results(&out, 1);
    assert!(
        !results[0].error,
        "practical profile admits one shell: {}",
        results[0].text
    );
    assert!(
        practical.exists("profile-marker.txt"),
        "the admitted shell command must have run under the practical profile"
    );

    let strict_provider = t::sequence(
        vec![t::call("shell", json!({ "command": command }))],
        "done",
    );
    let strict = t::Rig::new(&strict_provider, &[]);
    let out = strict.run_args_with_resource_profile(args, "go", Some("strict"));
    let results = t::results(&out);
    assert!(
        results.len() == 1 && results[0].error,
        "strict profile must refuse the shell as a typed error: {results:#?}\n{}",
        out.diag()
    );
    assert!(
        results[0]
            .text
            .contains("resource admission denied before shell spawn")
            || results[0].text.contains(
                "resource_limits_missing: tool 'shell' requires unavailable resource ChildProcess"
            ),
        "the refusal names the admission gate, not a spawn failure: {}",
        results[0].text
    );
    assert!(
        !strict.exists("profile-marker.txt"),
        "no CodeRight child may run under the strict profile"
    );
}
