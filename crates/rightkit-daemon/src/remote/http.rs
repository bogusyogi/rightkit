//! axum adapter and rustls listener. The adapter reads the body within the
//! route's bound, converts to a [`Request`], and runs [`Daemon::handle`] on the
//! blocking pool (the core does file I/O and rightkit-http calls).

use std::collections::BTreeMap;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::extract::State;
use axum::http::{header, HeaderName, HeaderValue, StatusCode};
use axum::response::Response as AxumResponse;
use axum::Router;
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::{Notify, Semaphore};
use tokio_rustls::TlsAcceptor;

use crate::link::frame::{
    DEAD_AFTER_SECS, HEARTBEAT_SECS, H_LINK_ID, H_LINK_KEY, LINK_PATH, MAX_LINE, UPGRADE_TOKEN,
};
use crate::remote::core::{Accepted, Daemon, FrameOutcome, Out, Request, Response};

pub fn router(daemon: Arc<Daemon>) -> Router {
    Router::new().fallback(entry).with_state(daemon)
}

async fn entry(State(daemon): State<Arc<Daemon>>, req: axum::extract::Request) -> AxumResponse {
    if req.method() == axum::http::Method::GET && req.uri().path() == LINK_PATH {
        return link_entry(daemon, req).await;
    }
    let (parts, body) = req.into_parts();
    let path = parts.uri.path().to_string();
    let method = parts.method.as_str().to_string();
    let limit = daemon.body_limit(&method, &path);
    let bytes = match to_bytes(body, limit).await {
        Ok(b) => b,
        Err(_) => {
            return to_axum(Response {
                status: 413,
                body: json!({"error": "body_too_large"}),
                headers: vec![],
            })
        }
    };
    let mut headers = BTreeMap::new();
    for (k, v) in &parts.headers {
        if let Ok(v) = v.to_str() {
            // The first occurrence wins; a duplicated credential header never
            // gets a second chance to override the first.
            headers
                .entry(k.as_str().to_ascii_lowercase())
                .or_insert_with(|| v.to_string());
        }
    }
    let request = Request {
        method,
        path_and_query: parts
            .uri
            .path_and_query()
            .map_or(path, |p| p.as_str().to_string()),
        headers,
        body: bytes.to_vec(),
    };
    let d = daemon.clone();
    match tokio::task::spawn_blocking(move || d.handle(request)).await {
        Ok(resp) => to_axum(resp),
        Err(_) => to_axum(Response {
            status: 500,
            body: json!({"error": "internal"}),
            headers: vec![],
        }),
    }
}

/// `GET /v1/link`: authenticate the signed upgrade, answer `101`, then hand the upgraded
/// connection to [`run_link`]. A refused upgrade is an ordinary error response.
async fn link_entry(daemon: Arc<Daemon>, mut req: axum::extract::Request) -> AxumResponse {
    let mut headers = BTreeMap::new();
    for (k, v) in req.headers() {
        if let Ok(v) = v.to_str() {
            headers
                .entry(k.as_str().to_ascii_lowercase())
                .or_insert_with(|| v.to_string());
        }
    }
    let request = Request {
        method: "GET".into(),
        path_and_query: req
            .uri()
            .path_and_query()
            .map_or_else(|| LINK_PATH.to_string(), |p| p.as_str().to_string()),
        headers,
        body: Vec::new(),
    };
    let on_upgrade = hyper::upgrade::on(&mut req);
    let d = daemon.clone();
    let accepted = match tokio::task::spawn_blocking(move || d.link_accept(&request)).await {
        Ok(Ok(a)) => a,
        Ok(Err(resp)) => return to_axum(resp),
        Err(_) => {
            return to_axum(Response {
                status: 500,
                body: json!({"error": "internal"}),
                headers: vec![],
            })
        }
    };
    let link_id = accepted.link_id.clone();
    let key_hex = accepted.key_hex.clone();
    let response = AxumResponse::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(header::UPGRADE, UPGRADE_TOKEN)
        .header(header::CONNECTION, "Upgrade")
        .header(H_LINK_ID, link_id)
        .header(H_LINK_KEY, key_hex)
        .body(Body::empty());
    let Ok(response) = response else {
        daemon.link_closed(&accepted.session, "internal");
        return to_axum(Response {
            status: 500,
            body: json!({"error": "internal"}),
            headers: vec![],
        });
    };
    tokio::spawn(async move {
        match on_upgrade.await {
            Ok(upgraded) => run_link(daemon, accepted, TokioIo::new(upgraded)).await,
            Err(_) => daemon.link_closed(&accepted.session, "upgrade_failed"),
        }
    });
    response
}

