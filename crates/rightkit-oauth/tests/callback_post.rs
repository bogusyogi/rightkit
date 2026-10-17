//! `CallbackBind::redirect_host` / `CallbackBind::custom_bind_address` (redirect spelling and explicit
//! bind opt-in) and `wait_post_json` (CORS-preflighted JSON callback).

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, TcpStream};
use std::thread;
use std::time::Duration;

use rightkit_http::CancelToken;
use rightkit_oauth::callback::{CallbackBind, CallbackListener, CallbackOptions, MAX_POST_BODY};
use rightkit_oauth::{pkce, OAuthError};
use serde_json::{json, Value};

const ORIGIN: &str = "https://provider.example";

fn listener(state: &str) -> CallbackListener {
    CallbackListener::bind(
        state,
        CallbackOptions {
            path: "/callback".into(),
            ..CallbackOptions::default()
        },
    )
    .unwrap()
}

type PostResult = Result<Value, OAuthError>;

fn spawn_post(l: CallbackListener, timeout: Duration) -> thread::JoinHandle<PostResult> {
    thread::spawn(move || l.wait_post_json(timeout, None, ORIGIN))
}

fn raw(port: u16, request: &[u8]) -> String {
    let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(request).unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out
}

fn status(reply: &str) -> &str {
    reply.lines().next().unwrap_or("")
}

fn lower(reply: &str) -> String {
    reply.to_ascii_lowercase()
}

fn post(port: u16, path: &str, origin: Option<&str>, body: &str) -> String {
    let origin = origin
        .map(|o| format!("Origin: {o}\r\n"))
        .unwrap_or_default();
    raw(
        port,
        format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{origin}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .as_bytes(),
    )
}

#[test]
fn redirect_host_changes_only_the_spelling_of_redirect_uri() {
    let state = pkce::generate_state().unwrap();
    let l = CallbackListener::bind_with(
        &state,
        CallbackBind::new(CallbackOptions {
            path: "/auth/callback".into(),
            ..CallbackOptions::default()
        })
        .redirect_host("localhost"),
    )
    .unwrap();
    assert_eq!(
        l.redirect_uri(),
        format!("http://localhost:{}/auth/callback", l.port())
    );
    // Still bound to loopback only.
    assert!(l.local_addrs().iter().all(|a| a.ip().is_loopback()));
    let port = l.port();
    let handle = thread::spawn(move || l.wait(Duration::from_secs(10), None));
    let ok = raw(
        port,
        format!("GET /auth/callback?code=c-1&state={state} HTTP/1.1\r\n\r\n").as_bytes(),
    );
    assert!(status(&ok).contains("200"));
    assert_eq!(handle.join().unwrap().unwrap().expose(), "c-1");

    // A loopback listener may not name another host in redirect_uri.
    for host in ["evil.example", "", "127.0.0.1.evil.example", "0.0.0.0"] {
        let e = CallbackListener::bind_with(
            pkce::generate_state().unwrap(),
            CallbackBind::new(CallbackOptions::default()).redirect_host(host),
        )
        .unwrap_err();
        assert!(matches!(e, OAuthError::InvalidConfig(_)), "{host:?}: {e}");
    }
    // The defaults are unchanged.
    let l = listener(&pkce::generate_state().unwrap());
    assert!(l.redirect_uri().starts_with("http://127.0.0.1:"));
}

#[test]
fn custom_host_is_an_explicit_opt_in_with_validation() {
    let state = pkce::generate_state().unwrap();
    let lo = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let l = CallbackListener::bind_with(
        &state,
        CallbackBind::new(CallbackOptions::default())
            .custom_bind_address(lo)
            .redirect_host("host.docker.internal"),
    )
    .unwrap();
    assert_eq!(
        l.redirect_uri(),
        format!("http://host.docker.internal:{}/callback", l.port())
    );

    // Without a spelling, a bound custom IP names itself.
    let l = CallbackListener::bind_with(
        &state,
        CallbackBind::new(CallbackOptions::default()).custom_bind_address(lo),
    )
    .unwrap();
    assert_eq!(
        l.redirect_uri(),
        format!("http://127.0.0.1:{}/callback", l.port())
    );

    // An unspecified address is not a redirect host, and a multicast one cannot be bound; both
    // are refused before anything is bound.
    for ip in [
        IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1)),
    ] {
        let e = CallbackListener::bind_with(
            &state,
            CallbackBind::new(CallbackOptions::default()).custom_bind_address(ip),
        )
        .unwrap_err();
        assert!(matches!(e, OAuthError::InvalidConfig(_)), "{ip}: {e}");
    }
    // A malformed spelling is refused.
    let e = CallbackListener::bind_with(
        &state,
        CallbackBind::new(CallbackOptions::default())
            .custom_bind_address(lo)
            .redirect_host("bad host/x"),
    )
    .unwrap_err();
    assert!(matches!(e, OAuthError::InvalidConfig(_)));
}

