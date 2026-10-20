//! The fenced engine token source.
//!
//! An engine process verifies the stored licence itself ([`EngineEntitlements`](crate::EngineEntitlements))
//! and is an authorizer like any other: before it reads the record it takes the per-app licence
//! operation lock with a bounded 250 ms try, then checks the credentials rotation fence. A writer
//! holds that lock until its backend worker has really finished (v3.2), so a lock it can take
//! means no write is in flight; a marker, or a fence still held by a timed-out child call, means
//! an earlier write is unresolved. Each refusal is a fixed-text `Err` the engine reports as
//! `Unreadable`, never "no licence" and never an entitlement. The record it reads is the envelope:
//! a tombstone is no licence, and an older bare record is read as active (see
//! [`stored_signed_token`](crate::stored_signed_token)).

use std::path::PathBuf;
use std::time::Duration;

use rightkit_credentials::Store;

use super::lock::{OpLock, STATUS_WAIT};
use super::order::{self, Level};
use super::view::LicenseError;
use crate::CredentialTokenSource;

/// The lock is held: a licence operation is in progress.
pub const ENGINE_BUSY_TEXT: &str = "licence busy";
/// A marker or an unfinished write stands: the stored licence cannot be trusted yet.
pub const ENGINE_UNRESOLVED_TEXT: &str = "licence unresolved";
/// The credential store could not be read or its fence could not be checked.
pub const ENGINE_STORE_TEXT: &str = "licence store unavailable";

/// How long the engine waits for the operation lock before it reports busy.
const ENGINE_WAIT: Duration = STATUS_WAIT;

impl CredentialTokenSource {
    /// A source over the native client's record: `store` and `account` are the app's credential
    /// namespace and the record account (the client's `LicenseClientConfig::account`), and
    /// `license_dir` the same `<app data>/license` directory the client uses (it holds the
    /// operation lock and the rotation lock directory, so every process derives the same ones).
    ///
    /// Every read first takes the operation lock (250 ms try) and checks the rotation fence;
    /// see the module docs. Pair it with
    /// [`EngineEntitlementsConfig::with_token_source`](crate::EngineEntitlementsConfig::with_token_source).
    pub fn from_record_store(
        store: Store,
        account: impl Into<String>,
        license_dir: impl Into<PathBuf>,
    ) -> Self {
        let account = account.into();
        let dir = license_dir.into();
        let store = store.with_rotation_lock_dir(dir.join("rotation"));
        let op = OpLock::new(dir.join(".lock"));

        let (fence_store, fence_account) = (store.clone(), account.clone());
        let reader = move || -> Result<Option<String>, String> {
            match store.get(&account) {
                Ok(None) => Ok(None),
                Ok(Some(secret)) if secret.is_empty() => Ok(None),
                Ok(Some(secret)) => secret
                    .expose_str()
                    .map(|text| Some(text.to_owned()))
                    .map_err(|_| "stored licence record is unreadable".to_string()),
                Err(_) => Err(ENGINE_STORE_TEXT.to_string()),
            }
        };
        CredentialTokenSource::new(reader).with_fence(move || {
            let level = order::enter(Level::Op);
            let guard = op.acquire(ENGINE_WAIT).map_err(|error| match error {
                LicenseError::Busy => ENGINE_BUSY_TEXT.to_string(),
                _ => ENGINE_STORE_TEXT.to_string(),
            })?;
            match fence_store.rotation_status(&fence_account) {
                Ok(status) if status.is_idle() => {}
                Ok(_) => return Err(ENGINE_UNRESOLVED_TEXT.to_string()),
                Err(_) => return Err(ENGINE_STORE_TEXT.to_string()),
            }
            // Kept until the read has returned: the writers stay out for the whole read.
            Ok(Box::new((guard, level)) as Box<dyn std::any::Any>)
        })
    }
}

/// Native-only credential source sharing every client writer's operation fence.
#[derive(Clone)]
pub struct ClientNativeEntitlementSource {
    core: std::sync::Arc<super::ops::Core>,
}

impl super::LicenseClient {
    /// Returns a native source; disabled licensing has no authorizer.
    pub fn native_entitlement_source(&self) -> Result<ClientNativeEntitlementSource, LicenseError> {
        Ok(ClientNativeEntitlementSource {
            core: self.configured()?.clone(),
        })
    }
}

pub(crate) fn native_store_error(error: &rightkit_credentials::Error) -> crate::NativeSourceError {
    use rightkit_credentials::Error;
    match error {
        Error::Unavailable(_) => crate::NativeSourceError::Unavailable,
        _ => crate::NativeSourceError::Refused,
    }
}

