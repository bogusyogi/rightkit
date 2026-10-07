//! Lifecycle events: start / stop / crash / denied, each with session id, reason, and time.

use std::collections::BTreeMap;
use std::io::{BufRead, Cursor, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use rightkit_mcp::*;
use serde_json::{json, Value};

fn example_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap().join("examples");
    let p = dir.join(if cfg!(windows) { "example_server.exe" } else { "example_server" });
    assert!(p.exists(), "example binary missing at {p:?}");
    p
}

fn stdio_spec() -> ServerSpec {
    ServerSpec::Stdio {
        command: example_bin().to_string_lossy().into(),
        args: vec!["stdio".into()],
        env: BTreeMap::new(),
        cwd: None,
    }
}

type Log = Arc<Mutex<Vec<McpEvent>>>;

fn recorder() -> (Log, impl Fn(&McpEvent) + Send + Sync + 'static) {
    let log: Log = Arc::default();
    let l2 = log.clone();
    (log, move |e: &McpEvent| l2.lock().unwrap().push(e.clone()))
}

fn kinds(log: &Log) -> Vec<LifecycleKind> {
    log.lock().unwrap().iter().map(|e| e.kind).collect()
}

fn wait_for(log: &Log, kind: LifecycleKind) -> bool {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if kinds(log).contains(&kind) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn assert_well_formed(log: &Log, since: SystemTime) {
    for e in log.lock().unwrap().iter() {
        assert!(!e.session_id.is_empty(), "{e:?}");
        assert!(!e.reason.is_empty(), "{e:?}");
        assert!(e.at >= since && e.timestamp_ms() > 0, "{e:?}");
        let j = e.to_json();
        assert_eq!(j["event"], e.kind.label());
        assert_eq!(j["session_id"], e.session_id.as_str());
    }
}

#[test]
fn stdio_client_graceful_start_stop() {
    let since = SystemTime::now();
    let (log, sink) = recorder();
    let c = McpClient::connect("demo", stdio_spec(), ClientOptions::default().on_event(sink)).unwrap();
    assert_eq!(c.call_tool("echo", json!({"message": "x"})).unwrap().text_content(), "echo: x");
    c.close();
    assert!(wait_for(&log, LifecycleKind::Stop));
    assert_eq!(kinds(&log), vec![LifecycleKind::Start, LifecycleKind::Stop]);
    let ev = log.lock().unwrap().clone();
    assert!(ev[0].session_id.starts_with("mcpc-"));
    assert_eq!(ev[0].session_id, ev[1].session_id);
    assert_eq!(ev[0].server.as_deref(), Some("demo"));
    assert_eq!(ev[0].transport, Some("stdio"));
    assert!(ev[0].pid.is_some());
    assert_well_formed(&log, since);
}

#[test]
fn stdio_server_death_is_a_crash_then_reconnect_starts_a_new_session() {
    let since = SystemTime::now();
    let (log, sink) = recorder();
    let c = McpClient::connect("demo", stdio_spec(), ClientOptions::default().on_event(sink)).unwrap();
    let first = log.lock().unwrap()[0].session_id.clone();
    assert!(c.call_tool("die", json!({})).is_err());
    assert!(wait_for(&log, LifecycleKind::Crash));
    {
        let ev = log.lock().unwrap();
        let crash = ev.iter().find(|e| e.kind == LifecycleKind::Crash).unwrap();
        assert_eq!(crash.session_id, first, "crash is reported for the connection that died");
        assert!(crash.reason.contains("exited") || crash.reason.contains("closed"), "{crash:?}");
        assert!(crash.pid.is_some());
    }
    // Next call re-spawns: a new session id starts.
    assert_eq!(c.call_tool("echo", json!({"message": "back"})).unwrap().text_content(), "echo: back");
    c.close();
    // Wait for the second session's Stop (bounded), not a fixed sleep: under load it can lag.
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end && log.lock().unwrap().last().map(|e| e.kind) != Some(LifecycleKind::Stop) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let ev = log.lock().unwrap().clone();
    let starts: Vec<_> = ev.iter().filter(|e| e.kind == LifecycleKind::Start).map(|e| e.session_id.clone()).collect();
    assert!(starts.len() >= 2 && starts.last() != Some(&first), "{ev:?}");
    assert_eq!(ev.last().unwrap().kind, LifecycleKind::Stop);
    assert_eq!(&ev.last().unwrap().session_id, starts.last().unwrap());
    assert_well_formed(&log, since);
}

#[test]
fn pool_trust_gate_denials_are_events() {
    let since = SystemTime::now();
    let (log, sink) = recorder();
    let spec = stdio_spec();
    let entry = ServerEntry { name: "ws".into(), spec: spec.clone(), source: ServerSource::Workspace("/p".into()) };
    let pool = ConnectionPool::new(
        PoolConfig { max_connections: 2, options: ClientOptions::default().on_event(sink) },
        TrustStore::new(),
    );
    assert!(matches!(pool.get(&entry), Err(McpError::NotApproved(_))));
    pool.trust().deny("/p", "ws", &spec);
    assert!(matches!(pool.get(&entry), Err(McpError::Denied(_))));
    let ev = log.lock().unwrap().clone();
    assert_eq!(kinds(&log), vec![LifecycleKind::Denied, LifecycleKind::Denied], "nothing was spawned");
    for e in &ev {
        assert_eq!(e.session_id, pool.id());
        assert_eq!(e.server.as_deref(), Some("ws"));
    }
    assert!(ev[1].reason.contains("denied"));
    assert_well_formed(&log, since);
}

struct Provider;
impl ToolProvider for Provider {
    fn list(&self) -> Vec<ToolDescriptor> {
        vec![
            ToolDescriptor::new("read", "read-only", json!({"type": "object"})).read_only(true),
            ToolDescriptor::new("write", "mutating", json!({"type": "object"})),
        ]
    }
    fn call(&self, name: &str, _args: Value) -> Result<ToolResult> {
        Ok(ToolResult::text(format!("ran {name}")))
    }
}

fn host(sink: impl Fn(&McpEvent) + Send + Sync + 'static) -> ServerHost {
    ServerHost::new("test-host", "0.0.0", Arc::new(Provider))
        .with_admission(Arc::new(ReadOnlyPolicy { allow_mutate: false, blocked: vec![] }))
        .on_event(sink)
}

fn lines(msgs: &[Value]) -> String {
    msgs.iter().map(|m| format!("{m}\n")).collect()
}

#[test]
fn server_stdio_start_denied_stop() {
    let since = SystemTime::now();
    let (log, sink) = recorder();
    let input = lines(&[
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"read","arguments":{}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"write","arguments":{"secret":"never logged"}}}),
    ]);
    let mut out = Vec::new();
    host(sink).serve_stdio_on(Cursor::new(input), &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("ran read") && !out.contains("ran write"), "{out}");
    assert_eq!(kinds(&log), vec![LifecycleKind::Start, LifecycleKind::Denied, LifecycleKind::Stop]);
    let ev = log.lock().unwrap().clone();
    assert!(ev.iter().all(|e| e.session_id == ev[0].session_id && e.session_id.starts_with("mcps-")));
    assert_eq!(ev[1].tool.as_deref(), Some("write"));
    assert_eq!(ev[1].transport, Some("stdio"));
    assert!(ev[1].reason.contains("mutat"), "{:?}", ev[1]);
    assert!(!format!("{ev:?}").contains("never logged"), "events must be content-free");
    assert_well_formed(&log, since);
}

