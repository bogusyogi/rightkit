//! Win32 plumbing: process and window enumeration, targeted messages (tier 3),
//! SendInput (tier 4), activation, window capture, clipboard, launch.

use crate::keys::{self, Chord, Key, Named};
use crate::model::*;
use crate::platform::{ClipSnapshot, InputEvent};
use std::time::Duration;
use windows::core::{BOOL, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::FileSystem::SearchPathW;
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::JobObjects::*;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Ole::{CF_LOCALE, CF_OEMTEXT, CF_TEXT, CF_UNICODETEXT};
use windows::Win32::System::Threading::*;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

fn sleep(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

fn hwnd(h: isize) -> HWND {
    HWND(h as *mut _)
}

/// Make coordinates physical pixels so UIA rectangles, SetCursorPos and captures agree.
pub fn set_dpi_aware() {
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

pub fn pid_of(h: HWND) -> Option<i32> {
    let mut pid = 0u32;
    let tid = unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    (tid != 0 && pid != 0).then_some(pid as i32)
}

pub fn foreground_pid() -> Option<i32> {
    let f = unsafe { GetForegroundWindow() };
    (!f.0.is_null()).then(|| pid_of(f)).flatten()
}

fn window_text(h: HWND) -> String {
    let mut buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

pub fn window_rect(h: HWND) -> Option<Rect> {
    let mut r = RECT::default();
    unsafe { GetWindowRect(h, &mut r) }.ok()?;
    Some(Rect {
        x: f64::from(r.left),
        y: f64::from(r.top),
        w: f64::from(r.right - r.left),
        h: f64::from(r.bottom - r.top),
    })
}

unsafe extern "system" fn collect(h: HWND, lp: LPARAM) -> BOOL {
    let out = &mut *(lp.0 as *mut Vec<HWND>);
    out.push(h);
    BOOL(1)
}

fn all_top_level() -> Vec<HWND> {
    let mut v: Vec<HWND> = vec![];
    let _ = unsafe { EnumWindows(Some(collect), LPARAM(&mut v as *mut Vec<HWND> as isize)) };
    v
}

/// Top-level windows, front to back (Z order).
pub fn list_windows(on_screen_only: bool) -> Vec<WindowInfo> {
    all_top_level()
        .into_iter()
        .filter_map(|h| {
            let visible = unsafe { IsWindowVisible(h) }.as_bool();
            let iconic = unsafe { IsIconic(h) }.as_bool();
            let title = window_text(h);
            let owner = unsafe { GetWindow(h, GW_OWNER) }
                .map(|o| !o.0.is_null())
                .unwrap_or(false);
            let ex = unsafe { GetWindowLongPtrW(h, GWL_EXSTYLE) } as u32;
            if title.is_empty() || ex & WS_EX_TOOLWINDOW.0 != 0 {
                return None;
            }
            let on_screen = visible && !iconic;
            if on_screen_only && !on_screen {
                return None;
            }
            let pid = pid_of(h)?;
            Some(WindowInfo {
                window_id: h.0 as u64,
                pid,
                owner: process_info(pid).map(|p| p.0).unwrap_or_default(),
                title,
                layer: i32::from(owner),
                on_screen,
                bounds: window_rect(h)?,
            })
        })
        .collect()
}

/// `(exe file name lowercase, full path, creation time ms)`.
pub fn process_info(pid: i32) -> Option<(String, Option<String>, Option<u64>)> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid as u32).ok()?;
        let mut buf = [0u16; 1024];
        let mut n = buf.len() as u32;
        let path =
            QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut n)
                .ok()
                .map(|_| String::from_utf16_lossy(&buf[..n as usize]));
        let (mut c, mut e, mut k, mut u) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        let started = GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u)
            .ok()
            .map(|_| ((u64::from(c.dwHighDateTime) << 32) | u64::from(c.dwLowDateTime)) / 10_000);
        let _ = CloseHandle(h);
        let exe = path
            .as_deref()
            .and_then(|p| p.rsplit(['\\', '/']).next())
            .unwrap_or("")
            .to_ascii_lowercase();
        Some((exe, path, started))
    }
}

