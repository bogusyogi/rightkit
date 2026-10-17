//! Extra token-endpoint parameters (RFC 8707 `resource`, `scope`), `wait_code`, RFC 7009
//! revocation and the in-memory `TokenStore`, against an in-process mock authorization server.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use rightkit_http::testing::{loopback_config, MockResponse, MockServer};
use rightkit_http::{
    percent_decode, percent_encode, CancelToken, Client, ClientConfig, RetryPolicy, Secret,
};
use rightkit_oauth::{
    pkce, MemoryTokenStore, OAuthClient, OAuthConfig, OAuthError, TokenSet, TokenStore,
    TokenTypeHint,
};
use serde_json::json;

fn client() -> Client {
    Client::new(ClientConfig {
        retry: RetryPolicy::none(),
        ..loopback_config()
    })
}

fn config(server: &MockServer) -> OAuthConfig {
    OAuthConfig::new(
        "desktop-client",
        server.url("/authorize"),
        server.url("/token"),
    )
}

fn port_of(redirect_uri: &str) -> u16 {
    redirect_uri
        .rsplit(':')
        .next()
        .unwrap()
        .split('/')
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

/// One GET to the loopback listener; returns the reply.
fn get(port: u16, target: &str) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write!(
        s,
        "GET {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out
}

fn query_param(url: &str, name: &str) -> String {
    let query = url.split_once('?').unwrap().1;
    query
        .split('&')
        .filter_map(|p| p.split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| percent_decode(v))
        .unwrap()
}

fn has(form: &[(String, String)], name: &str, value: &str) -> bool {
    form.iter().any(|(k, v)| k == name && v == value)
}

fn values<'a>(form: &'a [(String, String)], name: &str) -> Vec<&'a str> {
    form.iter()
        .filter(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
        .collect()
}

fn token_ok() -> MockResponse {
    MockResponse::json(
        200,
        &json!({
            "access_token": "access-1", "token_type": "Bearer", "expires_in": 3600,
            "refresh_token": "refresh-1"
        }),
    )
}

#[test]
fn token_params_ride_on_code_exchange_and_refresh_but_not_the_authorization_url() {
    let server = MockServer::start(|request, _| {
        let form = request.form();
        // The client's own fields are never displaced or duplicated.
        assert_eq!(values(&form, "client_id"), ["desktop-client"]);
        assert_eq!(values(&form, "grant_type").len(), 1);
        // Several values of one name stay in order (RFC 8707 allows repeated `resource`).
        assert_eq!(
            values(&form, "resource"),
            ["https://mcp.example/a", "https://mcp.example/b"]
        );
        assert_eq!(values(&form, "scope"), ["mcp.read"]);
        token_ok()
    });
    let oauth = OAuthClient::new(config(&server), client())
        .unwrap()
        .with_token_param("resource", "https://mcp.example/a")
        .unwrap()
        .with_token_param("resource", "https://mcp.example/b")
        .unwrap()
        .with_token_param("scope", "mcp.read")
        .unwrap();

    let flow = oauth.begin_loopback().unwrap();
    assert!(!flow.request.authorization_url.contains("resource="));
    let port = port_of(&flow.request.redirect_uri);
    let state = percent_encode(&flow.request.state);
    let join = thread::spawn(move || {
        get(port, &format!("/oauth/callback?code=code-1&state={state}"));
    });
    let tokens = oauth.finish_loopback(flow, Duration::from_secs(5)).unwrap();
    join.join().unwrap();
    assert_eq!(tokens.access_token(), "access-1");

    oauth.refresh(&tokens).unwrap();
    assert_eq!(server.request_count(), 2);
}

#[test]
fn token_params_never_override_client_fields_and_must_be_well_formed() {
    let server = MockServer::start(|_, _| token_ok());
    for name in [
        "code_verifier",
        "redirect_uri",
        "grant_type",
        "client_id",
        "client_secret",
        "refresh_token",
        "code",
        "Code_Verifier",
        "token",
        "token_type_hint",
    ] {
        let e = OAuthClient::new(config(&server), client())
            .unwrap()
            .with_token_param(name, "x")
            .err()
            .unwrap_or_else(|| panic!("{name} accepted"));
        assert!(matches!(e, OAuthError::InvalidConfig(_)), "{name}: {e}");
    }
    for name in ["", "has space", "new\nline"] {
        let oauth = OAuthClient::new(config(&server), client()).unwrap();
        assert!(oauth.with_token_param(name, "x").is_err(), "{name:?}");
    }
    // Plain HTTP to a non-loopback host, and userinfo that hides a remote host behind a
    // loopback-looking prefix, are refused for the revocation endpoint.
    for url in [
        "http://plain.example/revoke",
        "http://localhost:80@attacker.example/revoke",
        "http://127.0.0.1:80@attacker.example/revoke",
        "https://user:pw@provider.example/revoke",
        "http://localhost.attacker.example:80/revoke",
    ] {
        let oauth = OAuthClient::new(config(&server), client()).unwrap();
        let e = oauth.with_revocation_url(url).err().unwrap();
        assert!(matches!(e, OAuthError::InvalidConfig(_)), "{url}: {e}");
    }
    // The same rules apply to the configured endpoints.
    for url in [
        "http://localhost:80@attacker.example/token",
        "http://[::1]:80@attacker.example/token",
    ] {
        let c = OAuthConfig::new("c", server.url("/authorize"), url);
        assert!(OAuthClient::new(c, client()).is_err(), "{url}");
        let c = OAuthConfig::new("c", url, server.url("/token"));
        assert!(OAuthClient::new(c, client()).is_err(), "{url}");
    }
    assert_eq!(server.request_count(), 0);
}

