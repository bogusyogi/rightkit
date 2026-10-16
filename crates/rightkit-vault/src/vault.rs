//! The vault: server-key acquisition (cache first, service second), blob I/O, and
//! load / save / rotate / reset.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rightkit_credentials::Secret;
use serde::ser::{Error as _, SerializeMap};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zeroize::Zeroizing;

use crate::blob::{open_blob, rewrap, seal_blob, Blob};
use crate::error::{Error, Result};
use crate::keys::{CachedKeys, ClientSecretStore, ServerKeyCache, ServerKeySource, ServerKeys};

pub type SecretMap = BTreeMap<String, Secret>;
/// Supplies a fresh Google ID token. Called only when the service must be contacted.
pub type IdToken<'a> = &'a dyn Fn() -> Result<String>;

/// Largest payload `save` accepts. The vault is for a handful of API keys.
pub const MAX_PAYLOAD_BYTES: usize = 256 * 1024;
/// Largest blob `load` will parse.
const MAX_BLOB_BYTES: usize = 1024 * 1024;

/// Where the sealed blob lives: one small file in the user's own cloud app folder (a Drive
/// `appDataFolder` file, a CloudKit record, ...). The store only ever sees the sealed blob.
///
/// `write` replaces the whole blob. Concurrent writers are last-writer-wins unless the
/// implementation adds compare-and-swap; the vault does not detect a rolled-back blob.
pub trait BlobStore: Send + Sync {
    fn read(&self) -> Result<Option<Vec<u8>>>;
    fn write(&self, bytes: &[u8]) -> Result<()>;
}

#[derive(Debug, Clone)]
pub struct Config {
    /// App id, bound into the wrap key derivation and both AADs.
    pub app: String,
    /// Stable Google subject (`sub`), bound the same way.
    pub subject: String,
    /// How long a cached server key is trusted without the service re-confirming it (the
    /// longest a revocation or lapse can go unnoticed on an offline device). Default 7 days.
    pub offline_grace_secs: u64,
}

impl Config {
    pub fn new(app: impl Into<String>, subject: impl Into<String>) -> Self {
        Self {
            app: app.into(),
            subject: subject.into(),
            offline_grace_secs: 7 * 24 * 3600,
        }
    }
}

// ---- payload codec: no plain String ever holds a secret value ---------------------------

/// A JSON string value decoded straight into a zeroing `Secret`. (serde_json only builds an
/// intermediate buffer for strings that contain escape sequences.)
struct SecretValue(Secret);

impl<'de> Deserialize<'de> for SecretValue {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = SecretValue;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a string")
            }
            fn visit_str<E: serde::de::Error>(
                self,
                s: &str,
            ) -> std::result::Result<SecretValue, E> {
                Ok(SecretValue(Secret::from_bytes(s.as_bytes())))
            }
        }
        d.deserialize_str(V)
    }
}

struct Payload<'a>(&'a SecretMap);

impl Serialize for Payload<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(self.0.len()))?;
        for (name, value) in self.0 {
            let text = value
                .expose_str()
                .map_err(|_| S::Error::custom("secret is not valid utf-8"))?;
            map.serialize_entry(name, text)?;
        }
        map.end()
    }
}

fn encode_payload(secrets: &SecretMap) -> Result<Zeroizing<Vec<u8>>> {
    // Worst case every byte is a \u00XX escape: size once so the buffer never reallocates
    // (a realloc would leave an unzeroed copy of the plaintext behind).
    let cap: usize = secrets
        .iter()
        .map(|(k, v)| 6 * (k.len() + v.len()) + 8)
        .sum::<usize>()
        + 2;
    let mut buf = Zeroizing::new(Vec::with_capacity(cap));
    serde_json::to_writer(&mut *buf, &Payload(secrets))
        .map_err(|_| Error::Format("payload cannot be encoded".into()))?;
    if buf.len() > MAX_PAYLOAD_BYTES {
        return Err(Error::Format("payload too large".into()));
    }
    Ok(buf)
}

fn decode_payload(plain: &[u8]) -> Result<SecretMap> {
    // Fixed error text: serde's own messages can quote the value they failed on.
    let map: BTreeMap<String, SecretValue> = serde_json::from_slice(plain)
        .map_err(|_| Error::Format("payload is not a string map".into()))?;
    Ok(map.into_iter().map(|(k, v)| (k, v.0)).collect())
}

