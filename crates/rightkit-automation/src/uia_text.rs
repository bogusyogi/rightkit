//! Platform-independent rules for bounded UI Automation text reads. The Windows backend
//! (`win::uia`) applies them to real elements; the limits and the row accounting live here so
//! they are unit-tested on every OS.

/// Default cap, in UTF-16 code units, for one text read (matches the HeardRight proof budget).
pub const DEFAULT_MAX_UTF16: usize = 32_768;
/// Default cap, in UTF-8 bytes, for the whole text collected from terminal rows.
pub const DEFAULT_MAX_UTF8_BYTES: usize = 32_768;

/// Result of a bounded text read that keeps "nothing to read" apart from "too big to keep".
/// A caller that must fail closed on oversized text needs the difference; [`TextRead::into_option`]
/// collapses both to `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextRead {
    /// The element has no such pattern, or the read call failed.
    Absent,
    /// The text is longer than the limit; it was not converted or kept.
    TooLarge,
    /// The text, within the limit. May be empty (a readable empty field is a proven state).
    Text(String),
}

impl TextRead {
    /// `Some(text)` only for [`TextRead::Text`].
    pub fn into_option(self) -> Option<String> {
        match self {
            TextRead::Text(t) => Some(t),
            _ => None,
        }
    }
    /// True when the pattern answered but the text was over the limit.
    pub fn is_too_large(&self) -> bool {
        matches!(self, TextRead::TooLarge)
    }
}

/// Whether `units` UTF-16 code units fit within `limit`.
pub fn within_utf16_limit(units: usize, limit: usize) -> bool {
    units <= limit
}

/// The `maxLength` to hand to `ITextRangeProvider::GetText` so an over-limit read is detectable:
/// one unit past the limit, clamped to what the `i32` parameter can carry.
pub fn get_text_request_len(limit_utf16: usize) -> i32 {
    limit_utf16.saturating_add(1).min(i32::MAX as usize) as i32
}

/// Classify a read of `units` UTF-16 units. `make` converts the text and runs only when it fits,
/// so an oversized value is never copied into a `String`.
pub fn classify_read(units: usize, limit_utf16: usize, make: impl FnOnce() -> String) -> TextRead {
    if within_utf16_limit(units, limit_utf16) {
        TextRead::Text(make())
    } else {
        TextRead::TooLarge
    }
}

/// VS Code's terminal edit exposes this static label through TextPattern instead of the buffer.
pub fn is_static_terminal_label(value: &str) -> bool {
    let value = value.trim();
    value.starts_with("Terminal ") && value.contains("Toggle Screen Reader Accessibility Mode")
}

/// The focused edit of VS Code's terminal is named "Terminal ..." (case-insensitive).
pub fn is_terminal_edit_name(name: &str) -> bool {
    name.trim().to_ascii_lowercase().starts_with("terminal")
}

/// Text of the focused editable field from `win::uia::focused_field_context`. Either side is
/// `None` when the field exposes no such text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FocusedFieldText {
    pub field_text: Option<String>,
    pub selected_text: Option<String>,
}

/// Budgets for `win::uia::El::terminal_rows`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalLimits {
    /// Most `ListItem` rows read; more means the read is refused.
    pub max_rows: usize,
    /// Most UTF-16 units in one row; a longer row refuses the whole read.
    pub max_row_utf16: usize,
    /// Most UTF-8 bytes across all rows, counting one separator byte per row.
    pub max_total_bytes: usize,
}

impl Default for TerminalLimits {
    fn default() -> Self {
        TerminalLimits {
            max_rows: 256,
            max_row_utf16: DEFAULT_MAX_UTF16,
            max_total_bytes: DEFAULT_MAX_UTF8_BYTES,
        }
    }
}

/// The text of the terminal rows plus the scope that produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalRows {
    /// UIA runtime id of the single `List` container the rows came from.
    pub scope_runtime_id: Vec<i32>,
    /// Native window handle of that container (0 when it has none).
    pub scope_native_window_handle: isize,
    /// Row names joined with `\n`; empty rows and the static label are dropped.
    pub text: String,
}

/// What [`TerminalRowCollector::push`] did with one row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowPush {
    /// Empty or the static label; dropped.
    Skipped,
    /// Kept.
    Kept,
    /// A budget was exceeded; the whole read must be refused.
    Refused,
}

/// Accumulates row names under [`TerminalLimits`].
#[derive(Debug)]
pub struct TerminalRowCollector {
    limits: TerminalLimits,
    rows: Vec<String>,
    bytes: usize,
}

impl TerminalRowCollector {
    pub fn new(limits: TerminalLimits) -> Self {
        TerminalRowCollector {
            limits,
            rows: Vec::new(),
            bytes: 0,
        }
    }

    /// Offer one row. `units` is its UTF-16 length; `make` produces the text and runs only when
    /// the row is within the per-row limit.
    pub fn push(&mut self, units: usize, make: impl FnOnce() -> String) -> RowPush {
        if units > self.limits.max_row_utf16 {
            return RowPush::Refused;
        }
        let text = make();
        if text.is_empty() || is_static_terminal_label(&text) {
            return RowPush::Skipped;
        }
        let Some(bytes) = text
            .len()
            .checked_add(1)
            .and_then(|n| self.bytes.checked_add(n))
        else {
            return RowPush::Refused;
        };
        if bytes > self.limits.max_total_bytes {
            return RowPush::Refused;
        }
        self.bytes = bytes;
        self.rows.push(text);
        RowPush::Kept
    }

