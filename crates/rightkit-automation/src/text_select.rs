//! Verified text-range selection, the platform-independent half.
//!
//! A caller that wants to replace a span it typed earlier cannot trust "select, then type": if
//! the selection landed on other text, the typed replacement destroys the user's words. This
//! module holds the types and the proof rules; the native halves are
//! `win::text_select::TextTarget` (UI Automation `TextPattern`) and
//! `macos::text_select::TextTarget` (Accessibility `AXSelectedTextRange`). This crate selects and
//! proves; it never types. Typing the replacement belongs to the caller, which must re-read the
//! whole document afterwards.
//!
//! # Offsets
//!
//! Every offset in this API is a count of UTF-16 code units from the start of the document
//! text exactly as the platform returned it ([`DocumentSnapshot::text`]): the UTF-16 length of a
//! UIA `BSTR` on Windows, an `NSString` index on macOS. A caller that holds a Rust `&str`
//! converts with [`utf16_len`]; an offset inside a surrogate pair is refused. The unit the
//! platform uses to *move* an endpoint (UIA `Character`) may count differently (scalars,
//! graphemes, `\r\n` as one); the native half therefore never trusts a move count and always
//! reads the selection back and compares it, in UTF-16 units, with what was asked for.
//!
//! The document text is compared byte for byte as returned: line-break form, normalization and
//! hidden text are whatever the control reports, so `expected_full_text` must come from a
//! [`DocumentSnapshot`] of the same control (or equal one).
//!
//! # Side effects and errors
//!
//! Selecting moves the user's caret or selection. When the proof fails after the selection was
//! touched, the original selection is put back only if a fresh read shows the same control, the
//! unchanged full document and exactly the span this call set; otherwise the rollback is skipped
//! and the error says so ([`Rollback::RestoreSkipped`]). The error is typed and content-free;
//! the caller must then not type and falls back to its copy path.
//!
//! **A selection does not authorize a write.** A [`SelectionProof`] is true at the instant of its
//! last read. The caller must re-prove the full text and the selection immediately before any
//! paste or typing, and must treat a typed replacement as a separate gated effect.
//!
//! Selecting is an effect: it honours the process kill switch (`policy::killed`). Run the native
//! calls under `win::with_deadline_mutation` on Windows (a provider can hang), and set
//! `ax::El::set_timeout` on macOS.

use crate::uia_text::DEFAULT_MAX_UTF16;
use std::fmt;

/// Default cap, in UTF-16 units, for the document text one call reads.
pub const DEFAULT_LIMIT_UTF16: usize = DEFAULT_MAX_UTF16;

/// Who the control is, compared for equality only. It holds no text.
///
/// Equality includes an element token. On Windows it is the UIA runtime id. On macOS it is a
/// per-target token: the `TextTarget` retains the `AXUIElement` and proves `CFEqual` with the
/// focused element on every check before it hands the identity out, so identities of different
/// targets never compare equal and no hash is used. The process id and window are carried
/// for diagnostics only and are NOT identifying: two different controls of one window share them.
/// Never use [`ControlIdentity::pid`] or [`ControlIdentity::window_id`] as proof of "same control".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlIdentity {
    pid: i32,
    window_id: Option<u64>,
    element_token: Vec<i64>,
    native_handle: Option<u64>,
}

impl ControlIdentity {
    /// `element_token` must be non-empty.
    pub(crate) fn new(
        pid: i32,
        window_id: Option<u64>,
        element_token: Vec<i64>,
        native_handle: Option<u64>,
    ) -> Option<Self> {
        (!element_token.is_empty()).then_some(ControlIdentity {
            pid,
            window_id,
            element_token,
            native_handle,
        })
    }
    /// Process id of the control's application. Not identifying.
    pub fn pid(&self) -> i32 {
        self.pid
    }
    /// Top-level window when known. Not identifying.
    pub fn window_id(&self) -> Option<u64> {
        self.window_id
    }
    /// The control's own native window handle (Windows) when it has one. Not identifying.
    pub fn native_handle(&self) -> Option<u64> {
        self.native_handle
    }
}

/// The whole text of the focused control and its selection, read at one moment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentSnapshot {
    /// Document text exactly as the platform returned it.
    pub text: String,
    /// `(start, end)` of the selection in UTF-16 units, `start <= end`; a caret has `start == end`.
    pub selection: (usize, usize),
    pub control_identity: ControlIdentity,
}