fn parse_blob(bytes: &[u8]) -> Result<Blob> {
    if bytes.len() > MAX_BLOB_BYTES {
        return Err(Error::Format("blob too large".into()));
    }
    serde_json::from_slice(bytes).map_err(|_| Error::Format("blob is not valid json".into()))
}

fn encode_blob(blob: &Blob) -> Result<Vec<u8>> {
    serde_json::to_vec(blob).map_err(|_| Error::Format("blob cannot be encoded".into()))
}

// ---- the vault -----------------------------------------------------------------------------

pub struct Vault {
    cfg: Config,
    source: Arc<dyn ServerKeySource>,
    cache: Arc<dyn ServerKeyCache>,
    client: Arc<dyn ClientSecretStore>,
    blobs: Arc<dyn BlobStore>,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl Vault {
    pub fn new(
        cfg: Config,
        source: Arc<dyn ServerKeySource>,
        cache: Arc<dyn ServerKeyCache>,
        client: Arc<dyn ClientSecretStore>,
        blobs: Arc<dyn BlobStore>,
    ) -> Self {
        let clock = Arc::new(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        });
        Self {
            cfg,
            source,
            cache,
            client,
            blobs,
            clock,
        }
    }

    /// Replace the clock (tests).
    pub fn with_clock(mut self, clock: Arc<dyn Fn() -> u64 + Send + Sync>) -> Self {
        self.clock = clock;
        self
    }

    fn now(&self) -> u64 {
        (self.clock)()
    }

    /// Decrypt the stored map. `Ok(None)` = no blob yet. Works offline from the cached server
    /// key while it is inside the grace window.
    pub fn load(&self, token: IdToken) -> Result<Option<SecretMap>> {
        let Some(bytes) = self.blobs.read()? else {
            return Ok(None);
        };
        let blob = parse_blob(&bytes)?;
        let client = self.client.get()?.ok_or(Error::ClientSecretMissing)?;
        let keys = self.keys_for(token, Some(&blob.key_id))?;
        let key = keys.get(&blob.key_id).ok_or(Error::KeyUnavailable)?;
        let plain = open_blob(&blob, key, &client, &self.cfg.app, &self.cfg.subject)?;
        decode_payload(&plain).map(Some)
    }

    /// Seal under a fresh DEK and the current server key, then write. Also the DEK-rotation
    /// operation.
    ///
    /// An existing blob must open with the halves this device holds before it is replaced:
    /// a device without the client secret, a blob it cannot authenticate, or a blob from a
    /// newer format is refused rather than clobbered. `reset` is the explicit way to discard.
    pub fn save(&self, token: IdToken, secrets: &SecretMap) -> Result<()> {
        let plain = encode_payload(secrets)?;
        let existing = match self.blobs.read()? {
            Some(bytes) => Some(parse_blob(&bytes)?),
            None => None,
        };
        let client = if existing.is_some() {
            self.client.get()?.ok_or(Error::ClientSecretMissing)?
        } else {
            self.client.get_or_create()?
        };
        let keys = self.keys_for(token, existing.as_ref().map(|b| b.key_id.as_str()))?;
        if let Some(old) = &existing {
            let key = keys.get(&old.key_id).ok_or(Error::KeyUnavailable)?;
            open_blob(old, key, &client, &self.cfg.app, &self.cfg.subject)?;
        }
        let blob = seal_blob(
            &plain,
            &keys.current,
            &client,
            &self.cfg.app,
            &self.cfg.subject,
        )?;
        self.blobs.write(&encode_blob(&blob)?)
    }

    /// Server-key rotation: force a service fetch and re-wrap the DEK if the blob is under an
    /// older key. `Ok(true)` = blob rewritten.
    pub fn rotate(&self, token: IdToken) -> Result<bool> {
        let keys = self.fetch_and_cache(token)?;
        let Some(bytes) = self.blobs.read()? else {
            return Ok(false);
        };
        let blob = parse_blob(&bytes)?;
        if blob.key_id == keys.current.id() {
            return Ok(false);
        }
        let old = keys.get(&blob.key_id).ok_or(Error::KeyUnavailable)?;
        let client = self.client.get()?.ok_or(Error::ClientSecretMissing)?;
        let out = rewrap(
            &blob,
            old,
            &keys.current,
            &client,
            &self.cfg.app,
            &self.cfg.subject,
        )?;
        self.blobs.write(&encode_blob(&out)?)?;
        Ok(true)
    }

