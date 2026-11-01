//! Black-box CLI harness for the `e2e_engine_*` suites.
//!
//! Runs the REAL `coderight` binary (`run` / `-p`) against an independent,
//! scripted OpenAI-compatible provider, inside an isolated home + workspace.
//! The provider records every request body/header it receives so tests assert
//! on what the MODEL saw, and the child's stdout/exit code are what the user
//! (or a CI harness) sees.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub use serde_json::{json, Value};
use tempfile::TempDir;

pub const MODEL: &str = "deepseek/deepseek-v4-pro";

/// One scripted `/chat/completions` answer.
#[derive(Clone, Default)]
pub struct Chat {
    pub reasoning: Option<String>,
    pub text: Option<String>,
    /// (call id, tool name, arguments JSON text) — several = parallel calls.
    pub tool_calls: Vec<(String, String, String)>,
    pub prompt: u64,
    pub completion: u64,
    pub cached: u64,
    /// Omit the `usage` chunk entirely.
    pub no_usage: bool,
    /// Deliver tool-call arguments as several fragments (real providers do).
    pub fragment_args: bool,
    /// Report cached tokens as DeepSeek's `prompt_cache_hit_tokens` field.
    pub hit_tokens_field: bool,
}

impl Chat {
    pub fn text(t: &str) -> Self {
        Chat {
            text: Some(t.into()),
            prompt: 1200,
            completion: 20,
            ..Default::default()
        }
    }
    pub fn tool(id: &str, name: &str, args: Value) -> Self {
        Chat {
            tool_calls: vec![(id.into(), name.into(), args.to_string())],
            prompt: 1200,
            completion: 20,
            ..Default::default()
        }
    }
    pub fn tools(calls: &[(&str, &str, Value)]) -> Self {
        Chat {
            tool_calls: calls
                .iter()
                .map(|(i, n, a)| (i.to_string(), n.to_string(), a.to_string()))
                .collect(),
            prompt: 1200,
            completion: 20,
            ..Default::default()
        }
    }
    pub fn reasoning(mut self, r: &str) -> Self {
        self.reasoning = Some(r.into());
        self
    }
    pub fn usage(mut self, prompt: u64, completion: u64, cached: u64) -> Self {
        self.prompt = prompt;
        self.completion = completion;
        self.cached = cached;
        self
    }
}

#[derive(Clone)]
pub enum Reply {
    Chat(Chat),
    /// Verbatim SSE body (wire edge cases the `Chat` builder cannot express).
    Raw(String),
    /// Non-2xx answer with a JSON/text body (and optional extra headers).
    Error {
        status: u16,
        body: String,
        headers: Vec<(String, String)>,
    },
}

impl From<Chat> for Reply {
    fn from(c: Chat) -> Self {
        Reply::Chat(c)
    }
}

pub fn err(status: u16, body: Value) -> Reply {
    Reply::Error {
        status,
        body: body.to_string(),
        headers: vec![],
    }
}

fn sse(chunk: Value) -> String {
    format!("data: {chunk}\n\n")
}

