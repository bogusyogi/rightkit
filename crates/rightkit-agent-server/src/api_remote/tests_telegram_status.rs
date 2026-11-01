#[cfg(test)]
mod tests_telegram_status {
    use super::*;
    #[tokio::test]
    async fn remote_management_routes_reject_lower_trust_sources() {
        use axum::body::Body;
        use axum::http::{Method, Request};
        use tower::ServiceExt;

        let app = crate::api::build_router(DaemonState::new(""));
        let cases = [
            (
                Method::POST,
                "/remote/bindings",
                serde_json::json!({
                    "channelId": "telegram",
                    "targetKey": "123",
                    "sessionId": "session-1"
                }),
            ),
            (
                Method::POST,
                "/remote/channels/webhook/profile",
                serde_json::json!({
                    "profileId": "ops-webhook",
                    "toolset": "safe-notify"
                }),
            ),
            (
                Method::POST,
                "/remote/channels/webhook/outbound",
                serde_json::json!({
                    "text": "do not send",
                    "targets": [{
                        "target": "https://example.com/hook",
                        "allowPrivateNetwork": false
                    }]
                }),
            ),
        ];

        for (method, uri, body) in cases {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .header("content-type", "application/json")
                        .header("x-coderight-source-trust", "toolUntrusted")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{uri}");
        }
    }

    #[test]
    fn telegram_users_payload_omits_message_bodies() {
        let payload = telegram_users_payload(&serde_json::json!({
            "ok": true,
            "result": [{
                "update_id": 42,
                "message": {
                    "text": "do not leak this",
                    "from": {
                        "id": 123456,
                        "username": "operator",
                        "first_name": "Ada"
                    },
                    "chat": {
                        "id": 789,
                        "type": "private"
                    }
                }
            }]
        }));

        assert_eq!(payload["verdict"], "pass");
        assert_eq!(payload["userCount"], 1);
        assert_eq!(payload["users"][0]["userId"], 123456);
        assert_eq!(payload["users"][0]["username"], "operator");
        assert!(!serde_json::to_string(&payload)
            .unwrap()
            .contains("do not leak this"));
    }

    #[test]
    fn telegram_users_payload_reports_empty_updates_as_warn() {
        let payload = telegram_users_payload(&serde_json::json!({
            "ok": true,
            "result": []
        }));

        assert_eq!(payload["verdict"], "warn");
        assert_eq!(payload["userCount"], 0);
    }

    #[test]
    fn telegram_user_discovery_token_honors_env_override() {
        let mut env = std::collections::HashMap::new();
        env.insert(
            "RIGHTKIT_TELEGRAM_TOKEN".to_string(),
            " env-token ".to_string(),
        );

        assert_eq!(
            resolve_configured_telegram_token(&env).as_deref(),
            Some("env-token")
        );
    }

    #[test]
    fn telegram_users_query_rejects_api_base_override() {
        let query: Result<TelegramUsersQuery, _> =
            serde_json::from_value(serde_json::json!({ "limit": 10 }));
        assert!(query.is_ok());

        let override_query: Result<TelegramUsersQuery, _> =
            serde_json::from_value(serde_json::json!({ "apiBase": "http://127.0.0.1:9" }));
        assert!(
            override_query.is_err(),
            "daemon user discovery must not accept a caller-controlled Telegram API base"
        );
    }

    #[test]
    fn telegram_status_payload_prefers_sql_count_when_positive() {
        // Write 3 JSON bindings to a temp file. The SQL store (in-memory
        // placeholder) is simulated by the `sql_binding_count` argument.
        let dir = std::env::temp_dir().join(format!(
            "coderight-telegram-sql-pref-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bindings.json");
        let json_bindings = rightkit_bot_messaging::TelegramSessionBindings {
            chats: [
                ("a".to_string(), "s1".to_string()),
                ("b".to_string(), "s2".to_string()),
                ("c".to_string(), "s3".to_string()),
            ]
            .into_iter()
            .collect(),
        };
        rightkit_bot_messaging::save_telegram_session_bindings(&path, &json_bindings).unwrap();

        let mut state = DaemonState::new("");
        state.telegram_status.binding_store_path = path.clone();

        // SQL has 5, JSON has 3 → SQL wins.
        let payload = telegram_status_payload(&state, Some(5));
        assert_eq!(
            payload["bindingCount"], 5,
            "SQL count should win when >0; full payload: {payload}"
        );

        // SQL has 0, JSON has 3 → JSON fallback.
        let payload = telegram_status_payload(&state, Some(0));
        assert_eq!(
            payload["bindingCount"], 3,
            "Some(0) should fall back to JSON"
        );

        // SQL is None, JSON has 3 → JSON fallback.
        let payload = telegram_status_payload(&state, None);
        assert_eq!(payload["bindingCount"], 3, "None should fall back to JSON");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
