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
