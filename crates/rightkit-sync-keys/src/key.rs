//! The account data key, its identifier, and per-zone key derivation.

use crate::error::{Error, Result};
use crate::util::{hex_decode, hex_encode, push_lp, random_array};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// Length of every key in this crate, in bytes.
pub const KEY_LEN: usize = 32;

const KEY_ID_INFO: &[u8] = b"rightkit-sync-keys/v1/key-id";
const ZONE_INFO: &[u8] = b"rightkit-sync-keys/v1/zone";
const SCOPE_DOMAIN: &[u8] = b"rightkit-sync-keys/v1/account-scope";

/// Identifies one data key without revealing it: `k` plus 16 hex characters from an HKDF expand
/// of the key under a fixed label. Every device holding the key computes the same id; a reset
/// (a new random key) or a rotation yields a different one. Safe to store on the server and to
/// bind into an account scope.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyId(String);

impl KeyId {
    /// Parse the text form (`k` + 16 lower-case hex characters).
    pub fn parse(s: &str) -> Result<KeyId> {
        let ok = s.len() == 17
            && s.starts_with('k')
            && hex_decode(&s[1..]).is_some_and(|b| b.len() == 8);
        if ok {
            Ok(KeyId(s.to_string()))
        } else {
            Err(Error::Invalid {
                what: "key id",
                reason: "expected k followed by 16 lower-case hex characters".into(),
            })
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for KeyId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Non-empty, at most 128 bytes, no control characters. Applied to app, zone and account ids
/// before they enter a credential-store account name or a key derivation.
pub(crate) fn check_component(what: &'static str, v: &str) -> Result<()> {
    if v.is_empty() || v.len() > 128 || v.chars().any(char::is_control) {
        return Err(Error::Invalid {
            what,
            reason: "must be 1..=128 bytes with no control characters".into(),
        });
    }
    Ok(())
}

/// The account data key ("root key" in the design): 32 random bytes generated once on the first
/// device, a monotonically increasing `version` that rotation bumps, and the derived [`KeyId`].
/// Zeroed on drop. `Debug` is redacted; there is no `Display`, no serde.
#[derive(Clone)]
pub struct DataKey {
    version: u32,
    id: KeyId,
    bytes: Zeroizing<[u8; KEY_LEN]>,
}

impl DataKey {
    /// A fresh random key at `version` (must be at least 1).
    pub fn generate(version: u32) -> Result<DataKey> {
        let bytes = Zeroizing::new(random_array::<KEY_LEN>()?);
        DataKey::from_bytes(version, bytes)
    }

    /// Wrap existing key bytes (a transferred or stored key).
    pub fn from_bytes(version: u32, bytes: Zeroizing<[u8; KEY_LEN]>) -> Result<DataKey> {
        if version == 0 {
            return Err(Error::Invalid {
                what: "key version",
                reason: "versions start at 1".into(),
            });
        }
        let id = derive_key_id(&bytes);
        Ok(DataKey { version, id, bytes })
    }

    pub fn version(&self) -> u32 {
        self.version
    }
    pub fn key_id(&self) -> &KeyId {
        &self.id
    }
    /// The raw key. Only the transfer and storage paths of this crate, and a future recovery
    /// wrap, need it; callers that merely encrypt use [`DataKey::zone_key`].
    pub fn expose_bytes(&self) -> &[u8; KEY_LEN] {
        &self.bytes
    }

    /// Per-zone encryption key: HKDF-SHA256(ikm = data key, no salt, info = label plus
    /// length-prefixed `app` and `zone`). Distinct `(app, zone)` pairs give unrelated keys.
    pub fn zone_key(&self, app: &str, zone: &str) -> Result<ZoneKey> {
        check_component("app", app)?;
        check_component("zone", zone)?;
        let mut info = Vec::with_capacity(ZONE_INFO.len() + 8 + app.len() + zone.len());
        info.extend_from_slice(ZONE_INFO);
        push_lp(&mut info, app.as_bytes());
        push_lp(&mut info, zone.as_bytes());
        let mut out = Zeroizing::new([0u8; KEY_LEN]);
        Hkdf::<Sha256>::new(None, &self.bytes[..])
            .expand(&info, &mut out[..])
            .map_err(|_| Error::Invalid {
                what: "zone key",
                reason: "derivation failed".into(),
            })?;
        Ok(ZoneKey {
            key_id: self.id.clone(),
            version: self.version,
            bytes: out,
        })
    }
}

impl std::fmt::Debug for DataKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "DataKey(v{}, {}, <redacted {} bytes>)",
            self.version, self.id, KEY_LEN
        )
    }
}

fn derive_key_id(bytes: &[u8; KEY_LEN]) -> KeyId {
    let mut out = [0u8; 8];
    // Expanding 8 bytes from a 32-byte PRK can never exceed the HKDF output limit.
    Hkdf::<Sha256>::new(None, &bytes[..])
        .expand(KEY_ID_INFO, &mut out)
        .expect("8-byte HKDF output is within limits");
    KeyId(format!("k{}", hex_encode(&out)))
}

