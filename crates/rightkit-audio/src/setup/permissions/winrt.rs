//! Windows microphone consent: WinRT `AppCapability` status/request, the capture-probe
//! classifier, and the post-Settings grant poll.
//!
//! All functions block. Hosts call them from a blocking worker, never from a UI thread.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::{JoinHandle, ThreadId};
use std::time::{Duration, Instant};

use windows::core::HSTRING;
use windows::Security::Authorization::AppCapabilityAccess::{
    AppCapability, AppCapabilityAccessStatus,
};

use super::PermissionHost;

/// Settings page that controls the Windows microphone privacy switch.
pub(super) const SETTINGS_URL: &str = "ms-settings:privacy-microphone";

// Cancellation lives in each request ticket, never in process-global state.
#[derive(Default)]
struct Ticket {
    shared: Weak<Shared>,
    cancelled: AtomicBool,
    sleep: Mutex<()>,
    wake: Condvar,
}

impl Ticket {
    // Admission linearizes with cancel/supersession under session state, then releases
    // that lock before calling OS code or host callbacks. close drains admitted jobs.
    fn admit(&self) -> Option<AdmittedJob> {
        let shared = self.shared.upgrade()?;
        {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.closed || self.check().is_err() {
                return None;
            }
            state.admitted += 1;
        }
        Some(AdmittedJob(shared))
    }

    fn cancel(&self) {
        let _sleep = self.sleep.lock().unwrap_or_else(|e| e.into_inner());
        self.cancelled.store(true, Ordering::Release);
        self.wake.notify_all();
    }

    fn check(&self) -> Result<(), String> {
        if self.cancelled.load(Ordering::Acquire) {
            Err("microphone permission session cancelled or superseded".into())
        } else {
            Ok(())
        }
    }

    fn wait(&self, duration: Duration) -> Result<(), String> {
        let guard = self.sleep.lock().unwrap_or_else(|e| e.into_inner());
        drop(
            self.wake
                .wait_timeout_while(guard, duration, |_| !self.cancelled.load(Ordering::Acquire)),
        );
        self.check()
    }
}

struct AdmittedJob(Arc<Shared>);

impl Drop for AdmittedJob {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.admitted -= 1;
        self.0.wake.notify_all();
    }
}

// A queued native operation is still owned if cancellation wins the receive race.
struct PendingOperation<T> {
    value: Option<T>,
    cleanup: fn(&T),
}

impl<T> Drop for PendingOperation<T> {
    fn drop(&mut self) {
        if let Some(value) = self.value.as_ref() {
            (self.cleanup)(value);
        }
    }
}

struct Watch {
    host: PermissionHost,
    ticket: Arc<Ticket>,
    deadline: Instant,
}

#[derive(Default)]
struct State {
    closed: bool,
    ticket: Option<Arc<Ticket>>,
    watch: Option<Watch>,
    worker: Option<JoinHandle<()>>,
    worker_id: Option<ThreadId>,
    joining: bool,
    admitted: usize,
    requests: Vec<ThreadId>,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

/// Owned Windows permission worker; runtime-resource metadata may name this item.
#[derive(Default)]
pub struct PermissionSession {
    shared: Arc<Shared>,
}

struct Request<'a> {
    session: &'a PermissionSession,
    ticket: Arc<Ticket>,
}

impl Drop for Request<'_> {
    fn drop(&mut self) {
        let mut state = self
            .session
            .shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(index) = state
            .requests
            .iter()
            .position(|id| *id == std::thread::current().id())
        {
            state.requests.swap_remove(index);
        }
        self.session.shared.wake.notify_all();
    }
}

