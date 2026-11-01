//! Shared helpers for the `e2e_tools_*` journeys (tools / storage / mcp / media / history crates).
//!
//! The REAL `coderight run` binary is driven against a *dynamic* scripted provider: a closure
//! sees every request body the model would see (including the tool results of earlier steps)
//! and answers with the next tool call or final text. Isolated home + temp git workspace.
#![allow(dead_code)]

pub use super::e2e_cli as cli;
pub use cli::{json, messages, Chat, Out, Value};

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempfile::TempDir;

pub const MODEL: &str = cli::MODEL;

type Brain = dyn Fn(&Value, usize) -> Chat + Send + Sync;

/// Provider whose answer is computed from the request the model received.
pub struct Brainy {
    port: u16,
    seen: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    first: Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

fn sse(chunk: Value) -> String {
    format!("data: {chunk}\n\n")
}

fn render(c: &Chat) -> String {
    let mut out = String::new();
    if let Some(t) = &c.text {
        out += &sse(json!({"choices":[{"delta":{"content": t},"finish_reason":null}]}));
    }
    for (i, (id, name, args)) in c.tool_calls.iter().enumerate() {
        out += &sse(json!({"choices":[{"delta":{"tool_calls":[{
            "index":i,"id":id,"type":"function","function":{"name":name,"arguments":args}}]},
            "finish_reason":null}]}));
    }
    let reason = if c.tool_calls.is_empty() {
        "stop"
    } else {
        "tool_calls"
    };
    out += &sse(json!({"choices":[{"delta":{},"finish_reason":reason}],
        "usage":{"prompt_tokens":c.prompt,"completion_tokens":c.completion,
                 "total_tokens":c.prompt+c.completion,
                 "prompt_tokens_details":{"cached_tokens":0}}}));
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

impl Brainy {
    pub fn start(brain: impl Fn(&Value, usize) -> Chat + Send + Sync + 'static) -> Self {
        let brain: Arc<Brain> = Arc::new(brain);
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind provider");
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let next = Arc::new(AtomicUsize::new(0));
        let first: Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>> = Arc::new(Mutex::new(None));
        let (seen2, stop2, first2) = (seen.clone(), stop.clone(), first.clone());
        let handle = std::thread::spawn(move || {
            while !stop2.load(Ordering::SeqCst) {
                let mut stream = match listener.accept() {
                    Ok((s, _)) => s,
                    Err(_) => {
                        std::thread::sleep(Duration::from_millis(15));
                        continue;
                    }
                };
                let (seen, next, brain, first) =
                    (seen2.clone(), next.clone(), brain.clone(), first2.clone());
                std::thread::spawn(move || {
                    stream.set_nonblocking(false).ok();
                    let Some((line, text)) = read_request(&mut stream) else {
                        return;
                    };
                    if !line.contains("/chat/completions") {
                        let _ = stream.write_all(
                            b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\nConnection: close\r\n\r\nnot found",
                        );
                        return;
                    }
                    let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                    let hook = first.lock().unwrap().take();
                    if let Some(hook) = hook {
                        hook();
                    }
                    let index = next.fetch_add(1, Ordering::SeqCst);
                    seen.lock().unwrap().push(body.clone());
                    let reply = render(&brain(&body, index));
                    let _ = stream.write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                            reply.len()
                        )
                        .as_bytes(),
                    );
                    let _ = stream.flush();
                });
            }
        });
        Brainy {
            port,
            seen,
            stop,
            first,
            handle: Some(handle),
        }
    }

    /// Run `f` once, on the provider thread, when the FIRST model request arrives (the session
    /// and its execution root already exist by then, the first tool call has not run yet).
    pub fn on_first_request(&self, f: impl FnOnce() + Send + 'static) {
        *self.first.lock().unwrap() = Some(Box::new(f));
    }

    /// Fixed script: step `i` answers the i-th request, then "done".
    pub fn script(steps: Vec<Chat>) -> Self {
        Self::start(move |_, i| steps.get(i).cloned().unwrap_or_else(|| Chat::text("done")))
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }
    pub fn requests(&self) -> Vec<Value> {
        self.seen.lock().unwrap().clone()
    }
    /// Every tool-role message the model was shown, in order of first appearance
    /// (the final request carries all of them).
    pub fn tool_results(&self) -> Vec<String> {
        self.requests().last().map(tool_results).unwrap_or_default()
    }
}

impl Drop for Brainy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

include!("untrusted.rs");
include!("host_env.rs");

