//! The launcher (the Windows counterpart of the Mac's `Launcher`): Alt+Space opens a search
//! panel over whatever is in front. With nothing typed it lists pinned apps, the apps used
//! most, and the apps that are running; typing matches the Start menu's apps with the Mac's
//! fuzzy score (plus a bonus for use), a typed address or path, and Pulse's own pages; a sum
//! or a unit conversion ("12 km in mi") is answered first, and Return copies the answer.
//!
//! Keys: Up/Down move, Return runs, Esc closes, Alt+1..9 run a row, Alt+P pins or unpins an
//! app, Alt+Q asks the selected running app to close (the Mac's Command keys, on the key the
//! thumb rests on). The panel closes when it loses the foreground.
//!
//! One thread owns it: a layered window drawn with the notch's canvas, shown and hidden, never
//! destroyed. The search field is drawn too (a caret in a line of text), so there is no child
//! control. The hot key (Alt+Space, or Ctrl+Space when chosen) is held only while the
//! `launcherEnabled` switch is on. When another program already holds Alt+Space (ChatGPT's
//! app does by default) the panel asks once which one gets it: "Use Alt+Space for Pulse"
//! (the key layer's hook then takes the chord, so the other program no longer sees it),
//! "Keep it for <program>" (left alone, never retried) or "Choose another key" (the hub's
//! settings). The answer is the setting `launcherHotkeyConflict`; the hub shows the state under the switch
//! (`status`), with Ctrl+Space as the way to give the chord back.
//!
//! Typing is answered from memory: the Start menu's apps and their icons are read by a
//! thread of their own (`spawn_scan`), so a key never waits on the disk or the shell. File
//! names are searched on another thread after a 60 ms pause, and a newer key cancels the
//! older search. The rows run apps, files, then two closing rows holding the typed text:
//! "Ask ChatGPT" and "Search <engine>" (Tab selects them). `launcher_open` and
//! `launcher_query` diag lines carry the timings.
//!
//! Kept in `%LOCALAPPDATA%\Pulse\launcher\`: `usage.json` (how often and how recently each
//! app was run from here) and `pins.json` (the pinned apps, used while the hub's
//! `launcherConfig` lists none). Quicklinks, snippets and pins come from `launcherConfig`
//! (`launcher_config.rs`); Alt+P writes the pins back into it.

use crate::config::{self as launcher_config, Config};
use crate::host::{self, AltChord, LauncherHotkey};
use crate::json::{self, Value};
use crate::{
    calc as launcher_calc, commands as launcher_commands, files as launcher_files,
    notes as launcher_notes, response_layout,
};
use rightkit_hotkeys::windows::GlobalHotkeys;
use rightkit_win_ui::canvas::{Canvas, Mask};
use rightkit_win_ui::raii::{ClassGuard, OwnedWindow, hwnd_from_key, hwnd_key};
use rightkit_win_ui::surface::{self, TextPainter};
use rightkit_win_ui::{fileicon, glyphs};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use windows::Win32::Foundation::{CloseHandle, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::ValidateRect;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{
    GetCurrentProcessId, GetCurrentThreadId, OpenProcess, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PCWSTR, PWSTR, w};

/// Calls the `windows` crate does not need to grow a feature for; each matches the
/// declaration `send_sys.rs` already makes where both declare one.
#[allow(non_snake_case)]
mod ffi {
    use std::ffi::c_void;

    pub type Handle = *mut c_void;

    #[link(name = "user32")]
    unsafe extern "system" {
        pub fn SetFocus(window: Handle) -> Handle;
        pub fn AttachThreadInput(attach: u32, to: u32, on: i32) -> i32;
        pub fn OpenClipboard(owner: Handle) -> i32;
        pub fn CloseClipboard() -> i32;
        pub fn GetClipboardData(format: u32) -> Handle;
    }

    #[link(name = "version")]
    unsafe extern "system" {
        pub fn GetFileVersionInfoSizeW(file: *const u16, handle: *mut u32) -> u32;
        pub fn GetFileVersionInfoW(
            file: *const u16,
            handle: u32,
            length: u32,
            data: *mut c_void,
        ) -> i32;
        pub fn VerQueryValueW(
            block: *const c_void,
            sub_block: *const u16,
            buffer: *mut *mut c_void,
            length: *mut u32,
        ) -> i32;
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

use crate::dictionary;

const PANEL_CLASS: PCWSTR = w!("RightKitLauncher");
const WM_SYNC: u32 = WM_APP + 1;
const WM_TOGGLE: u32 = WM_APP + 2;
/// A command's thread left its outcome in `OUTCOME`.
const WM_OUTPUT: u32 = WM_APP + 3;
/// The exchange rates were fetched and saved; an open panel reads them.
const WM_RATES: u32 = WM_APP + 4;
/// Opens the panel on the clipboard history (the wheel and the Tools card, when the strip is off).
const WM_OPEN_CLIP: u32 = WM_APP + 5;
/// Asks which program gets Alt+Space (another one holds it and nobody has chosen yet).
const WM_PROMPT: u32 = WM_APP + 6;
/// The background scan left apps and running programs in `SCANNED`.
const WM_APPS: u32 = WM_APP + 7;
/// A file search finished and left its rows in `FILE_ROWS`.
const WM_FILE_ROWS: u32 = WM_APP + 8;
/// The icons of the listed apps were loaded; an open panel draws again.
const WM_ICONS: u32 = WM_APP + 9;
/// The rows that read the disk or the clipboard, computed away from this thread.
const WM_SLOW_ROWS: u32 = WM_APP + 10;
/// An AI answer from its thread (`launcher_chat::ask`).
const WM_AI_ANSWER: u32 = WM_APP + 11;
/// A dictionary entry from its thread (`launcher_dictionary::lookup`).
const WM_DEFINE: u32 = WM_APP + 12;
const REFRESH_TIMER: usize = 1;
const PASTE_TIMER: usize = 2;
/// Looks for the file names every little while until their thread has finished.
const FILES_TIMER: usize = 3;
const FILES_MS: u32 = 250;
/// The pause after the last key before the file search starts.
const QUERY_TIMER: usize = 4;
const QUERY_MS: u32 = 60;
/// The Start menu is read again when the list is older than this.
const APPS_FRESH_SECS: u64 = 120;
/// Most app icons held by the launcher itself (the shell cache keeps only a few).
const ICONS_MAX: usize = 400;
/// A desktop ChatGPT link that takes a question: `{query}` is the percent-encoded text. Unknown
/// (the Windows app documents no such link), so questions go to the web address.
const CHATGPT_APP_URL: Option<&str> = None;
/// After the foreground went back to the app, before Ctrl+V is sent to it (the Mac's wait).
const PASTE_MS: u32 = 120;
/// The most quicklink and snippet rows, as on the Mac.
const LINKS_MAX: usize = 4;
const REFRESH_MS: u32 = 600;
const CF_UNICODETEXT: u32 = 13;

const VK_BACK: u32 = 0x08;
const VK_TAB: u32 = 0x09;
const VK_RETURN: u32 = 0x0D;
const VK_ESCAPE: u32 = 0x1B;
const VK_SPACE: u32 = 0x20;
const VK_END: u32 = 0x23;
const VK_HOME: u32 = 0x24;
const VK_LEFT: u32 = 0x25;
const VK_UP: u32 = 0x26;
const VK_RIGHT: u32 = 0x27;
const VK_DOWN: u32 = 0x28;
const VK_DELETE: u32 = 0x2E;
const VK_1: u32 = 0x31;
const VK_9: u32 = 0x39;
const VK_D: u32 = 0x44;
const VK_P: u32 = 0x50;
const VK_Q: u32 = 0x51;
const VK_V: u32 = 0x56;

// The Mac panel's measures, in its points (scaled by the monitor's DPI here).
const WIDTH: f32 = 620.0;
const HEIGHT: f32 = 380.0;
const RADIUS: f32 = 22.0;
const FIELD_HEIGHT: f32 = 56.0;
const FIELD_PAD: f32 = 18.0;
const FIELD_TEXT: f32 = 20.0;
/// The magnifier before the search text, and the indent that leaves room for it.
const FIELD_MAGNIFIER: f32 = 17.0;
const FIELD_INDENT: f32 = 30.0;
const ROW_HEIGHT: f32 = 42.0;
const ROW_INSET: f32 = 8.0;
const ROW_PAD: f32 = 10.0;
const ROW_RADIUS: f32 = 10.0;
const ICON: f32 = 28.0;
const ICON_GAP: f32 = 12.0;
const TITLE_TEXT: f32 = 14.0;
const SUBTITLE_TEXT: f32 = 11.0;
const HEADER_TEXT: f32 = 10.0;
/// A section header's line: the Mac's 10 pt semibold line is 13 points tall.
const HEADER_LINE: f32 = 13.0;
/// Where a header's text box centres below the header's top, so that its baseline sits 10 points
/// down as the Mac's does (measured on the 2x shots: the baseline 20 px below the top).
const HEADER_MID: f32 = 5.5;
/// Where the subtitle's text box centres below the row's middle (the Mac's subtitle baseline is
/// 33.5 points below the row's top; measured on the 2x shots).
const SUBTITLE_MID: f32 = 8.5;
/// How far the shortcut hint sits above the row's middle. Each of the three lifts puts a baseline
/// where the Mac's is on its 2x shot, since the Mac centres a line by its own ascent and descent.
const HINT_LIFT: f32 = 0.5;
/// How far the search text (and its caret) sits above the field's middle.
const FIELD_LIFT: f32 = 1.0;
/// How far the empty-list note sits above the list's middle.
const NOTE_LIFT: f32 = 0.5;
const OUTPUT_TEXT: f32 = 12.0;
const OUTPUT_LINE: f32 = 17.0;
const OUTPUT_PAD: f32 = 10.0;
const HEADER_TOP_FIRST: f32 = 8.0;
const HEADER_TOP: f32 = 12.0;
const HEADER_BOTTOM: f32 = 4.0;
const LIST_BOTTOM: f32 = 8.0;

const INK: u32 = 0xFF_FFFF;
/// The panel: the Mac's still render of its glass is `Color(white: 0.2)` under black at 0.55,
/// 51 * 0.45 = 23 on every channel.
const FILL: u32 = 0x17_1717;
/// The border: the Mac's 1 pt `strokeBorder` of white at 0.12, drawn over the content (51 over
/// the fill, 90 where it crosses the divider).
const EDGE_WIDTH: f32 = 1.0;
const EDGE_ALPHA: f32 = 0.12;
/// The rule under the field: the Mac's `Divider` (white 0.1) with a white 0.1 overlay, 0.19
/// together (67 over the fill). It runs the panel's full width, under the border.
const DIVIDER_ALPHA: f32 = 0.19;

/// The most rows of each kind, as on the Mac.
const RECENT_MAX: usize = 6;
const RUNNING_MAX: usize = 10;
const APPS_MAX: usize = 6;
const NOTES_MAX: usize = 8;
const CHATS_MAX: usize = 20;
/// File rows shown, as on the Mac.
const FILES_MAX: usize = 6;
const PULSE_MAX: usize = 4;
/// Clipboard rows shown for "clip".
const CLIPS_MAX: usize = 8;
/// How deep the Start menu's folders are followed.
const SCAN_DEPTH: usize = 3;
const QUERY_MAX: usize = 200;
const USAGE_KEPT: usize = 300;
const FILE_MAX: usize = 256 * 1024;

static THREAD_ID: AtomicU32 = AtomicU32::new(0);
/// The launcher is on but another program holds the hot key: the key hook takes Alt+Space.
static HOOK_TAKES: AtomicBool = AtomicBool::new(false);

/// What stands in the hot key's way, for the hub.
static STATUS: Mutex<Option<&'static str>> = Mutex::new(None);

/// The question about Alt+Space has been put this run (an unanswered one comes back next run).
static PROMPTED: AtomicBool = AtomicBool::new(false);
/// The answer the held-by-another-app state was last settled for (`answer_code`); 0 for none yet.
static SETTLED: AtomicU8 = AtomicU8::new(0);

fn answer_code(answer: Option<bool>) -> u8 {
    match answer {
        None => 1,
        Some(true) => 2,
        Some(false) => 3,
    }
}
/// The last line written per hot key event, so a state is logged once and not on every pass.
static LAST_STATE: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());
/// When the hot key was pressed (milliseconds on `mono_ms`, plus one), 0 for none: the start
/// of "hotkey to visible".
static HOTKEY_AT: AtomicU64 = AtomicU64::new(0);
static EPOCH: OnceLock<Instant> = OnceLock::new();
/// The newest file search asked for; an older one stops and drops its answer.
static FILE_GEN: AtomicU64 = AtomicU64::new(0);
/// A finished file search: its generation, the text it was for, the rows and whether it finished.
type FileRows = (u64, String, Vec<launcher_files::Found>, bool);
/// The newest finished file search (`FileRows`).
static FILE_ROWS: Mutex<Option<FileRows>> = Mutex::new(None);
/// What the background scan found, taken by the panel on `WM_APPS`.
static SCANNED: Mutex<Option<Scanned>> = Mutex::new(None);
static SCANNING: AtomicBool = AtomicBool::new(false);
/// The newest slow rows, for the generation they were asked for.
static SLOW_ROWS: Mutex<Option<(u64, Slow)>> = Mutex::new(None);
/// The newest AI answer: its run, the question and the answer or why there is none.
static AI_ANSWER: Mutex<Option<AiAnswer>> = Mutex::new(None);
/// One AI answer: its run, the question, and the answer or why there is none.
type AiAnswer = (u32, String, Result<crate::chat::Reply, String>);
/// The newest dictionary lookup: its run and what it found.
static DEFINE_ANSWER: Mutex<Option<(u32, dictionary::Found)>> = Mutex::new(None);
/// Whether the launcher's memory after its first opening has been logged.
static MEMORY_LOGGED: AtomicBool = AtomicBool::new(false);
/// When the Start menu was last read (seconds since 1970), 0 for never.
static APPS_AT: AtomicU64 = AtomicU64::new(0);
/// Icons loaded away from the panel's thread, by path.
static ICONS: Mutex<Option<HashMap<String, Arc<fileicon::Icon>>>> = Mutex::new(None);

struct Scanned {
    /// `None`: the Start menu was not read this time.
    apps: Option<Vec<App>>,
    running: Vec<Running>,
}

const ALT_TAKEN: &str = "Another app holds Alt+Space. Pulse takes the key while the launcher \
                         is on, so that app's shortcut stops working. Choose Ctrl+Space to \
                         give it back.";
const ALT_ASK: &str = "Another app holds Alt+Space and the launcher has not taken it. Choose \
                       which one gets it in the prompt, or choose Ctrl+Space.";
const ALT_KEPT: &str = "Another app keeps Alt+Space. Open the launcher from the tool wheel, or \
                        choose Ctrl+Space.";
const CTRL_REFUSED: &str = "Another app holds Ctrl+Space, so the launcher cannot open with it. \
                            Choose Alt+Space.";

/// The note the hub shows under the launcher's switch, when there is one.
pub fn status() -> Option<&'static str> {
    *STATUS.lock().unwrap_or_else(PoisonError::into_inner)
}

fn set_status(next: Option<&'static str>) {
    let mut status = STATUS.lock().unwrap_or_else(PoisonError::into_inner);
    if *status != next {
        *status = next;
        drop(status);
        // The hub reads it from the published state.
        host::wake_ui();
    }
}

/// The key hook's extra handler (runs on the hook thread, returns at once): Alt+Space opens
/// or closes the panel while another program holds the hot key. True swallows the key.
pub fn alt_chord(chord: AltChord) -> bool {
    if chord.vk != VK_SPACE || chord.shift || !HOOK_TAKES.load(Ordering::Relaxed) {
        return false;
    }
    if !chord.repeat {
        note_hotkey();
        post(WM_TOGGLE);
    }
    true
}

/// Milliseconds on a clock that starts at the first call.
fn mono_ms() -> u64 {
    EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// The panel is showing (set by `show`, cleared by `hide`), for the hot key to know open from close.
static PANEL_UP: AtomicBool = AtomicBool::new(false);

/// The hot key was just pressed: "hotkey to visible" starts here.
fn note_hotkey() {
    // Keys typed before the panel owns the keyboard are held for it (`keys::capture_begin`);
    // a press that closes the open panel holds nothing.
    if !PANEL_UP.load(Ordering::Relaxed) {
        host::capture_begin();
    }
    HOTKEY_AT.store(mono_ms() + 1, Ordering::Relaxed);
}

/// Writes a hot key line only when it differs from the last one written.
fn log_state(event: &str, fields: &[(&str, &str)]) {
    let line = format!("{event}{fields:?}");
    let mut last = LAST_STATE.lock().unwrap_or_else(PoisonError::into_inner);
    if last.get(event) != Some(&line) {
        host::log_info(event, fields);
        last.insert(event.to_string(), line);
    }
}

/// Whether Windows has a link scheme (`chatgpt:`) registered.
fn scheme_registered(scheme: &str) -> bool {
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

/// The Alt+Space answer kept from an earlier run: `Some(true)` Pulse takes it, `Some(false)`
/// the other program keeps it, `None` nobody has said.
fn conflict_answer() -> Option<bool> {
    match host::launcher_hotkey_conflict()?.as_str() {
        "pulse" => Some(true),
        "other" => Some(false),
        _ => None,
    }
}

/// Keeps the answer as the setting `launcherHotkeyConflict`, through the notch's own command
/// folder (the one thread that owns the settings file writes it); in force here at once.
fn save_conflict_answer(pulse: bool) {
    let answer = if pulse { "pulse" } else { "other" };
    host::set_live_hotkey_conflict(Some(answer));
    let Some(commands) = launcher_dir().and_then(|dir| Some(dir.parent()?.join("hub-commands")))
    else {
        return;
    };
    let body = format!(
        "{{\"command\":\"set\",\"key\":\"launcherHotkeyConflict\",\"value\":\"{answer}\"}}"
    );
    if std::fs::create_dir_all(&commands).is_ok()
        && rightkit_fs::atomic_write(&commands.join("launcher-conflict.json"), body.as_bytes())
            .is_ok()
    {
        host::wake_ui();
    }
}

/// Starts the launcher's thread. It holds the hot key only while the switch is on.
pub fn start() {
    let spawned = std::thread::Builder::new()
        .name("rightkit-launcher".to_string())
        .spawn(thread_main);
    if spawned.is_err() {
        host::log_info("launcher", &[("result", "no_thread")]);
    }
    sync_app_keys();
}

/// The switches changed: take or give back the hot key to match.
pub fn sync() {
    sync_app_keys();
    post(WM_SYNC);
    crate::snippets::sync_later();
}

/// The launcher's per-app hot keys (the Mac's `appHotkeys`), held as system hot keys through
/// rightkit-hotkeys on its loop thread. Made only when the config lists one, and re-registered
/// only when that list changed. Runs when the settings change, never on the panel's show path.
struct AppKeys {
    kit: rightkit_hotkeys::windows::GlobalHotkeys,
    bound: Vec<AppKeyBinding>,
    last: Vec<launcher_config::AppHotkey>,
}

struct AppKeyBinding {
    chord: rightkit_hotkeys::Chord,
}

static APP_KEYS: Mutex<Option<AppKeys>> = Mutex::new(None);
/// The hot key id and program path of each held app key. The loop thread reads only this, so
/// a registration in progress never waits on `APP_KEYS`.
static APP_KEY_TARGETS: Mutex<Vec<(i32, String)>> = Mutex::new(Vec::new());

fn sync_app_keys() {
    let wanted = launcher_config::parse(&host::launcher_config_json()).app_hotkeys;
    let mut slot = APP_KEYS.lock().unwrap_or_else(PoisonError::into_inner);
    if slot.is_none() {
        if wanted.is_empty() {
            return;
        }
        match rightkit_hotkeys::windows::GlobalHotkeys::new(app_key_pressed) {
            Ok(kit) => {
                *slot = Some(AppKeys {
                    kit,
                    bound: Vec::new(),
                    last: Vec::new(),
                });
            }
            Err(error) => {
                host::log_info("launcher", &[("result", "no_thread"), ("error", &error)]);
                return;
            }
        }
    }
    let Some(keys) = slot.as_mut() else {
        return;
    };
    if keys.last == wanted {
        return;
    }
    for binding in keys.bound.drain(..) {
        let _ = keys.kit.unregister(&binding.chord);
    }
    APP_KEY_TARGETS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clear();
    for row in &wanted {
        let Some(chord) = app_key_chord(&row.hotkey) else {
            host::log_info(
                "launcher",
                &[("result", "app_key_unknown"), ("hotkey", &row.hotkey)],
            );
            continue;
        };
        match keys.kit.register(&chord) {
            Ok(id) => {
                APP_KEY_TARGETS
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push((id, row.path.clone()));
                keys.bound.push(AppKeyBinding { chord });
            }
            Err(_) => host::log_info(
                "launcher",
                &[("result", "app_key_refused"), ("hotkey", &row.hotkey)],
            ),
        }
    }
    keys.last = wanted;
}

/// The Mac's shortcut text ("opt+1", "cmd+shift+k") as RightKit's chord. None unless it names
/// exactly one key and at least one modifier.
fn app_key_chord(text: &str) -> Option<rightkit_hotkeys::Chord> {
    let (mut ctrl, mut alt, mut shift, mut win) = (false, false, false, false);
    let mut key = None;
    for part in text.split('+') {
        match part.trim().to_ascii_lowercase().as_str() {
            "ctrl" => ctrl = true,
            "opt" | "alt" => alt = true,
            "shift" => shift = true,
            "cmd" | "win" => win = true,
            other => {
                if key.is_some() {
                    return None;
                }
                key = Some(other.to_string());
            }
        }
    }
    let key = key?;
    if !(ctrl || alt || shift || win) {
        return None;
    }
    let mut kit = String::new();
    for (held, word) in [
        (ctrl, "Ctrl+"),
        (alt, "Alt+"),
        (shift, "Shift+"),
        (win, "Win+"),
    ] {
        if held {
            kit.push_str(word);
        }
    }
    kit.push_str(&key);
    rightkit_hotkeys::parse_chord(&kit).ok()
}

/// An app hot key was pressed (on RightKit's loop thread).
fn app_key_pressed(id: i32) {
    let path = APP_KEY_TARGETS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|(held, _)| *held == id)
        .map(|(_, path)| path.clone());
    if let Some(path) = path {
        toggle_app(&path);
    }
}

/// Shows the program at `path`, or minimises its window when it is already in front (the
/// Mac hides it). A program with no open window is started.
fn toggle_app(path: &str) {
    let front = unsafe { GetForegroundWindow() };
    let mut front_pid = 0u32;
    let _ = unsafe { GetWindowThreadProcessId(front, Some(&mut front_pid)) };
    if image_path(front_pid).is_some_and(|p| p.eq_ignore_ascii_case(path)) {
        // SAFETY: minimises the window that is in front; no pointers are passed.
        let _ = unsafe { ShowWindow(front, SW_MINIMIZE) };
        return;
    }
    match running_apps()
        .into_iter()
        .find(|app| app.path.eq_ignore_ascii_case(path))
    {
        Some(app) => {
            let hwnd = hwnd_from_key(app.window);
            unsafe {
                if IsIconic(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                }
                let _ = SetForegroundWindow(hwnd);
            }
        }
        None => run(&Action::Open(path.to_string())),
    }
}

/// Opens the panel, or closes it when it is open (the tool wheel's Launcher tool).
pub fn toggle() {
    post(WM_TOGGLE);
}

/// Fresh exchange rates are in the launcher's folder (told by the thread that fetched them).
pub fn rates_ready() {
    post(WM_RATES);
}

/// Opens the panel on the clipboard history ("clip" typed for the user).
pub fn open_clip() {
    post(WM_OPEN_CLIP);
}

fn post(message: u32) {
    let id = THREAD_ID.load(Ordering::Relaxed);
    if id != 0 {
        // Failure means the thread is gone or has no queue yet; nothing to recover.
        let _ = unsafe { PostThreadMessageW(id, message, WPARAM(0), LPARAM(0)) };
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---------------------------------------------------------------- rows

#[derive(Clone, Copy, PartialEq)]
enum Section {
    Answer,
    Open,
    Pinned,
    Apps,
    Running,
    Quicklinks,
    Snippets,
    Commands,
    Files,
    Notes,
    Pulse,
    Clipboard,
    /// "define word": one row that looks the word up when chosen.
    Dictionary,
    /// The two closing rows for typed text: ask ChatGPT, search the web.
    Actions,
    /// The AI chat history (`chat`): questions asked before, pinned ones first.
    Chat,
    /// The question about which program gets Alt+Space.
    Hotkey,
}

impl Section {
    /// The section's short name for diag lines.
    fn key(self) -> &'static str {
        match self {
            Section::Answer => "answer",
            Section::Open => "open",
            Section::Pinned => "pinned",
            Section::Apps => "apps",
            Section::Running => "running",
            Section::Quicklinks => "quicklinks",
            Section::Snippets => "snippets",
            Section::Commands => "commands",
            Section::Files => "files",
            Section::Notes => "notes",
            Section::Pulse => "pulse",
            Section::Clipboard => "clipboard",
            Section::Dictionary => "dictionary",
            Section::Actions => "actions",
            Section::Chat => "chat",
            Section::Hotkey => "hotkey",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Section::Answer => host::t("CALCULATE"),
            Section::Open => host::t("OPEN"),
            Section::Pinned => host::t("PINNED"),
            Section::Apps => host::t("APPS"),
            Section::Running => host::t("RUNNING"),
            Section::Quicklinks => host::t("QUICKLINKS"),
            Section::Snippets => host::t("SNIPPETS"),
            Section::Commands => host::t("COMMANDS"),
            Section::Files => host::t("FILES"),
            Section::Notes => host::t("NOTES"),
            Section::Pulse => host::t("PULSE"),
            Section::Clipboard => host::t("CLIPBOARD"),
            Section::Dictionary => host::t("DICTIONARY"),
            Section::Actions => host::t("ASK OR SEARCH"),
            Section::Chat => host::t("AI CHAT"),
            Section::Hotkey => "ALT+SPACE",
        }
    }
}

/// The answers to the Alt+Space question.
#[derive(Clone, Copy, PartialEq)]
enum Conflict {
    /// Pulse takes the chord from the other program.
    Pulse,
    /// The other program keeps it.
    Other,
    /// Neither yet: the hub's launcher key setting opens.
    Choose,
}

#[derive(Clone)]
enum Action {
    /// A shortcut, program, folder, file or address the shell opens.
    Open(String),
    /// A running app's window, brought to the front.
    Focus(isize),
    /// One of the hub's pages.
    Hub(&'static str),
    /// Asks every running app's window to close.
    QuitAll,
    /// Puts an answer on the clipboard.
    Copy(String),
    /// Puts a snippet on the clipboard and pastes it into the app that was in front.
    Paste(String),
    /// Runs a command line and shows what it prints in the panel.
    Command(String),
    /// Makes a note that starts with this text and opens it.
    NoteNew(String),
    /// Puts a deleted note back and opens it.
    NoteRestore(String),
    /// A row that only says something (the Mac's `run: {}`): choosing it closes the panel.
    Inert,
    /// A clipboard history item (its id): Return pastes it into the app that was in front.
    Clip(String),
    /// A clipboard clear row: Return swaps the list for its confirmation row (the Mac's
    /// "Delete N items?"); nothing is deleted yet.
    ClipAsk(rightkit_clipboard_history::model::ClearScope),
    /// The confirmation row's action: deletes the scope's items, once.
    ClipClear(rightkit_clipboard_history::model::ClearScope),
    /// Hands the typed text to ChatGPT: its desktop link when one takes a question, else the
    /// web address with the question in it.
    AskChatGpt(String),
    /// An answer to the Alt+Space question.
    Conflict(Conflict),
    /// Asks the resident engine's assistant this question; the answer shows in the panel.
    AiAsk(String),
    /// Shows a question from the AI chat history (its id) with its answer.
    ChatOpen(String),
    /// Looks a word up (`launcher_dictionary`); the entry shows in the panel.
    Define(String),
    /// A note (its file) opened for editing in the panel; with `launcherNoteEditor` off it opens
    /// in the program Windows has for the file.
    NoteEdit(String),
}

#[derive(Clone)]
struct Item {
    section: Section,
    title: String,
    subtitle: Option<String>,
    /// The file whose shell icon stands for the row.
    icon: Option<String>,
    /// What Alt+P pins or unpins.
    pin: Option<String>,
    /// The running app's window: Alt+Q asks it to close.
    window: isize,
    /// The key use is counted under.
    usage: Option<String>,
    /// A clipboard row's kind, which picks its glyph.
    kind: Option<rightkit_clipboard_history::model::Kind>,
    action: Action,
}

impl Item {
    /// The outline glyph of a row that has no file icon: the Mac's symbol for its kind.
    fn glyph(&self) -> glyphs::RowGlyph {
        use glyphs::RowGlyph;
        match self.section {
            Section::Answer => RowGlyph::Calc,
            Section::Open
                if host::arg("Open {}", &self.title)
                    .is_some_and(|rest| rest.starts_with("http")) =>
            {
                RowGlyph::Globe
            }
            Section::Open => RowGlyph::Folder,
            Section::Quicklinks => RowGlyph::Link,
            Section::Clipboard => RowGlyph::Doc,
            Section::Snippets => RowGlyph::Quote,
            Section::Commands => RowGlyph::Terminal,
            Section::Files => RowGlyph::Doc,
            Section::Notes
                if matches!(self.action, Action::NoteNew(_))
                    || self.title == host::t("New note") =>
            {
                RowGlyph::Compose
            }
            Section::Notes => RowGlyph::Note,
            Section::Actions if matches!(self.action, Action::AskChatGpt(_)) => RowGlyph::Bubble,
            Section::Actions if matches!(self.action, Action::AiAsk(_)) => RowGlyph::Sparkles,
            Section::Chat => RowGlyph::Bubble,
            Section::Dictionary => RowGlyph::Note,
            Section::Actions => RowGlyph::Magnifier,
            Section::Hotkey => RowGlyph::Sliders,
            Section::Pulse => match self.action {
                Action::Hub("cleanup") => RowGlyph::Sparkles,
                Action::Hub("monitor") => RowGlyph::Monitor,
                Action::Hub("apps") => RowGlyph::Grid,
                Action::Hub("settings") => RowGlyph::Sliders,
                Action::Hub("accounts") => RowGlyph::Gauge,
                // A shot row carries no page: the Mac's cleanup findings still read as such.
                _ if self.title.starts_with("Cleanup") => RowGlyph::Sparkles,
                _ => RowGlyph::Drive,
            },
            Section::Running if matches!(self.action, Action::QuitAll) => RowGlyph::Power,
            Section::Pinned | Section::Apps | Section::Running => RowGlyph::App,
        }
    }
}

struct App {
    name: String,
    path: String,
}

struct Running {
    name: String,
    path: String,
    window: isize,
}

/// A byte count as GB or TB with one decimal.
fn size_text(bytes: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let gib = bytes as f64 / GIB;
    let (value, unit) = if gib >= 1024.0 {
        (gib / 1024.0, "TB")
    } else {
        (gib, "GB")
    };
    let number = if value >= 100.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}").trim_end_matches(".0").to_string()
    };
    format!("{number} {unit}")
}

/// "Found 2 h ago", from the scan's own time (Unix seconds).
fn found_text(scanned: f64, now: f64) -> String {
    let seconds = (now - scanned).max(0.0);
    let age = if seconds < 60.0 {
        host::t("just now").to_string()
    } else if seconds < 3_600.0 {
        let minutes = host::tf("{} min", &[&((seconds / 60.0) as u64).to_string()]);
        host::tf("{} ago", &[&minutes])
    } else if seconds < 86_400.0 {
        let hours = host::tf("{} h", &[&((seconds / 3_600.0) as u64).to_string()]);
        host::tf("{} ago", &[&hours])
    } else {
        let days = host::tf("{} d", &[&((seconds / 86_400.0) as u64).to_string()]);
        host::tf("{} ago", &[&days])
    };
    host::tf("Found {}", &[&age])
}

fn score(query: &str, name: &str) -> Option<i32> {
    let q: Vec<char> = query
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    if q.is_empty() {
        return None;
    }
    let mut qi = 0;
    let mut score = 0i32;
    let mut previous: Option<usize> = None;
    for (i, c) in n.iter().enumerate() {
        if qi >= q.len() || *c != q[qi] {
            continue;
        }
        score += 10;
        if i > 0 && previous == Some(i - 1) {
            score += 15;
        }
        if i == 0 || !n[i - 1].is_alphanumeric() {
            score += 25;
        }
        previous = Some(i);
        qi += 1;
    }
    if qi != q.len() {
        return None;
    }
    let lowered: String = n.iter().collect();
    let joined: String = q.iter().collect();
    if lowered.starts_with(&joined) {
        score += 200;
    } else if lowered.contains(&joined) {
        score += 80;
    }
    Some(score - (n.len() as i32 - q.len() as i32))
}

// ---------------------------------------------------------------- kept files

fn launcher_dir() -> Option<PathBuf> {
    Some(host::data_dir()?.join("launcher"))
}

fn quoted(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn now_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn write_kept(name: &str, body: &str) {
    let Some(dir) = launcher_dir() else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_ok() {
        let _ = rightkit_fs::atomic_write(&dir.join(name), body.as_bytes());
    }
}

fn read_kept(name: &str) -> Option<Value> {
    let bytes = std::fs::read(launcher_dir()?.join(name)).ok()?;
    json::parse(&bytes, FILE_MAX)
}

/// How often and how recently each thing was run from the launcher (the Mac's "frecency").
struct Usage {
    /// Key to (count, last use in seconds since 1970).
    records: BTreeMap<String, (f64, f64)>,
}

impl Usage {
    fn load() -> Self {
        let mut records = BTreeMap::new();
        if let Some(Value::Object(members)) = read_kept("usage.json") {
            for (key, value) in members {
                let count = value.get("count").and_then(Value::as_f64);
                let last = value.get("last").and_then(Value::as_f64);
                if let (Some(count), Some(last)) = (count, last) {
                    records.insert(key, (count, last));
                }
            }
        }
        Self { records }
    }

    fn decayed(&(count, last): &(f64, f64), now: f64) -> f64 {
        let days = (now - last).max(0.0) / 86_400.0;
        count / (1.0 + days / 14.0)
    }

    /// Points added to a fuzzy score, at most 80.
    fn bonus(&self, key: &str) -> i32 {
        match self.records.get(key) {
            Some(record) => (Self::decayed(record, now_seconds()).min(20.0) * 4.0) as i32,
            None => 0,
        }
    }

    /// Keys with the prefix, most used first, without the prefix.
    fn top(&self, prefix: &str, limit: usize) -> Vec<String> {
        let now = now_seconds();
        let mut found: Vec<(&String, f64)> = self
            .records
            .iter()
            .filter(|(key, _)| key.starts_with(prefix))
            .map(|(key, record)| (key, Self::decayed(record, now)))
            .collect();
        found.sort_by(|a, b| b.1.total_cmp(&a.1));
        found
            .into_iter()
            .take(limit)
            .map(|(key, _)| key[prefix.len()..].to_string())
            .collect()
    }

    fn record(&mut self, key: &str) {
        let now = now_seconds();
        let entry = self.records.entry(key.to_string()).or_insert((0.0, now));
        entry.0 += 1.0;
        entry.1 = now;
        if self.records.len() > USAGE_KEPT {
            // Drop the least used so the file stays small.
            let mut all: Vec<(String, f64)> = self
                .records
                .iter()
                .map(|(key, record)| (key.clone(), Self::decayed(record, now)))
                .collect();
            all.sort_by(|a, b| b.1.total_cmp(&a.1));
            for (key, _) in all.into_iter().skip(USAGE_KEPT) {
                self.records.remove(&key);
            }
        }
        let mut body = String::from("{");
        for (i, (key, (count, last))) in self.records.iter().enumerate() {
            if i > 0 {
                body.push(',');
            }
            body.push_str(&format!(
                "{}:{{\"count\":{count},\"last\":{}}}",
                quoted(key),
                last.round()
            ));
        }
        body.push('}');
        write_kept("usage.json", &body);
    }
}

fn load_pins() -> Vec<String> {
    match read_kept("pins.json") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

/// Keeps the pins: in `pins.json`, and in the hub's `launcherConfig` through the notch's own
/// command folder, so the settings file is written by the one thread that owns it.
fn save_pins(pins: &[String]) {
    let items: Vec<String> = pins.iter().map(|pin| quoted(pin)).collect();
    write_kept("pins.json", &format!("[{}]", items.join(",")));
    let Some(config) = launcher_config::with_pins(&host::launcher_config_json(), pins) else {
        return;
    };
    let Some(commands) = launcher_dir().and_then(|dir| Some(dir.parent()?.join("hub-commands")))
    else {
        return;
    };
    let body = format!(
        "{{\"command\":\"set\",\"key\":\"launcherConfig\",\"value\":{}}}",
        launcher_config::quoted(&config)
    );
    if std::fs::create_dir_all(&commands).is_ok()
        && rightkit_fs::atomic_write(&commands.join("launcher-pins.json"), body.as_bytes()).is_ok()
    {
        host::wake_ui();
    }
}

/// Today's date as snippets and quicklinks show it: "Oct 11, 2026".
fn today() -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    // SAFETY: plain call with no arguments.
    let now = unsafe { GetLocalTime() };
    let month = MONTHS[(usize::from(now.wMonth).clamp(1, 12)) - 1];
    format!("{month} {}, {}", now.wDay, now.wYear)
}

/// The argument after a keyword typed as the first word, or `None` when the keyword does not
/// match.
fn keyword_argument(text: &str, keyword: &str) -> Option<String> {
    let key = keyword.trim().to_lowercase();
    if key.is_empty() {
        return None;
    }
    let mut parts = text.splitn(2, ' ');
    let first = parts.next()?;
    (first.to_lowercase() == key).then(|| parts.next().unwrap_or("").trim().to_string())
}

/// The argument when the keyword matches, or the whole text when it fuzzily matches the name
/// (two characters or more).
fn matches_item(text: &str, keyword: &str, name: &str) -> Option<String> {
    if let Some(argument) = keyword_argument(text, keyword) {
        return Some(argument);
    }
    (text.chars().count() >= 2 && score(text, name).is_some()).then(|| text.to_string())
}

/// The search engine's name and its address up to the query, for `launcherWebSearchEngine`: Google
/// unless the hub says "bing" or "duckduckgo".
fn web_engine(choice: &str) -> (&'static str, &'static str) {
    match choice {
        "bing" => ("Bing", "https://www.bing.com/search?q="),
        "duckduckgo" => ("DuckDuckGo", "https://duckduckgo.com/?q="),
        _ => ("Google", "https://www.google.com/search?q="),
    }
}

/// Opens ChatGPT with `question`: its desktop link when it is registered and takes a
/// question, else the web address (which opens it in the browser, or in the app when it
/// claims the address).
fn ask_chatgpt(question: &str) {
    let encoded = launcher_config::encode_query(question);
    if let Some(template) = CHATGPT_APP_URL
        && scheme_registered("chatgpt")
    {
        run(&Action::Open(template.replace("{query}", &encoded)));
        return;
    }
    run(&Action::Open(format!("https://chatgpt.com/?q={encoded}")));
}

// ---------------------------------------------------------------- apps

fn start_menu_roots() -> Vec<PathBuf> {
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

fn scan_folder(dir: &Path, depth: usize, seen: &mut HashSet<String>, out: &mut Vec<App>) {
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
            out.push(App {
                name: name.to_string(),
                path: path.to_string_lossy().into_owned(),
            });
        }
    }
}

/// The Start menu's shortcuts (everyone's, then this user's) and the Store apps, by name.
fn scan_apps() -> Vec<App> {
    let mut seen = HashSet::new();
    let mut apps = Vec::new();
    for root in start_menu_roots() {
        scan_folder(&root, SCAN_DEPTH, &mut seen, &mut apps);
    }
    for (name, path) in crate::store::apps() {
        if seen.insert(name.to_lowercase()) {
            apps.push(App { name, path });
        }
    }
    apps.sort_by_key(|app| app.name.to_lowercase());
    apps
}

/// The full path of the program a process runs, or `None` when it cannot be opened (an elevated
/// process, from a Pulse that is not).
pub fn image_path(pid: u32) -> Option<String> {
    // SAFETY: a plain OpenProcess for limited query rights; the handle is closed below.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buffer = [0u16; 1024];
    let mut size = buffer.len() as u32;
    // SAFETY: `buffer` holds `size` UTF-16 units for the call.
    let queried = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut size,
        )
    };
    // SAFETY: the handle came from OpenProcess above and is not used again.
    let _ = unsafe { CloseHandle(process) };
    queried.ok()?;
    Some(String::from_utf16_lossy(&buffer[..size as usize]))
}