    /// Start over after `Error::Revoked` (vault erased on the service) or when the blob can
    /// no longer be opened. DESTRUCTIVE: asks the service to re-initialise (new server key),
    /// discards the old blob (unreadable by design) and saves a fresh empty vault. The client
    /// secret is kept (created if this device has none). Call it only after the user has
    /// confirmed losing the stored secrets. If the final write fails, call it again.
    pub fn reset(&self, token: IdToken) -> Result<()> {
        let tok = Zeroizing::new(token()?);
        let keys = match self.source.reset(&tok) {
            Ok(k) => k,
            Err(e @ (Error::Revoked | Error::Denied)) => {
                let _ = self.cache.clear();
                return Err(e);
            }
            Err(e) => return Err(e),
        };
        self.remember(&keys);
        let client = self.client.get_or_create()?;
        let blob = seal_blob(
            &b"{}"[..],
            &keys.current,
            &client,
            &self.cfg.app,
            &self.cfg.subject,
        )?;
        self.blobs.write(&encode_blob(&blob)?)
    }

    /// Force-confirm with the service (detects a lapse or erase now, not at grace expiry).
    pub fn refresh(&self, token: IdToken) -> Result<()> {
        self.fetch_and_cache(token).map(|_| ())
    }

    /// Drop the cached server key (sign-out). The blob stays in the cloud folder and the
    /// client secret stays on this device; decrypting again needs the service. To also drop
    /// the client secret use `CredentialsClientSecret::delete`.
    pub fn sign_out(&self) -> Result<()> {
        self.cache.clear()
    }

    /// Best effort: a failed cache write (locked or missing keychain) costs offline use, not
    /// correctness, so it does not fail the call.
    fn remember(&self, keys: &ServerKeys) {
        let _ = self.cache.store(&CachedKeys {
            keys: keys.clone(),
            fetched_at: self.now(),
        });
    }

    fn fetch_and_cache(&self, token: IdToken) -> Result<ServerKeys> {
        let tok = Zeroizing::new(token()?);
        match self.source.fetch(&tok) {
            Ok(keys) => {
                self.remember(&keys);
                Ok(keys)
            }
            // Not entitled or erased: stop trusting the cached key at once.
            Err(e @ (Error::Revoked | Error::Denied)) => {
                let _ = self.cache.clear();
                Err(e)
            }
            Err(e) => Err(e),
        }
    }

