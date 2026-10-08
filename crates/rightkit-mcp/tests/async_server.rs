use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use rightkit_mcp::{
    Admission, AdmissionHook, AdmissionRequest, AsyncRequestHandler, RequestContext,
    ResponseLimits, Result, ServerHost, ToolProvider,
};
use serde_json::{json, Value};

struct Empty;
impl ToolProvider for Empty {
    fn list(&self) -> Vec<rightkit_mcp::ToolDescriptor> {
        vec![]
    }
    fn call(&self, name: &str, _args: Value) -> Result<rightkit_mcp::ToolResult> {
        Err(rightkit_mcp::McpError::ToolNotFound(name.into()))
    }
}

struct AsyncEcho;
impl AsyncRequestHandler for AsyncEcho {
    fn handle<'a>(
        &'a self,
        request: &'a Value,
        _context: &'a RequestContext,
    ) -> Pin<Box<dyn Future<Output = Option<Result<Value>>> + Send + 'a>> {
        Box::pin(async move {
            (request.get("method").and_then(Value::as_str) == Some("tools/call"))
                .then(|| Ok(json!({"idSeen": request.get("id"), "ok": true})))
        })
    }
}

#[tokio::test]
async fn async_handler_preserves_string_and_large_numeric_ids() {
    let host =
        ServerHost::new("test", "1", Arc::new(Empty)).with_async_handler(Arc::new(AsyncEcho));
    for id in [json!("request-id"), json!(u64::MAX)] {
        let request =
            json!({"jsonrpc":"2.0", "id":id, "method":"tools/call", "params":{"name":"x"}});
        let reply = host.dispatch_async(&request).await.unwrap();
        assert_eq!(reply["id"], id);
        assert_eq!(reply["result"]["idSeen"], id);
    }
    #[cfg(feature = "exact-json-ids")]
    {
        // Parsing & writing retain numeric spelling, including exponent notation &
        // integers outside IEEE-754/u64 precision, without numeric reconstruction.
        for spelling in ["1e+2", "90071992547409931234567890", "-9223372036854775808"] {
            let request: Value = serde_json::from_str(&format!(
                "{{\"id\":{spelling},\"method\":\"tools/call\",\"params\":{{\"name\":\"x\"}}}}"
            ))
            .unwrap();
            let reply = host.dispatch_async(&request).await.unwrap();
            assert_eq!(reply["id"].to_string(), spelling);
        }
    }
}

struct SlowFast;
impl AsyncRequestHandler for SlowFast {
    fn handle<'a>(
        &'a self,
        request: &'a Value,
        _context: &'a RequestContext,
    ) -> Pin<Box<dyn Future<Output = Option<Result<Value>>> + Send + 'a>> {
        Box::pin(async move {
            let id = request.get("id").cloned().unwrap_or(Value::Null);
            if id.as_str() == Some("slow") {
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            }
            Some(Ok(json!({"idSeen":id})))
        })
    }
}

#[tokio::test]
async fn batch_async_dispatch_runs_independently_and_preserves_ids() {
    let host = ServerHost::new("test", "1", Arc::new(Empty)).with_async_handler(Arc::new(SlowFast));
    let request =
        |id: Value| json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"x"}});
    let reply = host
        .dispatch_async(&json!([request(json!("slow")), request(json!(u64::MAX))]))
        .await
        .unwrap();
    assert_eq!(reply[0]["id"], json!(u64::MAX));
    assert_eq!(reply[1]["id"], "slow");
    assert_eq!(reply[0]["result"]["idSeen"], json!(u64::MAX));
    assert_eq!(reply[1]["result"]["idSeen"], "slow");
}

struct Gate;
impl AdmissionHook for Gate {
    fn admit(&self, _request: &AdmissionRequest) -> Admission {
        Admission::Deny("blocked".into())
    }
}

struct MustNotRun(Arc<Mutex<bool>>);
impl AsyncRequestHandler for MustNotRun {
    fn handle<'a>(
        &'a self,
        _request: &'a Value,
        _context: &'a RequestContext,
    ) -> Pin<Box<dyn Future<Output = Option<Result<Value>>> + Send + 'a>> {
        *self.0.lock().unwrap() = true;
        Box::pin(async { Some(Ok(json!({"ran":true}))) })
    }
}

#[tokio::test]
async fn async_handler_runs_after_admission() {
    let called = Arc::new(Mutex::new(false));
    let host = ServerHost::new("test", "1", Arc::new(Empty))
        .with_admission(Arc::new(Gate))
        .with_async_handler(Arc::new(MustNotRun(called.clone())));
    let reply = host
        .dispatch_async(
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"x"}}),
        )
        .await
        .unwrap();
    assert_eq!(reply["result"]["isError"], true);
    assert!(!*called.lock().unwrap());
    let reply = host
        .dispatch_async(&json!({"id":2,"method":"tools/call","params":{"name":null}}))
        .await
        .unwrap();
    assert_eq!(reply["error"]["code"], -32602);
    assert!(!*called.lock().unwrap());
}

#[tokio::test]
async fn checked_async_dispatch_rejects_oversize_serialized_reply() {
    let host = ServerHost::new("test", "1", Arc::new(Empty))
        .with_async_handler(Arc::new(AsyncEcho))
        .with_response_limits(ResponseLimits {
            max_response_bytes: 16,
            max_frame_bytes: 1024,
        });
    let err = host
        .dispatch_checked_async(
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"x"}}),
            &RequestContext::default(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        rightkit_mcp::McpError::ResponseTooLarge { limit: 16 }
    ));
}

#[tokio::test]
async fn configured_protocol_versions_choose_highest_common_or_error() {
    let host = ServerHost::new("test", "1", Arc::new(Empty)).with_protocol_versions(vec![
        "2024-11-05".into(),
        "2025-03-26".into(),
        "2026-01-01".into(),
    ]);
    let request = |params| json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":params});
    assert_eq!(host.dispatch(&request(json!({"protocolVersion":"2024-11-05", "supportedProtocolVersions":["2024-11-05","2026-01-01"]}))).unwrap()["result"]["protocolVersion"], "2026-01-01");
    assert_eq!(
        host.dispatch(&request(
            json!({"supportedProtocolVersions":["2030-01-01"]})
        ))
        .unwrap()["error"]["code"],
        -32602
    );
}