struct Found {
    own: u32,
    seen: HashSet<String>,
    found: Vec<Running>,
}

unsafe extern "system" fn collect_window(hwnd: HWND, data: LPARAM) -> BOOL {
    // SAFETY: EnumWindows supplies live handles; `data` is the caller's `Found`, alive for
    // the call. Read-only window queries only.
    unsafe {
        let context = &mut *(data.0 as *mut Found);
        if !IsWindowVisible(hwnd).as_bool() {
            return true.into();
        }
        let owned = match GetWindow(hwnd, GW_OWNER) {
            Ok(owner) => !owner.0.is_null(),
            Err(_) => false,
        };
        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        use rightkit_overlay::ex_style::{NOACTIVATE, TOOLWINDOW};
        if owned || ex_style & (NOACTIVATE | TOOLWINDOW) != 0 {
            return true.into();
        }
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut c_void,
            size_of::<u32>() as u32,
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
        let Some(path) = image_path(pid) else {
            return true.into();
        };
        if context.seen.insert(path.to_lowercase()) {
            let stem = Path::new(&path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("App");
            context.found.push(Running {
                name: product_name(&path).unwrap_or_else(|| display_name(stem)),
                path,
                window: hwnd_key(hwnd),
            });
        }
        true.into()
    }
}

/// The largest version resource read for a program's name.
const VERSION_MAX: u32 = 256 * 1024;

/// What the program at `path` calls itself (its version resource's file description, such as
/// "Google Chrome"); `None` when it has none.
pub fn product_name(path: &str) -> Option<String> {
    let file = wide(path);
    let mut ignored = 0u32;
    // SAFETY: `file` is NUL-terminated; `ignored` is a valid out pointer.
    let size = unsafe { ffi::GetFileVersionInfoSizeW(file.as_ptr(), &mut ignored) };
    if size == 0 || size > VERSION_MAX {
        return None;
    }
    let mut block = vec![0u8; size as usize];
    // SAFETY: `block` holds `size` bytes for the call.
    if unsafe { ffi::GetFileVersionInfoW(file.as_ptr(), 0, size, block.as_mut_ptr().cast()) } == 0 {
        return None;
    }
    let query = |sub_block: &str| -> Option<(*const u8, usize)> {
        let name = wide(sub_block);
        let mut found: *mut c_void = std::ptr::null_mut();
        let mut length = 0u32;
        // SAFETY: `block` is a version resource filled above; the out pointers are valid, and
        // what comes back points into `block`, which outlives every use below.
        let ok = unsafe {
            ffi::VerQueryValueW(
                block.as_ptr().cast(),
                name.as_ptr(),
                &mut found,
                &mut length,
            )
        };
        let found = found as *const u8;
        (ok != 0 && !found.is_null() && length > 0).then_some((found, length as usize))
    };
    // The first language the resource lists, then US English in Unicode as most programs have.
    let (pair, pair_bytes) = query("\\VarFileInfo\\Translation")?;
    let mut tables = vec!["040904b0".to_string()];
    if pair_bytes >= 4 {
        // SAFETY: the value is at least four bytes inside `block`: a language id and a code
        // page, two bytes each.
        let bytes = unsafe { std::slice::from_raw_parts(pair, 4) };
        let language = u16::from_le_bytes([bytes[0], bytes[1]]);
        let code_page = u16::from_le_bytes([bytes[2], bytes[3]]);
        tables.insert(0, format!("{language:04x}{code_page:04x}"));
    }
    for table in tables {
        let key = format!("\\StringFileInfo\\{table}\\FileDescription");
        let Some((text, units)) = query(&key) else {
            continue;
        };
        // SAFETY: `text` points at `units` UTF-16 units (two bytes each) inside `block`.
        let bytes = unsafe { std::slice::from_raw_parts(text, units * 2) };
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .take_while(|unit| *unit != 0)
            .collect();
        let name = String::from_utf16_lossy(&units).trim().to_string();
        if !name.is_empty() {
            return Some(name);
        }
    }
    None
}

/// A program's file name as a title: `chrome` reads `Chrome`.
fn display_name(stem: &str) -> String {
    let mut chars = stem.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// One entry per running program that has a window, front to back.
fn running_apps() -> Vec<Running> {
    let mut context = Found {
        own: unsafe { GetCurrentProcessId() },
        seen: HashSet::new(),
        found: Vec::new(),
    };
    // SAFETY: the callback only runs during this call; `context` lives past it.
    let _ = unsafe {
        EnumWindows(
            Some(collect_window),
            LPARAM(&mut context as *mut Found as isize),
        )
    };
    context.found
}

/// A single-threaded COM apartment for the life of the guard, left when it drops.
struct ComApartment(bool);

impl ComApartment {
    fn enter() -> Self {
        // SAFETY: initialises COM on the calling thread; `Drop` balances a success.
        let entered = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
        Self(entered)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful `CoInitializeEx` of `enter`, on the same thread.
            unsafe { CoUninitialize() };
        }
    }
}

/// Reads the Start menu (when the list is older than `APPS_FRESH_SECS`, or `force_apps`) and
/// the running programs on a thread of their own, then loads the icons of what it found. The
/// panel takes the lists on `WM_APPS` and draws again on `WM_ICONS`; typing never waits on
/// the disk or the shell.
fn spawn_scan(force_apps: bool) {
    if SCANNING.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("launcher-scan".to_string())
        .spawn(move || {
            // Shell icon calls need an apartment on the thread that makes them.
            let _com = ComApartment::enter();
            let now = now_seconds() as u64;
            let last = APPS_AT.load(Ordering::Relaxed);
            let apps = (force_apps || last == 0 || now.saturating_sub(last) > APPS_FRESH_SECS)
                .then(scan_apps);
            if apps.is_some() {
                APPS_AT.store(now.max(1), Ordering::Relaxed);
            }
            let running = running_apps();
            let mut paths: Vec<String> = running.iter().map(|app| app.path.clone()).collect();
            if let Some(apps) = &apps {
                paths.extend(apps.iter().map(|app| app.path.clone()));
            }
            {
                let mut slot = SCANNED.lock().unwrap_or_else(PoisonError::into_inner);
                // A Start menu list the panel has not taken yet stays.
                let kept = slot.take().and_then(|old| old.apps);
                *slot = Some(Scanned {
                    apps: apps.or(kept),
                    running,
                });
            }
            post(WM_APPS);
            if let Some(base) = launcher_dir() {
                launcher_notes::purge(&base, now);
            }
            for (done, path) in paths.iter().enumerate() {
                if !warm_icon(path) {
                    break;
                }
                if done % 25 == 24 {
                    post(WM_ICONS);
                }
            }
            post(WM_ICONS);
            SCANNING.store(false, Ordering::Release);
        });
    if spawned.is_err() {
        SCANNING.store(false, Ordering::Release);
    }
}

/// Loads the shell icon of `path` into the launcher's own cache. False when the cache is full.
fn warm_icon(path: &str) -> bool {
    if path.is_empty() || path.starts_with(crate::store::PREFIX) {
        return true;
    }
    let held = {
        let guard = ICONS.lock().unwrap_or_else(PoisonError::into_inner);
        guard
            .as_ref()
            .map(|icons| (icons.contains_key(path), icons.len()))
            .unwrap_or((false, 0))
    };
    if held.0 {
        return true;
    }
    if held.1 >= ICONS_MAX {
        return false;
    }
    // The shell is asked with no lock of ours held.
    if let Some(icon) = fileicon::of(path) {
        ICONS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert_with(HashMap::new)
            .insert(path.to_string(), icon);
    }
    true
}

/// Paths whose icon a thread has already been asked for, so a path the shell has no icon for
/// is asked once, not on every redraw.
static ICONS_ASKED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Loads the icons of `paths` that no thread has tried yet, away from the panel's thread, and
/// asks for a redraw when it is done.
fn warm_icons_later(paths: Vec<String>) {
    let fresh: Vec<String> = {
        let mut guard = ICONS_ASKED.lock().unwrap_or_else(PoisonError::into_inner);
        let asked = guard.get_or_insert_with(HashSet::new);
        paths
            .into_iter()
            .filter(|path| asked.insert(path.clone()))
            .collect()
    };
    if fresh.is_empty() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("launcher-icons".to_string())
        .spawn(move || {
            // Shell icon calls need an apartment on the thread that makes them.
            let _com = ComApartment::enter();
            for path in &fresh {
                fetch_icon(path);
            }
            post(WM_ICONS);
        });
}

/// Loads the icon of a row the panel is drawing. Unlike the scan's `warm_icon` it never gives
/// up on a full cache: the cache and the asked set are emptied so the rows on screen get their
/// icons again (the panel asks once more for what it still draws), and any path the panel was
/// drawing before (Store rows included) is asked, as it was when the panel asked the shell itself.
fn fetch_icon(path: &str) {
    if path.is_empty() || cached_icon(path).is_some() {
        return;
    }
    let Some(icon) = fileicon::of(path) else {
        return;
    };
    let mut guard = ICONS.lock().unwrap_or_else(PoisonError::into_inner);
    let icons = guard.get_or_insert_with(HashMap::new);
    if icons.len() >= ICONS_MAX {
        icons.clear();
        if let Some(asked) = ICONS_ASKED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
        {
            asked.clear();
        }
    }
    icons.insert(path.to_string(), icon);
}

/// An icon already loaded away from the panel's thread.
fn cached_icon(path: &str) -> Option<Arc<fileicon::Icon>> {
    ICONS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()?
        .get(path)
        .cloned()
}

/// The clipboard's text, for Ctrl+V in the search field.
fn clipboard_text() -> Option<String> {
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
                let all = std::slice::from_raw_parts(pointer, units.min(QUERY_MAX * 2));
                let end = all.iter().position(|unit| *unit == 0).unwrap_or(all.len());
                text = Some(String::from_utf16_lossy(&all[..end]));
                ffi::GlobalUnlock(handle);
            }
        }
        ffi::CloseClipboard();
        text
    }
}

