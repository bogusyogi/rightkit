//! The real listener: rustls termination, the axum adapter and graceful shutdown.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use rightkit_daemon::{load_tls, router, serve};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsConnector;
use tower::ServiceExt;

fn self_signed() -> (String, String) {
    let ck = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    (ck.cert.pem(), ck.key_pair.serialize_pem())
}

async fn https(addr: std::net::SocketAddr, cert_pem: &str, raw: &str) -> String {
    let mut roots = rustls::RootCertStore::empty();
    for c in rustls::pki_types::CertificateDer::pem_slice_iter(cert_pem.as_bytes()) {
        roots.add(c.unwrap()).unwrap();
    }
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    let tcp = TcpStream::connect(addr).await.unwrap();
    let mut tls = TlsConnector::from(Arc::new(cfg))
        .connect("localhost".try_into().unwrap(), tcp)
        .await
        .unwrap();
    tls.write_all(raw.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    let _ = tls.read_to_end(&mut out).await;
    String::from_utf8_lossy(&out).into_owned()
}

use rustls::pki_types::pem::PemObject;

#[tokio::test(flavor = "multi_thread")]
async fn serves_https_logs_lifecycle_and_shuts_down_gracefully() {
    let h = Harness::new();
    let (cert, key) = self_signed();
    let tls = load_tls(cert.as_bytes(), key.as_bytes()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let daemon = h.daemon.clone();
    let server = tokio::spawn(serve(daemon, listener, tls, async {
        let _ = stop_rx.await;
    }));

    // An unauthenticated mutation is refused over TLS.
    let resp = https(
        addr,
        &cert,
        "POST /v1/uploads HTTP/1.1\r\nHost: localhost\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
    )
    .await;
    assert!(resp.starts_with("HTTP/1.1 401"), "{resp}");
    assert!(resp.contains("unauthenticated"));

    // A correctly signed request is accepted over the same listener.
    let dev = h.enrol();
    let body = br#"{"a":1}"#;
    let req = dev.request_at(
        "PATCH",
        "/v1/publish-jobs/tls-job",
        body,
        h.now(),
        &next_nonce(),
        &[("if-match", "0"), ("idempotency-key", "tls-1")],
    );
    let mut raw = format!(
        "PATCH {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n",
        req.path_and_query,
        body.len()
    );
    for (k, v) in &req.headers {
        raw.push_str(&format!("{k}: {v}\r\n"));
    }
    raw.push_str("\r\n");
    raw.push_str(std::str::from_utf8(body).unwrap());
    let resp = https(addr, &cert, &raw).await;
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
    assert!(resp.to_ascii_lowercase().contains("etag: \"1\""));

    // Plain TCP without TLS never reaches the router.
    let mut plain = TcpStream::connect(addr).await.unwrap();
    plain
        .write_all(b"GET /v1/uploads/x HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .unwrap();
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), plain.read_to_end(&mut buf)).await;
    assert!(!String::from_utf8_lossy(&buf).contains("HTTP/1.1 4"));

    stop_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(15), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let names: Vec<String> = h
        .daemon
        .events()
        .iter()
        .map(|e| e["event"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains(&"LISTENING".to_string()));
    assert_eq!(names.last().map(String::as_str), Some("SHUTDOWN"));
    let listening = h
        .daemon
        .events()
        .into_iter()
        .find(|e| e["event"] == "LISTENING")
        .unwrap();
    assert_eq!(listening["addr"], addr.to_string());
    assert!(h.rejections().contains(&"missing_credentials".to_string()));

    // After shutdown the port no longer accepts.
    assert!(TcpStream::connect(addr).await.is_err());
}

#[test]
fn load_tls_rejects_garbage() {
    assert!(load_tls(b"not pem", b"not pem").is_err());
    let (cert, _) = self_signed();
    let (_, other_key) = self_signed();
    assert!(
        load_tls(cert.as_bytes(), other_key.as_bytes()).is_err(),
        "key must match the certificate"
    );
}

#[tokio::test]
async fn adapter_bounds_bodies_and_maps_headers() {
    let h = Harness::new();
    let dev = h.enrol();
    let app = router(h.daemon.clone());
    let (id, _) = dev.upload_init(&h, 500, 64);

    // Past chunk_max + 1 the adapter refuses before authenticating or buffering more.
    let oversized = vec![0u8; 66];
    let req = dev.request(&h, "PUT", &format!("/v1/uploads/{id}/chunks/0"), &oversized);
    let mut b = axum::http::Request::builder()
        .method("PUT")
        .uri(&req.path_and_query);
    for (k, v) in &req.headers {
        b = b.header(k, v);
    }
    let resp = app
        .clone()
        .oneshot(b.body(axum::body::Body::from(oversized)).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), 413);

    // A non-chunk route has the small JSON bound.
    let big = vec![b' '; 70 * 1024];
    let resp = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/v1/uploads")
                .body(axum::body::Body::from(big))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 413);

    // A duplicated credential header does not let a second value override the first.
    let req = dev.request(&h, "GET", &format!("/v1/uploads/{id}"), b"");
    let mut b = axum::http::Request::builder()
        .method("GET")
        .uri(&req.path_and_query);
    for (k, v) in &req.headers {
        b = b.header(k, v);
    }
    b = b.header("authorization", "RK-Device dev_other");
    let resp = app
        .oneshot(b.body(axum::body::Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["uploadId"], json!(id));
}
