//! In-process mock HTTP/1.1 server for end-to-end tests. Binds 127.0.0.1, records
//! every request exactly as received, and answers from a handler closure.
//! Never contacts the network.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::{AddressPolicy, ClientConfig};

/// Address policy that lets the [`MockServer`] be reached: the hosts `127.0.0.1`,
/// `localhost` and `[::1]`, loopback addresses only, through `AddressPolicy::allow_if`.
/// Every other destination keeps the default public-web rules.
pub fn loopback_policy() -> AddressPolicy {
    AddressPolicy::default().allow_if(
        vec!["127.0.0.1".into(), "localhost".into(), "::1".into()],
        |ip| ip.is_loopback(),
    )
}

/// `ClientConfig::default()` with [`loopback_policy`]; the starting point for tests that
/// talk to a [`MockServer`].
pub fn loopback_config() -> ClientConfig {
    ClientConfig::default().address_policy(loopback_policy())
}

#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    /// Path plus query, as sent.
    pub target: String,
    /// Lower-cased names.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl RecordedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or("")
    }
    pub fn query(&self) -> &str {
        self.target.split_once('?').map(|(_, q)| q).unwrap_or("")
    }
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    pub fn body_json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).expect("request body is JSON")
    }
    /// Decode an urlencoded body or query string into pairs.
    pub fn form(&self) -> Vec<(String, String)> {
        decode_pairs(&self.body_text())
    }
    pub fn query_pairs(&self) -> Vec<(String, String)> {
        decode_pairs(self.query())
    }
}

fn decode_pairs(s: &str) -> Vec<(String, String)> {
    s.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (crate::percent_decode(k), crate::percent_decode(v))
        })
        .collect()
}

pub enum MockBody {
    Full(Vec<u8>),
    /// Sent with chunked transfer-encoding; sleeps `delay` before each chunk.
    Chunks(Vec<(Duration, Vec<u8>)>),
    /// Headers are sent (announcing a longer body) and the connection is cut.
    TruncatedAfter(Vec<u8>),
}

pub struct MockResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: MockBody,
    /// Sleep before writing anything (to exercise timeouts).
    pub delay: Duration,
}

impl MockResponse {
    pub fn new(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: MockBody::Full(Vec::new()),
            delay: Duration::ZERO,
        }
    }
    pub fn json(status: u16, value: &serde_json::Value) -> Self {
        Self::new(status)
            .header("Content-Type", "application/json")
            .body(serde_json::to_vec(value).unwrap())
    }
    pub fn text(status: u16, text: &str) -> Self {
        Self::new(status)
            .header("Content-Type", "text/plain")
            .body(text.as_bytes().to_vec())
    }
    pub fn sse(frames: &[&str]) -> Self {
        Self::new(200)
            .header("Content-Type", "text/event-stream")
            .chunks(
                frames
                    .iter()
                    .map(|f| (Duration::ZERO, f.as_bytes().to_vec()))
                    .collect(),
            )
    }
    pub fn header(mut self, k: &str, v: &str) -> Self {
        self.headers.push((k.into(), v.into()));
        self
    }
    pub fn body(mut self, b: Vec<u8>) -> Self {
        self.body = MockBody::Full(b);
        self
    }
    pub fn chunks(mut self, c: Vec<(Duration, Vec<u8>)>) -> Self {
        self.body = MockBody::Chunks(c);
        self
    }
    pub fn delay(mut self, d: Duration) -> Self {
        self.delay = d;
        self
    }
}

type Handler = dyn Fn(&RecordedRequest, usize) -> MockResponse + Send + Sync;

pub struct MockServer {
    addr: std::net::SocketAddr,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl MockServer {
    /// `handler(request, zero_based_request_index)` produces each response.
    pub fn start(
        handler: impl Fn(&RecordedRequest, usize) -> MockResponse + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let handler: Arc<Handler> = Arc::new(handler);
        let (reqs, stop2) = (requests.clone(), stop.clone());
        let thread = thread::spawn(move || {
            for conn in listener.incoming() {
                if stop2.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = conn else { continue };
                let (handler, reqs) = (handler.clone(), reqs.clone());
                thread::spawn(move || {
                    let _ = serve(stream, &*handler, &reqs);
                });
            }
        });
        Self {
            addr,
            requests,
            stop,
            thread: Some(thread),
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }
    pub fn port(&self) -> u16 {
        self.addr.port()
    }
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }
    pub fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn serve(
    stream: TcpStream,
    handler: &Handler,
    reqs: &Mutex<Vec<RecordedRequest>>,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(());
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        reader.read_line(&mut h)?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    let len = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    let request = RecordedRequest {
        method,
        target,
        headers,
        body,
    };
    let index = {
        let mut g = reqs.lock().unwrap();
        g.push(request.clone());
        g.len() - 1
    };
    let resp = handler(&request, index);
    if !resp.delay.is_zero() {
        thread::sleep(resp.delay);
    }
    let mut out = stream;
    let reason = match resp.status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Status",
    };
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nConnection: close\r\n",
        resp.status, reason
    );
    for (k, v) in &resp.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    match resp.body {
        MockBody::Full(b) => {
            head.push_str(&format!("Content-Length: {}\r\n\r\n", b.len()));
            out.write_all(head.as_bytes())?;
            out.write_all(&b)?;
        }
        MockBody::Chunks(chunks) => {
            head.push_str("Transfer-Encoding: chunked\r\n\r\n");
            out.write_all(head.as_bytes())?;
            out.flush()?;
            for (delay, c) in chunks {
                if !delay.is_zero() {
                    thread::sleep(delay);
                }
                out.write_all(format!("{:x}\r\n", c.len()).as_bytes())?;
                out.write_all(&c)?;
                out.write_all(b"\r\n")?;
                out.flush()?;
            }
            out.write_all(b"0\r\n\r\n")?;
        }
        MockBody::TruncatedAfter(b) => {
            head.push_str(&format!("Content-Length: {}\r\n\r\n", b.len() + 1000));
            out.write_all(head.as_bytes())?;
            out.write_all(&b)?;
        }
    }
    out.flush()?;
    Ok(())
}
