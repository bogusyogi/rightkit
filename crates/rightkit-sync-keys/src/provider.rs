//! The seam to an encrypting sync backend.
//!
//! `rightkit-sync` gains an `EncryptingBackend` that seals record fields with XChaCha20-Poly1305.
//! It needs two things from the key side: "the key to seal with now, and its id" and "the key
//! with this id, to open an older record". [`KeyProvider`] is exactly that. This crate does not
//! depend on `rightkit-sync` (and `rightkit-sync` does not depend on this crate): whichever side
//! defines the final trait, a four-line adapter bridges them, for example
//!
//! ```ignore
//! struct Adapter(rightkit_sync_keys::ZoneKeys);
//! impl rightkit_sync::KeyProvider for Adapter {            // the sync crate's own trait
//!     fn current(&self) -> Result<(String, [u8; 32]), E> {
//!         let k = self.0.current_key().map_err(E::from)?;
//!         Ok((k.key_id().to_string(), *k.as_bytes()))
//!     }
//!     fn by_id(&self, id: &str) -> Result<Option<[u8; 32]>, E> {
//!         let id = KeyId::parse(id).map_err(E::from)?;
//!         Ok(self.0.key_by_id(&id).map_err(E::from)?.map(|k| *k.as_bytes()))
//!     }
//! }
//! ```
//!
//! Sealing: put `key_id().as_str()` in the blob header next to the nonce and include it in the
//! associated data; opening: parse it, ask [`KeyProvider::key_by_id`], and treat `Ok(None)` as
//! "undecodable record" (the engine's quarantine path), never as a failure of the whole pass.
//! The account scope for the engine's `AuthProvider::account_scope` is
//! [`ZoneKeys::account_scope`].

use crate::error::Result;
use crate::key::{account_scope, check_component, DataKey, KeyId, ZoneKey};
use crate::manager::KeyManager;

/// Source of zone keys for one `(app, zone)`.
pub trait KeyProvider: Send + Sync {
    /// The key to seal new records with. [`crate::Error::NotEnrolled`] when sync keys are not
    /// set up on this device.
    fn current_key(&self) -> Result<ZoneKey>;
    /// The key with this id, current or older. `Ok(None)` when this device does not hold it.
    fn key_by_id(&self, key_id: &KeyId) -> Result<Option<ZoneKey>>;
}

/// [`KeyProvider`] over a [`KeyManager`] (keys in the OS credential store).
#[derive(Clone)]
pub struct ZoneKeys {
    manager: KeyManager,
    app: String,
    zone: String,
}

impl ZoneKeys {
    pub fn new(manager: KeyManager, app: impl Into<String>, zone: impl Into<String>) -> Result<Self> {
        let (app, zone) = (app.into(), zone.into());
        check_component("app", &app)?;
        check_component("zone", &zone)?;
        Ok(Self { manager, app, zone })
    }

    pub fn manager(&self) -> &KeyManager {
        &self.manager
    }

    /// The scope string for the current key (changes on rotation or reset).
    pub fn account_scope(&self) -> Result<String> {
        let key = self.manager.require_current()?;
        account_scope(&self.app, &self.zone, self.manager.account_id(), key.key_id())
    }
}

impl KeyProvider for ZoneKeys {
    fn current_key(&self) -> Result<ZoneKey> {
        self.manager
            .require_current()?
            .zone_key(&self.app, &self.zone)
    }
    fn key_by_id(&self, key_id: &KeyId) -> Result<Option<ZoneKey>> {
        match self.manager.find_by_id(key_id)? {
            Some(k) => Ok(Some(k.zone_key(&self.app, &self.zone)?)),
            None => Ok(None),
        }
    }
}

/// A fixed set of keys, for tests of code that takes a [`KeyProvider`] and for tools that
/// already hold a key in memory. The highest version is current.
#[derive(Clone)]
pub struct StaticKeys {
    keys: Vec<DataKey>,
    app: String,
    zone: String,
}

impl StaticKeys {
    pub fn new(keys: Vec<DataKey>, app: impl Into<String>, zone: impl Into<String>) -> Result<Self> {
        let (app, zone) = (app.into(), zone.into());
        check_component("app", &app)?;
        check_component("zone", &zone)?;
        Ok(Self { keys, app, zone })
    }
}

impl KeyProvider for StaticKeys {
    fn current_key(&self) -> Result<ZoneKey> {
        self.keys
            .iter()
            .max_by_key(|k| k.version())
            .ok_or(crate::Error::NotEnrolled)?
            .zone_key(&self.app, &self.zone)
    }
    fn key_by_id(&self, key_id: &KeyId) -> Result<Option<ZoneKey>> {
        match self.keys.iter().find(|k| k.key_id() == key_id) {
            Some(k) => Ok(Some(k.zone_key(&self.app, &self.zone)?)),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;
    use rightkit_credentials::{Config, MemoryBackend, Namespace, Store};
    use std::sync::Arc;

    fn zone_keys(account: &str) -> ZoneKeys {
        let ns = Namespace::new("test.rightkit.sync-keys.provider").unwrap();
        let store = Store::open(Config::new(ns).backend(Arc::new(MemoryBackend::new())));
        ZoneKeys::new(KeyManager::new(store, account).unwrap(), "heardright", "transcripts").unwrap()
    }

    #[test]
    fn not_enrolled_until_a_key_exists() {
        let z = zone_keys("prov-1");
        assert_eq!(z.current_key().unwrap_err(), Error::NotEnrolled);
        assert_eq!(z.account_scope().unwrap_err(), Error::NotEnrolled);
    }

    #[test]
    fn current_and_by_id_follow_rotation() {
        let z = zone_keys("prov-2");
        z.manager().create_if_absent().unwrap();
        let k1 = z.current_key().unwrap();
        let s1 = z.account_scope().unwrap();
        assert_eq!(z.key_by_id(k1.key_id()).unwrap().unwrap().as_bytes(), k1.as_bytes());

        z.manager().rotate().unwrap();
        let k2 = z.current_key().unwrap();
        assert_ne!(k2.key_id(), k1.key_id());
        assert_eq!(k2.version(), 2);
        assert_ne!(z.account_scope().unwrap(), s1, "rotation changes the scope");
        // Old records stay readable until the old version is retired.
        assert_eq!(z.key_by_id(k1.key_id()).unwrap().unwrap().as_bytes(), k1.as_bytes());
        z.manager().retire(1).unwrap();
        assert!(z.key_by_id(k1.key_id()).unwrap().is_none());
    }

    #[test]
    fn static_keys_pick_the_highest_version() {
        let (a, b) = (DataKey::generate(1).unwrap(), DataKey::generate(2).unwrap());
        let s = StaticKeys::new(vec![a.clone(), b.clone()], "app", "zone").unwrap();
        assert_eq!(s.current_key().unwrap().key_id(), b.key_id());
        assert_eq!(s.key_by_id(a.key_id()).unwrap().unwrap().version(), 1);
        let unknown = DataKey::generate(3).unwrap();
        assert!(s.key_by_id(unknown.key_id()).unwrap().is_none());
        assert_eq!(
            StaticKeys::new(vec![], "app", "zone").unwrap().current_key().unwrap_err(),
            Error::NotEnrolled
        );
    }
}
