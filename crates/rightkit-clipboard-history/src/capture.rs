//! Watching the clipboard (the spec's section 3, Windows): a hidden message-only window asks
//! Windows to tell it about every change (`AddClipboardFormatListener`); a worker thread then
//! waits 40 ms, opens the clipboard, applies the skip rules, reads what is there once and hands
//! it to the store (`clip.rs`). Nothing is read while the `launcherClipboard` switch is off: the
//! listener is taken away, there is no timer, no clipboard call. The thread that owns the
//! window stays parked in `GetMessageW`, which costs nothing.
//!
//! Skipped, and never stored (spec 3.5): the application's own write-backs (its own-write format),
//! copies marked `ExcludeClipboardContentFromMonitorProcessing` or
//! `CanIncludeInClipboardHistory` = 0, copies of a kind whose switch is off, copies from an
//! excluded app, a text that is wholly a secret, and anything over the size limits. A skip is
//! not logged and not counted. Nothing here ever logs content, paths or previews.

use crate::host;
use crate::model::{self as clip, Capture, Kind, SourceApp, Status};
use crate::win::{self as win, CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT, Open};
use crate::{detect as clip_detect, img as clip_img};
use rightkit_win_ui::process;
use rightkit_win_ui::raii::{ClassGuard, OwnedWindow, hwnd_from_key, hwnd_key};
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PCWSTR, w};

const CLASS: PCWSTR = w!("RightKitClipboardListener");
const WM_SYNC: u32 = WM_APP + 1;
const WM_CLIPBOARDUPDATE: u32 = 0x031D;
const RETENTION_TIMER: usize = 1;
/// The retention pass runs once an hour while history is on.
const RETENTION_MS: u32 = 3_600_000;
/// After a change, before the clipboard is opened: some programs write in several steps.
const SETTLE_MS: u64 = 40;
/// When another program holds the clipboard open: tries again after each of these, then gives
/// up on that change.
const OPEN_WAITS: [u64; 5] = [25, 50, 100, 200, 400];
/// Reading one change takes at most this long (a delay-rendering app can block).
const READ_LIMIT: Duration = Duration::from_secs(2);
/// Largest UTF-16 text block read (a 1 MB UTF-8 text is at most 2 MB of units, and more is
/// refused by the store).
const TEXT_BLOCK_MAX: usize = 4 * 1024 * 1024;

/// Apps whose copies are screenshots (spec 4); the application's own tools are told by process.
const SCREENSHOT_APPS: [&str; 4] = [
    "snippingtool.exe",
    "screenclippinghost.exe",
    "shellexperiencehost.exe",
    "sharex.exe",
];

static THREAD_ID: AtomicU32 = AtomicU32::new(0);
/// The change count after the application's own last write: that change is not a copy.
static OWN_SEQ: AtomicU32 = AtomicU32::new(0);
/// The last change count the listener acted on (some programs announce one change twice).
static LAST_SEQ: AtomicU32 = AtomicU32::new(0);
static WORK: Mutex<Option<Sender<Change>>> = Mutex::new(None);

/// One announced change, with what was known the moment it came.
struct Change {
    seq: u32,
    /// The window that wrote the clipboard (a handle value), and the foreground window then.
    owner: isize,
    foreground: isize,
}

/// Starts the thread that owns the listener and the one that reads. Both stay parked until
/// the switch is on.
pub fn start() {
    let (tx, rx) = mpsc::channel::<Change>();
    *WORK.lock().unwrap_or_else(PoisonError::into_inner) = Some(tx);
    let reader = std::thread::Builder::new()
        .name("rightkit-clip-read".to_string())
        .spawn(move || reader_main(&rx));
    let owner = std::thread::Builder::new()
        .name("rightkit-clip-listen".to_string())
        .spawn(listener_main);
    if reader.is_err() || owner.is_err() {
        host::log_info("clipboard", &[("result", "no_thread")]);
    }
}

