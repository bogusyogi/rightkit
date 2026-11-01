//! Grapheme-aware line wrapping.
//!
//! Byte or code-point wrapping shears wide characters: a CJK ideograph or an
//! emoji occupies two terminal columns but is one `char`, and a tab occupies
//! several columns while being one byte. Wrapping on graphemes with an
//! explicit width table is the only way a wrapped line's *display* width
//! matches the column it is drawn into.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Columns a tab occupies. Terminals vary; a fixed stop keeps wrapping
/// deterministic regardless of the terminal's own tab handling, since we
/// print full lines rather than raw tab bytes into wrapped output.
const TAB_WIDTH: usize = 4;

/// Display width of one grapheme cluster, in terminal columns.
///
/// Control characters (other than tab, handled separately) have no visual
/// width; everything else falls back to `unicode-width`'s table, with a floor
/// of one column so a cluster ratatui would still draw something for is never
/// counted as free.
fn grapheme_width(g: &str) -> usize {
    if g == "\t" {
        return TAB_WIDTH;
    }
    let w = UnicodeWidthStr::width(g);
    if w == 0 && !g.chars().all(|c| c.is_control()) {
        1
    } else {
        w
    }
}

/// Display width of a whole string, summing per-grapheme widths.
///
/// Used by callers (and tests) that need to confirm a wrapped line actually
/// fits the column budget it was wrapped to.
pub fn display_width(s: &str) -> usize {
    s.graphemes(true).map(grapheme_width).sum()
}

/// Wrap one logical line to `width` columns, breaking on word boundaries and
/// falling back to a hard grapheme break when a single word exceeds the whole
/// width (a URL, a long identifier, a run of CJK with no spaces).
///
/// Leading whitespace introduced by a break is dropped rather than carried to
/// the next line, matching ordinary paragraph reflow. An empty input yields a
/// single empty line so callers never lose a blank line from the source.
pub fn wrap(text: &str, width: u16) -> Vec<String> {
    let width = width.max(1) as usize;

    if text.is_empty() {
        return vec![String::new()];
    }

    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_w = 0usize;

    for word in text.split_word_bounds() {
        if current.is_empty() && word.trim().is_empty() {
            // Don't carry a break-induced leading space to the next line.
            continue;
        }

        let word_w = display_width(word);

        if current_w + word_w <= width {
            current.push_str(word);
            current_w += word_w;
            continue;
        }

        if !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            current_w = 0;
        }

        if word.trim().is_empty() {
            // The break fell on a space: it ends the previous row and must
            // not start the next one.
            continue;
        }

        if word_w <= width {
            current.push_str(word);
            current_w = word_w;
        } else {
            // The word alone is wider than the line: break it by grapheme
            // rather than let ratatui/the terminal do it for us, which would
            // desynchronise our row accounting from what's on screen.
            for g in word.graphemes(true) {
                let g_w = grapheme_width(g);
                if current_w + g_w > width && !current.is_empty() {
                    lines.push(std::mem::take(&mut current));
                    current_w = 0;
                }
                current.push_str(g);
                current_w += g_w;
            }
        }
    }

    lines.push(current);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_break_on_a_space_does_not_start_the_next_row_with_it() {
        assert_eq!(wrap("abcd efgh", 4), vec!["abcd", "efgh"]);
        assert_eq!(wrap("ab cd ef", 5), vec!["ab cd", "ef"]);
    }

    #[test]
    fn an_empty_line_wraps_to_one_empty_line() {
        assert_eq!(wrap("", 10), vec![String::new()]);
    }

    #[test]
    fn short_text_fits_on_one_line() {
        assert_eq!(wrap("hello world", 40), vec!["hello world".to_string()]);
    }

    #[test]
    fn wrapping_breaks_at_a_word_boundary_not_mid_word() {
        let lines = wrap("hello there friend", 8);
        for line in &lines {
            assert!(display_width(line) <= 8, "line {:?} exceeds width 8", line);
        }
        // No word split mid-way: every produced line, once whitespace is
        // trimmed, is a whole word or run of whole words.
        let rejoined: String = lines.join(" ");
        for word in "hello there friend".split_whitespace() {
            assert!(rejoined.contains(word), "lost word {word:?} while wrapping");
        }
    }

    #[test]
    fn a_word_wider_than_the_whole_line_is_hard_broken_by_grapheme() {
        let lines = wrap("supercalifragilisticexpialidocious", 5);
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(display_width(line) <= 5, "line {:?} exceeds width 5", line);
        }
    }

    #[test]
    fn wide_characters_never_exceed_the_requested_width() {
        // CJK ideographs and emoji are two columns wide; a naive char-count
        // wrap would let a line run to double the requested width.
        let text = "文字文字文字文字文字😀😀😀😀😀";
        for width in [3, 6, 10, 20] {
            for line in wrap(text, width) {
                assert!(
                    display_width(&line) <= width as usize,
                    "line {:?} exceeds width {width}",
                    line
                );
            }
        }
    }

    #[test]
    fn tabs_occupy_more_than_one_column() {
        assert_eq!(display_width("\t"), TAB_WIDTH);
        assert!(display_width("a\tb") > 3);
    }

    #[test]
    fn resizing_narrower_then_wider_never_drops_a_word() {
        let text = "the quick brown fox jumps over the lazy dog and then some more";
        let words: Vec<&str> = text.split_whitespace().collect();

        // Widths at or above the longest word (5 columns, "quick"/"jumps")
        // never need a hard break, so word boundaries are preserved exactly.
        for width in [8, 12, 20, 80] {
            let joined = wrap(text, width).join(" ");
            let rejoined: Vec<&str> = joined.split_whitespace().collect();
            assert_eq!(rejoined, words, "width {width} lost or reordered a word");
        }

        // Below that, words get hard-broken -- but no character is dropped
        // (only the break-induced leading spaces at each new line, which the
        // wrap deliberately omits; see `wrap`'s doc comment).
        let recovered: String = wrap(text, 4).concat();
        assert_eq!(
            recovered.split_whitespace().collect::<String>(),
            text.split_whitespace().collect::<String>(),
            "a narrow rewrap must not drop characters"
        );
    }
}
