//! Choosing a clip: writing it back to the clipboard and pasting it into the app that was in
//! front (the spec's 7.11.2). The strip and the launcher's clip mode both come here.
//!
//! In order: the item's formats go on the clipboard, each in the form the original had (rich
//! text as HTML, RTF and text; an image as PNG and `CF_DIBV5`; files as a file list; "as plain
//! text" as the text alone), marked as Pulse's own so the watcher skips the change and, while
//! `clipboardWriteBackExcluded` is on, kept out of Windows' own Win+V history. Then the
//! window that was in front gets the foreground back, and once it has it, Ctrl+V is sent (or
//! Ctrl+Shift+V, to the terminals that paste with it). Shift, Alt and Ctrl the user may still
//! hold are lifted first and pressed again after, so the app sees Ctrl+V and not
//! Ctrl+Shift+V. Into an elevated window a paste from this process is dropped (UIPI), so the
//! text is only put on the clipboard and a toast says "Copied. Press Ctrl V".
//!
//! It all runs on a thread of its own, after the strip has closed: the paste never waits for
//! the animation. The clipboard keeps the pasted item afterwards, as Paste does. Nothing here
//! logs item content.

use crate::host;
use crate::model::{self as clip, Kind};
use crate::win::{self, Open};
use crate::{capture as clip_capture, img as clip_img};
use rightkit_automation::win::input::{ModifierSnapshot, RawInput, vk};
use rightkit_win_ui::process;
use rightkit_win_ui::raii::hwnd_from_key;
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowThreadProcessId, IsWindow, SetForegroundWindow,
};

type Handle = *mut c_void;

#[allow(non_snake_case)]
#[link(name = "user32")]
unsafe extern "system" {
    fn AttachThreadInput(attach: u32, to: u32, on: i32) -> i32;
}

#[allow(non_snake_case)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
    fn CloseHandle(object: Handle) -> i32;
    fn GetCurrentThreadId() -> u32;
}

#[allow(non_snake_case)]
#[link(name = "advapi32")]
unsafe extern "system" {
    fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
    fn GetTokenInformation(
        token: Handle,
        class: i32,
        info: *mut c_void,
        length: u32,
        returned: *mut u32,
    ) -> i32;
}

const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const TOKEN_QUERY: u32 = 0x0008;
const TOKEN_ELEVATION: i32 = 20;
/// How long the window that was in front has to take the foreground back, and how often that
/// is looked at.
const FRONT_WAIT: Duration = Duration::from_millis(300);
const FRONT_STEP: Duration = Duration::from_millis(10);
/// The clipboard is opened up to this many times, 20 ms apart, for a write-back.
pub const WRITE_WAITS: [u64; 9] = [20; 9];

static BUSY: AtomicBool = AtomicBool::new(false);

/// What the user asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// Paste as it was copied.
    Paste,
    /// Paste the plain text only (an image has none: it pastes as it is).
    Plain,
    /// Put it on the clipboard and nothing more (Alt+Return, Ctrl+C, the "copy" mode).
    CopyOnly,
}

/// Puts the item on the clipboard and, unless it is a copy only, pastes it into `previous`
/// (the window that was in front when the strip opened, as a handle value; 0 when none).
/// `previous` also gets the foreground back for a copy only.
pub fn choose(id: &str, choice: Choice, previous: isize) {
    start(id, choice, previous);
}

/// Starts the paste on its own thread. False when one is already running or no thread could be
/// started.
fn start(id: &str, choice: Choice, previous: isize) -> bool {
    if BUSY.swap(true, Ordering::AcqRel) {
        return false;
    }
    let id = id.to_string();
    let spawned = std::thread::Builder::new()
        .name("pulse-clip-paste".to_string())
        .spawn(move || {
            run(&id, choice, previous);
            BUSY.store(false, Ordering::Release);
        });
    if spawned.is_err() {
        BUSY.store(false, Ordering::Release);
        host::log_info("clipboard", &[("result", "no_paste_thread")]);
        return false;
    }
    true
}

