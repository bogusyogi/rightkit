//! The key manager: generate the account data key, keep versions, rotate, and install a
//! transferred key, all stored in a [`rightkit_credentials::Store`].
//!
//! Layout inside the store's namespace (one namespace per app; one manager per account):
//!
//! * `sync-keys/<account>/current`: decimal version of the current key.
//! * `sync-keys/<account>/v<N>`: the version-N key as 64 lower-case hex characters.
//!
//! Versions are written before the pointer moves, so a crash leaves either the old state or an
//! orphan version that the next `create_if_absent`, `rotate` or `install` adopts. A pointer whose
//! version is missing is [`Error::Corrupt`]; the manager never mints over a key it cannot read.
//! The OS store has no multi-item transaction, so two processes rotating at the same instant are
//! resolved by add-if-absent on the version item (one wins, the other adopts it).

use crate::error::{Error, Result};
use crate::key::{check_component, DataKey, KeyId, KEY_LEN};
use crate::util::{hex_decode, hex_encode};
use rightkit_credentials::{Secret, Store, WriteIntent};
use zeroize::Zeroizing;

/// What [`KeyManager::install`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallOutcome {
    /// The key (same key id) is already the current key; nothing was written.
    AlreadyCurrent,
    /// The key was newer than the current one (or none existed) and is now current.
    Advanced,
    /// `replace` was set: every earlier version was erased and this key is the only one.
    Replaced,
}

/// Manages the data keys of one account in one credential-store namespace.
#[derive(Clone)]
pub struct KeyManager {
    store: Store,
    account_id: String,
}

impl KeyManager {
    /// `store` should be non-interactive (the default); `account_id` is the cloud account id.
    pub fn new(store: Store, account_id: impl Into<String>) -> Result<KeyManager> {
        let account_id = account_id.into();
        check_component("account id", &account_id)?;
        Ok(KeyManager { store, account_id })
    }

    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    fn pointer_name(&self) -> String {
        format!("sync-keys/{}/current", self.account_id)
    }
    fn version_name(&self, version: u32) -> String {
        format!("sync-keys/{}/v{}", self.account_id, version)
    }

    fn read_pointer(&self) -> Result<Option<u32>> {
        let Some(secret) = self.store.get(&self.pointer_name())? else {
            return Ok(None);
        };
        let text = secret.expose_str().map_err(|_| Error::Corrupt)?;
        let v: u32 = text.parse().map_err(|_| Error::Corrupt)?;
        if v == 0 {
            return Err(Error::Corrupt);
        }
        Ok(Some(v))
    }

    fn read_version(&self, version: u32) -> Result<Option<DataKey>> {
        let Some(secret) = self.store.get(&self.version_name(version))? else {
            return Ok(None);
        };
        let text = secret.expose_str().map_err(|_| Error::Corrupt)?;
        let raw = Zeroizing::new(hex_decode(text).ok_or(Error::Corrupt)?);
        if raw.len() != KEY_LEN {
            return Err(Error::Corrupt);
        }
        let mut bytes = Zeroizing::new([0u8; KEY_LEN]);
        bytes.copy_from_slice(&raw);
        Ok(Some(DataKey::from_bytes(version, bytes)?))
    }

    /// `Ok(true)` when written; `Ok(false)` for `CreateOnly` over an existing item.
    fn write_version(&self, key: &DataKey, intent: WriteIntent) -> Result<bool> {
        let hex = Zeroizing::new(hex_encode(key.expose_bytes()));
        let secret = Secret::from_string(hex.as_str());
        Ok(self
            .store
            .put(&self.version_name(key.version()), &secret, intent)?)
    }

    fn write_pointer(&self, version: u32) -> Result<()> {
        self.store
            .set_str(&self.pointer_name(), &version.to_string())?;
        Ok(())
    }

    /// Passive presence check: a data key is enrolled on this device. Never reads key bytes.
    pub fn is_enrolled(&self) -> Result<bool> {
        Ok(self.store.exists(&self.pointer_name())?)
    }

    /// The current key, or `None` when this device holds none. A store failure is an error, never
    /// "none". A pointer without its key is [`Error::Corrupt`].
    pub fn current(&self) -> Result<Option<DataKey>> {
        let Some(version) = self.read_pointer()? else {
            return Ok(None);
        };
        match self.read_version(version)? {
            Some(k) => Ok(Some(k)),
            None => Err(Error::Corrupt),
        }
    }

    /// [`KeyManager::current`], or [`Error::NotEnrolled`].
    pub fn require_current(&self) -> Result<DataKey> {
        self.current()?.ok_or(Error::NotEnrolled)
    }

