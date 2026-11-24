//! macOS half of verified text-range selection: Accessibility `AXValue` and
//! `AXSelectedTextRange`.
//!
//! See `crate::text_select` for the offset unit (UTF-16 units, which is what `AXSelectedTextRange`
//! (a `CFRange`) counts for a standard text control), the proof order and the error rules. This
//! file only reads and sets the selection; it never types and never writes `AXValue`.
//!
//! A control qualifies only when it exposes a string `AXValue` (the whole document), a readable
//! `AXSelectedTextRange` that is settable, a role in an allowlist (`AXTextField`, `AXTextArea`,
//! `AXComboBox`; subrole none or `AXSearchField`), and exactly one selection range
//! (`AXSelectedTextRanges` must hold one range equal to `AXSelectedTextRange` where the app offers
//! it). Secure, protected or unknown roles and subroles are refused (`AXWebArea` included).
//! The role is required. An explicitly absent subrole is allowed. Controls without the
//! required text and selection attributes (many web and Electron fields) fail with
//! `Unsupported`; the caller falls back.
//!
//! # Typed reads
//!
//! Every attribute read is typed and three-way: a value of the expected CF type, explicit absence
//! (`kAXErrorAttributeUnsupported` or `kAXErrorNoValue`), or a refusal (any other error code or
//! the wrong CF type). A failed read is never taken as "absent", so a flaky `AXSubrole` or
//! `AXProtectedContent` read cannot let a protected field through. Strings are read as raw
//! UTF-16 (`CFStringGetCharacters`) and decoded strictly; a malformed string is refused. Ranges
//! are `AXValue`s checked with `AXValueGetTypeID` and `AXValueGetType` before they are unpacked.
//!
//! # Identity
//!
//! A [`TextTarget`] retains the `AXUIElement` and its `AXWindow`. Every check requires that the
//! system-wide `AXFocusedUIElement` is `CFEqual` to the retained element and that the element's
//! `AXWindow` is `CFEqual` to the retained window. The [`ControlIdentity`] it hands out carries a
//! per-target token only: two targets never compare equal, and no hash of the element is used as
//! identity.
//!
//! # Shared readers
//!
//! Exact string and range readers are available within the crate for desktop editor proofs.
//!
//! Needs the Accessibility grant (`PermissionDenied` otherwise). Set a per-element timeout with
//! `ax::El::set_timeout` before use; an app that stops answering otherwise blocks the call.

use super::ax::{self, El};
use super::ffi::*;
use crate::text_select::{
    select_and_verify, snapshot, ControlIdentity, DocumentSnapshot, ObservedSelection,
    SelectionProof, TextSelectError, TextSelectResult, TextSurface, DEFAULT_LIMIT_UTF16,
};
use std::ffi::c_void;
use std::sync::atomic::{AtomicI64, Ordering};

#[allow(clashing_extern_declarations)]
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringGetCharacters(s: CFTypeRef, range: CFRange, buffer: *mut u16);
}

/// `kAXValueTypeCFRange`.
const AX_VALUE_CF_RANGE: i32 = 4;
/// `kAXErrorAttributeUnsupported`, `kAXErrorNoValue`: the only codes that mean "absent".
const AX_ABSENT: [i32; 2] = [-25205, -25212];

/// Roles that can hold user-editable text. Anything else (including `AXWebArea`, `AXStaticText`,
/// unknown or missing roles) is refused: whether a web document's selection is the user's field
/// is not provable from here.
const ALLOWED_ROLES: [&str; 3] = ["AXTextField", "AXTextArea", "AXComboBox"];
/// Subroles allowed on an allowed role. No subrole is also allowed. Every other subrole,
/// protected ones included, is refused.
const ALLOWED_SUBROLES: [&str; 1] = ["AXSearchField"];

fn require_trusted() -> TextSelectResult<()> {
    if unsafe { AXIsProcessTrusted() } == 0 {
        Err(TextSelectError::PermissionDenied("Accessibility"))
    } else {
        Ok(())
    }
}

fn map_code(code: i32, op: &'static str) -> TextSelectError {
    match code {
        // attribute/action unsupported, no value, not implemented, parameterized unsupported
        -25205 | -25206 | -25212 | -25208 | -25207 => TextSelectError::Unsupported(op),
        -25211 => TextSelectError::PermissionDenied("Accessibility"),
        -25202 => TextSelectError::ElementUnavailable(op),
        c => TextSelectError::Platform(i64::from(c), op),
    }
}

