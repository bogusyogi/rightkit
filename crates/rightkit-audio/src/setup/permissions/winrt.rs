//! Windows microphone consent: WinRT `AppCapability` status/request, the capture-probe
//! classifier, and the post-Settings grant poll.
//!
//! All functions block. Hosts call them from a blocking worker, never from a UI thread.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::sync_channel;
use std::time::{Duration, Instant};

use windows::core::HSTRING;
use windows::Security::Authorization::AppCapabilityAccess::{
    AppCapability, AppCapabilityAccessStatus,
};

use super::PermissionHost;

/// Settings page that controls the Windows microphone privacy switch.
pub(super) const SETTINGS_URL: &str = "ms-settings:privacy-microphone";

/// Read package-scoped microphone state without probing hardware first.
///
/// Sparse identity is what gives an app its own Windows privacy row. WinRT capability calls must
/// originate on the host's UI thread (`PermissionHost::run_on_main_thread`); this call waits for
/// the answer off that thread, so the UI thread stays free.
pub(super) fn capability_status(
    host: &PermissionHost,
) -> Result<AppCapabilityAccessStatus, String> {
    let (sender, receiver) = sync_channel(1);
    (host.run_on_main_thread)(Box::new(move || {
        let result = AppCapability::Create(&HSTRING::from("microphone"))
            .and_then(|capability| capability.CheckAccess())
            .map_err(|error| error.to_string());
        let _ = sender.send(result);
    }))
    .map_err(|error| format!("schedule Windows microphone capability check: {error}"))?;

    receiver
        .recv_timeout(Duration::from_secs(5))
        .map_err(|error| format!("receive Windows microphone capability check: {error}"))?
}

/// Start Windows' package-scoped consent flow from its required UI thread.
///
/// The returned WinRT operation is kept alive until its result resolves off-thread. Falling back
/// to the capture probe remains intentional for unpackaged/dev launches and machines where sparse
/// identity registration is unavailable.
fn request_capability(host: &PermissionHost) -> Result<AppCapabilityAccessStatus, String> {
    let (sender, receiver) = sync_channel(1);
    (host.run_on_main_thread)(Box::new(move || {
        let result = AppCapability::Create(&HSTRING::from("microphone"))
            .and_then(|capability| capability.RequestAccessAsync())
            .map_err(|error| error.to_string());
        let _ = sender.send(result);
    }))
    .map_err(|error| format!("schedule Windows microphone capability request: {error}"))?;

    let operation = receiver
        .recv_timeout(Duration::from_secs(5))
        .map_err(|error| format!("receive Windows microphone capability request: {error}"))??;

    // Observe the operation on this blocking caller, rather than leaving an indefinitely
    // blocked join worker behind whenever an unanswered consent request times out.
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        match operation.Status() {
            // AsyncStatus::Started (0): the OS has not answered yet.
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
                return Err(format!("request Windows microphone capability: {error}"));
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let _ = operation.Cancel();
            return Ok(AppCapabilityAccessStatus::UserPromptRequired);
        }
        std::thread::sleep(remaining.min(Duration::from_millis(50)));
    }
}

/// Generation of the post-Settings grant poll. Each new request supersedes the previous poll, so
/// repeated clicks no longer stack 60 s watchers.
static GRANT_POLL_GENERATION: AtomicU64 = AtomicU64::new(0);

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
pub(super) fn request_access(host: &PermissionHost) -> Result<String, String> {
    // Every request invalidates its predecessor's watcher, including requests that now
    // return immediately because permission is already granted or hardware disappeared.
    let generation = GRANT_POLL_GENERATION
        .fetch_add(1, Ordering::SeqCst)
        .wrapping_add(1);
    let event_sink = host.event_sink;
    // Log each raw WinRT outcome before classifying, for support-bundle diagnostics. The
    // classified `status` below is what drives the actual return value and UI.
    let capability_result = request_capability(host);
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
    let status = state_from_capability(capability_result);
    match status {
        "authorized" => {
            event_sink("microphone_permission_result", "windows_capture_ready");
            return Ok("authorized".into());
        }
        "device_unavailable" => {
            // No usable input device: a hardware/config gap, not a permission problem. Opening
            // the privacy Settings pane would be a lie here (there is nothing there to fix), so
            // report it as-is and let the UI point at the device picker instead.
            event_sink("microphone_permission_result", "windows_device_unavailable");
            return Ok(status.into());
        }
        _ => {}
    }
    // Every remaining status (not_determined, denied_by_user, denied_by_system,
    // identity_unavailable, capture_failed, unknown) has the same honest next step: open the
    // Settings page that actually controls this. PRECEDENCE: this only opens Settings as a side
    // effect; the returned `status` is unchanged, so an explicit denial is still reported as a
    // denial, never silently upgraded.
    rightkit_child_process::open_url(SETTINGS_URL)
        .map_err(|e| format!("could not open Windows microphone settings: {e}"))?;
    // Windows has no completion callback like AVCaptureDevice's requestAccessForMediaType.
    // Re-run the SAME package-capability + capture classifier in the background; a successful
    // hardware probe alone must never overwrite a still-active WinRT denial. Give up after
    // 60 s; the host's focus/poll recheck remains the fallback.
    let poll_host = host.clone();
    let spawned = std::thread::Builder::new()
        .name("microphone-grant-poll".into())
        .spawn(move || {
            let event_sink = poll_host.event_sink;
            let deadline = Instant::now() + Duration::from_secs(60);
            while Instant::now() < deadline {
                std::thread::sleep(Duration::from_secs(2));
                if GRANT_POLL_GENERATION.load(Ordering::SeqCst) != generation {
                    return;
                }
                if Instant::now() >= deadline {
                    break;
                }
                let status = access_status(&poll_host);
                if GRANT_POLL_GENERATION.load(Ordering::SeqCst) != generation {
                    return;
                }
                if Instant::now() >= deadline {
                    break;
                }
                if status == "authorized" {
                    event_sink(
                        "microphone_permission_result",
                        "windows_capture_granted_after_settings",
                    );
                    (poll_host.on_granted)();
                    return;
                }
            }
            event_sink(
                "microphone_permission_result",
                "windows_capture_poll_timeout",
            );
        });
    if let Err(error) = spawned {
        event_sink(
            "microphone_permission_result",
            &format!("windows_capture_poll_spawn_failed={error}"),
        );
    }
    Ok(status.into())
}
