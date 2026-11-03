//! Journeys: workflow definition -> validate -> publish -> run -> pause/resume/cancel,
//! scheduled-task fire and restart recovery, foreign (vendor CLI) driver discovery via
//! generated stub executables, and an ACP bridge prompt round-trip.
//! Retires the workflow/api_workflows/tasks/driver_host/acp unit tests.

use crate::support::e2e_orch;
use e2e_orch::*;

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::Duration;

fn enable_workflows(home: &std::path::Path, _ws: &std::path::Path) {
    append_config(home, "[features]\nworkflows-enabled = true\n");
}

fn definition(workspace: &std::path::Path) -> Value {
    let mut d: Value =
        serde_json::from_str(include_str!("../fixtures/orch/budget-watch.workflow.json")).unwrap();
    d["workspace"] = json!({"root": slash_path(workspace), "trust": "trusted"});
    d
}

fn expect(s: u16, b: &Value, what: &str) {
    assert!((200..300).contains(&s), "{what}: {s} {b}");
}

/// create -> validate -> publish; returns (workflow id, version id).
fn publish(env: &Env, name: &str) -> (String, String) {
    let (s, b) = env.post(
        "/workflows",
        json!({"name": name, "definition": definition(&env.workspace)}),
    );
    assert_eq!(s, 201, "create workflow: {b}");
    assert_eq!(b["publicationState"], "draft", "{b}");
    let id = b["id"].as_str().unwrap().to_string();
    let vid = b["versionId"].as_str().unwrap().to_string();
    let (_, vers) = env.get(&format!("/workflows/{id}/versions"));
    assert_eq!(vers.as_array().unwrap().len(), 1, "{vers}");
    assert_eq!(vers[0]["id"], vid.as_str(), "{vers}");
    let (s, b) = env.post(
        &format!("/workflows/{id}/versions/{vid}/validate"),
        json!({}),
    );
    assert_eq!(s, 200, "validate: {b}");
    assert_eq!(b["valid"], true, "{b}");
    let (s, b) = env.post(
        &format!("/workflows/{id}/versions/{vid}/publish"),
        json!({}),
    );
    expect(s, &b, "publish");
    let (_, vers) = env.get(&format!("/workflows/{id}/versions"));
    assert_eq!(vers[0]["publicationState"], "published", "{vers}");
    (id, vid)
}

#[test]
fn workflow_is_invisible_until_enabled_and_invalid_definitions_are_rejected() {
    // Default: feature gate off -> the whole surface is not mounted.
    let off = Env::start(Provider::start(|_, _| Reply::text("ok")), &[], |_, _| {});
    assert_eq!(off.get("/workflows").0, 404);

    let on = Env::start(
        Provider::start(|_, _| Reply::text("ok")),
        &[],
        enable_workflows,
    );
    assert_eq!(on.get("/workflows").0, 200);
    let (s, _) = on.post("/workflows", json!({"name": "  ", "definition": {}}));
    assert_eq!(s, 400, "blank name");
    let (s, _) = on.post(
        "/workflows",
        json!({"name": "bad", "definition": {"nodes": "nope"}}),
    );
    assert_eq!(s, 400, "malformed definition");
    let (s, _) = on.post("/workflows/wfd_missing/runs", json!({}));
    assert_eq!(s, 404, "no published version");
}

