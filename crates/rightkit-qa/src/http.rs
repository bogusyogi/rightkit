//! Minimal blocking HTTP/1.1 client over loopback `TcpStream`. Every call has an
//! explicit deadline: a request that never answers is a failed step, not a hang.
use crate::util::{err, Result};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}

impl Response {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// One request. `auth` is the full `Authorization` header value when present.
pub fn request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    auth: Option<&str>,
    body: Option<&str>,
    timeout: Duration,
) -> Result<Response> {
    if !addr.ip().is_loopback() {
        return err(format!(
            "QA control endpoint must be loopback, received {addr}"
        ));
    }
    let deadline = Instant::now() + timeout;
    let mut s = TcpStream::connect_timeout(&addr, timeout.min(Duration::from_secs(5)))?;
    s.set_write_timeout(Some(timeout))?;
    let mut head =
        format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nAccept: */*\r\n");
    if let Some(a) = auth {
        head.push_str(&format!("Authorization: {a}\r\n"));
    }
    let payload = body.unwrap_or("");
    if body.is_some() {
        head.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            payload.len()
        ));
    }
    head.push_str("\r\n");
    s.write_all(head.as_bytes())?;
    s.write_all(payload.as_bytes())?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 16384];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return err(format!(
                "{method} {path} exceeded {}ms",
                timeout.as_millis()
            ));
        }
        s.set_read_timeout(Some(left))?;
        match s.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return err(format!(
                    "{method} {path} exceeded {}ms",
                    timeout.as_millis()
                ))
            }
            Err(e) => return Err(e.into()),
        }
    }
    let split = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| crate::util::Error("malformed HTTP response".into()))?;
    let head = String::from_utf8_lossy(&buf[..split]).to_string();
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let content_type = head
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-type:")
                .map(|v| v.trim().to_string())
        })
        .unwrap_or_default();
    Ok(Response {
        status,
        content_type,
        body: buf[split + 4..].to_vec(),
    })
}

pub fn loopback(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

pub fn port_open(port: u16) -> bool {
    TcpStream::connect_timeout(&loopback(port), Duration::from_millis(250)).is_ok()
}
