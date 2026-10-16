//! TLS against a real local rustls server under throwaway CAs generated here (rcgen).
//! Covers certificate acceptance and rejection in both clients, the ALPN the async client
//! offers, and `https_only` on an https to http redirect.

use std::io::{Read, Write};
use std::net::{IpAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rcgen::{
    date_time_ymd, BasicConstraints, Certificate, CertificateParams, DnType,
    ExtendedKeyUsagePurpose, IsCa, KeyPair, SanType,
};
use rightkit_http::testing::{add_bundled_test_root, loopback_config};
use rightkit_http::*;

struct Ca {
    cert: Certificate,
    key: KeyPair,
}

fn new_ca(name: &str) -> Ca {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.distinguished_name.push(DnType::CommonName, name);
    let cert = params.self_signed(&key).unwrap();
    Ca { cert, key }
}

/// The CA the clients are told to trust (through the testing seam, Bundled only).
fn trusted_ca() -> &'static Ca {
    static CA: OnceLock<Ca> = OnceLock::new();
    static REGISTER: Once = Once::new();
    let ca = CA.get_or_init(|| new_ca("rightkit-http test CA (trusted)"));
    REGISTER.call_once(|| add_bundled_test_root(ca.cert.der().to_vec()));
    ca
}

struct Leaf {
    chain: Vec<Vec<u8>>,
    key: Vec<u8>,
}

fn leaf(ca: &Ca, san: SanType, not_before: (i32, u8, u8), not_after: (i32, u8, u8)) -> Leaf {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.subject_alt_names = vec![san];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.not_before = date_time_ymd(not_before.0, not_before.1, not_before.2);
    params.not_after = date_time_ymd(not_after.0, not_after.1, not_after.2);
    let cert = params.signed_by(&key, &ca.cert, &ca.key).unwrap();
    Leaf {
        chain: vec![cert.der().to_vec()],
        key: key.serialize_der(),
    }
}

fn loopback_leaf(ca: &Ca) -> Leaf {
    leaf(
        ca,
        SanType::IpAddress("127.0.0.1".parse::<IpAddr>().unwrap()),
        (2020, 1, 1),
        (2999, 1, 1),
    )
}

struct TlsServer {
    port: u16,
    stop: Arc<AtomicBool>,
    #[cfg_attr(not(feature = "async"), allow(dead_code))] // read by the async ALPN test only
    alpn: Arc<Mutex<Vec<Option<Vec<u8>>>>>,
    thread: Option<JoinHandle<()>>,
}

impl TlsServer {
    /// Serves `leaf`; answers every request with `respond(path)` as raw HTTP/1.1 bytes.
    fn start(leaf: Leaf, respond: impl Fn(&str) -> Vec<u8> + Send + Sync + 'static) -> Self {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let chain = leaf
            .chain
            .into_iter()
            .map(rustls::pki_types::CertificateDer::from)
            .collect();
        let key = rustls::pki_types::PrivateKeyDer::try_from(leaf.key).unwrap();
        let mut config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .unwrap();
        // The server prefers h2 when offered: a client that offers it would get it.
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        let config = Arc::new(config);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let alpn = Arc::new(Mutex::new(Vec::new()));
        let respond = Arc::new(respond);
        let (s, a) = (stop.clone(), alpn.clone());
        let thread = thread::spawn(move || {
            for conn in listener.incoming() {
                if s.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(tcp) = conn else { continue };
                let (config, respond, a) = (config.clone(), respond.clone(), a.clone());
                thread::spawn(move || {
                    let _ = tcp.set_read_timeout(Some(Duration::from_secs(5)));
                    let Ok(conn) = rustls::ServerConnection::new(config) else {
                        return;
                    };
                    let mut tls = rustls::StreamOwned::new(conn, tcp);
                    let mut head = Vec::new();
                    let mut b = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        match tls.read(&mut b) {
                            Ok(1) => head.push(b[0]),
                            _ => return, // handshake refused by the client, or closed
                        }
                    }
                    a.lock()
                        .unwrap()
                        .push(tls.conn.alpn_protocol().map(<[u8]>::to_vec));
                    let text = String::from_utf8_lossy(&head).into_owned();
                    let path = text.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let _ = tls.write_all(&respond(&path));
                    let _ = tls.flush();
                    tls.conn.send_close_notify();
                    let _ = tls.flush();
                });
            }
        });
        Self {
            port,
            stop,
            alpn,
            thread: Some(thread),
        }
    }

    fn ok(leaf: Leaf) -> Self {
        Self::start(leaf, |_| {
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_vec()
        })
    }

    fn url(&self, path: &str) -> String {
        format!("https://127.0.0.1:{}{}", self.port, path)
    }

    #[cfg_attr(not(feature = "async"), allow(dead_code))]
    fn negotiated(&self) -> Vec<Option<Vec<u8>>> {
        self.alpn.lock().unwrap().clone()
    }
}

