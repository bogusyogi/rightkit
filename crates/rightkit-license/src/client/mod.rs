//! The native licence client (feature `client`, M12).
//!
//! The licence key and the trial e-mail pass through the renderer once, as input. Every credential
//! the server issues (the activation token, the signed token, a lease envelope) is held here, in
//! one envelope-versioned item of the OS credential store, and never returned, emitted, logged or
//! persisted anywhere else: the operations return the fixed-schema views of [`view`], and no
//! token-bearing type is `Serialize`.
//!
//! ## Modes
//! [`LicenseSetup::Disabled`] is an app with no licence product or no provisioned key: `status`
//! answers `mode: disabled`, every mutation is [`LicenseError::Unconfigured`] and uses no
//! network. [`LicenseSetup::configured`] needs a real public key and a compiled-in host.
//!
//! ## Locking and fencing
//! One OS lock per app (`<license dir>/.lock`), only ever taken with `try_acquire` polled against
//! a deadline (5 s for a mutation, 250 ms for a status or authorization read). A mutation holds
//! it for its WHOLE workflow, and every record write runs on a worker that owns the guard until
//! the backend (including any timed-out child call) has really finished. Fixed lock order:
//! operation lock, then credentials rotation, then clock; a reentry or inversion is a debug
//! assertion. See `lock`, `persist` and `order`.
//!
//! ## Persistence
//! Each write is a rotation pair (`begin_rotation`, `commit_rotation_of`) of a fresh envelope with
//! a random 128-bit revision; deactivation writes a tombstone, never a delete. Outcomes are
//! `Committed`, `CommittedCleanupPending`, `NotCommitted` or `Indeterminate`; anything not clean
//! is resolved by readback, and while a marker or an unfinished write stands every authorizer
//! (status, the engine reader, update authorization) fails closed. Recovery never resurrects a
//! tombstone.
//!
//! ## Account session, Pro bridge, link codes and invalidation (0.2.4, PK05-5)
//! `AccountSessionClient` (`account`) is the passwordless account session: request and complete a
//! sign-in, status, refresh, sign-out, list, rename and remove devices, and account deletion as a
//! typed destructive action that keeps a server refusal as a result. Its session token is one
//! revisioned credential item (`account_store`), never leaves native code, and a result that
//! arrives after a sign-out is discarded, not written. The Pro bridge (`bridge_publish`,
//! `bridge_exchange`) and the device-link codes (`link_request`, `link_redeem`) are licence
//! operations on [`LicenseClient`], over the service paths the apps use today (`bridge`, `wire`).
//! [`LicenseInvalidation`] carries every committed licence change to live engine verifiers
//! (`EngineEntitlements::with_invalidation`) and to listeners that hint another process.
//!
//! ## What it does not do
//! It has no background task: the caller invokes `refresh` when it wants one. It does not own the
//! device identity (a [`DeviceIdSource`]), the renderer bridge, or the updater transport.

mod account;
mod account_store;
mod bridge;
mod clockstore;
mod envelope;
mod invalidation;
mod lock;
mod ops;
mod order;
mod persist;
mod reader;
mod time;
mod types;
#[cfg(feature = "update-auth")]
mod update_auth;
mod view;
mod wire;

use std::sync::Arc;

use ops::Core;

