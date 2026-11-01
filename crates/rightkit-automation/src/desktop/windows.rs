#![cfg(target_os = "windows")]

use std::{
    ffi::c_void,
    mem::size_of,
    ptr::null_mut,
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

use crate::win::{send_input_batch, RawInput};
use windows_sys::Win32::Foundation::{CloseHandle, GlobalFree, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Globalization::{GetThreadLocale, WideCharToMultiByte};
use windows_sys::Win32::Graphics::Gdi::{
    CopyEnhMetaFileW, DeleteEnhMetaFile, DeleteObject, HBITMAP, HENHMETAFILE,
};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, CountClipboardFormats, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
    GetClipboardOwner, GetClipboardSequenceNumber, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, MapVirtualKeyW, KEYEVENTF_KEYUP, VK_CONTROL, VK_LWIN, VK_MENU, VK_RETURN,
    VK_RWIN, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{CopyImage, IMAGE_BITMAP};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, GetWindowLongPtrW,
    PostMessageW, PostQuitMessage, RegisterClassW, SetWindowLongPtrW, TranslateMessage,
    GWLP_USERDATA, HWND_MESSAGE, MSG, WM_CLOSE, WM_DESTROY, WM_DESTROYCLIPBOARD, WM_NCDESTROY,
    WM_RENDERALLFORMATS, WM_RENDERFORMAT, WNDCLASSW,
};

pub fn write_clipboard_text(text: &str) -> bool {
    write_clipboard_text_guarded(text, None).is_ok()
}

#[derive(Clone, Copy, Debug)]
pub struct ClipboardWriteFailure {
    /// Exact clipboard generation produced by the app's destructive write,
    /// when destruction occurred before the write failed.
    pub restore_sequence: Option<u32>,
}

/// Stage text & return the app's exact clipboard generation. If a failure
/// follows `EmptyClipboard`, carry that same generation so callers can restore
/// without mistaking a newer writer's generation for the app's.
pub fn stage_clipboard_text(
    text: &str,
    expected_sequence: u32,
) -> Result<u32, ClipboardWriteFailure> {
    write_clipboard_text_guarded(text, Some(expected_sequence))
}

// ---------------------------------------------------------------------------
// Promised clipboard (delayed rendering) — the Windows twin of macOS's
// `stage_promised_clipboard_text`. Closes the field failure where a delivery
// restored the user's PRIOR clipboard
// off a fixed `PASTE_SETTLE`/`restore_general_clipboard_after_delay` timer
// before the target's own asynchronous clipboard read had happened, so the
// target pasted (and, in send mode, sent) the stale content.
//
// A fixed timer cannot observe a LATE read. Instead the transcript is published
// as `CF_UNICODETEXT` with a NULL handle (delayed rendering); Windows sends
// `WM_RENDERFORMAT` to the owning window the instant a consumer calls
// `GetClipboardData(CF_UNICODETEXT)`. That callback is the exact receipt that
// the paste landed. Prior contents are restored only after a receipt observed
// AT OR AFTER `mark_posted` (or a bounded timeout), and only while this
// window still owns the clipboard generation it staged.
// ---------------------------------------------------------------------------

/// Shared state between the promised-clipboard owner window (pump thread) and
/// the delivery waiter. Reads before Ctrl+V are third-party/eager (clipboard
/// managers commonly do this) — only reads at or after `posted_at` are proof.
struct PromisedClipboardState {
    posted_at: Option<Instant>,
    receipts: Vec<Instant>,
    ownership_lost: bool,
    /// Clipboard generation after our own render answered a consumer.
    /// Unlike macOS, where a promised provider furnishing data leaves the
    /// change count alone, `SetClipboardData` inside `WM_RENDERFORMAT`
    /// increments the Windows sequence number, so restoration must be bound
    /// to the generation we produced last, not the one we staged.
    rendered_sequence: Option<u32>,
}

impl PromisedClipboardState {
    fn new() -> Self {
        Self {
            posted_at: None,
            receipts: Vec::new(),
            ownership_lost: false,
            rendered_sequence: None,
        }
    }
}

/// Count of render-format callbacks observed at or after `mark_posted`.
fn receipt_locked(state: &PromisedClipboardState) -> usize {
    state
        .receipts
        .iter()
        .filter(|at| state.posted_at.is_some_and(|posted| **at >= posted))
        .count()
}

/// Latest render-format callback at or after `mark_posted`.
fn last_receipt_locked(state: &PromisedClipboardState) -> Option<Instant> {
    state
        .receipts
        .iter()
        .filter(|at| state.posted_at.is_some_and(|posted| **at >= posted))
        .max()
        .copied()
}

/// Receipt evidence for a promised-clipboard delivery.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ClipboardReceipt {
    /// Number of post-`mark_posted` render callbacks (a consumer requested the
    /// promised text).
    pub count: usize,
    /// Another window replaced our clipboard declaration.
    pub ownership_lost: bool,
    /// Most recent post-`mark_posted` render callback, if any.
    pub last_at: Option<Instant>,
}

impl ClipboardReceipt {
    pub fn observed(self) -> bool {
        self.count > 0
    }
}

/// Owned by the pump thread; reached from the window proc through
/// `GWLP_USERDATA`.
struct PromisedWindow {
    state: Arc<(Mutex<PromisedClipboardState>, Condvar)>,
    /// UTF-16 transcript, NUL-terminated, ready to hand to a consumer.
    text: Vec<u16>,
}

