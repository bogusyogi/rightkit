//! Windows half of verified text-range selection: UI Automation `TextPattern`.
//!
//! See `crate::text_select` for the offset unit (UTF-16 code units of the document text as UIA
//! returns it), the proof order and the error rules. This file only reads and selects; it never
//! types and never writes the document.
//!
//! How a span is selected: clone the `DocumentRange`, collapse it to the document start, move its
//! End by N UIA `Character` units and then its Start by M units, and MEASURE that candidate (exact
//! UTF-16 offsets from the document start, and its text) without selecting it. UIA does not define
//! what one `Character` is for a given provider (UTF-16 unit, scalar, grapheme, `\r\n` pair),
//! so N and M are tried first as UTF-16 counts and then as Unicode scalar counts when those differ;
//! both trials are side-effect free. Only a candidate already proven equal to the requested span
//! is selected, with exactly one `Select()`, and the selection is read back once by the shared
//! `select_and_verify`. A provider whose `Character` unit matches neither count fails with
//! `SelectionMismatch` before anything was selected.
//!
//! Threading: call on a COM thread (`uia::ensure_com` runs on entry) and wrap in
//! `win::with_deadline_mutation` when the target may hang. Needs the `Win32_UI_Accessibility`
//! feature of `windows`, already enabled for this crate.

use super::uia::El;
use crate::text_select::{
    scalars_before, select_and_verify, snapshot, utf16_len, utf16_slice, ControlIdentity,
    DocumentSnapshot,
    ObservedSelection, SelectionProof, TextSelectError, TextSelectResult, TextSurface,
    DEFAULT_LIMIT_UTF16,
};
use crate::uia_text::get_text_request_len;
use windows::Win32::UI::Accessibility::{
    IUIAutomationTextPattern, IUIAutomationTextRange, TextPatternRangeEndpoint,
    TextPatternRangeEndpoint_End, TextPatternRangeEndpoint_Start, TextUnit_Character,
    UIA_TextPatternId,
};

fn map_err(e: windows::core::Error, op: &'static str) -> TextSelectError {
    let code = e.code().0;
    match code as u32 {
        // UIA_E_NOTSUPPORTED, UIA_E_INVALIDOPERATION, E_NOTIMPL
        0x8004_0201 | 0x8013_1509 | 0x8000_4001 => TextSelectError::Unsupported(op),
        // UIA_E_ELEMENTNOTAVAILABLE, UIA_E_ELEMENTNOTENABLED
        0x8004_0200 | 0x8004_0206 => TextSelectError::ElementUnavailable(op),
        // E_ACCESSDENIED: UIPI, the target is elevated and we are not
        0x8007_0005 => TextSelectError::PermissionDenied("target is elevated (UIPI)"),
        _ => TextSelectError::Platform(i64::from(code), op),
    }
}

fn text_pattern(el: &El) -> TextSelectResult<IUIAutomationTextPattern> {
    unsafe { el.0.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId) }.map_err(
        |e| match map_err(e, "GetCurrentPattern(TextPattern)") {
            // E_NOINTERFACE and friends: the control simply has no TextPattern.
            TextSelectError::Platform(..) => TextSelectError::Unsupported("no TextPattern"),
            other => other,
        },
    )
}

fn range_text(range: &IUIAutomationTextRange, limit_utf16: usize) -> TextSelectResult<String> {
    let text = unsafe { range.GetText(get_text_request_len(limit_utf16)) }
        .map_err(|e| map_err(e, "TextRange.GetText"))?;
    // Raw UTF-16 units, decoded strictly: `BSTR`'s `Display` replaces a lone surrogate with
    // U+FFFD, which keeps the length but changes the text, so a malformed string is refused
    // instead.
    let units = text.as_wide();
    if units.len() > limit_utf16 {
        return Err(TextSelectError::TooLarge { limit_utf16 });
    }
    String::from_utf16(units).map_err(|_| TextSelectError::Unreadable("malformed UTF-16 text"))
}

/// UTF-16 units of document text before `sel`'s `edge` endpoint.
fn offset_of(
    doc: &IUIAutomationTextRange,
    sel: &IUIAutomationTextRange,
    edge: TextPatternRangeEndpoint,
    limit_utf16: usize,
) -> TextSelectResult<usize> {
    let pre = unsafe { doc.Clone() }.map_err(|e| map_err(e, "TextRange.Clone"))?;
    unsafe { pre.MoveEndpointByRange(TextPatternRangeEndpoint_End, sel, edge) }
        .map_err(|e| map_err(e, "TextRange.MoveEndpointByRange"))?;
    Ok(utf16_len(&range_text(&pre, limit_utf16)?))
}

