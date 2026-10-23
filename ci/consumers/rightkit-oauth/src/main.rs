use rightkit_credentials::{Config, GuardMode, MemoryBackend, Namespace, Store};
use rightkit_http::Secret;
use rightkit_oauth::server::{
    FilePendingStateStore, KeychainTokenStore, ServerAuthError, TokenStore as ServerStore,
};
use rightkit_oauth::{credentials::SecretTokenStore, TokenSet, TokenStore as DesktopStore};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, UNIX_EPOCH},
};

fn credentials(backend: Arc<MemoryBackend>) -> Store {
    Store::open(Config::new(Namespace::new("test.rightkit.oauth.r2").unwrap()).backend(backend))
}
fn tokens() -> TokenSet {
    TokenSet {
        access_token: Secret::new("synthetic-access"),
        refresh_token: Some(Secret::new("synthetic-refresh")),
        token_type: "Bearer".into(),
        scope: Some("existing-consumer-scope".into()),
        id_token: None,
        obtained_at: UNIX_EPOCH + Duration::from_secs(1700000000),
        expires_in: Some(Duration::from_secs(3600)),
    }
}
fn main() {
    let mode = std::env::args().nth(1).expect("consumer mode required");
    assert_eq!(mode, "credential-contract");
    let root =
        PathBuf::from(std::env::var_os("RK_CONSUMER_RUN_ROOT").expect("fresh run root required"));
    // Injected synthetic backend only: never opens or reads user's credential stores.
    let backend = Arc::new(MemoryBackend::simulating_durable());
    let desktop = SecretTokenStore::new(credentials(backend.clone()));
    desktop.save("desktop-account", &tokens()).unwrap();
    let saved = desktop.load("desktop-account").unwrap().unwrap();
    assert_eq!(saved.access_token(), "synthetic-access");
    assert_eq!(saved.scope.as_deref(), Some("existing-consumer-scope"));
    desktop.delete("desktop-account").unwrap();
    assert!(desktop.load("desktop-account").unwrap().is_none());

    let server = KeychainTokenStore::new(credentials(backend.clone()), root.join("locks")).unwrap();
    server.put("account", "provider", &tokens()).unwrap();
    assert!(server.get("account", "other-provider").unwrap().is_none());
    let reopened =
        KeychainTokenStore::new(credentials(backend.clone()), root.join("locks")).unwrap();
    assert_eq!(
        reopened
            .get("account", "provider")
            .unwrap()
            .unwrap()
            .access_token(),
        "synthetic-access"
    );
    reopened.delete("account", "provider").unwrap();
    assert!(server.get("account", "provider").unwrap().is_none());

    let credentials = credentials(backend);
    let _pending =
        FilePendingStateStore::with_keychain_key(root.join("pending"), &credentials, "pending-key")
            .unwrap();
    let _reopened =
        FilePendingStateStore::with_keychain_key(root.join("pending"), &credentials, "pending-key")
            .unwrap();
    for mode in [GuardMode::Memory, GuardMode::Deny] {
        let guarded = Store::open(
            Config::new(Namespace::new("test.rightkit.oauth.guarded").unwrap())
                .backend(Arc::new(MemoryBackend::simulating_durable()))
                .guard_when(true, mode),
        );
        assert!(matches!(
            KeychainTokenStore::new(guarded, root.join("guarded-locks")),
            Err(ServerAuthError::StoreNotDurable)
        ));
    }
    println!("credential-contract consumer passed; synthetic backend only");
}