unsafe extern "system" fn promised_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let win_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const PromisedWindow;
    match msg {
        WM_RENDERFORMAT | WM_RENDERALLFORMATS => {
            if !win_ptr.is_null() {
                let win = &*win_ptr;
                let wants_text =
                    msg == WM_RENDERALLFORMATS || wparam as u32 == CF_UNICODETEXT as u32;
                if wants_text {
                    // WM_RENDERALLFORMATS is an owner teardown/clipboard-loss
                    // materialization request, not evidence that paste target
                    // consumed text. Only a consumer's WM_RENDERFORMAT can
                    // satisfy delivery receipt.
                    if msg == WM_RENDERFORMAT {
                        if let Ok(mut state) = win.state.0.lock() {
                            state.receipts.push(Instant::now());
                        }
                        win.state.1.notify_all();
                    }
                    render_promised_text(hwnd, win, msg == WM_RENDERALLFORMATS);
                    let rendered = GetClipboardSequenceNumber();
                    if let Ok(mut state) = win.state.0.lock() {
                        state.rendered_sequence = Some(rendered);
                    }
                }
            }
            0
        }
        WM_DESTROYCLIPBOARD => {
            if !win_ptr.is_null() {
                let win = &*win_ptr;
                if let Ok(mut state) = win.state.0.lock() {
                    state.ownership_lost = true;
                }
                win.state.1.notify_all();
            }
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        WM_NCDESTROY => {
            if !win_ptr.is_null() {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(win_ptr as *mut PromisedWindow));
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Hand the promised transcript to whoever is reading the clipboard right now.
/// For `WM_RENDERFORMAT` the clipboard is already open by the consumer; for
/// `WM_RENDERALLFORMATS` (shutdown/foreign empty) we must open it ourselves and
/// only while we still own it.
unsafe fn render_promised_text(hwnd: HWND, win: &PromisedWindow, all_formats: bool) {
    let mut opened = false;
    if all_formats {
        if OpenClipboard(hwnd) == 0 {
            return;
        }
        opened = true;
        if GetClipboardOwner() != hwnd {
            CloseClipboard();
            return;
        }
    }
    let byte_len = win.text.len() * size_of::<u16>();
    let handle = GlobalAlloc(GMEM_MOVEABLE, byte_len);
    if !handle.is_null() {
        let locked = GlobalLock(handle) as *mut u16;
        if !locked.is_null() {
            std::ptr::copy_nonoverlapping(win.text.as_ptr(), locked, win.text.len());
            GlobalUnlock(handle);
            if SetClipboardData(CF_UNICODETEXT as u32, handle).is_null() {
                GlobalFree(handle);
            }
        } else {
            GlobalFree(handle);
        }
    }
    if opened {
        CloseClipboard();
    }
}

unsafe fn promised_pump(
    tx: std::sync::mpsc::Sender<isize>,
    state: Arc<(Mutex<PromisedClipboardState>, Condvar)>,
    text: Vec<u16>,
) {
    let class_name: Vec<u16> = "RightKitDesktopPromisedClipboard\0"
        .encode_utf16()
        .collect();
    let hinstance = GetModuleHandleW(std::ptr::null());
    let wnd_class = WNDCLASSW {
        style: 0,
        lpfnWndProc: Some(promised_wnd_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinstance,
        hIcon: null_mut(),
        hCursor: null_mut(),
        hbrBackground: null_mut(),
        lpszMenuName: std::ptr::null(),
        lpszClassName: class_name.as_ptr(),
    };
    // 0 means "already registered by a prior promise" — reuse it.
    RegisterClassW(&wnd_class);

    let win = Box::into_raw(Box::new(PromisedWindow {
        state: Arc::clone(&state),
        text,
    }));
    let hwnd = CreateWindowExW(
        0,
        class_name.as_ptr(),
        std::ptr::null(),
        0,
        0,
        0,
        0,
        0,
        HWND_MESSAGE,
        null_mut(),
        hinstance,
        std::ptr::null(),
    );
    if hwnd.is_null() {
        drop(Box::from_raw(win));
        let _ = tx.send(0);
        return;
    }
    SetWindowLongPtrW(hwnd, GWLP_USERDATA, win as isize);
    let _ = tx.send(hwnd as isize);

    let mut msg: MSG = std::mem::zeroed();
    while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

/// A live promised-clipboard declaration. Call [`mark_posted`](Self::mark_posted)
/// immediately before posting Ctrl+V, wait on
/// [`wait_for_receipt`](Self::wait_for_receipt), then restore prior contents
/// through [`restore_if_unchanged`](Self::restore_if_unchanged). Dropping it
/// without restoring intentionally leaves the transcript staged.
pub struct PromisedClipboard {
    /// Message-only window handle kept as `isize` (raw `HWND` is not `Send`);
    /// only ever used from this type's own methods and `Drop`.
    hwnd: isize,
    thread: Option<std::thread::JoinHandle<()>>,
    state: Arc<(Mutex<PromisedClipboardState>, Condvar)>,
    /// Clipboard generation produced by our `EmptyClipboard` + delayed-render
    /// declaration; restoration is bound to it.
    sequence: u32,
}

/// Publish `text` as a delayed-render `CF_UNICODETEXT` declaration.
///
/// `expected_sequence` is checked before the destructive `EmptyClipboard`,
/// matching [`stage_clipboard_text`]. `Err(None)` — another process changed the
/// clipboard first (nothing destroyed). `Err(Some(seq))` — our declaration was
/// lost after destruction; restore against `seq`.
pub fn stage_promised_clipboard_text(
    text: &str,
    expected_sequence: u32,
) -> Result<PromisedClipboard, Option<u32>> {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);

    let state = Arc::new((Mutex::new(PromisedClipboardState::new()), Condvar::new()));
    let (tx, rx) = std::sync::mpsc::channel::<isize>();
    let pump_state = Arc::clone(&state);
    let pump_text = wide.clone();
    let pump = std::thread::Builder::new()
        .name("delivery-promised-clipboard".to_string())
        .spawn(move || unsafe { promised_pump(tx, pump_state, pump_text) })
        .map_err(|_| None::<u32>)?;

    let hwnd_raw = match rx.recv() {
        Ok(value) if value != 0 => value,
        _ => {
            let _ = pump.join();
            return Err(None);
        }
    };
    let hwnd = hwnd_raw as HWND;

    let staged = unsafe {
        if OpenClipboard(hwnd) == 0 {
            None
        } else if GetClipboardSequenceNumber() != expected_sequence || EmptyClipboard() == 0 {
            CloseClipboard();
            None
        } else {
            // NULL handle == delayed rendering: we serve CF_UNICODETEXT from
            // `promised_wnd_proc` on demand.
            let _ = SetClipboardData(CF_UNICODETEXT as u32, null_mut());
            let sequence = GetClipboardSequenceNumber();
            CloseClipboard();
            let owns_now = GetClipboardOwner() == hwnd;
            Some((sequence, owns_now))
        }
    };

    match staged {
        Some((sequence, true)) => Ok(PromisedClipboard {
            hwnd: hwnd_raw,
            thread: Some(pump),
            state,
            sequence,
        }),
        Some((sequence, false)) => {
            close_promised_window(hwnd_raw, Some(pump));
            Err(Some(sequence))
        }
        None => {
            close_promised_window(hwnd_raw, Some(pump));
            Err(None)
        }
    }
}

fn close_promised_window(hwnd: isize, thread: Option<std::thread::JoinHandle<()>>) {
    if hwnd != 0 {
        unsafe { PostMessageW(hwnd as HWND, WM_CLOSE, 0, 0) };
    }
    if let Some(thread) = thread {
        let _ = thread.join();
    }
}

impl PromisedClipboard {
    /// Exact Ctrl+V injection time. Receipts before this are ignored as eager
    /// third-party reads.
    pub fn mark_posted(&self) {
        if let Ok(mut state) = self.state.0.lock() {
            state.posted_at = Some(Instant::now());
        }
        self.state.1.notify_all();
    }

    /// Receipt evidence without waiting.
    pub fn receipt(&self) -> ClipboardReceipt {
        match self.state.0.lock() {
            Ok(state) => ClipboardReceipt {
                count: receipt_locked(&state),
                ownership_lost: state.ownership_lost,
                last_at: last_receipt_locked(&state),
            },
            Err(_) => ClipboardReceipt {
                count: 0,
                ownership_lost: true,
                last_at: None,
            },
        }
    }

    /// Block until a post-`mark_posted` receipt, ownership loss, or timeout.
    /// Uses a condition variable so the wait never polls the pump thread.
    pub fn wait_for_receipt(&self, timeout: Duration) -> ClipboardReceipt {
        let (lock, wake) = &*self.state;
        let mut guard = match lock.lock() {
            Ok(guard) => guard,
            Err(_) => {
                return ClipboardReceipt {
                    count: 0,
                    ownership_lost: true,
                    last_at: None,
                }
            }
        };
        let deadline = Instant::now() + timeout;
        loop {
            if receipt_locked(&guard) > 0 || guard.ownership_lost {
                break;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match wake.wait_timeout(guard, remaining) {
                Ok((next, _)) => guard = next,
                Err(_) => {
                    return ClipboardReceipt {
                        count: 0,
                        ownership_lost: true,
                        last_at: None,
                    }
                }
            }
        }
        ClipboardReceipt {
            count: receipt_locked(&guard),
            ownership_lost: guard.ownership_lost,
            last_at: last_receipt_locked(&guard),
        }
    }

    pub fn sequence(&self) -> u32 {
        self.sequence
    }

    /// Restore `snapshot` only while this declaration still owns the exact
    /// generation it staged. A newer writer, or lost ownership, always wins.
    pub fn restore_if_unchanged(&mut self, snapshot: ClipboardSnapshot) -> bool {
        let (ownership_lost, rendered_sequence) = self
            .state
            .0
            .lock()
            .map(|state| (state.ownership_lost, state.rendered_sequence))
            .unwrap_or((true, None));
        if ownership_lost {
            return false;
        }
        // Our own render is the only writer allowed to move the generation;
        // the owner check below still proves nobody else has taken over.
        let sequence = rendered_sequence.unwrap_or(self.sequence);
        let still_ours = unsafe {
            if OpenClipboard(self.hwnd as HWND) == 0 {
                false
            } else {
                let owner = GetClipboardOwner();
                let ours = GetClipboardSequenceNumber() == sequence && owner == self.hwnd as HWND;
                CloseClipboard();
                ours
            }
        };
        if !still_ours {
            return false;
        }
        restore_general_clipboard_if_unchanged(snapshot, sequence)
    }
}

impl Drop for PromisedClipboard {
    fn drop(&mut self) {
        close_promised_window(self.hwnd, self.thread.take());
    }
}

/// Full Windows clipboard snapshot: every global-memory format copied out
/// byte for byte, so text, HTML, RTF, DIB images, file lists and registered
/// formats survive the app's temporary transcript paste. Empty is
/// represented explicitly.
///
/// Why bytes and not an `IDataObject`: `OleGetClipboard` hands back a proxy
/// for the clipboard itself, and `OleSetClipboard` refuses that same proxy
/// with `CLIPBRD_E_CANT_SET`, so a data-object snapshot can never be put
/// back. Copying the rendered bytes has no such rule.
///
/// Formats the copy cannot represent (owner-display, private ranges, over-cap
/// sizes) are counted in `dropped_formats` rather than skipped silently: a
/// lossy snapshot must never back the destructive transcript transaction
/// (delivery spec section 4.5 — show recovery instead). GDI handles are
/// captured as owned copies (`CF_BITMAP`, `CF_ENHMETAFILE`), and a format
/// Windows re-creates from a captured source is not counted as lost.
pub struct ClipboardSnapshot {
    formats: Vec<(u32, Vec<u8>)>,
    bitmaps: Vec<OwnedBitmap>,
    metafiles: Vec<OwnedEnhMetaFile>,
    empty: bool,
    sequence: u32,
    dropped_formats: Vec<DroppedClipboardFormat>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipboardDropReason {
    UnsupportedFormat,
    MissingData,
    Oversized,
    GlobalLockFailed,
    BitmapCopyFailed,
    MetafileCopyFailed,
}

impl ClipboardDropReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedFormat => "unsupported_format",
            Self::MissingData => "missing_data",
            Self::Oversized => "oversized",
            Self::GlobalLockFailed => "global_lock_failed",
            Self::BitmapCopyFailed => "bitmap_copy_failed",
            Self::MetafileCopyFailed => "metafile_copy_failed",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DroppedClipboardFormat {
    pub format: u32,
    pub reason: ClipboardDropReason,
}

struct OwnedBitmap(HBITMAP);

// GDI handles are process-wide kernel objects. Snapshot ownership moves
// between delivery threads, but no handle is concurrently accessed or
// mutated; the wrapper keeps deletion/transfer explicit at each boundary.
unsafe impl Send for OwnedBitmap {}

impl OwnedBitmap {
    fn into_raw(mut self) -> HBITMAP {
        let raw = self.0;
        self.0 = null_mut();
        raw
    }
}

impl Drop for OwnedBitmap {
    fn drop(&mut self) {
        unsafe {
            if !self.0.is_null() {
                DeleteObject(self.0);
            }
        }
    }
}

struct OwnedEnhMetaFile(HENHMETAFILE);

// Same ownership rule as `OwnedBitmap`: the copy is private to the snapshot
// and only touched by whichever delivery thread currently holds it.
unsafe impl Send for OwnedEnhMetaFile {}

impl OwnedEnhMetaFile {
    fn into_raw(mut self) -> HENHMETAFILE {
        let raw = self.0;
        self.0 = null_mut();
        raw
    }
}

impl Drop for OwnedEnhMetaFile {
    fn drop(&mut self) {
        unsafe {
            if !self.0.is_null() {
                DeleteEnhMetaFile(self.0);
            }
        }
    }
}

impl ClipboardSnapshot {
    pub fn sequence(&self) -> u32 {
        self.sequence
    }

    /// True when every format present on the clipboard was captured. A lossy
    /// snapshot supports read-only use only.
    pub fn is_lossless(&self) -> bool {
        self.dropped_formats.is_empty()
    }

    /// Number of formats captured. Zero for an empty clipboard or one that
    /// held only non-copyable handles.
    pub fn format_count(&self) -> usize {
        self.formats.len() + self.bitmaps.len() + self.metafiles.len()
    }

    /// Number of formats present but not representable in this snapshot.
    pub fn dropped_formats(&self) -> usize {
        self.dropped_formats.len()
    }

    pub fn dropped_format_diagnostics(&self) -> &[DroppedClipboardFormat] {
        &self.dropped_formats
    }
}

/// One format's cap; anything larger (a huge DIB) is dropped from the
/// snapshot rather than held in memory for the whole delivery.
const SNAPSHOT_FORMAT_CAP_BYTES: usize = 64 * 1024 * 1024;

const CF_BITMAP: u32 = 2;
const CF_METAFILEPICT: u32 = 3;
const CF_DIB: u32 = 8;
const CF_PALETTE: u32 = 9;
const CF_ENHMETAFILE: u32 = 14;
const CF_DIBV5: u32 = 17;

/// Formats whose clipboard handle is not `HGLOBAL` memory and cannot be
/// copied with `GlobalLock`, or which only exist in relation to a live owner:
/// GDI handles, metafiles, palettes, owner-display, private, and GDI-object
/// ranges. `CF_BITMAP` and `CF_ENHMETAFILE` have their own owned-copy capture
/// path; the other GDI forms are covered by `synthesized_from`.
fn format_is_copyable(format: u32) -> bool {
    const CF_OWNERDISPLAY: u32 = 0x0080;
    const CF_DSPBITMAP: u32 = 0x0082;
    const CF_DSPMETAFILEPICT: u32 = 0x0083;
    const CF_DSPENHMETAFILE: u32 = 0x008E;
    !matches!(
        format,
        CF_BITMAP
            | CF_METAFILEPICT
            | CF_PALETTE
            | CF_ENHMETAFILE
            | CF_OWNERDISPLAY
            | CF_DSPBITMAP
            | CF_DSPMETAFILEPICT
            | CF_DSPENHMETAFILE
    ) && !(0x0200..=0x03FF).contains(&format)
}

/// True when Windows re-creates `format` on demand from a source format the
/// snapshot did capture, so leaving it out of the copy loses nothing: the
/// metafile-picture form from an enhanced metafile, and the bitmap, palette
/// and DIBv5 forms from a DIB. Anything else absent from the copy is lost.
fn synthesized_from(format: u32, captured: &[u32]) -> bool {
    let has = |source: u32| captured.contains(&source);
    match format {
        CF_METAFILEPICT => has(CF_ENHMETAFILE),
        CF_BITMAP | CF_PALETTE | CF_DIBV5 => has(CF_DIB),
        _ => false,
    }
}

/// Keep only the drops that really lose data once the full capture is known.
fn unsynthesized_drops(
    dropped: Vec<DroppedClipboardFormat>,
    captured: &[u32],
) -> Vec<DroppedClipboardFormat> {
    dropped
        .into_iter()
        .filter(|drop| !synthesized_from(drop.format, captured))
        .collect()
}

pub fn snapshot_general_clipboard() -> Option<ClipboardSnapshot> {
    unsafe {
        if OpenClipboard(null_mut::<c_void>() as HWND) == 0 {
            return None;
        }
        let sequence = GetClipboardSequenceNumber();
        if CountClipboardFormats() == 0 {
            CloseClipboard();
            return Some(ClipboardSnapshot {
                formats: Vec::new(),
                bitmaps: Vec::new(),
                metafiles: Vec::new(),
                empty: true,
                sequence,
                dropped_formats: Vec::new(),
            });
        }
        let mut formats = Vec::new();
        let mut bitmaps = Vec::new();
        let mut metafiles = Vec::new();
        let mut captured = Vec::new();
        let mut dropped_formats = Vec::new();
        let mut format = EnumClipboardFormats(0);
        while format != 0 {
            if format == CF_BITMAP {
                let handle = GetClipboardData(format);
                let bitmap = if !handle.is_null() {
                    // CF_BITMAP needs a device-dependent bitmap. A DIB-section
                    // copy is readable locally but cannot be shared this way.
                    // No COPYRETURNORG/COPYDELETEORG: own an independent copy.
                    CopyImage(handle, IMAGE_BITMAP, 0, 0, 0) as HBITMAP
                } else {
                    null_mut()
                };
                if bitmap.is_null() {
                    dropped_formats.push(DroppedClipboardFormat {
                        format,
                        reason: if handle.is_null() {
                            ClipboardDropReason::MissingData
                        } else {
                            ClipboardDropReason::BitmapCopyFailed
                        },
                    });
                } else {
                    bitmaps.push(OwnedBitmap(bitmap));
                    captured.push(format);
                }
            } else if format == CF_ENHMETAFILE {
                // Office copies carry a metafile handle, not HGLOBAL bytes:
                // own an independent copy, as with the bitmap above.
                let handle = GetClipboardData(format);
                let copy = if !handle.is_null() {
                    CopyEnhMetaFileW(handle as HENHMETAFILE, std::ptr::null())
                } else {
                    null_mut()
                };
                if copy.is_null() {
                    dropped_formats.push(DroppedClipboardFormat {
                        format,
                        reason: if handle.is_null() {
                            ClipboardDropReason::MissingData
                        } else {
                            ClipboardDropReason::MetafileCopyFailed
                        },
                    });
                } else {
                    metafiles.push(OwnedEnhMetaFile(copy));
                    captured.push(format);
                }
            } else if format_is_copyable(format) {
                // GetClipboardData renders delayed formats from their owner
                // right now, which is exactly what a copy needs.
                let handle = GetClipboardData(format);
                if !handle.is_null() {
                    let size = GlobalSize(handle);
                    if size > 0 && size <= SNAPSHOT_FORMAT_CAP_BYTES {
                        let locked = GlobalLock(handle) as *const u8;
                        if !locked.is_null() {
                            formats
                                .push((format, std::slice::from_raw_parts(locked, size).to_vec()));
                            captured.push(format);
                            GlobalUnlock(handle);
                        } else {
                            dropped_formats.push(DroppedClipboardFormat {
                                format,
                                reason: ClipboardDropReason::GlobalLockFailed,
                            });
                        }
                    } else {
                        dropped_formats.push(DroppedClipboardFormat {
                            format,
                            reason: ClipboardDropReason::Oversized,
                        });
                    }
                } else {
                    dropped_formats.push(DroppedClipboardFormat {
                        format,
                        reason: ClipboardDropReason::MissingData,
                    });
                }
            } else {
                dropped_formats.push(DroppedClipboardFormat {
                    format,
                    reason: ClipboardDropReason::UnsupportedFormat,
                });
            }
            format = EnumClipboardFormats(format);
        }
        // Rendering above may have changed nothing, but if another process
        // replaced the clipboard mid-walk the copy is a mix of generations.
        let unchanged = GetClipboardSequenceNumber() == sequence;
        CloseClipboard();
        if !unchanged {
            return None;
        }
        Some(ClipboardSnapshot {
            formats,
            bitmaps,
            metafiles,
            empty: false,
            sequence,
            dropped_formats: unsynthesized_drops(dropped_formats, &captured),
        })
    }
}

/// Restore every prior clipboard format only if the app's temporary
/// transcript is still current. Newer clipboard writes always win.
pub fn restore_general_clipboard_if_unchanged(
    mut snapshot: ClipboardSnapshot,
    expected_sequence: u32,
) -> bool {
    unsafe {
        if OpenClipboard(null_mut::<c_void>() as HWND) == 0 {
            return false;
        }
        if GetClipboardSequenceNumber() != expected_sequence {
            CloseClipboard();
            return false;
        }
        if EmptyClipboard() == 0 {
            CloseClipboard();
            return false;
        }
        // A snapshot that dropped formats cannot honestly claim a full
        // restore; surface the failure even though the captured subset is
        // still put back.
        let mut restored =
            snapshot.empty || (snapshot.is_lossless() && snapshot.format_count() > 0);
        for (format, bytes) in &snapshot.formats {
            let handle = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
            if handle.is_null() {
                restored = false;
                continue;
            }
            let locked = GlobalLock(handle) as *mut u8;
            if locked.is_null() {
                GlobalFree(handle);
                restored = false;
                continue;
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), locked, bytes.len());
            GlobalUnlock(handle);
            if SetClipboardData(*format, handle).is_null() {
                GlobalFree(handle);
                restored = false;
            }
        }
        for bitmap in snapshot.bitmaps.drain(..) {
            let bitmap = bitmap.into_raw();
            if SetClipboardData(2, bitmap as _).is_null() {
                DeleteObject(bitmap);
                restored = false;
            }
        }
        for metafile in snapshot.metafiles.drain(..) {
            let metafile = metafile.into_raw();
            if SetClipboardData(CF_ENHMETAFILE, metafile as _).is_null() {
                DeleteEnhMetaFile(metafile);
                restored = false;
            }
        }
        CloseClipboard();
        restored
    }
}

fn write_clipboard_text_guarded(
    text: &str,
    expected_sequence: Option<u32>,
) -> Result<u32, ClipboardWriteFailure> {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);
    let byte_len = wide.len() * size_of::<u16>();
    let ansi = clipboard_multibyte_bytes(&wide, 0)?;
    let oem = clipboard_multibyte_bytes(&wide, 1)?;
    let locale = unsafe { GetThreadLocale() }.to_ne_bytes().to_vec();

    unsafe {
        if OpenClipboard(std::ptr::null_mut::<std::ffi::c_void>() as HWND) == 0 {
            return Err(ClipboardWriteFailure {
                restore_sequence: None,
            });
        }
        if expected_sequence.is_some_and(|expected| GetClipboardSequenceNumber() != expected) {
            CloseClipboard();
            return Err(ClipboardWriteFailure {
                restore_sequence: None,
            });
        }

        let handle = GlobalAlloc(GMEM_MOVEABLE, byte_len);
        let ansi_handle = GlobalAlloc(GMEM_MOVEABLE, ansi.len());
        let oem_handle = GlobalAlloc(GMEM_MOVEABLE, oem.len());
        let locale_handle = GlobalAlloc(GMEM_MOVEABLE, locale.len());
        if handle.is_null()
            || ansi_handle.is_null()
            || oem_handle.is_null()
            || locale_handle.is_null()
        {
            if !handle.is_null() {
                GlobalFree(handle);
            }
            if !ansi_handle.is_null() {
                GlobalFree(ansi_handle);
            }
            if !oem_handle.is_null() {
                GlobalFree(oem_handle);
            }
            if !locale_handle.is_null() {
                GlobalFree(locale_handle);
            }
            CloseClipboard();
            return Err(ClipboardWriteFailure {
                restore_sequence: None,
            });
        }

        let locked = GlobalLock(handle) as *mut u16;
        let ansi_locked = GlobalLock(ansi_handle) as *mut u8;
        let oem_locked = GlobalLock(oem_handle) as *mut u8;
        let locale_locked = GlobalLock(locale_handle) as *mut u8;
        if locked.is_null()
            || ansi_locked.is_null()
            || oem_locked.is_null()
            || locale_locked.is_null()
        {
            if !locked.is_null() {
                GlobalUnlock(handle);
            }
            if !ansi_locked.is_null() {
                GlobalUnlock(ansi_handle);
            }
            if !oem_locked.is_null() {
                GlobalUnlock(oem_handle);
            }
            if !locale_locked.is_null() {
                GlobalUnlock(locale_handle);
            }
            GlobalFree(handle);
            GlobalFree(ansi_handle);
            GlobalFree(oem_handle);
            GlobalFree(locale_handle);
            CloseClipboard();
            return Err(ClipboardWriteFailure {
                restore_sequence: None,
            });
        }

        std::ptr::copy_nonoverlapping(wide.as_ptr(), locked, wide.len());
        GlobalUnlock(handle);
        std::ptr::copy_nonoverlapping(ansi.as_ptr(), ansi_locked, ansi.len());
        GlobalUnlock(ansi_handle);
        std::ptr::copy_nonoverlapping(oem.as_ptr(), oem_locked, oem.len());
        GlobalUnlock(oem_handle);
        std::ptr::copy_nonoverlapping(locale.as_ptr(), locale_locked, locale.len());
        GlobalUnlock(locale_handle);
        if EmptyClipboard() == 0 {
            // The caller still owns an uncommitted movable block until
            // SetClipboardData succeeds.
            GlobalFree(handle);
            GlobalFree(ansi_handle);
            GlobalFree(oem_handle);
            GlobalFree(locale_handle);
            CloseClipboard();
            return Err(ClipboardWriteFailure {
                restore_sequence: None,
            });
        }

        // Publish all common text formats while clipboard is open. If these
        // were omitted, CloseClipboard lets Windows synthesize them and bump
        // sequence after staging; every handle transfers ownership separately.
        let formats = [CF_UNICODETEXT as u32, 1, 7, 16];
        let mut handles = [handle, ansi_handle, oem_handle, locale_handle];
        for (index, format) in formats.into_iter().enumerate() {
            if SetClipboardData(format, handles[index]).is_null() {
                let restore_sequence = GetClipboardSequenceNumber();
                for pending in handles.iter().skip(index) {
                    if !pending.is_null() {
                        GlobalFree(*pending);
                    }
                }
                CloseClipboard();
                return Err(ClipboardWriteFailure {
                    restore_sequence: Some(restore_sequence),
                });
            }
            handles[index] = std::ptr::null_mut();
        }
        let transcript_sequence = GetClipboardSequenceNumber();
        CloseClipboard();
        Ok(transcript_sequence)
    }
}

fn clipboard_multibyte_bytes(
    wide: &[u16],
    codepage: u32,
) -> Result<Vec<u8>, ClipboardWriteFailure> {
    let required = unsafe {
        WideCharToMultiByte(
            codepage,
            0,
            wide.as_ptr(),
            -1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
            std::ptr::null_mut(),
        )
    };
    if required <= 0 {
        return Err(ClipboardWriteFailure {
            restore_sequence: None,
        });
    }
    let mut bytes = vec![0u8; required as usize];
    let written = unsafe {
        WideCharToMultiByte(
            codepage,
            0,
            wide.as_ptr(),
            -1,
            bytes.as_mut_ptr(),
            required,
            std::ptr::null(),
            std::ptr::null_mut(),
        )
    };
    if written == required {
        Ok(bytes)
    } else {
        Err(ClipboardWriteFailure {
            restore_sequence: None,
        })
    }
}

/// Read current Unicode text when present. Delivery preservation uses
/// `snapshot_general_clipboard`; this helper is only for reading copied text.
pub fn read_clipboard_text() -> Option<String> {
    unsafe {
        if OpenClipboard(std::ptr::null_mut::<std::ffi::c_void>() as HWND) == 0 {
            return None;
        }

        let handle = GetClipboardData(CF_UNICODETEXT as u32);
        if handle.is_null() {
            CloseClipboard();
            return None;
        }

        let locked = GlobalLock(handle) as *const u16;
        if locked.is_null() {
            CloseClipboard();
            return None;
        }

        // CF_UNICODETEXT is a NUL-terminated UTF-16 buffer; find the NUL.
        let mut len = 0usize;
        while *locked.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(locked, len);
        let text = String::from_utf16_lossy(slice);

        GlobalUnlock(handle);
        CloseClipboard();

        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }
}

/// Clipboard generation used to avoid overwriting content copied by another
/// app while the app waits for Ctrl+V to settle.
pub fn clipboard_sequence_number() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

fn key(vk: u16, flags: u32) -> RawInput {
    if flags & KEYEVENTF_KEYUP != 0 {
        RawInput::key_up(vk)
    } else {
        RawInput::key_down(vk)
    }
}

fn send_inputs(inputs: &[RawInput]) -> bool {
    send_input_batch(inputs).is_ok()
}

pub fn send_paste_keystroke() -> bool {
    const V_KEY: u16 = 0x56;
    send_inputs(&[
        key(VK_CONTROL, 0),
        key(V_KEY, 0),
        key(V_KEY, KEYEVENTF_KEYUP),
        key(VK_CONTROL, KEYEVENTF_KEYUP),
    ])
}

/// Ctrl+C — used by the summarize-on-selection fallback to copy a selection
/// that UIA cannot read (read-only content regions). Caller owns clipboard
/// save/restore around it.
pub fn send_copy_keystroke() -> bool {
    const C_KEY: u16 = 0x43;
    send_inputs(&[
        key(VK_CONTROL, 0),
        key(C_KEY, 0),
        key(C_KEY, KEYEVENTF_KEYUP),
        key(VK_CONTROL, KEYEVENTF_KEYUP),
    ])
}

/// Longest the paste/Enter chord waits for the physical modifiers of the stop
/// hotkey (Ctrl+Shift+Space etc.) to come up before forcing a release.
pub const PASTE_CHORD_MODIFIER_SETTLE_MS: u32 = 400;
const PASTE_CHORD_MODIFIER_POLL_MS: u32 = 5;
const VK_DUMMY_MASK: u16 = 0xFF;
const MAPVK_VK_TO_VSC_MODE: u32 = 0;

/// Physical modifier state, as reported by `GetAsyncKeyState`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeldModifiers {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub win: bool,
}

impl HeldModifiers {
    pub fn any(self) -> bool {
        self.ctrl || self.shift || self.alt || self.win
    }
}

/// Result of one paste/Enter chord injection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PasteChordOutcome {
    pub sent: bool,
    /// Wall time spent waiting for the physical modifiers to come up.
    pub waited_ms: u32,
    /// True when modifiers were still held at the cap and were released/re-pressed.
    pub forced_release: bool,
}

fn read_physical_modifiers() -> HeldModifiers {
    unsafe {
        HeldModifiers {
            ctrl: GetAsyncKeyState(VK_CONTROL as i32) < 0,
            shift: GetAsyncKeyState(VK_SHIFT as i32) < 0,
            alt: GetAsyncKeyState(VK_MENU as i32) < 0,
            win: GetAsyncKeyState(VK_LWIN as i32) < 0 || GetAsyncKeyState(VK_RWIN as i32) < 0,
        }
    }
}

/// Polls `held` every `poll_ms` until no modifier is held or `cap_ms` elapsed.
/// Returns (waited_ms, last observed state). Pure: all effects come in via closures.
fn settle_physical_modifiers(
    held: &mut dyn FnMut() -> HeldModifiers,
    elapsed_ms: &mut dyn FnMut() -> u32,
    sleep_ms: &mut dyn FnMut(u32),
    cap_ms: u32,
    poll_ms: u32,
) -> (u32, HeldModifiers) {
    loop {
        let state = held();
        let elapsed = elapsed_ms();
        if !state.any() || elapsed >= cap_ms {
            return (elapsed, state);
        }
        sleep_ms(poll_ms.min(cap_ms - elapsed).max(1));
    }
}

fn settle_physical_modifiers_live() -> (u32, HeldModifiers) {
    let start = Instant::now();
    settle_physical_modifiers(
        &mut read_physical_modifiers,
        &mut || start.elapsed().as_millis().min(u32::MAX as u128) as u32,
        &mut |ms| std::thread::sleep(Duration::from_millis(ms as u64)),
        PASTE_CHORD_MODIFIER_SETTLE_MS,
        PASTE_CHORD_MODIFIER_POLL_MS,
    )
}

fn scan_for_vk(vk: u16) -> u16 {
    unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC_MODE) as u16 }
}

