//! Shared host services behind one caller policy (feature `host-services`).
//!
//! Every Right Suite app hand-wrote the same privileged Tauri commands: settings get/save, API-key
//! slots, logs, licence, update. Each copy decided on its own which window may call it. This module
//! registers those services ONCE, under one [`HostCallerPolicy`], and the shell's [`crate::handler!`]
//! macro lists their commands, so an app neither re-declares the commands nor re-implements the
//! check.
//!
//! ```ignore
//! let services = HostServices::builder()
//!     .settings(
//!         SettingsHostService::new(SettingsBridge::new(document, MySchema)),
//!         ServiceTrust::for_labels(["settings"]),
//!     )
//!     .secrets(
//!         SecretsService::new(my_store, [ApiKeySlot::new(SecretRef::new("myapp", "openai"))])
//!             .with_writer(my_writer),
//!         ServiceTrust::for_labels(["settings"]),
//!     )
//!     .logs(ServiceTrust::for_labels(["main", "settings"]))
//!     .build()?;
//! services.install(app.handle())?;            // in the app's setup, after the shell plugin
//! // invoke_handler: rightkit_tauri_shell::handler![...]  (nothing extra to list)
//! ```
//!
//! # Who is calling
//!
//! The caller is read from Tauri's native invocation context: the `Webview` the IPC layer injects
//! into the command (its label and current URL) plus the shell's own registries. The renderer's
//! arguments are never consulted ([`policy`]). Every command here gathers the facts first, asks
//! [`HostServices::service_for`] (or the secrets and settings wrappers over it), and only then
//! touches its service, so a refused caller reaches no store, no document and no credential.
//!
//! # Registration points for later services
//!
//! The licence service is native-owned and shipped here (feature `license`): the five
//! `rightkit_license_*` commands and the `rightkit://license-changed` event over
//! `rightkit_license::LicenseClient`, registered with `HostServicesBuilder::license_client`
//! (the `license` module states the rules; no token or key ever reaches the renderer). Native
//! models & crash services have concrete registration methods under features `models` &
//! `host-services` + `crash`; their commands run through `service_for` & the shared handler chain.
//!
//! # The shell's own commands
//!
//! [`HostServicesBuilder::guard_shell_commands`] turns on [`guard_shell_command`] for every
//! `rk_shell_*` command: each one asks the policy, with the native caller, BEFORE it reads state
//! or has any effect (`rk_shell_clipboard_read` included). Off by default, so existing apps are
//! unchanged; an app that renders untrusted content next to its own windows enables it. One command
//! is exempt on purpose: [`SHELL_GUARD_EXEMPT`] (`rk_shell_open_external`, which the injected
//! hardening script calls from every webview, content webviews included, and which carries its
//! own scheme and size policy).
//!
//! # What is NOT here
//!
//! * No generic credential get or export, no arbitrary file path, SQL, shell command or
//!   authorization-header command exists in this module ([`secrets`] states the secret rules).
//! * No thread or timer: external settings edits are noticed when the app calls
//!   [`settings::SettingsBridge::poll_external`] (or on the next `get`).

#[cfg(all(feature = "host-services", feature = "crash"))]
pub mod crash;
pub mod legal;
#[cfg(feature = "license")]
pub mod license;
#[cfg(feature = "models")]
pub mod models;
pub mod policy;
pub mod secrets;
pub mod settings;

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Manager, Webview, Wry};

use crate::caller_gate::{self, CallerError, CallerFacts, Denial};
use crate::error::ShellError;

#[cfg(all(feature = "host-services", feature = "crash"))]
pub use crash::CrashHostService;
pub use legal::{
    CreateRequest as LegalCreateRequest, DeclaredDocument, DocumentReceiptStore, LegalClock,
    LegalManifest, LegalReceiptService, LegalReceiptStore, LegalRecord, SystemLegalClock,
};
#[cfg(feature = "license")]
pub use license::{LicenseHostService, LicenseOps, EVENT_LICENSE_CHANGED};
#[cfg(feature = "models")]
pub use models::{DeclaredModel, ModelEntry, ModelState, ModelsHostService, EVENT_MODELS_ENTRY};
pub use policy::{
    is_shell_guard_exempt, HostCallerPolicy, ServiceId, ServiceTrust, SHELL_GUARD_EXEMPT,
};
pub use secrets::{
    ApiKeySlot, SecretPresence, SecretStore, SecretStoreError, SecretValue, SecretWriter,
    SecretsError, SecretsService, WriteIntent,
};
pub use settings::{
    SettingsBridge, SettingsChange, SettingsDescription, SettingsError, SettingsHostService,
    SettingsSchema, SettingsSnapshot, SettingsSubscription, StaticSchema,
};

