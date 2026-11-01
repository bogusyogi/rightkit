//! Acceptance scenarios Q1, Q2, Q4 and C6 (plus receipts) against the real `membrane` binary,
//! walked as one user journey.
//!
//! Every process runs against an isolated temp state root, so nothing touches an installed
//! product or the user's folders.

use rightkit_child_process::command_std;
use serde_json::Value;
use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

fn isolate(command: &mut Command, root: &Path) {
    command
        .env("RIGHTKIT_CONTEXT_STATE_ROOT", root.join("state"))
        .env("RIGHTKIT_CONTEXT_CONFIG_ROOT", root.join("config"))
        .env("RIGHTKIT_CONTEXT_DATA_ROOT", root.join("data"))
        .env("RIGHTKIT_CONTEXT_CACHE_ROOT", root.join("cache"))
        .env("RIGHTKIT_CONTEXT_LOG_ROOT", root.join("log"))
        .env("HOME", root)
        .env("USERPROFILE", root)
        .env("APPDATA", root)
        .env("LOCALAPPDATA", root);
}

fn membrane(root: &Path) -> Command {
    let mut command = command_std(env!("CARGO_BIN_EXE_membrane"));
    isolate(&mut command, root);
    command
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Kills the child on drop so a failed assertion never leaves a process behind.
struct Reaper(Child);

impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A running `membrane serve` and the write end of its stdin pipe, which stands in for the host app:
/// the resident lives while this holds the pipe and exits within 2 s of it closing.
struct Served {
    child: Reaper,
    pipe: Option<std::process::ChildStdin>,
}

fn spawn_serve(root: &Path) -> Served {
    let mut command = membrane(root);
    command
        .arg("serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command.spawn().expect("spawn membrane serve");
    let pipe = child.stdin.take();
    Served {
        child: Reaper(child),
        pipe,
    }
}

fn wait_until(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let started = Instant::now();
    while started.elapsed() < limit {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    done()
}

fn wait_resident(root: &Path, serve: &mut Served) {
    let resident = root.join("state").join("resident.json");
    let appeared = wait_until(Duration::from_secs(20), || {
        if let Ok(Some(status)) = serve.child.0.try_wait() {
            panic!("serve exited early: {status}");
        }
        resident.is_file()
    });
    assert!(appeared, "resident.json did not appear within 20 s");
}

/// Run a command that must return within `limit`; a command that hangs fails the test.
fn run_bounded(command: Command, limit: Duration) -> Output {
    run_bounded_with_stdin(command, Stdio::null(), limit)
}

/// [`run_bounded`] with the child's stdin given (a held pipe for `serve`).
fn run_bounded_with_stdin(mut command: Command, stdin: Stdio, limit: Duration) -> Output {
    let mut child = command
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn membrane");
    let finished = wait_until(limit, || matches!(child.try_wait(), Ok(Some(_))));
    if !finished {
        let _ = child.kill();
        let _ = child.wait();
        panic!("command did not return within {limit:?}");
    }
    child.wait_with_output().expect("collect output")
}

fn hook(root: &Path, event: &str) -> Output {
    let mut command = membrane(root);
    command
        .args(["hook", "--wire", "claude", "--event", event])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn membrane hook");
    let payload = serde_json::json!({
        "session_id": "acceptance",
        "cwd": root,
        "hook_event_name": event,
    });
    {
        let mut stdin = child.stdin.take().unwrap();
        let _ = stdin.write_all(payload.to_string().as_bytes());
    }
    let finished = wait_until(Duration::from_secs(15), || {
        matches!(child.try_wait(), Ok(Some(_)))
    });
    if !finished {
        let _ = child.kill();
        let _ = child.wait();
        panic!("hook {event} did not return within 15 s");
    }
    child.wait_with_output().expect("collect hook output")
}

fn list_receipts(root: &Path) -> Vec<Value> {
    let mut command = membrane(root);
    command.args(["receipts", "list", "--json"]);
    let out = run_bounded(command, Duration::from_secs(10));
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    serde_json::from_slice(&out.stdout).expect("receipts list is a JSON array")
}

fn seed_receipt(root: &Path, session: &str, id: &str) {
    let directory = root.join("state").join("receipts").join(session);
    std::fs::create_dir_all(&directory).unwrap();
    let receipt = serde_json::json!({
        "id": id,
        "session": session,
        "harness": "claude",
        "event": "UserPromptSubmit",
        "state": "emitted",
        "ts": 1,
    });
    std::fs::write(directory.join(format!("{id}.json")), receipt.to_string()).unwrap();
}

fn confirm(root: &Path, session: &str, transcript: &Path) {
    let mut command = membrane(root);
    command
        .args([
            "receipts",
            "confirm",
            "--wire",
            "claude",
            "--session",
            session,
        ])
        .arg("--transcript")
        .arg(transcript);
    let out = run_bounded(command, Duration::from_secs(10));
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(out.stdout.is_empty(), "stdout: {}", text(&out.stdout));
}

fn one_shot(root: &Path, args: &[&str], limit: Duration) -> Output {
    let mut command = membrane(root);
    command.args(args);
    run_bounded(command, limit)
}

/// The one JSON document a `--json` command printed, with its `schema` checked.
fn document(out: &Output, schema: &str, what: &str) -> Value {
    assert_eq!(out.status.code(), Some(0), "{what}: {}", text(&out.stderr));
    let value: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|error| {
        panic!("{what} is one JSON object ({error}): {}", text(&out.stdout))
    });
    assert_eq!(value["schema"], schema, "{what}: {value}");
    value
}

#[test]
fn membrane_resident_and_one_shot_journey() {
    let root = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let limit = Duration::from_secs(10);

    // Phase 1 (Q2, G4): with no resident, commands answer in process; the CLI contract's exit
    // codes hold: 3 for no store, 2 for a usage error, 4 for a required resident that is absent.
    let out = one_shot(root.path(), &["memory", "list", "--json"], limit);
    assert_eq!(
        out.status.code(),
        Some(3),
        "phase 1 memory list before any store: {}",
        text(&out.stderr)
    );
    let out = one_shot(root.path(), &["context", "pull", "--no-such-flag"], limit);
    assert_eq!(out.status.code(), Some(2), "phase 1 usage error");
    let out = one_shot(root.path(), &["memory", "push"], limit);
    assert_eq!(out.status.code(), Some(2), "phase 1 push without text");
    let out = one_shot(
        root.path(),
        &["status", "--json", "--require-resident"],
        limit,
    );
    assert_eq!(out.status.code(), Some(4), "phase 1 required resident");
    let out = one_shot(root.path(), &["status", "--json"], limit);
    let value = document(&out, "membrane.status.v1", "phase 1 status");
    assert_ne!(
        value["resident"]["running"],
        Value::Bool(true),
        "phase 1 status: {value}"
    );
    let mut blueprint = membrane(root.path());
    blueprint
        .args(["blueprint", "status"])
        .current_dir(repo.path());
    let out = run_bounded(blueprint, limit);
    assert_eq!(
        out.status.code(),
        Some(0),
        "phase 1 blueprint: {}",
        text(&out.stderr)
    );
    let value: Value = serde_json::from_slice(&out.stdout).expect("blueprint is one JSON object");
    assert_eq!(
        value["schema"], "membrane.blueprint.v1",
        "phase 1 blueprint"
    );
    let mut pull = membrane(root.path());
    pull.args(["context", "pull", "--cwd"]).arg(repo.path());
    let out = run_bounded(pull, limit);
    assert_eq!(
        out.status.code(),
        Some(0),
        "phase 1 pull: {}",
        text(&out.stderr)
    );
    assert!(
        text(&out.stdout).trim().is_empty(),
        "phase 1 pull on a fresh store: {}",
        text(&out.stdout)
    );
    let mut pull = membrane(root.path());
    pull.args(["context", "pull", "--json", "--cwd"])
        .arg(repo.path());
    let out = run_bounded(pull, limit);
    let value = document(&out, "membrane.context.pull.v1", "phase 1 pull --json");
    assert!(value["context"].is_null(), "phase 1 pull --json: {value}");

    // Phase 2: memory push writes a memory, and context pull returns it.
    let out = one_shot(
        root.path(),
        &[
            "memory",
            "push",
            "the acceptance journey remembers pineapple",
        ],
        limit,
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "phase 2 memory push: {}",
        text(&out.stderr)
    );
    let mut pull = membrane(root.path());
    pull.args(["context", "pull", "--prompt", "pineapple", "--cwd"])
        .arg(repo.path());
    let out = run_bounded(pull, limit);
    assert_eq!(
        out.status.code(),
        Some(0),
        "phase 2 pull: {}",
        text(&out.stderr)
    );
    assert!(
        text(&out.stdout).contains("pineapple"),
        "phase 2 pull did not return the memory: {}",
        text(&out.stdout)
    );

    let out = one_shot(root.path(), &["memory", "list", "--json"], limit);
    let value = document(&out, "membrane.memory.list.v1", "phase 2 memory list");
    assert!(
        value["items"]
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        "phase 2 memory list: {value}"
    );
    let out = one_shot(
        root.path(),
        &["memory", "list", "--json", "--limit", "0"],
        limit,
    );
    assert_eq!(out.status.code(), Some(2), "phase 2 memory list --limit 0");
    let mut pull = membrane(root.path());
    pull.args([
        "context",
        "pull",
        "--json",
        "--prompt",
        "pineapple",
        "--cwd",
    ])
    .arg(repo.path());
    let out = run_bounded(pull, limit);
    let value = document(&out, "membrane.context.pull.v1", "phase 2 pull --json");
    assert!(
        value["context"]
            .as_str()
            .is_some_and(|context| context.contains("pineapple")),
        "phase 2 pull --json: {value}"
    );

    // Phase 3 (C6): hooks exit 0 and follow their schema.
    let mut marker_id: Option<String> = None;
    let start = hook(root.path(), "SessionStart");
    assert_eq!(
        start.status.code(),
        Some(0),
        "phase 3 SessionStart: {}",
        text(&start.stderr)
    );
    let stdout = text(&start.stdout);
    if !stdout.trim().is_empty() {
        let value: Value = serde_json::from_str(stdout.trim()).expect("stdout is valid JSON");
        assert_eq!(
            value["hookSpecificOutput"]["hookEventName"], "SessionStart",
            "phase 3: {value}"
        );
        // Context carries its receipt marker as the last line, and a matching `emitted` receipt.
        let context = value["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .expect("additionalContext is text");
        assert!(
            context.chars().count() <= 10_000,
            "phase 3: context too long"
        );
        let marker = context.lines().last().unwrap();
        let id = marker
            .strip_prefix("[membrane:")
            .and_then(|rest| rest.strip_suffix(']'))
            .unwrap_or_else(|| panic!("phase 3: last line is not a marker: {marker}"));
        let receipts = list_receipts(root.path());
        let found = receipts
            .iter()
            .find(|receipt| receipt["id"] == id)
            .unwrap_or_else(|| panic!("phase 3: no receipt {id} in {receipts:?}"));
        assert_eq!(found["state"], "emitted", "phase 3");
        assert_eq!(found["session"], "acceptance", "phase 3");
        assert_eq!(found["event"], "SessionStart", "phase 3");
        marker_id = Some(id.to_owned());
    }
    let end = hook(root.path(), "SessionEnd");
    assert_eq!(
        end.status.code(),
        Some(0),
        "phase 3 SessionEnd: {}",
        text(&end.stderr)
    );
    assert!(
        end.stdout.is_empty(),
        "phase 3 SessionEnd stdout: {}",
        text(&end.stdout)
    );

    // Phase 4: receipts confirm flips only the receipts whose marker is in the transcript.
    let matched = marker_id.unwrap_or_else(|| {
        seed_receipt(root.path(), "acceptance", "aaaa1111");
        "aaaa1111".to_owned()
    });
    seed_receipt(root.path(), "acceptance", "bbbb2222");
    let transcript = root.path().join("transcript.jsonl");
    std::fs::write(
        &transcript,
        format!(
            "{}\n{}\n",
            r#"{"type":"user","content":"hello"}"#,
            serde_json::json!({
                "type": "attachment",
                "content": format!("orientation\n[membrane:{matched}]"),
            })
        ),
    )
    .unwrap();
    confirm(root.path(), "acceptance", &transcript);
    let states: Vec<(String, String)> = list_receipts(root.path())
        .iter()
        .map(|receipt| {
            (
                receipt["id"].as_str().unwrap().to_owned(),
                receipt["state"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert!(
        states.contains(&(matched.clone(), "delivered".into())),
        "phase 4: {states:?}"
    );
    assert!(
        states.contains(&("bbbb2222".into(), "emitted".into())),
        "phase 4: {states:?}"
    );
    // A missing transcript is a miss, not a failure.
    confirm(root.path(), "acceptance", &root.path().join("absent.jsonl"));

    // Phase 5 (Q4): a resident starts, a second serve reports it, and a query is answered.
    let mut serve = spawn_serve(root.path());
    wait_resident(root.path(), &mut serve);
    // G7: the resident records the absolute path of the binary that runs it.
    let record: Value = serde_json::from_slice(
        &std::fs::read(root.path().join("state").join("resident.json"))
            .expect("read resident.json"),
    )
    .expect("resident.json is JSON");
    let recorded = Path::new(
        record["binary"]
            .as_str()
            .expect("resident.json has a binary"),
    );
    assert!(recorded.is_absolute(), "phase 5 binary: {record}");
    assert_eq!(
        std::fs::canonicalize(recorded).expect("recorded binary exists"),
        std::fs::canonicalize(env!("CARGO_BIN_EXE_membrane")).unwrap(),
        "phase 5 binary: {record}"
    );
    let out = one_shot(
        root.path(),
        &["status", "--json", "--require-resident"],
        limit,
    );
    let value = document(&out, "membrane.status.v1", "phase 5 status");
    assert_eq!(value["mode"], "resident", "phase 5 status: {value}");
    let mut command = membrane(root.path());
    command.arg("serve");
    let second = run_bounded_with_stdin(command, Stdio::piped(), limit);
    assert_eq!(
        second.status.code(),
        Some(0),
        "phase 5 second serve: {}",
        text(&second.stderr)
    );
    assert!(
        text(&second.stdout).contains("already serving"),
        "phase 5 stdout: {}",
        text(&second.stdout)
    );
    let mut pull = membrane(root.path());
    pull.args(["context", "pull", "--prompt", "pineapple", "--cwd"])
        .arg(repo.path());
    let out = run_bounded(pull, limit);
    assert_eq!(
        out.status.code(),
        Some(0),
        "phase 5 pull with resident: {}",
        text(&out.stderr)
    );
    assert!(
        text(&out.stdout).contains("pineapple"),
        "phase 5 pull with resident did not return the memory: {}",
        text(&out.stdout)
    );

    // Phase 6 (Q1): the resident exits within 2 s of its parent.
    drop(serve.pipe.take());
    let exited = wait_until(Duration::from_secs(2), || {
        matches!(serve.child.0.try_wait(), Ok(Some(_)))
    });
    assert!(
        exited,
        "phase 6: serve outlived its parent by more than 2 s"
    );

    // Phase 7: one-shot commands still answer after the resident is gone.
    let out = one_shot(root.path(), &["status", "--json"], limit);
    assert_eq!(
        out.status.code(),
        Some(0),
        "phase 7 status: {}",
        text(&out.stderr)
    );
    let value: Value = serde_json::from_slice(&out.stdout).expect("status is one JSON object");
    assert_ne!(
        value["resident"]["running"],
        Value::Bool(true),
        "phase 7 status: {value}"
    );
    assert_eq!(value["schema"], "membrane.status.v1", "phase 7 status");
    let out = one_shot(
        root.path(),
        &["status", "--json", "--require-resident"],
        limit,
    );
    assert_eq!(out.status.code(), Some(4), "phase 7 required resident");
}
