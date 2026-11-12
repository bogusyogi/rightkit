//! Which windows may call which host service: [`HostCallerPolicy`].
//!
//! A host service (settings, secrets, logs, licence, update, models, crash) is a privileged native
//! surface. Each one is registered with a [`ServiceTrust`]: the window labels (exact or `prefix-*`)
//! it answers, optionally the page origins it answers, and optionally the actions it enables. A call
//! is allowed only when ALL of these hold:
//!
//! 1. the shell-wide gate ([`crate::caller_gate::decide`]) allows the caller: an app webview, not a
//!    registered content webview, on the app's own origin;
//! 2. the service was registered with a non-empty trust;
//! 3. the caller's label matches one of the service's trusted labels;
//! 4. when the trust lists origins, the caller's current page origin is one of them;
//! 5. when the trust lists actions, the requested action is one of them.
//!
//! The caller's identity is [`CallerFacts`]: the label and URL of the `Webview` Tauri injected into
//! the command and the shell's own registries. The renderer's arguments never reach this module, so
//! a content page cannot claim to be the settings window. The decision is pure; the Tauri adapter in
//! [`crate::host`] only gathers the facts.
//!
//! Trust is never inherited between services: the settings window being trusted for settings says
//! nothing about the licence service. A service with no registered trust refuses every caller.

use std::collections::{BTreeMap, BTreeSet};

use crate::caller_gate::{self, origin_of, CallerError, CallerFacts, Denial};
use crate::devtools::glob_match;

/// Shell commands the guard never refuses, whoever calls them: the commands the shell's own
/// injected hardening script (`harden.js`) calls from EVERY webview, content webviews included.
/// Today only `rk_shell_open_external`: the script turns an external link click or `window.open`
/// in a content page into this call, and a guard that refused content webviews would break every
/// external link the app renders. The exemption is narrow and deliberate: the command has no
/// state to read and one effect, handing a URL to the OS, and it carries its own policy that no
/// caller can widen (only `http`, `https` and `mailto`, at most `MAX_EXTERNAL_URL_BYTES` bytes,
/// no control character; see `opener::open_checked` and `commands::rk_shell_open_external`).
/// Enabling the guard does not change who may open a link; every other `rk_shell_*` command is
/// guarded. An app that wants external links closed to content hardens its pages instead
/// (`Hardening { intercept_external_links: false, .. }`).
pub const SHELL_GUARD_EXEMPT: [&str; 1] = ["rk_shell_open_external"];

/// True when the guard never refuses `command` ([`SHELL_GUARD_EXEMPT`]).
pub fn is_shell_guard_exempt(command: &str) -> bool {
    SHELL_GUARD_EXEMPT.contains(&command)
}

/// Names one host service. The associated constants are the services the shell knows; an app may
/// register its own with [`ServiceId::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ServiceId(&'static str);

impl ServiceId {
    /// Settings bridge (`describe`, `get`, `patch`, `reset`, `subscribe`).
    pub const SETTINGS: Self = Self("settings");
    /// Write-only API-key slots, presence and clear.
    pub const SECRETS: Self = Self("secrets");
    /// Renderer log commands.
    pub const LOGS: Self = Self("logs");
    /// Licence commands (`rightkit_license_*`, `host::license::LicenseHostService`, feature `license`).
    pub const LICENSE: Self = Self("license");
    /// Account session commands (`rightkit_account_*`, `host::account::AccountHostService`, feature `license`).
    pub const ACCOUNT: Self = Self("account");
    /// Boot probation and the allowlisted fetch bridge (`rightkit_boot_*`, `rightkit_app_update_fetch`,
    /// `host::app_update::AppUpdateHostService`, feature `app-update`).
    pub const APP_UPDATE: Self = Self("app-update");
    /// Update commands (registered by the update package).
    pub const UPDATE: Self = Self("update");
    /// Local-model commands.
    pub const MODELS: Self = Self("models");
    /// Crash reporting commands.
    pub const CRASH: Self = Self("crash");
    /// Preview document registration (`rightkit_preview_*`, [`crate::preview::PreviewService`]).
    #[cfg(feature = "viewer")]
    pub const PREVIEW: Self = Self("preview");
    /// The native legal receipt (`rightkit_legal_receipt_*`, [`crate::host::legal::LegalReceiptService`]).
    pub const LEGAL: Self = Self("legal");

    pub const fn new(name: &'static str) -> Self {
        Self(name)
    }

    pub const fn name(self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for ServiceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// Who a service answers. Built with [`ServiceTrust::for_labels`] and refined with
/// [`ServiceTrust::with_origins`] and [`ServiceTrust::with_actions`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServiceTrust {
    labels: Vec<String>,
    origins: Vec<String>,
    actions: Option<BTreeSet<String>>,
}

impl ServiceTrust {
    /// Trust the windows whose label equals one of `labels`, or matches a `prefix-*` pattern. At
    /// least one label is required, and a bare `*` is refused when the trust is registered.
    pub fn for_labels<I, S>(labels: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            labels: labels.into_iter().map(Into::into).collect(),
            origins: Vec::new(),
            actions: None,
        }
    }

    /// Also require the caller's page origin to be one of `origins`, each `scheme://host[:port]`
    /// (for example `tauri://localhost`). Without this the origin only has to be the app's own
    /// under the navigation policy, which every caller needs anyway.
    pub fn with_origins<I, S>(mut self, origins: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.origins = origins.into_iter().map(Into::into).collect();
        self
    }

