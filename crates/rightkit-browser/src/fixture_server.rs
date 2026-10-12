//! Immutable loopback HTTP server that only serves an owned fixture bundle.

use crate::fixture_protocol::{
    explicit_fixture_port, parse_fixture_url, validate_fixture_path, FixtureBundle, FixtureError,
};
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::{JoinHandle, JoinSet};

const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_CONNECTIONS: usize = 128;
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'none'; frame-src 'none'; child-src 'none'; object-src 'none'; media-src 'self'; form-action 'none'; base-uri 'none'; frame-ancestors 'none'; sandbox allow-scripts allow-same-origin";

static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);

/// An exclusively owned server for one verified, immutable fixture bundle.
pub struct FixtureServer {
    bundle: FixtureBundle,
    addr: SocketAddr,
    instance_id: String,
    accept_task: Option<JoinHandle<()>>,
    shutdown_signal: Option<oneshot::Sender<()>>,
}

pub(crate) struct FixtureResponse {
    pub(crate) status: u16,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Arc<[u8]>,
}

impl FixtureServer {
    /// Bind an available ephemeral port on IPv4 loopback.
    pub async fn prepare(bundle: FixtureBundle) -> Result<Self, FixtureError> {
        Self::bind(bundle, SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).await
    }

    /// Bind the exact loopback port named in `exact_url`, after checking that
    /// its path belongs to this bundle. An existing listener makes this fail.
    pub async fn prepare_at(bundle: FixtureBundle, exact_url: &str) -> Result<Self, FixtureError> {
        let url = parse_fixture_url(exact_url)?;
        if !bundle.files().iter().any(|file| file.path == url.path()) {
            return Err(FixtureError::Invalid(
                "exact_url does not point to a fixture manifest resource".into(),
            ));
        }
        let port = explicit_fixture_port(exact_url)?;
        Self::bind(
            bundle,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
        )
        .await
    }

