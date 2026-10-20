//! The loopback host (`remote-loopback`, PK06-5) over real TCP: bounded parser, loopback bind,
//! Host/Origin/CSRF policy, bearer and capability verification, CodeRight's presence routes and
//! discovery record.

use rightkit_daemon::loopback::*;
use rightkit_daemon::ServiceError;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct TestApp {
    seen: Mutex<Vec<(String, String)>>,
}

impl LoopbackApp for TestApp {
    fn handle(&self, request: &Request, auth: &Authenticated) -> Response {
        self.seen
            .lock()
            .unwrap()
            .push((request.path.clone(), format!("{auth:?}")));
        if request.path == "/panic" {
            panic!("a handler bug");
        }
        if request.path.starts_with("/sessions/") && request.path.ends_with("/events") {
            return Response::event_stream(Box::new(|sink| {
                let _ = sink.send(b"data: 1\n\n");
                let _ = sink.send(b"data: 2\n\n");
            }));
        }
        Response::json(
            200,
            &json!({
                "path": request.path,
                "auth": format!("{auth:?}"),
                "len": request.body.len(),
            }),
        )
    }

    fn health(&self) -> Value {
        json!({"ok": true, "databasePath": "db"})
    }
}

struct Cap;

impl CapabilityVerifier for Cap {
    fn verify(
        &self,
        route: &str,
        presented: &str,
        _request: &Request,
    ) -> Result<CapabilityGrant, ServiceError> {
        if presented == "join-secret" {
            Ok(CapabilityGrant {
                subject: route.to_string(),
                scopes: vec!["room".into()],
            })
        } else {
            Err(ServiceError::new(
                rightkit_daemon::ERROR_UNAUTHENTICATED,
                "no",
            ))
        }
    }
}

fn base_config() -> LoopbackConfig {
    LoopbackConfig::new()
        .legacy_token_header("X-Legacy-Token")
        .origin_policy(OriginPolicy::new().allow_origin("tauri://localhost"))
        .route(RouteSpec::bearer_query_token("/sessions/{id}/events"))
        .route(RouteSpec::public(
            Method::Post,
            "/triggers/{id}/deliver/github",
        ))
        .route(RouteSpec::capability(
            Some(Method::Get),
            "/join/{capability}",
            CapabilitySource::PathSegment(0),
        ))
        .capability_verifier(Arc::new(Cap))
}

fn start(config: LoopbackConfig) -> (Loopback, Arc<TestApp>) {
    let app = Arc::new(TestApp {
        seen: Mutex::new(Vec::new()),
    });
    let host = Loopback::start(config, app.clone()).expect("the loopback host starts");
    (host, app)
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

fn connect(host: &Loopback) -> TcpStream {
    let stream = TcpStream::connect(("127.0.0.1", host.port())).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
}

/// One response: the head, then `Content-Length` bytes, or everything to the end of the stream
/// when the length is not stated (a chunked stream, which the host closes after).
fn read_reply(stream: &mut TcpStream) -> Reply {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            other => panic!("the response head ended early: {other:?} after {head:?}"),
        }
    }
    let text = String::from_utf8(head).unwrap();
    let mut lines = text.split("\r\n");
    let status: u16 = lines
        .next()
        .unwrap()
        .split(' ')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers: Vec<(String, String)> = lines
        .filter(|l| !l.is_empty())
        .filter_map(|l| l.split_once(": "))
        .map(|(n, v)| (n.to_ascii_lowercase(), v.to_string()))
        .collect();
    let length = headers
        .iter()
        .find(|(n, _)| n == "content-length")
        .and_then(|(_, v)| v.parse::<usize>().ok());
    let mut body = Vec::new();
    match length {
        Some(n) => {
            body.resize(n, 0);
            stream.read_exact(&mut body).unwrap();
        }
        None if status == 204 => {}
        None => {
            let _ = stream.read_to_end(&mut body);
        }
    }
    Reply {
        status,
        headers,
        body,
    }
}

fn send(stream: &mut TcpStream, raw: &str) -> Reply {
    stream.write_all(raw.as_bytes()).unwrap();
    read_reply(stream)
}

/// A request that closes after its answer. `extra` is header lines, each ending in CRLF.
fn call(host: &Loopback, method: &str, path: &str, extra: &str, body: &str) -> Reply {
    let mut stream = connect(host);
    let length = if body.is_empty() {
        String::new()
    } else {
        format!("Content-Length: {}\r\n", body.len())
    };
    send(
        &mut stream,
        &format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n{extra}{length}\r\n{body}",
            host.port()
        ),
    )
}

