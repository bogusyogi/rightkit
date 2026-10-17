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

/// A typed AX attribute value from [`El::attribute`].
#[derive(Clone)]
#[non_exhaustive]
pub enum AttrValue {
    /// The attribute exists but holds no value (`kAXErrorNoValue`).
    NoValue,
    Str(String),
    Bool(bool),
    Int(i64),
    Float(f64),
    /// `AXValue` of type CGPoint (for example `AXPosition`).
    Point {
        x: f64,
        y: f64,
    },
    /// `AXValue` of type CGSize (for example `AXSize`).
    Size {
        w: f64,
        h: f64,
    },
    /// `AXValue` of type CGRect.
    Rect(Rect),
    /// `AXValue` of type CFRange (for example `AXSelectedTextRange`), UTF-16 units.
    Range {
        location: i64,
        length: i64,
    },
    Element(El),
    /// An array; each item is converted the same way (a nested array becomes `Other`).
    List(Vec<AttrValue>),
    /// A CF type this crate does not convert (URL, date, dictionary, text marker). Holds the
    /// CF type id.
    Other(u64),
}

impl std::fmt::Debug for AttrValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoValue => f.write_str("NoValue"),
            Self::Str(s) => write!(f, "Str({s:?})"),
            Self::Bool(b) => write!(f, "Bool({b})"),
            Self::Int(i) => write!(f, "Int({i})"),
            Self::Float(v) => write!(f, "Float({v})"),
            Self::Point { x, y } => write!(f, "Point({x}, {y})"),
            Self::Size { w, h } => write!(f, "Size({w}, {h})"),
            Self::Rect(r) => write!(f, "Rect({r:?})"),
            Self::Range { location, length } => write!(f, "Range({location}, {length})"),
            Self::Element(_) => f.write_str("Element(..)"),
            Self::List(v) => write!(f, "List({v:?})"),
            Self::Other(t) => write!(f, "Other({t})"),
        }
    }
}

/// `AXValueType` codes (`AXValue.h`). Pure so the mapping is tested without a Mac session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AxValueKind {
    Point,
    Size,
    Rect,
    Range,
    Unknown(i32),
}

pub(crate) fn ax_value_kind(code: i32) -> AxValueKind {
    match code {
        1 => AxValueKind::Point,
        2 => AxValueKind::Size,
        3 => AxValueKind::Rect,
        4 => AxValueKind::Range,
        c => AxValueKind::Unknown(c),
    }
}

/// Outcome of `AXUIElementCopyAttributeValue` / `...Names`: `Ok(true)` has a value,
/// `Ok(false)` is `kAXErrorNoValue`, anything else maps through [`ax_err`].
pub(crate) fn attr_query_has_value(code: i32, op: &'static str) -> Result<bool> {
    match code {
        0 => Ok(true),
        -25212 => Ok(false),
        c => Err(ax_err(c, op)),
    }
}

fn attr_value_from_cf(c: &Cf) -> AttrValue {
    unsafe {
        let ty = CFGetTypeID(c.0);
        let other = AttrValue::Other(ty as u64);
        if ty == CFStringGetTypeID() {
            return cf_to_string(c.0).map_or(other, AttrValue::Str);
        }
        if ty == CFBooleanGetTypeID() {
            return cf_to_bool(c.0).map_or(other, AttrValue::Bool);
        }
        if ty == CFNumberGetTypeID() {
            if CFNumberIsFloatType(c.0) != 0 {
                return cf_to_f64(c.0).map_or(other, AttrValue::Float);
            }
            return cf_to_i64(c.0).map_or(other, AttrValue::Int);
        }
        if ty == AXUIElementGetTypeID() {
            return Cf::retain(c.0).map_or(other, |r| AttrValue::Element(El(r)));
        }
        if ty == AXValueGetTypeID() {
            return ax_value_from_cf(c.0).unwrap_or(other);
        }
        if ty == CFArrayGetTypeID() {
            return AttrValue::List(
                (0..CFArrayGetCount(c.0))
                    .filter_map(|i| Cf::retain(CFArrayGetValueAtIndex(c.0, i)))
                    .map(|item| {
                        if CFGetTypeID(item.0) == CFArrayGetTypeID() {
                            AttrValue::Other(CFArrayGetTypeID() as u64)
                        } else {
                            attr_value_from_cf(&item)
                        }
                    })
                    .collect(),
            );
        }
        other
    }
}