    async fn bind(bundle: FixtureBundle, addr: SocketAddr) -> Result<Self, FixtureError> {
        let listener = TcpListener::bind(addr).await.map_err(|error| {
            FixtureError::FixtureServerNotOwned(format!("cannot exclusively bind {addr}: {error}"))
        })?;
        let addr = listener.local_addr().map_err(|error| {
            FixtureError::FixtureServerNotOwned(format!(
                "cannot inspect bound fixture socket: {error}"
            ))
        })?;
        let files: Arc<HashMap<String, (String, Arc<[u8]>)>> = Arc::new(
            bundle
                .files()
                .iter()
                .map(|file| {
                    (
                        file.path.clone(),
                        (
                            file.content_type.clone(),
                            Arc::<[u8]>::from(file.body.clone()),
                        ),
                    )
                })
                .collect(),
        );
        let (shutdown_signal, mut shutdown_receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    _ = &mut shutdown_receiver => break,
                    accepted = listener.accept() => match accepted {
                        Ok((stream, peer)) if peer.ip().is_loopback() && connections.len() < MAX_CONNECTIONS => {
                            let files = Arc::clone(&files);
                            connections.spawn(async move { serve_connection(stream, files).await });
                        }
                        Ok((_stream, _peer)) => {}
                        Err(_) => break,
                    },
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            }
            connections.abort_all();
            while connections.join_next().await.is_some() {}
        });
        let instance_id = format!(
            "fixture-server-{}-{}",
            std::process::id(),
            NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed)
        );
        Ok(Self {
            bundle,
            addr,
            instance_id,
            accept_task: Some(task),
            shutdown_signal: Some(shutdown_signal),
        })
    }

    /// Build an exact URL for a resource in the owned bundle.
    pub fn url(&self, path: &str) -> Result<String, FixtureError> {
        validate_fixture_path(path)?;
        if !self.bundle.files().iter().any(|file| file.path == path) {
            return Err(FixtureError::Invalid(
                "path is not present in the owned fixture bundle".into(),
            ));
        }
        Ok(format!("http://127.0.0.1:{}{path}", self.addr.port()))
    }

    pub(crate) fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub(crate) fn bundle(&self) -> &FixtureBundle {
        &self.bundle
    }

    pub(crate) fn instance_id(&self) -> &str {
        &self.instance_id
    }

    pub(crate) fn is_alive(&self) -> bool {
        self.accept_task
            .as_ref()
            .is_some_and(|task| !task.is_finished())
    }

    pub(crate) fn permitted_urls(&self) -> Arc<HashSet<String>> {
        Arc::new(
            self.bundle
                .files()
                .iter()
                .map(|file| format!("http://127.0.0.1:{}{}", self.addr.port(), file.path))
                .collect(),
        )
    }

    pub(crate) fn response_for(
        &self,
        method: &str,
        exact_url: &str,
    ) -> Result<FixtureResponse, FixtureError> {
        if !self.is_alive() {
            return Err(FixtureError::FixtureServerNotOwned(
                "owned fixture server is no longer alive".into(),
            ));
        }
        if !matches!(method, "GET" | "HEAD") {
            return Err(FixtureError::Invalid(
                "fixture responses permit GET and HEAD only".into(),
            ));
        }
        if !self.permitted_urls().contains(exact_url) {
            return Err(FixtureError::Invalid(
                "URL is not an exact permitted fixture resource".into(),
            ));
        }
        let url = parse_fixture_url(exact_url)?;
        let file = self
            .bundle
            .files()
            .iter()
            .find(|file| file.path == url.path())
            .ok_or_else(|| {
                FixtureError::Invalid("URL has no matching fixture manifest resource".into())
            })?;
        let headers = response_headers(&file.content_type, file.body.len());
        let body = if method == "HEAD" {
            Arc::<[u8]>::from(&[][..])
        } else {
            Arc::<[u8]>::from(file.body.clone())
        };
        Ok(FixtureResponse {
            status: 200,
            headers,
            body,
        })
    }

    /// Stop accepting, abort owned connections, and wait for their cleanup.
    pub async fn shutdown(&mut self) -> Result<(), FixtureError> {
        let Some(mut task) = self.accept_task.take() else {
            return Ok(());
        };
        if let Some(signal) = self.shutdown_signal.take() {
            let _ = signal.send(());
        }
        match tokio::time::timeout(CONNECTION_TIMEOUT, &mut task).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(FixtureError::Cleanup(format!(
                "fixture server task failed during shutdown: {error}"
            ))),
            Err(_) => {
                task.abort();
                let _ = task.await;
                Err(FixtureError::Cleanup(
                    "fixture server cleanup exceeded its time limit".into(),
                ))
            }
        }
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.shutdown_signal.take();
        if let Some(task) = self.accept_task.take() {
            task.abort();
        }
    }
}

async fn serve_connection(mut stream: TcpStream, files: Arc<HashMap<String, (String, Arc<[u8]>)>>) {
    let _ = tokio::time::timeout(CONNECTION_TIMEOUT, async {
        let request = match read_request(&mut stream).await {
            Ok(request) => request,
            Err(status) => {
                let _ = write_response(
                    &mut stream,
                    status,
                    "text/plain; charset=utf-8",
                    b"fixture request rejected",
                    false,
                )
                .await;
                return;
            }
        };
        match files.get(&request.path) {
            Some((content_type, body)) => {
                let _ = write_response(
                    &mut stream,
                    "200 OK",
                    content_type,
                    body,
                    request.method == "HEAD",
                )
                .await;
            }
            None => {
                let _ = write_response(
                    &mut stream,
                    "404 Not Found",
                    "text/plain; charset=utf-8",
                    b"not found",
                    request.method == "HEAD",
                )
                .await;
            }
        }
    })
    .await;
}

struct Request {
    method: String,
    path: String,
}