pub fn apps() -> Vec<AppInfo> {
    let mut seen: Vec<AppInfo> = vec![];
    for w in list_windows(false) {
        if w.layer != 0 || seen.iter().any(|a| a.pid == w.pid) {
            continue;
        }
        if let Some((exe, path, started)) = process_info(w.pid) {
            seen.push(AppInfo {
                pid: w.pid,
                app_id: exe,
                name: w.title,
                path,
                started,
            });
        }
    }
    seen
}

pub fn frontmost() -> Option<FrontApp> {
    let f = unsafe { GetForegroundWindow() };
    if f.0.is_null() {
        return None;
    }
    let pid = pid_of(f)?;
    let app_id = process_info(pid).map(|p| p.0).unwrap_or_default();
    Some(FrontApp {
        pid,
        app_id,
        window_id: Some(f.0 as u64),
    })
}

/// The window to address for a pid: the requested id, else the largest visible top-level window.
pub fn main_window(pid: i32, window_id: Option<u64>) -> Option<isize> {
    if let Some(w) = window_id {
        return Some(w as isize);
    }
    list_windows(false)
        .into_iter()
        .filter(|w| w.pid == pid && w.layer == 0)
        .max_by(|a, b| {
            (u8::from(a.on_screen), a.bounds.w * a.bounds.h)
                .partial_cmp(&(u8::from(b.on_screen), b.bounds.w * b.bounds.h))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|w| w.window_id as isize)
}

// ------------------------------------------------------------------ tier 3

fn focus_target(top: HWND) -> HWND {
    let tid = unsafe { GetWindowThreadProcessId(top, None) };
    let mut gi = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetGUIThreadInfo(tid, &mut gi) }.is_ok() && !gi.hwndFocus.0.is_null() {
        gi.hwndFocus
    } else {
        top
    }
}

fn lparam(x: i32, y: i32) -> LPARAM {
    LPARAM(((y as u32 & 0xFFFF) << 16 | (x as u32 & 0xFFFF)) as i32 as isize)
}

fn post_msg(h: HWND, msg: u32, w: usize, l: LPARAM) -> Result<()> {
    unsafe { PostMessageW(Some(h), msg, WPARAM(w), l) }
        .map_err(|e| Error::Platform(i64::from(e.code().0), "PostMessage"))
}

fn deepest_child_at(top: HWND, sx: i32, sy: i32) -> (HWND, i32, i32) {
    let mut cur = top;
    let mut pt = POINT { x: sx, y: sy };
    // A failed conversion (stale window) leaves `pt` unchanged; the walk tolerates that.
    let _ = unsafe { ScreenToClient(cur, &mut pt) };
    for _ in 0..8 {
        let child = unsafe { RealChildWindowFromPoint(cur, pt) };
        if child.0.is_null() || child == cur {
            break;
        }
        cur = child;
        pt = POINT { x: sx, y: sy };
        let _ = unsafe { ScreenToClient(cur, &mut pt) };
    }
    (cur, pt.x, pt.y)
}

fn key_lparam(vk: u16, up: bool) -> LPARAM {
    let scan = unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC) } & 0xFF;
    let mut l: u32 = 1 | (scan << 16);
    if up {
        l |= (1 << 30) | (1 << 31);
    }
    LPARAM(l as i32 as isize)
}

