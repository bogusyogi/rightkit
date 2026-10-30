//! The fenced engine token source.
//!
//! An engine process verifies the stored licence itself ([`EngineEntitlements`](crate::EngineEntitlements))
//! and is an authorizer like any other: before it reads the record it takes the per-app licence
//! operation lock with a bounded 250 ms try, then checks the credentials rotation fence. A writer
//! holds that lock until its backend worker has really finished (v3.2), so a lock it can take
//! means no write is in flight; a marker, or a fence still held by a timed-out child call, means
//! an earlier write is unresolved. Each refusal is a fixed-text `Err` the engine reports as
//! `Unreadable`, never "no licence" and never an entitlement. The record it reads is the envelope:
//! a tombstone is no licence. An older bare record (no revision) never grants as it stands: the
//! client's native source (`ClientNativeEntitlementSource`) authenticates a same-item bare signed
//! active record and rewrites it as an envelope on its first read, under the operation lock it
//! already holds, and refuses it when it does not authenticate (see [`NativeRefusal`]).
//! [`stored_signed_token`](crate::stored_signed_token) still reads either form for the legacy
//! engine token source.

use std::path::PathBuf;
use std::time::Duration;

use rightkit_credentials::Store;

use super::lock::{OpLock, STATUS_WAIT};
use super::ops::{admit, Admission, UpgradeRefusal, Upgraded};
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
    #[deprecated(
        note = "an engine never reads the owner's credential store (rightkit-credentials README, \"Helpers, sidecars and CLIs\"): the owner sends it LicenseClient::engine_snapshot over rightkit_credentials::handoff and the engine verifies it with EngineEntitlements::from_snapshot"
    )]
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

/// Why the native source refused its last read: a fixed code, never a token, device id,
/// revision, path or store text. The native source reports the refusal itself only as
/// `NativeSourceError::Refused`; this says which one, through
/// [`LicenseClient::last_native_refusal`](super::LicenseClient::last_native_refusal).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum NativeRefusal {
    /// The licence operation lock is held.
    Busy,
    /// A marker or an unfinished write stands, or a bare record's upgrade did not settle.
    Unresolved,
    /// The credential store could not be read, or its fence could not be checked.
    Store,
    /// The stored item is not a record this crate understands.
    Corrupt,
    /// A bare (older, revision-less) record whose signed token did not authenticate. It is
    /// neither rewritten nor deleted, and the status view reports migration required.
    Unauthenticated,
    /// The device identity could not be read.
    Device,
    /// The record is present but is not active (neither revoked nor a signed active licence).
    InvalidRecord,
}

impl NativeRefusal {
    /// The stable snake_case code.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::Unresolved => "unresolved",
            Self::Store => "store",
            Self::Corrupt => "corrupt",
            Self::Unauthenticated => "unauthenticated",
            Self::Device => "device",
            Self::InvalidRecord => "invalid_record",
        }
    }

    pub(crate) const fn code(self) -> u8 {
        match self {
            Self::Busy => 1,
            Self::Unresolved => 2,
            Self::Store => 3,
            Self::Corrupt => 4,
            Self::Unauthenticated => 5,
            Self::Device => 6,
            Self::InvalidRecord => 7,
        }
    }

    pub(crate) const fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            1 => Self::Busy,
            2 => Self::Unresolved,
            3 => Self::Store,
            4 => Self::Corrupt,
            5 => Self::Unauthenticated,
            6 => Self::Device,
            7 => Self::InvalidRecord,
            _ => return None,
        })
    }
}

impl std::fmt::Display for NativeRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Native-only credential source sharing every client writer's operation fence.
#[derive(Clone)]
pub struct ClientNativeEntitlementSource {
    core: std::sync::Arc<super::ops::Core>,
}