#[test]
fn post_json_ignores_strays_and_returns_only_the_valid_post() {
    let state = pkce::generate_state().unwrap();
    let l = listener(&state);
    let port = l.port();
    let handle = spawn_post(l, Duration::from_secs(10));

    // A connection that never sends anything must not block the flow.
    let _stalled = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();

    let good = json!({"apiKey": "key-SECRET", "state": state, "userId": "u1"}).to_string();

    // Wrong or missing origin: refused, nothing leaks.
    let r = post(port, "/callback", Some("https://evil.example"), &good);
    assert!(status(&r).contains("403"), "{r}");
    assert!(!lower(&r).contains("access-control-allow-origin"));
    assert!(status(&post(port, "/callback", None, &good)).contains("403"));
    // Same host, different scheme or port is a different origin.
    for other in ["http://provider.example", "https://provider.example:8443"] {
        let r = post(port, "/callback", Some(other), &good);
        assert!(status(&r).contains("403"), "{other}: {r}");
    }

    // Wrong path, wrong method.
    assert!(status(&post(port, "/other", Some(ORIGIN), &good)).contains("404"));
    let r = raw(
        port,
        b"GET /callback?code=x HTTP/1.1\r\nOrigin: https://provider.example\r\n\r\n",
    );
    assert!(status(&r).contains("405"), "{r}");
    assert!(status(&raw(port, b"\x01\x02\r\n\r\n")).contains("400"));

    // State: missing, wrong, near miss, wrong type, not an object, not JSON.
    let mut near = state.clone().into_bytes();
    near[0] = if near[0] == b'A' { b'B' } else { b'A' };
    let near = String::from_utf8(near).unwrap();
    for body in [
        json!({"apiKey": "key-SECRET"}).to_string(),
        json!({"apiKey": "key-SECRET", "state": "wrong"}).to_string(),
        json!({"apiKey": "key-SECRET", "state": near}).to_string(),
        json!({"apiKey": "key-SECRET", "state": 7}).to_string(),
        json!(["key-SECRET", state]).to_string(),
        "key-SECRET not json".to_string(),
        String::new(),
    ] {
        let r = post(port, "/callback", Some(ORIGIN), &body);
        assert!(status(&r).contains("400"), "{body}: {r}");
        assert!(!r.contains("key-SECRET"), "the reply echoed the body: {r}");
    }

    // Length-less, chunked and oversized requests are refused without being read.
    let r = raw(
        port,
        b"POST /callback HTTP/1.1\r\nOrigin: https://provider.example\r\n\r\n",
    );
    assert!(status(&r).contains("411"), "{r}");
    let r = raw(
        port,
        b"POST /callback HTTP/1.1\r\nOrigin: https://provider.example\r\nTransfer-Encoding: chunked\r\n\r\n",
    );
    assert!(status(&r).contains("400"), "{r}");
    let r = raw(
        port,
        format!(
            "POST /callback HTTP/1.1\r\nOrigin: {ORIGIN}\r\nContent-Length: {}\r\n\r\n",
            MAX_POST_BODY + 1
        )
        .as_bytes(),
    );
    assert!(status(&r).contains("413"), "{r}");

    assert!(!handle.is_finished(), "a stray request ended the flow");

    let ok = post(port, "/callback", Some(ORIGIN), &good);
    assert!(status(&ok).contains("200"), "{ok}");
    assert!(lower(&ok).contains(&format!("access-control-allow-origin: {ORIGIN}")));
    assert!(lower(&ok).contains("cache-control: no-store"));
    assert!(!ok.contains("key-SECRET"));
    let value = handle.join().unwrap().unwrap();
    assert_eq!(value["apiKey"], "key-SECRET");
    assert_eq!(value["userId"], "u1");

    // Single use: the socket is closed once the callback has been served.
    assert!(TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_err());
}

