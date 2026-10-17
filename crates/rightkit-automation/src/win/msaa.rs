//! Window-targeted MSAA default action: `IAccessible::accDoDefaultAction` on one child of a
//! window (tier 3). It runs the same action as a click on the element (press a button, toggle a
//! check box, open a menu item) through the accessibility object, so no foreground window and
//! no global input is used.

use crate::model::*;
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use windows::core::Interface;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::System::Variant::{VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_I4};
use windows::Win32::UI::Accessibility::{AccessibleObjectFromWindow, IAccessible};

/// `OBJID_CLIENT` (`(DWORD)-4`): the client area object of a window.
const OBJID_CLIENT: u32 = 0xFFFF_FFFC;

/// Run the default action of child `child` of the window `hwnd`. `child` is the MSAA child id
/// (`CHILDID_SELF` = 0 acts on the client object itself; a 1-based child index addresses one of
/// its children). Returns the COM error when the target has no default action or refuses it.
pub fn default_action(hwnd: u64, child: i32) -> Result<()> {
    if hwnd == 0 {
        return Err(Error::NotFound("MSAA window 0".into()));
    }
    // Balanced with CoUninitialize on drop when this call added a reference (S_OK or S_FALSE);
    // RPC_E_CHANGED_MODE (the thread is already an STA) is an error, not a silent success.
    let _com = ComGuard::init()?;
    let handle = HWND(hwnd as isize as *mut _);
    let mut raw: *mut c_void = std::ptr::null_mut();
    unsafe { AccessibleObjectFromWindow(handle, OBJID_CLIENT, &IAccessible::IID, &mut raw) }
        .map_err(|e| Error::Platform(i64::from(e.code().0), "AccessibleObjectFromWindow"))?;
    if raw.is_null() {
        return Err(Error::NotFound(format!(
            "MSAA client object of window {hwnd}"
        )));
    }
    // SAFETY: `raw` is the +1 reference AccessibleObjectFromWindow returned; `from_raw` takes it
    // over and releases it on drop.
    let accessible = unsafe { IAccessible::from_raw(raw) };
    let id = variant_i32(child);
    unsafe { accessible.accDoDefaultAction(&id) }
        .map_err(|e| Error::Platform(i64::from(e.code().0), "accDoDefaultAction"))
}

/// Whether an `HRESULT` from `CoInitializeEx` took a COM reference on the thread that
/// `CoUninitialize` must release: `S_OK` and `S_FALSE` do, every failure does not.
fn init_needs_uninit(hr: i32) -> bool {
    hr == 0 || hr == 1
}

/// Releases this thread's COM initialization reference when dropped.
struct ComGuard;

impl ComGuard {
    fn init() -> Result<ComGuard> {
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if init_needs_uninit(hr.0) {
            Ok(ComGuard)
        } else {
            // Includes RPC_E_CHANGED_MODE: nothing to uninitialize.
            Err(Error::Platform(i64::from(hr.0), "CoInitializeEx"))
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

/// An `i32` `VARIANT` (`VT_I4`), the child id form `accDoDefaultAction` takes.
fn variant_i32(v: i32) -> VARIANT {
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_I4,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 { lVal: v },
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_object_id_is_objid_client() {
        assert_eq!(OBJID_CLIENT as i32, -4);
    }

    #[test]
    fn only_success_codes_take_a_com_reference() {
        assert!(init_needs_uninit(0)); // S_OK
        assert!(init_needs_uninit(1)); // S_FALSE
        assert!(!init_needs_uninit(0x8001_0106_u32 as i32)); // RPC_E_CHANGED_MODE
        assert!(!init_needs_uninit(0x8007_000E_u32 as i32)); // E_OUTOFMEMORY
    }

    #[test]
    fn null_window_is_refused() {
        assert!(matches!(default_action(0, 0), Err(Error::NotFound(_))));
    }
}
