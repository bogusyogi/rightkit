//! In-process pinning proxy. Chrome is started with `--proxy-server` pointing
//! here (loopback included, no bypass), so Chrome never resolves names itself:
//! this proxy resolves once through [`NetworkPolicy::pin`], checks the resolved
//! addresses against the policy, and connects to the vetted address. A name
//! that flips from public to private between check and connect (DNS
//! rebinding), or a request that bypassed page interception (workers), can
//! therefore never reach a blocked address. With a per-request callback set,
//! a connection to a `host:port` no page check of the session allowed gets the
//! page check here first (resource type `Connect`), so traffic page
//! interception never sees cannot pass a callback `Deny` (#925).
use crate::policy::BlockReason;
use crate::policy::Guard;
use std::collections::HashSet;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};

const MAX_FIXTURE_HEADER_BYTES: usize = 16 * 1024;
const FIXTURE_HEADER_TIMEOUT: Duration = Duration::from_secs(5);
const FIXTURE_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const FIXTURE_IO_TIMEOUT: Duration = Duration::from_secs(30);
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const MAX_FIXTURE_CONNECTIONS: usize = 128;

/// Immutable network boundary for a local browser fixture.
#[derive(Clone)]
pub(crate) struct FixtureProxyPolicy {
    pub server_endpoint: SocketAddr,
    pub permitted_urls: Arc<HashSet<String>>,
    pub expires_at: Instant,
}

/// One connection the proxy made, with the address it pinned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PinnedConnection {
    pub host: String,
    pub port: u16,
    pub ip: IpAddr,
}

pub(crate) struct PinProxy {
    pub addr: SocketAddr,
    pub log: Arc<Mutex<Vec<PinnedConnection>>>,
    shutdown: watch::Sender<bool>,
    task: Mutex<Option<JoinHandle<io::Result<()>>>>,
}

impl Drop for PinProxy {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        if let Some(task) = self
            .task
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            // The listener supervisor owns a JoinSet, whose drop aborts every
            // connection task before the listener task is released.
            task.abort();
        }
    }
}

impl PinProxy {
    pub async fn start(guard: Arc<Guard>) -> std::io::Result<PinProxy> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let log: Arc<Mutex<Vec<PinnedConnection>>> = Arc::default();
        Ok(Self::spawn(listener, addr, guard, log, None))
    }

    pub async fn start_fixture(
        guard: Arc<Guard>,
        policy: FixtureProxyPolicy,
    ) -> io::Result<PinProxy> {
        validate_fixture_policy(&policy)?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let log: Arc<Mutex<Vec<PinnedConnection>>> = Arc::default();
        Ok(Self::spawn(listener, addr, guard, log, Some(policy)))
    }

    fn spawn(
        listener: TcpListener,
        addr: SocketAddr,
        guard: Arc<Guard>,
        log: Arc<Mutex<Vec<PinnedConnection>>>,
        fixture: Option<FixtureProxyPolicy>,
    ) -> PinProxy {
        let (shutdown, receiver) = watch::channel(false);
        let task = tokio::spawn(run_listener(
            listener,
            guard,
            log.clone(),
            fixture,
            receiver,
        ));
        PinProxy {
            addr,
            log,
            shutdown,
            task: Mutex::new(Some(task)),
        }
    }

    /// Stop accepting, abort active connections, and join them within `grace`.
    pub async fn shutdown(&self, grace: Duration) -> io::Result<()> {
        let Some(mut task) = self
            .task
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        else {
            return Ok(());
        };
        let _ = self.shutdown.send(true);
        match tokio::time::timeout(grace, &mut task).await {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => Err(io::Error::other(error)),
            Err(_) => {
                task.abort();
                let _ = task.await;
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "proxy shutdown exceeded grace period",
                ))
            }
        }
    }
}

