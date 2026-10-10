//! Windows backend: UI Automation for observation and semantic actions,
//! window-addressed messages for tier 3, SendInput for the opt-in tier 4,
//! PrintWindow for per-window capture. No Python, no Node.

mod sys;
mod uia;

use crate::model::*;
use crate::platform::*;
use crate::proof::FieldText;
use std::path::Path;
use uia::El;

/// Windows backend. Every process it launches is owned: it lives in a kill-on-close
/// job object held here, so dropping the platform (run end, cancel, crash) kills
/// the launched apps and their helpers. Only owned processes can be terminated.
#[derive(Default)]
pub struct WinPlatform {
    owned: std::sync::Mutex<std::collections::HashMap<i32, sys::Owned>>,
    /// Live-process cap per launched job (0 = unlimited; 1 = no children; 2 = one helper).
    process_cap: u32,
}

impl WinPlatform {
    pub fn new() -> Self {
        sys::set_dpi_aware();
        uia::ensure_com();
        WinPlatform::default()
    }
    /// Cap how many processes a launched app may have alive (1 = zero-child profile).
    pub fn with_process_cap(mut self, cap: u32) -> Self {
        self.process_cap = cap;
        self
    }
}

impl Platform for WinPlatform {
    type El = El;

    fn kind(&self) -> PlatformKind {
        PlatformKind::Windows
    }

    fn status(&self) -> Status {
        uia::ensure_com();
        Status {
            platform: PlatformKind::Windows,
            accessibility: true,
            screen_capture: true,
            notes: vec!["UI Automation needs no grant; driving an elevated app from a non-elevated process is blocked by UIPI (UIAccess signing lifts it)".into()],
        }
    }

    fn apps(&self) -> Vec<AppInfo> {
        sys::apps()
    }
    fn windows(&self, on_screen_only: bool) -> Vec<WindowInfo> {
        sys::list_windows(on_screen_only)
    }
    fn frontmost(&self) -> Option<FrontApp> {
        sys::frontmost()
    }

    fn root(&self, t: &Target) -> Result<El> {
        uia::ensure_com();
        let h = sys::main_window(t.pid, t.window_id)
            .ok_or_else(|| Error::NotFound(format!("window of pid {}", t.pid)))?;
        uia::element_from_hwnd(h)
    }
    fn read(&self, el: &El) -> Option<RawNode> {
        uia::read_node(el)
    }
    fn children(&self, el: &El) -> Vec<El> {
        el.children()
    }
    fn focused(&self, pid: i32) -> Result<Option<El>> {
        uia::ensure_com();
        let root = sys::main_window(pid, None).and_then(|h| uia::element_from_hwnd(h).ok());
        Ok(uia::focused_editor(pid, root))
    }
    fn same(&self, a: &El, b: &El) -> bool {
        uia::same(a, b)
    }
    fn window_of(&self, el: &El) -> Option<u64> {
        el.top_hwnd().map(|h| h as u64)
    }
    fn capture(&self, window_id: u64, path: &Path) -> Result<()> {
        sys::capture(window_id, path)
    }

    fn semantic(&self, el: &El, action: &Action) -> Result<String> {
        uia::semantic(el, action)
    }
    fn menu_press(&self, _pid: i32, _path: &[String]) -> Result<()> {
        // UIA menu items only exist while the menu is open; opening it is visible and
        // takes focus. Use accelerators (tier 4) or the app's own commands instead.
        Err(Error::Unsupported(
            "menu commands need the menu opened; no background form on Windows",
        ))
    }

    fn field_text(&self, el: &El) -> Option<FieldText> {
        uia::field_text(el)
    }
    fn native_write_capable(&self, el: &El) -> bool {
        uia::native_write_capable(el)
    }
    fn native_write(&self, el: &El, text: &str) -> Result<()> {
        uia::native_write(el, text)
    }

    fn post(&self, pid: i32, window: Option<u64>, ev: &InputEvent) -> Result<()> {
        sys::post(pid, window, ev)
    }
    fn activate(&self, pid: i32) -> bool {
        sys::activate(pid)
    }
    fn foreground(&self, _pid: i32, ev: &InputEvent) -> Result<()> {
        sys::foreground_send(ev)
    }

    fn clip_snapshot(&self) -> Option<ClipSnapshot> {
        sys::clip_snapshot()
    }
    fn clip_write(&self, text: &str) -> Option<u64> {
        sys::clip_write(text)
    }
    fn clip_restore(&self, snapshot: &ClipSnapshot, expected_change: u64) -> bool {
        sys::clip_restore(snapshot, expected_change)
    }
    fn clip_change(&self) -> u64 {
        sys::clip_change()
    }

    fn resolve_launch(&self, spec: &str) -> Result<String> {
        sys::resolve_launch(spec).map(|(_, id)| id)
    }
    fn launch(&self, spec: &str) -> Result<i32> {
        // Resolve once and launch exactly the resolved file (what was admitted is what runs).
        let (path, id) = sys::resolve_launch(spec)?;
        let owned = sys::launch(&path, &id, self.process_cap)?;
        let pid = owned.pid;
        self.owned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(pid, owned);
        Ok(pid)
    }
    fn terminate(&self, pid: i32) -> Result<()> {
        if !self
            .owned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&pid)
        {
            return Err(Error::Blocked(format!(
                "pid {pid} was not launched by this engine; user apps are never terminated"
            )));
        }
        sys::terminate(pid)?;
        // Exited: release the job (kills any helper the app left behind).
        self.owned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&pid);
        Ok(())
    }
    fn kill_owned(&self, pid: i32) -> Result<()> {
        match self
            .owned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&pid)
        {
            Some(_) => Ok(()), // dropping the Owned terminates the job
            None => Err(Error::Blocked(format!(
                "pid {pid} was not launched by this engine"
            ))),
        }
    }
}
