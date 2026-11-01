//! Task / workflow / meta tool journeys through the REAL `coderight run` binary: the scripted
//! provider issues each tool call, the test asserts the real effect (files, git state, processes)
//! AND the exact text the model is shown back.
//! Tools: todo_write, sleep, get_time, sequentialthinking, config, notebook, task_create /
//! task_update / task_get / task_list / task_output / task_stop, exec, process, repl,
//! worktree_enter / worktree_exit, schedule_cron.

use crate::support::e2e_tools as t;
use t::*;

fn shell(cmd: &str) -> Step {
    call("shell", json!({ "command": cmd }))
}

fn sleep_ms(ms: u64) -> Step {
    call("sleep", json!({ "milliseconds": ms }))
}

/// `toolResultAdded` events keyed by tool-call id (nested composition calls included).
fn results_by_id(out: &Out) -> Vec<(String, Res)> {
    out.events()
        .iter()
        .filter(|e| e["event"] == "toolResultAdded")
        .map(|e| {
            let r = &e["data"]["result"];
            (
                r["toolCallId"].as_str().unwrap_or_default().to_string(),
                Res {
                    text: r["summary"].as_str().unwrap_or_default().to_string(),
                    error: r["isError"].as_bool().unwrap_or(false),
                },
            )
        })
        .collect()
}