/// The master switch changed (or settings did): the listener follows it.
pub fn sync() {
    let id = THREAD_ID.load(Ordering::Relaxed);
    if id != 0 {
        // Failure means the thread is gone or has no queue yet; nothing to recover.
        let _ = unsafe { PostThreadMessageW(id, WM_SYNC, WPARAM(0), LPARAM(0)) };
    }
}

/// The application wrote the clipboard itself (a paste or a copy from the strip): that change, and the
/// ones the write makes while it runs, are not copies. Called with the clipboard closed.
pub fn note_own_write() {
    OWN_SEQ.store(win::sequence(), Ordering::Relaxed);
}

// ---- the listener thread ------------------------------------------------------------------

/// What the listener thread keeps: the window and whether the listener is on.
struct Listener {
    window: OwnedWindow,
    on: bool,
}

thread_local! {
    static LISTENER: std::cell::RefCell<Option<Listener>> = const { std::cell::RefCell::new(None) };
}

fn listener_main() {
    let Some(instance) = unsafe { GetModuleHandleW(None) }.ok().map(HINSTANCE::from) else {
        return;
    };
    // Declared before the window so it is unregistered after the window is gone.
    let _class = match ClassGuard::register(CLASS, Some(listener_proc), instance) {
        Ok(class) => class,
        Err(error) => {
            host::log_error("RegisterClassW", &error, "clipboard");
            return;
        }
    };
    // A message-only window: no taskbar, no broadcast, only the clipboard's notices.
    // SAFETY: a plain window creation on this thread; the title is a NUL-terminated literal.
    let created = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            w!("RightKit clipboard"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND(-3isize as *mut c_void)), // HWND_MESSAGE
            None,
            Some(instance),
            None,
        )
    };
    let window = match created {
        Ok(hwnd) => OwnedWindow::adopt(hwnd),
        Err(error) => {
            host::log_error("CreateWindowExW", &error, "clipboard");
            return;
        }
    };
    LISTENER.with(|cell| *cell.borrow_mut() = Some(Listener { window, on: false }));

    let mut message = MSG::default();
    // Creates this thread's message queue before the id is published.
    let _ = unsafe { PeekMessageW(&mut message, None, WM_USER, WM_USER, PM_NOREMOVE) };
    THREAD_ID.store(unsafe { GetCurrentThreadId() }, Ordering::Relaxed);
    apply();
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {
        if message.hwnd.0.is_null() {
            if message.message == WM_SYNC {
                apply();
            }
        } else {
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
    THREAD_ID.store(0, Ordering::Relaxed);
    // The listener goes before the window does.
    LISTENER.with(|cell| {
        if let Some(listener) = cell.borrow_mut().as_ref()
            && listener.on
        {
            win::listen(listener.window.key(), false);
        }
        drop(cell.borrow_mut().take());
    });
}

/// Makes the listener and the hourly timer match the switch.
fn apply() {
    let want = host::switch_on("launcherClipboard");
    let changed = LISTENER.with(|cell| {
        let mut guard = cell.borrow_mut();
        let listener = guard.as_mut()?;
        if want == listener.on {
            return None;
        }
        let key = listener.window.key();
        if want {
            if !win::listen(key, true) {
                host::log_info("clipboard", &[("result", "no_listener")]);
                return None;
            }
            LAST_SEQ.store(win::sequence(), Ordering::Relaxed);
            // SAFETY: a timer on this thread's own window.
            let _ = unsafe {
                SetTimer(
                    Some(hwnd_from_key(key)),
                    RETENTION_TIMER,
                    RETENTION_MS,
                    None,
                )
            };
        } else {
            win::listen(key, false);
            // SAFETY: the timer set above, on this thread's own window.
            let _ = unsafe { KillTimer(Some(hwnd_from_key(key)), RETENTION_TIMER) };
        }
        listener.on = want;
        Some(want)
    });
    if changed == Some(true) {
        // Opens the store (and runs the pass of launch time) off the notch's own threads.
        clip::with(|_| ());
    }
}

extern "system" fn listener_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_CLIPBOARDUPDATE => {
            let seq = win::sequence();
            if LAST_SEQ.swap(seq, Ordering::Relaxed) != seq {
                let change = Change {
                    seq,
                    owner: win::owner(),
                    // SAFETY: a plain query.
                    foreground: hwnd_key(unsafe { GetForegroundWindow() }),
                };
                if let Some(sender) = WORK.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
                    let _ = sender.send(change);
                }
            }
            return LRESULT(0);
        }
        WM_TIMER if wparam.0 == RETENTION_TIMER => {
            clip::with(|store| store.retention_pass());
            return LRESULT(0);
        }
        _ => {}
    }
    // SAFETY: default processing of the unchanged message.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