#[test]
fn wait_code_returns_the_bare_code_and_the_request_without_exchanging() {
    let server = MockServer::start(|_, _| token_ok());
    let oauth = OAuthClient::new(config(&server), client()).unwrap();
    let flow = oauth.begin_loopback().unwrap();
    let port = port_of(&flow.request.redirect_uri);
    let state = flow.request.state.clone();
    let challenge = query_param(&flow.request.authorization_url, "code_challenge");
    let handle = thread::spawn(move || flow.wait_code(Duration::from_secs(10), None));

    // Strays do not end the flow.
    get(port, "/favicon.ico");
    get(
        port,
        &format!(
            "/oauth/callback?code=code-9&state={}",
            percent_encode(&state)
        ),
    );
    let (code, request) = handle.join().unwrap().unwrap();
    assert_eq!(code.expose(), "code-9");
    assert_eq!(request.state, state);
    // The verifier the caller gets is the one bound to the challenge in the URL.
    assert_eq!(
        pkce::challenge_s256(request.code_verifier()).unwrap(),
        challenge
    );
    assert_eq!(server.request_count(), 0, "wait_code never exchanges");
    assert!(!format!("{request:?}").contains(request.code_verifier()));
}

#[test]
fn wait_code_rejects_a_bad_state_and_a_denied_consent_and_honours_cancel() {
    let server = MockServer::start(|_, _| token_ok());
    let oauth = OAuthClient::new(config(&server), client()).unwrap();

    // Wrong state: StateMismatch, and the code is never returned.
    let flow = oauth.begin_loopback().unwrap();
    let port = port_of(&flow.request.redirect_uri);
    let handle = thread::spawn(move || flow.wait_code(Duration::from_secs(10), None));
    get(port, "/oauth/callback?code=stolen&state=wrong");
    assert!(matches!(
        handle.join().unwrap(),
        Err(OAuthError::StateMismatch)
    ));

    // Missing state is a mismatch too.
    let flow = oauth.begin_loopback().unwrap();
    let port = port_of(&flow.request.redirect_uri);
    let handle = thread::spawn(move || flow.wait_code(Duration::from_secs(10), None));
    get(port, "/oauth/callback?code=stolen");
    assert!(matches!(
        handle.join().unwrap(),
        Err(OAuthError::StateMismatch)
    ));

    // access_denied with the right state: sign in again.
    let flow = oauth.begin_loopback().unwrap();
    let port = port_of(&flow.request.redirect_uri);
    let state = percent_encode(&flow.request.state);
    let handle = thread::spawn(move || flow.wait_code(Duration::from_secs(10), None));
    get(
        port,
        &format!("/oauth/callback?error=access_denied&state={state}"),
    );
    assert!(matches!(
        handle.join().unwrap(),
        Err(OAuthError::NeedsReauth)
    ));

    // Timeout and cancel.
    let flow = oauth.begin_loopback().unwrap();
    let e = flow.wait_code(Duration::from_millis(50), None).unwrap_err();
    assert!(matches!(e, OAuthError::Callback(m) if m.contains("timed out")));
    let flow = oauth.begin_loopback().unwrap();
    let token = CancelToken::new();
    token.cancel();
    let e = flow
        .wait_code(Duration::from_secs(10), Some(&token))
        .unwrap_err();
    assert!(matches!(e, OAuthError::Cancelled));
    assert_eq!(server.request_count(), 0);
}

