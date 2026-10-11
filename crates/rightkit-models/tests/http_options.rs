//! `ModelHttpOptions`: selectable trust roots, a validated user agent, configurable timeouts and
//! retries (with one total budget), and the development-only loopback allowance behind the
//! `dev-worker-override` feature. Servers are in-process listeners on 127.0.0.1; nothing
//! leaves the machine.

use rightkit_http::AddressPolicy;
use rightkit_models::download::{download_resumable, DownloadRequest};
use rightkit_models::http::{
    HttpError, ModelHttp, ModelHttpOptions, ProxyMode, TrustRoots, DEFAULT_RETRIES,
    DEV_WORKER_OVERRIDE_ENABLED,
};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn own_user_agent() -> String {
    format!("rightkit-models/{}", env!("CARGO_PKG_VERSION"))
}

/// A response with `Content-Length` and `Connection: close`.
fn reply(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let mut out = head.into_bytes();
    out.extend_from_slice(body);
    out
}

struct Request {
    line: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// One thread per connection; `handler` answers on the stream. Returns the port and a count
/// of requests received.
fn serve(handler: impl Fn(Request, TcpStream) + Send + Sync + 'static) -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let handler = Arc::new(handler);
    let count = hits.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (handler, count) = (handler.clone(), count.clone());
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                let mut headers = Vec::new();
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':') {
                        headers.push((k.trim().to_string(), v.trim().to_string()));
                    }
                }
                let length = headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                    .and_then(|(_, v)| v.parse::<usize>().ok())
                    .unwrap_or(0);
                let mut body = vec![0u8; length];
                let _ = reader.read_exact(&mut body);
                count.fetch_add(1, Ordering::SeqCst);
                handler(
                    Request {
                        line,
                        headers,
                        body,
                    },
                    stream,
                );
            });
        }
    });
    (port, hits)
}

/// Answers every request with `bytes`.
fn fixed(bytes: Vec<u8>) -> (u16, Arc<AtomicUsize>) {
    serve(move |_, mut stream| {
        let _ = stream.write_all(&bytes);
    })
}

/// Sends the headers at once, then `chunk` every `gap`, `times` times.
fn drip(chunk: &'static [u8], gap: Duration, times: usize) -> (u16, Arc<AtomicUsize>) {
    serve(move |_, mut stream| {
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            chunk.len() * times
        );
        if stream.write_all(head.as_bytes()).is_err() {
            return;
        }
        for _ in 0..times {
            std::thread::sleep(gap);
            if stream.write_all(chunk).is_err() {
                return;
            }
            let _ = stream.flush();
        }
    })
}

/// Reaches 127.0.0.1 like a client built for a local test server.
fn loopback_policy() -> AddressPolicy {
    AddressPolicy::public_web().allow_if(vec!["127.0.0.1".to_string()], |ip: &IpAddr| {
        ip.is_loopback()
    })
}

fn options() -> ModelHttpOptions {
    ModelHttpOptions {
        proxy: ProxyMode::None,
        address_policy: loopback_policy(),
        ..Default::default()
    }
}

fn fetch(
    client: &ModelHttp,
    url: &str,
    dir: &std::path::Path,
) -> Result<(), rightkit_models::Error> {
    let dest = dir.join("f.bin");
    download_resumable(
        client,
        DownloadRequest {
            url,
            part_path: &rightkit_models::download::part_path_for(&dest),
            dest_path: &dest,
            expected_sha256: None,
            authorization: None,
            cancelled: &|| false,
        },
        |_, _| {},
    )
    .map(|_| ())
}

// ---- trust roots ----------------------------------------------------------------------

#[test]
fn trust_roots_default_to_the_platform_store_and_can_select_the_bundled_set() {
    let default = ModelHttp::new(ModelHttpOptions::default());
    assert_eq!(default.client().verifier_kind(), "platform");
    assert_eq!(default.client().config().trust, TrustRoots::Platform);

    let bundled = ModelHttp::new(ModelHttpOptions {
        trust: TrustRoots::Bundled,
        ..Default::default()
    });
    assert_eq!(bundled.client().verifier_kind(), "bundled");
    assert_eq!(bundled.client().config().trust, TrustRoots::Bundled);
}