/// Typed copy of attribute `name`: `Ok(None)` only for an explicit absent code, `Ok(Some)` for a
/// value, `Err` for everything else.
fn copy_typed(el: &El, name: &'static str) -> TextSelectResult<Option<Cf>> {
    match el.copy_attribute_raw(name) {
        Ok(cf) => Ok(Some(cf)),
        Err(code) if AX_ABSENT.contains(&code) => Ok(None),
        Err(code) => Err(map_code(code, name)),
    }
}

fn exact_element_attr(el: &El, name: &'static str) -> TextSelectResult<Option<El>> {
    let Some(value) = copy_typed(el, name)? else {
        return Ok(None);
    };
    if unsafe { CFGetTypeID(value.0) != AXUIElementGetTypeID() } {
        return Err(TextSelectError::Unreadable(name));
    }
    Ok(Some(El(value)))
}

fn focused_element() -> TextSelectResult<El> {
    let system = ax::system_wide().map_err(|_| TextSelectError::NotFocused)?;
    exact_element_attr(&system, "AXFocusedUIElement")?.ok_or(TextSelectError::NotFocused)
}

/// Exact UTF-16 text of a CFString, bounded before allocation, decoded strictly.
fn exact_cfstring(cf: &Cf, limit_utf16: usize) -> TextSelectResult<String> {
    unsafe {
        if CFGetTypeID(cf.0) != CFStringGetTypeID() {
            return Err(TextSelectError::Unreadable("attribute is not a string"));
        }
        let length = CFStringGetLength(cf.0);
        if length < 0 || length as usize > limit_utf16 {
            return Err(TextSelectError::TooLarge { limit_utf16 });
        }
        let mut units = vec![0u16; length as usize];
        CFStringGetCharacters(
            cf.0,
            CFRange {
                location: 0,
                length,
            },
            units.as_mut_ptr(),
        );
        String::from_utf16(&units).map_err(|_| TextSelectError::Unreadable("malformed UTF-16 text"))
    }
}

/// A string attribute as raw UTF-16: `Ok(None)` when explicitly absent, `Err` on any other
/// failure, wrong type, malformed UTF-16 or more than `limit_utf16` units. Shared with the
/// desktop replacement proof.
pub(crate) fn exact_string_attr(
    el: &El,
    name: &'static str,
    limit_utf16: usize,
) -> TextSelectResult<Option<String>> {
    match copy_typed(el, name)? {
        None => Ok(None),
        Some(cf) => exact_cfstring(&cf, limit_utf16).map(Some),
    }
}

/// An `AXValue` of kind CFRange as `(location, length)`, checked with `AXValueGetTypeID` and
/// `AXValueGetType` before it is unpacked; negative components are refused.
fn range_from_cf(item: CFTypeRef) -> TextSelectResult<(usize, usize)> {
    unsafe {
        if item.is_null()
            || CFGetTypeID(item) != AXValueGetTypeID()
            || AXValueGetType(item) != AX_VALUE_CF_RANGE
        {
            return Err(TextSelectError::Unreadable(
                "attribute is not an AXValue range",
            ));
        }
        let mut r = CFRange {
            location: 0,
            length: 0,
        };
        if AXValueGetValue(
            item,
            AX_VALUE_CF_RANGE,
            &mut r as *mut CFRange as *mut c_void,
        ) == 0
            || r.location < 0
            || r.length < 0
        {
            return Err(TextSelectError::Unreadable("range value unreadable"));
        }
        Ok((r.location as usize, r.length as usize))
    }
}

/// A CFRange attribute as `(location, length)` in UTF-16 units; absent is `Ok(None)`. Shared
/// with the desktop replacement proof.
pub(crate) fn exact_range_attr(
    el: &El,
    name: &'static str,
) -> TextSelectResult<Option<(usize, usize)>> {
    match copy_typed(el, name)? {
        None => Ok(None),
        Some(cf) => range_from_cf(cf.0).map(Some),
    }
}

fn typed_bool(el: &El, name: &'static str) -> TextSelectResult<Option<bool>> {
    match copy_typed(el, name)? {
        None => Ok(None),
        Some(cf) => unsafe {
            if CFGetTypeID(cf.0) != CFBooleanGetTypeID() {
                return Err(TextSelectError::Unreadable("attribute is not a boolean"));
            }
            Ok(Some(CFBooleanGetValue(cf.0) != 0))
        },
    }
}

