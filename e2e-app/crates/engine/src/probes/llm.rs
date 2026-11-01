//! rightkit-llm: chat requests over real HTTP to loopback servers speaking the Anthropic Messages and OpenAI-compatible
//! chat-completions shapes. Each provider's exact headers and JSON body are checked on the server side; the parsed reply
//! (text, tool input, usage), SSE streaming assembly, failover from a 503 route to a healthy one, and a 401 surfaced as a
//! typed status error are asserted.
//!
//! Needs: nothing beyond the engine's `rightkit-llm` dependency. The `cli` feature (CliProvider) is not exercised: it drives
//! an installed vendor CLI, covered by the rightkit-agents probe's discovery.
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rightkit_llm::anthropic::AnthropicProvider;
use rightkit_llm::credentials::StaticCredentials;
use rightkit_llm::failover::{FailoverChain, FailoverPolicy, Route};
use rightkit_llm::openai::OpenAiCompatProvider;
use rightkit_llm::{
    AddressPolicy, ChatRequest, Client, ClientConfig, LlmError, Message, Provider, RetryPolicy,
    StreamEvent, ToolSpec,
};
use serde_json::{json, Value};

#[derive(Clone, Debug)]
struct Seen {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

struct Reply {
    status: u16,
    content_type: &'static str,
    body: String,
}

fn json_reply(status: u16, v: Value) -> Reply {
    Reply {
        status,
        content_type: "application/json",
        body: v.to_string(),
    }
}
fn sse_reply(frames: &[&str]) -> Reply {
    Reply {
        status: 200,
        content_type: "text/event-stream",
        body: frames.concat(),
    }
}

struct Server {
    base: String,
    addr: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    fn start(handler: impl Fn(&Seen, usize) -> Reply + Send + 'static) -> Result<Server, String> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("bind: {e}"))?;
        let addr = listener
            .local_addr()
            .map_err(|e| e.to_string())?
            .to_string();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (s2, st2) = (seen.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            for conn in listener.incoming() {
                if st2.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut c) = conn else { continue };
                let _ = c.set_read_timeout(Some(Duration::from_secs(5)));
                let Some(req) = read_request(&mut c) else {
                    continue;
                };
                let n = {
                    let mut g = s2.lock().unwrap();
                    g.push(req.clone());
                    g.len() - 1
                };
                let r = handler(&req, n);
                let head = format!(
                    "HTTP/1.1 {} X\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    r.status,
                    r.content_type,
                    r.body.len()
                );
                let _ = c.write_all(head.as_bytes());
                let _ = c.write_all(r.body.as_bytes());
            }
        });
        Ok(Server {
            base: format!("http://{addr}"),
            addr,
            seen,
            stop,
            thread: Some(thread),
        })
    }
    fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(&self.addr);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn read_request(c: &mut TcpStream) -> Option<Seen> {
    let mut buf = Vec::new();
    let mut b = [0u8; 4096];
    let head_end = loop {
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
        let n = c.read(&mut b).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&b[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let (method, path) = (first.next()?.to_string(), first.next()?.to_string());
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| {
            l.split_once(':')
                .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        })
        .collect();
    let want: usize = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < want {
        let n = c.read(&mut b).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&b[..n]);
    }
    Some(Seen {
        method,
        path,
        headers,
        body,
    })
}

fn client() -> Client {
    Client::new(ClientConfig {
        retry: RetryPolicy::none(),
        ..ClientConfig::default().address_policy(AddressPolicy::default().allow_loopback())
    })
}

fn creds(id: &str) -> Arc<StaticCredentials> {
    Arc::new(StaticCredentials::default().with(id, "e2e-secret"))
}

