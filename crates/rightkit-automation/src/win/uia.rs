//! UI Automation: element reads, semantic actions, text proof reads. UIA is
//! initialized in the multithreaded apartment, so interface pointers may be used
//! from any thread that has called `ensure_com`.

use crate::model::*;
use crate::proof::{FieldText, TextSource, MAX_PROOF_BYTES};
use crate::uia_text::{
    classify_read, editable_decision, get_text_request_len, is_terminal_edit_name, truncate_utf16,
    FocusedFieldText, RowPush, TerminalLimits, TerminalRowCollector, TerminalRows, TextRead,
};
use std::cell::Cell;
use std::mem::ManuallyDrop;
use std::sync::OnceLock;
use std::time::Instant;
use windows::core::{IUnknown, Interface, BSTR};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::System::Ole::{
    SafeArrayAccessData, SafeArrayDestroy, SafeArrayGetDim, SafeArrayGetLBound, SafeArrayGetUBound,
    SafeArrayUnaccessData,
};
use windows::Win32::System::Variant::{VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_I4};
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::WindowsAndMessaging::{GetAncestor, GetForegroundWindow, GA_ROOT};

struct Shared<T>(T);
// SAFETY: UIA objects are created in the MTA and are free-threaded.
unsafe impl<T> Send for Shared<T> {}
unsafe impl<T> Sync for Shared<T> {}

thread_local! {
    static COM_READY: Cell<bool> = const { Cell::new(false) };
}

/// Join the multithreaded apartment on this thread (idempotent).
pub fn ensure_com() {
    COM_READY.with(|c| {
        if !c.get() {
            // S_FALSE / RPC_E_CHANGED_MODE are fine: COM is usable either way.
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            c.set(true);
        }
    });
}

fn automation() -> Option<&'static IUIAutomation> {
    static AUTO: OnceLock<Option<Shared<IUIAutomation>>> = OnceLock::new();
    ensure_com();
    AUTO.get_or_init(|| unsafe {
        CoCreateInstance::<Option<&IUnknown>, IUIAutomation>(
            &CUIAutomation8,
            None,
            CLSCTX_INPROC_SERVER,
        )
        .or_else(|_| {
            CoCreateInstance::<Option<&IUnknown>, IUIAutomation>(
                &CUIAutomation,
                None,
                CLSCTX_INPROC_SERVER,
            )
        })
        .ok()
        .map(Shared)
    })
    .as_ref()
    .map(|s| &s.0)
}

/// An opaque UI Automation element. The native interface pointer is private to the crate, so no
/// `windows` crate type appears in the public surface; keep and move an `El` between threads
/// (it is `Send + Sync`) and use the methods here to read it.
#[derive(Clone)]
pub struct El(pub(crate) IUIAutomationElement);
// SAFETY: see `Shared`.
unsafe impl Send for El {}
unsafe impl Sync for El {}

fn win_err(e: windows::core::Error, op: &'static str) -> Error {
    let code = e.code().0;
    match code as u32 {
        // UIA_E_NOTSUPPORTED, UIA_E_INVALIDOPERATION, E_NOTIMPL
        0x8004_0201 | 0x8013_1509 | 0x8000_4001 => Error::Unsupported(op),
        // UIA_E_ELEMENTNOTAVAILABLE, UIA_E_ELEMENTNOTENABLED
        0x8004_0200 | 0x8004_0206 => Error::NotFound(format!("element unavailable during {op}")),
        // E_ACCESSDENIED: UIPI, the target is elevated and we are not
        0x8007_0005 => Error::PermissionDenied("target is elevated (UIPI)"),
        _ => Error::Platform(i64::from(code), op),
    }
}

pub fn element_from_hwnd(hwnd: isize) -> Result<El> {
    let a = automation().ok_or(Error::Unsupported("UI Automation unavailable"))?;
    unsafe { a.ElementFromHandle(HWND(hwnd as *mut _)) }
        .map(El)
        .map_err(|e| win_err(e, "ElementFromHandle"))
}

