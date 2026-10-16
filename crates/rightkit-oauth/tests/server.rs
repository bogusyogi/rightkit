#![cfg(feature = "server")]
use base64::Engine as _;
use rightkit_http::testing::{loopback_config, MockResponse, MockServer};
use rightkit_http::{Client, ClientConfig, Secret};
use rightkit_oauth::server::*;
use rightkit_oauth::{OAuthConfig, TokenSet};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Default)]
struct Clock(AtomicU64);
impl ServerClock for Clock {
    fn now(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(10000 + self.0.load(Ordering::SeqCst))
    }
}
fn provider(server: &MockServer) -> ServerProvider {
    ServerProvider::new(
        "test",
        OAuthConfig::new("client", server.url("/authorize"), server.url("/token"))
            .with_client_secret("client-secret"),
        "https://postright.orthiclabs.com/oauth/callback",
    )
}
fn flow(
    p: ServerProvider,
    pending: Arc<dyn PendingStateStore>,
    clock: Arc<Clock>,
) -> ServerAuthFlow {
    // The default address policy refuses the `127.0.0.1` mock server.
    ServerAuthFlow::new(
        Client::new(loopback_config()),
        [p],
        pending,
        Duration::from_secs(300),
    )
    .unwrap()
    .with_clock(clock)
}
fn callback(state: &str) -> CallbackQuery {
    CallbackQuery {
        state: Some(state.into()),
        code: Some("code-secret".into()),
        error: None,
    }
}
fn token_response() -> MockResponse {
    MockResponse::json(
        200,
        &json!({"access_token":"access-secret", "refresh_token":"refresh-secret", "expires_in":3600}),
    )
}
fn pairs(url: &str) -> std::collections::HashMap<String, String> {
    url::Url::parse(url)
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect()
}
fn tokens(expires: u64) -> TokenSet {
    TokenSet {
        access_token: Secret::new("old-access"),
        refresh_token: Some(Secret::new("old-refresh")),
        token_type: "Bearer".into(),
        scope: None,
        id_token: None,
        obtained_at: Clock::default().now(),
        expires_in: Some(Duration::from_secs(expires)),
    }
}

#[test]
fn state_pkce_exchange_and_single_use() {
    let server = MockServer::start(|_, _| token_response());
    let pending = Arc::new(MemoryPendingStateStore::default());
    let f = flow(provider(&server), pending, Arc::new(Clock::default()));
    let request = f
        .begin("test", "tenant:account", &["read".into(), "write".into()])
        .unwrap();
    let another = f.begin("test", "other", &[]).unwrap();
    assert_ne!(request.state, another.state);
    assert_eq!(request.state.len(), 43);
    assert!(!request.state.contains('='));
    let query = pairs(&request.authorize_url);
    assert_eq!(query["scope"], "read write");
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(query["state"], request.state);
    assert!(!query.contains_key("nonce"));
    let result = f.complete(callback(&request.state)).unwrap();
    assert_eq!(result.provider, "test");
    assert_eq!(result.account_hint, "tenant:account");
    assert_eq!(result.tokens.access_token(), "access-secret");
    let requests = server.requests();
    let form: std::collections::HashMap<_, _> = requests[0].form().into_iter().collect();
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(form["code"], "code-secret");
    assert_eq!(form["client_id"], "client");
    assert_eq!(form["client_secret"], "client-secret");
    assert_eq!(form["redirect_uri"], query["redirect_uri"]);
    let verifier = &form["code_verifier"];
    assert!((43..=128).contains(&verifier.len()));
    assert!(verifier
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b)));
    assert_eq!(
        query["code_challenge"],
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()))
    );
    assert!(matches!(
        f.complete(callback(&request.state)),
        Err(ServerAuthError::StateMismatch)
    ));
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn expiry_at_deadline_consumes_state_and_mismatch_does_not_spend_code() {
    let server = MockServer::start(|_, _| token_response());
    let clock = Arc::new(Clock::default());
    let f = flow(
        provider(&server),
        Arc::new(MemoryPendingStateStore::default()),
        clock.clone(),
    );
    let r = f.begin("test", "account", &[]).unwrap();
    assert!(matches!(
        f.complete(callback(&"x".repeat(43))),
        Err(ServerAuthError::StateMismatch)
    ));
    assert!(matches!(
        f.complete(CallbackQuery::default()),
        Err(ServerAuthError::StateMismatch)
    ));
    clock.0.store(300, Ordering::SeqCst);
    assert!(matches!(
        f.complete(callback(&r.state)),
        Err(ServerAuthError::StateExpired)
    ));
    assert!(matches!(
        f.complete(callback(&r.state)),
        Err(ServerAuthError::StateMismatch)
    ));
    assert!(server.requests().is_empty());
}