    /// A specific version, if this device holds it.
    pub fn get_version(&self, version: u32) -> Result<Option<DataKey>> {
        self.read_version(version)
    }

    /// The held key (current or an older version) with this id, scanning versions newest first.
    pub fn find_by_id(&self, id: &KeyId) -> Result<Option<DataKey>> {
        let Some(current) = self.read_pointer()? else {
            return Ok(None);
        };
        for v in (1..=current).rev() {
            if let Some(k) = self.read_version(v)? {
                if k.key_id() == id {
                    return Ok(Some(k));
                }
            }
        }
        Ok(None)
    }

    /// First-device enable: return the current key, generating version 1 when none exists.
    /// `Ok((key, true))` when this call generated it. Concurrent callers adopt the winner.
    pub fn create_if_absent(&self) -> Result<(DataKey, bool)> {
        if let Some(k) = self.current()? {
            return Ok((k, false));
        }
        // No pointer. An orphan v1 (an interrupted first enable) is adopted, never replaced.
        let fresh = DataKey::generate(1)?;
        let created = self.write_version(&fresh, WriteIntent::CreateOnly)?;
        if !created && self.read_version(1)?.is_none() {
            return Err(Error::Corrupt);
        }
        // Pointer last, create-only: a concurrent first enable keeps the earlier pointer.
        let _ = self.store.put(
            &self.pointer_name(),
            &Secret::from_string("1"),
            WriteIntent::CreateOnly,
        )?;
        let key = self.current()?.ok_or(Error::Corrupt)?;
        let ours = created && key.key_id() == fresh.key_id();
        Ok((key, ours))
    }

    /// Rotation: generate version `current + 1`, store it, and make it current. Older versions
    /// stay (so old records can still be opened) until [`KeyManager::retire`]. The new key id
    /// changes the account scope, which makes the sync engine reset and republish. Other
    /// devices do not hold the new key; approve each of them again with the transfer protocol.
    ///
    /// If version `current + 1` already exists (an interrupted or concurrent rotation) that key
    /// is adopted and made current instead of generating another.
    pub fn rotate(&self) -> Result<DataKey> {
        let current = self.require_current()?;
        let next = current
            .version()
            .checked_add(1)
            .ok_or_else(|| Error::Invalid {
                what: "key version",
                reason: "version counter exhausted".into(),
            })?;
        let fresh = DataKey::generate(next)?;
        let key = if self.write_version(&fresh, WriteIntent::CreateOnly)? {
            fresh
        } else {
            self.read_version(next)?.ok_or(Error::Corrupt)?
        };
        self.write_pointer(next)?;
        Ok(key)
    }

    /// Store a key received from another device (see [`crate::transfer`]).
    ///
    /// * Same key id as the current key: [`InstallOutcome::AlreadyCurrent`].
    /// * A higher version than the current key, or no current key: becomes current
    ///   ([`InstallOutcome::Advanced`]).
    /// * Anything else is [`Error::VersionConflict`] unless `replace` is set. `replace` is for the
    ///   reset path (the account was reset and the user confirmed it): every held version is
    ///   erased first, so no old key survives.
    pub fn install(&self, key: &DataKey, replace: bool) -> Result<InstallOutcome> {
        let current = match self.read_pointer()? {
            None => None,
            Some(v) => self.read_version(v)?,
        };
        if let Some(cur) = &current {
            if cur.key_id() == key.key_id() {
                return Ok(InstallOutcome::AlreadyCurrent);
            }
        }
        let advance = current.as_ref().map_or(true, |c| key.version() > c.version());
        if advance && !replace {
            if !self.write_version(key, WriteIntent::CreateOnly)? {
                // The slot is taken: fine only if it holds this very key.
                let held = self.read_version(key.version())?.ok_or(Error::Corrupt)?;
                if held.key_id() != key.key_id() {
                    return Err(Error::VersionConflict {
                        version: key.version(),
                    });
                }
            }
            self.write_pointer(key.version())?;
            return Ok(InstallOutcome::Advanced);
        }
        if !replace {
            return Err(Error::VersionConflict {
                version: key.version(),
            });
        }
        self.forget_all()?;
        self.write_version(key, WriteIntent::Replace)?;
        self.write_pointer(key.version())?;
        Ok(InstallOutcome::Replaced)
    }

    /// Erase an older version. The current version cannot be retired.
    pub fn retire(&self, version: u32) -> Result<bool> {
        if self.read_pointer()? == Some(version) {
            return Err(Error::Invalid {
                what: "key version",
                reason: "the current version cannot be retired".into(),
            });
        }
        Ok(self.store.delete(&self.version_name(version))?)
    }