    /// Cache first (inside the grace window, holding the wanted key id), service otherwise.
    /// Past the grace with no service the call fails closed with `Offline`. A cache that
    /// cannot be read counts as a miss. A cache timestamp in the future (clock set back) is
    /// not trusted.
    fn keys_for(&self, token: IdToken, want: Option<&str>) -> Result<ServerKeys> {
        if let Ok(Some(cached)) = self.cache.load() {
            let now = self.now();
            let fresh =
                now >= cached.fetched_at && now - cached.fetched_at <= self.cfg.offline_grace_secs;
            let has_wanted = match want {
                Some(id) => cached.keys.get(id).is_some(),
                None => true,
            };
            if fresh && has_wanted {
                return Ok(cached.keys);
            }
        }
        self.fetch_and_cache(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{ClientSecret, ServerKey};
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemBlob(Mutex<Option<Vec<u8>>>);
    impl BlobStore for MemBlob {
        fn read(&self) -> Result<Option<Vec<u8>>> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn write(&self, b: &[u8]) -> Result<()> {
            *self.0.lock().unwrap() = Some(b.to_vec());
            Ok(())
        }
    }

    #[derive(Default)]
    struct MemCache(Mutex<Option<CachedKeys>>);
    impl ServerKeyCache for MemCache {
        fn load(&self) -> Result<Option<CachedKeys>> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn store(&self, e: &CachedKeys) -> Result<()> {
            *self.0.lock().unwrap() = Some(e.clone());
            Ok(())
        }
        fn clear(&self) -> Result<()> {
            *self.0.lock().unwrap() = None;
            Ok(())
        }
    }

    #[derive(Default)]
    struct MemClient(Mutex<Option<ClientSecret>>);
    impl ClientSecretStore for MemClient {
        fn get(&self) -> Result<Option<ClientSecret>> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn get_or_create(&self) -> Result<ClientSecret> {
            let mut g = self.0.lock().unwrap();
            if g.is_none() {
                *g = Some(ClientSecret::generate()?);
            }
            Ok(g.clone().unwrap())
        }
    }

    #[derive(Clone)]
    enum Mode {
        Serve(ServerKeys),
        Denied,
        Revoked,
        Offline,
    }

    struct MockSource {
        mode: Mutex<Mode>,
        reset_to: Mutex<ServerKeys>,
        calls: Mutex<u32>,
    }
    impl MockSource {
        fn new(cur: &str, b: u8) -> Arc<Self> {
            Arc::new(Self {
                mode: Mutex::new(Mode::Serve(bundle(cur, b))),
                reset_to: Mutex::new(bundle("k-reset", 99)),
                calls: Mutex::new(0),
            })
        }
        fn set(&self, m: Mode) {
            *self.mode.lock().unwrap() = m;
        }
        fn calls(&self) -> u32 {
            *self.calls.lock().unwrap()
        }
    }
    impl ServerKeySource for MockSource {
        fn fetch(&self, tok: &str) -> Result<ServerKeys> {
            assert_eq!(tok, "tok");
            *self.calls.lock().unwrap() += 1;
            match self.mode.lock().unwrap().clone() {
                Mode::Serve(b) => Ok(b),
                Mode::Denied => Err(Error::Denied),
                Mode::Revoked => Err(Error::Revoked),
                Mode::Offline => Err(Error::Offline),
            }
        }
        fn reset(&self, tok: &str) -> Result<ServerKeys> {
            assert_eq!(tok, "tok");
            let fresh = self.reset_to.lock().unwrap().clone();
            // After a reset the service serves the new key.
            *self.mode.lock().unwrap() = Mode::Serve(fresh.clone());
            Ok(fresh)
        }
    }

    fn bundle(id: &str, b: u8) -> ServerKeys {
        ServerKeys {
            current: ServerKey::new(id, [b; 32]),
            previous: vec![],
        }
    }

    fn tok() -> Result<String> {
        Ok("tok".into())
    }

    fn secrets() -> SecretMap {
        let mut m = SecretMap::new();
        m.insert("openai".into(), Secret::from_string("sk-test-1"));
        m
    }

    struct Rig {
        vault: Vault,
        src: Arc<MockSource>,
        cache: Arc<MemCache>,
        client: Arc<MemClient>,
        blobs: Arc<MemBlob>,
        now: Arc<Mutex<u64>>,
    }

    fn vault_over(
        sub: &str,
        src: Arc<MockSource>,
        cache: Arc<MemCache>,
        client: Arc<MemClient>,
        blobs: Arc<MemBlob>,
        now: Arc<Mutex<u64>>,
    ) -> Vault {
        Vault::new(Config::new("heardright", sub), src, cache, client, blobs)
            .with_clock(Arc::new(move || *now.lock().unwrap()))
    }

    fn rig() -> Rig {
        let src = MockSource::new("k1", 1);
        let cache = Arc::new(MemCache::default());
        let client = Arc::new(MemClient::default());
        let blobs = Arc::new(MemBlob::default());
        let now = Arc::new(Mutex::new(1_000u64));
        let vault = vault_over(
            "sub-1",
            src.clone(),
            cache.clone(),
            client.clone(),
            blobs.clone(),
            now.clone(),
        );
        Rig {
            vault,
            src,
            cache,
            client,
            blobs,
            now,
        }
    }

    fn raw_blob(r: &Rig) -> Vec<u8> {
        r.blobs.read().unwrap().unwrap()
    }

    #[test]
    fn save_load_roundtrip_and_blob_has_no_plaintext_or_names() {
        let r = rig();
        r.vault.save(&tok, &secrets()).unwrap();
        let raw = String::from_utf8(raw_blob(&r)).unwrap();
        assert!(!raw.contains("sk-test-1") && !raw.contains("openai"));
        let got = r.vault.load(&tok).unwrap().unwrap();
        assert_eq!(got["openai"].expose_str().unwrap(), "sk-test-1");
    }

    #[test]
    fn payload_with_escapes_and_unicode_roundtrips() {
        let r = rig();
        let mut m = SecretMap::new();
        m.insert(
            "a\"b".into(),
            Secret::from_string("line1\nline2\t\u{1}é漢\"\\"),
        );
        r.vault.save(&tok, &m).unwrap();
        let got = r.vault.load(&tok).unwrap().unwrap();
        assert_eq!(
            got["a\"b"].expose_str().unwrap(),
            "line1\nline2\t\u{1}é漢\"\\"
        );
    }

    #[test]
    fn non_utf8_secret_is_refused() {
        let r = rig();
        let mut m = SecretMap::new();
        m.insert("bin".into(), Secret::from_bytes(vec![0xff, 0xfe]));
        assert!(matches!(r.vault.save(&tok, &m), Err(Error::Format(_))));
        assert!(r.blobs.read().unwrap().is_none());
    }

    #[test]
    fn load_uses_cache_without_service() {
        let r = rig();
        r.vault.save(&tok, &secrets()).unwrap();
        let before = r.src.calls();
        r.src.set(Mode::Offline);
        r.vault.load(&tok).unwrap().unwrap();
        assert_eq!(r.src.calls(), before);
    }

    #[test]
    fn grace_expiry_fails_closed_offline_then_recovers() {
        let r = rig();
        r.vault.save(&tok, &secrets()).unwrap();
        *r.now.lock().unwrap() += 8 * 24 * 3600;
        r.src.set(Mode::Offline);
        assert!(matches!(r.vault.load(&tok), Err(Error::Offline)));
        r.src.set(Mode::Serve(bundle("k1", 1)));
        assert!(r.vault.load(&tok).unwrap().is_some());
    }

    #[test]
    fn clock_set_back_does_not_extend_the_grace() {
        let r = rig();
        r.vault.save(&tok, &secrets()).unwrap();
        *r.now.lock().unwrap() -= 500;
        r.src.set(Mode::Offline);
        assert!(matches!(r.vault.load(&tok), Err(Error::Offline)));
    }

    #[test]
    fn token_closure_is_called_only_when_the_service_is_needed() {
        let r = rig();
        r.vault.save(&tok, &secrets()).unwrap();
        let no_token = || -> Result<String> { panic!("token requested although cache is fresh") };
        r.vault.load(&no_token).unwrap().unwrap();
    }

    #[test]
    fn revoked_clears_cache_and_blocks_decrypt() {
        let r = rig();
        r.vault.save(&tok, &secrets()).unwrap();
        r.src.set(Mode::Revoked);
        assert!(matches!(r.vault.refresh(&tok), Err(Error::Revoked)));
        assert!(r.cache.load().unwrap().is_none());
        assert!(matches!(r.vault.load(&tok), Err(Error::Revoked)));
    }

    #[test]
    fn lapse_denies_then_restored_entitlement_reads_the_same_blob() {
        let r = rig();
        r.vault.save(&tok, &secrets()).unwrap();
        r.src.set(Mode::Denied);
        assert!(matches!(r.vault.refresh(&tok), Err(Error::Denied)));
        assert!(r.cache.load().unwrap().is_none());
        assert!(matches!(r.vault.load(&tok), Err(Error::Denied)));
        r.src.set(Mode::Serve(bundle("k1", 1)));
        let got = r.vault.load(&tok).unwrap().unwrap();
        assert_eq!(got["openai"].expose_str().unwrap(), "sk-test-1");
    }

    #[test]
    fn blob_plus_wrong_server_key_or_wrong_client_secret_cannot_decrypt() {
        let r = rig();
        r.vault.save(&tok, &secrets()).unwrap();
        // Attacker with the blob and the service key of the same id (say, a colluding
        // service) but no client secret on the device.
        let no_client = vault_over(
            "sub-1",
            r.src.clone(),
            Arc::new(MemCache::default()),
            Arc::new(MemClient::default()),
            r.blobs.clone(),
            r.now.clone(),
        );
        assert!(matches!(
            no_client.load(&tok),
            Err(Error::ClientSecretMissing)
        ));
        // Attacker with the blob and some other client secret.
        let other_client = Arc::new(MemClient::default());
        other_client.get_or_create().unwrap();
        let wrong_client = vault_over(
            "sub-1",
            r.src.clone(),
            Arc::new(MemCache::default()),
            other_client,
            r.blobs.clone(),
            r.now.clone(),
        );
        assert!(matches!(wrong_client.load(&tok), Err(Error::Decrypt)));
        // Attacker with the blob, the right client secret and a wrong server key.
        let wrong_server = vault_over(
            "sub-1",
            MockSource::new("k1", 9),
            Arc::new(MemCache::default()),
            r.client.clone(),
            r.blobs.clone(),
            r.now.clone(),
        );
        assert!(matches!(wrong_server.load(&tok), Err(Error::Decrypt)));
    }

    #[test]
    fn save_never_overwrites_a_blob_it_cannot_open() {
        let r = rig();
        r.vault.save(&tok, &secrets()).unwrap();
        let before = raw_blob(&r);

        // New device: no client secret.
        let fresh_device = vault_over(
            "sub-1",
            r.src.clone(),
            Arc::new(MemCache::default()),
            Arc::new(MemClient::default()),
            r.blobs.clone(),
            r.now.clone(),
        );
        assert!(matches!(
            fresh_device.save(&tok, &secrets()),
            Err(Error::ClientSecretMissing)
        ));
        assert_eq!(raw_blob(&r), before);

        // Device with a different client secret.
        let other_client = Arc::new(MemClient::default());
        other_client.get_or_create().unwrap();
        let wrong_device = vault_over(
            "sub-1",
            r.src.clone(),
            Arc::new(MemCache::default()),
            other_client,
            r.blobs.clone(),
            r.now.clone(),
        );
        assert!(matches!(
            wrong_device.save(&tok, &secrets()),
            Err(Error::Decrypt)
        ));
        assert_eq!(raw_blob(&r), before);

        // A blob from a newer format is not clobbered either.
        let mut newer: Blob = serde_json::from_slice(&before).unwrap();
        newer.v = 2;
        r.blobs.write(&serde_json::to_vec(&newer).unwrap()).unwrap();
        assert!(matches!(
            r.vault.save(&tok, &secrets()),
            Err(Error::Version(2))
        ));
    }

    #[test]
    fn second_device_with_the_installed_client_secret_reads_and_writes() {
        let r = rig();
        r.vault.save(&tok, &secrets()).unwrap();
        let second_client = Arc::new(MemClient::default());
        *second_client.0.lock().unwrap() = r.client.get().unwrap();
        let second = vault_over(
            "sub-1",
            r.src.clone(),
            Arc::new(MemCache::default()),
            second_client,
            r.blobs.clone(),
            r.now.clone(),
        );
        assert_eq!(
            second.load(&tok).unwrap().unwrap()["openai"]
                .expose_str()
                .unwrap(),
            "sk-test-1"
        );
        let mut m = secrets();
        m.insert("anthropic".into(), Secret::from_string("sk-ant-2"));
        second.save(&tok, &m).unwrap();
        assert_eq!(r.vault.load(&tok).unwrap().unwrap().len(), 2);
    }

    #[test]
    fn each_save_uses_a_fresh_dek_and_nonce() {
        let r = rig();
        r.vault.save(&tok, &secrets()).unwrap();
        let a: Blob = serde_json::from_slice(&raw_blob(&r)).unwrap();
        r.vault.save(&tok, &secrets()).unwrap();
        let b: Blob = serde_json::from_slice(&raw_blob(&r)).unwrap();
        assert_ne!(a.data, b.data);
        assert_ne!(a.wrapped_dek, b.wrapped_dek);
    }

    #[test]
    fn rotate_rewraps_and_load_still_works() {
        let r = rig();
        r.vault.save(&tok, &secrets()).unwrap();
        r.src.set(Mode::Serve(ServerKeys {
            current: ServerKey::new("k2", [2; 32]),
            previous: vec![ServerKey::new("k1", [1; 32])],
        }));
        let data_before = serde_json::from_slice::<Blob>(&raw_blob(&r)).unwrap().data;
        assert!(r.vault.rotate(&tok).unwrap());
        assert!(!r.vault.rotate(&tok).unwrap());
        let got = r.vault.load(&tok).unwrap().unwrap();
        assert_eq!(got["openai"].expose_str().unwrap(), "sk-test-1");
        let blob: Blob = serde_json::from_slice(&raw_blob(&r)).unwrap();
        assert_eq!(blob.key_id, "k2");
        assert_eq!(blob.data, data_before);
    }

    #[test]
    fn retired_key_is_key_unavailable() {
        let r = rig();
        r.vault.save(&tok, &secrets()).unwrap();
        // Service rotated and dropped k1 before this client re-wrapped.
        r.src.set(Mode::Serve(bundle("k2", 2)));
        assert!(matches!(r.vault.rotate(&tok), Err(Error::KeyUnavailable)));
        r.cache.clear().unwrap();
        assert!(matches!(r.vault.load(&tok), Err(Error::KeyUnavailable)));
        assert!(matches!(
            r.vault.save(&tok, &secrets()),
            Err(Error::KeyUnavailable)
        ));
    }

    #[test]
    fn other_subject_cannot_open() {
        let r = rig();
        r.vault.save(&tok, &secrets()).unwrap();
        let v2 = vault_over(
            "sub-2",
            r.src.clone(),
            Arc::new(MemCache::default()),
            r.client.clone(),
            r.blobs.clone(),
            r.now.clone(),
        );
        assert!(matches!(v2.load(&tok), Err(Error::Decrypt)));
    }

    #[test]
    fn empty_store_loads_none() {
        let r = rig();
        assert!(r.vault.load(&tok).unwrap().is_none());
    }

    #[test]
    fn reset_after_erase_starts_a_fresh_empty_vault_and_discards_the_old_one() {
        let r = rig();
        r.vault.save(&tok, &secrets()).unwrap();
        r.src.set(Mode::Revoked);
        assert!(matches!(r.vault.refresh(&tok), Err(Error::Revoked)));
        assert!(matches!(r.vault.load(&tok), Err(Error::Revoked)));
        let old = raw_blob(&r);

        r.vault.reset(&tok).unwrap();
        assert_ne!(raw_blob(&r), old);
        let got = r.vault.load(&tok).unwrap().unwrap();
        assert!(got.is_empty());
        let blob: Blob = serde_json::from_slice(&raw_blob(&r)).unwrap();
        assert_eq!(blob.key_id, "k-reset");

        // The old blob is unreadable under the new service state, by design.
        r.blobs.write(&old).unwrap();
        assert!(matches!(r.vault.load(&tok), Err(Error::KeyUnavailable)));

        // And a normal save works again after a reset.
        r.vault.reset(&tok).unwrap();
        r.vault.save(&tok, &secrets()).unwrap();
        assert_eq!(
            r.vault.load(&tok).unwrap().unwrap()["openai"]
                .expose_str()
                .unwrap(),
            "sk-test-1"
        );
    }

    #[test]
    fn reset_keeps_the_client_secret() {
        let r = rig();
        r.vault.save(&tok, &secrets()).unwrap();
        let before = *r.client.get().unwrap().unwrap().as_bytes();
        r.vault.reset(&tok).unwrap();
        assert_eq!(*r.client.get().unwrap().unwrap().as_bytes(), before);
    }

    #[test]
    fn sign_out_drops_the_cache_and_needs_the_service_again() {
        let r = rig();
        r.vault.save(&tok, &secrets()).unwrap();
        r.vault.sign_out().unwrap();
        r.src.set(Mode::Offline);
        assert!(matches!(r.vault.load(&tok), Err(Error::Offline)));
    }

    #[test]
    fn garbage_blob_is_a_format_error_and_save_refuses_to_replace_it() {
        let r = rig();
        r.blobs.write(b"not json").unwrap();
        assert!(matches!(r.vault.load(&tok), Err(Error::Format(_))));
        assert!(matches!(
            r.vault.save(&tok, &secrets()),
            Err(Error::Format(_))
        ));
        r.vault.reset(&tok).unwrap();
        assert!(r.vault.load(&tok).unwrap().unwrap().is_empty());
    }

    #[test]
    fn errors_never_contain_secret_values() {
        let r = rig();
        r.vault.save(&tok, &secrets()).unwrap();
        r.src.set(Mode::Serve(bundle("k1", 9)));
        r.cache.clear().unwrap();
        let e = r.vault.load(&tok).unwrap_err();
        let shown = format!("{e} {e:?}");
        assert!(!shown.contains("sk-test-1"));
    }
}
