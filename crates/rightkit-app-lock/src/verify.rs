//! OS user verification: Windows Hello on Windows, Touch ID or the device password on macOS.
//! Other platforms report the lock as unavailable.

#[cfg(target_os = "windows")]
/// Whether the platform can verify the user right now; the error says why not.
pub fn native_available() -> Result<(), String> {
    use windows::Security::Credentials::UI::{
        UserConsentVerifier, UserConsentVerifierAvailability,
    };

    let availability = UserConsentVerifier::CheckAvailabilityAsync()
        .map_err(|err| format!("Windows Hello availability check failed: {err}"))?
        .join()
        .map_err(|err| format!("Windows Hello availability check failed: {err}"))?;

    match availability {
        UserConsentVerifierAvailability::Available => Ok(()),
        UserConsentVerifierAvailability::DeviceNotPresent => {
            Err("Windows Hello is not available on this device.".into())
        }
        UserConsentVerifierAvailability::NotConfiguredForUser => {
            Err("Windows Hello is not configured for this Windows user.".into())
        }
        UserConsentVerifierAvailability::DisabledByPolicy => {
            Err("Windows Hello is disabled by policy.".into())
        }
        UserConsentVerifierAvailability::DeviceBusy => {
            Err("Windows Hello is currently busy.".into())
        }
        _ => Err("Windows Hello is unavailable.".into()),
    }
}

#[cfg(target_os = "windows")]
/// Prompt the user to verify. `Ok(false)` means the user canceled.
pub fn verify_user(reason: &str) -> Result<bool, String> {
    verify_user_hwnd(None, reason)
}

#[cfg(target_os = "windows")]
fn verify_user_hwnd(hwnd: Option<isize>, reason: &str) -> Result<bool, String> {
    // The WinRT consent API needs a WinRT-initialised thread and is blocking;
    // run it on a dedicated thread we initialise ourselves (blocking-pool
    // threads are not initialised for WinRT).
    let reason = reason.to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("hello-verify".into())
        .spawn(move || {
            let _ = tx.send(winrt_verify(hwnd, &reason));
        })
        .map_err(|err| format!("Windows Hello thread failed: {err}"))?;
    // Bounded like the macOS path (60 s). On timeout the verify thread is left
    // to finish on its own (a WinRT prompt cannot be cancelled from here).
    match rx.recv_timeout(std::time::Duration::from_secs(60)) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            Err("Windows Hello verification timed out.".to_string())
        }
        // Sender dropped without a value: the verify thread panicked.
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            Err("Windows Hello thread panicked.".to_string())
        }
    }
}

#[cfg(target_os = "windows")]
fn winrt_verify(hwnd: Option<isize>, reason: &str) -> Result<bool, String> {
    use windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED};

    // S_FALSE / RPC_E_CHANGED_MODE are fine: only uninitialise what we set up.
    let initialised = unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.is_ok();
    let result = native_available().and_then(|()| request_verification(hwnd, reason));
    if initialised {
        unsafe { RoUninitialize() };
    }
    result
}

#[cfg(target_os = "windows")]
fn request_verification(hwnd: Option<isize>, reason: &str) -> Result<bool, String> {
    use windows::core::HSTRING;
    use windows::Security::Credentials::UI::{UserConsentVerificationResult, UserConsentVerifier};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::WinRT::IUserConsentVerifierInterop;

    let message = HSTRING::from(reason);
    let owned = hwnd.filter(|h| *h != 0).and_then(|h| {
        let interop =
            windows::core::factory::<UserConsentVerifier, IUserConsentVerifierInterop>().ok()?;
        // SAFETY: `h` is the live main-window HWND captured from Tauri; the
        // interop call only reads it. Failure falls back to the ownerless prompt.
        unsafe {
            interop.RequestVerificationForWindowAsync(HWND(h as *mut core::ffi::c_void), &message)
        }
        .ok()
    });
    let operation = match owned {
        Some(op) => op,
        None => UserConsentVerifier::RequestVerificationAsync(&message)
            .map_err(|err| format!("Windows Hello prompt failed: {err}"))?,
    };
    let result = operation
        .join()
        .map_err(|err| format!("Windows Hello prompt failed: {err}"))?;

    match result {
        UserConsentVerificationResult::Verified => Ok(true),
        UserConsentVerificationResult::Canceled => Ok(false),
        UserConsentVerificationResult::RetriesExhausted => {
            Err("Windows Hello retries exhausted.".into())
        }
        UserConsentVerificationResult::DeviceNotPresent => {
            Err("Windows Hello is not available on this device.".into())
        }
        UserConsentVerificationResult::NotConfiguredForUser => {
            Err("Windows Hello is not configured for this Windows user.".into())
        }
        UserConsentVerificationResult::DisabledByPolicy => {
            Err("Windows Hello is disabled by policy.".into())
        }
        UserConsentVerificationResult::DeviceBusy => Err("Windows Hello is currently busy.".into()),
        _ => Err("Windows Hello verification failed.".into()),
    }
}