impl super::LicenseClient {
    /// The reason the native source refused its last read, or `None` when that read was not
    /// refused (or none has happened, or licensing is disabled). A fixed code only.
    pub fn last_native_refusal(&self) -> Option<NativeRefusal> {
        self.core
            .as_ref()
            .and_then(|core| core.last_native_refusal())
    }

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

impl ClientNativeEntitlementSource {
    /// One read under the operation lock. Must not call `signalled`: the engine's continuity
    /// mutex is held around this call. The refusal code rides beside the error.
    fn read_coded(
        &self,
    ) -> Result<crate::NativeCredential, (crate::NativeSourceError, NativeRefusal)> {
        use super::envelope::RecordStatus;
        use crate::{NativeCredential, NativeSignedCredential, NativeSourceError};
        let refuse = |why: NativeRefusal| (NativeSourceError::Refused, why);
        let _level = order::enter(Level::Op);
        let mut guard = self.core.op.acquire(ENGINE_WAIT).map_err(|error| {
            refuse(match error {
                LicenseError::Busy => NativeRefusal::Busy,
                _ => NativeRefusal::Store,
            })
        })?;
        let records = self
            .core
            .records
            .pinned()
            .map_err(|_| refuse(NativeRefusal::Store))?;
        guard.records = Some(Box::new(records.clone()));
        let mut snap = records.read_native()?;
        if matches!(
            admit(&snap, self.core.records.flagged_unresolved()),
            Admission::BareSigned(_)
        ) {
            match self.core.upgrade_bare_locked(&records, guard, snap) {
                Upgraded::Rebound(held, fresh) => {
                    guard = held;
                    snap = fresh;
                }
                Upgraded::Left(_, _, why) => {
                    return Err(refuse(match why {
                        UpgradeRefusal::Unauthenticated => NativeRefusal::Unauthenticated,
                        UpgradeRefusal::Device => NativeRefusal::Device,
                        UpgradeRefusal::Store => NativeRefusal::Store,
                        UpgradeRefusal::NotBare | UpgradeRefusal::Unresolved => {
                            NativeRefusal::Unresolved
                        }
                    }))
                }
                Upgraded::Failed(why) => {
                    return Err(refuse(match why {
                        UpgradeRefusal::Store => NativeRefusal::Store,
                        _ => NativeRefusal::Unresolved,
                    }))
                }
            }
        }
        // The guard stays held until the read has returned.
        let _guard = guard;
        match admit(&snap, self.core.records.flagged_unresolved()) {
            Admission::Unresolved => Err(refuse(NativeRefusal::Unresolved)),
            Admission::Absent | Admission::Tombstone => Ok(NativeCredential::Absent),
            Admission::Corrupt => Err(refuse(NativeRefusal::Corrupt)),
            // A bare record that is still bare here was not rebound: never a grant.
            Admission::BareSigned(_) => Err(refuse(NativeRefusal::Unauthenticated)),
            Admission::NonGranting(record) => match record.status {
                RecordStatus::Revoked => Ok(NativeCredential::Revoked),
                RecordStatus::Active => Ok(if record.activation_token.is_some() {
                    NativeCredential::LegacyUnsigned
                } else {
                    NativeCredential::Unsigned
                }),
                RecordStatus::Invalid => Err(refuse(NativeRefusal::InvalidRecord)),
            },
            Admission::Signed { revision, record } => {
                let token = record
                    .signed_token()
                    .ok_or_else(|| refuse(NativeRefusal::InvalidRecord))?;
                let device_binding = self
                    .binding(record)
                    .map_err(|_| refuse(NativeRefusal::Device))?;
                Ok(NativeCredential::Signed(NativeSignedCredential {
                    token: token.to_owned(),
                    device_binding,
                    clock_initialized: record.clock_initialized,
                    revision: revision.to_owned(),
                }))
            }
        }
    }
}

impl crate::NativeEntitlementSource for ClientNativeEntitlementSource {
    fn read(&self) -> Result<crate::NativeCredential, crate::NativeSourceError> {
        match self.read_coded() {
            Ok(credential) => {
                self.core.note_native_refusal(None);
                Ok(credential)
            }
            Err((error, why)) => {
                self.core.note_native_refusal(Some(why));
                Err(error)
            }
        }
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
            guard.records = Some(Box::new(records.clone()));
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