// ---------------------------------------------------------------- the panel

/// What a command printed, shown in place of the list until Escape.
struct Output {
    title: String,
    lines: Vec<String>,
    /// The first line on show.
    top: usize,
    /// Which run this is: an answer for an earlier run is dropped.
    run: u32,
    /// Set only for an Ask AI answer: it is laid out by `render_ask` instead of `lines`.
    ask: Option<AskView>,
}

/// Where an Ask AI output stands.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AskState {
    Pending,
    Answer,
    Error,
}

/// An Ask AI output : the answer parsed once into a `ResponseDocument` and
/// its wrapped visual lines, cached for the width they were laid out for. Scrolling (`Output::top`)
/// counts these lines, so Up, Down and Home reach every wrapped line.
struct AskView {
    state: AskState,
    /// Shown, secondary, only when the engine supplied a provider label with a fresh answer. A
    /// reopened answer has none: the saved history's schema carries no provider.
    provider: Option<String>,
    doc: response_layout::ResponseDocument,
    rows: Vec<response_layout::VisualLine>,
    /// The text width in pixels `rows` was laid out for; -1 before the first layout.
    laid_for: i32,
}

impl AskView {
    fn new(state: AskState, text: &str) -> Self {
        let doc = if state == AskState::Answer {
            response_layout::ResponseDocument::from_text(text, &response_layout::Limits::DEFAULT)
        } else {
            // Progress and error text are never parsed; they still wrap.
            response_layout::ResponseDocument::plain(text)
        };
        AskView {
            state,
            provider: None,
            doc,
            rows: Vec::new(),
            laid_for: -1,
        }
    }

    /// A readable error with the recovery that already exists: Esc, then Return on the question.
    fn error(reason: &str) -> Self {
        let text = format!(
            "{reason}\n{}",
            host::t("Esc goes back; choose Ask AI again to retry.")
        );
        Self::new(AskState::Error, &text)
    }
}

/// Face of one visual line of an Ask AI answer: text size in logical pixels and whether bold.
fn ask_face(role: response_layout::Role) -> (f32, bool) {
    match role {
        response_layout::Role::Title => (ASK_TITLE_BLOCK, true),
        response_layout::Role::Heading => (ASK_HEAD, true),
        response_layout::Role::Body => (ASK_TEXT, false),
        response_layout::Role::Metadata => (ASK_META, false),
        response_layout::Role::Code => (ASK_CODE, false),
    }
}

// Spec 5.2 desktop targets: body 15, section heading 18, metadata 12, code 13. A level 1 heading
// is the 18 px section heading and levels 2 to 6 are 16 px, so the levels differ. Adaptation: every
// row is one fixed height (scrolling steps one row), so a block gap is one blank row and a
// heading is not followed by a separate 8 px gap; the target gaps (20 / 10 / 8) are not drawn.
const ASK_TEXT: f32 = 15.0;
const ASK_TITLE_BLOCK: f32 = 18.0;
const ASK_HEAD: f32 = 16.0;
const ASK_META: f32 = 12.0;
const ASK_CODE: f32 = 13.0;
/// The compact semibold question header.
const ASK_TITLE: f32 = 15.0;
/// One row of an Ask AI answer (all roles share it so scrolling steps one row).
const ASK_LINE: f32 = 22.0;
/// One nesting step (list depth, quote, code, table value).
const ASK_INDENT: f32 = 16.0;

/// The most characters a note opened in the panel may hold; a longer file opens in its own program.
const EDITOR_MAX: usize = 256 * 1024;
/// Characters in one row of the note editor (the 620 px panel at 13 px text); longer lines wrap.
const EDITOR_WRAP: usize = 80;
const EDITOR_TEXT: f32 = 13.0;

/// A note open for editing in the panel (the Mac's `editingNoteID` and `editorText`): the text is
/// written to the note's file after every change, so Esc, hiding or quitting loses nothing.
struct Editor {
    path: PathBuf,
    text: Vec<char>,
    caret: usize,
    /// The first row on show.
    top: usize,
}

/// One row of the editor: where it starts and ends in the text, and whether it ends the line
/// (a row that wraps ends in the middle of one).
type EditorRow = (usize, usize, bool);

impl Editor {
    /// The text as rows: each line broken at a space (or hard) every `EDITOR_WRAP` characters.
    /// A row's end is where the next one starts, or the line's end.
    fn rows(&self) -> Vec<EditorRow> {
        let text = &self.text;
        let mut rows = Vec::new();
        let mut start = 0;
        loop {
            let newline = text[start..]
                .iter()
                .position(|c| *c == '\n')
                .map(|at| start + at);
            let line_end = newline.unwrap_or(text.len());
            let mut from = start;
            loop {
                if line_end - from <= EDITOR_WRAP {
                    rows.push((from, line_end, true));
                    break;
                }
                let window = from + EDITOR_WRAP;
                let cut = text[from..window]
                    .iter()
                    .rposition(|c| *c == ' ')
                    .map(|at| from + at + 1)
                    .filter(|cut| *cut > from + EDITOR_WRAP / 2)
                    .unwrap_or(window);
                rows.push((from, cut, false));
                from = cut;
            }
            match newline {
                Some(at) => start = at + 1,
                None => break,
            }
        }
        rows
    }

    /// The row the caret is in: the last one that starts at or before it.
    fn caret_row(&self, rows: &[EditorRow]) -> usize {
        rows.iter()
            .rposition(|row| row.0 <= self.caret)
            .unwrap_or(0)
    }

    /// The furthest the caret goes in a row: its end, or one short of it where the row wraps.
    fn row_limit(row: &EditorRow) -> usize {
        if row.2 {
            row.1
        } else {
            row.1.saturating_sub(1).max(row.0)
        }
    }

    /// Moves the caret `delta` rows, keeping its column; past the first or last row it goes to the
    /// start or the end of the text.
    fn vertical(&mut self, delta: isize) {
        let rows = self.rows();
        let current = self.caret_row(&rows);
        let target = current as isize + delta;
        if target < 0 {
            self.caret = 0;
        } else if target as usize >= rows.len() {
            self.caret = self.text.len();
        } else {
            let column = self.caret - rows[current].0;
            let row = rows[target as usize];
            self.caret = (row.0 + column).min(Self::row_limit(&row));
        }
    }

    fn home(&mut self, whole: bool) {
        let rows = self.rows();
        self.caret = if whole {
            0
        } else {
            rows[self.caret_row(&rows)].0
        };
    }

    fn end(&mut self, whole: bool) {
        let rows = self.rows();
        self.caret = if whole {
            self.text.len()
        } else {
            Self::row_limit(&rows[self.caret_row(&rows)])
        };
    }

    /// Inserts text at the caret (a newline kept, any other control character dropped). False
    /// when nothing was added.
    fn insert(&mut self, text: &str) -> bool {
        let mut added = false;
        for c in text.chars() {
            if self.text.len() >= EDITOR_MAX {
                break;
            }
            if c == '\n' || (c >= ' ' && c != '\u{7f}') {
                self.text.insert(self.caret, c);
                self.caret += 1;
                added = true;
            }
        }
        added
    }

    /// Deletes before the caret: one character, or with `word` the word before it.
    fn backspace(&mut self, word: bool) -> bool {
        if self.caret == 0 {
            return false;
        }
        let mut start = self.caret - 1;
        if word {
            while start > 0 && self.text[start].is_whitespace() {
                start -= 1;
            }
            while start > 0 && !self.text[start - 1].is_whitespace() {
                start -= 1;
            }
        }
        self.text.drain(start..self.caret);
        self.caret = start;
        true
    }

    fn delete(&mut self) -> bool {
        if self.caret >= self.text.len() {
            return false;
        }
        self.text.remove(self.caret);
        true
    }

    fn blank(&self) -> bool {
        self.text.iter().all(|c| c.is_whitespace())
    }

    /// Writes the note's file. False when it could not be written.
    fn save(&self) -> bool {
        let text: String = self.text.iter().collect();
        rightkit_fs::atomic_write(&self.path, text.as_bytes()).is_ok()
    }
}

/// The newest command's outcome, left here by its thread for `WM_OUTPUT`.
static OUTCOME: Mutex<Option<(u32, launcher_commands::Outcome)>> = Mutex::new(None);

/// What a key or click asks for, done once the panel's state is no longer borrowed.
enum Do {
    Nothing,
    Redraw,
    Hide,
    Run(Item),
}

struct Panel {
    window: OwnedWindow,
    painter: TextPainter,
    /// RightKit's hot key loop, started the first time a chord is asked for. A press posts
    /// `WM_TOGGLE` to this thread.
    keys: Option<GlobalHotkeys>,
    /// The chord registered with Windows (`None`: off, or taken through the key hook).
    hot: Option<rightkit_hotkeys::Chord>,
    /// The chord last asked for (`None`: the launcher is off).
    applied: Option<LauncherHotkey>,
    shown: bool,
    /// A view shot: drawn without the text caret, as the Mac's stills are.
    still: bool,
    scale: f32,
    origin: (i32, i32),
    /// The window that was in front, given the foreground back on Esc.
    previous: isize,
    query: Vec<char>,
    caret: usize,
    /// The first half of a character that arrives as two `WM_CHAR`s.
    high: Option<u16>,
    items: Vec<Item>,
    selected: usize,
    /// Top of each row in the list (unscrolled, in pixels), for clicks and scrolling.
    tops: Vec<f32>,
    scroll: f32,
    apps: Vec<App>,
    running: Vec<Running>,
    pins: Vec<String>,
    config: Config,
    output: Option<Output>,
    /// The note open for editing, in place of the field and the list.
    editor: Option<Editor>,
    runs: u32,
    /// File names under the chosen folders, while the panel is open.
    files: Option<Arc<launcher_files::Index>>,
    /// The thread reading them, until it has finished.
    files_job: Option<std::thread::JoinHandle<launcher_files::Index>>,
    /// The file rows found for `file_text` (searched away from this thread).
    file_rows: Vec<Item>,
    file_text: String,
    /// The holder named in the Alt+Space question while it is on show.
    prompt: Option<String>,
    /// When the last `recompute` finished, for the per-key timing line.
    rows_done: Option<Instant>,
    /// Counts changes to the typed text, so the timing line is written once per change.
    edits: u64,
    /// The row that was selected before Tab moved to the action rows (Shift+Tab goes back).
    before_tab: usize,
    /// The cached exchange rates, read when the panel opens; `None` with money conversions
    /// off or before the first fetch.
    rates: Option<crate::rates::Rates>,
    usage: Usage,
    /// The rows that read the disk or the clipboard, for the text they were asked for.
    slow: Option<Slow>,
    /// The AI chat history, newest first (`launcher_chat`).
    chats: Vec<crate::chat::Entry>,
    /// When the panel was last opened, and whether the first typing after that was logged.
    opened_at: Option<Instant>,
    first_rows_logged: bool,
}

/// Rows that read the disk or the clipboard. They are computed on a thread after a pause in
/// typing and shown when they arrive; `text` is the text they were computed for.
struct Slow {
    text: String,
    open: Vec<Item>,
    quicklinks: Vec<Item>,
    snippets: Vec<Item>,
    notes: Vec<Item>,
    clips: Vec<Item>,
    storage: Vec<Item>,
}

thread_local! {
    static PANEL: RefCell<Option<Panel>> = const { RefCell::new(None) };
}

/// Runs `f` on the panel unless it is already in use further up the stack (a call made while
/// handling one message can deliver another to the same window at once).
fn with_panel<R>(f: impl FnOnce(&mut Panel) -> R) -> Option<R> {
    PANEL.with(|cell| {
        let mut guard = cell.try_borrow_mut().ok()?;
        guard.as_mut().map(f)
    })
}

fn exists(path: &str) -> bool {
    // A Store app is an id the shell opens, not a file.
    path.starts_with(crate::store::PREFIX) || Path::new(path).exists()
}

impl Panel {
    fn text(&self) -> String {
        self.query.iter().collect::<String>().trim().to_string()
    }

    fn running_by_path(&self) -> BTreeMap<String, isize> {
        self.running
            .iter()
            .map(|app| (app.path.to_lowercase(), app.window))
            .collect()
    }

    fn app_item(
        &self,
        section: Section,
        name: &str,
        path: &str,
        subtitle: Option<String>,
        window: isize,
    ) -> Item {
        Item {
            section,
            title: name.to_string(),
            subtitle,
            icon: Some(path.to_string()),
            pin: Some(path.to_string()),
            window,
            usage: Some(format!("app:{path}")),
            kind: None,
            action: if window != 0 && section == Section::Running {
                Action::Focus(window)
            } else {
                Action::Open(path.to_string())
            },
        }
    }

    fn name_of(path: &str) -> String {
        let stem = Path::new(path).file_stem().and_then(|s| s.to_str());
        let is_program = path.to_lowercase().ends_with(".exe");
        let named = if is_program { product_name(path) } else { None };
        named.or_else(|| stem.map(display_name)).unwrap_or_default()
    }

    fn pinned_items(&self) -> Vec<Item> {
        let running = self.running_by_path();
        self.pins
            .iter()
            .filter(|path| exists(path))
            .map(|path| {
                let window = running.get(&path.to_lowercase()).copied().unwrap_or(0);
                let subtitle = if window != 0 {
                    host::t("Pinned · Running")
                } else {
                    host::t("Pinned")
                };
                self.app_item(
                    Section::Pinned,
                    &Self::name_of(path),
                    path,
                    Some(subtitle.to_string()),
                    window,
                )
            })
            .collect()
    }

    /// Apps used most often and most recently, when nothing is typed.
    fn recent_items(&self) -> Vec<Item> {
        let running = self.running_by_path();
        self.usage
            .top("app:", 10)
            .into_iter()
            .filter(|path| !self.pins.contains(path) && exists(path))
            .take(RECENT_MAX)
            .map(|path| {
                let window = running.get(&path.to_lowercase()).copied().unwrap_or(0);
                let subtitle = (window != 0).then(|| host::t("Running").to_string());
                self.app_item(
                    Section::Apps,
                    &Self::name_of(&path),
                    &path,
                    subtitle,
                    window,
                )
            })
            .collect()
    }

    fn running_items(&self) -> Vec<Item> {
        self.running
            .iter()
            .take(RUNNING_MAX)
            .map(|app| {
                let subtitle = if self.pins.contains(&app.path) {
                    host::t("Pinned · Running")
                } else {
                    host::t("Running · Alt+Q closes")
                };
                self.app_item(
                    Section::Running,
                    &app.name,
                    &app.path,
                    Some(subtitle.to_string()),
                    app.window,
                )
            })
            .collect()
    }

