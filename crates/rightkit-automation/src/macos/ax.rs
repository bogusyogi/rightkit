//! Accessibility element access: attribute reads, normalized `RawNode`, the
//! focused-editor resolver, semantic actions, native selection write, text proof reads.

use super::ffi::*;
use crate::model::*;
use crate::proof::{FieldText, TextSource, MAX_PROOF_BYTES};
use std::ffi::c_void;

pub const AX_TIMEOUT: f32 = 1.5;

#[derive(Clone)]
pub struct El(pub Cf);

fn ax_err(code: i32, op: &'static str) -> Error {
    match code {
        // attribute/action unsupported, no value, not implemented, parameterized unsupported
        -25205 | -25206 | -25212 | -25208 | -25207 => Error::Unsupported(op),
        -25211 => Error::PermissionDenied("Accessibility"),
        -25202 => Error::NotFound("accessibility element".into()),
        c => Error::Platform(i64::from(c), op),
    }
}

impl El {
    pub fn app(pid: i32) -> Result<El> {
        if unsafe { AXIsProcessTrusted() } == 0 {
            return Err(Error::PermissionDenied("Accessibility"));
        }
        let r = Cf::new(unsafe { AXUIElementCreateApplication(pid) }).ok_or_else(|| Error::NotFound(format!("pid {pid}")))?;
        unsafe { AXUIElementSetMessagingTimeout(r.0, AX_TIMEOUT) };
        Ok(El(r))
    }