pub fn text_of(message: &Value) -> String {
    match &message["content"] {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Text of every tool-role message in one request.
pub fn tool_results(req: &Value) -> Vec<String> {
    messages(req, "tool")
        .into_iter()
        .map(|m| unwrap_untrusted(&text_of(m)).to_string())
        .collect()
}

/// The `n`-th tool result (0-based) the model has seen in `req`.
pub fn tool_result(req: &Value, n: usize) -> String {
    tool_results(req).get(n).cloned().unwrap_or_default()
}

/// Names of the tools advertised in a request.
pub fn advertised_tools(req: &Value) -> Vec<String> {
    req["tools"]
        .as_array()
        .map(|t| {
            t.iter()
                .filter_map(|x| x["function"]["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// A tool-call chat in a sequence: `Seq` plays calls one per request, feeding earlier results
/// to later steps through closures.
pub type Step = Box<dyn Fn(&[String]) -> Chat + Send + Sync>;

pub fn call(name: &'static str, args: Value) -> Step {
    Box::new(move |_| Chat::tool("call", name, args.clone()))
}

pub fn call_with(
    name: &'static str,
    f: impl Fn(&[String]) -> Value + Send + Sync + 'static,
) -> Step {
    Box::new(move |seen| Chat::tool("call", name, f(seen)))
}

/// One model turn per step; the turn after the last step is plain text `finish`.
/// Each step gets the tool results observed so far (`results[i]` = result of step i).
pub fn sequence(steps: Vec<Step>, finish: &'static str) -> Brainy {
    Brainy::start(move |req, _| {
        let results = tool_results(req);
        match steps.get(results.len()) {
            Some(step) => {
                let mut c = step(&results);
                for (i, tc) in c.tool_calls.iter_mut().enumerate() {
                    tc.0 = format!("call_{}_{i}", results.len());
                }
                c
            }
            None => Chat::text(finish),
        }
    })
}

/// Isolated home + git workspace + provider route.
///
/// IMPORTANT: `coderight run --bypass-approvals` forces worktree isolation, so the tools do NOT
/// see the workspace root; they run in `<workspace>/.coderight/worktrees/<session>/` (created
/// empty when the session opens). `Rig::root()` resolves that directory, and the files passed to
/// `Rig::new` are seeded into it when the first model request arrives.
pub struct Rig {
    pub home: TempDir,
    pub workspace: TempDir,
    seeds: Arc<Mutex<Vec<(String, Vec<u8>)>>>,
}

fn git(dir: &Path, args: &[&str]) {
    let ok = rightkit_child_process::command_std(host_git_program())
        .args(args)
        .current_dir(dir)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(ok, "git {args:?} failed");
}

/// The directory the tools actually operate in for a `run` session on workspace `ws`.
pub fn session_root(ws: &Path) -> PathBuf {
    let worktrees = ws.join(".coderight").join("worktrees");
    if let Ok(rd) = std::fs::read_dir(&worktrees) {
        for entry in rd.flatten() {
            if entry.path().is_dir() {
                return entry.path();
            }
        }
    }
    ws.to_path_buf()
}

/// Lets a model-script closure (built before the `Rig` exists) resolve the session root later.
#[derive(Clone, Default)]
pub struct RootCell(Arc<Mutex<Option<PathBuf>>>);

impl RootCell {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn bind(&self, rig: &Rig) {
        *self.0.lock().unwrap() = Some(rig.ws());
    }
    pub fn root(&self) -> PathBuf {
        let ws = self
            .0
            .lock()
            .unwrap()
            .clone()
            .expect("RootCell not bound to a Rig");
        session_root(&ws)
    }
}

impl Rig {
    pub fn new(provider: &Brainy, files: &[(&str, &str)]) -> Self {
        Self::with_config(provider, "", files)
    }

    /// A rig whose workspace is NOT a git repository. Forced worktree isolation then falls back to
    /// running in place, so the tools see an empty directory in which a journey can `git init`
    /// itself (a versioned workspace hands the tools a pre-existing detached worktree instead).
    pub fn new_unversioned(provider: &Brainy, files: &[(&str, &str)]) -> Self {
        Self::build(provider, "", files, false)
    }

    pub fn with_config(provider: &Brainy, extra_config: &str, files: &[(&str, &str)]) -> Self {
        Self::build(provider, extra_config, files, true)
    }

    fn build(
        provider: &Brainy,
        extra_config: &str,
        files: &[(&str, &str)],
        versioned: bool,
    ) -> Self {
        // An unversioned fixture must have no enclosing Git repository.
        // Inside source checkout, forced isolation discovers the parent repo
        // and checks out CodeRight itself instead of exercising git init.
        let home = tempfile::tempdir().expect("temp test home");
        let workspace = tempfile::tempdir().expect("temp test workspace");
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
        // libgit2 (git_write commit, checkpoints) needs an author identity; HOME is the rig home.
        std::fs::write(
            home.path().join(".gitconfig"),
            "[user]\n\tname = e2e\n\temail = e2e@example.test\n",
        )
        .unwrap();
        let ws = workspace.path();
        if versioned {
            git(ws, &["init", "-q", "-b", "e2e-branch"]);
            git(ws, &["config", "user.email", "e2e@example.test"]);
            git(ws, &["config", "user.name", "e2e"]);
            git(ws, &["config", "core.autocrlf", "false"]);
            std::fs::write(ws.join("README.md"), "e2e\n").unwrap();
            for (rel, body) in files {
                let p = ws.join(rel);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(p, body).unwrap();
            }
            git(ws, &["add", "."]);
            git(ws, &["commit", "-q", "-m", "init"]);
        }

        let mut seed_list: Vec<(String, Vec<u8>)> =
            vec![("README.md".to_string(), b"e2e\n".to_vec())];
        seed_list.extend(
            files
                .iter()
                .map(|(r, b)| (r.to_string(), b.as_bytes().to_vec())),
        );
        let seeds = Arc::new(Mutex::new(seed_list));
        let (seed_ws, seed_files) = (ws.to_path_buf(), seeds.clone());
        provider.on_first_request(move || {
            let root = session_root(&seed_ws);
            for (rel, body) in seed_files.lock().unwrap().iter() {
                let p = root.join(rel);
                if let Some(parent) = p.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(&p, body);
            }
        });
        Rig {
            home,
            workspace,
            seeds,
        }
    }

    /// Add a (binary) file that will exist in the session root before the first tool call.
    pub fn seed_bytes(&self, rel: &str, body: &[u8]) {
        self.seeds
            .lock()
            .unwrap()
            .push((rel.to_string(), body.to_vec()));
    }

    pub fn ws(&self) -> PathBuf {
        self.workspace.path().to_path_buf()
    }

    /// The directory the session's tools really operate in.
    pub fn root(&self) -> PathBuf {
        session_root(self.workspace.path())
    }

    pub fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root().join(rel))
            .unwrap_or_else(|e| format!("<<missing: {e}>>"))
    }

    pub fn read_bytes(&self, rel: &str) -> Option<Vec<u8>> {
        std::fs::read(self.root().join(rel)).ok()
    }

    pub fn exists(&self, rel: &str) -> bool {
        self.root().join(rel).exists()
    }

    /// `git <args>` inside the session root (only meaningful after a tool ran `git_write init`).
    pub fn git_out(&self, args: &[&str]) -> String {
        let o = rightkit_child_process::command_std(host_git_program())
            .args(args)
            .current_dir(self.root())
            .output()
            .expect("git");
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    pub fn command(&self, args: &[&str]) -> std::process::Command {
        self.command_with_resource_profile(args, None)
    }

    /// Run with an explicit host resource profile when a journey intentionally
    /// exercises multiple concurrent process-backed capabilities.
    pub fn command_with_resource_profile(
        &self,
        args: &[&str],
        resource_profile: Option<&str>,
    ) -> std::process::Command {
        let mut c = rightkit_child_process::command_std(env!("CARGO_BIN_EXE_coderight"));
        c.env("RIGHTKIT_CONTEXT", "off");
        c.args(["run", "--model", &format!("qual-fixture::{MODEL}")])
            .args(args)
            .current_dir(self.workspace.path());
        let home = self.home.path();
        // The built-in `context7` default (`npx -y @upstash/context7-mcp`) stalls the first turn
        // on the hermetic rig; journeys that configure MCP declare their own servers.
        c.env("RIGHTKIT_DISABLE_DEFAULT_MCP", "1");
        c.envs([crate::common::suite_env(home)])
            .env("TEMP", self.workspace.path())
            .env("TMP", self.workspace.path())
            .env("RIGHTKIT_DISABLE_VENDOR_CLIS", "1")
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
        if let Some(profile) = resource_profile {
            c.env("RIGHTKIT_RESOURCE_PROFILE", profile);
        }
        restore_host_env(&mut c);
        c
    }

    /// `coderight run --stream-json --bypass-approvals <extra> "<prompt>"`.
    pub fn run(&self, prompt: &str) -> Out {
        self.run_args(&["--stream-json", "--bypass-approvals"], prompt)
    }

    /// Like [`Rig::run`] with an explicit wall-clock deadline. Only for journeys that chain
    /// many sandboxed `git` launches: on a loaded Windows host each governed launch (and the
    /// first mutation after it) was measured at 7-30 s, so a ten-step git journey legitimately
    /// exceeds the default 120 s without any step being unbounded.
    pub fn run_with_deadline(&self, prompt: &str, secs: u64) -> Out {
        let mut c =
            self.command_with_resource_profile(&["--stream-json", "--bypass-approvals"], None);
        c.arg(prompt);
        cli::finish(c, secs)
    }

    pub fn run_args(&self, args: &[&str], prompt: &str) -> Out {
        self.run_args_with_resource_profile(args, prompt, None)
    }

    pub fn run_args_with_resource_profile(
        &self,
        args: &[&str],
        prompt: &str,
        resource_profile: Option<&str>,
    ) -> Out {
        let mut c = self.command_with_resource_profile(args, resource_profile);
        c.arg(prompt);
        cli::finish(c, 120)
    }

    /// Start `run` without waiting; stdout events are timestamped as they arrive.
    pub fn spawn_live(&self, prompt: &str) -> Live {
        let mut c = self.command(&["--stream-json", "--bypass-approvals"]);
        c.arg(prompt)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let mut child = c.spawn().expect("spawn coderight run");
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if tx.send((std::time::Instant::now(), v)).is_err() {
                        break;
                    }
                }
            }
        });
        Live {
            child,
            rx,
            seen: Vec::new(),
            started: std::time::Instant::now(),
        }
    }
}

/// A running `coderight run` child; killed (and reaped) on drop.
pub struct Live {
    child: std::process::Child,
    rx: std::sync::mpsc::Receiver<(std::time::Instant, Value)>,
    pub seen: Vec<(std::time::Instant, Value)>,
    pub started: std::time::Instant,
}

impl Drop for Live {
    fn drop(&mut self) {
        crate::support::daemon_process::stop_child(&mut self.child);
    }
}

impl Live {
    /// Wait up to `secs` for a stdout event satisfying `pred`; returns its arrival time.
    pub fn wait_event(
        &mut self,
        secs: u64,
        pred: impl Fn(&Value) -> bool,
    ) -> Option<std::time::Instant> {
        if let Some((t, _)) = self.seen.iter().find(|(_, v)| pred(v)) {
            return Some(*t);
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(secs);
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return None;
            }
            match self.rx.recv_timeout(left) {
                Ok((t, v)) => {
                    let hit = pred(&v);
                    self.seen.push((t, v));
                    if hit {
                        return Some(t);
                    }
                }
                Err(_) => return None,
            }
        }
    }

    /// Wait for the child to exit; returns the instant it was observed gone and its exit code.
    pub fn wait_exit(&mut self, secs: u64) -> Option<(std::time::Instant, Option<i32>)> {
        let deadline = std::time::Instant::now() + Duration::from_secs(secs);
        loop {
            if let Ok(Some(st)) = self.child.try_wait() {
                return Some((std::time::Instant::now(), st.code()));
            }
            if std::time::Instant::now() > deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(40));
        }
    }
}

