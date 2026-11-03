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
        let advance = current.as_ref().is_none_or(|c| key.version() > c.version());
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
