//! Native fatal-startup dialog and the window icon (#498): what an app shows when it cannot start,
//! before any webview exists, and the icon the taskbar and Dock use for a window.
//!
//! The dialog is the platform's own: `MessageBoxW` on Windows, `display alert` through `osascript` on
//! macOS. Its text is passed as separate arguments, never spliced into a script, so a message with
//! quotes or AppleScript cannot change what runs. Elsewhere the message goes to stderr.

/// Show a blocking, modal error dialog with `title` and `message`. Returns once the person dismisses
/// it. Use it only when the app cannot continue; the app then exits.
pub fn show(title: &str, message: &str) {
    #[cfg(windows)]
    show_windows(title, message);
    #[cfg(target_os = "macos")]
    show_macos(title, message);
    #[cfg(not(any(windows, target_os = "macos")))]
    eprintln!("{title}: {message}");
}

#[cfg(windows)]
fn show_windows(title: &str, message: &str) {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "user32")]
    extern "system" {
        fn MessageBoxW(
            hwnd: *mut std::ffi::c_void,
            text: *const u16,
            caption: *const u16,
            kind: u32,
        ) -> i32;
    }
    const MB_OK: u32 = 0x0000_0000;
    const MB_ICONERROR: u32 = 0x0000_0010;
    let wide = |s: &str| -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let text = wide(message);
    let caption = wide(title);
    // SAFETY: both buffers are NUL-terminated UTF-16 and live for the call; a null owner window makes
    // the box application-modal.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            caption.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(target_os = "macos")]
fn show_macos(title: &str, message: &str) {
    // The script is fixed; the two strings travel as argv, so no text reaches the script source.
    let status = std::process::Command::new("/usr/bin/osascript")
        .args([
            "-e",
            "on run argv",
            "-e",
            "display alert (item 1 of argv) message (item 2 of argv) as critical",
            "-e",
            "end run",
            "--",
            title,
            message,
        ])
        .status();
    if status.is_err() {
        eprintln!("{title}: {message}");
    }
}

/// Set a window's icon (the taskbar and title bar on Windows, the Dock while it is shown on macOS)
/// from PNG bytes the app bundles (#498). Decoding is Tauri's (`image-png`). A bad image is an error,
/// and the window keeps the icon it had.
pub fn set_window_icon(
    window: &tauri::WebviewWindow<tauri::Wry>,
    png: &[u8],
) -> Result<(), crate::ShellError> {
    let image = tauri::image::Image::from_bytes(png)
        .map_err(|e| crate::ShellError::Invalid(format!("window icon: {e}")))?;
    window
        .set_icon(image)
        .map_err(|e| crate::ShellError::Other(format!("window icon: {e}")))
}