    fn app_items(&self, text: &str) -> Vec<Item> {
        let mut out = Vec::new();
        if text.eq_ignore_ascii_case("quit all") {
            out.push(Item {
                section: Section::Running,
                title: host::t("Quit all running apps").to_string(),
                subtitle: Some(
                    host::t("Asks every app's window to close, except Pulse").to_string(),
                ),
                icon: None,
                pin: None,
                window: 0,
                usage: None,
                kind: None,
                action: Action::QuitAll,
            });
        }
        let mut scored: Vec<(&App, i32)> = self
            .apps
            .iter()
            .filter_map(|app| {
                let bonus = self.usage.bonus(&format!("app:{}", app.path));
                score(text, &app.name).map(|points| (app, points + bonus))
            })
            .collect();
        scored.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then(a.0.name.chars().count().cmp(&b.0.name.chars().count()))
        });
        for (app, _) in scored.into_iter().take(APPS_MAX) {
            let subtitle = self
                .pins
                .contains(&app.path)
                .then(|| host::t("Pinned").to_string());
            out.push(self.app_item(Section::Apps, &app.name, &app.path, subtitle, 0));
        }
        // A running program the Start menu does not list (a portable app) is still findable.
        for app in &self.running {
            if out.len() > APPS_MAX {
                break;
            }
            let listed = out
                .iter()
                .any(|item| item.title.eq_ignore_ascii_case(&app.name));
            if !listed && score(text, &app.name).is_some() {
                out.push(self.app_item(
                    Section::Running,
                    &app.name,
                    &app.path,
                    Some(host::t("Running · Alt+Q closes").to_string()),
                    app.window,
                ));
            }
        }
        out
    }

    /// A unit conversion, else a sum; Return copies the result.
    fn answer_items(&self, text: &str) -> Vec<Item> {
        let money = self
            .rates
            .as_ref()
            .and_then(|rates| crate::rates::convert(text, rates));
        // The calculator and units switch (the Mac's `launcherCalculator`) covers units and sums;
        // money has its own switch (`launcherCurrency`).
        let calculator = host::switch_on("launcherCalculator");
        let (title, subtitle, copy) = if let Some(found) =
            calculator.then(|| launcher_calc::convert(text)).flatten()
        {
            (found.title, found.detail, found.copy)
        } else if let Some((title, detail)) = money {
            // The number alone is copied, as for units: the code after it holds no space.
            let copy = match title.rsplit_once(' ') {
                Some((number, _)) => number.to_string(),
                None => title.clone(),
            };
            (title, detail, copy)
        } else if let Some(answer) = calculator.then(|| launcher_calc::evaluate(text)).flatten() {
            let note = host::tf("{} — Return copies the result", &[text]);
            (answer.clone(), note, answer)
        } else {
            return Vec::new();
        };
        vec![Item {
            section: Section::Answer,
            title,
            subtitle: Some(subtitle),
            icon: None,
            pin: None,
            window: 0,
            usage: None,
            kind: None,
            action: Action::Copy(copy),
        }]
    }

    /// A typed http(s) address, or a path that exists.
    fn open_rows(text: &str) -> Vec<Item> {
        let lower = text.to_lowercase();
        let address = lower
            .strip_prefix("https://")
            .or(lower.strip_prefix("http://"));
        if address.is_some_and(|rest| !rest.is_empty() && !rest.contains(' ')) {
            return vec![Item {
                section: Section::Open,
                title: host::tf("Open {}", &[text]),
                subtitle: Some(host::t("In your default browser").to_string()),
                icon: None,
                pin: None,
                window: 0,
                usage: None,
                kind: None,
                action: Action::Open(text.to_string()),
            }];
        }
        let bytes = text.as_bytes();
        let drive = bytes.len() >= 3 && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/');
        let home = text.starts_with('~');
        if !(drive || home || text.starts_with("\\\\")) {
            return Vec::new();
        }
        let path = match (home, std::env::var_os("USERPROFILE")) {
            (true, Some(profile)) => {
                let rest = text[1..].trim_start_matches(['\\', '/']);
                PathBuf::from(profile).join(rest)
            }
            _ => PathBuf::from(text),
        };
        if !path.exists() {
            return Vec::new();
        }
        let shown = path.to_string_lossy().into_owned();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| shown.clone());
        vec![Item {
            section: Section::Open,
            title: host::tf("Open {}", &[&name]),
            subtitle: Some(shown.clone()),
            icon: Some(shown.clone()),
            pin: None,
            window: 0,
            usage: None,
            kind: None,
            action: Action::Open(shown),
        }]
    }

    fn plain_item(section: Section, title: String, subtitle: String, action: Action) -> Item {
        Item {
            section,
            title,
            subtitle: Some(subtitle),
            icon: None,
            pin: None,
            window: 0,
            usage: None,
            kind: None,
            action,
        }
    }

    /// The hub's quicklinks: a keyword and what follows it, or a name that matches.
    fn quicklink_rows(config: &Config, text: &str) -> Vec<Item> {
        let mut out = Vec::new();
        for link in config.quicklinks.iter().filter(|l| !l.template.is_empty()) {
            let Some(argument) = matches_item(text, &link.keyword, &link.name) else {
                continue;
            };
            let clipboard = clipboard_text().unwrap_or_default();
            let target = launcher_config::fill(
                &link.template,
                &[
                    ("query", &launcher_config::encode_query(&argument)),
                    ("clipboard", &clipboard),
                    ("date", &today()),
                ],
            );
            let name = if link.name.is_empty() {
                &link.keyword
            } else {
                &link.name
            };
            let title = if argument.is_empty() {
                name.clone()
            } else {
                format!("{name}: {argument}")
            };
            out.push(Self::plain_item(
                Section::Quicklinks,
                title,
                target.clone(),
                Action::Open(target),
            ));
            if out.len() >= LINKS_MAX {
                break;
            }
        }
        out
    }

    /// The hub's snippets: Return pastes the text into the app that was in front.
    fn snippet_rows(config: &Config, text: &str) -> Vec<Item> {
        let mut out = Vec::new();
        let snippets = &config.snippets;
        for snippet in snippets.iter().filter(|s| !s.body.is_empty()) {
            let Some(argument) = matches_item(text, &snippet.keyword, &snippet.name) else {
                continue;
            };
            let visiting: Vec<String> = [&snippet.name, &snippet.keyword]
                .iter()
                .map(|name| name.to_lowercase())
                .filter(|name| !name.is_empty())
                .collect();
            let nested = launcher_config::expand(&snippet.body, snippets, &visiting);
            let clipboard = clipboard_text().unwrap_or_default();
            let expanded = launcher_config::fill(
                &nested,
                &[
                    ("argument", &argument),
                    ("clipboard", &clipboard),
                    ("date", &today()),
                ],
            );
            let first_line: String = expanded
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(140)
                .collect();
            let title = if snippet.name.is_empty() {
                snippet.keyword.clone()
            } else {
                snippet.name.clone()
            };
            out.push(Self::plain_item(
                Section::Snippets,
                title,
                first_line,
                Action::Paste(expanded),
            ));
            if out.len() >= LINKS_MAX {
                break;
            }
        }
        out
    }

    /// The two closing rows for any typed text: ask ChatGPT, search the web. The text after
    /// "ask" or "ai" is the question when one of those starts it.
    fn action_items(&self, text: &str) -> Vec<Item> {
        let mut out = Vec::new();
        if text.is_empty() {
            return out;
        }
        // The text after "ask" or "ai" is the question when one of those starts it.
        let question = keyword_argument(text, "ask")
            .or_else(|| keyword_argument(text, "ai"))
            .filter(|question| !question.is_empty())
            .unwrap_or_else(|| text.to_string());
        // One Ask row, routed by `launcherAskTarget`.
        let shown: String = question.chars().take(80).collect();
        match host::launcher_ask_target() {
            "inLauncher" => out.push(Self::plain_item(
                Section::Actions,
                host::tf("Ask AI: {}", &[&shown]),
                host::t("Answered here by Pulse's assistant \u{00B7} Return asks").to_string(),
                Action::AiAsk(question),
            )),
            "chatgpt" => out.push(Self::plain_item(
                Section::Actions,
                host::tf("Ask ChatGPT: {}", &[&shown]),
                host::t("Opens ChatGPT with the question \u{00B7} Tab selects").to_string(),
                Action::AskChatGpt(question),
            )),
            _ => {}
        }
        if host::switch_on("launcherWebSearch") {
            let (name, base) = web_engine(&host::launcher_web_engine());
            let shown: String = text.chars().take(80).collect();
            out.push(Self::plain_item(
                Section::Actions,
                host::tf("Search {}: {}", &[name, &shown]),
                host::t("Opens the results in your default browser").to_string(),
                Action::Open(format!("{base}{}", launcher_config::encode_query(text))),
            ));
        }
        out
    }

    /// The AI chat history for "chat" and what follows it: pinned answers first, then newest.
    fn chat_items(&self, text: &str) -> Vec<Item> {
        if host::launcher_ask_target() != "inLauncher" {
            return Vec::new();
        }
        let Some(argument) = keyword_argument(text, "chat") else {
            return Vec::new();
        };
        let wanted = argument.to_lowercase();
        let mut shown: Vec<&crate::chat::Entry> = self
            .chats
            .iter()
            .filter(|entry| {
                wanted.is_empty()
                    || entry.question.to_lowercase().contains(&wanted)
                    || entry.answer.to_lowercase().contains(&wanted)
            })
            .collect();
        // A stable sort: the newest stays first within the pinned and the unpinned.
        shown.sort_by_key(|entry| !entry.pinned);
        if shown.is_empty() {
            return vec![Self::plain_item(
                Section::Chat,
                host::t("No AI chats yet").to_string(),
                host::t("Choose Ask AI after a question to start one").to_string(),
                Action::Inert,
            )];
        }
        shown
            .into_iter()
            .take(CHATS_MAX)
            .map(|entry| {
                let line: String = entry
                    .answer
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .chars()
                    .take(120)
                    .collect();
                let subtitle = if entry.pinned {
                    format!("{} \u{00B7} {line}", host::t("Pinned"))
                } else {
                    line
                };
                Item {
                    section: Section::Chat,
                    title: entry.question.chars().take(120).collect(),
                    subtitle: Some(subtitle),
                    icon: None,
                    pin: None,
                    window: 0,
                    usage: None,
                    kind: None,
                    action: Action::ChatOpen(entry.id.clone()),
                }
            })
            .collect()
    }

    /// Asks the assistant `question` on a thread. The panel shows "Asking" until the answer
    /// arrives (`WM_AI_ANSWER`, `take_ai_answer`).
    fn start_ai(&mut self, question: String) {
        self.runs = self.runs.wrapping_add(1);
        let run = self.runs;
        let shown: String = question.chars().take(60).collect();
        self.output = Some(Output {
            title: host::tf("Ask AI: {}", &[&shown]),
            lines: Vec::new(),
            top: 0,
            run,
            ask: Some(AskView::new(
                AskState::Pending,
                host::t("Asking Pulse's assistant\u{2026}"),
            )),
        });
        let spawned = std::thread::Builder::new()
            .name("launcher-ai".to_string())
            .spawn(move || {
                let answer = crate::chat::ask(&question);
                *AI_ANSWER.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some((run, question, answer));
                post(WM_AI_ANSWER);
            });
        if spawned.is_err()
            && let Some(output) = self.output.as_mut()
        {
            output.ask = Some(AskView::error(host::t("The assistant could not start.")));
        }
        self.draw();
    }

    /// Takes the assistant's answer. A good answer is kept in the history either way; it is shown
    /// when its run is still on show.
    fn take_ai_answer(&mut self) {
        let Some((run, question, answer)) = AI_ANSWER
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        else {
            return;
        };
        let view = match answer {
            Ok(reply) => {
                crate::chat::record(&mut self.chats, &question, &reply.answer);
                if let Some(dir) = launcher_dir() {
                    crate::chat::save(&dir, &self.chats);
                }
                let mut view = AskView::new(AskState::Answer, &reply.answer);
                view.provider = reply.provider;
                view
            }
            Err(reason) => AskView::error(&reason),
        };
        let Some(output) = self.output.as_mut().filter(|output| output.run == run) else {
            return;
        };
        output.lines = Vec::new();
        output.ask = Some(view);
        output.top = 0;
        if self.shown {
            self.draw();
        }
    }

    /// Shows a question from the history with its answer, as a command's output is shown.
    fn open_chat(&mut self, id: &str) {
        let Some(entry) = self.chats.iter().find(|entry| entry.id == id).cloned() else {
            return;
        };
        self.runs = self.runs.wrapping_add(1);
        self.output = Some(Output {
            title: entry.question.chars().take(80).collect(),
            lines: Vec::new(),
            top: 0,
            run: self.runs,
            ask: Some(AskView::new(AskState::Answer, &entry.answer)),
        });
        self.draw();
    }

    /// Alt+P on a history row: pinned, or unpinned when it was. Pinned answers stay listed first.
    fn pin_chat(&mut self) -> Option<Do> {
        let item = self.items.get(self.selected)?;
        let Action::ChatOpen(id) = &item.action else {
            return None;
        };
        if item.section != Section::Chat {
            return None;
        }
        let id = id.clone();
        let entry = self.chats.iter_mut().find(|entry| entry.id == id)?;
        entry.pinned = !entry.pinned;
        if let Some(dir) = launcher_dir() {
            crate::chat::save(&dir, &self.chats);
        }
        self.recompute();
        Some(Do::Redraw)
    }

    /// "define word": one row that looks the word up when it is chosen. Only the typed word
    /// after "define " is ever sent, and only on Return (`launcher_dictionary`).
    fn dictionary_items(&self, text: &str) -> Vec<Item> {
        if !host::switch_on("launcherDictionary") {
            return Vec::new();
        }
        let Some(argument) = keyword_argument(text, "define") else {
            return Vec::new();
        };
        let Some(word) = dictionary::word_of(&argument) else {
            return Vec::new();
        };
        vec![Self::plain_item(
            Section::Dictionary,
            host::tf("Define {}", &[&word]),
            host::t("Return looks the word up in an online dictionary").to_string(),
            Action::Define(word),
        )]
    }

    /// Looks `word` up on a thread. The panel shows "Looking up" until the entry arrives
    /// (`WM_DEFINE`, `take_definition`).
    fn start_define(&mut self, word: String) {
        self.runs = self.runs.wrapping_add(1);
        let run = self.runs;
        self.output = Some(Output {
            title: host::tf("Define {}", &[&word]),
            lines: vec![host::t("Looking up\u{2026}").to_string()],
            top: 0,
            run,
            ask: None,
        });
        let spawned = std::thread::Builder::new()
            .name("launcher-define".to_string())
            .spawn(move || {
                let found = dictionary::lookup(&word);
                *DEFINE_ANSWER.lock().unwrap_or_else(PoisonError::into_inner) = Some((run, found));
                post(WM_DEFINE);
            });
        if spawned.is_err()
            && let Some(output) = self.output.as_mut()
        {
            output.lines = vec![host::t("The lookup could not start.").to_string()];
        }
        self.draw();
    }

    /// Shows the entry a lookup left, when its run is still on show.
    fn take_definition(&mut self) {
        let Some((run, found)) = DEFINE_ANSWER
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        else {
            return;
        };
        let Some(output) = self.output.as_mut().filter(|output| output.run == run) else {
            return;
        };
        output.lines = match found {
            dictionary::Found::Entry(lines) => lines,
            dictionary::Found::NoEntry => {
                vec![host::t("The dictionary has no entry for that word.").to_string()]
            }
            dictionary::Found::Unavailable => vec![
                host::t("The dictionary could not be reached. Check the connection and try again.")
                    .to_string(),
            ],
        };
        output.top = 0;
        if self.shown {
            self.draw();
        }
    }

    // ------------------------------------------------------------ note editor

    /// Opens a note file in the panel. False when it cannot be edited here (not UTF-8, or longer
    /// than `EDITOR_MAX`): the caller opens it in its own program instead.
    fn open_editor(&mut self, path: &Path) -> bool {
        let Ok(bytes) = std::fs::read(path) else {
            return false;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            return false;
        };
        let text: Vec<char> = text.chars().filter(|c| *c != '\r').collect();
        if text.len() > EDITOR_MAX {
            return false;
        }
        self.runs = self.runs.wrapping_add(1);
        self.output = None;
        self.editor = Some(Editor {
            path: path.to_path_buf(),
            caret: text.len(),
            text,
            top: 0,
        });
        self.draw();
        true
    }

    /// Return on a note row: it opens in the panel, or in its own program with the switch off or
    /// when the panel cannot hold it. True when it opened in the panel.
    fn edit_note(&mut self, path: &str) -> bool {
        host::switch_on("launcherNoteEditor") && self.open_editor(Path::new(path))
    }

    /// Makes a note that starts with `text` and opens it in the panel.
    fn new_note(&mut self, text: &str) {
        let made = launcher_dir()
            .and_then(|base| launcher_notes::create(&base, text, now_seconds() as u64));
        match made {
            Some(path) if self.open_editor(&path) => {}
            Some(path) => {
                self.hide(false);
                run(&Action::Open(path.to_string_lossy().into_owned()));
            }
            None => host::log_info("launcher", &[("note", "not_made")]),
        }
    }

    /// Return on a trashed note: it goes back among the notes and opens in the panel.
    fn restore_note(&mut self, path: &str) {
        let Some(base) = launcher_dir() else {
            return;
        };
        if launcher_notes::restore(&base, Path::new(path))
            && let Some(note) = launcher_notes::list(&base, false, "").into_iter().next()
            && !self.open_editor(&note.path)
        {
            self.hide(false);
            run(&Action::Open(note.path.to_string_lossy().into_owned()));
        }
    }

    /// Ends the editing without drawing: a note left blank goes to the trash (the Mac drops an
    /// empty new note), any other is already saved after each change but is written once more.
    fn finish_editor(&mut self) {
        let Some(editor) = self.editor.take() else {
            return;
        };
        if editor.blank() {
            if let Some(base) = launcher_dir() {
                launcher_notes::delete(&base, &editor.path, now_seconds() as u64);
            }
        } else if !editor.save() {
            host::log_info("launcher", &[("note", "not_saved")]);
        }
    }

    /// Esc in the editor: back to the results.
    fn close_editor(&mut self) -> Do {
        self.finish_editor();
        self.recompute();
        Do::Redraw
    }

    /// Alt+D or Alt+Backspace in the editor: the note goes to the trash and the results return.
    fn delete_open_note(&mut self) -> Do {
        if let Some(editor) = self.editor.take()
            && let Some(base) = launcher_dir()
        {
            launcher_notes::delete(&base, &editor.path, now_seconds() as u64);
        }
        self.recompute();
        Do::Redraw
    }

    /// Writes the open note after a change.
    fn editor_changed(&mut self) -> Do {
        if let Some(editor) = self.editor.as_ref()
            && !editor.save()
        {
            host::log_info("launcher", &[("note", "not_saved")]);
        }
        Do::Redraw
    }

    /// A typed character in the editor (Enter arrives as a carriage return).
    fn editor_typed(&mut self, unit: u16) -> Do {
        let c = match (self.high.take(), unit) {
            (Some(high), 0xDC00..=0xDFFF) => char::decode_utf16([high, unit]).next(),
            (_, 0xD800..=0xDBFF) => {
                self.high = Some(unit);
                return Do::Nothing;
            }
            _ => char::decode_utf16([unit]).next(),
        };
        let Some(Ok(c)) = c else {
            return Do::Nothing;
        };
        let c = if c == '\r' { '\n' } else { c };
        if c != '\n' && (c < ' ' || c == '\u{7f}') {
            return Do::Nothing;
        }
        let added = self
            .editor
            .as_mut()
            .is_some_and(|editor| editor.insert(&c.to_string()));
        if added {
            self.editor_changed()
        } else {
            Do::Nothing
        }
    }

    /// The editor's keys: Escape returns to the results, the arrows, Home and End move the caret,
    /// Backspace and Delete remove, Ctrl+V pastes.
    fn editor_key(&mut self, vk: u32, ctrl: bool) -> Do {
        if vk == VK_ESCAPE {
            return self.close_editor();
        }
        let Some(editor) = self.editor.as_mut() else {
            return Do::Nothing;
        };
        match vk {
            VK_UP => editor.vertical(-1),
            VK_DOWN => editor.vertical(1),
            VK_LEFT => editor.caret = editor.caret.saturating_sub(1),
            VK_RIGHT => editor.caret = (editor.caret + 1).min(editor.text.len()),
            VK_HOME => editor.home(ctrl),
            VK_END => editor.end(ctrl),
            VK_BACK => {
                return if editor.backspace(ctrl) {
                    self.editor_changed()
                } else {
                    Do::Nothing
                };
            }
            VK_DELETE => {
                return if editor.delete() {
                    self.editor_changed()
                } else {
                    Do::Nothing
                };
            }
            VK_V if ctrl => {
                let Some(text) = clipboard_text() else {
                    return Do::Nothing;
                };
                let text = text.replace("\r\n", "\n").replace('\r', "\n");
                return if editor.insert(&text) {
                    self.editor_changed()
                } else {
                    Do::Nothing
                };
            }
            _ => return Do::Nothing,
        }
        Do::Redraw
    }

    /// The editor in place of the field and the list: the note's rows, the caret in them.
    fn render_editor(&mut self, canvas: &mut Canvas, (w, h): (f32, f32)) {
        let s = self.scale;
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        let line_height = OUTPUT_LINE * s;
        let first = (FIELD_HEIGHT + OUTPUT_PAD) * s;
        let room = ((h - first - OUTPUT_PAD * s) / line_height)
            .floor()
            .max(1.0) as usize;
        let rows = editor.rows();
        let at = editor.caret_row(&rows);
        // Keep the caret's row in view.
        if at < editor.top {
            editor.top = at;
        } else if at >= editor.top + room {
            editor.top = at + 1 - room;
        }
        let top = editor.top;
        let column = editor.caret - rows[at].0;
        let shown: Vec<String> = rows
            .iter()
            .skip(top)
            .take(room)
            .map(|(from, to, _)| {
                editor.text[*from..*to]
                    .iter()
                    .collect::<String>()
                    .trim_end_matches('\n')
                    .to_string()
            })
            .collect();
        let before: String = editor.text[rows[at].0..rows[at].0 + column]
            .iter()
            .collect();
        let width = w - 2.0 * FIELD_PAD * s;
        if let Some(mask) = self.mask(host::t("Note"), FIELD_TEXT, false) {
            Self::draw_text(
                canvas,
                &mask,
                FIELD_PAD * s,
                FIELD_HEIGHT * s / 2.0,
                width,
                1.0,
            );
        }
        let hint = host::t(
            "Saved as you type \u{00B7} Esc returns to search \u{00B7} Alt+D deletes the note",
        );
        if let Some(mask) = self.mask(hint, SUBTITLE_TEXT, false) {
            let x = w - FIELD_PAD * s - mask.width as f32;
            Self::draw_text(canvas, &mask, x, FIELD_HEIGHT * s / 2.0, width, 0.35);
        }
        canvas.fill_round_rect(
            0.0,
            FIELD_HEIGHT * s,
            w,
            s.max(1.0),
            [0.0; 4],
            INK,
            DIVIDER_ALPHA,
        );
        for (row, line) in shown.iter().enumerate() {
            if let Some(mask) = self.mask(line, EDITOR_TEXT, false) {
                let mid = first + (row as f32 + 0.5) * line_height;
                Self::draw_text(canvas, &mask, FIELD_PAD * s, mid, width, 0.9);
            }
        }
        if !self.still {
            // A trailing space has no ink of its own: measure it with a mark after it.
            let marked = self.mask(&format!("{before}|"), EDITOR_TEXT, false);
            let mark = self.mask("|", EDITOR_TEXT, false);
            let mut caret_x = FIELD_PAD * s;
            if let (Some(marked), Some(mark)) = (marked, mark) {
                caret_x += marked.width.saturating_sub(mark.width) as f32;
            }
            let mid = first + ((at - top) as f32 + 0.5) * line_height;
            let height = EDITOR_TEXT * 1.2 * s;
            canvas.fill_round_rect(
                caret_x.min(w - FIELD_PAD * s),
                mid - height / 2.0,
                (1.5 * s).max(1.0),
                height,
                [0.0; 4],
                INK,
                0.9,
            );
        }
    }

    /// The Alt+Space question: three rows, each an answer.
    fn prompt_items(holder: &str) -> Vec<Item> {
        vec![
            Self::plain_item(
                Section::Hotkey,
                host::t("Use Alt+Space for Pulse").to_string(),
                host::tf("Pulse opens first; {} no longer gets the key", &[holder]),
                Action::Conflict(Conflict::Pulse),
            ),
            Self::plain_item(
                Section::Hotkey,
                host::tf("Keep it for {}", &[holder]),
                host::t("Pulse leaves the key alone; open the launcher from the tool wheel")
                    .to_string(),
                Action::Conflict(Conflict::Other),
            ),
            Self::plain_item(
                Section::Hotkey,
                host::t("Choose another key").to_string(),
                host::t("Opens Settings, Launcher in the hub").to_string(),
                Action::Conflict(Conflict::Choose),
            ),
        ]
    }

    /// Moves the selection onto the action rows (Tab) or back (Shift+Tab). Tab on the first
    /// action row goes to the next; the two take turns. True when something changed.
    fn tab(&mut self, back: bool) -> bool {
        let actions: Vec<usize> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.section == Section::Actions)
            .map(|(position, _)| position)
            .collect();
        if actions.is_empty()
            || self.output.is_some()
            || self.editor.is_some()
            || self.prompt.is_some()
        {
            return false;
        }
        let at = actions.iter().position(|row| *row == self.selected);
        self.selected = match (at, back) {
            (None, false) => {
                self.before_tab = self.selected;
                actions[0]
            }
            (None, true) => return false,
            (Some(at), false) => actions[(at + 1) % actions.len()],
            (Some(0), true) => self.before_tab.min(self.items.len().saturating_sub(1)),
            (Some(at), true) => actions[at - 1],
        };
        true
    }

    /// Notes: "note text" starts one, "notes" lists them, "notes word" searches them and
    /// "notes trash" lists the deleted ones.
    fn note_rows(config: &Config, text: &str) -> Vec<Item> {
        if config.notes_off {
            return Vec::new();
        }
        let Some(base) = launcher_dir() else {
            return Vec::new();
        };
        let now = now_seconds() as u64;
        let mut out = Vec::new();
        if let Some(start) = keyword_argument(text, "note") {
            let subtitle = if start.is_empty() {
                host::t("Return starts an empty note").to_string()
            } else {
                start.chars().take(140).collect()
            };
            out.push(Self::plain_item(
                Section::Notes,
                host::t("New note").to_string(),
                subtitle,
                Action::NoteNew(start),
            ));
            return out;
        }
        let Some(wanted) = keyword_argument(text, "notes") else {
            return out;
        };
        let trash = wanted.eq_ignore_ascii_case("trash");
        let query = if trash { "" } else { wanted.as_str() };
        for note in launcher_notes::list(&base, trash, query)
            .into_iter()
            .take(NOTES_MAX)
        {
            let path = note.path.to_string_lossy().into_owned();
            let when = launcher_notes::ago(note.at, now);
            let (subtitle, action) = if trash {
                (
                    host::tf("Deleted {} \u{00B7} Return restores", &[&when]),
                    Action::NoteRestore(path),
                )
            } else {
                let edited = if note.pinned {
                    host::tf(
                        "Edited {} \u{00B7} Alt+P unpins \u{00B7} Alt+D deletes",
                        &[&when],
                    )
                } else {
                    host::tf(
                        "Edited {} \u{00B7} Alt+P pins \u{00B7} Alt+D deletes",
                        &[&when],
                    )
                };
                (
                    if note.pinned {
                        host::tf("Pinned \u{00B7} {}", &[&edited])
                    } else {
                        edited
                    },
                    Action::NoteEdit(path),
                )
            };
            out.push(Self::plain_item(
                Section::Notes,
                note.title,
                subtitle,
                action,
            ));
        }
        // Nothing to list: offer to start one (the trash just shows no matches).
        if out.is_empty() && !trash {
            out.push(Self::plain_item(
                Section::Notes,
                host::t("New note").to_string(),
                host::t("No notes yet \u{00B7} Return starts one").to_string(),
                Action::NoteNew(String::new()),
            ));
        }
        out
    }

    /// Alt+P on a note: it is pinned, or unpinned when it was, and listed first from then on.
    /// `None` when the selected row is not a note.
    fn pin_note(&mut self) -> Option<Do> {
        let item = self.items.get(self.selected)?;
        let Action::NoteEdit(path) = &item.action else {
            return None;
        };
        if item.section != Section::Notes {
            return None;
        }
        let base = launcher_dir()?;
        let path = PathBuf::from(path);
        let pinned = launcher_notes::list(&base, false, "")
            .iter()
            .any(|note| note.path == path && note.pinned);
        launcher_notes::set_pinned(&base, &path, !pinned);
        self.recompute();
        Some(Do::Redraw)
    }

    /// Alt+D on a note: it goes to the notes' trash and the list is read again.
    fn delete_note(&mut self) -> Option<Do> {
        let item = self.items.get(self.selected)?;
        let Action::NoteEdit(path) = &item.action else {
            return None;
        };
        if item.section != Section::Notes {
            return None;
        }
        let base = launcher_dir()?;
        launcher_notes::delete(&base, Path::new(path), now_seconds() as u64);
        self.recompute();
        Some(Do::Redraw)
    }

    /// Files in the chosen folders whose name holds the typed text, shortest name first.
    /// The file rows the search thread last found, when they are for exactly this text. The
    /// search itself never runs here (`queue_search`).
    fn file_items(&self, text: &str) -> Vec<Item> {
        if self.file_text == text {
            self.file_rows.clone()
        } else {
            Vec::new()
        }
    }

    /// Asks for the rows and the file search for the typed text once the keys have paused for
    /// `QUERY_MS`: a newer call makes an older search stop and drop its answer.
    fn queue_search(&mut self) {
        FILE_GEN.fetch_add(1, Ordering::AcqRel);
        // SAFETY: a timer on this thread's own window; setting it again restarts it.
        let _ = unsafe { SetTimer(Some(self.window.hwnd()), QUERY_TIMER, QUERY_MS, None) };
    }

    /// The pause is over: starts the rows that read the disk or the clipboard, and the file
    /// search, for what is typed now. Each is shown when it arrives.
    fn run_searches(&mut self) {
        self.run_slow_rows();
        self.run_file_search();
    }

    /// Computes the slow rows for the typed text on a thread (`WM_SLOW_ROWS` when they are in).
    fn run_slow_rows(&mut self) {
        let text = self.text();
        if text.is_empty() {
            self.slow = None;
            return;
        }
        let config = self.config.clone();
        let generation = FILE_GEN.load(Ordering::Acquire);
        let spawned = std::thread::Builder::new()
            .name("launcher-rows".to_string())
            .spawn(move || {
                let rows = Slow {
                    text: text.clone(),
                    open: Panel::open_rows(&text),
                    quicklinks: Panel::quicklink_rows(&config, &text),
                    snippets: Panel::snippet_rows(&config, &text),
                    notes: Panel::note_rows(&config, &text),
                    clips: Panel::clip_rows(&text),
                    storage: Panel::storage_rows(&text),
                };
                if FILE_GEN.load(Ordering::Acquire) != generation {
                    return;
                }
                *SLOW_ROWS.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some((generation, rows));
                post(WM_SLOW_ROWS);
            });
        if spawned.is_err() {
            host::log_info("launcher", &[("rows", "no_thread")]);
        }
    }

    /// Takes the slow rows once their thread has them, if they are still for the text typed.
    fn take_slow_rows(&mut self) {
        let Some((generation, rows)) = SLOW_ROWS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        else {
            return;
        };
        if generation != FILE_GEN.load(Ordering::Acquire) || !self.shown || self.text() != rows.text
        {
            return;
        }
        self.slow = Some(rows);
        if self.output.is_none() && self.prompt.is_none() {
            self.recompute();
            self.draw();
        }
    }

    /// The slow rows held for `text`: none while they are computed or when they were for other text.
    fn held(&self, text: &str, pick: impl Fn(&Slow) -> &Vec<Item>) -> Vec<Item> {
        self.slow
            .as_ref()
            .filter(|slow| slow.text == text)
            .map_or_else(Vec::new, |slow| pick(slow).clone())
    }

    /// The default file-name search (S12): Windows Search, asked through the engine's
    /// `files.search_names`, which reads rightkit-fsindex. It covers only the folders Windows
    /// indexes. `None` when the engine cannot answer or does not answer within 3 s (a stuck engine
    /// ends the search thread instead of piling up); no rows when Windows Search is not running.
    fn windows_search_files(text: &str, limit: usize) -> Option<Vec<launcher_files::Found>> {
        let reply = host::engine_call_within(
            "files.search_names",
            serde_json::json!({"query": text, "limit": limit}),
            std::time::Duration::from_secs(3),
        )
        .ok()?;
        if reply.get("ready").and_then(serde_json::Value::as_bool) != Some(true) {
            return Some(Vec::new());
        }
        let rows = reply.get("rows").and_then(serde_json::Value::as_array)?;
        Some(
            rows.iter()
                .filter(|row| row.get("is_dir").and_then(serde_json::Value::as_bool) != Some(true))
                .filter_map(|row| {
                    let path = row.get("path")?.as_str()?;
                    let file = Path::new(path);
                    Some(launcher_files::Found {
                        name: file.file_name()?.to_string_lossy().into_owned(),
                        path: path.to_string(),
                        folder: file.parent()?.to_string_lossy().into_owned(),
                    })
                })
                .collect(),
        )
    }

    /// The pause is over: starts the search for what is typed now. The walk index answers once
    /// it is built; with the walk on (`hubFileIndex`) and still building, the search waits for it.
    /// Otherwise Windows Search answers (S12).
    fn run_file_search(&mut self) {
        let text = self.text();
        if text.chars().count() < 2 {
            self.file_rows.clear();
            self.file_text.clear();
            return;
        }
        let index = self.files.clone();
        if index.is_none() && !host::switch_on("launcherFileResults") {
            self.file_rows.clear();
            self.file_text.clear();
            return;
        }
        if index.is_none() && host::switch_on("hubFileIndex") {
            return;
        }
        let generation = FILE_GEN.load(Ordering::Acquire);
        let spawned = std::thread::Builder::new()
            .name("launcher-find".to_string())
            .spawn(move || {
                let stale = || FILE_GEN.load(Ordering::Acquire) != generation;
                let (found, capped) = match index {
                    Some(index) => {
                        let wanted = text.to_lowercase();
                        let holds = |_: &str, name: &str| {
                            name.to_lowercase()
                                .contains(&wanted)
                                .then(|| -(name.chars().count() as i32))
                        };
                        let Some(found) = index.search(&text, FILES_MAX, holds, stale) else {
                            return;
                        };
                        (found, index.capped())
                    }
                    None => {
                        let Some(found) = Self::windows_search_files(&text, FILES_MAX) else {
                            return;
                        };
                        (found, false)
                    }
                };
                for hit in &found {
                    warm_icon(&hit.path);
                }
                if stale() {
                    return;
                }
                *FILE_ROWS.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some((generation, text, found, capped));
                post(WM_FILE_ROWS);
            });
        if spawned.is_err() {
            host::log_info("launcher", &[("files", "no_thread")]);
        }
    }

    /// Takes a finished file search if it is still the newest and the panel shows its text.
    fn take_file_rows(&mut self) {
        let Some((generation, text, found, capped)) = FILE_ROWS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        else {
            return;
        };
        if generation != FILE_GEN.load(Ordering::Acquire) || !self.shown || self.text() != text {
            return;
        }
        let mut rows: Vec<Item> = found
            .into_iter()
            .map(|found| Item {
                section: Section::Files,
                title: found.name,
                subtitle: Some(found.folder),
                icon: Some(found.path.clone()),
                pin: None,
                window: 0,
                usage: None,
                kind: None,
                action: Action::Open(found.path),
            })
            .collect();
        // The index stopped at its cap: say so, or a missing file reads as not there.
        if capped {
            rows.push(Self::plain_item(
                Section::Files,
                host::t("File search stopped at its memory limit; some files are not listed")
                    .to_string(),
                host::t("Choose narrower folders in the hub under Settings, Launcher").to_string(),
                Action::Inert,
            ));
        }
        self.file_rows = rows;
        self.file_text = text;
        if self.output.is_none() && self.prompt.is_none() {
            self.recompute();
            self.draw();
        }
    }

    /// Starts reading the chosen folders' file names (the panel has just opened).
    fn start_files(&mut self) {
        self.files = None;
        self.files_job = None;
        self.file_rows.clear();
        self.file_text.clear();
        // The walk runs when the hub's file index switch is on (Pulse's own index, opt-in) or folders
        // were chosen for it; the walk then covers the home folder, the Start Menu, the chosen
        // volumes and folders. Otherwise file rows come from Windows Search (S12, the default).
        if self.config.file_folders.is_empty() && !host::switch_on("hubFileIndex") {
            return;
        }
        // The index kept from an earlier open still fits the settings and is under ten minutes old.
        if let Some(index) = launcher_files::cached(&self.config.file_folders) {
            self.files = Some(Arc::new(index));
            return;
        }
        self.files_job = launcher_files::build_in_background(self.config.file_folders.clone());
        if self.files_job.is_some() {
            // SAFETY: a timer on this thread's own window.
            let _ = unsafe { SetTimer(Some(self.window.hwnd()), FILES_TIMER, FILES_MS, None) };
        }
    }

    /// Takes the file names once their thread has finished; true when they arrived.
    fn take_files(&mut self) -> bool {
        if !self.files_job.as_ref().is_some_and(|job| job.is_finished()) {
            return false;
        }
        self.files = self
            .files_job
            .take()
            .and_then(|job| job.join().ok())
            .map(Arc::new);
        true
    }

    /// Reads the exchange rates just saved; a panel that is open answers with them at once.
    fn take_rates(&mut self) {
        if !host::switch_on("launcherCurrency") {
            return;
        }
        let Some(dir) = launcher_dir() else {
            return;
        };
        self.rates = crate::rates::load(&dir);
        if self.shown && self.output.is_none() {
            self.recompute();
            self.draw();
        }
    }

    /// The hub's custom commands: a keyword, or a name that matches.
    fn command_items(&self, text: &str) -> Vec<Item> {
        let mut out = Vec::new();
        for command in self.config.commands.iter().filter(|c| !c.line.is_empty()) {
            if matches_item(text, &command.keyword, &command.name).is_none() {
                continue;
            }
            let title = if command.name.is_empty() {
                command.keyword.clone()
            } else {
                command.name.clone()
            };
            let shown: String = command.line.chars().take(140).collect();
            out.push(Self::plain_item(
                Section::Commands,
                title,
                shown,
                Action::Command(command.line.clone()),
            ));
            if out.len() >= LINKS_MAX {
                break;
            }
        }
        out
    }

    /// Starts `line` and shows "Running" until its thread reports.
    fn start_command(&mut self, title: String, line: String) {
        self.runs = self.runs.wrapping_add(1);
        let run = self.runs;
        self.output = Some(Output {
            title,
            lines: vec![host::t("Running\u{2026}").to_string()],
            top: 0,
            run,
            ask: None,
        });
        launcher_commands::spawn(line, move |outcome| {
            *OUTCOME.lock().unwrap_or_else(PoisonError::into_inner) = Some((run, outcome));
            post(WM_OUTPUT);
        });
        self.draw();
    }

    /// Shows the outcome left by a command's thread, if it is for the run on show.
    fn take_outcome(&mut self) {
        let Some((run, outcome)) = OUTCOME
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        else {
            return;
        };
        let Some(output) = self.output.as_mut().filter(|output| output.run == run) else {
            return;
        };
        let text = match outcome {
            launcher_commands::Outcome::Output(text) | launcher_commands::Outcome::Failed(text) => {
                text
            }
            launcher_commands::Outcome::TimedOut => {
                host::t("Stopped: the command ran for more than 30 seconds.").to_string()
            }
        };
        output.lines = text.lines().map(str::to_string).collect();
        output.top = 0;
        if self.shown {
            self.draw();
        }
    }

    /// The output view's keys: Escape goes back to the list, the arrows scroll.
    fn output_key(&mut self, vk: u32) -> Do {
        if vk == VK_ESCAPE {
            self.output = None;
            return Do::Redraw;
        }
        let Some(output) = self.output.as_mut() else {
            return Do::Nothing;
        };
        match vk {
            VK_UP => output.top = output.top.saturating_sub(1),
            VK_DOWN => {
                let len = output
                    .ask
                    .as_ref()
                    .map_or(output.lines.len(), |view| view.rows.len());
                output.top = (output.top + 1).min(len.saturating_sub(1));
            }
            VK_HOME => output.top = 0,
            _ => return Do::Nothing,
        }
        Do::Redraw
    }

    /// What a command printed, in place of the field and the list.
    fn render_output(&mut self, canvas: &mut Canvas, (w, h): (f32, f32)) {
        if self
            .output
            .as_ref()
            .is_some_and(|output| output.ask.is_some())
        {
            self.render_ask(canvas, (w, h));
            return;
        }
        let s = self.scale;
        let Some(output) = self.output.as_ref() else {
            return;
        };
        let (title, top) = (output.title.clone(), output.top);
        let line_height = OUTPUT_LINE * s;
        let first = (FIELD_HEIGHT + OUTPUT_PAD) * s;
        let room = ((h - first - OUTPUT_PAD * s) / line_height)
            .floor()
            .max(1.0) as usize;
        let lines: Vec<String> = output.lines.iter().skip(top).take(room).cloned().collect();
        let width = w - 2.0 * FIELD_PAD * s;
        if let Some(mask) = self.mask(&title, FIELD_TEXT, false) {
            Self::draw_text(
                canvas,
                &mask,
                FIELD_PAD * s,
                FIELD_HEIGHT * s / 2.0,
                width,
                1.0,
            );
        }
        if let Some(mask) = self.mask(host::t("Esc goes back"), SUBTITLE_TEXT, false) {
            let x = w - FIELD_PAD * s - mask.width as f32;
            Self::draw_text(canvas, &mask, x, FIELD_HEIGHT * s / 2.0, width, 0.35);
        }
        canvas.fill_round_rect(
            0.0,
            FIELD_HEIGHT * s,
            w,
            s.max(1.0),
            [0.0; 4],
            INK,
            DIVIDER_ALPHA,
        );
        for (row, line) in lines.iter().enumerate() {
            if let Some(mask) = self.mask(line, OUTPUT_TEXT, false) {
                let mid = first + (row as f32 + 0.5) * line_height;
                Self::draw_text(canvas, &mask, FIELD_PAD * s, mid, width, 0.85);
            }
        }
    }

    /// An Ask AI output: the question as a compact semibold header, a secondary provider label
    /// when supplied, then the answer as wrapped structured rows. The rows are laid out again only
    /// when the width changes, keeping the block on show; scrolling counts rows.
    fn render_ask(&mut self, canvas: &mut Canvas, (w, h): (f32, f32)) {
        let s = self.scale;
        let Some(output) = self.output.as_mut() else {
            return;
        };
        let title = output.title.clone();
        let mut top = output.top;
        let Some(mut view) = output.ask.take() else {
            return;
        };
        let width = w - 2.0 * FIELD_PAD * s;
        let key = width.round() as i32;
        if view.laid_for != key {
            let anchor = response_layout::block_at_row(&view.rows, top);
            let had_rows = !view.rows.is_empty();
            let rows =
                response_layout::layout(&view.doc, width, ASK_INDENT * s, &mut |text, role| {
                    let (size, bold) = ask_face(role);
                    let px = (size * self.scale).round() as i32;
                    self.painter.measure(text, px, bold) as f32
                });
            view.rows = rows;
            view.laid_for = key;
            if had_rows {
                top = response_layout::row_of_block(&view.rows, anchor);
            }
        }
        top = top.min(view.rows.len().saturating_sub(1));
        let line_height = ASK_LINE * s;
        let mut first = (FIELD_HEIGHT + OUTPUT_PAD) * s;
        let hint = self.mask(host::t("Esc goes back"), SUBTITLE_TEXT, false);
        let hint_room = hint
            .as_ref()
            .map_or(0.0, |mask| mask.width as f32 + 12.0 * s);
        if let Some(mask) = self.mask(&title, ASK_TITLE, true) {
            Self::draw_text(
                canvas,
                &mask,
                FIELD_PAD * s,
                FIELD_HEIGHT * s / 2.0,
                width - hint_room,
                1.0,
            );
        }
        if let Some(mask) = hint {
            let x = w - FIELD_PAD * s - mask.width as f32;
            Self::draw_text(canvas, &mask, x, FIELD_HEIGHT * s / 2.0, width, 0.35);
        }
        canvas.fill_round_rect(
            0.0,
            FIELD_HEIGHT * s,
            w,
            s.max(1.0),
            [0.0; 4],
            INK,
            DIVIDER_ALPHA,
        );
        if let Some(provider) = view.provider.as_deref()
            && let Some(mask) = self.mask(provider, ASK_META, false)
        {
            Self::draw_text(
                canvas,
                &mask,
                FIELD_PAD * s,
                first + line_height / 2.0,
                width,
                0.5,
            );
            first += line_height;
        }
        let room = ((h - first - OUTPUT_PAD * s) / line_height)
            .floor()
            .max(1.0) as usize;
        for (row, line) in view.rows.iter().skip(top).take(room).enumerate() {
            if line.text.is_empty() {
                continue;
            }
            let (size, bold) = ask_face(line.role);
            // The progress line is secondary until the answer replaces it.
            let calm = if view.state == AskState::Pending {
                0.6
            } else {
                1.0
            };
            let alpha = calm
                * match line.role {
                    response_layout::Role::Title | response_layout::Role::Heading => 0.95,
                    response_layout::Role::Body => 0.85,
                    response_layout::Role::Metadata => 0.5,
                    response_layout::Role::Code => 0.8,
                };
            if let Some(mask) = self.mask(&line.text, size, bold) {
                let mid = first + (row as f32 + 0.5) * line_height;
                Self::draw_text(
                    canvas,
                    &mask,
                    FIELD_PAD * s + line.indent,
                    mid,
                    width - line.indent,
                    alpha,
                );
            }
        }
        if let Some(output) = self.output.as_mut() {
            output.top = top;
            output.ask = Some(view);
        }
    }

    /// Pulse's own pages, and the notch's reading for each account.
    fn pulse_items(&self, text: &str) -> Vec<Item> {
        let pages: [(&str, &str, &str); 7] = [
            (
                host::t("Open Storage"),
                "storage",
                host::t("Pulse: what fills the drives"),
            ),
            (
                host::t("Open Cleanup"),
                "cleanup",
                host::t("Pulse: what can be removed"),
            ),
            (
                host::t("Open Monitor"),
                "monitor",
                host::t("Pulse: processor, memory, network"),
            ),
            (
                host::t("Open Apps"),
                "apps",
                host::t("Pulse: installed apps and updates"),
            ),
            (
                host::t("Open Settings"),
                "settings",
                host::t("Pulse: settings"),
            ),
            (
                host::t("Open Appearance"),
                "appearance",
                host::t("Pulse: appearance"),
            ),
            (
                host::t("Open Notifications"),
                "notifications",
                host::t("Pulse: notifications"),
            ),
        ];
        // The Pulse commands switch (the Mac's `launcherPulseCommands`) covers the pages, the
        // usage readings and the disk and cleanup rows.
        if text.chars().count() < 2 || !host::switch_on("launcherPulseCommands") {
            return Vec::new();
        }
        let mut out: Vec<Item> = pages
            .iter()
            .filter(|(name, _, _)| score(text, name).is_some())
            .take(PULSE_MAX)
            .map(|&(name, page, note)| Item {
                section: Section::Pulse,
                title: name.to_string(),
                subtitle: Some(note.to_string()),
                icon: None,
                pin: None,
                window: 0,
                usage: None,
                kind: None,
                action: Action::Hub(page),
            })
            .collect();
        // The clipboard's clear rows, while the history is on (the Mac's "Clear clipboard ...").
        if host::switch_on("launcherClipboard") {
            for scope in rightkit_clipboard_history::model::ClearScope::ALL {
                if score(text, scope.title()).is_some() {
                    out.push(clip_clear_item(scope));
                }
            }
        }
        // The Mac's "Claude usage" and "Codex usage": the live reading, found by the title or
        // by the start of one of its words.
        let typed = text.trim().to_lowercase();

        for (title, words, reading) in [
            (
                host::tf("{} usage", &["Claude"]),
                ["claude", "usage", "limit", "anthropic"],
                0usize,
            ),
            (
                host::tf("{} usage", &["Codex"]),
                ["codex", "usage", "limit", "openai"],
                1usize,
            ),
        ] {
            if out.len() >= PULSE_MAX {
                break;
            }
            let by_word = !typed.is_empty() && words.iter().any(|word| word.starts_with(&typed));
            if score(text, &title).is_some() || by_word {
                out.push(Self::plain_item(
                    Section::Pulse,
                    title,
                    host::usage_line(reading),
                    Action::Hub("accounts"),
                ));
            }
        }
        out
    }

    /// The Mac's disk rows: one per fixed drive with its used share, for a word that names the
    /// drive or a storage word ("disk", "storage", "space", "free", "clean", "cleanup"), and for
    /// those words the saved cleanup scan. Reads the drive thread's last reading and the saved
    /// findings file only: nothing here starts a scan.
    fn storage_rows(text: &str) -> Vec<Item> {
        // The Pulse commands switch and two typed characters, as for the pages above.
        if text.chars().count() < 2 || !host::switch_on("launcherPulseCommands") {
            return Vec::new();
        }
        const STORAGE_WORDS: [&str; 6] = ["disk", "storage", "space", "free", "clean", "cleanup"];
        let generic = STORAGE_WORDS
            .iter()
            .any(|&word| score(text, word).is_some());
        let mut out: Vec<Item> = Vec::new();
        for drive in host::drives() {
            let letter = drive.root.trim_end_matches('\\').to_string();
            if !generic && score(text, &letter).is_none() {
                continue;
            }
            let used = f64::from(drive.used_fraction());
            let percent = host::tf("{}% used", &[&((used * 100.0).round() as u32).to_string()]);
            let subtitle = if used >= 0.9 {
                host::tf("Almost full \u{00b7} {}", &[&percent])
            } else if used >= 0.7 {
                host::tf("Filling up \u{00b7} {}", &[&percent])
            } else {
                percent
            };
            let title = format!(
                "{letter}: {}",
                host::tf(
                    "{} free of {}",
                    &[&size_text(drive.free), &size_text(drive.total),],
                )
            );
            out.push(Self::plain_item(
                Section::Pulse,
                title,
                subtitle,
                Action::Hub("storage"),
            ));
        }
        if generic {
            out.push(Self::cleanup_item());
        }
        out
    }

    /// The saved cleanup scan (`cleanup-findings-v1.json` in `%LOCALAPPDATA%\Pulse`, the hub's
    /// `{version, data}` envelope): its item count and safe bytes. With no saved scan, the row
    /// only opens Cleanup.
    fn cleanup_item() -> Item {
        const FILE_MAX: usize = 64 * 1024 * 1024;
        let saved = host::data_dir()
            .map(|dir| dir.join("cleanup-findings-v1.json"))
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| json::parse(&bytes, FILE_MAX));
        let report = saved.as_ref().and_then(|root| {
            if root.get("version").and_then(Value::as_f64) != Some(1.0) {
                return None;
            }
            let data = root.get("data")?;
            let items = data.get("findings")?.as_array()?.len();
            let safe = data.get("safe_bytes").and_then(Value::as_f64)?;
            let scanned = data.get("scanned_at").and_then(Value::as_f64)?;
            Some((items, safe, scanned))
        });
        match report {
            Some((items, safe, scanned)) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(scanned);
                let freed = size_text(safe.max(0.0) as u64);
                Self::plain_item(
                    Section::Pulse,
                    if items == 1 {
                        host::tf("Cleanup: 1 item, {} could be freed", &[&freed])
                    } else {
                        host::tf(
                            "Cleanup: {} items, {} could be freed",
                            &[&items.to_string(), &freed],
                        )
                    },
                    found_text(scanned, now),
                    Action::Hub("cleanup"),
                )
            }
            None => Self::plain_item(
                Section::Pulse,
                host::t("Cleanup: open to scan").to_string(),
                host::t("No saved scan yet \u{00b7} opens Cleanup").to_string(),
                Action::Hub("cleanup"),
            ),
        }
    }

    /// The clipboard history's rows for "clip" or "clipboard" and what follows it: the same
    /// search the strip uses (`clip_search::search`), so the order is the same.
    fn clip_rows(text: &str) -> Vec<Item> {
        if !host::switch_on("launcherClipboard") {
            return Vec::new();
        }
        let Some(argument) =
            keyword_argument(text, "clip").or_else(|| keyword_argument(text, "clipboard"))
        else {
            return Vec::new();
        };
        let offset = rightkit_win_ui::clock::local_offset_secs();
        let snapshot = rightkit_clipboard_history::model::snapshot();
        let now = rightkit_clipboard_history::model::now();
        let query = rightkit_clipboard_history::search::parse_query(&argument, offset);
        rightkit_clipboard_history::search::search(
            &snapshot.items,
            &snapshot.boards,
            &rightkit_clipboard_history::search::View::History,
            &query,
            now,
            CLIPS_MAX,
        )
        .into_iter()
        .map(|index| {
            let item = &snapshot.items[index];
            let title: String = item
                .preview
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .chars()
                .take(120)
                .collect();
            let title = if title.is_empty() {
                item.kind.as_str().to_string()
            } else {
                title
            };
            // "2 min ago · Chrome · Link", the parts separated as in the Mac's rows.
            let mut parts = vec![launcher_notes::ago(item.used as u64, now as u64)];
            if let Some(app) = &item.source {
                parts.push(app.name.clone());
            }
            let mut kind = item.kind.as_str().to_string();
            kind[..1].make_ascii_uppercase();
            parts.push(kind);
            Item {
                section: Section::Clipboard,
                title,
                subtitle: Some(parts.join(" \u{00B7} ")),
                icon: None,
                pin: None,
                window: 0,
                usage: None,
                kind: Some(item.kind),
                action: Action::Clip(item.id.clone()),
            }
        })
        .collect()
    }

    /// Opens on the clipboard history with "clip " typed.
    fn open_clip_mode(&mut self) {
        if !self.shown {
            self.show();
        }
        self.query = "clip ".chars().collect();
        self.caret = self.query.len();
        self.selected = 0;
        self.scroll = 0.0;
        self.recompute();
        self.draw();
    }

    /// Return on a clipboard row: pastes it; Shift pastes plain text; Ctrl opens the strip with
    /// that row selected. `None` for any other row or key.
    fn clip_key(&mut self, vk: u32, ctrl: bool, shift: bool) -> Option<Do> {
        if vk != VK_RETURN || self.output.is_some() || self.editor.is_some() {
            return None;
        }
        let Action::Clip(id) = self.items.get(self.selected)?.action.clone() else {
            return None;
        };
        let previous = self.previous;
        self.hide(false);
        if ctrl {
            host::open_strip_at(&id);
        } else {
            let choice = if shift {
                rightkit_clipboard_history::paste::Choice::Plain
            } else {
                rightkit_clipboard_history::paste::Choice::Paste
            };
            rightkit_clipboard_history::paste::choose(&id, choice, previous);
        }
        Some(Do::Nothing)
    }

    fn recompute(&mut self) {
        let text = self.text();
        self.items = if let Some(holder) = &self.prompt {
            Self::prompt_items(holder)
        } else if text.is_empty() {
            let mut items = self.pinned_items();
            items.extend(self.recent_items());
            items.extend(self.running_items());
            items
        } else {
            let mut items = self.answer_items(&text);
            items.extend(self.held(&text, |slow| &slow.open));
            items.extend(self.app_items(&text));
            items.extend(self.held(&text, |slow| &slow.quicklinks));
            items.extend(self.held(&text, |slow| &slow.snippets));
            items.extend(self.command_items(&text));
            items.extend(self.file_items(&text));
            items.extend(self.held(&text, |slow| &slow.notes));
            items.extend(self.pulse_items(&text));
            items.extend(self.held(&text, |slow| &slow.storage));
            items.extend(self.held(&text, |slow| &slow.clips));
            items.extend(self.dictionary_items(&text));
            items.extend(self.chat_items(&text));
            items.extend(self.action_items(&text));
            items
        };
        self.selected = self.selected.min(self.items.len().saturating_sub(1));
        self.rows_done = Some(Instant::now());
    }

    /// Takes the apps and running programs the scan thread left, and shows them.
    fn take_scanned(&mut self) {
        let Some(scanned) = SCANNED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        else {
            return;
        };
        if let Some(apps) = scanned.apps {
            self.apps = apps;
        }
        self.running = scanned.running;
        if self.shown && self.output.is_none() && self.prompt.is_none() {
            self.recompute();
            self.draw();
        }
    }

    /// Reads the settings the panel lists from (cheap: a small text and one small file). The
    /// Start menu and the running programs come from the scan thread (`spawn_scan`).
    fn refresh_sources(&mut self) {
        self.config = launcher_config::parse(&host::launcher_config_json());
        self.chats = if host::launcher_ask_target() == "inLauncher" {
            launcher_dir()
                .map(|dir| crate::chat::load(&dir))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        self.pins = if self.config.pinned.is_empty() {
            load_pins()
        } else {
            self.config.pinned.clone()
        };
        // Money conversions answer from the cache; a stale cache is fetched again on its own
        // thread and serves the next time the panel opens.
        self.rates = None;
        if host::switch_on("launcherCurrency")
            && let Some(dir) = launcher_dir()
        {
            self.rates = crate::rates::load(&dir);
            crate::rates::refresh_in_background(&dir);
        }
    }

    // ------------------------------------------------------------ keys

    fn insert(&mut self, text: &str) {
        for c in text.chars() {
            if self.query.len() >= QUERY_MAX {
                break;
            }
            if c >= ' ' && c != '\u{7f}' {
                self.query.insert(self.caret, c);
                self.caret += 1;
            }
        }
        self.selected = 0;
        self.scroll = 0.0;
        self.edits += 1;
        self.recompute();
        self.queue_search();
    }

    fn typed(&mut self, unit: u16) -> Do {
        // The note editor takes the typing.
        if self.editor.is_some() {
            return self.editor_typed(unit);
        }
        // The output view takes no typing; Escape goes back to the field. Nor does the
        // Alt+Space question.
        if self.output.is_some() || self.prompt.is_some() {
            return Do::Nothing;
        }
        let c = match (self.high.take(), unit) {
            (Some(high), 0xDC00..=0xDFFF) => char::decode_utf16([high, unit]).next(),
            (_, 0xD800..=0xDBFF) => {
                self.high = Some(unit);
                return Do::Nothing;
            }
            _ => char::decode_utf16([unit]).next(),
        };
        match c {
            Some(Ok(c)) if c >= ' ' && c != '\u{7f}' => {
                self.insert(&c.to_string());
                Do::Redraw
            }
            _ => Do::Nothing,
        }
    }

    fn edited(&mut self) -> Do {
        self.selected = 0;
        self.scroll = 0.0;
        self.edits += 1;
        self.recompute();
        self.queue_search();
        Do::Redraw
    }

    fn move_by(&mut self, delta: isize) -> Do {
        // The wheel moves the editor's caret three rows at a time.
        if let Some(editor) = self.editor.as_mut() {
            editor.vertical(delta * 3);
            return Do::Redraw;
        }
        let count = self.items.len() as isize;
        if count == 0 {
            return Do::Nothing;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(count) as usize;
        Do::Redraw
    }

    fn activate(&mut self, position: usize) -> Do {
        let Some(item) = self.items.get(position).cloned() else {
            return Do::Nothing;
        };
        // A clear row asks first: the list becomes its confirmation row, and nothing is deleted yet.
        if let Action::ClipAsk(scope) = item.action {
            let count = rightkit_clipboard_history::model::with(|store| {
                store.scope_count(scope, rightkit_clipboard_history::model::now())
            });
            self.items = vec![clip_confirm_item(scope, count)];
            self.selected = 0;
            return Do::Redraw;
        }
        Do::Run(item)
    }

    fn key(&mut self, vk: u32, ctrl: bool) -> Do {
        if self.editor.is_some() {
            return self.editor_key(vk, ctrl);
        }
        if self.output.is_some() {
            return self.output_key(vk);
        }
        // The Alt+Space question takes only the choosing keys.
        if self.prompt.is_some() && !matches!(vk, VK_UP | VK_DOWN | VK_RETURN | VK_ESCAPE) {
            return Do::Nothing;
        }
        match vk {
            VK_UP => self.move_by(-1),
            VK_DOWN => self.move_by(1),
            VK_RETURN => self.activate(self.selected),
            VK_ESCAPE => Do::Hide,
            VK_LEFT => {
                self.caret = self.caret.saturating_sub(1);
                Do::Redraw
            }
            VK_RIGHT => {
                self.caret = (self.caret + 1).min(self.query.len());
                Do::Redraw
            }
            VK_HOME => {
                self.caret = 0;
                Do::Redraw
            }
            VK_END => {
                self.caret = self.query.len();
                Do::Redraw
            }
            VK_BACK if self.caret > 0 => {
                let mut start = self.caret - 1;
                if ctrl {
                    // The word before the caret, with the spaces after it.
                    while start > 0 && self.query[start] == ' ' {
                        start -= 1;
                    }
                    while start > 0 && self.query[start - 1] != ' ' {
                        start -= 1;
                    }
                }
                self.query.drain(start..self.caret);
                self.caret = start;
                self.edited()
            }
            VK_DELETE if self.caret < self.query.len() => {
                self.query.remove(self.caret);
                self.edited()
            }
            VK_V if ctrl => match clipboard_text() {
                Some(text) => {
                    let line = text.lines().next().unwrap_or("").to_string();
                    self.insert(&line);
                    Do::Redraw
                }
                None => Do::Nothing,
            },
            _ => Do::Nothing,
        }
    }

    /// A key pressed with Alt held: the Mac's Command keys.
    fn alt_key(&mut self, vk: u32) -> Option<Do> {
        if self.editor.is_some() {
            // Alt+D or Alt+Backspace deletes the open note (the Mac's Command-Delete).
            return Some(if matches!(vk, VK_D | VK_BACK) {
                self.delete_open_note()
            } else {
                Do::Nothing
            });
        }
        if self.output.is_some() {
            return None;
        }
        match vk {
            VK_1..=VK_9 => Some(self.activate((vk - VK_1) as usize)),
            VK_D => self.delete_note(),
            VK_P => {
                if let Some(done) = self.pin_note() {
                    return Some(done);
                }
                if let Some(done) = self.pin_chat() {
                    return Some(done);
                }
                let path = self.items.get(self.selected)?.pin.clone()?;
                match self.pins.iter().position(|pin| *pin == path) {
                    Some(position) => {
                        self.pins.remove(position);
                    }
                    None => self.pins.push(path),
                }
                save_pins(&self.pins);
                self.recompute();
                Some(Do::Redraw)
            }
            VK_Q => {
                let window = self.items.get(self.selected)?.window;
                if window != 0 {
                    // SAFETY: posting to a window handle that may have gone is harmless.
                    let _ = unsafe {
                        PostMessageW(Some(hwnd_from_key(window)), WM_CLOSE, WPARAM(0), LPARAM(0))
                    };
                    // The list is read again once the app has had time to go.
                    let _ = unsafe {
                        SetTimer(Some(self.window.hwnd()), REFRESH_TIMER, REFRESH_MS, None)
                    };
                }
                Some(Do::Nothing)
            }
            _ => None,
        }
    }

    fn click(&mut self, x: i32, y: i32) -> Do {
        if self.output.is_some() || self.editor.is_some() {
            return Do::Nothing;
        }
        let s = self.scale;
        let list_top = (FIELD_HEIGHT + 1.0) * s;
        let (x, y) = (x as f32, y as f32);
        if y < list_top || x < ROW_INSET * s || x > (WIDTH - ROW_INSET) * s {
            return Do::Nothing;
        }
        let at = y - list_top + self.scroll;
        let hit = self
            .tops
            .iter()
            .position(|top| at >= *top && at < *top + ROW_HEIGHT * s);
        match hit {
            Some(position) => self.activate(position),
            None => Do::Nothing,
        }
    }

    // ------------------------------------------------------------ drawing

    fn mask(&mut self, text: &str, size: f32, bold: bool) -> Option<Mask> {
        let px = (size * self.scale).round() as i32;
        self.painter.render(text, px, bold)
    }

    /// Draws `mask` no wider than `room` pixels, its box centred on `mid_y`.
    fn draw_text(canvas: &mut Canvas, mask: &Mask, x: f32, mid_y: f32, room: f32, alpha: f32) {
        let width = mask.width.min(room.max(0.0) as usize);
        if width == 0 || mask.height == 0 {
            return;
        }
        let y = (mid_y - mask.height as f32 / 2.0).round() as i32;
        if width == mask.width {
            canvas.draw_mask(mask, x.round() as i32, y, INK, alpha);
            return;
        }
        let mut cut = Mask {
            width,
            height: mask.height,
            coverage: Vec::with_capacity(width * mask.height),
        };
        for row in 0..mask.height {
            let start = row * mask.width;
            cut.coverage
                .extend_from_slice(&mask.coverage[start..start + width]);
        }
        canvas.draw_mask(&cut, x.round() as i32, y, INK, alpha);
    }

    /// The list, as tall as its rows, with the panel's colour behind it.
    fn draw_list(&mut self, width: usize) -> Canvas {
        let s = self.scale;
        let mut tops = Vec::with_capacity(self.items.len());
        let mut headers = Vec::new();
        let mut y = 0.0f32;
        for (position, item) in self.items.iter().enumerate() {
            if position == 0 || self.items[position - 1].section != item.section {
                let gap = if position == 0 {
                    HEADER_TOP_FIRST
                } else {
                    HEADER_TOP
                };
                y += gap * s;
                headers.push((y, item.section));
                y += (HEADER_LINE + HEADER_BOTTOM) * s;
            }
            tops.push(y);
            y += ROW_HEIGHT * s;
        }
        let height = (y + LIST_BOTTOM * s).ceil().max(1.0) as usize;
        let mut canvas = Canvas::new(width, height);
        canvas.fill_round_rect(0.0, 0.0, width as f32, height as f32, [0.0; 4], FILL, 1.0);
        for (top, section) in headers {
            if let Some(mask) = self.mask(section.title(), HEADER_TEXT, true) {
                let mid = top + HEADER_MID * s;
                Self::draw_text(&mut canvas, &mask, FIELD_PAD * s, mid, width as f32, 0.4);
            }
        }
        let items = self.items.clone();
        let mut missing: Vec<String> = Vec::new();
        for (position, item) in items.iter().enumerate() {
            let top = tops[position];
            let mid = top + ROW_HEIGHT * s / 2.0;
            if position == self.selected {
                // The Mac's highlight is a continuous-cornered rectangle too.
                canvas.fill_continuous_rect(
                    (
                        ROW_INSET * s,
                        top,
                        width as f32 - 2.0 * ROW_INSET * s,
                        ROW_HEIGHT * s,
                    ),
                    ROW_RADIUS * s,
                    INK,
                    0.14,
                );
            }
            let icon_x = (ROW_INSET + ROW_PAD) * s;
            // Only icons already loaded away from this thread are drawn. A miss never asks the
            // shell here (a first `SHGetFileInfoW` costs hundreds of ms and `fileicon::of`
            // can wait on the scan thread's own shell call): the row shows its glyph, the
            // icon is fetched on a thread and `WM_ICONS` draws the panel again.
            let icon = item.icon.as_deref().and_then(|path| {
                let held = cached_icon(path);
                if held.is_none() {
                    missing.push(path.to_string());
                }
                held
            });
            match icon {
                Some(icon) => canvas.draw_image(
                    &icon.pixels,
                    (icon.width, icon.height),
                    icon_x,
                    mid - ICON * s / 2.0,
                    ICON * s,
                    ICON * s,
                ),
                None => {
                    // No tile: the kind's outline glyph, as the Mac draws its symbol.
                    let size = ICON * s * 0.66;
                    let centre = (icon_x + ICON * s / 2.0, mid);
                    match item.kind {
                        Some(kind) => host::kind_mark(&mut canvas, kind, centre, size, (INK, 0.8)),
                        None => {
                            glyphs::row_glyph(&mut canvas, item.glyph(), centre, size, INK, 0.8)
                        }
                    }
                }
            }
            let hint = if position < 9 {
                self.mask(&format!("Alt+{}", position + 1), SUBTITLE_TEXT, false)
            } else {
                None
            };
            let right = width as f32 - (ROW_INSET + ROW_PAD) * s;
            let hint_width = hint.as_ref().map_or(0.0, |mask| mask.width as f32);
            if let Some(mask) = &hint {
                Self::draw_text(
                    &mut canvas,
                    mask,
                    right - hint_width,
                    mid - HINT_LIFT * s,
                    hint_width,
                    0.35,
                );
            }
            let text_x = icon_x + (ICON + ICON_GAP) * s;
            let room = right - hint_width - 8.0 * s - text_x;
            let title = self
                .painter
                .render_semibold(&item.title, (TITLE_TEXT * self.scale).round() as i32);
            let subtitle = item
                .subtitle
                .as_deref()
                .and_then(|text| self.mask(text, SUBTITLE_TEXT, false));
            match (title, subtitle) {
                (Some(title), Some(subtitle)) => {
                    Self::draw_text(&mut canvas, &title, text_x, mid - 8.0 * s, room, 1.0);
                    Self::draw_text(
                        &mut canvas,
                        &subtitle,
                        text_x,
                        mid + SUBTITLE_MID * s,
                        room,
                        0.5,
                    );
                }
                (Some(title), None) => {
                    Self::draw_text(&mut canvas, &title, text_x, mid, room, 1.0);
                }
                _ => {}
            }
        }
        self.tops = tops;
        warm_icons_later(missing);
        canvas
    }

    /// The panel, with its border drawn last: the Mac strokes the border over the content.
    fn render(&mut self) -> Canvas {
        let s = self.scale;
        let mut canvas = self.render_content();
        let (w, h) = (canvas.width as f32, canvas.height as f32);
        canvas.stroke_continuous_rect(
            (0.0, 0.0, w, h),
            RADIUS * s,
            EDGE_WIDTH * s,
            INK,
            EDGE_ALPHA,
        );
        canvas
    }

    /// The panel's fill and everything on it, without the border.
    fn render_content(&mut self) -> Canvas {
        let s = self.scale;
        let width = (WIDTH * s).round() as usize;
        let height = (HEIGHT * s).round() as usize;
        let (w, h) = (width as f32, height as f32);
        let mut canvas = Canvas::new(width, height);
        // The Mac's panel has continuous corners (`RoundedRectangle(cornerRadius: 22, style:
        // .continuous)`), not circular ones.
        canvas.fill_continuous_rect((0.0, 0.0, w, h), RADIUS * s, FILL, 1.0);
        if self.editor.is_some() {
            self.render_editor(&mut canvas, (w, h));
            self.tops.clear();
            return canvas;
        }
        if self.output.is_some() {
            self.render_output(&mut canvas, (w, h));
            self.tops.clear();
            return canvas;
        }

        // The search field: the text with a caret, or what can be typed.
        let field_mid = FIELD_HEIGHT * s / 2.0;
        let text_x = (FIELD_PAD + FIELD_INDENT) * s;
        let room = w - text_x - FIELD_PAD * s;
        glyphs::row_glyph(
            &mut canvas,
            glyphs::RowGlyph::Magnifier,
            (FIELD_PAD * s + FIELD_MAGNIFIER * s / 2.0, field_mid),
            FIELD_MAGNIFIER * s,
            INK,
            0.5,
        );
        let typed: String = self.query.iter().collect();
        // The text and the caret ride the Mac's baseline; the magnifier stays on the field's
        // middle.
        let text_mid = field_mid - FIELD_LIFT * s;
        let mut caret_x = text_x;
        if let Some(holder) = self.prompt.clone() {
            let ask = host::tf("Alt+Space is already used by {}", &[&holder]);
            if let Some(mask) = self.mask(&ask, FIELD_TEXT, false) {
                Self::draw_text(&mut canvas, &mask, text_x, text_mid, room, 1.0);
            }
        } else if typed.is_empty() {
            if let Some(mask) = self.mask(
                host::t("Search apps, files, clipboard, or run a command"),
                FIELD_TEXT,
                false,
            ) {
                Self::draw_text(&mut canvas, &mask, text_x, text_mid, room, 0.35);
            }
        } else {
            if let Some(mask) = self.mask(&typed, FIELD_TEXT, false) {
                Self::draw_text(&mut canvas, &mask, text_x, text_mid, room, 1.0);
            }
            let before: String = self.query[..self.caret].iter().collect();
            // A trailing space has no ink of its own: measure it with a mark after it.
            let marked = self.mask(&format!("{before}|"), FIELD_TEXT, false);
            let mark = self.mask("|", FIELD_TEXT, false);
            if let (Some(marked), Some(mark)) = (marked, mark) {
                caret_x += marked.width.saturating_sub(mark.width) as f32;
            }
        }
        if !self.still && self.prompt.is_none() {
            let caret_height = FIELD_TEXT * 1.2 * s;
            canvas.fill_round_rect(
                caret_x.min(w - FIELD_PAD * s),
                text_mid - caret_height / 2.0,
                (1.5 * s).max(1.0),
                caret_height,
                [0.0; 4],
                INK,
                0.9,
            );
        }
        canvas.fill_round_rect(
            0.0,
            FIELD_HEIGHT * s,
            w,
            s.max(1.0),
            [0.0; 4],
            INK,
            DIVIDER_ALPHA,
        );

        let list_top = ((FIELD_HEIGHT + 1.0) * s).ceil() as usize;
        let view = height.saturating_sub(list_top + 1);
        if self.items.is_empty() {
            let note = if self.text().is_empty() {
                host::t("Type to search")
            } else {
                host::t("No matches")
            };
            if let Some(mask) = self.mask(note, 13.0, false) {
                let x = (w - mask.width as f32) / 2.0;
                let mid = list_top as f32 + view as f32 / 2.0 - NOTE_LIFT * s;
                Self::draw_text(&mut canvas, &mask, x, mid, w, 0.4);
            }
            self.tops.clear();
            return canvas;
        }

        let list = self.draw_list(width);
        // Keep the selected row in view.
        if let Some(top) = self.tops.get(self.selected).copied() {
            let row = ROW_HEIGHT * s;
            let lead = (HEADER_LINE + HEADER_BOTTOM + HEADER_TOP) * s;
            if top - lead < self.scroll {
                self.scroll = (top - lead).max(0.0);
            }
            if top + row + LIST_BOTTOM * s > self.scroll + view as f32 {
                self.scroll = top + row + LIST_BOTTOM * s - view as f32;
            }
        }
        let limit = list.height.saturating_sub(view) as f32;
        self.scroll = self.scroll.clamp(0.0, limit);
        let offset = self.scroll.round() as usize;

        // Where the panel is opaque, so the list keeps the rounded bottom corners.
        let mut shape = Canvas::new(width, height);
        shape.fill_continuous_rect((0.0, 0.0, w, h), RADIUS * s, INK, 1.0);
        for row in 0..view.min(list.height.saturating_sub(offset)) {
            let source = (offset + row) * list.width;
            let target = (list_top + row) * width;
            for column in 0..list.width {
                if shape.pixels[target + column] >> 24 == 0xFF {
                    canvas.pixels[target + column] = list.pixels[source + column];
                }
            }
        }
        canvas
    }

    fn draw(&mut self) {
        let canvas = self.render();
        if let Err(error) = surface::present(self.window.hwnd(), &canvas, Some(self.origin)) {
            host::log_error("UpdateLayeredWindow", &error, "launcher");
        }
    }

    // ------------------------------------------------------------ show and hide

    fn show(&mut self) {
        let began = Instant::now();
        let pressed = HOTKEY_AT.swap(0, Ordering::Relaxed);
        let mut point = POINT::default();
        let _ = unsafe { GetCursorPos(&mut point) };
        let Some(monitor) = rightkit_overlay::windows::monitor_at(point.x, point.y) else {
            host::capture_release();
            return;
        };
        self.scale = monitor.scale() as f32;
        let work = (
            monitor.work.left as i32,
            monitor.work.top as i32,
            monitor.work.right as i32,
            monitor.work.bottom as i32,
        );
        let width = (WIDTH * self.scale).round() as i32;
        let height = (HEIGHT * self.scale).round() as i32;
        // Centred across the screen, a little above the middle, as the Mac panel sits.
        self.origin = (
            work.0 + (work.2 - work.0 - width) / 2,
            work.1 + ((work.3 - work.1 - height) as f32 * 0.38) as i32,
        );
        self.previous = hwnd_key(unsafe { GetForegroundWindow() });
        self.query.clear();
        self.caret = 0;
        self.high = None;
        self.selected = 0;
        self.scroll = 0.0;
        self.opened_at = Some(began);
        self.first_rows_logged = false;
        // Take the keyboard before anything is read or drawn: until this window is the
        // foreground, a key typed behind Alt+Space goes to the program that was in front. Keys
        // that arrive during the reads and the draw below wait in this thread's queue and are
        // typed into the cleared query after this call returns. A blank frame goes up first so
        // the window never shows the last opening's picture.
        let hwnd = self.window.hwnd();
        let blank = Canvas::new(width.max(1) as usize, height.max(1) as usize);
        if let Err(error) = surface::present(hwnd, &blank, Some(self.origin)) {
            host::log_error("UpdateLayeredWindow", &error, "launcher");
        }
        // Opened from the key hook or the wheel, this thread has no right to the foreground
        // of its own: sharing the front window's input queue for the call gives it one.
        let front = unsafe { GetWindowThreadProcessId(hwnd_from_key(self.previous), None) };
        let own = unsafe { GetCurrentThreadId() };
        let joined =
            front != 0 && front != own && unsafe { ffi::AttachThreadInput(own, front, 1) } != 0;
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            ffi::SetFocus(hwnd.0);
        }
        if joined {
            // SAFETY: undoes the attachment made just above.
            unsafe { ffi::AttachThreadInput(own, front, 0) };
        }
        self.refresh_sources();
        // The lists on hand are shown at once; the scan thread brings fresh ones in a moment.
        spawn_scan(false);
        if self.prompt.is_none() {
            self.start_files();
        }
        // Characters typed behind the chord, while this window was not yet the foreground, go
        // into the cleared query in order (the hook held them; see `keys::capture_begin`).
        let early = host::capture_take();
        if !early.is_empty() {
            self.insert(&early);
        }
        self.recompute();
        self.shown = true;
        PANEL_UP.store(true, Ordering::Relaxed);
        let rows_ms = began.elapsed().as_secs_f32() * 1000.0;
        self.draw();
        let drawn_ms = began.elapsed().as_secs_f32() * 1000.0;
        // From the key press when there was one (the key hook or the hot key loop stamped
        // it), else from the start of this call.
        let (source, visible_ms) = if pressed > 0 {
            ("hotkey", (mono_ms() + 1).saturating_sub(pressed) as f32)
        } else {
            ("other", began.elapsed().as_secs_f32() * 1000.0)
        };
        host::log_info(
            "launcher_open",
            &[
                ("source", source),
                ("visible_ms", &format!("{visible_ms:.1}")),
                ("rows_ms", &format!("{rows_ms:.1}")),
                ("drawn_ms", &format!("{drawn_ms:.1}")),
                ("prompt", if self.prompt.is_some() { "yes" } else { "no" }),
            ],
        );
    }

    /// Hides the panel; `give_back` returns the foreground to the window that had it.
    fn hide(&mut self, give_back: bool) {
        if !self.shown {
            return;
        }
        self.shown = false;
        PANEL_UP.store(false, Ordering::Relaxed);
        if !MEMORY_LOGGED.swap(true, Ordering::Relaxed) {
            log_memory();
        }
        let hwnd = self.window.hwnd();
        unsafe {
            let _ = KillTimer(Some(hwnd), REFRESH_TIMER);
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        let previous = hwnd_from_key(self.previous);
        if give_back && self.previous != 0 && unsafe { IsWindow(Some(previous)) }.as_bool() {
            let _ = unsafe { SetForegroundWindow(previous) };
        }
        // A note open in the editor is written, or trashed when blank, before the panel goes.
        self.finish_editor();
        self.items.clear();
        self.tops.clear();
        self.output = None;
        self.slow = None;
        // The file names are held only while the panel shows.
        self.files = None;
        self.files_job = None;
        self.file_rows.clear();
        self.file_text.clear();
        self.prompt = None;
        // A search still running has nothing left to show.
        FILE_GEN.fetch_add(1, Ordering::AcqRel);
        let _ = unsafe { KillTimer(Some(hwnd), FILES_TIMER) };
        let _ = unsafe { KillTimer(Some(hwnd), QUERY_TIMER) };
    }

    fn sync_hotkey(&mut self) {
        let want = host::switch_on("launcherEnabled").then(host::launcher_hotkey);
        if want == self.applied {
            return;
        }
        if let (Some(keys), Some(old)) = (&self.keys, self.hot.take()) {
            let _ = keys.unregister(&old);
        }
        HOOK_TAKES.store(false, Ordering::Relaxed);
        host::launcher_needs_hook(false);
        self.applied = want;
        let Some(chord) = want else {
            self.hide(true);
            set_status(None);
            log_state("launcher_hotkey", &[("chord", "off")]);
            return;
        };
        // The apps are listed and their icons loaded before the first press.
        spawn_scan(false);
        let text = match chord {
            LauncherHotkey::AltSpace => "Alt+Space",
            LauncherHotkey::CtrlSpace => "Ctrl+Space",
        };
        if self.keys.is_none() {
            // One press, one toggle: the loop's thread only posts to this one.
            self.keys = GlobalHotkeys::new(|_| {
                note_hotkey();
                post(WM_TOGGLE);
            })
            .ok();
        }
        let asked = rightkit_hotkeys::parse_chord(text).ok();
        let outcome = match (&self.keys, &asked) {
            (Some(keys), Some(asked)) => keys.register(asked).map_err(|error| error.message()),
            _ => Err("no hot key loop".to_string()),
        };
        let taken = outcome.is_ok();
        self.hot = asked.filter(|_| taken);
        if taken {
            set_status(None);
            log_state(
                "launcher_hotkey",
                &[("chord", chord.as_str()), ("result", "registered")],
            );
            return;
        }
        if let Err(why) = &outcome {
            // "already registered" is another program holding the chord; anything else is
            // another fault. Written once for a state, not on every pass through here.
            log_state("launcher_hotkey_refused", &[("chord", text), ("why", why)]);
        }
        match chord {
            LauncherHotkey::AltSpace => self.settle_alt_conflict(),
            LauncherHotkey::CtrlSpace => {
                set_status(Some(CTRL_REFUSED));
                log_state(
                    "launcher_hotkey",
                    &[("chord", chord.as_str()), ("result", "held_by_another_app")],
                );
            }
        }
    }

    /// Another program holds Alt+Space. Pulse takes it through the key hook only when the
    /// user said so (the setting `launcherHotkeyConflict`); with no answer yet it asks once, with the
    /// answer "keep it" it leaves the key alone and does not try again.
    fn settle_alt_conflict(&mut self) {
        let answer = conflict_answer();
        SETTLED.store(answer_code(answer), Ordering::Relaxed);
        let (take, status, result) = match answer {
            Some(true) => (true, ALT_TAKEN, "held_by_another_app_taken_by_key_hook"),
            Some(false) => (false, ALT_KEPT, "held_by_another_app_kept"),
            None => (false, ALT_ASK, "held_by_another_app_undecided"),
        };
        HOOK_TAKES.store(take, Ordering::Relaxed);
        host::launcher_needs_hook(take);
        set_status(Some(status));
        log_state(
            "launcher_hotkey",
            &[
                ("chord", LauncherHotkey::AltSpace.as_str()),
                ("result", result),
            ],
        );
        if answer.is_none() && !PROMPTED.swap(true, Ordering::Relaxed) {
            post(WM_PROMPT);
        }
    }

    /// The hub changed the Alt+Space answer (including clearing it, which asks again): settle
    /// the held-by-another-app state again for the new answer.
    fn resettle_if_answer_changed(&mut self) {
        let refused = self.applied == Some(LauncherHotkey::AltSpace) && self.hot.is_none();
        if !refused || SETTLED.load(Ordering::Relaxed) == answer_code(conflict_answer()) {
            return;
        }
        if conflict_answer().is_none() {
            PROMPTED.store(false, Ordering::Relaxed);
        }
        self.settle_alt_conflict();
    }

    /// Puts the Alt+Space question on screen, naming the program that holds the key when it
    /// can (a running ChatGPT), else "the other app".
    fn show_prompt(&mut self) {
        let refused = self.applied == Some(LauncherHotkey::AltSpace) && self.hot.is_none();
        if !refused || conflict_answer().is_some() {
            return;
        }
        let chatgpt = running_apps().iter().any(|app| {
            app.name.to_lowercase().contains("chatgpt")
                || app.path.to_lowercase().contains("chatgpt")
        });
        let holder = if chatgpt {
            "ChatGPT"
        } else {
            host::t("the other app")
        };
        // The question has no field: whatever was typed behind the chord goes back to the window
        // still in front, before the panel takes the foreground.
        host::capture_release();
        self.prompt = Some(holder.to_string());
        if self.shown {
            self.recompute();
            self.draw();
        } else {
            self.show();
        }
    }
}

/// One row of a launcher view shot: its section (by the shared fixtures' name), title and
/// subtitle. A shot row has no file icon, so it shows its kind's glyph.
#[cfg_attr(not(feature = "qa-views"), allow(dead_code))] // used by the qa-views screenshots
pub struct ShotRow<'a> {
    pub section: &'a str,
    pub title: &'a str,
    pub subtitle: Option<&'a str>,
    /// The fixture's SF Symbol, which tells a clipboard row's kind.
    pub symbol: Option<&'a str>,
}