impl PermissionSession {
    fn begin(&self) -> Result<Request<'_>, String> {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return Err("microphone permission session cancelled".into());
        }
        if let Some(ticket) = state.ticket.take() {
            ticket.cancel();
        }
        let stale_watch = state.watch.take();
        let ticket = Arc::new(Ticket {
            shared: Arc::downgrade(&self.shared),
            ..Ticket::default()
        });
        state.ticket = Some(ticket.clone());
        state.requests.push(std::thread::current().id());
        self.shared.wake.notify_all();
        drop(state);
        drop(stale_watch);
        Ok(Request {
            session: self,
            ticket,
        })
    }

    pub(super) fn cancel(&self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.closed = true;
        if let Some(ticket) = state.ticket.take() {
            ticket.cancel();
        }
        let stale_watch = state.watch.take();
        self.shared.wake.notify_all();
        drop(state);
        drop(stale_watch);
    }

    pub(super) fn close(&self) {
        self.cancel();
        let current = std::thread::current().id();
        let handle = {
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            // Reentrant callbacks finish on their existing stack. Waiting here would self-deadlock.
            if state.worker_id == Some(current) || state.requests.contains(&current) {
                return;
            }
            while state.joining {
                state = self
                    .shared
                    .wake
                    .wait(state)
                    .unwrap_or_else(|e| e.into_inner());
            }
            state.joining = true;
            state.worker.take()
        };
        if let Some(handle) = handle {
            let _ = handle.join();
        }
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        while !state.requests.is_empty() || state.admitted != 0 {
            state = self
                .shared
                .wake
                .wait(state)
                .unwrap_or_else(|e| e.into_inner());
        }
        state.joining = false;
        self.shared.wake.notify_all();
    }

    fn watch(&self, host: &PermissionHost, ticket: &Arc<Ticket>) -> Result<(), String> {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        ticket.check()?;
        if state.closed {
            return Err("microphone permission session cancelled".into());
        }
        if state.worker.is_none() {
            let shared = self.shared.clone();
            let handle = std::thread::Builder::new()
                .name("microphone-grant-poll".into())
                .spawn(move || watch_worker(shared))
                .map_err(|e| e.to_string())?;
            state.worker_id = Some(handle.thread().id());
            state.worker = Some(handle);
        }
        let stale_watch = state.watch.replace(Watch {
            host: host.clone(),
            ticket: ticket.clone(),
            deadline: Instant::now() + Duration::from_secs(60),
        });
        self.shared.wake.notify_all();
        drop(state);
        drop(stale_watch);
        Ok(())
    }
}

fn watch_worker(shared: Arc<Shared>) {
    loop {
        let watch = {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            while !state.closed && state.watch.is_none() {
                state = shared.wake.wait(state).unwrap_or_else(|e| e.into_inner());
            }
            if state.closed {
                return;
            }
            state.watch.take().expect("watch available")
        };
        poll_grant(watch);
    }
}

fn receive<T>(
    receiver: std::sync::mpsc::Receiver<T>,
    ticket: Option<&Ticket>,
) -> Result<T, String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(ticket) = ticket {
            ticket.check()?;
        }
        match receiver.try_recv() {
            Ok(value) => return Ok(value),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return Err("UI job disconnected".into())
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("UI job timed out".into());
        }
        let duration = remaining.min(Duration::from_millis(10));
        if let Some(ticket) = ticket {
            ticket.wait(duration)?;
        } else {
            std::thread::sleep(duration);
        }
    }
}

/// Read package-scoped microphone state without probing hardware first.
pub(super) fn capability_status(
    host: &PermissionHost,
) -> Result<AppCapabilityAccessStatus, String> {
    capability_status_owned(host, None)
}

fn capability_status_owned(
    host: &PermissionHost,
    ticket: Option<&Arc<Ticket>>,
) -> Result<AppCapabilityAccessStatus, String> {
    let (sender, receiver) = sync_channel(1);
    let queued_ticket = ticket.cloned();
    let dispatch_deadline = Instant::now() + Duration::from_secs(5);
    (host.run_on_main_thread)(Box::new(move || {
        if Instant::now() >= dispatch_deadline {
            return;
        }
        let _admission = if let Some(ticket) = queued_ticket.as_ref() {
            let Some(admission) = ticket.admit() else {
                return;
            };
            Some(admission)
        } else {
            None
        };
        let result = AppCapability::Create(&HSTRING::from("microphone"))
            .and_then(|capability| capability.CheckAccess())
            .map_err(|error| error.to_string());
        let _ = sender.send(result);
    }))
    .map_err(|error| format!("schedule Windows microphone capability check: {error}"))?;
    receive(receiver, ticket.map(Arc::as_ref))
        .map_err(|error| format!("receive Windows microphone capability check: {error}"))?
}

