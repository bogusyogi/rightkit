//! Bounded display of arbitrarily large tool output.
//!
//! A bare `...` tells the reader nothing about how much was cut. Every
//! truncation this module performs records the original size and the cap
//! applied, so the marker reads like `… 200 of 4,813 bytes shown (cap 200)`
//! instead of just trailing off.

/// What was cut, so the display marker can say so explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Truncation {
    /// Size of the untruncated input, in bytes.
    pub original_len: usize,
    /// The cap that was applied.
    pub cap: usize,
}

impl Truncation {
    /// Human-readable marker naming both the original size and the cap --
    /// never a bare ellipsis.
    pub fn marker(&self) -> String {
        let shown = self.cap.min(self.original_len);
        format!(
            "… {shown} of {orig} bytes shown (cap {cap})",
            shown = shown,
            orig = self.original_len,
            cap = self.cap
        )
    }
}

/// Truncates `s` to at most `cap` bytes, on a char boundary. Returns the
/// (possibly shortened) text and, if anything was cut, the [`Truncation`]
/// describing it.
pub fn truncate(s: &str, cap: usize) -> (String, Option<Truncation>) {
    if s.len() <= cap {
        return (s.to_string(), None);
    }
    let mut end = cap;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (
        s[..end].to_string(),
        Some(Truncation {
            original_len: s.len(),
            cap,
        }),
    )
}