/// Key event by hardware scan code (what a real keyboard produces); falls
/// back to the virtual key when the layout has no scan code for it.
fn key_scan(vk: u16, scan: u16, up: bool) -> RawInput {
    if scan == 0 {
        if up {
            RawInput::key_up(vk)
        } else {
            RawInput::key_down(vk)
        }
    } else {
        RawInput::scan(scan, up, false)
    }
}

/// Pure chord builder. Ctrl-down ... V-down are one atomic batch with the
/// release of exactly the still-held Alt/Shift/Win in between; `repress`
/// restores the user's physically held modifiers after the chord.
fn build_paste_chord(held: HeldModifiers, v_scan: u16, ctrl_scan: u16, v_vk: u16) -> Vec<RawInput> {
    let mut inputs = Vec::with_capacity(12);
    inputs.push(key_scan(VK_CONTROL, ctrl_scan, false));
    if held.alt || held.win {
        // Mask so releasing Alt/Win does not activate the menu bar / Start.
        inputs.extend_from_slice(&RawInput::key_press(VK_DUMMY_MASK));
    }
    if held.alt {
        inputs.push(RawInput::key_up(VK_MENU));
    }
    if held.shift {
        inputs.push(RawInput::key_up(VK_SHIFT));
    }
    if held.win {
        inputs.push(RawInput::key_up(VK_LWIN));
        inputs.push(RawInput::key_up(VK_RWIN));
    }
    inputs.push(key_scan(v_vk, v_scan, false));
    inputs.push(key_scan(v_vk, v_scan, true));
    inputs.push(key_scan(VK_CONTROL, ctrl_scan, true));
    inputs.extend(repress_inputs(held));
    inputs
}