fn control_name(id: i32) -> &'static str {
    const NAMES: [&str; 41] = [
        "Button",
        "Calendar",
        "CheckBox",
        "ComboBox",
        "Edit",
        "Hyperlink",
        "Image",
        "ListItem",
        "List",
        "Menu",
        "MenuBar",
        "MenuItem",
        "ProgressBar",
        "RadioButton",
        "ScrollBar",
        "Slider",
        "Spinner",
        "StatusBar",
        "Tab",
        "TabItem",
        "Text",
        "ToolBar",
        "ToolTip",
        "Tree",
        "TreeItem",
        "Custom",
        "Group",
        "Thumb",
        "DataGrid",
        "DataItem",
        "Document",
        "SplitButton",
        "Window",
        "Pane",
        "Header",
        "HeaderItem",
        "Table",
        "TitleBar",
        "Separator",
        "SemanticZoom",
        "AppBar",
    ];
    NAMES
        .get((id - 50000) as usize)
        .copied()
        .unwrap_or("Unknown")
}

fn normalize_role(native: &str) -> &'static str {
    match native {
        "Button" | "SplitButton" => "button",
        "Edit" | "ComboBox" => "textfield",
        "Document" => "textarea",
        "Text" => "text",
        "Window" => "window",
        "CheckBox" => "checkbox",
        "RadioButton" => "radio",
        "MenuItem" => "menuitem",
        "Menu" | "MenuBar" => "menu",
        "Group" | "Pane" | "Custom" => "group",
        "Image" => "image",
        "List" | "Tree" | "Table" | "DataGrid" => "list",
        "ListItem" | "TreeItem" | "DataItem" => "row",
        "ToolBar" => "toolbar",
        "Slider" => "slider",
        "Tab" => "tabs",
        "Hyperlink" => "link",
        _ => "other",
    }
}

fn b(v: windows::core::Result<windows::core::BOOL>) -> Option<bool> {
    v.ok().map(|x| x.as_bool())
}
fn s(v: windows::core::Result<BSTR>) -> Option<String> {
    v.ok().map(|x| x.to_string()).filter(|x| !x.is_empty())
}

impl El {
    fn pattern<T: Interface>(&self, id: UIA_PATTERN_ID) -> Option<T> {
        unsafe { self.0.GetCurrentPatternAs::<T>(id) }.ok()
    }
    /// `IsPassword`, or `None` when UIA cannot read it (unlike `is_password`, which hides that).
    pub fn password_state(&self) -> Option<bool> {
        b(unsafe { self.0.CurrentIsPassword() })
    }
    pub fn is_password(&self) -> bool {
        b(unsafe { self.0.CurrentIsPassword() }).unwrap_or(false)
    }
    pub fn pid(&self) -> Option<i32> {
        unsafe { self.0.CurrentProcessId() }.ok()
    }
    pub fn rect(&self) -> Option<Rect> {
        let r: RECT = unsafe { self.0.CurrentBoundingRectangle() }.ok()?;
        (r.right > r.left && r.bottom > r.top).then(|| Rect {
            x: f64::from(r.left),
            y: f64::from(r.top),
            w: f64::from(r.right - r.left),
            h: f64::from(r.bottom - r.top),
        })
    }
    pub fn children(&self) -> Vec<El> {
        let Some(a) = automation() else { return vec![] };
        unsafe {
            let Ok(cond) = a.ControlViewCondition() else {
                return vec![];
            };
            let Ok(arr) = self.0.FindAll(TreeScope_Children, &cond) else {
                return vec![];
            };
            let n = arr.Length().unwrap_or(0);
            (0..n)
                .filter_map(|i| arr.GetElement(i).ok())
                .map(El)
                .collect()
        }
    }
    pub fn parent(&self) -> Option<El> {
        let a = automation()?;
        unsafe { a.ControlViewWalker().ok()?.GetParentElement(&self.0).ok() }.map(El)
    }
    pub fn has_focus(&self) -> bool {
        b(unsafe { self.0.CurrentHasKeyboardFocus() }).unwrap_or(false)
    }
    pub fn native_hwnd(&self) -> Option<isize> {
        let h = unsafe { self.0.CurrentNativeWindowHandle() }.ok()?;
        (!h.0.is_null()).then_some(h.0 as isize)
    }
    /// Top-level window handle that owns this element.
    pub fn top_hwnd(&self) -> Option<isize> {
        let mut cur = Some(self.clone());
        for _ in 0..64 {
            let e = cur?;
            if let Some(h) = e.native_hwnd() {
                let root = unsafe { GetAncestor(HWND(h as *mut _), GA_ROOT) };
                return Some(if root.0.is_null() { h } else { root.0 as isize });
            }
            cur = e.parent();
        }
        None
    }
    pub fn value_text(&self) -> Option<String> {
        let v: IUIAutomationValuePattern = self.pattern(UIA_ValuePatternId)?;
        unsafe { v.CurrentValue() }.ok().map(|x| x.to_string())
    }
}