#[test]
fn provider_denial_requires_state_and_is_single_use() {
    let server = MockServer::start(|_, _| token_response());
    let f = flow(
        provider(&server),
        Arc::new(MemoryPendingStateStore::default()),
        Arc::new(Clock::default()),
    );
    let mut q = CallbackQuery {
        error: Some("echoed-token-secret".into()),
        state: None,
        code: None,
    };
    assert!(matches!(f.complete(q), Err(ServerAuthError::StateMismatch)));
    let r = f.begin("test", "account", &[]).unwrap();
    q = callback(&r.state);
    q.error = Some("echoed-token-secret".into());
    let e = f.complete(q).unwrap_err();
    assert!(matches!(e, ServerAuthError::ProviderError));
    assert!(!format!("{e:?} {e}").contains("echoed-token-secret"));
    assert!(matches!(
        f.complete(callback(&r.state)),
        Err(ServerAuthError::StateMismatch)
    ));
    assert!(server.requests().is_empty());
}

#[test]
fn failed_exchange_never_retries_and_consumes_state() {
    let server = MockServer::start(|_, _| {
        MockResponse::json(
            503,
            &json!({"error":"access-secret", "error_description":"refresh-secret"}),
        )
    });
    let f = flow(
        provider(&server),
        Arc::new(MemoryPendingStateStore::default()),
        Arc::new(Clock::default()),
    );
    let r = f.begin("test", "account", &[]).unwrap();
    let e = f.complete(callback(&r.state)).unwrap_err();
    assert!(matches!(
        e,
        ServerAuthError::ExchangeFailure { status: Some(503) }
    ));
    assert!(!format!("{e:?} {e}").contains("secret"));
    assert!(matches!(
        f.complete(callback(&r.state)),
        Err(ServerAuthError::StateMismatch)
    ));
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn basic_auth_and_unsupported_pkce() {
    let server = MockServer::start(|_, _| token_response());
    let mut p = provider(&server);
    p.client_auth = ClientAuth::SecretBasic;
    p.pkce = false;
    let f = flow(
        p,
        Arc::new(MemoryPendingStateStore::default()),
        Arc::new(Clock::default()),
    );
    let r = f.begin("test", "account", &[]).unwrap();
    assert!(!pairs(&r.authorize_url).contains_key("code_challenge"));
    f.complete(callback(&r.state)).unwrap();
    let requests = server.requests();
    assert_eq!(
        requests[0].header("authorization"),
        Some("Basic Y2xpZW50OmNsaWVudC1zZWNyZXQ=")
    );
    let form = requests[0].form();
    assert!(!form
        .iter()
        .any(|(k, _)| matches!(k.as_str(), "client_secret" | "client_id" | "code_verifier")));
}

#[test]
fn tiktok_uses_client_key_and_comma_scopes() {
    let server = MockServer::start(|_, _| token_response());
    let mut p = ServerProvider::tiktok("key", "secret", "https://example.test/callback");
    p.config.authorization_url = server.url("/authorize");
    p.config.token_url = server.url("/token");
    let f = flow(
        p,
        Arc::new(MemoryPendingStateStore::default()),
        Arc::new(Clock::default()),
    );
    let r = f
        .begin(
            "tiktok",
            "account",
            &["video.list".into(), "video.publish".into()],
        )
        .unwrap();
    let query = pairs(&r.authorize_url);
    assert_eq!(query["client_key"], "key");
    assert_eq!(query["scope"], "video.list,video.publish");
    f.complete(callback(&r.state)).unwrap();
    let form = server.requests()[0].form();
    assert!(form.iter().any(|(k, v)| k == "client_key" && v == "key"));
    assert!(!form.iter().any(|(k, _)| k == "client_id"));
}

struct Validator {
    calls: Mutex<Vec<String>>,
    reject: bool,
}
impl OidcValidator for Validator {
    fn validate(
        &self,
        _: &ServerProvider,
        token: &Secret,
        nonce: &str,
    ) -> Result<(), ServerAuthError> {
        assert_eq!(token.expose(), "id-secret");
        self.calls.lock().unwrap().push(nonce.into());
        if self.reject {
            Err(ServerAuthError::OidcValidation)
        } else {
            Ok(())
        }
    }
}
#[test]
fn oidc_nonce_is_unique_and_validator_required() {
    let server = MockServer::start(|_, _| {
        MockResponse::json(
            200,
            &json!({"access_token":"access-secret", "id_token":"id-secret"}),
        )
    });
    let mut p = provider(&server);
    p.oidc = true;
    let f = flow(
        p.clone(),
        Arc::new(MemoryPendingStateStore::default()),
        Arc::new(Clock::default()),
    );
    assert!(matches!(
        f.begin("test", "account", &["openid".into()]),
        Err(ServerAuthError::OidcValidation)
    ));
    let validator = Arc::new(Validator {
        calls: Mutex::new(vec![]),
        reject: false,
    });
    let f = f.with_oidc_validator(validator.clone());
    let a = f.begin("test", "account", &["openid".into()]).unwrap();
    let b = f.begin("test", "account", &["openid".into()]).unwrap();
    assert_ne!(
        pairs(&a.authorize_url)["nonce"],
        pairs(&b.authorize_url)["nonce"]
    );
    f.complete(callback(&a.state)).unwrap();
    assert_eq!(
        validator.calls.lock().unwrap()[0],
        pairs(&a.authorize_url)["nonce"]
    );
    let reject = flow(
        p,
        Arc::new(MemoryPendingStateStore::default()),
        Arc::new(Clock::default()),
    )
    .with_oidc_validator(Arc::new(Validator {
        calls: Mutex::new(vec![]),
        reject: true,
    }));
    let r = reject.begin("test", "account", &["openid".into()]).unwrap();
    assert!(matches!(
        reject.complete(callback(&r.state)),
        Err(ServerAuthError::OidcValidation)
    ));
}

#[test]
fn state_store_capacity_and_policy_binding() {
    let server = MockServer::start(|_, _| token_response());
    let pending = Arc::new(MemoryPendingStateStore::new(1));
    let clock = Arc::new(Clock::default());
    let f = flow(provider(&server), pending.clone(), clock.clone());
    let r = f.begin("test", "account", &[]).unwrap();
    assert!(matches!(
        f.begin("test", "account", &[]),
        Err(ServerAuthError::PendingCapacity)
    ));
    let mut changed = provider(&server);
    changed.redirect_uri = "https://example.test/different".into();
    let changed = flow(changed, pending, clock.clone());
    assert!(matches!(
        changed.complete(callback(&r.state)),
        Err(ServerAuthError::StateMismatch)
    ));
    f.begin("test", "account", &[]).unwrap();
    clock.0.store(301, Ordering::SeqCst);
    f.begin("test", "account", &[]).unwrap();
    assert!(server.requests().is_empty());
}

#[test]
fn rejects_insecure_redirect_and_reserved_query_parameters() {
    let server = MockServer::start(|_, _| token_response());
    for redirect in [
        "http://example.test/callback",
        "https://user:pass@example.test/callback",
        "https://example.test/callback#fragment",
    ] {
        let mut p = provider(&server);
        p.redirect_uri = redirect.into();
        assert!(ServerAuthFlow::new(
            Client::new(ClientConfig::default()),
            [p],
            Arc::new(MemoryPendingStateStore::default()),
            Duration::from_secs(300)
        )
        .is_err());
    }
    let mut p = provider(&server);
    p.config
        .authorization_params
        .push(("state".into(), "attacker".into()));
    assert!(ServerAuthFlow::new(
        Client::new(ClientConfig::default()),
        [p],
        Arc::new(MemoryPendingStateStore::default()),
        Duration::from_secs(300)
    )
    .is_err());
    let redirecting_http = Client::new(ClientConfig {
        max_redirects: 1,
        ..ClientConfig::default()
    });
    assert!(ServerAuthFlow::new(
        redirecting_http,
        [provider(&server)],
        Arc::new(MemoryPendingStateStore::default()),
        Duration::from_secs(300)
    )
    .is_err());
}

#[cfg(unix)]
#[test]
fn sealed_store_roundtrip_isolation_ciphertext_and_wrong_key() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tokens");
    let s = SealedFileTokenStore::new(&root, [7; 32]).unwrap();
    let t = tokens(3600);
    s.put("account", "test", &t).unwrap();
    let t2 = s.get("account", "test").unwrap().unwrap();
    assert_eq!(t2.access_token(), "old-access");
    assert_eq!(t2.refresh_token.unwrap().expose(), "old-refresh");
    assert_eq!(t2.obtained_at, t.obtained_at);
    assert!(s.get("account", "other").unwrap().is_none());
    assert!(s.get("other", "test").unwrap().is_none());
    let path = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().unwrap() == "sealed")
        .unwrap();
    let ciphertext = std::fs::read(&path).unwrap();
    assert!(!String::from_utf8_lossy(&ciphertext).contains("old-access"));
    assert!(!String::from_utf8_lossy(&ciphertext).contains("old-refresh"));
    let reopened = SealedFileTokenStore::new(&root, [7; 32]).unwrap();
    assert_eq!(
        reopened
            .get("account", "test")
            .unwrap()
            .unwrap()
            .access_token(),
        "old-access"
    );
    let wrong = SealedFileTokenStore::new(&root, [8; 32]).unwrap();
    assert!(matches!(
        wrong.get("account", "test"),
        Err(ServerAuthError::Storage)
    ));
    let mut altered = ciphertext.clone();
    let last = altered.len() - 1;
    altered[last] ^= 1;
    std::fs::write(path, altered).unwrap();
    assert!(matches!(
        s.get("account", "test"),
        Err(ServerAuthError::Storage)
    ));
    s.delete("account", "test").unwrap();
    s.delete("account", "test").unwrap();
    assert!(s.get("account", "test").unwrap().is_none());
}

