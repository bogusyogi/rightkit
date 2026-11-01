fn write_task_fixture(root: &std::path::Path) {
    let coderight_dir = root.join(".coderight");
    std::fs::create_dir_all(&coderight_dir).unwrap();
    std::fs::write(
        coderight_dir.join("tasks.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "task-42": {
                "node": {
                    "id": "task-42",
                    "title": "Run verification suite",
                    "deps": [],
                    "doneWhen": "cargo test passes",
                    "doneWhenPredicates": [],
                    "state": "inProgress",
                    "category": "verification",
                    "estimatedDuration": "",
                    "assignedRole": "coder",
                    "escalationPath": "",
                    "openQuestions": [],
                    "createdAt": "t1",
                    "updatedAt": "t2"
                },
                "kind": "shell",
                "command": "cargo test",
                "shellId": "shell-1",
                "output": "running tests\nall green\n"
            }
        }))
        .unwrap(),
    )
    .unwrap();
}

fn write_task_fixture_with_shell(root: &std::path::Path, shell_id: &str, command: &str) {
    let coderight_dir = root.join(".coderight");
    std::fs::create_dir_all(&coderight_dir).unwrap();
    std::fs::write(
        coderight_dir.join("tasks.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "task-live": {
                "node": {
                    "id": "task-live",
                    "title": "Run live shell",
                    "deps": [],
                    "doneWhen": "shell exits",
                    "doneWhenPredicates": [],
                    "state": "inProgress",
                    "category": "verification",
                    "estimatedDuration": "",
                    "assignedRole": "coder",
                    "escalationPath": "",
                    "openQuestions": [],
                    "createdAt": "t1",
                    "updatedAt": "t2"
                },
                "kind": "shell",
                "command": command,
                "shellId": shell_id,
                "output": ""
            }
        }))
        .unwrap(),
    )
    .unwrap();
}

fn write_session_config_fixture(root: &std::path::Path) {
    let coderight_dir = root.join(".coderight");
    std::fs::create_dir_all(&coderight_dir).unwrap();
    std::fs::write(
        coderight_dir.join("config.toml"),
        r#"
[model]
default-model = "workspace-model"
fallback-models = ["workspace-fallback-a", "workspace-fallback-b"]

[permissions]
mode = "accept-edits"

[session]
allowed-tools = ["read_file", "search"]
disallowed-tools = ["shell"]
effort = "high"
thinking-budget-tokens = 8192
"#,
    )
    .unwrap();
}