pub fn probe(_ctx: &super::Ctx) -> Result<Value, String> {
    // ---- Anthropic Messages: complete with a tool, exact request on the wire ------------------------------------------
    let server = Server::start(|_, _| {
        json_reply(
            200,
            json!({"id": "msg_1", "model": "claude-test", "stop_reason": "tool_use",
                   "content": [{"type": "text", "text": "Looking."}, {"type": "tool_use", "id": "call_1", "name": "lookup", "input": {"q": "rust"}}],
                   "usage": {"input_tokens": 10, "output_tokens": 4}}),
        )
    })?;
    let provider = AnthropicProvider::new(&server.base, client(), creds("anthropic"));
    let mut req = ChatRequest::simple("claude-test", "You are concise", "Find Rust");
    req.max_tokens = 77;
    req.tools.push(ToolSpec {
        name: "lookup".into(),
        description: "Find item".into(),
        input_schema: json!({"type": "object"}),
        strict: false,
    });
    let answer = provider
        .complete(&req)
        .map_err(|e| format!("anthropic complete: {e:?}"))?;
    if answer.text() != "Looking."
        || answer.tool_input("lookup").map(|v| v["q"].clone()) != Some(json!("rust"))
    {
        return Err(format!(
            "anthropic reply parsed wrong: text {:?}",
            answer.text()
        ));
    }
    if answer.usage.input_tokens != 10 || answer.usage.output_tokens != 4 {
        return Err("anthropic usage not parsed".into());
    }
    let seen = server.requests();
    let r = seen.first().ok_or("anthropic server saw no request")?;
    if r.method != "POST"
        || r.path != "/v1/messages"
        || r.header("x-api-key") != Some("e2e-secret")
        || r.header("anthropic-version") != Some("2023-06-01")
    {
        return Err(format!(
            "anthropic request line/headers wrong: {} {} {:?}",
            r.method, r.path, r.headers
        ));
    }
    let b = r.json();
    if b["model"] != "claude-test"
        || b["max_tokens"] != 77
        || b["system"] != "You are concise"
        || b["tools"][0]["input_schema"]["type"] != "object"
        || b["messages"][0]["role"] != "user"
    {
        return Err(format!("anthropic body wrong: {b}"));
    }
    drop(server);

    // ---- Anthropic streaming ----------------------------------------------------------------------------------------------
    let frames = [
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"model\":\"claude\",\"usage\":{\"input_tokens\":3}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"Hi\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" there\"}}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    ];
    let server = Server::start(move |_, _| sse_reply(&frames))?;
    let provider = AnthropicProvider::new(&server.base, client(), creds("anthropic"));
    let mut deltas = Vec::new();
    let answer = provider
        .stream(
            &ChatRequest::new("claude", vec![Message::user("go")]),
            &mut |e| {
                if let StreamEvent::Text(t) = e {
                    deltas.push(t);
                }
                ControlFlow::Continue(())
            },
        )
        .map_err(|e| format!("anthropic stream: {e:?}"))?;
    if answer.text() != "Hi there" || deltas.last().map(String::as_str) != Some(" there") {
        return Err(format!(
            "anthropic stream assembled {:?} from deltas {deltas:?}",
            answer.text()
        ));
    }
    if server.requests()[0].json()["stream"] != true {
        return Err("a streaming call did not ask for stream:true".into());
    }
    drop(server);

    // ---- OpenAI-compatible chat completions ----------------------------------------------------------------------------------
    let server = Server::start(|_, _| {
        json_reply(
            200,
            json!({"id": "chat_1", "model": "gpt-test", "choices": [{"message": {"role": "assistant", "content": "four"}, "finish_reason": "stop"}],
                   "usage": {"prompt_tokens": 12, "completion_tokens": 1}}),
        )
    })?;
    let provider = OpenAiCompatProvider::new("openai", &server.base, client(), creds("openai"));
    let answer = provider
        .complete(&ChatRequest::simple("gpt-test", "system", "2+2?"))
        .map_err(|e| format!("openai complete: {e:?}"))?;
    if answer.text() != "four" {
        return Err(format!("openai reply text {:?}", answer.text()));
    }
    let seen = server.requests();
    let r = &seen[0];
    if !r.path.ends_with("/chat/completions")
        || r.header("authorization") != Some("Bearer e2e-secret")
    {
        return Err(format!("openai request wrong: {} {:?}", r.path, r.headers));
    }
    let b = r.json();
    if b["model"] != "gpt-test"
        || b["messages"][0] != json!({"role": "system", "content": "system"})
        || b["messages"][1] != json!({"role": "user", "content": "2+2?"})
    {
        return Err(format!("openai body wrong: {b}"));
    }
    drop(server);

    // ---- failover: a 503 route is skipped for a healthy one; a 401 is surfaced, never failed over ----------------------------
    let bad = Server::start(|_, _| Reply {
        status: 503,
        content_type: "text/plain",
        body: "busy".into(),
    })?;
    let good = Server::start(|_, _| {
        json_reply(
            200,
            json!({"id": "ok", "model": "gpt", "choices": [{"message": {"role": "assistant", "content": "yes"}, "finish_reason": "stop"}], "usage": {}}),
        )
    })?;
    let first = Arc::new(OpenAiCompatProvider::new(
        "one",
        &bad.base,
        client(),
        creds("one"),
    ));
    let second = Arc::new(OpenAiCompatProvider::new(
        "two",
        &good.base,
        client(),
        creds("two"),
    ));
    let chain = FailoverChain::new(
        vec![
            Route::new("one", first, "gpt"),
            Route::new("two", second, "gpt"),
        ],
        FailoverPolicy {
            breaker_threshold: 10,
            ..FailoverPolicy::default()
        },
    );
    let text = chain
        .complete(&ChatRequest::new("ignored", vec![Message::user("x")]))
        .map_err(|e| format!("failover chain: {e:?}"))?
        .text()
        .to_string();
    if text != "yes" || bad.requests().len() != 1 || good.requests().len() != 1 {
        return Err(format!(
            "failover did not go 503 -> healthy: text {text:?}, bad {} good {}",
            bad.requests().len(),
            good.requests().len()
        ));
    }
    let auth = Server::start(|_, _| Reply {
        status: 401,
        content_type: "text/plain",
        body: "bad key".into(),
    })?;
    let ap = OpenAiCompatProvider::new("auth", &auth.base, client(), creds("auth"));
    if !matches!(
        ap.complete(&ChatRequest::new("g", vec![Message::user("x")])),
        Err(LlmError::Status { status: 401, .. })
    ) {
        return Err("a 401 was not surfaced as LlmError::Status 401".into());
    }
    // A missing credential is refused before the network is touched.
    let nocred = Server::start(|_, _| Reply {
        status: 200,
        content_type: "text/plain",
        body: String::new(),
    })?;
    let np = OpenAiCompatProvider::new(
        "nokey",
        &nocred.base,
        client(),
        Arc::new(StaticCredentials::default()),
    );
    if !matches!(
        np.complete(&ChatRequest::new("g", vec![Message::user("x")])),
        Err(LlmError::MissingCredential(_))
    ) || !nocred.requests().is_empty()
    {
        return Err(
            "a request without a credential reached the network or was not MissingCredential"
                .into(),
        );
    }

    Ok(
        json!({"anthropic": "complete+tool+stream", "openai": "complete", "failover": "503 -> healthy", "auth": 401, "no_credential": "refused offline"}),
    )
}