#[cfg(unix)]
#[test]
fn sealed_store_refuses_symlinks_and_world_readable_directories() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tokens");
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(SealedFileTokenStore::new(&root, [7; 32]).is_err());
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let link = dir.path().join("link");
    symlink(&root, &link).unwrap();
    assert!(SealedFileTokenStore::new(&link, [7; 32]).is_err());
}

#[cfg(unix)]
#[test]
fn refresh_proactively_rotates_and_retains_refresh_when_omitted() {
    let server = MockServer::start(|request, n| {
        let form: std::collections::HashMap<_, _> = request.form().into_iter().collect();
        assert_eq!(form["grant_type"], "refresh_token");
        assert_eq!(
            form["refresh_token"],
            if n == 0 {
                "old-refresh"
            } else {
                "rotated-refresh"
            }
        );
        if n == 0 {
            MockResponse::json(
                200,
                &json!({"access_token":"new-access", "refresh_token":"rotated-refresh", "expires_in":3600}),
            )
        } else {
            MockResponse::json(
                200,
                &json!({"access_token":"newer-access", "expires_in":3600}),
            )
        }
    });
    let clock = Arc::new(Clock::default());
    let f = flow(
        provider(&server),
        Arc::new(MemoryPendingStateStore::default()),
        clock.clone(),
    );
    let dir = tempfile::tempdir().unwrap();
    let store = SealedFileTokenStore::new(dir.path().join("tokens"), [7; 32]).unwrap();
    store.put("account", "test", &tokens(90)).unwrap();
    let first = f
        .refresh(&store, "account", "test", Duration::from_secs(120))
        .unwrap();
    assert_eq!(first.access_token(), "new-access");
    assert_eq!(
        store
            .get("account", "test")
            .unwrap()
            .unwrap()
            .refresh_token
            .unwrap()
            .expose(),
        "rotated-refresh"
    );
    f.refresh(&store, "account", "test", Duration::from_secs(120))
        .unwrap();
    assert_eq!(server.requests().len(), 1);
    clock.0.store(3500, Ordering::SeqCst);
    let next = f
        .refresh(&store, "account", "test", Duration::from_secs(120))
        .unwrap();
    assert_eq!(next.refresh_token.unwrap().expose(), "rotated-refresh");
    assert_eq!(server.requests().len(), 2);
}

