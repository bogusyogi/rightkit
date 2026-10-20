//! Protected key storage for rightkit-db's encrypted lifecycle, over one pinned credential store.
//!
//! This adapter owns no database workflow or authentication policy. The app supplies its existing
//! Store (including Namespace, interaction/guard policy and shared rotation-lock directory),
//! canonical account, recovery account and explicit legacy staging accounts. rightkit-db owns
//! transition exclusion, key selection, readback, promotion decisions and recovery.

use std::time::Duration;

use rightkit_db::encrypted_lifecycle::{
    Key, KeyStoreError, KeyStoreErrorKind, PendingKey, PendingRevision, ProtectedKeySession,
    ProtectedKeyStore,
};
use sha2::{Digest, Sha256};

use crate::rotation::{commit_locked, remove_marker, RotationHeld};
use crate::{Error, Secret, Store, ROTATION_PENDING_SUFFIX};

/// Credential-backed key slots for a single encrypted database. Clone an Arc around this value
/// when sharing it with a lifecycle; construct another adapter to represent another database.
pub struct CredentialsKeyStore {
    store: Store,
    account: String,
    recovery: String,
    legacy: Vec<String>,
}

struct Session {
    store: Store,
    account: String,
    recovery: String,
    legacy: Vec<String>,
    _held: RotationHeld,
}

fn mapped(error: Error) -> KeyStoreError {
    let kind = match error {
        Error::Denied | Error::InteractionRequired | Error::Guarded { .. } => KeyStoreErrorKind::Denied,
        // An OS mutation worker outlives its caller's bounded wait. Never let transient
        // readback after this error authorize rollback or deletion of either viable key.
        Error::Timeout { .. } | Error::InProgress { .. } => KeyStoreErrorKind::Unsettled,
        Error::Unavailable(_) => KeyStoreErrorKind::Unavailable,
        Error::NotUtf8 => KeyStoreErrorKind::Corrupt,
        _ => KeyStoreErrorKind::Other,
    };
    // Fixed text avoids ever forwarding a backend-provided message as key material.
    KeyStoreError::new(kind, "protected credential operation failed")
}

fn key(secret: Secret) -> Result<Key, KeyStoreError> {
    let value = secret.expose_str().map_err(mapped)?;
    if value.is_empty() {
        return Err(KeyStoreError::new(KeyStoreErrorKind::Corrupt, "empty protected key"));
    }
    Ok(Key::new(value.to_owned()))
}

// Credentials' generation is the exact pending value. Only its digest crosses the DB revision
// surface (PendingRevision has public Debug); no plaintext key is used as a revision token.
fn revision(secret: &Secret) -> PendingRevision {
    let digest = Sha256::digest(secret.expose_bytes());
    PendingRevision(format!("credentials-v1:{digest:x}"))
}

fn ensure_settled(
    store: &Store,
    account: &str,
    recovery: &str,
    legacy: &[String],
) -> Result<(), KeyStoreError> {
    let marker = format!("{account}{ROTATION_PENDING_SUFFIX}");
    for slot in std::iter::once(account)
        .chain(std::iter::once(marker.as_str()))
        .chain(std::iter::once(recovery))
        .chain(legacy.iter().map(String::as_str))
    {
        if store.mutation_in_flight(slot).map_err(mapped)? {
            return Err(KeyStoreError::new(
                KeyStoreErrorKind::Unsettled,
                "protected credential mutation has not settled",
            ));
        }
    }
    Ok(())
}

impl CredentialsKeyStore {
    pub fn new(
        store: Store,
        account: impl Into<String>,
        recovery_account: impl Into<String>,
        legacy_staging_accounts: Vec<String>,
    ) -> Result<Self, KeyStoreError> {
        let account = account.into();
        let recovery = recovery_account.into();
        let store = store.pinned().map_err(mapped)?;
        let pending = format!("{account}{ROTATION_PENDING_SUFFIX}");
        let mut identities = std::collections::BTreeSet::new();
        for name in std::iter::once(&account)
            .chain(std::iter::once(&pending))
            .chain(std::iter::once(&recovery))
            .chain(legacy_staging_accounts.iter())
        {
            crate::namespace::check_target(store.namespace(), name).map_err(mapped)?;
            // Conservatively reject case aliases on every platform; Windows/macOS native
            // identities fold case, so otherwise a legacy deletion could erase a live slot.
            if !identities.insert(crate::namespace::normalize_target_name(name)) {
                return Err(KeyStoreError::other("protected key accounts overlap"));
            }
        }
        if account.ends_with(ROTATION_PENDING_SUFFIX) {
            return Err(KeyStoreError::other("primary account names a pending marker"));
        }
        // Establish that this backend has the required cross-process lock configuration.
        let _ = store.rotation_store(&account).map_err(mapped)?;
        Ok(Self {
            store,
            account,
            recovery,
            legacy: legacy_staging_accounts,
        })
    }
}