/// Bounded, safety-first reads and guarded writes on an element. Every method here is
/// synchronous; wrap the call in [`super::bounded::with_deadline`] (or the `_within` helpers)
/// when the target may hang.
impl El {
    /// Whether the element is keyboard focusable, or `None` when UIA cannot say.
    pub fn is_keyboard_focusable(&self) -> Option<bool> {
        b(unsafe { self.0.CurrentIsKeyboardFocusable() })
    }

    /// The UIA runtime id (`UIA_RuntimeIdPropertyId`), which identifies this element for as long
    /// as it lives. `None` when the provider returns none or it cannot be read.
    pub fn runtime_id(&self) -> Option<Vec<i32>> {
        unsafe {
            let array = self.0.GetRuntimeId().ok()?;
            if array.is_null() {
                return None;
            }
            let result = (|| {
                if SafeArrayGetDim(array) != 1 {
                    return None;
                }
                let lower = SafeArrayGetLBound(array, 1).ok()?;
                let upper = SafeArrayGetUBound(array, 1).ok()?;
                if upper < lower {
                    return None;
                }
                let count = (upper - lower + 1) as usize;
                let mut data = std::ptr::null_mut();
                SafeArrayAccessData(array, &mut data).ok()?;
                let values = if data.is_null() {
                    None
                } else {
                    Some(std::slice::from_raw_parts(data as *const i32, count).to_vec())
                };
                let _ = SafeArrayUnaccessData(array);
                values
            })();
            let _ = SafeArrayDestroy(array);
            result
        }
    }

    /// First descendant whose native window handle is `hwnd` (a subtree search).
    pub fn find_descendant_by_native_window_handle(&self, hwnd: isize) -> Option<El> {
        let a = automation()?;
        unsafe {
            let cond = a
                .CreatePropertyCondition(
                    UIA_NativeWindowHandlePropertyId,
                    &variant_i32(hwnd as i32),
                )
                .ok()?;
            self.0.FindFirst(TreeScope_Descendants, &cond).ok().map(El)
        }
    }

    /// Whether this is an editable text target. An explicit `ValuePattern` read-only flag always
    /// makes it not editable. Otherwise it is editable when it is an Edit or Document control that
    /// is keyboard focusable (a control UIA cannot report as focusable counts as not focusable),
    /// or when it has a writable `ValuePattern` or a `TextEditPattern` (the last is what Chromium
    /// contenteditable composers expose). `None` when the control type or a present
    /// `ValuePattern`'s read-only flag cannot be read. A password field still counts as editable;
    /// check [`El::password_state`] separately.
    pub fn is_editable(&self) -> Option<bool> {
        let ct = unsafe { self.0.CurrentControlType() }.ok()?;
        let text_control = ct == UIA_EditControlTypeId || ct == UIA_DocumentControlTypeId;
        let keyboard_focusable = self.is_keyboard_focusable().unwrap_or(false);
        let value_read_only = match self.pattern::<IUIAutomationValuePattern>(UIA_ValuePatternId) {
            Some(v) => Some(b(unsafe { v.CurrentIsReadOnly() })?),
            None => None,
        };
        let has_text_edit = self
            .pattern::<IUIAutomationTextEditPattern>(UIA_TextEditPatternId)
            .is_some();
        Some(editable_decision(
            text_control,
            keyboard_focusable,
            value_read_only,
            has_text_edit,
        ))
    }

    /// Whether the `ValuePattern` value is empty. `None` when there is no `ValuePattern` or the
    /// read fails. Does not check for a password field.
    pub fn value_is_empty(&self) -> Option<bool> {
        let v = self.pattern::<IUIAutomationValuePattern>(UIA_ValuePatternId)?;
        unsafe { v.CurrentValue() }.ok().map(|x| x.is_empty())
    }

    /// `ValuePattern` text of at most `limit_utf16` UTF-16 units, keeping "no pattern or read
    /// failed" ([`TextRead::Absent`]) apart from "over the limit" ([`TextRead::TooLarge`]).
    /// Fails closed to `Absent` unless UIA positively reports the element is not a password field.
    pub fn read_value_checked(&self, limit_utf16: usize) -> TextRead {
        if self.password_state() != Some(false) {
            return TextRead::Absent;
        }
        let Some(v) = self.pattern::<IUIAutomationValuePattern>(UIA_ValuePatternId) else {
            return TextRead::Absent;
        };
        match unsafe { v.CurrentValue() } {
            Ok(text) => classify_read(text.len(), limit_utf16, || text.to_string()),
            Err(_) => TextRead::Absent,
        }
    }