#[cfg(unix)]
#[test]
fn concurrent_refresh_across_store_instances_spends_one_grant() {
    let server = MockServer::start(|_, _| token_response());
    let f = Arc::new(flow(
        provider(&server),
        Arc::new(MemoryPendingStateStore::default()),
        Arc::new(Clock::default()),
    ));
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tokens");
    let a = SealedFileTokenStore::new(&root, [7; 32]).unwrap();
    let b = SealedFileTokenStore::new(&root, [7; 32]).unwrap();
    a.put("account", "test", &tokens(90)).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let mut handles = vec![];
    for store in [a, b] {
        let f = f.clone();
        let barrier = barrier.clone();
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            f.refresh(&store, "account", "test", Duration::from_secs(120))
                .unwrap()
        }));
    }
    for handle in handles {
        assert_eq!(handle.join().unwrap().access_token(), "access-secret");
    }
    assert_eq!(server.requests().len(), 1);
}

#[cfg(unix)]
#[test]
fn refresh_invalid_grant_reauth_and_transient_failure_preserves_tokens() {
    let server = MockServer::start(|_, n| {
        if n == 0 {
            MockResponse::text(503, "old-access")
        } else {
            MockResponse::json(400, &json!({"error":"invalid_grant"}))
        }
    });
    let f = flow(
        provider(&server),
        Arc::new(MemoryPendingStateStore::default()),
        Arc::new(Clock::default()),
    );
    let dir = tempfile::tempdir().unwrap();
    let store = SealedFileTokenStore::new(dir.path().join("tokens"), [7; 32]).unwrap();
    assert!(matches!(
        f.refresh(&store, "account", "test", Duration::ZERO),
        Err(ServerAuthError::ReauthRequired)
    ));
    store.put("account", "test", &tokens(0)).unwrap();
    assert!(matches!(
        f.refresh(&store, "account", "test", Duration::ZERO),
        Err(ServerAuthError::ExchangeFailure { .. })
    ));
    assert_eq!(
        store
            .get("account", "test")
            .unwrap()
            .unwrap()
            .access_token(),
        "old-access"
    );
    assert!(matches!(
        f.refresh(&store, "account", "test", Duration::ZERO),
        Err(ServerAuthError::ReauthRequired)
    ));
    assert!(store.get("account", "test").unwrap().is_none());
    let mut no_refresh = tokens(0);
    no_refresh.refresh_token = None;
    store.put("account", "test", &no_refresh).unwrap();
    assert!(matches!(
        f.refresh(&store, "account", "test", Duration::ZERO),
        Err(ServerAuthError::ReauthRequired)
    ));
}