    /// Enable only these actions. Without this every action of the service is enabled.
    pub fn with_actions<I, S>(mut self, actions: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.actions = Some(actions.into_iter().map(Into::into).collect());
        self
    }

    /// The trusted label patterns.
    pub fn labels(&self) -> &[String] {
        &self.labels
    }

    /// The trusted origins; empty when only the app's own origin is required.
    pub fn origins(&self) -> &[String] {
        &self.origins
    }

    /// True when `label` is one of the trusted labels or matches a trusted pattern.
    pub fn trusts_label(&self, label: &str) -> bool {
        self.labels.iter().any(|p| glob_match(p, label))
    }

    /// True when `action` is enabled.
    pub fn enables(&self, action: &str) -> bool {
        self.actions.as_ref().is_none_or(|set| set.contains(action))
    }

    /// Check that this trust is well formed. Run by the builder; the messages name no secret.
    pub fn validate(&self) -> Result<(), String> {
        if self.labels.is_empty() {
            return Err("a service trust needs at least one window label".into());
        }
        for label in &self.labels {
            if label.is_empty() || label.chars().all(|c| c == '*') {
                return Err(format!(
                    "window label pattern {label:?} would trust every window"
                ));
            }
            if label.chars().any(char::is_control) {
                return Err("a window label holds a control character".into());
            }
        }
        for origin in &self.origins {
            let ok = tauri::Url::parse(origin)
                .ok()
                .and_then(|u| origin_of(&u))
                .is_some_and(|canonical| canonical == *origin);
            if !ok {
                return Err(format!(
                    "origin {origin:?} is not a canonical scheme://host[:port]"
                ));
            }
        }
        Ok(())
    }

    /// Identity part of the decision (rules 3 and 4).
    fn trusts_caller(&self, facts: &CallerFacts<'_>) -> Result<(), Denial> {
        if !self.trusts_label(facts.label) {
            return Err(Denial::UntrustedLabel);
        }
        if !self.origins.is_empty() {
            let origin = facts.url.and_then(origin_of);
            if !origin.is_some_and(|o| self.origins.contains(&o)) {
                return Err(Denial::UntrustedOrigin);
            }
        }
        Ok(())
    }
}

/// The central caller policy of the host services. See the module documentation.
#[derive(Debug, Clone, Default)]
pub struct HostCallerPolicy {
    services: BTreeMap<ServiceId, ServiceTrust>,
    shell: Option<ServiceTrust>,
}

impl HostCallerPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `trust` for `service`, replacing an earlier one.
    pub fn with_service(mut self, service: ServiceId, trust: ServiceTrust) -> Self {
        self.services.insert(service, trust);
        self
    }

    /// Also guard the shell's own privileged `rk_shell_*` commands for hosts that run untrusted
    /// webviews next to the app's own: a guarded command is allowed only for these windows. Off by
    /// default, so existing apps are unchanged. The command name is the action.
    pub fn guard_shell_commands(mut self, trust: ServiceTrust) -> Self {
        self.shell = Some(trust);
        self
    }

    /// The trust registered for `service`.
    pub fn trust_of(&self, service: ServiceId) -> Option<&ServiceTrust> {
        self.services.get(&service)
    }

    /// The trust that guards shell commands, when enabled.
    pub fn shell_trust(&self) -> Option<&ServiceTrust> {
        self.shell.as_ref()
    }

    /// Every registered service id.
    pub fn services(&self) -> impl Iterator<Item = ServiceId> + '_ {
        self.services.keys().copied()
    }

    /// Check every trust is well formed.
    pub fn validate(&self) -> Result<(), String> {
        for (id, trust) in &self.services {
            trust.validate().map_err(|e| format!("service {id}: {e}"))?;
        }
        if let Some(trust) = &self.shell {
            trust.validate().map_err(|e| format!("shell guard: {e}"))?;
        }
        Ok(())
    }

    /// Who may call `service` at all: the shell-wide gate plus the service's labels and origins.
    /// Ignores the action. Used where a second service's caller must also be trusted (a secret is
    /// written only from the trusted settings caller).
    pub fn decide_caller(
        &self,
        service: ServiceId,
        facts: &CallerFacts<'_>,
    ) -> Result<(), CallerError> {
        caller_gate::decide(facts)?;
        let trust = self
            .services
            .get(&service)
            .ok_or(CallerError::Forbidden(Denial::ServiceNotRegistered))?;
        trust.trusts_caller(facts).map_err(CallerError::Forbidden)
    }

    /// May this caller run `action` of `service`.
    pub fn decide(
        &self,
        service: ServiceId,
        action: &str,
        facts: &CallerFacts<'_>,
    ) -> Result<(), CallerError> {
        self.decide_caller(service, facts)?;
        let enabled = self
            .services
            .get(&service)
            .is_some_and(|trust| trust.enables(action));
        if enabled {
            Ok(())
        } else {
            Err(CallerError::Forbidden(Denial::ActionNotEnabled))
        }
    }

    /// May this caller run the shell command `command`. Always allowed while the guard is off, and
    /// for the commands in [`SHELL_GUARD_EXEMPT`] whoever calls.
    pub fn decide_shell(&self, command: &str, facts: &CallerFacts<'_>) -> Result<(), CallerError> {
        let Some(trust) = &self.shell else {
            return Ok(());
        };
        if is_shell_guard_exempt(command) {
            return Ok(());
        }
        caller_gate::decide(facts)?;
        trust.trusts_caller(facts).map_err(CallerError::Forbidden)?;
        if trust.enables(command) {
            Ok(())
        } else {
            Err(CallerError::Forbidden(Denial::ActionNotEnabled))
        }
    }
}
