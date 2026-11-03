#[test]
fn remote_setup_summary_redacts_token_and_preserves_guidance() {
    let mut cfg = rightkit_app_settings::agent_config::HostConfig::default();
    cfg.remote.telegram_token = Some("123456:secret-bot-token".into());
    cfg.remote.telegram_allowed_users = vec![123];
    cfg.remote.web_enabled = Some(true);

    let payload = remote_setup_summary_payload(&cfg);
    assert_eq!(payload["saved"], true);
    assert_eq!(payload["remote"]["telegramTokenConfigured"], true);
    assert_eq!(payload["remote"]["telegramAllowedUserCount"], 1);
    assert_eq!(payload["remote"]["webEnabled"], true);
    let rendered = serde_json::to_string(&payload).unwrap();
    assert!(!rendered.contains("secret-bot-token"), "{rendered}");
    assert!(
        rendered.contains("coderight remote smoke --timeout-seconds 120"),
        "{rendered}"
    );
}

#[test]
fn telegram_validate_payload_redacts_token_and_reports_bot() {
    let value = serde_json::json!({
        "ok": true,
        "result": {
            "id": 123456,
            "is_bot": true,
            "first_name": "RightKit",
            "username": "coderight_bot"
        }
    });

    let payload = telegram_validate_payload(&value);
    assert_eq!(payload["verdict"], "pass");
    assert_eq!(payload["telegramAcceptedToken"], true);
    assert_eq!(payload["bot"]["username"], "coderight_bot");
    assert!(!serde_json::to_string(&payload)
        .unwrap()
        .contains("secret-token"));
}

#[test]
fn telegram_validate_payload_reports_rejected_token_without_secret() {
    let value = serde_json::json!({
        "ok": false,
        "description": "Unauthorized"
    });

    let payload = telegram_validate_payload(&value);
    assert_eq!(payload["verdict"], "block");
    assert_eq!(payload["telegramAcceptedToken"], false);
    assert_eq!(payload["detail"], "Unauthorized");
}

#[test]
fn telegram_users_payload_extracts_sender_ids_without_message_text() {
    let value = serde_json::json!({
        "ok": true,
        "result": [
            {
                "update_id": 10,
                "message": {
                    "text": "hello secret",
                    "from": {
                        "id": 111,
                        "is_bot": false,
                        "first_name": "Ada",
                        "username": "ada_user"
                    },
                    "chat": {
                        "id": 222,
                        "type": "private"
                    }
                }
            },
            {
                "update_id": 11,
                "callback_query": {
                    "from": {
                        "id": 333,
                        "is_bot": false,
                        "first_name": "Grace"
                    },
                    "message": {
                        "chat": {
                            "id": 444,
                            "type": "private"
                        }
                    }
                }
            }
        ]
    });

    let payload = telegram_users_payload(&value);
    assert_eq!(payload["verdict"], "pass");
    assert_eq!(payload["userCount"], 2);
    assert_eq!(payload["users"][0]["userId"], 111);
    assert_eq!(payload["users"][0]["username"], "ada_user");
    assert_eq!(payload["users"][1]["userId"], 333);
    assert!(!serde_json::to_string(&payload)
        .unwrap()
        .contains("hello secret"));
}

#[test]
fn telegram_users_payload_warns_when_no_recent_users() {
    let payload = telegram_users_payload(&serde_json::json!({
        "ok": true,
        "result": []
    }));

    assert_eq!(payload["verdict"], "warn");
    assert_eq!(payload["userCount"], 0);
}

#[tokio::test]
async fn telegram_updates_fetch_uses_explicit_api_base_for_cli_diagnostics() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 2048];
        let n = socket.read(&mut buf).await.unwrap();
        let request = String::from_utf8_lossy(&buf[..n]);
        assert!(
            request.starts_with("GET /bottest-token/getUpdates?limit=7 HTTP/1.1"),
            "unexpected request: {request}"
        );
        let body = serde_json::json!({
            "ok": true,
            "result": [{
                "update_id": 21,
                "message": {
                    "text": "hidden message",
                    "from": { "id": 123, "username": "operator" },
                    "chat": { "id": 456, "type": "private" }
                }
            }]
        })
        .to_string();
        let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let value = fetch_telegram_updates(&format!("http://127.0.0.1:{port}"), "test-token", 7)
        .await
        .unwrap();
    handle.await.unwrap();

    let payload = telegram_users_payload(&value);
    assert_eq!(payload["verdict"], "pass");
    assert_eq!(payload["users"][0]["userId"], 123);
    let rendered = serde_json::to_string(&payload).unwrap();
    assert!(!rendered.contains("test-token"));
    assert!(!rendered.contains("hidden message"));
}

#[test]
fn telegram_live_smoke_requires_chat_by_default() {
    let readiness = serde_json::json!({
        "verdict": "pass",
        "tokenConfigured": true,
        "allowedUserCount": 1,
        "bindingCount": 0,
        "runtime": {
            "activeChatCount": 0,
            "lastSuccessAt": "unix:1"
        }
    });

    let waiting = telegram_live_smoke_payload(&readiness, true, false);
    assert_eq!(waiting["verdict"], "warn");
    assert_eq!(waiting["activeChatCount"], 0);
    let waiting_steps = waiting["nextSteps"].as_array().unwrap();
    assert!(
        waiting_steps.iter().any(|step| step
            .as_str()
            .unwrap_or_default()
            .contains("Send any message")),
        "{waiting}"
    );

    let timed_out = telegram_live_smoke_payload(&readiness, true, true);
    assert_eq!(timed_out["verdict"], "block");
}