impl ClientNativeEntitlementSource {
    fn binding(
        &self,
        record: &super::envelope::StoredRecord,
    ) -> Result<String, crate::NativeSourceError> {
        let result = if record.lease.is_some() || record.binding_lease_id.is_some() {
            self.core.device.device_id_hash()
        } else {
            self.core.device.device_id()
        };
        result.map_err(|_| crate::NativeSourceError::Refused)
    }
}

impl crate::NativeEntitlementSource for ClientNativeEntitlementSource {
    fn read(&self) -> Result<crate::NativeCredential, crate::NativeSourceError> {
        use super::envelope::{RecordState, RecordStatus};
        use super::persist::Loaded;
        use crate::{NativeCredential, NativeSignedCredential, NativeSourceError};
        let _level = order::enter(Level::Op);
        let _guard = self
            .core
            .op
            .acquire(ENGINE_WAIT)
            .map_err(|_| NativeSourceError::Refused)?;
        let snap = self.core.records.read_native()?;
        if snap.unresolved() || self.core.records.flagged_unresolved() {
            return Err(NativeSourceError::Refused);
        }
        let envelope = match snap.loaded {
            Loaded::Absent => return Ok(NativeCredential::Absent),
            Loaded::Corrupt => return Err(NativeSourceError::Refused),
            Loaded::Present(value) => value,
        };
        if envelope.state == RecordState::Tombstone {
            return Ok(NativeCredential::Absent);
        }
        let record = envelope.record.as_ref().ok_or(NativeSourceError::Refused)?;
        if record.status == RecordStatus::Revoked {
            return Ok(NativeCredential::Revoked);
        }
        if record.status != RecordStatus::Active {
            return Err(NativeSourceError::Refused);
        }
        let Some(token) = record.signed_token() else {
            return Ok(if record.activation_token.is_some() {
                NativeCredential::LegacyUnsigned
            } else {
                NativeCredential::Unsigned
            });
        };
        // Bare records must pass the client's authenticated import before marker CAS is possible.
        let revision = envelope
            .revision
            .clone()
            .ok_or(NativeSourceError::Refused)?;
        Ok(NativeCredential::Signed(NativeSignedCredential {
            token: token.to_owned(),
            device_binding: self.binding(record)?,
            clock_initialized: record.clock_initialized,
            revision,
        }))
    }

    fn mark_clock_initialized(
        &self,
        credential: &crate::NativeSignedCredential,
    ) -> Result<(), crate::NativeSourceError> {
        use super::envelope::{Envelope, RecordState, RecordStatus};
        use super::persist::Loaded;
        use crate::NativeSourceError;
        self.core.signalled(|| {
            let _level = order::enter(Level::Op);
            let mut guard = self
                .core
                .op
                .acquire(ENGINE_WAIT)
                .map_err(|_| NativeSourceError::Refused)?;
            let records = self
                .core
                .records
                .pinned()
                .map_err(|_| NativeSourceError::Refused)?;
            guard.records = Some(records.clone());
            let snap = records
                .read_typed()
                .map_err(|_| NativeSourceError::Refused)?;
            if snap.unresolved() || self.core.records.flagged_unresolved() {
                return Err(NativeSourceError::Refused);
            }
            let Loaded::Present(envelope) = snap.loaded else {
                return Err(NativeSourceError::Refused);
            };
            if envelope.state != RecordState::Active
                || envelope.revision.as_deref() != Some(credential.revision.as_str())
            {
                return Err(NativeSourceError::Refused);
            }
            let mut record = envelope.record.ok_or(NativeSourceError::Refused)?;
            if record.status != RecordStatus::Active
                || record.signed_token() != Some(credential.token.as_str())
                || self.binding(&record)? != credential.device_binding
            {
                return Err(NativeSourceError::Refused);
            }
            if record.clock_initialized {
                return Ok(());
            }
            record.clock_initialized = true;
            let updated = Envelope::active(record).map_err(|_| NativeSourceError::Refused)?;
            let _guard = self
                .core
                .commit_to(&records, guard, &updated)
                .map_err(|_| NativeSourceError::Refused)?;
            // Cleanup-pending commits cannot acknowledge initialization.
            let confirmed = records
                .read_typed()
                .map_err(|_| NativeSourceError::Refused)?;
            if confirmed.unresolved() || self.core.records.flagged_unresolved() {
                return Err(NativeSourceError::Refused);
            }
            Ok(())
        })
    }
}
