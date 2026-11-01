//! Snippet keyword expansion as you type (the Mac's `SnippetExpansion.swift`): typing a
//! snippet's keyword in any program replaces it with the snippet's text. Off until the
//! `launcherSnippetExpansion` switch is on, and only while the launcher is on too (as on the Mac).
//!
//! Trigger rule (as on the Mac): a keyword is 2 to 32 characters, starts with a symbol (not a
//! letter or digit), and holds no space, control character, `--` or `...`. It is matched exactly,
//! case included; a keyword that begins a longer keyword is dropped, since it would block the
//! longer one. It fires the moment its last character is typed, and only when the character
//! before it is not a letter or digit, so it cannot fire inside a word. A snippet that uses
//! `{argument}` never expands: nothing can supply the argument.
//!
//! Privacy: the rolling buffer (at most the longest keyword, never more than 32 characters) lives
//! in memory only. It is cleared by a left click (`clicked`, from the keys hook), a key that is not
//! a plain character (arrows, Escape, Ctrl, Alt or Windows combinations; AltGr characters count as
//! typing), a change of foreground window, and 5 seconds after the last key. Nothing is kept while
//! a password field has focus (a classic Win32 password box, or a UI Automation element reporting
//! IsPassword, read from a cache a watcher thread refreshes every 100 ms, never queried per key;
//! a stale or missing answer reads as a password field), a
//! window of the application is in front, or the keyboard layout is an input method (Chinese, Japanese or
//! Korean). Keys another program injects never reach this module (the keys hook drops every
//! `LLKHF_INJECTED` event first). The hook never swallows a key, and nothing typed is logged.
//!
//! Replacing: 50 ms after the last character, if no other key or click came in, and only when the
//! focused element of the same program is an edit or combo box that is not a password field (UI
//! Automation), it saves the clipboard, puts the snippet there as the application's own item (excluded from
//! clipboard monitoring, Windows' clipboard history and the cloud), checks everything again, and
//! only then sends one Backspace per keyword character (the typing generation is read once more
//! right before them, so a key typed while the clipboard was saved cancels the expansion before
//! any Backspace). Ctrl+V follows after checking everything again. The clipboard's previous formats
//! go back 1.5 s later, unless something else was copied meanwhile; a failed restore is retried
//! with backoff on a thread, and a snapshot still not restored is kept for the next expansion. Nothing happens when the clipboard
//! holds a password manager's item or cannot be kept whole.
//!
//! Differences from the Mac, named: a program whose focused element does not report to UI
//! Automation as an edit or combo box (some web pages' editable areas, for one) is not expanded
//! in; a document with no edit control is never a target. Input methods are judged by the keyboard
//! layout's language, not by their composition state.

use crate::config as launcher_config;
use crate::host;
use rightkit_automation::win::input::{ModifierSnapshot, RawInput, vk};
use rightkit_automation::win::uia;
use rightkit_clipboard_history::capture as clip_capture;
use rightkit_clipboard_history::win::{self, Open};
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyState, GetKeyboardLayout, ToUnicodeEx,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GUITHREADINFO, GWL_STYLE, GetForegroundWindow, GetGUIThreadInfo, GetWindowLongPtrW,
    GetWindowThreadProcessId,
};

const VK_BACK: u32 = 0x08;
const VK_TAB: u32 = 0x09;
const VK_RETURN: u32 = 0x0D;
const VK_CONTROL: u32 = 0x11;
const VK_SHIFT: u32 = 0x10;
const VK_MENU: u32 = 0x12;
const VK_CAPITAL: u32 = 0x14;
const VK_NUMLOCK: u32 = 0x90;
const VK_LSHIFT: u32 = 0xA0;
const VK_RSHIFT: u32 = 0xA1;
/// The edit style of a classic password box.
const ES_PASSWORD: isize = 0x0020;
/// The most characters a keyword may have, as on the Mac.
const HARD_CAP: usize = 32;
const IDLE: Duration = Duration::from_secs(5);
const EXPAND_DELAY: Duration = Duration::from_millis(50);
const CHECK_DELAY: Duration = Duration::from_millis(80);
const RESTORE_DELAY: Duration = Duration::from_millis(1500);
const SNAPSHOT_BYTES: usize = 32 * 1024 * 1024;
const WRITE_WAITS: [u64; 9] = [20; 9];
/// Pauses between attempts to put the clipboard back after a failed restore.
const RESTORE_RETRIES: [u64; 6] = [100, 200, 400, 800, 1600, 3200];
/// How often the focus watcher looks at the focused element, and how old its answer may be.
const FOCUS_POLL: Duration = Duration::from_millis(100);
const FOCUS_FRESH: Duration = Duration::from_millis(400);
/// Formats held as GDI handles: they cannot be kept as bytes, so such a clipboard is not restored.
const GDI_FORMATS: [u32; 8] = [2, 3, 9, 14, 0x0080, 0x0082, 0x0083, 0x008E];