impl ProtectedKeyStore for CredentialsKeyStore {
    fn lock_rotation(
        &self,
        timeout: Duration,
    ) -> Result<Box<dyn ProtectedKeySession + Send + 'static>, KeyStoreError> {
        let (store, lock) = self.store.rotation_store(&self.account).map_err(mapped)?;
        let held = lock.with_timeout(timeout).acquire().map_err(mapped)?;
        // Durable workers also retain this same file lock after session Drop. The target
        // table covers non-durable backends & every slot, not only the primary account.
        ensure_settled(&store, &self.account, &self.recovery, &self.legacy)?;
        Ok(Box::new(Session {
            store: held.bind(store),
            account: self.account.clone(),
            recovery: self.recovery.clone(),
            legacy: self.legacy.clone(),
            _held: held,
        }))
    }
}

impl Session {
    fn with_store<T>(
        &self,
        operation: impl FnOnce(&Store) -> Result<T, KeyStoreError>,
    ) -> Result<T, KeyStoreError> {
        // Windows reads do not acquire a mutation permit. An old primary read while a
        // timed-out promotion is still running therefore proves nothing: refuse ALL slot
        // reads/writes until that worker actually returns, including recovery cleanup.
        ensure_settled(&self.store, &self.account, &self.recovery, &self.legacy)?;
        operation(&self.store)
    }

    fn check_legacy(&self, alias: &str) -> Result<(), KeyStoreError> {
        if self.legacy.iter().any(|name| name == alias) {
            Ok(())
        } else {
            Err(KeyStoreError::other("legacy staging account is not mapped"))
        }
    }
}

impl ProtectedKeySession for Session {
    fn read_primary(&mut self) -> Result<Option<Key>, KeyStoreError> {
        self.with_store(|store| store.get(&self.account).map_err(mapped)?.map(key).transpose())
    }

    fn read_pending(&mut self) -> Result<Option<PendingKey>, KeyStoreError> {
        self.with_store(|store| {
            // Read the exact marker instead of filtering empty secrets as pending_rotation
            // does: malformed storage must be Corrupt, never an empty slot.
            let pending = store.get(&format!("{}{ROTATION_PENDING_SUFFIX}", self.account)).map_err(mapped)?;
            pending.map(|secret| {
                let revision = revision(&secret);
                Ok(PendingKey { key: key(secret)?, revision })
            }).transpose()
        })
    }

    fn write_pending(&mut self, value: &Key) -> Result<PendingRevision, KeyStoreError> {
        self.with_store(|store| {
            let secret = Secret::from_string(value.as_str());
            if secret.is_empty() {
                return Err(KeyStoreError::other("cannot stage an empty key"));
            }
            store.rotation_ready(&self.account).map_err(mapped)?;
            let marker = format!("{}{ROTATION_PENDING_SUFFIX}", self.account);
            if !store.add_if_absent(&marker, &secret).map_err(mapped)? {
                return Err(KeyStoreError::other("a pending rotation already exists"));
            }
            Ok(revision(&secret))
        })
    }

    fn promote_pending(&mut self, expected: &PendingRevision) -> Result<(), KeyStoreError> {
        self.with_store(|store| {
            let Some(pending) = store.pending_rotation(&self.account).map_err(mapped)? else {
                if store.get(&self.account).map_err(mapped)?.is_some_and(|key| revision(&key) == *expected) {
                    return Ok(());
                }
                return Err(KeyStoreError::other("pending revision is no longer present"));
            };
            if revision(&pending) != *expected {
                return Err(KeyStoreError::other("pending revision was superseded"));
            }
            commit_locked(store, &self.account, Some(&pending)).map_err(mapped)?;
            Ok(())
        })
    }

    fn discard_pending(&mut self, expected: &PendingRevision) -> Result<(), KeyStoreError> {
        self.with_store(|store| {
            store.rotation_ready(&self.account).map_err(mapped)?;
            if let Some(pending) = store.pending_rotation(&self.account).map_err(mapped)? {
                if revision(&pending) != *expected {
                    return Err(KeyStoreError::other("pending revision was superseded"));
                }
                remove_marker(store, &self.account, &pending).map_err(mapped)?;
            }
            Ok(())
        })
    }

    fn read_legacy_staging(&mut self, alias: &str) -> Result<Option<Key>, KeyStoreError> {
        self.check_legacy(alias)?;
        self.with_store(|store| store.get(alias).map_err(mapped)?.map(key).transpose())
    }

    fn delete_legacy_staging(&mut self, alias: &str) -> Result<(), KeyStoreError> {
        self.check_legacy(alias)?;
        self.with_store(|store| store.delete(alias).map(|_| ()).map_err(mapped))
    }

    fn read_recovery(&mut self) -> Result<Option<Key>, KeyStoreError> {
        self.with_store(|store| store.get(&self.recovery).map_err(mapped)?.map(key).transpose())
    }

    fn write_recovery(&mut self, value: &Key) -> Result<(), KeyStoreError> {
        if value.is_empty() {
            return Err(KeyStoreError::other("cannot retain an empty recovery key"));
        }
        self.with_store(|store| store.set_str(&self.recovery, value.as_str()).map_err(mapped))
    }

    fn clear_recovery(&mut self) -> Result<(), KeyStoreError> {
        self.with_store(|store| store.delete(&self.recovery).map(|_| ()).map_err(mapped))
    }

}
