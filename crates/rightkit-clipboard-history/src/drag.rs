//! Drag of a card out of the strip (spec 7.11.1): OLE `DoDragDrop` over a data object and a drop
//! source. Text, links, colours and code travel as text;
//! rich text as HTML, RTF and text; images as PNG, `CF_DIBV5`, and a Shell virtual file
//! named `Clipboard <date> <time>.png`. Actual file items carry their paths (`CF_HDROP`).
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
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
use windows::Win32::System::Com::STREAM_SEEK_END;
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::UI::Shell::{
    CFSTR_FILECONTENTS, CFSTR_FILEDESCRIPTORW, FD_ATTRIBUTES, FD_FILESIZE, FD_UNICODE,
    FILEDESCRIPTORW, FILEGROUPDESCRIPTORW, SHCreateMemStream,
};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
use windows::core::Interface;

type Hresult = i32;

const S_OK: Hresult = 0;
const E_NOINTERFACE: Hresult = 0x8000_4002u32 as i32;
const E_NOTIMPL: Hresult = 0x8000_4001u32 as i32;
const E_POINTER: Hresult = 0x8000_4003u32 as i32;
const E_OUTOFMEMORY: Hresult = 0x8007_000Eu32 as i32;
const DV_E_FORMATETC: Hresult = 0x8004_0064u32 as i32;
const DV_E_DVTARGETDEVICE: Hresult = 0x8004_0065u32 as i32;
const DV_E_DVASPECT: Hresult = 0x8004_006Bu32 as i32;
const DV_E_LINDEX: Hresult = 0x8004_0068u32 as i32;
const DV_E_TYMED: Hresult = 0x8004_0069u32 as i32;
const OLE_E_ADVISENOTSUPPORTED: Hresult = 0x8004_0003u32 as i32;
const DATA_S_SAMEFORMATETC: Hresult = 0x0004_0130;
const DRAGDROP_S_DROP: Hresult = 0x0004_0100;
const DRAGDROP_S_CANCEL: Hresult = 0x0004_0101;
const DRAGDROP_S_USEDEFAULTCURSORS: Hresult = 0x0004_0102;
const TYMED_HGLOBAL: u32 = 1;
const TYMED_ISTREAM: u32 = 4;
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

/// The HGLOBAL and IStream forms of `STGMEDIUM`.
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
    fn OleUninitialize();
    fn DoDragDrop(data: *mut c_void, source: *mut c_void, allowed: u32, effect: *mut u32) -> i32;
}

#[allow(non_snake_case)]
#[link(name = "shell32")]
unsafe extern "system" {
    fn SHCreateStdEnumFmtEtc(count: u32, formats: *const FormatEtc, out: *mut *mut c_void) -> i32;
}

/// What a drag carries and where it may land inside the strip (screen rectangles as
/// `left, top, right, bottom`).
pub struct Drag {
    pub formats: Formats,
    /// The board tabs, History and Pinned, by id (`history`, `pinned`, a board's id).
    pub tabs: Vec<([i32; 4], String)>,
    /// The cards of a board view, by index.
    pub cards: Vec<([i32; 4], usize)>,
}

/// Where a drag landed inside the strip.
pub enum Landing {
    Tab(String),
    Card(usize),
}

pub struct Outcome {
    pub landing: Option<Landing>,
    /// Another app took the card.
    pub accepted: bool,
}

// ---- what a card carries ------------------------------------------------------------------

/// Formats advertised by one drag. Images may include a memory-backed file.
pub struct Formats {
    entries: Vec<(u32, Vec<u8>)>,
    virtual_png: Option<VirtualPng>,
}

impl From<Vec<(u32, Vec<u8>)>> for Formats {
    fn from(entries: Vec<(u32, Vec<u8>)>) -> Self {
        Self {
            entries,
            virtual_png: None,
        }
    }
}

/// The format IDs for the indexed file contents & the same bytes as a direct PNG.
struct VirtualPng {
    contents: u16,
    png: u16,
}