#[cfg(target_os = "macos")]
/// Whether the platform can verify the user right now; the error says why not.
pub fn native_available() -> Result<(), String> {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};

    const DEVICE_OWNER_AUTHENTICATION: i64 = 1;

    unsafe {
        let context: Option<Retained<AnyObject>> = msg_send![class!(LAContext), new];
        let Some(context) = context else {
            return Err("Touch ID context could not be created.".into());
        };
        let ok: bool = msg_send![
            &*context,
            canEvaluatePolicy: DEVICE_OWNER_AUTHENTICATION,
            error: std::ptr::null_mut::<*mut AnyObject>()
        ];
        if ok {
            Ok(())
        } else {
            Err("Touch ID or device password is not available for this Mac user.".into())
        }
    }
}

#[cfg(target_os = "macos")]
/// Prompt the user to verify. `Ok(false)` means the user canceled.
pub fn verify_user(reason: &str) -> Result<bool, String> {
    use std::sync::mpsc;
    use std::time::Duration;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Bool};
    use objc2::{class, msg_send};
    use objc2_foundation::NSString;

    const DEVICE_OWNER_AUTHENTICATION: i64 = 1;

    native_available()?;

    unsafe {
        let context: Option<Retained<AnyObject>> = msg_send![class!(LAContext), new];
        let Some(context) = context else {
            return Err("Touch ID context could not be created.".into());
        };

        let reason = NSString::from_str(reason);
        let (tx, rx) = mpsc::channel();
        let block: RcBlock<dyn Fn(Bool, *mut AnyObject)> =
            RcBlock::new(move |success: Bool, _error: *mut AnyObject| {
                let _ = tx.send(success.as_bool());
            });

        let _: () = msg_send![
            &*context,
            evaluatePolicy: DEVICE_OWNER_AUTHENTICATION,
            localizedReason: &*reason,
            reply: &*block
        ];

        rx.recv_timeout(Duration::from_secs(60))
            .map_err(|_| "Touch ID verification timed out.".to_string())
    }
}

#[cfg(target_os = "macos")]
#[link(name = "LocalAuthentication", kind = "framework")]
extern "C" {}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
/// Whether the platform can verify the user right now; the error says why not.
pub fn native_available() -> Result<(), String> {
    Err("App lock is only available on Windows and macOS.".into())
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
/// Prompt the user to verify. `Ok(false)` means the user canceled.
pub fn verify_user(_reason: &str) -> Result<bool, String> {
    Err("App lock is only available on Windows and macOS.".into())
}

/// Verify with a window as the prompt owner so the prompt is parented to (and shown above) the
/// app. `hwnd` is the native window handle as an integer; pass `None` when no window exists yet
/// (start-up). Platforms without an owner-window concept (macOS shows its own sheet) ignore it.
#[cfg(target_os = "windows")]
pub fn verify_user_for_window(hwnd: Option<isize>, reason: &str) -> Result<bool, String> {
    verify_user_hwnd(hwnd, reason)
}

/// Verify with a window as the prompt owner; see the Windows variant. Here the handle is ignored.
#[cfg(not(target_os = "windows"))]
pub fn verify_user_for_window(_hwnd: Option<isize>, reason: &str) -> Result<bool, String> {
    verify_user(reason)
}