    /// [`El::read_value_checked`] as an `Option`: `None` when absent or over the limit.
    pub fn read_value(&self, limit_utf16: usize) -> Option<String> {
        self.read_value_checked(limit_utf16).into_option()
    }

    /// `TextPattern` document text of at most `limit_utf16` UTF-16 units. The read asks UIA for
    /// one unit more than the limit so an oversized document is reported as
    /// [`TextRead::TooLarge`] and never copied. Same password rule as [`El::read_value_checked`].
    pub fn read_document_text_checked(&self, limit_utf16: usize) -> TextRead {
        if self.password_state() != Some(false) {
            return TextRead::Absent;
        }
        let Some(tp) = self.pattern::<IUIAutomationTextPattern>(UIA_TextPatternId) else {
            return TextRead::Absent;
        };
        let text = unsafe {
            tp.DocumentRange()
                .and_then(|r| r.GetText(get_text_request_len(limit_utf16)))
        };
        match text {
            Ok(text) => classify_read(text.len(), limit_utf16, || text.to_string()),
            Err(_) => TextRead::Absent,
        }
    }

    /// [`El::read_document_text_checked`] as an `Option`: `None` when absent or over the limit.
    pub fn read_document_text(&self, limit_utf16: usize) -> Option<String> {
        self.read_document_text_checked(limit_utf16).into_option()
    }

    /// Rows of VS Code's integrated terminal, which exposes its buffer only as `ListItem` rows
    /// when screen-reader mode is on (the focused edit itself returns a static label).
    ///
    /// `self` must be the focused `Edit` whose name starts with "Terminal", in process
    /// `expected_pid`, with `expected_window` (an `HWND` as `isize`) the foreground window. The
    /// rows are read only from the single `List` found under the element two control-view
    /// parents up, so an unrelated pane can never become the source. Returns `None` when any of
    /// that does not hold, when `deadline` passes, or when a [`TerminalLimits`] budget is
    /// exceeded. The result names the list's runtime id and native window handle so two reads can
    /// be compared for the same scope.
    pub fn terminal_rows(
        &self,
        expected_pid: i32,
        expected_window: isize,
        limits: &TerminalLimits,
        deadline: Instant,
    ) -> Option<TerminalRows> {
        let expired = || Instant::now() >= deadline;
        let foreground_is_expected =
            || unsafe { GetForegroundWindow() }.0 as isize == expected_window;
        let handle_of = |e: &El| -> Option<isize> {
            Some(unsafe { e.0.CurrentNativeWindowHandle() }.ok()?.0 as isize)
        };
        if expired() {
            return None;
        }
        if unsafe { self.0.CurrentControlType() }.ok()? != UIA_EditControlTypeId {
            return None;
        }
        let name = unsafe { self.0.CurrentName() }.ok()?.to_string();
        if !is_terminal_edit_name(&name) {
            return None;
        }
        if self.pid()? != expected_pid || !foreground_is_expected() {
            return None;
        }
        let a = automation()?;
        let walker = unsafe { a.ControlViewWalker() }.ok()?;
        let mut ancestor = self.0.clone();
        for _ in 0..2 {
            if expired() {
                return None;
            }
            ancestor = unsafe { walker.GetParentElement(&ancestor) }.ok()?;
        }
        let ancestor = El(ancestor);
        if ancestor.pid()? != expected_pid || !foreground_is_expected() {
            return None;
        }
        let ancestor_handle = handle_of(&ancestor)?;
        if ancestor_handle != 0 && ancestor_handle != expected_window {
            return None;
        }
        unsafe {
            let list_condition = a
                .CreatePropertyCondition(
                    UIA_ControlTypePropertyId,
                    &variant_i32(UIA_ListControlTypeId.0),
                )
                .ok()?;
            let lists = ancestor
                .0
                .FindAll(TreeScope_Subtree, &list_condition)
                .ok()?;
            if expired() || lists.Length().ok()? != 1 {
                return None;
            }
            let list = El(lists.GetElement(0).ok()?);
            if list.0.CurrentControlType().ok()? != UIA_ListControlTypeId
                || list.pid()? != expected_pid
            {
                return None;
            }
            let scope_runtime_id = list.runtime_id()?;
            let scope_handle = handle_of(&list)?;
            if scope_handle != 0 && scope_handle != expected_window {
                return None;
            }
            let all = a.CreateTrueCondition().ok()?;
            let rows = list.0.FindAll(TreeScope_Children, &all).ok()?;
            let count = usize::try_from(rows.Length().ok()?).ok()?;
            if expired() || count > limits.max_rows {
                return None;
            }
            let mut collector = TerminalRowCollector::new(*limits);
            for index in 0..count {
                if expired() {
                    return None;
                }
                let Ok(row) = rows.GetElement(index as i32) else {
                    continue;
                };
                if row.CurrentControlType().ok()? != UIA_ListItemControlTypeId
                    || row.CurrentProcessId().ok()? != expected_pid
                {
                    continue;
                }
                let Ok(text) = row.CurrentName() else {
                    continue;
                };
                if collector.push(text.len(), || text.to_string()) == RowPush::Refused {
                    return None;
                }
            }
            let text = collector.finish()?;
            Some(TerminalRows {
                scope_runtime_id,
                scope_native_window_handle: scope_handle,
                text,
            })
        }
    }

