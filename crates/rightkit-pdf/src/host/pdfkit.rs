//! The macOS backend: AppKit and PDFKit objects behind [`super::backend::HostBackend`].
//!
//! Donor: ViewRight `mac_pdf_view.rs`. Differences: no Tauri types (the parent view arrives as a
//! [`NativeParent`]), no process-wide table keyed by window label (the host owns the policy and
//! hands this backend a [`ViewId`]), links and key events go through [`HostHooks`], and every entry
//! point checks it runs on the AppKit main thread before it touches an object.
//!
//! The table holds owning raw pointers only (`usize`). Each pointer is reconstructed, used and
//! returned on the main thread, so no `MainThreadOnly` object crosses a thread. Objects are
//! released by [`HostBackend::release`]; a record that is dropped without it leaks, which is why the
//! host calls `release` on every path out of its tables.

use std::collections::HashMap;
use std::ffi::c_void;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::{define_class, msg_send, AnyThread, MainThreadMarker, MainThreadOnly, Message};
use objc2_app_kit::{
    NSColor, NSEvent, NSEventMask, NSEventModifierFlags, NSPrintInfo, NSView, NSWindow,
};
use objc2_foundation::{
    NSArray, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSStringCompareOptions,
    NSURL,
};
use objc2_pdf_kit::{
    PDFActionNamed, PDFActionNamedName, PDFDisplayBox, PDFDisplayDirection, PDFDisplayMode,
    PDFDocument, PDFOutline, PDFSelection, PDFThumbnailView, PDFView, PDFViewDelegate,
};

use super::backend::{BackendOpen, FindQuery, HostBackend, ViewId, ViewState};
use super::{
    clamp_page, normalized_rotation, HostConfig, HostError, HostFrame, HostHooks, HostKey,
    HostOutlineItem, NativeParent, UpdateRequest,
};

/// Set once, by the one PDFKit host the process may build. The delegate class below is registered
/// process-wide, so its link policy is process-wide too.
static LINK_HOOKS: OnceLock<HostHooks> = OnceLock::new();
static CREATED: AtomicBool = AtomicBool::new(false);

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RightKitPdfReaderDelegate"]
    struct ReaderDelegate;
    unsafe impl NSObjectProtocol for ReaderDelegate {}
    unsafe impl PDFViewDelegate for ReaderDelegate {
        #[unsafe(method(PDFViewWillClickOnLink:withURL:))]
        unsafe fn click_link(&self, _sender: &PDFView, url: &NSURL) {
            // Implementing this method makes PDFKit leave the URL to us. The hook decides; with no
            // hook (or a refusing one) the click does nothing.
            if let Some(url) = url.absoluteString() {
                if let Some(hooks) = LINK_HOOKS.get() {
                    let text = url.to_string();
                    let _ = hooks.open_link(text.trim_start());
                }
            }
        }
    }
);

struct Record {
    view: usize,
    document: usize,
    thumbnail: usize,
    monitor: usize,
    delegate: usize,
    /// A retained reference to the webview, kept to give it focus back.
    webview: usize,
    fit: Option<String>,
    rotation: i32,
    panel: Option<String>,
    base_rotations: Option<Vec<i32>>,
    outline: Vec<HostOutlineItem>,
    parked: bool,
}

struct Inner {
    records: Mutex<HashMap<u64, Record>>,
    next_id: AtomicU64,
    hooks: HostHooks,
    rail: f64,
}

pub(super) struct PdfKitBackend {
    inner: Arc<Inner>,
}

impl PdfKitBackend {
    pub(super) fn new(config: &HostConfig, hooks: HostHooks) -> Result<Self, HostError> {
        if CREATED.swap(true, Ordering::SeqCst) {
            return Err(HostError::Native(
                "a PDFKit host already exists in this process".into(),
            ));
        }
        let _ = LINK_HOOKS.set(hooks.clone());
        Ok(Self {
            inner: Arc::new(Inner {
                records: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
                hooks,
                rail: config.thumbnail_rail,
            }),
        })
    }
}

fn main_thread() -> Result<MainThreadMarker, HostError> {
    MainThreadMarker::new().ok_or(HostError::NotMainThread)
}

fn lock(inner: &Inner) -> MutexGuard<'_, HashMap<u64, Record>> {
    inner.records.lock().unwrap_or_else(PoisonError::into_inner)
}

