//! Regression tests for the adversarial read of 0.2.6 (docs/audits/2026-10-11-astra-rightkit-http-0.2.6.md).
//! Each test names the finding it reproduces.

mod common;

use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::TestProxy;
use rightkit_http::testing::{
    loopback_config, loopback_policy, MockBody, MockResponse, MockServer,
};
use rightkit_http::*;

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

fn cfg() -> ClientConfig {
    ClientConfig {
        retry: RetryPolicy::none(),
        request_timeout: Duration::from_secs(5),
        response_timeout: Some(Duration::from_secs(5)),
        ..loopback_config()
    }
}

fn denied_class(err: &HttpError) -> Option<AddressClass> {
    match err {
        HttpError::AddressDenied { class, .. } => *class,
        other => panic!("expected AddressDenied, got {other:?}"),
    }
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("audit-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------- finding 1
#[test]
fn f1_legacy_ipv4_spellings_are_canonicalised_direct() {
    let c = Client::new(ClientConfig {
        retry: RetryPolicy::none(),
        ..ClientConfig::default()
    });
    for (url, class) in [
        ("http://127.1/", AddressClass::Loopback),
        ("http://0177.0.0.1/", AddressClass::Loopback),
        ("http://0x7f.0.0.1/", AddressClass::Loopback),
        ("http://2130706433/", AddressClass::Loopback),
        ("http://017700000001/", AddressClass::Loopback),
        ("http://0x7f000001/", AddressClass::Loopback),
        ("http://0251.0376.0251.0376/", AddressClass::LinkLocal),
        ("http://0xa9.0xfe.0xa9.0xfe/", AddressClass::LinkLocal),
        ("http://10.1/", AddressClass::Private),
        ("http://0/", AddressClass::Unspecified),
    ] {
        let err = c.send(&Request::get(url)).unwrap_err();
        assert_eq!(denied_class(&err), Some(class), "{url}");
    }
}

#[test]
fn f1_legacy_ipv4_spellings_are_denied_through_a_proxy_and_on_redirects() {
    let proxy = TestProxy::start();
    let c = Client::new(ClientConfig {
        proxy: ProxyMode::Explicit(proxy.url()),
        retry: RetryPolicy::none(),
        max_redirects: 3,
        ..ClientConfig::default()
    });
    for url in [
        "http://127.1/",
        "http://0177.0.0.1/",
        "http://0x7f.0.0.1/",
        "http://2130706433/",
        "http://0251.0376.0251.0376/latest/meta-data/",
    ] {
        let err = c.send(&Request::get(url)).unwrap_err();
        assert!(
            matches!(err, HttpError::AddressDenied { .. }),
            "{url}: {err:?}"
        );
    }
    assert_eq!(proxy.total(), 0);

    // A redirect to a legacy spelling, through the proxy.
    let a = MockServer::start(|_, _| MockResponse::new(302).header("Location", "http://0x7f.1/"));
    let backend = format!("127.0.0.1:{}", a.port()).parse().unwrap();
    let proxy = TestProxy::start_to(Some(backend));
    let c = Client::new(ClientConfig {
        proxy: ProxyMode::Explicit(proxy.url()),
        retry: RetryPolicy::none(),
        max_redirects: 3,
        ..ClientConfig::default()
    });
    let err = c
        .send(&Request::get(format!(
            "http://app.example.com:{}/",
            a.port()
        )))
        .unwrap_err();
    assert!(matches!(err, HttpError::AddressDenied { .. }), "{err:?}");
    assert_eq!(proxy.connects().len(), 1);
}

// ---------------------------------------------------------------- finding 3
#[test]
fn f3_local_use_nat64_embeds_ipv4() {
    use rightkit_http::netpolicy::{classify_ip, embedded_v4s};
    assert_eq!(classify_ip(ip("64:ff9b:1::a00:1")), AddressClass::Private);
    assert_eq!(classify_ip(ip("64:ff9b:1::7f00:1")), AddressClass::Loopback);
    // The /48 layout hides 10.0.0.1 in bytes 6, 7, 9 and 10.
    let v48 = ip("64:ff9b:1:a00:0:100::");
    assert!(embedded_v4s(v48).contains(&"10.0.0.1".parse().unwrap()));
    assert_ne!(classify_ip(v48), AddressClass::Public);
    // A /96 layout of a public address stays public.
    assert!(embedded_v4s(ip("64:ff9b:1::808:808")).contains(&"8.8.8.8".parse().unwrap()));

    let c = Client::new(ClientConfig {
        retry: RetryPolicy::none(),
        ..ClientConfig::default()
    });
    for url in ["http://[64:ff9b:1::a00:1]/", "http://[64:ff9b:1::7f00:1]/"] {
        let err = c.send(&Request::get(url)).unwrap_err();
        assert!(
            matches!(err, HttpError::AddressDenied { .. }),
            "{url}: {err:?}"
        );
    }
    // Hidden metadata address in the /48 layout is a hard block even with a permissive hook.
    let policy =
        AddressPolicy::default().allow_if(vec!["64:ff9b:1:a9fe:a9:fe00::".into()], |_| true);
    assert!(policy
        .resolve_vetted("64:ff9b:1:a9fe:a9:fe00::", 80)
        .is_err());
}

// ---------------------------------------------------------------- finding 4
#[test]
fn f4_metadata_addresses_cannot_be_allowed_by_any_hook() {
    let permissive = |hosts: &[&str]| {
        AddressPolicy::default()
            .with_resolver(|h| match h {
                "aws6.test" => vec![ip("fd00:ec2::254")],
                "aws6dns.test" => vec![ip("fd00:ec2::253")],
                "alibaba.test" => vec![ip("100.100.100.200")],
                "azure.test" => vec![ip("168.63.129.16")],
                "oracle.test" => vec![ip("192.0.0.192")],
                "mapped.test" => vec![ip("::ffff:100.100.100.200")],
                "nat64.test" => vec![ip("64:ff9b::6464:64c8")],
                "link.test" => vec![ip("169.254.169.254")],
                _ => vec![],
            })
            .allow_if(hosts.iter().map(|h| h.to_string()).collect(), |_| true)
    };
    let hosts = [
        "aws6.test",
        "aws6dns.test",
        "alibaba.test",
        "azure.test",
        "oracle.test",
        "mapped.test",
        "nat64.test",
        "link.test",
    ];
    let policy = permissive(&hosts);
    for h in hosts {
        let err = policy.resolve_vetted(h, 80).unwrap_err();
        assert!(
            matches!(err, HttpError::AddressDenied { .. }),
            "{h}: {err:?}"
        );
    }
    // Literals too, and through the client.
    let c = Client::new(ClientConfig {
        address_policy: AddressPolicy::default().allow_if(
            vec!["fd00:ec2::254".into(), "100.100.100.200".into()],
            |_| true,
        ),
        retry: RetryPolicy::none(),
        ..ClientConfig::default()
    });
    for url in [
        "http://[fd00:ec2::254]/latest/",
        "http://100.100.100.200/latest/",
    ] {
        let err = c.send(&Request::get(url)).unwrap_err();
        assert!(
            matches!(err, HttpError::AddressDenied { .. }),
            "{url}: {err:?}"
        );
    }
    // First-party opt-out cannot reach them either.
    let fp = AddressPolicy::first_party(vec!["aws6.test".into()])
        .allow_private_resolution("aws6.test")
        .with_resolver(|_| vec![ip("fd00:ec2::254")]);
    assert!(fp.resolve_vetted("aws6.test", 443).is_err());
    // An ordinary ULA address is still just private, and allowable.
    let ula = AddressPolicy::default()
        .with_resolver(|_| vec![ip("fd12::1")])
        .allow_if(vec!["ula.test".into()], |_| true);
    assert!(ula.resolve_vetted("ula.test", 80).is_ok());
}

// ---------------------------------------------------------------- finding 5
#[test]
fn f5_huge_retry_after_does_not_panic_and_is_capped() {
    let server = MockServer::start(|_, i| {
        if i == 0 {
            MockResponse::new(503).header("Retry-After", "18446744073709551616")
        } else {
            MockResponse::text(200, "ok")
        }
    });
    let started = Instant::now();
    let c = Client::new(ClientConfig {
        retry: RetryPolicy {
            max_attempts: 2,
            initial_backoff: Duration::from_millis(5),
            max_backoff: Duration::from_millis(5),
            max_retry_after: Duration::from_millis(60),
            jitter: 0.0,
            ..RetryPolicy::default()
        },
        ..loopback_config()
    });
    assert_eq!(c.send(&Request::get(server.url("/"))).unwrap().text(), "ok");
    assert!(started.elapsed() < Duration::from_secs(2));
    // Also when retries are disabled: the header is parsed for the error.
    let server = MockServer::start(|_, _| MockResponse::new(503).header("Retry-After", "1e300"));
    let err = Client::new(cfg())
        .send(&Request::get(server.url("/")))
        .unwrap_err();
    assert_eq!(err.status(), Some(503));
}

// ---------------------------------------------------------------- finding 6, 21
#[cfg(unix)]
#[test]
fn f6_part_symlink_is_never_followed_and_policy_runs_first() {
    use std::os::unix::fs::symlink;
    let server = MockServer::start(|_, _| MockResponse::text(200, "fresh bytes"));
    let dir = scratch("symlink");
    let victim = dir.join("victim.txt");
    std::fs::write(&victim, b"precious").unwrap();
    let dest = dir.join("out.bin");
    let part = dir.join("out.bin.part");

    // (a) A denied URL must not touch the filesystem at all.
    symlink(&victim, &part).unwrap();
    let denied = Client::new(ClientConfig {
        retry: RetryPolicy::none(),
        ..ClientConfig::default()
    });
    let err = denied
        .download_to_path(&server.url("/f"), &dest, DownloadOptions::default())
        .unwrap_err();
    assert!(
        matches!(err, DownloadError::Http(HttpError::AddressDenied { .. })),
        "{err:?}"
    );
    assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
    assert!(std::fs::symlink_metadata(&part)
        .unwrap()
        .file_type()
        .is_symlink());

    // (b) An allowed download replaces the link and never writes through it.
    let c = Client::new(cfg());
    let report = c
        .download_to_path(&server.url("/f"), &dest, DownloadOptions::default())
        .unwrap();
    assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
    assert_eq!(std::fs::read(&dest).unwrap(), b"fresh bytes");
    assert_eq!(report.bytes, 11);
    assert!(!part.exists());

    // (c) Resume with a link in place: the link is removed, the victim untouched.
    symlink(&victim, &part).unwrap();
    let dest2 = dir.join("out2.bin");
    let part2 = dir.join("out2.bin.part");
    symlink(&victim, &part2).unwrap();
    c.download_to_path(
        &server.url("/f"),
        &dest2,
        DownloadOptions {
            resume: true,
            ..DownloadOptions::default()
        },
    )
    .unwrap();
    assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
    assert_eq!(std::fs::read(&dest2).unwrap(), b"fresh bytes");

    // (d) A directory at the partial path is refused.
    let dest3 = dir.join("out3.bin");
    std::fs::create_dir(dir.join("out3.bin.part")).unwrap();
    let err = c
        .download_to_path(&server.url("/f"), &dest3, DownloadOptions::default())
        .unwrap_err();
    assert!(matches!(err, DownloadError::Io(_)), "{err:?}");
}

// ---------------------------------------------------------------- finding 10, 11
#[test]
fn f10_request_debug_hides_bearer_tokens_and_embedded_secrets() {
    let req = Request::get("https://example.com/p?key=URLKEY-1")
        .bearer("BEARER-TOKEN-9")
        .secret_header("X-Opaque", "prefix BEARER-TOKEN-9 suffix")
        .header("Authorization-Extra", "plain")
        .header("Cookie", "sid=COOKIEVAL-2")
        .header("X-Note", "see https://h.example/x?token=INLINE-3");
    let text = format!("{req:?}");
    for needle in ["BEARER-TOKEN-9", "URLKEY-1", "COOKIEVAL-2", "INLINE-3"] {
        assert!(!text.contains(needle), "{needle} leaked: {text}");
    }
    assert!(text.contains("[redacted]"));
    // A caller-set Authorization header that was not recorded as a secret is hidden by name.
    let text = format!(
        "{:?}",
        Request::get("http://x/").header("Authorization", "Basic ZZZ-5")
    );
    assert!(!text.contains("ZZZ-5"), "{text}");
}

#[test]
fn f11_response_debug_hides_signed_urls_and_cookies() {
    let server = MockServer::start(|_, _| {
        MockResponse::new(302)
            .header("Location", "https://example.com/?token=SIGNED-4")
            .header("Set-Cookie", "session=COOKIE-5; HttpOnly")
            .header("X-Next", "https://other.example/a?sig=NEXT-6")
    });
    let c = Client::new(cfg());
    let resp = c.send_any(&Request::get(server.url("/"))).unwrap();
    let head = c.send_headers(&Request::get(server.url("/"))).unwrap();
    for text in [format!("{resp:?}"), format!("{head:?}")] {
        for needle in ["SIGNED-4", "COOKIE-5", "NEXT-6"] {
            assert!(!text.contains(needle), "{needle} leaked: {text}");
        }
        assert!(text.contains("example.com"), "{text}");
    }
    // The values are still available to the caller.
    assert!(resp.header("location").unwrap().contains("SIGNED-4"));
}

// ---------------------------------------------------------------- finding 12
#[test]
fn f12_download_errors_are_scrubbed() {
    let dir = scratch("dl-redact");
    let server = MockServer::start(|req, _| match req.path() {
        "/401" => MockResponse::text(401, "token is SECRET-7 here"),
        _ => MockResponse::text(200, "x").header("Content-Encoding", "SECRET-7"),
    });
    let c = Client::new(ClientConfig {
        redact_values: vec!["SECRET-7".into()],
        ..cfg()
    });
    for path in ["/401", "/enc"] {
        let err = c
            .download_to_path(
                &server.url(path),
                &dir.join("f.bin"),
                DownloadOptions::default(),
            )
            .unwrap_err();
        let text = format!("{err} {err:?}");
        assert!(!text.contains("SECRET-7"), "{path}: {text}");
    }
}

// ---------------------------------------------------------------- finding 13
#[test]
fn f13_json_decode_errors_do_not_echo_the_body() {
    let server = MockServer::start(|_, _| {
        MockResponse::new(200)
            .header("Content-Type", "application/json")
            .body(b"\"SECRET-8\"".to_vec())
    });
    let c = Client::new(ClientConfig {
        redact_values: vec!["SECRET-8".into()],
        ..cfg()
    });
    let err = c.get_json::<u64>(&server.url("/")).unwrap_err();
    assert!(matches!(err, HttpError::Json(_)));
    assert!(!format!("{err} {err:?}").contains("SECRET-8"));
    // Even without configured values: the decode error names no content.
    let resp = c.send(&Request::get(server.url("/"))).unwrap();
    let err = resp.json::<u64>().unwrap_err();
    assert!(!format!("{err}").contains("SECRET-8"), "{err}");
}

// ---------------------------------------------------------------- finding 15
#[test]
fn f15_resume_does_not_splice_a_different_resource() {
    let n = Arc::new(AtomicUsize::new(0));
    let counter = n.clone();
    let server = MockServer::start(move |req, _| match req.path() {
        "/dl" => {
            let i = counter.fetch_add(1, Ordering::SeqCst);
            let to = if i == 0 { "/a" } else { "/b" };
            MockResponse::new(302).header("Location", to)
        }
        "/a" => {
            let mut r = MockResponse::new(200).header("ETag", "\"v1\"");
            r.body = MockBody::TruncatedAfter(b"AAAA".to_vec());
            r
        }
        _ => {
            if req.header("range").is_some() {
                MockResponse::new(206)
                    .header("ETag", "\"v1\"")
                    .header("Content-Range", "bytes 4-11/12")
                    .body(b"BBBBBBBB".to_vec())
            } else {
                MockResponse::new(200)
                    .header("ETag", "\"v1\"")
                    .body(b"BBBBBBBBBBBB".to_vec())
            }
        }
    });
    let dir = scratch("splice");
    let dest = dir.join("out.bin");
    let c = Client::new(ClientConfig {
        max_redirects: 3,
        ..cfg()
    });
    c.download_to_path(
        &server.url("/dl"),
        &dest,
        DownloadOptions {
            max_reconnects: 2,
            ..DownloadOptions::default()
        },
    )
    .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"BBBBBBBBBBBB");
}

// ---------------------------------------------------------------- finding 16
#[test]
fn f16_incomplete_or_malformed_content_range_is_not_published() {
    let payload = [7u8; 200];
    let dir = scratch("range");
    for (label, content_range) in [
        ("unknown total, short body", "bytes 80-199/*"),
        ("garbage end", "bytes 80-garbage/*"),
        ("end beyond total", "bytes 80-300/200"),
    ] {
        let dest = dir.join(format!("{}.bin", label.replace(' ', "_")));
        std::fs::write(
            dir.join(format!("{}.bin.part", label.replace(' ', "_"))),
            &payload[..80],
        )
        .unwrap();
        let cr = content_range.to_string();
        let server = MockServer::start(move |_, _| {
            MockResponse::new(206)
                .header("Content-Range", &cr)
                .body(vec![7u8; 20])
        });
        let c = Client::new(cfg());
        let err = c
            .download_to_path(
                &server.url("/f"),
                &dest,
                DownloadOptions {
                    resume: true,
                    ..DownloadOptions::default()
                },
            )
            .unwrap_err();
        assert!(!dest.exists(), "{label}: a partial file was published");
        assert!(
            matches!(err, DownloadError::Http(HttpError::Stream(_))),
            "{label}: unexpected {err:?}"
        );
    }
}

// ---------------------------------------------------------------- finding 17
#[test]
fn f17_transparently_decoded_downloads_are_refused() {
    let server = MockServer::start(|_, _| {
        MockResponse::new(200)
            .header("Content-Encoding", "gzip")
            .body(vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3, 1, 2, 3])
    });
    let dir = scratch("gzip");
    let dest = dir.join("out.bin");
    let err = Client::new(cfg())
        .download_to_path(&server.url("/f"), &dest, DownloadOptions::default())
        .unwrap_err();
    assert!(
        matches!(err, DownloadError::Http(HttpError::Stream(_))),
        "{err:?}"
    );
    assert!(!dest.exists());
}