#[test]
fn debug_redacts_tokens_codes_state_and_pending_verifier() {
    let pending = PendingAuthorization {
        state: "state-secret".into(),
        provider: "test".into(),
        account_hint: "account".into(),
        expires_at: Clock::default().now(),
        verifier: Some("verifier-secret".into()),
        nonce: Some("nonce-secret".into()),
        binding: "binding".into(),
    };
    let result = CompletedAuthorization {
        provider: "test".into(),
        account_hint: "account".into(),
        tokens: tokens(3600),
    };
    let error = ServerAuthError::RefreshPersistence {
        tokens: Box::new(tokens(3600)),
    };
    let request = ServerAuthorization {
        authorize_url: "url-secret".into(),
        state: "state-secret".into(),
    };
    let q = callback("state-secret");
    let mut t = tokens(3600);
    t.token_type = "old-access".into();
    t.scope = Some("old-refresh".into());
    let debug = format!("{pending:?} {result:?} {error:?} {error} {request:?} {q:?} {t:?}");
    for value in [
        "old-access",
        "old-refresh",
        "state-secret",
        "verifier-secret",
        "nonce-secret",
        "code-secret",
        "url-secret",
    ] {
        assert!(!debug.contains(value), "debug exposed a credential");
    }
}

struct FailedSaveStore;
struct FailedSaveGuard;
impl TokenStore for FailedSaveStore {
    fn lock(&self, _: &str, _: &str) -> Result<Box<dyn TokenStoreGuard + '_>, ServerAuthError> {
        Ok(Box::new(FailedSaveGuard))
    }
}
impl TokenStoreGuard for FailedSaveGuard {
    fn get(&mut self) -> Result<Option<TokenSet>, ServerAuthError> {
        Ok(Some(tokens(0)))
    }
    fn put(&mut self, _: &TokenSet) -> Result<(), ServerAuthError> {
        Err(ServerAuthError::Storage)
    }
    fn delete(&mut self) -> Result<(), ServerAuthError> {
        Ok(())
    }
}
#[test]
fn failed_rotation_save_returns_recoverable_redacted_tokens() {
    let server = MockServer::start(|_, _| token_response());
    let f = flow(
        provider(&server),
        Arc::new(MemoryPendingStateStore::default()),
        Arc::new(Clock::default()),
    );
    let e = f
        .refresh(&FailedSaveStore, "account", "test", Duration::ZERO)
        .unwrap_err();
    assert!(!format!("{e:?} {e}").contains("secret"));
    match e {
        ServerAuthError::RefreshPersistence { tokens } => {
            assert_eq!(
                tokens.refresh_token.as_ref().unwrap().expose(),
                "refresh-secret"
            );
            assert_eq!(tokens.access_token(), "access-secret");
        }
        other => panic!("unexpected error: {other}"),
    }
    assert_eq!(server.requests().len(), 1);
}

