//! **Spot takeover, the owner's side** . When an agent hands one control to
//! the owner (`screen.act` with `{type: "handover"}`, approved on the notch's approval card like
//! any other act), the engine holds the takeover and this overlay shows it: only that control's
//! region stays clear, the rest of the virtual desktop is dimmed, and a small card beside the
//! region says what the agent is about to fill and where (the control's label, the app and the
//! window), how long the handover lasts, and offers Done and a clear Stop. The agent's call waits
//! in the engine until one is pressed (or its time limit ends), so the owner is asked to do nothing
//! but the step itself. The overlay follows the target window when it moves, is released the moment
//! the engine ends the takeover, is taken down when the target disappears, and there is only ever
//! one (this thread shows one at a time).
//!
//! The notch only draws and relays. One thread polls the owner-only `screen.spot.current` (every 2
//! s idle, every second while a takeover is up, the live-activity ceiling of #448) while the always-on agent is running and answers with
//! `screen.spot.end`. Four dim popups frame the region instead of one window with a hole, so a click
//! inside the region reaches the app underneath and a click outside it reaches nothing; every popup
//! is no-activate, so the app being used keeps focus. No pixel of the region is read: the engine
//! refuses every screen tool while the takeover is up (`user_in_control`). The region arrives in
//! physical pixels of the virtual desktop (the crate's UI Automation frames; the notch is per-monitor
//! DPI aware, as `shot.rs` relies on).
//!
//! Region source, in order: the engine's `screen.spot.current` region, else the target app's focused control, else its front
//! window (bounds only, never pixels, the application's own windows skipped), else the whole primary display.
//! Only the first two dim the rest of the desktop; a card toggle ("Show full screen" / "Show only
//! this part") removes the dim until it is switched back or the takeover ends.
//!
//! This mirrors `mac/Notch/Sources/Agent/SpotTakeover.swift`. Not yet compiled or run on the Dell.

use crate::host;
use rightkit_win_ui::raii;
use serde_json::json;
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Foundation::{CloseHandle, RECT};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BLACK_BRUSH, DEFAULT_GUI_FONT, GetStockObject, HBRUSH, WHITE_BRUSH,
};
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetSystemMetrics, HMENU,
    IDC_ARROW, LWA_ALPHA, LoadCursorW, MSG, PM_REMOVE, PeekMessageW, RegisterClassW,
    SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    SW_SHOWNOACTIVATE, SendMessageW, SetLayeredWindowAttributes, SetWindowTextW, ShowWindow,
    TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_COMMAND, WM_SETFONT, WNDCLASSW, WS_CHILD,
    WS_EX_TOOLWINDOW, WS_VISIBLE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetWindowLongPtrW, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, SM_CXSCREEN, SM_CYSCREEN,
};
use windows::core::{BOOL as WinBool, PCWSTR, PWSTR, w};

const METHOD_CURRENT: &str = "screen.spot.current";
const METHOD_END: &str = "screen.spot.end";
const IDLE_EVERY: Duration = Duration::from_secs(2);
const SHOWN_EVERY: Duration = Duration::from_secs(1);
/// Clear space kept around the control, in pixels.
const MARGIN: i32 = 8;
/// Dim level of the covered desktop (0 clear to 255 black).
const DIM_ALPHA: u8 = 220;
const CARD_W: i32 = 320;
const CARD_H: i32 = 134;
const ID_DONE: usize = 1;
const ID_CANCEL: usize = 2;
const ID_TOGGLE: usize = 3;

static STARTED: AtomicBool = AtomicBool::new(false);
/// The owner's click on the card: 0 none, 1 Done, 2 Cancel.
static ANSWER: AtomicU8 = AtomicU8::new(0);
/// The owner chose "Show full screen": nothing is dimmed until they switch back or the takeover ends.
static SHOW_ALL: AtomicBool = AtomicBool::new(false);

/// A pixel rectangle on the virtual desktop.
#[derive(Clone, Copy, PartialEq)]
struct Px {
    l: i32,
    t: i32,
    r: i32,
    b: i32,
}

impl Px {
    fn w(&self) -> i32 {
        self.r - self.l
    }

    fn h(&self) -> i32 {
        self.b - self.t
    }

    fn solid(&self) -> bool {
        self.w() >= 1 && self.h() >= 1
    }

    fn grow(&self, by: i32) -> Px {
        Px {
            l: self.l - by,
            t: self.t - by,
            r: self.r + by,
            b: self.b + by,
        }
    }