pub use account::{
    AccountClientConfig, AccountCommandError, AccountDeletion, AccountDeviceView, AccountError,
    AccountErrorCode, AccountRejectReason, AccountSessionClient, AccountStatusView,
    DeleteAccountConfirmation, DeletionRefusal, ACCOUNT_SESSION_TTL_SECS, DEFAULT_SESSION_ACCOUNT,
};
pub use bridge::{BridgePlatform, LinkCodeView, LinkRequestOutcome};
pub use invalidation::LicenseInvalidation;
pub use ops::LegacySource;
pub use reader::{ENGINE_BUSY_TEXT, ENGINE_STORE_TEXT, ENGINE_UNRESOLVED_TEXT};
pub use types::{
    DeviceIdError, DeviceIdSource, LicenseClientConfig, LicenseSetup, SecretString,
    DEFAULT_RECORD_ACCOUNT,
};
#[cfg(feature = "update-auth")]
pub use update_auth::{
    AuthorizedHeaders, LicenseRevision, StagedIdentity, UpdateAuthConfig, UpdateAuthError,
    UpdateAuthProvider, UpdateBinding, UpdateRoute, DEFAULT_HEADER_TTL,
};
pub use view::{
    CommandCode, CommandError, LicenseError, LicenseMode, LicenseView, MessageCode, RefreshView,
    RefreshViewOutcome, RejectReason, RevisionState, TrialView, ViewKind, ViewStage, ViewStatus,
};

/// The native licence client. Cheap to clone (a handle).
#[derive(Clone)]
pub struct LicenseClient {
    core: Option<Arc<Core>>,
    invalidation: LicenseInvalidation,
}

impl std::fmt::Debug for LicenseClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LicenseClient")
            .field("configured", &self.core.is_some())
            .finish_non_exhaustive()
    }
}

impl LicenseClient {
    /// Build the client for `setup`. A configured client reads the system clock once for its
    /// process anchor and builds its HTTP client; neither touches the network or the store.
    pub fn new(setup: LicenseSetup, device: Arc<dyn DeviceIdSource>) -> Result<Self, LicenseError> {
        match setup {
            LicenseSetup::Disabled => Ok(Self {
                core: None,
                invalidation: LicenseInvalidation::new(),
            }),
            LicenseSetup::Configured(config) => {
                let invalidation = config.invalidation.clone().unwrap_or_default();
                Ok(Self {
                    core: Some(Arc::new(Core::new(*config, device, invalidation.clone())?)),
                    invalidation,
                })
            }
        }
    }

    /// The change counter this client signals after every operation that changed, or tried to
    /// change, the stored licence (and when a leftover write was resolved). Give clones to the
    /// engine verifiers (`EngineEntitlements::with_invalidation`) and subscribe the shell to it to
    /// hint another process. A disabled client never signals.
    pub fn invalidation(&self) -> LicenseInvalidation {
        self.invalidation.clone()
    }

    /// Whether licensing is disabled in this build.
    pub fn is_disabled(&self) -> bool {
        self.core.is_none()
    }

    pub(crate) fn core(&self) -> Option<&Arc<Core>> {
        self.core.as_ref()
    }

    fn configured(&self) -> Result<&Arc<Core>, LicenseError> {
        self.core.as_ref().ok_or(LicenseError::Unconfigured)
    }

    /// The licence as it stands. No network. Takes the lock with a 250 ms try; when it is held
    /// the answer is the last committed view marked `busy` (display only), and an unresolved
    /// write is shown as `unresolved` with no entitlement. A leftover rotation marker is
    /// reconciled here when the lock is free.
    pub fn status(&self) -> LicenseView {
        match &self.core {
            Some(core) => core.status(),
            None => LicenseView::disabled(),
        }
    }

    /// Exchange a licence key for an activation, verify the signed token it returns, move the
    /// clock, and store the record. The key is zeroed when this returns.
    pub fn activate(&self, key: SecretString) -> Result<LicenseView, LicenseError> {
        let core = self.configured()?;
        core.signalled(|| core.activate(key))
    }

    /// Re-validate with the server and re-anchor the clock. A lease record renews its lease.
    pub fn refresh(&self) -> Result<RefreshView, LicenseError> {
        let core = self.configured()?;
        core.signalled(|| core.refresh())
    }

    /// Free this device's seat (best effort) and write the deactivation tombstone. Never
    /// succeeds without the tombstone committed: a failed clear is `Store` or `Unresolved`.
    pub fn deactivate(&self) -> Result<LicenseView, LicenseError> {
        let core = self.configured()?;
        core.signalled(|| core.deactivate())
    }

