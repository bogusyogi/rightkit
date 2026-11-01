#[cfg(test)]
mod tests_outbound_ntfy {
    use super::*;
    #[tokio::test(flavor = "multi_thread")]
    async fn remote_channel_outbound_posts_to_ntfy_targets_with_channel_delivery_kind() {
        use axum::body::Body;
        use axum::http::Request;
        use http_body_util::BodyExt;
        use std::sync::{Arc, Mutex};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tower::ServiceExt;

        let received = Arc::new(Mutex::new(String::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let received_for_server = received.clone();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = vec![0u8; 4096];
            let n = socket.read(&mut buffer).await.unwrap();
            *received_for_server.lock().unwrap() =
                String::from_utf8_lossy(&buffer[..n]).to_string();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        });

        let storage = Arc::new(tokio::sync::Mutex::new(
            rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
        ));
        let state = DaemonState::new("").with_storage(storage.clone());
        let app = crate::api::build_router(state);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/remote/channels/ntfy/outbound")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "text": "ntfy outbound status ready",
                            "source": "route-test",
                            "sessionId": "session-ntfy",
                            "targets": [{
                                "target": format!("http://{addr}/ops-topic?token=secret"),
                                "allowPrivateNetwork": true
                            }]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        server.await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let outbound: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(outbound["ok"], true);
        assert_eq!(outbound["channelId"], "ntfy");
        assert_eq!(outbound["results"][0]["deliveryKind"], "outbound-ntfy");
        assert!(outbound["results"][0]["targetRedacted"]
            .as_str()
            .unwrap()
            .ends_with("/ops-topic"));
        assert!(!outbound["results"][0]["targetRedacted"]
            .as_str()
            .unwrap()
            .contains("secret"));
        assert!(received
            .lock()
            .unwrap()
            .contains("ntfy outbound status ready"));

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
        let ntfy = snapshot["channels"]
            .as_array()
            .unwrap()
            .iter()
            .find(|channel| channel["id"] == "ntfy")
            .expect("ntfy channel");
        let recent = ntfy["recentDeliveries"].as_array().unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0]["deliveryKind"], "outbound-ntfy");
        assert_eq!(recent[0]["status"], "delivered");
        assert!(!serde_json::to_string(&recent[0])
            .unwrap()
            .contains("ntfy outbound status ready"));

        let rows = storage
            .lock()
            .await
            .list_remote_deliveries_for_channel("ntfy", 10)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].delivery_kind, "outbound-ntfy");
        assert_eq!(rows[0].status, "delivered");
        assert!(!rows[0].target_redacted.contains("secret"));
    }
}