/// A key for one `(app, zone)` derived from a [`DataKey`]. This is what an AEAD record cipher
/// uses. Zeroed on drop, redacted `Debug`.
#[derive(Clone)]
pub struct ZoneKey {
    key_id: KeyId,
    version: u32,
    bytes: Zeroizing<[u8; KEY_LEN]>,
}

impl ZoneKey {
    /// Id of the data key this was derived from. Seal it next to the ciphertext so a reader
    /// can ask a [`crate::KeyProvider`] for the same key later.
    pub fn key_id(&self) -> &KeyId {
        &self.key_id
    }
    /// Version of the data key this was derived from.
    pub fn version(&self) -> u32 {
        self.version
    }
    /// The 32-byte AEAD key (XChaCha20-Poly1305).
    pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.bytes
    }
}

impl std::fmt::Debug for ZoneKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ZoneKey({}, <redacted {} bytes>)", self.key_id, KEY_LEN)
    }
}

/// The engine's account scope string for a key: lower-case hex SHA-256 over a domain label and
/// the length-prefixed `app`, `zone`, `account_id` and `key_id`. A rotation or reset changes the
/// key id, so the scope changes and the sync engine resets its ledger through the existing
/// scope-change path. (The design sketch spells this `sha256("rightkit-cloud|app|zone|account|key_id")`;
/// length prefixes replace `|` so the join cannot be ambiguous. Only consistency between
/// devices matters, and every device uses this function.)
pub fn account_scope(app: &str, zone: &str, account_id: &str, key_id: &KeyId) -> Result<String> {
    check_component("app", app)?;
    check_component("zone", zone)?;
    check_component("account id", account_id)?;
    let mut buf = Vec::new();
    buf.extend_from_slice(SCOPE_DOMAIN);
    push_lp(&mut buf, app.as_bytes());
    push_lp(&mut buf, zone.as_bytes());
    push_lp(&mut buf, account_id.as_bytes());
    push_lp(&mut buf, key_id.as_str().as_bytes());
    Ok(hex_encode(&Sha256::digest(&buf)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(fill: u8, version: u32) -> DataKey {
        DataKey::from_bytes(version, Zeroizing::new([fill; KEY_LEN])).unwrap()
    }

    #[test]
    fn key_id_is_stable_distinct_and_parses() {
        let a = key(1, 1);
        assert_eq!(a.key_id(), key(1, 7).key_id(), "id depends on the key, not the version");
        assert_ne!(a.key_id(), key(2, 1).key_id());
        assert_eq!(KeyId::parse(a.key_id().as_str()).unwrap(), *a.key_id());
        assert!(KeyId::parse("k123").is_err());
        assert!(KeyId::parse("x0123456789abcdef").is_err());
        assert!(KeyId::parse("k0123456789ABCDEF").is_err());
    }

    #[test]
    fn version_zero_is_rejected() {
        assert!(DataKey::from_bytes(0, Zeroizing::new([0; KEY_LEN])).is_err());
        assert!(DataKey::generate(0).is_err());
    }

    #[test]
    fn zone_keys_are_deterministic_and_separated() {
        let k = key(9, 1);
        let a = k.zone_key("heardright", "transcripts").unwrap();
        let a2 = k.zone_key("heardright", "transcripts").unwrap();
        assert_eq!(a.as_bytes(), a2.as_bytes());
        assert_ne!(a.as_bytes(), k.expose_bytes(), "never the root itself");
        assert_ne!(a.as_bytes(), k.zone_key("heardright", "other").unwrap().as_bytes());
        assert_ne!(a.as_bytes(), k.zone_key("otherapp", "transcripts").unwrap().as_bytes());
        // Length prefixes: moving a byte between app and zone changes the key.
        let x = k.zone_key("ab", "c").unwrap();
        let y = k.zone_key("a", "bc").unwrap();
        assert_ne!(x.as_bytes(), y.as_bytes());
        assert_ne!(a.as_bytes(), key(8, 1).zone_key("heardright", "transcripts").unwrap().as_bytes());
        assert_eq!(a.key_id(), k.key_id());
    }

    #[test]
    fn components_are_validated() {
        let k = key(3, 1);
        assert!(k.zone_key("", "z").is_err());
        assert!(k.zone_key("a", "z\u{0}").is_err());
        assert!(k.zone_key(&"x".repeat(129), "z").is_err());
    }

    #[test]
    fn account_scope_changes_with_every_input() {
        let id = key(1, 1).key_id().clone();
        let id2 = key(2, 1).key_id().clone();
        let base = account_scope("app", "zone", "acct", &id).unwrap();
        assert_eq!(base.len(), 64);
        assert_eq!(base, account_scope("app", "zone", "acct", &id).unwrap());
        assert_ne!(base, account_scope("app2", "zone", "acct", &id).unwrap());
        assert_ne!(base, account_scope("app", "zone2", "acct", &id).unwrap());
        assert_ne!(base, account_scope("app", "zone", "acct2", &id).unwrap());
        assert_ne!(base, account_scope("app", "zone", "acct", &id2).unwrap());
    }

    #[test]
    fn debug_never_prints_key_bytes() {
        let k = key(0xab, 2);
        let s = format!("{k:?} {:?}", k.zone_key("a", "b").unwrap());
        assert!(s.contains("redacted"));
        assert!(!s.contains("abab"));
    }
}