pub fn post(pid: i32, window: Option<u64>, ev: &InputEvent) -> Result<()> {
    let top = hwnd(
        main_window(pid, window).ok_or_else(|| Error::NotFound(format!("window of pid {pid}")))?,
    );
    match ev {
        InputEvent::Text(t) => {
            let target = focus_target(top);
            for u in t.replace('\n', "\r").encode_utf16() {
                post_msg(target, WM_CHAR, usize::from(u), LPARAM(1))?;
                sleep(2);
            }
            Ok(())
        }
        InputEvent::Key(c) => {
            // Posted messages do not change the thread's key state, so shortcuts that
            // read Ctrl/Alt/Shift cannot work here: climb to the foreground tier instead.
            if c.mods.any() {
                return Err(Error::Unsupported(
                    "modifier chords need the foreground tier",
                ));
            }
            let vk = keys::win_vk(c.key)
                .ok_or_else(|| Error::Invalid(format!("no virtual key for {:?}", c.key)))?;
            let target = focus_target(top);
            post_msg(target, WM_KEYDOWN, usize::from(vk), key_lparam(vk, false))?;
            let ch = match c.key {
                Key::Named(Named::Return) => Some(13u16),
                Key::Named(Named::Tab) => Some(9),
                Key::Named(Named::Backspace) => Some(8),
                Key::Named(Named::Escape) => Some(27),
                _ => keys::inserted_char(*c).map(|c| c as u16),
            };
            if let Some(ch) = ch {
                post_msg(target, WM_CHAR, usize::from(ch), key_lparam(vk, false))?;
            }
            post_msg(target, WM_KEYUP, usize::from(vk), key_lparam(vk, true))
        }
        InputEvent::Click {
            x,
            y,
            button,
            count,
        } => {
            let (child, cx, cy) = deepest_child_at(top, *x as i32, *y as i32);
            let (down, up, dbl, mk) = match button {
                Button::Left => (WM_LBUTTONDOWN, WM_LBUTTONUP, WM_LBUTTONDBLCLK, 1usize),
                Button::Right => (WM_RBUTTONDOWN, WM_RBUTTONUP, WM_RBUTTONDBLCLK, 2usize),
            };
            post_msg(child, WM_MOUSEMOVE, 0, lparam(cx, cy))?;
            sleep(20);
            for n in 1..=(*count).max(1) {
                post_msg(child, if n > 1 { dbl } else { down }, mk, lparam(cx, cy))?;
                sleep(30);
                post_msg(child, up, 0, lparam(cx, cy))?;
                sleep(30);
            }
            Ok(())
        }
        InputEvent::Scroll { x, y, dy, .. } => {
            let (child, _, _) = deepest_child_at(top, *x as i32, *y as i32);
            let delta = (-dy * 120.0).round() as i32 as i16 as u16 as usize;
            post_msg(
                child,
                WM_MOUSEWHEEL,
                delta << 16,
                lparam(*x as i32, *y as i32),
            )
        }
    }
}

// ------------------------------------------------------------------ tier 4

pub(super) fn send(inputs: &[INPUT]) -> Result<()> {
    let n = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    if n as usize == inputs.len() {
        Ok(())
    } else {
        Err(Error::Platform(
            i64::from(n),
            "SendInput (blocked by UIPI or a secure desktop?)",
        ))
    }
}

fn kb(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: crate::marker::event_marker() as usize,
            },
        },
    }
}

fn mouse(flags: MOUSE_EVENT_FLAGS, data: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: crate::marker::event_marker() as usize,
            },
        },
    }
}

fn extended(key: Key) -> bool {
    matches!(
        key,
        Key::Named(
            Named::Left
                | Named::Right
                | Named::Up
                | Named::Down
                | Named::Home
                | Named::End
                | Named::PageUp
                | Named::PageDown
                | Named::Delete
        )
    )
}

fn chord_inputs(c: Chord) -> Result<Vec<INPUT>> {
    let vk = keys::win_vk(c.key)
        .ok_or_else(|| Error::Invalid(format!("no virtual key for {:?}", c.key)))?;
    let mut v = vec![];
    let mods: Vec<u16> = [
        (c.mods.ctrl, 0x11u16),
        (c.mods.alt, 0x12),
        (c.mods.shift, 0x10),
        (c.mods.meta, 0x5B),
    ]
    .iter()
    .filter(|m| m.0)
    .map(|m| m.1)
    .collect();
    for m in &mods {
        v.push(kb(*m, 0, KEYBD_EVENT_FLAGS(0)));
    }
    let ext = if extended(c.key) {
        KEYEVENTF_EXTENDEDKEY
    } else {
        KEYBD_EVENT_FLAGS(0)
    };
    v.push(kb(vk, 0, ext));
    v.push(kb(vk, 0, ext | KEYEVENTF_KEYUP));
    for m in mods.iter().rev() {
        v.push(kb(*m, 0, KEYEVENTF_KEYUP));
    }
    Ok(v)
}