async fn read_request(stream: &mut TcpStream) -> Result<Request, &'static str> {
    let mut bytes = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let header_end = loop {
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
        if bytes.len() >= MAX_HEADER_BYTES {
            return Err("431 Request Header Fields Too Large");
        }
        let count = stream
            .read(&mut chunk)
            .await
            .map_err(|_| "400 Bad Request")?;
        if count == 0 {
            return Err("400 Bad Request");
        }
        bytes.extend_from_slice(&chunk[..count]);
        if bytes.len() > MAX_HEADER_BYTES {
            return Err("431 Request Header Fields Too Large");
        }
    };
    if header_end != bytes.len() {
        return Err("400 Bad Request");
    }
    let text = std::str::from_utf8(&bytes[..header_end]).map_err(|_| "400 Bad Request")?;
    let mut lines = text[..header_end - 4].split("\r\n");
    let request_line = lines.next().ok_or("400 Bad Request")?;
    let mut request_parts = request_line.split(' ');
    let method = request_parts.next().ok_or("400 Bad Request")?;
    let target = request_parts.next().ok_or("400 Bad Request")?;
    let version = request_parts.next().ok_or("400 Bad Request")?;
    if request_parts.next().is_some() || !matches!(version, "HTTP/1.0" | "HTTP/1.1") {
        return Err("400 Bad Request");
    }
    if method == "CONNECT" {
        return Err("405 Method Not Allowed");
    }
    if !matches!(method, "GET" | "HEAD") {
        return Err("405 Method Not Allowed");
    }
    if !target.starts_with('/')
        || target.starts_with("//")
        || target.contains(['?', '#', '\\', '%'])
    {
        return Err("400 Bad Request");
    }

    let mut host: Option<&str> = None;
    let mut content_length: Option<&str> = None;
    let mut seen_headers = HashSet::new();
    for line in lines {
        let (name, value) = line.split_once(':').ok_or("400 Bad Request")?;
        if name.is_empty()
            || !name.bytes().all(is_token_byte)
            || value.bytes().any(|b| b == b'\r' || b == b'\n')
        {
            return Err("400 Bad Request");
        }
        if !seen_headers.insert(name.to_ascii_lowercase()) {
            return Err("400 Bad Request");
        }
        let value = value.trim();
        if name.eq_ignore_ascii_case("host") {
            if host.replace(value).is_some() {
                return Err("400 Bad Request");
            }
        } else if name.eq_ignore_ascii_case("content-length") {
            if content_length.replace(value).is_some() {
                return Err("400 Bad Request");
            }
        } else if name.eq_ignore_ascii_case("transfer-encoding")
            || name.eq_ignore_ascii_case("upgrade")
            || (name.eq_ignore_ascii_case("connection")
                && value.to_ascii_lowercase().contains("upgrade"))
        {
            return Err("400 Bad Request");
        }
    }
    let host = host.ok_or("400 Bad Request")?;
    if host
        != format!(
            "127.0.0.1:{}",
            stream.local_addr().map_err(|_| "400 Bad Request")?.port()
        )
    {
        return Err("400 Bad Request");
    }
    if let Some(length) = content_length {
        if length != "0" {
            return Err("400 Bad Request");
        }
    }
    let path = target.to_owned();
    validate_fixture_path(&path).map_err(|_| "400 Bad Request")?;
    Ok(Request {
        method: method.to_owned(),
        path,
    })
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

async fn write_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
    head_only: bool,
) -> Result<(), std::io::Error> {
    let mut header = format!("HTTP/1.1 {status}\r\n");
    for (name, value) in response_headers(content_type, body.len()) {
        header.push_str(&name);
        header.push_str(": ");
        header.push_str(&value);
        header.push_str("\r\n");
    }
    header.push_str("Connection: close\r\n\r\n");
    stream.write_all(header.as_bytes()).await?;
    if !head_only {
        stream.write_all(body).await?;
    }
    stream.shutdown().await
}

fn response_headers(content_type: &str, content_length: usize) -> Vec<(String, String)> {
    vec![
        ("Content-Type".into(), content_type.into()),
        ("Content-Length".into(), content_length.to_string()),
        ("Cache-Control".into(), "no-store".into()),
        ("X-Content-Type-Options".into(), "nosniff".into()),
        ("Content-Security-Policy".into(), CSP.into()),
    ]
}