fn render(c: &Chat) -> String {
    let mut out = String::new();
    if let Some(r) = &c.reasoning {
        out += &sse(json!({"choices":[{"delta":{"reasoning_content": r},"finish_reason":null}]}));
    }
    if let Some(t) = &c.text {
        let mid = t
            .char_indices()
            .map(|(i, _)| i)
            .nth(t.chars().count() / 2)
            .unwrap_or(0);
        out += &sse(json!({"choices":[{"delta":{"content": &t[..mid]},"finish_reason":null}]}));
        out += &sse(json!({"choices":[{"delta":{"content": &t[mid..]},"finish_reason":null}]}));
    }
    for (i, (id, name, args)) in c.tool_calls.iter().enumerate() {
        if c.fragment_args {
            out += &sse(json!({"choices":[{"delta":{"tool_calls":[{
                "index":i,"id":id,"type":"function","function":{"name":name,"arguments":""}}]},
                "finish_reason":null}]}));
            let chars: Vec<char> = args.chars().collect();
            for piece in chars.chunks(7) {
                let piece: String = piece.iter().collect();
                out += &sse(json!({"choices":[{"delta":{"tool_calls":[{
                    "index":i,"function":{"arguments":piece}}]},"finish_reason":null}]}));
            }
        } else {
            out += &sse(json!({"choices":[{"delta":{"tool_calls":[{
                "index":i,"id":id,"type":"function","function":{"name":name,"arguments":args}}]},
                "finish_reason":null}]}));
        }
    }
    let reason = if c.tool_calls.is_empty() {
        "stop"
    } else {
        "tool_calls"
    };
    let mut fin = json!({"choices":[{"delta":{},"finish_reason":reason}]});
    if !c.no_usage {
        fin["usage"] = json!({
            "prompt_tokens": c.prompt, "completion_tokens": c.completion,
            "total_tokens": c.prompt + c.completion,
        });
        if c.hit_tokens_field {
            fin["usage"]["prompt_cache_hit_tokens"] = json!(c.cached);
            fin["usage"]["prompt_cache_miss_tokens"] = json!(c.prompt.saturating_sub(c.cached));
        } else {
            fin["usage"]["prompt_tokens_details"] = json!({"cached_tokens": c.cached});
        }
    }
    out += &sse(fin);
    out += "data: [DONE]\n\n";
    out
}

pub struct Seen {
    pub body: Value,
    pub raw_body: Vec<u8>,
    pub headers: Vec<(String, String)>,
}

pub struct Provider {
    port: u16,
    seen: Arc<Mutex<Vec<Seen>>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

type RawRequest = (String, Vec<(String, String)>, String);

fn read_request(stream: &mut TcpStream) -> Option<RawRequest> {
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
    let headers: Vec<(String, String)> = head
        .lines()
        .skip(1)
        .filter_map(|l| {
            l.split_once(':')
                .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        })
        .collect();
    let len = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).ok()?;
    Some((line, headers, String::from_utf8_lossy(&body).to_string()))
}

