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
    /// Every response header in wire order, names lowercased, values trimmed. Repeated headers appear repeatedly.
    pub headers: Vec<(String, String)>,
}

impl Response {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// First value of the response header `name` (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Every value of the response header `name` (case-insensitive), in wire order.
    pub fn header_all(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .collect()
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
    request_with(addr, method, path, auth, &[], body, timeout)
}

fn header_ok(text: &str) -> bool {
    !text.chars().any(|c| c == '\r' || c == '\n' || c == '\0')
}

/// Like [`request`] with custom request headers. `headers` are written after the defaults; a `Host`, `Connection`,
/// `Accept`, `Content-Length` or `Authorization` (when `auth` is given) entry replaces the default of that name, and
/// a `Content-Type` entry replaces the JSON default sent with a body. A name or value holding CR, LF or NUL, or a
/// name that is empty or holds `:`, is refused. The response carries every header in [`Response::headers`].
pub fn request_with(
    addr: SocketAddr,
    method: &str,
    path: &str,
    auth: Option<&str>,
    headers: &[(&str, &str)],
    body: Option<&str>,
    timeout: Duration,
) -> Result<Response> {
    for (name, value) in headers {
        if name.is_empty() || name.contains(':') || !header_ok(name) || !header_ok(value) {
            return err(format!("header `{name}` is not a plain header"));
        }
    }
    let custom = |name: &str| headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name));
    if !addr.ip().is_loopback() {
        return err(format!(
            "QA control endpoint must be loopback, received {addr}"
        ));
    }
    let deadline = Instant::now() + timeout;
    let mut s = TcpStream::connect_timeout(&addr, timeout.min(Duration::from_secs(5)))?;
    s.set_write_timeout(Some(timeout))?;
    let mut head = format!("{method} {path} HTTP/1.1\r\n");
    if !custom("host") {
        head.push_str(&format!("Host: {addr}\r\n"));
    }
    if !custom("connection") {
        head.push_str("Connection: close\r\n");
    }
    if !custom("accept") {
        head.push_str("Accept: */*\r\n");
    }
    if let Some(a) = auth.filter(|_| !custom("authorization")) {
        head.push_str(&format!("Authorization: {a}\r\n"));
    }
    let payload = body.unwrap_or("");
    if body.is_some() {
        if !custom("content-type") {
            head.push_str("Content-Type: application/json\r\n");
        }
        if !custom("content-length") {
            head.push_str(&format!("Content-Length: {}\r\n", payload.len()));
        }
    }
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
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
    let headers = parse_headers(&head);
    Ok(Response {
        status,
        content_type,
        body: buf[split + 4..].to_vec(),
        headers,
    })
}

fn parse_headers(head: &str) -> Vec<(String, String)> {
    head.lines()
        .skip(1)
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect()
}

pub fn loopback(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

pub fn port_open(port: u16) -> bool {
    TcpStream::connect_timeout(&loopback(port), Duration::from_millis(250)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// One-shot loopback server: returns the raw request it received, after sending `reply`.
    fn serve(reply: &'static str) -> (SocketAddr, std::thread::JoinHandle<String>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut raw = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = s.read(&mut chunk).unwrap();
                raw.extend_from_slice(&chunk[..n]);
                if n == 0 || raw.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            // Body bytes may arrive in a later segment; give them a moment so the request text is whole.
            std::thread::sleep(Duration::from_millis(50));
            s.set_read_timeout(Some(Duration::from_millis(50))).ok();
            while let Ok(n) = s.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&chunk[..n]);
            }
            s.write_all(reply.as_bytes()).unwrap();
            String::from_utf8_lossy(&raw).into_owned()
        });
        (addr, h)
    }

    const REPLY: &str = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nX-Thing: One\r\nSet-Cookie: a=1\r\nset-cookie: b=2\r\nConnection: close\r\n\r\nok";

    #[test]
    fn custom_headers_are_sent_and_response_headers_returned() {
        let (addr, h) = serve(REPLY);
        let r = request_with(
            addr,
            "GET",
            "/x",
            None,
            &[("X-Api-Key", "k1"), ("Accept", "text/plain")],
            None,
            Duration::from_secs(5),
        )
        .unwrap();
        let sent = h.join().unwrap();
        assert!(sent.contains("X-Api-Key: k1\r\n"));
        assert!(sent.contains("Accept: text/plain\r\n"));
        assert!(!sent.contains("Accept: */*"));
        assert_eq!(r.status, 200);
        assert_eq!(r.text(), "ok");
        assert_eq!(r.header("X-THING"), Some("One"));
        assert_eq!(r.header_all("set-cookie"), vec!["a=1", "b=2"]);
        assert_eq!(r.header("missing"), None);
    }

    #[test]
    fn custom_header_overrides_defaults_and_plain_request_is_unchanged() {
        let (addr, h) = serve(REPLY);
        request_with(
            addr,
            "POST",
            "/x",
            Some("Bearer a"),
            &[
                ("authorization", "Bearer b"),
                ("Content-Type", "text/plain"),
            ],
            Some("hi"),
            Duration::from_secs(5),
        )
        .unwrap();
        let sent = h.join().unwrap();
        assert!(sent.contains("authorization: Bearer b\r\n"));
        assert!(!sent.contains("Bearer a"));
        assert!(sent.contains("Content-Type: text/plain\r\n"));
        assert!(!sent.contains("application/json"));
        assert!(sent.contains("Content-Length: 2\r\n"));

        let (addr, h) = serve(REPLY);
        let r = request(
            addr,
            "POST",
            "/y",
            Some("Bearer a"),
            Some("{}"),
            Duration::from_secs(5),
        )
        .unwrap();
        let sent = h.join().unwrap();
        assert!(sent.contains("Authorization: Bearer a\r\n"));
        assert!(sent.contains("Content-Type: application/json\r\n"));
        assert_eq!(r.content_type, "text/plain");
    }

    #[test]
    fn header_injection_is_refused_before_connecting() {
        let addr = loopback(1);
        for bad in [
            ("X\r\nEvil", "v"),
            ("X", "v\r\nEvil: 1"),
            ("", "v"),
            ("A:B", "v"),
        ] {
            let e = request_with(
                addr,
                "GET",
                "/",
                None,
                &[bad],
                None,
                Duration::from_millis(50),
            )
            .unwrap_err();
            assert!(e.0.contains("not a plain header"), "{}", e.0);
        }
    }
}
