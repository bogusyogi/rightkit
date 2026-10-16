//! Public PKCE helpers, the tolerant loopback listener and the gateway JSON exchange, run
//! against an in-process fake gateway (`rightkit_http::testing::MockServer`).

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use rightkit_http::testing::{loopback_config, MockResponse, MockServer};
use rightkit_http::{percent_encode, CancelToken, Client, ClientConfig, RetryPolicy, Secret};
use rightkit_oauth::callback::{CallbackListener, CallbackOptions, LoopbackHost};
use rightkit_oauth::gateway::GatewayExchange;
use rightkit_oauth::pkce::{self, Pkce};
use rightkit_oauth::OAuthError;

fn client() -> Client {
    Client::new(ClientConfig {
        retry: RetryPolicy::none(),
        ..loopback_config()
    })
}

fn listener(state: &str) -> CallbackListener {
    CallbackListener::bind(
        state,
        CallbackOptions {
            path: "/oauth/callback".into(),
            ..CallbackOptions::default()
        },
    )
    .unwrap()
}

type WaitResult = Result<Secret, OAuthError>;

fn spawn_wait(l: CallbackListener, timeout: Duration) -> thread::JoinHandle<WaitResult> {
    thread::spawn(move || l.wait(timeout, None))
}

/// Send raw bytes to the listener and return the full reply (it closes after answering).
fn raw(port: u16, request: &str) -> String {
    let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(request.as_bytes()).unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out
}

fn get(port: u16, target: &str) -> String {
    raw(
        port,
        &format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"),
    )
}

fn status(reply: &str) -> &str {
    reply.lines().next().unwrap_or("")
}

#[test]
fn listener_ignores_strays_and_returns_only_the_valid_callback() {
    let state = pkce::generate_state().unwrap();
    let l = listener(&state);
    let port = l.port();
    let handle = spawn_wait(l, Duration::from_secs(10));

    // A connection that never sends anything must not block the flow.
    let _stalled = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();

    assert!(status(&get(port, "/favicon.ico")).contains("404"));
    assert!(status(&raw(
        port,
        "OPTIONS /oauth/callback HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: https://evil.example\r\n\r\n"
    ))
    .contains("204"));
    assert!(!raw(port, "OPTIONS /oauth/callback HTTP/1.1\r\n\r\n")
        .to_ascii_lowercase()
        .contains("access-control-allow"));
    assert!(status(&raw(port, "POST /oauth/callback HTTP/1.1\r\n\r\n")).contains("405"));
    assert!(status(&raw(port, "\x01\x02\r\n")).contains("400"));
    assert!(status(&get(port, "/oauth/callback")).contains("400"));
    assert!(status(&get(port, "/oauth/callback?code=stolen")).contains("400"));
    assert!(status(&get(port, "/oauth/callback?code=stolen&state=wrong")).contains("400"));
    // Same length, one character different.
    let mut near = state.clone().into_bytes();
    near[0] = if near[0] == b'A' { b'B' } else { b'A' };
    let near = String::from_utf8(near).unwrap();
    assert!(status(&get(port, &format!("/oauth/callback?code=stolen&state={near}"))).contains("400"));
    assert!(!handle.is_finished(), "a stray request ended the flow");

    let ok = get(
        port,
        &format!("/oauth/callback?code=real-code&state={}", percent_encode(&state)),
    );
    assert!(status(&ok).contains("200"));
    assert!(ok.contains("Authorization complete"));
    assert!(ok.to_ascii_lowercase().contains("cache-control: no-store"));
    let code = handle.join().unwrap().unwrap();
    assert_eq!(code.expose(), "real-code");

    // Single use: the socket is closed once the callback has been served.
    assert!(TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_err());
}

