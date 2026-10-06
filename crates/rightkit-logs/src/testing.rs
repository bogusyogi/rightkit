//! Loopback HTTP ingest mock for app end-to-end tests (feature `testing`).
//!
//! Records every JSON POST and replies from a script (status, Retry-After);
//! once the script is exhausted it answers 200. Binds 127.0.0.1 on a free port.

use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Minimal recording HTTP server. Responses are scripted; once the script is
/// exhausted every request gets 200.
pub struct MockServer {
    pub url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    script: Arc<Mutex<Vec<(u16, Option<u64>)>>>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl MockServer {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        listener.set_nonblocking(true).expect("nonblocking");
        let url = format!("http://127.0.0.1:{}/v1/ingest", listener.local_addr().unwrap().port());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let script = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (r, s, st) = (requests.clone(), script.clone(), stop.clone());
        let handle = thread::spawn(move || {
            while !st.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).ok();
                        stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
                        if let Some(body) = read_request(&mut stream) {
                            r.lock().unwrap().push(serde_json::from_slice(&body).unwrap_or(Value::Null));
                        }
                        let (status, retry_after) = {
                            let mut script = s.lock().unwrap();
                            if script.is_empty() { (200, None) } else { script.remove(0) }
                        };
                        let mut response = format!("HTTP/1.1 {status} X\r\ncontent-length: 0\r\nconnection: close\r\n");
                        if let Some(seconds) = retry_after {
                            response.push_str(&format!("retry-after: {seconds}\r\n"));
                        }
                        response.push_str("\r\n");
                        let _ = stream.write_all(response.as_bytes());
                    }
                    Err(_) => thread::sleep(Duration::from_millis(10)),
                }
            }
        });
        Self { url, requests, script, stop, handle: Some(handle) }
    }

    pub fn script(&self, responses: &[(u16, Option<u64>)]) {
        *self.script.lock().unwrap() = responses.to_vec();
    }

    pub fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    pub fn bodies(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn read_request(stream: &mut std::net::TcpStream) -> Option<Vec<u8>> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let headers = String::from_utf8_lossy(&buffer[..header_end]).to_ascii_lowercase();
    let length = headers
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while buffer.len() < header_end + length {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..n]);
    }
    Some(buffer[header_end..].to_vec())
}