/// A shot output drawn as an Ask AI view (`render_ask`) instead of command output lines: its
/// state (`pending`, `error`, anything else is an answer) and the provider label when given.
#[cfg_attr(not(feature = "qa-views"), allow(dead_code))] // used by the qa-views screenshots
pub struct ShotAsk<'a> {
    pub state: &'a str,
    pub provider: Option<&'a str>,
}

/// The clipboard kind a fixture row's SF Symbol stands for.
#[cfg_attr(not(feature = "qa-views"), allow(dead_code))] // used by the qa-views screenshots
fn clip_kind_of(symbol: &str) -> Option<rightkit_clipboard_history::model::Kind> {
    use rightkit_clipboard_history::model::Kind;
    match symbol {
        "doc.text" => Some(Kind::Text),
        "link" => Some(Kind::Link),
        "paintpalette" => Some(Kind::Colour),
        "chevron.left.forwardslash.chevron.right" => Some(Kind::Code),
        "doc" => Some(Kind::File),
        "photo" => Some(Kind::Image),
        "doc.richtext" => Some(Kind::Rich),
        _ => None,
    }
}

/// The panel drawn off-screen for a view shot, `scale` times the 96 DPI design size: the
/// query typed with the caret at its end, `rows` with row `selected` highlighted, or in place
/// of both an `output` (title and text) a command printed. A section the Windows launcher
/// does not have is an `Err` that says so. It never opens the real panel: the panel it draws
/// has no window, hot key, file index or saved usage.
#[cfg_attr(not(feature = "qa-views"), allow(dead_code))] // used by the qa-views screenshots
pub fn shot(
    query: &str,
    rows: &[ShotRow<'_>],
    selected: usize,
    output: Option<(&str, &str)>,
    ask: Option<ShotAsk<'_>>,
    scale: f32,
) -> Result<Canvas, &'static str> {
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        let section = match row.section {
            "answer" => Section::Answer,
            "open" => Section::Open,
            "pinned" => Section::Pinned,
            "apps" => Section::Apps,
            "running" => Section::Running,
            "quicklinks" => Section::Quicklinks,
            "snippets" => Section::Snippets,
            "commands" => Section::Commands,
            "files" => Section::Files,
            "notes" => Section::Notes,
            "pulse" => Section::Pulse,
            "clipboard" => Section::Clipboard,
            "shortcuts" => return Err("The Windows launcher has no shortcuts section"),
            "dictionary" => Section::Dictionary,
            "assistant" | "chat" => Section::Chat,
            _ => return Err("The Windows launcher has no such section"),
        };
        items.push(Item {
            section,
            title: row.title.to_string(),
            subtitle: row.subtitle.map(str::to_string),
            icon: None,
            pin: None,
            window: 0,
            usage: None,
            kind: row.symbol.and_then(clip_kind_of),
            action: Action::Open(String::new()),
        });
    }
    let painter = TextPainter::new().ok_or("No text rasteriser")?;
    let query: Vec<char> = query.chars().collect();
    let mut panel = Panel {
        window: OwnedWindow::adopt(HWND::default()),
        painter,
        keys: None,
        hot: None,
        applied: None,
        shown: false,
        still: true,
        scale,
        origin: (0, 0),
        previous: 0,
        caret: query.len(),
        query,
        high: None,
        items,
        selected,
        tops: Vec::new(),
        scroll: 0.0,
        apps: Vec::new(),
        running: Vec::new(),
        pins: Vec::new(),
        config: Config::default(),
        output: output.map(|(title, text)| Output {
            title: title.to_string(),
            lines: text.lines().map(str::to_string).collect(),
            top: 0,
            run: 0,
            ask: ask.as_ref().map(|ask| {
                let state = match ask.state {
                    "pending" => AskState::Pending,
                    "error" => AskState::Error,
                    _ => AskState::Answer,
                };
                let mut view = AskView::new(state, text);
                view.provider = ask.provider.map(str::to_string);
                view
            }),
        }),
        editor: None,
        runs: 0,
        files: None,
        files_job: None,
        file_rows: Vec::new(),
        file_text: String::new(),
        prompt: None,
        rows_done: None,
        edits: 0,
        before_tab: 0,
        rates: None,
        usage: Usage {
            records: BTreeMap::new(),
        },
        slow: None,
        chats: Vec::new(),
        opened_at: None,
        first_rows_logged: false,
    };
    Ok(panel.render())
}