    fn raw(&self, name: &str) -> Option<Cf> {
        let k = Cf::string(name);
        let mut out: CFTypeRef = std::ptr::null();
        let e = unsafe { AXUIElementCopyAttributeValue((self.0).0, k.0, &mut out) };
        if e == 0 {
            Cf::new(out)
        } else {
            None
        }
    }
    pub fn el_attr(&self, name: &str) -> Option<El> {
        self.raw(name).map(El)
    }
    pub fn str_attr(&self, name: &str) -> Option<String> {
        self.raw(name).and_then(|c| cf_to_string(c.0))
    }
    pub fn bool_attr(&self, name: &str) -> Option<bool> {
        self.raw(name).and_then(|c| cf_to_bool(c.0))
    }
    pub fn f64_attr(&self, name: &str) -> Option<f64> {
        self.raw(name).and_then(|c| cf_to_f64(c.0))
    }
    /// Text of AXValue when it is a string; numbers and booleans render as text.
    pub fn value_text(&self) -> Option<String> {
        let v = self.raw("AXValue")?;
        if let Some(s) = cf_to_string(v.0) {
            return Some(s);
        }
        if let Some(b) = cf_to_bool(v.0) {
            return Some(if b { "1" } else { "0" }.into());
        }
        if let Some(i) = cf_to_i64(v.0) {
            return Some(i.to_string());
        }
        cf_to_f64(v.0).map(|f| f.to_string())
    }
    pub fn pid(&self) -> Option<i32> {
        let mut p = 0;
        (unsafe { AXUIElementGetPid((self.0).0, &mut p) } == 0).then_some(p)
    }
    pub fn list(&self, name: &str) -> Vec<El> {
        let Some(a) = self.raw(name) else { return vec![] };
        unsafe {
            if CFGetTypeID(a.0) != CFArrayGetTypeID() {
                return vec![];
            }
            (0..CFArrayGetCount(a.0)).filter_map(|i| Cf::retain(CFArrayGetValueAtIndex(a.0, i)).map(El)).collect()
        }
    }
    pub fn children(&self) -> Vec<El> {
        self.list("AXChildren")
    }
    pub fn actions(&self) -> Vec<String> {
        let mut out: CFTypeRef = std::ptr::null();
        if unsafe { AXUIElementCopyActionNames((self.0).0, &mut out) } != 0 {
            return vec![];
        }
        let Some(a) = Cf::new(out) else { return vec![] };
        unsafe { (0..CFArrayGetCount(a.0)).filter_map(|i| cf_to_string(CFArrayGetValueAtIndex(a.0, i))).collect() }
    }
    pub fn settable(&self, attr: &str) -> bool {
        let k = Cf::string(attr);
        let mut b = 0u8;
        unsafe { AXUIElementIsAttributeSettable((self.0).0, k.0, &mut b) == 0 && b != 0 }
    }
    pub fn frame(&self) -> Option<Rect> {
        let (p, s) = (self.raw("AXPosition")?, self.raw("AXSize")?);
        let (mut pt, mut sz) = (CGPoint { x: 0.0, y: 0.0 }, CGSize { w: 0.0, h: 0.0 });
        unsafe {
            (AXValueGetValue(p.0, 1, &mut pt as *mut _ as *mut c_void) != 0 && AXValueGetValue(s.0, 2, &mut sz as *mut _ as *mut c_void) != 0)
                .then_some(Rect { x: pt.x, y: pt.y, w: sz.w, h: sz.h })
        }
    }
    pub fn window_id(&self) -> Option<u64> {
        let mut w = 0u32;
        let win = if self.str_attr("AXRole").as_deref() == Some("AXWindow") { self.clone() } else { self.el_attr("AXWindow")? };
        (unsafe { _AXUIElementGetWindow((win.0).0, &mut w) } == 0 && w != 0).then_some(u64::from(w))
    }
    pub fn same(&self, other: &El) -> bool {
        unsafe { CFEqual((self.0).0, (other.0).0) != 0 }
    }
    /// Selection as UTF-16 (location, length).
    pub fn selected_range(&self) -> Option<(usize, usize)> {
        let v = self.raw("AXSelectedTextRange")?;
        let mut r = CFRange { location: 0, length: 0 };
        unsafe { (AXValueGetValue(v.0, 4, &mut r as *mut _ as *mut c_void) != 0 && r.location >= 0 && r.length >= 0).then_some((r.location as usize, r.length as usize)) }
    }
    pub fn perform(&self, action: &str) -> Result<()> {
        let a = Cf::string(action);
        match unsafe { AXUIElementPerformAction((self.0).0, a.0) } {
            0 => Ok(()),
            c => Err(ax_err(c, "perform")),
        }
    }
    pub fn set_str(&self, attr: &str, v: &str) -> Result<()> {
        let (k, s) = (Cf::string(attr), Cf::string(v));
        match unsafe { AXUIElementSetAttributeValue((self.0).0, k.0, s.0) } {
            0 => Ok(()),
            c => Err(ax_err(c, "set_value")),
        }
    }
    pub fn set_bool(&self, attr: &str, v: bool) -> Result<()> {
        let k = Cf::string(attr);
        let b = unsafe { if v { kCFBooleanTrue } else { kCFBooleanFalse } };
        match unsafe { AXUIElementSetAttributeValue((self.0).0, k.0, b) } {
            0 => Ok(()),
            c => Err(ax_err(c, "set_bool")),
        }
    }
    pub fn set_f64(&self, attr: &str, v: f64) -> Result<()> {
        let k = Cf::string(attr);
        let n = Cf::new(unsafe { CFNumberCreate(std::ptr::null(), 6, &v as *const f64 as *const c_void) }).ok_or(Error::Unsupported("number"))?;
        match unsafe { AXUIElementSetAttributeValue((self.0).0, k.0, n.0) } {
            0 => Ok(()),
            c => Err(ax_err(c, "set_number")),
        }
    }
    fn text_marker_string(&self) -> Option<String> {
        let (k1, k2) = (Cf::string("AXTextMarkerRangeForUIElement"), Cf::string("AXStringForTextMarkerRange"));
        unsafe {
            let mut range: CFTypeRef = std::ptr::null();
            if AXUIElementCopyParameterizedAttributeValue((self.0).0, k1.0, (self.0).0, &mut range) != 0 {
                return None;
            }
            let range = Cf::new(range)?;
            let mut s: CFTypeRef = std::ptr::null();
            if AXUIElementCopyParameterizedAttributeValue((self.0).0, k2.0, range.0, &mut s) != 0 {
                return None;
            }
            cf_to_string(Cf::new(s)?.0)
        }
    }
    pub fn has_web_area_ancestor(&self) -> bool {
        let mut cur = self.clone();
        for _ in 0..32 {
            if cur.str_attr("AXRole").as_deref() == Some("AXWebArea") {
                return true;
            }
            match cur.el_attr("AXParent") {
                Some(p) => cur = p,
                None => return false,
            }
        }
        false
    }
    pub fn is_secure(&self) -> bool {
        self.str_attr("AXRole").as_deref() == Some("AXSecureTextField") || self.str_attr("AXSubrole").as_deref() == Some("AXSecureTextField")
    }
}