// ---------------------------------------------------------------- finding 18
#[test]
fn f18_download_stops_at_expected_bytes_while_writing() {
    let server = MockServer::start(|_, _| {
        MockResponse::new(200).chunks(vec![
            (Duration::ZERO, vec![1u8; 12]),
            (Duration::from_secs(3), vec![2u8; 12]),
        ])
    });
    let dir = scratch("expected");
    let dest = dir.join("out.bin");
    let started = Instant::now();
    let err = Client::new(cfg())
        .download_to_path(
            &server.url("/f"),
            &dest,
            DownloadOptions {
                expected_bytes: Some(10),
                ..DownloadOptions::default()
            },
        )
        .unwrap_err();
    assert!(
        matches!(err, DownloadError::LengthMismatch { expected: 10, .. }),
        "{err:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(!dest.exists() && !dir.join("out.bin.part").exists());
}

// ---------------------------------------------------------------- finding 19
#[test]
fn f19_name_lookup_runs_under_the_connect_deadline() {
    let policy = AddressPolicy::default().with_resolver(|_| {
        std::thread::sleep(Duration::from_secs(3));
        vec![ip("93.184.216.34")]
    });
    let c = Client::new(ClientConfig {
        address_policy: policy,
        connect_timeout: Some(Duration::from_millis(250)),
        retry: RetryPolicy::none(),
        ..ClientConfig::default()
    });
    // `stream` has no global timeout, which is the case the finding describes.
    let started = Instant::now();
    let err = c
        .stream(&Request::get("http://slow-dns.test/"))
        .err()
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert!(
        matches!(
            err,
            HttpError::Timeout {
                phase: Phase::Connect,
                ..
            }
        ),
        "{err:?}"
    );
}

// ---------------------------------------------------------------- finding 20
#[test]
fn f20_cancellation_stops_error_body_and_retry_drains() {
    let trickle = || {
        MockResponse::new(503).chunks(
            (0..30)
                .map(|_| (Duration::from_millis(300), vec![b'x']))
                .collect(),
        )
    };
    let server = MockServer::start(move |_, _| trickle());
    // Retry drain.
    let token = CancelToken::new();
    let t = token.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        t.cancel();
    });
    let c = Client::new(ClientConfig {
        retry: RetryPolicy {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(5),
            jitter: 0.0,
            ..RetryPolicy::default()
        },
        ..loopback_config()
    });
    let started = Instant::now();
    let err = c
        .send(&Request::get(server.url("/")).cancel(token))
        .unwrap_err();
    assert!(matches!(err, HttpError::Cancelled), "{err:?}");
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "{:?}",
        started.elapsed()
    );

    // Error-body read of `stream`.
    let token = CancelToken::new();
    let t = token.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        t.cancel();
    });
    let started = Instant::now();
    let err = Client::new(cfg())
        .stream(&Request::get(server.url("/")).cancel(token))
        .err()
        .unwrap();
    assert!(matches!(err, HttpError::Cancelled), "{err:?}");
    assert!(started.elapsed() < Duration::from_millis(1500));

    // Error-body read of a download.
    let flag = Arc::new(AtomicBool::new(false));
    let f = flag.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        f.store(true, Ordering::SeqCst);
    });
    let dir = scratch("drain");
    let started = Instant::now();
    let err = Client::new(cfg())
        .download_to_path(
            &server.url("/"),
            &dir.join("f.bin"),
            DownloadOptions {
                cancel: Some(&flag),
                ..DownloadOptions::default()
            },
        )
        .unwrap_err();
    assert!(matches!(err, DownloadError::Cancelled { .. }), "{err:?}");
    assert!(started.elapsed() < Duration::from_millis(1500));
}

