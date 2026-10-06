//! Local HTTP mock for PAID providers: paid-tier scenarios assert the exact
//! requests the product would send, and never make a billed call.
use crate::util::Result;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Route {
    pub method: Option<String>,
    pub path: String,
    pub status: u16,
    pub body: Value,
    pub content_type: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Seen {
    pub fn to_value(&self) -> Value {
        let headers: serde_json::Map<String, Value> = self
            .headers
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), json!(v)))
            .collect();
        json!({"method": self.method, "path": self.path, "headers": headers, "body": self.body,
               "json": serde_json::from_str::<Value>(&self.body).unwrap_or(Value::Null)})
    }
}

pub struct MockServer {
    pub base: String,
    addr: SocketAddr,
    seen: Arc<Mutex<Vec<Seen>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
}

impl MockServer {
    pub fn start(routes: Vec<Route>) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let addr = listener.local_addr()?;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (s2, st2) = (seen.clone(), stop.clone());
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                if st2.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                let _ = serve(conn, &routes, &s2);
            }
        });
        Ok(Self {
            base: format!("http://{addr}"),
            addr,
            seen,
            stop,
        })
    }
    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
    pub fn close(&self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(200));
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.close();
    }
}

fn serve(mut s: TcpStream, routes: &[Route], seen: &Mutex<Vec<Seen>>) -> std::io::Result<()> {
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let (head_end, content_len) = loop {
        let n = s.read(&mut chunk)?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..p]).to_ascii_lowercase();
            let len = head
                .lines()
                .find_map(|l| {
                    l.strip_prefix("content-length:")
                        .and_then(|v| v.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            break (p + 4, len);
        }
    };
    while buf.len() < head_end + content_len {
        let n = s.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.lines();
    let mut first = lines.next().unwrap_or("").split_whitespace();
    let (method, path) = (
        first.next().unwrap_or("").to_string(),
        first.next().unwrap_or("").to_string(),
    );
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    let body = String::from_utf8_lossy(&buf[head_end..(head_end + content_len).min(buf.len())])
        .to_string();
    seen.lock().unwrap().push(Seen {
        method: method.clone(),
        path: path.clone(),
        headers,
        body,
    });
    let route = routes.iter().find(|r| {
        r.path == path.split('?').next().unwrap_or("")
            && r.method
                .as_deref()
                .map(|m| m.eq_ignore_ascii_case(&method))
                .unwrap_or(true)
    });
    let (status, ctype, payload) = match route {
        Some(r) => (
            r.status,
            r.content_type
                .clone()
                .unwrap_or_else(|| "application/json".into()),
            r.body.to_string(),
        ),
        None => (
            404,
            "application/json".into(),
            r#"{"error":"unmocked"}"#.into(),
        ),
    };
    write!(s, "HTTP/1.1 {status} X\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}", payload.len())?;
    s.flush()
}
