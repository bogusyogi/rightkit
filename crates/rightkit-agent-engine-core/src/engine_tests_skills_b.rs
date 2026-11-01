use super::*;
use crate::test_skill_catalog::install_test_skill_catalog;

#[tokio::test]
async fn pre_tool_use_skill_effect_allowed_tools_blocks_unlisted_tool() {
    install_test_skill_catalog();
    let tmp = tempfile::tempdir().unwrap();
    let skills_dir = tmp.path().join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
            skills_dir.join("reader.md"),
            "+++\nname = \"reader\"\ndescription = \"Read-only mode.\"\ntrigger = \"event == \\\"PreToolUse\\\"\"\nenabled = true\n[effects]\nallowedTools = [\"read_file\"]\n+++\n\n# Reader\n",
        )
        .unwrap();
    let call = ToolCall {
        id: "tc1".into(),
        name: "shell".into(),
        arguments_json: serde_json::json!({"command": "echo should-not-run"}).to_string(),
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

    e.submit_user_message("run command").await.unwrap();

    let denied = e.transcript().messages().iter().any(|m| {
        matches!(
            m,
            TranscriptMessage::Tool {
                summary,
                is_error: true,
                ..
            } if summary.contains("skill effect denied tool 'shell'")
                && summary.contains("allow only read_file")
        )
    });
    assert!(denied, "{:#?}", e.transcript().messages());
}

#[tokio::test]
async fn skill_allowed_tools_cannot_expand_session_tool_ceiling_and_records_evidence() {
    install_test_skill_catalog();
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("visible.txt"), "safe read\n").unwrap();
    let skills_dir = tmp.path().join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
            skills_dir.join("expander.md"),
            "+++\nname = \"expander\"\ndescription = \"Tries to expand tools.\"\ntrigger = \"event == \\\"PreToolUse\\\"\"\nenabled = true\n[effects]\nallowedTools = [\"read_file\", \"shell\"]\n+++\n\n# Expander\n",
        )
        .unwrap();
    let call = ToolCall {
        id: "tc1".into(),
        name: "read_file".into(),
        arguments_json: serde_json::json!({"path": "visible.txt"}).to_string(),
    };
    let llm = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::default().with_tool_call(call),
        LlmResponse::text("done"),
    ]));
    let tools = Arc::new(ToolRegistry::build_for_allowed(&["read_file".into()]));
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        workspace_root: tmp.path().to_path_buf(),
        skill_dirs: vec![skills_dir],
        ..Default::default()
    };
    let mut e = SessionEngine::new(llm, tools, conduit.clone(), cfg);

    e.submit_user_message("read").await.unwrap();

    let read_succeeded = e.transcript().messages().iter().any(|m| {
        matches!(
            m,
            TranscriptMessage::Tool {
                summary,
                is_error: false,
                ..
            } if summary.contains("safe read")
        )
    });
    assert!(read_succeeded, "{:#?}", e.transcript().messages());

    let observation = conduit
        .events_after(0)
        .into_iter()
        .find_map(|event| match event {
            ProtocolEvent::OrchestrationObservationRecorded { observation, .. }
                if observation.kind == "skill_toolset_ceiling" =>
            {
                Some(observation)
            }
            _ => None,
        })
        .expect("skill ceiling observation");
    assert_eq!(
        observation.fields.get("skill").map(String::as_str),
        Some("expander")
    );
    assert_eq!(
        observation
            .fields
            .get("requestedToolsOutsideCeiling")
            .map(String::as_str),
        Some("shell")
    );
    assert_eq!(
        observation.fields.get("allowedTools").map(String::as_str),
        Some("read_file,shell")
    );
}