// ---------------------------------------------------------------- finding 9 (policy part)
#[test]
fn f9_https_to_http_redirects_are_refused_unless_enabled() {
    // The wiring runs in the hop loop of both clients; the decision is this policy method,
    // which is also what `allow_insecure_redirects` flips. (An end-to-end HTTPS fixture
    // would need a TLS server, which this crate's tests do not carry.)
    let policy = AddressPolicy::default();
    assert!(!policy.allows_redirect("https", "http"));
    assert!(policy.allows_redirect("https", "https"));
    assert!(policy.allows_redirect("http", "http"));
    assert!(policy.allows_redirect("http", "https"));
    let open = AddressPolicy::default().allow_insecure_redirects();
    assert!(open.allows_redirect("https", "http"));
    let _ = loopback_policy();
}

// ---------------------------------------------------------------- ScrapeRight: spelling policy
#[test]
fn non_canonical_ipv4_spellings_are_refused_by_default_with_a_typed_error() {
    let policy = AddressPolicy::default();
    for h in ["0177.0.0.1", "0x7f.1", "2130706433", "127.1", "1.1.1.1."] {
        match policy.resolve_vetted(h, 80).unwrap_err() {
            HttpError::AddressDenied { reason, addr, .. } => {
                assert!(reason.contains("non-canonical"), "{h}: {reason}");
                assert!(addr.is_some(), "{h}");
            }
            other => panic!("{h}: {other:?}"),
        }
    }
    // Canonical spellings pass the spelling rule (and then the class rules).
    assert!(policy.resolve_vetted("1.1.1.1", 443).is_ok());
    assert!(policy.resolve_vetted("::ffff:1.1.1.1", 443).is_ok());
    assert!(policy.resolve_vetted("[2606:4700:4700::1111]", 443).is_ok());
    // A listed host does not rescue a non-canonical spelling.
    let listed = AddressPolicy::default().allow_if(vec!["127.1".into()], |_| true);
    assert!(listed.resolve_vetted("127.1", 80).is_err());
}