// ---- the reading thread -------------------------------------------------------------------

fn reader_main(rx: &Receiver<Change>) {
    clip_img::init_com();
    while let Ok(mut change) = rx.recv() {
        std::thread::sleep(Duration::from_millis(SETTLE_MS));
        // Only the newest change is read: what came between is already gone.
        while let Ok(next) = rx.try_recv() {
            change = next;
        }
        if host::switch_on("launcherClipboard") {
            read(&change);
        }
    }
}

/// Which kind of copy this is, told from the list of formats alone (spec 3.2).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Wanted {
    File,
    Image,
    Rich,
    Text,
}

fn wanted_kind() -> Option<Wanted> {
    let f = win::formats();
    let has_image = win::available(f.png) || win::available(CF_DIBV5) || win::available(CF_DIB);
    let has_rich = win::available(f.html) || win::available(f.rtf);
    let has_text = win::available(CF_UNICODETEXT);
    if win::available(CF_HDROP) {
        Some(Wanted::File)
    } else if has_image && has_text && has_rich {
        // A spreadsheet or document app that adds a picture of the selection: the text wins.
        Some(Wanted::Rich)
    } else if has_image {
        Some(Wanted::Image)
    } else if has_rich && has_text {
        Some(Wanted::Rich)
    } else if has_text {
        Some(Wanted::Text)
    } else {
        None
    }
}

/// The skip rules, then one read of the clipboard, then the store.
fn read(change: &Change) {
    let f = win::formats();
    let started = Instant::now();
    if change.seq == OWN_SEQ.load(Ordering::Relaxed)
        || win::available(f.own)
        || win::available(f.exclude)
    {
        return;
    }
    let Some(mut wanted) = wanted_kind() else {
        return;
    };
    let switch = |key: &str| host::switch_on(key);
    match wanted {
        Wanted::File if !switch("clipboardCaptureFiles") => return,
        Wanted::Image if !switch("clipboardCaptureImages") => return,
        // Rich text with its switch off is the plain text alone.
        Wanted::Rich if !switch("clipboardCaptureRich") => wanted = Wanted::Text,
        _ => {}
    }
    if clip::with(|store| store.status()) != Status::Ready {
        return;
    }
    let source = source_of(change);
    if let Some(app) = &source {
        let id = app.id.to_ascii_lowercase();
        if host::excluded_apps().contains(&id) {
            return;
        }
    }
    // Everything is copied out while the clipboard is open, and nothing is hashed or written
    // until it is closed again.
    let raw = {
        let Some(open) = Open::acquire(&OPEN_WAITS) else {
            return;
        };
        // The retry gap may have let the copy change or mark itself exclude-only.
        if win::sequence() != change.seq
            || win::available(f.own)
            || win::available(f.exclude)
            || win::read_dword(&open, f.no_history) == Some(0)
        {
            return;
        }
        read_formats(&open, wanted, started)
    };
    let Some(raw) = raw else {
        return;
    };
    if started.elapsed() > READ_LIMIT {
        return;
    }
    store(raw, source, change);
}

/// What was copied out of the open clipboard.
struct Raw {
    wanted: Wanted,
    text: Option<String>,
    html: Option<Vec<u8>>,
    rtf: Option<Vec<u8>>,
    png: Option<Vec<u8>>,
    dib: Option<Vec<u8>>,
    paths: Vec<String>,
}