/// Key-up events for every modifier that is currently held down (generic, left and right forms).
fn held_modifier_keyups() -> Vec<INPUT> {
    const MODS: [u16; 11] = [
        0x10, 0x11, 0x12, 0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0x5B, 0x5C,
    ];
    MODS.iter()
        .filter(|vk| (unsafe { GetAsyncKeyState(i32::from(**vk)) } as u16) & 0x8000 != 0)
        .map(|vk| kb(*vk, 0, KEYEVENTF_KEYUP))
        .collect()
}

pub fn foreground_send(ev: &InputEvent) -> Result<()> {
    match ev {
        InputEvent::Text(t) => {
            for u in t.replace('\n', "\r").encode_utf16() {
                if u == 13 {
                    send(&chord_inputs(Chord {
                        mods: Default::default(),
                        key: Key::Named(Named::Return),
                    })?)?;
                } else {
                    send(&[
                        kb(0, u, KEYEVENTF_UNICODE),
                        kb(0, u, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
                    ])?;
                }
                sleep(2);
            }
            Ok(())
        }
        InputEvent::Key(c) => {
            // Foreground hygiene (HeardRight command-host `release_held_modifiers`): a modifier the
            // user is physically holding (e.g. a push-to-talk chord) would contaminate the chord.
            let mut inputs = held_modifier_keyups();
            inputs.extend(chord_inputs(*c)?);
            send(&inputs)
        }
        InputEvent::Click {
            x,
            y,
            button,
            count,
        } => {
            let mut saved = POINT::default();
            let had = unsafe { GetCursorPos(&mut saved) }.is_ok();
            unsafe { SetCursorPos(*x as i32, *y as i32) }
                .map_err(|e| Error::Platform(i64::from(e.code().0), "SetCursorPos"))?;
            sleep(20);
            let (d, u) = match button {
                Button::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
                Button::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
            };
            for _ in 0..(*count).max(1) {
                send(&[mouse(d, 0), mouse(u, 0)])?;
                sleep(30);
            }
            if had {
                let _ = unsafe { SetCursorPos(saved.x, saved.y) };
            }
            Ok(())
        }
        InputEvent::Scroll { x, y, dy, .. } => {
            let mut saved = POINT::default();
            let had = unsafe { GetCursorPos(&mut saved) }.is_ok();
            let _ = unsafe { SetCursorPos(*x as i32, *y as i32) };
            let r = send(&[mouse(MOUSEEVENTF_WHEEL, (-dy * 120.0).round() as i32)]);
            if had {
                let _ = unsafe { SetCursorPos(saved.x, saved.y) };
            }
            r
        }
    }
}

pub fn activate(pid: i32) -> bool {
    let Some(h) = main_window(pid, None).map(hwnd) else {
        return false;
    };
    unsafe {
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        let _ = SetForegroundWindow(h);
        if GetForegroundWindow() != h {
            // Foreground lock: borrow the foreground thread's input queue for the call.
            let fg = GetForegroundWindow();
            let (t_fg, t_me) = (GetWindowThreadProcessId(fg, None), GetCurrentThreadId());
            if t_fg != 0 && t_fg != t_me {
                let _ = AttachThreadInput(t_me, t_fg, true);
                let _ = SetForegroundWindow(h);
                let _ = AttachThreadInput(t_me, t_fg, false);
            }
        }
    }
    foreground_pid() == Some(pid)
}

// ------------------------------------------------------------------ capture

pub fn capture(window_id: u64, path: &std::path::Path) -> Result<()> {
    let h = hwnd(window_id as isize);
    let r = window_rect(h).ok_or_else(|| Error::NotFound(format!("window {window_id}")))?;
    let (w, ht) = (r.w as i32, r.h as i32);
    if w <= 0 || ht <= 0 {
        return Err(Error::Unsupported("empty window"));
    }
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, w, ht);
        let old = SelectObject(mem, bmp.into());
        // PW_RENDERFULLCONTENT (2) captures DWM/Chromium content even when occluded.
        let ok = PrintWindow(h, mem, PRINT_WINDOW_FLAGS(2)).as_bool();
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -ht,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let mut px = vec![0u8; (w * ht * 4) as usize];
        let rows = GetDIBits(
            mem,
            bmp,
            0,
            ht as u32,
            Some(px.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        );
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        if !ok || rows == 0 {
            return Err(Error::Unsupported("PrintWindow failed"));
        }
        for p in px.as_chunks_mut::<4>().0 {
            p.swap(0, 2); // BGRA -> RGBA
            p[3] = 255;
        }
        let png = crate::png::encode_rgba(w as u32, ht as u32, &px)
            .ok_or(Error::Unsupported("png encode"))?;
        std::fs::write(path, png).map_err(|_| Error::Platform(0, "write png"))
    }
}