fn check_role(el: &El) -> TextSelectResult<()> {
    let role = exact_string_attr(el, "AXRole", 256)?
        .ok_or(TextSelectError::Unsupported("AXRole is absent"))?;
    // A failed read of subrole or protected-content is a refusal, never "absent".
    let subrole = exact_string_attr(el, "AXSubrole", 256)?;
    let protected = typed_bool(el, "AXProtectedContent")?;
    if role == "AXSecureTextField"
        || subrole.as_deref() == Some("AXSecureTextField")
        || protected == Some(true)
    {
        return Err(TextSelectError::Secure);
    }
    if !ALLOWED_ROLES.contains(&role.as_str()) {
        return Err(TextSelectError::Unsupported(
            "role is not an allowed text role",
        ));
    }
    match subrole.as_deref() {
        None => Ok(()),
        Some(sub) if ALLOWED_SUBROLES.contains(&sub) => Ok(()),
        Some(_) => Err(TextSelectError::Unsupported(
            "subrole is not an allowed text subrole",
        )),
    }
}

/// Where the app offers `AXSelectedTextRanges`, require exactly one range equal to the single
/// `AXSelectedTextRange`. An explicitly absent attribute is judged by the single range alone;
/// any other failure is unreadable.
fn require_single_range(el: &El, range: (usize, usize)) -> TextSelectResult<()> {
    let Some(ranges) = copy_typed(el, "AXSelectedTextRanges")? else {
        return Ok(());
    };
    unsafe {
        if CFGetTypeID(ranges.0) != CFArrayGetTypeID() || CFArrayGetCount(ranges.0) != 1 {
            return Err(TextSelectError::Unreadable(
                "selection is not exactly one range",
            ));
        }
        if range_from_cf(CFArrayGetValueAtIndex(ranges.0, 0))? != range {
            return Err(TextSelectError::Unreadable(
                "AXSelectedTextRanges differs from AXSelectedTextRange",
            ));
        }
    }
    Ok(())
}

/// Per-process counter for target tokens. A token is meaningful only inside the target that
/// minted it.
static NEXT_TARGET_TOKEN: AtomicI64 = AtomicI64::new(1);

struct Surface<'a> {
    el: &'a El,
    window: &'a El,
    token: i64,
}

impl TextSurface for Surface<'_> {
    fn identity(&self) -> TextSelectResult<ControlIdentity> {
        require_trusted()?;
        check_role(self.el)?;
        let focused = focused_element()?;
        if !focused.same(self.el) {
            return Err(TextSelectError::NotFocused);
        }
        match exact_element_attr(self.el, "AXWindow")? {
            Some(w) if w.same(self.window) => {}
            _ => return Err(TextSelectError::IdentityChanged),
        }
        let pid = self
            .el
            .pid()
            .ok_or(TextSelectError::Unreadable("process id"))?;
        let mut window_number = 0;
        let result = unsafe { _AXUIElementGetWindow(self.window.as_raw(), &mut window_number) };
        let window_id = (result == 0 && window_number != 0).then_some(u64::from(window_number));
        ControlIdentity::new(pid, window_id, vec![self.token], None)
            .ok_or(TextSelectError::Unreadable("target token"))
    }

    fn read_text(&self, limit_utf16: usize) -> TextSelectResult<String> {
        require_trusted()?;
        exact_string_attr(self.el, "AXValue", limit_utf16)?
            .ok_or(TextSelectError::Unsupported("AXValue is absent"))
    }

    fn read_selection(&self, limit_utf16: usize) -> TextSelectResult<ObservedSelection> {
        require_trusted()?;
        let range = exact_range_attr(self.el, "AXSelectedTextRange")?
            .ok_or(TextSelectError::Unreadable("AXSelectedTextRange is absent"))?;
        require_single_range(self.el, range)?;
        let (start, len) = range;
        let end = start
            .checked_add(len)
            .ok_or(TextSelectError::Unreadable("AXSelectedTextRange overflow"))?;
        if end > limit_utf16 {
            return Err(TextSelectError::TooLarge { limit_utf16 });
        }
        // The same exact reader as the document. A caret may have no AXSelectedText (explicitly
        // absent); any other missing or failed read is unreadable.
        let text = match exact_string_attr(self.el, "AXSelectedText", limit_utf16)? {
            Some(t) => t,
            None if len == 0 => String::new(),
            None => return Err(TextSelectError::Unreadable("AXSelectedText is absent")),
        };
        Ok(ObservedSelection { text, start, end })
    }

    fn apply_selection(
        &self,
        _document: &str,
        start: usize,
        end: usize,
        _limit_utf16: usize,
    ) -> TextSelectResult<()> {
        require_trusted()?;
        if !self.el.settable("AXSelectedTextRange") {
            return Err(TextSelectError::Unsupported(
                "AXSelectedTextRange is not settable",
            ));
        }
        let range = CFRange {
            location: start as CFIndex,
            length: end.saturating_sub(start) as CFIndex,
        };
        let value = Cf::new(unsafe {
            AXValueCreate(AX_VALUE_CF_RANGE, &range as *const CFRange as *const c_void)
        })
        .ok_or(TextSelectError::Unsupported("AXValue(CFRange)"))?;
        let key = Cf::string("AXSelectedTextRange");
        match unsafe { AXUIElementSetAttributeValue((self.el.0).0, key.0, value.0) } {
            0 => Ok(()),
            code => Err(map_code(code, "set AXSelectedTextRange")),
        }
    }
}