async fn run_listener(
    listener: TcpListener,
    guard: Arc<Guard>,
    log: Arc<Mutex<Vec<PinnedConnection>>>,
    fixture: Option<FixtureProxyPolicy>,
    mut shutdown: watch::Receiver<bool>,
) -> io::Result<()> {
    let mut connections = JoinSet::new();
    loop {
        while connections.try_join_next().is_some() {}
        if *shutdown.borrow() {
            break;
        }
        match tokio::time::timeout(ACCEPT_POLL_INTERVAL, listener.accept()).await {
            Ok(Ok((mut conn, _))) => {
                let guard = guard.clone();
                let log = log.clone();
                if let Some(policy) = fixture.as_ref() {
                    if connections.len() >= MAX_FIXTURE_CONNECTIONS {
                        let _ = tokio::time::timeout(
                            Duration::from_secs(1),
                            refuse(
                                &mut conn,
                                "503 Service Unavailable",
                                "fixture proxy connection limit reached",
                            ),
                        )
                        .await;
                        continue;
                    }
                    let policy = policy.clone();
                    connections.spawn(async move {
                        let _ = serve_fixture(conn, guard, log, policy).await;
                    });
                } else {
                    connections.spawn(async move {
                        let _ = serve(conn, guard, log).await;
                    });
                }
            }
            Ok(Err(error)) => {
                connections.abort_all();
                while connections.join_next().await.is_some() {}
                return Err(error);
            }
            Err(_) => {
                if shutdown.has_changed().unwrap_or(true) {
                    let _ = shutdown.borrow_and_update();
                }
            }
        }
    }
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
struct FixtureRequest {
    method: String,
    path_and_query: String,
    version: String,
    headers: Vec<(String, String)>,
}

fn validate_fixture_policy(policy: &FixtureProxyPolicy) -> io::Result<()> {
    if policy.server_endpoint.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST)
        || policy.server_endpoint.port() == 0
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "fixture endpoint must be 127.0.0.1 with a nonzero port",
        ));
    }
    if policy.permitted_urls.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "fixture URL allowlist must not be empty",
        ));
    }
    if Instant::now() >= policy.expires_at {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "fixture proxy policy is already expired",
        ));
    }
    for raw in policy.permitted_urls.iter() {
        let parsed = url::Url::parse(raw).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid permitted fixture URL: {error}"),
            )
        })?;
        if parsed.as_str() != raw
            || parsed.scheme() != "http"
            || parsed.host_str() != Some("127.0.0.1")
            || parsed.port_or_known_default() != Some(policy.server_endpoint.port())
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.fragment().is_some()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "permitted fixture URLs must be canonical HTTP URLs at the owned endpoint",
            ));
        }
    }
    Ok(())
}