unsafe fn ax_value_from_cf(v: CFTypeRef) -> Option<AttrValue> {
    match ax_value_kind(AXValueGetType(v)) {
        AxValueKind::Point => {
            let mut p = CGPoint { x: 0.0, y: 0.0 };
            (AXValueGetValue(v, 1, &mut p as *mut _ as *mut c_void) != 0)
                .then_some(AttrValue::Point { x: p.x, y: p.y })
        }
        AxValueKind::Size => {
            let mut s = CGSize { w: 0.0, h: 0.0 };
            (AXValueGetValue(v, 2, &mut s as *mut _ as *mut c_void) != 0)
                .then_some(AttrValue::Size { w: s.w, h: s.h })
        }
        AxValueKind::Rect => {
            let mut r = CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize { w: 0.0, h: 0.0 },
            };
            (AXValueGetValue(v, 3, &mut r as *mut _ as *mut c_void) != 0).then_some(
                AttrValue::Rect(Rect {
                    x: r.origin.x,
                    y: r.origin.y,
                    w: r.size.w,
                    h: r.size.h,
                }),
            )
        }
        AxValueKind::Range => {
            let mut r = CFRange {
                location: 0,
                length: 0,
            };
            (AXValueGetValue(v, 4, &mut r as *mut _ as *mut c_void) != 0).then_some(
                AttrValue::Range {
                    location: r.location,
                    length: r.length,
                },
            )
        }
        AxValueKind::Unknown(_) => None,
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

    /// Wrap an `AXUIElementRef` the caller already holds, **retaining** it (`CFRetain`).
    ///
    /// The caller keeps its own reference and stays responsible for releasing it; the returned
    /// `El` owns a separate +1 reference and releases it on drop. Returns `None` for a null
    /// pointer. The element's messaging timeout is left as the caller set it (use
    /// [`El::retain_raw_with`] to apply an [`AxOptions`] timeout). Does not check that the
    /// process is Accessibility-trusted; AX calls on the element report that themselves.
    ///
    /// # Safety
    ///
    /// `ptr` must be null or a valid, live `AXUIElementRef` (not another CF type, not a
    /// dangling or already-released pointer) for the duration of this call.
    pub unsafe fn retain_raw(ptr: *const c_void) -> Option<El> {
        Cf::retain(ptr).map(El)
    }

    /// [`El::retain_raw`], then set the element's messaging timeout from `opts`. Note that
    /// this changes the timeout on the caller's element too, since both references name the
    /// same AX object.
    ///
    /// # Safety
    ///
    /// Same contract as [`El::retain_raw`].
    pub unsafe fn retain_raw_with(ptr: *const c_void, opts: &AxOptions) -> Option<El> {
        let el = El::retain_raw(ptr)?;
        el.set_timeout(opts.timeout);
        Some(el)
    }

    /// Wrap an `AXUIElementRef` **taking ownership** of one reference (no `CFRetain`). Use it
    /// for a +1 reference from a `Create`/`Copy` AX call; the returned `El` releases it on
    /// drop, so the caller must not release it again. Returns `None` for a null pointer.
    /// The element's messaging timeout is left unchanged.
    ///
    /// # Safety
    ///
    /// `ptr` must be null or a valid `AXUIElementRef` whose reference the caller owns and
    /// gives up. Passing a borrowed (+0) reference causes an over-release.
    pub unsafe fn from_raw(ptr: *const c_void) -> Option<El> {
        Cf::new(ptr).map(El)
    }

    /// The underlying `AXUIElementRef`, borrowed: valid only while this `El` is alive, and
    /// not retained. Retain it (`CFRetain`, or [`El::retain_raw`]) to keep it longer.
    pub fn as_raw(&self) -> *const c_void {
        (self.0).0
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
    /// Read one attribute with its full type and its error. Read-only attribute query.
    ///
    /// **Not gated by the interaction switches.** The kill switch (`policy::kill`), `Policy`,
    /// the allowlist, admission and leases guard *effects* (input, writes, actions, focus
    /// changes). This call is `AXUIElementCopyAttributeValue` only: it changes nothing in the
    /// target and posts no event, so it works while interaction is disabled (for example to
    /// show where a window is while a kill switch is engaged). It still needs the Accessibility
    /// grant (`PermissionDenied` otherwise) and honours the element's messaging timeout.
    /// `El::str_attr`, `bool_attr`, `f64_attr`, `el_attr`, `list`, `frame` and `pid` read the
    /// same way (they are also not gated) but collapse every failure to `None`.
    ///
    /// A missing attribute is `Err(Error::Unsupported)`; an attribute that exists but holds no
    /// value is `Ok(AttrValue::NoValue)`.
    pub fn attribute(&self, name: &str) -> Result<AttrValue> {
        require_trusted()?;
        let k = Cf::string(name);
        let mut out: CFTypeRef = std::ptr::null();
        let code = unsafe { AXUIElementCopyAttributeValue((self.0).0, k.0, &mut out) };
        if !attr_query_has_value(code, "attribute")? {
            return Ok(AttrValue::NoValue);
        }
        let c = Cf::new(out).ok_or(Error::Unsupported("attribute value"))?;
        Ok(attr_value_from_cf(&c))
    }
    /// Attribute names the element supports (`AXUIElementCopyAttributeNames`). Read-only and
    /// not gated by the interaction switches, like [`El::attribute`].
    pub fn attribute_names(&self) -> Result<Vec<String>> {
        require_trusted()?;
        let mut out: CFTypeRef = std::ptr::null();
        let code = unsafe { AXUIElementCopyAttributeNames((self.0).0, &mut out) };
        if !attr_query_has_value(code, "attribute_names")? {
            return Ok(vec![]);
        }
        let a = Cf::new(out).ok_or(Error::Unsupported("attribute names"))?;
        unsafe {
            Ok((0..CFArrayGetCount(a.0))
                .filter_map(|i| cf_to_string(CFArrayGetValueAtIndex(a.0, i)))
                .collect())
        }
    }
    /// Read several attributes, each with its own result. Read-only and not gated.
    pub fn attributes(&self, names: &[&str]) -> Vec<(String, Result<AttrValue>)> {
        names
            .iter()
            .map(|n| ((*n).to_string(), self.attribute(n)))
            .collect()
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
    fn set_ax_value(
        &self,
        attr: &str,
        ty: i32,
        value: *const c_void,
        op: &'static str,
    ) -> Result<()> {
        require_trusted()?;
        let k = Cf::string(attr);
        let v =
            Cf::new(unsafe { AXValueCreate(ty, value) }).ok_or(Error::Unsupported("ax value"))?;
        match unsafe { AXUIElementSetAttributeValue((self.0).0, k.0, v.0) } {
            0 => Ok(()),
            c => Err(ax_err(c, op)),
        }
    }
    /// Write `AXPosition` (global display coordinates, top-left origin, points). Moves a window.
    /// The app may clamp or refuse; read [`El::frame`] back to see what it did.
    pub fn set_point(&self, x: f64, y: f64) -> Result<()> {
        let pt = CGPoint { x, y };
        self.set_ax_value(
            "AXPosition",
            1,
            &pt as *const _ as *const c_void,
            "set_point",
        )
    }
    /// Write `AXSize` (points). Resizes a window. The app may clamp it to its own limits.
    pub fn set_size(&self, w: f64, h: f64) -> Result<()> {
        let sz = CGSize { w, h };
        self.set_ax_value("AXSize", 2, &sz as *const _ as *const c_void, "set_size")
    }
    /// Write position then size: window maximise, move and restore. Stops at the first error, so
    /// a failure on the size leaves the window moved. Read [`El::frame`] back to confirm.
    pub fn set_frame(&self, r: Rect) -> Result<()> {
        self.set_point(r.x, r.y)?;
        self.set_size(r.w, r.h)
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

    #[test]
    fn raw_constructors_refuse_null() {
        unsafe {
            assert!(El::retain_raw(std::ptr::null()).is_none());
            assert!(El::retain_raw_with(std::ptr::null(), &AxOptions::default()).is_none());
            assert!(El::from_raw(std::ptr::null()).is_none());
        }
    }

    #[test]
    fn retain_raw_leaves_the_callers_reference_valid() {
        if !trusted() {
            return;
        }
        let owned = El::app(std::process::id() as i32).expect("own app element");
        let raw = owned.as_raw();
        // retain_raw takes its own +1: dropping the wrapper must not release the caller's.
        let wrapped = unsafe { El::retain_raw(raw) }.expect("wrap");
        assert_eq!(wrapped.as_raw(), raw);
        drop(wrapped);
        // The caller's reference is still live: reads through it do not crash.
        let _ = owned.str_attr("AXRole");
    }

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

#[cfg(test)]
mod ax_value_tests {
    use super::*;

    /// `set_point` / `set_size` build their `AXValue` with these type codes and structs; check
    /// they read back as the same values (no app or Accessibility grant needed).
    #[test]
    fn ax_point_and_size_values_round_trip() {
        let pt = CGPoint { x: 12.5, y: -3.0 };
        let v = Cf::new(unsafe { AXValueCreate(1, &pt as *const _ as *const c_void) })
            .expect("point value");
        let mut back = CGPoint { x: 0.0, y: 0.0 };
        assert!(unsafe { AXValueGetValue(v.0, 1, &mut back as *mut _ as *mut c_void) } != 0);
        assert_eq!((back.x, back.y), (12.5, -3.0));

        let sz = CGSize { w: 800.0, h: 600.0 };
        let v = Cf::new(unsafe { AXValueCreate(2, &sz as *const _ as *const c_void) })
            .expect("size value");
        let mut back = CGSize { w: 0.0, h: 0.0 };
        assert!(unsafe { AXValueGetValue(v.0, 2, &mut back as *mut _ as *mut c_void) } != 0);
        assert_eq!((back.w, back.h), (800.0, 600.0));
    }
}

#[cfg(test)]
mod attr_query_tests {
    use super::*;

    #[test]
    fn ax_value_type_codes_map() {
        assert_eq!(ax_value_kind(1), AxValueKind::Point);
        assert_eq!(ax_value_kind(2), AxValueKind::Size);
        assert_eq!(ax_value_kind(3), AxValueKind::Rect);
        assert_eq!(ax_value_kind(4), AxValueKind::Range);
        assert_eq!(ax_value_kind(0), AxValueKind::Unknown(0));
        assert_eq!(ax_value_kind(5), AxValueKind::Unknown(5));
    }

    #[test]
    fn query_outcome_separates_no_value_from_errors() {
        assert!(matches!(attr_query_has_value(0, "q"), Ok(true)));
        assert!(matches!(attr_query_has_value(-25212, "q"), Ok(false)));
        assert!(matches!(
            attr_query_has_value(-25205, "q"),
            Err(Error::Unsupported("q"))
        ));
        assert!(matches!(
            attr_query_has_value(-25211, "q"),
            Err(Error::PermissionDenied("Accessibility"))
        ));
        assert!(matches!(
            attr_query_has_value(-25204, "q"),
            Err(Error::Platform(-25204, "q"))
        ));
    }

    #[test]
    fn point_value_converts_to_attr_value() {
        let pt = CGPoint { x: 4.0, y: 9.5 };
        let v = Cf::new(unsafe { AXValueCreate(1, &pt as *const _ as *const c_void) }).expect("v");
        assert!(matches!(
            attr_value_from_cf(&v),
            AttrValue::Point { x, y } if x == 4.0 && y == 9.5
        ));
    }
}