// ------------------------------------------------------------------ clipboard

struct Clip;
impl Clip {
    fn open() -> Option<Clip> {
        for _ in 0..20 {
            if unsafe { OpenClipboard(None) }.is_ok() {
                return Some(Clip);
            }
            sleep(10);
        }
        None
    }
}
impl Drop for Clip {
    fn drop(&mut self) {
        let _ = unsafe { CloseClipboard() };
    }
}

pub fn clip_change() -> u64 {
    u64::from(unsafe { GetClipboardSequenceNumber() })
}

fn read_unicode() -> Option<String> {
    unsafe {
        let h = GetClipboardData(u32::from(CF_UNICODETEXT.0)).ok()?;
        let g = HGLOBAL(h.0);
        let p = GlobalLock(g) as *const u16;
        if p.is_null() {
            return None;
        }
        let max = GlobalSize(g) / 2;
        let mut n = 0;
        while n < max && *p.add(n) != 0 {
            n += 1;
        }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
        let _ = GlobalUnlock(g);
        Some(s)
    }
}

pub fn clip_snapshot() -> Option<ClipSnapshot> {
    let _c = Clip::open()?;
    let change = clip_change();
    let mut formats = 0usize;
    let mut lossless = true;
    let mut f = 0u32;
    loop {
        f = unsafe { EnumClipboardFormats(f) };
        if f == 0 {
            break;
        }
        formats += 1;
        lossless &= [CF_TEXT.0, CF_OEMTEXT.0, CF_UNICODETEXT.0, CF_LOCALE.0]
            .iter()
            .any(|k| u32::from(*k) == f);
    }
    Some(ClipSnapshot {
        text: read_unicode(),
        lossless,
        change,
        formats,
    })
}

fn write_unicode(text: &str) -> bool {
    unsafe {
        let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let Ok(g) = GlobalAlloc(GMEM_MOVEABLE, units.len() * 2) else {
            return false;
        };
        let p = GlobalLock(g) as *mut u16;
        if p.is_null() {
            return false;
        }
        std::ptr::copy_nonoverlapping(units.as_ptr(), p, units.len());
        let _ = GlobalUnlock(g);
        if SetClipboardData(u32::from(CF_UNICODETEXT.0), Some(HANDLE(g.0))).is_err() {
            return false;
        }
        true
    }
}

pub fn clip_write(text: &str) -> Option<u64> {
    {
        let _c = Clip::open()?;
        unsafe { EmptyClipboard() }.ok()?;
        if !write_unicode(text) {
            return None;
        }
    }
    Some(clip_change())
}

pub fn clip_restore(snap: &ClipSnapshot, expected: u64) -> bool {
    let Some(_c) = Clip::open() else { return false };
    if clip_change() != expected {
        return false;
    }
    if unsafe { EmptyClipboard() }.is_err() {
        return false;
    }
    match &snap.text {
        Some(t) => write_unicode(t),
        None => true,
    }
}