#[test]
fn provider_error_with_valid_state_ends_the_flow() {
    let state = pkce::generate_state().unwrap();
    let l = listener(&state);
    let port = l.port();
    let handle = spawn_wait(l, Duration::from_secs(10));
    get(port, &format!("/oauth/callback?error=access_denied&state={state}"));
    assert!(matches!(
        handle.join().unwrap(),
        Err(OAuthError::AccessDenied)
    ));

    let l = listener(&state);
    let port = l.port();
    let handle = spawn_wait(l, Duration::from_secs(10));
    get(port, &format!("/oauth/callback?error=server_error&state={state}"));
    let err = handle.join().unwrap().unwrap_err();
    assert!(matches!(err, OAuthError::Callback(m) if m.contains("server_error")));

    // An error callback without the right state is a stray, not a result.
    let l = listener(&state);
    let port = l.port();
    let handle = spawn_wait(l, Duration::from_secs(10));
    get(port, "/oauth/callback?error=access_denied&state=nope");
    thread::sleep(Duration::from_millis(100));
    assert!(!handle.is_finished());
    get(port, &format!("/oauth/callback?code=c&state={state}"));
    assert_eq!(handle.join().unwrap().unwrap().expose(), "c");
}

#[test]
fn valid_state_without_a_code_is_an_error() {
    let state = pkce::generate_state().unwrap();
    let l = listener(&state);
    let port = l.port();
    let handle = spawn_wait(l, Duration::from_secs(10));
    get(port, &format!("/oauth/callback?state={state}"));
    assert!(matches!(
        handle.join().unwrap(),
        Err(OAuthError::Callback(_))
    ));
}

#[test]
fn wait_is_bounded_and_cancellable() {
    let state = pkce::generate_state().unwrap();
    let started = Instant::now();
    let err = listener(&state)
        .wait(Duration::from_millis(200), None)
        .unwrap_err();
    assert!(matches!(err, OAuthError::Callback(m) if m.contains("timed out")));
    assert!(started.elapsed() < Duration::from_secs(5));

    let token = CancelToken::new();
    let t2 = token.clone();
    let l = listener(&state);
    let handle = thread::spawn(move || l.wait(Duration::from_secs(30), Some(&t2)));
    thread::sleep(Duration::from_millis(100));
    token.cancel();
    assert!(matches!(
        handle.join().unwrap(),
        Err(OAuthError::Cancelled)
    ));

    for bad in [Duration::ZERO, Duration::from_secs(60 * 60 + 1)] {
        assert!(matches!(
            listener(&state).wait(bad, None),
            Err(OAuthError::InvalidConfig(_))
        ));
    }
}

#[test]
fn bind_validates_state_and_path_and_stays_on_loopback() {
    assert!(CallbackListener::bind("short", CallbackOptions::default()).is_err());
    assert!(CallbackListener::bind("has space in the state value", CallbackOptions::default()).is_err());
    for path in ["callback", "/a?b=c", "/a#f", "/a b", ""] {
        let opts = CallbackOptions {
            path: path.into(),
            ..CallbackOptions::default()
        };
        assert!(CallbackListener::bind(pkce::generate_state().unwrap(), opts).is_err(), "{path:?}");
    }

    let l = listener(&pkce::generate_state().unwrap());
    assert!(l.port() != 0);
    assert_eq!(
        l.redirect_uri(),
        format!("http://127.0.0.1:{}/oauth/callback", l.port())
    );
    let addrs = l.local_addrs();
    assert!(!addrs.is_empty() && addrs.iter().all(|a| a.ip().is_loopback()));


    // Two ephemeral listeners never collide.
    let l2 = listener(&pkce::generate_state().unwrap());
    assert_ne!(l.port(), l2.port());

    // A fixed port that is already taken is an error, not a silent fallback.
    let busy = CallbackOptions {
        port: l.port(),
        ..CallbackOptions::default()
    };
    assert!(CallbackListener::bind(pkce::generate_state().unwrap(), busy).is_err());
}