/// A selection as read back from the platform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedSelection {
    pub text: String,
    pub start: usize,
    pub end: usize,
}

/// Evidence that the requested span, and only it, is selected in the unchanged document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectionProof {
    pub start: usize,
    pub end: usize,
    /// The selected text read back; equals `expected_full_text[start..end]` (UTF-16 units).
    pub selected_text: String,
    /// UTF-16 length of the unchanged document.
    pub text_utf16_len: usize,
    /// The selection before this call changed it.
    pub previous_selection: (usize, usize),
    pub control_identity: ControlIdentity,
}

/// When a document change was seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    BeforeSelect,
    AfterSelect,
}

/// What became of the original selection after a failure past the point of changing it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rollback {
    /// The selection was already the original one (or the control was never changed).
    NotNeeded,
    /// Put back, and a fresh read confirmed it.
    Restored,
    /// Not attempted: a fresh read did not show the same control, the unchanged full document and
    /// exactly the selection this call had set, so ownership of the selection could not be
    /// proven (the user or the app may have changed it). Nothing was written.
    RestoreSkipped,
    /// Attempted, but the read-back did not confirm it.
    RestoreFailed,
}

/// Why a selection was not made or not proven. Content-free by design.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TextSelectError {
    /// The control has no `TextPattern` (Windows) or no settable `AXSelectedTextRange` or no
    /// string `AXValue` (macOS).
    Unsupported(&'static str),
    /// A password or secure field.
    Secure,
    /// The kill switch is engaged.
    Blocked(&'static str),
    PermissionDenied(&'static str),
    /// The element is gone or no longer answers.
    ElementUnavailable(&'static str),
    /// The platform call failed with this code.
    Platform(i64, &'static str),
    /// A read failed or was inconsistent.
    Unreadable(&'static str),
    /// The document is longer than the limit; nothing was selected.
    TooLarge { limit_utf16: usize },
    /// `start > end` or `end` is past the document.
    InvalidSpan {
        start: usize,
        end: usize,
        len_utf16: usize,
    },
    /// An offset falls inside a surrogate pair.
    SplitsSurrogatePair { offset: usize },
    /// The control is not the focused element.
    NotFocused,
    /// The focused control is not the one this target was made for.
    IdentityChanged,
    /// The document text differs from `expected_full_text`.
    TextChanged { stage: Stage },
    /// A failure after the selection was touched, with what happened to the original selection.
    AfterSelection {
        cause: Box<TextSelectError>,
        rollback: Rollback,
    },
    /// The selection read back is not the requested span. `text_matched` says whether the
    /// selected text itself matched.
    SelectionMismatch {
        requested: (usize, usize),
        observed: Option<(usize, usize)>,
        text_matched: bool,
    },
}

impl fmt::Display for TextSelectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(w) => write!(f, "unsupported: {w}"),
            Self::Secure => f.write_str("secure field"),
            Self::Blocked(w) => write!(f, "blocked: {w}"),
            Self::PermissionDenied(w) => write!(f, "permission denied: {w}"),
            Self::ElementUnavailable(w) => write!(f, "element unavailable: {w}"),
            Self::Platform(c, w) => write!(f, "platform error {c} in {w}"),
            Self::Unreadable(w) => write!(f, "unreadable: {w}"),
            Self::TooLarge { limit_utf16 } => write!(f, "document over {limit_utf16} UTF-16 units"),
            Self::InvalidSpan {
                start,
                end,
                len_utf16,
            } => write!(f, "span {start}..{end} outside document of {len_utf16} units"),
            Self::SplitsSurrogatePair { offset } => {
                write!(f, "offset {offset} splits a surrogate pair")
            }
            Self::NotFocused => f.write_str("control is not focused"),
            Self::IdentityChanged => f.write_str("focused control changed"),
            Self::TextChanged { stage } => write!(f, "document text changed ({stage:?})"),
            Self::AfterSelection { cause, rollback } => {
                write!(f, "{cause} (rollback: {rollback:?})")
            }
            Self::SelectionMismatch {
                requested,
                observed,
                text_matched,
            } => write!(
                f,
                "selection {observed:?} (text matched: {text_matched}) is not the requested {requested:?}"
            ),
        }
    }
}

impl std::error::Error for TextSelectError {}

pub type TextSelectResult<T> = std::result::Result<T, TextSelectError>;

/// UTF-16 length of `s`.
pub fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// Byte index in `s` of UTF-16 offset `offset`; `None` when it is past the end or inside a pair.
pub fn utf16_to_byte(s: &str, offset: usize) -> Option<usize> {
    let mut units = 0usize;
    for (idx, ch) in s.char_indices() {
        if units == offset {
            return Some(idx);
        }
        if units > offset {
            return None;
        }
        units += ch.len_utf16();
    }
    (units == offset).then_some(s.len())
}

/// `s[start..end]` by UTF-16 offsets; `None` when out of range, reversed or splitting a pair.
pub fn utf16_slice(s: &str, start: usize, end: usize) -> Option<&str> {
    if start > end {
        return None;
    }
    let b0 = utf16_to_byte(s, start)?;
    let b1 = utf16_to_byte(s, end)?;
    s.get(b0..b1)
}

/// Number of Unicode scalar values before UTF-16 offset `offset` (`None` as in [`utf16_to_byte`]).
pub fn scalars_before(s: &str, offset: usize) -> Option<usize> {
    let byte = utf16_to_byte(s, offset)?;
    Some(s[..byte].chars().count())
}

/// Refuse a span that is reversed, outside `text`, or inside a surrogate pair.
pub fn validate_span(text: &str, start: usize, end: usize) -> TextSelectResult<()> {
    let len = utf16_len(text);
    if start > end || end > len {
        return Err(TextSelectError::InvalidSpan {
            start,
            end,
            len_utf16: len,
        });
    }
    for offset in [start, end] {
        if utf16_to_byte(text, offset).is_none() {
            return Err(TextSelectError::SplitsSurrogatePair { offset });
        }
    }
    Ok(())
}

/// What a native half implements. All reads are of the one retained control.
pub trait TextSurface {
    /// The control's identity. Fails with [`TextSelectError::NotFocused`] when it is not the
    /// focused element, [`TextSelectError::Secure`] for a password field, and
    /// [`TextSelectError::Unsupported`] when it cannot expose text.
    fn identity(&self) -> TextSelectResult<ControlIdentity>;
    /// The full document text, at most `limit_utf16` units.
    fn read_text(&self, limit_utf16: usize) -> TextSelectResult<String>;
    /// The current selection; a multi-range selection is [`TextSelectError::Unreadable`].
    fn read_selection(&self, limit_utf16: usize) -> TextSelectResult<ObservedSelection>;
    /// Set the selection to `start..end` (UTF-16 units of `document`). The native half may
    /// verify internally; the caller verifies again.
    fn apply_selection(
        &self,
        document: &str,
        start: usize,
        end: usize,
        limit_utf16: usize,
    ) -> TextSelectResult<()>;
}

/// Read the focused control's text, selection and identity once, and require them to agree:
/// identity unchanged across the read, selection inside the text, selection text equal to that
/// slice of the text.
pub fn snapshot<S: TextSurface>(s: &S, limit_utf16: usize) -> TextSelectResult<DocumentSnapshot> {
    let before = s.identity()?;
    let text = s.read_text(limit_utf16)?;
    let sel = s.read_selection(limit_utf16)?;
    if s.identity()? != before {
        return Err(TextSelectError::IdentityChanged);
    }
    validate_span(&text, sel.start, sel.end)
        .map_err(|_| TextSelectError::Unreadable("selection outside document"))?;
    if utf16_slice(&text, sel.start, sel.end) != Some(sel.text.as_str()) {
        return Err(TextSelectError::Unreadable(
            "selection text differs from document",
        ));
    }
    Ok(DocumentSnapshot {
        text,
        selection: (sel.start, sel.end),
        control_identity: before,
    })
}

/// Put the original selection back, only when a fresh read proves it is still ours: the same
/// control, the full document equal to `expected`, and exactly the span this call set. Otherwise
/// nothing is written ([`Rollback::RestoreSkipped`]).
fn rollback<S: TextSurface>(
    s: &S,
    baseline: &ControlIdentity,
    expected: &str,
    set: (usize, usize),
    previous: (usize, usize),
    limit_utf16: usize,
) -> Rollback {
    let owned = (|| -> Option<ObservedSelection> {
        if s.identity().ok().as_ref() != Some(baseline) {
            return None;
        }
        if s.read_text(limit_utf16).ok().as_deref() != Some(expected) {
            return None;
        }
        s.read_selection(limit_utf16).ok()
    })();
    let Some(sel) = owned else {
        return Rollback::RestoreSkipped;
    };
    if (sel.start, sel.end) == previous
        && utf16_slice(expected, sel.start, sel.end) == Some(sel.text.as_str())
    {
        return Rollback::NotNeeded;
    }
    if (sel.start, sel.end) != set || utf16_slice(expected, set.0, set.1) != Some(sel.text.as_str())
    {
        return Rollback::RestoreSkipped;
    }
    if s.apply_selection(expected, previous.0, previous.1, limit_utf16)
        .is_err()
    {
        return Rollback::RestoreFailed;
    }
    let confirmed = s.identity().ok().as_ref() == Some(baseline)
        && s.read_text(limit_utf16).ok().as_deref() == Some(expected)
        && s.read_selection(limit_utf16)
            .ok()
            .is_some_and(|o| (o.start, o.end) == previous);
    if confirmed {
        Rollback::Restored
    } else {
        Rollback::RestoreFailed
    }
}

fn after_selection(cause: TextSelectError, rollback: Rollback) -> TextSelectError {
    TextSelectError::AfterSelection {
        cause: Box::new(cause),
        rollback,
    }
}

/// Select `start..end` (UTF-16 units) of `expected_full_text` and prove it.
///
/// Order: kill switch; span valid for `expected_full_text`; re-read the whole document and
/// require text == `expected_full_text` and identity == `baseline`; set the selection; re-read
/// identity, the whole document (must still equal `expected_full_text`) and the selection
/// (offsets and text must equal the request); identity once more. Any failure after the
/// selection was touched returns [`TextSelectError::AfterSelection`] naming the cause and the
/// [`Rollback`]: the original selection is put back only after a fresh read shows the same
/// control, the unchanged full document and exactly the span set here; otherwise the rollback is
/// skipped and reported as such.
///
/// A success does not authorize any later write. The caller must re-prove the full document and
/// the selection (a new `snapshot`) immediately before it types or pastes.
pub fn select_and_verify<S: TextSurface>(
    s: &S,
    baseline: &ControlIdentity,
    expected_full_text: &str,
    start: usize,
    end: usize,
    limit_utf16: usize,
) -> TextSelectResult<SelectionProof> {
    if crate::policy::killed() {
        return Err(TextSelectError::Blocked("kill switch"));
    }
    validate_span(expected_full_text, start, end)?;
    let expected_len = utf16_len(expected_full_text);
    if expected_len > limit_utf16 {
        return Err(TextSelectError::TooLarge { limit_utf16 });
    }
    let want = utf16_slice(expected_full_text, start, end)
        .ok_or(TextSelectError::SplitsSurrogatePair { offset: start })?;

    let before = snapshot(s, limit_utf16)?;
    if before.control_identity != *baseline {
        return Err(TextSelectError::IdentityChanged);
    }
    if before.text != expected_full_text {
        return Err(TextSelectError::TextChanged {
            stage: Stage::BeforeSelect,
        });
    }
    let previous = before.selection;

    if let Err(e) = s.apply_selection(expected_full_text, start, end, limit_utf16) {
        let rb = rollback(
            s,
            baseline,
            expected_full_text,
            (start, end),
            previous,
            limit_utf16,
        );
        return Err(after_selection(e, rb));
    }

    let verified = (|| -> TextSelectResult<ObservedSelection> {
        if s.identity()? != *baseline {
            return Err(TextSelectError::IdentityChanged);
        }
        if s.read_text(limit_utf16)? != expected_full_text {
            return Err(TextSelectError::TextChanged {
                stage: Stage::AfterSelect,
            });
        }
        let sel = s.read_selection(limit_utf16)?;
        if sel.start != start || sel.end != end || sel.text != want {
            return Err(TextSelectError::SelectionMismatch {
                requested: (start, end),
                observed: Some((sel.start, sel.end)),
                text_matched: sel.text == want,
            });
        }
        if s.identity()? != *baseline {
            return Err(TextSelectError::IdentityChanged);
        }
        Ok(sel)
    })();

    match verified {
        Ok(sel) => Ok(SelectionProof {
            start,
            end,
            selected_text: sel.text,
            text_utf16_len: expected_len,
            previous_selection: previous,
            control_identity: baseline.clone(),
        }),
        Err(e) => {
            let rb = rollback(
                s,
                baseline,
                expected_full_text,
                (start, end),
                previous,
                limit_utf16,
            );
            Err(after_selection(e, rb))
        }
    }
}