/// Keep consent operation on its blocking caller, with cancellable UI & native waits.
fn request_capability(
    host: &PermissionHost,
    ticket: &Arc<Ticket>,
) -> Result<AppCapabilityAccessStatus, String> {
    ticket.check()?;
    let (sender, receiver) = sync_channel(1);
    let queued_ticket = ticket.clone();
    let dispatch_deadline = Instant::now() + Duration::from_secs(5);
    (host.run_on_main_thread)(Box::new(move || {
        if Instant::now() >= dispatch_deadline {
            return;
        }
        let Some(_admission) = queued_ticket.admit() else {
            return;
        };
        let result = AppCapability::Create(&HSTRING::from("microphone"))
            .and_then(|capability| capability.RequestAccessAsync())
            .map_err(|error| error.to_string());
        let result = result.map(|operation| PendingOperation {
            value: Some(operation),
            cleanup: |operation| {
                let _ = operation.Cancel();
                let _ = operation.Close();
            },
        });
        // PendingOperation cancels even if the receiving caller disappeared concurrently.
        if queued_ticket.check().is_ok() && Instant::now() < dispatch_deadline {
            let _ = sender.send(result);
        }
    }))
    .map_err(|error| format!("schedule Windows microphone capability request: {error}"))?;
    let mut pending = receive(receiver, Some(ticket.as_ref()))
        .map_err(|error| format!("receive Windows microphone capability request: {error}"))??;
    let operation = pending.value.take().expect("owned capability operation");
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Err(error) = ticket.check() {
            let _ = operation.Cancel();
            let _ = operation.Close();
            return Err(error);
        }
        match operation.Status() {
            Ok(status) if status.0 == 0 => {}
            Ok(_) => {
                let result = operation
                    .GetResults()
                    .map_err(|error| format!("request Windows microphone capability: {error}"));
                let _ = operation.Close();
                return result;
            }
            Err(error) => {
                let _ = operation.Cancel();
                let _ = operation.Close();
                return Err(format!("request Windows microphone capability: {error}"));
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let _ = operation.Cancel();
            let _ = operation.Close();
            return Ok(AppCapabilityAccessStatus::UserPromptRequired);
        }
        // Cancellation is checked at loop entry so every exit releases the operation.
        let _ = ticket.wait(remaining.min(Duration::from_millis(50)));
    }
}

/// Fine-grained outcome of an actual capture attempt, used to refine a non-denied WinRT result
/// into one of the distinct microphone states. Never consulted when WinRT already returned an
/// explicit denial; see `state_from_capability`'s precedence rule.
enum CaptureProbe {
    /// A short capture session opened and closed cleanly.
    Available,
    /// `resolve_device` found no usable input device (no default device, requested platform id
    /// vanished, or index out of range): a hardware/config gap, never a permission denial, and
    /// labeled as such to the user.
    DeviceUnavailable,
    /// The device resolved but capture still failed (stream config, backend or driver error):
    /// distinct from "no device" and from any permission state.
    Failed,
}

fn capture_probe() -> CaptureProbe {
    match crate::capture::CaptureSession::start(crate::capture::SessionConfig {
        device_id: None,
        device_platform_id: None,
        target_rate: 16_000,
        target_channels: 1,
        target_dtype: crate::capture::OutDType::Float32,
        block_ms: 20,
    }) {
        Ok(mut session) => {
            let _ = session.stop();
            CaptureProbe::Available
        }
        Err(error) => {
            // `capture::device::resolve_device` phrases every "no device" case with one of these
            // fragments; anything else is a capture-path failure with a device present.
            let message = error.to_string();
            if message.contains("no default input device available")
                || message.contains("is unavailable")
                || message.contains("out of range")
            {
                CaptureProbe::DeviceUnavailable
            } else {
                CaptureProbe::Failed
            }
        }
    }
}

