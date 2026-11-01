//! Narrow seam for the donor's `inline_visualization.rs`
//! (`444da310e108da16aaeb18fd790b0ac464f08aca`, Apache-2.0).
//!
//! The donor module materializes `::codex-inline-vis{file="..."}` artifacts
//! through its config/sandbox subsystem and emits trusted file links.
//! the host app has no such subsystem: this port keeps the *renderer-side* half
//! of the algorithm verbatim — the code-block-aware line scan that rewrites
//! directives into a readable fallback — while `link_for` is reduced to
//! `None`, so directives always render the donor's
//! `_Visualization unavailable on this device._` notice. Placeholder-link
//! bookkeeping (`TrustedFileLink`) is retained so the render pipeline's
//! trusted-file-link retargeting compiles and harmlessly no-ops.

use pulldown_cmark::Event;
use pulldown_cmark::Options;
use pulldown_cmark::Parser;
use pulldown_cmark::Tag;
use pulldown_cmark::TagEnd;
use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;

const DIRECTIVE_PREFIX: &str = "::codex-inline-vis{";
const CONTENT_REFERENCE_PREFIX: &str = "\u{e200}visualize\u{e202}";
const CONTENT_REFERENCE_SUFFIX: char = '\u{e201}';

/// Visualization materialization context. In this adaptation the context is
/// never constructed — the donor's backing config/sandbox seam is not
/// absorbed — so directives always take the unavailable-notice branch.
#[derive(Clone, Debug)]
pub(crate) struct InlineVisualizationContext {
    _private: (),
}

/// A link created from a rewritten directive, to be retargeted after render.
pub(crate) struct TrustedFileLink {
    pub(crate) destination: url::Url,
    #[allow(dead_code)]
    pub(crate) markdown_label: String,
    #[allow(dead_code)]
    pub(crate) display_label: String,
    #[allow(dead_code)]
    pub(crate) markdown_destination_label: String,
}

pub(crate) struct InlineVisualizationRewrite<'a> {
    pub(crate) markdown: Cow<'a, str>,
    /// Empty under this adaptation: no context means no directive ever
    /// becomes a link.
    pub(crate) trusted_file_links: HashMap<String, TrustedFileLink>,
}

pub(crate) fn contains_inline_visualization(markdown: &str) -> bool {
    markdown.contains(DIRECTIVE_PREFIX) || markdown.contains(CONTENT_REFERENCE_PREFIX)
}

/// Donor `rewrite_inline_visualizations`, verbatim except that the
/// `context.link_for(file)` call is `None` — the host app never materializes
/// visualization artifacts, so every directive degrades to the donor's
/// unavailable notice.
pub(crate) fn rewrite_inline_visualizations<'a>(
    markdown: &'a str,
    _context: Option<&InlineVisualizationContext>,
) -> InlineVisualizationRewrite<'a> {
    if !contains_inline_visualization(markdown) {
        return InlineVisualizationRewrite {
            markdown: Cow::Borrowed(markdown),
            trusted_file_links: HashMap::new(),
        };
    }

    let mut code_block_ranges = Vec::<Range<usize>>::new();
    let mut code_block_start = None;
    for (event, range) in Parser::new_ext(markdown, Options::empty()).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(_)) => code_block_start = Some(range.start),
            Event::End(TagEnd::CodeBlock) => {
                if let Some(start) = code_block_start.take() {
                    code_block_ranges.push(start..range.end);
                }
            }
            _ => {}
        }
    }
    if let Some(start) = code_block_start {
        code_block_ranges.push(start..markdown.len());
    }

    let mut rewritten = String::with_capacity(markdown.len());
    let trusted_file_links = HashMap::new();
    let mut source_offset = 0;
    for source_line in markdown.split_inclusive('\n') {
        let line_start = source_offset;
        source_offset += source_line.len();
        let (line, newline) = source_line
            .strip_suffix('\n')
            .map_or((source_line, ""), |line| (line, "\n"));
        let trimmed = line.trim();
        let is_code = code_block_ranges
            .iter()
            .any(|range| range.start < source_offset && line_start < range.end);
        if is_code
            || (!trimmed.starts_with(DIRECTIVE_PREFIX)
                && !trimmed.starts_with(CONTENT_REFERENCE_PREFIX))
        {
            rewritten.push_str(line);
        } else if is_inline_visualization_line(trimmed) {
            // The donor branches here on `context.link_for(&file)`; with no
            // absorbed context every well-formed directive takes its
            // unavailable fallback.
            rewritten.push_str("_Visualization unavailable on this device._");
        }
        rewritten.push_str(newline);
    }
    InlineVisualizationRewrite {
        markdown: Cow::Owned(rewritten),
        trusted_file_links,
    }
}

/// Whether `trimmed` is a complete visualization directive or content
/// reference (donor `parse_directive_file` success condition plus the two
/// unmatched-payload fallbacks, merged because both land on the same
/// unavailable notice without a context).
fn is_inline_visualization_line(trimmed: &str) -> bool {
    if let Some(attributes) = trimmed.strip_prefix(DIRECTIVE_PREFIX) {
        return attributes
            .strip_suffix('}')
            .map(str::trim)
            .and_then(|v| v.strip_prefix("file=\""))
            .and_then(|v| v.strip_suffix('"'))
            .is_some_and(|value| !value.is_empty() && !value.contains('"'))
            || trimmed.ends_with('}');
    }
    trimmed.starts_with(CONTENT_REFERENCE_PREFIX) && trimmed.ends_with(CONTENT_REFERENCE_SUFFIX)
}

pub(crate) const LINK_PLACEHOLDER_PREFIX: &str = "https://codex.invalid/inline-visualization/";
