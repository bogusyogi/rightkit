//! Durable, cross-process pending state: begin in one store instance, complete in another.
#![cfg(feature = "server")]
use rightkit_http::testing::{loopback_config, MockResponse, MockServer};
use rightkit_http::Client;
use rightkit_oauth::server::*;
use rightkit_oauth::OAuthConfig;
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

fn flow(
    server: &MockServer,
    pending: Arc<dyn PendingStateStore>,
    clock: Arc<Clock>,
) -> ServerAuthFlow {
    let p = ServerProvider::new(
        "test",
        OAuthConfig::new("client", server.url("/authorize"), server.url("/token"))
            .with_client_secret("client-secret"),
        "https://postright.orthiclabs.com/oauth/callback",
    );
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
fn ok_server() -> MockServer {
    MockServer::start(|_, _| {
        MockResponse::json(
            200,
            &json!({"access_token":"access-secret","refresh_token":"refresh-secret","expires_in":3600}),
        )
    })
}
fn store(dir: &std::path::Path, key: u8) -> Arc<FilePendingStateStore> {
    Arc::new(FilePendingStateStore::new(dir.join("pending"), [key; 32]).unwrap())
}
fn record_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(dir.join("pending"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "pending"))
        .collect()
}

#[test]
fn begin_in_one_instance_completes_in_another_exactly_once() {
    let server = ok_server();
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(Clock::default());
    let begin_side = flow(&server, store(dir.path(), 7), clock.clone());
    let complete_side = flow(&server, store(dir.path(), 7), clock.clone());
    let request = begin_side.begin("test", "acct", &[]).unwrap();
    assert_eq!(record_files(dir.path()).len(), 1);
    let done = complete_side.complete(callback(&request.state)).unwrap();
    assert_eq!(done.account_hint, "acct");
    assert_eq!(done.tokens.access_token(), "access-secret");
    // The verifier crossed the process boundary: the exchange carried it.
    let form: std::collections::HashMap<_, _> = server.requests()[0].form().into_iter().collect();
    assert!((43..=128).contains(&form["code_verifier"].len()));
    assert!(record_files(dir.path()).is_empty());
    // Replay against either instance is refused and spends no code.
    for side in [&complete_side, &begin_side] {
        assert!(matches!(
            side.complete(callback(&request.state)),
            Err(ServerAuthError::StateMismatch)
        ));
    }
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn expired_state_is_rejected_and_consumed() {
    let server = ok_server();
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(Clock::default());
    let a = flow(&server, store(dir.path(), 7), clock.clone());
    let b = flow(&server, store(dir.path(), 7), clock.clone());
    let request = a.begin("test", "acct", &[]).unwrap();
    clock.0.store(300, Ordering::SeqCst);
    assert!(matches!(
        b.complete(callback(&request.state)),
        Err(ServerAuthError::StateExpired)
    ));
    assert!(matches!(
        b.complete(callback(&request.state)),
        Err(ServerAuthError::StateMismatch)
    ));
    assert_eq!(server.requests().len(), 0);
}

#[test]
fn records_are_sealed_and_bound_to_the_key() {
    let server = ok_server();
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(Clock::default());
    let a = flow(&server, store(dir.path(), 7), clock.clone());
    let request = a.begin("test", "acct-marker", &[]).unwrap();
    let files = record_files(dir.path());
    assert_eq!(files.len(), 1);
    let raw = std::fs::read(&files[0]).unwrap();
    for needle in [request.state.as_str(), "acct-marker", "code_verifier"] {
        assert!(
            !raw.windows(needle.len()).any(|w| w == needle.as_bytes()),
            "record is plaintext"
        );
    }
    // A different key cannot open it, and the failed attempt still consumes the state.
    let wrong = flow(&server, store(dir.path(), 8), clock.clone());
    assert!(matches!(
        wrong.complete(callback(&request.state)),
        Err(ServerAuthError::Storage)
    ));
    assert!(record_files(dir.path()).is_empty());
    assert!(matches!(
        a.complete(callback(&request.state)),
        Err(ServerAuthError::StateMismatch)
    ));
}

#[test]
fn a_record_cannot_be_moved_to_another_state_name() {
    let server = ok_server();
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(Clock::default());
    let f = flow(&server, store(dir.path(), 7), clock.clone());
    let first = f.begin("test", "one", &[]).unwrap();
    let second = f.begin("test", "two", &[]).unwrap();
    let files = record_files(dir.path());
    assert_eq!(files.len(), 2);
    // Overwrite the first record with the second's bytes: AAD binds the file name.
    let by_len: Vec<_> = files.iter().map(|p| std::fs::read(p).unwrap()).collect();
    std::fs::write(&files[0], &by_len[1]).unwrap();
    let results = [
        f.complete(callback(&first.state)),
        f.complete(callback(&second.state)),
    ];
    // Whichever record was overwritten fails closed; none exchanges under a foreign state.
    assert!(results
        .iter()
        .any(|r| matches!(r, Err(ServerAuthError::Storage))));
    assert!(server.requests().len() <= 1);
}

#[test]
fn capacity_is_bounded_and_expired_records_are_purged_on_insert() {
    let server = ok_server();
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(Clock::default());
    let small = Arc::new(
        FilePendingStateStore::new(dir.path().join("pending"), [7; 32])
            .unwrap()
            .with_capacity(1),
    );
    let f = flow(&server, small, clock.clone());
    f.begin("test", "one", &[]).unwrap();
    assert!(matches!(
        f.begin("test", "two", &[]),
        Err(ServerAuthError::PendingCapacity)
    ));
    clock.0.store(301, Ordering::SeqCst);
    f.begin("test", "three", &[]).unwrap();
    assert_eq!(record_files(dir.path()).len(), 1);
}

#[test]
fn rejects_duplicate_keys_and_unsafe_key_names() {
    let dir = tempfile::tempdir().unwrap();
    let s = FilePendingStateStore::new(dir.path().join("pending"), [7; 32]).unwrap();
    let now = Clock::default().now();
    let record = || PendingAuthorization {
        state: "s".repeat(43),
        provider: "test".into(),
        account_hint: "a".into(),
        expires_at: now + Duration::from_secs(60),
        verifier: Some("v".repeat(43)),
        nonce: None,
        binding: "b".into(),
    };
    s.insert("abc-DEF_123", record(), now).unwrap();
    assert!(matches!(
        s.insert("abc-DEF_123", record(), now),
        Err(ServerAuthError::StateMismatch)
    ));
    for bad in ["", "../escape", "a/b", "a.b", &"x".repeat(129)] {
        assert!(matches!(
            s.insert(bad, record(), now),
            Err(ServerAuthError::InvalidConfig)
        ));
        assert!(s.take(bad).unwrap().is_none());
    }
    let taken = s.take("abc-DEF_123").unwrap().unwrap();
    assert_eq!(taken.account_hint, "a");
    assert!(s.take("abc-DEF_123").unwrap().is_none());
}

#[test]
fn concurrent_takes_hand_out_one_record() {
    let dir = tempfile::tempdir().unwrap();
    let now = Clock::default().now();
    let a = Arc::new(FilePendingStateStore::new(dir.path().join("pending"), [7; 32]).unwrap());
    let b = Arc::new(FilePendingStateStore::new(dir.path().join("pending"), [7; 32]).unwrap());
    a.insert(
        "k1",
        PendingAuthorization {
            state: "s".repeat(43),
            provider: "test".into(),
            account_hint: "a".into(),
            expires_at: now + Duration::from_secs(60),
            verifier: None,
            nonce: None,
            binding: "b".into(),
        },
        now,
    )
    .unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let handles: Vec<_> = [a, b]
        .into_iter()
        .map(|s| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                s.take("k1").unwrap().is_some()
            })
        })
        .collect();
    let won: usize = handles
        .into_iter()
        .map(|h| usize::from(h.join().unwrap()))
        .sum();
    assert_eq!(won, 1);
}