fn device_probe() -> CaptureProbe {
    match crate::capture::list_input_devices() {
        Ok(devices) if devices.is_empty() => CaptureProbe::DeviceUnavailable,
        Ok(_) => CaptureProbe::Available,
        Err(_) => CaptureProbe::Failed,
    }
}

/// Run a probe on its own thread so a panic inside it reads as `Failed` instead of unwinding
/// into the caller.
fn run_probe(probe: fn() -> CaptureProbe) -> CaptureProbe {
    std::thread::Builder::new()
        .name("microphone-probe".into())
        .spawn(probe)
        .ok()
        .and_then(|handle| handle.join().ok())
        .unwrap_or(CaptureProbe::Failed)
}

/// Turn a WinRT `AppCapability` result into one of the eight Windows microphone states an app
/// renders distinctly: `authorized`, `not_determined`, `denied_by_user`, `denied_by_system`,
/// `identity_unavailable`, `device_unavailable`, `capture_failed`, `unknown`.
///
/// PRECEDENCE RULE: `DeniedByUser`/`DeniedBySystem` from WinRT is authority and is returned
/// immediately. `Allowed` establishes permission, not device availability, so it is refined by
/// non-invasive endpoint enumeration (never opening a second capture stream). The actual capture
/// probe is reserved for `NotDeclaredByApp` (unpackaged/dev) & WinRT errors. It can never
/// manufacture or override an explicit denial.
pub(super) fn state_from_capability(
    capability: Result<AppCapabilityAccessStatus, String>,
) -> &'static str {
    // WinRT itself failed to answer (sparse identity not registered/settled, or the call
    // errored) vs. it answered "no capability declared": both are inconclusive, but only the
    // former is an identity problem.
    let identity_unavailable = capability.is_err();
    match capability {
        Ok(AppCapabilityAccessStatus::DeniedByUser) => "denied_by_user",
        Ok(AppCapabilityAccessStatus::DeniedBySystem) => "denied_by_system",
        Ok(AppCapabilityAccessStatus::UserPromptRequired) => "not_determined",
        Ok(AppCapabilityAccessStatus::Allowed) => match run_probe(device_probe) {
            CaptureProbe::Available => "authorized",
            CaptureProbe::DeviceUnavailable => "device_unavailable",
            CaptureProbe::Failed => "capture_failed",
        },
        Ok(AppCapabilityAccessStatus::NotDeclaredByApp) | Err(_) => {
            match run_probe(capture_probe) {
                CaptureProbe::Available => "authorized",
                CaptureProbe::DeviceUnavailable => "device_unavailable",
                CaptureProbe::Failed => {
                    if identity_unavailable {
                        "identity_unavailable"
                    } else {
                        "capture_failed"
                    }
                }
            }
        }
        Ok(_) => "unknown",
    }
}

/// Current Windows microphone state, classified by the same authority every time.
pub(super) fn access_status(host: &PermissionHost) -> &'static str {
    state_from_capability(capability_status(host))
}