// ---- user agent -----------------------------------------------------------------------

#[test]
fn the_user_agent_is_validated_as_a_header_value() {
    for bad in [
        "Bad/1\r\nX-Injected: 1",
        "Bad/1\n",
        "",
        "  ",
        " lead",
        "trail ",
        "caf\u{e9}/1",
        "nul\0",
    ] {
        let opts = ModelHttpOptions {
            user_agent: Some(bad.to_string()),
            ..Default::default()
        };
        assert!(opts.validate().is_err(), "{bad:?}");
        match ModelHttp::try_new(opts) {
            Err(HttpError::InvalidRequest(message)) => {
                assert!(message.contains("user agent"), "{message}");
            }
            Err(other) => panic!("{bad:?}: {other}"),
            Ok(_) => panic!("{bad:?} was accepted"),
        }
    }
    let bad_app = ModelHttpOptions {
        app: Some(("My\r\nApp".to_string(), "1".to_string())),
        ..Default::default()
    };
    assert!(bad_app.validate().is_err());
}

#[test]
fn a_valid_app_value_is_accepted_and_sent() {
    let (port, _) = serve(|request, mut stream| {
        let agent = request.header("user-agent").unwrap_or_default().to_string();
        let _ = stream.write_all(&reply("200 OK", &[], agent.as_bytes()));
    });
    let client = ModelHttp::try_new(ModelHttpOptions {
        app: Some(("HeardRight".to_string(), "2.0.1".to_string())),
        ..options()
    })
    .expect("a valid app value");
    let tmp = tempfile::tempdir().unwrap();
    fetch(&client, &format!("http://127.0.0.1:{port}/ua"), tmp.path()).unwrap();
    assert_eq!(
        fs::read_to_string(tmp.path().join("f.bin")).unwrap(),
        format!("HeardRight/2.0.1 {}", own_user_agent())
    );
}

#[test]
fn new_never_fails_and_falls_back_to_the_default_user_agent() {
    let client = ModelHttp::new(ModelHttpOptions {
        user_agent: Some("Bad/1\r\nX-Injected: 1".to_string()),
        ..options()
    });
    assert_eq!(client.client().config().user_agent, own_user_agent());

    // A bad explicit value does not discard a valid app value.
    let client = ModelHttp::new(ModelHttpOptions {
        user_agent: Some("Bad/1\r\n".to_string()),
        app: Some(("HeardRight".to_string(), "2.0.1".to_string())),
        ..options()
    });
    assert_eq!(
        client.client().config().user_agent,
        format!("HeardRight/2.0.1 {}", own_user_agent())
    );

    // On the wire: the injected header never arrives.
    let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
    let record = seen.clone();
    let (port, _) = serve(move |request, mut stream| {
        *record.lock().unwrap() = request.headers.clone();
        let _ = stream.write_all(&reply("200 OK", &[], b"ok"));
    });
    let client = ModelHttp::new(ModelHttpOptions {
        user_agent: Some("Bad/1\r\nX-Injected: 1".to_string()),
        ..options()
    });
    let tmp = tempfile::tempdir().unwrap();
    fetch(&client, &format!("http://127.0.0.1:{port}/ua"), tmp.path()).unwrap();
    let headers = seen.lock().unwrap().clone();
    assert!(
        headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("user-agent") && *v == own_user_agent()),
        "{headers:?}"
    );
    assert!(!headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("x-injected")));
}

// ---- timeouts and retries are configurable, defaults unchanged ---------------------------

#[test]
fn defaults_are_the_settings_downloads_always_had() {
    let http = ModelHttp::default();
    let config = http.client().config();
    assert_eq!(config.connect_timeout, Some(Duration::from_secs(30)));
    assert_eq!(config.response_timeout, Some(Duration::from_secs(60)));
    assert_eq!(config.recv_body_timeout, None);
    assert_eq!(http.read_timeout(), Some(Duration::from_secs(120)));
    assert_eq!(http.total_timeout(), None);
    assert_eq!(http.retries(), DEFAULT_RETRIES);
    assert_eq!(DEFAULT_RETRIES, 2, "three attempts in all, as before");
    assert_eq!(http.dev_loopback_host(), None);
}

