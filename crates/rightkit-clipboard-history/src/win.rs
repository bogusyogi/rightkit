//! The clipboard's Win32 calls for the history (`clip_capture.rs` reads, `clip_paste.rs`
//! writes): raw declarations (user32, kernel32, shell32), like `send_sys.rs`, so no `windows`
//! crate feature has to change; the registered format names of the spec's 3.1 and 3.5; and
//! small readers and writers over an open clipboard. Each `unsafe` block says what it relies
//! on. Nothing here logs: what is read is item content.

use std::ffi::c_void;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::sync::OnceLock;
use std::time::Duration;

pub type Handle = *mut c_void;

pub const CF_UNICODETEXT: u32 = 13;
pub const CF_DIB: u32 = 8;
pub const CF_HDROP: u32 = 15;
pub const CF_DIBV5: u32 = 17;
const GMEM_MOVEABLE: u32 = 0x0002;

#[allow(non_snake_case)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GlobalAlloc(flags: u32, bytes: usize) -> Handle;
    fn GlobalFree(memory: Handle) -> Handle;
    fn GlobalLock(memory: Handle) -> *mut c_void;
    fn GlobalUnlock(memory: Handle) -> i32;
    fn GlobalSize(memory: Handle) -> usize;
}

#[allow(non_snake_case)]
#[link(name = "user32")]
unsafe extern "system" {
    fn OpenClipboard(owner: Handle) -> i32;
    fn CloseClipboard() -> i32;
    fn EmptyClipboard() -> i32;
    fn GetClipboardData(format: u32) -> Handle;
    fn SetClipboardData(format: u32, memory: Handle) -> Handle;
    fn IsClipboardFormatAvailable(format: u32) -> i32;
    fn RegisterClipboardFormatW(name: *const u16) -> u32;
    fn GetClipboardSequenceNumber() -> u32;
    fn EnumClipboardFormats(format: u32) -> u32;
    fn GetClipboardOwner() -> Handle;
    fn AddClipboardFormatListener(window: Handle) -> i32;
    fn RemoveClipboardFormatListener(window: Handle) -> i32;
}

#[allow(non_snake_case)]
#[link(name = "shell32")]
unsafe extern "system" {
    fn DragQueryFileW(drop: Handle, index: u32, buffer: *mut u16, length: u32) -> u32;
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The registered formats the history reads, tells apart or sets, looked up once.
pub struct Formats {
    pub png: u32,
    pub html: u32,
    pub rtf: u32,
    /// The application's own marker on everything it writes (3.1).
    pub own: u32,
    /// Microsoft's "do not monitor" flag, any value (3.5).
    pub exclude: u32,
    /// Microsoft's history flag: a DWORD, zero means keep out of history (3.5).
    pub no_history: u32,
    pub no_cloud: u32,
}

pub fn formats() -> &'static Formats {
    static FORMATS: OnceLock<Formats> = OnceLock::new();
    FORMATS.get_or_init(|| {
        let register = |name: &str| {
            let name = wide(name);
            // SAFETY: `name` is NUL-terminated and outlives the call.
            unsafe { RegisterClipboardFormatW(name.as_ptr()) }
        };
        Formats {
            png: register("PNG"),
            html: register("HTML Format"),
            rtf: register("Rich Text Format"),
            own: register(&crate::host::own_write_format()),
            exclude: register("ExcludeClipboardContentFromMonitorProcessing"),
            no_history: register("CanIncludeInClipboardHistory"),
            no_cloud: register("CanUploadToCloudClipboard"),
        }
    })
}

/// Whether the clipboard offers `format` (told from its list of formats; nothing is opened).
pub fn available(format: u32) -> bool {
    // SAFETY: only looks at the clipboard's list of formats.
    format != 0 && unsafe { IsClipboardFormatAvailable(format) } != 0
}

/// The clipboard's change count.
pub fn sequence() -> u32 {
    // SAFETY: a plain query.
    unsafe { GetClipboardSequenceNumber() }
}

