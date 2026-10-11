//! Accessibility element access: attribute reads, normalized `RawNode`, the
//! focused-editor resolver, semantic actions, native selection write, text proof reads.

use super::ffi::*;
use crate::model::*;
use crate::proof::{FieldText, TextSource, MAX_PROOF_BYTES};
use std::ffi::c_void;

use std::time::Duration;

/// Default AX messaging timeout in seconds. Every element this crate creates uses it
/// unless the caller passes an [`AxOptions`].
pub const AX_TIMEOUT: f32 = 1.5;

/// `AX_TIMEOUT` as a `Duration`.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(1500);

/// Per-call options for creating an AX element.
///
/// `timeout` is applied with `AXUIElementSetMessagingTimeout`: an unresponsive target app
/// fails the call with `Error::Platform(-25204, ..)` (CannotComplete) after this long instead
/// of the system default (about 6 s). `Duration::ZERO` asks AX to reset to its own default.
///
/// **Process-wide on the system-wide element.** Apple documents that the timeout set on the
/// system-wide element becomes the timeout for every AX element in the process that has no
/// timeout of its own, and AX offers no getter to restore the previous value. So
/// [`system_wide`], [`system_wide_with`], [`element_at_position`] and
/// [`element_at_position_with`] change that process-wide default (to 1.5 s, or to the value
/// you pass). A host that has its own AX code and needs a different default should pass its
/// value in `AxOptions` here, or set its own timeouts on the elements it owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AxOptions {
    pub timeout: Duration,
}