unsafe fn take<T: Message>(pointer: usize) -> Option<Retained<T>> {
    Retained::from_raw(pointer as *mut T)
}

unsafe fn borrow<T: Message>(pointer: usize) -> Option<Retained<T>> {
    Retained::retain(pointer as *mut T)
}

fn put<T: Message>(object: Retained<T>) -> usize {
    Retained::into_raw(object) as usize
}

/// Read the parent view's geometry from the platform's webview and window pointers.
pub(super) unsafe fn native_parent(
    webview: *mut c_void,
    ns_window: *mut c_void,
) -> Result<NativeParent, HostError> {
    main_thread()?;
    if webview.is_null() {
        return Err(HostError::invalid("there is no native webview"));
    }
    let view = unsafe { &*(webview as *mut NSView) };
    let frame = view.frame();
    let parent = unsafe { view.superview() }
        .ok_or_else(|| HostError::Native("the webview has no superview".into()))?;
    let window_number = if ns_window.is_null() {
        0
    } else {
        unsafe { (&*(ns_window as *const NSWindow)).windowNumber() }
    };
    Ok(NativeParent {
        webview: webview as usize,
        superview: (&*parent) as *const NSView as usize,
        window_number,
        frame: HostFrame {
            x: frame.origin.x,
            y: frame.origin.y,
            width: frame.size.width,
            height: frame.size.height,
        },
        _not_send: PhantomData,
    })
}

fn webview_rect(frame: HostFrame) -> NSRect {
    NSRect::new(
        NSPoint::new(frame.x, frame.y),
        NSSize::new(frame.width, frame.height),
    )
}

/// CSS pixels from the webview's top-left to an AppKit rect in the superview's bottom-left space.
fn native_frame(frame: HostFrame, webview_frame: NSRect) -> NSRect {
    let width = frame.width.max(1.0);
    let height = frame.height.max(1.0);
    NSRect::new(
        NSPoint::new(
            webview_frame.origin.x + frame.x,
            webview_frame.origin.y + webview_frame.size.height - frame.y - height,
        ),
        NSSize::new(width, height),
    )
}

fn string_value(value: Option<Retained<NSString>>) -> String {
    value.map(|text| text.to_string()).unwrap_or_default()
}

fn srgb(rgb: [u8; 3]) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(
        rgb[0] as f64 / 255.0,
        rgb[1] as f64 / 255.0,
        rgb[2] as f64 / 255.0,
        1.0,
    )
}

fn apply_user_rotation(
    document: &PDFDocument,
    rotation: i32,
    base_rotations: &mut Option<Vec<i32>>,
) {
    if rotation == 0 && base_rotations.is_none() {
        return;
    }
    let count = unsafe { document.pageCount() };
    let bases = base_rotations.get_or_insert_with(|| {
        (0..count)
            .filter_map(|index| unsafe { document.pageAtIndex(index) })
            .map(|page| normalized_rotation(unsafe { page.rotation() } as i32))
            .collect()
    });
    for (index, base) in bases.iter().enumerate() {
        if let Some(page) = unsafe { document.pageAtIndex(index) } {
            // PDFPage rotation is in-memory only; the document is never written by this host.
            unsafe { page.setRotation(normalized_rotation(*base + rotation) as isize) };
        }
    }
}

fn fit_scale(view: &PDFView, document: &PDFDocument, fit: &str) -> Option<f64> {
    let page = unsafe { view.currentPage() }.or_else(|| unsafe { document.pageAtIndex(0) })?;
    let bounds = unsafe { page.boundsForBox(PDFDisplayBox::CropBox) };
    let (page_width, page_height) =
        if normalized_rotation(unsafe { page.rotation() } as i32) % 180 == 90 {
            (bounds.size.height, bounds.size.width)
        } else {
            (bounds.size.width, bounds.size.height)
        };
    let frame = view.frame();
    if bounds.size.width <= 0.0
        || bounds.size.height <= 0.0
        || frame.size.width <= 0.0
        || frame.size.height <= 0.0
    {
        return None;
    }
    let width_scale = (frame.size.width - 16.0).max(1.0) / page_width;
    let page_scale = width_scale.min((frame.size.height - 16.0).max(1.0) / page_height);
    let scale = if fit == "width" {
        width_scale
    } else {
        page_scale
    };
    Some(scale.clamp(0.25, 5.0))
}