#[test]
fn every_timeout_and_the_retry_count_reach_the_transport() {
    let http = ModelHttp::new(ModelHttpOptions {
        connect_timeout: Some(Duration::from_secs(5)),
        header_timeout: Some(Duration::from_secs(7)),
        body_timeout: Some(Duration::from_secs(9)),
        read_timeout: Some(Duration::from_secs(11)),
        total_timeout: Some(Duration::from_secs(13)),
        retries: 5,
        ..Default::default()
    });
    let config = http.client().config();
    assert_eq!(config.connect_timeout, Some(Duration::from_secs(5)));
    assert_eq!(config.response_timeout, Some(Duration::from_secs(7)));
    assert_eq!(config.recv_body_timeout, Some(Duration::from_secs(9)));
    assert_eq!(http.read_timeout(), Some(Duration::from_secs(11)));
    assert_eq!(http.total_timeout(), Some(Duration::from_secs(13)));
    assert_eq!(http.retries(), 5);

    let off = ModelHttp::new(ModelHttpOptions {
        connect_timeout: None,
        header_timeout: None,
        read_timeout: None,
        ..Default::default()
    });
    assert_eq!(off.client().config().connect_timeout, None);
    assert_eq!(off.client().config().response_timeout, None);
    assert_eq!(off.read_timeout(), None);
}

fn unavailable() -> (u16, Arc<AtomicUsize>) {
    fixed(reply("503 Service Unavailable", &[], b""))
}

#[test]
fn zero_retries_makes_one_attempt() {
    let (port, hits) = unavailable();
    let client = ModelHttp::new(ModelHttpOptions {
        retries: 0,
        ..options()
    });
    let tmp = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let err = fetch(&client, &format!("http://127.0.0.1:{port}/f"), tmp.path()).unwrap_err();
    assert!(err.message.contains("503"), "{err}");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    // No pause after the only attempt.
    assert!(started.elapsed() < Duration::from_millis(240));
}

#[test]
fn the_retry_count_sets_the_number_of_attempts() {
    for (retries, attempts) in [(1u32, 2usize), (2, 3), (3, 4)] {
        let (port, hits) = unavailable();
        let client = ModelHttp::new(ModelHttpOptions {
            retries,
            ..options()
        });
        let tmp = tempfile::tempdir().unwrap();
        let err = fetch(&client, &format!("http://127.0.0.1:{port}/f"), tmp.path()).unwrap_err();
        assert!(err.message.contains("503"), "{err}");
        assert_eq!(hits.load(Ordering::SeqCst), attempts, "retries {retries}");
    }
}

#[test]
fn the_default_is_three_attempts() {
    let (port, hits) = unavailable();
    let client = ModelHttp::new(options());
    let tmp = tempfile::tempdir().unwrap();
    fetch(&client, &format!("http://127.0.0.1:{port}/f"), tmp.path()).unwrap_err();
    assert_eq!(hits.load(Ordering::SeqCst), 3);
}

#[test]
fn a_retry_recovers_from_a_transient_status() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let (port, _) = serve(move |_, mut stream| {
        let response = if seen.fetch_add(1, Ordering::SeqCst) == 0 {
            reply("503 Service Unavailable", &[], b"")
        } else {
            reply("200 OK", &[], b"recovered")
        };
        let _ = stream.write_all(&response);
    });
    let client = ModelHttp::new(ModelHttpOptions {
        retries: 1,
        ..options()
    });
    let tmp = tempfile::tempdir().unwrap();
    fetch(&client, &format!("http://127.0.0.1:{port}/f"), tmp.path()).unwrap();
    assert_eq!(fs::read(tmp.path().join("f.bin")).unwrap(), b"recovered");
}

