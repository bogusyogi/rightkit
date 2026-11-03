#[cfg(test)]
mod tests_fanout {
    use super::*;
    #[tokio::test(flavor = "multi_thread")]
    async fn remote_channel_fanout_submits_untrusted_message_to_bound_sessions() {
        use axum::body::Body;
        use axum::http::Request;
        use http_body_util::BodyExt;
        use rightkit_agent_engine_protocol::{ContentTrust, ProtocolEvent, TranscriptMessage};
        use std::sync::Arc;
        use tower::ServiceExt;

        let storage = Arc::new(tokio::sync::Mutex::new(
            rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
        ));
        let state = DaemonState::new("").with_storage(storage.clone());
        state.create_session("s-remote", None, "", "Remote session", None);
        state.create_session("s-other", None, "", "Other remote session", None);
        {
            let store = storage.lock().await;
            store
                .upsert_remote_channel(&rightkit_agent_engine_storage::RemoteChannelRow {
                    id: "telegram".to_string(),
                    kind: "chat".to_string(),
                    profile_id: "default".to_string(),
                    enabled: true,
                    configured: true,
                    toolset: "telegram-remote".to_string(),
                    delivery_policy: "fanout-bound-sessions".to_string(),
                    trust: "remoteAgentUntrusted".to_string(),
                    state: "ready".to_string(),
                    last_success_at: None,
                    last_error_at: None,
                    last_error: None,
                    updated_at: "unix:1".to_string(),
                })
                .unwrap();
            store
                .upsert_remote_binding(&rightkit_agent_engine_storage::RemoteBindingRow {
                    id: "telegram-target-1".to_string(),
                    channel_id: "telegram".to_string(),
                    target_key: opaque_remote_target_key("telegram", "chat-abc"),
                    session_id: Some("s-remote".to_string()),
                    target_label: Some("telegram:target-abc".to_string()),
                    target_redacted: Some("telegram:target-abc".to_string()),
                    status: "active".to_string(),
                    created_at: "unix:1".to_string(),
                    updated_at: "unix:2".to_string(),
                })
                .unwrap();
            store
                .upsert_remote_binding(&rightkit_agent_engine_storage::RemoteBindingRow {
                    id: "telegram-target-2".to_string(),
                    channel_id: "telegram".to_string(),
                    target_key: opaque_remote_target_key("telegram", "other-chat"),
                    session_id: Some("s-other".to_string()),
                    target_label: Some("telegram:target-other".to_string()),
                    target_redacted: Some("telegram:target-other".to_string()),
                    status: "active".to_string(),
                    created_at: "unix:1".to_string(),
                    updated_at: "unix:2".to_string(),
                })
                .unwrap();
        }
        let live = state.get_session("s-remote").unwrap();
        let other_live = state.get_session("s-other").unwrap();
        let app = crate::api::build_router(state);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/remote/channels/telegram/fanout")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "text": "remote operator says review status",
                            "source": "telegram-test",
                            "targetKey": "chat-abc",
                            "sessionId": "s-remote"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let fanout: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(fanout["ok"], true);
        assert_eq!(fanout["attempted"], 1);
        assert_eq!(fanout["delivered"], 1);
        assert_eq!(
            fanout["results"][0]["targetRedacted"],
            "telegram:target-abc"
        );
        assert_eq!(other_live.transcript.lock().unwrap().len(), 0);

        let snapshot_response = app
            .oneshot(
                Request::builder()
                    .uri("/remote/channels")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(snapshot_response.status(), axum::http::StatusCode::OK);
        let body = snapshot_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let snapshot: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let telegram = snapshot["channels"]
            .as_array()
            .unwrap()
            .iter()
            .find(|channel| channel["id"] == "telegram")
            .expect("telegram channel");
        let recent = telegram["recentDeliveries"].as_array().unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0]["bindingId"], "telegram-target-1");
        assert_eq!(recent[0]["targetRedacted"], "telegram:target-abc");
        assert_eq!(recent[0]["deliveryKind"], "inbound_session_message");
        assert_eq!(recent[0]["status"], "delivered");
        assert!(!serde_json::to_string(&recent[0])
            .unwrap()
            .contains("remote operator says review status"));