#[test]
fn preflight_is_answered_only_for_the_allowed_origin() {
    let state = pkce::generate_state().unwrap();
    let l = listener(&state);
    let port = l.port();
    let handle = spawn_post(l, Duration::from_secs(10));

    let r = raw(
        port,
        b"OPTIONS /callback HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: https://provider.example\r\nAccess-Control-Request-Method: POST\r\nAccess-Control-Request-Headers: content-type\r\nAccess-Control-Request-Private-Network: true\r\n\r\n",
    );
    let low = lower(&r);
    assert!(status(&r).contains("204"), "{r}");
    assert!(low.contains("access-control-allow-origin: https://provider.example"));
    assert!(low.contains("access-control-allow-methods: post, options"));
    assert!(low.contains("access-control-allow-headers: content-type"));
    assert!(low.contains("access-control-allow-private-network: true"));
    assert!(low.contains("vary: origin"));

    // No private-network request: no private-network grant. No requested headers: a default.
    let r = raw(
        port,
        b"OPTIONS /callback HTTP/1.1\r\nOrigin: https://provider.example\r\n\r\n",
    );
    let low = lower(&r);
    assert!(status(&r).contains("204"));
    assert!(!low.contains("allow-private-network"));
    assert!(low.contains("access-control-allow-headers: content-type"));

    // Another origin, or none: 403 and no CORS headers at all.
    for head in [
        "Origin: https://evil.example\r\n",
        "",
        "Origin: *\r\n",
        "Origin: null\r\n",
    ] {
        let r = raw(
            port,
            format!(
                "OPTIONS /callback HTTP/1.1\r\n{head}Access-Control-Request-Method: POST\r\n\r\n"
            )
            .as_bytes(),
        );
        assert!(status(&r).contains("403"), "{head:?}: {r}");
        assert!(!lower(&r).contains("access-control-allow"), "{head:?}");
    }
    // A preflight for another method is refused.
    let r = raw(
        port,
        b"OPTIONS /callback HTTP/1.1\r\nOrigin: https://provider.example\r\nAccess-Control-Request-Method: DELETE\r\n\r\n",
    );
    assert!(status(&r).contains("405"), "{r}");
    // A preflight on another path is a stray.
    let r = raw(
        port,
        b"OPTIONS /elsewhere HTTP/1.1\r\nOrigin: https://provider.example\r\n\r\n",
    );
    assert!(status(&r).contains("404"));

    assert!(!handle.is_finished(), "a preflight ended the flow");
    let good = json!({"state": state}).to_string();
    assert!(status(&post(port, "/callback", Some(ORIGIN), &good)).contains("200"));
    assert!(handle.join().unwrap().is_ok());
}

#[test]
fn request_split_across_writes_is_reassembled() {
    let state = pkce::generate_state().unwrap();
    let l = listener(&state);
    let port = l.port();
    let handle = spawn_post(l, Duration::from_secs(10));
    let body = json!({"state": state, "n": 1}).to_string();

    let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let head = format!(
        "POST /callback HTTP/1.1\r\nOrigin: {ORIGIN}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    s.write_all(&head.as_bytes()[..20]).unwrap();
    thread::sleep(Duration::from_millis(60));
    s.write_all(&head.as_bytes()[20..]).unwrap();
    thread::sleep(Duration::from_millis(60));
    let (first, second) = body.as_bytes().split_at(5);
    s.write_all(first).unwrap();
    thread::sleep(Duration::from_millis(60));
    s.write_all(second).unwrap();
    let mut reply = String::new();
    let _ = s.read_to_string(&mut reply);
    assert!(status(&reply).contains("200"), "{reply}");
    assert_eq!(handle.join().unwrap().unwrap()["n"], 1);
}

#[test]
fn post_json_validates_inputs_times_out_and_honours_cancel() {
    let state = pkce::generate_state().unwrap();
    for origin in [
        "*",
        "",
        "null",
        "https://",
        "https://provider.example/path",
        "ftp://provider.example",
        "provider.example",
        "https://pro vider.example",
        "https://user@provider.example",
        "https://provider.example\r\nX: y",
    ] {
        let e = listener(&state)
            .wait_post_json(Duration::from_secs(1), None, origin)
            .unwrap_err();
        assert!(matches!(e, OAuthError::InvalidConfig(_)), "{origin:?}: {e}");
    }
    for bad in [Duration::ZERO, Duration::from_secs(60 * 60 + 1)] {
        let e = listener(&state)
            .wait_post_json(bad, None, ORIGIN)
            .unwrap_err();
        assert!(matches!(e, OAuthError::InvalidConfig(_)));
    }
    // An origin with an explicit port is valid.
    let l = listener(&state);
    let e = l
        .wait_post_json(Duration::from_millis(50), None, "http://localhost:3000")
        .unwrap_err();
    assert!(matches!(e, OAuthError::Callback(m) if m.contains("timed out")));

    let token = CancelToken::new();
    token.cancel();
    let e = listener(&state)
        .wait_post_json(Duration::from_secs(10), Some(&token), ORIGIN)
        .unwrap_err();
    assert!(matches!(e, OAuthError::Cancelled));
}