fn read_formats(open: &Open, wanted: Wanted, started: Instant) -> Option<Raw> {
    let f = win::formats();
    let mut raw = Raw {
        wanted,
        text: None,
        html: None,
        rtf: None,
        png: None,
        dib: None,
        paths: Vec::new(),
    };
    let late = || started.elapsed() > READ_LIMIT;
    match wanted {
        Wanted::File => raw.paths = win::read_paths(open),
        Wanted::Image => {
            raw.png = win::read_bytes(open, f.png, clip::IMAGE_MAX);
            if raw.png.is_none() && !late() {
                raw.dib = win::read_bytes(open, CF_DIBV5, clip::IMAGE_MAX)
                    .or_else(|| win::read_bytes(open, CF_DIB, clip::IMAGE_MAX));
            }
        }
        Wanted::Rich => {
            raw.text = win::read_text(open, TEXT_BLOCK_MAX);
            if !late() {
                raw.html = win::read_bytes(open, f.html, clip::RICH_MAX);
            }
            if !late() {
                raw.rtf = win::read_bytes(open, f.rtf, clip::RICH_MAX).map(trim_nul);
            }
            // A picture of a selection beside the text is not kept: the text is the copy.
        }
        Wanted::Text => raw.text = win::read_text(open, TEXT_BLOCK_MAX),
    }
    (!late()).then_some(raw)
}

/// An RTF block has a NUL and slack after its last byte.
fn trim_nul(mut bytes: Vec<u8>) -> Vec<u8> {
    while bytes.last() == Some(&0) {
        bytes.pop();
    }
    bytes
}

/// Applies the skip rules that need the content (the secret filter, the size limits of an
/// image) and sorts what was read into a capture; `None` is a copy that is not kept.
fn capture_from(raw: Raw, source: Option<SourceApp>, change: &Change) -> Option<Capture> {
    let Raw {
        wanted,
        text,
        html,
        rtf,
        png,
        dib,
        paths,
    } = raw;
    let mut capture = Capture {
        kind: Kind::Text,
        text: String::new(),
        html: None,
        rtf: None,
        png: None,
        thumb: None,
        paths: Vec::new(),
        source: source.clone(),
        screenshot: false,
    };
    match wanted {
        Wanted::File => {
            if paths.is_empty() {
                return None;
            }
            capture.kind = Kind::File;
            capture.paths = paths;
        }
        Wanted::Image => {
            let bytes = png.or_else(|| dib.as_deref().and_then(clip_img::dib_to_png))?;
            if bytes.len() > clip::IMAGE_MAX {
                return None;
            }
            let (width, height) = clip_img::png_size(&bytes)?;
            if width > clip::IMAGE_SIDE_MAX || height > clip::IMAGE_SIDE_MAX {
                return None;
            }
            capture.kind = Kind::Image;
            capture.thumb = thumbnail_of(&bytes);
            capture.screenshot = is_screenshot(source.as_ref(), change, width, height);
            capture.png = Some((bytes, width, height));
        }
        Wanted::Rich | Wanted::Text => {
            let text = text?;
            if host::switch_on("clipboardSecretFilter") && clip_detect::is_secret(&text) {
                return None;
            }
            capture.kind = if wanted == Wanted::Rich {
                Kind::Rich
            } else {
                Kind::Text
            };
            capture.text = text;
            capture.html = html;
            capture.rtf = rtf;
        }
    }
    Some(capture)
}

/// What the QA view renderer copies: `kind` is `text`, `rich`, `file` or `image`.
#[cfg(feature = "qa-views")]
pub struct QaCopy {
    pub kind: &'static str,
    pub text: Option<String>,
    pub html: Option<Vec<u8>>,
    pub png: Option<Vec<u8>>,
    pub paths: Vec<String>,
    pub exe: &'static str,
    pub name: &'static str,
}

