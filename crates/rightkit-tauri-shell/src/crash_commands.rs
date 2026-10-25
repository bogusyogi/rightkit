//! The renderer crash sink over `rightkit-crash`, without host services (feature `crash`).
//!
//! `@rightkit/logs` `installWindowCrashCapture` sends each webview error to the command
//! `rightkit_crash_window_error`. Six apps hand-wrote it because the shell's own sink needed the whole
//! `host-services` stack. This module registers the same command once for an app that only wants the
//! sink: the app configures the native recording once with [`install`], and [`crate::handler!`] lists
//! the command whenever feature `crash` is on.
//!
//! ```ignore
//! let config = CrashConfig::new("MyApp", env!("CARGO_PKG_VERSION"), app_data.join("crashes"));
//! crash_commands::install(app.handle(), config, WindowErrorGate::default());   // in the app's setup
//! // invoke_handler: rightkit_tauri_shell::handler![...]                      // lists the sink itself
//! ```
//!
//! # Who may report, and where the report goes
//!
//! [`crash_route`] picks one of three answers for each call:
//!
//! * [`CrashRoute::Host`]: host services are installed and a `crate::host::CrashHostService` is
//!   registered. The central caller policy decides who may report and the owner's service redacts and
//!   rate-limits, exactly as before this module existed. It wins over a sink.
//! * [`CrashRoute::Sink`]: a [`CrashSink`] is managed ([`install`]). The caller must be an app webview
//!   ([`crate::caller_gate::require_app_caller`]); the sink then records on a blocking thread.
//! * [`CrashRoute::Unsupported`]: neither. The command answers `unsupported`, never a silent success.
//!
//! The renderer is untrusted: `rightkit-crash` bounds, scrubs and rate-limits every report
//! ([`WindowErrorGate`]), and nothing is returned to the page.

use rightkit_crash::{record_window_error, CrashConfig, WindowErrorGate, WindowErrorReport};
use tauri::{AppHandle, Manager, Runtime, Webview, Wry};

use crate::error::ShellError;

/// The native recording of renderer errors: the crash configuration (where reports go and what is
/// redacted) and the rate limit shared by every report of the process.
pub struct CrashSink {
    config: CrashConfig,
    gate: WindowErrorGate,
}

impl CrashSink {
    pub fn new(config: CrashConfig, gate: WindowErrorGate) -> Self {
        Self { config, gate }
    }

    /// Replace the rate limit.
    pub fn with_gate(mut self, gate: WindowErrorGate) -> Self {
        self.gate = gate;
        self
    }

    /// Record one renderer error as a window crash report, unless the gate refuses it. No path or
    /// recording state is returned to the renderer.
    pub fn record(&self, report: WindowErrorReport) {
        let _ = record_window_error(&self.config, &self.gate, &report);
    }
}

/// Manage a [`CrashSink`] for the command. `false` when one was already installed (the first one
/// stays).
pub fn install<R: Runtime>(app: &AppHandle<R>, config: CrashConfig, gate: WindowErrorGate) -> bool {
    app.manage(CrashSink::new(config, gate))
}

/// Which owner takes a renderer error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashRoute {
    /// The host crash service, behind the central caller policy.
    Host,
    /// The managed [`CrashSink`], behind the app caller gate.
    Sink,
    /// Nothing is registered.
    Unsupported,
}

/// The route for what is registered. The host service wins, so an app that registered one keeps the
/// owner's caller policy even when it also installed a sink.
pub fn crash_route(host_registered: bool, sink_registered: bool) -> CrashRoute {
    if host_registered {
        CrashRoute::Host
    } else if sink_registered {
        CrashRoute::Sink
    } else {
        CrashRoute::Unsupported
    }
}

/// The route for the state `app` manages. A host route needs host services installed with a
/// `crate::host::CrashHostService` registered (feature `host-services`); without that feature it is
/// never taken.
pub fn route_for<R: Runtime>(app: &AppHandle<R>) -> CrashRoute {
    crash_route(host_registered(app), app.try_state::<CrashSink>().is_some())
}

#[cfg(feature = "host-services")]
fn host_registered<R: Runtime>(app: &AppHandle<R>) -> bool {
    use crate::host::{CrashHostService, HostServices, ServiceId};
    app.try_state::<HostServices>()
        .is_some_and(|host| host.service::<CrashHostService>(ServiceId::CRASH).is_some())
}

#[cfg(not(feature = "host-services"))]
fn host_registered<R: Runtime>(_app: &AppHandle<R>) -> bool {
    false
}

/// What the caller check chose: the owner that records the report.
enum Target {
    #[cfg(feature = "host-services")]
    Host(std::sync::Arc<crate::host::CrashHostService>),
    Sink,
}

/// Check the caller for the route `route_for` picks, and only then hand back the recorder.
fn crash_service(webview: &Webview<Wry>, action: &str) -> Result<Target, ShellError> {
    match route_for(webview.app_handle()) {
        #[cfg(feature = "host-services")]
        CrashRoute::Host => crate::host::crash::crash_service(webview, action)
            .map(Target::Host)
            .map_err(from_host),
        #[cfg(not(feature = "host-services"))]
        CrashRoute::Host => {
            let _ = action;
            Err(not_configured())
        }
        CrashRoute::Sink => {
            crate::caller_gate::require_app_caller(webview)?;
            Ok(Target::Sink)
        }
        CrashRoute::Unsupported => Err(not_configured()),
    }
}

fn not_configured() -> ShellError {
    ShellError::Unsupported("renderer crash capture is not configured".into())
}

/// A host refusal in the shell's error type. The wire shape is the same: `{ code, message }`.
#[cfg(feature = "host-services")]
fn from_host(error: crate::host::HostError) -> ShellError {
    match error.code {
        "permission_denied" => ShellError::PermissionDenied(error.message),
        "unsupported" => ShellError::Unsupported(error.message),
        "invalid" => ShellError::Invalid(error.message),
        _ => ShellError::Other(error.message),
    }
}

/// Record one renderer error (`{ report }`). Nothing is returned to the page.
#[tauri::command]
pub async fn rightkit_crash_window_error(
    webview: Webview<Wry>,
    app: AppHandle<Wry>,
    report: WindowErrorReport,
) -> Result<(), ShellError> {
    let target = crash_service(&webview, "window_error")?;
    tauri::async_runtime::spawn_blocking(move || match target {
        #[cfg(feature = "host-services")]
        Target::Host(service) => service.record(report),
        Target::Sink => app.state::<CrashSink>().record(report),
    })
    .await
    .map_err(|e| ShellError::Other(e.to_string()))
}
