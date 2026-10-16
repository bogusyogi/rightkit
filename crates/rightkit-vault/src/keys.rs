//! The two key halves and where they live.
//!
//! * [`ServerKey`] / [`ServerKeys`]: the half the entitlement service releases to the signed-in
//!   subject (current key plus, during rotation, still-valid previous keys).
//! * [`ClientSecret`]: the half only the user's devices hold (the OS credential store through
//!   `rightkit-credentials`). Neither half alone derives the wrap key.
//! * [`ServerKeyCache`]: the offline cache of the server half, in the OS credential store.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rightkit_credentials::{Secret, Store};
use ring::rand::{SecureRandom, SystemRandom};
use zeroize::Zeroizing;

use crate::error::{Error, Result};

pub const KEY_LEN: usize = 32;
const MAX_KEY_ID: usize = 128;
/// Previous keys kept in a bundle (and in the OS cache entry: Windows caps a credential blob at
/// 2560 bytes, this keeps an entry well below that).
pub(crate) const MAX_PREVIOUS: usize = 8;

pub(crate) fn valid_key_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > MAX_KEY_ID || id.chars().any(|c| c.is_control()) {
        return Err(Error::Service("invalid wrapping key id".into()));
    }
    Ok(())
}

/// One server-held wrapping key and its service-assigned id. Zeroed on drop, redacted `Debug`.
#[derive(Clone)]
pub struct ServerKey {
    id: String,
    key: Zeroizing<[u8; KEY_LEN]>,
}

impl ServerKey {
    pub fn new(id: impl Into<String>, key: [u8; KEY_LEN]) -> Self {
        Self {
            id: id.into(),
            key: Zeroizing::new(key),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn bytes(&self) -> &[u8; KEY_LEN] {
        &self.key
    }
}

impl std::fmt::Debug for ServerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ServerKey({}, <redacted>)", self.id)
    }
}

/// What the service returns: the current key and, during rotation, previous keys it still
/// serves so a client can re-wrap.
#[derive(Debug, Clone)]
pub struct ServerKeys {
    pub current: ServerKey,
    pub previous: Vec<ServerKey>,
}

impl ServerKeys {
    pub fn get(&self, id: &str) -> Option<&ServerKey> {
        if self.current.id == id {
            Some(&self.current)
        } else {
            self.previous.iter().find(|k| k.id == id)
        }
    }
}

/// A cached [`ServerKeys`] and when the service last confirmed it (drives the offline grace).
#[derive(Debug, Clone)]
pub struct CachedKeys {
    pub keys: ServerKeys,
    /// Unix seconds.
    pub fetched_at: u64,
}

/// The client-held half: 32 random bytes that never leave the user's devices and are never
/// sent to the service or written to the cloud folder. Zeroed on drop, redacted `Debug`.
///
/// It must exist on every device that opens the vault. The first device creates it
/// ([`ClientSecretStore::get_or_create`]); other devices receive it through the app's
/// device-approval transfer (for example `rightkit-sync-keys`' transfer protocol) or derive it
/// from an account key they already share, then [`CredentialsClientSecret::install_if_absent`].
#[derive(Clone)]
pub struct ClientSecret(Zeroizing<[u8; KEY_LEN]>);

impl ClientSecret {
    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    pub fn generate() -> Result<Self> {
        let mut b = Zeroizing::new([0u8; KEY_LEN]);
        SystemRandom::new()
            .fill(&mut b[..])
            .map_err(|_| Error::Entropy)?;
        Ok(Self(b))
    }

    /// The raw bytes, for a device-to-device transfer layer. Handle as key material.
    pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }
}

impl std::fmt::Debug for ClientSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ClientSecret(<redacted>)")
    }
}

