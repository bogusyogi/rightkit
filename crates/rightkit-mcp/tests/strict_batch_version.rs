//! Strict request validation, per-version batch policy, `TransformContext::request` and exact
//! whole-number progress (rightkit#283, rightkit#421).

use std::io::Cursor;
use std::sync::{Arc, Mutex};

use rightkit_mcp::*;
use serde_json::{json, Value};

struct Echo;

impl ToolProvider for Echo {
    fn list(&self) -> Vec<ToolDescriptor> {
        vec![ToolDescriptor::new(
            "echo",
            "Echo",
            json!({"type":"object"}),
        )]
    }

    fn call(&self, _name: &str, args: Value) -> Result<ToolResult> {
        Ok(ToolResult::json(args))
    }
}

fn host() -> ServerHost {
    ServerHost::new("t", "1", Arc::new(Echo))
}

fn strict() -> ServerHost {
    host().with_request_validation(RequestValidation::Strict)
}

fn ping(id: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"ping"})
}

fn invalid(reply: &Option<Value>, id: Value) {
    let reply = reply.as_ref().expect("an Invalid Request reply");
    assert_eq!(reply["error"]["code"], json!(-32600), "{reply}");
    assert_eq!(reply["error"]["message"], json!("Invalid Request"));
    assert_eq!(reply["id"], id);
}

#[test]
fn lenient_default_still_drops_malformed_frames() {
    for frame in [
        json!(5),
        json!("x"),
        json!({"jsonrpc":"2.0","id":1}),
        json!([]),
    ] {
        assert_eq!(host().dispatch(&frame), None, "{frame}");
    }
}

#[test]
fn strict_rejects_each_malformed_shape_with_32600() {
    let h = strict();
    invalid(&h.dispatch(&json!(5)), Value::Null);
    invalid(&h.dispatch(&json!("ping")), Value::Null);
    invalid(&h.dispatch(&json!(null)), Value::Null);
    invalid(&h.dispatch(&json!({"jsonrpc":"2.0","id":4})), json!(4));
    invalid(
        &h.dispatch(&json!({"jsonrpc":"2.0","id":4,"method":7})),
        json!(4),
    );
    invalid(&h.dispatch(&ping(json!(true))), Value::Null);
    invalid(&h.dispatch(&ping(json!({"a":1}))), Value::Null);
    invalid(&h.dispatch(&ping(Value::Null)), Value::Null);
    invalid(
        &h.dispatch(&json!({"jsonrpc":"2.0","id":9,"method":"ping","params":[1]})),
        json!(9),
    );
    invalid(
        &h.dispatch(&json!({"jsonrpc":"2.0","id":9,"method":"ping","params":null})),
        json!(9),
    );
    invalid(&h.dispatch(&json!([])), Value::Null);
}

#[test]
fn strict_accepts_valid_frames_and_notifications() {
    let h = strict();
    assert_eq!(
        h.dispatch(&ping(json!("a"))),
        Some(json!({"jsonrpc":"2.0","id":"a","result":{}}))
    );
    assert_eq!(
        h.dispatch(&json!({"jsonrpc":"2.0","id":2,"method":"ping","params":{}})),
        Some(json!({"jsonrpc":"2.0","id":2,"result":{}}))
    );
    let notification = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
    assert_eq!(h.dispatch(&notification), None);
}

#[test]
fn strict_null_id_follows_id_policy() {
    let h = strict().with_options(ServerOptions {
        id_policy: IdPolicy::NullResponse,
        ..ServerOptions::default()
    });
    let reply = h.dispatch(&ping(Value::Null)).expect("null-id reply");
    assert_eq!(reply["result"], json!({}));
}

#[test]
fn strict_validates_each_batch_item_independently() {
    let reply = strict()
        .dispatch(&json!([ping(json!(1)), json!(7), ping(json!([]))]))
        .expect("batch reply");
    let items = reply.as_array().expect("array");
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["result"], json!({}));
    assert_eq!(items[1]["error"]["code"], json!(-32600));
    assert_eq!(items[2]["error"]["code"], json!(-32600));
}