#[cfg(unix)]
#[test]
fn directory_and_records_are_private_on_unix() {
    use std::os::unix::fs::PermissionsExt;
    let server = ok_server();
    let dir = tempfile::tempdir().unwrap();
    let f = flow(&server, store(dir.path(), 7), Arc::new(Clock::default()));
    f.begin("test", "acct", &[]).unwrap();
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dir.path().join("pending")), 0o700);
    assert_eq!(mode(&record_files(dir.path())[0]), 0o600);
    // A directory opened up afterwards is refused.
    std::fs::set_permissions(
        dir.path().join("pending"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(f.begin("test", "acct2", &[]).is_err());
}

#[cfg(feature = "secrets")]
#[test]
fn keychain_held_key_is_minted_once_and_shared() {
    use rightkit_credentials::{Config, MemoryBackend, Namespace};
    let server = ok_server();
    let dir = tempfile::tempdir().unwrap();
    let creds = rightkit_credentials::Store::open(
        Config::new(Namespace::new("test.rightkit.oauth.pendingkey").unwrap())
            .backend(Arc::new(MemoryBackend::simulating_durable())),
    );
    let clock = Arc::new(Clock::default());
    let mk = || -> Arc<dyn PendingStateStore> {
        Arc::new(
            FilePendingStateStore::with_keychain_key(
                dir.path().join("pending"),
                &creds,
                "pending-key",
            )
            .unwrap(),
        )
    };
    let a = flow(&server, mk(), clock.clone());
    let b = flow(&server, mk(), clock.clone());
    let request = a.begin("test", "acct", &[]).unwrap();
    b.complete(callback(&request.state)).unwrap();
    // A malformed stored key is an error, never silently replaced.
    creds.set_str("pending-key", "not-hex").unwrap();
    assert!(FilePendingStateStore::with_keychain_key(
        dir.path().join("pending"),
        &creds,
        "pending-key"
    )
    .is_err());
    assert_eq!(
        creds
            .get("pending-key")
            .unwrap()
            .unwrap()
            .expose_str()
            .unwrap(),
        "not-hex"
    );
}

#[cfg(feature = "secrets")]
#[test]
fn a_guarded_credential_store_cannot_hold_the_pending_key() {
    use rightkit_credentials::{Config, GuardMode, MemoryBackend, Namespace};
    let dir = tempfile::tempdir().unwrap();
    for mode in [GuardMode::Memory, GuardMode::Deny] {
        let guarded = rightkit_credentials::Store::open(
            Config::new(Namespace::new("test.rightkit.oauth.pendingguard").unwrap())
                .backend(Arc::new(MemoryBackend::simulating_durable()))
                .guard_when(true, mode),
        );
        assert!(matches!(
            FilePendingStateStore::with_keychain_key(
                dir.path().join("pending"),
                &guarded,
                "pending-key"
            ),
            Err(ServerAuthError::StoreNotDurable)
        ));
    }
}