fn parse_fixture_request(
    bytes: &[u8],
    trailing: &[u8],
    policy: &FixtureProxyPolicy,
    now: Instant,
) -> Result<FixtureRequest, &'static str> {
    if now >= policy.expires_at {
        return Err("fixture proxy policy expired");
    }
    if !trailing.is_empty() {
        return Err("fixture requests cannot contain a body or pipelined bytes");
    }
    let head = std::str::from_utf8(bytes).map_err(|_| "fixture headers must be ASCII")?;
    if !head.is_ascii() || !head.ends_with("\r\n\r\n") {
        return Err("malformed fixture request headers");
    }
    let fields = head[..head.len() - 4].split("\r\n").collect::<Vec<_>>();
    let mut request_line = fields.first().copied().unwrap_or("").split(' ');
    let method = request_line.next().unwrap_or("");
    let target = request_line.next().unwrap_or("");
    let version = request_line.next().unwrap_or("");
    if request_line.next().is_some()
        || target.is_empty()
        || !matches!(method, "GET" | "HEAD")
        || !matches!(version, "HTTP/1.0" | "HTTP/1.1")
    {
        return Err("only well-formed GET and HEAD fixture requests are allowed");
    }
    if !policy.permitted_urls.contains(target) {
        return Err("fixture URL is not in the exact allowlist");
    }
    let url = url::Url::parse(target).map_err(|_| "fixture URL is invalid")?;
    if url.as_str() != target
        || url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port_or_known_default() != Some(policy.server_endpoint.port())
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("fixture URL must target the exact owned HTTP endpoint");
    }

    let mut seen = HashSet::new();
    let mut host_seen = false;
    let mut headers = Vec::new();
    for line in fields.iter().skip(1) {
        let (name, raw_value) = line.split_once(':').ok_or("malformed fixture header")?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
        {
            return Err("malformed fixture header name");
        }
        let lower_name = name.to_ascii_lowercase();
        if !seen.insert(lower_name.clone()) {
            return Err("duplicate fixture header");
        }
        let value = raw_value.trim_matches(|character| character == ' ' || character == '\t');
        if value
            .bytes()
            .any(|byte| (byte < 0x20 && byte != b'\t') || byte == 0x7f)
        {
            return Err("malformed fixture header value");
        }
        match lower_name.as_str() {
            "host" => {
                host_seen = true;
                if !value.eq_ignore_ascii_case(&policy.server_endpoint.to_string()) {
                    return Err("fixture Host must match the owned endpoint");
                }
            }
            "content-length" if value == "0" => {}
            "content-length" => return Err("fixture request bodies are not allowed"),
            "connection" | "proxy-connection"
                if value.eq_ignore_ascii_case("keep-alive")
                    || value.eq_ignore_ascii_case("close") => {}
            "connection" | "proxy-connection" => {
                return Err("fixture connection tokens must be exactly keep-alive or close");
            }
            "transfer-encoding" | "te" | "trailer" | "expect" | "upgrade" | "keep-alive" => {
                return Err("fixture connection and transfer controls are not allowed");
            }
            name if name.starts_with("proxy-") => {
                return Err("proxy control headers are not allowed");
            }
            "accept" | "accept-encoding" | "accept-language" | "cache-control"
            | "if-modified-since" | "if-none-match" | "range" => {
                headers.push((lower_name, value.to_string()));
            }
            // Caller credentials, forwarding, origin, and browser-specific
            // headers are intentionally not copied to the fixture server.
            _ => {}
        }
    }
    if !host_seen {
        return Err("fixture request must include the owned Host header");
    }
    let path_and_query = match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_string(),
    };
    Ok(FixtureRequest {
        method: method.to_string(),
        path_and_query,
        version: version.to_string(),
        headers,
    })
}

async fn read_fixture_headers(conn: &mut TcpStream) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let end = index + 4;
            if end > MAX_FIXTURE_HEADER_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "fixture request headers exceed limit",
                ));
            }
            return Ok((bytes[..end].to_vec(), bytes[end..].to_vec()));
        }
        if bytes.len() >= MAX_FIXTURE_HEADER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "fixture request headers exceed limit",
            ));
        }
        let count = conn.read(&mut chunk).await?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "fixture request ended before headers",
            ));
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
}