#[test]
fn a_code_verifier_from_another_flow_is_rejected_by_the_provider() {
    let expected = Arc::new(Mutex::new(String::new()));
    let seen = expected.clone();
    let server = MockServer::start(move |request, _| {
        let form = request.form();
        let verifier = values(&form, "code_verifier")[0].to_string();
        let challenge = pkce::challenge_s256(&verifier).unwrap();
        if challenge == *seen.lock().unwrap() {
            token_ok()
        } else {
            MockResponse::json(400, &json!({"error": "invalid_grant"}))
        }
    });
    let oauth = OAuthClient::new(config(&server), client()).unwrap();
    let a = oauth.begin_loopback().unwrap();
    let b = oauth.begin_loopback().unwrap();
    *expected.lock().unwrap() = query_param(&a.request.authorization_url, "code_challenge");

    // Flow B's verifier does not hash to flow A's challenge.
    let e = oauth.exchange_code(&b.request, "code").unwrap_err();
    assert!(matches!(e, OAuthError::NeedsReauth), "{e}");
    assert!(oauth.exchange_code(&a.request, "code").is_ok());
}

#[test]
fn revoke_posts_token_hint_client_id_and_resource() {
    let server = MockServer::start(|request, _| {
        assert_eq!(request.method, "POST");
        assert_eq!(request.path(), "/revoke");
        let form = request.form();
        assert!(has(&form, "token", "refresh-SECRET"));
        assert!(has(&form, "token_type_hint", "refresh_token"));
        assert!(has(&form, "client_id", "desktop-client"));
        assert!(has(&form, "client_secret", "shh"));
        assert!(has(&form, "resource", "https://mcp.example/a"));
        MockResponse::new(200)
    });
    let oauth = OAuthClient::new(config(&server).with_client_secret("shh"), client())
        .unwrap()
        .with_token_param("resource", "https://mcp.example/a")
        .unwrap()
        .with_revocation_url(server.url("/revoke"))
        .unwrap();
    oauth
        .revoke(
            &Secret::new("refresh-SECRET"),
            Some(TokenTypeHint::RefreshToken),
        )
        .unwrap();
    assert_eq!(server.request_count(), 1);

    // No hint: the field is simply absent.
    let server = MockServer::start(|request, _| {
        assert!(request.form().iter().all(|(k, _)| k != "token_type_hint"));
        MockResponse::new(200)
    });
    let oauth = OAuthClient::new(config(&server), client())
        .unwrap()
        .with_revocation_url(server.url("/revoke"))
        .unwrap();
    oauth.revoke(&Secret::new("a-1"), None).unwrap();
    assert_eq!(server.request_count(), 1);
}

#[test]
fn revoke_needs_a_configured_endpoint_and_scrubs_the_token_from_errors() {
    let server = MockServer::start(|_, _| token_ok());
    let oauth = OAuthClient::new(config(&server), client()).unwrap();
    let e = oauth.revoke(&Secret::new("t"), None).unwrap_err();
    assert!(matches!(e, OAuthError::InvalidConfig(_)));
    assert_eq!(
        server.request_count(),
        0,
        "nothing is sent without an endpoint"
    );

    let server = MockServer::start(|_, _| {
        MockResponse::json(
            400,
            &json!({
                "error": "unsupported_token_type",
                "error_description": "cannot revoke tok-SECRET-1"
            }),
        )
    });
    let oauth = OAuthClient::new(config(&server), client())
        .unwrap()
        .with_revocation_url(server.url("/revoke"))
        .unwrap();
    assert!(matches!(
        oauth.revoke(&Secret::new(""), None),
        Err(OAuthError::InvalidConfig(_))
    ));
    let e = oauth
        .revoke(
            &Secret::new("tok-SECRET-1"),
            Some(TokenTypeHint::AccessToken),
        )
        .unwrap_err();
    let text = format!("{e} {e:?}");
    assert!(
        matches!(&e, OAuthError::Provider { error, .. } if error == "unsupported_token_type"),
        "{text}"
    );
    assert!(!text.contains("tok-SECRET-1"), "{text}");

    // A provider that reports its error with HTTP 200 is still an error.
    let server = MockServer::start(|_, _| {
        MockResponse::json(200, &json!({"error": "invalid_client tok-SECRET-1"}))
    });
    let oauth = OAuthClient::new(config(&server), client())
        .unwrap()
        .with_revocation_url(server.url("/revoke"))
        .unwrap();
    let e = oauth
        .revoke(&Secret::new("tok-SECRET-1"), None)
        .unwrap_err();
    assert!(!format!("{e} {e:?}").contains("tok-SECRET-1"));
}

fn tokens(access: &str, refresh: Option<&str>, expires_in: u64, age: Duration) -> TokenSet {
    TokenSet {
        access_token: Secret::new(access),
        token_type: "Bearer".into(),
        expires_in: Some(Duration::from_secs(expires_in)),
        refresh_token: refresh.map(Secret::new),
        scope: None,
        id_token: None,
        obtained_at: SystemTime::now() - age,
    }
}