/// The error every host command rejects with: `{ "code", "message", "details"? }`. The first two
/// fields are the shape of [`ShellError`], so a renderer that handles shell errors handles these.
/// `details` carries typed facts for the codes that have them (`revision_conflict`:
/// `{ expected, actual }`). The message never carries a secret value or a file path.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HostError {
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl HostError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid", message)
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new("unsupported", message)
    }

    pub fn other(message: impl Into<String>) -> Self {
        Self::new("other", message)
    }
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for HostError {}

impl From<CallerError> for HostError {
    /// A refused caller is `permission_denied`, the code [`ShellError`] gives it.
    fn from(e: CallerError) -> Self {
        Self::new("permission_denied", e.to_string())
    }
}

impl From<ShellError> for HostError {
    fn from(e: ShellError) -> Self {
        Self::new(e.code(), e.message().to_owned())
    }
}

type Slot = Arc<dyn Any + Send + Sync>;

/// The registered host services and their caller policy. Managed Tauri state, installed once with
/// [`HostServices::install`].
pub struct HostServices {
    policy: HostCallerPolicy,
    slots: BTreeMap<ServiceId, Slot>,
}

impl HostServices {
    pub fn builder() -> HostServicesBuilder {
        HostServicesBuilder::default()
    }

    /// The central caller policy.
    pub fn policy(&self) -> &HostCallerPolicy {
        &self.policy
    }

    /// The instance registered for `id`, as type `T`. `None` when nothing is registered, the
    /// service was registered with trust only (logs), or it was registered as another type. This
    /// does NOT check a caller: command code uses [`HostServices::service_for`].
    pub fn service<T: Any + Send + Sync>(&self, id: ServiceId) -> Option<Arc<T>> {
        self.slots.get(&id).cloned()?.downcast::<T>().ok()
    }

    /// Check `facts` may run `action` of `id` and, only then, hand back the service instance. The
    /// first call of every host command.
    pub fn service_for<T: Any + Send + Sync>(
        &self,
        id: ServiceId,
        action: &str,
        facts: &CallerFacts<'_>,
    ) -> Result<Arc<T>, HostError> {
        self.policy.decide(id, action, facts)?;
        self.service::<T>(id).ok_or_else(|| {
            HostError::unsupported(format!("service {id} has no instance of the expected kind"))
        })
    }

    /// Attach the settings change emitter (when a settings service is registered) and manage the
    /// services as Tauri state. Fails when host services were already installed: they register
    /// once.
    pub fn install(self, app: &AppHandle<Wry>) -> Result<(), HostError> {
        if app.try_state::<HostServices>().is_some() {
            return Err(HostError::invalid("host services are already installed"));
        }
        if let Some(settings) = self.service::<SettingsHostService>(ServiceId::SETTINGS) {
            let trust = self.policy.trust_of(ServiceId::SETTINGS).cloned();
            settings.attach_emitter(app.clone(), trust);
        }
        #[cfg(feature = "license")]
        {
            if let Some(license) = self.service::<LicenseHostService>(ServiceId::LICENSE) {
                license.attach_emitter(app.clone());
            }
        }
        if app.manage(self) {
            Ok(())
        } else {
            Err(HostError::invalid("host services are already installed"))
        }
    }
}

/// Builds [`HostServices`]. Every service is registered once; trust is checked when
/// [`HostServicesBuilder::build`] runs.
#[derive(Default)]
pub struct HostServicesBuilder {
    policy: HostCallerPolicy,
    slots: BTreeMap<ServiceId, Slot>,
    duplicates: Vec<ServiceId>,
}

impl HostServicesBuilder {
    /// Register any service as a typed slot with its trust: the generic registration point.
    pub fn service<T: Any + Send + Sync>(
        mut self,
        id: ServiceId,
        trust: ServiceTrust,
        instance: T,
    ) -> Self {
        if self.slots.contains_key(&id) || self.policy.trust_of(id).is_some() {
            self.duplicates.push(id);
            return self;
        }
        self.policy = self.policy.with_service(id, trust);
        self.slots.insert(id, Arc::new(instance));
        self
    }

    /// Register the settings bridge.
    pub fn settings(self, service: SettingsHostService, trust: ServiceTrust) -> Self {
        self.service(ServiceId::SETTINGS, trust, service)
    }

    /// Register the secret slots, presence and clear. `set` is accepted only from a caller the
    /// settings service also trusts, so register the settings service too.
    pub fn secrets(self, service: SecretsService, trust: ServiceTrust) -> Self {
        self.service(ServiceId::SECRETS, trust, service)
    }