fn apply_fit(view: &PDFView, document: &PDFDocument, fit: &str) {
    if let Some(scale) = fit_scale(view, document, fit) {
        unsafe {
            view.setAutoScales(false);
            view.setScaleFactor(scale);
        }
    }
}

fn outline_children(
    outline: &PDFOutline,
    document: &PDFDocument,
    depth: usize,
    remaining: &mut usize,
) -> Vec<HostOutlineItem> {
    let mut result = Vec::new();
    if depth >= 32 {
        return result;
    }
    for index in 0..unsafe { outline.numberOfChildren() } {
        if *remaining == 0 {
            break;
        }
        *remaining -= 1;
        let Some(child) = (unsafe { outline.childAtIndex(index) }) else {
            continue;
        };
        let title = string_value(unsafe { child.label() });
        let page = unsafe { child.destination() }
            .and_then(|destination| unsafe { destination.page() })
            .and_then(|page| {
                let index = unsafe { document.indexForPage(&page) };
                (index < unsafe { document.pageCount() }).then(|| index as u32 + 1)
            });
        result.push(HostOutlineItem {
            title,
            page,
            children: outline_children(&child, document, depth + 1, remaining),
        });
    }
    result
}

fn outline_for(document: &PDFDocument) -> Vec<HostOutlineItem> {
    let Some(root) = (unsafe { document.outlineRoot() }) else {
        return Vec::new();
    };
    outline_children(&root, document, 0, &mut 10_000)
}