fn forward(p: &std::path::Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

// ─────────────────────────────── todo_write ───────────────────────────────

#[test]
fn todo_write_renders_the_list_and_rejects_invalid_lists() {
    let p = sequence(
        vec![
            call(
                "todo_write",
                json!({"todos":[
                    {"content":"Write the failing test","status":"completed"},
                    {"content":"Implement the tool","status":"in_progress","activeForm":"Implementing the tool"},
                    {"content":"Ship it","status":"pending"}
                ]}),
            ),
            call(
                "todo_write",
                json!({"todos":[
                    {"content":"a","status":"in_progress"},
                    {"content":"b","status":"in_progress"}
                ]}),
            ),
            call("todo_write", json!({"todos":[]})),
            call(
                "todo_write",
                json!({"todos":[{"content":"a","status":"bogus"}]}),
            ),
            call(
                "todo_write",
                json!({"todos":[{"content":"  ","status":"pending"}]}),
            ),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    let out = rig.run("go");
    let r = expect_results(&out, 5);

    assert!(!r[0].error, "{}", r[0].text);
    assert_eq!(
        r[0].text,
        "Todos (1/3 done):\n[x] Write the failing test\n[~] Implementing the tool\n[ ] Ship it"
    );
    assert!(r[1].error);
    assert!(
        r[1].text.contains("2 tasks are in_progress"),
        "{}",
        r[1].text
    );
    assert!(r[2].error);
    assert!(r[2].text.contains("todos is empty"), "{}", r[2].text);
    assert!(r[3].error);
    assert!(
        r[3].text.contains("todos[0].status 'bogus' is invalid"),
        "{}",
        r[3].text
    );
    assert!(r[4].error);
    assert!(
        r[4].text.contains("todos[0].content is missing or empty"),
        "{}",
        r[4].text
    );

    // The model was shown exactly what the user-visible stream reported.
    let seen = p.tool_results();
    assert_eq!(seen.len(), 5, "{seen:?}");
    assert_eq!(seen[0], r[0].text);
}

// ───────────────────────────────── sleep ──────────────────────────────────

#[test]
fn sleep_waits_the_requested_time_and_enforces_the_ceiling() {
    let p = sequence(
        vec![sleep_ms(250), sleep_ms(61_000), call("sleep", json!({}))],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    let started = std::time::Instant::now();
    let out = rig.run("go");
    let r = expect_results(&out, 3);

    assert!(!r[0].error, "{}", r[0].text);
    assert!(r[0].text.starts_with("Slept for "), "{}", r[0].text);
    assert!(
        r[0].text.ends_with(" ms (requested: 250 ms)"),
        "{}",
        r[0].text
    );
    let slept: u64 = number_after(&r[0].text, "Slept for ")
        .expect("elapsed ms")
        .parse()
        .unwrap();
    assert!(slept >= 250, "slept only {slept} ms");
    assert!(started.elapsed() >= std::time::Duration::from_millis(250));

    assert!(r[1].error);
    assert!(
        r[1].text
            .contains("Sleep duration 61000 ms exceeds maximum of 60000 ms"),
        "{}",
        r[1].text
    );
    assert!(r[2].error, "missing milliseconds must fail: {}", r[2].text);
}

// ──────────────────────────────── get_time ────────────────────────────────

#[test]
fn get_time_reports_current_time_converts_and_diffs() {
    let p = sequence(
        vec![
            call(
                "get_time",
                json!({"operation":"current_time","timezone":"Asia/Tokyo"}),
            ),
            call(
                "get_time",
                json!({"operation":"convert","datetime":"2026-06-02T09:00:00Z","to_timezone":"Asia/Tokyo"}),
            ),
            call(
                "get_time",
                json!({"operation":"diff","start":"2026-01-01T00:00:00Z","end":"2026-01-02T03:04:05Z"}),
            ),
            call(
                "get_time",
                json!({"operation":"diff","start":"nonsense","end":"2026-01-02T03:04:05Z"}),
            ),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let out = rig.run("go");
    let r = expect_results(&out, 4);

    let now: Value = serde_json::from_str(&r[0].text).expect("current_time returns JSON");
    let stamp = now["unix_timestamp"].as_i64().expect("unix_timestamp");
    assert!(
        (stamp - before).abs() < 180,
        "tool clock {stamp} is not the real clock {before}"
    );
    let tokyo = now["times"]["Asia/Tokyo"].as_str().expect("Tokyo time");
    assert!(tokyo.contains("+09:00"), "{tokyo}");

    let converted: Value = serde_json::from_str(&r[1].text).expect("convert returns JSON");
    assert!(
        converted["result"]
            .as_str()
            .unwrap()
            .starts_with("2026-06-02T18:00:00+09:00"),
        "{converted}"
    );

    let diff: Value = serde_json::from_str(&r[2].text).expect("diff returns JSON");
    assert_eq!(diff["total_seconds"], 97_445);
    assert_eq!(diff["direction"], "future");
    assert_eq!(diff["human"], "1d 3h 4m 5s (future)");

    assert!(r[3].error);
    assert!(
        r[3].text.contains("cannot parse start datetime 'nonsense'"),
        "{}",
        r[3].text
    );
}

// ───────────────────────────── sequentialthinking ─────────────────────────

#[test]
fn sequentialthinking_accumulates_a_chain_across_calls() {
    let p = sequence(
        vec![
            call(
                "sequentialthinking",
                json!({"thought":"Look at the failing input","thoughtNumber":1,"totalThoughts":2,"nextThoughtNeeded":true}),
            ),
            call(
                "sequentialthinking",
                json!({"thought":"Revise: the input was fine","thoughtNumber":2,"totalThoughts":2,"nextThoughtNeeded":false,"isRevision":true,"revisesThought":1}),
            ),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    let out = rig.run("go");
    let r = expect_results(&out, 2);

    let first: Value = serde_json::from_str(&r[0].text).expect("JSON");
    assert_eq!(first["thoughtNumber"], 1);
    assert_eq!(first["nextThoughtNeeded"], true);
    assert_eq!(first["status"], "Thought 1/2 recorded. Continue reasoning.");

    let second: Value = serde_json::from_str(&r[1].text).expect("JSON");
    assert_eq!(second["thought"], "Revise: the input was fine");
    assert_eq!(second["nextThoughtNeeded"], false);
    assert_eq!(
        second["status"],
        "Thought 2/2 recorded. Reasoning complete (2 thoughts total)."
    );
}

// ─────────────────────────────────── config ───────────────────────────────

#[test]
fn config_tool_sets_gets_lists_and_persists_keys() {
    let p = sequence(
        vec![
            call("config", json!({"operation":"list"})),
            call(
                "config",
                json!({"operation":"set","key":"ui.theme","value":"dark"}),
            ),
            call(
                "config",
                json!({"operation":"set","key":"retries","value":3}),
            ),
            call("config", json!({"operation":"get","key":"ui.theme"})),
            call("config", json!({"operation":"get","key":"retries"})),
            call("config", json!({"operation":"get","key":"missing.key"})),
            call(
                "config",
                json!({"operation":"set","key":"bad key!","value":1}),
            ),
            call("config", json!({"operation":"list"})),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    let out = rig.run("go");
    let r = expect_results(&out, 8);

    assert_eq!(r[0].text, "No configuration keys set");
    assert_eq!(r[1].text, "Set config key 'ui.theme' to \"dark\"");
    assert_eq!(r[2].text, "Set config key 'retries' to 3");
    assert_eq!(r[3].text, "\"dark\"");
    assert_eq!(r[4].text, "3");
    assert!(r[5].error);
    assert_eq!(r[5].text, "Configuration key 'missing.key' not found");
    assert!(r[6].error);
    assert_eq!(
        r[6].text,
        "Key must contain only alphanumeric characters, underscores, and dots"
    );
    assert!(r[7].text.starts_with("Configuration:\n"), "{}", r[7].text);
    assert!(r[7].text.contains("ui.theme: \"dark\""), "{}", r[7].text);
    assert!(r[7].text.contains("retries: 3"), "{}", r[7].text);
    assert!(!r[7].text.contains("bad key"), "{}", r[7].text);

    // The value really is persisted in the workspace, not just echoed back.
    let saved: Value = serde_json::from_str(&rig.read(".coderight/agent-config.json"))
        .expect("agent-config.json is JSON");
    assert_eq!(saved["ui.theme"], "dark");
    assert_eq!(saved["retries"], 3);
    assert!(saved.get("bad key!").is_none());
}

// ───────────────────────────────── notebook ───────────────────────────────

const NOTEBOOK: &str = r##"{
 "cells": [
  {"cell_type":"markdown","metadata":{},"source":["# Title"]},
  {"cell_type":"code","execution_count":null,"metadata":{},"outputs":[],"source":["print('original')"]}
 ],
 "metadata": {},
 "nbformat": 4,
 "nbformat_minor": 5
}"##;

#[test]
fn notebook_reads_edits_inserts_and_deletes_cells_on_disk() {
    let p = sequence(
        vec![
            call(
                "notebook",
                json!({"path":"nb.ipynb","operation":"read_cell","index":0}),
            ),
            call(
                "notebook",
                json!({"path":"nb.ipynb","operation":"edit_cell","index":1,"source":"print('edited')"}),
            ),
            call(
                "notebook",
                json!({"path":"nb.ipynb","operation":"insert_cell","index":1,"kind":"code","source":"x = 41"}),
            ),
            call(
                "notebook",
                json!({"path":"nb.ipynb","operation":"read_cell","index":1}),
            ),
            call(
                "notebook",
                json!({"path":"nb.ipynb","operation":"delete_cell","index":0}),
            ),
            call(
                "notebook",
                json!({"path":"nb.ipynb","operation":"read_cell","index":9}),
            ),
            call(
                "notebook",
                json!({"notebook_path":"nb.ipynb","cell_id":"cell-0","new_source":"x = 42","edit_mode":"replace"}),
            ),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[("nb.ipynb", NOTEBOOK)]);
    let out = rig.run("go");
    let r = expect_results(&out, 7);

    assert_eq!(r[0].text, "Cell [0] (type: markdown)\n# Title");
    assert_eq!(r[1].text, "Edited cell [1]");
    assert_eq!(r[2].text, "Inserted code cell at index [1]");
    assert_eq!(r[3].text, "Cell [1] (type: code)\nx = 41");
    assert_eq!(r[4].text, "Deleted cell [0]");
    assert!(r[5].error);
    assert_eq!(
        r[5].text,
        "Cell index 9 out of range (notebook has 2 cells)"
    );
    assert!(!r[6].error, "{}", r[6].text);
    assert_eq!(r[6].text, "Updated cell cell-0");

    // On disk: markdown cell deleted, inserted cell replaced via the donor-mode edit,
    // the edited code cell survives, nbformat is preserved.
    let nb: Value = serde_json::from_str(&rig.read("nb.ipynb")).expect("notebook stays JSON");
    let cells = nb["cells"].as_array().unwrap();
    assert_eq!(cells.len(), 2, "{nb}");
    assert_eq!(cells[0]["source"], "x = 42");
    assert_eq!(cells[1]["source"], "print('edited')");
    assert_eq!(nb["nbformat"], 4);
}

// ──────────────────────────────── task tools ──────────────────────────────

fn first_token(text: &str) -> String {
    text.split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string()
}

#[test]
fn task_tools_create_update_get_list_output_and_stop() {
    let p = sequence(
        vec![
            // 0: plain record task
            call(
                "task_create",
                json!({"title":"record task","done_when":"it is recorded","category":"qa"}),
            ),
            // 1: shell-backed task that finishes on its own
            call(
                "task_create",
                json!({"title":"quick shell task","command":"echo task-shell-out"}),
            ),
            // 2: let the background shell exit (scripted steps cannot loop; bounded 10s wait)
            sleep_ms(10_000),
            // 3: task_output polls the shell and appends its output to the record
            call_with("task_output", |s| json!({"id": first_token(&s[1])})),
            // 4: state moved to Done because the shell exited 0
            call_with("task_get", |s| json!({"id": first_token(&s[1])})),
            // 5: manual lifecycle change on the record task
            call_with(
                "task_update",
                |s| json!({"id": first_token(&s[0]), "state":"done", "append_output":"finished by test", "title":"record task (done)"}),
            ),
            // 6
            call("task_list", json!({})),
            // 7: stopping a terminal task is a no-op
            call_with("task_stop", |s| json!({"id": first_token(&s[0])})),
            // 8: a long-running shell task...
            call(
                "task_create",
                json!({"title":"long task","command":"sleep 60"}),
            ),
            // 9: ...is really stopped
            call_with("task_stop", |s| json!({"id": first_token(&s[8])})),
            // 10
            call_with("task_get", |s| json!({"id": first_token(&s[8])})),
            // 11: errors
            call(
                "task_update",
                json!({"id":"task-does-not-exist","state":"done"}),
            ),
            call_with(
                "task_update",
                |s| json!({"id": first_token(&s[0]), "state":"bogus"}),
            ),
            call("task_get", json!({"id":"task-does-not-exist"})),
            // 14: the other record-only stop error path
            call("task_create", json!({"title":"open record"})),
            call_with("task_stop", |s| json!({"id": first_token(&s[14])})),
            // 16: let the killed process settle before the run ends
            sleep_ms(500),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    let out = rig.run("go");
    let r = expect_results(&out, 17);

    // 0
    assert!(!r[0].error, "{}", r[0].text);
    let rec = first_token(&r[0].text);
    assert!(rec.starts_with("task-"), "{}", r[0].text);
    assert_eq!(
        r[0].text,
        format!("{rec} [record Open] record task\ndone_when: it is recorded")
    );
    // 1
    assert!(!r[1].error, "{}", r[1].text);
    let quick = first_token(&r[1].text);
    assert!(
        r[1].text
            .starts_with(&format!("{quick} [shell InProgress] quick shell task\n")),
        "{}",
        r[1].text
    );
    assert!(
        r[1].text.contains("command: echo task-shell-out"),
        "{}",
        r[1].text
    );
    assert!(r[1].text.contains("shell_id: bg-"), "{}", r[1].text);
    // 3
    assert!(!r[3].error, "{}", r[3].text);
    assert!(r[3].text.contains("task-shell-out"), "{}", r[3].text);
    // 4
    assert!(
        r[4].text
            .starts_with(&format!("{quick} [shell Done] quick shell task")),
        "task must be Done after the shell exited 0: {}",
        r[4].text
    );
    assert!(r[4].text.contains("task-shell-out"), "{}", r[4].text);
    // 5
    assert!(!r[5].error, "{}", r[5].text);
    assert!(
        r[5].text
            .starts_with(&format!("{rec} [record Done] record task (done)")),
        "{}",
        r[5].text
    );
    assert!(r[5].text.contains("finished by test"), "{}", r[5].text);
    // 6
    assert!(r[6].text.starts_with("Tasks:\n"), "{}", r[6].text);
    assert!(
        r[6].text
            .contains(&format!("{rec} [record Done] record task (done)")),
        "{}",
        r[6].text
    );
    assert!(
        r[6].text
            .contains(&format!("{quick} [shell Done] quick shell task")),
        "{}",
        r[6].text
    );
    // 7
    assert!(!r[7].error, "{}", r[7].text);
    assert_eq!(
        r[7].text,
        format!("Task '{rec}' is already Done; no stop needed.")
    );
    // 8-10
    assert!(!r[8].error, "{}", r[8].text);
    let long = first_token(&r[8].text);
    assert!(r[8].text.contains("[shell InProgress]"), "{}", r[8].text);
    assert!(!r[9].error, "{}", r[9].text);
    assert!(
        r[9].text.starts_with(&format!("Stopped task '{long}'.")),
        "{}",
        r[9].text
    );
    assert!(
        // Explicit stop settles as Cancelled (task_tools/other_tools.rs),
        // distinct from a shell that exits with an error.
        r[10]
            .text
            .starts_with(&format!("{long} [shell Cancelled] long task")),
        "{}",
        r[10].text
    );
    // 11-13
    assert!(r[11].error);
    assert_eq!(r[11].text, "No task with id 'task-does-not-exist'.");
    assert!(r[12].error);
    assert_eq!(r[12].text, "Invalid task state 'bogus'.");
    assert!(r[13].error);
    assert_eq!(r[13].text, "No task with id 'task-does-not-exist'.");
    // 14-15
    assert!(!r[14].error, "{}", r[14].text);
    assert!(r[15].error);
    assert!(
        r[15]
            .text
            .contains("task_stop only supports active shell-backed tasks"),
        "{}",
        r[15].text
    );

    // Durable record on disk.
    let saved: Value =
        serde_json::from_str(&rig.read(".coderight/tasks.json")).expect("tasks.json is JSON");
    assert_eq!(saved[&rec]["node"]["title"], "record task (done)");
    assert_eq!(saved[&rec]["output"], "finished by test");
    // Explicit stop is Cancelled, per task_tools::TaskStopTool and protocol::TaskState.
    assert_eq!(saved[&long]["node"]["state"], "cancelled");
    assert_eq!(saved[&quick]["node"]["state"], "done");
}

// ─────────────────────────────────── exec ─────────────────────────────────

#[test]
fn exec_runs_governed_read_only_compositions_and_refuses_everything_else() {
    let p = sequence(
        vec![
            // JS form: nested read_file via the governed dispatcher.
            call(
                "exec",
                json!({"source":"const body = await tools.read({path: 'a.txt'}); return 'composed:' + body.includes('alpha-body');"}),
            ),
            // A nested failure that the program catches is still reported as a failure.
            call(
                "exec",
                json!({"source":"try { await tools.read({path: 'no-such-file.txt'}); } catch (e) { return 'caught'; }"}),
            ),
            // Mutating capability cannot be reached through the JS bridge.
            call(
                "exec",
                json!({"source":"await tools.write_file({path: 'exec-wrote.txt', content: 'x'}); return 'wrote';"}),
            ),
            // Capability form with a stale/unknown digest is refused before any nested effect.
            call(
                "exec",
                json!({"capability":"tool:read_file","schema_digest":"sha256:stale","args":{"path":"a.txt"}}),
            ),
            // Mixed forms are rejected.
            call(
                "exec",
                json!({"source":"return 1;","capability":"tool:read_file","schema_digest":"x"}),
            ),
        ],
        "done",
    );
    let rig = Rig::with_config(
        &p,
        "\n[tools]\nmodel-interface = \"fixed-v1\"\n",
        &[("a.txt", "alpha-body\n")],
    );
    let out = rig.run("go");
    assert_eq!(out.code, Some(0), "{}", out.diag());
    let all = results_by_id(&out);

    let outer = |n: usize| -> &Res {
        let id = format!("call_{n}_0");
        &all.iter()
            .find(|(i, _)| *i == id)
            .unwrap_or_else(|| panic!("no result for {id}: {all:#?}"))
            .1
    };

    let first = outer(0);
    assert!(!first.error, "{}", first.text);
    assert!(first.text.contains("composed:true"), "{}", first.text);
    // The nested read is its own child call with its own lifecycle receipt.
    assert!(
        all.iter()
            .any(|(id, _)| id.starts_with("call_0_0.exec-nested.")),
        "nested read must be dispatched as a child call: {all:#?}"
    );

    let second = outer(1);
    assert!(second.error, "{}", second.text);
    assert!(
        second.text.contains("nested_call_failed"),
        "{}",
        second.text
    );

    let third = outer(2);
    assert!(third.error, "mutating call must not run: {}", third.text);
    assert!(!rig.exists("exec-wrote.txt"), "exec must stay read-only");

    let fourth = outer(3);
    assert!(fourth.error, "{}", fourth.text);
    assert!(!fourth.text.contains("alpha-body"), "{}", fourth.text);

    let fifth = outer(4);
    assert!(fifth.error, "{}", fifth.text);
    assert!(
        fifth
            .text
            .contains("exactly one of `source` or `capability`"),
        "mixed exec forms must be rejected: {}",
        fifth.text
    );
}

// ───────────────────────────────── process ────────────────────────────────

#[test]
fn process_tool_lists_inspects_and_stops_a_background_process() {
    fn bg_id(text: &str) -> String {
        format!(
            "bg-{}",
            number_after(text, "bg-").expect("a bg-<n> shell id")
        )
    }
    let p = sequence(
        vec![
            call(
                "shell",
                json!({"command":"echo proc-started; sleep 30","run_in_background":true}),
            ),
            // Confined spawn + PowerShell start take ~2 s under suite load.
            sleep_ms(5000),
            call("process", json!({"operation":"list"})),
            call_with(
                "process",
                |s| json!({"operation":"get","process_id": bg_id(&s[0])}),
            ),
            call_with(
                "process",
                |s| json!({"operation":"output","process_id": bg_id(&s[0])}),
            ),
            call_with(
                "process",
                |s| json!({"operation":"stop","process_id": bg_id(&s[0])}),
            ),
            sleep_ms(1500),
            call_with(
                "process",
                |s| json!({"operation":"get","process_id": bg_id(&s[0])}),
            ),
            call(
                "process",
                json!({"operation":"get","process_id":"bg-999999"}),
            ),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    let out = rig.run("go");
    let r = expect_results(&out, 9);
    let id = bg_id(&r[0].text);

    let listed: Value = serde_json::from_str(&r[2].text).expect("list returns JSON");
    let rows = listed["processes"].as_array().expect("processes array");
    let row = rows
        .iter()
        .find(|row| row["processId"] == id.as_str())
        .unwrap_or_else(|| panic!("{id} missing from {listed}"));
    assert_eq!(row["status"], "running");
    assert!(
        row["command"].as_str().unwrap().contains("proc-started"),
        "{row}"
    );

    let got: Value = serde_json::from_str(&r[3].text).expect("get returns JSON");
    assert_eq!(got["processId"], id.as_str());
    assert_eq!(got["status"], "running");

    let output: Value = serde_json::from_str(&r[4].text).expect("output returns JSON");
    assert!(
        output["output"].as_str().unwrap().contains("proc-started"),
        "{output}"
    );

    let stopped: Value = serde_json::from_str(&r[5].text).expect("stop returns JSON");
    assert_eq!(stopped["processId"], id.as_str());
    assert_eq!(stopped["signal"], "stop");

    let after: Value = serde_json::from_str(&r[7].text).expect("get returns JSON");
    assert_eq!(
        after["status"], "killed",
        "stopped process must be reported killed: {after}"
    );

    assert!(r[8].error);
    assert_eq!(
        r[8].text,
        "No daemon-owned process handle 'bg-999999'; it cannot be reattached."
    );
}

// ─────────────────────────────────── repl ─────────────────────────────────

#[test]
fn repl_keeps_state_between_evals_and_reports_errors_without_dying() {
    fn session(text: &str) -> String {
        text.rsplit(": ")
            .next()
            .unwrap_or_default()
            .trim()
            .to_string()
    }
    let p = sequence(
        vec![
            call("repl", json!({"operation":"start","language":"node"})),
            call_with(
                "repl",
                |s| json!({"operation":"eval","session_id": session(&s[0]),"code":"var answer = 40; answer"}),
            ),
            call_with(
                "repl",
                |s| json!({"operation":"eval","session_id": session(&s[0]),"code":"console.log('hello-from-repl'); answer + 2"}),
            ),
            call_with(
                "repl",
                |s| json!({"operation":"eval","session_id": session(&s[0]),"code":"throw new Error('boom-in-repl')"}),
            ),
            call_with(
                "repl",
                |s| json!({"operation":"eval","session_id": session(&s[0]),"code":"answer"}),
            ),
            call_with(
                "repl",
                |s| json!({"operation":"status","session_id": session(&s[0])}),
            ),
            call_with(
                "repl",
                |s| json!({"operation":"reset","session_id": session(&s[0])}),
            ),
            call_with(
                "repl",
                |s| json!({"operation":"close","session_id": session(&s[0])}),
            ),
            call(
                "repl",
                json!({"operation":"eval","session_id":"repl-nope","code":"1"}),
            ),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    let out = rig.run_args_with_resource_profile(
        &["--stream-json", "--bypass-approvals"],
        "go",
        Some("expanded"),
    );
    let r = expect_results(&out, 9);

    assert!(!r[0].error, "{}", r[0].text);
    assert!(
        r[0].text.starts_with("Started node REPL session: "),
        "{}",
        r[0].text
    );
    assert_eq!(r[1].text, "result:\n40");
    assert!(
        r[2].text.contains("stdout:\nhello-from-repl"),
        "{}",
        r[2].text
    );
    assert!(
        r[2].text.contains("result:\n42"),
        "state must persist: {}",
        r[2].text
    );
    assert!(r[3].error);
    assert!(r[3].text.contains("boom-in-repl"), "{}", r[3].text);
    // A thrown error is a correlated round-trip: the session (and its state) survive.
    assert_eq!(r[4].text, "result:\n40");
    assert!(r[5].text.contains("language=node"), "{}", r[5].text);
    assert!(!r[6].error, "{}", r[6].text);
    assert!(
        r[6].text.starts_with("Reset node REPL session: "),
        "{}",
        r[6].text
    );
    assert!(!r[7].error, "{}", r[7].text);
    assert!(r[8].error);
    assert_eq!(r[8].text, "Session 'repl-nope' not found");
}

// ─────────────────────────── worktree_enter / worktree_exit ────────────────

#[test]
fn worktree_enter_and_exit_create_and_remove_a_real_git_worktree() {
    // CG-B02 confines model-facing processes to workspace/granted roots.
    // Keep this Git lifecycle fixture inside its workspace, without widening ACLs.
    let cell = RootCell::new();
    let wt_enter = cell.clone();
    let wt_cat = cell.clone();
    let wt_exit = cell.clone();
    let wt_exit2 = cell.clone();
    let wt_missing = cell.clone();

    let p = sequence(
        vec![
            call("git_write", json!({"operation":"init"})),
            call(
                "write_file",
                json!({"path":"hello.txt","content":"hi-from-main\n"}),
            ),
            call(
                "git_write",
                json!({"operation":"stage","paths":["hello.txt"]}),
            ),
            call(
                "git_write",
                json!({"operation":"commit","message":"add hello"}),
            ),
            call(
                "git_write",
                json!({"operation":"branch_create","branch":"wt-branch"}),
            ),
            call_with(
                "worktree_enter",
                move |_| json!({"branch":"wt-branch","path": forward(&wt_enter.root().join("wt-feature"))}),
            ),
            // The new worktree holds the committed file.
            call_with("shell", move |_| {
                let reader = if cfg!(windows) { "Get-Content" } else { "cat" };
                json!({"command": format!("{reader} '{}'", forward(&wt_cat.root().join("wt-feature/hello.txt")))})
            }),
            call_with(
                "worktree_exit",
                move |_| json!({"path": forward(&wt_exit.root().join("wt-feature"))}),
            ),
            call_with(
                "worktree_exit",
                move |_| json!({"path": forward(&wt_exit2.root().join("wt-feature"))}),
            ),
            call_with(
                "worktree_enter",
                move |_| json!({"branch":"no-such-branch","path": forward(&wt_missing.root().join("wt-missing"))}),
            ),
        ],
        "done",
    );
    let rig = Rig::new_unversioned(&p, &[]);
    cell.bind(&rig);
    let wt = rig.root().join("wt-feature");
    let wt_fwd = forward(&wt);
    let out = rig.run("go");
    let r = expect_results(&out, 10);

    assert!(!r[5].error, "{}", r[5].text);
    assert_eq!(r[5].text, format!("Worktree created at: {wt_fwd}"));
    assert!(!r[6].error, "{}", r[6].text);
    assert_eq!(r[6].text.trim(), "hi-from-main");
    assert!(!r[7].error, "{}", r[7].text);
    assert_eq!(r[7].text, format!("Worktree removed: {wt_fwd}"));
    assert!(!wt.exists(), "worktree directory must be gone after exit");
    assert!(r[8].error);
    assert!(
        r[8].text.starts_with("worktree_exit: git error:"),
        "{}",
        r[8].text
    );
    assert!(r[9].error);
    assert!(
        r[9].text.starts_with("worktree_enter: git error:"),
        "{}",
        r[9].text
    );
    assert!(
        !rig.git_out(&["worktree", "list"]).contains("wt-feature"),
        "worktree registration must be pruned"
    );
}

// ─────────────────────────────── schedule_cron ────────────────────────────

#[test]
fn schedule_cron_registers_lists_and_cancels_durable_schedules() {
    fn schedule_id(text: &str) -> String {
        text.split('\'').nth(1).unwrap_or_default().to_string()
    }
    let p = sequence(
        vec![
            call("schedule_cron", json!({"operation":"list"})),
            call(
                "schedule_cron",
                json!({"operation":"register","cron_expr":"*/5 * * * *","prompt":"nightly check","time_zone":"UTC"}),
            ),
            call("schedule_cron", json!({"operation":"list"})),
            call(
                "schedule_cron",
                json!({"operation":"register","cron_expr":"not a cron","prompt":"x"}),
            ),
            call_with(
                "schedule_cron",
                |s| json!({"operation":"cancel","id": schedule_id(&s[1])}),
            ),
            call("schedule_cron", json!({"operation":"list"})),
            call(
                "schedule_cron",
                json!({"operation":"cancel","id":"schedule_missing"}),
            ),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    let out = rig.run("go");
    let r = expect_results(&out, 7);

    assert_eq!(r[0].text, "No schedules registered");
    assert!(!r[1].error, "{}", r[1].text);
    let id = schedule_id(&r[1].text);
    assert!(id.starts_with("schedule_"), "{}", r[1].text);
    assert_eq!(
        r[1].text,
        format!("Registered schedule '{id}' with cron expression '*/5 * * * *' in UTC")
    );
    assert!(
        r[2].text.starts_with("Registered schedules:\n"),
        "{}",
        r[2].text
    );
    assert!(
        r[2].text
            .contains(&format!("[{id}] */5 * * * * -> 'nightly check'")),
        "{}",
        r[2].text
    );
    assert!(r[3].error);
    assert!(
        r[3].text.starts_with("Invalid cron expression"),
        "{}",
        r[3].text
    );
    assert_eq!(r[4].text, format!("Cancelled schedule '{id}'"));
    assert_eq!(r[5].text, "No schedules registered");
    assert!(r[6].error);
    assert_eq!(r[6].text, "Schedule 'schedule_missing' not found");
}

// ───────────────────────────────── brief ──────────────────────────────────

#[test]
fn brief_condenses_text_to_headings_bullets_and_first_sentences() {
    let doc = "# Release notes\n\nThe parser was rewritten. It is faster now.\n\n- fixed crash on empty input\n* added --json flag\n\n```\nlet hidden = 1;\n```\n\nSecond paragraph starts here! More detail follows.";
    let p = sequence(
        vec![
            call("brief", json!({ "context": doc })),
            call("brief", json!({ "context": "" })),
            call("brief", json!({})),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    let out = rig.run("go");
    let r = expect_results(&out, 3);

    assert_eq!(
        r[0].text,
        "# Release notes\nThe parser was rewritten.\n- fixed crash on empty input\n* added --json flag\nSecond paragraph starts here!"
    );
    assert!(!r[0].text.contains("hidden"), "code blocks are skipped");
    assert!(!r[1].error, "{}", r[1].text);
    assert!(r[2].error);
    assert!(
        r[2].text
            .starts_with("Invalid tool arguments: $.context is required"),
        "{}",
        r[2].text
    );
}

// ────────────────────────────── async_inbox ───────────────────────────────

#[test]
fn async_inbox_reports_empty_and_validates_actions_for_a_cli_session() {
    // Producers are the mobile runtime / deferred-resolution paths; a plain CLI session has none,
    // so the contract exercised here is the empty read, idempotent acknowledge and validation.
    let p = sequence(
        vec![
            call("async_inbox", json!({})),
            call("async_inbox", json!({"action":"acknowledge","throughId":7})),
            call("async_inbox", json!({"action":"acknowledge"})),
            call("async_inbox", json!({"action":"explode"})),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    let out = rig.run("go");
    let r = expect_results(&out, 4);

    assert_eq!(r[0].text, "Inbox empty: no messages pending.");
    let acked: Value = serde_json::from_str(&r[1].text).expect("ack JSON");
    assert_eq!(acked["acknowledged"], 0);
    assert_eq!(acked["throughId"], 7);
    assert_eq!(acked["pendingCount"], 0);
    assert!(r[2].error);
    assert_eq!(
        r[2].text,
        "async_inbox acknowledge requires integer throughId."
    );
    assert!(r[3].error);
    assert!(r[3].text.contains("acknowledge"), "{}", r[3].text);
}
