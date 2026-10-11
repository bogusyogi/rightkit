//! Model downloads and grants go through rightkit-http's address policy: a
//! manifest or a redirect cannot steer a request at loopback, a private network
//! or a metadata address, and the vetted path keeps resume, SHA-256 verification,
//! progress and cancellation.

use rightkit_http::AddressPolicy;
use rightkit_models::download::{download_resumable, DownloadRequest};
use rightkit_models::http::{
    http_client, http_client_with_options, ModelHttp, ModelHttpOptions, ProxyMode,
};
use rightkit_models::suite::grant::{
    acquire_grant_pinned, acquire_grant_with_client, grant_policy, ModelGrantRequest,
};
use rightkit_models::{ErrorCode, ModelSpec, RevisionSpec, Store};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn payload(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u32).wrapping_mul(2_654_435_761).rotate_left(9) as u8)
        .collect()
}

/// The test client may reach exactly 127.0.0.1, like a `Store` configured with
/// `with_address_policy`. Everything else keeps the public-web rules.
fn loopback_policy() -> AddressPolicy {
    AddressPolicy::public_web().allow_if(vec!["127.0.0.1".to_string()], |ip: &IpAddr| {
        ip.is_loopback()
    })
}

fn loopback_client() -> ModelHttp {
    http_client_with_options(ModelHttpOptions {
        proxy: ProxyMode::None,
        address_policy: loopback_policy(),
        ..Default::default()
    })
}

#[derive(Clone, Debug)]
struct Seen {
    method: String,
    path: String,
    headers: HashMap<String, String>,
}

struct Server {
    base: String,
    port: u16,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Server {
    fn paths(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.path.clone())
            .collect()
    }
    fn requests_for(&self, path: &str) -> Vec<Seen> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.path == path)
            .cloned()
            .collect()
    }
}

/// One request per connection; `handler` returns the raw response bytes.
fn serve(handler: impl Fn(&Seen, &[u8]) -> Vec<u8> + Send + Sync + 'static) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let handler = Arc::new(handler);
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (handler, log) = (handler.clone(), log.clone());
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                let mut parts = line.split_whitespace();
                let method = parts.next().unwrap_or("").to_string();
                let path = parts.next().unwrap_or("/").to_string();
                let mut headers = HashMap::new();
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':') {
                        headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                    }
                }
                let length = headers
                    .get("content-length")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(0);
                let mut body = vec![0u8; length];
                let _ = reader.read_exact(&mut body);
                let request = Seen {
                    method,
                    path,
                    headers,
                };
                log.lock().unwrap().push(request.clone());
                let mut out = stream;
                let _ = out.write_all(&handler(&request, &body));
            });
        }
    });
    Server {
        base: format!("http://127.0.0.1:{port}"),
        port,
        seen,
    }
}

fn redirect(location: &str) -> Vec<u8> {
    format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        .into_bytes()
}

/// Strong-ETag server for `/m.bin` with `Range` support.
fn model_server(body: Vec<u8>) -> Server {
    let etag = format!("\"{}\"", &sha(&body)[..16]);
    serve(move |req, _| {
        if req.path != "/m.bin" {
            return b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_vec();
        }
        let total = body.len();
        let start = req
            .headers
            .get("range")
            .and_then(|r| r.strip_prefix("bytes="))
            .and_then(|r| r.trim_end_matches('-').parse::<usize>().ok())
            .filter(|s| *s < total);
        let mut out = match start {
            Some(start) => format!(
                "HTTP/1.1 206 Partial Content\r\nETag: {etag}\r\nContent-Range: bytes {start}-{}/{total}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                total - 1,
                total - start
            )
            .into_bytes(),
            None => format!(
                "HTTP/1.1 200 OK\r\nETag: {etag}\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n"
            )
            .into_bytes(),
        };
        out.extend_from_slice(&body[start.unwrap_or(0)..]);
        out
    })
}

