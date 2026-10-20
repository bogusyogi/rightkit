#![cfg(windows)]

use rightkit_overlay::{windows, OverlayRect, OverlayStyle};
use windows_sys::Win32::Foundation::{GetLastError, HINSTANCE, HWND, RECT};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DestroyWindow, GetDesktopWindow, GetForegroundWindow, GetWindowLongPtrW,
    GetWindowRect, IsWindow, IsWindowVisible, RegisterClassW, UnregisterClassW, GWL_EXSTYLE,
    WNDCLASSW,
};

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: *const u16) -> HINSTANCE;
}

struct NativeFixture {
    hwnd: HWND,
    class: Vec<u16>,
    instance: HINSTANCE,
}

impl Drop for NativeFixture {
    fn drop(&mut self) {
        // Test thread owns both resources. This path also covers assertion unwinding.
        unsafe {
            if !self.hwnd.is_null() {
                DestroyWindow(self.hwnd);
            }
            UnregisterClassW(self.class.as_ptr(), self.instance);
        }
    }
}

#[test]
fn create_overlay_window_preserves_native_style_geometry_and_foreground() {
    // Require a managed interactive desktop; missing prerequisites are failures, never skips.
    assert!(
        !unsafe { GetDesktopWindow() }.is_null(),
        "native desktop required"
    );
    let foreground = unsafe { GetForegroundWindow() };
    assert!(!foreground.is_null(), "managed foreground desktop required");
    assert!(
        windows::monitor_at(0, 0).is_some(),
        "native monitor required"
    );
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    assert!(!instance.is_null(), "native module handle required");
    let class_name = format!("RightKitOverlaySmoke-{}", std::process::id());
    let class: Vec<u16> = class_name.encode_utf16().chain(Some(0)).collect();
    let descriptor = WNDCLASSW {
        lpfnWndProc: Some(DefWindowProcW),
        hInstance: instance,
        lpszClassName: class.as_ptr(),
        ..unsafe { std::mem::zeroed() }
    };
    assert_ne!(
        unsafe { RegisterClassW(&descriptor) },
        0,
        "register private class: {}",
        unsafe { GetLastError() }
    );
    let mut fixture = NativeFixture {
        hwnd: std::ptr::null_mut(),
        class,
        instance,
    };
    let rect = OverlayRect {
        x: 32,
        y: 32,
        width: 64,
        height: 32,
    };
    let hwnd = windows::create_overlay_window(
        instance as isize,
        &class_name,
        "",
        rect,
        OverlayStyle::PASSIVE,
    );
    fixture.hwnd = hwnd as HWND;
    assert_ne!(hwnd, 0, "native create_overlay_window: {}", unsafe {
        GetLastError()
    });
    assert_eq!(unsafe { IsWindowVisible(fixture.hwnd) }, 0);
    use rightkit_overlay::ex_style::*;
    let owned = TOPMOST | TRANSPARENT | TOOLWINDOW | APPWINDOW | LAYERED | NOACTIVATE;
    assert_eq!(
        unsafe { GetWindowLongPtrW(fixture.hwnd, GWL_EXSTYLE) } as u32 & owned,
        OverlayStyle::PASSIVE.unlayered_ex_style(0)
    );
    let mut actual: RECT = unsafe { std::mem::zeroed() };
    assert_ne!(unsafe { GetWindowRect(fixture.hwnd, &mut actual) }, 0);
    assert_eq!(
        (
            actual.left,
            actual.top,
            actual.right - actual.left,
            actual.bottom - actual.top
        ),
        (rect.x, rect.y, rect.width, rect.height)
    );
    assert_eq!(unsafe { GetForegroundWindow() }, foreground);
    assert!(windows::show_noactivate(hwnd));
    assert_ne!(unsafe { IsWindowVisible(fixture.hwnd) }, 0);
    assert_eq!(unsafe { GetForegroundWindow() }, foreground);
    assert!(windows::hide(hwnd));
    assert_eq!(unsafe { IsWindowVisible(fixture.hwnd) }, 0);
    assert_ne!(unsafe { DestroyWindow(fixture.hwnd) }, 0);
    fixture.hwnd = std::ptr::null_mut();
    assert_eq!(unsafe { IsWindow(hwnd as HWND) }, 0);
}