/// Preserve widgets and internal links. URL clicks use the hook; file-launch, script and
/// remote-document actions are removed in memory.
fn configure_read_only_annotations(document: &PDFDocument) {
    let count = unsafe { document.pageCount() };
    for index in 0..count {
        let Some(page) = (unsafe { document.pageAtIndex(index) }) else {
            continue;
        };
        for annotation in unsafe { page.annotations() }.to_vec() {
            let kind = string_value(unsafe { annotation.r#type() });
            if kind == "Widget" {
                unsafe { annotation.setReadOnly(true) };
            }
            if let Some(action) = unsafe { annotation.action() } {
                let allowed = match unsafe { action.r#type() }.to_string().as_str() {
                    "GoTo" | "URL" | "URI" => true,
                    "Named" => action
                        .downcast_ref::<PDFActionNamed>()
                        .is_some_and(|named| {
                            matches!(
                                unsafe { named.name() },
                                PDFActionNamedName::NextPage
                                    | PDFActionNamedName::PreviousPage
                                    | PDFActionNamedName::FirstPage
                                    | PDFActionNamedName::LastPage
                            )
                        }),
                    _ => false,
                };
                if !allowed {
                    unsafe { annotation.setAction(None) };
                }
            }
        }
    }
}

fn live_objects(record: &Record) -> Result<(Retained<PDFView>, Retained<PDFDocument>), HostError> {
    let view = unsafe { borrow::<PDFView>(record.view) }
        .ok_or_else(|| HostError::Native("the PDFView has already been released".into()))?;
    let document = unsafe { borrow::<PDFDocument>(record.document) }
        .ok_or_else(|| HostError::Native("the PDFDocument has already been released".into()))?;
    Ok((view, document))
}

fn state_for(record: &Record) -> Result<ViewState, HostError> {
    let (view, document) = live_objects(record)?;
    let count = unsafe { document.pageCount() as u32 };
    let page = unsafe { view.currentPage() }
        .map(|current| unsafe { document.indexForPage(&current) as u32 + 1 })
        .unwrap_or(1);
    let scale = unsafe { view.scaleFactor() };
    Ok(ViewState {
        page: clamp_page(page, count),
        num_pages: count,
        scale: if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        },
        fit: record.fit.clone(),
        rotation: record.rotation,
        panel: record.panel.clone(),
        locked: unsafe { document.isLocked() },
        outline: record.outline.clone(),
    })
}

fn dom_key_code(code: u16) -> String {
    match code {
        0 => "KeyA",
        1 => "KeyS",
        2 => "KeyD",
        3 => "KeyF",
        4 => "KeyH",
        5 => "KeyG",
        6 => "KeyZ",
        7 => "KeyX",
        8 => "KeyC",
        9 => "KeyV",
        11 => "KeyB",
        12 => "KeyQ",
        13 => "KeyW",
        14 => "KeyE",
        15 => "KeyR",
        16 => "KeyY",
        17 => "KeyT",
        18 => "Digit1",
        19 => "Digit2",
        20 => "Digit3",
        21 => "Digit4",
        22 => "Digit6",
        23 => "Digit5",
        24 => "Equal",
        27 => "Minus",
        28 => "Digit8",
        25 => "Digit9",
        26 => "Digit7",
        29 => "Digit0",
        30 => "BracketRight",
        31 => "KeyO",
        32 => "KeyU",
        36 => "Enter",
        48 => "Tab",
        49 => "Space",
        51 => "Backspace",
        53 => "Escape",
        96 => "F5",
        99 => "F3",
        111 => "F12",
        115 => "Home",
        116 => "PageUp",
        117 => "Delete",
        119 => "End",
        121 => "PageDown",
        123 => "ArrowLeft",
        124 => "ArrowRight",
        125 => "ArrowDown",
        126 => "ArrowUp",
        value => return value.to_string(),
    }
    .to_string()
}

/// Forward the key presses the app claims (through [`HostHooks`]) while this view has focus, and
/// swallow them so the view does not act on them too. The monitor belongs to one view and is
/// removed when the view is parked or released.
fn install_key_monitor(
    inner: &Arc<Inner>,
    id: u64,
    window: String,
    owner: String,
    window_number: isize,
) -> usize {
    let inner = Arc::clone(inner);
    let block: RcBlock<dyn Fn(NonNull<NSEvent>) -> *mut NSEvent> =
        RcBlock::new(move |event: NonNull<NSEvent>| {
            let Some(mtm) = MainThreadMarker::new() else {
                return event.as_ptr();
            };
            let event_ref = unsafe { event.as_ref() };
            let Some(event_window) = event_ref.window(mtm) else {
                return event.as_ptr();
            };
            if event_window.windowNumber() != window_number {
                return event.as_ptr();
            }
            let flags = event_ref.modifierFlags();
            let code = event_ref.keyCode();
            let key = match code {
                48 => "Tab".to_string(),
                53 => "Escape".to_string(),
                99 => "F3".to_string(),
                96 => "F5".to_string(),
                111 => "F12".to_string(),
                _ => event_ref
                    .charactersIgnoringModifiers()
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            };
            let payload = HostKey {
                owner: owner.clone(),
                key,
                code: dom_key_code(code),
                meta_key: flags.contains(NSEventModifierFlags::Command),
                ctrl_key: flags.contains(NSEventModifierFlags::Control),
                alt_key: flags.contains(NSEventModifierFlags::Option),
                shift_key: flags.contains(NSEventModifierFlags::Shift),
            };
            if !inner.hooks.claim_key(&payload) {
                return event.as_ptr();
            }
            let current = {
                let table = lock(&inner);
                table.get(&id).is_some_and(|record| {
                    if record.parked {
                        return false;
                    }
                    let view = unsafe { borrow::<PDFView>(record.view) };
                    let responder = event_window.firstResponder();
                    match (view, responder) {
                        (Some(view), Some(responder)) => responder
                            .downcast_ref::<NSView>()
                            .is_some_and(|responder| responder.isDescendantOf(&view)),
                        _ => false,
                    }
                })
            };
            if current {
                inner.hooks.deliver_key(&window, payload);
                std::ptr::null_mut()
            } else {
                event.as_ptr()
            }
        });
    unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &block)
            .map(put)
            .unwrap_or(0)
    }
}

