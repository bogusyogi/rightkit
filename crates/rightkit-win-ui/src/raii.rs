//! RAII owners for every Win32 resource the pill creates. Each Drop releases on every path
//! (early return, `?`, error). No Drop here ever takes the app-state lock.

use crate::diag;
use rightkit_overlay::{OverlayRect, OverlayStyle};
use std::ffi::c_void;
use windows::Win32::Foundation::{E_INVALIDARG, HINSTANCE, HWND};
use windows::Win32::Graphics::Gdi::{DeleteObject, HDC, HGDIOBJ, SelectObject};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyWindow, IDC_ARROW, IsWindow, KillTimer, LoadCursorW, RegisterClassW, UnregisterClassW,
    WNDCLASSW, WNDPROC,
};
use windows::core::{Error, PCWSTR};

pub fn hwnd_key(hwnd: HWND) -> isize {
    hwnd.0 as isize
}

pub fn hwnd_from_key(key: isize) -> HWND {
    HWND(key as *mut c_void)
}

/// A hidden, unlayered topmost tool window of `style` at `(x, y, w, h)`, made by the owner's
/// creator (which refuses click-through, so `style` must not ask for it). The caller registered
/// `class` with `instance`. The error is the last OS error of a failed `CreateWindowExW`.
pub fn create_overlay_unlayered(
    class: PCWSTR,
    title: &str,
    (x, y, w, h): (i32, i32, i32, i32),
    style: OverlayStyle,
    instance: HINSTANCE,
) -> Result<HWND, Error> {
    debug_assert!(!style.click_through);
    // SAFETY: every class is a NUL-terminated constant that outlives the call.
    let name = unsafe { class.to_string() }.map_err(|_| Error::from(E_INVALIDARG))?;
    let rect = OverlayRect {
        x,
        y,
        width: w,
        height: h,
    };
    let raw = rightkit_overlay::windows::create_overlay_window(
        instance.0 as isize,
        &name,
        title,
        rect,
        style,
    );
    if raw == 0 {
        return Err(Error::from_thread());
    }
    Ok(hwnd_from_key(raw))
}

/// The same window made layered, with click-through when `style` asks for it: the creator makes
/// the window plain, then `apply_style` adds the bits, so it ends with `style.ex_style(0)`.
pub fn create_overlay(
    class: PCWSTR,
    title: &str,
    rect: (i32, i32, i32, i32),
    style: OverlayStyle,
    instance: HINSTANCE,
) -> Result<HWND, Error> {
    let plain = OverlayStyle {
        click_through: false,
        ..style
    };
    let hwnd = create_overlay_unlayered(class, title, rect, plain, instance)?;
    rightkit_overlay::windows::apply_style(hwnd_key(hwnd), style);
    Ok(hwnd)
}

/// Registered window class; unregistered on drop. Declare it before any window of the class
/// so reverse drop order destroys windows first.
pub struct ClassGuard {
    name: PCWSTR,
    instance: HINSTANCE,
}

impl ClassGuard {
    pub fn register(name: PCWSTR, proc: WNDPROC, instance: HINSTANCE) -> Result<Self, Error> {
        let cursor = unsafe { LoadCursorW(None, IDC_ARROW) }?;
        let class = WNDCLASSW {
            hInstance: instance,
            lpszClassName: name,
            lpfnWndProc: proc,
            hCursor: cursor,
            ..Default::default()
        };
        if unsafe { RegisterClassW(&class) } == 0 {
            return Err(Error::from_thread());
        }
        Ok(Self { name, instance })
    }
}

impl Drop for ClassGuard {
    fn drop(&mut self) {
        if let Err(error) = unsafe { UnregisterClassW(self.name, Some(self.instance)) } {
            diag::win32_error("UnregisterClassW", &error, "class");
        }
    }
}

/// Owns one top-level window (stored as an integer key so it is `Send` inside the state
/// mutex). Destroyed on drop if it still exists. Must drop on the creating thread.
pub struct OwnedWindow {
    key: isize,
}

impl OwnedWindow {
    /// Adopt a freshly created HWND so later failures still destroy it.
    pub fn adopt(hwnd: HWND) -> Self {
        Self {
            key: hwnd_key(hwnd),
        }
    }

    /// A hidden layered overlay window of `style` (see `create_overlay`), adopted as soon as its
    /// style is applied.
    pub fn overlay(
        class: PCWSTR,
        title: &str,
        rect: (i32, i32, i32, i32),
        style: OverlayStyle,
        instance: HINSTANCE,
    ) -> Result<Self, Error> {
        create_overlay(class, title, rect, style, instance).map(Self::adopt)
    }

    pub fn hwnd(&self) -> HWND {
        hwnd_from_key(self.key)
    }

    pub fn key(&self) -> isize {
        self.key
    }

    pub fn try_destroy(&mut self) -> Result<(), Error> {
        if self.key == 0 {
            return Ok(());
        }
        if unsafe { IsWindow(Some(self.hwnd())) }.as_bool() {
            unsafe { DestroyWindow(self.hwnd()) }?;
        }
        self.key = 0;
        Ok(())
    }
}

impl Drop for OwnedWindow {
    fn drop(&mut self) {
        if let Err(error) = self.try_destroy() {
            diag::win32_error("DestroyWindow", &error, "window");
        }
    }
}

/// Timer owner: KillTimer on drop. Arming goes through `arm_timer` in main.rs, which
/// tracks the applied interval; the guard only guarantees cleanup.
pub struct TimerGuard {
    hwnd: isize,
    id: usize,
}

impl TimerGuard {
    pub fn new(hwnd: HWND, id: usize) -> Self {
        Self {
            hwnd: hwnd_key(hwnd),
            id,
        }
    }
}

impl Drop for TimerGuard {
    fn drop(&mut self) {
        // Fails harmlessly if the window (and thus the timer) is already gone.
        let _ = unsafe { KillTimer(Some(hwnd_from_key(self.hwnd)), self.id) };
    }
}

/// GDI object (brush/pen/font) deleted on drop. Always deselect (see `SelectScope`) first;
/// declare the scope after the object so it drops first.
pub struct GdiObject<T: Copy + Into<HGDIOBJ>>(T);

impl<T: Copy + Into<HGDIOBJ>> GdiObject<T> {
    pub fn new(handle: T, op: &str) -> Option<Self> {
        let object: HGDIOBJ = handle.into();
        if object.0.is_null() {
            diag::last_error(op, "gdi_create");
            None
        } else {
            Some(Self(handle))
        }
    }
    pub fn get(&self) -> T {
        self.0
    }
}

impl<T: Copy + Into<HGDIOBJ>> Drop for GdiObject<T> {
    fn drop(&mut self) {
        if !unsafe { DeleteObject(self.0.into()) }.as_bool() {
            diag::last_error("DeleteObject", "gdi_delete");
        }
    }
}

/// Selects an object into a DC and restores the previous one on drop.
pub struct SelectScope {
    hdc: HDC,
    old: HGDIOBJ,
}

impl SelectScope {
    pub fn select(hdc: HDC, object: HGDIOBJ, op: &str) -> Option<Self> {
        let old = unsafe { SelectObject(hdc, object) };
        if old.0.is_null() || old.0 as isize == -1 {
            diag::last_error(op, "select_object");
            return None;
        }
        Some(Self { hdc, old })
    }
}

impl Drop for SelectScope {
    fn drop(&mut self) {
        unsafe { SelectObject(self.hdc, self.old) };
    }
}