/// Pump one upgraded link: a writer task sends queued frames and a heartbeat `ping` every
/// [`HEARTBEAT_SECS`] (and runs the lease housekeeping), the reader waits for the next line with
/// a [`DEAD_AFTER_SECS`] bound, so a silent peer is dropped after three missed heartbeats. One
/// blocked read per link; no polling.
async fn run_link<IO>(daemon: Arc<Daemon>, accepted: Accepted, io: IO)
where
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
{
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

    let (session, mut rx) = accepted.into_parts();
    let (rd, mut wr) = tokio::io::split(io);

    let writer_session = session.clone();
    let writer_daemon = daemon.clone();
    let writer = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(HEARTBEAT_SECS));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tick.tick().await;
        loop {
            tokio::select! {
                out = rx.recv() => match out {
                    Some(Out::Frame(frame)) => {
                        let line = writer_session.encode(frame);
                        if wr.write_all(line.as_bytes()).await.is_err() || wr.flush().await.is_err() {
                            break;
                        }
                    }
                    Some(Out::Close(reason)) => {
                        let line = writer_session.encode(json!({ "t": "close", "reason": reason }));
                        let _ = wr.write_all(line.as_bytes()).await;
                        let _ = wr.flush().await;
                        let _ = wr.shutdown().await;
                        break;
                    }
                    None => break,
                },
                _ = tick.tick() => {
                    let line = writer_session.encode(json!({ "t": "ping" }));
                    if wr.write_all(line.as_bytes()).await.is_err() || wr.flush().await.is_err() {
                        break;
                    }
                    let d = writer_daemon.clone();
                    let _ = tokio::task::spawn_blocking(move || d.link_tick()).await;
                }
            }
        }
    });

    let mut reader = tokio::io::BufReader::new(rd);
    let reason: String = loop {
        let mut buf: Vec<u8> = Vec::new();
        let read = tokio::select! {
            r = tokio::time::timeout(
                Duration::from_secs(DEAD_AFTER_SECS),
                (&mut reader).take(MAX_LINE as u64 + 1).read_until(b'\n', &mut buf),
            ) => Some(r),
            _ = session.stop.notified() => None,
        };
        match read {
            None => break "closed".to_string(),
            Some(Err(_)) => break "heartbeat_timeout".to_string(),
            Some(Ok(Err(_))) => break "io_error".to_string(),
            Some(Ok(Ok(0))) => break "eof".to_string(),
            Some(Ok(Ok(_))) => {}
        }
        // A line without its newline is a frame over the size bound, or a cut-off one.
        if buf.last() != Some(&b'\n') {
            break "bad_frame".to_string();
        }
        buf.pop();
        let Ok(text) = String::from_utf8(buf) else {
            break "bad_utf8".to_string();
        };
        let (d, s) = (daemon.clone(), session.clone());
        match tokio::task::spawn_blocking(move || d.link_frame(&s, &text)).await {
            Ok(FrameOutcome::Continue) => {}
            Ok(FrameOutcome::Close(r)) => break r,
            Err(_) => break "internal".to_string(),
        }
    };
    daemon.link_closed(&session, &reason);
    let _ = tokio::time::timeout(Duration::from_secs(2), writer).await;
}

fn to_axum(resp: Response) -> AxumResponse {
    let status = StatusCode::from_u16(resp.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut builder = AxumResponse::builder().status(status);
    let body = match &resp.body {
        Value::Null => Body::empty(),
        v => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(v.to_string())
        }
    };
    if let Some(h) = builder.headers_mut() {
        for (k, v) in resp.headers {
            if let (Ok(k), Ok(v)) = (HeaderName::try_from(k), HeaderValue::try_from(v)) {
                h.insert(k, v);
            }
        }
    }
    builder
        .body(body)
        .unwrap_or_else(|_| AxumResponse::new(Body::empty()))
}

/// Build a rustls server configuration from PEM certificate chain and key.
pub fn load_tls(cert_pem: &[u8], key_pem: &[u8]) -> io::Result<Arc<rustls::ServerConfig>> {
    let bad =
        |e: &dyn std::fmt::Display| io::Error::new(io::ErrorKind::InvalidInput, e.to_string());
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(cert_pem)
        .collect::<Result<_, _>>()
        .map_err(|e| bad(&e))?;
    let key = PrivateKeyDer::from_pem_slice(key_pem).map_err(|e| bad(&e))?;
    let mut cfg = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| bad(&e))?
    .with_no_client_auth()
    .with_single_cert(certs, key)
    .map_err(|e| bad(&e))?;
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(cfg))
}

const MAX_CONNECTIONS: usize = 1024;

/// Serve the daemon over TLS until `shutdown` resolves, then drain in-flight
/// connections (bounded) and return. Emits `LISTENING` and `SHUTDOWN`.
pub async fn serve(
    daemon: Arc<Daemon>,
    listener: TcpListener,
    tls: Arc<rustls::ServerConfig>,
    shutdown: impl Future<Output = ()>,
) -> io::Result<()> {
    serve_with(daemon, listener, tls, shutdown, |_| {}).await
}

