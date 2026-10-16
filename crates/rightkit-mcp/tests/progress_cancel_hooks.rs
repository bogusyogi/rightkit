//! Progress/cancel notifications during a tool call, initialize overrides, response hook.

use std::io::{self, BufRead, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use rightkit_mcp::*;
use serde_json::{json, Value};

/// A BufRead fed line by line from the test while the server is running.
struct ChanReader {
    rx: mpsc::Receiver<Vec<u8>>,
    buf: Vec<u8>,
    pos: usize,
}

impl Read for ChanReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = {
            let available = self.fill_buf()?;
            let n = available.len().min(out.len());
            out[..n].copy_from_slice(&available[..n]);
            n
        };
        self.consume(n);
        Ok(n)
    }
}

impl BufRead for ChanReader {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.pos >= self.buf.len() {
            self.buf = self.rx.recv().unwrap_or_default();
            self.pos = 0;
        }
        Ok(&self.buf[self.pos..])
    }
    fn consume(&mut self, amt: usize) {
        self.pos += amt;
    }
}

#[derive(Clone, Default)]
struct SharedOut(Arc<Mutex<Vec<u8>>>);

impl Write for SharedOut {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl SharedOut {
    fn frames(&self) -> Vec<Value> {
        String::from_utf8(self.0.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
    fn wait_for(&self, what: &str, pred: impl Fn(&Value) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.frames().iter().any(&pred) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("timed out waiting for {what}; got {:?}", self.frames());
    }
}

/// `work` emits one progress notification, then either finishes (`mode: finish`, with a second
/// progress) or waits for cancellation (`mode: wait`).
struct Worker {
    saw_cancel: Arc<AtomicBool>,
}

impl ToolProvider for Worker {
    fn list(&self) -> Vec<ToolDescriptor> {
        vec![ToolDescriptor::new("work", "Work", json!({"type":"object"}))]
    }
    fn call(&self, name: &str, _args: Value) -> Result<ToolResult> {
        Err(McpError::ToolNotFound(name.into()))
    }
    fn call_tool_with_handle(
        &self,
        request: &Value,
        _context: &RequestContext,
        call: &CallHandle,
    ) -> Option<Result<Value>> {
        let mode = request
            .pointer("/params/arguments/mode")
            .and_then(Value::as_str)
            .unwrap_or("finish");
        call.notify_progress(1.0, Some(3.0), Some("started"));
        if mode == "wait" {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !call.is_cancelled() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(2));
            }
            self.saw_cancel
                .store(call.is_cancelled(), Ordering::SeqCst);
            return Some(Ok(ToolResult::text("stopped").to_value()));
        }
        call.notify_progress(2.0, Some(3.0), None);
        Some(Ok(ToolResult::text("done").to_value()))
    }
}

struct Session {
    tx: mpsc::Sender<Vec<u8>>,
    out: SharedOut,
    join: std::thread::JoinHandle<io::Result<()>>,
    saw_cancel: Arc<AtomicBool>,
}

impl Session {
    fn start(configure: impl FnOnce(ServerHost) -> ServerHost) -> Self {
        let saw_cancel = Arc::new(AtomicBool::new(false));
        let host = configure(ServerHost::new(
            "worker",
            "1",
            Arc::new(Worker {
                saw_cancel: saw_cancel.clone(),
            }),
        ));
        let (tx, rx) = mpsc::channel();
        let out = SharedOut::default();
        let reader = ChanReader {
            rx,
            buf: vec![],
            pos: 0,
        };
        let writer = out.clone();
        let join = std::thread::spawn(move || host.serve_stdio_concurrent_on(reader, writer));
        Session {
            tx,
            out,
            join,
            saw_cancel,
        }
    }
    fn send(&self, v: Value) {
        self.tx.send(format!("{v}\n").into_bytes()).unwrap();
    }
    fn finish(self) -> (Vec<Value>, bool) {
        drop(self.tx);
        self.join.join().unwrap().unwrap();
        (self.out.frames(), self.saw_cancel.load(Ordering::SeqCst))
    }
}

fn call(id: u64, token: Option<Value>, mode: &str) -> Value {
    let mut params = json!({"name":"work","arguments":{"mode":mode}});
    if let Some(t) = token {
        params["_meta"] = json!({"progressToken": t});
    }
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":params})
}

fn is_progress(v: &Value) -> bool {
    v["method"] == "notifications/progress"
}

#[test]
fn progress_notification_carries_the_request_token() {
    let s = Session::start(|h| h);
    s.send(call(7, Some(json!("tok-1")), "finish"));
    let (frames, _) = s.finish();
    let progress: Vec<&Value> = frames.iter().filter(|f| is_progress(f)).collect();
    assert_eq!(progress.len(), 2, "{frames:?}");
    for p in &progress {
        assert_eq!(p["jsonrpc"], "2.0");
        assert!(p.get("id").is_none(), "a notification has no id");
        assert_eq!(p["params"]["progressToken"], "tok-1");
    }
    assert_eq!(progress[0]["params"]["progress"].as_f64(), Some(1.0));
    assert_eq!(progress[0]["params"]["total"].as_f64(), Some(3.0));
    assert_eq!(progress[0]["params"]["message"], "started");
    assert_eq!(progress[1]["params"]["progress"].as_f64(), Some(2.0));
    // Progress arrives before the reply it belongs to.
    let reply_at = frames.iter().position(|f| f["id"] == 7).expect("reply");
    assert!(frames[..reply_at].iter().filter(|f| is_progress(f)).count() == 2);
    assert_eq!(frames[reply_at]["result"]["content"][0]["text"], "done");
}

#[test]
fn numeric_token_is_echoed_and_no_token_means_no_notification() {
    let s = Session::start(|h| h);
    s.send(call(1, Some(json!(42)), "finish"));
    s.send(call(2, None, "finish"));
    let (frames, _) = s.finish();
    let progress: Vec<&Value> = frames.iter().filter(|f| is_progress(f)).collect();
    assert_eq!(progress.len(), 2, "only the call with a token reports");
    assert!(progress.iter().all(|p| p["params"]["progressToken"] == 42));
    assert!(frames.iter().any(|f| f["id"] == 2 && f.get("result").is_some()));
}

#[test]
fn cancellation_reaches_the_running_handler_and_ping_is_answered_mid_call() {
    let s = Session::start(|h| h);
    s.send(call(7, Some(json!("t")), "wait"));
    s.out.wait_for("progress", is_progress);
    // The handler is still running; the reader thread answers ping now.
    s.send(json!({"jsonrpc":"2.0","id":8,"method":"ping"}));
    s.out.wait_for("ping reply", |f| f["id"] == 8);
    assert!(!s.out.frames().iter().any(|f| f["id"] == 7));
    s.send(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":7,"reason":"user"}}));
    let (frames, saw_cancel) = s.finish();
    assert!(saw_cancel, "handler must observe the cancellation");
    assert!(
        !frames.iter().any(|f| f["id"] == 7),
        "spec: no reply to a cancelled request: {frames:?}"
    );
}

#[test]
fn cancelled_response_send_still_replies() {
    let s = Session::start(|h| h.with_cancelled_response(CancelledResponse::Send));
    s.send(call(7, Some(json!("t")), "wait"));
    s.out.wait_for("progress", is_progress);
    s.send(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":7}}));
    let (frames, saw_cancel) = s.finish();
    assert!(saw_cancel);
    assert!(frames.iter().any(|f| f["id"] == 7 && f.get("result").is_some()));
}

#[test]
fn cancelling_an_unknown_or_finished_request_is_ignored() {
    let s = Session::start(|h| h);
    s.send(call(1, None, "finish"));
    s.send(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}));
    s.send(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":99}}));
    let (frames, _) = s.finish();
    assert!(frames.iter().any(|f| f["id"] == 1 && f.get("result").is_some()));
}

#[test]
fn handle_is_inert_outside_concurrent_serving() {
    let host = ServerHost::new(
        "worker",
        "1",
        Arc::new(Worker {
            saw_cancel: Arc::new(AtomicBool::new(false)),
        }),
    );
    let reply = host
        .dispatch(&call(3, Some(json!("t")), "finish"))
        .expect("reply");
    assert_eq!(reply["result"]["content"][0]["text"], "done");
    let handle = CallHandle::default();
    assert!(!handle.notify_progress(1.0, None, None));
    assert!(!handle.is_cancelled());
    assert!(handle.progress_token().is_none());
}

struct Quiet;

impl ToolProvider for Quiet {
    fn list(&self) -> Vec<ToolDescriptor> {
        vec![]
    }
    fn call(&self, name: &str, _args: Value) -> Result<ToolResult> {
        Err(McpError::ToolNotFound(name.into()))
    }
    fn resources(&self) -> Vec<ResourceDescriptor> {
        vec![]
    }
}

fn init_request(version: &str) -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":version}})
}

#[test]
fn initialize_overrides_appear_in_the_result_and_negotiation_is_kept() {
    let host = ServerHost::new("ember", "9", Arc::new(Quiet))
        .with_instructions("old text")
        .with_initialize_overrides(InitializeOverrides {
            server_title: Some("Ember".into()),
            server_info: Some(json!({"websiteUrl":"https://example.test"})),
            instructions: Some("use the tools".into()),
            capabilities: Some(json!({
                "tools": {"listChanged": false},
                "resources": {"subscribe": false, "listChanged": false},
            })),
            default_protocol_version: Some("2025-06-18".into()),
        });
    let r = host.dispatch(&init_request("2025-03-26")).unwrap()["result"].clone();
    assert_eq!(r["protocolVersion"], "2025-03-26", "negotiates the client's version");
    assert_eq!(r["serverInfo"]["name"], "ember");
    assert_eq!(r["serverInfo"]["version"], "9");
    assert_eq!(r["serverInfo"]["title"], "Ember");
    assert_eq!(r["serverInfo"]["websiteUrl"], "https://example.test");
    assert_eq!(r["instructions"], "use the tools");
    assert_eq!(r["capabilities"]["tools"], json!({"listChanged": false}));
    assert_eq!(
        r["capabilities"]["resources"],
        json!({"subscribe": false, "listChanged": false})
    );
    // Unknown version: the configured default, not the crate default.
    let r = host.dispatch(&init_request("1999-01-01")).unwrap()["result"].clone();
    assert_eq!(r["protocolVersion"], "2025-06-18");
    let r = host.dispatch(&init_request("2024-11-05")).unwrap()["result"].clone();
    assert_eq!(r["protocolVersion"], "2024-11-05");
}

#[test]
fn without_overrides_initialize_is_unchanged() {
    let host = ServerHost::new("ember", "9", Arc::new(Quiet));
    let r = host.dispatch(&init_request("1999-01-01")).unwrap()["result"].clone();
    assert_eq!(r["protocolVersion"], PROTOCOL_VERSION);
    assert_eq!(r["serverInfo"], json!({"name":"ember","version":"9"}));
    assert_eq!(r["capabilities"], json!({"tools": {}}));
    assert!(r.get("instructions").is_none());
}

#[test]
fn response_hook_fires_once_per_response_and_sees_it_unchanged() {
    let seen: Arc<Mutex<Vec<(Option<String>, Value)>>> = Arc::default();
    let sink = seen.clone();
    let host = ServerHost::new("ember", "9", Arc::new(Quiet)).with_response_hook(move |info| {
        sink.lock()
            .unwrap()
            .push((info.method.map(str::to_string), info.response.clone()));
    });
    let ping = host
        .dispatch(&json!({"jsonrpc":"2.0","id":1,"method":"ping"}))
        .unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert_eq!(seen.lock().unwrap()[0].0.as_deref(), Some("ping"));
    assert_eq!(seen.lock().unwrap()[0].1, ping, "the hook saw exactly the sent reply");

    // A notification has no response: no hook call.
    assert!(host
        .dispatch(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
        .is_none());
    assert_eq!(seen.lock().unwrap().len(), 1);

    // An error reply counts; a batch of two requests yields two calls.
    host.dispatch(&json!({"jsonrpc":"2.0","id":2,"method":"nope"}));
    assert_eq!(seen.lock().unwrap().len(), 2);
    assert!(seen.lock().unwrap()[1].1.get("error").is_some());
    host.dispatch(&json!([
        {"jsonrpc":"2.0","id":3,"method":"ping"},
        {"jsonrpc":"2.0","id":4,"method":"ping"},
    ]));
    assert_eq!(seen.lock().unwrap().len(), 4);
}

#[test]
fn response_hook_fires_on_stdio_including_parse_errors() {
    let count = Arc::new(Mutex::new(0usize));
    let c = count.clone();
    let host = ServerHost::new("ember", "9", Arc::new(Quiet)).with_response_hook(move |_| {
        *c.lock().unwrap() += 1;
    });
    let input = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\nnot json\n";
    let mut out = Vec::new();
    host.serve_stdio_on(io::Cursor::new(input), &mut out).unwrap();
    assert_eq!(out.split(|b| *b == b'\n').filter(|l| !l.is_empty()).count(), 2);
    assert_eq!(*count.lock().unwrap(), 2);
}

#[test]
fn a_panicking_hook_does_not_stop_serving() {
    let host = ServerHost::new("ember", "9", Arc::new(Quiet)).with_response_hook(|_| {
        panic!("hook bug");
    });
    let reply = host.dispatch(&json!({"jsonrpc":"2.0","id":1,"method":"ping"}));
    assert_eq!(reply.unwrap()["result"], json!({}));
}