/// Release the native objects of a record. `restore_focus` gives the webview focus back when the
/// PDF view had it.
unsafe fn release_record(record: Record, restore_focus: bool) {
    if record.monitor != 0 {
        if let Some(monitor) = take::<objc2::runtime::AnyObject>(record.monitor) {
            NSEvent::removeMonitor(&monitor);
        }
    }
    if let Some(view) = take::<PDFView>(record.view) {
        if let Some(window) = view.window().filter(|_| restore_focus) {
            let focused = window
                .firstResponder()
                .and_then(|responder| {
                    responder
                        .downcast_ref::<NSView>()
                        .map(|view| view as *const NSView)
                })
                .is_some_and(|responder| unsafe { (&*responder).isDescendantOf(&view) });
            if focused {
                if let Some(webview) = borrow::<NSView>(record.webview) {
                    window.makeFirstResponder(Some(&webview));
                }
            }
        }
        view.setDelegate(None);
        view.setDocument(None);
        view.removeFromSuperview();
    }
    if record.thumbnail != 0 {
        if let Some(thumbnail) = take::<PDFThumbnailView>(record.thumbnail) {
            thumbnail.setPDFView(None);
            thumbnail.removeFromSuperview();
        }
    }
    let _ = take::<PDFDocument>(record.document);
    let _ = take::<ReaderDelegate>(record.delegate);
    let _ = take::<NSView>(record.webview);
}

fn utf16_boundary_is_word(text: &str, location: usize, length: usize) -> bool {
    let mut before = None;
    let mut after = None;
    let mut offset = 0usize;
    for character in text.chars() {
        let end = offset + character.len_utf16();
        if end == location {
            before = Some(character);
        }
        if offset == location + length {
            after = Some(character);
            break;
        }
        offset = end;
    }
    !before.is_some_and(|character| character.is_alphanumeric() || character == '_')
        && !after.is_some_and(|character| character.is_alphanumeric() || character == '_')
}

fn selection_is_whole_word(selection: &PDFSelection) -> bool {
    let Some(page) = unsafe { selection.pages() }.firstObject() else {
        return true;
    };
    let Some(text) = (unsafe { page.string() }) else {
        return true;
    };
    let range = unsafe { selection.rangeAtIndex_onPage(0, &page) };
    utf16_boundary_is_word(&text.to_string(), range.location, range.length)
}

impl HostBackend for PdfKitBackend {
    fn supported(&self) -> bool {
        true
    }