fn normalize_role(r: &str) -> String {
    match r {
        "AXButton" | "AXMenuButton" | "AXPopUpButton" => "button",
        "AXTextArea" => "textarea",
        "AXTextField" | "AXSearchField" | "AXComboBox" | "AXSecureTextField" => "textfield",
        "AXStaticText" => "text",
        "AXWindow" => "window",
        "AXCheckBox" => "checkbox",
        "AXRadioButton" => "radio",
        "AXMenuItem" | "AXMenuBarItem" => "menuitem",
        "AXMenu" | "AXMenuBar" => "menu",
        "AXGroup" | "AXSplitGroup" | "AXLayoutArea" => "group",
        "AXScrollArea" => "scrollarea",
        "AXImage" => "image",
        "AXList" | "AXTable" | "AXOutline" => "list",
        "AXRow" | "AXCell" => "row",
        "AXToolbar" => "toolbar",
        "AXWebArea" => "web",
        "AXSlider" => "slider",
        "AXTabGroup" => "tabs",
        other => return other.trim_start_matches("AX").to_ascii_lowercase(),
    }
    .to_string()
}

pub fn read_node(el: &El) -> Option<RawNode> {
    let native = el.str_attr("AXRole")?;
    let secure = el.is_secure();
    let nonempty = |s: Option<String>| s.filter(|v| !v.is_empty());
    let actions = el.actions();
    let mut caps: Vec<String> = vec![];
    if actions.iter().any(|a| a == "AXPress" || a == "AXConfirm" || a == "AXPick") {
        caps.push("press".into());
    }
    let texty = matches!(native.as_str(), "AXTextField" | "AXTextArea" | "AXComboBox" | "AXSearchField" | "AXSecureTextField");
    if texty {
        if el.settable("AXValue") {
            caps.push("set_value".into());
        }
        if el.settable("AXSelectedText") {
            caps.push("insert_text".into());
        }
    }
    if el.settable("AXFocused") {
        caps.push("focus".into());
    }
    if native == "AXScrollArea" || native == "AXScrollBar" || el.raw("AXVerticalScrollBar").is_some() {
        caps.push("scroll".into());
    }
    if actions.iter().any(|a| a == "AXShowMenu") {
        caps.push("menu".into());
    }
    Some(RawNode {
        role: normalize_role(&native),
        native_role: native,
        subrole: nonempty(el.str_attr("AXSubrole")),
        name: nonempty(el.str_attr("AXTitle")),
        // Never read the value of a secure field.
        value: if secure { None } else { nonempty(el.value_text()) },
        description: nonempty(el.str_attr("AXDescription")),
        identifier: nonempty(el.str_attr("AXIdentifier")),
        enabled: el.bool_attr("AXEnabled"),
        secure,
        frame: el.frame(),
        caps,
    })
}

/// The window element for a target: explicit window id, else focused, main, first.
pub fn window_root(app: &El, window_id: Option<u64>) -> Option<El> {
    let by_id = |id: u64| app.list("AXWindows").into_iter().find(|w| w.window_id() == Some(id));
    match window_id {
        Some(id) => by_id(id),
        None => app.el_attr("AXFocusedWindow").or_else(|| app.el_attr("AXMainWindow")).or_else(|| app.list("AXWindows").into_iter().next()),
    }
}

fn is_editable_role(role: &str) -> bool {
    matches!(role, "AXTextField" | "AXTextArea" | "AXComboBox" | "AXSearchField" | "AXSecureTextField")
}

/// Deepest focused element, resolved to an editable editor when the focus sits
/// on a container (bounded search, as web/Electron composers require).
pub fn focused_editor(app: &El) -> Option<El> {
    let mut f = app.el_attr("AXFocusedUIElement");
    if f.is_none() {
        let w = app.el_attr("AXFocusedWindow")?;
        f = find_focused_descendant(&w, 7, &mut 400);
    }
    let f = f?;
    let role = f.str_attr("AXRole").unwrap_or_default();
    if is_editable_role(&role) {
        return Some(f);
    }
    if matches!(role.as_str(), "AXGroup" | "AXScrollArea" | "AXWebArea" | "AXWindow") {
        if let Some(d) = find_focused_descendant(&f, 6, &mut 300) {
            return Some(d);
        }
    }
    Some(f)
}

