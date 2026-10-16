//! Keychain-backed TokenStore, bare-token refresh and rotation persistence.
#![cfg(all(feature = "server", feature = "secrets"))]
use rightkit_credentials::{Config, MemoryBackend, Namespace};
use rightkit_http::testing::{loopback_config, MockResponse, MockServer};
use rightkit_http::{Client, Secret};
use rightkit_oauth::server::*;
use rightkit_oauth::{OAuthConfig, TokenSet};
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Default)]
struct Clock(AtomicU64);
impl ServerClock for Clock {
    fn now(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(10000 + self.0.load(Ordering::SeqCst))
    }
}
fn flow(server: &MockServer, clock: Arc<Clock>) -> ServerAuthFlow {
    let p = ServerProvider::new(
        "test",
        OAuthConfig::new("client", server.url("/authorize"), server.url("/token"))
            .with_client_secret("client-secret"),
        "https://postright.orthiclabs.com/oauth/callback",
    );
    ServerAuthFlow::new(
        Client::new(loopback_config()),
        [p],
        Arc::new(MemoryPendingStateStore::default()),
        Duration::from_secs(300),
    )
    .unwrap()
    .with_clock(clock)
}
fn creds(namespace: &str) -> rightkit_credentials::Store {
    rightkit_credentials::Store::open(
        Config::new(Namespace::new(namespace).unwrap()).backend(Arc::new(MemoryBackend::new())),
    )
}
fn tokens(refresh: &str, expires: u64) -> TokenSet {
    TokenSet {
        access_token: Secret::new("old-access"),
        refresh_token: Some(Secret::new(refresh)),
        token_type: "Bearer".into(),
        scope: Some("tweet.read".into()),
        id_token: None,
        obtained_at: Clock::default().now(),
        expires_in: Some(Duration::from_secs(expires)),
    }
}
fn form(r: &rightkit_http::testing::RecordedRequest) -> std::collections::HashMap<String, String> {
    r.form().into_iter().collect()
}

#[test]
fn roundtrip_isolation_and_delete() {
    let dir = tempfile::tempdir().unwrap();
    let store = KeychainTokenStore::new(creds("test.rightkit.oauth.kc1"), dir.path().join("locks"))
        .unwrap();
    assert!(store.get("acct", "x").unwrap().is_none());
    store.put("acct", "x", &tokens("r1", 3600)).unwrap();
    store.put("acct", "y", &tokens("r2", 3600)).unwrap();
    store.put("other", "x", &tokens("r3", 3600)).unwrap();
    let get = |a, p| {
        store
            .get(a, p)
            .unwrap()
            .unwrap()
            .refresh_token
            .unwrap()
            .expose()
            .to_string()
    };
    assert_eq!(
        (get("acct", "x"), get("acct", "y"), get("other", "x")),
        ("r1".into(), "r2".into(), "r3".into())
    );
    let loaded = store.get("acct", "x").unwrap().unwrap();
    assert_eq!(loaded.access_token(), "old-access");
    assert_eq!(loaded.scope.as_deref(), Some("tweet.read"));
    assert_eq!(loaded.expires_in, Some(Duration::from_secs(3600)));
    store.delete("acct", "x").unwrap();
    store.delete("acct", "x").unwrap();
    assert!(store.get("acct", "x").unwrap().is_none());
    assert!(store.get("acct", "y").unwrap().is_some());
    assert!(matches!(
        store.get("", "x"),
        Err(ServerAuthError::InvalidConfig)
    ));
    assert!(!format!("{store:?}").contains("r1"));
}

#[test]
fn tokens_are_not_written_to_the_lock_directory() {
    let dir = tempfile::tempdir().unwrap();
    let locks = dir.path().join("locks");
    let store = KeychainTokenStore::new(creds("test.rightkit.oauth.kc2"), &locks).unwrap();
    store
        .put("acct", "x", &tokens("refresh-marker", 3600))
        .unwrap();
    for entry in std::fs::read_dir(&locks).unwrap() {
        let bytes = std::fs::read(entry.unwrap().path()).unwrap();
        assert!(bytes.is_empty());
    }
}

