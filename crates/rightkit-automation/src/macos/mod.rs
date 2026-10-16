//! macOS backend: public ApplicationServices / CoreGraphics / AppKit APIs only.
//! No Python, no Swift, no third-party crates.

mod appkit;
pub mod ax;
pub mod background;
mod capture;
pub(crate) mod capture_safety;
mod events;
mod ffi;

use crate::model::*;
use crate::platform::*;
use crate::proof::FieldText;
use ax::El;
use ffi::*;
use std::path::Path;

#[derive(Default)]
pub struct MacPlatform;

impl MacPlatform {
    pub fn new() -> Self {
        MacPlatform
    }
}

/// All windows, front to back. `on_screen_only=false` includes minimized/other-Space windows.
pub fn list_windows(on_screen_only: bool) -> Vec<WindowInfo> {
    let opt = if on_screen_only { 1 | 16 } else { 16 };
    let Some(arr) = Cf::new(unsafe { CGWindowListCopyWindowInfo(opt, 0) }) else {
        return vec![];
    };
    let mut out = vec![];
    unsafe {
        for i in 0..CFArrayGetCount(arr.0) {
            let d = CFArrayGetValueAtIndex(arr.0, i);
            let b = dict_get(d, "kCGWindowBounds");
            let num = |k: &str| cf_to_f64(dict_get(b, k)).unwrap_or(0.0);
            out.push(WindowInfo {
                window_id: cf_to_i64(dict_get(d, "kCGWindowNumber")).unwrap_or(0) as u64,
                pid: cf_to_i64(dict_get(d, "kCGWindowOwnerPID")).unwrap_or(0) as i32,
                owner: cf_to_string(dict_get(d, "kCGWindowOwnerName")).unwrap_or_default(),
                title: cf_to_string(dict_get(d, "kCGWindowName")).unwrap_or_default(),
                layer: cf_to_i64(dict_get(d, "kCGWindowLayer")).unwrap_or(0) as i32,
                on_screen: cf_to_bool(dict_get(d, "kCGWindowIsOnscreen")).unwrap_or(false),
                bounds: Rect {
                    x: num("X"),
                    y: num("Y"),
                    w: num("Width"),
                    h: num("Height"),
                },
            });
        }
    }
    out
}

impl Platform for MacPlatform {
    type El = El;

    fn kind(&self) -> PlatformKind {
        PlatformKind::MacOs
    }

    fn status(&self) -> Status {
        let (a, s) = unsafe {
            (
                AXIsProcessTrusted() != 0,
                CGPreflightScreenCaptureAccess() != 0,
            )
        };
        let mut notes = vec![];
        if !a {
            notes.push(
                "grant Accessibility to the host process (System Settings > Privacy & Security)"
                    .into(),
            );
        }
        if !s {
            notes.push("grant Screen Recording to the host process for window capture".into());
        }
        Status {
            platform: PlatformKind::MacOs,
            accessibility: a,
            screen_capture: s,
            notes,
        }
    }

    fn apps(&self) -> Vec<AppInfo> {
        appkit::apps()
    }
    fn app(&self, pid: i32) -> Option<AppInfo> {
        appkit::app(pid)
    }
    fn windows(&self, on_screen_only: bool) -> Vec<WindowInfo> {
        list_windows(on_screen_only)
    }
    fn frontmost(&self) -> Option<FrontApp> {
        let pid = appkit::frontmost_pid()?;
        let app_id = appkit::app(pid).map(|a| a.app_id).unwrap_or_default();
        let window_id = El::app(pid)
            .ok()
            .and_then(|a| a.el_attr("AXFocusedWindow"))
            .and_then(|w| w.window_id());
        Some(FrontApp {
            pid,
            app_id,
            window_id,
        })
    }

    fn wake(&self, pid: i32) {
        // Chromium/Electron/WebKit only build their AX trees after being asked.
        if let Ok(app) = El::app(pid) {
            let _ = app.set_bool("AXManualAccessibility", true);
        }
    }

    fn root(&self, t: &Target) -> Result<El> {
        let app = El::app(t.pid)?;
        match ax::window_root(&app, t.window_id) {
            Some(w) => Ok(w),
            None if t.window_id.is_none() => Ok(app),
            None => Err(Error::NotFound(format!(
                "window {:?} of pid {}",
                t.window_id, t.pid
            ))),
        }
    }
    fn read(&self, el: &El) -> Option<RawNode> {
        ax::read_node(el)
    }
    fn children(&self, el: &El) -> Vec<El> {
        el.children()
    }
    fn focused(&self, pid: i32) -> Result<Option<El>> {
        let app = El::app(pid)?;
        Ok(ax::focused_editor(&app))
    }
    fn same(&self, a: &El, b: &El) -> bool {
        a.same(b)
    }
    fn window_of(&self, el: &El) -> Option<u64> {
        el.window_id()
    }
    fn capture(&self, window_id: u64, path: &Path) -> Result<()> {
        capture::capture(window_id, path)
    }

    fn semantic(&self, el: &El, action: &Action) -> Result<String> {
        ax::semantic(el, action)
    }
    fn menu_press(&self, pid: i32, path: &[String]) -> Result<()> {
        ax::menu_press(&El::app(pid)?, path)
    }

    fn field_text(&self, el: &El) -> Option<FieldText> {
        ax::field_text(el)
    }
    fn native_write_capable(&self, el: &El) -> bool {
        ax::native_write_capable(el)
    }
    fn native_write(&self, el: &El, text: &str) -> Result<()> {
        ax::native_write(el, text)
    }

    fn post(&self, pid: i32, window: Option<u64>, ev: &InputEvent) -> Result<()> {
        // AppKit swallows the first click on an inactive window, so mark the window
        // main/key inside the app (AX) first. This never activates the app.
        if matches!(ev, InputEvent::Click { .. } | InputEvent::Scroll { .. }) {
            if let Ok(app) = El::app(pid) {
                if let Some(w) = ax::window_root(&app, window) {
                    let _ = w.set_bool("AXMain", true);
                    let _ = w.set_bool("AXFocused", true);
                }
            }
        }
        events::send(events::Dest::Pid(pid), window, ev)
    }
    fn activate(&self, pid: i32) -> bool {
        appkit::activate(pid)
    }
    fn foreground(&self, _pid: i32, ev: &InputEvent) -> Result<()> {
        events::send(events::Dest::Hid, None, ev)
    }

    fn clip_snapshot(&self) -> Option<ClipSnapshot> {
        appkit::clip_snapshot()
    }
    fn clip_write(&self, text: &str) -> Option<u64> {
        appkit::clip_write(text)
    }
    fn clip_restore(&self, snapshot: &ClipSnapshot, expected_change: u64) -> bool {
        appkit::clip_restore(snapshot, expected_change)
    }
    fn clip_change(&self) -> u64 {
        appkit::clip_change()
    }

    fn resolve_launch(&self, spec: &str) -> Result<String> {
        appkit::resolve_launch(spec)
    }
    fn launch(&self, spec: &str) -> Result<i32> {
        appkit::launch(spec)
    }
    fn terminate(&self, pid: i32) -> Result<()> {
        if appkit::terminate(pid) {
            Ok(())
        } else {
            Err(Error::NotFound(format!("pid {pid}")))
        }
    }
}
