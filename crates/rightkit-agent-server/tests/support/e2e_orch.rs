//! Shared black-box helpers for the `e2e_orch_*` journey tests.
//!
//! Everything here drives the REAL `coderight` binary (`serve` daemon, `acp-server`,
//! `run`) in an isolated temp home/workspace against a scripted OpenAI-compatible
//! provider whose replies are computed per request, so a test can assert both what
//! the product did and exactly what the model was shown.
#![allow(dead_code)]

pub use super::daemon_process;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

pub use daemon_process::TestDaemon;

/// Embedded-catalog slug (the capacity gate resolves the model against it).
pub const MODEL: &str = "deepseek/deepseek-v4-pro";

// ───────────────────────── scripted provider ─────────────────────────

/// One model reply: optional text plus zero or more tool calls `(id, name, args)`.
#[derive(Clone, Default)]
pub struct Reply {
    pub text: Option<String>,
    pub tools: Vec<(String, String, Value)>,
}

impl Reply {
    pub fn text(t: &str) -> Self {
        Reply {
            text: Some(t.to_string()),
            tools: vec![],
        }
    }
    pub fn tool(id: &str, name: &str, args: Value) -> Self {
        Reply {
            text: None,
            tools: vec![(id.to_string(), name.to_string(), args)],
        }
    }
}

type Responder = dyn Fn(&Value, usize) -> Reply + Send + Sync;

/// Per-request scripted `/chat/completions` server. The closure receives the
/// request body and its 0-based arrival index.
pub struct Provider {
    port: u16,
    requests: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

fn sse(chunk: Value) -> String {
    format!("data: {chunk}\n\n")
}

fn render(reply: &Reply) -> String {
    let mut out = String::new();
    if let Some(t) = &reply.text {
        out += &sse(json!({"choices":[{"delta":{"content": t},"finish_reason":null}]}));
    }
    for (i, (id, name, args)) in reply.tools.iter().enumerate() {
        out += &sse(json!({"choices":[{"delta":{"tool_calls":[{
            "index":i,"id":id,"type":"function",
            "function":{"name":name,"arguments":args.to_string()}}]},"finish_reason":null}]}));
    }
    let reason = if reply.tools.is_empty() {
        "stop"
    } else {
        "tool_calls"
    };
    out += &sse(json!({
        "choices":[{"delta":{},"finish_reason":reason}],
        "usage":{"prompt_tokens":900,"completion_tokens":20,"total_tokens":920,
                 "prompt_tokens_details":{"cached_tokens":0}}
    }));
    out + "data: [DONE]\n\n"
}

fn read_request(stream: &mut TcpStream) -> Option<(String, String)> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).ok()? == 0 {
            return None;
        }
        buf.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&buf).to_string();
    let line = head.lines().next()?.to_string();
    let len = head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).ok()?;
    Some((line, String::from_utf8_lossy(&body).to_string()))
}

impl Provider {
    pub fn start(f: impl Fn(&Value, usize) -> Reply + Send + Sync + 'static) -> Self {
        let f: Arc<Responder> = Arc::new(f);
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind provider");
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let (requests, stop) = (requests.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let mut stream = match listener.accept() {
                        Ok((s, _)) => s,
                        Err(_) => {
                            std::thread::sleep(Duration::from_millis(15));
                            continue;
                        }
                    };
                    let (requests, f) = (requests.clone(), f.clone());
                    std::thread::spawn(move || {
                        stream.set_nonblocking(false).ok();
                        let Some((line, text)) = read_request(&mut stream) else {
                            return;
                        };
                        if !line.contains("/chat/completions") {
                            let _ = stream.write_all(
                                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            );
                            return;
                        }
                        let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                        let index = {
                            let mut g = requests.lock().unwrap();
                            g.push(body.clone());
                            g.len() - 1
                        };
                        let payload = render(&f(&body, index));
                        let _ = stream.write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                        );
                        let _ = stream.write_all(
                            format!("{:x}\r\n{payload}\r\n0\r\n\r\n", payload.len()).as_bytes(),
                        );
                        let _ = stream.flush();
                    });
                }
            })
        };
        Provider {
            port,
            requests,
            stop,
            handle: Some(handle),
        }
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }
    pub fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
    pub fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl Drop for Provider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