        let transcript = live.transcript.lock().unwrap().clone();
        let remote_user = transcript
            .iter()
            .find_map(|message| match message {
                TranscriptMessage::User { content, trust, .. }
                    if content.contains("remote operator says review status") =>
                {
                    trust.clone()
                }
                _ => None,
            })
            .expect("remote user message should be recorded");
        assert_eq!(remote_user.trust, ContentTrust::RemoteAgentUntrusted);
        assert!(remote_user.instructions_are_data);
        assert!(remote_user.source.contains("remote.channel.telegram"));

        // Durable events become visible only after the asynchronous commit gate.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let events = live.conduit.events_after(0);
            if events.iter().any(|event| {
                matches!(
                    event,
                    ProtocolEvent::UserMessageSubmitted {
                        text,
                        trust: Some(trust),
                        ..
                    } if text.contains("remote operator says review status")
                        && trust.trust == ContentTrust::RemoteAgentUntrusted
                        && trust.instructions_are_data
                )
            }) {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "trusted remote event was not committed"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let store = storage.lock().await;
        let deliveries = store
            .list_remote_deliveries_for_channel("telegram", 10)
            .unwrap();
        assert_eq!(deliveries.len(), 1);
        assert_eq!(
            deliveries[0].binding_id.as_deref(),
            Some("telegram-target-1")
        );
        assert_eq!(deliveries[0].status, "delivered");
        assert_eq!(deliveries[0].delivery_kind, "inbound_session_message");
        assert!(!deliveries[0].target_redacted.contains("sha256:target"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn remote_channel_fanout_rejects_lower_trust_source_header() {
        use axum::body::Body;
        use axum::http::Request;
        use http_body_util::BodyExt;
        use std::sync::Arc;
        use tower::ServiceExt;

        let storage = Arc::new(tokio::sync::Mutex::new(
            rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
        ));
        let state = DaemonState::new("").with_storage(storage.clone());
        state.create_session("s-remote", None, "", "Remote session", None);
        {
            let store = storage.lock().await;
            store
                .upsert_remote_channel(&rightkit_agent_engine_storage::RemoteChannelRow {
                    id: "telegram".to_string(),
                    kind: "chat".to_string(),
                    profile_id: "default".to_string(),
                    enabled: true,
                    configured: true,
                    toolset: "telegram-remote".to_string(),
                    delivery_policy: "fanout-bound-sessions".to_string(),
                    trust: "remoteAgentUntrusted".to_string(),
                    state: "ready".to_string(),
                    last_success_at: None,
                    last_error_at: None,
                    last_error: None,
                    updated_at: "unix:1".to_string(),
                })
                .unwrap();
            store
                .upsert_remote_binding(&rightkit_agent_engine_storage::RemoteBindingRow {
                    id: "telegram-target-1".to_string(),
                    channel_id: "telegram".to_string(),
                    target_key: opaque_remote_target_key("telegram", "chat-abc"),
                    session_id: Some("s-remote".to_string()),
                    target_label: Some("telegram:target-abc".to_string()),
                    target_redacted: Some("telegram:target-abc".to_string()),
                    status: "active".to_string(),
                    created_at: "unix:1".to_string(),
                    updated_at: "unix:2".to_string(),
                })
                .unwrap();
        }
        let live = state.get_session("s-remote").unwrap();
        let app = crate::api::build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/remote/channels/telegram/fanout")
                    .header("content-type", "application/json")
                    .header("x-coderight-source-trust", "untrusted")
                    .body(Body::from(
                        serde_json::json!({
                            "text": "attacker-controlled fanout attempt",
                            "source": "telegram-test",
                            "targetKey": "chat-abc",
                            "sessionId": "s-remote"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::FORBIDDEN);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let rejected: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(rejected["ok"], false);

        // The rejected request must never reach the session transcript or event stream.
        assert_eq!(live.transcript.lock().unwrap().len(), 0);
        assert!(live.conduit.events_after(0).is_empty());

        let store = storage.lock().await;
        let deliveries = store
            .list_remote_deliveries_for_channel("telegram", 10)
            .unwrap();
        assert!(deliveries.is_empty());
    }
}