/// Fetches the server half from the entitlement service with a Google ID token.
pub trait ServerKeySource: Send + Sync {
    /// The current key (and previous ones). Errors: `Unauthorized` (401), `Denied` (403),
    /// `Revoked` (410), `Offline`, `Service`.
    fn fetch(&self, id_token: &str) -> Result<ServerKeys>;
    /// Ask the service to re-initialise the vault with a brand-new key (the old blob becomes
    /// unreadable by design). Only `Vault::reset` calls this.
    fn reset(&self, id_token: &str) -> Result<ServerKeys>;
}

/// Local cache of the server half. The production impl is [`CredentialsKeyCache`].
pub trait ServerKeyCache: Send + Sync {
    fn load(&self) -> Result<Option<CachedKeys>>;
    fn store(&self, entry: &CachedKeys) -> Result<()>;
    fn clear(&self) -> Result<()>;
}

/// Local home of the client half. The production impl is [`CredentialsClientSecret`].
pub trait ClientSecretStore: Send + Sync {
    /// `Ok(None)` = this device has none.
    fn get(&self) -> Result<Option<ClientSecret>>;
    /// The existing secret, or a newly generated one if (and only if) none exists.
    fn get_or_create(&self) -> Result<ClientSecret>;
}

// ---- credentials-backed cache ----------------------------------------------------------

const CACHE_FORMAT: u8 = 1;

/// `[format][fetched_at u64 BE][count u8]` then per key `[id len u16 BE][id][key 32]`, current
/// key first. Binary so the key bytes never pass through a `String`.
fn encode_cache(entry: &CachedKeys) -> Zeroizing<Vec<u8>> {
    let mut out = Zeroizing::new(Vec::with_capacity(
        10 + (1 + entry.keys.previous.len()) * (2 + MAX_KEY_ID + KEY_LEN),
    ));
    out.push(CACHE_FORMAT);
    out.extend_from_slice(&entry.fetched_at.to_be_bytes());
    out.push((1 + entry.keys.previous.len()) as u8);
    for k in std::iter::once(&entry.keys.current).chain(entry.keys.previous.iter()) {
        out.extend_from_slice(&(k.id.len() as u16).to_be_bytes());
        out.extend_from_slice(k.id.as_bytes());
        out.extend_from_slice(k.bytes());
    }
    out
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Some(head)
    }
}

/// `None` for any malformed entry: an unreadable cache behaves as a miss.
fn decode_cache(bytes: &[u8]) -> Option<CachedKeys> {
    let mut r = Reader(bytes);
    if r.take(1)?[0] != CACHE_FORMAT {
        return None;
    }
    let fetched_at = u64::from_be_bytes(r.take(8)?.try_into().ok()?);
    let count = r.take(1)?[0] as usize;
    if count == 0 || count > 1 + MAX_PREVIOUS {
        return None;
    }
    let mut keys: Vec<ServerKey> = Vec::with_capacity(count);
    for _ in 0..count {
        let len = u16::from_be_bytes(r.take(2)?.try_into().ok()?) as usize;
        let id = std::str::from_utf8(r.take(len)?).ok()?;
        valid_key_id(id).ok()?;
        let key: [u8; KEY_LEN] = r.take(KEY_LEN)?.try_into().ok()?;
        keys.push(ServerKey::new(id, key));
    }
    if !r.0.is_empty() {
        return None;
    }
    let current = keys.remove(0);
    Some(CachedKeys {
        keys: ServerKeys {
            current,
            previous: keys,
        },
        fetched_at,
    })
}

/// Stores the bundle as one `rightkit-credentials` secret (Keychain / Credential Manager /
/// Secret Service), account `vault-keys.<app>.<subject>`.
///
/// The cached server key is the *unwrapped* wrapping key: a process or user that can read this
/// credential has the server half without asking the service. See the README for the
/// trade-off.
pub struct CredentialsKeyCache {
    store: Store,
    account: String,
}

impl CredentialsKeyCache {
    pub fn new(store: Store, app: &str, subject: &str) -> Self {
        Self {
            store,
            account: format!("vault-keys.{app}.{subject}"),
        }
    }
}