fn bearer(host: &Loopback) -> String {
    format!("Authorization: Bearer {}\r\n", host.token().expose())
}

const JSON: &str = "Content-Type: application/json\r\n";

#[test]
fn health_needs_no_token_and_everything_else_does() {
    let (host, app) = start(base_config());
    let health = call(&host, "GET", "/health", "", "");
    assert_eq!(health.status, 200);
    assert_eq!(health.json(), json!({"ok": true, "databasePath": "db"}));
    assert_eq!(health.header("cache-control"), Some("no-store"));

    let none = call(&host, "GET", "/anything", "", "");
    assert_eq!((none.status, none.text().as_str()), (401, "unauthorized"));
    let wrong = call(
        &host,
        "GET",
        "/anything",
        "Authorization: Bearer nope\r\n",
        "",
    );
    assert_eq!((wrong.status, wrong.text().as_str()), (401, "unauthorized"));
    let basic = call(
        &host,
        "GET",
        "/anything",
        "Authorization: Basic abc\r\n",
        "",
    );
    assert_eq!(basic.status, 401);
    assert!(
        app.seen.lock().unwrap().is_empty(),
        "no handler ran for a refused request"
    );

    let ok = call(&host, "GET", "/anything", &bearer(&host), "");
    assert_eq!(ok.status, 200);
    assert_eq!(ok.json()["path"], "/anything");
    assert_eq!(ok.json()["auth"], "Bearer");
    let legacy = call(
        &host,
        "GET",
        "/anything",
        &format!("X-Legacy-Token: {}\r\n", host.token().expose()),
        "",
    );
    assert_eq!(legacy.status, 200);
    assert_eq!(app.seen.lock().unwrap().len(), 2);
}

