//! DWM backdrop for the overlay window: blur-behind or acrylic. Best effort by design: the
//! window is a per-pixel-alpha layered window, and whether DWM paints a backdrop under such a
//! window depends on the Windows build and on how the window is composed. The overlay therefore
//! never relies on it; `PaintCtx::glass_layer` is the always-available look, and the result of
//! this call is exposed as `OverlayHandle::backdrop_applied` so a host can choose between them.

use std::ffi::c_void;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Dwm::{DwmEnableBlurBehindWindow, DWM_BB_ENABLE, DWM_BLURBEHIND};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};

use crate::color::Rgba;
use crate::window::Backdrop;

/// `WCA_ACCENT_POLICY` of `SetWindowCompositionAttribute`.
const WCA_ACCENT_POLICY: u32 = 19;
/// `ACCENT_ENABLE_ACRYLICBLURBEHIND`.
const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;

#[repr(C)]
struct AccentPolicy {
    accent_state: u32,
    accent_flags: u32,
    /// `0xAABBGGRR`.
    gradient_color: u32,
    animation_id: u32,
}

#[repr(C)]
struct WindowCompositionAttribData {
    attribute: u32,
    data: *mut c_void,
    size_of_data: usize,
}

type SetWindowCompositionAttributeFn =
    unsafe extern "system" fn(HWND, *mut WindowCompositionAttribData) -> i32;

/// Applies the requested backdrop; returns whether the OS accepted the request.
pub(super) fn apply(hwnd: HWND, backdrop: Backdrop) -> bool {
    match backdrop {
        Backdrop::None => false,
        Backdrop::Blur => enable_blur(hwnd),
        Backdrop::Acrylic { tint } => enable_acrylic(hwnd, tint),
    }
}

fn enable_blur(hwnd: HWND) -> bool {
    let blur = DWM_BLURBEHIND {
        dwFlags: DWM_BB_ENABLE,
        fEnable: 1,
        hRgnBlur: std::ptr::null_mut(),
        fTransitionOnMaximized: 0,
    };
    // SAFETY: `blur` is a valid DWM_BLURBEHIND for the call; `hwnd` is the owner window.
    unsafe { DwmEnableBlurBehindWindow(hwnd, &blur) >= 0 }
}

fn enable_acrylic(hwnd: HWND, tint: Rgba) -> bool {
    // user32 exports SetWindowCompositionAttribute without documenting it, so it is looked up at
    // run time and its absence simply means no acrylic.
    let user32: Vec<u16> = "user32.dll\0".encode_utf16().collect();
    // SAFETY: GetModuleHandleW and GetProcAddress take NUL-terminated names that outlive the
    // call. The transmute converts the untyped export to its known signature.
    unsafe {
        let module = GetModuleHandleW(user32.as_ptr());
        if module.is_null() {
            return false;
        }
        let Some(export) = GetProcAddress(module, b"SetWindowCompositionAttribute\0".as_ptr())
        else {
            return false;
        };
        let set = std::mem::transmute::<
            unsafe extern "system" fn() -> isize,
            SetWindowCompositionAttributeFn,
        >(export);
        // An acrylic tint with zero alpha renders as clear glass on some builds; keep at least 1.
        let alpha = u32::from(tint.a.max(1));
        let mut policy = AccentPolicy {
            accent_state: ACCENT_ENABLE_ACRYLICBLURBEHIND,
            accent_flags: 0,
            gradient_color: (alpha << 24)
                | (u32::from(tint.b) << 16)
                | (u32::from(tint.g) << 8)
                | u32::from(tint.r),
            animation_id: 0,
        };
        let mut data = WindowCompositionAttribData {
            attribute: WCA_ACCENT_POLICY,
            data: (&mut policy as *mut AccentPolicy).cast::<c_void>(),
            size_of_data: std::mem::size_of::<AccentPolicy>(),
        };
        set(hwnd, &mut data) != 0
    }
}