/// Exact UTF-16 offsets (from the document start) and text of `range`, read without changing
/// anything. The two offsets and the text length must agree.
fn measure(
    doc: &IUIAutomationTextRange,
    range: &IUIAutomationTextRange,
    limit_utf16: usize,
) -> TextSelectResult<ObservedSelection> {
    let text = range_text(range, limit_utf16)?;
    let start = offset_of(doc, range, TextPatternRangeEndpoint_Start, limit_utf16)?;
    let end = offset_of(doc, range, TextPatternRangeEndpoint_End, limit_utf16)?;
    if start.checked_add(utf16_len(&text)) != Some(end) {
        return Err(TextSelectError::Unreadable(
            "range offsets disagree with its text",
        ));
    }
    Ok(ObservedSelection { text, start, end })
}

struct Surface<'a>(&'a El);

impl TextSurface for Surface<'_> {
    fn identity(&self) -> TextSelectResult<ControlIdentity> {
        super::uia::ensure_com();
        match self.0.password_state() {
            Some(false) => {}
            Some(true) => return Err(TextSelectError::Secure),
            None => return Err(TextSelectError::Unreadable("password state")),
        }
        text_pattern(self.0)?;
        if !self.0.has_focus() {
            return Err(TextSelectError::NotFocused);
        }
        let pid = self
            .0
            .pid()
            .ok_or(TextSelectError::Unreadable("process id"))?;
        // The UIA runtime id is the element's identity; pid and windows are diagnostic only.
        let runtime_id: Vec<i64> = self
            .0
            .runtime_id()
            .ok_or(TextSelectError::Unreadable("runtime id"))?
            .into_iter()
            .map(i64::from)
            .collect();
        ControlIdentity::new(
            pid,
            self.0.top_hwnd().map(|h| h as u64),
            runtime_id,
            self.0.native_hwnd().map(|h| h as u64),
        )
        .ok_or(TextSelectError::Unreadable("runtime id"))
    }

    fn read_text(&self, limit_utf16: usize) -> TextSelectResult<String> {
        let tp = text_pattern(self.0)?;
        let doc = unsafe { tp.DocumentRange() }.map_err(|e| map_err(e, "DocumentRange"))?;
        range_text(&doc, limit_utf16)
    }

    fn read_selection(&self, limit_utf16: usize) -> TextSelectResult<ObservedSelection> {
        let tp = text_pattern(self.0)?;
        let doc = unsafe { tp.DocumentRange() }.map_err(|e| map_err(e, "DocumentRange"))?;
        let sels = unsafe { tp.GetSelection() }.map_err(|e| map_err(e, "GetSelection"))?;
        match unsafe { sels.Length() }.map_err(|e| map_err(e, "GetSelection.Length"))? {
            1 => {}
            0 => return Err(TextSelectError::Unreadable("no selection range")),
            _ => return Err(TextSelectError::Unreadable("multi-range selection")),
        }
        let sel = unsafe { sels.GetElement(0) }.map_err(|e| map_err(e, "GetSelection.Element"))?;
        measure(&doc, &sel, limit_utf16)
    }

    /// Builds each candidate range and measures it BEFORE any `Select()`: the candidate's exact
    /// UTF-16 offsets and text must equal the request (`start..end` of `document`). Counting by
    /// UTF-16 units is tried first, then by scalar values when those differ; both trials are
    /// side-effect free. `Select()` is then called exactly once, on the proven candidate. The
    /// shared `select_and_verify` reads the selection back once afterwards.
    fn apply_selection(
        &self,
        document: &str,
        start: usize,
        end: usize,
        limit_utf16: usize,
    ) -> TextSelectResult<()> {
        let tp = text_pattern(self.0)?;
        let doc = unsafe { tp.DocumentRange() }.map_err(|e| map_err(e, "DocumentRange"))?;
        let want = utf16_slice(document, start, end)
            .ok_or(TextSelectError::SplitsSurrogatePair { offset: start })?;

        let mut counts = vec![(start, end)];
        if let (Some(s), Some(e)) = (scalars_before(document, start), scalars_before(document, end))
        {
            if (s, e) != (start, end) {
                counts.push((s, e));
            }
        }

        let mut last = TextSelectError::SelectionMismatch {
            requested: (start, end),
            observed: None,
            text_matched: false,
        };
        for (n_start, n_end) in counts {
            let (Ok(n_start), Ok(n_end)) = (i32::try_from(n_start), i32::try_from(n_end)) else {
                return Err(TextSelectError::TooLarge { limit_utf16 });
            };
            let candidate = match build_candidate(&doc, n_start, n_end) {
                Ok(c) => c,
                Err(e @ TextSelectError::SelectionMismatch { .. }) => {
                    last = e;
                    continue;
                }
                Err(e) => return Err(e),
            };
            let measured = measure(&doc, &candidate, limit_utf16)?;
            if measured.start == start && measured.end == end && measured.text == want {
                // The only side effect of this call, on a range already proven equal to the span.
                return unsafe { candidate.Select() }.map_err(|e| map_err(e, "TextRange.Select"));
            }
            last = TextSelectError::SelectionMismatch {
                requested: (start, end),
                observed: Some((measured.start, measured.end)),
                text_matched: measured.text == want,
            };
        }
        Err(last)
    }
}

