//! Display width for one grapheme cluster.
//!
//! Everything in the composer that walks text for cursor placement or
//! wrapping does it in grapheme units (never bytes, never `char`s) and asks
//! this module how many terminal columns that grapheme occupies. Getting
//! this wrong is how a cursor ends up rendered one cell off from the glyph
//! it is supposedly next to -- "shearing" -- on emoji, tabs, or paste
//! markers.

use unicode_width::UnicodeWidthStr;

/// Columns a tab advances the cursor by. Terminals disagree on real tab
/// stops; a fixed width keeps our own wrap math internally consistent even
/// when it doesn't match the terminal's own rendering to the column.
pub const TAB_WIDTH: usize = 4;

/// Width of one grapheme cluster as it will occupy on screen.
///
/// Paste placeholders are donor atomic `TextElement`s rendered as labels,
/// so no private-use marker resolution is needed here — the old
/// `PasteRegistry` marker path is superseded by `textarea` elements.
pub fn grapheme_width(g: &str) -> usize {
    if let Some(c) = single_char(g)
        && c == '\t'
    {
        return TAB_WIDTH;
    }
    // unicode-width returns 0 for combining marks and control characters;
    // a lone unprintable grapheme still occupies a cell when rendered as a
    // replacement glyph, so floor non-empty input at 1.
    let w = UnicodeWidthStr::width(g);
    if w == 0 && !g.is_empty() { 1 } else { w }
}

/// Returns the display width Ratatui uses for terminal text without its `u16` limit.
///
/// Lifted from the Codex donor (`width.rs @ 444da310`): matches Ratatui's
/// terminal-cell semantics while retaining `usize` precision for long lines,
/// and counts halfwidth sound marks that `unicode-width` reports as 0.
pub(crate) fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
        + text
            .chars()
            .filter(|ch| matches!(ch, '\u{FF9E}' | '\u{FF9F}'))
            .count()
}

fn single_char(g: &str) -> Option<char> {
    let mut it = g.chars();
    let c = it.next()?;
    if it.next().is_none() { Some(c) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_one_column() {
        assert_eq!(grapheme_width("a"), 1);
    }

    #[test]
    fn a_tab_takes_a_fixed_number_of_columns() {
        assert_eq!(grapheme_width("\t"), TAB_WIDTH);
    }

    #[test]
    fn wide_emoji_take_two_columns() {
        assert_eq!(grapheme_width("\u{1F600}"), 2);
    }
}
