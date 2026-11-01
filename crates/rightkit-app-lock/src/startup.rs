//! Start-up decisions: which runtime state follows the stored preference and the verification.
use crate::preference::LockPreference;
use crate::state::AppLockRuntime;

/// The runtime state after start-up verification. `lock_enabled` is the (fail-closed) preference,
/// `verify_result` the outcome of the OS prompt.
pub fn startup_runtime(lock_enabled: bool, verify_result: Result<bool, String>) -> AppLockRuntime {
    if !lock_enabled {
        return AppLockRuntime::disabled();
    }

    match verify_result {
        Ok(true) => AppLockRuntime::unlocked(),
        Ok(false) => AppLockRuntime::locked(Some("Verification was canceled.".into())),
        Err(err) => AppLockRuntime::unavailable(true, err),
    }
}

/// The startup verification outcome for a stored preference. A disabled lock needs none; an
/// unreadable preference is an error, never a pass, and `verify` is not called (the app stays
/// locked and shows the lock as unavailable).
pub fn startup_verification(
    preference: &LockPreference,
    verify: impl FnOnce() -> Result<bool, String>,
) -> Result<bool, String> {
    match preference {
        LockPreference::Disabled => Ok(true),
        LockPreference::Enabled => verify(),
        LockPreference::Unreadable(error) => Err(format!(
            "The app-lock setting could not be read, so the app stays locked: {error}"
        )),
    }
}