fn run_git(root: &std::path::Path, args: &[&str]) {
    let output = rightkit_child_process::command_std("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap_or_else(|e| panic!("failed to run git {args:?}: {e}"));
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout:{}\nstderr:{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write_git_diff_fixture(root: &std::path::Path) {
    std::fs::create_dir_all(root).unwrap();
    run_git(root, &["init"]);
    run_git(root, &["config", "user.email", "test@example.com"]);
    run_git(root, &["config", "user.name", "Test User"]);
    std::fs::write(root.join("tracked.txt"), "before\n").unwrap();
    run_git(root, &["add", "tracked.txt"]);
    run_git(root, &["commit", "-m", "initial"]);
    std::fs::write(root.join("tracked.txt"), "after\n").unwrap();
}

fn encode_query_value(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn write_executable_test_plugin(
    dir: &std::path::Path,
    id: &str,
    enabled: bool,
    tools: serde_json::Value,
    commands: serde_json::Value,
) {
    let plugin_dir = dir.join(id);
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("plugin.py"),
        r#"
import json, sys
payload = json.load(sys.stdin)
print(json.dumps({
    "success": True,
    "output": f"{payload['kind']}:{payload['name']}"
}))
"#,
    )
    .unwrap();
    let manifest = serde_json::json!({
        "id": id,
        "name": "Executable Plugin",
        "version": "1.0.0",
        "description": "A subprocess plugin fixture",
        "entry": "plugin.py",
        "provides_tools": tools,
        "provides_commands": commands,
        "sandbox": {
            "isolation": "trustedSubprocess"
        },
        "enabled": enabled
    });
    std::fs::write(
        plugin_dir.join("plugin.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

fn write_plugin_package(source_path: &std::path::Path, package_path: &std::path::Path) {
    if let Some(parent) = package_path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let file = std::fs::File::create(package_path).unwrap();
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    let root_name = source_path.file_name().unwrap();
    builder.append_dir_all(root_name, source_path).unwrap();
    let encoder = builder.into_inner().unwrap();
    encoder.finish().unwrap();
}

fn write_mcp_config(path: &std::path::Path, body: serde_json::Value) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, serde_json::to_string_pretty(&body).unwrap()).unwrap();
}

async fn execute_slash_command(app: axum::Router, input: &str) -> serde_json::Value {
    execute_slash_command_with_context(app, input, None, None).await
}

async fn execute_slash_command_with_token(
    app: axum::Router,
    input: &str,
    token: Option<&str>,
) -> serde_json::Value {
    execute_slash_command_with_context(app, input, token, None).await
}

async fn execute_slash_command_for_session(
    app: axum::Router,
    input: &str,
    session_id: &str,
) -> serde_json::Value {
    execute_slash_command_with_context(app, input, None, Some(session_id)).await
}

async fn execute_slash_command_with_context(
    app: axum::Router,
    input: &str,
    token: Option<&str>,
    session_id: Option<&str>,
) -> serde_json::Value {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/commands/execute")
        .header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let mut body = serde_json::json!({ "input": input });
    if let Some(session_id) = session_id {
        body["sessionId"] = serde_json::json!(session_id);
    }
    let resp = app
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

fn query_encode(value: &str) -> String {
    value
        .bytes()
        .flat_map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => {
                vec![b as char]
            }
            other => format!("%{other:02X}").chars().collect(),
        })
        .collect()
}

#[tokio::test]
async fn live_message_dispatcher_routes_to_target_session() {
    let root = unique_temp_dir("message-history");
    let state = DaemonState::new("").with_message_store_path(root.join("messages.json"));
    state.create_session("s1", None, "/tmp", "Sender", None);
    state.create_session("s2", None, "/tmp", "Target", None);

    let summary = rightkit_agent_tools::coding_tools::MessageDispatcher::dispatch(
        &state,
        rightkit_agent_tools::coding_tools::MessageDispatch {
            from_session_id: "s1".to_string(),
            target_session_id: Some("s2".to_string()),
            channel: "handoff".to_string(),
            message: "please inspect the diff".to_string(),
        },
    )
    .await
    .unwrap();

    assert!(summary.contains("s2"));
    let target = state.get_session("s2").unwrap();
    let transcript = lock_recover(&target.transcript, "session.transcript");
    // Relayed messages land as untrusted, pinned User turns (see
    // submit_relayed_message) — not Assistant output. Mirrors the
    // schedule-delivery test below.
    assert!(transcript.iter().any(|message| matches!(
        message,
        TranscriptMessage::User { content, pinned: true, trust: Some(_), .. }
            if content.contains("Message from s1")
                && content.contains("handoff")
                && content.contains("please inspect the diff")
                && content.contains("context only")
    )));
    // Security invariant: relay content must never arrive as a *trusted*
    // user turn — it always carries an untrusted trust envelope, so no
    // `trust: None` user message may contain it.
    assert!(!transcript.iter().any(|message| matches!(
        message,
        TranscriptMessage::User { content, trust: None, .. }
            if content.contains("please inspect the diff")
    )));

    let history = lock_recover(&state.message_history, "message_history");
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].from_session_id, "s1");
    assert_eq!(history[0].target_session_id.as_deref(), Some("s2"));
    assert_eq!(history[0].channel, "handoff");
    assert_eq!(history[0].delivery_status, "delivered");
    assert_eq!(history[0].delivered_session_ids, vec!["s2"]);
    assert_eq!(history[0].message, "please inspect the diff");

    let reloaded = DaemonState::new("").with_message_store_path(root.join("messages.json"));
    assert_eq!(
        lock_recover(&reloaded.message_history, "message_history").len(),
        1
    );

    let _ = std::fs::remove_dir_all(root);
}
