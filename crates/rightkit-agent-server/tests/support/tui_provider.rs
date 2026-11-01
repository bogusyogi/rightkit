//! Scripted OpenAI-compatible streaming provider for end-to-end TUI tests.
//!
//! Unlike `mock_provider.rs` (one canned answer, serial server) this fixture:
//! - serves each `/chat/completions` request on its own thread over a raw
//!   socket with per-chunk flushing, so one held response never blocks another
//!   request and partial streams are really delivered;
//! - plays a per-request script (reasoning + text deltas, tool calls, usage
//!   including cached prompt tokens);
//! - can HOLD a response open mid-stream until the test releases it, which is
//!   how "turn in flight" scenarios are made deterministic;
//! - records every request body so tests assert what the MODEL received.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

#[derive(Clone, Default)]
pub struct Step {
    pub reasoning: Option<String>,
    /// Reasoning delta emitted AFTER the answer text (late summary).
    pub late_reasoning: Option<String>,
    pub text: Option<String>,
    /// (call id, tool name, arguments JSON text)
    pub tool_call: Option<(String, String, String)>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
    /// Hold the stream open after the first content chunk until released.
    pub hold: bool,
}

impl Step {
    pub fn text(text: &str) -> Self {
        Step {
            text: Some(text.to_string()),
            prompt_tokens: 1200,
            completion_tokens: 20,
            ..Step::default()
        }
    }
    pub fn tool(id: &str, name: &str, args: Value) -> Self {
        Step {
            tool_call: Some((id.to_string(), name.to_string(), args.to_string())),
            prompt_tokens: 1200,
            completion_tokens: 20,
            ..Step::default()
        }
    }
    pub fn with_reasoning(mut self, reasoning: &str) -> Self {
        self.reasoning = Some(reasoning.to_string());
        self
    }
    pub fn with_usage(mut self, prompt: u64, completion: u64, cached: u64) -> Self {
        self.prompt_tokens = prompt;
        self.completion_tokens = completion;
        self.cached_tokens = cached;
        self
    }
    pub fn with_late_reasoning(mut self, reasoning: &str) -> Self {
        self.late_reasoning = Some(reasoning.to_string());
        self
    }
    pub fn held(mut self) -> Self {
        self.hold = true;
        self
    }
}

pub struct ScriptedProvider {
    port: u16,
    requests: Arc<Mutex<Vec<Value>>>,
    raw: Arc<Mutex<Vec<String>>>,
    count: Arc<AtomicUsize>,
    releases: Arc<Mutex<Vec<Arc<AtomicBool>>>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

fn sse(chunk: Value) -> String {
    format!("data: {chunk}\n\n")
}

/// (head, tail): `head` is sent immediately, `tail` after the hold is released.
fn render(step: &Step) -> (String, String) {
    let mut head = String::new();
    if let Some(r) = &step.reasoning {
        head += &sse(json!({"choices":[{"delta":{"reasoning_content": r},"finish_reason":null}]}));
    }
    if let Some(t) = &step.text {
        // Split into two deltas so streaming is observable.
        let mid = t
            .char_indices()
            .map(|(i, _)| i)
            .nth(t.chars().count() / 2)
            .unwrap_or(0);
        head += &sse(json!({"choices":[{"delta":{"content": &t[..mid]},"finish_reason":null}]}));
        let rest = sse(json!({"choices":[{"delta":{"content": &t[mid..]},"finish_reason":null}]}));
        if step.hold {
            return (head, finish(step, rest));
        }
        head += &rest;
    }
    if let Some(r) = &step.late_reasoning {
        head += &sse(json!({"choices":[{"delta":{"reasoning_content": r},"finish_reason":null}]}));
    }
    if let Some((id, name, args)) = &step.tool_call {
        head += &sse(json!({"choices":[{"delta":{"tool_calls":[{
            "index":0,"id":id,"type":"function","function":{"name":name,"arguments":args}}]},
            "finish_reason":null}]}));
    }
    if step.hold {
        return (head, finish(step, String::new()));
    }
    let tail = finish(step, String::new());
    head += &tail;
    (head, String::new())
}

fn finish(step: &Step, prefix: String) -> String {
    let reason = if step.tool_call.is_some() {
        "tool_calls"
    } else {
        "stop"
    };
    format!(
        "{prefix}{}data: [DONE]\n\n",
        sse(json!({
            "choices":[{"delta":{},"finish_reason":reason}],
            "usage":{
                "prompt_tokens": step.prompt_tokens,
                "completion_tokens": step.completion_tokens,
                "total_tokens": step.prompt_tokens + step.completion_tokens,
                "prompt_tokens_details": {"cached_tokens": step.cached_tokens}
            }
        }))
    )
}

fn write_chunk(stream: &mut TcpStream, data: &[u8]) -> std::io::Result<()> {
    if data.is_empty() {
        return Ok(());
    }
    stream.write_all(format!("{:x}\r\n", data.len()).as_bytes())?;
    stream.write_all(data)?;
    stream.write_all(b"\r\n")?;
    stream.flush()
}

/// Read one HTTP request: (request line, body).
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
    let request_line = head.lines().next()?.to_string();
    let length = head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).ok()?;
    Some((request_line, String::from_utf8_lossy(&body).to_string()))
}