/// A reader that fails after its first line, standing in for a broken pipe.
struct Broken(bool);
impl Read for Broken {
    fn read(&mut self, _b: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("pipe broke"))
    }
}
impl BufRead for Broken {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        if !self.0 {
            self.0 = true;
            return Ok(b"\n");
        }
        Err(std::io::Error::other("pipe broke"))
    }
    fn consume(&mut self, _n: usize) {}
}

#[test]
fn server_stdio_io_failure_is_a_crash() {
    let since = SystemTime::now();
    let (log, sink) = recorder();
    assert!(host(sink).serve_stdio_on(Broken(false), Vec::new()).is_err());
    assert_eq!(kinds(&log), vec![LifecycleKind::Start, LifecycleKind::Crash]);
    assert!(log.lock().unwrap()[1].reason.contains("pipe broke"));
    assert_well_formed(&log, since);
}

#[test]
fn server_http_start_denied_stop() {
    let since = SystemTime::now();
    let (log, sink) = recorder();
    let srv = host(sink).serve_http(HttpServeOptions { port: 0, token: Some("tok".into()) }).unwrap();
    let port = srv.addr().port();
    let raw = |req: String| {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(req.as_bytes()).unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    };
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
    let no_token = raw(format!("POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\n\r\n{body}", body.len()));
    assert!(no_token.starts_with("HTTP/1.1 401"), "{no_token}");
    let evil = raw(format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: http://evil.example\r\nAuthorization: Bearer tok\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    ));
    assert!(evil.starts_with("HTTP/1.1 403"), "{evil}");
    let sid = srv.session_id().to_string();
    srv.shutdown();
    assert_eq!(
        kinds(&log),
        vec![LifecycleKind::Start, LifecycleKind::Denied, LifecycleKind::Denied, LifecycleKind::Stop]
    );
    let ev = log.lock().unwrap().clone();
    assert!(ev.iter().all(|e| e.session_id == sid && e.transport == Some("http")));
    assert!(ev[0].reason.contains("127.0.0.1"));
    assert!(ev[1].reason.contains("bearer"));
    assert!(ev[2].reason.contains("Origin"));
    assert!(!format!("{ev:?}").contains("tok\""), "token never appears in events");
    assert_well_formed(&log, since);
}
