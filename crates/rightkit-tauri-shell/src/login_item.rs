//! macOS 13+ launch at login through `SMAppService.mainApp` (ServiceManagement), the backend that
//! replaces the LaunchAgent plist. Orthic-Labs/rightkit#247.
//!
//! What it is. `SMAppService.mainApp` registers the running app bundle as a login item: it appears
//! under System Settings > General > Login Items with the app's own name and icon (a LaunchAgent plist
//! shows as an unnamed legacy background item). It needs a real signed `.app` bundle, and it cannot
//! carry launch arguments or `LowPriorityIO`; see [`crate::autostart::MacosBackend`] and
//! [`set_background_io`].
//!
//! Layout. The status enum, its raw mapping and the backend selection rule are pure and compile on
//! every host (so CI covers them). The `SMAppService` calls are macOS only and use the `objc2`
//! runtime already in the crate (`AnyClass::get` plus `msg_send!`), with the ServiceManagement
//! framework linked by `#[link]`. No new crate or Cargo feature is needed, so `Cargo.lock` does not change.
//! `SMAppService` exists only on macOS 13 and later; on an older macOS [`available`] is false and the
//! shell keeps writing the LaunchAgent.

use serde::{Deserialize, Serialize};

/// `SMAppService.Status`, typed. Raw values are Apple's `SMAppServiceStatus`
/// (`ServiceManagement/SMAppService.h`): `notRegistered` 0, `enabled` 1, `requiresApproval` 2,
/// `notFound` 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LoginItemStatus {
    /// Not registered (never registered, or unregistered).
    NotRegistered,
    /// Registered and allowed: the app launches at login.
    Enabled,
    /// Registered, but the user has not approved it (or switched it off) in System Settings > General
    /// > Login Items. The app should offer [`open_login_items_settings`].
    RequiresApproval,
    /// The system cannot find the service (for example the app is not running from a valid bundle).
    NotFound,
    /// A raw value Apple added after macOS 13; carried verbatim, never treated as enabled.
    Unknown(i64),
}

impl LoginItemStatus {
    /// True only for [`LoginItemStatus::Enabled`].
    pub fn is_enabled(self) -> bool {
        self == Self::Enabled
    }

    /// True when the user must act in System Settings before the item launches at login.
    pub fn needs_approval(self) -> bool {
        self == Self::RequiresApproval
    }
}

/// Map a raw `SMAppServiceStatus` to [`LoginItemStatus`].
pub fn login_item_status_from_raw(raw: i64) -> LoginItemStatus {
    match raw {
        0 => LoginItemStatus::NotRegistered,
        1 => LoginItemStatus::Enabled,
        2 => LoginItemStatus::RequiresApproval,
        3 => LoginItemStatus::NotFound,
        other => LoginItemStatus::Unknown(other),
    }
}

/// True when `exe` is inside a `.app` bundle (`...App.app/Contents/MacOS/<exe>`), the only place
/// `SMAppService.mainApp` can register from.
pub fn executable_in_app_bundle(exe: &std::path::Path) -> bool {
    exe.to_string_lossy().contains(".app/Contents/MacOS/")
}

/// Whether the shell uses `SMAppService` for this call: the host asked for it, the class exists
/// (macOS 13+), the executable is in a bundle, and the entry location is not redirected (an
/// `RIGHTKIT_SHELL_HOME` or `config_home` run is a sandbox or QA run, and `SMAppService` would
/// register the real user's login item). Pure.
pub fn service_management_selected(
    host_wants_it: bool,
    class_available: bool,
    in_bundle: bool,
    redirected: bool,
) -> bool {
    host_wants_it && class_available && in_bundle && !redirected
}

/// Decide a register call from its raw result and the status read after it. `SMAppService` reports a
/// `requiresApproval` registration (and an already-registered item) as an error on some macOS builds
/// although the item is registered, so a failure that leaves the item `Enabled` or `RequiresApproval`
/// is a success carrying that status. Pure.
pub fn register_result(
    call: Result<(), String>,
    status_after: LoginItemStatus,
) -> Result<LoginItemStatus, String> {
    match (call, status_after) {
        (Ok(()), s) => Ok(s),
        (Err(_), s @ (LoginItemStatus::Enabled | LoginItemStatus::RequiresApproval)) => Ok(s),
        (Err(e), _) => Err(e),
    }
}

