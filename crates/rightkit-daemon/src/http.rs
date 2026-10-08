//! axum adapter and rustls listener. The adapter reads the body within the
//! route's bound, converts to a [`Request`], and runs [`Daemon::handle`] on the
//! blocking pool (the core does file I/O and rightkit-http calls).

use std::collections::BTreeMap;
use std::future::Future;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::extract::State;
use axum::http::{header, HeaderName, HeaderValue, StatusCode};
use axum::response::Response as AxumResponse;
use axum::Router;
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::server::graceful::GracefulShutdown;
use hyper_util::service::TowerToHyperService;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio_rustls::TlsAcceptor;

use crate::daemon::{Daemon, Request, Response};

pub fn router(daemon: Arc<Daemon>) -> Router {
    Router::new().fallback(entry).with_state(daemon)
}

async fn entry(State(daemon): State<Arc<Daemon>>, req: axum::extract::Request) -> AxumResponse {
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
    let acceptor = TlsAcceptor::from(tls);
    let app = router(daemon.clone());
    let graceful = GracefulShutdown::new();
    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    daemon.emit(
        "LISTENING",
        json!({ "addr": listener.local_addr()?.to_string() }),
    );
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                let Ok(permit) = permits.clone().try_acquire_owned() else { continue };
                let acceptor = acceptor.clone();
                let svc = TowerToHyperService::new(app.clone());
                let watcher = graceful.watcher();
                tokio::spawn(async move {
                    let _permit = permit;
                    let Ok(Ok(tls_stream)) =
                        tokio::time::timeout(Duration::from_secs(10), acceptor.accept(stream)).await
                    else {
                        return;
                    };
                    let conn = http1::Builder::new()
                        .timer(TokioTimer::new())
                        .header_read_timeout(Duration::from_secs(10))
                        .serve_connection(TokioIo::new(tls_stream), svc);
                    let _ = watcher.watch(conn).await;
                });
            }
        }
    }
    drop(listener);
    tokio::select! {
        _ = graceful.shutdown() => {}
        _ = tokio::time::sleep(Duration::from_secs(10)) => {}
    }
    daemon.emit("SHUTDOWN", json!({}));
    Ok(())
}
