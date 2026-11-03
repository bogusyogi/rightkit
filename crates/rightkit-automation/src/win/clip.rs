//! Windows clipboard: every format's raw bytes, and a change listener on its own message-only
//! window (`AddClipboardFormatListener`). The cross-platform surface is [`crate::clipboard`].

use crate::clipboard::{RawClipboard, RawFormat, RawLimits};
use crate::model::*;
use std::sync::mpsc;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HGLOBAL, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS_NAME: PCWSTR = w!("RightKitClipboardWatch");

/// The change counter the OS bumps on every clipboard write.
pub fn sequence() -> u64 {
    u64::from(unsafe { GetClipboardSequenceNumber() })
}

/// The clipboard, open for this thread; closed on drop.
struct Open;
impl Open {
    fn acquire() -> Option<Open> {
        for _ in 0..20 {
            if unsafe { OpenClipboard(None) }.is_ok() {
                return Some(Open);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        None
    }
}
impl Drop for Open {
    fn drop(&mut self) {
        let _ = unsafe { CloseClipboard() };
    }
}

/// Name of clipboard format `f`: the `CF_*` name for a predefined one, the registered name for a
/// registered one, else `format:N`.
fn format_name(f: u32) -> String {
    let std = match f {
        1 => "CF_TEXT",
        2 => "CF_BITMAP",
        3 => "CF_METAFILEPICT",
        4 => "CF_SYLK",
        5 => "CF_DIF",
        6 => "CF_TIFF",
        7 => "CF_OEMTEXT",
        8 => "CF_DIB",
        9 => "CF_PALETTE",
        10 => "CF_PENDATA",
        11 => "CF_RIFF",
        12 => "CF_WAVE",
        13 => "CF_UNICODETEXT",
        14 => "CF_ENHMETAFILE",
        15 => "CF_HDROP",
        16 => "CF_LOCALE",
        17 => "CF_DIBV5",
        _ => "",
    };
    if !std.is_empty() {
        return std.to_string();
    }
    let mut buf = [0u16; 256];
    let n = unsafe { GetClipboardFormatNameW(f, &mut buf) };
    if n > 0 {
        String::from_utf16_lossy(&buf[..n as usize])
    } else {
        format!("format:{f}")
    }
}

/// Formats whose data handle is a GDI or display object, not a global memory block, so there
/// are no bytes to copy.
fn is_handle_format(f: u32) -> bool {
    matches!(f, 2 | 3 | 9 | 14) || (0x80..=0x8F).contains(&f) || (0x300..=0x3FF).contains(&f)
}

fn registered(name: PCWSTR) -> u32 {
    unsafe { RegisterClipboardFormatW(name) }
}

/// Whether the copying app asked that this content stay out of clipboard monitors, history and
/// cloud sync (password managers do): `ExcludeClipboardContentFromMonitorProcessing` present, or
/// `CanIncludeInClipboardHistory` / `CanUploadToCloudClipboard` holding a zero DWORD.
fn concealed(present: &[u32]) -> bool {
    let exclude = registered(w!("ExcludeClipboardContentFromMonitorProcessing"));
    if present.contains(&exclude) {
        return true;
    }
    [
        w!("CanIncludeInClipboardHistory"),
        w!("CanUploadToCloudClipboard"),
    ]
    .into_iter()
    .any(|name| {
        let id = registered(name);
        present.contains(&id)
            && read_global(id, 4).is_some_and(|(b, _)| b.len() == 4 && b.iter().all(|x| *x == 0))
    })
}

/// The bytes of one format (at most `max`), and whether more existed. The clipboard must be open.
fn read_global(format: u32, max: usize) -> Option<(Vec<u8>, bool)> {
    unsafe {
        let h = GetClipboardData(format).ok()?;
        let g = HGLOBAL(h.0);
        let size = GlobalSize(g);
        if size == 0 {
            return None;
        }
        let p = GlobalLock(g) as *const u8;
        if p.is_null() {
            return None;
        }
        let take = size.min(max);
        let bytes = std::slice::from_raw_parts(p, take).to_vec();
        let _ = GlobalUnlock(g);
        Some((bytes, size > take))
    }
}

fn read_once(limits: &RawLimits, sequence: u64) -> Result<RawClipboard> {
    let _open = Open::acquire().ok_or(Error::Platform(0, "OpenClipboard (held by another app)"))?;
    let mut present = Vec::new();
    let mut f = 0u32;
    loop {
        f = unsafe { EnumClipboardFormats(f) };
        if f == 0 {
            break;
        }
        present.push(f);
    }
    let mut out = RawClipboard {
        sequence,
        formats: Vec::new(),
        skipped: Vec::new(),
        concealed: false,
    };
    if concealed(&present) {
        out.concealed = true;
        out.skipped = present
            .iter()
            .map(|f| {
                format!(
                    "{} (withheld: the copying app marked the content as private)",
                    format_name(*f)
                )
            })
            .collect();
        return Ok(out);
    }
    let mut total = 0usize;
    for f in present {
        let name = format_name(f);
        if is_handle_format(f) {
            out.skipped.push(format!("{name} (GDI handle, no bytes)"));
            continue;
        }
        if total >= limits.max_total_bytes {
            out.skipped
                .push(format!("{name} (total byte limit reached)"));
            continue;
        }
        let room = limits.max_format_bytes.min(limits.max_total_bytes - total);
        match read_global(f, room) {
            Some((bytes, truncated)) => {
                total += bytes.len();
                out.formats.push(RawFormat {
                    name,
                    bytes,
                    truncated,
                });
            }
            None => out.skipped.push(format!("{name} (unreadable or empty)")),
        }
    }
    Ok(out)
}

/// Every format on the clipboard with its bytes, read while holding the clipboard open. Retries
/// when the sequence number moved during the read.
pub fn read_raw(limits: &RawLimits) -> Result<RawClipboard> {
    for _ in 0..3 {
        let before = sequence();
        let r = read_once(limits, before)?;
        if sequence() == before {
            return Ok(r);
        }
    }
    Err(Error::Platform(
        0,
        "the clipboard kept changing during the read",
    ))
}

// ------------------------------------------------------------------ listener

struct Ctx {
    on_change: Box<dyn Fn(u64) + Send>,
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CLIPBOARDUPDATE => {
            let p = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Ctx;
            if !p.is_null() {
                // A panic must not unwind through the window procedure into the OS.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    // SAFETY: the pointer is the Box leaked by the listener thread, freed only
                    // after its message loop (and so this procedure) has ended.
                    (unsafe { &*p }.on_change)(sequence());
                }));
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = unsafe { RemoveClipboardFormatListener(hwnd) };
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// A running listener. Dropping it stops the listener and joins its thread.
pub struct Watcher {
    hwnd: isize,
    thread: Option<std::thread::JoinHandle<()>>,
}

pub fn watch(on_change: Box<dyn Fn(u64) + Send>) -> Result<Watcher> {
    let (tx, rx) = mpsc::channel::<std::result::Result<isize, Error>>();
    let thread = std::thread::Builder::new()
        .name("rightkit-clipboard-watch".into())
        .spawn(move || {
            let ctx = Box::into_raw(Box::new(Ctx { on_change }));
            let fail = |e: Error| {
                let _ = tx.send(Err(e));
            };
            unsafe {
                let hinst = match GetModuleHandleW(PCWSTR::null()) {
                    Ok(m) => HINSTANCE(m.0),
                    Err(_) => HINSTANCE::default(),
                };
                let class = WNDCLASSW {
                    lpfnWndProc: Some(wndproc),
                    hInstance: hinst,
                    lpszClassName: CLASS_NAME,
                    ..Default::default()
                };
                // Zero with ERROR_CLASS_ALREADY_EXISTS when an earlier watcher registered it;
                // the class is then reused, so the result is not an error.
                RegisterClassW(&class);
                let created = CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    CLASS_NAME,
                    w!(""),
                    WINDOW_STYLE(0),
                    0,
                    0,
                    0,
                    0,
                    Some(HWND_MESSAGE),
                    None,
                    Some(hinst),
                    None,
                );
                match created {
                    Err(e) => {
                        fail(Error::Platform(i64::from(e.code().0), "CreateWindowEx"));
                    }
                    Ok(h) => {
                        SetWindowLongPtrW(h, GWLP_USERDATA, ctx as isize);
                        match AddClipboardFormatListener(h) {
                            Err(e) => {
                                let _ = DestroyWindow(h);
                                fail(Error::Platform(
                                    i64::from(e.code().0),
                                    "AddClipboardFormatListener",
                                ));
                            }
                            Ok(()) => {
                                let _ = tx.send(Ok(h.0 as isize));
                                let mut msg = MSG::default();
                                while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                                    let _ = TranslateMessage(&msg);
                                    DispatchMessageW(&msg);
                                }
                            }
                        }
                    }
                }
                // The loop (or the failed start) is over: nothing can call the procedure now.
                drop(Box::from_raw(ctx));
            }
        })
        .map_err(|_| Error::Platform(0, "spawn clipboard watcher thread"))?;
    match rx.recv() {
        Ok(Ok(hwnd)) => Ok(Watcher {
            hwnd,
            thread: Some(thread),
        }),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => {
            let _ = thread.join();
            Err(Error::Platform(0, "clipboard watcher thread ended early"))
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let h = HWND(self.hwnd as *mut _);
        let _ = unsafe { PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