/// Decide an unregister call. A failure that leaves the item `NotRegistered` or `NotFound` is the
/// wanted state. Pure.
pub fn unregister_result(
    call: Result<(), String>,
    status_after: LoginItemStatus,
) -> Result<LoginItemStatus, String> {
    match (call, status_after) {
        (Ok(()), s) => Ok(s),
        (Err(_), s @ (LoginItemStatus::NotRegistered | LoginItemStatus::NotFound)) => Ok(s),
        (Err(e), _) => Err(e),
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use objc2::rc::{autoreleasepool, Retained};
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use objc2::msg_send;
    use objc2_foundation::NSError;

    use super::{login_item_status_from_raw, LoginItemStatus};

    #[link(name = "ServiceManagement", kind = "framework")]
    extern "C" {}

    // libSystem `setiopolicy_np(int iotype, int scope, int policy)`.
    extern "C" {
        fn setiopolicy_np(iotype: i32, scope: i32, policy: i32) -> i32;
    }
    const IOPOL_TYPE_DISK: i32 = 0;
    const IOPOL_SCOPE_PROCESS: i32 = 0;
    const IOPOL_DEFAULT: i32 = 0;
    const IOPOL_THROTTLE: i32 = 3;

    fn class() -> Option<&'static AnyClass> {
        AnyClass::get(c"SMAppService")
    }

    pub fn available() -> bool {
        class().is_some()
    }

    fn main_app() -> Result<Retained<AnyObject>, String> {
        let cls = class().ok_or_else(|| "SMAppService needs macOS 13 or later".to_owned())?;
        // SAFETY: `+[SMAppService mainApp]` takes no arguments and returns an SMAppService (or nil).
        let svc: Option<Retained<AnyObject>> = unsafe { msg_send![cls, mainApp] };
        svc.ok_or_else(|| "SMAppService.mainApp returned nil".to_owned())
    }

    pub fn status() -> Result<LoginItemStatus, String> {
        let svc = main_app()?;
        // SAFETY: `-[SMAppService status]` returns an NSInteger.
        let raw: isize = unsafe { msg_send![&*svc, status] };
        Ok(login_item_status_from_raw(raw as i64))
    }

    fn call_with_error(selector: &str, svc: &AnyObject) -> Result<(), String> {
        let mut err: *mut NSError = std::ptr::null_mut();
        // SAFETY: both selectors are `-(BOOL)xAndReturnError:(NSError **)`, and `err` is a valid
        // out-pointer for the call.
        let ok: Bool = unsafe {
            match selector {
                "register" => msg_send![svc, registerAndReturnError: &mut err],
                _ => msg_send![svc, unregisterAndReturnError: &mut err],
            }
        };
        if ok.as_bool() {
            return Ok(());
        }
        autoreleasepool(|_| {
            // SAFETY: a failed call sets `err` to an autoreleased NSError (or leaves nil).
            Err(match unsafe { err.as_ref() } {
                Some(e) => format!("SMAppService {selector}: {}", e.localizedDescription()),
                None => format!("SMAppService {selector} failed"),
            })
        })
    }

    pub fn register() -> Result<Result<(), String>, String> {
        let svc = main_app()?;
        Ok(call_with_error("register", &svc))
    }

    pub fn unregister() -> Result<Result<(), String>, String> {
        let svc = main_app()?;
        Ok(call_with_error("unregister", &svc))
    }

    pub fn open_settings() -> Result<(), String> {
        let cls = class().ok_or_else(|| "SMAppService needs macOS 13 or later".to_owned())?;
        // SAFETY: `+[SMAppService openSystemSettingsLoginItems]` takes no arguments, returns void.
        unsafe {
            let _: () = msg_send![cls, openSystemSettingsLoginItems];
        }
        Ok(())
    }

    pub fn set_background_io(on: bool) -> Result<(), String> {
        let policy = if on { IOPOL_THROTTLE } else { IOPOL_DEFAULT };
        // SAFETY: plain libSystem call with constant arguments.
        let rc = unsafe { setiopolicy_np(IOPOL_TYPE_DISK, IOPOL_SCOPE_PROCESS, policy) };
        if rc == 0 {
            Ok(())
        } else {
            Err(format!(
                "setiopolicy_np: {}",
                std::io::Error::last_os_error()
            ))
        }
    }
}

/// True when `SMAppService` exists on this host (macOS 13+). Always false off macOS.
pub fn available() -> bool {
    #[cfg(target_os = "macos")]
    {
        mac::available()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Current `SMAppService.mainApp` status.
pub fn status() -> Result<LoginItemStatus, String> {
    #[cfg(target_os = "macos")]
    {
        mac::status()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("SMAppService is macOS only".into())
    }
}

/// `SMAppService.mainApp.register()`. Returns the status afterwards, which is
/// [`LoginItemStatus::RequiresApproval`] when the user still has to approve the item.
pub fn register() -> Result<LoginItemStatus, String> {
    #[cfg(target_os = "macos")]
    {
        let call = mac::register()?;
        register_result(call, mac::status()?)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("SMAppService is macOS only".into())
    }
}

/// `SMAppService.mainApp.unregister()`. Returns the status afterwards.
pub fn unregister() -> Result<LoginItemStatus, String> {
    #[cfg(target_os = "macos")]
    {
        let call = mac::unregister()?;
        unregister_result(call, mac::status()?)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("SMAppService is macOS only".into())
    }
}

/// `SMAppService.openSystemSettingsLoginItems()`: opens System Settings > General > Login Items, where
/// the user approves a [`LoginItemStatus::RequiresApproval`] item.
pub fn open_login_items_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        mac::open_settings()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("SMAppService is macOS only".into())
    }
}