/// Key-downs that restore modifiers the chord released while the user still holds them.
/// Ctrl is included because the chord's own Ctrl-up clears it.
fn repress_inputs(held: HeldModifiers) -> Vec<RawInput> {
    let mut out = Vec::new();
    if held.ctrl {
        out.push(RawInput::key_down(VK_CONTROL));
    }
    if held.shift {
        out.push(RawInput::key_down(VK_SHIFT));
    }
    if held.alt {
        out.push(RawInput::key_down(VK_MENU));
    }
    if held.win {
        out.push(RawInput::key_down(VK_LWIN));
    }
    out
}

/// Waits for the stop-hotkey modifiers to come up (bounded), then sends Ctrl+V.
pub fn send_paste_chord_after_hotkey() -> PasteChordOutcome {
    const V_KEY: u16 = 0x56;
    let (waited_ms, _) = settle_physical_modifiers_live();
    // Re-read immediately before injecting so the release set is as fresh as possible.
    let held = read_physical_modifiers();
    let inputs = build_paste_chord(held, scan_for_vk(V_KEY), scan_for_vk(VK_CONTROL), V_KEY);
    PasteChordOutcome {
        sent: send_inputs(&inputs),
        waited_ms,
        forced_release: held.any(),
    }
}

pub fn send_paste_keystroke_after_hotkey() -> bool {
    send_paste_chord_after_hotkey().sent
}

