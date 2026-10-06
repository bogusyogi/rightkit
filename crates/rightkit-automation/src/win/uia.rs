//! UI Automation: element reads, semantic actions, text proof reads. UIA is
//! initialized in the multithreaded apartment, so interface pointers may be used
//! from any thread that has called `ensure_com`.

use crate::model::*;
use crate::proof::{FieldText, TextSource, MAX_PROOF_BYTES};
use std::cell::Cell;
use std::sync::OnceLock;
use windows::core::{IUnknown, Interface, BSTR};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::WindowsAndMessaging::{GetAncestor, GA_ROOT};

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
        CoCreateInstance::<Option<&IUnknown>, IUIAutomation>(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)
            .or_else(|_| CoCreateInstance::<Option<&IUnknown>, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER))
            .ok()
            .map(Shared)
    })
    .as_ref()
    .map(|s| &s.0)
}

#[derive(Clone)]
pub struct El(pub IUIAutomationElement);
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
    unsafe { a.ElementFromHandle(HWND(hwnd as *mut _)) }.map(El).map_err(|e| win_err(e, "ElementFromHandle"))
}

fn control_name(id: i32) -> &'static str {
    const NAMES: [&str; 41] = [
        "Button", "Calendar", "CheckBox", "ComboBox", "Edit", "Hyperlink", "Image", "ListItem", "List", "Menu", "MenuBar", "MenuItem",
        "ProgressBar", "RadioButton", "ScrollBar", "Slider", "Spinner", "StatusBar", "Tab", "TabItem", "Text", "ToolBar", "ToolTip", "Tree",
        "TreeItem", "Custom", "Group", "Thumb", "DataGrid", "DataItem", "Document", "SplitButton", "Window", "Pane", "Header", "HeaderItem",
        "Table", "TitleBar", "Separator", "SemanticZoom", "AppBar",
    ];
    NAMES.get((id - 50000) as usize).copied().unwrap_or("Unknown")
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
    pub fn is_password(&self) -> bool {
        b(unsafe { self.0.CurrentIsPassword() }).unwrap_or(false)
    }
    pub fn pid(&self) -> Option<i32> {
        unsafe { self.0.CurrentProcessId() }.ok()
    }
    pub fn rect(&self) -> Option<Rect> {
        let r: RECT = unsafe { self.0.CurrentBoundingRectangle() }.ok()?;
        (r.right > r.left && r.bottom > r.top).then(|| Rect { x: f64::from(r.left), y: f64::from(r.top), w: f64::from(r.right - r.left), h: f64::from(r.bottom - r.top) })
    }
    pub fn children(&self) -> Vec<El> {
        let Some(a) = automation() else { return vec![] };
        unsafe {
            let Ok(cond) = a.ControlViewCondition() else { return vec![] };
            let Ok(arr) = self.0.FindAll(TreeScope_Children, &cond) else { return vec![] };
            let n = arr.Length().unwrap_or(0);
            (0..n).filter_map(|i| arr.GetElement(i).ok()).map(El).collect()
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

pub fn same(a: &El, b: &El) -> bool {
    automation().and_then(|au| unsafe { au.CompareElements(&a.0, &b.0) }.ok()).is_some_and(|x| x.as_bool())
}

pub fn read_node(el: &El) -> Option<RawNode> {
    let ct = unsafe { el.0.CurrentControlType() }.ok()?;
    let native = control_name(ct.0);
    let secure = el.is_password();
    let mut caps: Vec<String> = vec![];
    if el.pattern::<IUIAutomationInvokePattern>(UIA_InvokePatternId).is_some()
        || el.pattern::<IUIAutomationTogglePattern>(UIA_TogglePatternId).is_some()
        || el.pattern::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId).is_some()
        || el.pattern::<IUIAutomationExpandCollapsePattern>(UIA_ExpandCollapsePatternId).is_some()
    {
        caps.push("press".into());
    }
    let mut value = None;
    if let Some(v) = el.pattern::<IUIAutomationValuePattern>(UIA_ValuePatternId) {
        if !secure {
            value = unsafe { v.CurrentValue() }.ok().map(|x| x.to_string()).filter(|x| !x.is_empty());
        }
        if !b(unsafe { v.CurrentIsReadOnly() }).unwrap_or(true) {
            caps.push("set_value".into());
            caps.push("insert_text".into());
        }
    }
    if el.pattern::<IUIAutomationScrollPattern>(UIA_ScrollPatternId).is_some() {
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
            if let Some(p) = el.pattern::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId) {
                unsafe { p.Select() }.map_err(|e| win_err(e, "Select"))?;
                return Ok("SelectionItemPattern".into());
            }
            if let Some(p) = el.pattern::<IUIAutomationExpandCollapsePattern>(UIA_ExpandCollapsePatternId) {
                unsafe { p.Expand() }.map_err(|e| win_err(e, "Expand"))?;
                return Ok("ExpandCollapsePattern".into());
            }
            Err(Error::Unsupported("no press pattern"))
        }
        Action::SetValue(v) => {
            if el.is_password() {
                return Err(Error::Blocked("secure field".into()));
            }
            let p: IUIAutomationValuePattern = el.pattern(UIA_ValuePatternId).ok_or(Error::Unsupported("no ValuePattern"))?;
            if b(unsafe { p.CurrentIsReadOnly() }).unwrap_or(true) {
                return Err(Error::Unsupported("value is read-only"));
            }
            unsafe { p.SetValue(&BSTR::from(v.as_str())) }.map_err(|e| win_err(e, "SetValue"))?;
            Ok("ValuePattern".into())
        }
        Action::Scroll { dx, dy } => {
            let p: IUIAutomationScrollPattern = el.pattern(UIA_ScrollPatternId).ok_or(Error::Unsupported("no ScrollPattern"))?;
            let amt = |d: f64| if d > 0.0 { ScrollAmount_LargeIncrement } else if d < 0.0 { ScrollAmount_LargeDecrement } else { ScrollAmount_NoAmount };
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
                Err(Error::Unsupported("element is not in the foreground app (SetFocus would activate it)"))
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
    (v.len() <= MAX_PROOF_BYTES).then_some(FieldText { text: v, source: Some(TextSource::Value), selection: None })
}

fn text_pattern_read(tp: &IUIAutomationTextPattern) -> Option<FieldText> {
    unsafe {
        let doc = tp.DocumentRange().ok()?;
        let text = doc.GetText(MAX_PROOF_BYTES as i32).ok()?.to_string();
        let mut selection = None;
        if let Ok(sels) = tp.GetSelection() {
            if sels.Length().unwrap_or(0) > 0 {
                if let Ok(sel) = sels.GetElement(0) {
                    let len = sel.GetText(MAX_PROOF_BYTES as i32).ok().map(|t| t.to_string().encode_utf16().count());
                    // start = text between the document start and the selection start
                    let pre = doc.Clone().ok()?;
                    if pre.MoveEndpointByRange(TextPatternRangeEndpoint_End, &sel, TextPatternRangeEndpoint_Start).is_ok() {
                        let start = pre.GetText(MAX_PROOF_BYTES as i32).ok().map(|t| t.to_string().encode_utf16().count());
                        if let (Some(st), Some(l)) = (start, len) {
                            selection = Some((st, l));
                        }
                    }
                }
            }
        }
        Some(FieldText { text, source: Some(TextSource::Value), selection })
    }
}

/// Selection-exact native write on Windows exists only when replacing the whole
/// value is the same as replacing the selection: a writable ValuePattern on an
/// empty, non-password field. Anything else climbs to key events.
pub fn native_write_capable(el: &El) -> bool {
    if el.is_password() {
        return false;
    }
    let Some(role) = read_native_role(el) else { return false };
    if !is_text_role(&role) {
        return false;
    }
    let Some(v) = el.pattern::<IUIAutomationValuePattern>(UIA_ValuePatternId) else { return false };
    let writable = !b(unsafe { v.CurrentIsReadOnly() }).unwrap_or(true);
    let empty = unsafe { v.CurrentValue() }.map(|x| x.is_empty()).unwrap_or(false);
    writable && empty
}

fn read_native_role(el: &El) -> Option<String> {
    let ct = unsafe { el.0.CurrentControlType() }.ok()?;
    Some(format!("UIA_{}", control_name(ct.0)))
}

pub fn native_write(el: &El, text: &str) -> Result<()> {
    let v: IUIAutomationValuePattern = el.pattern(UIA_ValuePatternId).ok_or(Error::Unsupported("no ValuePattern"))?;
    unsafe { v.SetValue(&BSTR::from(text)) }.map_err(|e| win_err(e, "SetValue"))
}