impl ServerKeyCache for CredentialsKeyCache {
    fn load(&self) -> Result<Option<CachedKeys>> {
        let secret = self
            .store
            .get(&self.account)
            .map_err(|e| Error::Cache(e.to_string()))?;
        Ok(secret.and_then(|s| decode_cache(s.expose_bytes())))
    }

    fn store(&self, entry: &CachedKeys) -> Result<()> {
        if entry.keys.previous.len() > MAX_PREVIOUS {
            return Err(Error::Cache("too many previous keys".into()));
        }
        let bytes = encode_cache(entry);
        self.store
            .set(&self.account, &Secret::from_bytes(bytes.as_slice()))
            .map_err(|e| Error::Cache(e.to_string()))
    }

    fn clear(&self) -> Result<()> {
        self.store
            .delete(&self.account)
            .map(|_| ())
            .map_err(|e| Error::Cache(e.to_string()))
    }
}

// ---- credentials-backed client secret --------------------------------------------------

/// The client half in one `rightkit-credentials` secret, account `vault-client.<app>.<subject>`,
/// stored as base64 text of 32 random bytes (the shape `Store::get_or_create_key` mints).
pub struct CredentialsClientSecret {
    store: Store,
    account: String,
}

impl CredentialsClientSecret {
    pub fn new(store: Store, app: &str, subject: &str) -> Self {
        Self {
            store,
            account: format!("vault-client.{app}.{subject}"),
        }
    }

    fn decode(secret: &Secret) -> Result<ClientSecret> {
        let raw = Zeroizing::new(
            B64.decode(secret.expose_bytes())
                .map_err(|_| Error::ClientSecret("stored client secret is malformed".into()))?,
        );
        let arr: [u8; KEY_LEN] = raw
            .as_slice()
            .try_into()
            .map_err(|_| Error::ClientSecret("stored client secret is malformed".into()))?;
        Ok(ClientSecret::from_bytes(arr))
    }

    /// Install a secret received from another device, only if this device has none. `Ok(false)`
    /// = one already exists and was left alone (replacing it would orphan the blob it opens).
    pub fn install_if_absent(&self, secret: &ClientSecret) -> Result<bool> {
        let text = Zeroizing::new(B64.encode(secret.as_bytes()));
        self.store
            .add_if_absent(&self.account, &Secret::from_bytes(text.as_bytes()))
            .map_err(|e| Error::ClientSecret(e.to_string()))
    }

    /// Remove this device's client secret (sign-out everywhere on this device). The blob in
    /// the cloud folder is then unreadable here until the secret is installed again.
    pub fn delete(&self) -> Result<bool> {
        self.store
            .delete(&self.account)
            .map_err(|e| Error::ClientSecret(e.to_string()))
    }
}

impl ClientSecretStore for CredentialsClientSecret {
    fn get(&self) -> Result<Option<ClientSecret>> {
        match self
            .store
            .get(&self.account)
            .map_err(|e| Error::ClientSecret(e.to_string()))?
        {
            Some(s) if !s.is_empty() => Self::decode(&s).map(Some),
            _ => Ok(None),
        }
    }