    /// Register the trust of the renderer log commands (`logs_commands`, feature `logs`). The
    /// runtime stays the managed `LogsRuntime`; only the caller policy moves here.
    pub fn logs(mut self, trust: ServiceTrust) -> Self {
        if self.policy.trust_of(ServiceId::LOGS).is_some() {
            self.duplicates.push(ServiceId::LOGS);
            return self;
        }
        self.policy = self.policy.with_service(ServiceId::LOGS, trust);
        self
    }

    /// Slot for the licence service. The shell's own licence commands need a
    /// `LicenseHostService` here (`HostServicesBuilder::license_client` builds it); any other
    /// type is stored but every `rightkit_license_*` command then answers `unsupported`.
    pub fn license<T: Any + Send + Sync>(self, trust: ServiceTrust, instance: T) -> Self {
        self.service(ServiceId::LICENSE, trust, instance)
    }

    /// Register the native licence client behind the five `rightkit_license_*` commands
    /// (`status`, `activate`, `refresh`, `deactivate`, `request_trial` are the trust's actions) and
    /// the `rightkit://license-changed` event (feature `license`). Trust the window that manages
    /// the licence; a registered content webview is refused regardless.
    #[cfg(feature = "license")]
    pub fn license_client(
        self,
        trust: ServiceTrust,
        client: rightkit_license::LicenseClient,
    ) -> Self {
        self.service(ServiceId::LICENSE, trust, LicenseHostService::new(client))
    }

    /// Slot for the update service.
    pub fn update<T: Any + Send + Sync>(self, trust: ServiceTrust, instance: T) -> Self {
        self.service(ServiceId::UPDATE, trust, instance)
    }

    /// Register the native model store & declared catalog (feature `models`).
    #[cfg(feature = "models")]
    pub fn models(self, trust: ServiceTrust, service: ModelsHostService) -> Self {
        self.service(ServiceId::MODELS, trust, service)
    }

    /// Register the native legal receipt (`rightkit_legal_receipt_read`, `_create`, `_reset`). Trust
    /// the window that shows the legal gate; a registered content webview is refused regardless.
    pub fn legal(self, trust: ServiceTrust, service: LegalReceiptService) -> Self {
        self.service(ServiceId::LEGAL, trust, service)
    }

    /// Register renderer crash capture under caller policy (features `host-services` & `crash`).
    #[cfg(all(feature = "host-services", feature = "crash"))]
    pub fn crash(self, trust: ServiceTrust, service: CrashHostService) -> Self {
        self.service(ServiceId::CRASH, trust, service)
    }

    /// Also guard the shell's privileged `rk_shell_*` commands for these windows (see
    /// [`HostCallerPolicy::guard_shell_commands`]).
    pub fn guard_shell_commands(mut self, trust: ServiceTrust) -> Self {
        self.policy = self.policy.guard_shell_commands(trust);
        self
    }

    /// Validate every trust and finish. Fails on a service registered twice or a trust that would
    /// trust every window.
    pub fn build(self) -> Result<HostServices, HostError> {
        if let Some(id) = self.duplicates.first() {
            return Err(HostError::invalid(format!(
                "service {id} is registered more than once"
            )));
        }
        self.policy.validate().map_err(HostError::invalid)?;
        Ok(HostServices {
            policy: self.policy,
            slots: self.slots,
        })
    }
}

/// The `HostServices` managed by the app, or the refusal a command answers when there are none.
pub(crate) fn managed(app: &AppHandle<Wry>) -> Result<tauri::State<'_, HostServices>, CallerError> {
    app.try_state::<HostServices>()
        .ok_or(CallerError::Forbidden(Denial::HostServicesNotInstalled))
}

/// Check the invoking webview may run `action` of `service`. The caller is read from the native
/// invocation context ([`caller_gate::gather`]).
pub fn authorize(
    webview: &Webview<Wry>,
    service: ServiceId,
    action: &str,
) -> Result<(), CallerError> {
    let app = webview.app_handle();
    let host = managed(app)?;
    let owned = caller_gate::gather(webview)?;
    host.policy().decide(service, action, &owned.facts())
}

/// [`authorize`], except that an app which installed no host services keeps the shell-wide app
/// caller gate. The renderer log commands use it, so `logs` without `host-services` setup behaves
/// as before.
pub fn authorize_or_app_gate(
    webview: &Webview<Wry>,
    service: ServiceId,
    action: &str,
) -> Result<(), CallerError> {
    if webview.app_handle().try_state::<HostServices>().is_none() {
        return caller_gate::require_app_caller(webview);
    }
    authorize(webview, service, action)
}

