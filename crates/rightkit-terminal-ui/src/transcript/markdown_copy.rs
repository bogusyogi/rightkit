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
}

// Entity decoding and omitted link wrappers can turn a delimiter's punctuation neighbor into
// a word character. Protect that boundary without changing the selected text or adding spacing.

// Remove unselected ancestor list padding without discarding blockquote markers.

// Append escaped prose without allowing its leading whitespace to become code indentation.
