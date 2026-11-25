//! The assembled native licence owner: client, account session, in-place legacy import, observed
//! entitlements and release queue, held together so the host builds it once.
//!
//! The host keeps what is its own: app key, host name, public key, credential item names, the
//! `LicenseClientConfig` and `AccountClientConfig` chains (they take the app's credential stores)
//! and the log vocabulary. This module owns the assembly that was identical for every app:
//! the client from a setup, the account client, the legacy import with its launch attempt, the
//! native entitlement authoriser wrapped with its verdict log and refusal cause, a disabled
//! runtime for targets with no compatible credential item, and the "retry a pending legacy import
//! before every verdict" rule.

use std::sync::Arc;

use rightkit_license::{
    AccountClientConfig, AccountSessionClient, ClientClockStore, ClientNativeEntitlementSource,
    CredentialReleaseQueue, DeviceIdSource, LegacyImport, LegacyImportEvents, LegacySource,
    LicenseClient, LicenseSetup, LifecyclePolicy, NativeEngineEntitlements, ObservedEntitlements,
    ProcessAnchor,
};

use super::license::LicenseHostService;

/// The native paid-command verdict: the signed lifecycle, verified now, against the anti-rollback
/// clock, with its cause and the verdict-change log.
pub type Entitlements = ObservedEntitlements<ClientNativeEntitlementSource, ClientClockStore>;

/// Inputs for the native entitlement authoriser.
pub struct EntitlementSpec {
    /// The suite public key that verifies entitlement tokens.
    pub public_key: [u8; 32],
    /// The app key the licence server matches.
    pub app: String,
    /// The app's entitlement policy (grace, rollback behaviour).
    pub policy: LifecyclePolicy,
    /// The `(event, detail)` sink for the verdict log.
    pub log: fn(&str, &str),
}

/// The in-place import of an older bare licence record.
pub struct LegacyImportSpec {
    /// The store and account item the older record lives in.
    pub source: LegacySource,
    /// The host's two event names (failed, retried).
    pub events: LegacyImportEvents,
    /// The `(event, detail)` sink.
    pub log: fn(&str, &str),
}

/// The process-wide native licence owner.
pub struct LicenseRuntime {
    /// The licence client.
    pub client: LicenseClient,
    /// The account session client.
    pub account: AccountSessionClient,
    /// `None` when licensing is disabled in this build (no authoriser exists, so no paid command
    /// can be authorised).
    pub native_source: Option<ClientNativeEntitlementSource>,
    entitlements: Option<Entitlements>,
    legacy_import: Option<Arc<LegacyImport>>,
    /// The durable pending-removal queue; `None` when licensing is disabled.
    pub release_queue: Option<Arc<CredentialReleaseQueue>>,
}

impl LicenseRuntime {
    /// A runtime with licensing disabled: every authorisation fails closed. For a target that has
    /// no credential item compatible with the app's existing entries. The host passes the device
    /// source (`rightkit_license::UnavailableDeviceId` when there is none).
    pub fn disabled(device: Arc<dyn DeviceIdSource>) -> Result<Self, String> {
        let client = LicenseClient::new(LicenseSetup::Disabled, device)
            .map_err(|error| error.to_string())?;
        let account = AccountSessionClient::disabled(client.clone());
        Ok(Self {
            client,
            account,
            native_source: None,
            entitlements: None,
            legacy_import: None,
            release_queue: None,
        })
    }

    /// Assemble the configured runtime.
    ///
    /// `setup` is `LicenseSetup::configured(config)`, `account_config` the account client's
    /// config built from the same `LicenseClientConfig`, `legacy` the optional legacy import
    /// (its launch attempt runs here, a failure is retried by [`Self::require_pro`] and the
    /// service's status hook), `release_queue` the durable pending-removal queue and
    /// `entitlements` the authoriser inputs. The authoriser's time anchor is the process anchor
    /// at the current wall clock; a transient clock-store error is not a refusal.
    pub fn configured(
        setup: LicenseSetup,
        device: Arc<dyn DeviceIdSource>,
        account_config: AccountClientConfig,
        legacy: Option<LegacyImportSpec>,
        release_queue: Option<Arc<CredentialReleaseQueue>>,
        entitlements: EntitlementSpec,
    ) -> Result<Self, String> {
        let client = LicenseClient::new(setup, device).map_err(|error| error.to_string())?;

        // An older build stored a bare record the native source refuses until it is imported:
        // upgrade it in place at launch, retried by the next licence call when this attempt fails.
        let legacy_import = legacy.map(|spec| {
            let importer = client.clone();
            let source = spec.source;
            let import = Arc::new(LegacyImport::new(
                rightkit_license::DEFAULT_RETRY_COOLDOWN,
                spec.events,
                spec.log,
                move || {
                    importer
                        .import_legacy(&source)
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                },
            ));
            import.first_attempt();
            import
        });

        let account = AccountSessionClient::new(account_config, client.clone())
            .map_err(|error| error.to_string())?;

        let source = client
            .native_entitlement_source()
            .map_err(|error| error.to_string())?;
        let clock = client
            .native_clock_store()
            .map_err(|error| error.to_string())?;
        let wall = rightkit_license::system_now_unix()?;
        let native = NativeEngineEntitlements::new(
            source.clone(),
            clock,
            ProcessAnchor::process(wall),
            entitlements.public_key,
            entitlements.app,
            entitlements.policy,
        )
        .with_transient_clock_errors(|error| error.is_transient());
        // The owner's fixed refusal code names why a refused source read was refused.
        let refusal_client = client.clone();
        let observed = ObservedEntitlements::new(
            native,
            Box::new(move || {
                refusal_client
                    .last_native_refusal()
                    .map(|refusal| refusal.as_str())
            }),
            entitlements.log,
        );

        Ok(Self {
            client,
            account,
            native_source: Some(source),
            entitlements: Some(observed),
            legacy_import,
            release_queue,
        })
    }

    /// Native paid-command authorisation: the signed lifecycle, verified now, against the
    /// anti-rollback clock. A pending legacy import is retried first, so a transient failure at
    /// launch does not turn every paid command off until the next launch. Never reads a renderer
    /// value, a pushed flag or the environment. `false` when licensing is disabled.
    pub fn require_pro(&self) -> bool {
        if let Some(import) = &self.legacy_import {
            import.retry_if_due();
        }
        self.entitlements
            .as_ref()
            .is_some_and(Entitlements::require_pro)
    }

    /// The cause of the most recent verdict (`granted`, `not_entitled` or a refusal code); `None`
    /// before the first check. A fixed vocabulary only.
    pub fn last_cause(&self) -> Option<&'static str> {
        self.entitlements.as_ref()?.last_cause()
    }

    /// The host service for the shell's `rightkit_license_*` commands over this client: the
    /// legacy import is retried before each status read (so the view and the native gates read
    /// the same record) and a deactivation persists the release to the durable queue before the
    /// local tombstone.
    pub fn license_service(&self) -> LicenseHostService {
        let legacy_import = self.legacy_import.clone();
        let mut service =
            LicenseHostService::new(self.client.clone()).with_before_status(Arc::new(move || {
                if let Some(import) = &legacy_import {
                    import.retry_if_due();
                }
            }));
        if let Some(queue) = &self.release_queue {
            service = service.with_release_queue(queue.clone());
        }
        service
    }
}
