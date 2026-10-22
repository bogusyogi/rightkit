//! Off-screen launch for recording scenarios. The implementation lives in
//! `rightkit_automation::offscreen` and is re-exported here unchanged; this file adds only
//! the Tauri window helpers (`tauri-plugin` feature), which need `tauri` and so cannot live
//! in `rightkit-automation`.
//!
//! Tauri embedded path: the app calls `start_from_env` / `start_for_window` at startup,
//! before its window is first ordered, when it was launched with `RIGHTKIT_OFFSCREEN=1`.

pub use rightkit_automation::offscreen::*;

/// Tauri embedded path: start off-screen mode on `win` (must be created hidden,
/// i.e. `visible: false`, so it has never been ordered). `done` runs on the main
/// thread with the outcome; no blocking, so it is safe to call from `setup`.
#[cfg(feature = "tauri-plugin")]
pub fn start_for_window<R: tauri::Runtime>(
    win: &tauri::WebviewWindow<R>,
    done: impl FnOnce(Result<SessionReport, OffscreenError>) + Send + 'static,
) {
    #[cfg(target_os = "macos")]
    let handle = win.ns_window().map(|p| p as usize);
    #[cfg(windows)]
    let handle = win.hwnd().map(|h| h.0 as usize);
    #[cfg(not(any(target_os = "macos", windows)))]
    let handle: tauri::Result<usize> = Ok(0);
    let handle = match handle {
        Ok(h) => h,
        Err(e) => return done(Err(OffscreenError::Native(e.to_string()))),
    };
    let r = win.with_webview(move |pv| {
        #[cfg(target_os = "macos")]
        let wk = pv.inner();
        #[cfg(not(target_os = "macos"))]
        let wk = {
            let _ = &pv;
            std::ptr::null_mut()
        };
        // SAFETY: handles come from the live window; this closure runs on the main thread.
        done(unsafe { start_native(handle as *mut std::ffi::c_void, wk) });
    });
    if let Err(e) = r {
        // `done` was moved into the closure; surface via the report file only.
        write_report(&Err(OffscreenError::Native(e.to_string())));
    }
}

/// App startup hook: if `RIGHTKIT_OFFSCREEN=1`, start off-screen mode, write the
/// report file for the controller, and call `done`. Returns whether it was requested.
#[cfg(feature = "tauri-plugin")]
pub fn start_from_env<R: tauri::Runtime>(
    win: &tauri::WebviewWindow<R>,
    done: impl FnOnce(&Result<SessionReport, OffscreenError>) + Send + 'static,
) -> bool {
    if !offscreen_requested() {
        return false;
    }
    start_for_window(win, move |r| {
        write_report(&r);
        done(&r);
    });
    true
}