    fn within(&self, outer: Px) -> Px {
        Px {
            l: self.l.max(outer.l),
            t: self.t.max(outer.t),
            r: self.r.min(outer.r),
            b: self.b.min(outer.b),
        }
    }
}

struct Spot {
    id: String,
    label: String,
    app: String,
    /// The target window's title, empty when the engine names none.
    window: String,
    rect: Px,
    /// Seconds the handover has left, when the engine says.
    seconds_left: Option<u64>,
    /// Where `rect` came from: the engine's region, else the app's focused control, else its front window, else the display.
    source: Source,
}

#[derive(Clone, Copy, PartialEq)]
enum Source {
    Region,
    FocusedElement,
    FrontWindow,
    FullScreen,
}

/// How one `show` ended.
enum Ended {
    /// The owner pressed Done or Stop here.
    Answered,
    /// The target window moved or resized: draw it again at its new place.
    Moved,
    /// Released by the engine, the target is gone, or the agent switched off.
    Gone,
}

/// Starts the poll thread once. It runs for the life of the notch.
pub fn start() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("rightkit-spot-takeover".into())
        .spawn(run);
    if let Err(error) = spawned {
        host::log_info(
            "spot_takeover_thread_failed",
            &[("reason", error.to_string().as_str())],
        );
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn run() {
    let Ok(module) = (unsafe { GetModuleHandleW(None) }) else {
        return;
    };
    let instance = HINSTANCE(module.0);
    if !register(instance) {
        return;
    }
    // The takeover the owner last answered: not shown again until the engine has caught up.
    let mut answered: Option<String> = None;
    loop {
        if host::engine_running() {
            match current() {
                Ok(Some(spot)) if answered.as_deref() != Some(spot.id.as_str()) => {
                    match show(&spot, instance) {
                        Ended::Answered => {
                            SHOW_ALL.store(false, Ordering::SeqCst);
                            answered = Some(spot.id);
                        }
                        Ended::Gone => SHOW_ALL.store(false, Ordering::SeqCst),
                        Ended::Moved => {}
                    }
                    continue;
                }
                Ok(None) => {
                    answered = None;
                    SHOW_ALL.store(false, Ordering::SeqCst);
                }
                _ => {}
            }
        }
        std::thread::sleep(IDLE_EVERY);
    }
}

fn register(instance: HINSTANCE) -> bool {
    let cursor = unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default();
    let classes = [
        (w!("RightKitSpotShade"), unsafe {
            GetStockObject(BLACK_BRUSH)
        }),
        (w!("RightKitSpotCard"), unsafe {
            GetStockObject(WHITE_BRUSH)
        }),
    ];
    for (name, brush) in classes {
        let class = WNDCLASSW {
            hInstance: instance,
            lpszClassName: name,
            lpfnWndProc: Some(proc),
            hCursor: cursor,
            hbrBackground: HBRUSH(brush.0),
            ..Default::default()
        };
        if unsafe { RegisterClassW(&class) } == 0 {
            host::log_error(
                "RegisterClassW",
                &windows::core::Error::from_thread(),
                "spot_takeover",
            );
            return false;
        }
    }
    true
}

/// The takeover the engine holds now, `Ok(None)` when there is none, `Err` when it did not answer
/// (the engine starting or busy is normal: the caller keeps what it has).
fn current() -> Result<Option<Spot>, String> {
    let reply = host::engine_poll(METHOD_CURRENT, json!({}))?;
    if reply["active"].as_bool() != Some(true) {
        return Ok(None);
    }
    let Some(id) = reply["id"].as_str() else {
        return Ok(None);
    };
    // The region is `x, y, w, h` on the object, or a `region` object or `[x, y, w, h]` array.
    let region = if reply["region"].is_array() {
        let at = |n: usize| reply["region"][n].as_f64().unwrap_or(0.0);
        [at(0), at(1), at(2), at(3)]
    } else {
        let box_ = if reply["region"].is_object() {
            &reply["region"]
        } else {
            &reply
        };
        let number = |key: &str| box_[key].as_f64().unwrap_or(0.0);
        [number("x"), number("y"), number("w"), number("h")]
    };
    let mut rect = Px {
        l: region[0].floor() as i32,
        t: region[1].floor() as i32,
        r: (region[0] + region[2]).ceil() as i32,
        b: (region[1] + region[3]).ceil() as i32,
    };
    let mut source = Source::Region;
    let target = &reply["targetWindow"];
    let mut app = reply["app"].as_str().unwrap_or("").to_string();
    let window = target["window"]
        .as_str()
        .or_else(|| target["title"].as_str())
        .or_else(|| target.as_str())
        .unwrap_or("")
        .to_string();
    if app.is_empty() {
        app = target["app"].as_str().unwrap_or("").to_string();
    }
    if !rect.solid() {
        // No region from the agent: the app's front window, else the whole primary display.
        if let Some((front, hwnd)) = front_window_rect(&app) {
            // The focused control of that window when it sits inside it and is not nearly all of
            // it; else the window. Rectangles only, never pixels.
            if let Some(control) = focused_control_rect(hwnd, front) {
                rect = control;
                source = Source::FocusedElement;
            } else {
                rect = front;
                source = Source::FrontWindow;
            }
        } else {
            rect = Px {
                l: 0,
                t: 0,
                r: unsafe { GetSystemMetrics(SM_CXSCREEN) },
                b: unsafe { GetSystemMetrics(SM_CYSCREEN) },
            };
            source = Source::FullScreen;
            if !rect.solid() {
                return Ok(None);
            }
        }
    }
    let seconds_left = reply["secondsLeft"].as_u64().or_else(|| {
        let expires = reply["expires"].as_u64()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs();
        Some(expires.saturating_sub(now))
    });
    Ok(Some(Spot {
        id: id.to_string(),
        label: reply["label"].as_str().unwrap_or("").to_string(),
        app,
        window,
        rect,
        seconds_left,
        source,
    }))
}

struct Front<'a> {
    /// The wanted app, lowercase, without `.exe`; empty matches any app.
    app: &'a str,
    own: u32,
    found: Option<(Px, HWND)>,
    /// The topmost window whose name only starts with the wanted app (an exact match wins).
    prefix: Option<(Px, HWND)>,
}

