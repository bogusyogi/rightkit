use rightkit_credentials::{Config, GuardMode, Namespace, Store};
use rightkit_http::Secret;
use rightkit_oauth::server::{
    FilePendingStateStore, KeychainTokenStore, ServerAuthError, TokenStore as ServerStore,
};
use rightkit_oauth::{credentials::SecretTokenStore, TokenSet, TokenStore as DesktopStore};
use std::{
    path::PathBuf,
    time::{Duration, UNIX_EPOCH},
};

/// The real OS credential store (Keychain / Credential Manager), in a namespace only this consumer
/// uses. No dummy backend (rightkit#1437): every item the run creates is deleted before it exits.
fn credentials() -> Store {
    Store::open(Config::new(
        Namespace::new("test.rightkit.oauth.r2").unwrap(),
    ))
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
    let desktop = SecretTokenStore::new(credentials());
    desktop.save("desktop-account", &tokens()).unwrap();
    let saved = desktop.load("desktop-account").unwrap().unwrap();
    assert_eq!(saved.access_token(), "synthetic-access");
    assert_eq!(saved.scope.as_deref(), Some("existing-consumer-scope"));
    desktop.delete("desktop-account").unwrap();
    assert!(desktop.load("desktop-account").unwrap().is_none());

    let server = KeychainTokenStore::new(credentials(), root.join("locks")).unwrap();
    server.put("account", "provider", &tokens()).unwrap();
    assert!(server.get("account", "other-provider").unwrap().is_none());
    let reopened = KeychainTokenStore::new(credentials(), root.join("locks")).unwrap();
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

    let credentials = credentials();
    let _pending =
        FilePendingStateStore::with_keychain_key(root.join("pending"), &credentials, "pending-key")
            .unwrap();
    let _reopened =
        FilePendingStateStore::with_keychain_key(root.join("pending"), &credentials, "pending-key")
            .unwrap();
    // A tripped guard denies every operation and is never durable: the server store refuses it.
    let guarded = Store::open(
        Config::new(Namespace::new("test.rightkit.oauth.guarded").unwrap())
            .guard_when(true, GuardMode::Deny),
    );
    assert!(matches!(
        KeychainTokenStore::new(guarded, root.join("guarded-locks")),
        Err(ServerAuthError::StoreNotDurable)
    ));
    // Remove what this run put in the real store.
    let _ = credentials.delete("pending-key");
    println!("credential-contract consumer passed; real OS store, items removed");
}
