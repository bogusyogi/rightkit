//! Drag of a card out of the strip (spec 7.11.1): OLE `DoDragDrop` over a data object and a drop
//! source written here by hand, as raw vtables, because the `windows` crate's `implement`
//! macro needs `windows-core` named in `windows/Cargo.toml` (it is in the lockfile already, as
//! the crate's own dependency). What a card carries: text, links, colours and code as text;
//! rich text as HTML, RTF and text; an image as a PNG, a `CF_DIBV5` and a file promise (a copy
//! named `Clipboard <date> <time>.png` in `the host's drag folder under the temp directory`, which is emptied the next time
//! the strip starts); a file item as its paths (`CF_HDROP`).
//!
//! The drop source also finds a drop on the strip's own board tabs and cards (the screen
//! rectangles it is given) and cancels the drag there: the strip then does the move itself, so
//! no window has to be a drop target. Nothing here logs item content.

use crate::img as clip_img;
use crate::model::{self as clip, Item, Kind};
use crate::win;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::sync::atomic::{AtomicU32, Ordering};
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

type Hresult = i32;

const S_OK: Hresult = 0;
const E_NOINTERFACE: Hresult = 0x8000_4002u32 as i32;
const E_NOTIMPL: Hresult = 0x8000_4001u32 as i32;
const E_POINTER: Hresult = 0x8000_4003u32 as i32;
const E_OUTOFMEMORY: Hresult = 0x8007_000Eu32 as i32;
const DV_E_FORMATETC: Hresult = 0x8004_0064u32 as i32;
const DV_E_TYMED: Hresult = 0x8004_0069u32 as i32;
const OLE_E_ADVISENOTSUPPORTED: Hresult = 0x8004_0003u32 as i32;
const DATA_S_SAMEFORMATETC: Hresult = 0x0004_0130;
const DRAGDROP_S_DROP: Hresult = 0x0004_0100;
const DRAGDROP_S_CANCEL: Hresult = 0x0004_0101;
const DRAGDROP_S_USEDEFAULTCURSORS: Hresult = 0x0004_0102;
const TYMED_HGLOBAL: u32 = 1;
const DVASPECT_CONTENT: u32 = 1;
const DATADIR_GET: u32 = 1;
const DROPEFFECT_COPY: u32 = 1;
const DROPEFFECT_LINK: u32 = 4;
const MK_LBUTTON: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

const IID_UNKNOWN: Guid = Guid {
    data1: 0,
    data2: 0,
    data3: 0,
    data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46],
};
const IID_DATA_OBJECT: Guid = Guid {
    data1: 0x0000_010E,
    data2: 0,
    data3: 0,
    data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46],
};
const IID_DROP_SOURCE: Guid = Guid {
    data1: 0x0000_0121,
    data2: 0,
    data3: 0,
    data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46],
};

/// `FORMATETC`.
#[repr(C)]
struct FormatEtc {
    format: u16,
    device: *mut c_void,
    aspect: u32,
    index: i32,
    medium: u32,
}

/// `STGMEDIUM`, for `TYMED_HGLOBAL` only.
#[repr(C)]
struct StgMedium {
    medium: u32,
    handle: *mut c_void,
    release: *mut c_void,
}

#[allow(non_snake_case)]
#[link(name = "ole32")]
unsafe extern "system" {
    fn OleInitialize(reserved: *mut c_void) -> i32;
    fn DoDragDrop(data: *mut c_void, source: *mut c_void, allowed: u32, effect: *mut u32) -> i32;
}

#[allow(non_snake_case)]
#[link(name = "shell32")]
unsafe extern "system" {
    fn SHCreateStdEnumFmtEtc(count: u32, formats: *const FormatEtc, out: *mut *mut c_void) -> i32;
}

/// What a drag carries and where it may land inside the strip (screen rectangles as
/// `left, top, right, bottom`).
pub(crate) struct Drag {
    pub formats: Vec<(u32, Vec<u8>)>,
    /// The board tabs, History and Pinned, by id (`history`, `pinned`, a board's id).
    pub tabs: Vec<([i32; 4], String)>,
    /// The cards of a board view, by index.
    pub cards: Vec<([i32; 4], usize)>,
}