#[test]
fn both_hosts_serves_ipv4_and_ipv6_when_available() {
    let state = pkce::generate_state().unwrap();
    let l = CallbackListener::bind(
        &state,
        CallbackOptions {
            host: LoopbackHost::Both,
            path: "/cb".into(),
            ..CallbackOptions::default()
        },
    )
    .unwrap();
    let port = l.port();
    assert_eq!(l.redirect_uri(), format!("http://127.0.0.1:{port}/cb"));
    let has_v6 = l.local_addrs().iter().any(|a| a.is_ipv6());
    let handle = spawn_wait(l, Duration::from_secs(10));
    if has_v6 {
        let mut s = TcpStream::connect(("::1", port)).unwrap();
        s.write_all(b"GET /favicon.ico HTTP/1.1\r\n\r\n").unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        assert!(out.contains("404"));
    }
    get(port, &format!("/cb?code=c&state={state}"));
    assert_eq!(handle.join().unwrap().unwrap().expose(), "c");
}

#[test]
fn full_flow_listener_then_gateway_exchange() {
    let pair = Pkce::generate().unwrap();
    let expected_challenge = pair.challenge.clone();
    let state = pkce::generate_state().unwrap();
    let l = listener(&state);
    let port = l.port();
    let redirect_uri = l.redirect_uri();

    let gateway = MockServer::start(move |request, _| {
        assert_eq!(request.method, "POST");
        assert_eq!(request.path(), "/exchange");
        assert_eq!(request.header("content-type"), Some("application/json"));
        assert_eq!(request.header("accept"), Some("application/json"));
        let body = request.body_json();
        assert_eq!(body["code"], "auth-code-1");
        assert_eq!(body["redirect_uri"], format!("http://127.0.0.1:{port}/oauth/callback"));
        assert_eq!(body["resource"], "https://mcp.example/api");
        // The gateway can check S256 binding with the public helper.
        let verifier = body["code_verifier"].as_str().unwrap();
        assert_eq!(pkce::challenge_s256(verifier).unwrap(), expected_challenge);
        MockResponse::json(
            200,
            &serde_json::json!({
                "access_token": "gw-access", "token_type": "Bearer", "expires_in": 1800,
                "refresh_token": "gw-refresh", "scope": "a b"
            }),
        )
    });

    let handle = spawn_wait(l, Duration::from_secs(10));
    get(port, "/favicon.ico");
    get(
        port,
        &format!("/oauth/callback?code=auth-code-1&state={}", percent_encode(&state)),
    );
    let code = handle.join().unwrap().unwrap();

    let tokens = GatewayExchange::new(gateway.url("/exchange"))
        .unwrap()
        .with_field("redirect_uri", redirect_uri)
        .with_field("resource", "https://mcp.example/api")
        .exchange(&client(), code.expose(), pair.verifier.expose(), None)
        .unwrap();
    assert_eq!(tokens.access_token(), "gw-access");
    assert_eq!(tokens.token_type, "Bearer");
    assert_eq!(tokens.expires_in, Some(Duration::from_secs(1800)));
    assert_eq!(tokens.refresh_token.as_ref().unwrap().expose(), "gw-refresh");
    assert_eq!(tokens.scope.as_deref(), Some("a b"));
    assert_eq!(gateway.request_count(), 1);
    let debug = format!("{tokens:?}");
    assert!(!debug.contains("gw-access") && !debug.contains("gw-refresh"));
}

fn verifier() -> String {
    Pkce::generate().unwrap().verifier.expose().to_string()
}