/// A candidate range: a clone of the document range collapsed to its start, End moved by
/// `n_end` Character units then Start by `n_start`, each required to move exactly that far.
/// Nothing is selected.
fn build_candidate(
    doc: &IUIAutomationTextRange,
    n_start: i32,
    n_end: i32,
) -> TextSelectResult<IUIAutomationTextRange> {
    let range = unsafe { doc.Clone() }.map_err(|e| map_err(e, "TextRange.Clone"))?;
    unsafe {
        range.MoveEndpointByRange(
            TextPatternRangeEndpoint_End,
            doc,
            TextPatternRangeEndpoint_Start,
        )
    }
    .map_err(|e| map_err(e, "TextRange.MoveEndpointByRange"))?;
    let mismatch = TextSelectError::SelectionMismatch {
        requested: (n_start.max(0) as usize, n_end.max(0) as usize),
        observed: None,
        text_matched: false,
    };
    if n_end > 0 {
        let moved = unsafe {
            range.MoveEndpointByUnit(TextPatternRangeEndpoint_End, TextUnit_Character, n_end)
        }
        .map_err(|e| map_err(e, "TextRange.MoveEndpointByUnit(End)"))?;
        if moved != n_end {
            return Err(mismatch);
        }
    }
    if n_start > 0 {
        let moved = unsafe {
            range.MoveEndpointByUnit(TextPatternRangeEndpoint_Start, TextUnit_Character, n_start)
        }
        .map_err(|e| map_err(e, "TextRange.MoveEndpointByUnit(Start)"))?;
        if moved != n_start {
            return Err(mismatch);
        }
    }
    Ok(range)
}

/// A focused text control, retained, with the identity captured when it was made.
///
/// Every call re-checks that the control is still the one captured, still focused and not a
/// password field. Provider calls can block: wrap them in `win::with_deadline_mutation`.
pub struct TextTarget {
    el: El,
    baseline: ControlIdentity,
    limit_utf16: usize,
}

impl TextTarget {
    /// The currently focused element, which must be a non-password control with a `TextPattern`.
    pub fn from_focused() -> TextSelectResult<Self> {
        super::uia::ensure_com();
        let el = super::uia::focused_element().ok_or(TextSelectError::NotFocused)?;
        Self::from_element(el)
    }

    /// `el`, which must be focused, non-password and have a `TextPattern`
    /// (`TextSelectError::Unsupported` otherwise).
    pub fn from_element(el: El) -> TextSelectResult<Self> {
        let baseline = Surface(&el).identity()?;
        Ok(TextTarget {
            el,
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

    /// The identity captured when the target was made.
    pub fn control_identity(&self) -> &ControlIdentity {
        &self.baseline
    }

    /// Full document text, selection and identity now; the identity must equal the captured one.
    pub fn snapshot(&self) -> TextSelectResult<DocumentSnapshot> {
        let snap = snapshot(&Surface(&self.el), self.limit_utf16)?;
        if snap.control_identity != self.baseline {
            return Err(TextSelectError::IdentityChanged);
        }
        Ok(snap)
    }

    /// Select `start..end` (UTF-16 units) of `expected_full_text` and prove it; see
    /// `crate::text_select::select_and_verify`. Never types.
    pub fn select_and_verify(
        &self,
        expected_full_text: &str,
        start: usize,
        end: usize,
    ) -> TextSelectResult<SelectionProof> {
        select_and_verify(
            &Surface(&self.el),
            &self.baseline,
            expected_full_text,
            start,
            end,
            self.limit_utf16,
        )
    }
}