fn run(id: &str, choice: Choice, previous: isize) {
    clip_img::init_com();
    // The clipboard as it was, kept for the restore after the paste (7.11.2 step 6). Only when a
    // paste will follow: never for a copy only, or in the "copy" paste mode.
    let saved = if choice != Choice::CopyOnly
        && !host::paste_copies_only()
        && host::switch_on("clipboardRestoreAfterPaste")
    {
        snapshot()
    } else {
        None
    };
    if !write_back(id, choice == Choice::Plain) {
        // The strip's window was in front for the choice: the previous one gets focus back.
        let front = hwnd_from_key(previous);
        // SAFETY: a plain query on a handle that may have gone.
        if previous != 0 && unsafe { IsWindow(Some(front)) }.as_bool() {
            give_foreground_to(previous);
        }
        return;
    }
    // The change count the write left: the restore only goes back if nothing else changed it.
    let written = win::sequence();
    clip::with(|store| store.touch(id, clip::now()));
    let copy_mode = host::paste_copies_only();
    let front = hwnd_from_key(previous);
    // SAFETY: a plain query on a handle that may have gone.
    let window_ok = previous != 0 && unsafe { IsWindow(Some(front)) }.as_bool();
    if window_ok {
        give_foreground_to(previous);
    }
    if choice == Choice::CopyOnly || copy_mode || !window_ok {
        return;
    }
    // Waits until the app is frontmost, as far as it will be: 300 ms, every 10 ms.
    let started = Instant::now();
    // SAFETY: plain queries.
    while unsafe { GetForegroundWindow() } != front && started.elapsed() < FRONT_WAIT {
        std::thread::sleep(FRONT_STEP);
    }
    let mut pid = 0u32;
    // SAFETY: a plain query on a handle that may have gone.
    let _ = unsafe { GetWindowThreadProcessId(front, Some(&mut pid)) };
    if is_elevated(pid) && !is_elevated(0) {
        host::note("Copied. Press Ctrl V");
        return;
    }
    let exe = process::image_path(pid)
        .and_then(|path| {
            std::path::Path::new(&path)
                .file_name()
                .map(|name| name.to_string_lossy().to_ascii_lowercase())
        })
        .unwrap_or_default();
    let shift = host::shift_paste_apps().contains(&exe);
    press_paste(shift);
    if let Some(saved) = saved {
        schedule_restore(saved, written);
    }
}

/// How long the pasted app has to take the keystroke before the previous clipboard goes back.
const RESTORE_DELAY: Duration = Duration::from_millis(1500);

/// Formats held as GDI handles (a bitmap, a palette, the metafiles and the owner-drawn ones): they
/// cannot be kept as bytes, so a clipboard that has one is not restored.
const GDI_FORMATS: [u32; 8] = [2, 3, 9, 14, 0x0080, 0x0082, 0x0083, 0x008E];

