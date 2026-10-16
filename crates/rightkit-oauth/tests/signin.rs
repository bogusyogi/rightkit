//! Device sign-in, presets, and credential resolution against a local mock authorization
//! server. No network, no OS credential store (memory backend), no real waiting.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use rightkit_http::testing::{loopback_config, MockResponse, MockServer};
use rightkit_http::{Client, ClientConfig, RetryPolicy, Secret};
use rightkit_oauth::credentials::{Resolution, Resolver, SecretTokenStore, Source};
use rightkit_oauth::{
    Clock, DeviceWait, OAuthClient, OAuthConfig, OAuthError, TokenSet, TokenStore,
};
use serde_json::json;

/// The default address policy refuses the `127.0.0.1` mock server, so the client starts from the
/// crate's loopback test configuration.
fn http() -> Client {
    Client::new(ClientConfig {
        retry: RetryPolicy::none(),
        ..loopback_config()
    })
}

/// Sleeping advances time instead of waiting.
#[derive(Default)]
struct VirtualClock(AtomicU64);

impl Clock for VirtualClock {
    fn now(&self) -> Duration {
        Duration::from_millis(self.0.load(Ordering::SeqCst))
    }
    fn sleep(&self, d: Duration) {
        self.0.fetch_add(d.as_millis() as u64, Ordering::SeqCst);
    }
}

fn virtual_wait() -> (DeviceWait, Arc<VirtualClock>) {
    let clock = Arc::new(VirtualClock::default());
    (
        DeviceWait {
            cancel: Arc::new(AtomicBool::new(false)),
            clock: clock.clone(),
        },
        clock,
    )
}

/// Device endpoint at `/device`, token endpoint at `/token` answering with `polls` in order
/// (GitHub style: pending states as HTTP 200 with an `error`).
fn device_server(polls: Vec<MockResponse>) -> MockServer {
    let polls = Mutex::new(polls.into_iter());
    MockServer::start(move |request, _| match request.path() {
        "/device" => {
            assert_eq!(request.header("accept"), Some("application/json"));
            MockResponse::json(
                200,
                &json!({
                    "device_code": "dev-code-1", "user_code": "WDJB-MJHT",
                    "verification_uri": "https://example.test/device",
                    "expires_in": 900, "interval": 5
                }),
            )
        }
        "/token" => {
            let form = request.form();
            assert!(form
                .iter()
                .any(|(k, v)| k == "grant_type"
                    && v == "urn:ietf:params:oauth:grant-type:device_code"));
            assert!(form
                .iter()
                .any(|(k, v)| k == "device_code" && v == "dev-code-1"));
            polls
                .lock()
                .unwrap()
                .next()
                .expect("polled more than scripted")
        }
        other => panic!("unexpected path {other}"),
    })
}

fn device_client(server: &MockServer) -> OAuthClient {
    let config = OAuthConfig::new("app-client", server.url("/authorize"), server.url("/token"))
        .with_device_authorization_url(server.url("/device"));
    OAuthClient::new(config, http()).unwrap()
}

#[test]
fn device_flow_waits_out_pending_and_slow_down_then_returns_tokens() {
    let server = device_server(vec![
        MockResponse::json(200, &json!({"error": "authorization_pending"})),
        MockResponse::json(400, &json!({"error": "slow_down", "interval": 10})),
        MockResponse::json(
            200,
            &json!({
                "access_token": "gho_device", "token_type": "bearer", "expires_in": 28800,
                "refresh_token": "ghr_device"
            }),
        ),
    ]);
    let client = device_client(&server);
    let authorization = client.begin_device().unwrap();
    assert_eq!(authorization.user_code, "WDJB-MJHT");
    assert_eq!(
        authorization.verification_uri,
        "https://example.test/device"
    );
    assert!(
        !format!("{authorization:?}").contains("dev-code-1"),
        "device code is redacted"
    );

    let (wait, clock) = virtual_wait();
    let tokens = client.finish_device(&authorization, &wait).unwrap();
    assert_eq!(tokens.access_token(), "gho_device");
    // 5 s before the first poll, 5 s after pending, then the server's 10 s after slow_down.
    assert_eq!(clock.now(), Duration::from_secs(20));
}