/// The Windows consent request: ask WinRT, classify, and, unless the state is final, open the
/// Settings page and watch for a grant for 60 s.
pub(super) fn request_access(
    session: &PermissionSession,
    host: &PermissionHost,
) -> Result<String, String> {
    let request = session.begin()?;
    let ticket = &request.ticket;
    let event_sink = |event: &str, detail: &str| {
        if let Some(_admission) = ticket.admit() {
            (host.event_sink)(event, detail);
        }
    };
    // Log each raw WinRT outcome before classifying, for support-bundle diagnostics. The
    // classified `status` below is what drives the actual return value and UI.
    let capability_result = request_capability(host, ticket);
    ticket.check()?;
    match &capability_result {
        Ok(AppCapabilityAccessStatus::Allowed) => {
            event_sink("microphone_permission_result", "windows_capability_allowed")
        }
        Ok(AppCapabilityAccessStatus::DeniedByUser) => event_sink(
            "microphone_permission_result",
            "windows_capability_denied_by_user",
        ),
        Ok(AppCapabilityAccessStatus::DeniedBySystem) => event_sink(
            "microphone_permission_result",
            "windows_capability_denied_by_system",
        ),
        Ok(AppCapabilityAccessStatus::UserPromptRequired) => event_sink(
            "microphone_permission_result",
            "windows_capability_prompt_pending",
        ),
        Ok(AppCapabilityAccessStatus::NotDeclaredByApp) => {
            // Development/unpackaged launches have no package capability. Preserve their
            // hardware-probe path without masking an explicit package-scoped user or system
            // denial above.
            event_sink(
                "microphone_permission_result",
                "windows_capability_not_declared_fallback",
            )
        }
        Ok(_) => event_sink(
            "microphone_permission_result",
            "windows_capability_unknown_fallback",
        ),
        Err(error) => {
            // WinRT may be unavailable before sparse registration settles. The capture probe
            // remains a compatibility fallback, never an override for DeniedByUser or
            // DeniedBySystem.
            event_sink(
                "microphone_permission_result",
                &format!("windows_capability_error_fallback={error}"),
            )
        }
    }

    // PRECEDENCE RULE: classify via the same authority as `access_status`: an explicit WinRT
    // denial always wins; the capture probe only ever refines an inconclusive capability result
    // and can never manufacture or override a denial.
    ticket.check()?;
    let status = state_from_capability(capability_result);
    ticket.check()?;
    match status {
        "authorized" => {
            event_sink("microphone_permission_result", "windows_capture_ready");
            ticket.check()?;
            return Ok("authorized".into());
        }
        "device_unavailable" => {
            // No usable input device: a hardware/config gap, not a permission problem. Opening
            // the privacy Settings pane would be a lie here (there is nothing there to fix), so
            // report it as-is and let the UI point at the device picker instead.
            event_sink("microphone_permission_result", "windows_device_unavailable");
            ticket.check()?;
            return Ok(status.into());
        }
        _ => {}
    }
    // Every remaining status (not_determined, denied_by_user, denied_by_system,
    // identity_unavailable, capture_failed, unknown) has the same honest next step: open the
    // Settings page that actually controls this. PRECEDENCE: this only opens Settings as a side
    // effect; the returned `status` is unchanged, so an explicit denial is still reported as a
    // denial, never silently upgraded.
    ticket.check()?;
    rightkit_child_process::open_url(SETTINGS_URL)
        .map_err(|e| format!("could not open Windows microphone settings: {e}"))?;
    ticket.check()?;
    if let Err(error) = session.watch(host, ticket) {
        ticket.check()?;
        event_sink(
            "microphone_permission_result",
            &format!("windows_capture_poll_spawn_failed={error}"),
        );
    }
    ticket.check()?;
    Ok(status.into())
}

fn poll_grant(watch: Watch) {
    let Watch {
        host,
        ticket,
        deadline,
    } = watch;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if ticket.wait(remaining.min(Duration::from_secs(2))).is_err() {
            return;
        }
        if Instant::now() >= deadline {
            break;
        }
        let capability = capability_status_owned(&host, Some(&ticket));
        if ticket.check().is_err() {
            return;
        }
        let status = state_from_capability(capability);
        if ticket.check().is_err() {
            return;
        }
        if Instant::now() >= deadline {
            break;
        }
        if status == "authorized" {
            let Some(_diagnostic) = ticket.admit() else {
                return;
            };
            (host.event_sink)(
                "microphone_permission_result",
                "windows_capture_granted_after_settings",
            );
            // Diagnostics may cancel/reenter; never invoke a grant after that cancellation.
            if let Some(_grant) = ticket.admit() {
                (host.on_granted)();
            }
            return;
        }
    }
    if let Some(_timeout) = ticket.admit() {
        (host.event_sink)(
            "microphone_permission_result",
            "windows_capture_poll_timeout",
        );
    }
}