/// The full executable path of process `pid`, empty when it cannot be read.
fn process_path(pid: u32) -> String {
    let Ok(process) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
    else {
        return String::new();
    };
    let mut buffer = [0u16; 1024];
    let mut size = buffer.len() as u32;
    let named = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut size,
        )
    }
    .is_ok();
    let _ = unsafe { CloseHandle(process) };
    if !named {
        return String::new();
    }
    String::from_utf16_lossy(&buffer[..size as usize])
}

/// One string of the executable's version resource (`FileDescription`, `ProductName`), `None` when
/// it has none.
fn version_strings(path: &str, keys: &[&str]) -> Vec<String> {
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is NUL-terminated and outlives the calls.
    let size = unsafe { GetFileVersionInfoSizeW(PCWSTR(wide.as_ptr()), None) } as usize;
    if size == 0 || size > 1 << 20 {
        return Vec::new();
    }
    // u64 cells keep the block aligned.
    let mut block = vec![0u64; size.div_ceil(8)];
    if unsafe {
        GetFileVersionInfoW(
            PCWSTR(wide.as_ptr()),
            None,
            size as u32,
            block.as_mut_ptr().cast(),
        )
    }
    .is_err()
    {
        return Vec::new();
    }
    let query = |sub_block: &str| -> Option<(*mut c_void, u32)> {
        let name: Vec<u16> = sub_block.encode_utf16().chain(std::iter::once(0)).collect();
        let mut found: *mut c_void = std::ptr::null_mut();
        let mut length = 0u32;
        // SAFETY: `block` is the version information just read; the answer points into it and
        // `block` outlives every use below.
        let ok = unsafe {
            VerQueryValueW(
                block.as_ptr().cast(),
                PCWSTR(name.as_ptr()),
                &mut found,
                &mut length,
            )
        }
        .as_bool();
        (ok && !found.is_null()).then_some((found, length))
    };
    let mut tables = vec!["040904b0".to_string()];
    if let Some((pair, bytes)) = query("\\VarFileInfo\\Translation")
        && bytes >= 4
    {
        // SAFETY: one (language, code page) pair of u16s lies at `pair`.
        let (language, code_page) = unsafe {
            let at = pair.cast::<u16>();
            (at.read_unaligned(), at.add(1).read_unaligned())
        };
        tables.insert(0, format!("{language:04x}{code_page:04x}"));
    }
    let mut out = Vec::new();
    for key in keys {
        for table in &tables {
            let Some((text, units)) = query(&format!("\\StringFileInfo\\{table}\\{key}")) else {
                continue;
            };
            // SAFETY: a string value is `units` UTF-16 units at `text`, inside `block`.
            let slice = unsafe { std::slice::from_raw_parts(text.cast::<u16>(), units as usize) };
            let value = String::from_utf16_lossy(slice)
                .trim_end_matches('\0')
                .trim()
                .to_lowercase();
            if !value.is_empty() {
                out.push(value);
                break;
            }
        }
    }
    out
}

