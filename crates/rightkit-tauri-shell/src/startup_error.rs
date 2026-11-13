//! Native fatal-startup dialog and the window icon (#498): what an app shows when it cannot start,
//! before any webview exists, and the icon the taskbar and Dock use for a window.
//!
//! The dialog is the platform's own: `MessageBoxW` on Windows, `display alert` through `osascript` on
//! macOS. Its text is passed as separate arguments, never spliced into a script, so a message with
//! quotes or AppleScript cannot change what runs. Elsewhere the message goes to stderr.

/// The macOS dialog [`show_with`] draws.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum MacosDialog {
    /// `display alert ... as critical` (the default).
    #[default]
    AlertCritical,
    /// `display dialog` with the title bar text, one `OK` default button and the stop icon.
    DialogStop,
}

/// Look and stderr text of the dialog for [`show_with`]. `Default` is exactly what [`show`] does.
#[derive(Clone, Debug, Default)]
pub struct StartupErrorOptions {
    macos_dialog: MacosDialog,
    windows_foreground: bool,
    stderr_prefix: String,
    failure_note: bool,
}

impl StartupErrorOptions {
    /// The defaults of [`show`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Which dialog macOS shows (default [`MacosDialog::AlertCritical`]).
    #[must_use]
    pub fn macos_dialog(mut self, dialog: MacosDialog) -> Self {
        self.macos_dialog = dialog;
        self
    }

    /// Windows only: add `MB_SETFOREGROUND` so the box comes to the front of a process that has no
    /// window yet (default off).
    #[must_use]
    pub fn windows_foreground(mut self, on: bool) -> Self {
        self.windows_foreground = on;
        self
    }

    /// Text put at the start of every stderr line, for example `"[myapp] "` (default empty).
    #[must_use]
    pub fn stderr_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.stderr_prefix = prefix.into();
        self
    }

    /// macOS only: when `osascript` cannot be started, print `could not show the start-up error: <error>`
    /// instead of the title and message (default off: the title and message are printed).
    #[must_use]
    pub fn failure_note(mut self, on: bool) -> Self {
        self.failure_note = on;
        self
    }
}

/// Show a blocking, modal error dialog with `title` and `message`. Returns once the person dismisses
/// it. Use it only when the app cannot continue; the app then exits.
pub fn show(title: &str, message: &str) {
    show_with(title, message, &StartupErrorOptions::default());
}

/// [`show`] with the dialog style and stderr text chosen through [`StartupErrorOptions`].
pub fn show_with(title: &str, message: &str, options: &StartupErrorOptions) {
    #[cfg(windows)]
    show_windows(title, message, options);
    #[cfg(target_os = "macos")]
    show_macos(title, message, options);
    #[cfg(not(any(windows, target_os = "macos")))]
    eprintln!("{}{title}: {message}", options.stderr_prefix);
}

#[cfg(windows)]
fn show_windows(title: &str, message: &str, options: &StartupErrorOptions) {
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
    const MB_SETFOREGROUND: u32 = 0x0001_0000;
    let kind = MB_OK
        | MB_ICONERROR
        | if options.windows_foreground {
            MB_SETFOREGROUND
        } else {
            0
        };
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
        MessageBoxW(std::ptr::null_mut(), text.as_ptr(), caption.as_ptr(), kind);
    }
}