async fn serve_fixture(
    mut conn: TcpStream,
    guard: Arc<Guard>,
    log: Arc<Mutex<Vec<PinnedConnection>>>,
    policy: FixtureProxyPolicy,
) -> io::Result<()> {
    if Instant::now() >= policy.expires_at {
        let reason = "fixture proxy policy expired";
        guard.denied_network(
            None,
            "FixtureProxy",
            &BlockReason::InvalidUrl(reason.to_string()),
        );
        refuse(&mut conn, "403 Forbidden", reason).await;
        return Ok(());
    }
    let read = tokio::time::timeout(FIXTURE_HEADER_TIMEOUT, read_fixture_headers(&mut conn)).await;
    let (head, trailing) = match read {
        Ok(Ok(headers)) => headers,
        Ok(Err(error)) if error.kind() == io::ErrorKind::InvalidData => {
            refuse(
                &mut conn,
                "413 Payload Too Large",
                "fixture headers exceed limit",
            )
            .await;
            return Ok(());
        }
        Ok(Err(_)) => return Ok(()),
        Err(_) => {
            refuse(
                &mut conn,
                "408 Request Timeout",
                "fixture request headers timed out",
            )
            .await;
            return Ok(());
        }
    };
    let request = match parse_fixture_request(&head, &trailing, &policy, Instant::now()) {
        Ok(request) => request,
        Err(reason) => {
            guard.denied_network(
                None,
                "FixtureProxy",
                &BlockReason::InvalidUrl(reason.to_string()),
            );
            refuse(&mut conn, "403 Forbidden", reason).await;
            return Ok(());
        }
    };
    let upstream = tokio::time::timeout(
        FIXTURE_CONNECT_TIMEOUT,
        TcpStream::connect(policy.server_endpoint),
    )
    .await;
    let mut upstream = match upstream {
        Ok(Ok(stream)) => stream,
        Ok(Err(_)) | Err(_) => {
            refuse(
                &mut conn,
                "502 Bad Gateway",
                "fixture server connect failed",
            )
            .await;
            return Ok(());
        }
    };
    log.lock().unwrap().push(PinnedConnection {
        host: "127.0.0.1".to_string(),
        port: policy.server_endpoint.port(),
        ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
    });

    let mut outbound = format!(
        "{} {} {}\r\nHost: {}\r\nConnection: close\r\n",
        request.method, request.path_and_query, request.version, policy.server_endpoint
    )
    .into_bytes();
    for (name, value) in request.headers {
        outbound.extend_from_slice(name.as_bytes());
        outbound.extend_from_slice(b": ");
        outbound.extend_from_slice(value.as_bytes());
        outbound.extend_from_slice(b"\r\n");
    }
    outbound.extend_from_slice(b"\r\n");
    if !matches!(
        tokio::time::timeout(FIXTURE_CONNECT_TIMEOUT, upstream.write_all(&outbound)).await,
        Ok(Ok(()))
    ) {
        refuse(
            &mut conn,
            "504 Gateway Timeout",
            "fixture request write timed out",
        )
        .await;
        return Ok(());
    }
    if tokio::time::timeout(
        FIXTURE_IO_TIMEOUT,
        tokio::io::copy(&mut upstream, &mut conn),
    )
    .await
    .is_err()
    {
        // The response is already in flight; closing both sides is the only
        // safe bounded outcome once its time budget expires.
    }
    Ok(())
}