/// [`serve`] with a hook called with the bound address once `LISTENING` is
/// emitted, immediately before the first accept is polled (no await between).
async fn serve_with(
    daemon: Arc<Daemon>,
    listener: TcpListener,
    tls: Arc<rustls::ServerConfig>,
    shutdown: impl Future<Output = ()>,
    on_listening: impl FnOnce(SocketAddr),
) -> io::Result<()> {
    let acceptor = TlsAcceptor::from(tls);
    let app = router(daemon.clone());
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let local = listener.local_addr()?;
    daemon.emit("LISTENING", json!({ "addr": local.to_string() }));
    on_listening(local);
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                let Ok(permit) = permits.clone().try_acquire_owned() else { continue };
                let acceptor = acceptor.clone();
                let svc = TowerToHyperService::new(app.clone());
                let mut stop = stop_rx.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let Ok(Ok(tls_stream)) =
                        tokio::time::timeout(Duration::from_secs(10), acceptor.accept(stream)).await
                    else {
                        return;
                    };
                    // `with_upgrades` lets `GET /v1/link` take over the connection (`rk-link/1`).
                    let conn = http1::Builder::new()
                        .timer(TokioTimer::new())
                        .header_read_timeout(Duration::from_secs(10))
                        .serve_connection(TokioIo::new(tls_stream), svc)
                        .with_upgrades();
                    tokio::pin!(conn);
                    tokio::select! {
                        _ = conn.as_mut() => return,
                        _ = stop.changed() => {}
                    }
                    conn.as_mut().graceful_shutdown();
                    let _ = conn.await;
                });
            }
        }
    }
    drop(listener);
    // Stop in-flight connections gracefully and end every link; wait (bounded) for the
    // connection tasks, each of which holds one permit.
    let _ = stop_tx.send(true);
    daemon.close_all_links("shutdown");
    let _ = tokio::time::timeout(
        Duration::from_secs(10),
        permits.acquire_many(MAX_CONNECTIONS as u32),
    )
    .await;
    daemon.emit("SHUTDOWN", json!({}));
    Ok(())
}

/// Cancels a [`serve_blocking`] call from any thread. Clones share one signal.
///
/// Built on [`Notify::notify_one`], which stores a permit: `stop` is safe to
/// call before the server starts waiting, from a non-runtime thread, more than
/// once, and without any polling.
#[derive(Clone, Default)]
pub struct StopHandle {
    notify: Arc<Notify>,
}

impl StopHandle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask the server to stop accepting, drain, and return. Never blocks.
    pub fn stop(&self) {
        self.notify.notify_one();
    }
}

/// Worker threads of the runtime [`serve_blocking`] owns. The accept loop, TLS
/// handshakes and connection I/O are async and light; request handling runs on
/// the blocking pool, so two workers are enough and keep the thread count small.
const BLOCKING_WORKERS: usize = 2;
/// Cap on the blocking pool that runs [`Daemon::handle`] (file I/O).
const BLOCKING_POOL_MAX: usize = 32;
/// How long the owned runtime waits for blocking handlers after the drain.
const RUNTIME_SHUTDOWN: Duration = Duration::from_secs(5);

/// Blocking entry point for [`serve`]: owns a dedicated tokio runtime so callers
/// need no tokio dependency.
///
/// `listener` is already bound; it is switched to non-blocking and handed to
/// tokio. `ready` receives the bound address once `LISTENING` is emitted,
/// immediately before the first accept (the socket was already listening, so
/// no connection is refused after this). `stop` ends the call: the listener
/// closes, in-flight connections drain for the same 10 s bound as [`serve`],
/// then the runtime is shut down (blocking handlers get a further 5 s; the
/// threads of a handler stuck past that are abandoned). Runtime build failure
/// and panics come back as `Err`.
///
/// Must not be called from inside a tokio runtime (including `spawn_blocking`
/// threads): that returns an error instead of panicking.
pub fn serve_blocking(
    daemon: Arc<Daemon>,
    listener: std::net::TcpListener,
    tls: Arc<rustls::ServerConfig>,
    ready: std::sync::mpsc::Sender<SocketAddr>,
    stop: StopHandle,
) -> io::Result<()> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(io::Error::other(
            "serve_blocking called from inside a tokio runtime; use serve or a plain thread",
        ));
    }
    listener.set_nonblocking(true)?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(BLOCKING_WORKERS)
        .max_blocking_threads(BLOCKING_POOL_MAX)
        .thread_name("rightkit-daemon")
        .enable_all()
        .build()?;
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        rt.block_on(async move {
            let listener = TcpListener::from_std(listener)?;
            serve_with(
                daemon,
                listener,
                tls,
                async move { stop.notify.notified().await },
                move |addr| {
                    // A dropped receiver only means nobody waits for readiness.
                    let _ = ready.send(addr);
                },
            )
            .await
        })
    }));
    // Runs on every path (success, error, panic): no worker outlives the call.
    rt.shutdown_timeout(RUNTIME_SHUTDOWN);
    match outcome {
        Ok(result) => result,
        Err(payload) => {
            let what = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
            Err(io::Error::other(format!("daemon server panicked: {what}")))
        }
    }
}