impl ScriptedProvider {
    /// `script[i]` answers the i-th `/chat/completions` request; later
    /// requests get a plain "ok" completion. Responses are written straight to
    /// the socket and flushed per chunk, so a held step really delivers its
    /// first chunks to the client while the stream stays open.
    pub fn start(script: Vec<Step>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind scripted provider");
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let raw: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let count = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let releases: Arc<Mutex<Vec<Arc<AtomicBool>>>> = Arc::new(Mutex::new(
            (0..script.len().max(16))
                .map(|_| Arc::new(AtomicBool::new(false)))
                .collect(),
        ));
        let script = Arc::new(script);
        let accept = {
            let (requests, raw, count, stop, releases) = (
                requests.clone(),
                raw.clone(),
                count.clone(),
                stop.clone(),
                releases.clone(),
            );
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let mut stream = match listener.accept() {
                        Ok((stream, _)) => stream,
                        Err(_) => {
                            std::thread::sleep(Duration::from_millis(20));
                            continue;
                        }
                    };
                    let (requests, raw, count, stop, releases, script) = (
                        requests.clone(),
                        raw.clone(),
                        count.clone(),
                        stop.clone(),
                        releases.clone(),
                        script.clone(),
                    );
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
                        // Number + record under one lock so index order == arrival order.
                        let index = {
                            let mut guard = requests.lock().unwrap();
                            guard.push(body);
                            raw.lock().unwrap().push(text);
                            count.fetch_add(1, Ordering::SeqCst);
                            guard.len() - 1
                        };
                        let step = script
                            .get(index)
                            .cloned()
                            .unwrap_or_else(|| Step::text("ok"));
                        let (head, tail) = render(&step);
                        let _ = stream.write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                        );
                        if write_chunk(&mut stream, head.as_bytes()).is_err() {
                            return;
                        }
                        if step.hold {
                            let gate = releases.lock().unwrap()[index].clone();
                            while !gate.load(Ordering::SeqCst) {
                                if stop.load(Ordering::SeqCst) {
                                    return;
                                }
                                std::thread::sleep(Duration::from_millis(25));
                            }
                        }
                        if write_chunk(&mut stream, tail.as_bytes()).is_err() {
                            return;
                        }
                        let _ = stream.write_all(b"0\r\n\r\n");
                        let _ = stream.flush();
                    });
                }
            })
        };
        Self {
            port,
            requests,
            raw,
            count,
            releases,
            stop,
            handle: Some(accept),
        }
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }

    pub fn request_count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }

    pub(crate) fn request_counter(&self) -> Arc<AtomicUsize> {
        self.count.clone()
    }

    /// Exact request bodies as received on the wire.
    pub fn raw_requests(&self) -> Vec<String> {
        self.raw.lock().unwrap().clone()
    }

    pub fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }

    /// Let a held step (by 0-based request index) finish its stream.
    pub fn release(&self, index: usize) {
        self.releases.lock().unwrap()[index].store(true, Ordering::SeqCst);
    }
}

impl Drop for ScriptedProvider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