/// The `SMAppService` replacement for the LaunchAgent's `LowPriorityIO`: `setiopolicy_np` with
/// `IOPOL_THROTTLE` (the policy launchd applies for `LowPriorityIO`) for the whole process, or
/// `IOPOL_DEFAULT` when `on` is false. A login-launched app calls `set_background_io(true)` at start
/// and `set_background_io(false)` once the login storm has passed. macOS only.
pub fn set_background_io(on: bool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        mac::set_background_io(on)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = on;
        Err("throttled disk I/O policy is macOS only".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn raw_values_map_to_the_four_apple_states() {
        assert_eq!(login_item_status_from_raw(0), LoginItemStatus::NotRegistered);
        assert_eq!(login_item_status_from_raw(1), LoginItemStatus::Enabled);
        assert_eq!(
            login_item_status_from_raw(2),
            LoginItemStatus::RequiresApproval
        );
        assert_eq!(login_item_status_from_raw(3), LoginItemStatus::NotFound);
    }

    #[test]
    fn an_unknown_raw_value_is_carried_and_never_enabled() {
        let s = login_item_status_from_raw(7);
        assert_eq!(s, LoginItemStatus::Unknown(7));
        assert!(!s.is_enabled());
        assert!(!s.needs_approval());
        assert_eq!(login_item_status_from_raw(-1), LoginItemStatus::Unknown(-1));
    }

    #[test]
    fn only_enabled_is_enabled_and_only_requires_approval_needs_approval() {
        use LoginItemStatus::*;
        for s in [NotRegistered, Enabled, RequiresApproval, NotFound, Unknown(9)] {
            assert_eq!(s.is_enabled(), s == Enabled);
            assert_eq!(s.needs_approval(), s == RequiresApproval);
        }
    }

    #[test]
    fn status_serialises_camel_case() {
        assert_eq!(
            serde_json::to_string(&LoginItemStatus::RequiresApproval).unwrap(),
            "\"requiresApproval\""
        );
        assert_eq!(
            serde_json::to_string(&LoginItemStatus::NotRegistered).unwrap(),
            "\"notRegistered\""
        );
    }

    #[test]
    fn bundle_detection() {
        assert!(executable_in_app_bundle(Path::new(
            "/Applications/Demo.app/Contents/MacOS/Demo"
        )));
        assert!(!executable_in_app_bundle(Path::new(
            "/Users/me/target/debug/demo"
        )));
    }

    #[test]
    fn service_management_needs_all_four_conditions() {
        assert!(service_management_selected(true, true, true, false));
        assert!(!service_management_selected(false, true, true, false));
        assert!(!service_management_selected(true, false, true, false));
        assert!(!service_management_selected(true, true, false, false));
        assert!(!service_management_selected(true, true, true, true));
    }

    #[test]
    fn register_failure_that_left_the_item_registered_is_a_success() {
        let err = || Err("already registered".to_owned());
        assert_eq!(
            register_result(Ok(()), LoginItemStatus::RequiresApproval),
            Ok(LoginItemStatus::RequiresApproval)
        );
        assert_eq!(
            register_result(err(), LoginItemStatus::Enabled),
            Ok(LoginItemStatus::Enabled)
        );
        assert_eq!(
            register_result(err(), LoginItemStatus::RequiresApproval),
            Ok(LoginItemStatus::RequiresApproval)
        );
        assert!(register_result(err(), LoginItemStatus::NotRegistered).is_err());
        assert!(register_result(err(), LoginItemStatus::NotFound).is_err());
    }

    #[test]
    fn unregister_failure_that_left_the_item_gone_is_a_success() {
        let err = || Err("not registered".to_owned());
        assert_eq!(
            unregister_result(err(), LoginItemStatus::NotRegistered),
            Ok(LoginItemStatus::NotRegistered)
        );
        assert_eq!(
            unregister_result(err(), LoginItemStatus::NotFound),
            Ok(LoginItemStatus::NotFound)
        );
        assert!(unregister_result(err(), LoginItemStatus::Enabled).is_err());
        assert!(unregister_result(err(), LoginItemStatus::RequiresApproval).is_err());
    }
}