#[test]
fn device_flow_reports_denial_expiry_and_cancellation() {
    let denied = device_server(vec![MockResponse::json(
        400,
        &json!({"error": "access_denied"}),
    )]);
    let client = device_client(&denied);
    let authorization = client.begin_device().unwrap();
    assert!(matches!(
        client.finish_device(&authorization, &virtual_wait().0),
        Err(OAuthError::AccessDenied)
    ));

    let expired = device_server(vec![MockResponse::json(
        400,
        &json!({"error": "expired_token"}),
    )]);
    let client = device_client(&expired);
    let authorization = client.begin_device().unwrap();
    assert!(matches!(
        client.finish_device(&authorization, &virtual_wait().0),
        Err(OAuthError::DeviceCodeExpired)
    ));

    let never = device_server(vec![]);
    let client = device_client(&never);
    let authorization = client.begin_device().unwrap();
    let (wait, _) = virtual_wait();
    wait.cancel.store(true, Ordering::SeqCst);
    assert!(matches!(
        client.finish_device(&authorization, &wait),
        Err(OAuthError::Cancelled)
    ));
    assert_eq!(
        never
            .requests()
            .iter()
            .filter(|r| r.path() == "/token")
            .count(),
        0
    );
}

#[test]
fn presets_carry_endpoints_and_the_apps_own_client_id() {
    let github = OAuthConfig::github("Iv1.app-own-id", Vec::<String>::new());
    assert_eq!(
        github.device_authorization_url.as_deref(),
        Some("https://github.com/login/device/code")
    );
    assert_eq!(
        github.token_url,
        "https://github.com/login/oauth/access_token"
    );
    assert_eq!(github.client_id, "Iv1.app-own-id");
    let google = OAuthConfig::google("g-id", ["https://www.googleapis.com/auth/gmail.readonly"]);
    assert!(google.device_authorization_url.is_some());
    assert!(OAuthConfig::cloudflare("cf", ["account:read"])
        .device_authorization_url
        .is_none());

    // A registration that fixes the redirect is honoured exactly.
    let openai =
        OAuthConfig::openai("app-id", ["openid"]).with_redirect("localhost", 0, "/auth/callback");
    let flow = OAuthClient::new(openai, http())
        .unwrap()
        .begin_loopback()
        .unwrap();
    assert!(flow.request.redirect_uri.starts_with("http://localhost:"));
    assert!(flow.request.redirect_uri.ends_with("/auth/callback"));
    assert!(flow
        .request
        .authorization_url
        .starts_with("https://auth.openai.com/oauth/authorize?"));

    let bad =
        OAuthConfig::github("x", Vec::<String>::new()).with_redirect("evil.example", 0, "/cb");
    assert!(matches!(
        OAuthClient::new(bad, http()),
        Err(OAuthError::InvalidConfig(_))
    ));
}

fn memory_store() -> Arc<dyn TokenStore> {
    let store = rightkit_credentials::Store::open(
        rightkit_credentials::Config::new(
            rightkit_credentials::Namespace::new("dev.rightkit.oauth.test").unwrap(),
        )
        .backend(Arc::new(rightkit_credentials::MemoryBackend::new())),
    );
    Arc::new(SecretTokenStore::new(store))
}

fn tokens(access: &str, renew: Option<&str>, expires_in: Option<u64>, age: Duration) -> TokenSet {
    TokenSet {
        access_token: Secret::new(access),
        token_type: "bearer".into(),
        expires_in: expires_in.map(Duration::from_secs),
        refresh_token: renew.map(Secret::new),
        scope: None,
        obtained_at: SystemTime::now() - age,
    }
}

fn resolver(
    server: &MockServer,
    store: Arc<dyn TokenStore>,
    lock_dir: &std::path::Path,
) -> Resolver {
    Resolver::new(device_client(server), store, "github").lock_dir(lock_dir)
}

