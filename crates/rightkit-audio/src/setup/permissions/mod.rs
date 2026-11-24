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
//! Version 0.4 migration: Windows callers retain a [`MicrophonePermissionSession`] before
//! scheduling blocking work, call [`MicrophonePermissionSession::request`], then cancel on
//! teardown & move final ownership off UI for `close`/Drop. Windows no longer exposes the
//! legacy free request: returning its status cannot also retain an unowned grant watcher.
//! Non-Windows callers may keep using `request_microphone_access(&host)` or use the session API.
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

/// Owns Windows consent work & at most one grant-watch worker.
/// After its first watch, that worker sleeps on a condition variable between requests,
/// retaining one thread/stack until cancellation or final teardown; idle has no poll timer.
///
/// Create & retain this value before scheduling a blocking request. `cancel` is terminal:
/// no subsequent request is accepted. A new request supersedes previous session work.
/// `close` & final Drop belong on a blocking thread, never the UI thread. They cancel &
/// drain owned work; already-running OS/device calls can delay completion. On Windows,
/// callbacks must return without waiting for UI work or another thread closing this session.
/// A callback closing/dropping its own worker signals cancellation; that worker finishes
/// naturally after its callback returns, without trying to join itself.
///
/// macOS cancellation releases the app wait; its OS-owned prompt may complete later.
#[derive(Default)]
pub struct MicrophonePermissionSession {
    #[cfg(target_os = "windows")]
    windows: winrt::PermissionSession,
    #[cfg(not(target_os = "windows"))]
    cancelled: std::sync::atomic::AtomicBool,
}

impl MicrophonePermissionSession {
    pub fn new() -> Self {
        Self::default()
    }

    /// Request native consent on a blocking thread, retaining this session for deferred grants.
    ///
    /// Windows returns its classified status before the 60 s grant watch completes.
    /// A superseding request or cancellation interrupts pending Windows waits with an error.
    /// macOS keeps its existing prompt/result flow; other platforms return `"authorized"`.
    pub fn request(&self, host: &PermissionHost) -> Result<String, String> {
        #[cfg(target_os = "windows")]
        {
            winrt::request_access(&self.windows, host)
        }
        #[cfg(not(target_os = "windows"))]
        {
            if self.cancelled.load(std::sync::atomic::Ordering::Acquire) {
                return Err("microphone permission session cancelled".into());
            }
            #[cfg(target_os = "macos")]
            {
                apple::request_access_cancellable(host.event_sink, || {
                    self.cancelled.load(std::sync::atomic::Ordering::Acquire)
                })
            }
            #[cfg(not(target_os = "macos"))]
            {
                request_microphone_access(host)
            }
        }
    }

    /// Invalidate pending Windows jobs & wake waits without waiting for completion.
    /// Callbacks already admitted before cancellation may finish; `close` drains workers.
    pub fn cancel(&self) {
        #[cfg(target_os = "windows")]
        self.windows.cancel();
        #[cfg(not(target_os = "windows"))]
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// Cancel & drain Windows work from a blocking thread; never call on the UI thread.
    pub fn close(&self) {
        #[cfg(target_os = "windows")]
        self.windows.close();
        #[cfg(not(target_os = "windows"))]
        self.cancel();
    }
}

impl Drop for MicrophonePermissionSession {
    fn drop(&mut self) {
        self.close();
    }
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

/// Request microphone consent through the existing non-Windows flow.
///
/// macOS shows its native prompt & returns its answer. Other non-Windows platforms return
/// `"authorized"`. Windows 0.4 callers use [`MicrophonePermissionSession::request`] so deferred
/// grant work always has a caller-held owner.
#[cfg(not(target_os = "windows"))]
pub fn request_microphone_access(
    #[allow(unused_variables)] host: &PermissionHost,
) -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        apple::request_access(host.event_sink)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok("authorized".into())
    }
}
