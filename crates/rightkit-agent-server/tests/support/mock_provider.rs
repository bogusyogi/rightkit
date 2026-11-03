//! A tiny, real, independently observable OpenAI-compatible provider fixture
//! used by qualification drivers. It runs its own process-local HTTP server
//! (`tiny_http`, a real dev-dependency) on a loopback port picked by the OS,
//! and counts requests to `/chat/completions` in an `Arc<AtomicUsize>` that
//! the test can inspect directly. CodeRight never asserts its own dispatch
//! count from inside this fixture — the count is observed independently.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tiny_http::{Header, Response, Server};

pub struct MockProvider {
    pub port: u16,
    /// Number of POST requests this fixture has received whose path ends in
    /// `/chat/completions` — i.e. real provider dispatches.
    pub dispatch_count: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl MockProvider {
    /// Bind a loopback listener on an OS-assigned port and start serving a
    /// minimal OpenAI-compatible `/v1/chat/completions` that always answers
    /// with a short plain-text completion (and a 404 for anything else,
    /// which correctly makes model-catalog discovery fall back to the
    /// caller's manually configured model list).
    pub fn start() -> Self {
        Self::start_with(|_request_number| sse_text_completion_body())
    }

    /// Like [`Self::start`], but the first `/chat/completions` request gets a
    /// single `write_file` tool call, and every request after that gets a
    /// plain-text completion — enough to drive one real tool-call turn
    /// through the native model loop end to end.
    pub fn start_with_one_tool_call(
        tool_call_id: &str,
        tool_name: &str,
        arguments_json: &str,
    ) -> Self {
        let tool_call_id = tool_call_id.to_string();
        let tool_name = tool_name.to_string();
        let arguments_json = arguments_json.to_string();
        Self::start_with(move |request_number| {
            if request_number == 1 {
                sse_tool_call_body(&tool_call_id, &tool_name, &arguments_json)
            } else {
                sse_text_completion_body()
            }
        })
    }

    /// Hold a real provider request open until fixture cleanup.
    pub fn start_stalled() -> Self {
        Self::start_internal(|_| String::new(), true)
    }

    fn start_with<F>(body_for_request: F) -> Self
    where
        F: FnMut(usize) -> String + Send + 'static,
    {
        Self::start_internal(body_for_request, false)
    }

    fn start_internal<F>(mut body_for_request: F, stall: bool) -> Self
    where
        F: FnMut(usize) -> String + Send + 'static,
    {
        let server = Server::http("127.0.0.1:0").expect("bind mock provider listener");
        let addr = server.server_addr();
        let port = match addr {
            tiny_http::ListenAddr::IP(socket_addr) => socket_addr.port(),
            _ => panic!("mock provider must bind a loopback IP address"),
        };
        let dispatch_count = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let thread_requests = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_count = dispatch_count.clone();
        let thread_stop = stop.clone();
        let handle = std::thread::spawn(move || {
            while !thread_stop.load(Ordering::SeqCst) {
                match server.recv_timeout(Duration::from_millis(100)) {
                    Ok(Some(mut request)) => {
                        let url = request.url().to_string();
                        if url.ends_with("/chat/completions") {
                            let body: serde_json::Value =
                                serde_json::from_reader(request.as_reader())
                                    .expect("provider request is JSON");
                            thread_requests.lock().unwrap().push(body);
                            let request_number = thread_count.fetch_add(1, Ordering::SeqCst) + 1;
                            if stall {
                                while !thread_stop.load(Ordering::SeqCst) {
                                    std::thread::sleep(Duration::from_millis(50));
                                }
                                break;
                            }
                            let body = body_for_request(request_number);
                            let header =
                                Header::from_bytes(&b"Content-Type"[..], &b"text/event-stream"[..])
                                    .expect("valid content-type header");
                            let response = Response::from_string(body).with_header(header);
                            let _ = request.respond(response);
                        } else {
                            let _ = request
                                .respond(Response::from_string("not found").with_status_code(404));
                        }
                    }
                    Ok(None) => continue,
                    Err(_) => break,
                }
            }
        });
        Self {
            port,
            dispatch_count,
            requests,
            stop,
            handle: Some(handle),
        }
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }

    pub fn dispatches(&self) -> usize {
        self.dispatch_count.load(Ordering::SeqCst)
    }

    pub fn requests(&self) -> Vec<serde_json::Value> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for MockProvider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn sse_text_completion_body() -> String {
    // A minimal, spec-shaped OpenAI-compatible streaming completion: one
    // content delta, a terminal finish_reason + usage chunk, then [DONE].
    concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],",
        "\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1}}\n\n",
        "data: [DONE]\n\n",
    )
    .to_string()
}

fn sse_tool_call_body(tool_call_id: &str, tool_name: &str, arguments_json: &str) -> String {
    // One complete tool-call delta (id + name + fully-formed argument JSON
    // in a single fragment), then a `finish_reason: "tool_calls"` chunk.
    let escaped_args = serde_json_escape(arguments_json);
    format!(
        "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"{tool_call_id}\",\
         \"function\":{{\"name\":\"{tool_name}\",\"arguments\":\"{escaped_args}\"}}}}]}},\
         \"finish_reason\":null}}]}}\n\n\
         data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}],\
         \"usage\":{{\"prompt_tokens\":1,\"completion_tokens\":1}}}}\n\n\
         data: [DONE]\n\n"
    )
}

/// Escape a JSON string so it can be embedded as the value of another JSON
/// string field (arguments are wire-encoded as a JSON string containing
/// JSON, exactly like a real OpenAI-compatible provider would send).
fn serde_json_escape(raw: &str) -> String {
    serde_json::to_string(raw)
        .expect("string always encodes")
        .trim_start_matches('"')
        .trim_end_matches('"')
        .to_string()
}
