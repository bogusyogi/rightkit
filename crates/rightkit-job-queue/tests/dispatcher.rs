//! Dispatcher robustness: handler panics always answer, and an app's own
//! `doctor` takes precedence over the built-in.

use std::io::{BufReader, Cursor};
use std::sync::{Arc, Mutex};

use rightkit_job_queue::*;
use serde_json::{json, Value};

#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for SharedBuf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn run(d: Dispatcher, lines: &[&str]) -> Vec<Value> {
    let out = SharedBuf::default();
    serve(
        Arc::new(d),
        BufReader::new(Cursor::new(lines.join("\n"))),
        out.clone(),
        false,
    )
    .unwrap();
    let bytes = out.0.lock().unwrap().clone();
    String::from_utf8(bytes)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn response(frames: &[Value], id: u64) -> Value {
    frames
        .iter()
        .find(|f| f["type"] == "response" && f["id"] == id)
        .unwrap_or_else(|| panic!("no response for request {id}: {frames:?}"))
        .clone()
}

/// `boom` panics inline; `render` panics on a long-request thread.
struct Panicky;
impl CommandSet for Panicky {
    fn methods(&self) -> Vec<String> {
        vec!["boom".into(), "render".into(), "echo".into()]
    }
    fn is_long(&self, method: &str, _: &Value) -> bool {
        method == "render"
    }
    fn call(&self, method: &str, p: &Value, _: bool, _: &Sink) -> JobResult<Value> {
        match method {
            "boom" => panic!("inline handler exploded"),
            "render" => panic!("{}", format!("render thread exploded for {}", p["what"])),
            _ => Ok(p.clone()),
        }
    }
}

#[test]
fn handler_panics_always_get_an_internal_error_and_serving_continues() {
    let frames = run(
        Dispatcher::new("t", "1").with(Arc::new(Panicky)),
        &[
            r#"{"type":"request","id":1,"method":"boom"}"#,
            r#"{"type":"request","id":2,"method":"render","params":{"what":"clip"}}"#,
            r#"{"type":"request","id":3,"method":"echo","params":{"x":1}}"#,
            r#"{"type":"request","id":4,"method":"ping"}"#,
            r#"{"type":"request","id":5,"method":"render","params":{"what":"again"}}"#,
            r#"{"type":"request","id":6,"method":"shutdown"}"#,
        ],
    );
    let inline = response(&frames, 1);
    assert_eq!(
        (inline["ok"].as_bool(), inline["error"]["code"].as_str()),
        (Some(false), Some("internal"))
    );
    assert!(inline["error"]["message"]
        .as_str()
        .unwrap()
        .contains("inline handler exploded"));
    for id in [2, 5] {
        let long = response(&frames, id);
        assert_eq!(long["error"]["code"], "internal", "{long}");
        assert!(long["error"]["message"]
            .as_str()
            .unwrap()
            .contains("render thread exploded"));
    }
    assert_eq!(response(&frames, 3)["result"], json!({"x": 1}));
    assert_eq!(response(&frames, 4)["result"]["pong"], true);
    assert_eq!(response(&frames, 6)["ok"], true);
    // Exactly one response per request.
    assert_eq!(frames.iter().filter(|f| f["type"] == "response").count(), 6);

    // The same holds for direct calls (CLI and rightkit-daemon transports).
    let d = Dispatcher::new("t", "1").with(Arc::new(Panicky));
    let sink: Sink = Arc::new(|_| {});
    assert_eq!(
        d.call("boom", &json!({}), false, &sink).unwrap_err().code,
        "internal"
    );
    assert_eq!(
        d.call("echo", &json!({"y": 2}), false, &sink).unwrap()["y"],
        2
    );
}

struct AppDoctor;
impl CommandSet for AppDoctor {
    fn methods(&self) -> Vec<String> {
        vec!["doctor".into(), "spend.status".into()]
    }
    fn call(&self, method: &str, _: &Value, dry_run: bool, _: &Sink) -> JobResult<Value> {
        Ok(match method {
            "doctor" => json!({"app_doctor": true, "capabilities": ["spend"], "dry_run": dry_run}),
            _ => json!({"cap_usd": 5.0}),
        })
    }
}

#[test]
fn an_app_doctor_takes_precedence_over_the_builtin() {
    let sink: Sink = Arc::new(|_| {});
    let app = Dispatcher::new("t", "1").with(Arc::new(AppDoctor));
    let v = app.call("doctor", &json!({}), true, &sink).unwrap();
    assert_eq!(
        v,
        json!({"app_doctor": true, "capabilities": ["spend"], "dry_run": true})
    );
    let methods = app.methods();
    assert_eq!(
        methods.iter().filter(|m| *m == "doctor").count(),
        1,
        "{methods:?}"
    );
    assert!(methods.contains(&"ping".to_string()) && methods.contains(&"spend.status".to_string()));
    let frames = run(app, &[r#"{"type":"request","id":1,"method":"doctor"}"#]);
    assert_eq!(response(&frames, 1)["result"]["app_doctor"], true);

    // Without an app doctor the built-in still answers.
    let plain = Dispatcher::new("t", "1").with(Arc::new(Panicky));
    let v = plain.call("doctor", &json!({}), false, &sink).unwrap();
    assert_eq!(
        (v["engine"].as_str(), v["contract_version"].as_u64()),
        (Some("t"), Some(CONTRACT_VERSION as u64))
    );
    assert!(v["methods"].as_array().unwrap().contains(&json!("doctor")));
}