/// Every format on the clipboard, in the system's order (the clipboard must be open).
pub fn format_list(_open: &Open) -> Vec<u32> {
    let mut list = Vec::new();
    let mut format = 0;
    loop {
        // SAFETY: the clipboard is open (the `Open` borrow); the system walks its own list.
        format = unsafe { EnumClipboardFormats(format) };
        if format == 0 {
            return list;
        }
        list.push(format);
    }
}

/// The window that last wrote the clipboard, as a handle value (0 when none).
pub fn owner() -> isize {
    // SAFETY: a plain query.
    unsafe { GetClipboardOwner() as isize }
}

/// Starts or stops the change notification to `window` (a handle value). True when it worked.
pub fn listen(window: isize, on: bool) -> bool {
    // SAFETY: `window` is a window of this thread; a failure only returns zero.
    unsafe {
        if on {
            AddClipboardFormatListener(window as Handle) != 0
        } else {
            RemoveClipboardFormatListener(window as Handle) != 0
        }
    }
}

/// The clipboard held open; closed when dropped.
pub struct Open;

impl Open {
    /// Opens the clipboard, trying again after each of `waits` when another program holds it.
    pub fn acquire(waits: &[u64]) -> Option<Open> {
        let mut delays = vec![0u64];
        delays.extend_from_slice(waits);
        for wait in delays {
            if wait > 0 {
                std::thread::sleep(Duration::from_millis(wait));
            }
            // SAFETY: a null owner associates the open clipboard with the current task.
            if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                return Some(Open);
            }
        }
        None
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        // SAFETY: balances the successful OpenClipboard in `acquire`.
        unsafe { CloseClipboard() };
    }
}

// ---- reading (the clipboard must be open) -------------------------------------------------

/// The bytes of `format`, or `None` when it is absent, empty or over `max` bytes.
pub fn read_bytes(_open: &Open, format: u32, max: usize) -> Option<Vec<u8>> {
    if !available(format) {
        return None;
    }
    // SAFETY: the clipboard is open (the `Open` borrow); the handle is only read, within the
    // size the system reports, while locked.
    unsafe {
        let memory = GetClipboardData(format);
        if memory.is_null() {
            return None;
        }
        let size = GlobalSize(memory);
        if size == 0 || size > max {
            return None;
        }
        let pointer = GlobalLock(memory) as *const u8;
        if pointer.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(pointer, size).to_vec();
        GlobalUnlock(memory);
        Some(bytes)
    }
}