/// Carries out what a key or click asked for, with the panel's state free again.
fn carry_out(action: Do) {
    match action {
        Do::Nothing => {}
        Do::Redraw => {
            with_panel(Panel::draw);
        }
        Do::Hide => {
            with_panel(|panel| panel.hide(true));
        }
        // A command keeps the panel open: its output shows there.
        Do::Run(Item {
            action: Action::Command(line),
            title,
            ..
        }) => {
            with_panel(|panel| panel.start_command(title, line));
        }
        // The AI question and a history answer keep the panel open too.
        Do::Run(Item {
            action: Action::AiAsk(question),
            ..
        }) => {
            with_panel(|panel| panel.start_ai(question));
        }
        Do::Run(Item {
            action: Action::ChatOpen(id),
            ..
        }) => {
            with_panel(|panel| panel.open_chat(&id));
        }
        // A dictionary lookup shows its entry in the panel.
        Do::Run(Item {
            action: Action::Define(word),
            ..
        }) => {
            with_panel(|panel| panel.start_define(word));
        }
        // A note opens in the panel's editor, or in its own program when it cannot.
        Do::Run(Item {
            action: Action::NoteEdit(path),
            ..
        }) => {
            let opened = with_panel(|panel| panel.edit_note(&path)).unwrap_or(true);
            if !opened {
                with_panel(|panel| panel.hide(false));
                run(&Action::Open(path));
            }
        }
        Do::Run(Item {
            action: Action::NoteNew(text),
            ..
        }) if host::switch_on("launcherNoteEditor") => {
            with_panel(|panel| panel.new_note(&text));
        }
        Do::Run(Item {
            action: Action::NoteRestore(path),
            ..
        }) if host::switch_on("launcherNoteEditor") => {
            with_panel(|panel| panel.restore_note(&path));
        }
        Do::Run(item) => {
            with_panel(|panel| {
                if let Some(key) = &item.usage {
                    panel.usage.record(key);
                }
                // An answer or a snippet goes to the app that was in front; anything else
                // that runs next takes the foreground itself.
                let back = matches!(item.action, Action::Copy(_) | Action::Paste(_));
                if let Action::Clip(id) = &item.action {
                    // A clipboard row pastes itself into the app that was in front.
                    let previous = panel.previous;
                    panel.hide(false);
                    rightkit_clipboard_history::paste::choose(
                        id,
                        rightkit_clipboard_history::paste::Choice::Paste,
                        previous,
                    );
                    return;
                }
                panel.hide(back);
            });
            run(&item.action);
        }
    }
}

