//! Loopback HTTP host for one Motion composition project (feature `server`; donor: GenRight
//! `genright-engine/src/motion_server.rs`). Managed headless Chrome
//! (`rightkit-browser`) loads the project from `http://127.0.0.1:<port>/<entry>`; this server
//! serves the project files, injects the deterministic runtime into the entry page, swaps the GSAP
//! CDN script for the caller-supplied copy (so renders are reproducible), and takes `/__hf/log` posts.
//!
//! It binds 127.0.0.1 only, answers only requests whose `Host` is its own origin (a DNS-rebinding
//! guard), and never serves a path outside the project directory (lexically or through a link).

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The deterministic runtime injected into every composition entry page.
pub const RUNTIME_JS: &str = include_str!("assets/hf-runtime.js");
const MAX_HEAD: usize = 64 * 1024;
const MAX_BODY: usize = 1024 * 1024;
const MAX_LOGS: usize = 200;

/// One answer, before it is written to the socket.
pub struct Reply {
    pub status: u16,
    pub mime: &'static str,
    pub extra: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
}

impl Reply {
    fn new(status: u16, mime: &'static str, body: Vec<u8>) -> Self {
        Self {
            status,
            mime,
            extra: Vec::new(),
            body,
        }
    }
}

/// What the router needs to answer: the project and its entry page.
pub struct Project {
    /// The project folder.
    pub dir: PathBuf,
    /// The entry page, relative to `dir`.
    pub entry: String,
    /// The GSAP build served at `/__hf/gsap.js` in place of any remote GSAP URL, so renders are
    /// reproducible. The caller supplies it (GSAP is not bundled: its licence is not this
    /// crate's); `None` leaves remote GSAP URLs as they are.
    pub gsap_js: Option<Arc<str>>,
}

pub struct ProjectServer {
    port: u16,
    stop: Arc<AtomicBool>,
    logs: Arc<Mutex<Vec<String>>>,
    accept: Option<std::thread::JoinHandle<()>>,
}

impl ProjectServer {
    /// Bind 127.0.0.1 on a free port and serve `dir` with `entry` as the entry page; `gsap_js`
    /// is the GSAP build to serve in place of remote GSAP URLs (see [`Project::gsap_js`]).
    pub fn start(dir: PathBuf, entry: String, gsap_js: Option<Arc<str>>) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let logs = Arc::new(Mutex::new(Vec::new()));
        let project = Arc::new(Project {
            dir,
            entry,
            gsap_js,
        });
        let accept = {
            let (stop, logs) = (Arc::clone(&stop), Arc::clone(&logs));
            std::thread::Builder::new()
                .name("rightkit-motion-http".to_string())
                .spawn(move || {
                    while !stop.load(Ordering::SeqCst) {
                        match listener.accept() {
                            Ok((stream, _)) => {
                                let (project, logs) = (Arc::clone(&project), Arc::clone(&logs));
                                let _ = std::thread::Builder::new()
                                    .name("rightkit-motion-conn".to_string())
                                    .spawn(move || serve_connection(stream, port, &project, &logs));
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                std::thread::sleep(Duration::from_millis(10));
                            }
                            Err(_) => std::thread::sleep(Duration::from_millis(50)),
                        }
                    }
                })?
        };
        Ok(Self {
            port,
            stop,
            logs,
            accept: Some(accept),
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn url(&self, entry: &str) -> String {
        format!(
            "http://127.0.0.1:{}/{}",
            self.port,
            entry.trim_start_matches('/')
        )
    }

    /// Page `console`-style log lines the runtime posted (capped).
    pub fn logs(&self) -> Vec<String> {
        self.logs.lock().unwrap().clone()
    }

    pub fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.accept.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for ProjectServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn serve_connection(
    mut stream: TcpStream,
    port: u16,
    project: &Project,
    logs: &Mutex<Vec<String>>,
) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
    // One request per connection (`Connection: close`): Chrome opens as many as it needs.
    let Some((head, mut body)) = read_head(&mut stream) else {
        return;
    };
    let head_text = String::from_utf8_lossy(&head).into_owned();
    let mut lines = head_text.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let (method, target) = (first.next().unwrap_or(""), first.next().unwrap_or("/"));
    let (mut host, mut range, mut length) = (None, None, 0usize);
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            let value = value.trim();
            match name.trim().to_ascii_lowercase().as_str() {
                "host" => host = Some(value.to_string()),
                "range" => range = Some(value.to_string()),
                "content-length" => length = value.parse().unwrap_or(0),
                _ => {}
            }
        }
    }
    while body.len() < length.min(MAX_BODY) {
        let mut chunk = [0u8; 4096];
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    let reply = if !host_is_ours(host.as_deref(), port) {
        Reply::new(421, "text/plain", b"wrong host".to_vec())
    } else {
        route(project, method, target, range.as_deref(), &body, logs)
    };
    write_reply(&mut stream, method, &reply);
    let _ = stream.shutdown(Shutdown::Both);
}

/// The request head (up to the blank line) and any body bytes already read after it.
fn read_head(stream: &mut TcpStream) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let rest = buf.split_off(at + 4);
            buf.truncate(at);
            return Some((buf, rest));
        }
        if buf.len() > MAX_HEAD {
            return None;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

pub fn host_is_ours(host: Option<&str>, port: u16) -> bool {
    host.is_some_and(|h| h.eq_ignore_ascii_case(&format!("127.0.0.1:{port}")))
}

fn write_reply(stream: &mut TcpStream, method: &str, reply: &Reply) {
    let reason = match reply.status {
        200 => "OK",
        206 => "Partial Content",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        421 => "Misdirected Request",
        _ => "Error",
    };
    let mut head = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n",
        reply.status,
        reply.mime,
        reply.body.len()
    );
    for (name, value) in &reply.extra {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    if method != "HEAD" {
        let _ = stream.write_all(&reply.body);
    }
    let _ = stream.flush();
}

/// Answer one request. Pure apart from reading project files and appending to `logs`.
pub fn route(
    project: &Project,
    method: &str,
    target: &str,
    range: Option<&str>,
    body: &[u8],
    logs: &Mutex<Vec<String>>,
) -> Reply {
    let (raw_path, _query) = target.split_once('?').unwrap_or((target, ""));
    let path = rightkit_http::percent_decode_component(raw_path);
    if let Some(op) = path.strip_prefix("/__hf/") {
        return match op {
            "runtime.js" => Reply::new(200, "text/javascript", RUNTIME_JS.as_bytes().to_vec()),
            "gsap.js" => match &project.gsap_js {
                Some(gsap) => Reply::new(200, "text/javascript", gsap.as_bytes().to_vec()),
                None => Reply::new(404, "text/plain", Vec::new()),
            },
            "log" | "loaded" | "ready" => {
                if op == "log" {
                    let mut logs = logs.lock().unwrap();
                    if logs.len() < MAX_LOGS {
                        logs.push(String::from_utf8_lossy(body).chars().take(400).collect());
                    }
                }
                Reply::new(200, "text/plain", Vec::new())
            }
            _ => Reply::new(404, "text/plain", Vec::new()),
        };
    }
    if method != "GET" && method != "HEAD" {
        return Reply::new(405, "text/plain", Vec::new());
    }
    let rel = path.trim_start_matches('/');
    let rel = if rel.is_empty() {
        project.entry.as_str()
    } else {
        rel
    };
    let Some(file) = resolve_inside(&project.dir, rel) else {
        return Reply::new(403, "text/plain", b"outside project".to_vec());
    };
    // A link inside the project must not lead out of it.
    match (file.canonicalize(), project.dir.canonicalize()) {
        (Ok(real), Ok(root)) if !real.starts_with(&root) => {
            return Reply::new(403, "text/plain", b"outside project".to_vec());
        }
        _ => {}
    }
    let Ok(mut bytes) = std::fs::read(&file) else {
        return Reply::new(404, "text/plain", Vec::new());
    };
    let mime = mime_for(&file);
    if mime == "text/html" {
        let text = String::from_utf8_lossy(&bytes);
        let mut html = if project.gsap_js.is_some() {
            swap_gsap_cdn(&text)
        } else {
            text.into_owned()
        };
        if rel == project.entry {
            html = inject_runtime(&html);
        }
        return Reply::new(200, mime, html.into_bytes());
    }
    // Media needs byte ranges for seeking.
    let mut reply = Reply::new(200, mime, Vec::new());
    reply.extra.push(("Accept-Ranges", "bytes".to_string()));
    if let Some((a, b)) = range.and_then(|r| parse_range(r, bytes.len())) {
        let total = bytes.len();
        let mut slice = bytes.split_off(a);
        slice.truncate(b - a + 1);
        reply.status = 206;
        reply
            .extra
            .push(("Content-Range", format!("bytes {a}-{b}/{total}")));
        reply.body = slice;
    } else {
        reply.body = bytes;
    }
    reply
}

pub fn resolve_inside(root: &Path, rel: &str) -> Option<PathBuf> {
    let p = Path::new(rel);
    if p.is_absolute()
        || p.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        return None;
    }
    Some(root.join(p))
}