/// QA view renderer only (`qa-views`, never in a shipped build): one copy through the capture
/// path's own `capture_from` (the secret filter, the size limits, the kind sort) and the store's
/// `add`, as `read` then `store` do for a real clipboard change, but without a clipboard, a
/// window, the burst card or a hub wake. `None` is a copy the capture refused (a secret).
#[cfg(feature = "qa-views")]
pub fn qa_copy(copy: QaCopy, now: f64) -> Option<clip::Added> {
    let wanted = match copy.kind {
        "file" => Wanted::File,
        "image" => Wanted::Image,
        "rich" => Wanted::Rich,
        _ => Wanted::Text,
    };
    let raw = Raw {
        wanted,
        text: copy.text,
        html: copy.html,
        rtf: None,
        png: copy.png,
        dib: None,
        paths: copy.paths,
    };
    let source = SourceApp {
        id: copy.exe.to_string(),
        name: copy.name.to_string(),
    };
    let change = Change {
        seq: 0,
        owner: 0,
        foreground: 0,
    };
    let capture = capture_from(raw, Some(source), &change)?;
    Some(clip::with(|store| store.add(capture, now)))
}

/// Turns what was read into a capture and hands it to the store.
fn store(raw: Raw, source: Option<SourceApp>, change: &Change) {
    let Some(capture) = capture_from(raw, source, change) else {
        return;
    };
    let added = clip::with(|store| {
        let added = store.add(capture, clip::now());
        let id = (added != clip::Added::Skipped)
            .then(|| store.newest_id())
            .flatten();
        (added, id)
    });
    let (added, id) = added;
    if let Some(id) = id {
        host::copied(&id);
    }
    if added != clip::Added::Skipped {
        // The hub's count of items follows.
        host::store_changed();
    }
}

/// The card's thumbnail of a PNG, made from a temporary file.
fn thumbnail_of(png: &[u8]) -> Option<Vec<u8>> {
    let path = clip::tmp_file("png")?;
    let made = rightkit_fs::atomic_write(&path, png)
        .ok()
        .and_then(|()| clip_img::thumbnail(&path));
    let _ = std::fs::remove_file(&path);
    made
}

/// A picture is a screenshot when its source is a screenshot tool or the application's own, or it is
/// as large as a display (within two pixels).
fn is_screenshot(source: Option<&SourceApp>, change: &Change, width: u32, height: u32) -> bool {
    if source.is_some_and(|app| SCREENSHOT_APPS.contains(&app.id.as_str())) {
        return true;
    }
    // The application's own Snip, Screen and Window tools put their picture on the clipboard from this
    // process.
    // SAFETY: plain queries.
    let own = unsafe { GetCurrentProcessId() };
    if process_of(change.owner) == own {
        return true;
    }
    let (primary_w, primary_h) = metrics(0, 1);
    let (virtual_w, virtual_h) = metrics(78, 79);
    let near = |a: u32, b: i32| (i64::from(a) - i64::from(b)).abs() <= 2;
    [(primary_w, primary_h), (virtual_w, virtual_h)]
        .iter()
        .any(|&(w, h)| near(width, w) && near(height, h))
}

#[allow(non_snake_case)]
#[link(name = "user32")]
unsafe extern "system" {
    fn GetSystemMetrics(index: i32) -> i32;
}

fn metrics(x: i32, y: i32) -> (i32, i32) {
    // SAFETY: plain queries (SM_CXSCREEN 0, SM_CYSCREEN 1, SM_CXVIRTUALSCREEN 78,
    // SM_CYVIRTUALSCREEN 79).
    unsafe { (GetSystemMetrics(x), GetSystemMetrics(y)) }
}

// ---- the source app -----------------------------------------------------------------------

fn process_of(window: isize) -> u32 {
    if window == 0 {
        return 0;
    }
    let mut pid = 0u32;
    // SAFETY: a plain query on a handle that may have gone, which only fails.
    let _ = unsafe { GetWindowThreadProcessId(hwnd_from_key(window), Some(&mut pid)) };
    pid
}