/// Keyword characters, and the text each expands to (nested snippets already included).
type Entry = (Vec<char>, String);

struct State {
    table: Vec<Entry>,
    listening: bool,
    buffer: Vec<char>,
    /// Whether the character before `buffer` is known not to be a letter or digit.
    boundary_known: bool,
    last_key: Option<Instant>,
    /// The foreground window at the last key (its handle as an address).
    front: usize,
    /// Advances with every key, click and clear; a pending expansion checks it is unchanged.
    generation: u64,
}

impl State {
    const EMPTY: State = State {
        table: Vec::new(),
        listening: false,
        buffer: Vec::new(),
        boundary_known: false,
        last_key: None,
        front: 0,
        generation: 0,
    };

    fn clear(&mut self, boundary: bool) {
        self.buffer.clear();
        self.boundary_known = boundary;
        self.generation += 1;
    }
}

static STATE: Mutex<State> = Mutex::new(State::EMPTY);
/// Serialises `sync`, which runs on a thread of its own per settings change.
static SYNC: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The settings changed: reads the switch and the snippets again. It runs on a thread of its own,
/// because the caller holds the settings lock while it publishes them.
pub fn sync_later() {
    let _ = std::thread::Builder::new()
        .name("rightkit-snippet-sync".to_string())
        .spawn(sync);
}

fn sync() {
    let _turn = SYNC.lock().unwrap_or_else(PoisonError::into_inner);
    let on = host::switch_on("launcherSnippetExpansion") && host::switch_on("launcherEnabled");
    let table = if on {
        build_table(&launcher_config::parse(&host::launcher_config_json()).snippets)
    } else {
        Vec::new()
    };
    let listening = !table.is_empty();
    host::snippets_need_hook(listening);
    let mut state = lock();
    state.table = table;
    state.listening = listening;
    state.clear(false);
    drop(state);
    if listening {
        start_focus_watch();
    }
}

/// The expandable keywords and their texts, as the Mac's `configure` builds them. The first
/// snippet with a keyword decides it, even when that snippet cannot expand.
fn build_table(snippets: &[launcher_config::Snippet]) -> Vec<Entry> {
    let mut seen: Vec<&str> = Vec::new();
    let mut found: Vec<Entry> = Vec::new();
    for snippet in snippets.iter().filter(|s| !s.body.is_empty()) {
        if seen.contains(&snippet.keyword.as_str()) {
            continue;
        }
        seen.push(&snippet.keyword);
        let Some(keyword) = trigger_keyword(&snippet.keyword) else {
            continue;
        };
        let visiting: Vec<String> = [&snippet.name, &snippet.keyword]
            .iter()
            .map(|name| name.to_lowercase())
            .filter(|name| !name.is_empty())
            .collect();
        let nested = launcher_config::expand(&snippet.body, snippets, &visiting);
        if nested.contains("{argument}") {
            continue;
        }
        found.push((keyword, nested));
    }
    let all = found.clone();
    found.retain(|(keyword, _)| {
        !all.iter()
            .any(|(other, _)| other.len() > keyword.len() && other[..keyword.len()] == keyword[..])
    });
    found
}

/// The keyword's characters when it may trigger, else nil.
fn trigger_keyword(raw: &str) -> Option<Vec<char>> {
    let chars: Vec<char> = raw.chars().collect();
    if chars.len() < 2 || chars.len() > HARD_CAP {
        return None;
    }
    let first = chars[0];
    if is_word(first) {
        return None;
    }
    // Smart substitutions in the target field would change the length.
    if raw.contains("--") || raw.contains("...") {
        return None;
    }
    for &ch in &chars {
        if ch.is_whitespace() || (ch as u32) < 0x20 || ch as u32 == 0x7F {
            return None;
        }
    }
    Some(chars)
}

