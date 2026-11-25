//! The seam to `rightkit-sync`'s cipher (feature `sync`).
//!
//! [`SyncKeys`] turns any [`KeyProvider`] of this crate into the `VersionedKeyProvider` that
//! `rightkit_sync::cipher::EncryptingBackend::new_versioned` takes. It seals under the key's version and
//! writes its key id into the header of a record sealed above version 255 (format 2), and it opens such a
//! record only with the key that has that id at that version, never another.

use crate::error::Error;
use crate::key::{KeyId, ZoneKey};
use crate::provider::KeyProvider;
use rightkit_sync::cipher::{CipherKey, VersionedKeyProvider};
use rightkit_sync::{SyncError, SyncResult};
use std::sync::Arc;

/// A [`KeyProvider`] as a `rightkit-sync` key provider.
#[derive(Clone)]
pub struct SyncKeys {
    inner: Arc<dyn KeyProvider>,
}

impl SyncKeys {
    pub fn new(inner: Arc<dyn KeyProvider>) -> Self {
        Self { inner }
    }
}

fn cipher_key(key: &ZoneKey) -> CipherKey {
    CipherKey::from_bytes(*key.as_bytes())
}

/// A device with no key, or one that does not hold the key, cannot sync yet (`Unavailable`, retried on the
/// backoff cadence); any other failure is transient. The messages carry no key material.
fn to_sync_error(error: Error) -> SyncError {
    match &error {
        Error::NotEnrolled | Error::UnknownKey | Error::Corrupt => {
            SyncError::Unavailable(error.to_string())
        }
        _ => SyncError::Transient(error.to_string()),
    }
}

impl VersionedKeyProvider for SyncKeys {
    fn current_versioned(&self) -> SyncResult<(u32, CipherKey)> {
        let key = self.inner.current_key().map_err(to_sync_error)?;
        Ok((key.version(), cipher_key(&key)))
    }

    fn key_for_version(&self, version: u32) -> SyncResult<Option<CipherKey>> {
        let key = self.inner.key_by_version(version).map_err(to_sync_error)?;
        Ok(key.as_ref().map(cipher_key))
    }

    fn current_identified(&self) -> SyncResult<(u32, Option<String>, CipherKey)> {
        let key = self.inner.current_key().map_err(to_sync_error)?;
        Ok((
            key.version(),
            Some(key.key_id().as_str().to_owned()),
            cipher_key(&key),
        ))
    }

    fn key_for_identified(
        &self,
        version: u32,
        key_id: Option<&str>,
    ) -> SyncResult<Option<CipherKey>> {
        let Some(key_id) = key_id else {
            return self.key_for_version(version);
        };
        // An id this crate would not have written is an id this device does not hold.
        let Ok(key_id) = KeyId::parse(key_id) else {
            return Ok(None);
        };
        let key = self.inner.key_by_id(&key_id).map_err(to_sync_error)?;
        Ok(key
            .filter(|found| found.version() == version)
            .as_ref()
            .map(cipher_key))
    }
}
