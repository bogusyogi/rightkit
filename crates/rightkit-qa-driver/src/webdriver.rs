//! W3C-WebDriver-shaped session bridge over a native [`Backend`].
//!
//! Wire-compatible subset: status, new/delete session, find element(s), element
//! click/value/text, screenshot. On macOS the backend resolves elements through
//! Accessibility and acts with background per-pid input. A proxy backend that
//! forwards to an in-app embedded WebDriver (tauri-plugin-wdio) and only
//! overrides input is specified in the design doc.
use crate::admission::{CancelToken, EffectGate, EffectKind, EffectRequest, ExecError};
use crate::json::{esc, str_field};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

pub const ELEMENT_KEY: &str = "element-6066-11e4-a52e-4f735466cecf";

pub trait Backend {
    fn find(&mut self, using: &str, value: &str) -> Result<String, String>;
    fn click(&mut self, id: &str) -> Result<(), String>;
    fn send_keys(&mut self, id: &str, text: &str) -> Result<(), String>;
    fn text(&mut self, id: &str) -> Result<String, String>;
    fn screenshot_png_b64(&mut self) -> Result<String, String>;
}

pub struct Server<B: Backend> {
    pub backend: B,
    sessions: HashMap<String, ()>,
    next: u32,
    gate: Arc<EffectGate>,
}

pub fn b64(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut o = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                o.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                o.push('=');
            }
        }
    }
    o
}

fn ok(v: &str) -> (u16, String) {
    (200, format!("{{\"value\":{v}}}"))
}
fn err(status: u16, code: &str, msg: &str) -> (u16, String) {
    (
        status,
        format!(
            "{{\"value\":{{\"error\":\"{code}\",\"message\":\"{}\",\"stacktrace\":\"\"}}}}",
            esc(msg)
        ),
    )
}

impl<B: Backend> Server<B> {
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            sessions: HashMap::new(),
            next: 1,
            gate: Arc::new(EffectGate::allow_all()),
        }
    }

    /// Route every effectful command (element click / send keys / screenshot) through
    /// `gate` (EFF-001). Default is [`EffectGate::allow_all`].
    pub fn with_gate(mut self, gate: Arc<EffectGate>) -> Self {
        self.gate = gate;
        self
    }

    pub fn gate(&self) -> &Arc<EffectGate> {
        &self.gate
    }

    fn effect<T>(
        &mut self,
        kind: EffectKind,
        method: &str,
        element: &str,
        params: &str,
        run: impl FnOnce(&mut B) -> Result<T, String>,
    ) -> Result<T, String> {
        let gate = self.gate.clone();
        let backend = &mut self.backend;
        let req = EffectRequest::new(kind, method, element).with_params(params);
        let effect = gate.admit(req, &CancelToken::new(), |_| {
            run(backend).map_err(ExecError::from)
        });
        match effect.settlement.outcome {
            // Executor errors keep their pre-gate text; only gate refusals are prefixed.
            crate::admission::SettleOutcome::Failed if !effect.settlement.crashed => {
                Err(effect.settlement.reason.unwrap_or_default())
            }
            _ => effect.into_result(),
        }
    }

    pub fn handle(&mut self, method: &str, path: &str, body: &str) -> (u16, String) {
        let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
        match (method, parts.as_slice()) {
            ("GET", ["status"]) => ok("{\"ready\":true,\"message\":\"rightkit-control\"}"),
            ("POST", ["session"]) => {
                let id = format!("rkc-{}", self.next);
                self.next += 1;
                self.sessions.insert(id.clone(), ());
                ok(&format!(
                    "{{\"sessionId\":\"{id}\",\"capabilities\":{{\"browserName\":\"tauri\",\"rightkit:input\":\"background-native\"}}}}"
                ))
            }
            ("DELETE", ["session", id]) => {
                self.sessions.remove(*id);
                ok("null")
            }
            (_, ["session", id, rest @ ..]) if !self.sessions.contains_key(*id) => {
                let _ = rest;
                err(404, "invalid session id", "unknown session")
            }
            ("POST", ["session", _, "element"]) => {
                let using = str_field(body, "using").unwrap_or_default();
                let value = str_field(body, "value").unwrap_or_default();
                match self.backend.find(&using, &value) {
                    Ok(e) => ok(&format!("{{\"{ELEMENT_KEY}\":\"{}\"}}", esc(&e))),
                    Err(m) => err(404, "no such element", &m),
                }
            }
            ("POST", ["session", _, "element", e, "click"]) => {
                match self.effect(EffectKind::Input, "click", e, body, |b| b.click(e)) {
                    Ok(()) => ok("null"),
                    Err(m) => err(400, "element click intercepted", &m),
                }
            }
            ("POST", ["session", _, "element", e, "value"]) => {
                let text = str_field(body, "text").unwrap_or_default();
                match self.effect(EffectKind::Input, "send_keys", e, body, |b| {
                    b.send_keys(e, &text)
                }) {
                    Ok(()) => ok("null"),
                    Err(m) => err(400, "element not interactable", &m),
                }
            }
            ("GET", ["session", _, "element", e, "text"]) => match self.backend.text(e) {
                Ok(t) => ok(&format!("\"{}\"", esc(&t))),
                Err(m) => err(404, "stale element reference", &m),
            },
            ("GET", ["session", _, "screenshot"]) => {
                match self.effect(EffectKind::Capture, "screenshot", "window", "", |b| {
                    b.screenshot_png_b64()
                }) {
                    Ok(b) => ok(&format!("\"{b}\"")),
                    Err(m) => err(500, "unknown error", &m),
                }
            }
            _ => err(404, "unknown command", &format!("{method} {path}")),
        }
    }

    /// Blocking loopback server for third-party WebDriver clients. Every request
    /// must pass [`check_request`]: loopback peer, a `Host` that names this
    /// listener, no browser `Origin`, and the per-launch bearer credential. There
    /// is no unauthenticated mode: loopback alone does not identify the caller.
    pub fn serve(&mut self, listener: TcpListener, credential: &str) -> std::io::Result<()> {
        if credential.len() < 32 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "credential must be at least 32 characters",
            ));
        }
        let port = listener.local_addr()?.port();
        for conn in listener.incoming() {
            let mut s = match conn {
                Ok(s) => s,
                Err(_) => continue,
            };
            let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
            let _ = s.set_write_timeout(Some(Duration::from_secs(5)));
            let peer_ok = s.peer_addr().map(|a| a.ip().is_loopback()).unwrap_or(false);
            let (code, out) = match read_request(&mut s) {
                Some((head, body)) => match check_request(&head, peer_ok, port, credential) {
                    Ok(()) => {
                        let mut first = head.lines().next().unwrap_or("").split(' ');
                        let (m, p) = (first.next().unwrap_or(""), first.next().unwrap_or("/"));
                        self.handle(m, p, &body)
                    }
                    Err(e) => e,
                },
                None => err(400, "invalid argument", "malformed request"),
            };
            let _ = write!(s, "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}", out.len());
        }
        Ok(())
    }
}