/// The clipboard's formats with their bytes, for the restore after a paste, or `None` when it cannot
/// be kept whole: a password manager's copy (never read), a GDI format, a format that cannot be
/// read, or more than 32 MB in all.
fn snapshot() -> Option<Vec<(u32, Vec<u8>)>> {
    const SNAPSHOT_BYTES: usize = 32 * 1024 * 1024;
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

/// Runs `restore` after the keystroke has had its moment, on a thread of its own so the next paste
/// is not held up.
fn schedule_restore(saved: Vec<(u32, Vec<u8>)>, written: u32) {
    let _ = std::thread::Builder::new()
        .name("pulse-clip-restore".to_string())
        .spawn(move || {
            std::thread::sleep(RESTORE_DELAY);
            restore(&saved, written);
        });
}

/// Puts `saved` back, but only while the clipboard still holds the change `written` (what the paste
/// wrote): anything copied meanwhile is left alone. Written as Pulse's own, as the paste is.
fn restore(saved: &[(u32, Vec<u8>)], written: u32) {
    if win::sequence() != written {
        return;
    }
    let Some(open) = Open::acquire(&WRITE_WAITS) else {
        return;
    };
    if win::sequence() != written || !win::empty(&open) {
        return;
    }
    for (format, bytes) in saved {
        let _ = win::set_bytes(&open, *format, bytes);
    }
    let f = win::formats();
    win::set_dword(&open, f.own, 1);
    if host::switch_on("clipboardWriteBackExcluded") {
        win::set_dword(&open, f.exclude, 1);
        win::set_dword(&open, f.no_history, 0);
        win::set_dword(&open, f.no_cloud, 0);
    }
    drop(open);
    clip_capture::note_own_write();
}

/// Writes the item's formats to the clipboard. False when the item is gone or the clipboard
/// could not be taken.
/// Writes item `id` back to the clipboard (plain text only when `plain`).
pub fn write_back(id: &str, plain: bool) -> bool {
    let Some((kind, original, payload)) = clip::with(|store| {
        let item = store.get(id)?;
        let original = item.meta_str("original").map(str::to_string);
        Some((item.kind, original, store.payload(id)?))
    }) else {
        return false;
    };
    let f = win::formats();
    // Everything is read and encoded before the clipboard is emptied, so that a payload that
    // can't be read never leaves the clipboard with nothing on it.
    let mut image: Option<(Vec<u8>, Option<Vec<u8>>)> = None;
    if kind == Kind::Image {
        let Some(path) = &payload.png else {
            return false;
        };
        let Some(png) = crate::seal::read(path) else {
            host::log_info("clipboard", &[("result", "paste_payload_unreadable")]);
            return false;
        };
        let dib = crate::seal::with_plain_file(&png, "png", |plain| {
            clip_img::decode_file(plain, None).and_then(|bitmap| clip_img::dibv5(&bitmap))
        })
        .flatten();
        image = Some((png, dib));
    } else if kind == Kind::File && payload.paths.is_empty() {
        return false;
    }
    let Some(open) = Open::acquire(&WRITE_WAITS) else {
        host::log_info("clipboard", &[("result", "paste_clipboard_busy")]);
        return false;
    };
    if !win::empty(&open) {
        return false;
    }
    let mut wrote = false;
    // A plain paste is the text alone; an image has no text and goes as it is.
    let as_plain = plain && kind != Kind::Image;
    match kind {
        Kind::Image => {
            if let Some((png, dib)) = &image {
                wrote |= win::set_bytes(&open, f.png, png);
                if let Some(dib) = dib {
                    win::set_bytes(&open, win::CF_DIBV5, dib);
                }
            }
        }
        Kind::File if !as_plain => wrote |= win::set_paths(&open, &payload.paths),
        Kind::File => wrote |= win::set_text(&open, &payload.paths.join("\n")),
        Kind::Rich if !as_plain => {
            if let Some(html) = &payload.html {
                wrote |= win::set_bytes(&open, f.html, html);
            }
            if let Some(rtf) = &payload.rtf {
                wrote |= win::set_bytes(&open, f.rtf, rtf);
            }
            wrote |= win::set_text(&open, &payload.text);
        }
        Kind::Colour if as_plain => {
            // The colour as it was copied, not as it was normalised.
            let text = original.as_deref().unwrap_or(&payload.text);
            wrote |= win::set_text(&open, text);
        }
        _ => wrote |= win::set_text(&open, &payload.text),
    }
    if !wrote {
        return false;
    }
    mark_own(open);
    true
}

/// Marks what was just written as Pulse's own (and, with `clipboardWriteBackExcluded`, keeps it
/// out of Windows' history and cloud), then closes the clipboard and tells the watcher the change
/// is not a copy.
/// Marks the clipboard contents just written as the application's own.
pub fn mark_own(open: Open) {
    let f = win::formats();
    win::set_dword(&open, f.own, 1);
    if host::switch_on("clipboardWriteBackExcluded") {
        win::set_dword(&open, f.exclude, 1);
        win::set_dword(&open, f.no_history, 0);
        win::set_dword(&open, f.no_cloud, 0);
    }
    drop(open);
    clip_capture::note_own_write();
}

/// Gives the foreground back to `window`; sharing its input queue for the call lets a thread
/// that was not opened by a key press do it (the launcher's own way).
fn give_foreground_to(window: isize) {
    let target = hwnd_from_key(window);
    // SAFETY: plain window calls on a handle that may have gone, which only fail; the thread
    // attachment made for the call is undone right after.
    unsafe {
        let front = GetWindowThreadProcessId(target, None);
        let own = GetCurrentThreadId();
        let joined = front != 0 && front != own && AttachThreadInput(own, front, 1) != 0;
        let _ = SetForegroundWindow(target);
        if joined {
            AttachThreadInput(own, front, 0);
        }
    }
}

/// Whether the process runs elevated; `0` asks about this one. An unreadable process counts
/// as elevated for another process (a protected one cannot be pasted into either).
fn is_elevated(pid: u32) -> bool {
    // SAFETY: a limited-query handle, a token handle and a four-byte answer, all closed or
    // read within the calls; the pseudo handle of this process (-1) is never closed.
    unsafe {
        let own = pid == 0;
        let process: Handle = if own {
            -1isize as Handle
        } else {
            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid)
        };
        if process.is_null() {
            return !own;
        }
        let mut token: Handle = std::ptr::null_mut();
        let mut elevated = !own;
        if OpenProcessToken(process, TOKEN_QUERY, &mut token) != 0 {
            let mut flag = 0u32;
            let mut returned = 0u32;
            if GetTokenInformation(
                token,
                TOKEN_ELEVATION,
                (&mut flag as *mut u32).cast(),
                size_of::<u32>() as u32,
                &mut returned,
            ) != 0
            {
                elevated = flag != 0;
            }
            CloseHandle(token);
        }
        if !own {
            CloseHandle(process);
        }
        elevated
    }
}