/// Exercises constant-time validation after lookup, not just absent-key handling.
struct CorruptPending(MemoryPendingStateStore);
impl PendingStateStore for CorruptPending {
    fn insert(
        &self,
        key: &str,
        mut pending: PendingAuthorization,
        now: SystemTime,
    ) -> Result<(), ServerAuthError> {
        pending.state = "x".repeat(43);
        self.0.insert(key, pending, now)
    }
    fn take(&self, key: &str) -> Result<Option<PendingAuthorization>, ServerAuthError> {
        self.0.take(key)
    }
}
#[test]
fn mismatched_record_state_fails_before_exchange() {
    let server = MockServer::start(|_, _| token_response());
    let f = flow(
        provider(&server),
        Arc::new(CorruptPending(MemoryPendingStateStore::default())),
        Arc::new(Clock::default()),
    );
    let r = f.begin("test", "account", &[]).unwrap();
    assert!(matches!(
        f.complete(callback(&r.state)),
        Err(ServerAuthError::StateMismatch)
    ));
    assert!(server.requests().is_empty());
}

#[test]
fn simultaneous_callbacks_spend_one_code() {
    let server = MockServer::start(|_, _| token_response());
    let f = Arc::new(flow(
        provider(&server),
        Arc::new(MemoryPendingStateStore::default()),
        Arc::new(Clock::default()),
    ));
    let r = f.begin("test", "account", &[]).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let mut handles = vec![];
    for _ in 0..2 {
        let f = f.clone();
        let state = r.state.clone();
        let barrier = barrier.clone();
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            f.complete(callback(&state))
        }));
    }
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert!(results
        .iter()
        .any(|r| matches!(r, Err(ServerAuthError::StateMismatch))));
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn requires_nonzero_connect_and_request_deadlines() {
    let server = MockServer::start(|_, _| token_response());
    for config in [
        ClientConfig {
            connect_timeout: None,
            ..ClientConfig::default()
        },
        ClientConfig {
            connect_timeout: Some(Duration::ZERO),
            ..ClientConfig::default()
        },
        ClientConfig {
            request_timeout: Duration::ZERO,
            ..ClientConfig::default()
        },
    ] {
        assert!(matches!(
            ServerAuthFlow::new(
                Client::new(config),
                [provider(&server)],
                Arc::new(MemoryPendingStateStore::default()),
                Duration::from_secs(300)
            ),
            Err(ServerAuthError::InvalidConfig)
        ));
    }
}

#[test]
fn redirect_response_is_not_followed_or_accepted() {
    let destination = MockServer::start(|_, _| token_response());
    let location = destination.url("/capture");
    let server = MockServer::start(move |_, _| {
        MockResponse::json(307, &json!({"access_token": "redirect-secret"}))
            .header("Location", &location)
    });
    let f = flow(
        provider(&server),
        Arc::new(MemoryPendingStateStore::default()),
        Arc::new(Clock::default()),
    );
    let r = f.begin("test", "account", &[]).unwrap();
    assert!(matches!(
        f.complete(callback(&r.state)),
        Err(ServerAuthError::ExchangeFailure { status: Some(307) })
    ));
    assert!(matches!(
        f.complete(callback(&r.state)),
        Err(ServerAuthError::StateMismatch)
    ));
    assert_eq!(server.requests().len(), 1);
    assert!(destination.requests().is_empty());
}