#[test]
fn memory_token_store_round_trips_isolates_accounts_and_redacts_debug() {
    let store = MemoryTokenStore::new();
    assert!(store.load("a").unwrap().is_none());
    store
        .save(
            "a",
            &tokens("access-SECRET", Some("refresh-SECRET"), 60, Duration::ZERO),
        )
        .unwrap();
    store
        .save("b", &tokens("other", None, 60, Duration::ZERO))
        .unwrap();
    let a = store.load("a").unwrap().unwrap();
    assert_eq!(a.access_token(), "access-SECRET");
    assert_eq!(a.refresh_token.unwrap().expose(), "refresh-SECRET");
    assert_eq!(store.load("b").unwrap().unwrap().access_token(), "other");

    // Save replaces.
    store
        .save("a", &tokens("newer", None, 60, Duration::ZERO))
        .unwrap();
    assert_eq!(store.load("a").unwrap().unwrap().access_token(), "newer");

    let debug = format!("{store:?}");
    assert!(debug.contains("accounts"), "{debug}");
    assert!(
        !debug.contains("newer") && !debug.contains("SECRET"),
        "{debug}"
    );

    store.delete("a").unwrap();
    assert!(store.load("a").unwrap().is_none());
    assert!(store.load("b").unwrap().is_some(), "delete is per account");
    // Deleting an absent account is not an error.
    store.delete("a").unwrap();
    store.delete("never-existed").unwrap();
}

#[test]
fn memory_token_store_is_shared_safely_between_threads() {
    let store = Arc::new(MemoryTokenStore::new());
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let store = store.clone();
            thread::spawn(move || {
                let account = format!("acct-{i}");
                store
                    .save(&account, &tokens("t", None, 60, Duration::ZERO))
                    .unwrap();
                assert!(store.load(&account).unwrap().is_some());
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    for i in 0..8 {
        assert!(store.load(&format!("acct-{i}")).unwrap().is_some());
    }
}

#[cfg(feature = "secrets")]
mod resolver {
    use super::*;
    use rightkit_oauth::credentials::{Resolution, Resolver, Source};

    fn resolver(
        server: &MockServer,
        store: Arc<dyn TokenStore>,
        dir: &std::path::Path,
    ) -> Resolver {
        let oauth = OAuthClient::new(config(server), client()).unwrap();
        Resolver::new(oauth, store, "acct").lock_dir(dir)
    }

    #[test]
    fn resolver_runs_on_the_memory_store_with_refresh_and_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let server = MockServer::start(|request, _| {
            assert!(has(&request.form(), "refresh_token", "r0"));
            MockResponse::json(
                200,
                &json!({"access_token": "fresh", "expires_in": 3600, "refresh_token": "r1"}),
            )
        });
        let store: Arc<dyn TokenStore> = Arc::new(MemoryTokenStore::new());
        let r = resolver(&server, store.clone(), dir.path());

        // Nothing stored.
        assert!(matches!(
            r.resolve().unwrap(),
            Resolution::NeedsSignIn { device: false }
        ));

        // A valid token is used as stored, no request.
        store
            .save("acct", &tokens("valid", Some("r0"), 3600, Duration::ZERO))
            .unwrap();
        match r.resolve().unwrap() {
            Resolution::Token { token, source } => {
                assert_eq!(token.expose(), "valid");
                assert_eq!(source, Source::Stored);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(server.request_count(), 0);

        // An expired token is refreshed and the rotated grant is written back.
        store
            .save(
                "acct",
                &tokens("stale", Some("r0"), 60, Duration::from_secs(3600)),
            )
            .unwrap();
        match r.resolve().unwrap() {
            Resolution::Token { token, source } => {
                assert_eq!(token.expose(), "fresh");
                assert_eq!(source, Source::Refreshed);
            }
            other => panic!("{other:?}"),
        }
        let saved = store.load("acct").unwrap().unwrap();
        assert_eq!(saved.refresh_token.unwrap().expose(), "r1");
        assert_eq!(server.request_count(), 1);
    }

    #[test]
    fn an_expired_token_without_a_refresh_grant_asks_for_sign_in() {
        let dir = tempfile::tempdir().unwrap();
        let server = MockServer::start(|_, _| token_ok());
        let store: Arc<dyn TokenStore> = Arc::new(MemoryTokenStore::new());
        store
            .save(
                "acct",
                &tokens("stale", None, 60, Duration::from_secs(3600)),
            )
            .unwrap();
        let r = resolver(&server, store.clone(), dir.path());
        assert!(matches!(
            r.resolve().unwrap(),
            Resolution::NeedsSignIn { .. }
        ));
        assert!(
            store.load("acct").unwrap().is_none(),
            "dead sign-in removed"
        );
        assert_eq!(server.request_count(), 0);
    }
}