/// Letters and digits: what a keyword may not follow.
fn is_word(ch: char) -> bool {
    ch.is_alphabetic() || ch.is_numeric()
}

/// A left click: the typed text is no longer adjacent to what comes next. Called from the keys
/// hook's mouse callback, on the hook thread.
pub fn clicked() {
    let mut state = lock();
    if state.listening {
        state.clear(true);
    }
}

/// A physical key went down, other than a modifier and other than an injected key. Called from
/// the keys hook, on the hook thread: it reads a few window and keyboard states and returns.
pub fn key_down(vk: u32, scan: u32) {
    let front = foreground();
    let now = Instant::now();
    let mut state = lock();
    if !state.listening {
        return;
    }
    // `None`: a shortcut (Ctrl, Alt or Windows key), not typing. `Some(true)`: AltGr, which types.
    let altgr = typing_modifiers();
    let blocked = front == 0
        || altgr.is_none()
        || own_window(front)
        || classic_password_focus(front)
        || focused_password(front)
        || !plain_layout(front);
    if blocked {
        state.clear(false);
        return;
    }
    if front != state.front {
        state.clear(true);
    } else if state
        .last_key
        .is_some_and(|last| now.duration_since(last) > IDLE)
    {
        state.clear(false);
    }
    state.front = front;
    state.last_key = Some(now);
    state.generation += 1;
    if vk == VK_RETURN || vk == VK_TAB {
        state.clear(true);
        return;
    }
    if vk == VK_BACK {
        if state.buffer.is_empty() {
            state.boundary_known = false;
        } else {
            state.buffer.pop();
        }
        return;
    }
    let Some(text) = typed_text(vk, scan, front, altgr == Some(true)) else {
        state.clear(false);
        return;
    };
    for ch in text.chars() {
        state.buffer.push(ch);
        if state.buffer.len() > state.table_cap() {
            let removed = state.buffer.remove(0);
            state.boundary_known = !is_word(removed);
        }
    }
    let mut matched: Option<usize> = None;
    for (index, (keyword, _)) in state.table.iter().enumerate() {
        let length = keyword.len();
        if length > state.buffer.len() {
            continue;
        }
        let start = state.buffer.len() - length;
        if state.buffer[start..] != keyword[..] {
            continue;
        }
        let boundary = if start > 0 {
            !is_word(state.buffer[start - 1])
        } else {
            state.boundary_known
        };
        if !boundary {
            continue;
        }
        if matched.is_some_and(|current| state.table[current].0.len() >= length) {
            continue;
        }
        matched = Some(index);
    }
    let Some(index) = matched else {
        return;
    };
    state.clear(false);
    let token = state.generation;
    let count = state.table[index].0.len();
    let body = state.table[index].1.clone();
    drop(state);
    let spawned = std::thread::Builder::new()
        .name("rightkit-snippets".to_string())
        .spawn(move || {
            std::thread::sleep(EXPAND_DELAY);
            expand(&body, count, token, front);
        });
    if spawned.is_err() {
        host::log_info("snippets", &[("result", "no_thread")]);
    }
}

impl State {
    /// The longest keyword, which is the most the buffer keeps.
    fn table_cap(&self) -> usize {
        self.table
            .iter()
            .map(|(keyword, _)| keyword.len())
            .max()
            .unwrap_or(0)
            .min(HARD_CAP)
    }
}

/// The whole guard, run before the Backspaces and again before the paste.
fn may_replace(token: u64, front: usize) -> bool {
    let (listening, generation_ok) = {
        let state = lock();
        (state.listening, state.generation == token)
    };
    if !listening || !generation_ok {
        return false;
    }
    let now = foreground();
    if now == 0 || now != front || own_window(now) || classic_password_focus(now) {
        return false;
    }
    plain_layout(now) && text_field_focused(now)
}

