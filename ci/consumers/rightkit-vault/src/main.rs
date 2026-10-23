// rightkit-vault 0.2.0 consumed by exact registry pin, outside the workspace.
//
// Proves the sealed-blob family an app's vault calls (rightkit-vault vault.rs): Vault::new over four
// caller-supplied stores (BlobStore, ServerKeyCache, ClientSecretStore, ServerKeySource) with a
// fixed clock; load of an absent blob returns None; save then load round-trips a secret map; and a
// load by a different subject is refused, because the AAD binds the subject.
// Does not prove: the HTTP key source (rightkit-http), the credentials-backed cache and client
// secret, the Drive or CloudKit blob store, rotation, reset, sign-out, offline grace expiry, or a
// payload larger than one entry.
// State: in-memory stores, fixed key and client-secret fixtures, and a fixed clock. No network, no
// files, no real user data.
use std::sync::{Arc, Mutex};

use rightkit_credentials::Secret;
use rightkit_vault::{
    BlobStore, CachedKeys, ClientSecret, ClientSecretStore, Config, IdToken, SecretMap, ServerKey,
    ServerKeyCache, ServerKeySource, ServerKeys, Vault,
};

// Unix seconds, fixed so the cache age does not depend on the run time.
const NOW: u64 = 1_792_458_000;

#[derive(Default)]
struct MemBlob(Mutex<Option<Vec<u8>>>);

impl BlobStore for MemBlob {
    fn read(&self) -> rightkit_vault::Result<Option<Vec<u8>>> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn write(&self, bytes: &[u8]) -> rightkit_vault::Result<()> {
        *self.0.lock().unwrap() = Some(bytes.to_vec());
        Ok(())
    }
}

#[derive(Default)]
struct MemCache(Mutex<Option<CachedKeys>>);

impl ServerKeyCache for MemCache {
    fn load(&self) -> rightkit_vault::Result<Option<CachedKeys>> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn store(&self, entry: &CachedKeys) -> rightkit_vault::Result<()> {
        *self.0.lock().unwrap() = Some(entry.clone());
        Ok(())
    }
    fn clear(&self) -> rightkit_vault::Result<()> {
        *self.0.lock().unwrap() = None;
        Ok(())
    }
}

struct FixedClient(ClientSecret);

impl ClientSecretStore for FixedClient {
    fn get(&self) -> rightkit_vault::Result<Option<ClientSecret>> {
        Ok(Some(self.0.clone()))
    }
    fn get_or_create(&self) -> rightkit_vault::Result<ClientSecret> {
        Ok(self.0.clone())
    }
}

struct FixedSource(ServerKeys);

impl ServerKeySource for FixedSource {
    fn fetch(&self, _id_token: &str) -> rightkit_vault::Result<ServerKeys> {
        Ok(self.0.clone())
    }
    fn reset(&self, _id_token: &str) -> rightkit_vault::Result<ServerKeys> {
        Ok(self.0.clone())
    }
}

fn server_keys(id: &str, byte: u8) -> ServerKeys {
    ServerKeys {
        current: ServerKey::new(id, [byte; 32]),
        previous: Vec::new(),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let blobs = Arc::new(MemBlob::default());
    let cache = Arc::new(MemCache::default());
    let source = Arc::new(FixedSource(server_keys("k-1", 7)));
    let client = Arc::new(FixedClient(ClientSecret::from_bytes([9u8; 32])));
    let clock = || -> Arc<dyn Fn() -> u64 + Send + Sync> { Arc::new(|| NOW) };

    let vault = Vault::new(
        Config::new("consumer-app", "fixture-subject"),
        source.clone(),
        cache.clone(),
        client.clone(),
        blobs.clone(),
    )
    .with_clock(clock());
    let token = || -> rightkit_vault::Result<String> { Ok("fixture-id-token".to_string()) };
    let token: IdToken = &token;

    // No blob yet: load reports absence, not an error.
    assert!(
        vault.load(token)?.is_none(),
        "no blob before the first save"
    );

    // Save, then load: the secret round-trips under the same subject.
    let mut secrets = SecretMap::new();
    secrets.insert(
        "provider-a".to_string(),
        Secret::from_string("fixture-value-a"),
    );
    vault.save(token, &secrets)?;
    let loaded = vault.load(token)?.expect("the blob was just written");
    assert_eq!(loaded.len(), 1);
    assert_eq!(
        loaded["provider-a"].expose_str().ok(),
        Some("fixture-value-a")
    );

    // Another subject reading the same blob is refused: the AAD binds the subject.
    let other = Vault::new(
        Config::new("consumer-app", "other-subject"),
        source.clone(),
        cache.clone(),
        client.clone(),
        blobs.clone(),
    )
    .with_clock(clock());
    let refused = match other.load(token) {
        Err(e) => e,
        Ok(_) => return Err("another subject opened the blob".into()),
    };
    assert!(matches!(refused, rightkit_vault::Error::Decrypt));

    println!("OK rightkit-vault 0.2.0 (registry consumer)");
    Ok(())
}