/// Where a drag landed inside the strip.
pub(crate) enum Landing {
    Tab(String),
    Card(usize),
}

pub(crate) struct Outcome {
    pub landing: Option<Landing>,
    /// Another app took the card.
    pub accepted: bool,
}

/// Targets of the drag in progress, and where it ended on one of them.
type TabTargets = Vec<([i32; 4], String)>;
type CardTargets = Vec<([i32; 4], usize)>;
static TARGETS: Mutex<(TabTargets, CardTargets)> = Mutex::new((Vec::new(), Vec::new()));
static LANDED: Mutex<Option<Landing>> = Mutex::new(None);

/// Makes the thread an OLE one, and empties the folder the last run left file promises in.
pub(super) fn init() {
    // SAFETY: OLE initialisation of the calling thread, which keeps its apartment for life.
    unsafe {
        OleInitialize(std::ptr::null_mut());
    }
    let _ = std::fs::remove_dir_all(std::env::temp_dir().join(crate::host::drag_copy_dir()));
}

// ---- what a card carries ------------------------------------------------------------------

fn utf16z(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// The formats `item` is dragged as, or `None` when it cannot be read.
pub(super) fn formats_for(item: &Item) -> Option<Vec<(u32, Vec<u8>)>> {
    let payload = clip::with(|store| store.payload(&item.id))?;
    let f = win::formats();
    let mut out: Vec<(u32, Vec<u8>)> = Vec::new();
    match item.kind {
        Kind::File => {
            if payload.paths.is_empty() {
                return None;
            }
            out.push((win::CF_HDROP, win::hdrop_bytes(&payload.paths)));
        }
        Kind::Image => {
            let path = payload.png.as_ref()?;
            let png = crate::seal::read(path)?;
            if let Some(file) = promise_file(&png) {
                out.push((win::CF_HDROP, win::hdrop_bytes(&[file])));
            }
            if let Some(bitmap) = clip_img::decode_file(path, None)
                && let Some(dib) = clip_img::dibv5(&bitmap)
            {
                out.push((win::CF_DIBV5, dib));
            }
            out.push((f.png, png));
        }
        Kind::Rich => {
            if let Some(html) = &payload.html {
                out.push((f.html, html.clone()));
            }
            if let Some(rtf) = &payload.rtf {
                out.push((f.rtf, rtf.clone()));
            }
            out.push((win::CF_UNICODETEXT, utf16z(&payload.text)));
        }
        _ => out.push((win::CF_UNICODETEXT, utf16z(&payload.text))),
    }
    (!out.is_empty()).then_some(out)
}

/// A copy of `png` in the promise folder, named for the moment: its path.
fn promise_file(png: &[u8]) -> Option<String> {
    let dir = std::env::temp_dir().join(crate::host::drag_copy_dir());
    std::fs::create_dir_all(&dir).ok()?;
    let name = format!("Clipboard {}.png", stamp());
    let path = dir.join(name);
    rightkit_fs::atomic_write(&path, png).ok()?;
    Some(path.to_string_lossy().into_owned())
}

/// `2026-10-13 14.05.22`: the local date and time, with no character a file name refuses.
fn stamp() -> String {
    let offset = rightkit_win_ui::clock::local_offset_secs();
    let seconds = clip::now() as i64 + offset;
    let (days, rest) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    // Civil date from a day count (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}.{:02}.{:02}",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

// ---- the data object ----------------------------------------------------------------------

#[repr(C)]
struct DataVtbl {
    query_interface:
        unsafe extern "system" fn(*mut DataObject, *const Guid, *mut *mut c_void) -> Hresult,
    add_ref: unsafe extern "system" fn(*mut DataObject) -> u32,
    release: unsafe extern "system" fn(*mut DataObject) -> u32,
    get_data:
        unsafe extern "system" fn(*mut DataObject, *const FormatEtc, *mut StgMedium) -> Hresult,
    get_data_here:
        unsafe extern "system" fn(*mut DataObject, *const FormatEtc, *mut StgMedium) -> Hresult,
    query_get_data: unsafe extern "system" fn(*mut DataObject, *const FormatEtc) -> Hresult,
    canonical:
        unsafe extern "system" fn(*mut DataObject, *const FormatEtc, *mut FormatEtc) -> Hresult,
    set_data: unsafe extern "system" fn(
        *mut DataObject,
        *const FormatEtc,
        *const StgMedium,
        i32,
    ) -> Hresult,
    enum_format: unsafe extern "system" fn(*mut DataObject, u32, *mut *mut c_void) -> Hresult,
    advise: unsafe extern "system" fn(
        *mut DataObject,
        *const FormatEtc,
        u32,
        *mut c_void,
        *mut u32,
    ) -> Hresult,
    unadvise: unsafe extern "system" fn(*mut DataObject, u32) -> Hresult,
    enum_advise: unsafe extern "system" fn(*mut DataObject, *mut *mut c_void) -> Hresult,
}

#[repr(C)]
struct DataObject {
    vtbl: &'static DataVtbl,
    refs: AtomicU32,
    formats: Vec<(u16, Vec<u8>)>,
}

static DATA_VTBL: DataVtbl = DataVtbl {
    query_interface: data_query_interface,
    add_ref: data_add_ref,
    release: data_release,
    get_data: data_get_data,
    get_data_here: data_get_data_here,
    query_get_data: data_query_get_data,
    canonical: data_canonical,
    set_data: data_set_data,
    enum_format: data_enum_format,
    advise: data_advise,
    unadvise: data_unadvise,
    enum_advise: data_enum_advise,
};

unsafe extern "system" fn data_query_interface(
    this: *mut DataObject,
    iid: *const Guid,
    out: *mut *mut c_void,
) -> Hresult {
    if iid.is_null() || out.is_null() {
        return E_POINTER;
    }
    // SAFETY: both pointers are the caller's, valid for these reads and writes; `this` is the
    // object made in `run`, alive while it holds a reference.
    unsafe {
        let wanted = *iid;
        if wanted == IID_UNKNOWN || wanted == IID_DATA_OBJECT {
            *out = this.cast();
            (*this).refs.fetch_add(1, Ordering::AcqRel);
            S_OK
        } else {
            *out = std::ptr::null_mut();
            E_NOINTERFACE
        }
    }
}

unsafe extern "system" fn data_add_ref(this: *mut DataObject) -> u32 {
    // SAFETY: `this` is alive while a reference is held.
    unsafe { (*this).refs.fetch_add(1, Ordering::AcqRel) + 1 }
}

unsafe extern "system" fn data_release(this: *mut DataObject) -> u32 {
    // SAFETY: the last release frees the box `run` made; no reference is used after it.
    unsafe {
        let left = (*this).refs.fetch_sub(1, Ordering::AcqRel) - 1;
        if left == 0 {
            drop(Box::from_raw(this));
        }
        left
    }
}

/// Whether the request is one the object can answer: the content aspect on global memory, of a
/// format it holds.
fn answers(object: &DataObject, format: &FormatEtc) -> Option<usize> {
    if format.aspect != DVASPECT_CONTENT || format.medium & TYMED_HGLOBAL == 0 {
        return None;
    }
    object.formats.iter().position(|(f, _)| *f == format.format)
}

unsafe extern "system" fn data_get_data(
    this: *mut DataObject,
    format: *const FormatEtc,
    medium: *mut StgMedium,
) -> Hresult {
    if format.is_null() || medium.is_null() {
        return E_POINTER;
    }
    // SAFETY: the pointers are the caller's; the block handed back is the caller's to free.
    unsafe {
        let (object, format) = (&*this, &*format);
        let Some(at) = answers(object, format) else {
            return if format.medium & TYMED_HGLOBAL == 0 {
                DV_E_TYMED
            } else {
                DV_E_FORMATETC
            };
        };
        let memory = win::global_from(&object.formats[at].1);
        if memory.is_null() {
            return E_OUTOFMEMORY;
        }
        *medium = StgMedium {
            medium: TYMED_HGLOBAL,
            handle: memory,
            release: std::ptr::null_mut(),
        };
        S_OK
    }
}

unsafe extern "system" fn data_get_data_here(
    _this: *mut DataObject,
    _format: *const FormatEtc,
    _medium: *mut StgMedium,
) -> Hresult {
    E_NOTIMPL
}

unsafe extern "system" fn data_query_get_data(
    this: *mut DataObject,
    format: *const FormatEtc,
) -> Hresult {
    if format.is_null() {
        return E_POINTER;
    }
    // SAFETY: the pointers are the caller's and valid for these reads.
    let (object, format) = unsafe { (&*this, &*format) };
    if answers(object, format).is_some() {
        S_OK
    } else {
        DV_E_FORMATETC
    }
}

unsafe extern "system" fn data_canonical(
    _this: *mut DataObject,
    _format: *const FormatEtc,
    out: *mut FormatEtc,
) -> Hresult {
    if !out.is_null() {
        // SAFETY: the caller's out parameter.
        unsafe { (*out).device = std::ptr::null_mut() };
    }
    DATA_S_SAMEFORMATETC
}

unsafe extern "system" fn data_set_data(
    _this: *mut DataObject,
    _format: *const FormatEtc,
    _medium: *const StgMedium,
    _release: i32,
) -> Hresult {
    E_NOTIMPL
}

unsafe extern "system" fn data_enum_format(
    this: *mut DataObject,
    direction: u32,
    out: *mut *mut c_void,
) -> Hresult {
    if out.is_null() {
        return E_POINTER;
    }
    if direction != DATADIR_GET {
        return E_NOTIMPL;
    }
    // SAFETY: `this` is alive; the shell copies the list it is given.
    unsafe {
        let list: Vec<FormatEtc> = (*this)
            .formats
            .iter()
            .map(|(format, _)| FormatEtc {
                format: *format,
                device: std::ptr::null_mut(),
                aspect: DVASPECT_CONTENT,
                index: -1,
                medium: TYMED_HGLOBAL,
            })
            .collect();
        SHCreateStdEnumFmtEtc(list.len() as u32, list.as_ptr(), out)
    }
}

unsafe extern "system" fn data_advise(
    _this: *mut DataObject,
    _format: *const FormatEtc,
    _flags: u32,
    _sink: *mut c_void,
    _connection: *mut u32,
) -> Hresult {
    OLE_E_ADVISENOTSUPPORTED
}

unsafe extern "system" fn data_unadvise(_this: *mut DataObject, _connection: u32) -> Hresult {
    OLE_E_ADVISENOTSUPPORTED
}

unsafe extern "system" fn data_enum_advise(
    _this: *mut DataObject,
    _out: *mut *mut c_void,
) -> Hresult {
    OLE_E_ADVISENOTSUPPORTED
}

// ---- the drop source ----------------------------------------------------------------------

#[repr(C)]
struct SourceVtbl {
    query_interface:
        unsafe extern "system" fn(*mut DropSource, *const Guid, *mut *mut c_void) -> Hresult,
    add_ref: unsafe extern "system" fn(*mut DropSource) -> u32,
    release: unsafe extern "system" fn(*mut DropSource) -> u32,
    query_continue: unsafe extern "system" fn(*mut DropSource, i32, u32) -> Hresult,
    give_feedback: unsafe extern "system" fn(*mut DropSource, u32) -> Hresult,
}

#[repr(C)]
struct DropSource {
    vtbl: &'static SourceVtbl,
    refs: AtomicU32,
}

static SOURCE_VTBL: SourceVtbl = SourceVtbl {
    query_interface: source_query_interface,
    add_ref: source_add_ref,
    release: source_release,
    query_continue: source_query_continue,
    give_feedback: source_give_feedback,
};

unsafe extern "system" fn source_query_interface(
    this: *mut DropSource,
    iid: *const Guid,
    out: *mut *mut c_void,
) -> Hresult {
    if iid.is_null() || out.is_null() {
        return E_POINTER;
    }
    // SAFETY: as for the data object.
    unsafe {
        let wanted = *iid;
        if wanted == IID_UNKNOWN || wanted == IID_DROP_SOURCE {
            *out = this.cast();
            (*this).refs.fetch_add(1, Ordering::AcqRel);
            S_OK
        } else {
            *out = std::ptr::null_mut();
            E_NOINTERFACE
        }
    }
}

unsafe extern "system" fn source_add_ref(this: *mut DropSource) -> u32 {
    // SAFETY: `this` is alive while a reference is held.
    unsafe { (*this).refs.fetch_add(1, Ordering::AcqRel) + 1 }
}

unsafe extern "system" fn source_release(this: *mut DropSource) -> u32 {
    // SAFETY: the last release frees the box `run` made.
    unsafe {
        let left = (*this).refs.fetch_sub(1, Ordering::AcqRel) - 1;
        if left == 0 {
            drop(Box::from_raw(this));
        }
        left
    }
}

/// Whether `at` is inside a screen rectangle `[left, top, right, bottom]`.
fn within(rect: &[i32; 4], at: &POINT) -> bool {
    at.x >= rect[0] && at.x < rect[2] && at.y >= rect[1] && at.y < rect[3]
}

unsafe extern "system" fn source_query_continue(
    _this: *mut DropSource,
    escape: i32,
    keys: u32,
) -> Hresult {
    if escape != 0 {
        return DRAGDROP_S_CANCEL;
    }
    if keys & MK_LBUTTON != 0 {
        return S_OK;
    }
    // The button is up: on one of the strip's own tabs or cards the drag is cancelled and the
    // strip does the move; anywhere else the target gets the drop.
    let mut at = POINT::default();
    // SAFETY: a plain query of the pointer.
    if unsafe { GetCursorPos(&mut at) }.is_ok() {
        let targets = TARGETS.lock().unwrap_or_else(PoisonError::into_inner);
        let landing = targets
            .0
            .iter()
            .find(|(rect, _)| within(rect, &at))
            .map(|(_, id)| Landing::Tab(id.clone()))
            .or_else(|| {
                targets
                    .1
                    .iter()
                    .find(|(rect, _)| within(rect, &at))
                    .map(|(_, index)| Landing::Card(*index))
            });
        drop(targets);
        if let Some(landing) = landing {
            *LANDED.lock().unwrap_or_else(PoisonError::into_inner) = Some(landing);
            return DRAGDROP_S_CANCEL;
        }
    }
    DRAGDROP_S_DROP
}

unsafe extern "system" fn source_give_feedback(_this: *mut DropSource, _effect: u32) -> Hresult {
    DRAGDROP_S_USEDEFAULTCURSORS
}

// ---- running a drag -----------------------------------------------------------------------

/// Drags `carried` until the button is let go, and says where it landed.
pub(crate) fn run(carried: Drag) -> Outcome {
    *TARGETS.lock().unwrap_or_else(PoisonError::into_inner) = (carried.tabs, carried.cards);
    *LANDED.lock().unwrap_or_else(PoisonError::into_inner) = None;
    let formats: Vec<(u16, Vec<u8>)> = carried
        .formats
        .into_iter()
        .map(|(format, bytes)| (format as u16, bytes))
        .collect();
    let object = Box::into_raw(Box::new(DataObject {
        vtbl: &DATA_VTBL,
        refs: AtomicU32::new(1),
        formats,
    }));
    let source = Box::into_raw(Box::new(DropSource {
        vtbl: &SOURCE_VTBL,
        refs: AtomicU32::new(1),
    }));
    let mut effect = 0u32;
    // SAFETY: both are COM objects with the vtables above, alive until released below; the
    // call runs its own message loop and returns when the drag ends.
    let result = unsafe {
        let result = DoDragDrop(
            object.cast(),
            source.cast(),
            DROPEFFECT_COPY | DROPEFFECT_LINK,
            &mut effect,
        );
        ((*object).vtbl.release)(object);
        ((*source).vtbl.release)(source);
        result
    };
    let landing = LANDED.lock().unwrap_or_else(PoisonError::into_inner).take();
    Outcome {
        accepted: landing.is_none() && result == DRAGDROP_S_DROP && effect != 0,
        landing,
    }
}