/// Runs on a thread of its own: checks, the snippet text, the clipboard, the Backspaces, the paste
/// and the restore of the clipboard.
fn expand(body: &str, count: usize, token: u64, front: usize) {
    let _com = Com::start();
    if !may_replace(token, front) {
        return;
    }
    // `{clipboard}` is read before the clipboard is replaced.
    let text = host::snippet_text(body);
    if text.is_empty() {
        return;
    }
    // A snapshot an earlier expansion could not put back goes first (the clipboard still holds its
    // transient text, which `snapshot` would refuse).
    retry_pending();
    // Save the clipboard first: if it cannot be kept whole, or holds a password manager's item,
    // nothing changes.
    let Some(saved) = snapshot() else {
        return;
    };
    if !write_transient(&text) {
        give_back(saved, None);
        return;
    }
    let written = win::sequence();
    // The snapshot above can take a while, and keys typed meanwhile reached the program: check
    // everything again, and the typing generation once more at the last moment, so a Backspace is
    // never sent once the buffer has changed.
    if !may_replace(token, front) || !still_current(token) {
        give_back(saved, Some(written));
        return;
    }
    let mut events = Vec::with_capacity(count * 2);
    for _ in 0..count {
        events.extend(RawInput::key_press(VK_BACK as u16));
    }
    host::send_input(&events, "snippets");
    std::thread::sleep(CHECK_DELAY);
    // A key, click or window change since the checks may have moved the caret.
    if !may_replace(token, front) {
        give_back(saved, Some(written));
        return;
    }
    host::paste();
    host::log_info("snippets", &[("result", "expanded")]);
    std::thread::sleep(RESTORE_DELAY);
    give_back(saved, Some(written));
}

/// Whether no key, click or clear came in since the expansion was scheduled. Cheap: it only reads
/// the typing generation, so it can sit right before the Backspaces.
fn still_current(token: u64) -> bool {
    let state = lock();
    state.listening && state.generation == token
}

/// The clipboard's formats with their bytes, or `None` when it cannot be kept whole: a password
/// manager's copy (never read), a GDI format, a format that cannot be read, or more than 32 MB.
fn snapshot() -> Option<Vec<(u32, Vec<u8>)>> {
    let open = Open::acquire(&WRITE_WAITS)?;
    let f = win::formats();
    let formats = win::format_list(&open);
    if formats
        .iter()
        .any(|&format| format == f.exclude || format == f.no_history)
    {
        return None;
    }
    let mut total = 0;
    let mut kept = Vec::with_capacity(formats.len());
    for format in formats {
        if GDI_FORMATS.contains(&format) {
            return None;
        }
        let bytes = win::read_bytes(&open, format, SNAPSHOT_BYTES - total)?;
        total += bytes.len();
        kept.push((format, bytes));
    }
    Some(kept)
}

/// Puts the snippet on the clipboard as the application's own transient item. False when it could not.
fn write_transient(text: &str) -> bool {
    let Some(open) = Open::acquire(&WRITE_WAITS) else {
        return false;
    };
    if !win::empty(&open) || !win::set_text(&open, text) {
        return false;
    }
    let f = win::formats();
    win::set_dword(&open, f.own, 1);
    win::set_dword(&open, f.exclude, 1);
    win::set_dword(&open, f.no_history, 0);
    win::set_dword(&open, f.no_cloud, 0);
    drop(open);
    clip_capture::note_own_write();
    true
}

/// How one attempt to put the clipboard back ended.
enum Restored {
    /// Every format is back.
    Done,
    /// The clipboard no longer holds what the expansion wrote (the user copied something), so
    /// nothing is put back and nothing is left to retry.
    Moved,
    /// The clipboard was busy or a format could not be set: try again.
    Failed,
}

/// A snapshot that could not be put back yet, with the sequence of the text the expansion wrote:
/// a later expansion retries it first. Only while the clipboard still holds that text.
/// A clipboard snapshot (format, bytes) waiting to be put back, and the sequence it was taken at.
type PendingRestore = (Vec<(u32, Vec<u8>)>, Option<u32>);
static PENDING: Mutex<Option<PendingRestore>> = Mutex::new(None);

/// One attempt to put the saved formats back. With `written`, only while the clipboard still holds
/// what the expansion wrote; anything copied meanwhile is left alone.
fn restore(saved: &[(u32, Vec<u8>)], written: Option<u32>) -> Restored {
    if written.is_some_and(|sequence| win::sequence() != sequence) {
        return Restored::Moved;
    }
    let Some(open) = Open::acquire(&WRITE_WAITS) else {
        return Restored::Failed;
    };
    if written.is_some_and(|sequence| win::sequence() != sequence) {
        return Restored::Moved;
    }
    if !win::empty(&open) {
        return Restored::Failed;
    }
    let mut complete = true;
    for (format, bytes) in saved {
        if !win::set_bytes(&open, *format, bytes) {
            complete = false;
        }
    }
    if !win::set_dword(&open, win::formats().own, 1) {
        complete = false;
    }
    drop(open);
    clip_capture::note_own_write();
    if complete {
        Restored::Done
    } else {
        Restored::Failed
    }
}

