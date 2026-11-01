use super::*;

#[tokio::test]
async fn invoke_skill_uses_configured_skill_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let skills_dir = tmp.path().join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(skills_dir.join("reviewer.md"), "# Reviewer\n\nReview code.").unwrap();
    let call = ToolCall {
        id: "tc1".into(),
        name: "invoke_skill".into(),
        arguments_json: serde_json::json!({"skill": "reviewer"}).to_string(),
    };
    let llm = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::default().with_tool_call(call),
        LlmResponse::text("used skill"),
    ]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        workspace_root: tmp.path().to_path_buf(),
        skill_dirs: vec![skills_dir],
        ..Default::default()
    };
    let mut e = SessionEngine::new(llm, tools, conduit, cfg);

    let out = e.submit_user_message("use reviewer").await.unwrap();
    assert_eq!(out, "used skill");
    let skill_result = e.transcript().messages().iter().any(|m| {
        matches!(
            m,
            TranscriptMessage::Tool {
                summary,
                is_error: false,
                ..
            } if summary.contains("# Reviewer")
        )
    });
    assert!(skill_result);
}

#[tokio::test]
async fn user_message_triggered_skill_hint_is_added_to_transcript() {
    let tmp = tempfile::tempdir().unwrap();
    let skills_dir = tmp.path().join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
            skills_dir.join("reviewer.md"),
            "+++\nname = \"reviewer\"\ndescription = \"Review code when users ask for review.\"\ntrigger = \"event == \\\"UserPromptSubmit\\\" and message contains \\\"review\\\"\"\nenabled = true\n[effects]\nexecutionMode = \"review\"\nallowedTools = [\"read_file\", \"git_diff\"]\ndisallowedTools = [\"write_file\"]\nprompt = \"Inspect before editing.\"\n+++\n\n# Reviewer\n\nFull body should not autoload.",
        )
        .unwrap();
    let llm = Arc::new(ScriptedLlm::new(vec![LlmResponse::text("ok")]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        workspace_root: tmp.path().to_path_buf(),
        skill_dirs: vec![skills_dir],
        ..Default::default()
    };
    let mut e = SessionEngine::new(llm, tools, conduit, cfg);

    e.submit_user_message("please review this diff")
        .await
        .unwrap();

    let hint = e.transcript().messages().iter().any(|m| {
        matches!(
            m,
            TranscriptMessage::System { content, .. }
                if content.contains("Triggered local skills")
                    && content.contains("reviewer")
                    && content.contains("invoke_skill")
                    && content.contains("executionMode=review")
                    && content.contains("allowedTools=read_file,git_diff")
                    && content.contains("disallowedTools=write_file")
                    && content.contains("Inspect before editing.")
                    && !content.contains("Full body should not autoload")
        )
    });
    assert!(hint, "{:#?}", e.transcript().messages());
}

#[tokio::test]
async fn pre_tool_use_file_triggered_skill_hint_is_added_to_transcript() {
    let tmp = tempfile::tempdir().unwrap();
    let skills_dir = tmp.path().join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
            skills_dir.join("rust.md"),
            "+++\nname = \"rust\"\ndescription = \"Use for Rust files.\"\ntrigger = \"event == \\\"PreToolUse\\\" and file_path matches \\\"*.rs\\\"\"\nenabled = true\n+++\n\n# Rust\n\nFull body should not autoload.",
        )
        .unwrap();
    std::fs::write(tmp.path().join("main.rs"), "fn main() {}\n").unwrap();
    let call = ToolCall {
        id: "tc1".into(),
        name: "read_file".into(),
        arguments_json: serde_json::json!({"path": "main.rs"}).to_string(),
    };
    let llm = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::default().with_tool_call(call),
        LlmResponse::text("done"),
    ]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        workspace_root: tmp.path().to_path_buf(),
        skill_dirs: vec![skills_dir],
        ..Default::default()
    };
    let mut e = SessionEngine::new(llm, tools, conduit, cfg);

    e.submit_user_message("read rust").await.unwrap();

    let hint = e.transcript().messages().iter().any(|m| {
        matches!(
            m,
            TranscriptMessage::System { content, .. }
                if content.contains("Triggered local skills")
                    && content.contains("rust")
                    && content.contains("PreToolUse")
                    && !content.contains("Full body should not autoload")
        )
    });
    assert!(hint, "{:#?}", e.transcript().messages());
}