impl Drop for TlsServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = std::net::TcpStream::connect(("127.0.0.1", self.port));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn cfg(trust: TrustRoots) -> ClientConfig {
    ClientConfig {
        trust,
        retry: RetryPolicy::none(),
        request_timeout: Duration::from_secs(8),
        response_timeout: Some(Duration::from_secs(8)),
        connect_timeout: Some(Duration::from_secs(5)),
        ..loopback_config()
    }
}

fn is_tls_refusal(e: &HttpError) -> bool {
    matches!(e, HttpError::Transport { .. })
}

// ------------------------------------------------------------- blocking client
#[test]
fn blocking_bundled_accepts_a_valid_chain_from_the_injected_root() {
    let server = TlsServer::ok(loopback_leaf(trusted_ca()));
    let c = Client::new(cfg(TrustRoots::Bundled));
    assert_eq!(c.send(&Request::get(server.url("/"))).unwrap().text(), "ok");
}

#[test]
fn blocking_platform_rejects_a_root_that_is_not_in_the_os_store() {
    let server = TlsServer::ok(loopback_leaf(trusted_ca()));
    let c = Client::new(cfg(TrustRoots::Platform));
    let err = c.send(&Request::get(server.url("/"))).unwrap_err();
    assert!(is_tls_refusal(&err), "{err:?}");
}

#[test]
fn blocking_bundled_rejects_an_untrusted_root() {
    let other = new_ca("some other CA");
    let _ = trusted_ca();
    let server = TlsServer::ok(loopback_leaf(&other));
    let err = Client::new(cfg(TrustRoots::Bundled))
        .send(&Request::get(server.url("/")))
        .unwrap_err();
    assert!(is_tls_refusal(&err), "{err:?}");
}

#[test]
fn blocking_bundled_rejects_the_wrong_host() {
    let ca = trusted_ca();
    let wrong = leaf(
        ca,
        SanType::DnsName("other.example".try_into().unwrap()),
        (2020, 1, 1),
        (2999, 1, 1),
    );
    let server = TlsServer::ok(wrong);
    let err = Client::new(cfg(TrustRoots::Bundled))
        .send(&Request::get(server.url("/")))
        .unwrap_err();
    assert!(is_tls_refusal(&err), "{err:?}");
}

#[test]
fn blocking_bundled_rejects_an_expired_certificate() {
    let ca = trusted_ca();
    let expired = leaf(
        ca,
        SanType::IpAddress("127.0.0.1".parse::<IpAddr>().unwrap()),
        (2019, 1, 1),
        (2020, 1, 1),
    );
    let server = TlsServer::ok(expired);
    let err = Client::new(cfg(TrustRoots::Bundled))
        .send(&Request::get(server.url("/")))
        .unwrap_err();
    assert!(is_tls_refusal(&err), "{err:?}");
}