// ------------------------------------------------------------------ lifecycle

/// A process this engine started, inside a kill-on-close job object. Dropping it
/// closes the job, which kills the process and every helper it spawned, so a
/// cancelled or crashed run can never leave children behind. Processes that were
/// already running (the user's own apps) are never in a job and never owned.
pub struct Owned {
    pub pid: i32,
    process: isize,
    job: isize,
}

impl Drop for Owned {
    fn drop(&mut self) {
        unsafe {
            // KILL_ON_JOB_CLOSE does the kill when the last handle goes; terminate first so
            // a handle inherited elsewhere cannot keep the job (and its children) alive.
            let _ = TerminateJobObject(HANDLE(self.job as *mut _), 1);
            let _ = CloseHandle(HANDLE(self.job as *mut _));
            let _ = CloseHandle(HANDLE(self.process as *mut _));
        }
    }
}

/// Resolve a launch spec to `(absolute exe path, app id)` WITHOUT running anything,
/// so admission can decide before the first effect. A bare name is resolved with
/// `SearchPathW` (PATH + system dirs) once; the resolved path is what gets launched,
/// so what was admitted is exactly what runs.
pub fn resolve_launch(spec: &str) -> Result<(String, String)> {
    let path = if std::path::Path::new(spec).is_absolute() {
        spec.to_string()
    } else {
        if spec.contains(['\\', '/']) {
            return Err(Error::Invalid(
                "launch spec must be an absolute path or a bare executable name".into(),
            ));
        }
        let name: Vec<u16> = spec.encode_utf16().chain(std::iter::once(0)).collect();
        let ext: Vec<u16> = ".exe".encode_utf16().chain(std::iter::once(0)).collect();
        let mut buf = vec![0u16; 1024];
        let n = unsafe {
            SearchPathW(
                PCWSTR::null(),
                PCWSTR(name.as_ptr()),
                PCWSTR(ext.as_ptr()),
                Some(&mut buf),
                None,
            )
        } as usize;
        if n == 0 || n >= buf.len() {
            return Err(Error::NotFound(format!("executable {spec}")));
        }
        String::from_utf16_lossy(&buf[..n])
    };
    let p = std::path::Path::new(&path);
    if !p.is_file() || !p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) {
        return Err(Error::NotFound(format!("executable {path}")));
    }
    let id = p
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    Ok((path, id))
}