/// Every name process `pid` answers to, lowercase: its executable stem, its version resource's file
/// description and its product name. The resource reads are cached per executable path.
fn process_names(pid: u32) -> Vec<String> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, Vec<String>>>> = OnceLock::new();
    let path = process_path(pid);
    if path.is_empty() {
        return Vec::new();
    }
    let key = path.to_lowercase();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = cache.lock().ok().and_then(|map| map.get(&key).cloned()) {
        return hit;
    }
    let file = key.rsplit('\\').next().unwrap_or("");
    let mut names = vec![file.strip_suffix(".exe").unwrap_or(file).to_string()];
    names.extend(version_strings(&path, &["FileDescription", "ProductName"]));
    if let Ok(mut map) = cache.lock() {
        map.insert(key, names.clone());
    }
    names
}

unsafe extern "system" fn front_window(hwnd: HWND, data: LPARAM) -> WinBool {
    // EnumWindows passes live handles; `data` is the caller's stack `Front`.
    let state = unsafe { &mut *(data.0 as *mut Front) };
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() || unsafe { IsIconic(hwnd) }.as_bool() {
        return WinBool(1);
    }
    if (unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32) & WS_EX_TOOLWINDOW.0 != 0 {
        return WinBool(1);
    }
    let mut cloaked = 0u32;
    let _ = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut c_void,
            size_of::<u32>() as u32,
        )
    };
    if cloaked != 0 {
        return WinBool(1);
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == state.own {
        return WinBool(1);
    }
    let mut frame = RECT::default();
    let framed = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut frame as *mut RECT as *mut c_void,
            size_of::<RECT>() as u32,
        )
    }
    .is_ok();
    if !framed
        && unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut frame) }
            .is_err()
    {
        return WinBool(1);
    }
    let rect = Px {
        l: frame.left,
        t: frame.top,
        r: frame.right,
        b: frame.bottom,
    };
    if rect.w() < 40 || rect.h() < 40 {
        return WinBool(1);
    }
    if !state.app.is_empty() {
        let names = process_names(pid);
        if !names.iter().any(|name| name == state.app) {
            if state.prefix.is_none() && names.iter().any(|name| name.starts_with(state.app)) {
                state.prefix = Some((rect, hwnd));
            }
            return WinBool(1);
        }
    }
    state.found = Some((rect, hwnd));
    WinBool(0)
}

/// The topmost normal visible window of `app` (any app when the name is empty), skipping the application's own
/// windows. Reads window bounds only, never pixels.
fn front_window_rect(app: &str) -> Option<(Px, HWND)> {
    let wanted = app.trim().to_lowercase();
    let mut state = Front {
        app: wanted.strip_suffix(".exe").unwrap_or(&wanted),
        own: unsafe { GetCurrentProcessId() },
        found: None,
        prefix: None,
    };
    // EnumWindows walks top to bottom; it reports an error when the callback stops it early.
    let _ = unsafe {
        EnumWindows(
            Some(front_window),
            LPARAM(&mut state as *mut Front as isize),
        )
    };
    state.found.or(state.prefix)
}

fn end(id: &str, outcome: &str) {
    if let Err(failure) = host::engine_call(METHOD_END, json!({"id": id, "outcome": outcome})) {
        host::log_info("spot_takeover_end_failed", &[("reason", failure.as_str())]);
    }
}

/// Destroys what was created, on every path out of `show`.
struct Windows(Vec<HWND>);

impl Drop for Windows {
    fn drop(&mut self) {
        for hwnd in self.0.drain(..) {
            let _ = unsafe { DestroyWindow(hwnd) };
        }
    }
}

/// A no-activate popup. The dim bands are `layered` (they take `DIM_ALPHA`); the card is not: it
/// is never given an alpha, and a layered window without one shows nothing.
fn popup(class: PCWSTR, area: Px, instance: HINSTANCE, layered: bool) -> Option<HWND> {
    let rect = (area.l, area.t, area.w(), area.h());
    let style = rightkit_overlay::OverlayStyle::PASSIVE;
    let created = if layered {
        raii::create_overlay(class, &host::window_title(), rect, style, instance)
    } else {
        raii::create_overlay_unlayered(class, &host::window_title(), rect, style, instance)
    };
    match created {
        Ok(hwnd) => Some(hwnd),
        Err(error) => {
            host::log_error("CreateWindowExW", &error, "spot_takeover");
            None
        }
    }
}

