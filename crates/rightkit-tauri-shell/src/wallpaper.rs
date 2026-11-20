//! The user's current desktop wallpaper as a data URI. Moved from HeardRight `commands/system.rs`
//! (`wallpaper_data_blocking`). Read-only, never leaves the device; `None` on failure. WebKit
//! renders HEIC natively, so macOS bytes ship unconverted.
//!
//! For dynamic/aerial wallpapers macOS returns a generic default image rather than the live
//! frame, which still looks like a real desktop. Static wallpapers return the real file.
//!
//! Blocking. A Tauri command that calls it must run it off the main thread (`spawn_blocking`):
//! the macOS path queues a closure to the main thread and waits for it, so a call made on the main
//! thread would deadlock until the 3 s timeout.
//!
//! Host inputs: on Windows, the Roaming AppData folder (the host's known-folder resolver; the
//! transcoded wallpaper cache lives under it).

use std::path::PathBuf;

/// Largest wallpaper file read (24 MiB).
#[cfg(any(target_os = "macos", target_os = "windows"))]
const MAX_WALLPAPER_BYTES: usize = 24 * 1024 * 1024;

pub fn data_uri(
    app: tauri::AppHandle,
    windows_roaming_appdata: fn() -> Option<PathBuf>,
) -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        let _ = app;
        // Windows renders the CURRENT desktop wallpaper, whatever its source format, slideshow
        // frame, or Fit/Fill crop, to a JPEG cache. Read that so a taskbar mock shows the user's
        // REAL desktop. SPI_GETDESKWALLPAPER returns the ORIGINAL path (empty for a solid color or
        // odd format); the transcoded cache is always a WebView2-native JPEG of what is on screen.
        let appdata = windows_roaming_appdata()?;
        let cache = appdata
            .join("Microsoft")
            .join("Windows")
            .join("Themes")
            .join("TranscodedWallpaper");
        let bytes = std::fs::read(&cache).ok()?;
        if bytes.is_empty() || bytes.len() > MAX_WALLPAPER_BYTES {
            return None;
        }
        Some(format!(
            "data:image/jpeg;base64,{}",
            crate::b64::encode(&bytes)
        ))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (app, windows_roaming_appdata);
        None
    }
    #[cfg(target_os = "macos")]
    {
        let _ = windows_roaming_appdata;
        // NSScreen/NSWorkspace are main-thread-only; Tauri commands run on a worker thread, so hop
        // for the path lookup, then read/encode off-main.
        let (tx, rx) = std::sync::mpsc::channel::<Option<String>>();
        let _ = app.run_on_main_thread(move || {
            use objc2_app_kit::{NSScreen, NSWorkspace};
            let path = (|| {
                let mtm = objc2_foundation::MainThreadMarker::new()?;
                let screen = NSScreen::mainScreen(mtm)?;
                let url = NSWorkspace::sharedWorkspace().desktopImageURLForScreen(&screen)?;
                Some(url.path()?.to_string())
            })();
            let _ = tx.send(path);
        });
        let path = rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .ok()
            .flatten()?;
        let bytes = std::fs::read(&path).ok()?;
        if bytes.len() > MAX_WALLPAPER_BYTES {
            return None;
        }
        let mime = match std::path::Path::new(&path)
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("heic") => "image/heic",
            Some("png") => "image/png",
            Some("tiff") | Some("tif") => "image/tiff",
            _ => "image/jpeg",
        };
        Some(format!("data:{mime};base64,{}", crate::b64::encode(&bytes)))
    }
}