const MAX_REQUEST: usize = 1 << 20;

/// Read one HTTP request (head, body), bounded; `None` on malformed or oversized input.
fn read_request(s: &mut std::net::TcpStream) -> Option<(String, String)> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    let (head_end, len) = loop {
        let n = s.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > MAX_REQUEST {
            return None;
        }
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..i]).to_string();
            let cl = header(&head, "content-length")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            if cl > MAX_REQUEST {
                return None;
            }
            break (i + 4, cl);
        }
    };
    while buf.len() < head_end + len {
        let n = s.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > MAX_REQUEST + 8192 {
            return None;
        }
    }
    let head = String::from_utf8_lossy(&buf[..head_end - 4]).to_string();
    let body = String::from_utf8_lossy(&buf[head_end..head_end + len]).to_string();
    Some((head, body))
}

fn header(head: &str, name: &str) -> Option<String> {
    head.lines().skip(1).find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case(name)
            .then(|| v.trim().to_string())
    })
}

fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut d = u32::from(a.len() != b.len());
    for i in 0..a.len().max(b.len()) {
        d |= u32::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0));
    }
    d == 0
}

/// Admission for one request head. Pure so it is testable without a socket.
pub fn check_request(
    head: &str,
    peer_is_loopback: bool,
    port: u16,
    credential: &str,
) -> Result<(), (u16, String)> {
    if !peer_is_loopback {
        return Err(err(403, "unknown error", "peer is not loopback"));
    }
    let host_ok = header(head, "host")
        .map(|h| {
            h == format!("127.0.0.1:{port}")
                || h == format!("localhost:{port}")
                || h == format!("[::1]:{port}")
        })
        .unwrap_or(false);
    if !host_ok {
        return Err(err(403, "unknown error", "unexpected Host header"));
    }
    if header(head, "origin").is_some() {
        return Err(err(
            403,
            "unknown error",
            "browser-originated requests are refused",
        ));
    }
    let presented = header(head, "authorization")
        .and_then(|v| v.strip_prefix("Bearer ").map(str::to_string))
        .unwrap_or_default();
    if !ct_eq(&presented, credential) {
        return Err(err(
            401,
            "unknown error",
            "missing or wrong bridge credential",
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub mod macos {
    use super::Backend;
    use crate::mac::{self, Node};
    use std::collections::HashMap;

    /// Accessibility-resolved elements, background real-input actions.
    ///
    /// This is an adapter: its [`Backend`] methods act directly. Drive it
    /// through a gated [`super::Server`] (`Server::new(AxBackend::new(pid))
    /// .with_gate(gate)`), which admits click / send keys / screenshot.
    pub struct AxBackend {
        pub pid: i32,
        els: HashMap<String, Node>,
        n: u32,
    }
    impl AxBackend {
        pub fn new(pid: i32) -> Self {
            Self {
                pid,
                els: HashMap::new(),
                n: 0,
            }
        }
        fn get(&self, id: &str) -> Result<&Node, String> {
            self.els
                .get(id)
                .ok_or_else(|| "unknown element".to_string())
        }
    }
    impl Backend for AxBackend {
        fn find(&mut self, using: &str, value: &str) -> Result<String, String> {
            let exact = using == "link text" || using == "name";
            let node = mac::ax_find(self.pid, value, exact)
                .ok_or_else(|| format!("no element matching {value:?}"))?;
            self.n += 1;
            let id = format!("ax-{}", self.n);
            self.els.insert(id.clone(), node);
            Ok(id)
        }
        fn click(&mut self, id: &str) -> Result<(), String> {
            let n = self.get(id)?.clone();
            // AXPress first: works on inactive apps whose WKWebView swallows
            // first-mouse clicks. Fall back to a real per-pid mouse click.
            if mac::ax_press(&n) {
                return Ok(());
            }
            mac::click_node(self.pid, &n)
        }
        fn send_keys(&mut self, id: &str, text: &str) -> Result<(), String> {
            let n = self.get(id)?.clone();
            mac::click_node(self.pid, &n)?; // focus inside the app, not the OS
            mac::type_text(self.pid, text);
            Ok(())
        }
        fn text(&mut self, id: &str) -> Result<String, String> {
            Ok(mac::ax_text(self.get(id)?))
        }
        fn screenshot_png_b64(&mut self) -> Result<String, String> {
            let w = mac::main_window(self.pid).ok_or("no window")?;
            let p = std::env::temp_dir().join(format!("rkc-{}.png", std::process::id()));
            if !mac::capture_window(w.id, &p.to_string_lossy()) {
                return Err("screencapture failed".into());
            }
            let bytes = std::fs::read(&p).map_err(|e| e.to_string())?;
            let _ = std::fs::remove_file(&p);
            Ok(super::b64(&bytes))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fake(Vec<String>);
    impl Backend for Fake {
        fn find(&mut self, u: &str, v: &str) -> Result<String, String> {
            self.0.push(format!("find {u} {v}"));
            Ok("e1".into())
        }
        fn click(&mut self, id: &str) -> Result<(), String> {
            self.0.push(format!("click {id}"));
            Ok(())
        }
        fn send_keys(&mut self, _: &str, t: &str) -> Result<(), String> {
            self.0.push(format!("keys {t}"));
            Ok(())
        }
        fn text(&mut self, _: &str) -> Result<String, String> {
            Ok("hi".into())
        }
        fn screenshot_png_b64(&mut self) -> Result<String, String> {
            Ok(b64(b"abcd"))
        }
    }
    #[test]
    fn session_flow() {
        let mut s = Server::new(Fake(vec![]));
        let (_, r) = s.handle("POST", "/session", "{}");
        assert!(r.contains("rkc-1"));
        let (c, r) = s.handle(
            "POST",
            "/session/rkc-1/element",
            r#"{"using":"link text","value":"Go"}"#,
        );
        assert_eq!(c, 200);
        assert!(r.contains(ELEMENT_KEY));
        assert_eq!(
            s.handle("POST", "/session/rkc-1/element/e1/click", "{}").0,
            200
        );
        assert_eq!(s.handle("POST", "/session/zzz/element", "{}").0, 404);
        assert_eq!(s.backend.0, ["find link text Go", "click e1"]);
        assert_eq!(b64(b"abcd"), "YWJjZA==");
    }

    #[test]
    fn denied_click_never_reaches_backend() {
        use crate::admission::{AdmissionDecision, SettleOutcome};
        let gate = Arc::new(EffectGate::new(|r: &EffectRequest| {
            if r.method == "click" {
                AdmissionDecision::deny("clicks blocked")
            } else {
                AdmissionDecision::Allow
            }
        }));
        let mut s = Server::new(Fake(vec![])).with_gate(gate.clone());
        s.handle("POST", "/session", "{}");
        s.handle(
            "POST",
            "/session/rkc-1/element",
            r#"{"using":"css","value":"x"}"#,
        );
        let (c, r) = s.handle("POST", "/session/rkc-1/element/e1/click", "{}");
        assert_eq!(c, 400);
        assert!(r.contains("denied"), "{r}");
        let (c, _) = s.handle(
            "POST",
            "/session/rkc-1/element/e1/value",
            r#"{"text":"hi"}"#,
        );
        assert_eq!(c, 200);
        assert_eq!(s.backend.0, ["find css x", "keys hi"]);
        let outcomes: Vec<_> = gate.settlements().iter().map(|x| x.outcome).collect();
        assert_eq!(outcomes, [SettleOutcome::Denied, SettleOutcome::Ok]);
    }
}