#[test]
fn resolver_prefers_a_valid_stored_sign_in_and_refreshes_an_expired_one() {
    let lock_dir = tempfile::tempdir().unwrap();
    let server = MockServer::start(|request, _| {
        let form = request.form();
        assert!(form
            .iter()
            .any(|(k, v)| k == "grant_type" && v == "refresh_token"));
        // GitHub rotates the refresh grant on every use.
        MockResponse::json(
            200,
            &json!({
                "access_token": "fresh-access", "expires_in": 28800, "refresh_token": "rotated"
            }),
        )
    });
    let store = memory_store();
    let r = resolver(&server, store.clone(), lock_dir.path());

    store
        .save(
            "github",
            &tokens("valid", Some("r0"), Some(3600), Duration::ZERO),
        )
        .unwrap();
    match r.resolve().unwrap() {
        Resolution::Token { token, source } => {
            assert_eq!(token.expose(), "valid");
            assert_eq!(source, Source::Stored);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(server.request_count(), 0);

    store
        .save(
            "github",
            &tokens("stale", Some("r0"), Some(60), Duration::from_secs(3600)),
        )
        .unwrap();
    match r.resolve().unwrap() {
        Resolution::Token { token, source } => {
            assert_eq!(token.expose(), "fresh-access");
            assert_eq!(source, Source::Refreshed);
        }
        other => panic!("{other:?}"),
    }
    let saved = store.load("github").unwrap().unwrap();
    assert_eq!(
        saved.refresh_token.unwrap().expose(),
        "rotated",
        "the rotated grant is written back"
    );
    assert_eq!(server.request_count(), 1);
}

#[test]
fn a_dead_grant_clears_the_sign_in_and_asks_for_a_new_one() {
    let lock_dir = tempfile::tempdir().unwrap();
    // GitHub reports a dead refresh grant with HTTP 200.
    let server =
        MockServer::start(|_, _| MockResponse::json(200, &json!({"error": "bad_refresh_token"})));
    let store = memory_store();
    store
        .save(
            "github",
            &tokens("stale", Some("dead"), Some(60), Duration::from_secs(3600)),
        )
        .unwrap();
    let r = resolver(&server, store.clone(), lock_dir.path());
    assert!(matches!(
        r.resolve().unwrap(),
        Resolution::NeedsSignIn { device: true }
    ));
    assert!(
        store.load("github").unwrap().is_none(),
        "dead sign-in removed"
    );
}

#[test]
fn device_sign_in_through_the_resolver_is_stored_for_next_time() {
    let lock_dir = tempfile::tempdir().unwrap();
    let server = device_server(vec![MockResponse::json(
        200,
        &json!({
            "access_token": "signed-in", "expires_in": 28800, "refresh_token": "r1"
        }),
    )]);
    let store = memory_store();
    let r = resolver(&server, store.clone(), lock_dir.path());
    assert!(matches!(
        r.resolve().unwrap(),
        Resolution::NeedsSignIn { device: true }
    ));
    let mut shown = None;
    let token = r
        .sign_in_device(|a| shown = Some(a.user_code.clone()), &virtual_wait().0)
        .unwrap();
    assert_eq!(token.expose(), "signed-in");
    assert_eq!(shown.as_deref(), Some("WDJB-MJHT"));
    assert!(matches!(
        r.resolve().unwrap(),
        Resolution::Token {
            source: Source::Stored,
            ..
        }
    ));
}

#[test]
fn concurrent_refreshes_spend_the_grant_once() {
    let lock_dir = tempfile::tempdir().unwrap();
    let server = MockServer::start(|_, _| {
        std::thread::sleep(Duration::from_millis(150));
        MockResponse::json(
            200,
            &json!({"access_token": "once", "expires_in": 28800, "refresh_token": "next"}),
        )
    });
    let store = memory_store();
    store
        .save(
            "github",
            &tokens("stale", Some("r0"), Some(60), Duration::from_secs(3600)),
        )
        .unwrap();
    let r = Arc::new(resolver(&server, store, lock_dir.path()));
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let r = r.clone();
            std::thread::spawn(move || match r.resolve().unwrap() {
                Resolution::Token { token, .. } => token.expose().to_string(),
                other => panic!("{other:?}"),
            })
        })
        .collect();
    for h in handles {
        assert_eq!(h.join().unwrap(), "once");
    }
    assert_eq!(
        server.request_count(),
        1,
        "one refresh, the rest read the stored result"
    );
}
