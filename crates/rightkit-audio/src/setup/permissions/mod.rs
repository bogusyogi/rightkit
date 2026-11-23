//! Microphone consent for a setup flow: the status query and the request, per OS.
//!
//! * macOS: `AVCaptureDevice` authorization status and the native consent prompt.
//! * Windows: the package-scoped WinRT `AppCapability` answer, refined by a non-invasive device
//!   probe (or, for unpackaged/dev launches and WinRT errors, a short capture probe). An explicit
//!   WinRT denial always wins and is never overridden by a probe.
//! * Other platforms: `"unknown"` for the status, `"authorized"` for the request.
//!
//! Every function blocks (the macOS prompt waits up to 120 s, the Windows request up to about
//! 125 s), so hosts call them from a blocking worker. Status strings are a stable contract with
//! the host's UI: macOS `authorized | denied | restricted | not_determined | unknown`; Windows
//! `authorized | not_determined | denied_by_user | denied_by_system | identity_unavailable |
//! device_unavailable | capture_failed | unknown`; a request may also return `pending` (macOS, no
//! answer in time).
//!
//! Host inputs are in [`PermissionHost`]. Diagnostics go to its `event_sink` with the event names
//! `microphone_permission_status`, `microphone_permission_prompt` and
//! `microphone_permission_result`.
//!
//! Requires the host app to declare `NSMicrophoneUsageDescription` and the
//! `com.apple.security.device.audio-input` entitlement on macOS, and (for the package-scoped
//! Windows row) a sparse-package identity that declares the `microphone` capability.

use std::sync::Arc;

#[cfg(target_os = "macos")]
mod apple;
#[cfg(target_os = "windows")]
mod winrt;

/// A unit of work the host must run on its UI thread.
pub type MainThreadJob = Box<dyn FnOnce() + Send + 'static>;

/// What the host provides.
#[derive(Clone)]
pub struct PermissionHost {
    /// Diagnostic sink, `(event, detail)`.
    pub event_sink: fn(&str, &str),
    /// Run a job on the UI thread (WinRT capability calls must start there). Return the
    /// scheduling error text if the job could not be queued. Unused off Windows.
    pub run_on_main_thread: Arc<dyn Fn(MainThreadJob) -> Result<(), String> + Send + Sync>,
    /// Called from the background poll when a grant arrives after the user was sent to Windows
    /// Settings (the host emits its "permission granted" event here). Unused off Windows.
    pub on_granted: Arc<dyn Fn() + Send + Sync>,
}

/// Current microphone permission state. See the module docs for the status strings.
pub fn microphone_access_status(
    #[allow(unused_variables)] host: &PermissionHost,
) -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        let status = apple::authorization_status()?;
        (host.event_sink)("microphone_permission_status", &format!("macos={status}"));
        Ok(apple::status_label(status).into())
    }
    #[cfg(target_os = "windows")]
    {
        let status = winrt::access_status(host);
        (host.event_sink)("microphone_permission_status", &format!("windows={status}"));
        Ok(status.into())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Ok("unknown".into())
    }
}

/// Open the macOS Privacy > Microphone Settings pane. A no-op on other platforms.
pub fn open_macos_microphone_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        rightkit_child_process::open_url(
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone",
        )
        .map_err(|e| format!("could not open Microphone settings: {e}"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(())
    }
}

/// Request microphone consent.
///
/// macOS shows the native prompt and returns the answer. Windows has no completion callback: it
/// asks WinRT, classifies, opens the Settings page for any non-final state and polls for a grant
/// for 60 s (`PermissionHost::on_granted`), returning the classified status. Other platforms
/// return `"authorized"`.
pub fn request_microphone_access(
    #[allow(unused_variables)] host: &PermissionHost,
) -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        apple::request_access(host.event_sink)
    }
    #[cfg(target_os = "windows")]
    {
        winrt::request_access(host)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Ok("authorized".into())
    }
}
