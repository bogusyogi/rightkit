//! Boot probation and the allowlisted fetch bridge of `rightkit-app-update` as host commands
//! (feature `app-update`, #448).
//!
//! * Boot probation: after an update installs the app starts on probation. [`AppUpdateHostService`]
//!   evaluates the sentinel once when the host services are installed (it knows the running
//!   version), and the renderer reports how far the boot got with `rightkit_boot_stage` and that
//!   it is healthy with `rightkit_boot_ok`. A version that starts more than the allowed number of
//!   times without `rightkit_boot_ok` is declared failed; `rightkit_boot_status` then answers
//!   `reinstallRequired`, and the app tells the person to reinstall the release they came from.
//!   Nothing is backed up or restored. `rightkit_boot_begin` records an install right before the
//!   relaunch.
//! * Fetch: `rightkit_app_update_fetch` runs a webview `fetch` natively through the app's
//!   [`FetchPolicy`] (exact https hosts, optional path prefixes). The policy is native; the page
//!   sends only the request. Errors are the closed codes of `FetchError`.
//!
//! Every command is gated by the caller policy of [`ServiceId::APP_UPDATE`]: a content webview, a
//! foreign origin or an untrusted window is refused before the service is touched.

use std::sync::{Arc, Mutex};

use rightkit_app_update::{
    BootDecision, BootProbation, FetchBridge, FetchError, FetchPolicy, FetchRequest, FetchResponse,
    ProbationError,
};
use serde::Serialize;
use tauri::{Manager, Webview, Wry};

use super::{blocking, managed, HostError, ServiceId};
use crate::caller_gate;

/// What the boot evaluation found, in the renderer's shape (`kind` is the discriminant).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum BootStatus {
    /// Not evaluated: no running version was given, or the sentinel could not be read.
    Unknown,
    /// No update is on probation.
    None,
    /// This boot is on probation; call `rightkit_boot_ok` once the UI is healthy.
    #[serde(rename_all = "camelCase")]
    Probation {
        to_version: String,
        boot_attempts: u32,
    },
    /// The version never reported healthy within the allowed starts: show reinstall guidance.
    #[serde(rename_all = "camelCase")]
    ReinstallRequired {
        failed_version: String,
        from_version: String,
        boot_attempts: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        last_stage: Option<String>,
        guidance: &'static str,
    },
    /// An earlier sentinel was superseded or unreadable and was cleared.
    Stale,
}

impl From<BootDecision> for BootStatus {
    fn from(decision: BootDecision) -> Self {
        match decision {
            BootDecision::None => Self::None,
            BootDecision::Probation {
                to_version,
                boot_attempts,
            } => Self::Probation {
                to_version,
                boot_attempts,
            },
            BootDecision::ReinstallRequired(failure) => Self::ReinstallRequired {
                guidance: failure.guidance(),
                failed_version: failure.failed_version,
                from_version: failure.from_version,
                boot_attempts: failure.boot_attempts,
                last_stage: failure.last_stage,
            },
            BootDecision::Stale => Self::Stale,
        }
    }
}

/// The registered app-update service: the probation sentinel, the running version and, when the
/// app declared a fetch policy, the fetch bridge.
pub struct AppUpdateHostService {
    probation: BootProbation,
    fetch: Option<FetchBridge>,
    running_version: Mutex<Option<String>>,
    status: Mutex<BootStatus>,
}

impl AppUpdateHostService {
    /// Probation over `probation`'s sentinel file, with no fetch bridge.
    pub fn new(probation: BootProbation) -> Self {
        Self {
            probation,
            fetch: None,
            running_version: Mutex::new(None),
            status: Mutex::new(BootStatus::Unknown),
        }
    }

    /// Also serve `rightkit_app_update_fetch` under `policy`.
    pub fn with_fetch(mut self, policy: FetchPolicy) -> Self {
        self.fetch = Some(FetchBridge::new(policy));
        self
    }

    /// Evaluate the sentinel for the running `version`. Called once by
    /// [`super::HostServices::install`]; a failure leaves the status `unknown` and never stops
    /// the app from starting.
    pub(crate) fn evaluate(&self, version: &str) {
        *self
            .running_version
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(version.to_owned());
        let status = match self.probation.evaluate_boot(version) {
            Ok(decision) => decision.into(),
            Err(_) => BootStatus::Unknown,
        };
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = status;
    }

