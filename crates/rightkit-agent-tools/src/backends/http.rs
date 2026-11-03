//! Loopback HTTP transport for an app's control surface (rightkit#795).
//!
//! Some apps do not host a `rightkit-daemon` pipe service named `control-<app>`: CodeRight's
//! control surface is a loopback HTTP server. [`HttpEndpoint`] lets an [`crate::AppProfile`] reach
//! such an app with the same tools, the same profile allowlist (methods and command policy) and the
//! same hard exposure rules as the pipe transport; only the wire differs.
//!
//! # Wire contract (control-http v1)
//!
//! One request per tool call: `POST <path>` to a loopback address with a JSON body
//! `{"method": "<control method>", "params": {...}}`. A `200` response carries JSON, either
//! `{"ok": true, "result": <value>}` or `{"ok": false, "error": {"code", "message", "retryable"}}`.
//! Any other status is a failure (`http_<status>`, retryable for 429 and 5xx).
//!
//! The endpoint is loopback only: [`HttpEndpoint::new`] refuses every other address, so a profile
//! can never send agent input off the machine. When the app protects its server with a bearer
//! token, [`HttpEndpoint::with_token_file`] names a file the token is read from on each call (the
//! token is never held in the profile, argv, environment or a log).

use crate::spec::{ToolError, ERR_INVALID_ARGS};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Largest response body accepted (an inline screenshot is the biggest legitimate one).
const MAX_BODY: usize = 32 * 1024 * 1024;
/// Largest response header block accepted.
const MAX_HEAD: usize = 64 * 1024;

/// A loopback HTTP control endpoint: socket address, request path and an optional bearer-token file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpEndpoint {
    addr: SocketAddr,
    path: String,
    token_file: Option<PathBuf>,
}

impl HttpEndpoint {
    /// `addr` must be a loopback address; `path` must start with `/` and hold only URL-safe path
    /// characters (letters, digits and `/-._~`).
    pub fn new(addr: SocketAddr, path: &str) -> Result<Self, ToolError> {
        if !addr.ip().is_loopback() {
            return Err(ToolError::new(
                ERR_INVALID_ARGS,
                format!("control endpoint {addr} is not a loopback address"),
            ));
        }
        let ok = path.starts_with('/')
            && !path.contains("//")
            && !path.split('/').any(|s| s == "." || s == "..")
            && path
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "/-._~".contains(c));
        if !ok {
            return Err(ToolError::new(
                ERR_INVALID_ARGS,
                format!("control endpoint path `{path}` is not a plain absolute path"),
            ));
        }
        Ok(Self {
            addr,
            path: path.to_string(),
            token_file: None,
        })
    }

    /// Parse `127.0.0.1:PORT/path` or `[::1]:PORT/path` (the path defaults to `/control`).
    pub fn parse(spec: &str) -> Result<Self, ToolError> {
        let (host, path) = match spec.find('/') {
            Some(i) => (&spec[..i], &spec[i..]),
            None => (spec, "/control"),
        };
        let addr: SocketAddr = host.parse().map_err(|_| {
            ToolError::new(
                ERR_INVALID_ARGS,
                format!("`{host}` is not a loopback ip:port"),
            )
        })?;
        Self::new(addr, path)
    }

    /// Read a bearer token from `file` (trimmed) on every call and send it as `Authorization`.
    pub fn with_token_file(mut self, file: impl Into<PathBuf>) -> Self {
        self.token_file = Some(file.into());
        self
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn token_file(&self) -> Option<&Path> {
        self.token_file.as_deref()
    }

    /// One control call: `method` with `params`, bounded by `timeout` end to end.
    pub fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, ToolError> {
        let deadline = Instant::now() + timeout;
        let token = match &self.token_file {
            Some(f) => {
                let raw = std::fs::read_to_string(f).map_err(|e| {
                    ToolError::new(
                        "control_token_unreadable",
                        format!("cannot read the control token file {}: {e}", f.display()),
                    )
                })?;
                let t = raw.trim().to_string();
                if t.is_empty() || t.chars().any(|c| c.is_control() || c.is_whitespace()) {
                    return Err(ToolError::new(
                        "control_token_unreadable",
                        format!(
                            "the control token file {} is empty or malformed",
                            f.display()
                        ),
                    ));
                }
                Some(t)
            }
            None => None,
        };
        let body = json!({"method": method, "params": params}).to_string();
        let mut head = format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nAccept: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
            self.path,
            self.addr,
            body.len()
        );
        if let Some(t) = &token {
            head.push_str(&format!("Authorization: Bearer {t}\r\n"));
        }
        head.push_str("\r\n");

        let left = |what: &str| {
            deadline
                .checked_duration_since(Instant::now())
                .filter(|d| !d.is_zero())
                .ok_or_else(|| {
                    ToolError::retryable("timeout", format!("control call timed out ({what})"))
                })
        };
        let io = |e: std::io::Error| match e.kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
                ToolError::retryable("timeout", "control call timed out")
            }
            _ => ToolError::retryable("control_io", format!("control connection failed: {e}")),
        };
        let mut stream = TcpStream::connect_timeout(&self.addr, left("connect")?).map_err(|e| {
            ToolError::retryable(
                "control_unreachable",
                format!("control endpoint {} is not reachable: {e}", self.addr),
            )
        })?;
        stream.set_write_timeout(Some(left("send")?)).map_err(io)?;
        stream.write_all(head.as_bytes()).map_err(io)?;
        stream.write_all(body.as_bytes()).map_err(io)?;

        let mut raw = Vec::new();
        let mut buf = [0u8; 16 * 1024];
        loop {
            stream.set_read_timeout(Some(left("read")?)).map_err(io)?;
            let n = stream.read(&mut buf).map_err(io)?;
            if n == 0 {
                break;
            }
            raw.extend_from_slice(&buf[..n]);
            if raw.len() > MAX_BODY + MAX_HEAD {
                return Err(ToolError::new(
                    "control_response_too_large",
                    "control response exceeds the size limit",
                ));
            }
        }
        parse_response(&raw)
    }
}