#[test]
fn a_slow_header_wait_is_cut_by_the_header_timeout() {
    let (port, _) = serve(|_, mut stream| {
        std::thread::sleep(Duration::from_secs(3));
        let _ = stream.write_all(&reply("200 OK", &[], b"late"));
    });
    let client = ModelHttp::new(ModelHttpOptions {
        header_timeout: Some(Duration::from_millis(300)),
        retries: 0,
        ..options()
    });
    let tmp = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let err = fetch(&client, &format!("http://127.0.0.1:{port}/f"), tmp.path()).unwrap_err();
    assert!(err.message.contains("timeout"), "{err}");
    assert!(
        started.elapsed() < Duration::from_millis(2500),
        "{:?}",
        started.elapsed()
    );
}

// ---- one total budget -----------------------------------------------------------------

#[test]
fn a_total_budget_ends_a_slow_download_and_stops_retrying() {
    // 10 chunks, 300 ms apart: 3 s of body against a 600 ms budget. Bytes keep arriving, so
    // the idle limit never fires; only the budget can end it.
    let (port, hits) = drip(b"ab", Duration::from_millis(300), 10);
    let client = ModelHttp::new(ModelHttpOptions {
        total_timeout: Some(Duration::from_millis(600)),
        ..options()
    });
    let tmp = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let err = fetch(&client, &format!("http://127.0.0.1:{port}/f"), tmp.path()).unwrap_err();
    let elapsed = started.elapsed();
    assert!(err.message.contains("total time budget"), "{err}");
    assert!(
        elapsed >= Duration::from_millis(500) && elapsed < Duration::from_millis(2500),
        "{elapsed:?}"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1, "no retry after the budget");
    // The partial file is kept for a later resume; the destination was never written.
    assert!(!tmp.path().join("f.bin").exists());
}

#[test]
fn a_download_inside_its_total_budget_completes() {
    let (port, _) = drip(b"ab", Duration::from_millis(100), 6);
    let client = ModelHttp::new(ModelHttpOptions {
        total_timeout: Some(Duration::from_secs(30)),
        ..options()
    });
    let tmp = tempfile::tempdir().unwrap();
    fetch(&client, &format!("http://127.0.0.1:{port}/f"), tmp.path()).unwrap();
    assert_eq!(fs::read(tmp.path().join("f.bin")).unwrap().len(), 12);
}

#[test]
fn the_total_budget_also_bounds_the_pauses_between_attempts() {
    let (port, hits) = unavailable();
    // Three attempts would pause 250 ms and 500 ms; the budget leaves room for one pause.
    let client = ModelHttp::new(ModelHttpOptions {
        total_timeout: Some(Duration::from_millis(400)),
        retries: 10,
        ..options()
    });
    let tmp = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let err = fetch(&client, &format!("http://127.0.0.1:{port}/f"), tmp.path()).unwrap_err();
    assert!(err.message.contains("total time budget"), "{err}");
    assert!(started.elapsed() < Duration::from_millis(2000));
    assert!(hits.load(Ordering::SeqCst) <= 3);
}

// ---- the loopback allowance -----------------------------------------------------------

#[test]
fn a_bad_loopback_allowance_is_refused_by_validation() {
    for (host, port) in [
        ("", 8787u16),
        ("a/b", 8787),
        ("host name", 8787),
        ("user@127.0.0.1", 8787),
        ("127.0.0.1:8787", 8787),
        ("127.0.0.1", 0),
    ] {
        let opts = ModelHttpOptions::default().allow_loopback_host(host, port);
        assert!(opts.validate().is_err(), "{host:?}:{port}");
        assert!(ModelHttp::try_new(opts.clone()).is_err(), "{host:?}:{port}");
        // `new` does not fail: the allowance is dropped.
        assert_eq!(ModelHttp::new(opts).dev_loopback_host(), None);
    }
    for host in ["127.0.0.1", "localhost", "[::1]", "::1", "LocalHost."] {
        let opts = ModelHttpOptions::default().allow_loopback_host(host, 8787);
        assert!(opts.validate().is_ok(), "{host:?}");
    }
}