#[test]
fn the_spelling_rule_has_an_opt_out_that_still_applies_the_class_rules() {
    let server = MockServer::start(|_, _| MockResponse::text(200, "ok"));
    let url = format!("http://127.1:{}/", server.port());
    // Default: refused before any connection.
    let strict = Client::new(ClientConfig {
        address_policy: AddressPolicy::default()
            .allow_if(vec!["127.1".into()], |a| a.is_loopback()),
        retry: RetryPolicy::none(),
        ..ClientConfig::default()
    });
    let err = strict.send(&Request::get(&url)).unwrap_err();
    assert!(err.to_string().contains("non-canonical"), "{err}");
    assert_eq!(server.request_count(), 0);
    // Opt-out plus an explicit allow reaches the server.
    let lax = Client::new(ClientConfig {
        address_policy: AddressPolicy::default()
            .allow_legacy_ipv4()
            .allow_if(vec!["127.1".into()], |a| a.is_loopback()),
        retry: RetryPolicy::none(),
        ..ClientConfig::default()
    });
    assert_eq!(lax.send(&Request::get(&url)).unwrap().text(), "ok");
    // Opt-out alone still denies by class.
    let only = Client::new(ClientConfig {
        address_policy: AddressPolicy::default().allow_legacy_ipv4(),
        retry: RetryPolicy::none(),
        ..ClientConfig::default()
    });
    let err = only.send(&Request::get(&url)).unwrap_err();
    assert_eq!(denied_class(&err), Some(AddressClass::Loopback));
    assert!(!err.to_string().contains("non-canonical"), "{err}");
}