/// A clipboard clear row in the launcher's Pulse section (docs/clipboard-spec.md 9).
fn clip_clear_item(scope: rightkit_clipboard_history::model::ClearScope) -> Item {
    let subtitle = if scope == rightkit_clipboard_history::model::ClearScope::All {
        host::t("Removes every stored copy")
    } else {
        host::t("Removes copies in this window; pinned and board items stay")
    };
    Item {
        section: Section::Clipboard,
        title: scope.title().to_string(),
        subtitle: Some(subtitle.to_string()),
        icon: None,
        pin: None,
        window: 0,
        usage: None,
        kind: None,
        action: Action::ClipAsk(scope),
    }
}

/// The confirmation row a clipboard clear shows before it deletes (the Mac's "Delete N items?").
/// Return deletes; Escape closes the panel and keeps everything.
fn clip_confirm_item(scope: rightkit_clipboard_history::model::ClearScope, count: usize) -> Item {
    let subtitle = if scope == rightkit_clipboard_history::model::ClearScope::All {
        host::t("Pinned and board items go too; boards stay. Return deletes, Esc cancels.")
    } else {
        host::t("Pinned and board items stay. Return deletes, Esc cancels.")
    };
    Item {
        section: Section::Clipboard,
        title: host::tf(
            "Delete {} items? This can't be undone.",
            &[&count.to_string()],
        ),
        subtitle: Some(subtitle.to_string()),
        icon: None,
        pin: None,
        window: 0,
        usage: None,
        kind: None,
        action: Action::ClipClear(scope),
    }
}