fn child(
    parent: HWND,
    class: PCWSTR,
    text: &str,
    area: Px,
    id: usize,
    instance: HINSTANCE,
) -> Option<HWND> {
    let text = wide(text);
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            PCWSTR(text.as_ptr()),
            WS_CHILD | WS_VISIBLE | WINDOW_STYLE(0),
            area.l,
            area.t,
            area.w(),
            area.h(),
            Some(parent),
            Some(HMENU(id as *mut c_void)),
            Some(instance),
            None,
        )
    }
    .ok()
}

/// The line under the label: how long the agent waits.
fn waiting_line(seconds_left: Option<u64>) -> String {
    match seconds_left {
        Some(seconds) => host::tf(
            "The agent is waiting. {} s left, then it stops on its own.",
            &[&seconds.to_string()],
        ),
        None => host::t("The agent is waiting for you.").to_string(),
    }
}

/// Shows one takeover until it is answered here, moves, is gone, replaced or the agent switched
/// off. All windows belong to this thread, which pumps their messages. The windows are no-activate
/// popups, so the owner's keystrokes reach only the app under the clear region.
/// The rectangle of the focused control inside `top` (virtual-desktop pixels, the same space as
/// the window rectangles and the popups, so a second monitor's origin and scale need no
/// correction). Nil when focus is elsewhere, the control is tiny, lies outside the window or covers
/// nearly all of it.
fn focused_control_rect(top: HWND, window: Px) -> Option<Px> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GA_ROOT, GUITHREADINFO, GetAncestor, GetGUIThreadInfo, GetWindowRect,
    };
    let mut pid = 0u32;
    let thread = unsafe { GetWindowThreadProcessId(top, Some(&mut pid)) };
    let mut info = GUITHREADINFO {
        cbSize: size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetGUIThreadInfo(thread, &mut info) }.ok()?;
    let focus = info.hwndFocus;
    if focus.0.is_null() || unsafe { GetAncestor(focus, GA_ROOT) } != top {
        return None;
    }
    let mut frame = RECT::default();
    unsafe { GetWindowRect(focus, &mut frame) }.ok()?;
    let rect = Px {
        l: frame.left,
        t: frame.top,
        r: frame.right,
        b: frame.bottom,
    }
    .within(window);
    if rect.w() < 8 || rect.h() < 8 {
        return None;
    }
    let area = |p: &Px| p.w() as i64 * p.h() as i64;
    (area(&rect) * 10 <= area(&window) * 9).then_some(rect)
}