#[test]
fn download_resumes_with_range_and_verifies_the_sha256_over_the_vetted_client() {
    let body = payload(1_000_000);
    let server = model_server(body.clone());
    let url = format!("{}/m.bin", server.base);
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("m.bin");
    let part = rightkit_models::download::part_path_for(&dest);
    let pin = sha(&body);
    let client = loopback_client();

    // First attempt: cancel after ~200 kB. The cancel keeps the .part.
    let stop = AtomicBool::new(false);
    let progress_stop = &stop;
    let err = download_resumable(
        &client,
        DownloadRequest {
            url: &url,
            part_path: &part,
            dest_path: &dest,
            expected_sha256: Some(&pin),
            authorization: Some("Bearer secret-token"),
            cancelled: &|| stop.load(Ordering::SeqCst),
        },
        |received, _| {
            if received >= 200_000 {
                progress_stop.store(true, Ordering::SeqCst);
            }
        },
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::Cancelled, "{err}");
    let kept = fs::metadata(&part).unwrap().len();
    assert!((200_000..1_000_000).contains(&kept), "kept {kept}");
    assert!(!dest.exists());

    // Second attempt resumes from `kept` and the file verifies.
    let mut last = (0, 0);
    let result = download_resumable(
        &client,
        DownloadRequest {
            url: &url,
            part_path: &part,
            dest_path: &dest,
            expected_sha256: Some(&pin),
            authorization: Some("Bearer secret-token"),
            cancelled: &|| false,
        },
        |received, total| last = (received, total),
    )
    .unwrap();
    assert_eq!(result.sha256, pin);
    assert_eq!(last, (1_000_000, 1_000_000), "progress reaches the total");
    assert_eq!(fs::read(&dest).unwrap(), body);
    assert!(!part.exists());

    let gets = server.requests_for("/m.bin");
    assert_eq!(gets.len(), 2, "{gets:?}");
    assert_eq!(gets[0].headers.get("range"), None);
    assert_eq!(
        gets[1].headers.get("range").map(String::as_str),
        Some(format!("bytes={kept}-").as_str())
    );
    assert_eq!(
        gets[1].headers.get("if-range").map(String::as_str),
        Some(format!("\"{}\"", &pin[..16]).as_str())
    );
    assert_eq!(
        gets[1].headers.get("accept-encoding").map(String::as_str),
        Some("identity")
    );
    assert_eq!(
        gets[1].headers.get("authorization").map(String::as_str),
        Some("Bearer secret-token")
    );
}

#[test]
fn download_with_a_wrong_pin_fails_integrity_and_leaves_no_file() {
    let body = payload(50_000);
    let server = model_server(body);
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("m.bin");
    let part = rightkit_models::download::part_path_for(&dest);
    let wrong = sha(b"something else");
    let err = download_resumable(
        &loopback_client(),
        DownloadRequest {
            url: &format!("{}/m.bin", server.base),
            part_path: &part,
            dest_path: &dest,
            expected_sha256: Some(&wrong),
            authorization: None,
            cancelled: &|| false,
        },
        |_, _| {},
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::HashMismatch, "{err}");
    assert!(!dest.exists() && !part.exists());
    // Nothing is left in the destination directory: no `*.part`, no `*.part.meta`
    // resume record, and no other temporary file.
    let leftovers: Vec<String> = fs::read_dir(tmp.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        leftovers.is_empty(),
        "failed download left files behind: {leftovers:?}"
    );
}

#[test]
fn redirect_to_a_private_or_metadata_address_is_refused_and_never_followed() {
    let tmp = tempfile::tempdir().unwrap();
    for target in [
        "http://10.255.255.1/secret".to_string(),
        "http://169.254.169.254/latest/meta-data".to_string(),
        // Loopback under another name than the one the policy allows.
        "SAME_SERVER_LOCALHOST".to_string(),
    ] {
        let target_for_handler = target.clone();
        let server = serve(move |req, _| {
            if req.path == "/m.bin" {
                redirect(&target_for_handler)
            } else {
                b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecret".to_vec()
            }
        });
        // The server needs its own port in the target, so it is rebuilt here.
        let target = if target == "SAME_SERVER_LOCALHOST" {
            let port = server.port;
            let fixed = format!("http://localhost:{port}/secret");
            let server = serve(move |req, _| {
                if req.path == "/m.bin" {
                    redirect(&fixed)
                } else {
                    b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecret"
                        .to_vec()
                }
            });
            let dest = tmp.path().join("l.bin");
            let err = download_resumable(
                &loopback_client(),
                DownloadRequest {
                    url: &format!("{}/m.bin", server.base),
                    part_path: &rightkit_models::download::part_path_for(&dest),
                    dest_path: &dest,
                    expected_sha256: None,
                    authorization: Some("Bearer secret-token"),
                    cancelled: &|| false,
                },
                |_, _| {},
            )
            .unwrap_err();
            assert_eq!(err.code, ErrorCode::Download, "{err}");
            assert!(err.message.contains("address denied"), "{err}");
            assert_eq!(server.paths(), vec!["/m.bin"], "the redirect was followed");
            assert!(!dest.exists());
            continue;
        } else {
            target
        };
        let dest = tmp.path().join("m.bin");
        let err = download_resumable(
            &loopback_client(),
            DownloadRequest {
                url: &format!("{}/m.bin", server.base),
                part_path: &rightkit_models::download::part_path_for(&dest),
                dest_path: &dest,
                expected_sha256: None,
                authorization: Some("Bearer secret-token"),
                cancelled: &|| false,
            },
            |_, _| {},
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::Download, "{target}: {err}");
        assert!(err.message.contains("address denied"), "{target}: {err}");
        assert!(
            !err.message.contains("secret-token"),
            "bearer leaked: {err}"
        );
        assert_eq!(server.paths(), vec!["/m.bin"]);
        assert!(!dest.exists());
    }
}