/// Opens a launcher result the way Explorer would, with its registered handler: a shortcut, a
/// program, a script, a document or a folder that exists goes to `explorer.exe <canonical path>`
/// (the owner's `reveal::plan`, the path its own scope root), and anything else (a `ms-settings:`
/// page or another scheme) goes to `explorer.exe <target>`. The launcher's results are arbitrary
/// paths by design, so no scope narrower than the path itself applies. The process is
/// started through `OwnedCommand` and left running (docs/rightkit-exceptions/M5.md#b3).
fn open_with_shell(target: &str) {
    use rightkit_child_process::reveal::{PathScope, RevealAction, RevealPlatform, plan};
    use std::path::Path;
    use std::process::Stdio;
    let path = Path::new(target);
    let planned = path
        .exists()
        .then(|| PathScope::new([path]).and_then(|scope| scope.resolve(path)))
        .and_then(Result::ok)
        .and_then(|canonical| plan(RevealPlatform::Windows, RevealAction::Open, &canonical).ok());
    let (program, args) = match planned {
        Some(plan) => (plan.program, plan.args),
        None => ("explorer.exe".into(), vec![target.into()]),
    };
    let mut command = rightkit_child_process::OwnedCommand::new(&program);
    command
        .command_mut()
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.windows_visible();
    if let Err(error) = command.spawn_uncontained_detached() {
        host::log_info("launcher", &[("open_failed", error.to_string().as_str())]);
    }
}

fn run(action: &Action) {
    match action {
        Action::Open(target) => {
            // A web address goes to its handler through RightKit. Anything else is an app, a
            // shortcut, a file or a folder the launcher opens with the shell's "open" verb
            // (docs/rightkit-exceptions/M5.md#b3).
            if target.starts_with("https://") || target.starts_with("http://") {
                let _ = rightkit_child_process::open_url(target);
                return;
            }
            open_with_shell(target);
        }
        Action::Focus(window) => {
            let hwnd = hwnd_from_key(*window);
            unsafe {
                if IsIconic(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                }
                let _ = SetForegroundWindow(hwnd);
            }
        }
        Action::Hub(page) => {
            host::open_hub(page);
        }
        Action::Copy(text) => {
            host::copy_text(text);
        }
        Action::Paste(text) => {
            // The panel has given the foreground back; Ctrl+V follows once the app has it.
            if host::copy_text(text) {
                with_panel(|panel| {
                    let _ =
                        unsafe { SetTimer(Some(panel.window.hwnd()), PASTE_TIMER, PASTE_MS, None) };
                });
            }
        }
        // Started by `carry_out`, which keeps the panel open for the output.
        Action::Command(_) | Action::AiAsk(_) | Action::ChatOpen(_) | Action::Define(_) => {}
        // Opened by `carry_out`; with the editor off the file opens in its own program.
        Action::NoteEdit(path) => run(&Action::Open(path.clone())),
        Action::NoteNew(text) => {
            let made = launcher_dir()
                .and_then(|base| launcher_notes::create(&base, text, now_seconds() as u64));
            match made {
                Some(path) => run(&Action::Open(path.to_string_lossy().into_owned())),
                None => host::log_info("launcher", &[("note", "not_made")]),
            }
        }
        Action::NoteRestore(path) => {
            let Some(base) = launcher_dir() else {
                return;
            };
            // Listed again after the move, the newest note is the one put back.
            if launcher_notes::restore(&base, Path::new(path))
                && let Some(note) = launcher_notes::list(&base, false, "").into_iter().next()
            {
                run(&Action::Open(note.path.to_string_lossy().into_owned()));
            }
        }
        Action::Inert | Action::Clip(_) | Action::ClipAsk(_) => {}
        Action::ClipClear(scope) => {
            rightkit_clipboard_history::model::with(|store| {
                store.clear_scope(*scope, rightkit_clipboard_history::model::now())
            });
        }
        Action::AskChatGpt(question) => ask_chatgpt(question),
        Action::Conflict(Conflict::Choose) => {
            host::open_hub("settings");
        }
        Action::Conflict(answer) => {
            save_conflict_answer(*answer == Conflict::Pulse);
            with_panel(Panel::settle_alt_conflict);
        }
        Action::QuitAll => {
            for app in running_apps() {
                // SAFETY: posting to a window handle that may have gone is harmless.
                let _ = unsafe {
                    PostMessageW(
                        Some(hwnd_from_key(app.window)),
                        WM_CLOSE,
                        WPARAM(0),
                        LPARAM(0),
                    )
                };
            }
        }
    }
}

fn toggle_panel() {
    with_panel(|panel| {
        if panel.shown {
            HOTKEY_AT.store(0, Ordering::Relaxed);
            panel.hide(true);
        } else {
            panel.show();
        }
    });
}

/// Writes the timing line for a key that changed the typed text: from the start of its
/// message to the rows being ready (`rows_ms`: the apps and every other row held in memory)
/// and to the panel being drawn (`drawn_ms`). File rows come later, from their own thread.
fn report_query(began: Instant, edits_before: u64) {
    let drawn_ms = began.elapsed().as_secs_f32() * 1000.0;
    with_panel(|panel| {
        if panel.edits == edits_before {
            return;
        }
        let rows_ms = panel.rows_done.map_or(drawn_ms, |done| {
            done.duration_since(began).as_secs_f32() * 1000.0
        });
        if let (Some(opened), false) = (panel.opened_at, panel.first_rows_logged) {
            panel.first_rows_logged = true;
            log_first_rows(opened, began, rows_ms, drawn_ms);
        }
        let chars = panel.query.len().to_string();
        let rows = panel.items.len().to_string();
        let apps = panel
            .items
            .iter()
            .filter(|item| item.section == Section::Apps)
            .count()
            .to_string();
        let mut order: Vec<&str> = Vec::new();
        for item in &panel.items {
            let key = item.section.key();
            if order.last() != Some(&key) {
                order.push(key);
            }
        }
        let sections = order.join(",");
        let first_action = panel
            .items
            .iter()
            .find(|item| item.section == Section::Actions)
            .map(|item| item.title.split(':').next().unwrap_or_default().to_string())
            .unwrap_or_default();
        host::log_info(
            "launcher_query",
            &[
                ("chars", &chars),
                ("rows", &rows),
                ("apps", &apps),
                ("sections", &sections),
                ("first_action", &first_action),
                ("rows_ms", &format!("{rows_ms:.1}")),
                ("drawn_ms", &format!("{drawn_ms:.1}")),
            ],
        );
    });
}

/// Release builds record nothing here: the first-rows timing log is a no-op.
fn log_first_rows(_: Instant, _: Instant, _: f32, _: f32) {}

/// The notch process's memory after the first opening (the 11 to 19 MB target of
/// ), once per run.
fn log_memory() {
    let Ok(usage) = rightkit_child_process::process_usage() else {
        return;
    };
    let mb = |bytes: u64| format!("{:.1}", bytes as f64 / 1_048_576.0);
    host::log_info(
        "launcher_memory",
        &[
            ("resident_mb", &mb(usage.resident_bytes)),
            ("footprint_mb", &mb(usage.footprint_bytes)),
            ("when", "first_hide"),
        ],
    );
}

fn edit_count() -> u64 {
    with_panel(|panel| panel.edits).unwrap_or(0)
}

extern "system" fn panel_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let vk = wparam.0 as u32;
    match message {
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            let _ = unsafe { ValidateRect(Some(hwnd), None) };
            return LRESULT(0);
        }
        WM_ACTIVATE if wparam.0 & 0xFFFF == 0 => {
            // Another window took the foreground: it keeps it.
            with_panel(|panel| panel.hide(false));
            return LRESULT(0);
        }
        WM_CLOSE => {
            with_panel(|panel| panel.hide(true));
            return LRESULT(0);
        }
        WM_CHAR => {
            let (began, before) = (Instant::now(), edit_count());
            let action = with_panel(|panel| panel.typed(wparam.0 as u16));
            carry_out(action.unwrap_or(Do::Nothing));
            report_query(began, before);
            return LRESULT(0);
        }
        WM_KEYDOWN => {
            let (began, before) = (Instant::now(), edit_count());
            let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
            let shift = unsafe { GetKeyState(0x10) } < 0;
            let action = with_panel(|panel| {
                if vk == VK_TAB {
                    // Tab and Shift+Tab walk the action rows.
                    return if panel.tab(shift) {
                        Do::Redraw
                    } else {
                        Do::Nothing
                    };
                }
                match panel.clip_key(vk, ctrl, shift) {
                    Some(done) => done,
                    None => panel.key(vk, ctrl),
                }
            });
            carry_out(action.unwrap_or(Do::Nothing));
            report_query(began, before);
            return LRESULT(0);
        }
        WM_SYSKEYDOWN => {
            if let Some(Some(action)) = with_panel(|panel| panel.alt_key(vk)) {
                carry_out(action);
                return LRESULT(0);
            }
        }
        // No menu to open and no beep for a letter typed with Alt.
        WM_SYSCHAR => return LRESULT(0),
        WM_LBUTTONUP => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let action = with_panel(|panel| panel.click(x, y));
            carry_out(action.unwrap_or(Do::Nothing));
            return LRESULT(0);
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) & 0xFFFF) as i16;
            let action = with_panel(|panel| panel.move_by(if delta > 0 { -1 } else { 1 }));
            carry_out(action.unwrap_or(Do::Nothing));
            return LRESULT(0);
        }
        WM_TIMER if wparam.0 == PASTE_TIMER => {
            let _ = unsafe { KillTimer(Some(hwnd), PASTE_TIMER) };
            host::paste();
            return LRESULT(0);
        }
        WM_TIMER if wparam.0 == FILES_TIMER => {
            with_panel(|panel| {
                if panel.files_job.is_none() || panel.take_files() {
                    let _ = unsafe { KillTimer(Some(hwnd), FILES_TIMER) };
                    // The names are in: search for what is already typed.
                    if panel.shown && panel.output.is_none() {
                        panel.run_file_search();
                    }
                }
            });
            return LRESULT(0);
        }
        WM_TIMER if wparam.0 == QUERY_TIMER => {
            let _ = unsafe { KillTimer(Some(hwnd), QUERY_TIMER) };
            with_panel(|panel| {
                if panel.shown && panel.output.is_none() && panel.prompt.is_none() {
                    panel.run_searches();
                }
            });
            return LRESULT(0);
        }
        WM_TIMER if wparam.0 == REFRESH_TIMER => {
            let _ = unsafe { KillTimer(Some(hwnd), REFRESH_TIMER) };
            // The running programs are read again away from this thread (`WM_APPS`).
            spawn_scan(false);
            return LRESULT(0);
        }
        _ => {}
    }
    // SAFETY: default processing of the unchanged message.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

fn module_instance() -> Option<HINSTANCE> {
    unsafe { GetModuleHandleW(None) }.ok().map(Into::into)
}

fn thread_main() {
    let Some(instance) = module_instance() else {
        return;
    };
    // Declared before the window so it is unregistered after the window is gone.
    let _class = match ClassGuard::register(PANEL_CLASS, Some(panel_proc), instance) {
        Ok(class) => class,
        Err(error) => {
            host::log_error("RegisterClassW", &error, "launcher");
            return;
        }
    };
    let window = match OwnedWindow::overlay(
        PANEL_CLASS,
        &host::window_title(),
        (0, 0, 0, 0),
        rightkit_overlay::OverlayStyle::KEYBOARD,
        instance,
    ) {
        Ok(window) => window,
        Err(error) => {
            host::log_error("CreateWindowExW", &error, "launcher");
            return;
        }
    };
    let Some(painter) = TextPainter::new() else {
        return;
    };
    PANEL.with(|cell| {
        *cell.borrow_mut() = Some(Panel {
            window,
            painter,
            keys: None,
            hot: None,
            applied: None,
            shown: false,
            still: false,
            scale: 1.0,
            origin: (0, 0),
            previous: 0,
            query: Vec::new(),
            caret: 0,
            high: None,
            items: Vec::new(),
            selected: 0,
            tops: Vec::new(),
            scroll: 0.0,
            apps: Vec::new(),
            running: Vec::new(),
            pins: Vec::new(),
            config: Config::default(),
            output: None,
            editor: None,
            runs: 0,
            files: None,
            files_job: None,
            file_rows: Vec::new(),
            file_text: String::new(),
            prompt: None,
            rows_done: None,
            edits: 0,
            before_tab: 0,
            rates: None,
            usage: Usage::load(),
            slow: None,
            chats: Vec::new(),
            opened_at: None,
            first_rows_logged: false,
        });
    });

    let mut message = MSG::default();
    // Creates this thread's message queue before the id is published.
    let _ = unsafe { PeekMessageW(&mut message, None, WM_USER, WM_USER, PM_NOREMOVE) };
    THREAD_ID.store(unsafe { GetCurrentThreadId() }, Ordering::Relaxed);
    with_panel(Panel::sync_hotkey);
    // Warm what the first Alt+Space would otherwise pay for: one off-screen render (the text
    // painter's fonts and glyph cache). The window exists already, and the app list with its
    // icons is read by `spawn_scan` as soon as the launcher is switched on (`sync_hotkey`).
    with_panel(|panel| {
        let _ = panel.render();
    });

    while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {
        if message.hwnd.0.is_null() {
            match message.message {
                WM_SYNC => {
                    with_panel(|panel| {
                        panel.sync_hotkey();
                        panel.resettle_if_answer_changed();
                    });
                }
                WM_TOGGLE => toggle_panel(),
                WM_OUTPUT => {
                    with_panel(Panel::take_outcome);
                }
                WM_RATES => {
                    with_panel(Panel::take_rates);
                }
                WM_OPEN_CLIP => {
                    with_panel(Panel::open_clip_mode);
                }
                WM_PROMPT => {
                    with_panel(Panel::show_prompt);
                }
                WM_APPS => {
                    with_panel(Panel::take_scanned);
                }
                WM_FILE_ROWS => {
                    with_panel(Panel::take_file_rows);
                }
                WM_SLOW_ROWS => {
                    with_panel(Panel::take_slow_rows);
                }
                WM_AI_ANSWER => {
                    with_panel(Panel::take_ai_answer);
                }
                WM_DEFINE => {
                    with_panel(Panel::take_definition);
                }
                WM_ICONS => {
                    with_panel(|panel| {
                        if panel.shown && panel.output.is_none() {
                            panel.draw();
                        }
                    });
                }
                _ => {}
            }
        } else {
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
    THREAD_ID.store(0, Ordering::Relaxed);
    PANEL.with(|cell| drop(cell.borrow_mut().take()));
}