fn find_focused_descendant(root: &El, depth: usize, budget: &mut usize) -> Option<El> {
    for c in root.children() {
        if *budget == 0 {
            return None;
        }
        *budget -= 1;
        let role = c.str_attr("AXRole").unwrap_or_default();
        if c.bool_attr("AXFocused") == Some(true) && (is_editable_role(&role) || c.children().is_empty()) {
            return Some(c);
        }
        if depth > 0 {
            if let Some(f) = find_focused_descendant(&c, depth - 1, budget) {
                return Some(f);
            }
        }
    }
    None
}

pub fn semantic(el: &El, action: &Action) -> Result<String> {
    match action {
        Action::Press => {
            let acts = el.actions();
            for a in ["AXPress", "AXConfirm", "AXPick"] {
                if acts.iter().any(|x| x == a) {
                    el.perform(a)?;
                    return Ok(a.to_string());
                }
            }
            Err(Error::Unsupported("no press action"))
        }
        Action::SetValue(v) => {
            if el.is_secure() {
                return Err(Error::Blocked("secure field".into()));
            }
            if !el.settable("AXValue") {
                return Err(Error::Unsupported("AXValue not settable"));
            }
            el.set_str("AXValue", v)?;
            Ok("AXValue".into())
        }
        Action::Focus => {
            el.set_bool("AXFocused", true)?;
            Ok("AXFocused".into())
        }
        Action::Scroll { dy, .. } => {
            let mut cur = Some(el.clone());
            for _ in 0..8 {
                let Some(c) = cur else { break };
                if let Some(bar) = c.el_attr("AXVerticalScrollBar") {
                    if let Some(v) = bar.f64_attr("AXValue") {
                        bar.set_f64("AXValue", (v + dy).clamp(0.0, 1.0))?;
                        return Ok("AXVerticalScrollBar".into());
                    }
                }
                cur = c.el_attr("AXParent");
            }
            Err(Error::Unsupported("no scroll bar"))
        }
        _ => Err(Error::Unsupported("not a semantic action")),
    }
}

pub fn menu_press(app: &El, path: &[String]) -> Result<()> {
    let mut cur = app.el_attr("AXMenuBar").ok_or_else(|| Error::NotFound("menu bar".into()))?;
    for (i, want) in path.iter().enumerate() {
        let item = cur
            .children()
            .into_iter()
            .find(|c| c.str_attr("AXTitle").as_deref() == Some(want.as_str()))
            .ok_or_else(|| Error::NotFound(format!("menu item {want}")))?;
        if i + 1 == path.len() {
            return item.perform("AXPress");
        }
        cur = item.children().into_iter().next().ok_or_else(|| Error::NotFound(format!("submenu {want}")))?;
    }
    Err(Error::Invalid("empty menu path".into()))
}

pub fn field_text(el: &El) -> Option<FieldText> {
    let selection = el.selected_range();
    if let Some(v) = el.raw("AXValue").and_then(|c| cf_to_string(c.0)) {
        if v.len() <= MAX_PROOF_BYTES {
            return Some(FieldText { text: v, source: Some(TextSource::Value), selection });
        }
    }
    el.text_marker_string().filter(|t| t.len() <= MAX_PROOF_BYTES).map(|t| FieldText { text: t, source: Some(TextSource::TextMarker), selection: None })
}

/// HR's four-signal capability decision: a real native text control, not
/// web/rich-editor content, whose value is directly readable and whose
/// selected text is settable.
pub fn native_write_capable(el: &El) -> bool {
    if unsafe { AXIsProcessTrusted() } == 0 || el.is_secure() || el.has_web_area_ancestor() {
        return false;
    }
    el.raw("AXValue").and_then(|c| cf_to_string(c.0)).is_some() && el.settable("AXSelectedText")
}

pub fn native_write(el: &El, text: &str) -> Result<()> {
    el.set_str("AXSelectedText", text)
}