/// The first child window of a Store app's frame whose process is not the frame host.
unsafe extern "system" fn first_real_child(hwnd: HWND, data: LPARAM) -> BOOL {
    // SAFETY: EnumChildWindows supplies live handles; `data` is the caller's `(frame, found)`.
    unsafe {
        let context = &mut *(data.0 as *mut (u32, u32));
        let mut pid = 0u32;
        let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid != 0 && pid != context.0 {
            context.1 = pid;
            return false.into();
        }
        true.into()
    }
}

/// The program behind a window: the clipboard's owner first, else the foreground window
/// (spec 3.6). Null when neither can be read.
fn source_of(change: &Change) -> Option<SourceApp> {
    for window in [change.owner, change.foreground] {
        if let Some(app) = app_of(window) {
            return Some(app);
        }
    }
    None
}

fn app_of(window: isize) -> Option<SourceApp> {
    let mut pid = process_of(window);
    if pid == 0 {
        return None;
    }
    let mut path = process::image_path(pid)?;
    let mut id = file_name_of(&path)?;
    if id == "applicationframehost.exe" {
        // A Store app hides behind its frame: ask for the real process of a child window.
        let mut context = (pid, 0u32);
        // SAFETY: the callback only runs during this call; `context` lives past it.
        let _ = unsafe {
            EnumChildWindows(
                Some(hwnd_from_key(window)),
                Some(first_real_child),
                LPARAM(&mut context as *mut (u32, u32) as isize),
            )
        };
        if context.1 != 0 {
            pid = context.1;
            if let Some(real) = process::image_path(pid) {
                path = real;
                id = file_name_of(&path)?;
            }
        }
    }
    let stem = id.strip_suffix(".exe").unwrap_or(&id).to_string();
    let name = process::product_name(&path).unwrap_or(stem);
    remember_path(&id, &path);
    Some(SourceApp { id, name })
}

/// The lower-case file name of a path (`chrome.exe`).
fn file_name_of(path: &str) -> Option<String> {
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
}

/// The exe paths of the apps copied from, by app id, for the cards' icons. Kept in
/// `apppaths.json` beside the history, so the icons come back after a restart (spec 3.6).
static APP_PATHS: Mutex<Option<std::collections::HashMap<String, String>>> = Mutex::new(None);

/// Runs `f` on the map, reading `apppaths.json` the first time.
fn with_paths<R>(f: impl FnOnce(&mut std::collections::HashMap<String, String>) -> R) -> R {
    let mut guard = APP_PATHS.lock().unwrap_or_else(PoisonError::into_inner);
    f(guard.get_or_insert_with(load_paths))
}

fn load_paths() -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let Some(bytes) = clip::root().and_then(|root| crate::seal::read(&root.join("apppaths.json")))
    else {
        return map;
    };
    if let Some(crate::json::Value::Object(members)) = crate::json::parse(&bytes, 1 << 20) {
        for (id, value) in members {
            if let Some(path) = value.as_str() {
                map.insert(id, path.to_string());
            }
        }
    }
    map
}

fn remember_path(id: &str, path: &str) {
    let changed = with_paths(|map| {
        if map.get(id).is_some_and(|known| known.as_str() == path) {
            return false;
        }
        map.insert(id.to_string(), path.to_string());
        true
    });
    if changed {
        save_paths();
    }
}

fn save_paths() {
    let Some(root) = clip::root() else {
        return;
    };
    let map = with_paths(|map| map.clone());
    let mut body = String::from("{");
    for (index, (id, path)) in map.iter().enumerate() {
        if index > 0 {
            body.push(',');
        }
        body.push_str(&format!("{}:{}", json_quoted(id), json_quoted(path)));
    }
    body.push('}');
    let _ = crate::seal::write(&root.join("apppaths.json"), body.as_bytes());
}

/// A JSON string literal; control characters become spaces (as the strip's own writer does).
fn json_quoted(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The exe path of the app with this id, from this run or an earlier one.
pub fn app_path(id: &str) -> Option<String> {
    with_paths(|map| map.get(id).cloned())
}
