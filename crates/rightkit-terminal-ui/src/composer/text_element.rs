//! Atomic text-element spans lifted from the donor `codex_protocol::user_input`
//! types (codex @ 444da310). These are the composer-local equivalents; the
//! parent owns the protocol-side projection if/when submission needs to carry
//! element metadata upstream.
//!
//! A `TextElement` marks a byte range in the composer text that the textarea
//! must treat as one atomic unit: cursor motion skips over it, deletion removes
//! the whole span, and wrapping never splits it. Large pastes use elements to
//! keep a `[Pasted Content N chars]` placeholder collapsible while the payload
//! lives in `pending_pastes`.

use std::ops::Range;

/// Conservative cap so one user message cannot monopolize a large context window.
#[cfg(test)]
pub const MAX_USER_INPUT_TEXT_CHARS: usize = 1 << 20;

/// Byte range in the parent `text` buffer that this element occupies.
#[derive(Debug, Clone, PartialEq)]
pub struct TextElement {
    /// Byte range in the parent `text` buffer that this element occupies.
    pub byte_range: ByteRange,
    /// Optional human-readable placeholder for the element, displayed in the UI.
    placeholder: Option<String>,
}

impl TextElement {
    pub fn new(byte_range: ByteRange, placeholder: Option<String>) -> Self {
        Self {
            byte_range,
            placeholder,
        }
    }

    /// Returns a copy of this element with a remapped byte range.
    ///
    /// The placeholder is preserved as-is; callers must ensure the new range
    /// still refers to the same logical element (and same placeholder)
    /// within the new text.
    pub fn map_range<F>(&self, map: F) -> Self
    where
        F: FnOnce(ByteRange) -> ByteRange,
    {
        Self {
            byte_range: map(self.byte_range),
            placeholder: self.placeholder.clone(),
        }
    }

    /// Returns the stored placeholder without falling back to the text buffer.
    #[doc(hidden)]
    pub fn _placeholder_for_conversion_only(&self) -> Option<&str> {
        self.placeholder.as_deref()
    }

    pub fn placeholder<'a>(&'a self, text: &'a str) -> Option<&'a str> {
        self.placeholder
            .as_deref()
            .or_else(|| text.get(self.byte_range.start..self.byte_range.end))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    /// Start byte offset (inclusive) within the UTF-8 text buffer.
    pub start: usize,
    /// End byte offset (exclusive) within the UTF-8 text buffer.
    pub end: usize,
}

impl From<Range<usize>> for ByteRange {
    fn from(range: Range<usize>) -> Self {
        Self {
            start: range.start,
            end: range.end,
        }
    }
}