#[test]
fn loopback_is_refused_when_no_allowance_is_configured() {
    let (port, hits) = fixed(reply("200 OK", &[], b"x"));
    let client = ModelHttp::new(ModelHttpOptions {
        proxy: ProxyMode::None,
        ..Default::default()
    });
    let tmp = tempfile::tempdir().unwrap();
    let err = fetch(&client, &format!("http://127.0.0.1:{port}/f"), tmp.path()).unwrap_err();
    assert!(err.message.contains("address denied"), "{err}");
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[cfg(not(feature = "dev-worker-override"))]
mod release_build {
    use super::*;

    #[test]
    fn the_allowance_is_accepted_and_ignored_without_the_feature() {
        assert!(!DEV_WORKER_OVERRIDE_ENABLED);
        let (port, hits) = fixed(reply("200 OK", &[], b"x"));
        let client = ModelHttp::new(ModelHttpOptions {
            proxy: ProxyMode::None,
            ..Default::default()
        });
        assert_eq!(client.dev_loopback_host(), None);
        let options = ModelHttpOptions {
            proxy: ProxyMode::None,
            ..Default::default()
        }
        .allow_loopback_host("127.0.0.1", port);
        // The option is carried, but the transport does not honour it.
        assert!(options.loopback_host.is_some());
        let client = ModelHttp::new(options);
        assert_eq!(client.dev_loopback_host(), None);
        let url = format!("http://127.0.0.1:{port}/f");
        assert!(std::ptr::eq(client.client_for(&url), client.client()));
        let tmp = tempfile::tempdir().unwrap();
        let err = fetch(&client, &url, tmp.path()).unwrap_err();
        assert!(err.message.contains("address denied"), "{err}");
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }
}

#[cfg(feature = "dev-worker-override")]
mod dev_worker_override {
    use super::*;
    use rightkit_models::suite::grant::{acquire_grant_with_client, ModelGrantRequest};
    use serde_json::json;
    use std::net::Ipv4Addr;

    /// Default address policy (loopback refused) plus the allowance for `127.0.0.1:port`.
    fn dev_client(port: u16) -> ModelHttp {
        ModelHttp::new(
            ModelHttpOptions {
                proxy: ProxyMode::None,
                ..Default::default()
            }
            .allow_loopback_host("127.0.0.1", port),
        )
    }

    #[test]
    fn the_feature_is_reported_on() {
        assert!(DEV_WORKER_OVERRIDE_ENABLED);
    }

    #[test]
    fn the_named_host_and_port_can_be_downloaded_from() {
        let (port, hits) = fixed(reply("200 OK", &[], b"from the debug worker"));
        let client = dev_client(port);
        assert_eq!(client.dev_loopback_host(), Some(("127.0.0.1", port)));
        let tmp = tempfile::tempdir().unwrap();
        fetch(&client, &format!("http://127.0.0.1:{port}/f"), tmp.path()).unwrap();
        assert_eq!(
            fs::read(tmp.path().join("f.bin")).unwrap(),
            b"from the debug worker"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn another_port_on_the_same_host_is_still_refused() {
        let (allowed, _) = fixed(reply("200 OK", &[], b"x"));
        let (other, other_hits) = fixed(reply("200 OK", &[], b"y"));
        let client = dev_client(allowed);
        let tmp = tempfile::tempdir().unwrap();
        let err = fetch(&client, &format!("http://127.0.0.1:{other}/f"), tmp.path()).unwrap_err();
        assert!(err.message.contains("address denied"), "{err}");
        assert_eq!(other_hits.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn another_loopback_host_is_still_refused() {
        // `localhost` is loopback too, but only the named host is allowed.
        let (port, hits) = fixed(reply("200 OK", &[], b"x"));
        let client = dev_client(port);
        let tmp = tempfile::tempdir().unwrap();
        let err = fetch(&client, &format!("http://localhost:{port}/f"), tmp.path()).unwrap_err();
        assert!(err.message.contains("address denied"), "{err}");
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn client_for_picks_the_loopback_client_for_exactly_that_host_and_port() {
        let client = dev_client(8787);
        let normal = client.client();
        assert!(!std::ptr::eq(
            client.client_for("http://127.0.0.1:8787/v1/x"),
            normal
        ));
        assert!(!std::ptr::eq(
            client.client_for("HTTP://127.0.0.1:8787"),
            normal
        ));
        for other in [
            "http://127.0.0.1:8788/v1/x",
            "http://127.0.0.1/v1/x",
            "http://localhost:8787/v1/x",
            "https://models.example.com/v1/x",
            "http://user@127.0.0.1:8787/v1/x",
            "ftp://127.0.0.1:8787/x",
        ] {
            assert!(std::ptr::eq(client.client_for(other), normal), "{other}");
        }
    }

    #[test]
    fn a_redirect_answered_by_the_allowed_host_is_not_followed() {
        let (target, target_hits) = fixed(reply("200 OK", &[], b"redirect target"));
        let location = format!("http://127.0.0.1:{target}/f");
        let (port, _) = fixed(reply("302 Found", &[("Location", location.as_str())], b""));
        let client = dev_client(port);
        let tmp = tempfile::tempdir().unwrap();
        let err = fetch(&client, &format!("http://127.0.0.1:{port}/f"), tmp.path()).unwrap_err();
        assert!(err.message.contains("302"), "{err}");
        assert_eq!(target_hits.load(Ordering::SeqCst), 0);
        assert!(!tmp.path().join("f.bin").exists());
    }

    #[test]
    fn a_redirect_from_another_server_to_the_allowed_host_is_refused() {
        // `origin.example` is a stand-in public-looking name that this test's resolver points
        // at 127.0.0.1. Its redirect to the allowed 127.0.0.1 host and port must still be
        // refused: the allowance never applies to a redirect hop.
        let (allowed, allowed_hits) = fixed(reply("200 OK", &[], b"must not be reached"));
        let location = format!("http://127.0.0.1:{allowed}/f");
        let (origin, _) = fixed(reply("302 Found", &[("Location", location.as_str())], b""));
        let policy = AddressPolicy::public_web()
            .with_resolver(|host| {
                if host == "origin.example" {
                    vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]
                } else {
                    Vec::new()
                }
            })
            .allow_if(vec!["origin.example".to_string()], |ip: &IpAddr| {
                ip.is_loopback()
            });
        let client = ModelHttp::new(
            ModelHttpOptions {
                proxy: ProxyMode::None,
                address_policy: policy,
                max_redirects: 10,
                ..Default::default()
            }
            .allow_loopback_host("127.0.0.1", allowed),
        );
        let tmp = tempfile::tempdir().unwrap();
        let err = fetch(
            &client,
            &format!("http://origin.example:{origin}/f"),
            tmp.path(),
        )
        .unwrap_err();
        assert!(err.message.contains("address denied"), "{err}");
        assert_eq!(allowed_hits.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn grant_requests_to_the_allowed_worker_use_the_loopback_client() {
        let expires = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600;
        let sha = "ab".repeat(32);
        let body = json!({
            "token": "grant-token",
            "expiresAt": expires,
            "route": "/v1/models/demo.tar.gz",
            "sha256": sha,
        })
        .to_string();
        let (port, hits) = serve(move |request, mut stream| {
            assert!(request.line.starts_with("POST /v1/model-grants "));
            assert!(!request.body.is_empty());
            let _ = stream.write_all(&reply(
                "200 OK",
                &[("Content-Type", "application/json")],
                body.as_bytes(),
            ));
        });
        let client = dev_client(port);
        let base = format!("http://127.0.0.1:{port}");
        let grant = acquire_grant_with_client(
            client.client_for(&base),
            ModelGrantRequest {
                worker_base: &base,
                route: "/v1/models/demo.tar.gz",
                model: "demo",
                platform: "macos",
                sha256: &sha,
                device_id: "0123456789abcdef0123",
            },
        )
        .unwrap();
        assert_eq!(grant.token, "grant-token");
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        // The normal client cannot reach the same worker.
        let refused = acquire_grant_with_client(
            client.client(),
            ModelGrantRequest {
                worker_base: &base,
                route: "/v1/models/demo.tar.gz",
                model: "demo",
                platform: "macos",
                sha256: &sha,
                device_id: "0123456789abcdef0123",
            },
        )
        .unwrap_err();
        assert!(refused.contains("address denied"), "{refused}");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}