#[test]
fn default_clients_refuse_a_loopback_origin_without_connecting() {
    let body = payload(10_000);
    let server = model_server(body);
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("m.bin");
    let err = download_resumable(
        &http_client(),
        DownloadRequest {
            url: &format!("{}/m.bin", server.base),
            part_path: &rightkit_models::download::part_path_for(&dest),
            dest_path: &dest,
            expected_sha256: None,
            authorization: None,
            cancelled: &|| false,
        },
        |_, _| {},
    )
    .unwrap_err();
    assert!(err.message.contains("address denied"), "{err}");
    assert!(
        !rightkit_models::download::is_connectivity_error(&err.message),
        "a policy denial is not an offline error"
    );
    assert!(server.paths().is_empty(), "the origin was contacted");

    // The same holds through a Store that was not configured for local servers.
    let spec = ModelSpec::new(
        "demo",
        vec![RevisionSpec {
            revision: "1".into(),
            files: vec![rightkit_models::FileSpec {
                path: "m.bin".into(),
                url: format!("{}/m.bin", server.base),
                ..Default::default()
            }],
            ..Default::default()
        }],
    );
    let store = Store::new(tmp.path().join("models"), "t");
    let err = store
        .install(&spec, &Default::default(), |_| {})
        .unwrap_err();
    assert!(err.message.contains("address denied"), "{err}");
    assert!(server.paths().is_empty());
}

fn grant_request<'a>(base: &'a str, sha: &'a str) -> ModelGrantRequest<'a> {
    ModelGrantRequest {
        worker_base: base,
        route: "/m/model.tar.gz",
        model: "demo",
        platform: "macos",
        sha256: sha,
        device_id: "device-0123456789abcdef",
    }
}