#[test]
fn host_ip_parsing_is_public() {
    let p = parse_host_ip("0x7f.1").unwrap();
    assert_eq!(p.addr, ip("127.0.0.1"));
    assert_eq!(p.kind, HostIpKind::Ipv4Legacy);
    assert!(!p.is_canonical);
    let p = parse_host_ip("1.1.1.1").unwrap();
    assert!(p.is_canonical && p.kind == HostIpKind::Ipv4DottedQuad);
    assert!(!parse_host_ip("1.1.1.1.").unwrap().is_canonical);
    assert!(parse_host_ip("[::ffff:127.0.0.1]").unwrap().is_canonical);
    assert!(parse_host_ip("example.com").is_none());
    assert_eq!(
        parse_legacy_ipv4("2130706433"),
        Some("127.0.0.1".parse().unwrap())
    );
}

// ---------------------------------------------------------------- proxy diagnostics
#[test]
fn proxy_diagnostic_names_the_exempt_endpoint_without_credentials() {
    let proxy = TestProxy::start();
    let with_creds = proxy
        .url()
        .replace("http://", "http://svc-user:svc-pass-3@");
    let c = Client::new(ClientConfig {
        proxy: ProxyMode::Explicit(with_creds),
        ..cfg()
    });
    let d = c.proxy_diagnostic().unwrap();
    assert_eq!(d.endpoint, proxy.url());
    assert!(!d.from_environment && d.endpoint_exempt_from_class_check);
    assert!(!format!("{d:?}").contains("svc-pass-3"));
    assert!(Client::new(cfg()).proxy_diagnostic().is_none());

    // A denial through the proxy says only names and literals were checked.
    let err = c.send(&Request::get("http://printer.local/")).unwrap_err();
    assert!(err.to_string().contains("through a proxy"), "{err}");
    let text = format!("{err} {err:?}");
    assert!(
        !text.contains("svc-pass-3") && !text.contains("svc-user"),
        "{text}"
    );
}