/// Puts the clipboard back. A failed attempt is retried with a growing pause on a thread of its
/// own; if every retry fails the snapshot is kept for the next expansion and the failure is logged.
fn give_back(saved: Vec<(u32, Vec<u8>)>, written: Option<u32>) {
    if !matches!(restore(&saved, written), Restored::Failed) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("rightkit-snippets-restore".to_string())
        .spawn(move || {
            for wait in RESTORE_RETRIES {
                std::thread::sleep(Duration::from_millis(wait));
                match restore(&saved, written) {
                    Restored::Failed => {}
                    _ => return,
                }
            }
            host::log_info("snippets", &[("result", "restore_failed_kept")]);
            *PENDING.lock().unwrap_or_else(PoisonError::into_inner) = Some((saved, written));
        });
    if spawned.is_err() {
        host::log_info("snippets", &[("result", "restore_no_thread")]);
    }
}

/// Tries once to put back a snapshot kept by `give_back`, keeping it when that fails again.
fn retry_pending() {
    let pending = PENDING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    let Some((saved, written)) = pending else {
        return;
    };
    if matches!(restore(&saved, written), Restored::Failed) {
        *PENDING.lock().unwrap_or_else(PoisonError::into_inner) = Some((saved, written));
    }
}

/// True only when the focused element of the foreground program is positively an edit or combo
/// box that is not a password field. Anything unknown, or a failed query, reads as false.
fn text_field_focused(window: usize) -> bool {
    let Some(element) = uia::focused_element() else {
        return false;
    };
    if element.pid() != Some(process_of(window) as i32) {
        return false;
    }
    // An unreadable password flag reads as a password field.
    if element.password_state().unwrap_or(true) {
        return false;
    }
    // The published role is "textfield" for exactly the Edit and ComboBox control types.
    uia::read_node(&element).is_some_and(|node| node.role == "textfield")
}

/// Whether the focused classic edit control of the window's program is a password box.
fn classic_password_focus(window: usize) -> bool {
    if window == 0 {
        return false;
    }
    let mut info = GUITHREADINFO {
        cbSize: size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetGUIThreadInfo(thread_of(window), &mut info) }.is_err() {
        return false;
    }
    let focus = info.hwndFocus;
    if focus.0.is_null() {
        return false;
    }
    // SAFETY: a plain query; the style is read as its low bits, which is all ES_* uses.
    let style = unsafe { GetWindowLongPtrW(focus, GWL_STYLE) };
    style & ES_PASSWORD != 0
}

/// Whether the window belongs to this application (the launcher and every other window of the process).
fn own_window(window: usize) -> bool {
    process_of(window) == unsafe { GetCurrentProcessId() }
}

/// Whether the keyboard layout is a plain one, not an input method (Chinese, Japanese, Korean).
fn plain_layout(window: usize) -> bool {
    if window == 0 {
        return false;
    }
    let layout = unsafe { GetKeyboardLayout(thread_of(window)) };
    let primary = ((layout.0 as usize) & 0xFFFF) & 0x3FF;
    !matches!(primary, 0x04 | 0x11 | 0x12)
}

/// What the held modifiers make of a key. `None`: a shortcut (the Windows key, Ctrl alone or Alt
/// alone), which is not typing. `Some(true)`: Ctrl and Alt together, which is how Windows reports
/// AltGr (right Alt, with a Ctrl it adds itself), so the key may type a character; `typed_text`
/// decides, and a Ctrl+Alt chord that types nothing is dropped there. `Some(false)`: plain typing.
fn typing_modifiers() -> Option<bool> {
    let held = ModifierSnapshot::capture();
    if held.contains(vk::LWIN) || held.contains(vk::RWIN) {
        return None;
    }
    let ctrl = held.contains(vk::LCONTROL) || held.contains(vk::RCONTROL);
    let alt = held.contains(vk::LMENU) || held.contains(vk::RMENU);
    match (ctrl, alt) {
        (false, false) => Some(false),
        (true, true) => Some(true),
        _ => None,
    }
}

/// The focused element as the watcher last saw it: the foreground window it was seen under, whether
/// it is a password field, and when.
struct FocusSeen {
    window: usize,
    password: bool,
    at: Instant,
}