    /// Guarded `ValuePattern.SetValue`: writes `text` only when the element is positively not a
    /// password field, its value is writable and the current value is empty.
    ///
    /// - `None`: password state unknown or true, no `ValuePattern`, or a property read failed.
    /// - `Some(false)`: read-only, not empty, or the `SetValue` call failed.
    /// - `Some(true)`: written.
    pub fn set_value_if_empty(&self, text: &str) -> Option<bool> {
        self.set_value_if_empty_until(text, None)
    }

    /// [`El::set_value_if_empty`] that also refuses (`Some(false)`) when `deadline` has passed
    /// by the time the preconditions have been read.
    pub fn set_value_if_empty_until(&self, text: &str, deadline: Option<Instant>) -> Option<bool> {
        if self.password_state() != Some(false) {
            return None;
        }
        let v = self.pattern::<IUIAutomationValuePattern>(UIA_ValuePatternId)?;
        let read_only = b(unsafe { v.CurrentIsReadOnly() })?;
        let empty = unsafe { v.CurrentValue() }.ok()?.is_empty();
        if read_only || !empty || deadline.is_some_and(|d| Instant::now() >= d) {
            return Some(false);
        }
        Some(unsafe { v.SetValue(&BSTR::from(text)) }.is_ok())
    }
}

/// An `i32` `VARIANT` (`VT_I4`), used for property conditions.
fn variant_i32(v: i32) -> VARIANT {
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_I4,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 { lVal: v },
            }),
        },
    }
}

/// The element that currently has keyboard focus anywhere on the desktop (the raw
/// `GetFocusedElement`, not scoped to a process; `focused_editor` is the pid-scoped search).
pub fn focused_element() -> Option<El> {
    let a = automation()?;
    unsafe { a.GetFocusedElement() }.ok().map(El)
}

/// The element for the window or control `hwnd` if its own native window handle is `hwnd`
/// (`element_from_hwnd` also returns a nearby element when `hwnd` has no UIA provider of its own).
pub fn find_by_native_window_handle(hwnd: isize) -> Option<El> {
    let el = element_from_hwnd(hwnd).ok()?;
    (el.native_hwnd() == Some(hwnd)).then_some(el)
}

/// Hit-test the element at screen point `(x, y)` in physical pixels.
pub fn element_at_point(x: i32, y: i32) -> Result<El> {
    let a = automation().ok_or(Error::Unsupported("UI Automation unavailable"))?;
    unsafe { a.ElementFromPoint(POINT { x, y }) }
        .map(El)
        .map_err(|e| win_err(e, "ElementFromPoint"))
}

/// Whether the element at `(x, y)`, or one of its first `max_ancestors` control-view ancestors,
/// is a `Hyperlink`. A hit test usually lands on a text run inside the link, so the walk climbs.
/// Joins the MTA on the calling thread and answers `false` on any failure, so it is safe to call
/// from a helper thread (not from a low-level hook callback: UIA is slow). A walk of the hit
/// node plus 3 ancestors is `max_ancestors = 3`.
pub fn point_is_hyperlink(x: i32, y: i32, max_ancestors: usize) -> bool {
    let Some(a) = automation() else { return false };
    unsafe {
        let Ok(mut current) = a.ElementFromPoint(POINT { x, y }) else {
            return false;
        };
        let Ok(walker) = a.ControlViewWalker() else {
            return false;
        };
        for level in 0..=max_ancestors {
            if current.CurrentControlType().ok() == Some(UIA_HyperlinkControlTypeId) {
                return true;
            }
            if level == max_ancestors {
                break;
            }
            match walker.GetParentElement(&current) {
                Ok(parent) => current = parent,
                Err(_) => break,
            }
        }
        false
    }
}