fn build_enter_release(held: HeldModifiers) -> Vec<RawInput> {
    let mut release = Vec::with_capacity(6);
    if held.alt || held.win {
        release.push(RawInput::key_down(VK_CONTROL));
        release.extend_from_slice(&RawInput::key_press(VK_DUMMY_MASK));
    }
    if held.alt {
        release.push(RawInput::key_up(VK_MENU));
    }
    if held.shift {
        release.push(RawInput::key_up(VK_SHIFT));
    }
    if held.win {
        release.push(RawInput::key_up(VK_LWIN));
        release.push(RawInput::key_up(VK_RWIN));
    }
    if held.ctrl || held.alt || held.win {
        release.push(RawInput::key_up(VK_CONTROL));
    }
    release
}

/// Waits for the stop-hotkey modifiers to come up (bounded), then sends Enter.
pub fn send_enter_chord_after_hotkey() -> PasteChordOutcome {
    let (waited_ms, _) = settle_physical_modifiers_live();
    let held = read_physical_modifiers();
    let release = build_enter_release(held);
    if !release.is_empty() {
        let _ = send_inputs(&release);
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut inputs = vec![RawInput::key_down(VK_RETURN), RawInput::key_up(VK_RETURN)];
    inputs.extend(repress_inputs(held));
    PasteChordOutcome {
        sent: send_inputs(&inputs),
        waited_ms,
        forced_release: held.any(),
    }
}

pub fn send_enter_keystroke() -> bool {
    send_enter_chord_after_hotkey().sent
}

/// True while the user physically holds Ctrl, Shift, Alt or Win. Streaming
/// insertion defers typing until they are released (a push-to-talk hotkey
/// held during recording would otherwise turn typed text into shortcuts).
pub fn physical_modifiers_held() -> bool {
    read_physical_modifiers().any()
}

/// Streaming insertion: type `text` into the focused control as Unicode key
/// packets in one `SendInput` batch, so user keystrokes cannot interleave.
pub fn type_unicode_text(text: &str) -> bool {
    let inputs: Vec<RawInput> = text
        .encode_utf16()
        .flat_map(RawInput::unicode_press)
        .collect();
    inputs.is_empty() || send_inputs(&inputs)
}

/// Streaming insertion: delete `count` characters before the caret.
pub fn send_backspaces(count: usize) -> bool {
    const VK_BACK_KEY: u16 = 0x08;
    let inputs: Vec<RawInput> = (0..count)
        .flat_map(|_| RawInput::key_press(VK_BACK_KEY))
        .collect();
    inputs.is_empty() || send_inputs(&inputs)
}

/// Full Win32 executable path of `process_id` (`QueryFullProcessImageNameW`).
pub fn process_path_for_pid(process_id: u32) -> Option<String> {
    if process_id == 0 {
        return None;
    }
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id);
        if handle.is_null() {
            return None;
        }
        let mut buffer = vec![0u16; 32768];
        let mut len = buffer.len() as u32;
        let ok =
            QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut len);
        let _ = CloseHandle(handle);
        if ok == 0 || len == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buffer[..len as usize]))
    }
}