fn bad(msg: &str) -> ToolError {
    ToolError::new("control_bad_response", msg.to_string())
}

fn parse_response(raw: &[u8]) -> Result<Value, ToolError> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| bad("incomplete HTTP response"))?;
    if split > MAX_HEAD {
        return Err(bad("HTTP response head too large"));
    }
    let head =
        std::str::from_utf8(&raw[..split]).map_err(|_| bad("HTTP response head is not UTF-8"))?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .filter(|_| status_line.starts_with("HTTP/1."))
        .ok_or_else(|| bad("malformed HTTP status line"))?;
    let mut chunked = false;
    let mut length: Option<usize> = None;
    for l in lines {
        let Some((k, v)) = l.split_once(':') else {
            continue;
        };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
        if k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked") {
            chunked = true;
        } else if k == "content-length" {
            length = v.parse().ok();
        }
    }
    let rest = &raw[split + 4..];
    let body = if chunked {
        dechunk(rest)?
    } else {
        match length {
            Some(n) if rest.len() >= n => rest[..n].to_vec(),
            Some(_) => return Err(bad("HTTP body shorter than Content-Length")),
            None => rest.to_vec(),
        }
    };
    if body.len() > MAX_BODY {
        return Err(ToolError::new(
            "control_response_too_large",
            "control response exceeds the size limit",
        ));
    }
    if !(200..300).contains(&status) {
        return Err(ToolError {
            code: format!("http_{status}"),
            message: format!("control endpoint answered HTTP {status}"),
            retryable: status == 429 || status >= 500,
        });
    }
    let v: Value =
        serde_json::from_slice(&body).map_err(|_| bad("control response is not JSON"))?;
    match v.get("ok").and_then(Value::as_bool) {
        Some(true) => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
        Some(false) => {
            let e = &v["error"];
            Err(ToolError {
                code: e["code"].as_str().unwrap_or("control_error").to_string(),
                message: e["message"]
                    .as_str()
                    .unwrap_or("the control call failed")
                    .to_string(),
                retryable: e["retryable"].as_bool().unwrap_or(false),
            })
        }
        None => Err(bad("control response has no `ok` field")),
    }
}

fn dechunk(mut rest: &[u8]) -> Result<Vec<u8>, ToolError> {
    let mut out = Vec::new();
    loop {
        let eol = rest
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| bad("truncated chunked body"))?;
        let size_text = std::str::from_utf8(&rest[..eol]).map_err(|_| bad("bad chunk size"))?;
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| bad("bad chunk size"))?;
        rest = &rest[eol + 2..];
        if size == 0 {
            return Ok(out);
        }
        if rest.len() < size + 2 || out.len() + size > MAX_BODY {
            return Err(bad("truncated or oversized chunked body"));
        }
        out.extend_from_slice(&rest[..size]);
        rest = &rest[size + 2..];
    }
}
