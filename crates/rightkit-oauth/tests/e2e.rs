use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use rightkit_http::testing::{MockResponse, MockServer};
use rightkit_http::{Client, ClientConfig, RetryPolicy};
use rightkit_oauth::{OAuthClient, OAuthConfig, OAuthError};

fn client() -> Client {
    Client::new(ClientConfig { retry: RetryPolicy::none(), ..ClientConfig::default() })
}

#[test]
fn google_preset_uses_offline_pkce_consent_defaults() {
    let config = OAuthConfig::google("client", ["openid", "email"]);
    let oauth = OAuthClient::new(config, client()).unwrap();
    let flow = oauth.begin_loopback().unwrap();
    assert!(flow.request.authorization_url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
    assert!(flow.request.authorization_url.contains("access_type=offline"));
    assert!(flow.request.authorization_url.contains("prompt=consent"));
    assert!(flow.request.authorization_url.contains("code_challenge_method=S256"));
}

#[test]
fn loopback_pkce_state_and_exact_token_exchange() {
    let token_server = MockServer::start(|request, _| {
        assert_eq!(request.method, "POST");
        assert_eq!(request.path(), "/token");
        let form = request.form();
        assert!(form.iter().any(|(k, v)| k == "grant_type" && v == "authorization_code"));
        assert!(form.iter().any(|(k, v)| k == "code" && v == "code-123"));
        assert!(form.iter().any(|(k, v)| k == "code_verifier" && !v.is_empty()));
        assert!(form.iter().any(|(k, v)| k == "redirect_uri" && v.starts_with("http://127.0.0.1:")));
        MockResponse::json(200, &serde_json::json!({
            "access_token": "access-abc", "token_type": "Bearer", "expires_in": 3600,
            "refresh_token": "refresh-abc", "scope": "mail.read"
        }))
    });
    let config = OAuthConfig::new("desktop-client", token_server.url("/authorize"), token_server.url("/token"))
        .with_scope("mail.read");
    let oauth = Arc::new(OAuthClient::new(config, client()).unwrap());
    let flow = oauth.begin_loopback().unwrap();
    assert!(flow.request.authorization_url.contains("code_challenge_method=S256"));
    assert!(flow.request.authorization_url.contains("scope=mail.read"));
    let callback_url = flow.request.redirect_uri.clone();
    let state = flow.request.state.clone();
    let callback_port = callback_url.rsplit(':').next().unwrap().split('/').next().unwrap().parse::<u16>().unwrap();
    let join = std::thread::spawn(move || {
        let mut stream = TcpStream::connect(("127.0.0.1", callback_port)).unwrap();
        write!(stream, "GET /oauth/callback?code=code-123&state={} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n", rightkit_http::percent_encode(&state)).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.contains("Authorization complete"));
    });
    let tokens = oauth.finish_loopback(flow, Duration::from_secs(2)).unwrap();
    join.join().unwrap();
    assert_eq!(tokens.access_token(), "access-abc");
    assert_eq!(tokens.refresh_token.as_ref().unwrap().expose(), "refresh-abc");
    assert_eq!(token_server.request_count(), 1);
}