    pub fn status(&self) -> BootStatus {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Record how far this boot got.
    pub fn report_stage(&self, stage: &str) -> Result<(), HostError> {
        self.probation.report_stage(stage).map_err(probation_error)
    }

    /// The new version is healthy: clear the sentinel and the status.
    pub fn acknowledge_healthy(&self) -> Result<(), HostError> {
        self.probation
            .acknowledge_healthy()
            .map_err(probation_error)?;
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(*status, BootStatus::Probation { .. }) {
            *status = BootStatus::None;
        }
        Ok(())
    }

    /// Record an install that is about to take effect (right before the relaunch), from the
    /// running version to `to_version`.
    pub fn begin(&self, to_version: &str) -> Result<(), HostError> {
        let from = self
            .running_version
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| HostError::unsupported("the running version is not known yet"))?;
        self.probation
            .begin(&from, to_version, None)
            .map_err(probation_error)
    }
}

/// A probation failure as a host error with fixed text (the sentinel path stays native).
fn probation_error(error: ProbationError) -> HostError {
    match error {
        ProbationError::Invalid(message) => HostError::invalid(message),
        ProbationError::Io(_) | ProbationError::Write(_) => {
            HostError::other("the boot sentinel could not be read or written")
        }
    }
}

fn app_update_service(
    webview: &Webview<Wry>,
    action: &str,
) -> Result<Arc<AppUpdateHostService>, HostError> {
    let host = managed(webview.app_handle())?;
    let caller = caller_gate::gather(webview)?;
    host.service_for(ServiceId::APP_UPDATE, action, &caller.facts())
}

/// The boot evaluation of this start (`{ kind: "none" | "probation" | "reinstallRequired" | ... }`).
#[tauri::command]
pub fn rightkit_boot_status(webview: Webview<Wry>) -> Result<BootStatus, HostError> {
    Ok(app_update_service(&webview, "boot_status")?.status())
}

/// Report a boot stage (`{ stage }`: 1 to 64 letters, digits, `_`, `-`, `.`). Does nothing when no
/// update is on probation.
#[tauri::command]
pub async fn rightkit_boot_stage(webview: Webview<Wry>, stage: String) -> Result<(), HostError> {
    let service = app_update_service(&webview, "boot_stage")?;
    blocking(move || service.report_stage(&stage)).await
}

/// The new version is healthy: end its probation.
#[tauri::command]
pub async fn rightkit_boot_ok(webview: Webview<Wry>) -> Result<(), HostError> {
    let service = app_update_service(&webview, "boot_ok")?;
    blocking(move || service.acknowledge_healthy()).await
}

/// Put the install of `to_version` on probation (`{ toVersion }`); call it right before the
/// relaunch.
#[tauri::command]
pub async fn rightkit_boot_begin(
    webview: Webview<Wry>,
    to_version: String,
) -> Result<(), HostError> {
    let service = app_update_service(&webview, "boot_begin")?;
    blocking(move || service.begin(&to_version)).await
}

/// Run a webview `fetch` request under the app's native allowlist (`{ request }`). Blocking work
/// runs off the async runtime; the error is the closed `{ code, message }` of `FetchError`.
#[tauri::command]
pub async fn rightkit_app_update_fetch(
    webview: Webview<Wry>,
    request: FetchRequest,
) -> Result<FetchResponse, FetchCommandError> {
    let service = app_update_service(&webview, "fetch").map_err(FetchCommandError::Host)?;
    let outcome = tauri::async_runtime::spawn_blocking(move || match &service.fetch {
        Some(bridge) => bridge.fetch(request).map_err(FetchCommandError::Fetch),
        None => Err(FetchCommandError::Host(HostError::unsupported(
            "no fetch policy is configured",
        ))),
    })
    .await;
    match outcome {
        Ok(answer) => answer,
        Err(_) => Err(FetchCommandError::Host(HostError::other(
            "the fetch task failed",
        ))),
    }
}

/// The rejection of `rightkit_app_update_fetch`: a host refusal or a fetch failure, each already in
/// its own `{ code, message }` shape.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum FetchCommandError {
    Host(HostError),
    Fetch(FetchError),
}