#[test]
fn blocking_https_only_refuses_an_https_to_http_redirect() {
    let server = TlsServer::start(loopback_leaf(trusted_ca()), |_| {
        b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            .to_vec()
    });
    let c = Client::new(ClientConfig {
        https_only: true,
        max_redirects: 3,
        ..cfg(TrustRoots::Bundled)
    });
    let err = c.send(&Request::get(server.url("/"))).unwrap_err();
    assert!(
        matches!(err, HttpError::HttpsRequired { redirect: true, ref scheme, .. } if scheme == "http"),
        "{err:?}"
    );
}

// ---------------------------------------------------------------- async client
#[cfg(feature = "async")]
mod async_side {
    use super::*;

    #[tokio::test]
    async fn bundled_accepts_a_valid_chain_and_negotiates_http1_not_h2() {
        let server = TlsServer::ok(loopback_leaf(trusted_ca()));
        let c = AsyncClient::new(cfg(TrustRoots::Bundled)).unwrap();
        let resp = c.send(&Request::get(server.url("/"))).await.unwrap();
        assert_eq!(resp.text(), "ok");
        // The server offered h2 first; the client advertised only http/1.1, so a
        // server-selected h2 (which would panic without reqwest's `http2` feature) cannot
        // happen whatever other crates enable.
        assert_eq!(server.negotiated(), vec![Some(b"http/1.1".to_vec())]);
    }

    #[tokio::test]
    async fn platform_rejects_a_root_that_is_not_in_the_os_store() {
        let server = TlsServer::ok(loopback_leaf(trusted_ca()));
        let c = AsyncClient::new(cfg(TrustRoots::Platform)).unwrap();
        let err = c.send(&Request::get(server.url("/"))).await.unwrap_err();
        assert!(is_tls_refusal(&err), "{err:?}");
    }

    #[tokio::test]
    async fn bundled_rejects_an_untrusted_root() {
        let other = new_ca("some other CA");
        let _ = trusted_ca();
        let server = TlsServer::ok(loopback_leaf(&other));
        let c = AsyncClient::new(cfg(TrustRoots::Bundled)).unwrap();
        let err = c.send(&Request::get(server.url("/"))).await.unwrap_err();
        assert!(is_tls_refusal(&err), "{err:?}");
    }

    #[tokio::test]
    async fn bundled_rejects_the_wrong_host() {
        let wrong = leaf(
            trusted_ca(),
            SanType::DnsName("other.example".try_into().unwrap()),
            (2020, 1, 1),
            (2999, 1, 1),
        );
        let server = TlsServer::ok(wrong);
        let c = AsyncClient::new(cfg(TrustRoots::Bundled)).unwrap();
        let err = c.send(&Request::get(server.url("/"))).await.unwrap_err();
        assert!(is_tls_refusal(&err), "{err:?}");
    }

    #[tokio::test]
    async fn bundled_rejects_an_expired_certificate() {
        let expired = leaf(
            trusted_ca(),
            SanType::IpAddress("127.0.0.1".parse::<IpAddr>().unwrap()),
            (2019, 1, 1),
            (2020, 1, 1),
        );
        let server = TlsServer::ok(expired);
        let c = AsyncClient::new(cfg(TrustRoots::Bundled)).unwrap();
        let err = c.send(&Request::get(server.url("/"))).await.unwrap_err();
        assert!(is_tls_refusal(&err), "{err:?}");
    }

    #[tokio::test]
    async fn https_only_refuses_an_https_to_http_redirect() {
        let server = TlsServer::start(loopback_leaf(trusted_ca()), |_| {
            b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_vec()
        });
        let c = AsyncClient::new(ClientConfig {
            https_only: true,
            max_redirects: 3,
            ..cfg(TrustRoots::Bundled)
        })
        .unwrap();
        let err = c.send(&Request::get(server.url("/"))).await.unwrap_err();
        assert!(
            matches!(err, HttpError::HttpsRequired { redirect: true, ref scheme, .. } if scheme == "http"),
            "{err:?}"
        );
    }
}