#[tokio::test]
async fn pre_tool_use_skill_effect_disallows_readonly_batch_tool_execution() {
    install_test_skill_catalog();
    let tmp = tempfile::tempdir().unwrap();
    let skills_dir = tmp.path().join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(tmp.path().join("secret.txt"), "do not read\n").unwrap();
    std::fs::write(
            skills_dir.join("privacy.md"),
            "+++\nname = \"privacy\"\ndescription = \"Block reads.\"\ntrigger = \"event == \\\"PreToolUse\\\" and tool == \\\"read_file\\\"\"\nenabled = true\n[effects]\ndisallowedTools = [\"read_file\"]\n+++\n\n# Privacy\n",
        )
        .unwrap();
    let call = ToolCall {
        id: "tc1".into(),
        name: "read_file".into(),
        arguments_json: serde_json::json!({"path": "secret.txt"}).to_string(),
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

    e.submit_user_message("read").await.unwrap();

    let denied = e.transcript().messages().iter().any(|m| {
        matches!(
            m,
            TranscriptMessage::Tool {
                summary,
                is_error: true,
                ..
            } if summary.contains("skill effect denied tool 'read_file'")
                && summary.contains("privacy")
                && !summary.contains("do not read")
        )
    });
    assert!(denied, "{:#?}", e.transcript().messages());
}

#[tokio::test]
async fn hooks_fire_around_tool_use() {
    install_test_skill_catalog();
    let call = ToolCall {
        id: "tc1".into(),
        name: "shell".into(),
        arguments_json: serde_json::json!({"command": "echo hi"}).to_string(),
    };
    let (mut e, _c) = engine_with(
        vec![
            LlmResponse::default().with_tool_call(call),
            LlmResponse::text("done"),
        ],
        PermissionMode::Bypass,
    );
    let bus = rightkit_hooks::agent_hooks::HookBus::new(64);
    let mut sub = bus.subscribe();
    e.set_hook_bus(bus);
    e.submit_user_message("go").await.unwrap();

    // Drain the bus (sender stays alive in the engine, so recv would block â€”
    // use a short timeout to stop once events are exhausted).
    let mut names = Vec::new();
    while let Ok(Some(ev)) =
        tokio::time::timeout(std::time::Duration::from_millis(50), sub.recv()).await
    {
        names.push(ev.name().to_string());
    }
    assert!(names.contains(&"UserPromptSubmit".to_string()), "{names:?}");
    assert!(names.contains(&"PreToolUse".to_string()), "{names:?}");
    assert!(names.contains(&"PostToolUse".to_string()), "{names:?}");
}

#[tokio::test]
async fn pre_tool_use_exit_2_blocks_tool_with_stdout_reason() {
    install_test_skill_catalog();
    struct EnvGuard(&'static str);
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            std::env::remove_var(self.0);
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let call = ToolCall {
        id: "tc-hook-deny".into(),
        name: "write_file".into(),
        arguments_json: serde_json::json!({
            "path": "blocked.txt",
            "content": "should not be written"
        })
        .to_string(),
    };
    let llm = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::default().with_tool_call(call),
        LlmResponse::text("done"),
    ]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        session_id: "pre-tool-deny-test".into(),
        permission_mode: PermissionMode::Bypass,
        workspace_root: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let mut engine = SessionEngine::new(llm, tools, conduit, cfg);
    engine.set_hook_bus(rightkit_hooks::agent_hooks::HookBus::new(16));

    let cmd = if cfg!(windows) {
        r#"if "%RIGHTKIT_HOOK_SESSION%"=="pre-tool-deny-test" (echo blocked by hook & exit /b 2) else (exit /b 0)"#
    } else {
        r#"if [ "$RIGHTKIT_HOOK_SESSION" = "pre-tool-deny-test" ]; then echo blocked by hook; exit 2; else exit 0; fi"#
    };
    // Hook env vars are process-wide; share the hook-test lock so another
    // test's hooks (e.g. a blocking Stop) cannot leak into this run.
    let _env_lock = super::engine_tests_basic::eff002_hook_env_guard().await;
    std::env::set_var(
        "RIGHTKIT_HOOKS_JSON",
        serde_json::json!({"PreToolUse": [{"command": cmd}]}).to_string(),
    );
    let _guard = EnvGuard("RIGHTKIT_HOOKS_JSON");

    let out = engine.submit_user_message("write").await.unwrap();

    assert_eq!(out, "done");
    assert!(!tmp.path().join("blocked.txt").exists());
    let denied = engine.transcript().messages().iter().any(|message| {
        matches!(
            message,
            TranscriptMessage::Tool {
                tool_call_id,
                summary,
                is_error: true,
                ..
            } if tool_call_id == "tc-hook-deny" && summary == "blocked by hook"
        )
    });
    assert!(denied, "{:#?}", engine.transcript().messages());
}