#[test]
fn the_host_listens_on_loopback_and_publishes_a_matching_record() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("daemon.port");
    let labels = RecordLabels {
        database_path: "app.db".into(),
        protocol_version: 1,
        schema_id: "test.daemon.v1".into(),
        runtime: "rust".into(),
        generation: 1,
    };
    let (host, _app) = start(base_config().record_path(&path).record_labels(labels));
    assert_ne!(host.port(), 0);
    assert_eq!(host.base_url(), format!("http://127.0.0.1:{}", host.port()));
    let written = LoopbackRecord::parse(&std::fs::read_to_string(&path).unwrap()).expect("record");
    assert_eq!(&written, host.record());
    assert_eq!(written.port, host.port());
    assert_eq!(written.token, host.token().expose());
    assert_eq!(written.pid, std::process::id());
    assert_eq!(written.database_path, "app.db");
    assert!(written.process_matches());
    // A reader finds the host with what the record says.
    let mut stream = TcpStream::connect(("127.0.0.1", written.port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let reply = send(
        &mut stream,
        &format!(
            "GET /x HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n",
            written.port, written.token
        ),
    );
    assert_eq!(reply.status, 200);
    host.shutdown();
    assert!(!path.exists(), "the record goes with the host");
}

#[test]
fn a_rebound_name_and_a_foreign_origin_never_reach_the_handler() {
    let (host, app) = start(base_config());
    let auth = bearer(&host);
    // DNS rebinding: the page's own name arrives in Host.
    let mut stream = connect(&host);
    let rebound = send(
        &mut stream,
        &format!(
            "GET /x HTTP/1.1\r\nHost: evil.example:{}\r\n{auth}Connection: close\r\n\r\n",
            host.port()
        ),
    );
    assert_eq!(rebound.status, 421);
    // A cross-origin page (and `null`) is refused even with a valid token.
    for origin in ["https://evil.example", "null"] {
        let r = call(
            &host,
            "GET",
            "/x",
            &format!("{auth}Origin: {origin}\r\n"),
            "",
        );
        assert_eq!(r.status, 403, "{origin}");
        assert_eq!(r.header("access-control-allow-origin"), None);
    }
    assert!(app.seen.lock().unwrap().is_empty());
    // The webview origin is allowed and gets the CORS headers to read the answer.
    let ok = call(
        &host,
        "GET",
        "/x",
        &format!("{auth}Origin: tauri://localhost\r\n"),
        "",
    );
    assert_eq!(ok.status, 200);
    assert_eq!(
        ok.header("access-control-allow-origin"),
        Some("tauri://localhost")
    );
    // Even a 401 is readable by the allowed origin.
    let denied = call(&host, "GET", "/x", "Origin: tauri://localhost\r\n", "");
    assert_eq!(denied.status, 401);
    assert_eq!(
        denied.header("access-control-allow-origin"),
        Some("tauri://localhost")
    );
    // A preflight needs no token, is answered for the allowed origin only.
    let preflight = call(
        &host,
        "OPTIONS",
        "/x",
        "Origin: tauri://localhost\r\nAccess-Control-Request-Method: POST\r\nAccess-Control-Request-Headers: authorization, content-type\r\n",
        "",
    );
    assert_eq!(preflight.status, 204);
    assert_eq!(
        preflight.header("access-control-allow-origin"),
        Some("tauri://localhost")
    );
    assert!(preflight
        .header("access-control-allow-headers")
        .unwrap()
        .contains("authorization"));
    let foreign = call(
        &host,
        "OPTIONS",
        "/x",
        "Origin: https://evil.example\r\n",
        "",
    );
    assert_eq!(foreign.status, 403);
    // The host's own origin is allowed (a page it serves itself).
    let own = call(
        &host,
        "GET",
        "/x",
        &format!("{auth}Origin: http://127.0.0.1:{}\r\n", host.port()),
        "",
    );
    assert_eq!(own.status, 200);
}

#[test]
fn a_cross_site_form_post_cannot_change_state() {
    let (host, app) = start(base_config());
    let auth = bearer(&host);
    // Browser metadata says cross-site, no Origin header: refused.
    let cross = call(
        &host,
        "POST",
        "/x",
        &format!("{auth}Sec-Fetch-Site: cross-site\r\n{JSON}"),
        "{}",
    );
    assert_eq!(cross.status, 403);
    // A form body is not JSON: refused (415) even with a token.
    let form = call(
        &host,
        "POST",
        "/x",
        &format!("{auth}Content-Type: application/x-www-form-urlencoded\r\n"),
        "a=1",
    );
    assert_eq!(form.status, 415);
    let json = call(&host, "POST", "/x", &format!("{auth}{JSON}"), "{\"a\":1}");
    assert_eq!(json.status, 200);
    assert_eq!(json.json()["len"], 7);
    assert_eq!(app.seen.lock().unwrap().len(), 1);
}

#[test]
fn the_query_token_works_on_the_declared_stream_route_only() {
    let (host, _app) = start(base_config());
    let token = host.token().expose().to_string();
    let stream = call(
        &host,
        "GET",
        &format!("/sessions/s1/events?access_token={token}"),
        "",
        "",
    );
    assert_eq!(stream.status, 200);
    assert_eq!(stream.header("content-type"), Some("text/event-stream"));
    assert_eq!(stream.header("transfer-encoding"), Some("chunked"));
    let text = stream.text();
    assert!(
        text.contains("data: 1\n\n") && text.contains("data: 2\n\n"),
        "{text}"
    );
    for path in [
        format!("/sessions/s1/events/history?access_token={token}"),
        format!("/sessions/s1/messages?access_token={token}"),
        format!("/anything?access_token={token}"),
    ] {
        assert_eq!(call(&host, "GET", &path, "", "").status, 401, "{path}");
    }
}

#[test]
fn public_and_capability_routes_are_exact_and_verified_inside_the_owner() {
    let (host, app) = start(base_config());
    // A signed-delivery route has no host auth; the handler is told so.
    let public = call(&host, "POST", "/triggers/t1/deliver/github", JSON, "{}");
    assert_eq!(public.status, 200);
    assert_eq!(public.json()["auth"], "Public");
    for (method, path) in [
        ("POST", "/triggers/t1/deliver/github/extra"),
        ("POST", "/triggers/t1/deliver"),
        ("GET", "/triggers/t1/deliver/github"),
    ] {
        assert_eq!(call(&host, method, path, JSON, "").status, 401, "{path}");
    }
    // A capability in the path is verified by the owner before the handler.
    let joined = call(&host, "GET", "/join/join-secret", "", "");
    assert_eq!(joined.status, 200);
    assert!(joined.json()["auth"]
        .as_str()
        .unwrap()
        .contains("Capability"));
    assert!(joined.json()["auth"].as_str().unwrap().contains("room"));
    let guessed = call(&host, "GET", "/join/guess", "", "");
    assert_eq!(
        (guessed.status, guessed.text().as_str()),
        (401, "unauthorized")
    );
    // The bearer token is not a capability.
    let bearer_on_capability = call(
        &host,
        "POST",
        "/join/join-secret",
        &format!("{}{JSON}", bearer(&host)),
        "{}",
    );
    assert_eq!(
        bearer_on_capability.status, 200,
        "an undeclared method is an ordinary bearer route"
    );
    assert_eq!(bearer_on_capability.json()["auth"], "Bearer");
    assert_eq!(app.seen.lock().unwrap().len(), 3);
}

#[test]
fn the_parser_refuses_what_it_should_before_a_handler_runs() {
    let (host, app) = start(base_config());
    let auth = bearer(&host);
    let port = host.port();
    let raw = |request: String| {
        let mut stream = connect(&host);
        send(&mut stream, &request)
    };
    // Over the body limit: refused on the head alone, nothing is read.
    let big = raw(format!(
        "POST /x HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{auth}{JSON}Content-Length: 2000000\r\n\r\n"
    ));
    assert_eq!(big.status, 413);
    // No chunked requests at all (no smuggling).
    let chunked = raw(format!(
        "POST /x HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{auth}{JSON}Transfer-Encoding: chunked\r\n\r\n0\r\n\r\n"
    ));
    assert_eq!(chunked.status, 501);
    let expect = raw(format!(
        "POST /x HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{auth}{JSON}Expect: 100-continue\r\nContent-Length: 2\r\n\r\n{{}}"
    ));
    assert_eq!(expect.status, 417);
    // A repeated Host or Authorization is refused, not resolved.
    let two_hosts = raw(format!(
        "GET /x HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nHost: evil.example\r\n{auth}\r\n"
    ));
    assert_eq!(two_hosts.status, 400);
    let two_auth = raw(format!(
        "GET /x HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{auth}Authorization: Bearer other\r\n\r\n"
    ));
    assert_eq!(two_auth.status, 400);
    // Path tricks, wrong version, unknown method.
    for target in ["/a/../b", "/a/%2e%2e/b", "/a%00b", "//x"] {
        let r = raw(format!(
            "GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{auth}\r\n"
        ));
        assert_eq!(r.status, 400, "{target}");
    }
    let old = raw(format!(
        "GET /x HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n{auth}\r\n"
    ));
    assert_eq!(old.status, 505);
    let trace = raw(format!(
        "TRACE /x HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{auth}\r\n"
    ));
    assert_eq!(trace.status, 405);
    let long = raw(format!(
        "GET /{} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{auth}\r\n",
        "a".repeat(9000)
    ));
    assert_eq!(long.status, 414);
    // A JSON body nested past the bound is refused before the handler.
    let deep = format!("{}{}", "[".repeat(100), "]".repeat(100));
    let nested = call(&host, "POST", "/x", &format!("{auth}{JSON}"), &deep);
    assert_eq!(nested.status, 400);
    assert!(
        app.seen.lock().unwrap().is_empty(),
        "{:?}",
        app.seen.lock().unwrap()
    );
}

#[test]
fn a_client_that_trickles_its_head_is_cut_off() {
    let limits = Limits {
        head_timeout: Duration::from_millis(300),
        idle_timeout: Duration::from_secs(5),
        ..Limits::default()
    };
    let (host, _app) = start(base_config().limits(limits));
    let mut stream = connect(&host);
    stream.write_all(b"GET /x HT").unwrap();
    std::thread::sleep(Duration::from_millis(1200));
    // The host gave up: the connection ends without an answer.
    let mut rest = Vec::new();
    let outcome = stream.read_to_end(&mut rest);
    assert!(outcome.is_err() || rest.is_empty(), "{rest:?}");
    // And it still serves others.
    assert_eq!(call(&host, "GET", "/health", "", "").status, 200);
}

#[test]
fn a_handler_panic_is_a_500_and_the_host_keeps_serving() {
    let (host, _app) = start(base_config());
    let auth = bearer(&host);
    let boom = call(&host, "GET", "/panic", &auth, "");
    assert_eq!(boom.status, 500);
    assert_eq!(call(&host, "GET", "/fine", &auth, "").status, 200);
}

#[test]
fn guessing_the_token_is_limited_but_the_right_token_is_never_refused() {
    let (host, _app) = start(base_config());
    let mut stream = connect(&host);
    let port = host.port();
    let attempt = |stream: &mut TcpStream, token: &str| {
        send(
            stream,
            &format!(
                "GET /x HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\n\r\n"
            ),
        )
    };
    for i in 0..5 {
        assert_eq!(attempt(&mut stream, &format!("wrong-{i}")).status, 401);
    }
    let blocked = attempt(&mut stream, "wrong-again");
    assert_eq!(
        (blocked.status, blocked.text().as_str()),
        (429, "rate limited")
    );
    // The same connection with the right token is served: the limiter only judges failures.
    let right = attempt(&mut stream, host.token().expose());
    assert_eq!(right.status, 200);
}

#[test]
fn the_global_token_bucket_answers_429() {
    let (host, _app) = start(base_config().rate_limit(Some((0.001, 3.0))));
    for _ in 0..3 {
        assert_eq!(call(&host, "GET", "/health", "", "").status, 200);
    }
    let limited = call(&host, "GET", "/health", "", "");
    assert_eq!(
        (limited.status, limited.text().as_str()),
        (429, "rate limited")
    );
}

#[test]
fn presence_follows_coderight_and_the_host_may_exit_when_the_last_client_leaves() {
    let idle = Arc::new(AtomicBool::new(false));
    let flag = idle.clone();
    let config = base_config()
        .presence(PresenceConfig {
            heartbeat_seconds: 1,
            expiry_seconds: 3,
            linger_seconds: 0,
        })
        .on_idle(move || flag.store(true, Ordering::SeqCst));
    let (host, _app) = start(config);
    let auth = bearer(&host);
    let headers = format!("{auth}{JSON}");
    let attach = |kind: &str| {
        call(
            &host,
            "POST",
            "/clients",
            &headers,
            &format!("{{\"kind\":\"{kind}\",\"pid\":42,\"label\":\"tauri-shell\"}}"),
        )
    };
    assert_eq!(
        call(
            &host,
            "POST",
            "/clients",
            JSON,
            "{\"kind\":\"app\",\"pid\":1}"
        )
        .status,
        401,
        "presence is behind the token"
    );
    let first = attach("app");
    assert_eq!(first.status, 200);
    let client_id = first.json()["clientId"].as_str().unwrap().to_string();
    assert_eq!(first.json()["heartbeatSeconds"], 1);
    assert_eq!(first.json()["expirySeconds"], 3);
    assert_eq!(attach("app").status, 409, "one live app at a time");
    assert_eq!(attach("window").status, 400);
    assert_eq!(attach("cli").status, 200);
    let list = call(&host, "GET", "/clients", &auth, "");
    assert_eq!(list.status, 200);
    assert_eq!(list.json()["clients"].as_array().unwrap().len(), 2);
    assert_eq!(list.json()["clients"][0]["label"], "tauri-shell");
    let heartbeat = format!("/clients/{client_id}/heartbeat");
    assert_eq!(call(&host, "POST", &heartbeat, &auth, "").status, 204);
    assert_eq!(
        call(&host, "POST", "/clients/c-unknown/heartbeat", &auth, "").status,
        404
    );
    assert_eq!(
        call(&host, "DELETE", &format!("/clients/{client_id}"), &auth, "").status,
        204
    );
    assert_eq!(
        call(&host, "DELETE", &format!("/clients/{client_id}"), &auth, "").status,
        404
    );
    // The cli client stops heartbeating and expires: the count reaches zero and the owner is told.
    let until = Instant::now() + Duration::from_secs(15);
    while !idle.load(Ordering::SeqCst) {
        assert!(Instant::now() < until, "the idle callback never fired");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(host.presence().unwrap().live_count(Instant::now()), 0);
}

#[test]
fn shutdown_closes_open_connections_and_frees_the_port() {
    let (host, _app) = start(base_config());
    let mut open = connect(&host);
    let port = host.port();
    let reply = send(
        &mut open,
        &format!("GET /health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
    );
    assert_eq!(reply.status, 200);
    host.shutdown();
    assert!(host.wait_stopped(Duration::from_secs(5)));
    let mut rest = Vec::new();
    let outcome = open.read_to_end(&mut rest);
    assert!(outcome.is_err() || rest.is_empty(), "{rest:?}");
    assert!(
        TcpStream::connect(("127.0.0.1", port)).is_err(),
        "the listener is closed"
    );
    host.shutdown();
}

#[test]
fn a_bad_pattern_or_a_capability_route_without_a_verifier_does_not_start() {
    let app = Arc::new(TestApp {
        seen: Mutex::new(Vec::new()),
    });
    let bad = LoopbackConfig::new().route(RouteSpec::public(Method::Get, "no-slash"));
    assert!(Loopback::start(bad, app.clone()).is_err());
    let no_verifier = LoopbackConfig::new().route(RouteSpec::capability(
        None,
        "/x",
        CapabilitySource::Header("x-capability".into()),
    ));
    assert!(Loopback::start(no_verifier, app).is_err());
}