fn virtual_png(png_format: u32, size: usize) -> Option<(u32, Vec<u8>, VirtualPng)> {
    let size = u32::try_from(size).ok()?;
    // SAFETY: the Windows constants point to static NUL-terminated strings.
    let (descriptor, contents) = unsafe {
        (
            RegisterClipboardFormatW(CFSTR_FILEDESCRIPTORW),
            RegisterClipboardFormatW(CFSTR_FILECONTENTS),
        )
    };
    if descriptor == 0 || contents == 0 || png_format == 0 {
        return None;
    }
    let mut group = vec![0; std::mem::size_of::<FILEGROUPDESCRIPTORW>()];
    group[..4].copy_from_slice(&1u32.to_le_bytes());
    // Serialize fields into the zeroed Windows layout, including any ABI padding.
    let start = std::mem::offset_of!(FILEGROUPDESCRIPTORW, fgd);
    let mut set_u32 = |offset: usize, value: u32| {
        group[start + offset..start + offset + 4].copy_from_slice(&value.to_le_bytes());
    };
    set_u32(
        std::mem::offset_of!(FILEDESCRIPTORW, dwFlags),
        (FD_ATTRIBUTES.0 | FD_FILESIZE.0 | FD_UNICODE.0) as u32,
    );
    set_u32(
        std::mem::offset_of!(FILEDESCRIPTORW, dwFileAttributes),
        FILE_ATTRIBUTE_NORMAL.0,
    );
    set_u32(std::mem::offset_of!(FILEDESCRIPTORW, nFileSizeLow), size);
    let name = utf16z(&format!("Clipboard {}.png", stamp()));
    let name_start = start + std::mem::offset_of!(FILEDESCRIPTORW, cFileName);
    group[name_start..name_start + name.len()].copy_from_slice(&name);
    Some((
        descriptor,
        group,
        VirtualPng {
            contents: contents as u16,
            png: png_format as u16,
        },
    ))
}

