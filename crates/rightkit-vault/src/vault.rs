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