/// One tool result as the user-visible stream reports it.
#[derive(Debug, Clone)]
pub struct Res {
    /// The tool's own output text (`toolResultAdded.result.summary`).
    pub text: String,
    /// `toolResultAdded.result.isError`.
    pub error: bool,
}

/// Every `toolResultAdded` event of a `--stream-json` run, in order.
pub fn results(out: &Out) -> Vec<Res> {
    out.events()
        .iter()
        .filter(|e| e["event"] == "toolResultAdded")
        .map(|e| {
            let r = &e["data"]["result"];
            Res {
                text: r["summary"].as_str().unwrap_or_default().to_string(),
                error: r["isError"].as_bool().unwrap_or(false),
            }
        })
        .collect()
}

/// Assert the run exited 0 and produced exactly `n` tool results; returns them.
pub fn expect_results(out: &Out, n: usize) -> Vec<Res> {
    assert_eq!(out.code, Some(0), "run failed: {}", out.diag());
    let r = results(out);
    assert_eq!(
        r.len(),
        n,
        "expected {n} tool results, got {r:#?}\n{}",
        out.diag()
    );
    r
}

/// First run of ASCII digits directly after `prefix` in `text`.
pub fn number_after(text: &str, prefix: &str) -> Option<String> {
    let start = text.find(prefix)? + prefix.len();
    let digits: String = text[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    (!digits.is_empty()).then_some(digits)
}

/// Platform-portable `sleep` for the `shell` tool (PowerShell on Windows, sh elsewhere).
pub fn sleep_cmd(secs: u32) -> String {
    format!("sleep {secs}")
}

/// Does the stream carry a typed terminal-success run event?
pub fn run_completed(out: &Out) -> bool {
    out.code == Some(0)
}

/// A valid 1x1 red PNG.
pub const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x08, 0xd7, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xdd, 0x8d, 0xb0, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

/// Portable "write `msg` to stderr" command for the `shell` tool.
pub fn stderr_cmd(msg: &str) -> String {
    if cfg!(windows) {
        format!("[Console]::Error.WriteLine('{msg}')")
    } else {
        format!("echo {msg} 1>&2")
    }
}

/// True when `text` is a `[search status=... ]` footer line.
pub fn is_search_footer(line: &str) -> bool {
    line.starts_with("[search status=")
}