/// Probe: is the focused element a password field? `None` when nothing is focused or UIA cannot
/// answer. Single attempt; a caller that wants a retry (Chromium builds its tree lazily) repeats
/// the call.
pub fn focused_element_is_password() -> Option<bool> {
    focused_element()?.password_state()
}

/// Probe: is the focused element an editable text target (see [`El::is_editable`])? Single
/// attempt; the first probe of a Chromium app often only wakes its accessibility tree, so
/// callers typically re-probe after about 140 ms when this is `Some(false)`.
pub fn focused_element_editable() -> Option<bool> {
    focused_element()?.is_editable()
}

/// Probe: text of the focused editable field, with the current selection. Password fields and
/// non-editable elements give `None` (a password state UIA cannot read also gives `None`).
/// Text is read as UIA returns it, bounded to `max_chars` UTF-16 units on both the `TextPattern`
/// and the `ValuePattern` paths (the fallback is truncated at a character boundary). `field_text`
/// comes from `TextPattern` and falls back to `ValuePattern`; `selected_text` only from
/// `TextPattern`.
pub fn focused_field_context(max_chars: usize) -> Option<FocusedFieldText> {
    let el = focused_element()?;
    if el.password_state()? {
        return None;
    }
    if !el.is_editable()? {
        return None;
    }
    let text_pattern = el.pattern::<IUIAutomationTextPattern>(UIA_TextPatternId);
    let value_pattern = el.pattern::<IUIAutomationValuePattern>(UIA_ValuePatternId);
    let request = max_chars.min(i32::MAX as usize) as i32;
    unsafe {
        let field_text = text_pattern
            .as_ref()
            .and_then(|t| t.DocumentRange().ok())
            .and_then(|r| r.GetText(request).ok())
            .map(|t| t.to_string())
            .or_else(|| {
                value_pattern
                    .as_ref()
                    .and_then(|v| v.CurrentValue().ok())
                    .map(|t| t.to_string())
            })
            .map(|t| truncate_utf16(&t, max_chars));
        let selected_text = text_pattern
            .as_ref()
            .and_then(|t| t.GetSelection().ok())
            .and_then(|ranges| (ranges.Length().ok()? > 0).then_some(ranges))
            .and_then(|ranges| ranges.GetElement(0).ok())
            .and_then(|r| r.GetText(request).ok())
            .map(|t| t.to_string());
        Some(FocusedFieldText {
            field_text,
            selected_text,
        })
    }
}

/// Probe: the focused element's whole text as UIA returns it, with no editability gate and no
/// normalisation, for exact before/after comparison. `max_chars = None` reads without a cap
/// (UIA `-1`). Password fields give `None`. `TextPattern` first, `ValuePattern` as the fallback;
/// with a cap, the `ValuePattern` fallback is truncated to `max_chars` UTF-16 units too.
pub fn focused_field_raw_text(max_chars: Option<usize>) -> Option<String> {
    let el = focused_element()?;
    if el.password_state()? {
        return None;
    }
    let request = max_chars.map_or(-1, |n| n.min(i32::MAX as usize) as i32);
    unsafe {
        if let Some(text) = el
            .pattern::<IUIAutomationTextPattern>(UIA_TextPatternId)
            .and_then(|t| t.DocumentRange().ok())
            .and_then(|r| r.GetText(request).ok())
            .map(|t| t.to_string())
        {
            return Some(text);
        }
    }
    el.value_text().map(|t| match max_chars {
        Some(n) => truncate_utf16(&t, n),
        None => t,
    })
}

pub fn same(a: &El, b: &El) -> bool {
    automation()
        .and_then(|au| unsafe { au.CompareElements(&a.0, &b.0) }.ok())
        .is_some_and(|x| x.as_bool())
}