    fn open(&self, request: &BackendOpen<'_>) -> Result<(ViewId, ViewState), HostError> {
        let mtm = main_thread()?;
        if request.parent.is_detached() {
            return Err(HostError::invalid("there is no native parent view"));
        }
        let webview = unsafe { borrow::<NSView>(request.parent.webview) }
            .ok_or_else(|| HostError::Native("the webview was released".into()))?;
        let parent = unsafe { borrow::<NSView>(request.parent.superview) }
            .ok_or_else(|| HostError::Native("the webview superview is unavailable".into()))?;

        let mut base_rotations = None;
        let mut panel = None;
        let reused = match request.reuse {
            Some(ViewId(old)) => {
                let record = lock(&self.inner)
                    .remove(&old)
                    .filter(|record| record.parked)
                    .ok_or_else(|| {
                        HostError::Native("the cached PDF view is not available".into())
                    })?;
                base_rotations = record.base_rotations.clone();
                panel = record.panel.clone();
                let objects = unsafe {
                    (
                        take::<PDFDocument>(record.document),
                        take::<PDFView>(record.view),
                        take::<ReaderDelegate>(record.delegate),
                    )
                };
                // The new webview reference replaces the cached one.
                let _ = unsafe { take::<NSView>(record.webview) };
                match objects {
                    (Some(document), Some(view), Some(delegate)) => {
                        Some((document, view, delegate))
                    }
                    _ => {
                        return Err(HostError::Native(
                            "the cached PDF view was incomplete".into(),
                        ))
                    }
                }
            }
            None => None,
        };
        let reused_reader = reused.is_some();
        let (document, view, delegate) = match reused {
            Some(objects) => objects,
            None => {
                let url = NSURL::fileURLWithPath(&NSString::from_str(request.path));
                let document = unsafe { PDFDocument::initWithURL(PDFDocument::alloc(), &url) }
                    .ok_or_else(|| HostError::Load("PDFKit could not load the file".into()))?;
                if unsafe { document.isLocked() } {
                    if let Some(password) = request.password {
                        let password = NSString::from_str(password);
                        unsafe { document.unlockWithPassword(&password) };
                    }
                }
                let view = unsafe { PDFView::new(mtm) };
                let delegate: Retained<ReaderDelegate> =
                    unsafe { msg_send![ReaderDelegate::alloc(mtm), init] };
                unsafe {
                    view.setDelegate(Some(objc2::runtime::ProtocolObject::from_ref(&*delegate)))
                };
                unsafe {
                    view.setDocument(Some(&document));
                    view.setDisplayMode(PDFDisplayMode::SinglePageContinuous);
                    view.setDisplayDirection(PDFDisplayDirection::Vertical);
                    view.setAutoScales(true);
                    view.enablePageShadows(false);
                    view.setDisplaysPageBreaks(true);
                }
                configure_read_only_annotations(&document);
                (document, view, delegate)
            }
        };

        let webview_frame = webview_rect(request.parent.frame);
        // Set PDFKit's canvas before attaching it to the webview, reused views included.
        if let Some(rgb) = request.reader_rgb {
            let color = srgb(rgb);
            unsafe { view.setBackgroundColor(&color) };
        }
        view.setFrame(native_frame(request.frame, webview_frame));
        view.setHidden(false);
        parent.addSubview(&view);

        let mut fit = None;
        let mut rotation = 0;
        if let Some(meta) = request.initial_view {
            if let Some(page) = meta.page {
                let count = unsafe { document.pageCount() as u32 };
                if let Some(page) =
                    unsafe { document.pageAtIndex((clamp_page(page, count) - 1) as usize) }
                {
                    unsafe { view.goToPage(&page) };
                }
            }
            if let Some(value) = meta.scale.filter(|value| value.is_finite() && *value > 0.0) {
                unsafe {
                    view.setAutoScales(false);
                    view.setScaleFactor(value);
                }
            }
            if let Some(value) = meta.fit.as_deref() {
                if value == "width" || value == "page" {
                    fit = Some(value.to_owned());
                    unsafe { view.setAutoScales(true) };
                }
            }
            rotation = normalized_rotation(meta.rotation.unwrap_or(0));
        }
        // A reused view may carry an earlier rotation; base rotations restore it to 0 too.
        if rotation != 0 || base_rotations.is_some() {
            apply_user_rotation(&document, rotation, &mut base_rotations);
        }
        if let Some(fit) = fit.as_deref() {
            apply_fit(&view, &document, fit);
        }

        let outline = outline_for(&document);
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let monitor = install_key_monitor(
            &self.inner,
            id,
            request.window.to_owned(),
            request.owner.to_owned(),
            request.parent.window_number,
        );
        let record = Record {
            view: put(view),
            document: put(document),
            thumbnail: 0,
            monitor,
            delegate: put(delegate),
            webview: put(webview),
            fit,
            rotation,
            panel: if reused_reader { panel.take() } else { None },
            base_rotations,
            outline,
            parked: false,
        };
        match state_for(&record) {
            Ok(state) => {
                lock(&self.inner).insert(id, record);
                Ok((ViewId(id), state))
            }
            Err(error) => {
                unsafe { release_record(record, true) };
                Err(error)
            }
        }
    }