#[test]
fn refresh_persists_rotated_token_and_keeps_old_when_omitted() {
    let server = MockServer::start(|request, n| {
        let f = form(request);
        assert_eq!(f["grant_type"], "refresh_token");
        assert_eq!(
            f["refresh_token"],
            if n == 0 {
                "old-refresh"
            } else {
                "rotated-refresh"
            }
        );
        if n == 0 {
            MockResponse::json(
                200,
                &json!({"access_token":"a1","refresh_token":"rotated-refresh","expires_in":3600}),
            )
        } else {
            MockResponse::json(200, &json!({"access_token":"a2","expires_in":3600}))
        }
    });
    let clock = Arc::new(Clock::default());
    let f = flow(&server, clock.clone());
    let dir = tempfile::tempdir().unwrap();
    let store = KeychainTokenStore::new(creds("test.rightkit.oauth.kc3"), dir.path().join("locks"))
        .unwrap();
    store
        .put("acct", "test", &tokens("old-refresh", 90))
        .unwrap();
    let first = f
        .refresh(&store, "acct", "test", Duration::from_secs(120))
        .unwrap();
    assert_eq!(first.access_token(), "a1");
    let stored = store.get("acct", "test").unwrap().unwrap();
    assert_eq!(stored.refresh_token.unwrap().expose(), "rotated-refresh");
    assert_eq!(stored.access_token(), "a1");
    clock.0.store(3500, Ordering::SeqCst);
    let second = f
        .refresh(&store, "acct", "test", Duration::from_secs(120))
        .unwrap();
    assert_eq!(second.access_token(), "a2");
    assert_eq!(
        second.refresh_token.as_ref().unwrap().expose(),
        "rotated-refresh"
    );
    assert_eq!(
        store
            .get("acct", "test")
            .unwrap()
            .unwrap()
            .refresh_token
            .unwrap()
            .expose(),
        "rotated-refresh"
    );
}