pub fn read_node(el: &El) -> Option<RawNode> {
    let ct = unsafe { el.0.CurrentControlType() }.ok()?;
    let native = control_name(ct.0);
    let secure = el.is_password();
    let mut caps: Vec<String> = vec![];
    if el
        .pattern::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
        .is_some()
        || el
            .pattern::<IUIAutomationTogglePattern>(UIA_TogglePatternId)
            .is_some()
        || el
            .pattern::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)
            .is_some()
        || el
            .pattern::<IUIAutomationExpandCollapsePattern>(UIA_ExpandCollapsePatternId)
            .is_some()
    {
        caps.push("press".into());
    }
    let mut value = None;
    if let Some(v) = el.pattern::<IUIAutomationValuePattern>(UIA_ValuePatternId) {
        if !secure {
            value = unsafe { v.CurrentValue() }
                .ok()
                .map(|x| x.to_string())
                .filter(|x| !x.is_empty());
        }
        if !b(unsafe { v.CurrentIsReadOnly() }).unwrap_or(true) {
            caps.push("set_value".into());
            caps.push("insert_text".into());
        }
    }
    if el
        .pattern::<IUIAutomationScrollPattern>(UIA_ScrollPatternId)
        .is_some()
    {
        caps.push("scroll".into());
    }
    if b(unsafe { el.0.CurrentIsKeyboardFocusable() }).unwrap_or(false) {
        caps.push("focus".into());
    }
    Some(RawNode {
        role: normalize_role(native).into(),
        native_role: format!("UIA_{native}"),
        subrole: s(unsafe { el.0.CurrentClassName() }),
        name: s(unsafe { el.0.CurrentName() }),
        value,
        description: s(unsafe { el.0.CurrentHelpText() }),
        identifier: s(unsafe { el.0.CurrentAutomationId() }),
        enabled: b(unsafe { el.0.CurrentIsEnabled() }),
        secure,
        frame: el.rect(),
        caps,
    })
}

fn is_text_role(role: &str) -> bool {
    matches!(role, "UIA_Edit" | "UIA_Document" | "UIA_ComboBox")
}

/// The focused element of `pid`, resolved to an editable control. The global UIA
/// focus only describes the foreground app, so a background app is searched for
/// the element that has keyboard focus inside its own window.
pub fn focused_editor(pid: i32, window_root: Option<El>) -> Option<El> {
    let a = automation()?;
    if let Ok(f) = unsafe { a.GetFocusedElement() } {
        let f = El(f);
        if f.pid() == Some(pid) {
            return Some(f);
        }
    }
    let root = window_root?;
    let mut budget = 500usize;
    find_focus(&root, 8, &mut budget)
}

fn find_focus(root: &El, depth: usize, budget: &mut usize) -> Option<El> {
    for c in root.children() {
        if *budget == 0 {
            return None;
        }
        *budget -= 1;
        if c.has_focus() {
            return Some(c);
        }
        if depth > 0 {
            if let Some(f) = find_focus(&c, depth - 1, budget) {
                return Some(f);
            }
        }
    }
    None
}

pub fn semantic(el: &El, action: &Action) -> Result<String> {
    match action {
        Action::Press => {
            if let Some(p) = el.pattern::<IUIAutomationInvokePattern>(UIA_InvokePatternId) {
                unsafe { p.Invoke() }.map_err(|e| win_err(e, "Invoke"))?;
                return Ok("InvokePattern".into());
            }
            if let Some(p) = el.pattern::<IUIAutomationTogglePattern>(UIA_TogglePatternId) {
                unsafe { p.Toggle() }.map_err(|e| win_err(e, "Toggle"))?;
                return Ok("TogglePattern".into());
            }
            if let Some(p) =
                el.pattern::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)
            {
                unsafe { p.Select() }.map_err(|e| win_err(e, "Select"))?;
                return Ok("SelectionItemPattern".into());
            }
            if let Some(p) =
                el.pattern::<IUIAutomationExpandCollapsePattern>(UIA_ExpandCollapsePatternId)
            {
                unsafe { p.Expand() }.map_err(|e| win_err(e, "Expand"))?;
                return Ok("ExpandCollapsePattern".into());
            }
            Err(Error::Unsupported("no press pattern"))
        }
        Action::SetValue(v) => {
            if el.is_password() {
                return Err(Error::Blocked("secure field".into()));
            }
            let p: IUIAutomationValuePattern = el
                .pattern(UIA_ValuePatternId)
                .ok_or(Error::Unsupported("no ValuePattern"))?;
            if b(unsafe { p.CurrentIsReadOnly() }).unwrap_or(true) {
                return Err(Error::Unsupported("value is read-only"));
            }
            unsafe { p.SetValue(&BSTR::from(v.as_str())) }.map_err(|e| win_err(e, "SetValue"))?;
            Ok("ValuePattern".into())
        }
        Action::Scroll { dx, dy } => {
            let p: IUIAutomationScrollPattern = el
                .pattern(UIA_ScrollPatternId)
                .ok_or(Error::Unsupported("no ScrollPattern"))?;
            let amt = |d: f64| {
                if d > 0.0 {
                    ScrollAmount_LargeIncrement
                } else if d < 0.0 {
                    ScrollAmount_LargeDecrement
                } else {
                    ScrollAmount_NoAmount
                }
            };
            unsafe { p.Scroll(amt(*dx), amt(*dy)) }.map_err(|e| win_err(e, "Scroll"))?;
            Ok("ScrollPattern".into())
        }
        Action::Focus => {
            // SetFocus can pull a background window forward; only focus inside the app that is already foreground.
            let fg = super::sys::foreground_pid();
            if fg.is_some() && fg == el.pid() {
                unsafe { el.0.SetFocus() }.map_err(|e| win_err(e, "SetFocus"))?;
                Ok("SetFocus".into())
            } else {
                Err(Error::Unsupported(
                    "element is not in the foreground app (SetFocus would activate it)",
                ))
            }
        }
        _ => Err(Error::Unsupported("not a semantic action")),
    }
}

