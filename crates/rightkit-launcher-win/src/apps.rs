//! What a Windows launcher lists to open: the Start menu's shortcuts with the Store apps, the
//! programs that have a window open now, whether a link scheme is registered, and the clipboard's
//! text for a search field. Plain queries of the shell and the registry; nothing here writes, and
//! nothing is cached (the application schedules its own scans).

use std::collections::HashSet;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GWL_EXSTYLE, GetWindow, GetWindowLongPtrW, GetWindowTextW,
    GetWindowThreadProcessId, IsWindowVisible,
};
use windows::core::BOOL;

const WS_EX_NOACTIVATE: u32 = 0x0800_0000;
const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
const CF_UNICODETEXT: u32 = 13;

#[allow(non_snake_case)]
mod ffi {
    use std::ffi::c_void;

    pub type Handle = *mut c_void;

    #[link(name = "user32")]
    unsafe extern "system" {
        pub fn OpenClipboard(owner: Handle) -> i32;
        pub fn CloseClipboard() -> i32;
        pub fn GetClipboardData(format: u32) -> Handle;
    }

    #[link(name = "advapi32")]
    unsafe extern "system" {
        pub fn RegOpenKeyExW(
            key: isize,
            sub_key: *const u16,
            options: u32,
            desired: u32,
            result: *mut isize,
        ) -> i32;
        pub fn RegCloseKey(key: isize) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn GlobalLock(memory: Handle) -> *mut c_void;
        pub fn GlobalUnlock(memory: Handle) -> i32;
        pub fn GlobalSize(memory: Handle) -> usize;
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Whether Windows has a link scheme (`chatgpt:`) registered under `HKEY_CLASSES_ROOT`.
pub fn scheme_registered(scheme: &str) -> bool {
    const HKEY_CLASSES_ROOT: isize = 0x8000_0000_u32 as i32 as isize;
    const KEY_READ: u32 = 0x0002_0019;
    let name = wide(scheme);
    let mut key = 0isize;
    // SAFETY: NUL-terminated key name and a valid out pointer, alive for the call; the
    // handle is closed right after a success.
    unsafe {
        if ffi::RegOpenKeyExW(HKEY_CLASSES_ROOT, name.as_ptr(), 0, KEY_READ, &mut key) != 0 {
            return false;
        }
        ffi::RegCloseKey(key);
    }
    true
}

/// The Start menu's program folders: everyone's (`%ProgramData%`), then this user's (`%APPDATA%`).
pub fn start_menu_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for variable in ["ProgramData", "APPDATA"] {
        if let Some(base) = std::env::var_os(variable) {
            roots.push(
                PathBuf::from(base)
                    .join("Microsoft")
                    .join("Windows")
                    .join("Start Menu")
                    .join("Programs"),
            );
        }
    }
    roots
}

fn scan_folder(
    dir: &Path,
    depth: usize,
    seen: &mut HashSet<String>,
    out: &mut Vec<(String, String)>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            if depth > 0 {
                scan_folder(&path, depth - 1, seen, out);
            }
            continue;
        }
        let is_shortcut = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("lnk"));
        let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let lower = name.to_lowercase();
        // Uninstallers and read-me links fill the Start menu's folders; they are not apps.
        if !is_shortcut || lower.contains("uninstall") || lower.contains("readme") {
            continue;
        }
        if seen.insert(lower) {
            out.push((name.to_string(), path.to_string_lossy().into_owned()));
        }
    }
}

/// The Start menu's shortcuts (everyone's, then this user's, `depth` folders deep) and the Store
/// apps ([`crate::store::apps`]), as (name, what opens it), sorted by name. A name already taken
/// by a shortcut is not listed again.
pub fn scan_apps(depth: usize) -> Vec<(String, String)> {
    let mut seen = HashSet::new();
    let mut apps = Vec::new();
    for root in start_menu_roots() {
        scan_folder(&root, depth, &mut seen, &mut apps);
    }
    for (name, path) in crate::store::apps() {
        if seen.insert(name.to_lowercase()) {
            apps.push((name, path));
        }
    }
    apps.sort_by_key(|(name, _)| name.to_lowercase());
    apps
}

/// A program's file name as a title: `chrome` reads `Chrome`.
pub fn display_name(stem: &str) -> String {
    let mut chars = stem.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// A top-level window a user would call an open app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppWindow {
    /// The `HWND` as an integer.
    pub hwnd: isize,
    /// The owning process.
    pub pid: u32,
}

struct Collect {
    own: u32,
    found: Vec<AppWindow>,
}

unsafe extern "system" fn collect_window(hwnd: HWND, data: LPARAM) -> BOOL {
    // SAFETY: EnumWindows supplies live handles; `data` is the caller's `Collect`, alive for
    // the call. Read-only window queries only.
    unsafe {
        let context = &mut *(data.0 as *mut Collect);
        if !IsWindowVisible(hwnd).as_bool() {
            return true.into();
        }
        let owned = match GetWindow(hwnd, GW_OWNER) {
            Ok(owner) => !owner.0.is_null(),
            Err(_) => false,
        };
        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        if owned || ex_style & (WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW) != 0 {
            return true.into();
        }
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut c_void,
            std::mem::size_of::<u32>() as u32,
        );
        let mut title = [0u16; 8];
        if cloaked != 0 || GetWindowTextW(hwnd, &mut title) <= 0 {
            return true.into();
        }
        let mut pid = 0u32;
        let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == context.own {
            return true.into();
        }
        context.found.push(AppWindow {
            hwnd: hwnd.0 as isize,
            pid,
        });
        true.into()
    }
}

/// The windows of open apps, front to back: visible, not owned by another window, not a tool or
/// no-activate window, not cloaked (a window on another virtual desktop), with a title, and not of
/// process `own`. One entry per window; the caller picks one per program.
pub fn app_windows(own: u32) -> Vec<AppWindow> {
    let mut context = Collect {
        own,
        found: Vec::new(),
    };
    // SAFETY: the callback only runs during this call; `context` lives past it.
    let _ = unsafe {
        EnumWindows(
            Some(collect_window),
            LPARAM(&mut context as *mut Collect as isize),
        )
    };
    context.found
}

/// The clipboard's text, at most `max_chars` UTF-16 units of it (up to the first NUL), for Ctrl+V in
/// a search field. `None` when the clipboard cannot be opened or holds no text.
pub fn clipboard_text(max_chars: usize) -> Option<String> {
    // SAFETY: the clipboard is opened and closed here; the handle it returns is only read,
    // within its reported size, while locked.
    unsafe {
        if ffi::OpenClipboard(std::ptr::null_mut()) == 0 {
            return None;
        }
        let mut text = None;
        let handle = ffi::GetClipboardData(CF_UNICODETEXT);
        if !handle.is_null() {
            let units = ffi::GlobalSize(handle) / 2;
            let pointer = ffi::GlobalLock(handle) as *const u16;
            if !pointer.is_null() {
                let all = std::slice::from_raw_parts(pointer, units.min(max_chars));
                let end = all.iter().position(|unit| *unit == 0).unwrap_or(all.len());
                text = Some(String::from_utf16_lossy(&all[..end]));
                ffi::GlobalUnlock(handle);
            }
        }
        ffi::CloseClipboard();
        text
    }
}

#[cfg(test)]
mod tests {
    use super::display_name;

    #[test]
    fn titles_start_upper() {
        assert_eq!(display_name("chrome"), "Chrome");
        assert_eq!(display_name(""), "");
    }
}