#[test]
fn public_clients_require_pkce() {
    let server = MockServer::start(|_, _| token_response());
    let mut p = provider(&server);
    p.client_auth = ClientAuth::None;
    p.config.client_secret = None;
    p.pkce = false;
    assert!(matches!(
        ServerAuthFlow::new(
            Client::new(ClientConfig::default()),
            [p.clone()],
            Arc::new(MemoryPendingStateStore::default()),
            Duration::from_secs(300)
        ),
        Err(ServerAuthError::InvalidConfig)
    ));
    p.pkce = true;
    let f = flow(
        p,
        Arc::new(MemoryPendingStateStore::default()),
        Arc::new(Clock::default()),
    );
    let r = f.begin("test", "account", &[]).unwrap();
    f.complete(callback(&r.state)).unwrap();
    let form = server.requests()[0].form();
    assert!(form.iter().any(|(k, _)| k == "code_verifier"));
    assert!(!form.iter().any(|(k, _)| k == "client_secret"));
}

#[test]
fn bound_callback_rejects_wrong_account_or_provider_before_exchange() {
    let server = MockServer::start(|_, _| token_response());
    let f = flow(
        provider(&server),
        Arc::new(MemoryPendingStateStore::default()),
        Arc::new(Clock::default()),
    );
    for (provider, account) in [("test", "other-account"), ("other-provider", "account")] {
        let r = f.begin("test", "account", &[]).unwrap();
        assert!(matches!(
            f.complete_bound(callback(&r.state), provider, account),
            Err(ServerAuthError::StateMismatch)
        ));
        assert!(matches!(
            f.complete_bound(callback(&r.state), "test", "account"),
            Err(ServerAuthError::StateMismatch)
        ));
    }
    assert!(server.requests().is_empty());
    let r = f.begin("test", "account", &[]).unwrap();
    assert_eq!(
        f.complete_bound(callback(&r.state), "test", "account")
            .unwrap()
            .account_hint,
        "account"
    );
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn malformed_callback_consumes_matching_state() {
    let server = MockServer::start(|_, _| token_response());
    let f = flow(
        provider(&server),
        Arc::new(MemoryPendingStateStore::default()),
        Arc::new(Clock::default()),
    );
    for code in [None, Some(String::new())] {
        let r = f.begin("test", "account", &[]).unwrap();
        assert!(matches!(
            f.complete(CallbackQuery {
                state: Some(r.state.clone()),
                code,
                error: None
            }),
            Err(ServerAuthError::InvalidCallback)
        ));
        assert!(matches!(
            f.complete(callback(&r.state)),
            Err(ServerAuthError::StateMismatch)
        ));
    }
    assert!(server.requests().is_empty());
}

#[test]
fn oidc_missing_id_token_fails_closed_and_consumes_nonce_state() {
    let server = MockServer::start(|_, _| token_response());
    let mut p = provider(&server);
    p.oidc = true;
    let validator = Arc::new(Validator {
        calls: Mutex::new(vec![]),
        reject: false,
    });
    let f = flow(
        p,
        Arc::new(MemoryPendingStateStore::default()),
        Arc::new(Clock::default()),
    )
    .with_oidc_validator(validator.clone());
    let r = f.begin("test", "account", &["openid".into()]).unwrap();
    assert!(matches!(
        f.complete(callback(&r.state)),
        Err(ServerAuthError::OidcValidation)
    ));
    assert!(matches!(
        f.complete(callback(&r.state)),
        Err(ServerAuthError::StateMismatch)
    ));
    assert!(validator.calls.lock().unwrap().is_empty());
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn provider_debug_redacts_arbitrary_authorization_values() {
    let server = MockServer::start(|_, _| token_response());
    let mut p = provider(&server);
    p.config
        .authorization_params
        .push(("hint".into(), "hint-secret".into()));
    p.redirect_uri = "https://user:password-secret@example.test/callback".into();
    let debug = format!("{p:?}");
    for secret in ["client-secret", "hint-secret", "password-secret"] {
        assert!(!debug.contains(secret));
    }
}

#[cfg(unix)]
fn sealed_path(root: &std::path::Path) -> std::path::PathBuf {
    std::fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "sealed"))
        .unwrap()
}