/// A focused text control, retained, with its window and the identity captured when it was
/// made. Every call re-checks role, focus (`CFEqual` with the system-wide focused element) and
/// window (`CFEqual`).
pub struct TextTarget {
    el: El,
    window: El,
    token: i64,
    baseline: ControlIdentity,
    limit_utf16: usize,
}

impl TextTarget {
    fn surface(&self) -> Surface<'_> {
        Surface {
            el: &self.el,
            window: &self.window,
            token: self.token,
        }
    }

    /// The system-wide focused element, which must be an allowed, non-secure text control.
    pub fn from_focused() -> TextSelectResult<Self> {
        require_trusted()?;
        Self::from_element(focused_element()?)
    }

    /// `el`, which must be focused, an allowed non-secure text role with a string `AXValue`. A
    /// control without a settable `AXSelectedTextRange` is refused with `Unsupported`, before
    /// anything is read.
    pub fn from_element(el: El) -> TextSelectResult<Self> {
        require_trusted()?;
        if !el.settable("AXSelectedTextRange") {
            return Err(TextSelectError::Unsupported(
                "AXSelectedTextRange is not settable",
            ));
        }
        let window = exact_element_attr(&el, "AXWindow")?
            .ok_or(TextSelectError::Unsupported("AXWindow is absent"))?;
        let token = NEXT_TARGET_TOKEN.fetch_add(1, Ordering::Relaxed);
        let baseline = Surface {
            el: &el,
            window: &window,
            token,
        }
        .identity()?;
        Ok(TextTarget {
            el,
            window,
            token,
            baseline,
            limit_utf16: DEFAULT_LIMIT_UTF16,
        })
    }

    /// Cap, in UTF-16 units, for the document this target will read (default
    /// [`DEFAULT_LIMIT_UTF16`]). A larger document fails with `TooLarge`.
    pub fn with_limit_utf16(mut self, limit_utf16: usize) -> Self {
        self.limit_utf16 = limit_utf16;
        self
    }

    /// The identity captured when the target was made. Comparable only with identities of this
    /// target.
    pub fn control_identity(&self) -> &ControlIdentity {
        &self.baseline
    }

    /// Full document text, selection and identity now; the identity must equal the captured one.
    pub fn snapshot(&self) -> TextSelectResult<DocumentSnapshot> {
        let snap = snapshot(&self.surface(), self.limit_utf16)?;
        if snap.control_identity != self.baseline {
            return Err(TextSelectError::IdentityChanged);
        }
        Ok(snap)
    }

    /// Select `start..end` (UTF-16 units) of `expected_full_text` and prove it; see
    /// `crate::text_select::select_and_verify`. Never types. Authorizes no later write.
    pub fn select_and_verify(
        &self,
        expected_full_text: &str,
        start: usize,
        end: usize,
    ) -> TextSelectResult<SelectionProof> {
        select_and_verify(
            &self.surface(),
            &self.baseline,
            expected_full_text,
            start,
            end,
            self.limit_utf16,
        )
    }
}