    fn update(
        &self,
        view: ViewId,
        parent: &NativeParent,
        request: &UpdateRequest,
    ) -> Result<ViewState, HostError> {
        let mtm = main_thread()?;
        if parent.is_detached() {
            return Err(HostError::invalid("there is no native parent view"));
        }
        let mut table = lock(&self.inner);
        let record = table
            .get_mut(&view.0)
            .filter(|record| !record.parked)
            .ok_or(HostError::NotOpen)?;
        let (pdf_view, document) = live_objects(record)?;
        if let Some(rgb) = request.reader_rgb {
            let color = srgb(rgb);
            unsafe { pdf_view.setBackgroundColor(&color) };
        }
        let mut supplied_frame = false;
        if let Some(frame) = request.frame {
            pdf_view.setFrame(native_frame(frame, webview_rect(parent.frame)));
            supplied_frame = true;
        }
        if let Some(page) = request.page {
            let count = unsafe { document.pageCount() as u32 };
            if let Some(page) =
                unsafe { document.pageAtIndex((clamp_page(page, count) - 1) as usize) }
            {
                unsafe { pdf_view.goToPage(&page) };
            }
        }
        if supplied_frame {
            if let Some(fit) = record.fit.as_deref() {
                apply_fit(&pdf_view, &document, fit);
            }
        }
        if let Some(value) = request
            .scale
            .filter(|value| value.is_finite() && *value > 0.0)
        {
            unsafe {
                pdf_view.setAutoScales(false);
                pdf_view.setScaleFactor(value);
            }
            record.fit = None;
        }
        if let Some(value) = request.fit.as_deref() {
            if value.is_empty() {
                unsafe { pdf_view.setAutoScales(false) };
                record.fit = None;
            } else if value == "width" || value == "page" {
                record.fit = Some(value.to_owned());
                apply_fit(&pdf_view, &document, value);
            }
        }
        if let Some(rotation) = request.rotation {
            record.rotation = normalized_rotation(rotation);
            apply_user_rotation(&document, record.rotation, &mut record.base_rotations);
        }
        let previous_panel = record.panel.clone();
        if let Some(panel) = request.panel.as_deref() {
            record.panel = match panel {
                "" | "none" => None,
                // The outline is drawn by the page; the thumbnail rail is the only native panel.
                "outline" | "thumbs" => Some(panel.to_owned()),
                _ => return Err(HostError::invalid("unknown native PDF panel")),
            };
        }
        if record.panel.as_deref() == Some("thumbs") && (record.thumbnail == 0 || supplied_frame) {
            let superview = unsafe { borrow::<NSView>(parent.superview) }
                .ok_or_else(|| HostError::Native("the thumbnail panel has no superview".into()))?;
            if record.thumbnail == 0 {
                let thumbnail = unsafe { PDFThumbnailView::new(mtm) };
                unsafe {
                    thumbnail.setPDFView(Some(&pdf_view));
                    superview.addSubview(&thumbnail);
                }
                record.thumbnail = put(thumbnail);
            }
            let view_frame = pdf_view.frame();
            // The page already reserves the rail to the left of the PDF view.
            let rail = self.inner.rail;
            let thumbnail_frame = NSRect::new(
                NSPoint::new(view_frame.origin.x - rail, view_frame.origin.y),
                NSSize::new(rail, view_frame.size.height),
            );
            if let Some(thumbnail) = unsafe { borrow::<PDFThumbnailView>(record.thumbnail) } {
                thumbnail.setFrame(thumbnail_frame);
            }
        } else if record.panel.is_none() && previous_panel.as_deref() == Some("thumbs") {
            if let Some(thumbnail) = unsafe { take::<PDFThumbnailView>(record.thumbnail) } {
                unsafe { thumbnail.setPDFView(None) };
                thumbnail.removeFromSuperview();
                record.thumbnail = 0;
            }
        }
        if let Some(fit) = record.fit.as_deref() {
            apply_fit(&pdf_view, &document, fit);
        }
        if let Some(visible) = request.visible {
            pdf_view.setHidden(!visible);
            if let Some(thumbnail) = unsafe { borrow::<PDFThumbnailView>(record.thumbnail) } {
                thumbnail.setHidden(!visible);
            }
        }
        state_for(record)
    }

    fn state(&self, view: ViewId) -> Result<ViewState, HostError> {
        main_thread()?;
        let table = lock(&self.inner);
        let record = table
            .get(&view.0)
            .filter(|record| !record.parked)
            .ok_or(HostError::NotOpen)?;
        state_for(record)
    }

    fn find(
        &self,
        view: ViewId,
        query: &FindQuery<'_>,
        pick: &dyn Fn(usize) -> usize,
    ) -> Result<(usize, Option<usize>), HostError> {
        main_thread()?;
        let table = lock(&self.inner);
        let record = table
            .get(&view.0)
            .filter(|record| !record.parked)
            .ok_or(HostError::NotOpen)?;
        let (pdf_view, document) = live_objects(record)?;
        let options = if query.match_case {
            NSStringCompareOptions::LiteralSearch
        } else {
            NSStringCompareOptions::LiteralSearch | NSStringCompareOptions::CaseInsensitiveSearch
        };
        let needle = NSString::from_str(query.text);
        let mut hits = Vec::<Retained<PDFSelection>>::new();
        for selection in unsafe { document.findString_withOptions(&needle, options) }.to_vec() {
            if !query.whole_word || selection_is_whole_word(&selection) {
                hits.push(selection);
            }
        }
        let count = hits.len();
        if count == 0 {
            unsafe { pdf_view.setHighlightedSelections(None) };
            return Ok((0, None));
        }
        let active = pick(count).min(count - 1);
        let all = NSArray::from_retained_slice(&hits);
        unsafe {
            pdf_view.setHighlightedSelections(Some(&all));
            pdf_view.setCurrentSelection(Some(&hits[active]));
            pdf_view.goToSelection(&hits[active]);
            pdf_view.scrollSelectionToVisible(None);
        }
        Ok((count, Some(active)))
    }

