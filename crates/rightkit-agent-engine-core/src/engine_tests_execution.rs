use super::*;

#[tokio::test]
async fn execution_backend_reaches_shell_tool_context() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("nonlocal-engine-shell-ran.txt");
    let command = if cfg!(windows) {
        "cmd /C echo nonlocal-engine-shell-ran>nonlocal-engine-shell-ran.txt"
    } else {
        "printf nonlocal-engine-shell-ran > nonlocal-engine-shell-ran.txt"
    };
    let call = ToolCall {
        id: "tc-backend".into(),
        name: "shell".into(),
        arguments_json: serde_json::json!({"command": command}).to_string(),
    };
    let llm = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::default().with_tool_call(call),
        LlmResponse::text("finished"),
    ]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        workspace_root: tmp.path().to_path_buf(),
        permission_mode: PermissionMode::Bypass,
        execution_backend: Some("missing-backend".to_string()),
        ..Default::default()
    };
    let mut engine = SessionEngine::new(llm, tools, conduit, cfg);

    let out = engine.submit_user_message("run echo").await.unwrap();
    assert_eq!(out, "finished");
    assert!(
        !marker.exists(),
        "non-local engine backend must not execute shell on the host"
    );
    let tool_result = engine
        .transcript()
        .messages()
        .iter()
        .find_map(|message| match message {
            TranscriptMessage::Tool {
                tool_call_id,
                summary,
                ..
            } if tool_call_id == "tc-backend" => Some(summary),
            _ => None,
        })
        .expect("shell tool result");
    assert!(
        tool_result.contains("shell executionBackend 'missing-backend' is not available")
            && tool_result.contains("supported backends: local"),
        "{tool_result}"
    );
}

#[tokio::test]
async fn execution_backend_reaches_task_create_command_context() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("nonlocal-engine-task-ran.txt");
    let command = if cfg!(windows) {
        "Set-Content -Path nonlocal-engine-task-ran.txt -Value nonlocal-engine-task-ran"
    } else {
        "printf nonlocal-engine-task-ran > nonlocal-engine-task-ran.txt"
    };
    let call = ToolCall {
        id: "tc-task-backend".into(),
        name: "task_create".into(),
        arguments_json: serde_json::json!({
            "title": "backend task",
            "command": command
        })
        .to_string(),
    };
    let llm = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::default().with_tool_call(call),
        LlmResponse::text("finished"),
    ]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        workspace_root: tmp.path().to_path_buf(),
        permission_mode: PermissionMode::Bypass,
        execution_backend: Some("missing-backend".to_string()),
        ..Default::default()
    };
    let mut engine = SessionEngine::new(llm, tools, conduit, cfg);

    let out = engine.submit_user_message("run task").await.unwrap();
    assert_eq!(out, "finished");
    assert!(
        !marker.exists(),
        "non-local engine backend must not execute task_create command on the host"
    );
    let tool_result = engine
        .transcript()
        .messages()
        .iter()
        .find_map(|message| match message {
            TranscriptMessage::Tool {
                tool_call_id,
                summary,
                ..
            } if tool_call_id == "tc-task-backend" => Some(summary),
            _ => None,
        })
        .expect("task_create tool result");
    assert!(
        tool_result.contains("task_create command executionBackend 'missing-backend'"),
        "{tool_result}"
    );
    assert!(
        tool_result.contains("shell(run_in_background=true)"),
        "{tool_result}"
    );
}

#[tokio::test]
async fn execution_network_profile_reaches_shell_tool_context() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("restricted-engine-shell-ran.txt");
    let command = if cfg!(windows) {
        "cmd /C echo restricted-engine-shell-ran>restricted-engine-shell-ran.txt"
    } else {
        "printf restricted-engine-shell-ran > restricted-engine-shell-ran.txt"
    };
    let call = ToolCall {
        id: "tc-network".into(),
        name: "shell".into(),
        arguments_json: serde_json::json!({"command": command}).to_string(),
    };
    let llm = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::default().with_tool_call(call),
        LlmResponse::text("finished"),
    ]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        workspace_root: tmp.path().to_path_buf(),
        permission_mode: PermissionMode::Bypass,
        execution_backend: Some("local".to_string()),
        execution_network_profile: Some("egress-restricted".to_string()),
        ..Default::default()
    };
    let mut engine = SessionEngine::new(llm, tools, conduit, cfg);

    let out = engine.submit_user_message("run echo").await.unwrap();
    assert_eq!(out, "finished");
    assert!(
        !marker.exists(),
        "restricted engine network profile must not execute shell on the host"
    );
    let tool_result = engine
        .transcript()
        .messages()
        .iter()
        .find_map(|message| match message {
            TranscriptMessage::Tool {
                tool_call_id,
                summary,
                ..
            } if tool_call_id == "tc-network" => Some(summary),
            _ => None,
        })
        .expect("shell tool result");
    assert!(
        tool_result.contains("shell networkProfile 'egress-restricted'"),
        "{tool_result}"
    );
}

/// CQ-07: an oversized current-turn result is externalized at record time —
/// the transcript carries the bounded head+tail preview and a retrievable
/// reference, not the full payload.
#[tokio::test]
async fn large_current_turn_tool_output_externalizes_with_reference() {
    let large_output = (0..300)
        .map(|i| format!("line-{i:03}: compiler diagnostic detail"))
        .collect::<Vec<_>>()
        .join("\n");
    let call = ToolCall {
        id: "tc-large".into(),
        name: "synthetic_output".into(),
        arguments_json: serde_json::json!({"output": large_output}).to_string(),
    };
    let tmp = tempfile::tempdir().unwrap();
    let llm = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::default().with_tool_call(call),
        LlmResponse::text("finished"),
    ]));
    let tools = Arc::new(ToolRegistry::build_default_for_evaluation());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        workspace_root: tmp.path().to_path_buf(),
        permission_mode: PermissionMode::Bypass,
        ..Default::default()
    };
    let mut engine = SessionEngine::new(llm, tools, conduit, cfg);

    let out = engine
        .submit_user_message("emit large output")
        .await
        .unwrap();
    assert_eq!(out, "finished");

    let tool_message = engine
        .transcript()
        .messages()
        .iter()
        .find_map(|m| match m {
            TranscriptMessage::Tool {
                tool_call_id,
                summary,
                output,
                ..
            } if tool_call_id == "tc-large" => Some((summary, output)),
            _ => None,
        })
        .expect("large tool result should be recorded");

    let (summary, output) = tool_message;
    let output = output.as_ref().expect("large output should have OutputRef");
    assert_eq!(output.kind, "tool_output");
    assert!(summary.contains("[Large tool output externalized]"));
    assert!(
        summary.contains("line-000"),
        "head must survive the preview"
    );
    assert!(
        summary.contains("line-299"),
        "tail must survive the preview"
    );
    assert!(
        !summary.contains("line-150"),
        "the elided middle must stay out of the model projection"
    );
    assert!(output.retrievable);
    assert!(output.truncated);
    assert_eq!(output.size_bytes, large_output.len() as i64);
    // Execution data: the full payload stays retrievable behind the ref.
    let retrieved = engine.tool_results.get(&output.id);
    assert_eq!(retrieved.as_deref(), Some(large_output.as_str()));
}
