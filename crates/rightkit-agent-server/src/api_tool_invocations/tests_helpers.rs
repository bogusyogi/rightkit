use super::*;
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;

async fn json_get(app: Router, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or(Value::Null)
    };
    (status, json)
}

async fn json_post(app: Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or(Value::Null)
    };
    (status, json)
}

fn publish_read_file_lifecycle(state: &DaemonState) {
    state.create_session("s-tools", None, "/tmp", "Tools", None);
    let live = state.get_session("s-tools").unwrap();
    let call = ToolCall {
        id: "turn-1:tc-read".to_string(),
        name: "read_file".to_string(),
        arguments_json: r#"{"path":"Cargo.toml","api_key":"secret"}"#.to_string(),
    };
    live.conduit.publish(ProtocolEvent::ToolCallStarted {
        session_id: "s-tools".to_string(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T00:00:00Z".to_string(),
        call,
    });
    live.conduit.publish(ProtocolEvent::ToolResultAdded {
        session_id: "s-tools".to_string(),
        seq: live.conduit.next_seq(),
        created_at: "2026-06-08T00:00:01Z".to_string(),
        result: TranscriptMessage::Tool {
            id: "tr-1".to_string(),
            created_at: "2026-06-08T00:00:01Z".to_string(),
            tool_call_id: "turn-1:tc-read".to_string(),
            summary: "read ok".to_string(),
            output: None,
            trust: None,
            is_error: false,
            process_id: None,
            pinned: false,
        },
    });
}