    /// Sign-out or reset: erase every version and the pointer. Versions go first and the pointer
    /// last, so an interrupted call leaves a pointer without keys (reported as
    /// [`Error::Corrupt`], and repaired by calling this again), never keys without a pointer.
    pub fn forget_all(&self) -> Result<()> {
        if let Some(current) = self.read_pointer()? {
            for v in 1..=current {
                self.store.delete(&self.version_name(v))?;
            }
        }
        self.store.delete(&self.pointer_name())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_credentials::{Config, MemoryBackend, Namespace};
    use std::sync::Arc;

    fn manager(account: &str) -> KeyManager {
        let ns = Namespace::new("test.rightkit.sync-keys").unwrap();
        let store = Store::open(Config::new(ns).backend(Arc::new(MemoryBackend::new())));
        KeyManager::new(store, account).unwrap()
    }

    /// Two managers over one backend: two processes sharing one OS store.
    fn pair(account: &str) -> (KeyManager, KeyManager) {
        let ns = Namespace::new("test.rightkit.sync-keys").unwrap();
        let backend: Arc<MemoryBackend> = Arc::new(MemoryBackend::new());
        let a = Store::open(Config::new(ns.clone()).backend(backend.clone()));
        let b = Store::open(Config::new(ns).backend(backend));
        (
            KeyManager::new(a, account).unwrap(),
            KeyManager::new(b, account).unwrap(),
        )
    }

    fn acct(tag: &str) -> String {
        format!("acct-{tag}-{}", hex_encode(&crate::util::random_array::<6>().unwrap()))
    }

    #[test]
    fn first_device_generates_once_then_returns_the_same_key() {
        let m = manager(&acct("first"));
        assert!(!m.is_enrolled().unwrap());
        assert!(m.current().unwrap().is_none());
        assert_eq!(m.require_current().unwrap_err(), Error::NotEnrolled);
        let (k1, created) = m.create_if_absent().unwrap();
        assert!(created);
        assert_eq!(k1.version(), 1);
        let (k2, created2) = m.create_if_absent().unwrap();
        assert!(!created2);
        assert_eq!(k1.key_id(), k2.key_id());
        assert_eq!(k1.expose_bytes(), k2.expose_bytes());
        assert!(m.is_enrolled().unwrap());
    }

    #[test]
    fn concurrent_first_enable_converges_on_one_key() {
        let (a, b) = pair(&acct("race"));
        let (ka, _) = a.create_if_absent().unwrap();
        let (kb, created) = b.create_if_absent().unwrap();
        assert!(!created);
        assert_eq!(ka.key_id(), kb.key_id());
    }

    #[test]
    fn rotation_bumps_version_changes_id_and_keeps_old_until_retired() {
        let m = manager(&acct("rot"));
        let (k1, _) = m.create_if_absent().unwrap();
        let k2 = m.rotate().unwrap();
        assert_eq!(k2.version(), 2);
        assert_ne!(k2.key_id(), k1.key_id());
        assert_ne!(k2.expose_bytes(), k1.expose_bytes());
        assert_eq!(m.require_current().unwrap().key_id(), k2.key_id());
        // The scope an engine binds to changes with the key id.
        let s1 = crate::account_scope("app", "z", m.account_id(), k1.key_id()).unwrap();
        let s2 = crate::account_scope("app", "z", m.account_id(), k2.key_id()).unwrap();
        assert_ne!(s1, s2);
        // The old key is still readable by id and version.
        assert_eq!(m.find_by_id(k1.key_id()).unwrap().unwrap().version(), 1);
        assert_eq!(m.get_version(1).unwrap().unwrap().key_id(), k1.key_id());
        assert_eq!(m.find_by_id(k2.key_id()).unwrap().unwrap().version(), 2);
        // Retire the old one; the current one cannot be retired.
        assert!(m.retire(1).unwrap());
        assert!(m.find_by_id(k1.key_id()).unwrap().is_none());
        assert!(matches!(m.retire(2), Err(Error::Invalid { .. })));
        assert_eq!(m.rotate().unwrap().version(), 3);
    }

    #[test]
    fn rotate_without_a_key_is_not_enrolled() {
        assert_eq!(manager(&acct("norot")).rotate().unwrap_err(), Error::NotEnrolled);
    }

    #[test]
    fn rotation_adopts_an_interrupted_rotation() {
        let m = manager(&acct("orphan"));
        let (_k1, _) = m.create_if_absent().unwrap();
        // A rotation that wrote v2 but crashed before moving the pointer.
        let orphan = DataKey::generate(2).unwrap();
        assert!(m.write_version(&orphan, WriteIntent::CreateOnly).unwrap());
        let adopted = m.rotate().unwrap();
        assert_eq!(adopted.key_id(), orphan.key_id());
        assert_eq!(m.require_current().unwrap().key_id(), orphan.key_id());
    }

    #[test]
    fn pointer_without_key_is_corrupt_and_forget_all_repairs_it() {
        let m = manager(&acct("corrupt"));
        m.create_if_absent().unwrap();
        m.store.delete(&m.version_name(1)).unwrap();
        assert_eq!(m.current().unwrap_err(), Error::Corrupt);
        assert_eq!(m.create_if_absent().unwrap_err(), Error::Corrupt);
        m.forget_all().unwrap();
        assert!(!m.is_enrolled().unwrap());
        assert!(m.create_if_absent().unwrap().1);
    }

    #[test]
    fn malformed_stored_key_is_corrupt() {
        let m = manager(&acct("badhex"));
        m.create_if_absent().unwrap();
        m.store.set_str(&m.version_name(1), "not-hex").unwrap();
        assert_eq!(m.current().unwrap_err(), Error::Corrupt);
        m.store.set_str(&m.version_name(1), "00ff").unwrap();
        assert_eq!(m.current().unwrap_err(), Error::Corrupt);
    }

    #[test]
    fn install_advances_is_idempotent_and_refuses_conflicts() {
        let m = manager(&acct("install"));
        let k3 = DataKey::generate(3).unwrap();
        assert_eq!(m.install(&k3, false).unwrap(), InstallOutcome::Advanced);
        assert_eq!(m.require_current().unwrap().version(), 3);
        assert_eq!(m.install(&k3, false).unwrap(), InstallOutcome::AlreadyCurrent);
        // A different key at an older or equal version is refused without `replace`.
        let other = DataKey::generate(3).unwrap();
        assert_eq!(
            m.install(&other, false).unwrap_err(),
            Error::VersionConflict { version: 3 }
        );
        assert_eq!(m.require_current().unwrap().key_id(), k3.key_id());
        // A newer key advances and keeps the old one.
        let k4 = DataKey::generate(4).unwrap();
        assert_eq!(m.install(&k4, false).unwrap(), InstallOutcome::Advanced);
        assert!(m.get_version(3).unwrap().is_some());
    }

    #[test]
    fn install_replace_erases_every_old_version() {
        let m = manager(&acct("replace"));
        let (k1, _) = m.create_if_absent().unwrap();
        m.rotate().unwrap();
        let reset = DataKey::generate(1).unwrap();
        assert_eq!(m.install(&reset, true).unwrap(), InstallOutcome::Replaced);
        let cur = m.require_current().unwrap();
        assert_eq!(cur.key_id(), reset.key_id());
        assert_eq!(cur.version(), 1);
        assert!(m.get_version(2).unwrap().is_none());
        assert!(m.find_by_id(k1.key_id()).unwrap().is_none());
    }

    #[test]
    fn forget_all_signs_out() {
        let m = manager(&acct("forget"));
        m.create_if_absent().unwrap();
        m.rotate().unwrap();
        m.forget_all().unwrap();
        assert!(m.current().unwrap().is_none());
        assert!(m.get_version(1).unwrap().is_none());
        assert!(m.get_version(2).unwrap().is_none());
        // Signed out; a later enable mints a brand-new key.
        assert!(m.create_if_absent().unwrap().1);
    }

    #[test]
    fn accounts_do_not_share_keys() {
        let ns = Namespace::new("test.rightkit.sync-keys").unwrap();
        let store = Store::open(Config::new(ns).backend(Arc::new(MemoryBackend::new())));
        let a = KeyManager::new(store.clone(), "alice").unwrap();
        let b = KeyManager::new(store, "bob").unwrap();
        let (ka, _) = a.create_if_absent().unwrap();
        assert!(b.current().unwrap().is_none());
        let (kb, _) = b.create_if_absent().unwrap();
        assert_ne!(ka.key_id(), kb.key_id());
    }

    #[test]
    fn bad_account_ids_are_rejected() {
        let ns = Namespace::new("test.rightkit.sync-keys").unwrap();
        let store = Store::open(Config::new(ns).backend(Arc::new(MemoryBackend::new())));
        assert!(KeyManager::new(store.clone(), "").is_err());
        assert!(KeyManager::new(store, "a\nb").is_err());
    }
}
