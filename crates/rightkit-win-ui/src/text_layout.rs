//! The notch's text fitting and wrapping, owned by `TextPainter` so no card, toast or tile can
//! make layout cost grow with the length of what it shows (issue 558). A received
//! 20,000-character message froze the window thread for minutes: each caller cut text one
//! character at a time and measured the whole remainder again.
//!
//! Two rules hold for every call here, whatever the caller passes:
//! - only the characters that could be visible are looked at (`visible_chars`); the rest is
//!   never measured, and text cut that way always ends in an ellipsis;
//! - a cut point is found by bisection, so a fit costs a handful of measurements.
//!
//! Widths are in device pixels, as `TextPainter::measure` returns them.

use crate::surface::TextPainter;

const ELLIPSIS: char = '\u{2026}';

/// The narrowest advance any character is assumed to have, as a fraction of the font's pixel
/// size. Segoe UI's narrowest printing characters are about 0.2 em; a tenth of an em leaves
/// room for thin spaces. Only zero-width characters can make the bound cut text short.
const MIN_ADVANCE_EM: f32 = 0.1;

/// At most this many characters fit in `max` pixels at `size_px`, plus slack.
fn visible_chars(size_px: i32, max: f32) -> usize {
    let narrowest = size_px.max(1) as f32 * MIN_ADVANCE_EM;
    (max.max(0.0) / narrowest).ceil() as usize + 8
}

/// The largest `k` in `0..=hi` for which `fits(k)`, where `fits` holds up to some `k` and
/// fails above it; `None` when not even `0` fits.
fn largest(hi: usize, mut fits: impl FnMut(usize) -> bool) -> Option<usize> {
    if !fits(0) {
        return None;
    }
    let (mut lo, mut hi) = (0, hi);
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    Some(lo)
}

/// Length in characters of a trailing "<number> <word> (<size>)" count (or just the
/// parenthesised part when no number precedes it); 0 when `chars` does not end in one.
fn counted_tail(chars: &[char]) -> usize {
    if chars.last() != Some(&')') {
        return 0;
    }
    let Some(open) = chars.iter().rposition(|c| *c == '(') else {
        return 0;
    };
    let mut start = open;
    // Up to two words before the parenthesis, ending at a number.
    for _ in 0..2 {
        let mut word = start;
        while word > 0 && chars[word - 1] == ' ' {
            word -= 1;
        }
        let end = word;
        while word > 0 && chars[word - 1] != ' ' {
            word -= 1;
        }
        if word == end {
            break;
        }
        start = word;
        if chars[word..end].iter().all(|c| c.is_ascii_digit()) {
            break;
        }
    }
    chars.len() - start
}

impl TextPainter {
    /// `text` cut at the end with an ellipsis to at most `max` pixels (trailing spaces before
    /// the ellipsis dropped); empty when not even the ellipsis fits.
    pub fn fit_end(&mut self, text: &str, size_px: i32, bold: bool, max: f32) -> String {
        self.fit_end_weight(text, size_px, u8::from(bold), max, false)
    }

    /// `fit_end`; `continues` says the text goes on past `text`, so it ends in an ellipsis
    /// even when it fits.
    fn fit_end_weight(
        &mut self,
        text: &str,
        size_px: i32,
        weight: u8,
        max: f32,
        continues: bool,
    ) -> String {
        let bound = visible_chars(size_px, max);
        let chars: Vec<char> = text.chars().take(bound + 1).collect();
        let cut_short = continues || chars.len() > bound;
        if !cut_short && self.measure_weight(text, size_px, weight) as f32 <= max {
            return text.to_string();
        }
        let chars = &chars[..chars.len().min(bound)];
        let cut = |k: usize| {
            let mut kept = &chars[..k];
            while let [rest @ .., last] = kept
                && last.is_whitespace()
            {
                kept = rest;
            }
            let mut out: String = kept.iter().collect();
            out.push(ELLIPSIS);
            out
        };
        // Whole text that does not fit gives up at least its last character.
        let hi = if cut_short {
            chars.len()
        } else {
            chars.len().saturating_sub(1)
        };
        largest(hi, |k| {
            self.measure_weight(&cut(k), size_px, weight) as f32 <= max
        })
        .map(cut)
        .unwrap_or_default()
    }

    /// `text` cut in the middle with an ellipsis to at most `max` pixels, so the start of a
    /// name and the count or extension at its end both stay readable; a trailing count such as
    /// "2 files (12.4 MB)" stays whole when it can.
    pub fn fit_middle(&mut self, text: &str, size_px: i32, bold: bool, max: f32) -> String {
        let weight = u8::from(bold);
        let bound = visible_chars(size_px, max);
        let chars: Vec<char> = text.chars().collect();
        let n = chars.len();
        if n <= 2 * bound && self.measure_weight(text, size_px, weight) as f32 <= max {
            return text.to_string();
        }
        let fits = |painter: &mut Self, cut: &str| {
            painter.measure_weight(cut, size_px, weight) as f32 <= max
        };
        let kept_tail = counted_tail(&chars);
        if kept_tail > 0 && kept_tail < n && kept_tail <= bound {
            let tail = &chars[n - kept_tail..];
            let cut = |head: usize| {
                let mut out: String = chars[..head].iter().collect();
                out.push(ELLIPSIS);
                out.extend(tail);
                out
            };
            if let Some(head) = largest((n - kept_tail - 1).min(bound), |head| {
                fits(self, &cut(head))
            }) {
                return cut(head);
            }
        }
        // Otherwise the Mac's split: the end keeps the odd character.
        let cut = |keep: usize| {
            let head = keep / 2;
            let mut out: String = chars[..head].iter().collect();
            out.push(ELLIPSIS);
            out.extend(&chars[n - (keep - head)..]);
            out
        };
        largest(n.saturating_sub(1).min(2 * bound), |keep| {
            fits(self, &cut(keep))
        })
        .map(cut)
        .unwrap_or_else(|| ELLIPSIS.to_string())
    }