    /// The joined text, or `None` when it would exceed the byte budget.
    pub fn finish(self) -> Option<String> {
        let text = self.rows.join("\n");
        (text.len() <= self.limits.max_total_bytes).then_some(text)
    }
}

/// Editability of a focused element. An explicit `ValuePattern` read-only flag always wins. An
/// Edit or Document control counts only when it is keyboard focusable (#360). Otherwise a writable
/// `ValuePattern` or a `TextEditPattern` counts. `value_read_only` is `None` when there is no
/// `ValuePattern`.
pub fn editable_decision(
    is_edit_or_document: bool,
    keyboard_focusable: bool,
    value_read_only: Option<bool>,
    has_text_edit: bool,
) -> bool {
    if value_read_only == Some(true) {
        return false;
    }
    (is_edit_or_document && keyboard_focusable) || value_read_only == Some(false) || has_text_edit
}

/// Keep at most `max_units` UTF-16 code units of `text`, cutting only at a character boundary so
/// a surrogate pair is never split. Returns `text` unchanged when it already fits.
pub fn truncate_utf16(text: &str, max_units: usize) -> String {
    let mut units = 0usize;
    let mut end = text.len();
    for (idx, ch) in text.char_indices() {
        let next = units + ch.len_utf16();
        if next > max_units {
            end = idx;
            break;
        }
        units = next;
    }
    text[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_len_is_one_past_limit_and_clamped() {
        assert_eq!(get_text_request_len(10), 11);
        assert_eq!(get_text_request_len(usize::MAX), i32::MAX);
        assert_eq!(get_text_request_len(0), 1);
    }

    #[test]
    fn classify_read_never_builds_oversized_text() {
        let r = classify_read(5, 5, || "hello".into());
        assert_eq!(r, TextRead::Text("hello".into()));
        let r = classify_read(6, 5, || panic!("must not convert"));
        assert!(r.is_too_large());
        assert_eq!(r.into_option(), None);
        assert_eq!(
            classify_read(0, 5, String::new).into_option(),
            Some(String::new())
        );
        assert_eq!(TextRead::Absent.into_option(), None);
    }

    #[test]
    fn static_label_and_terminal_name() {
        assert!(is_static_terminal_label(
            "  Terminal 1, bash Toggle Screen Reader Accessibility Mode  "
        ));
        assert!(!is_static_terminal_label("Terminal output"));
        assert!(is_terminal_edit_name(" TERMINAL 1, pwsh"));
        assert!(!is_terminal_edit_name("Editor"));
    }

    fn limits(rows: usize, row: usize, total: usize) -> TerminalLimits {
        TerminalLimits {
            max_rows: rows,
            max_row_utf16: row,
            max_total_bytes: total,
        }
    }

    #[test]
    fn collector_joins_and_skips() {
        let mut c = TerminalRowCollector::new(limits(8, 100, 100));
        assert_eq!(c.push(2, || "ls".into()), RowPush::Kept);
        assert_eq!(c.push(0, String::new), RowPush::Skipped);
        assert_eq!(
            c.push(60, || "Terminal 1 Toggle Screen Reader Accessibility Mode"
                .into()),
            RowPush::Skipped
        );
        assert_eq!(c.push(3, || "pwd".into()), RowPush::Kept);
        assert_eq!(c.finish().as_deref(), Some("ls\npwd"));
    }

    #[test]
    fn collector_refuses_long_row_without_converting() {
        let mut c = TerminalRowCollector::new(limits(8, 4, 100));
        assert_eq!(c.push(5, || panic!("must not convert")), RowPush::Refused);
    }

    #[test]
    fn collector_total_budget_counts_separators() {
        // "ab" costs 3, "cd" costs 3: 6 bytes fits 6, a third byte does not.
        let mut c = TerminalRowCollector::new(limits(8, 100, 6));
        assert_eq!(c.push(2, || "ab".into()), RowPush::Kept);
        assert_eq!(c.push(2, || "cd".into()), RowPush::Kept);
        assert_eq!(c.push(1, || "e".into()), RowPush::Refused);
    }

    #[test]
    fn empty_collector_finishes_empty() {
        let c = TerminalRowCollector::new(TerminalLimits::default());
        assert_eq!(c.finish().as_deref(), Some(""));
    }

    #[test]
    fn editable_rules() {
        // Edit/Document needs keyboard focusability.
        assert!(editable_decision(true, true, None, false));
        assert!(!editable_decision(true, false, None, false));
        // A non-focusable Document with a writable ValuePattern still counts (value is writable).
        assert!(editable_decision(true, false, Some(false), false));
        // Explicit read-only wins over type, focus and TextEditPattern.
        assert!(!editable_decision(true, true, Some(true), false));
        assert!(!editable_decision(false, true, Some(true), true));
        assert!(!editable_decision(false, false, None, false));
        assert!(editable_decision(false, false, None, true));
    }

    #[test]
    fn truncate_utf16_bounds_and_keeps_pairs_whole() {
        assert_eq!(truncate_utf16("hello", 10), "hello");
        assert_eq!(truncate_utf16("hello", 5), "hello");
        assert_eq!(truncate_utf16("hello", 3), "hel");
        assert_eq!(truncate_utf16("hello", 0), "");
        // U+1F600 is two UTF-16 units; a limit of 1 must not split it.
        assert_eq!(truncate_utf16("\u{1F600}a", 1), "");
        assert_eq!(truncate_utf16("\u{1F600}a", 2), "\u{1F600}");
        assert_eq!(truncate_utf16("\u{1F600}a", 3), "\u{1F600}a");
        // Multi-byte BMP character counts as one unit.
        assert_eq!(truncate_utf16("é€x", 2), "é€");
    }
}