fn show(spot: &Spot, instance: HINSTANCE) -> Ended {
    let all = Px {
        l: unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) },
        t: unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) },
        r: 0,
        b: 0,
    };
    let all = Px {
        r: all.l + unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) },
        b: all.t + unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) },
        ..all
    };
    let hole = spot.rect.grow(MARGIN).within(all);
    if !hole.solid() || !all.solid() {
        return Ended::Gone;
    }
    ANSWER.store(0, Ordering::SeqCst);

    let mut owned = Windows(Vec::new());
    // The covered desktop: a band above, a band below, and the two sides between.
    let showing_all = SHOW_ALL.load(Ordering::SeqCst);
    let dimming = !showing_all && spot.source != Source::FullScreen;
    let bands = [
        Px {
            l: all.l,
            t: all.t,
            r: all.r,
            b: hole.t,
        },
        Px {
            l: all.l,
            t: hole.b,
            r: all.r,
            b: all.b,
        },
        Px {
            l: all.l,
            t: hole.t,
            r: hole.l,
            b: hole.b,
        },
        Px {
            l: hole.r,
            t: hole.t,
            r: all.r,
            b: hole.b,
        },
    ];
    for band in bands.into_iter().filter(|band| dimming && band.solid()) {
        let Some(hwnd) = popup(w!("RightKitSpotShade"), band, instance, true) else {
            return Ended::Gone;
        };
        owned.0.push(hwnd);
        let _ = unsafe { SetLayeredWindowAttributes(hwnd, COLORREF(0), DIM_ALPHA, LWA_ALPHA) };
    }

    // The card: below the region when there is room, else above it, kept on the desktop.
    let mut top = hole.b + 10;
    if top + CARD_H > all.b {
        top = hole.t - CARD_H - 10;
        if top < all.t {
            top = (all.b - CARD_H - 10).max(all.t);
        }
    }
    let left = (hole.r - CARD_W).clamp(all.l, (all.r - CARD_W).max(all.l));
    let card_area = Px {
        l: left,
        t: top,
        r: left + CARD_W,
        b: top + CARD_H,
    };
    let Some(card) = popup(w!("RightKitSpotCard"), card_area, instance, false) else {
        return Ended::Gone;
    };
    owned.0.push(card);
    // What the agent is about to fill, and where. The agent never sees this region.
    let place = match (spot.app.is_empty(), spot.window.is_empty()) {
        (true, _) => String::new(),
        (false, true) => spot.app.clone(),
        (false, false) => format!("{}, {}", spot.app, spot.window),
    };
    let text = if place.is_empty() {
        host::tf("Your turn: {}", &[spot.label.as_str()])
    } else {
        host::tf(
            "Your turn in {}: {}",
            &[place.as_str(), spot.label.as_str()],
        )
    };
    let font = unsafe { GetStockObject(DEFAULT_GUI_FONT) };
    let note = child(
        card,
        w!("STATIC"),
        &waiting_line(spot.seconds_left),
        Px {
            l: 14,
            t: 38,
            r: CARD_W - 14,
            b: 60,
        },
        0,
        instance,
    );
    let toggle = if spot.source == Source::FullScreen {
        None
    } else {
        child(
            card,
            w!("BUTTON"),
            if showing_all {
                host::t("Show only this part")
            } else {
                host::t("Show full screen")
            },
            Px {
                l: 14,
                t: 62,
                r: CARD_W - 14,
                b: 86,
            },
            ID_TOGGLE,
            instance,
        )
    };
    let controls = [
        note,
        toggle,
        child(
            card,
            w!("STATIC"),
            &text,
            Px {
                l: 14,
                t: 12,
                r: CARD_W - 14,
                b: 36,
            },
            0,
            instance,
        ),
        child(
            card,
            w!("BUTTON"),
            host::t("Stop"),
            Px {
                l: CARD_W - 14 - 100 - 8 - 100,
                t: 96,
                r: CARD_W - 14 - 100 - 8,
                b: 124,
            },
            ID_CANCEL,
            instance,
        ),
        child(
            card,
            w!("BUTTON"),
            host::t("Done"),
            Px {
                l: CARD_W - 14 - 100,
                t: 96,
                r: CARD_W - 14,
                b: 124,
            },
            ID_DONE,
            instance,
        ),
    ];
    for control in controls.into_iter().flatten() {
        unsafe {
            SendMessageW(
                control,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            );
        }
    }
    for hwnd in &owned.0 {
        let _ = unsafe { ShowWindow(*hwnd, SW_SHOWNOACTIVATE) };
    }

    let mut next_poll = Instant::now() + SHOWN_EVERY;
    loop {
        let mut message = MSG::default();
        while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        match ANSWER.load(Ordering::SeqCst) {
            1 => {
                end(&spot.id, "done");
                return Ended::Answered;
            }
            2 => {
                end(&spot.id, "cancelled");
                return Ended::Answered;
            }
            3 => {
                // Switch between the clear region and the whole screen; the takeover is untouched.
                ANSWER.store(0, Ordering::SeqCst);
                SHOW_ALL.fetch_xor(true, Ordering::SeqCst);
                return Ended::Moved;
            }
            _ => {}
        }
        if Instant::now() >= next_poll {
            next_poll = Instant::now() + SHOWN_EVERY;
            if !host::engine_running() {
                end(&spot.id, "cancelled");
                return Ended::Gone;
            }
            match current() {
                Ok(Some(now)) if now.id == spot.id => {
                    if now.rect != spot.rect {
                        return Ended::Moved;
                    }
                    if let Some(note) = note {
                        let line = wide(&waiting_line(now.seconds_left));
                        let _ = unsafe { SetWindowTextW(note, PCWSTR(line.as_ptr())) };
                    }
                }
                // Gone (the agent's time ran out) or replaced by another takeover.
                Ok(_) => return Ended::Gone,
                // The engine did not answer; keep the overlay up and ask again.
                Err(_) => {}
            }
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_COMMAND {
        match wparam.0 & 0xFFFF {
            ID_DONE => ANSWER.store(1, Ordering::SeqCst),
            ID_CANCEL => ANSWER.store(2, Ordering::SeqCst),
            ID_TOGGLE => ANSWER.store(3, Ordering::SeqCst),
            _ => {}
        }
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}