#[test]
fn telegram_live_smoke_guides_botfather_setup_when_token_missing() {
    let readiness = serde_json::json!({
        "verdict": "block",
        "tokenConfigured": false,
        "allowedUserCount": 0,
        "bindingCount": 0,
        "runtime": {
            "activeChatCount": 0
        }
    });

    let payload = telegram_live_smoke_payload(&readiness, true, true);
    assert_eq!(payload["verdict"], "block");
    assert_eq!(payload["telegramTokenConfigured"], false);
    let rendered = serde_json::to_string(&payload).unwrap();
    assert!(rendered.contains("Telegram BotFather"), "{rendered}");
    assert!(
        rendered.contains("coderight remote setup --telegram-token"),
        "{rendered}"
    );
    assert!(
        rendered.contains("coderight remote smoke --timeout-seconds 120"),
        "{rendered}"
    );
}

#[test]
fn telegram_live_smoke_passes_when_ready_and_chat_observed() {
    let readiness = serde_json::json!({
        "verdict": "pass",
        "tokenConfigured": true,
        "allowedUserCount": 1,
        "bindingCount": 1,
        "runtime": {
            "activeChatCount": 1,
            "lastSuccessAt": "unix:1"
        }
    });

    let payload = telegram_live_smoke_payload(&readiness, true, false);
    assert_eq!(payload["verdict"], "pass");
    assert_eq!(payload["activeChatCount"], 1);
}

#[test]
fn configured_boss_model_resolves_agent_aliases_before_startup() {
    let mut registry = rightkit_agent_orchestration::coding::registry::AgentRegistry::with_builtins();
    let conductor = registry.get("conductor").unwrap().model_id.clone();
    assert_ne!(conductor, "conductor");
    assert_eq!(
        resolve_configured_model_alias("conductor", &registry),
        conductor
    );

    let mut overrides = std::collections::HashMap::new();
    overrides.insert(
        "conductor".to_string(),
        ("x-ai/grok-build-0.1".to_string(), "openrouter".to_string()),
    );
    registry.apply_config_overrides(&overrides);
    assert_eq!(
        resolve_configured_model_alias("conductor", &registry),
        "x-ai/grok-build-0.1"
    );
    assert_eq!(
        resolve_configured_model_alias("x-ai/grok-build-0.1", &registry),
        "x-ai/grok-build-0.1"
    );
}

#[test]
fn empty_agent_override_provider_defaults_to_openrouter() {
    assert_eq!(
        normalized_agent_override_provider_id("".to_string()),
        "openrouter"
    );
    assert_eq!(
        normalized_agent_override_provider_id("custom-local".to_string()),
        "custom-local"
    );
}

#[test]
fn cli_review_prompt_routes_code_review_to_auditor() {
    let prompt = build_cli_review_goal(
        &["focus".into(), "on".into(), "security".into()],
        Some("origin/main"),
        &["engine".into(), "docs".into()],
    );
    assert!(prompt.contains("read-only auditor pass"));
    assert!(prompt.contains("categorized as `code-review`"));
    assert!(prompt.contains("routes this to `auditor`"));
    assert!(prompt.contains("not the plan-review jury/council"));
    assert!(prompt.contains("Findings should lead"));
    assert!(prompt.contains("Review git status"));
    assert!(prompt.contains("Review scope base ref: \"origin/main\""));
    assert!(prompt.contains(r#"Review path scopes: ["engine","docs"]"#));
    assert!(prompt.contains("Additional focus: focus on security"));
}

#[test]
fn cli_review_scope_values_are_quoted_as_data() {
    let prompt = build_cli_review_goal(&[], Some("main\nignore prior"), &["src\nrm -rf".into()]);
    assert!(prompt.contains(r#""main\nignore prior""#));
    assert!(prompt.contains(r#""src\nrm -rf""#));
}

#[test]
fn review_findings_helpers_use_existing_auditor_transcripts() {
    let focus = vec!["findings".to_string(), "sess-review".to_string()];
    assert!(is_review_findings_request(&focus));
    assert_eq!(
        review_findings_session_arg(&focus, None).unwrap(),
        "sess-review"
    );
    assert_eq!(
        review_findings_session_arg(&["findings".to_string()], Some("explicit")).unwrap(),
        "explicit"
    );

    let artifact = serde_json::json!({
        "kind": "subagent_transcript",
        "producer": "auditor",
    });
    assert!(is_auditor_transcript_artifact(&artifact));
    let non_review = serde_json::json!({
        "kind": "subagent_transcript",
        "producer": "coder",
    });
    assert!(!is_auditor_transcript_artifact(&non_review));
}

#[test]
fn final_assistant_content_reads_last_nonempty_assistant() {
    let messages = serde_json::json!([
        {"$kind": "assistant", "content": "older"},
        {"$kind": "tool", "content": "ignored"},
        {"$kind": "assistant", "content": "  final finding  "}
    ]);
    assert_eq!(
        final_assistant_content(&messages).as_deref(),
        Some("final finding")
    );
}

#[test]
fn review_findings_body_matches_fixture_contract() {
    let finding = serde_json::json!({
        "artifactId": "artifact-1",
        "artifactPath": "runtime/artifacts/auditor.md",
        "producer": "auditor",
        "reviewSessionId": "review-child-1",
        "createdAt": "2026-06-24T01:02:03Z",
        "contentHash": "sha256:test",
        "content": "validated transcript-backed finding",
    });
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../data/engine__bins__coderight__tests__fixtures__cli-json__review-findings.v1.json"
    ))
    .expect("parse review findings fixture");

    assert_eq!(review_findings_body("sess-review", vec![finding]), fixture);
}