fn grant_json(route: &str, sha: &str, expires_at: u64) -> Vec<u8> {
    let body = format!(
        "{{\"token\":\"tok1\",\"expiresAt\":{expires_at},\"route\":\"{route}\",\"sha256\":\"{sha}\"}}"
    );
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[test]
fn grant_policy_pins_the_worker_host_and_refuses_everything_else() {
    // Name-level refusals need no network.
    let policy = grant_policy("https://worker.example/").unwrap();
    for other in ["evil.example", "worker.example.evil.example", "127.0.0.1"] {
        let denied = policy.resolve_vetted(other, 443).unwrap_err();
        assert!(
            matches!(denied, rightkit_http::HttpError::AddressDenied { .. }),
            "{other}: {denied:?}"
        );
    }
    // The pinned host itself is still held to the public-address rule.
    let private = policy
        .clone()
        .with_resolver(|_| vec!["10.1.2.3".parse().unwrap()]);
    assert!(matches!(
        private.resolve_vetted("worker.example", 443).unwrap_err(),
        rightkit_http::HttpError::AddressDenied { .. }
    ));
    let public = policy.with_resolver(|_| vec!["93.184.216.34".parse().unwrap()]);
    assert_eq!(
        public.resolve_vetted("worker.example", 443).unwrap().len(),
        1
    );
    // Host extraction ignores port, path and case.
    assert!(grant_policy("HTTPS://Worker.Example:8443/base/").is_ok());
}

#[test]
fn grant_endpoint_must_be_https_without_credentials() {
    for bad in [
        "http://worker.example",
        "ftp://worker.example",
        "worker.example",
        "https://",
        "https://user:pw@worker.example",
    ] {
        assert!(grant_policy(bad).is_err(), "{bad}");
        let err = acquire_grant_pinned(grant_request(bad, &sha(b"x"))).unwrap_err();
        assert!(err.contains("model grant endpoint"), "{bad}: {err}");
    }
}

#[test]
fn grant_request_through_a_client_verifies_scope_and_never_follows_redirects() {
    let pin = sha(b"archive");
    let (route_pin, expiry) = (pin.clone(), now() + 3600);
    let server = serve(move |req, body| match req.path.as_str() {
        "/v1/model-grants" => {
            assert_eq!(req.method, "POST");
            let text = String::from_utf8_lossy(body);
            assert!(
                text.contains("\"deviceId\":\"device-0123456789abcdef\""),
                "{text}"
            );
            grant_json("/m/model.tar.gz", &route_pin, expiry)
        }
        _ => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
    });
    let grant = acquire_grant_with_client(
        loopback_client().client(),
        grant_request(&server.base, &pin),
    )
    .unwrap();
    assert_eq!(grant.token, "tok1");
    assert_eq!(grant.expires_at, expiry);

    // A redirecting worker is an error, and the redirect target is not contacted.
    let hits = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = hits.clone();
    let redirecting = serve(move |req, _| {
        seen.lock().unwrap().push(req.path.clone());
        redirect("http://127.0.0.1:1/elsewhere")
    });
    let err = acquire_grant_with_client(
        loopback_client().client(),
        grant_request(&redirecting.base, &pin),
    )
    .unwrap_err();
    assert!(err.starts_with("model grant request failed"), "{err}");
    assert_eq!(*hits.lock().unwrap(), vec!["/v1/model-grants".to_string()]);

    // The pinned client cannot reach a loopback worker at all.
    let err = acquire_grant_pinned(grant_request(&server.base, &pin)).unwrap_err();
    assert!(err.contains("https"), "{err}");
}

/// An HTTP proxy that is also the origin: it answers `CONNECT host:port` with 200 and then
/// answers the tunnelled request itself, and answers an absolute-form request directly.
fn serve_proxy(body: &'static str) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let log = log.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut out = stream;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let mut parts = line.split_whitespace();
                    let method = parts.next().unwrap_or("").to_string();
                    let path = parts.next().unwrap_or("/").to_string();
                    let mut headers = HashMap::new();
                    loop {
                        let mut h = String::new();
                        if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                            break;
                        }
                        if let Some((k, v)) = h.split_once(':') {
                            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                        }
                    }
                    log.lock().unwrap().push(Seen {
                        method: method.clone(),
                        path,
                        headers,
                    });
                    if method == "CONNECT" {
                        let _ = out.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n");
                        continue;
                    }
                    let _ = out.write_all(&ok_body(body));
                    return;
                }
            });
        }
    });
    Server {
        base: format!("http://127.0.0.1:{port}"),
        port,
        seen,
    }
}