#[test]
fn state_mismatch_is_rejected_before_token_request() {
    let token_server = MockServer::start(|_, _| MockResponse::json(200, &serde_json::json!({"access_token": "never"})));
    let config = OAuthConfig::new("client", token_server.url("/authorize"), token_server.url("/token"));
    let oauth = Arc::new(OAuthClient::new(config, client()).unwrap());
    let flow = oauth.begin_loopback().unwrap();
    let callback_port = flow.request.redirect_uri.rsplit(':').next().unwrap().split('/').next().unwrap().parse::<u16>().unwrap();
    let join = std::thread::spawn(move || {
        let mut stream = TcpStream::connect(("127.0.0.1", callback_port)).unwrap();
        stream.write_all(b"GET /oauth/callback?code=x&state=wrong HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
    });
    assert!(matches!(oauth.finish_loopback(flow, Duration::from_secs(2)), Err(OAuthError::StateMismatch)));
    join.join().unwrap();
    assert_eq!(token_server.request_count(), 0);
}

#[test]
fn refresh_preserves_old_refresh_token_and_invalid_grant_requires_reauth() {
    let calls = std::sync::Arc::new(std::sync::Mutex::new(0usize));
    let calls2 = calls.clone();
    let token_server = MockServer::start(move |_, _| {
        let mut count = calls2.lock().unwrap();
        *count += 1;
        if *count == 1 {
            MockResponse::json(200, &serde_json::json!({"access_token": "new-access", "expires_in": 600}))
        } else {
            MockResponse::json(400, &serde_json::json!({"error": "invalid_grant", "error_description": "revoked"}))
        }
    });
    let config = OAuthConfig::new("client", token_server.url("/authorize"), token_server.url("/token"));
    let oauth = OAuthClient::new(config, client()).unwrap();
    let current = rightkit_oauth::TokenSet {
        access_token: rightkit_http::Secret::new("old-access"), token_type: "Bearer".into(),
        expires_in: Some(Duration::from_secs(1)), refresh_token: Some(rightkit_http::Secret::new("keep-me")),
        scope: None, obtained_at: std::time::SystemTime::now(),
    };
    let refreshed = oauth.refresh(&current).unwrap();
    assert_eq!(refreshed.access_token(), "new-access");
    assert_eq!(refreshed.refresh_token.as_ref().unwrap().expose(), "keep-me");
    assert!(matches!(oauth.refresh(&refreshed), Err(OAuthError::NeedsReauth)));
}

#[test]
fn slow_loris_oversized_and_debug_redaction() {
    let oauth = OAuthClient::new(
        OAuthConfig::new("c", "http://127.0.0.1:1/authorize", "http://127.0.0.1:1/token"),
        client(),
    )
    .unwrap();
    let port_of = |uri: &str| uri.rsplit(':').next().unwrap().split('/').next().unwrap().parse::<u16>().unwrap();

    // Debug output never carries the PKCE verifier.
    let flow = oauth.begin_loopback().unwrap();
    let verifier = flow.request.code_verifier().to_string();
    assert!(verifier.len() >= 43);
    let dbg = format!("{:?} {:?}", flow, flow.request);
    assert!(!dbg.contains(&verifier), "verifier leaked into Debug: {dbg}");
    assert!(dbg.contains("redacted"));
    drop(flow);

    // Slow-loris: bytes trickle in, newline never arrives; the absolute deadline fires.
    let flow = oauth.begin_loopback().unwrap();
    let port = port_of(&flow.request.redirect_uri);
    let drip = std::thread::spawn(move || {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        for _ in 0..40 {
            if s.write_all(b"G").is_err() { break; }
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    let started = std::time::Instant::now();
    let err = oauth.finish_loopback(flow, Duration::from_secs(1)).unwrap_err();
    let elapsed = started.elapsed();
    drip.join().unwrap();
    assert!(matches!(&err, OAuthError::Callback(m) if m.contains("timed out")), "{err:?}");
    assert!(elapsed < Duration::from_millis(2500), "deadline must be absolute, took {elapsed:?}");

    // Oversized request line (no newline within the cap) is rejected.
    let flow = oauth.begin_loopback().unwrap();
    let port = port_of(&flow.request.redirect_uri);
    let big = std::thread::spawn(move || {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let _ = s.write_all(format!("GET /oauth/callback?code={} HTTP/1.1\r\n\r\n", "a".repeat(64 * 1024)).as_bytes());
        let mut sink = Vec::new();
        let _ = s.read_to_end(&mut sink);
    });
    let err = oauth.finish_loopback(flow, Duration::from_secs(3)).unwrap_err();
    big.join().unwrap();
    assert!(matches!(&err, OAuthError::Callback(m) if m.contains("too long")), "{err:?}");
}