fn utf16z(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// The formats `item` is dragged as, or `None` when it cannot be read.
pub fn formats_for(item: &Item) -> Option<Formats> {
    let payload = clip::with(|store| store.payload(&item.id))?;
    let f = win::formats();
    let mut out: Vec<(u32, Vec<u8>)> = Vec::new();
    let mut virtual_file = None;
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
            if let Some((descriptor, group, file)) = virtual_png(f.png, png.len()) {
                out.push((descriptor, group));
                virtual_file = Some(file);
            }
            if let Some(dib) = clip_img::dibv5_from_png(&png) {
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
    (!out.is_empty()).then_some(Formats {
        entries: out,
        virtual_png: virtual_file,
    })
}

/// `2026-10-13 14.05.22`: the local date and time, with no character a file name refuses.
fn stamp() -> String {
    let offset = rightkit_win_ui::clock::local_offset_secs();
    let seconds = clip::now() as i64 + offset;
    let (days, rest) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let (year, month, day) = rightkit_time::civil_from_days(days);
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
    virtual_png: Option<VirtualPng>,
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

/// Validates the same FORMATETC in GetData and QueryGetData and selects one medium.
fn requested_data<'a>(
    object: &'a DataObject,
    format: &FormatEtc,
) -> Result<(&'a [u8], u32), Hresult> {
    if format.aspect != DVASPECT_CONTENT {
        return Err(DV_E_DVASPECT);
    }
    if !format.device.is_null() {
        return Err(DV_E_DVTARGETDEVICE);
    }
    let (kind, index, supported) = match &object.virtual_png {
        Some(file) if file.contents == format.format => {
            (file.png, 0, TYMED_ISTREAM | TYMED_HGLOBAL)
        }
        _ => (format.format, -1, TYMED_HGLOBAL),
    };
    let bytes = object
        .formats
        .iter()
        .find(|(number, _)| *number == kind)
        .map(|(_, bytes)| bytes.as_slice())
        .ok_or(DV_E_FORMATETC)?;
    if format.index != index {
        return Err(DV_E_LINDEX);
    }
    match format.medium & supported {
        mask if mask & TYMED_ISTREAM != 0 => Ok((bytes, TYMED_ISTREAM)),
        mask if mask & TYMED_HGLOBAL != 0 => Ok((bytes, TYMED_HGLOBAL)),
        _ => Err(DV_E_TYMED),
    }
}

unsafe extern "system" fn data_get_data(
    this: *mut DataObject,
    format: *const FormatEtc,
    medium: *mut StgMedium,
) -> Hresult {
    if format.is_null() || medium.is_null() {
        return E_POINTER;
    }
    // SAFETY: the input and output are the caller's. Windows owns the returned medium.
    unsafe {
        let (bytes, kind) = match requested_data(&*this, &*format) {
            Ok(data) => data,
            Err(error) => return error,
        };
        let handle = if kind == TYMED_ISTREAM {
            let Some(stream) = SHCreateMemStream(Some(bytes)) else {
                return E_OUTOFMEMORY;
            };
            // GetData transfers bytes from zero through the returned cursor. This is a
            // fresh stream for each request. A failed seek drops its reference here.
            if let Err(error) = stream.Seek(0, STREAM_SEEK_END, None) {
                return error.code().0;
            }
            stream.into_raw()
        } else {
            win::global_from(bytes)
        };
        if handle.is_null() {
            return E_OUTOFMEMORY;
        }
        *medium = StgMedium {
            medium: kind,
            handle,
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
    requested_data(unsafe { &*this }, unsafe { &*format }).map_or_else(|error| error, |_| S_OK)
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
        let object = &*this;
        let mut list: Vec<FormatEtc> = object
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
        if let Some(file) = &object.virtual_png {
            list.push(FormatEtc {
                format: file.contents,
                device: std::ptr::null_mut(),
                aspect: DVASPECT_CONTENT,
                index: 0,
                medium: TYMED_ISTREAM | TYMED_HGLOBAL,
            });
        }
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
    tabs: Vec<([i32; 4], String)>,
    cards: Vec<([i32; 4], usize)>,
    landing: Mutex<Option<Landing>>,
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
    this: *mut DropSource,
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
        // SAFETY: OLE holds the source reference for this callback.
        let source = unsafe { &*this };
        let landing = source
            .tabs
            .iter()
            .find(|(rect, _)| within(rect, &at))
            .map(|(_, id)| Landing::Tab(id.clone()))
            .or_else(|| {
                source
                    .cards
                    .iter()
                    .find(|(rect, _)| within(rect, &at))
                    .map(|(_, index)| Landing::Card(*index))
            });
        if let Some(landing) = landing {
            *source
                .landing
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(landing);
            return DRAGDROP_S_CANCEL;
        }
    }
    DRAGDROP_S_DROP
}

unsafe extern "system" fn source_give_feedback(_this: *mut DropSource, _effect: u32) -> Hresult {
    DRAGDROP_S_USEDEFAULTCURSORS
}

// ---- running a drag -----------------------------------------------------------------------

struct OleApartment;

impl Drop for OleApartment {
    fn drop(&mut self) {
        // SAFETY: balances the successful initialisation on this thread.
        unsafe { OleUninitialize() };
    }
}

/// Drags `carried` until the button is let go, and says where it landed.
pub fn run(carried: Drag) -> Outcome {
    // SAFETY: the reserved pointer must be null.
    let started = unsafe { OleInitialize(std::ptr::null_mut()) };
    if started < 0 {
        let error = windows::core::Error::from(windows::core::HRESULT(started));
        crate::host::log_error("OleInitialize", &error, "clipboard_drag");
        return Outcome {
            landing: None,
            accepted: false,
        };
    }
    let _ole = OleApartment;
    let formats: Vec<(u16, Vec<u8>)> = carried
        .formats
        .entries
        .into_iter()
        .map(|(format, bytes)| (format as u16, bytes))
        .collect();
    let object = Box::into_raw(Box::new(DataObject {
        vtbl: &DATA_VTBL,
        refs: AtomicU32::new(1),
        formats,
        virtual_png: carried.formats.virtual_png,
    }));
    let source = Box::into_raw(Box::new(DropSource {
        vtbl: &SOURCE_VTBL,
        refs: AtomicU32::new(1),
        tabs: carried.tabs,
        cards: carried.cards,
        landing: Mutex::new(None),
    }));
    let mut effect = 0u32;
    // SAFETY: both are COM objects with the vtables above, alive until released below; the
    // call runs its own message loop and returns when the drag ends.
    let (result, landing) = unsafe {
        let result = DoDragDrop(
            object.cast(),
            source.cast(),
            DROPEFFECT_COPY | DROPEFFECT_LINK,
            &mut effect,
        );
        let landing = (*source)
            .landing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        ((*object).vtbl.release)(object);
        ((*source).vtbl.release)(source);
        (result, landing)
    };
    Outcome {
        accepted: landing.is_none() && result == DRAGDROP_S_DROP && effect != 0,
        landing,
    }
}