fn parse_range(value: &str, len: usize) -> Option<(usize, usize)> {
    let spec = value.strip_prefix("bytes=")?;
    let (a, b) = spec.split_once('-')?;
    if len == 0 {
        return None;
    }
    let (start, end) = if a.is_empty() {
        let n: usize = b.parse().ok()?;
        (len.saturating_sub(n), len - 1)
    } else {
        (
            a.parse().ok()?,
            if b.is_empty() {
                len - 1
            } else {
                b.parse().ok()?
            },
        )
    };
    (start <= end && start < len).then(|| (start, end.min(len - 1)))
}

fn mime_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("html") | Some("htm") => "text/html",
        Some("js") | Some("mjs") => "text/javascript",
        Some("css") => "text/css",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("mp4") | Some("m4v") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mov") => "video/quicktime",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("m4a") => "audio/mp4",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        _ => "application/octet-stream",
    }
}

/// Replace remote GSAP URLs (any version on any CDN) with the served copy (`/__hf/gsap.js`).
pub fn swap_gsap_cdn(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    loop {
        let Some(pos) = rest.find("http") else {
            out.push_str(rest);
            break;
        };
        let tail = &rest[pos..];
        if !(tail.starts_with("https://") || tail.starts_with("http://")) {
            out.push_str(&rest[..pos + 4]);
            rest = &rest[pos + 4..];
            continue;
        }
        let end = tail.find(['"', '\'', ' ', '>']).unwrap_or(tail.len());
        let url = &tail[..end];
        let lower = url.to_ascii_lowercase();
        out.push_str(&rest[..pos]);
        if lower.contains("gsap") && lower.ends_with("gsap.min.js") {
            out.push_str("/__hf/gsap.js");
        } else {
            out.push_str(url);
        }
        rest = &tail[end..];
    }
    out
}

pub fn inject_runtime(html: &str) -> String {
    let tag = "<script src=\"/__hf/runtime.js\"></script>";
    let lower = html.to_ascii_lowercase();
    if let Some(p) = lower.find("<head") {
        if let Some(gt) = lower[p..].find('>') {
            let at = p + gt + 1;
            return format!("{}{}{}", &html[..at], tag, &html[at..]);
        }
    }
    format!("{tag}{html}")
}