impl Default for AxOptions {
    fn default() -> Self {
        AxOptions {
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

impl AxOptions {
    pub fn with_timeout(timeout: Duration) -> Self {
        AxOptions { timeout }
    }
}

/// Seconds as the `f32` AX takes. Clamped into `[0, f32::MAX]`, so an absurdly long
/// `Duration` cannot become infinity.
fn timeout_secs(d: Duration) -> f32 {
    d.as_secs_f64().min(f64::from(f32::MAX)) as f32
}

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

fn require_trusted() -> Result<()> {
    if unsafe { AXIsProcessTrusted() } == 0 {
        Err(Error::PermissionDenied("Accessibility"))
    } else {
        Ok(())
    }
}

/// The system-wide element (`AXUIElementCreateSystemWide`) with the default 1.5 s timeout.
/// Process-wide timeout effect: see [`AxOptions`].
pub fn system_wide() -> Result<El> {
    system_wide_with(&AxOptions::default())
}

/// The system-wide element with the caller's timeout. Process-wide timeout effect: see
/// [`AxOptions`]. Fails with `PermissionDenied` when the process is not Accessibility-trusted.
pub fn system_wide_with(opts: &AxOptions) -> Result<El> {
    require_trusted()?;
    let el = El(Cf::new(unsafe { AXUIElementCreateSystemWide() })
        .ok_or(Error::Unsupported("system-wide accessibility element"))?);
    el.set_timeout(opts.timeout);
    Ok(el)
}

/// Outcome of `AXUIElementCopyElementAtPosition`: `Ok(true)` found, `Ok(false)` nothing at
/// that point (kAXErrorNoValue), `Err` for everything else (permission, hung app, bad element).
fn hit_test_outcome(code: i32) -> Result<bool> {
    match code {
        0 => Ok(true),
        -25212 => Ok(false),
        -25211 => Err(Error::PermissionDenied("Accessibility")),
        c => Err(Error::Platform(i64::from(c), "element_at_position")),
    }
}

/// The deepest accessible element under a point in global display coordinates (top-left
/// origin, points; `f32` precision, as AX takes), across all apps, with the default 1.5 s timeout. `Ok(None)` when nothing
/// accessible is there. The returned element carries the same timeout. Process-wide timeout
/// effect: see [`AxOptions`].
pub fn element_at_position(x: f64, y: f64) -> Result<Option<El>> {
    element_at_position_with(x, y, &AxOptions::default())
}

/// [`element_at_position`] with the caller's timeout (for example 150 ms for a hit test in a
/// latency-sensitive event tap). A hung app yields `Err(Error::Platform(-25204, ..))`.
pub fn element_at_position_with(x: f64, y: f64, opts: &AxOptions) -> Result<Option<El>> {
    let sys = system_wide_with(opts)?;
    let mut out: CFTypeRef = std::ptr::null();
    let code = unsafe { AXUIElementCopyElementAtPosition((sys.0).0, x as f32, y as f32, &mut out) };
    if !hit_test_outcome(code)? {
        return Ok(None);
    }
    let el = El(Cf::new(out).ok_or(Error::Unsupported("accessibility element at position"))?);
    el.set_timeout(opts.timeout);
    Ok(Some(el))
}

impl El {
    /// Application element with the default 1.5 s messaging timeout.
    pub fn app(pid: i32) -> Result<El> {
        El::app_with(pid, &AxOptions::default())
    }

    /// Application element with the caller's messaging timeout (per element).
    pub fn app_with(pid: i32, opts: &AxOptions) -> Result<El> {
        require_trusted()?;
        let r = Cf::new(unsafe { AXUIElementCreateApplication(pid) })
            .ok_or_else(|| Error::NotFound(format!("pid {pid}")))?;
        let el = El(r);
        el.set_timeout(opts.timeout);
        Ok(el)
    }

    /// Set this element's messaging timeout (`AXUIElementSetMessagingTimeout`). On the
    /// system-wide element this is the process-wide default; see [`AxOptions`].
    pub fn set_timeout(&self, timeout: Duration) {
        unsafe { AXUIElementSetMessagingTimeout((self.0).0, timeout_secs(timeout)) };
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
        let Some(a) = self.raw(name) else {
            return vec![];
        };
        unsafe {
            if CFGetTypeID(a.0) != CFArrayGetTypeID() {
                return vec![];
            }
            (0..CFArrayGetCount(a.0))
                .filter_map(|i| Cf::retain(CFArrayGetValueAtIndex(a.0, i)).map(El))
                .collect()
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
        unsafe {
            (0..CFArrayGetCount(a.0))
                .filter_map(|i| cf_to_string(CFArrayGetValueAtIndex(a.0, i)))
                .collect()
        }
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
            (AXValueGetValue(p.0, 1, &mut pt as *mut _ as *mut c_void) != 0
                && AXValueGetValue(s.0, 2, &mut sz as *mut _ as *mut c_void) != 0)
                .then_some(Rect {
                    x: pt.x,
                    y: pt.y,
                    w: sz.w,
                    h: sz.h,
                })
        }
    }
    pub fn window_id(&self) -> Option<u64> {
        let mut w = 0u32;
        let win = if self.str_attr("AXRole").as_deref() == Some("AXWindow") {
            self.clone()
        } else {
            self.el_attr("AXWindow")?
        };
        (unsafe { _AXUIElementGetWindow((win.0).0, &mut w) } == 0 && w != 0).then_some(u64::from(w))
    }
    pub fn same(&self, other: &El) -> bool {
        unsafe { CFEqual((self.0).0, (other.0).0) != 0 }
    }
    /// Selection as UTF-16 (location, length).
    pub fn selected_range(&self) -> Option<(usize, usize)> {
        let v = self.raw("AXSelectedTextRange")?;
        let mut r = CFRange {
            location: 0,
            length: 0,
        };
        unsafe {
            (AXValueGetValue(v.0, 4, &mut r as *mut _ as *mut c_void) != 0
                && r.location >= 0
                && r.length >= 0)
                .then_some((r.location as usize, r.length as usize))
        }
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
        let b = unsafe {
            if v {
                kCFBooleanTrue
            } else {
                kCFBooleanFalse
            }
        };
        match unsafe { AXUIElementSetAttributeValue((self.0).0, k.0, b) } {
            0 => Ok(()),
            c => Err(ax_err(c, "set_bool")),
        }
    }
    pub fn set_f64(&self, attr: &str, v: f64) -> Result<()> {
        let k = Cf::string(attr);
        let n = Cf::new(unsafe {
            CFNumberCreate(std::ptr::null(), 6, &v as *const f64 as *const c_void)
        })
        .ok_or(Error::Unsupported("number"))?;
        match unsafe { AXUIElementSetAttributeValue((self.0).0, k.0, n.0) } {
            0 => Ok(()),
            c => Err(ax_err(c, "set_number")),
        }
    }
    fn text_marker_string(&self) -> Option<String> {
        let (k1, k2) = (
            Cf::string("AXTextMarkerRangeForUIElement"),
            Cf::string("AXStringForTextMarkerRange"),
        );
        unsafe {
            let mut range: CFTypeRef = std::ptr::null();
            if AXUIElementCopyParameterizedAttributeValue((self.0).0, k1.0, (self.0).0, &mut range)
                != 0
            {
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
        self.str_attr("AXRole").as_deref() == Some("AXSecureTextField")
            || self.str_attr("AXSubrole").as_deref() == Some("AXSecureTextField")
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

pub(super) fn read_node(el: &El) -> Option<RawNode> {
    let native = el.str_attr("AXRole")?;
    let secure = el.is_secure();
    let nonempty = |s: Option<String>| s.filter(|v| !v.is_empty());
    let actions = el.actions();
    let mut caps: Vec<String> = vec![];
    if actions
        .iter()
        .any(|a| a == "AXPress" || a == "AXConfirm" || a == "AXPick")
    {
        caps.push("press".into());
    }
    let texty = matches!(
        native.as_str(),
        "AXTextField" | "AXTextArea" | "AXComboBox" | "AXSearchField" | "AXSecureTextField"
    );
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
    if native == "AXScrollArea"
        || native == "AXScrollBar"
        || el.raw("AXVerticalScrollBar").is_some()
    {
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
        value: if secure {
            None
        } else {
            nonempty(el.value_text())
        },
        description: nonempty(el.str_attr("AXDescription")),
        identifier: nonempty(el.str_attr("AXIdentifier")),
        enabled: el.bool_attr("AXEnabled"),
        secure,
        frame: el.frame(),
        caps,
    })
}

/// The window element for a target: explicit window id, else focused, main, first.
pub(super) fn window_root(app: &El, window_id: Option<u64>) -> Option<El> {
    let by_id = |id: u64| {
        app.list("AXWindows")
            .into_iter()
            .find(|w| w.window_id() == Some(id))
    };
    match window_id {
        Some(id) => by_id(id),
        None => app
            .el_attr("AXFocusedWindow")
            .or_else(|| app.el_attr("AXMainWindow"))
            .or_else(|| app.list("AXWindows").into_iter().next()),
    }
}

fn is_editable_role(role: &str) -> bool {
    matches!(
        role,
        "AXTextField" | "AXTextArea" | "AXComboBox" | "AXSearchField" | "AXSecureTextField"
    )
}

/// Deepest focused element, resolved to an editable editor when the focus sits
/// on a container (bounded search, as web/Electron composers require).
pub(super) fn focused_editor(app: &El) -> Option<El> {
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
    if matches!(
        role.as_str(),
        "AXGroup" | "AXScrollArea" | "AXWebArea" | "AXWindow"
    ) {
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
        if c.bool_attr("AXFocused") == Some(true)
            && (is_editable_role(&role) || c.children().is_empty())
        {
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

pub(super) fn semantic(el: &El, action: &Action) -> Result<String> {
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

pub(super) fn menu_press(app: &El, path: &[String]) -> Result<()> {
    let mut cur = app
        .el_attr("AXMenuBar")
        .ok_or_else(|| Error::NotFound("menu bar".into()))?;
    for (i, want) in path.iter().enumerate() {
        let item = cur
            .children()
            .into_iter()
            .find(|c| c.str_attr("AXTitle").as_deref() == Some(want.as_str()))
            .ok_or_else(|| Error::NotFound(format!("menu item {want}")))?;
        if i + 1 == path.len() {
            return item.perform("AXPress");
        }
        cur = item
            .children()
            .into_iter()
            .next()
            .ok_or_else(|| Error::NotFound(format!("submenu {want}")))?;
    }
    Err(Error::Invalid("empty menu path".into()))
}

pub(super) fn field_text(el: &El) -> Option<FieldText> {
    let selection = el.selected_range();
    if let Some(v) = el.raw("AXValue").and_then(|c| cf_to_string(c.0)) {
        if v.len() <= MAX_PROOF_BYTES {
            return Some(FieldText {
                text: v,
                source: Some(TextSource::Value),
                selection,
            });
        }
    }
    el.text_marker_string()
        .filter(|t| t.len() <= MAX_PROOF_BYTES)
        .map(|t| FieldText {
            text: t,
            source: Some(TextSource::TextMarker),
            selection: None,
        })
}

/// HR's four-signal capability decision: a real native text control, not
/// web/rich-editor content, whose value is directly readable and whose
/// selected text is settable.
pub(super) fn native_write_capable(el: &El) -> bool {
    if unsafe { AXIsProcessTrusted() } == 0 || el.is_secure() || el.has_web_area_ancestor() {
        return false;
    }
    el.raw("AXValue").and_then(|c| cf_to_string(c.0)).is_some() && el.settable("AXSelectedText")
}

pub(super) fn native_write(el: &El, text: &str) -> Result<()> {
    el.set_str("AXSelectedText", text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trusted() -> bool {
        let t = unsafe { AXIsProcessTrusted() } != 0;
        if !t {
            eprintln!("skipped: this process is not Accessibility-trusted (live AX test)");
        }
        t
    }

    #[test]
    fn default_timeout_is_unchanged_at_one_and_a_half_seconds() {
        assert_eq!(AX_TIMEOUT, 1.5);
        assert_eq!(DEFAULT_TIMEOUT, Duration::from_millis(1500));
        assert_eq!(AxOptions::default().timeout, DEFAULT_TIMEOUT);
        assert_eq!(timeout_secs(AxOptions::default().timeout), AX_TIMEOUT);
    }

    #[test]
    fn timeout_converts_to_f32_seconds_without_overflow() {
        assert_eq!(timeout_secs(Duration::from_millis(150)), 0.15_f32);
        assert_eq!(timeout_secs(Duration::ZERO), 0.0);
        // Duration::MAX is u64::MAX seconds (about 1.8e19), which is inside f32 range, so it is
        // not clamped: it converts to the nearest f32 and stays finite.
        assert!(timeout_secs(Duration::MAX).is_finite());
        assert_eq!(
            timeout_secs(Duration::MAX),
            Duration::MAX.as_secs_f64() as f32
        );
        assert_eq!(
            AxOptions::with_timeout(Duration::from_secs(3)).timeout,
            Duration::from_secs(3)
        );
    }

    #[test]
    fn hit_test_codes_map_to_found_nothing_or_error() {
        assert!(matches!(hit_test_outcome(0), Ok(true)));
        // kAXErrorNoValue: nothing accessible at that point is not a failure.
        assert!(matches!(hit_test_outcome(-25212), Ok(false)));
        assert!(matches!(
            hit_test_outcome(-25211),
            Err(Error::PermissionDenied(_))
        ));
        // kAXErrorCannotComplete (hung or slow app) must surface, not read as "nothing".
        assert!(matches!(
            hit_test_outcome(-25204),
            Err(Error::Platform(-25204, _))
        ));
        assert!(matches!(hit_test_outcome(-25202), Err(Error::Platform(..))));
    }

    #[test]
    fn untrusted_process_is_refused_not_guessed() {
        if unsafe { AXIsProcessTrusted() } != 0 {
            eprintln!("skipped: process is trusted, so the refusal path cannot be reached");
            return;
        }
        assert!(matches!(system_wide(), Err(Error::PermissionDenied(_))));
        assert!(matches!(
            element_at_position(10.0, 10.0),
            Err(Error::PermissionDenied(_))
        ));
        assert!(matches!(El::app(1), Err(Error::PermissionDenied(_))));
    }

    /// Live: needs Accessibility for the test host. Skips (with a message) otherwise.
    #[test]
    fn live_system_wide_and_hit_test() {
        if !trusted() {
            return;
        }
        system_wide().expect("system-wide element");
        let tight = AxOptions::with_timeout(Duration::from_millis(150));
        // The menu bar's left edge is accessible on every Mac with a visible menu bar.
        let hit = element_at_position_with(8.0, 4.0, &tight).expect("hit test must not error");
        if let Some(el) = hit {
            let pid = el.pid().expect("a hit element belongs to a process");
            assert!(pid > 0);
        }
        // Far outside every display: never an element. AX answers NoValue (None) or an
        // illegal-argument platform error depending on the OS build; both are non-hits.
        assert!(!matches!(
            element_at_position_with(-100_000.0, -100_000.0, &tight),
            Ok(Some(_))
        ));
        // Restore the crate default so later tests in this process see 1.5 s again.
        let _ = system_wide();
    }
}