// ───────────────────────── request inspection ─────────────────────────

fn content_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Text of the first `user` message (the task a session/subagent was started with).
pub fn first_user_text(body: &Value) -> String {
    body["messages"]
        .as_array()
        .and_then(|m| m.iter().find(|m| m["role"] == "user"))
        .map(|m| content_text(&m["content"]))
        .unwrap_or_default()
}

/// Text of the last `user` message.
pub fn last_user_text(body: &Value) -> String {
    body["messages"]
        .as_array()
        .and_then(|m| m.iter().rev().find(|m| m["role"] == "user"))
        .map(|m| content_text(&m["content"]))
        .unwrap_or_default()
}

/// All text the model was shown (system + every message), for "did the model see X".
pub fn whole_prompt(body: &Value) -> String {
    body["messages"]
        .as_array()
        .map(|m| {
            m.iter()
                .map(|m| content_text(&m["content"]))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

include!("untrusted.rs");

/// Contents of every `tool` role message (results the model received).
pub fn tool_results(body: &Value) -> Vec<String> {
    body["messages"]
        .as_array()
        .map(|m| {
            m.iter()
                .filter(|m| m["role"] == "tool")
                .map(|m| unwrap_untrusted(&content_text(&m["content"])).to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Names of the tools the request offered to the model.
pub fn offered_tools(body: &Value) -> Vec<String> {
    body["tools"]
        .as_array()
        .map(|t| {
            t.iter()
                .filter_map(|t| t["function"]["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

// ───────────────────────── environment ─────────────────────────

pub fn temp_dir(label: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("e2e-orch-{label}-"))
        .tempdir()
        .expect("temp dir")
}

/// Field order is drop order: the daemon is killed before its home is deleted.
pub struct Env {
    pub daemon: TestDaemon,
    pub provider: Provider,
    pub workspace: PathBuf,
    pub home: tempfile::TempDir,
}

impl Env {
    /// Start an isolated daemon pointed at `provider`. `setup(home, workspace)` runs
    /// BEFORE boot (write plugins, skills, hook scripts, extra config...).
    pub fn start(
        provider: Provider,
        extra_env: &[(&str, &str)],
        setup: impl FnOnce(&Path, &Path),
    ) -> Env {
        let home = temp_dir("home");
        let workspace = home.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        daemon_process::write_local_provider_route_config(home.path(), &provider.base_url(), MODEL);
        setup(home.path(), &workspace);
        let peer_root = home.path().join("peer-teams");
        let mut env: Vec<(&str, String)> = vec![(
            "RIGHTKIT_PEER_TEAM_STORE_ROOT",
            peer_root.to_string_lossy().to_string(),
        )];
        for (k, v) in extra_env {
            env.push((k, v.to_string()));
        }
        let refs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let daemon = daemon_process::start_daemon(home.path(), &refs);
        Env {
            home,
            workspace,
            daemon,
            provider,
        }
    }

    pub fn url(&self) -> String {
        self.daemon.url()
    }
    pub fn token(&self) -> &str {
        &self.daemon.startup.token
    }
    pub fn db_path(&self) -> PathBuf {
        PathBuf::from(&self.daemon.startup.database_path)
    }

    pub fn get(&self, path: &str) -> (u16, Value) {
        let r = daemon_process::send(
            &rightkit_http::Request::get(format!("{}{path}", self.url())).bearer(self.token()),
        )
        .expect("GET");
        (r.status, r.json().unwrap_or(Value::Null))
    }
    pub fn post(&self, path: &str, body: Value) -> (u16, Value) {
        daemon_process::post_json(&self.url(), path, self.token(), &body)
    }
    pub fn send(&self, method: &str, path: &str, body: Value) -> (u16, Value) {
        let m = match method {
            "GET" => rightkit_http::Method::Get,
            "POST" => rightkit_http::Method::Post,
            "PUT" => rightkit_http::Method::Put,
            "PATCH" => rightkit_http::Method::Patch,
            "DELETE" => rightkit_http::Method::Delete,
            other => panic!("method {other}"),
        };
        let request = rightkit_http::Request::new(m, format!("{}{path}", self.url()))
            .bearer(self.token())
            .json(&body)
            .expect("JSON request body");
        let r = daemon_process::send(&request).expect("request");
        (r.status, r.json().unwrap_or(Value::Null))
    }

    pub fn create_session(&self, id: &str) {
        let (s, b) = self.post(
            "/sessions",
            json!({"id": id, "workspaceRoot": self.workspace, "title": id, "permissionMode": "bypass"}),
        );
        assert!((200..300).contains(&s), "create session {id}: {s} {b}");
    }

    /// Create a session whose body is `{"id","workspaceRoot","title"}` merged with `extra`.
    pub fn create_session_with(&self, id: &str, extra: Value) {
        let mut body = json!({"id": id, "workspaceRoot": self.workspace, "title": id});
        for (k, v) in extra.as_object().cloned().unwrap_or_default() {
            body[k] = v;
        }
        let (s, b) = self.post("/sessions", body);
        assert!((200..300).contains(&s), "create session {id}: {s} {b}");
    }

    /// Execute a slash command through the daemon; returns (success, output).
    pub fn slash(&self, input: &str) -> (bool, String) {
        let (s, b) = self.post("/commands/execute", json!({"input": input}));
        assert_eq!(s, 200, "slash {input}: {b}");
        (
            b["success"].as_bool().unwrap_or(false),
            b["output"].as_str().unwrap_or_default().to_string(),
        )
    }

    /// Kill the daemon and boot a fresh one on the same home/db (same provider).
    pub fn restart(&mut self, extra_env: &[(&str, &str)]) {
        let peer_root = self.home.path().join("peer-teams");
        let mut env: Vec<(&str, String)> = vec![(
            "RIGHTKIT_PEER_TEAM_STORE_ROOT",
            peer_root.to_string_lossy().to_string(),
        )];
        for (k, v) in extra_env {
            env.push((k, v.to_string()));
        }
        let refs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let home = self.home.path().to_path_buf();
        // Old daemon must be gone (and its db released) before the new one boots.
        self.daemon.stop();
        self.daemon = daemon_process::start_daemon(&home, &refs);
    }

    pub fn say(&self, id: &str, text: &str) {
        let (s, b) = self.post(&format!("/sessions/{id}/messages"), json!({"text": text}));
        assert!((200..300).contains(&s), "message to {id}: {s} {b}");
    }

    /// Durable history rows of `kind` for a session.
    pub fn events(&self, id: &str, kind: &str) -> Vec<Value> {
        match self
            .get(&format!("/sessions/{id}/events/history?kind={kind}"))
            .1
        {
            Value::Array(a) => a,
            _ => vec![],
        }
    }

    /// Wait until `n` turns have durably finished; returns the last turn status.
    pub fn wait_turns(&self, id: &str, n: usize) -> String {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let rows = self.events(id, "sessionTurnFinished");
            if rows.len() >= n {
                return rows.last().unwrap()["status"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
            }
            assert!(
                Instant::now() < deadline,
                "session {id} did not settle {n} turn(s); provider saw {} request(s); history={}",
                self.provider.count(),
                self.get(&format!("/sessions/{id}/events/history")).1
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Create a session, send one message, wait for it to settle.
    pub fn turn(&self, id: &str, text: &str) -> String {
        self.create_session(id);
        self.say(id, text);
        self.wait_turns(id, 1)
    }

    /// Poll `f` until it returns Some, or panic with `what`.
    pub fn eventually<T>(&self, what: &str, secs: u64, mut f: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            if let Some(v) = f() {
                return v;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Poll a closure (no env) until Some.
pub fn wait_for<T>(what: &str, secs: u64, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Append raw TOML to the isolated home's `<app root>/config.toml`.
pub fn append_config(home: &Path, toml: &str) {
    let p = crate::common::app_root(&home).join("config.toml");
    let mut cur = std::fs::read_to_string(&p).unwrap_or_default();
    cur.push('\n');
    cur.push_str(toml);
    std::fs::write(p, cur).unwrap();
}

/// Forward-slash form of a path, safe inside `cmd /C` and `sh -c` command lines.
pub fn slash_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}