/// Check the invoking webview may run the shell command `command`. Allowed while no host services
/// are installed, [`HostCallerPolicy::guard_shell_commands`] was not enabled, or `command` is in
/// [`SHELL_GUARD_EXEMPT`]. Every `rk_shell_*` command calls it first (`commands::guard_command`),
/// before it reads state or has any effect.
pub fn guard_shell_command(webview: &Webview<Wry>, command: &str) -> Result<(), CallerError> {
    let app = webview.app_handle();
    let Some(host) = app.try_state::<HostServices>() else {
        return Ok(());
    };
    if host.policy().shell_trust().is_none() || is_shell_guard_exempt(command) {
        return Ok(());
    }
    let owned = caller_gate::gather(webview)?;
    shell_command_for(&host, command, &owned.facts())
}

/// The decision [`guard_shell_command`] takes over already gathered facts: allowed while the guard
/// is off or `command` is exempt ([`SHELL_GUARD_EXEMPT`]), otherwise only for a trusted app window
/// on a trusted origin with `command` enabled.
/// Pure, so a test exercises the denial of a content window, a foreign origin or a disabled
/// command without a webview.
pub fn shell_command_for(
    host: &HostServices,
    command: &str,
    facts: &CallerFacts<'_>,
) -> Result<(), CallerError> {
    host.policy().decide_shell(command, facts)
}

/// Run blocking work off the command thread.
pub(crate) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, HostError> + Send + 'static,
) -> Result<T, HostError> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| HostError::other(e.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nav::NavPolicy;

    struct Probe(u32);

    fn trust() -> ServiceTrust {
        ServiceTrust::for_labels(["main"])
    }

    #[test]
    fn a_slot_is_typed_and_registered_once() {
        let services = HostServices::builder()
            .license(trust(), Probe(7))
            .build()
            .unwrap();
        assert_eq!(services.service::<Probe>(ServiceId::LICENSE).unwrap().0, 7);
        // Another type under the same id is not the instance.
        assert!(services.service::<String>(ServiceId::LICENSE).is_none());
        assert!(services.service::<Probe>(ServiceId::UPDATE).is_none());

        let twice = HostServices::builder()
            .license(trust(), Probe(1))
            .license(trust(), Probe(2))
            .build();
        assert!(twice.is_err());
    }

    #[test]
    fn a_trust_that_would_admit_every_window_does_not_build() {
        let bad = HostServices::builder()
            .update(ServiceTrust::for_labels(["*"]), Probe(1))
            .build();
        assert!(bad.is_err());
        let none = HostServices::builder()
            .service(
                ServiceId::MODELS,
                ServiceTrust::for_labels(Vec::<String>::new()),
                Probe(1),
            )
            .build();
        assert!(none.is_err());
    }

    #[test]
    fn service_for_checks_the_caller_before_it_hands_out_the_instance() {
        let services = HostServices::builder()
            .service(ServiceId::CRASH, trust(), Probe(3))
            .build()
            .unwrap();
        let nav = NavPolicy::default();
        let good = tauri::Url::parse("tauri://localhost/").unwrap();
        let bad = tauri::Url::parse("https://evil.example/").unwrap();
        fn facts<'a>(label: &'a str, url: &'a tauri::Url, nav: &'a NavPolicy) -> CallerFacts<'a> {
            CallerFacts {
                label,
                is_app_window: true,
                content_labels: &[],
                url: Some(url),
                nav,
            }
        }
        let ok =
            services.service_for::<Probe>(ServiceId::CRASH, "report", &facts("main", &good, &nav));
        assert_eq!(ok.unwrap().0, 3);
        let foreign =
            services.service_for::<Probe>(ServiceId::CRASH, "report", &facts("main", &bad, &nav));
        assert_eq!(foreign.err().unwrap().code, "permission_denied");
        let other_window =
            services.service_for::<Probe>(ServiceId::CRASH, "report", &facts("popup", &good, &nav));
        assert_eq!(other_window.err().unwrap().code, "permission_denied");
    }

    #[test]
    fn host_errors_keep_the_shell_error_wire_shape() {
        let e = HostError::new("revision_conflict", "stale")
            .with_details(serde_json::json!({"expected": 1, "actual": 2}));
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["code"], "revision_conflict");
        assert_eq!(v["message"], "stale");
        assert_eq!(v["details"]["actual"], 2);
        let plain = serde_json::to_value(HostError::invalid("x")).unwrap();
        assert!(plain.get("details").is_none());
        let from_shell: HostError = ShellError::Unsupported("nope".into()).into();
        assert_eq!(from_shell.code, "unsupported");
        let from_caller: HostError = CallerError::Forbidden(Denial::ContentWebview).into();
        assert_eq!(from_caller.code, "permission_denied");
        assert!(from_caller.message.starts_with("forbidden: "));
    }
}