    /// Greedy word wrap of `text` to `max` pixels, at most `lines` lines: what does not fit on
    /// the last one is cut with an ellipsis (the Mac's `lineLimit`). A word wider than a line
    /// (a link) is cut into pieces that each fit, as the Mac's wrap does. Never empty.
    pub fn wrap(
        &mut self,
        text: &str,
        size_px: i32,
        bold: bool,
        max: f32,
        lines: usize,
    ) -> Vec<String> {
        let weight = u8::from(bold);
        let bound = lines.max(1) * visible_chars(size_px, max);
        let truncated = text.chars().nth(bound).is_some();
        let text: String = text.chars().take(bound).collect();
        let fits = |painter: &mut Self, value: &str| {
            painter.measure_weight(value, size_px, weight) as f32 <= max
        };
        let mut pieces: Vec<String> = Vec::new();
        for word in text.split_ascii_whitespace() {
            if fits(self, word) {
                pieces.push(word.to_string());
                continue;
            }
            let chars: Vec<char> = word.chars().collect();
            let mut start = 0;
            while start < chars.len() {
                let rest = &chars[start..];
                let take = largest(rest.len(), |p| {
                    p <= 1 || fits(self, &rest[..p].iter().collect::<String>())
                })
                .unwrap_or(1)
                .max(1);
                pieces.push(rest[..take].iter().collect());
                start += take;
            }
        }
        let mut out: Vec<String> = Vec::new();
        let mut overflowed = false;
        let mut words = pieces.iter().map(String::as_str);
        while let Some(word) = words.next() {
            let joined = out.last().map(|line| format!("{line} {word}"));
            if let Some(joined) = joined.filter(|joined| fits(self, joined.as_str())) {
                if let Some(line) = out.last_mut() {
                    *line = joined;
                }
            } else if out.len() == lines {
                let mut rest = out.pop().unwrap_or_default();
                rest.push(' ');
                rest.push_str(word);
                for more in words.by_ref() {
                    rest.push(' ');
                    rest.push_str(more);
                }
                out.push(self.fit_end_weight(&rest, size_px, weight, max, truncated));
                overflowed = true;
                break;
            } else {
                out.push(word.to_string());
            }
        }
        if out.is_empty() {
            out.push(String::new());
        }
        if truncated
            && !overflowed
            && let Some(last) = out.last_mut()
        {
            *last = self.fit_end_weight(last, size_px, weight, max, true);
        }
        for line in &mut out {
            if !fits(self, line.as_str()) {
                *line = self.fit_end_weight(line, size_px, weight, max, false);
            }
        }
        out
    }
}

/// Whether `text` has more after the lines shown so far (each counted with its line break).
fn has_more(text: &str, shown: &[String]) -> bool {
    let joined: usize = shown.iter().map(|line| line.chars().count() + 1).sum();
    text.chars().count() > joined
}

/// `text` broken into at most `lines` lines for which `fits` holds, keeping the layout the
/// text has: line breaks start a new line, a tab is four spaces, leading spaces are
/// indentation and stay, and a space is never collapsed. A word for which `fits` fails alone
/// is broken where it stops fitting (a piece is at least one character). Only the first
/// `lines * char_budget` characters are looked at, and a text cut by a limit ends in an
/// ellipsis on its last line. A carriage return stays in the line it ends.
///
/// `fits` is the host's own measure of a candidate line (its font, its width), so any painter
/// can use it. Cost is bounded by the budget, never by the length of `text`. The result has at
/// least one entry.
pub fn wrap_preserving(
    text: &str,
    lines: usize,
    char_budget: usize,
    mut fits: impl FnMut(&str) -> bool,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cut = false;
    let limit = lines.saturating_mul(char_budget);
    let text: String = text.chars().take(limit).collect();
    'paragraphs: for paragraph in text.replace('\t', "    ").split('\n') {
        let mut current = String::new();
        let body = paragraph.trim_start_matches(' ');
        let indent = " ".repeat(paragraph.len() - body.len());
        for (n, word) in body.split(' ').enumerate() {
            let word = &if n == 0 {
                format!("{indent}{word}")
            } else {
                word.to_string()
            };
            let candidate = if current.is_empty() {
                word.to_string()
            } else {
                format!("{current} {word}")
            };
            if fits(&candidate) {
                current = candidate;
                continue;
            }
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
                if out.len() >= lines {
                    cut = true;
                    break 'paragraphs;
                }
            }
            let mut piece = String::new();
            for c in word.chars() {
                piece.push(c);
                if !fits(&piece) && piece.chars().count() > 1 {
                    piece.pop();
                    out.push(std::mem::take(&mut piece));
                    piece.push(c);
                    if out.len() >= lines {
                        cut = true;
                        break 'paragraphs;
                    }
                }
            }
            current = piece;
        }
        out.push(current);
        if out.len() >= lines {
            cut = out.len() > lines || has_more(&text, &out);
            out.truncate(lines);
            break;
        }
    }
    if out.len() > lines {
        out.truncate(lines);
        cut = true;
    }
    if cut && let Some(last) = out.last_mut() {
        while last.chars().count() > 1 && !fits(&format!("{last}{ELLIPSIS}")) {
            last.pop();
        }
        last.push(ELLIPSIS);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}