    /// Ask the licence service to e-mail a trial key. Issues no credential.
    pub fn request_trial(&self, email: &str) -> Result<TrialView, LicenseError> {
        self.configured()?.request_trial(email)
    }

    /// Native only: activate through the renewable device-lease endpoint.
    pub fn lease_activate(
        &self,
        key: SecretString,
        device_label: Option<&str>,
    ) -> Result<LicenseView, LicenseError> {
        let core = self.configured()?;
        core.signalled(|| core.lease_activate(key, device_label))
    }

    /// Native only: renew the stored device lease.
    pub fn lease_renew(&self) -> Result<RefreshView, LicenseError> {
        let core = self.configured()?;
        core.signalled(|| core.lease_renew())
    }

    /// Native only: release the lease on the server (best effort) and tombstone the record.
    pub fn lease_deactivate(&self) -> Result<LicenseView, LicenseError> {
        let core = self.configured()?;
        core.signalled(|| core.deactivate())
    }

    /// Native only migration of an app's older record. Reads `source`, authenticates its signed
    /// token (signature, app, device; no time check, so an expired licence imports as expired
    /// and a grace one keeps its anchors), writes the envelope, and only after that commits
    /// removes the legacy item (never when it is the licence item itself, which is upgraded in
    /// place). An install that already holds an envelope, active or tombstone, imports nothing.
    pub fn import_legacy(&self, source: &LegacySource) -> Result<LicenseView, LicenseError> {
        let core = self.configured()?;
        core.signalled(|| core.import_legacy(source))
    }

    /// [`import_legacy`](Self::import_legacy) for record text the caller already read. The caller
    /// removes its own copy after `Ok`.
    pub fn import_legacy_json(&self, json: SecretString) -> Result<LicenseView, LicenseError> {
        let core = self.configured()?;
        core.signalled(|| core.import_legacy_json(json))
    }

    /// Native only: publish this device's activation to the Pro bridge
    /// (`POST /v1/licenses/bridge/from-activation`) and return the opaque bridge token the caller
    /// stores in its synchronizable store. `Ok(None)` when nothing can be published: no stored
    /// licence, one that does not currently grant access, or no activation credential. It never
    /// changes the stored licence. The token is not an entitlement and unlocks nothing by itself;
    /// it is native-only and never belongs in a renderer view.
    pub fn bridge_publish(&self) -> Result<Option<SecretString>, LicenseError> {
        self.configured()?.bridge_publish()
    }

    /// Native only: exchange a bridge token for a signed device lease on this device
    /// (`POST /v1/licenses/bridge/exchange`), verify it like any lease and store it. A device
    /// that already holds a licence granting access is left alone and answered with its current
    /// view (no network). The token is zeroed when this returns.
    pub fn bridge_exchange(
        &self,
        bridge_token: SecretString,
        platform: BridgePlatform,
    ) -> Result<LicenseView, LicenseError> {
        let core = self.configured()?;
        core.signalled(|| core.bridge_exchange(bridge_token, platform))
    }

    /// Native only: ask the licence service for a short-lived, single-use link code that a second
    /// device redeems (`POST /v1/pro/{app}/devices/windows-link/request`). The caller proves
    /// ownership with this device's own lease; the code is meant to be shown to the user and typed
    /// into the other device, so the view carries it.
    pub fn link_request(&self) -> Result<LinkRequestOutcome, LicenseError> {
        self.configured()?.link_request()
    }

    /// Native only: redeem a link code on this device (`POST /v1/pro/{app}/devices/windows-link/redeem`),
    /// verify the lease it returns like any lease and store it.
    pub fn link_redeem(
        &self,
        code: &str,
        device_label: Option<&str>,
    ) -> Result<LicenseView, LicenseError> {
        let core = self.configured()?;
        core.signalled(|| core.link_redeem(code, device_label))
    }
}