#[cfg(unix)]
#[test]
fn sealed_store_fresh_nonces_private_modes_and_aad_binding() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tokens");
    let s = SealedFileTokenStore::new(&root, [7; 32]).unwrap();
    s.put("account", "test", &tokens(3600)).unwrap();
    let a = sealed_path(&root);
    let first = std::fs::read(&a).unwrap();
    s.put("account", "test", &tokens(3600)).unwrap();
    let second = std::fs::read(&a).unwrap();
    assert_ne!(&first[5..17], &second[5..17]);
    assert_ne!(first, second);
    assert_eq!(
        std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for entry in std::fs::read_dir(&root).unwrap() {
        let entry = entry.unwrap();
        assert_eq!(
            entry.metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_ne!(entry.path().extension().unwrap(), "tmp");
    }
    s.put("other-account", "test", &tokens(3600)).unwrap();
    let b = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "sealed") && p != &a)
        .unwrap();
    std::fs::write(&b, second).unwrap();
    assert!(matches!(
        s.get("other-account", "test"),
        Err(ServerAuthError::Storage)
    ));
    assert!(s.get("account", "test").unwrap().is_some());
}

#[cfg(unix)]
#[test]
fn sealed_store_rejects_file_symlinks_hardlinks_and_public_modes() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tokens");
    let s = SealedFileTokenStore::new(&root, [7; 32]).unwrap();
    s.put("account", "test", &tokens(3600)).unwrap();
    let path = sealed_path(&root);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        s.get("account", "test"),
        Err(ServerAuthError::Storage)
    ));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let copy = dir.path().join("linked");
    std::fs::hard_link(&path, &copy).unwrap();
    assert!(matches!(
        s.get("account", "test"),
        Err(ServerAuthError::Storage)
    ));
    std::fs::remove_file(&copy).unwrap();
    std::fs::rename(&path, &copy).unwrap();
    symlink(&copy, &path).unwrap();
    assert!(matches!(
        s.get("account", "test"),
        Err(ServerAuthError::Storage)
    ));
    let lock = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "lock"))
        .unwrap();
    std::fs::remove_file(&lock).unwrap();
    symlink(&copy, &lock).unwrap();
    assert!(matches!(
        s.get("account", "test"),
        Err(ServerAuthError::Storage)
    ));
}

#[cfg(unix)]
#[test]
fn sealed_store_rechecks_root_privacy_and_bounds_lock_wait() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tokens");
    let s = SealedFileTokenStore::new(&root, [7; 32]).unwrap();
    let other = SealedFileTokenStore::new(&root, [7; 32])
        .unwrap()
        .with_lock_timeout(Duration::ZERO);
    let mut guard = s.lock("account", "test").unwrap();
    assert!(matches!(
        other.get("account", "test"),
        Err(ServerAuthError::Storage)
    ));
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(guard.get(), Err(ServerAuthError::Storage)));
    assert!(matches!(
        guard.put(&tokens(3600)),
        Err(ServerAuthError::Storage)
    ));
    assert!(matches!(guard.delete(), Err(ServerAuthError::Storage)));
    drop(guard);
    assert!(matches!(
        s.get("account", "test"),
        Err(ServerAuthError::Storage)
    ));
}

#[test]
fn slow_exchange_times_out_without_retry_and_consumes_state() {
    let server = MockServer::start(|_, _| token_response().delay(Duration::from_millis(250)));
    let f = ServerAuthFlow::new(
        Client::new(ClientConfig {
            request_timeout: Duration::from_millis(50),
            ..loopback_config()
        }),
        [provider(&server)],
        Arc::new(MemoryPendingStateStore::default()),
        Duration::from_secs(300),
    )
    .unwrap();
    let r = f.begin("test", "account", &[]).unwrap();
    assert!(matches!(
        f.complete(callback(&r.state)),
        Err(ServerAuthError::ExchangeFailure { status: None })
    ));
    assert!(matches!(
        f.complete(callback(&r.state)),
        Err(ServerAuthError::StateMismatch)
    ));
    assert_eq!(server.requests().len(), 1);
}