/// Start an already-admitted executable without activating it, inside a
/// kill-on-close job object. `max_processes` (0 = unlimited) caps the job's live
/// processes: 1 is the zero-child profile, 2 allows one helper. The process is
/// created suspended and resumed only after it is in the job, so it cannot spawn
/// an unowned child first. Any failure tears the job down (nothing survives).
pub fn launch(path: &str, app_id: &str, max_processes: u32) -> Result<Owned> {
    let before: std::collections::HashSet<i32> = apps().into_iter().map(|a| a.pid).collect();
    unsafe {
        let job = CreateJobObjectW(None, PCWSTR::null())
            .map_err(|e| Error::NotFound(format!("job object: {e}")))?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if max_processes > 0 {
            info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
            info.BasicLimitInformation.ActiveProcessLimit = max_processes;
        }
        if let Err(e) = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) {
            let _ = CloseHandle(job);
            return Err(Error::NotFound(format!("job limits: {e}")));
        }
        let app: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let mut cmd: Vec<u16> = format!("\"{path}\"")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut si: STARTUPINFOW = std::mem::zeroed();
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        si.dwFlags = STARTF_USESHOWWINDOW;
        si.wShowWindow = SW_SHOWNOACTIVATE.0 as u16;
        let mut pi = PROCESS_INFORMATION::default();
        if let Err(e) = CreateProcessW(
            PCWSTR(app.as_ptr()),
            Some(PWSTR(cmd.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_SUSPENDED,
            None,
            PCWSTR::null(),
            &si,
            &mut pi,
        ) {
            let _ = CloseHandle(job);
            return Err(Error::NotFound(format!("could not start {path}: {e}")));
        }
        if let Err(e) = AssignProcessToJobObject(job, pi.hProcess) {
            let _ = TerminateProcess(pi.hProcess, 1);
            let _ = CloseHandle(pi.hThread);
            let _ = CloseHandle(pi.hProcess);
            let _ = CloseHandle(job);
            return Err(Error::NotFound(format!("could not own {path}: {e}")));
        }
        ResumeThread(pi.hThread);
        let _ = CloseHandle(pi.hThread);
        let started_pid = pi.dwProcessId as i32;
        // From here `owned` tears everything down on any early return.
        let owned = Owned {
            pid: started_pid,
            process: pi.hProcess.0 as isize,
            job: job.0 as isize,
        };
        for _ in 0..100 {
            sleep(50);
            let now = apps();
            // The process we started, or a NEW process of the same executable that is
            // inside our job (a launcher that hands off to a helper). A matching window
            // owned by anything outside the job is someone else's app and is never adopted.
            let hit = now.iter().find(|a| {
                a.pid == started_pid
                    || (!before.contains(&a.pid) && a.app_id == app_id && in_job(a.pid, job))
            });
            if let Some(a) = hit {
                // Re-key to the window-owning pid (it may be a helper in our job); keep the handles.
                let o = std::mem::ManuallyDrop::new(owned);
                return Ok(Owned {
                    pid: a.pid,
                    process: o.process,
                    job: o.job,
                });
            }
            if !process_alive(started_pid) && !job_has_processes(job) {
                break;
            }
        }
        Err(Error::NotFound(format!(
            "no window appeared for {path}; the launched process tree was terminated"
        )))
    }
}

fn in_job(pid: i32, job: HANDLE) -> bool {
    unsafe {
        match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid as u32) {
            Ok(h) => {
                let mut r = BOOL(0);
                let ok = IsProcessInJob(h, Some(job), &mut r).is_ok() && r.as_bool();
                let _ = CloseHandle(h);
                ok
            }
            Err(_) => false,
        }
    }
}

fn job_has_processes(job: HANDLE) -> bool {
    let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
    unsafe {
        QueryInformationJobObject(
            Some(job),
            JobObjectBasicAccountingInformation,
            &mut info as *mut _ as *mut std::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
            None,
        )
        .is_ok()
            && info.ActiveProcesses > 0
    }
}

/// Politely close an OWNED process (WM_CLOSE to its windows) and wait for it to exit.
pub fn terminate(pid: i32) -> Result<()> {
    for w in list_windows(false).into_iter().filter(|w| w.pid == pid) {
        let _ = post_msg(hwnd(w.window_id as isize), WM_CLOSE, 0, LPARAM(0));
    }
    for _ in 0..40 {
        sleep(50);
        if !process_alive(pid) {
            return Ok(());
        }
    }
    // Never force-kill: an unsaved-changes prompt must be answered, not bypassed.
    // (Dropping the `Owned` handle is the explicit, caller-chosen hard stop.)
    Err(Error::Platform(
        0,
        "process did not exit after WM_CLOSE (a dialog may be waiting)",
    ))
}

pub fn process_alive(pid: i32) -> bool {
    unsafe {
        match OpenProcess(PROCESS_SYNCHRONIZE, false, pid as u32) {
            Ok(h) => {
                let alive = WaitForSingleObject(h, 0) == WAIT_TIMEOUT;
                let _ = CloseHandle(h);
                alive
            }
            Err(_) => false,
        }
    }
}

/// Move and resize a top-level window to `r` (physical pixels, the space [`window_rect`] reports)
/// without changing its z-order or activating it.
pub fn set_window_frame(h: isize, r: Rect) -> Result<()> {
    unsafe {
        SetWindowPos(
            hwnd(h),
            None,
            r.x.round() as i32,
            r.y.round() as i32,
            r.w.round().max(1.0) as i32,
            r.h.round().max(1.0) as i32,
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
    }
    .map_err(|e| Error::Platform(i64::from(e.code().0), "SetWindowPos"))
}