#[cfg(target_os = "macos")]
fn show_macos(title: &str, message: &str, options: &StartupErrorOptions) {
    // The script is fixed; the two strings travel as argv, so no text reaches the script source.
    let (script, first, second) = match options.macos_dialog {
        MacosDialog::AlertCritical => (
            "display alert (item 1 of argv) message (item 2 of argv) as critical",
            title,
            message,
        ),
        MacosDialog::DialogStop => (
            "display dialog (item 1 of argv) with title (item 2 of argv) buttons {\"OK\"} default button \"OK\" with icon stop",
            message,
            title,
        ),
    };
    let status = std::process::Command::new("/usr/bin/osascript")
        .args([
            "-e",
            "on run argv",
            "-e",
            script,
            "-e",
            "end run",
            "--",
            first,
            second,
        ])
        .stdout(std::process::Stdio::null())
        .status();
    if let Err(e) = status {
        let prefix = &options.stderr_prefix;
        if options.failure_note {
            eprintln!("{prefix}could not show the start-up error: {e}");
        } else {
            eprintln!("{prefix}{title}: {message}");
        }
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

/// [`set_window_icon`] for any Tauri runtime, from an already decoded image, and on Windows also
/// the native icon messages: the window's own small (`ICON_SMALL`, `ICON_SMALL2`) and big
/// (`ICON_BIG`) icons are set with `WM_SETICON`, which some Windows shells need before the taskbar
/// stops showing a cached icon. The window owns those handles afterwards; they are never
/// destroyed here. Both steps are always tried; the first error is returned.
pub fn set_window_icon_native<R: tauri::Runtime>(
    window: &tauri::WebviewWindow<R>,
    icon: &tauri::image::Image<'_>,
) -> Result<(), crate::ShellError> {
    let tauri_result = window
        .set_icon(icon.clone())
        .map_err(|e| crate::ShellError::Other(format!("window icon: {e}")));
    #[cfg(windows)]
    let native_result = wm_seticon(window, icon);
    #[cfg(not(windows))]
    let native_result = Ok(());
    tauri_result.and(native_result)
}

#[cfg(windows)]
fn wm_seticon<R: tauri::Runtime>(
    window: &tauri::WebviewWindow<R>,
    icon: &tauri::image::Image<'_>,
) -> Result<(), crate::ShellError> {
    use std::ffi::c_void;
    #[link(name = "user32")]
    extern "system" {
        fn CreateIcon(
            instance: *mut c_void,
            width: i32,
            height: i32,
            planes: u8,
            bits_per_pixel: u8,
            and_bits: *const u8,
            xor_bits: *const u8,
        ) -> *mut c_void;
        fn SendMessageW(hwnd: *mut c_void, msg: u32, wparam: usize, lparam: isize) -> isize;
    }
    const WM_SETICON: u32 = 0x0080;
    const ICON_SMALL: usize = 0;
    const ICON_BIG: usize = 1;
    const ICON_SMALL2: usize = 2;
    let fail = |what: String| crate::ShellError::Other(format!("window icon WM_SETICON: {what}"));
    let make = || -> Result<*mut c_void, crate::ShellError> {
        let rgba = icon.rgba();
        let pixels = (icon.width() as usize)
            .checked_mul(icon.height() as usize)
            .ok_or_else(|| fail("icon dimensions overflow".into()))?;
        if rgba.len() != pixels * 4 {
            return Err(fail(format!(
                "icon buffer length mismatch: {} bytes for {}x{}",
                rgba.len(),
                icon.width(),
                icon.height()
            )));
        }
        let mut bgra = Vec::with_capacity(rgba.len());
        let mut and_mask = Vec::with_capacity(pixels);
        for px in rgba.as_chunks::<4>().0 {
            bgra.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
            and_mask.push(px[3].wrapping_sub(u8::MAX));
        }
        // SAFETY: the masks are `pixels` bytes and `pixels * 4` bytes for a 1-plane 32-bit icon of
        // the stated size, and live for the call; Windows copies them.
        let handle = unsafe {
            CreateIcon(
                std::ptr::null_mut(),
                icon.width() as i32,
                icon.height() as i32,
                1,
                32,
                and_mask.as_ptr(),
                bgra.as_ptr(),
            )
        };
        if handle.is_null() {
            return Err(fail("CreateIcon failed".into()));
        }
        Ok(handle)
    };
    let hwnd = window
        .hwnd()
        .map_err(|e| fail(format!("window HWND unavailable: {e}")))?
        .0;
    let small = make()?;
    let big = make()?;
    // SAFETY: `hwnd` is this window's live handle and the icon handles were just created; the
    // window owns them after the messages, so they are not destroyed here.
    unsafe {
        SendMessageW(hwnd.cast(), WM_SETICON, ICON_SMALL, small as isize);
        SendMessageW(hwnd.cast(), WM_SETICON, ICON_SMALL2, small as isize);
        SendMessageW(hwnd.cast(), WM_SETICON, ICON_BIG, big as isize);
    }
    Ok(())
}