    fn get_or_create(&self) -> Result<ClientSecret> {
        let (secret, _created) = self
            .store
            .get_or_create_key(&self.account, KEY_LEN)
            .map_err(|e| Error::ClientSecret(e.to_string()))?;
        Self::decode(&secret)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_credentials::{Config, MemoryBackend, Namespace};
    use std::sync::Arc;

    fn store(name: &str) -> Store {
        let ns = Namespace::new(format!("dev.rightkit.vault.test.{name}")).unwrap();
        Store::open(Config::new(ns).backend(Arc::new(MemoryBackend::new())))
    }

    fn keys() -> ServerKeys {
        ServerKeys {
            current: ServerKey::new("k2", [2; 32]),
            previous: vec![ServerKey::new("k1", [1; 32])],
        }
    }

    #[test]
    fn debug_never_shows_key_bytes() {
        let k = ServerKey::new("k1", [0xAB; 32]);
        let s = format!("{k:?} {:?}", ClientSecret::from_bytes([0xCD; 32]));
        assert!(!s.contains("171") && !s.contains("205") && !s.contains("ab") && !s.contains("cd"));
        assert!(s.contains("redacted"));
    }

    #[test]
    fn cache_codec_roundtrip() {
        let entry = CachedKeys {
            keys: keys(),
            fetched_at: 1234,
        };
        let got = decode_cache(&encode_cache(&entry)).unwrap();
        assert_eq!(got.fetched_at, 1234);
        assert_eq!(got.keys.current.id(), "k2");
        assert_eq!(got.keys.current.bytes(), &[2; 32]);
        assert_eq!(got.keys.previous.len(), 1);
        assert_eq!(got.keys.previous[0].bytes(), &[1; 32]);
    }

    #[test]
    fn cache_codec_rejects_garbage() {
        let entry = CachedKeys {
            keys: keys(),
            fetched_at: 1,
        };
        let good = encode_cache(&entry);
        assert!(decode_cache(&[]).is_none());
        assert!(decode_cache(&good[..good.len() - 1]).is_none());
        let mut trailing = good.to_vec();
        trailing.push(0);
        assert!(decode_cache(&trailing).is_none());
        let mut bad_format = good.to_vec();
        bad_format[0] = 9;
        assert!(decode_cache(&bad_format).is_none());
    }

    #[test]
    fn credentials_cache_roundtrip_clear_and_garbage_is_a_miss() {
        let st = store("cache");
        let cache = CredentialsKeyCache::new(st.clone(), "app", "sub");
        assert!(cache.load().unwrap().is_none());
        cache
            .store(&CachedKeys {
                keys: keys(),
                fetched_at: 77,
            })
            .unwrap();
        let got = cache.load().unwrap().unwrap();
        assert_eq!(got.fetched_at, 77);
        assert!(got.keys.get("k1").is_some() && got.keys.get("k2").is_some());
        assert!(got.keys.get("k3").is_none());
        cache.clear().unwrap();
        assert!(cache.load().unwrap().is_none());
        st.set(
            "vault-keys.app.sub",
            &Secret::from_string("not a cache entry"),
        )
        .unwrap();
        assert!(cache.load().unwrap().is_none());
    }

    #[test]
    fn client_secret_is_created_once_and_installs_never_replace() {
        let st = store("client");
        let c = CredentialsClientSecret::new(st, "app", "sub");
        assert!(c.get().unwrap().is_none());
        let a = c.get_or_create().unwrap();
        let b = c.get_or_create().unwrap();
        assert_eq!(a.as_bytes(), b.as_bytes());
        assert_eq!(c.get().unwrap().unwrap().as_bytes(), a.as_bytes());
        let other = ClientSecret::from_bytes([5; 32]);
        assert!(!c.install_if_absent(&other).unwrap());
        assert_eq!(c.get().unwrap().unwrap().as_bytes(), a.as_bytes());
        assert!(c.delete().unwrap());
        assert!(c.install_if_absent(&other).unwrap());
        assert_eq!(c.get().unwrap().unwrap().as_bytes(), &[5; 32]);
    }

    #[test]
    fn malformed_client_secret_is_an_error_not_a_fresh_one() {
        let st = store("malformed");
        st.set("vault-client.app.sub", &Secret::from_string("AAAA"))
            .unwrap();
        let c = CredentialsClientSecret::new(st, "app", "sub");
        assert!(matches!(c.get(), Err(Error::ClientSecret(_))));
        assert!(matches!(c.get_or_create(), Err(Error::ClientSecret(_))));
    }

    #[test]
    fn key_id_validation() {
        assert!(valid_key_id("k1").is_ok());
        assert!(valid_key_id("").is_err());
        assert!(valid_key_id("a\nb").is_err());
        assert!(valid_key_id(&"x".repeat(129)).is_err());
    }
}