/// Ctrl+V, or Ctrl+Shift+V with `with_shift`, with the modifiers the user holds lifted first
/// and pressed again after, in one batch.
fn press_paste(with_shift: bool) {
    // The side actually held is the one released and put back (the generic codes mean the
    // left key to Windows, which would leave a held right key down). The Windows key is left
    // alone.
    let held = ModifierSnapshot::from_held(
        ModifierSnapshot::capture()
            .held()
            .iter()
            .copied()
            .filter(|key| !matches!(*key, vk::LWIN | vk::RWIN)),
    );
    let alt = held.contains(vk::LMENU) || held.contains(vk::RMENU);
    let mut events: Vec<RawInput> = Vec::with_capacity(20);
    if alt {
        // A key between Alt's press and release keeps the release from opening a menu.
        events.extend(RawInput::menu_mask());
    }
    events.extend(held.release_inputs());
    events.push(RawInput::key_down(vk::CONTROL));
    if with_shift {
        events.push(RawInput::key_down(vk::SHIFT));
    }
    events.extend(RawInput::key_press(0x56));
    if with_shift {
        events.push(RawInput::key_up(vk::SHIFT));
    }
    events.push(RawInput::key_up(vk::CONTROL));
    host::send_input(&events, "clipboard");
    // Then put back each released key the hand still holds, as the key hook saw it (our own
    // events are not counted there); without the hook, none: a stuck key is worse than one dropped modifier.
    let back = ModifierSnapshot::from_held(
        held.held()
            .iter()
            .copied()
            .filter(|key| host::physically_down(u32::from(*key)).unwrap_or(false)),
    );
    if back.is_empty() {
        return;
    }
    let mut restore = back.restore_inputs();
    if back.contains(vk::LMENU) || back.contains(vk::RMENU) {
        restore.extend(RawInput::menu_mask());
    }
    host::send_input(&restore, "clipboard");
}

/// "Open" in the context menu: a link opens in the browser, a file shows in Explorer. The
/// other kinds have nothing to open.
pub fn open_item(id: &str) {
    let Some((kind, preview, paths)) = clip::with(|store| {
        let item = store.get(id)?;
        let paths = if item.kind == Kind::File {
            store.payload(id)?.paths
        } else {
            Vec::new()
        };
        Some((item.kind, item.preview.clone(), paths))
    }) else {
        return;
    };
    match kind {
        // `open_url` refuses anything that is not a URL with a scheme (and `file:`).
        Kind::Link => {
            let _ = rightkit_child_process::open_url(&preview);
        }
        // The file is selected in Explorer; the scope is its own folder.
        Kind::File => {
            let Some(first) = paths.first() else {
                return;
            };
            let path = std::path::Path::new(first);
            if let Some(parent) = path.parent()
                && let Ok(scope) = rightkit_child_process::reveal::PathScope::new([parent])
            {
                let _ = rightkit_child_process::reveal::reveal(path, &scope);
            }
        }
        _ => {}
    }
}
