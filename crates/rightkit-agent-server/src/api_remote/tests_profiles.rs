#[cfg(test)]
mod tests_profiles {
    use super::*;
    #[test]
    fn remote_binding_target_key_is_opaque_and_redacted() {
        let opaque = opaque_remote_target_key("telegram", "123456789");
        assert!(opaque.starts_with("sha256:"));
        assert!(!opaque.contains("123456789"));

        let redacted = redacted_remote_target("telegram", &opaque);
        assert!(redacted.starts_with("telegram:target-"));
        assert!(!redacted.contains("123456789"));
    }

    #[tokio::test]
    async fn remote_channel_profile_write_persists_sql_override_into_snapshot() {
        use axum::body::Body;
        use axum::http::Request;
        use http_body_util::BodyExt;
        use std::sync::Arc;
        use tower::ServiceExt;

        let storage = Arc::new(tokio::sync::Mutex::new(
            rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
        ));
        let state = DaemonState::new("").with_storage(storage);
        let app = crate::api::build_router(state);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/remote/channels/webhook/profile")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&serde_json::json!({
                            "profileId": "ops-webhook",
                            "toolset": "safe-notify",
                            "deliveryPolicy": "manual-approval",
                            "trust": "remoteAgentUntrusted",
                            "enabled": true
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let saved: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(saved["ok"], true);
        assert_eq!(saved["channel"]["profile"]["profileId"], "ops-webhook");

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
        let webhook = snapshot["channels"]
            .as_array()
            .unwrap()
            .iter()
            .find(|channel| channel["id"] == "webhook")
            .expect("webhook channel");
        assert_eq!(webhook["profile"]["profileId"], "ops-webhook");
        assert_eq!(webhook["profile"]["toolset"], "safe-notify");
        assert_eq!(webhook["profile"]["deliveryPolicy"], "manual-approval");
        assert!(webhook["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|note| note.as_str().unwrap_or("").contains("SQL override")));
        assert!(snapshot["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .any(|profile| profile["id"] == "ops-webhook" && profile["channels"][0] == "webhook"));
    }
}