#[test]
fn workflow_run_journal_pause_resume_cancel_and_restart_durability() {
    let mut env = Env::start(
        Provider::start(|_, _| Reply::text("ok")),
        &[],
        enable_workflows,
    );
    let (id, vid) = publish(&env, "e2e budget watch");

    let (s, b) = env.post(&format!("/workflows/{id}/runs"), json!({"input": {"n": 1}}));
    assert_eq!(s, 201, "start run: {b}");
    assert_eq!(b["workflowId"], id.as_str(), "{b}");
    assert_eq!(b["workflowVersionId"], vid.as_str(), "{b}");
    assert_eq!(b["lifecyclePhase"], "running", "{b}");
    let run = b["id"].as_str().unwrap().to_string();

    let phase = |env: &Env| {
        env.get(&format!("/runs/{run}")).1["lifecyclePhase"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(phase(&env), "running");
    let (s, b) = env.post(&format!("/runs/{run}/pause"), json!({}));
    expect(s, &b, "pause");
    assert_eq!(phase(&env), "suspended");
    let (s, b) = env.post(&format!("/runs/{run}/resume"), json!({}));
    expect(s, &b, "resume");
    assert_eq!(phase(&env), "running");
    let (s, b) = env.post(&format!("/runs/{run}/cancel"), json!({}));
    expect(s, &b, "cancel");
    assert_eq!(phase(&env), "cancelled");

    let kinds = |env: &Env| -> Vec<String> {
        let (s, j) = env.get(&format!("/runs/{run}/journal"));
        assert_eq!(s, 200, "{j}");
        j["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["eventKind"].as_str().unwrap().to_string())
            .collect()
    };
    let before = kinds(&env);
    let pos = |k: &str| {
        before
            .iter()
            .position(|x| x == k)
            .unwrap_or_else(|| panic!("{k} missing in {before:?}"))
    };
    assert_eq!(pos("run_started"), 0, "{before:?}");
    assert!(
        pos("run_paused") < pos("run_resumed") && pos("run_resumed") < pos("run_cancelled"),
        "{before:?}"
    );

    // Durable across a daemon restart: definition, version and run state survive.
    env.restart(&[]);
    assert_eq!(env.get(&format!("/workflows/{id}")).0, 200);
    assert_eq!(kinds(&env), before, "journal changed across restart");
    assert_eq!(phase(&env), "cancelled", "run phase lost across restart");
    assert_eq!(env.get("/runs/wfr_does_not_exist").0, 404);
}

/// `?workspaceRoot=` query (schedules are stored per workspace, default is the daemon cwd).
fn ws_query(env: &Env) -> String {
    format!(
        "?workspaceRoot={}",
        slash_path(&env.workspace)
            .replace(':', "%3A")
            .replace('/', "%2F")
    )
}

#[test]
fn scheduled_task_fires_on_demand_pauses_and_survives_restart() {
    let provider = Provider::start(|_, _| Reply::text("scheduled answer"));
    let mut env = Env::start(provider, &[], |_, _| {});
    let q = ws_query(&env);
    let (s, b) = env.post(
        &format!("/schedules{q}"),
        json!({"id":"nightly","cronExpr":"0 3 1 1 *","prompt":"SCHEDULED-PROMPT-ZULU"}),
    );
    assert_eq!(s, 201, "create schedule: {b}");

    let (s, b) = env.post(&format!("/schedules/nightly/run{q}"), json!({}));
    assert_eq!(s, 200, "run now: {b}");
    assert_eq!(b["submitted"], true, "{b}");
    // The run is submitted asynchronously: wait for the model to see the prompt, then for a
    // terminal (non-"submitted") run record.
    env.eventually("scheduled prompt reaches the model", 60, || {
        env.provider
            .requests()
            .iter()
            .any(|r| whole_prompt(r).contains("SCHEDULED-PROMPT-ZULU"))
            .then_some(())
    });
    let runs = env.eventually("terminal schedule run", 60, || {
        let (_, r) = env.get(&format!("/schedules/nightly/runs{q}"));
        let list = r["runs"].as_array().cloned().unwrap_or_default();
        list.iter()
            .any(|e| e["status"] != "submitted")
            .then_some(list)
    });
    assert!(
        runs.iter()
            .all(|e| e["scheduleId"] == "nightly" || e["schedule_id"] == "nightly"),
        "{runs:?}"
    );
    let run_count = runs.len();
    let fired = env.provider.count();

    // Paused schedules refuse to fire.
    let (s, b) = env.post(&format!("/schedules/nightly/pause{q}"), json!({}));
    assert_eq!(s, 200, "{b}");
    let (s, _) = env.post(&format!("/schedules/nightly/run{q}"), json!({}));
    assert_eq!(s, 500, "paused schedule must not run");
    assert_eq!(
        env.provider.count(),
        fired,
        "paused schedule reached the model"
    );

    // Restart: schedule, paused flag and run history are durable.
    env.restart(&[]);
    let (_, list) = env.get(&format!("/schedules{q}"));
    let row = list["schedules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "nightly")
        .unwrap_or_else(|| panic!("schedule lost: {list}"));
    assert_eq!(row["paused"], true, "{row}");
    assert_eq!(row["runs"].as_array().unwrap().len(), run_count, "{row}");
    let (s, _) = env.post(&format!("/schedules/nightly/resume{q}"), json!({}));
    assert_eq!(s, 200);
    let (_, list) = env.get(&format!("/schedules{q}"));
    assert_eq!(list["schedules"][0]["paused"], false, "{list}");
    assert_eq!(env.get(&format!("/schedules/ghost/runs{q}")).0, 404);
}

/// Executable stub named `claude` (the adapter looks for that exact file name on PATH).
fn write_vendor_stub(dir: &std::path::Path) {
    std::fs::create_dir_all(dir).unwrap();
    #[cfg(windows)]
    std::fs::write(dir.join("claude.cmd"), "@echo 9.9.9-e2e-stub\r\n").unwrap();
    #[cfg(not(windows))]
    let p = dir.join("claude");
    #[cfg(not(windows))]
    std::fs::write(&p, "#!/bin/sh\necho 9.9.9-e2e-stub\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[test]
fn foreign_driver_is_discovered_from_stub_executable() {
    let stubs = temp_dir("vendor-stubs");
    write_vendor_stub(stubs.path());
    let mut dirs = vec![stubs.path().to_path_buf()];
    dirs.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let path = std::env::join_paths(dirs)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let env = Env::start(
        Provider::start(|_, _| Reply::text("ok")),
        &[("PATH", &path)],
        |_, _| {},
    );
    let (s, d) = env.get("/cli-providers/claude/discovery");
    assert_eq!(s, 200, "{d}");
    assert_eq!(d["installed"], true, "{d}");
    assert!(
        d["executable"]
            .as_str()
            .unwrap()
            .replace('\\', "/")
            .contains(&slash_path(stubs.path())),
        "{d}"
    );
    assert!(d["executable_sha256"].is_string(), "{d}");
}

#[test]
fn foreign_driver_without_stub_is_not_installed_and_launch_is_unauthorized() {
    let empty = temp_dir("no-vendor");
    let env = Env::start(
        Provider::start(|_, _| Reply::text("ok")),
        &[("PATH", &empty.path().to_string_lossy())],
        |_, _| {},
    );
    let (s, d) = env.get("/cli-providers/claude/discovery");
    assert_eq!(s, 200, "{d}");
    assert_eq!(d["installed"], false, "{d}");
    assert!(d["executable"].is_null(), "{d}");

    env.create_session_with("drv", json!({"driverKind":"claude"}));
    let (s, st) = env.get("/sessions/drv/driver/status");
    assert_eq!(s, 200, "{st}");
    assert_eq!(st["driverKind"], "claude", "{st}");
    assert_eq!(st["drivers"], json!([]), "{st}");
    // Launch without a capability lease is refused; the native model is never used instead.
    let (s, b) = env.post("/sessions/drv/driver/start", json!({"bindingId":"nope"}));
    assert_eq!(s, 401, "{b}");
    assert_eq!(
        env.provider.count(),
        0,
        "driver gate must not fall back to the native model"
    );
}

struct Acp(Child);
impl Drop for Acp {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn acp_call(
    child: &mut Child,
    rx: &mpsc::Receiver<Value>,
    req: Value,
    notes: &mut Vec<Value>,
) -> Value {
    let stdin = child.stdin.as_mut().unwrap();
    writeln!(stdin, "{req}").unwrap();
    stdin.flush().unwrap();
    loop {
        let v = rx
            .recv_timeout(Duration::from_secs(60))
            .unwrap_or_else(|e| {
                panic!(
                    "no ACP response to {} ({e}); notifications so far: {notes:?}",
                    req["method"]
                )
            });
        if v["id"] == req["id"] {
            return v;
        }
        notes.push(v);
    }
}

#[test]
fn acp_bridge_prompt_round_trip_streams_model_text_and_unknown_session_errors() {
    let provider = Provider::start(|_, _| Reply::text("ACP-MODEL-ANSWER"));
    let env = Env::start(provider, &[], |_, _| {});
    let acp_home = temp_dir("acp-home");
    let mut child = Acp(
        rightkit_child_process::command_std(daemon_process::executable())
            .args([
                "acp-server",
                "--port",
                &env.daemon.startup.port.to_string(),
                "--token",
                env.token(),
            ])
            .envs([crate::common::suite_env(acp_home.path())])
            .env("HOME", acp_home.path())
            .env("USERPROFILE", acp_home.path())
            .env("RIGHTKIT_DISABLE_OS_KEYCHAIN", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn acp-server"),
    );
    let stdout = child.0.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if tx.send(v).is_err() {
                    break;
                }
            }
        }
    });
    let mut notes = vec![];
    let init = acp_call(
        &mut child.0,
        &rx,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1}}),
        &mut notes,
    );
    assert_eq!(init["result"]["protocolVersion"], 1, "{init}");
    let new = acp_call(
        &mut child.0,
        &rx,
        json!({"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd": env.workspace}}),
        &mut notes,
    );
    let sid = new["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();
    let done = acp_call(
        &mut child.0,
        &rx,
        json!({"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":sid,"prompt":[{"type":"text","text":"say hi"}]}}),
        &mut notes,
    );
    assert!(done["error"].is_null(), "prompt failed: {done}");
    assert!(
        notes
            .iter()
            .any(|n| n.to_string().contains("ACP-MODEL-ANSWER"))
            || done.to_string().contains("ACP-MODEL-ANSWER"),
        "model text never reached the ACP client: notes={notes:?} done={done}"
    );
    assert_eq!(env.provider.count(), 1);

    let bad = acp_call(
        &mut child.0,
        &rx,
        json!({"jsonrpc":"2.0","id":4,"method":"session/prompt","params":{"sessionId":"no-such-session","prompt":[{"type":"text","text":"x"}]}}),
        &mut notes,
    );
    assert!(
        !bad["error"].is_null(),
        "unknown session must be an error: {bad}"
    );
    assert_eq!(
        env.provider.count(),
        1,
        "unknown session must not reach the model"
    );
}