/// A DWORD flag format's value.
pub fn read_dword(open: &Open, format: u32) -> Option<u32> {
    let bytes = read_bytes(open, format, 64)?;
    (bytes.len() >= 4).then(|| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// The text (`CF_UNICODETEXT`) up to its NUL, or `None` when it is absent or over `max_bytes`.
pub fn read_text(_open: &Open, max_bytes: usize) -> Option<String> {
    if !available(CF_UNICODETEXT) {
        return None;
    }
    // SAFETY: as `read_bytes`; the UTF-16 units are read within the reported size.
    unsafe {
        let memory = GetClipboardData(CF_UNICODETEXT);
        if memory.is_null() {
            return None;
        }
        let size = GlobalSize(memory);
        if size > max_bytes {
            return None;
        }
        let pointer = GlobalLock(memory) as *const u16;
        if pointer.is_null() {
            return None;
        }
        let units = std::slice::from_raw_parts(pointer, size / 2);
        let end = units
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(units.len());
        let text = String::from_utf16_lossy(&units[..end]);
        GlobalUnlock(memory);
        Some(text)
    }
}

/// The paths of a file copy (`CF_HDROP`).
pub fn read_paths(_open: &Open) -> Vec<String> {
    let mut paths = Vec::new();
    if !available(CF_HDROP) {
        return paths;
    }
    // SAFETY: as `read_bytes`; the drop handle is only queried.
    unsafe {
        let drop = GetClipboardData(CF_HDROP);
        if drop.is_null() {
            return paths;
        }
        let count = DragQueryFileW(drop, u32::MAX, std::ptr::null_mut(), 0);
        for index in 0..count {
            let length = DragQueryFileW(drop, index, std::ptr::null_mut(), 0) as usize;
            if length == 0 {
                continue;
            }
            let mut buffer = vec![0u16; length + 1];
            let copied =
                DragQueryFileW(drop, index, buffer.as_mut_ptr(), buffer.len() as u32) as usize;
            if copied > 0 && copied <= length {
                paths.push(
                    std::ffi::OsString::from_wide(&buffer[..copied])
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    paths
}

// ---- writing (the clipboard must be open) -------------------------------------------------

/// Empties the clipboard; what is set after belongs to the window-less open.
pub fn empty(_open: &Open) -> bool {
    // SAFETY: the clipboard is open (the `Open` borrow).
    unsafe { EmptyClipboard() != 0 }
}

/// Sets `format` to `bytes`. The block is handed to the clipboard on success and freed here
/// when it is refused.
pub fn set_bytes(_open: &Open, format: u32, bytes: &[u8]) -> bool {
    if format == 0 {
        return false;
    }
    // SAFETY: the clipboard is open; the block is filled before it is handed over.
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1));
        if memory.is_null() {
            return false;
        }
        let pointer = GlobalLock(memory) as *mut u8;
        if pointer.is_null() {
            GlobalFree(memory);
            return false;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer, bytes.len());
        GlobalUnlock(memory);
        if SetClipboardData(format, memory).is_null() {
            GlobalFree(memory);
            return false;
        }
    }
    true
}

pub fn set_text(open: &Open, text: &str) -> bool {
    let bytes: Vec<u8> = std::ffi::OsStr::new(text)
        .encode_wide()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    set_bytes(open, CF_UNICODETEXT, &bytes)
}

pub fn set_dword(open: &Open, format: u32, value: u32) -> bool {
    set_bytes(open, format, &value.to_le_bytes())
}

/// A file list as the bytes of `CF_HDROP`: a `DROPFILES` header (the offset of the list, a
/// point, two flags, the last for wide characters) and the paths, each ended by a NUL, the list
/// by another.
pub fn hdrop_bytes(paths: &[String]) -> Vec<u8> {
    const HEADER: usize = 20;
    let mut bytes = vec![0u8; HEADER];
    bytes[0..4].copy_from_slice(&(HEADER as u32).to_le_bytes());
    bytes[16..20].copy_from_slice(&1u32.to_le_bytes());
    for path in paths {
        for unit in std::ffi::OsStr::new(path).encode_wide() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes.extend_from_slice(&[0, 0]);
    }
    bytes.extend_from_slice(&[0, 0]);
    bytes
}

/// A file list as `CF_HDROP`.
pub fn set_paths(open: &Open, paths: &[String]) -> bool {
    set_bytes(open, CF_HDROP, &hdrop_bytes(paths))
}

/// Deletes the copies a card drag made for other apps (the host's drag folder under the temp directory): what was dragged
/// out is not kept once the history lets go of it. A drag still in the air has its file already
/// handed over; a later one makes its copy again.
pub fn clear_drag_copies() {
    let _ = std::fs::remove_dir_all(std::env::temp_dir().join(crate::host::drag_copy_dir()));
}

/// A movable block of global memory holding `bytes`, for a data object's medium; the receiver
/// frees it. Null when none could be had.
pub fn global_from(bytes: &[u8]) -> Handle {
    // SAFETY: the block is filled before it is handed over.
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1));
        if memory.is_null() {
            return memory;
        }
        let pointer = GlobalLock(memory) as *mut u8;
        if pointer.is_null() {
            GlobalFree(memory);
            return std::ptr::null_mut();
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer, bytes.len());
        GlobalUnlock(memory);
        memory
    }
}
