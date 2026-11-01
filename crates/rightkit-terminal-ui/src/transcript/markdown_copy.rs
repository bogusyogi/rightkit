//! Semantic copy annotations in rendered-byte coordinates, independent of terminal styling.
//!
//! Wrapping and selection snapshots share each logical line's annotations. Only copying
//! serializes Markdown; literal-only selections keep their original text and clipboard format.
//! Rich selections preserve selected words and semantic formatting with balanced markup,
//! rather than reproducing the complete response's source spelling (available through `/copy`).

use std::ops::Range;
use std::sync::Arc;

use pulldown_cmark::Event;
use pulldown_cmark::Tag;

pub(crate) mod table;

// Bound retained inline stacks and container prefixes independently of parser nesting.
pub(crate) const MAX_COPY_DEPTH: usize = 64;

// A fixed, deduplicated style order gives each nested wrapper a distinct delimiter family.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Inline {
    // Keep one link around its formatted label instead of repeating its destination per style.
    Link(Arc<str>),
    Delimiter(&'static str),
    Code,
    /// Copy a generated file target literally without changing mixed-selection Markdown.
    Literal,
    /// Balance parser events for links whose visible text has no copied wrapper.
    Ignored,
}

impl Inline {
    pub(crate) fn from_event(event: &Event<'_>) -> Option<Self> {
        match event {
            Event::Start(Tag::Emphasis) => Some(Self::Delimiter("_")),
            Event::Start(Tag::Strong) => Some(Self::Delimiter("**")),
            Event::Start(Tag::Strikethrough) => Some(Self::Delimiter("~~")),
            Event::Start(Tag::Link { dest_url, .. }) => Some(
                if crate::terminal::hyperlinks::web_destination(dest_url).is_some()
                    && !dest_url.starts_with(crate::transcript::inline_viz::LINK_PLACEHOLDER_PREFIX)
                {
                    Self::Link(dest_url.as_ref().into())
                } else {
                    Self::Ignored
                },
            ),
            Event::Code(_) => Some(Self::Code),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CopyLine {
    pub(crate) prefix: String,
    pub(crate) continuation: String,
    /// Restore the containing item when a multiline selection starts in its later paragraph.
    pub(crate) item_prefix: String,
    pub(crate) code: bool,
    pub(crate) table: Option<table::TableLine>,
    table_cell: bool,
    pub(crate) rule: bool,
    pub(crate) heading: usize,
    pub(crate) hard_break: bool,
    /// A visual separator inserted between sibling list items, never source content.
    pub(crate) omit: bool,
    runs: Vec<(Range<usize>, Vec<Inline>)>,
    literal_ranges: Vec<Range<usize>>,
}

impl CopyLine {
    pub(crate) fn push(&mut self, length: usize, inline: &[Inline]) {
        if length == 0 {
            return;
        }
        let inline = &inline[..inline.len().min(MAX_COPY_DEPTH)];
        let literal = inline
            .iter()
            .any(|mark| matches!(mark, Inline::Code | Inline::Literal));
        let mut inline = inline
            .iter()
            .filter(|mark| !matches!(mark, Inline::Ignored | Inline::Literal))
            .cloned()
            .collect::<Vec<_>>();
        inline.sort();
        inline.dedup();
        let start = self
            .runs
            .last()
            .map_or(/*default*/ 0, |(range, _)| range.end);
        // Keep literal spans separate so they never split context-sensitive prose escaping.
        if literal {
            if let Some(range) = self.literal_ranges.last_mut()
                && range.end == start
            {
                range.end += length;
            } else {
                self.literal_ranges.push(start..start + length);
            }
        }
        if let Some((range, previous)) = self.runs.last_mut()
            && previous == &inline
        {
            range.end += length;
        } else {
            self.runs.push((start..start + length, inline));
        }
    }

    #[cfg(test)]
    fn render(&self, text: &str, range: Range<usize>, depth: usize) -> String {
        if self.rule && range == (0..text.len()) {
            return "***".to_owned();
        }
        let mut out = String::new();
        let first = self.runs.partition_point(|(run, _)| run.end <= range.start);
        let mut runs = self.runs[first..]
            .iter()
            .take_while(|(run, _)| run.start < range.end)
            .peekable();
        while let Some((run, inline)) = runs.next() {
            let start = run.start.max(range.start);
            let mut end = run.end.min(range.end);
            if start >= end {
                continue;
            }
            let mark = inline.get(depth);
            while let Some((next, inline)) = runs.peek()
                && next.start == end
                && inline.get(depth) == mark
                && end < range.end
            {
                end = next.end.min(range.end);
                runs.next();
            }
            match mark {
                Some(Inline::Code) => {
                    let content = &text[start..end];
                    let content = if self.table_cell {
                        content.replace('|', "\\|")
                    } else {
                        content.to_owned()
                    };
                    let fence = fence(&content, /*minimum*/ 1);
                    let padding =
                        if content.starts_with(['`', ' ']) || content.ends_with(['`', ' ']) {
                            if content.chars().all(|ch| ch == ' ') {
                                ""
                            } else {
                                " "
                            }
                        } else {
                            ""
                        };
                    append_inline(
                        &mut out,
                        &format!("{fence}{padding}{content}{padding}{fence}"),
                    );
                }
                Some(mark) => {
                    let body = self.render(text, start..end, depth + 1);
                    let trimmed = body.trim();
                    if trimmed.is_empty() {
                        out.push_str(&body);
                        continue;
                    }
                    out.push_str(&body[..body.len() - body.trim_start().len()]);
                    match mark {
                        Inline::Delimiter(delimiter) => {
                            append_inline(&mut out, &format!("{delimiter}{trimmed}{delimiter}"))
                        }
                        Inline::Link(destination) => {
                            let destination = destination
                                .replace('\\', "\\\\")
                                .replace('&', "&amp;")
                                .replace('<', "%3C")
                                .replace('>', "%3E")
                                .replace('|', "%7C")
                                .replace('\n', "%0A")
                                .replace('\r', "%0D");
                            append_inline(&mut out, &format!("[{trimmed}](<{destination}>)"));
                        }
                        Inline::Code | Inline::Literal | Inline::Ignored => unreachable!(),
                    }
                    out.push_str(&body[body.trim_end().len()..]);
                }
                None => {
                    let heading_end = self.heading.min(end).max(start);
                    out.push_str(&text[start..heading_end]);
                    append_inline(&mut out, &escape(&text[heading_end..end]));
                }
            }
        }
        out
    }
}

// Entity decoding and omitted link wrappers can turn a delimiter's punctuation neighbor into
// a word character. Protect that boundary without changing the selected text or adding spacing.
#[cfg(test)]
fn append_inline(out: &mut String, text: &str) {
    let word = |ch: char| !ch.is_whitespace() && !ch.is_ascii_punctuation();
    let left = text.starts_with('_')
        || text.starts_with(['*', '~'])
            && text
                .chars()
                .next()
                .and_then(|delimiter| text.trim_start_matches(delimiter).chars().next())
                .is_some_and(|ch| !ch.is_alphanumeric());
    if left && let Some(ch) = out.chars().last().filter(|ch| word(*ch)) {
        out.pop();
        out.push_str(&format!("&#{};", u32::from(ch)));
    }
    let right = out.ends_with('_')
        || out.ends_with(['*', '~'])
            && out
                .chars()
                .next_back()
                .and_then(|delimiter| out.trim_end_matches(delimiter).chars().next_back())
                .is_some_and(|ch| !ch.is_alphanumeric());
    if right && let Some(ch) = text.chars().next().filter(|ch| word(*ch)) {
        out.push_str(&format!("&#{};", u32::from(ch)));
        out.push_str(&text[ch.len_utf8()..]);
    } else {
        out.push_str(text);
    }
}

// Remove unselected ancestor list padding without discarding blockquote markers.

#[cfg(test)]
fn fence(text: &str, minimum: usize) -> String {
    "`".repeat(
        text.split(|ch| ch != '`')
            .map(str::len)
            .max()
            .unwrap_or(/*default*/ 0)
            .saturating_add(/*rhs*/ 1)
            .max(minimum),
    )
}

// Append escaped prose without allowing its leading whitespace to become code indentation.

#[cfg(test)]
fn escape(text: &str) -> String {
    let mut out = String::new();
    let mut whitespace_prefix = true;
    let mut numeric_prefix = false;
    let mut number_has_trailing_space = false;
    for ch in text.chars() {
        let block_marker =
            "=-+".contains(ch) && whitespace_prefix || matches!(ch, '.' | ')') && numeric_prefix;
        if "\\`*_[]<>~#|&!".contains(ch) || block_marker {
            out.push('\\');
        }
        out.push(ch);
        if ch.is_whitespace() {
            number_has_trailing_space |= numeric_prefix;
        } else {
            numeric_prefix = ch.is_ascii_digit()
                && (whitespace_prefix || numeric_prefix && !number_has_trailing_space);
            whitespace_prefix = false;
        }
    }
    out
}

#[cfg(test)]
#[path = "markdown_copy_tests.rs"]
mod tests;