#[test]
fn strict_handlers_are_not_invoked_for_invalid_frames() {
    struct Count(Arc<Mutex<usize>>);
    impl ToolProvider for Count {
        fn list(&self) -> Vec<ToolDescriptor> {
            *self.0.lock().unwrap() += 1;
            vec![]
        }
        fn call(&self, name: &str, _args: Value) -> Result<ToolResult> {
            Err(McpError::ToolNotFound(name.into()))
        }
    }
    let calls = Arc::new(Mutex::new(0));
    let h = ServerHost::new("t", "1", Arc::new(Count(calls.clone())))
        .with_request_validation(RequestValidation::Strict);
    let bad = json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":[]});
    invalid(&h.dispatch(&bad), json!(1));
    assert_eq!(*calls.lock().unwrap(), 0);
}

#[test]
fn strict_applies_on_stdio_and_keeps_serving() {
    let input = format!("5\n{}\n[]\n", ping(json!(3)));
    let mut out = Vec::new();
    strict()
        .serve_stdio_on(Cursor::new(input), &mut out)
        .unwrap();
    let replies: Vec<Value> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(replies.len(), 3, "{replies:?}");
    assert_eq!(replies[0]["error"]["code"], json!(-32600));
    assert_eq!(replies[1]["result"], json!({}));
    assert_eq!(replies[2]["error"]["code"], json!(-32600));
}

#[tokio::test]
async fn strict_applies_to_async_dispatch() {
    let h = strict();
    invalid(&h.dispatch_async(&json!(5)).await, Value::Null);
    invalid(&h.dispatch_async(&json!([])).await, Value::Null);
    let reply = h
        .dispatch_async(&json!([ping(json!(1)), json!("x")]))
        .await
        .expect("batch");
    assert_eq!(reply[0]["result"], json!({}));
    assert_eq!(reply[1]["error"]["code"], json!(-32600));
}

fn with_version(version: &str) -> RequestContext {
    RequestContext {
        metadata: json!({"protocolVersion": version}),
        ..RequestContext::default()
    }
}

fn per_version() -> ServerHost {
    host()
        .with_batch_policy_for_version("2025-06-18", BatchPolicy::Reject)
        .with_batch_policy_for_version("2025-03-26", BatchPolicy::Process)
}

#[test]
fn batch_policy_follows_context_protocol_version() {
    let batch = json!([ping(json!(1))]);
    let rejected = per_version().dispatch_with_context(&batch, &with_version("2025-06-18"));
    invalid(&rejected, Value::Null);
    let processed = per_version()
        .dispatch_with_context(&batch, &with_version("2025-03-26"))
        .expect("processed");
    assert_eq!(processed[0]["result"], json!({}));
}

#[test]
fn unlisted_or_unknown_version_uses_the_global_batch_policy() {
    let batch = json!([ping(json!(1))]);
    let h = per_version().with_options(ServerOptions {
        batch_policy: BatchPolicy::Ignore,
        ..ServerOptions::default()
    });
    assert_eq!(h.dispatch(&batch), None);
    assert_eq!(
        h.dispatch_with_context(&batch, &with_version("2024-11-05")),
        None
    );
    let rejected = h.dispatch_with_context(&batch, &with_version("2025-06-18"));
    invalid(&rejected, Value::Null);
}

#[test]
fn last_rule_for_a_version_wins() {
    let h = host()
        .with_batch_policy_for_version("2025-06-18", BatchPolicy::Reject)
        .with_batch_policy_for_version("2025-06-18", BatchPolicy::Ignore);
    let batch = json!([ping(json!(1))]);
    assert_eq!(
        h.dispatch_with_context(&batch, &with_version("2025-06-18")),
        None
    );
}