#[test]
fn concurrent_refresh_across_store_instances_spends_one_grant() {
    let server = MockServer::start(|_, _| {
        MockResponse::json(
            200,
            &json!({"access_token":"access-secret","refresh_token":"new-refresh","expires_in":3600}),
        )
    });
    let f = Arc::new(flow(&server, Arc::new(Clock::default())));
    let dir = tempfile::tempdir().unwrap();
    let shared = creds("test.rightkit.oauth.kc4");
    let locks = dir.path().join("locks");
    let a = KeychainTokenStore::new(shared.clone(), &locks).unwrap();
    let b = KeychainTokenStore::new(shared, &locks).unwrap();
    a.put("acct", "test", &tokens("old-refresh", 90)).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let handles: Vec<_> = [a, b]
        .into_iter()
        .map(|store| {
            let (f, barrier) = (f.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                f.refresh(&store, "acct", "test", Duration::from_secs(120))
                    .unwrap()
            })
        })
        .collect();
    for h in handles {
        assert_eq!(h.join().unwrap().access_token(), "access-secret");
    }
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn bare_refresh_token_returns_rotated_or_retains_the_old_one() {
    let server = MockServer::start(|request, n| {
        let f = form(request);
        assert_eq!(f["grant_type"], "refresh_token");
        assert_eq!(f["client_id"], "client");
        if n == 0 {
            assert_eq!(f["refresh_token"], "bare-old");
            MockResponse::json(
                200,
                &json!({"access_token":"a1","refresh_token":"bare-new","expires_in":7200,"scope":"s"}),
            )
        } else {
            assert_eq!(f["refresh_token"], "bare-new");
            MockResponse::json(200, &json!({"access_token":"a2"}))
        }
    });
    let f = flow(&server, Arc::new(Clock::default()));
    let first = f.refresh_token("test", &Secret::new("bare-old")).unwrap();
    assert_eq!(first.access_token(), "a1");
    assert_eq!(first.refresh_token.as_ref().unwrap().expose(), "bare-new");
    assert_eq!(first.expires_in, Some(Duration::from_secs(7200)));
    let second = f.refresh_token("test", &Secret::new("bare-new")).unwrap();
    assert_eq!(second.refresh_token.as_ref().unwrap().expose(), "bare-new");
    assert!(matches!(
        f.refresh_token("nope", &Secret::new("x")),
        Err(ServerAuthError::InvalidConfig)
    ));
    assert!(matches!(
        f.refresh_token("test", &Secret::new("")),
        Err(ServerAuthError::InvalidConfig)
    ));
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn bare_refresh_and_store_persists_rotation_atomically() {
    let server = MockServer::start(|_, _| {
        MockResponse::json(
            200,
            &json!({"access_token":"a1","refresh_token":"rotated","expires_in":3600}),
        )
    });
    let f = flow(&server, Arc::new(Clock::default()));
    let dir = tempfile::tempdir().unwrap();
    let store = KeychainTokenStore::new(creds("test.rightkit.oauth.kc5"), dir.path().join("locks"))
        .unwrap();
    let out = f
        .refresh_token_and_store(&store, "acct", "test", &Secret::new("imported"))
        .unwrap();
    assert_eq!(out.refresh_token.as_ref().unwrap().expose(), "rotated");
    let stored = store.get("acct", "test").unwrap().unwrap();
    assert_eq!(stored.refresh_token.unwrap().expose(), "rotated");
    assert_eq!(stored.access_token(), "a1");
}

#[test]
fn dead_grant_deletes_only_the_rejected_stored_token() {
    let server =
        MockServer::start(|_, _| MockResponse::json(400, &json!({"error":"invalid_grant"})));
    let f = flow(&server, Arc::new(Clock::default()));
    let dir = tempfile::tempdir().unwrap();
    let store = KeychainTokenStore::new(creds("test.rightkit.oauth.kc6"), dir.path().join("locks"))
        .unwrap();
    store.put("acct", "test", &tokens("newer", 3600)).unwrap();
    assert!(matches!(
        f.refresh_token_and_store(&store, "acct", "test", &Secret::new("stale")),
        Err(ServerAuthError::ReauthRequired)
    ));
    assert!(
        store.get("acct", "test").unwrap().is_some(),
        "a newer stored token survives"
    );
    assert!(matches!(
        f.refresh_token_and_store(&store, "acct", "test", &Secret::new("newer")),
        Err(ServerAuthError::ReauthRequired)
    ));
    assert!(store.get("acct", "test").unwrap().is_none());
}

#[test]
fn failed_persistence_returns_the_rotated_tokens() {
    struct Failing;
    struct FailingGuard;
    impl TokenStore for Failing {
        fn lock(&self, _: &str, _: &str) -> Result<Box<dyn TokenStoreGuard + '_>, ServerAuthError> {
            Ok(Box::new(FailingGuard))
        }
    }
    impl TokenStoreGuard for FailingGuard {
        fn get(&mut self) -> Result<Option<TokenSet>, ServerAuthError> {
            Ok(None)
        }
        fn put(&mut self, _: &TokenSet) -> Result<(), ServerAuthError> {
            Err(ServerAuthError::Storage)
        }
        fn delete(&mut self) -> Result<(), ServerAuthError> {
            Ok(())
        }
    }
    let server = MockServer::start(|_, _| {
        MockResponse::json(
            200,
            &json!({"access_token":"a1","refresh_token":"rotated","expires_in":3600}),
        )
    });
    let f = flow(&server, Arc::new(Clock::default()));
    match f.refresh_token_and_store(&Failing, "acct", "test", &Secret::new("old")) {
        Err(ServerAuthError::RefreshPersistence { tokens }) => {
            assert_eq!(tokens.refresh_token.unwrap().expose(), "rotated");
        }
        _ => panic!("expected RefreshPersistence"),
    }
}