#[test]
fn gateway_errors_are_typed_and_never_echo_credentials() {
    let v = verifier();

    // invalid_grant means sign in again.
    let gw = MockServer::start(|_, _| {
        MockResponse::json(400, &serde_json::json!({"error": "invalid_grant"}))
    });
    let e = GatewayExchange::new(gw.url("/x"))
        .unwrap()
        .exchange(&client(), "c1", &v, None)
        .unwrap_err();
    assert!(matches!(e, OAuthError::NeedsReauth));

    // A gateway that echoes the code and verifier in its error is scrubbed, on 4xx and on 200.
    let echo_v = v.clone();
    let gw = MockServer::start(move |_, _| {
        MockResponse::json(
            400,
            &serde_json::json!({
                "error": "denied code-SECRET-77",
                "error_description": format!("bad verifier {echo_v}")
            }),
        )
    });
    let e = GatewayExchange::new(gw.url("/x"))
        .unwrap()
        .exchange(&client(), "code-SECRET-77", &v, None)
        .unwrap_err();
    let text = format!("{e} {e:?}");
    assert!(matches!(e, OAuthError::Provider { .. }));
    assert!(!text.contains("code-SECRET-77") && !text.contains(&v), "{text}");

    let echo_v = v.clone();
    let gw = MockServer::start(move |_, _| {
        MockResponse::json(
            200,
            &serde_json::json!({"error": "nope code-SECRET-77", "error_description": echo_v}),
        )
    });
    let e = GatewayExchange::new(gw.url("/x"))
        .unwrap()
        .exchange(&client(), "code-SECRET-77", &v, None)
        .unwrap_err();
    let text = format!("{e} {e:?}");
    assert!(matches!(e, OAuthError::Provider { .. }));
    assert!(!text.contains("code-SECRET-77") && !text.contains(&v), "{text}");

    // A server fault is an HTTP error.
    let gw = MockServer::start(|_, _| MockResponse::text(502, "bad gateway"));
    let e = GatewayExchange::new(gw.url("/x"))
        .unwrap()
        .exchange(&client(), "c1", &v, None)
        .unwrap_err();
    assert!(matches!(e, OAuthError::Provider { status: 502, .. } | OAuthError::Http(_)));
}

#[test]
fn gateway_rejects_wrong_typed_or_malformed_token_responses() {
    let v = verifier();
    for body in [
        serde_json::json!({"access_token": "tok-SECRET", "expires_in": "3600"}),
        serde_json::json!({"access_token": "tok-SECRET", "expires_in": -5}),
        serde_json::json!({"access_token": "tok-SECRET", "refresh_token": 9}),
        serde_json::json!({"access_token": 12}),
        serde_json::json!({"token_type": "Bearer"}),
        serde_json::json!(["tok-SECRET"]),
    ] {
        let gw = MockServer::start(move |_, _| MockResponse::json(200, &body));
        let e = GatewayExchange::new(gw.url("/x"))
            .unwrap()
            .exchange(&client(), "c1", &v, None)
            .unwrap_err();
        assert!(matches!(e, OAuthError::Callback(_)), "{e}");
        assert!(!format!("{e} {e:?}").contains("tok-SECRET"));
    }
    let gw = MockServer::start(|_, _| MockResponse::text(200, "not json tok-SECRET"));
    let e = GatewayExchange::new(gw.url("/x"))
        .unwrap()
        .exchange(&client(), "c1", &v, None)
        .unwrap_err();
    assert!(matches!(e, OAuthError::Http(_)));
    assert!(!format!("{e} {e:?}").contains("tok-SECRET"));
}

#[test]
fn gateway_validates_inputs_before_any_request() {
    assert!(matches!(
        GatewayExchange::new("http://gateway.example/exchange"),
        Err(OAuthError::InvalidConfig(_))
    ));
    assert!(GatewayExchange::new("ftp://gateway.example/exchange").is_err());
    assert!(GatewayExchange::new("https://gateway.example/exchange").is_ok());

    let gw = MockServer::start(|_, _| MockResponse::json(200, &serde_json::json!({"access_token": "a"})));
    let g = GatewayExchange::new(gw.url("/x")).unwrap();
    let v = verifier();
    assert!(g.exchange(&client(), "", &v, None).is_err());
    assert!(g.exchange(&client(), "c", "too-short", None).is_err());
    let e = g
        .clone()
        .with_field("code_verifier", "attacker")
        .exchange(&client(), "c", &v, None)
        .unwrap_err();
    assert!(matches!(e, OAuthError::InvalidConfig(_)));
    assert!(g
        .clone()
        .with_field("a", "1")
        .with_field("a", "2")
        .exchange(&client(), "c", &v, None)
        .is_err());
    assert_eq!(gw.request_count(), 0);

    // A cancelled token stops the call.
    let token = CancelToken::new();
    token.cancel();
    assert!(g.exchange(&client(), "c", &v, Some(&token)).is_err());
}