#[test]
fn stdio_batch_policy_uses_the_version_negotiated_by_initialize() {
    let init = |v: &str| json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":v}});
    for (version, rejected) in [("2025-06-18", true), ("2025-03-26", false)] {
        let input = format!("{}\n{}\n", init(version), json!([ping(json!(2))]));
        let mut out = Vec::new();
        per_version()
            .serve_stdio_on(Cursor::new(input), &mut out)
            .unwrap();
        let replies: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(replies.len(), 2, "{replies:?}");
        assert_eq!(replies[0]["result"]["protocolVersion"], json!(version));
        if rejected {
            assert_eq!(replies[1]["error"]["code"], json!(-32600));
        } else {
            assert_eq!(replies[1][0]["result"], json!({}));
        }
    }
}

#[test]
fn a_batch_before_initialize_uses_the_global_policy() {
    let input = format!("{}\n", json!([ping(json!(2))]));
    let mut out = Vec::new();
    per_version()
        .serve_stdio_on(Cursor::new(input), &mut out)
        .unwrap();
    let reply: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(reply[0]["result"], json!({}));
}

#[test]
fn transform_receives_the_request_frame() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let h = host().with_request_transform(move |ctx, result| {
        sink.lock().unwrap().push(ctx.request().clone());
        let gate = ctx
            .request()
            .pointer("/params/_meta/protocolVersion")
            .and_then(Value::as_str)
            .map(str::to_string);
        if let Some(gate) = gate {
            result["resultType"] = json!(gate);
        }
    });
    let gated = json!({"jsonrpc":"2.0","id":1,"method":"tools/list",
        "params":{"_meta":{"protocolVersion":"2026-07-28"}}});
    let plain = json!({"jsonrpc":"2.0","id":2,"method":"tools/list"});
    let first = h.dispatch(&gated).unwrap();
    let second = h.dispatch(&plain).unwrap();
    assert_eq!(first["result"]["resultType"], json!("2026-07-28"));
    assert!(second["result"].get("resultType").is_none());
    assert_eq!(*seen.lock().unwrap(), vec![gated, plain]);
}

#[test]
fn transform_sees_each_batch_item_as_its_own_request() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let h = host().with_request_transform(move |ctx, _| {
        sink.lock().unwrap().push(ctx.request()["id"].clone());
    });
    h.dispatch(&json!([ping(json!(1)), ping(json!(2))]))
        .unwrap();
    assert_eq!(*seen.lock().unwrap(), vec![json!(1), json!(2)]);
}

#[test]
fn transform_context_debug_omits_request_contents() {
    let shown = Arc::new(Mutex::new(String::new()));
    let sink = shown.clone();
    let h = host().with_request_transform(move |ctx, _| {
        *sink.lock().unwrap() = format!("{ctx:?}");
    });
    h.dispatch(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list",
        "params":{"password":"hunter2-secret"}}))
        .unwrap();
    let text = shown.lock().unwrap().clone();
    assert!(text.contains("tools/list"), "{text}");
    assert!(
        !text.contains("hunter2-secret") && !text.contains("password"),
        "{text}"
    );
}

#[test]
fn response_transform_keeps_the_two_field_shape() {
    let seen = Arc::new(Mutex::new(None));
    let sink = seen.clone();
    let h = host().with_response_transform(move |&TransformInfo { method, transport }, _| {
        *sink.lock().unwrap() = Some((method.map(str::to_string), transport));
    });
    h.dispatch(&ping(json!(1))).unwrap();
    assert!(seen.lock().unwrap().is_some());
}

#[derive(Clone, Default)]
struct SharedOut(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for SharedOut {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn concurrent_stdio_validates_cancellation_before_acting_on_it() {
    let out = SharedOut::default();
    let input = format!(
        "{}\n{}\n",
        json!({"jsonrpc":"2.0","id":null,"method":"notifications/cancelled",
            "params":{"requestId":1}}),
        ping(json!(2))
    );
    strict()
        .serve_stdio_concurrent_on(Cursor::new(input), out.clone())
        .unwrap();
    let text = String::from_utf8(out.0.lock().unwrap().clone()).unwrap();
    let replies: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(
        replies
            .iter()
            .any(|r| r["error"]["code"] == json!(-32600) && r["id"].is_null()),
        "{replies:?}"
    );
}