impl Provider {
    /// `script[i]` answers the i-th chat request; later requests get "ok".
    pub fn start(script: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind provider");
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let next = Arc::new(AtomicUsize::new(0));
        let script = Arc::new(script);
        let (s2, stop2) = (seen.clone(), stop.clone());
        let handle = std::thread::spawn(move || {
            while !stop2.load(Ordering::SeqCst) {
                let mut stream = match listener.accept() {
                    Ok((s, _)) => s,
                    Err(_) => {
                        std::thread::sleep(Duration::from_millis(15));
                        continue;
                    }
                };
                let (seen, script, next) = (s2.clone(), script.clone(), next.clone());
                std::thread::spawn(move || {
                    stream.set_nonblocking(false).ok();
                    let Some((line, headers, text)) = read_request(&mut stream) else {
                        return;
                    };
                    if !line.contains("/chat/completions") {
                        let _ = stream.write_all(
                            b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\nConnection: close\r\n\r\nnot found",
                        );
                        return;
                    }
                    let index = {
                        let mut g = seen.lock().unwrap();
                        g.push(Seen {
                            body: serde_json::from_str(&text).unwrap_or(Value::Null),
                            raw_body: text.into_bytes(),
                            headers,
                        });
                        next.fetch_add(1, Ordering::SeqCst)
                    };
                    let reply = script
                        .get(index)
                        .cloned()
                        .unwrap_or_else(|| Reply::Chat(Chat::text("ok")));
                    match reply {
                        Reply::Raw(body) => {
                            let _ = stream.write_all(
                                format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                    body.len()
                                )
                                .as_bytes(),
                            );
                        }
                        Reply::Chat(c) => {
                            let body = render(&c);
                            let _ = stream.write_all(
                                format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                    body.len()
                                )
                                .as_bytes(),
                            );
                        }
                        Reply::Error {
                            status,
                            body,
                            headers,
                        } => {
                            let extra: String = headers
                                .iter()
                                .map(|(k, v)| format!("{k}: {v}\r\n"))
                                .collect();
                            let _ = stream.write_all(
                                format!(
                                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                    body.len()
                                )
                                .as_bytes(),
                            );
                        }
                    }
                    let _ = stream.flush();
                });
            }
        });
        Provider {
            port,
            seen,
            stop,
            handle: Some(handle),
        }
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }
    pub fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
    /// Request bodies in arrival order.
    pub fn requests(&self) -> Vec<Value> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.body.clone())
            .collect()
    }
    /// Exact UTF-8 request body bytes received by the scripted provider.
    pub fn raw_requests(&self) -> Vec<Vec<u8>> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|seen| seen.raw_body.clone())
            .collect()
    }
    pub fn headers(&self, i: usize) -> Vec<(String, String)> {
        self.seen.lock().unwrap()[i].headers.clone()
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

pub struct Out {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Out {
    /// Every stdout line that parses as JSON.
    pub fn events(&self) -> Vec<Value> {
        self.stdout
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }
    pub fn kinds(&self) -> Vec<String> {
        self.events().iter().map(kind_of).collect()
    }
    pub fn diag(&self) -> String {
        format!(
            "code={:?}\n--stdout--\n{}\n--stderr--\n{}",
            self.code, self.stdout, self.stderr
        )
    }
}

pub fn kind_of(e: &Value) -> String {
    ["$kind", "kind", "event", "type"]
        .iter()
        .find_map(|k| e.get(*k).and_then(|v| v.as_str()))
        .unwrap_or("?")
        .to_string()
}

/// Isolated home + git workspace + route config pointing at a provider.
pub struct Cli {
    pub home: TempDir,
    pub workspace: TempDir,
}

pub fn git(dir: &Path, args: &[&str]) {
    let ok =
        rightkit_child_process::command_std(crate::support::daemon_process::host_git_program())
            .args(args)
            .current_dir(dir)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
    assert!(ok, "git {args:?} failed");
}

impl Cli {
    pub fn new(provider: &Provider) -> Self {
        Self::with_config(provider, "", &[])
    }

    pub fn with_config(provider: &Provider, extra_config: &str, files: &[(&str, &str)]) -> Self {
        let home = TempDir::new().unwrap();
        let workspace = TempDir::new().unwrap();
        let cfg = crate::common::app_root(home.path());
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::write(
            cfg.join("config.toml"),
            format!(
                "[model]\ndefault-model = \"{MODEL}\"\n\n[provider-routes.qual-fixture]\n\
                 display-name = \"Qualification fixture\"\nbase-url = \"{}\"\nmodel = \"{MODEL}\"\n\
                 api-key-required = false\n{extra_config}",
                provider.base_url()
            ),
        )
        .unwrap();
        let ws = workspace.path();
        git(ws, &["init", "-q", "-b", "e2e-branch"]);
        git(ws, &["config", "user.email", "e2e@example.test"]);
        git(ws, &["config", "user.name", "e2e"]);
        std::fs::write(ws.join("README.md"), "e2e\n").unwrap();
        for (rel, body) in files {
            let p = ws.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        git(ws, &["add", "."]);
        git(ws, &["commit", "-q", "-m", "init"]);
        Cli { home, workspace }
    }

    pub fn ws(&self) -> PathBuf {
        self.workspace.path().to_path_buf()
    }

    /// `coderight run <args>` with context off (model comes from config).
    pub fn command(&self, args: &[&str]) -> Command {
        let mut c = self.base();
        c.env("RIGHTKIT_CONTEXT", "off");
        c.args(["run"]).args(args);
        c
    }

    /// Bare `coderight` command in the isolated environment, cwd = workspace.
    pub fn base(&self) -> Command {
        let mut c = rightkit_child_process::command_std(env!("CARGO_BIN_EXE_coderight"));
        c.current_dir(self.workspace.path());
        let home = self.home.path();
        c.envs([crate::common::suite_env(home)])
            .env("RIGHTKIT_DISABLE_VENDOR_CLIS", "1")
            .env("RIGHTKIT_DISABLE_DEFAULT_MCP", "1")
            .env("HOME", home)
            .env("USERPROFILE", home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_DATA_HOME", home.join("data"))
            .env("RIGHTKIT_DISABLE_OS_KEYCHAIN", "1")
            .env("RIGHTKIT_QUAL_ISOLATED_MEMBRANE", "1")
            .env("RIGHTKIT_NO_AUTOSTART", "1")
            .env_remove("RIGHTKIT_DAEMON_URL")
            .env_remove("RIGHTKIT_DAEMON_TOKEN")
            .env_remove("RIGHTKIT_SERVE_FORCE");
        crate::support::daemon_process::restore_host_env(&mut c);
        c
    }

    /// Run `coderight <args>` (no `run` prefix) with a hard timeout.
    pub fn exec(&self, args: &[&str], env: &[(&str, &str)]) -> Out {
        let mut c = self.base();
        c.args(args);
        for (k, v) in env {
            c.env(k, v);
        }
        finish(c, 60)
    }

    /// `coderight run <args> "<prompt>"` with a hard timeout.
    pub fn run(&self, args: &[&str], prompt: &str) -> Out {
        self.run_env(args, prompt, &[])
    }

    pub fn run_env(&self, args: &[&str], prompt: &str, env: &[(&str, &str)]) -> Out {
        let mut c = self.command(args);
        c.arg(prompt);
        for (k, v) in env {
            c.env(k, v);
        }
        finish(c, 90)
    }
}

/// Reap the real CLI and its descendants on success, timeout, or unwind.
struct CliChild(std::process::Child);

impl Drop for CliChild {
    fn drop(&mut self) {
        crate::support::daemon_process::stop_child(&mut self.0);
    }
}

pub fn finish(mut c: Command, secs: u64) -> Out {
    c.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = CliChild(c.spawn().expect("spawn coderight"));
    let mut so = child.0.stdout.take().unwrap();
    let mut se = child.0.stderr.take().unwrap();
    let t1 = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = so.read_to_string(&mut s);
        s
    });
    let t2 = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = se.read_to_string(&mut s);
        s
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    let code = loop {
        if let Some(st) = child.0.try_wait().unwrap() {
            break st.code();
        }
        if std::time::Instant::now() > deadline {
            crate::support::daemon_process::stop_child(&mut child.0);
            break Some(-999);
        }
        std::thread::sleep(Duration::from_millis(40));
    };
    // Stop before joining pipe readers: a surviving descendant can hold
    // either pipe open as well as files beneath the fixture TempDir.
    drop(child);
    Out {
        code,
        stdout: t1.join().unwrap(),
        stderr: t2.join().unwrap(),
    }
}

impl Cli {
    /// Contents of `rel` as written by a tool. Sessions run in an execution
    /// worktree under `<workspace>/.coderight/worktrees/<session>/`; fall back
    /// to the workspace root itself.
    pub fn written(&self, rel: &str) -> Option<String> {
        let root = self.workspace.path().join(".coderight").join("worktrees");
        if let Ok(rd) = std::fs::read_dir(&root) {
            for entry in rd.flatten() {
                if let Ok(s) = std::fs::read_to_string(entry.path().join(rel)) {
                    return Some(s);
                }
            }
        }
        std::fs::read_to_string(self.workspace.path().join(rel)).ok()
    }
}

/// `data` payload of a CLI `stream_event.v1` record (the daemon event).
pub fn data(e: &Value) -> &Value {
    e.get("data").unwrap_or(e)
}

/// Events of one kind, in stream order.
pub fn of_kind<'a>(events: &'a [Value], kind: &str) -> Vec<&'a Value> {
    events.iter().filter(|e| kind_of(e) == kind).collect()
}

pub fn first_index(kinds: &[String], kind: &str) -> usize {
    kinds
        .iter()
        .position(|k| k == kind)
        .unwrap_or_else(|| panic!("no {kind} in {kinds:?}"))
}

/// Names of the tools advertised in a provider request.
pub fn tool_names(req: &Value) -> Vec<String> {
    req["tools"]
        .as_array()
        .map(|t| {
            t.iter()
                .filter_map(|x| x["function"]["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Concatenated text of every system message of a provider request.
pub fn system_text(req: &Value) -> String {
    messages(req, "system")
        .iter()
        .map(|m| m["content"].to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// All `messages` of a request with a given role.
pub fn messages<'a>(req: &'a Value, role: &str) -> Vec<&'a Value> {
    req["messages"]
        .as_array()
        .map(|m| m.iter().filter(|x| x["role"] == role).collect())
        .unwrap_or_default()
}