static FOCUS: Mutex<Option<FocusSeen>> = Mutex::new(None);
static WATCHING: AtomicBool = AtomicBool::new(false);

/// Whether the focused element is a password field, from the watcher's cache: the hook never makes
/// a UI Automation query itself. An answer that is missing, for another window or older than
/// `FOCUS_FRESH` reads as true (not typing we may read), which only costs a keyword started in the
/// first moments after a focus change.
fn focused_password(window: usize) -> bool {
    let seen = FOCUS.lock().unwrap_or_else(PoisonError::into_inner);
    match &*seen {
        Some(known) if known.window == window && known.at.elapsed() <= FOCUS_FRESH => {
            known.password
        }
        _ => true,
    }
}

/// Starts the focus watcher (one thread) while snippets are on. It asks UI Automation for the
/// focused element every `FOCUS_POLL` and caches whether it is a password field; it ends when
/// snippets turn off.
fn start_focus_watch() {
    if WATCHING.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("rightkit-snippet-focus".to_string())
        .spawn(|| {
            let _com = Com::start();
            loop {
                std::thread::sleep(FOCUS_POLL);
                if !lock().listening {
                    break;
                }
                let window = foreground();
                let password = uia::focused_element().and_then(|element| {
                    if element.pid()? != process_of(window) as i32 {
                        return None;
                    }
                    element.password_state()
                });
                let mut seen = FOCUS.lock().unwrap_or_else(PoisonError::into_inner);
                *seen = password.map(|password| FocusSeen {
                    window,
                    password,
                    at: Instant::now(),
                });
            }
            *FOCUS.lock().unwrap_or_else(PoisonError::into_inner) = None;
            WATCHING.store(false, Ordering::Release);
            // Snippets may have turned on again between the last check and the flag going down.
            if lock().listening {
                start_focus_watch();
            }
        });
    if spawned.is_err() {
        WATCHING.store(false, Ordering::Release);
        host::log_info("snippets", &[("result", "no_focus_thread")]);
    }
}

/// The characters the key types in the window's keyboard layout, with the shift and lock keys as
/// they are now (with `altgr`, Ctrl and Alt both down, as the layout reads AltGr). Nil for a dead key, a key that types nothing, or a control character.
fn typed_text(vk: u32, scan: u32, window: usize, altgr: bool) -> Option<String> {
    let mut key_state = [0u8; 256];
    if altgr {
        for code in [VK_CONTROL, VK_MENU] {
            key_state[code as usize] = 0x80;
        }
    }
    for code in [VK_SHIFT, VK_LSHIFT, VK_RSHIFT] {
        if unsafe { GetAsyncKeyState(code as i32) } < 0 {
            key_state[code as usize] = 0x80;
        }
    }
    for code in [VK_CAPITAL, VK_NUMLOCK] {
        if (unsafe { GetKeyState(code as i32) } & 1) != 0 {
            key_state[code as usize] = 1;
        }
    }
    let layout = unsafe { GetKeyboardLayout(thread_of(window)) };
    let mut units = [0u16; 8];
    // Flag 0x4 leaves a pending dead key alone, so this probe does not change the layout's state.
    let written = unsafe { ToUnicodeEx(vk, scan, &key_state, &mut units, 0x4, Some(layout)) };
    if written <= 0 {
        return None;
    }
    let count = (written as usize).min(units.len());
    let typed = String::from_utf16(&units[..count]).ok()?;
    let controlled = typed.chars().any(|ch| {
        let code = ch as u32;
        code < 0x20 || code == 0x7F || (0xE000..=0xF8FF).contains(&code)
    });
    if controlled { None } else { Some(typed) }
}

fn foreground() -> usize {
    unsafe { GetForegroundWindow() }.0 as usize
}

fn hwnd(window: usize) -> HWND {
    HWND(window as *mut std::ffi::c_void)
}

/// The thread that owns the window (its keyboard layout and focus belong to it).
fn thread_of(window: usize) -> u32 {
    unsafe { GetWindowThreadProcessId(hwnd(window), None) }
}

fn process_of(window: usize) -> u32 {
    let mut process = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd(window), Some(&mut process as *mut u32)) };
    process
}

/// Balances a successful `CoInitializeEx` on this thread.
struct Com(bool);

impl Com {
    fn start() -> Com {
        Com(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}