pub fn process_name_for_pid(process_id: u32) -> Option<String> {
    let path = process_path_for_pid(process_id)?;
    std::path::Path::new(&path)
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_string())
}

/// Shared `GetProcessTimes` process-creation-time helper. Combines the
/// creation `FILETIME`'s high/low DWORDs into one `u64`, the same encoding
/// every caller (`windows_focus_continuity.rs`, and `src-tauri`'s
/// `windows_wezterm.rs` via its own copy) uses to compare process identity
/// across a stop/resume boundary. Byte-identical logic to what was
/// previously duplicated per call site: open the process, call
/// `GetProcessTimes`, close the handle, combine the FILETIME on success.
pub fn process_creation_time(pid: u32) -> Option<u64> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut creation: windows_sys::Win32::Foundation::FILETIME = std::mem::zeroed();
        let mut exit: windows_sys::Win32::Foundation::FILETIME = std::mem::zeroed();
        let mut kernel: windows_sys::Win32::Foundation::FILETIME = std::mem::zeroed();
        let mut user: windows_sys::Win32::Foundation::FILETIME = std::mem::zeroed();
        let ok = windows_sys::Win32::System::Threading::GetProcessTimes(
            handle,
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        );
        let _ = CloseHandle(handle);
        (ok != 0)
            .then_some(((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64)
    }
}

#[cfg(test)]
#[path = "windows_tests.rs"]
mod promised_render_tests;