pub fn field_text(el: &El) -> Option<FieldText> {
    if el.is_password() {
        return None;
    }
    // TextPattern first (document text and selection), ValuePattern as the fallback.
    if let Some(tp) = el.pattern::<IUIAutomationTextPattern>(UIA_TextPatternId) {
        if let Some(ft) = text_pattern_read(&tp) {
            return Some(ft);
        }
    }
    let v = el.value_text()?;
    (v.len() <= MAX_PROOF_BYTES).then_some(FieldText {
        text: v,
        source: Some(TextSource::Value),
        selection: None,
    })
}

fn text_pattern_read(tp: &IUIAutomationTextPattern) -> Option<FieldText> {
    unsafe {
        let doc = tp.DocumentRange().ok()?;
        let text = doc.GetText(MAX_PROOF_BYTES as i32).ok()?.to_string();
        let mut selection = None;
        if let Ok(sels) = tp.GetSelection() {
            if sels.Length().unwrap_or(0) > 0 {
                if let Ok(sel) = sels.GetElement(0) {
                    let len = sel
                        .GetText(MAX_PROOF_BYTES as i32)
                        .ok()
                        .map(|t| t.to_string().encode_utf16().count());
                    // start = text between the document start and the selection start
                    let pre = doc.Clone().ok()?;
                    if pre
                        .MoveEndpointByRange(
                            TextPatternRangeEndpoint_End,
                            &sel,
                            TextPatternRangeEndpoint_Start,
                        )
                        .is_ok()
                    {
                        let start = pre
                            .GetText(MAX_PROOF_BYTES as i32)
                            .ok()
                            .map(|t| t.to_string().encode_utf16().count());
                        if let (Some(st), Some(l)) = (start, len) {
                            selection = Some((st, l));
                        }
                    }
                }
            }
        }
        Some(FieldText {
            text,
            source: Some(TextSource::Value),
            selection,
        })
    }
}

/// Selection-exact native write on Windows exists only when replacing the whole
/// value is the same as replacing the selection: a writable ValuePattern on an
/// empty, non-password field. Anything else climbs to key events.
pub fn native_write_capable(el: &El) -> bool {
    if el.is_password() {
        return false;
    }
    let Some(role) = read_native_role(el) else {
        return false;
    };
    if !is_text_role(&role) {
        return false;
    }
    let Some(v) = el.pattern::<IUIAutomationValuePattern>(UIA_ValuePatternId) else {
        return false;
    };
    let writable = !b(unsafe { v.CurrentIsReadOnly() }).unwrap_or(true);
    let empty = unsafe { v.CurrentValue() }
        .map(|x| x.is_empty())
        .unwrap_or(false);
    writable && empty
}

fn read_native_role(el: &El) -> Option<String> {
    let ct = unsafe { el.0.CurrentControlType() }.ok()?;
    Some(format!("UIA_{}", control_name(ct.0)))
}

pub fn native_write(el: &El, text: &str) -> Result<()> {
    let v: IUIAutomationValuePattern = el
        .pattern(UIA_ValuePatternId)
        .ok_or(Error::Unsupported("no ValuePattern"))?;
    unsafe { v.SetValue(&BSTR::from(text)) }.map_err(|e| win_err(e, "SetValue"))
}