#[tokio::test]
async fn pre_tool_use_path_affinity_skill_hint_is_added_to_transcript() {
    let tmp = tempfile::tempdir().unwrap();
    let skills_dir = tmp.path().join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
            skills_dir.join("rust.md"),
            "---\nname: rust\ndescription: Use for Rust files.\npathAffinity: [\"*.rs\"]\n---\n\n# Rust\n\nFull body should not autoload.",
        )
        .unwrap();
    std::fs::write(tmp.path().join("main.rs"), "fn main() {}\n").unwrap();
    let call = ToolCall {
        id: "tc1".into(),
        name: "read_file".into(),
        arguments_json: serde_json::json!({"path": "main.rs"}).to_string(),
    };
    let llm = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::default().with_tool_call(call),
        LlmResponse::text("done"),
    ]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        workspace_root: tmp.path().to_path_buf(),
        skill_dirs: vec![skills_dir],
        ..Default::default()
    };
    let mut e = SessionEngine::new(llm, tools, conduit, cfg);

    e.submit_user_message("read rust").await.unwrap();

    let hint = e.transcript().messages().iter().any(|m| {
        matches!(
            m,
            TranscriptMessage::System { content, .. }
                if content.contains("Triggered local skills")
                    && content.contains("rust")
                    && content.contains("path affinity: *.rs")
                    && !content.contains("Full body should not autoload")
        )
    });
    assert!(hint, "{:#?}", e.transcript().messages());
}

#[tokio::test]
async fn model_invocation_disabled_skill_does_not_auto_hint_or_enforce() {
    let tmp = tempfile::tempdir().unwrap();
    let skills_dir = tmp.path().join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
            skills_dir.join("deploy.md"),
            "---\nname: deploy\ndescription: Deploy production.\ntrigger: event == \"PreToolUse\"\ndisable-model-invocation: true\ndisallowedTools: [\"write_file\"]\n---\n\n# Deploy\n",
        )
        .unwrap();
    let call = ToolCall {
        id: "tc1".into(),
        name: "write_file".into(),
        arguments_json: serde_json::json!({"path": "allowed.txt", "content": "ok"}).to_string(),
    };
    let llm = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::default().with_tool_call(call),
        LlmResponse::text("done"),
    ]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        workspace_root: tmp.path().to_path_buf(),
        skill_dirs: vec![skills_dir],
        // One scripted write + one answer: skip the completion-time
        // verification recheck, which is not what this test exercises.
        auto_post_edit_diagnostics: false,
        ..Default::default()
    };
    let mut e = SessionEngine::new(llm, tools, conduit, cfg);

    e.submit_user_message("write").await.unwrap();

    assert!(tmp.path().join("allowed.txt").exists());
    let auto_hint_or_denial = e.transcript().messages().iter().any(|m| match m {
        TranscriptMessage::System { content, .. } => content.contains("deploy"),
        TranscriptMessage::Tool { summary, .. } => summary.contains("skill effect denied"),
        _ => false,
    });
    assert!(!auto_hint_or_denial, "{:#?}", e.transcript().messages());
}

#[tokio::test]
async fn pre_tool_use_skill_effect_disallows_mutating_tool_execution() {
    let tmp = tempfile::tempdir().unwrap();
    let skills_dir = tmp.path().join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
            skills_dir.join("reviewer.md"),
            "+++\nname = \"reviewer\"\ndescription = \"Review before editing.\"\ntrigger = \"event == \\\"PreToolUse\\\"\"\nenabled = true\n[effects]\ndisallowedTools = [\"write_file\"]\n+++\n\n# Reviewer\n",
        )
        .unwrap();
    let call = ToolCall {
        id: "tc1".into(),
        name: "write_file".into(),
        arguments_json: serde_json::json!({"path": "blocked.txt", "content": "nope"}).to_string(),
    };
    let llm = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::default().with_tool_call(call),
        LlmResponse::text("done"),
    ]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        workspace_root: tmp.path().to_path_buf(),
        skill_dirs: vec![skills_dir],
        ..Default::default()
    };
    let mut e = SessionEngine::new(llm, tools, conduit, cfg);

    e.submit_user_message("edit").await.unwrap();

    assert!(!tmp.path().join("blocked.txt").exists());
    let denied = e.transcript().messages().iter().any(|m| {
        matches!(
            m,
            TranscriptMessage::Tool {
                summary,
                is_error: true,
                ..
            } if summary.contains("skill effect denied tool 'write_file'")
                && summary.contains("reviewer")
        )
    });
    assert!(denied, "{:#?}", e.transcript().messages());
}
