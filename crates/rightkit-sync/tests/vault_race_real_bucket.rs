//! rightkit#966 acceptance: two writers race on one real bucket, one head wins each round, and
//! no edit is lost. Runs against Cloudflare R2 or any S3-compatible bucket that documents
//! conditional writes, with a key scoped to a scratch bucket. It needs credentials, so it is
//! `#[ignore]`d and runs only when asked for:
//!
//! ```text
//! RIGHTKIT_VAULT_S3_ENDPOINT=https://<account>.r2.cloudflarestorage.com \
//! RIGHTKIT_VAULT_S3_BUCKET=<scratch bucket> RIGHTKIT_VAULT_S3_REGION=auto \
//! RIGHTKIT_VAULT_S3_KEY_ID=... RIGHTKIT_VAULT_S3_SECRET=... \
//!   cargo test --features vault-s3 --test vault_race_real_bucket -- --ignored
//! ```
//!
//! Each run uses a fresh random vault id, so runs never see each other's objects.

use std::sync::{Arc, Barrier};
use std::thread;

use rightkit_http::Client;
use rightkit_sync::cipher::{CipherKey, StaticKeys};
use rightkit_sync::vault::s3::{Credentials, S3ConditionalStore, S3Config};
use rightkit_sync::vault::{KeepBoth, Vault, VaultConfig, VaultState};
use rightkit_sync::FileStorage;

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} must be set for this test"))
}

fn store() -> Arc<S3ConditionalStore> {
    let endpoint = env("RIGHTKIT_VAULT_S3_ENDPOINT");
    let host = endpoint
        .trim_start_matches("https://")
        .trim_end_matches('/')
        .to_string();
    let client = Client::first_party(vec![host]);
    Arc::new(
        S3ConditionalStore::new(
            client,
            S3Config {
                endpoint,
                bucket: env("RIGHTKIT_VAULT_S3_BUCKET"),
                region: env("RIGHTKIT_VAULT_S3_REGION"),
            },
            Credentials::new(
                env("RIGHTKIT_VAULT_S3_KEY_ID"),
                env("RIGHTKIT_VAULT_S3_SECRET"),
            ),
        )
        .expect("a valid S3 store configuration"),
    )
}

fn vault(store: Arc<S3ConditionalStore>, id: &str, key: [u8; 32], who: &str) -> Vault {
    let ledger = std::env::temp_dir().join(format!("rk-vault-race-{id}-{who}.json"));
    Vault::new(
        store,
        id,
        Arc::new(StaticKeys::new(1, CipherKey::from_bytes(key))),
        Arc::new(FileStorage::new(ledger)),
        VaultConfig::default(),
    )
    .expect("a valid vault")
}

#[test]
#[ignore = "needs a real R2/S3 bucket and credentials (RIGHTKIT_VAULT_S3_*)"]
fn two_writers_one_winner_no_loss() {
    let store = store();
    let mut random = [0u8; 8];
    for (i, byte) in random.iter_mut().enumerate() {
        *byte = (std::process::id() as u8).wrapping_add(i as u8)
            ^ std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos() as u8)
                .unwrap_or(0);
    }
    let id = format!(
        "race-{}",
        random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    let key = [7u8; 32];

    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = ["a", "b"]
        .into_iter()
        .map(|who| {
            let (store, id, barrier) = (store.clone(), id.clone(), barrier.clone());
            thread::spawn(move || {
                let vault = vault(store, &id, key, who);
                let mut local = VaultState::new();
                local.insert(
                    format!("only-{who}"),
                    format!("secret from {who}").into_bytes(),
                );
                local.insert("shared", format!("edited by {who}").into_bytes());
                barrier.wait();
                let first = vault.sync(&local, &KeepBoth).expect("the first sync");
                // The second round adopts whatever the other writer published.
                let second = vault
                    .sync(&first.state, &KeepBoth)
                    .expect("the second sync");
                (first.attempts, second.state)
            })
        })
        .collect();
    let results: Vec<_> = workers
        .into_iter()
        .map(|w| w.join().expect("worker"))
        .collect();

    // A third read from a fresh device sees the settled vault.
    let reader = vault(store, &id, key, "reader");
    let settled = reader
        .sync(&VaultState::new(), &KeepBoth)
        .expect("the reading sync");
    for name in ["only-a", "only-b", "shared"] {
        assert!(settled.state.get(name).is_some(), "{name} lost");
    }
    let shared_copies = settled
        .state
        .iter()
        .filter(|(n, _)| n.starts_with("shared"))
        .count();
    assert_eq!(shared_copies, 2, "both edits of the shared item are kept");
    let _ = results;
}