fn ok_body(body: &'static str) -> Vec<u8> {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
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

#[test]
fn defaults_use_environment_proxies_and_an_identifying_user_agent() {
    let options = ModelHttpOptions::default();
    assert!(matches!(options.proxy, ProxyMode::Environment));
    assert_eq!(options.body_timeout, None, "no total body deadline");
    assert!(
        options.read_timeout.is_some(),
        "stalls are caught by the idle limit"
    );
    let own = format!("rightkit-models/{}", env!("CARGO_PKG_VERSION"));
    assert_eq!(options.resolved_user_agent(), own);
    let app = ModelHttpOptions {
        app: Some(("ScrapeRight".into(), "1.4.2".into())),
        ..Default::default()
    };
    assert_eq!(
        app.resolved_user_agent(),
        format!("ScrapeRight/1.4.2 {own}")
    );
    let explicit = ModelHttpOptions {
        user_agent: Some("Custom/9".into()),
        app: Some(("ignored".into(), "0".into())),
        ..Default::default()
    };
    assert_eq!(explicit.resolved_user_agent(), "Custom/9");
}

#[test]
fn user_agent_option_reaches_the_wire() {
    let server = serve(|_, _| ok_body("hello"));
    let tmp = tempfile::tempdir().unwrap();
    for (options_ua, app, expected) in [
        (
            None,
            None,
            format!("rightkit-models/{}", env!("CARGO_PKG_VERSION")),
        ),
        (
            None,
            Some(("HeardRight".to_string(), "2.0.1".to_string())),
            format!(
                "HeardRight/2.0.1 rightkit-models/{}",
                env!("CARGO_PKG_VERSION")
            ),
        ),
        (
            Some("Explicit/3".to_string()),
            None,
            "Explicit/3".to_string(),
        ),
    ] {
        let client = http_client_with_options(ModelHttpOptions {
            proxy: ProxyMode::None,
            user_agent: options_ua,
            app,
            address_policy: loopback_policy(),
            ..Default::default()
        });
        fetch(&client, &format!("{}/ua", server.base), tmp.path()).unwrap();
        let seen = server.requests_for("/ua");
        assert_eq!(
            seen.last().unwrap().headers.get("user-agent"),
            Some(&expected)
        );
    }
}

#[test]
fn proxy_option_is_passed_through_and_the_destination_is_still_vetted() {
    // The "proxy" answers for any origin. The destination name is public, so the
    // address policy admits it without a lookup; the connection goes to the proxy.
    let proxy = serve_proxy("via-proxy");
    let client = http_client_with_options(ModelHttpOptions {
        proxy: ProxyMode::Explicit(proxy.base.clone()),
        ..Default::default()
    });
    let tmp = tempfile::tempdir().unwrap();
    fetch(&client, "http://models.example.com/m.bin", tmp.path()).unwrap();
    assert_eq!(fs::read(tmp.path().join("f.bin")).unwrap(), b"via-proxy");
    let seen = proxy.seen.lock().unwrap().clone();
    // The destination reached the proxy by name (CONNECT or absolute form), not resolved here.
    assert!(
        seen.iter().any(|r| r.path.contains("models.example.com")),
        "{seen:?}"
    );

    // A literal private destination is refused even though a proxy is configured.
    let before = proxy.paths().len();
    let err = fetch(&client, "http://10.0.0.5/m.bin", tmp.path()).unwrap_err();
    assert!(err.message.contains("address denied"), "{err}");
    assert_eq!(
        proxy.paths().len(),
        before,
        "the proxy was asked to reach a private host"
    );
}

#[test]
fn a_slow_body_is_not_cut_by_a_total_deadline_but_a_stalled_one_is_cut_by_the_idle_limit() {
    // Drip: 6 chunks, 300 ms apart (1.8 s total) against a 700 ms idle limit.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                        break;
                    }
                }
                let stall = line.contains("/stall");
                let mut out = stream;
                let _ = out.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\nConnection: close\r\n\r\n",
                );
                for _ in 0..6 {
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    if out.write_all(b"ab").is_err() {
                        return;
                    }
                    let _ = out.flush();
                    if stall {
                        // Send one chunk, then go quiet past the idle limit.
                        std::thread::sleep(std::time::Duration::from_secs(5));
                        return;
                    }
                }
            });
        }
    });
    let client = http_client_with_options(ModelHttpOptions {
        proxy: ProxyMode::None,
        address_policy: loopback_policy(),
        read_timeout: Some(std::time::Duration::from_millis(700)),
        body_timeout: None,
        ..Default::default()
    });
    let tmp = tempfile::tempdir().unwrap();
    let started = std::time::Instant::now();
    fetch(
        &client,
        &format!("http://127.0.0.1:{port}/drip"),
        tmp.path(),
    )
    .unwrap();
    assert!(started.elapsed() > std::time::Duration::from_millis(1500));
    assert_eq!(fs::read(tmp.path().join("f.bin")).unwrap().len(), 12);

    let started = std::time::Instant::now();
    let err = fetch(
        &client,
        &format!("http://127.0.0.1:{port}/stall"),
        tmp.path(),
    )
    .unwrap_err();
    // What the implementation guarantees: the download loop makes 3 attempts (MAX_ATTEMPTS),
    // sleeping 250 ms then 500 ms between them. Each attempt gets its first chunk after 300 ms
    // (the server's pace), then waits one idle limit (700 ms) for the next and fails. So the
    // stalled fetch ends after 3 x (300 + 700) ms + 750 ms = 3.75 s, never hanging for the
    // server's 5 s stall, plus scheduling and thread start-up, allowed 1.5 s.
    let elapsed = started.elapsed();
    assert!(
        elapsed >= std::time::Duration::from_millis(3 * 700),
        "idle limit not waited: {elapsed:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_millis(3750 + 1500),
        "{elapsed:?}"
    );
    assert!(err.message.contains("no data"), "{err}");
}