async fn refuse(conn: &mut TcpStream, code: &str, why: &str) {
    let body = format!("blocked by rightkit-browser network policy: {why}");
    let _ = conn
        .write_all(
            format!(
                "HTTP/1.1 {code}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await;
}

async fn serve(
    mut conn: TcpStream,
    guard: Arc<Guard>,
    log: Arc<Mutex<Vec<PinnedConnection>>>,
) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > 64 * 1024 {
            return Ok(());
        }
        let n = conn.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let rest = buf[head_end..].to_vec();
    let mut lines = head.split("\r\n");
    let first = lines.next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let (method, target, version) = (
        parts.next().unwrap_or(""),
        parts.next().unwrap_or(""),
        parts.next().unwrap_or("HTTP/1.1"),
    );

    let is_connect = method.eq_ignore_ascii_case("CONNECT");
    let (host, port, forward_head) = if is_connect {
        let (h, p) = target.rsplit_once(':').unwrap_or((target, "443"));
        (h.to_string(), p.parse::<u16>().unwrap_or(443), None)
    } else {
        let Ok(u) = url::Url::parse(target) else {
            refuse(
                &mut conn,
                "400 Bad Request",
                "proxy requests must use absolute URLs",
            )
            .await;
            return Ok(());
        };
        let Some(h) = u.host_str() else {
            refuse(&mut conn, "400 Bad Request", "no host").await;
            return Ok(());
        };
        let path = match u.query() {
            Some(q) => format!("{}?{q}", u.path()),
            None => u.path().to_string(),
        };
        let mut out = format!("{method} {path} {version}\r\n");
        for l in lines.filter(|l| !l.is_empty()) {
            let name = l.split(':').next().unwrap_or("").to_ascii_lowercase();
            if matches!(
                name.as_str(),
                "connection" | "proxy-connection" | "keep-alive" | "proxy-authorization"
            ) {
                continue;
            }
            out.push_str(l);
            out.push_str("\r\n");
        }
        out.push_str("Connection: close\r\n\r\n");
        (
            h.to_string(),
            u.port_or_known_default().unwrap_or(80),
            Some(out),
        )
    };

    let ips = match guard
        .admit_proxied(&host, port, (!is_connect).then_some(target))
        .await
    {
        Ok(ips) => ips,
        Err(reason) => {
            guard.denied_network(None, "Connect", &reason);
            refuse(&mut conn, "403 Forbidden", &reason.to_string()).await;
            return Ok(());
        }
    };
    let mut upstream = None;
    for ip in ips {
        if let Ok(Ok(s)) = tokio::time::timeout(
            Duration::from_secs(3),
            TcpStream::connect(SocketAddr::new(ip, port)),
        )
        .await
        {
            log.lock().unwrap().push(PinnedConnection {
                host: host.clone(),
                port,
                ip,
            });
            upstream = Some(s);
            break;
        }
        log.lock().unwrap().push(PinnedConnection {
            host: host.clone(),
            port,
            ip,
        });
    }
    let Some(mut upstream) = upstream else {
        refuse(&mut conn, "502 Bad Gateway", "connect failed").await;
        return Ok(());
    };
    match forward_head {
        None => {
            conn.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await?;
            upstream.write_all(&rest).await?;
        }
        Some(h) => {
            upstream.write_all(h.as_bytes()).await?;
            upstream.write_all(&rest).await?;
        }
    }
    let _ = tokio::io::copy_bidirectional(&mut conn, &mut upstream).await;
    Ok(())
}

#[cfg(test)]
mod fixture_tests {
    use super::*;
    use crate::policy::NetworkPolicy;
    use std::time::Instant;

    fn fixture_policy(endpoint: SocketAddr, urls: &[&str]) -> FixtureProxyPolicy {
        FixtureProxyPolicy {
            server_endpoint: endpoint,
            permitted_urls: Arc::new(urls.iter().map(|url| (*url).to_string()).collect()),
            expires_at: Instant::now() + Duration::from_secs(60),
        }
    }

    fn endpoint() -> SocketAddr {
        "127.0.0.1:43210".parse().unwrap()
    }

    #[test]
    fn fixture_policy_accepts_only_canonical_urls_at_owned_endpoint() {
        let endpoint = endpoint();
        let url = "http://127.0.0.1:43210/index.html?view=one";
        assert!(validate_fixture_policy(&fixture_policy(endpoint, &[url])).is_ok());

        for invalid in [
            "http://127.0.0.1:43211/index.html",
            "https://127.0.0.1:43210/index.html",
            "http://user@127.0.0.1:43210/index.html",
            "http://127.0.0.1:43210/index.html#fragment",
            "http://localhost:43210/index.html",
        ] {
            assert!(
                validate_fixture_policy(&fixture_policy(endpoint, &[invalid])).is_err(),
                "accepted {invalid}"
            );
        }
    }

    #[test]
    fn fixture_parser_accepts_exact_get_and_strips_untrusted_headers() {
        let endpoint = endpoint();
        let url = "http://127.0.0.1:43210/index.html?view=one";
        let policy = fixture_policy(endpoint, &[url]);
        let request = format!(
            "GET {url} HTTP/1.1\r\nHost: {endpoint}\r\nAccept: text/html\r\nAuthorization: Bearer secret\r\nCookie: session=secret\r\nX-Forwarded-For: 192.0.2.8\r\n\r\n"
        );
        let parsed =
            parse_fixture_request(request.as_bytes(), &[], &policy, Instant::now()).unwrap();
        assert_eq!(parsed.method, "GET");
        assert_eq!(parsed.path_and_query, "/index.html?view=one");
        assert_eq!(parsed.headers, vec![("accept".into(), "text/html".into())]);
    }

    #[test]
    fn fixture_parser_accepts_chrome_keep_alive_headers_and_strips_them() {
        let endpoint = endpoint();
        let url = "http://127.0.0.1:43210/index.html";
        let policy = fixture_policy(endpoint, &[url]);
        let request = format!(
            "GET {url} HTTP/1.1\r\nHost: {endpoint}\r\nConnection: keep-alive\r\nProxy-Connection: Keep-Alive\r\n\r\n"
        );
        let parsed =
            parse_fixture_request(request.as_bytes(), &[], &policy, Instant::now()).unwrap();
        assert!(parsed.headers.is_empty());

        let request = format!(
            "GET {url} HTTP/1.1\r\nHost: {endpoint}\r\nConnection: keep-alive, Upgrade\r\n\r\n"
        );
        assert!(parse_fixture_request(request.as_bytes(), &[], &policy, Instant::now()).is_err());
    }

    #[test]
    fn fixture_parser_rejects_host_mismatch_duplicates_methods_and_body_data() {
        let endpoint = endpoint();
        let url = "http://127.0.0.1:43210/index.html";
        let policy = fixture_policy(endpoint, &[url]);
        for request in [
            format!("GET {url} HTTP/1.1\r\nHost: 127.0.0.1:43211\r\n\r\n"),
            format!("GET {url} HTTP/1.1\r\nHost: {endpoint}\r\nHost: {endpoint}\r\n\r\n"),
            format!("POST {url} HTTP/1.1\r\nHost: {endpoint}\r\n\r\n"),
            format!("GET {url} HTTP/1.1\r\nHost: {endpoint}\r\nContent-Length: 1\r\n\r\n"),
            format!("GET {url} HTTP/1.1\r\nHost: {endpoint}\r\nTransfer-Encoding: chunked\r\n\r\n"),
            format!("GET {url} HTTP/1.1\r\nHost: {endpoint}\r\nUpgrade: websocket\r\n\r\n"),
        ] {
            assert!(
                parse_fixture_request(request.as_bytes(), &[], &policy, Instant::now()).is_err(),
                "accepted request: {request:?}"
            );
        }
        let request = format!("GET {url} HTTP/1.1\r\nHost: {endpoint}\r\n\r\n");
        assert!(
            parse_fixture_request(request.as_bytes(), b"late body", &policy, Instant::now())
                .is_err()
        );
    }

    #[test]
    fn fixture_parser_rejects_urls_outside_allowlist_and_after_deadline() {
        let endpoint = endpoint();
        let url = "http://127.0.0.1:43210/index.html";
        let policy = fixture_policy(endpoint, &[url]);
        let other = "http://127.0.0.1:43210/other.html";
        let request = format!("GET {other} HTTP/1.1\r\nHost: {endpoint}\r\n\r\n");
        assert!(parse_fixture_request(request.as_bytes(), &[], &policy, Instant::now()).is_err());

        let expired = FixtureProxyPolicy {
            expires_at: Instant::now() - Duration::from_millis(1),
            ..policy
        };
        let request = format!("GET {url} HTTP/1.1\r\nHost: {endpoint}\r\n\r\n");
        assert!(parse_fixture_request(request.as_bytes(), &[], &expired, Instant::now()).is_err());
    }

    #[tokio::test]
    async fn shutdown_closes_listener_and_joins_partial_request() {
        let guard = Arc::new(Guard {
            session_id: "fixture-test".into(),
            admission: None,
            network: NetworkPolicy::default(),
            events: None,
            page_allowed: Mutex::default(),
        });
        let server_endpoint = endpoint();
        let proxy = PinProxy::start_fixture(
            guard,
            fixture_policy(server_endpoint, &["http://127.0.0.1:43210/index.html"]),
        )
        .await
        .unwrap();
        let mut client = TcpStream::connect(proxy.addr).await.unwrap();
        client.write_all(b"GET ").await.unwrap();

        proxy.shutdown(Duration::from_secs(1)).await.unwrap();
        let mut byte = [0u8; 1];
        assert!(matches!(client.read(&mut byte).await, Ok(0) | Err(_)));
        assert!(TcpStream::connect(proxy.addr).await.is_err());
    }
}

/// #925: traffic page interception never sees gets the page decision in the proxy.
#[cfg(test)]
mod connect_policy_tests {
    use super::*;
    use crate::policy::{
        BrowserEvent, BrowserRequest, LifecycleKind, NetworkPolicy, PolicyDecision,
    };

    type Calls = Arc<Mutex<Vec<(String, String)>>>;

    /// Offline names: `public.example` is public and never reachable here; `allowed.example`
    /// and `other.example` resolve to loopback, where the test listens.
    fn offline() -> NetworkPolicy {
        NetworkPolicy::default().with_resolver(|h| match h {
            "public.example" => vec!["93.184.216.34".parse().unwrap()],
            "allowed.example" | "other.example" => vec!["127.0.0.1".parse().unwrap()],
            _ => vec![],
        })
    }

    fn recording(
        decide: impl Fn(&BrowserRequest) -> PolicyDecision + Send + Sync + 'static,
    ) -> (NetworkPolicy, Calls) {
        let calls: Calls = Arc::default();
        let c = calls.clone();
        let network = offline().allow_with(move |r: &BrowserRequest| {
            c.lock()
                .unwrap()
                .push((r.resource_type.clone(), r.url.clone()));
            decide(r)
        });
        (network, calls)
    }

    struct Harness {
        guard: Arc<Guard>,
        proxy: PinProxy,
        denied: Arc<Mutex<Vec<BrowserEvent>>>,
    }

    impl Harness {
        async fn start(network: NetworkPolicy) -> Harness {
            let denied: Arc<Mutex<Vec<BrowserEvent>>> = Arc::default();
            let d = denied.clone();
            let guard = Arc::new(Guard {
                session_id: "connect-test".into(),
                admission: None,
                network,
                events: Some(Arc::new(move |e: &BrowserEvent| {
                    if matches!(e.kind, LifecycleKind::Denied) {
                        d.lock().unwrap().push(e.clone());
                    }
                })),
                page_allowed: Mutex::default(),
            });
            let proxy = PinProxy::start(guard.clone()).await.unwrap();
            Harness {
                guard,
                proxy,
                denied,
            }
        }

        /// Sends one proxy request head and returns the status line.
        async fn send(&self, head: &str) -> String {
            let mut client = TcpStream::connect(self.proxy.addr).await.unwrap();
            client.write_all(head.as_bytes()).await.unwrap();
            let mut buf = vec![0u8; 512];
            let n = tokio::time::timeout(Duration::from_secs(10), client.read(&mut buf))
                .await
                .unwrap()
                .unwrap();
            String::from_utf8_lossy(&buf[..n])
                .lines()
                .next()
                .unwrap_or("")
                .to_string()
        }

        async fn connect(&self, target: &str) -> String {
            self.send(&format!(
                "CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n"
            ))
            .await
        }

        fn connect_denials(&self) -> usize {
            self.denied
                .lock()
                .unwrap()
                .iter()
                .filter(|e| e.what.as_deref() == Some("Connect"))
                .count()
        }

        fn logged(&self) -> Vec<PinnedConnection> {
            self.proxy.log.lock().unwrap().clone()
        }
    }

    async fn listener() -> (u16, JoinHandle<io::Result<TcpStream>>) {
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = upstream.local_addr().unwrap().port();
        (
            port,
            tokio::spawn(async move { upstream.accept().await.map(|(s, _)| s) }),
        )
    }

    /// Test 1.
    #[tokio::test]
    async fn deny_all_callback_refuses_connect_before_any_upstream() {
        let (network, calls) = recording(|_| PolicyDecision::Deny("deny all".into()));
        let h = Harness::start(network).await;
        let status = h.connect("public.example:443").await;
        assert!(status.contains("403"), "{status}");
        assert!(h.logged().is_empty(), "no upstream connection may be made");
        assert_eq!(h.connect_denials(), 1);
        assert_eq!(
            *calls.lock().unwrap(),
            [(
                "Connect".to_string(),
                "https://public.example:443/".to_string()
            )]
        );
    }

    /// Test 2.
    #[tokio::test]
    async fn plain_http_absolute_form_is_checked_with_its_full_url() {
        let (network, calls) = recording(|_| PolicyDecision::Deny("deny all".into()));
        let h = Harness::start(network).await;
        let url = "http://public.example/path?q=1";
        let status = h
            .send(&format!(
                "GET {url} HTTP/1.1\r\nHost: public.example\r\n\r\n"
            ))
            .await;
        assert!(status.contains("403"), "{status}");
        assert!(h.logged().is_empty());
        assert_eq!(h.connect_denials(), 1);
        assert_eq!(
            *calls.lock().unwrap(),
            [("Connect".to_string(), url.to_string())]
        );
    }

    /// Test 3.
    #[tokio::test]
    async fn callback_allowing_one_host_admits_exactly_that_host() {
        let (port, accepted) = listener().await;
        let (network, _) = recording(|r| {
            if r.host == "allowed.example" {
                PolicyDecision::Allow
            } else {
                PolicyDecision::Deny("not listed".into())
            }
        });
        let h = Harness::start(network).await;
        let status = h.connect(&format!("allowed.example:{port}")).await;
        assert!(status.contains("200"), "{status}");
        assert!(accepted.await.unwrap().is_ok());
        let status = h.connect(&format!("other.example:{port}")).await;
        assert!(status.contains("403"), "{status}");
        let log = h.logged();
        assert_eq!(log.len(), 1, "{log:?}");
        assert_eq!(
            (log[0].host.as_str(), log[0].port),
            ("allowed.example", port)
        );
        assert_eq!(h.connect_denials(), 1);
    }

    /// Test 4.
    #[tokio::test]
    async fn a_pair_a_page_check_allowed_is_not_decided_again() {
        let (port, accepted) = listener().await;
        let (network, calls) = recording(|r| PolicyDecision::from(r.host == "allowed.example"));
        let h = Harness::start(network).await;
        // Page interception checks the full URL first, as Chrome's Fetch domain does.
        let url = format!("https://allowed.example:{port}/page?q=1");
        h.guard.check_request(&url, "Document").await.unwrap();
        let status = h.connect(&format!("allowed.example:{port}")).await;
        assert!(status.contains("200"), "{status}");
        assert!(accepted.await.unwrap().is_ok());
        assert_eq!(*calls.lock().unwrap(), [("Document".to_string(), url)]);
        assert_eq!(h.connect_denials(), 0);
    }

    /// Test 5: without a callback the proxy decision is the pin alone, as before.
    #[tokio::test]
    async fn without_a_callback_the_decision_is_unchanged() {
        let guard = Harness::start(offline()).await.guard;
        assert_eq!(
            guard.admit_proxied("public.example", 443, None).await,
            guard.network.pin("public.example").await
        );
        assert!(guard
            .admit_proxied("public.example", 443, None)
            .await
            .is_ok());
        assert!(guard
            .admit_proxied("allowed.example", 443, None)
            .await
            .is_err());
        assert!(guard.page_allowed.lock().unwrap().is_empty());
    }

    /// Test 6: an Allow cannot open the metadata or link-local hard block.
    #[tokio::test]
    async fn allow_callback_cannot_open_metadata_or_link_local() {
        let (network, _) = recording(|_| PolicyDecision::Allow);
        let h = Harness::start(network).await;
        for target in [
            "169.254.169.254:80",
            "metadata.google.internal:80",
            "[fe80::1]:443",
        ] {
            let status = h.connect(target).await;
            assert!(status.contains("403"), "{target}: {status}");
        }
        assert!(h.logged().is_empty());
        assert_eq!(h.connect_denials(), 3);
    }
}