    fn clear_find(&self, view: ViewId) -> Result<(), HostError> {
        main_thread()?;
        let table = lock(&self.inner);
        let record = table
            .get(&view.0)
            .filter(|record| !record.parked)
            .ok_or(HostError::NotOpen)?;
        let (pdf_view, _document) = live_objects(record)?;
        unsafe {
            pdf_view.clearSelection();
            pdf_view.setHighlightedSelections(None);
        }
        Ok(())
    }

    fn print(&self, view: ViewId) -> Result<bool, HostError> {
        let mtm = main_thread()?;
        // The print panel runs a modal loop: take what it needs and release the table first.
        let document = {
            let table = lock(&self.inner);
            let record = table
                .get(&view.0)
                .filter(|record| !record.parked)
                .ok_or(HostError::NotOpen)?;
            let (_view, document) = live_objects(record)?;
            document
        };
        if unsafe { document.isLocked() } || !unsafe { document.allowsPrinting() } {
            return Ok(false);
        }
        let print_info = NSPrintInfo::sharedPrintInfo();
        let operation = unsafe {
            document.printOperationForPrintInfo_scalingMode_autoRotate(
                Some(&print_info),
                objc2_pdf_kit::PDFPrintScalingMode::PageScaleToFit,
                true,
                mtm,
            )
        }
        .ok_or_else(|| HostError::Native("PDFKit could not create a print operation".into()))?;
        operation.setShowsPrintPanel(true);
        operation.setShowsProgressPanel(true);
        Ok(operation.runOperation())
    }

    fn is_encrypted(&self, view: ViewId) -> bool {
        if main_thread().is_err() {
            // Fail closed: an unknown document is never parked.
            return true;
        }
        let table = lock(&self.inner);
        table
            .get(&view.0)
            .and_then(|record| unsafe { borrow::<PDFDocument>(record.document) })
            .map(|document| unsafe { document.isEncrypted() })
            .unwrap_or(true)
    }

    fn park(&self, view: ViewId) -> Result<(), HostError> {
        main_thread()?;
        let mut table = lock(&self.inner);
        let record = table
            .get_mut(&view.0)
            .filter(|record| !record.parked)
            .ok_or(HostError::NotOpen)?;
        if record.monitor != 0 {
            if let Some(monitor) = unsafe { take::<objc2::runtime::AnyObject>(record.monitor) } {
                unsafe { NSEvent::removeMonitor(&monitor) };
            }
            record.monitor = 0;
        }
        if record.thumbnail != 0 {
            if let Some(thumbnail) = unsafe { take::<PDFThumbnailView>(record.thumbnail) } {
                unsafe { thumbnail.setPDFView(None) };
                thumbnail.removeFromSuperview();
            }
            record.thumbnail = 0;
        }
        if let Some(pdf_view) = unsafe { borrow::<PDFView>(record.view) } {
            if let Some(window) = pdf_view.window() {
                let focused = window
                    .firstResponder()
                    .and_then(|responder| {
                        responder
                            .downcast_ref::<NSView>()
                            .map(|view| view as *const NSView)
                    })
                    .is_some_and(|responder| unsafe { (&*responder).isDescendantOf(&pdf_view) });
                if focused {
                    if let Some(webview) = unsafe { borrow::<NSView>(record.webview) } {
                        window.makeFirstResponder(Some(&webview));
                    }
                }
            }
            unsafe { pdf_view.clearSelection() };
            pdf_view.setHidden(true);
        }
        record.parked = true;
        Ok(())
    }

    fn release(&self, view: ViewId, restore_focus: bool) {
        if main_thread().is_err() {
            // Objects must be released on the main thread. The host's callers dispatch there; a
            // call from elsewhere leaves the record in place for a later release rather than
            // touching AppKit off-thread.
            return;
        }
        let removed = lock(&self.inner).remove(&view.0);
        if let Some(record) = removed {
            unsafe { release_record(record, restore_focus) };
        }
    }
}
