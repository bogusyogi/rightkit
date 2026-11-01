//! Modest markdown: headings, lists, code blocks, and inline emphasis/code
//! markers, resolved to *roles* rather than literal styling.
//!
//! This is a terminal, not a browser: there is no nested inline-span layout
//! here, only a role and a weight per logical line. Fenced code blocks and
//! headings get a distinct role and are excluded from inline-marker
//! stripping so code content survives verbatim.

use crate::component::Role;
use crate::render::{DiffKind, SemanticBlock, SemanticDocument};

/// One logical (unwrapped) line of parsed markdown, ready for width-wrapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownLine {
    pub text: String,
    pub role: Role,
    pub bold: bool,
}

/// Parse markdown into frontend-neutral blocks before TUI line styling.
/// Non-Ratatui consumers can use [`SemanticDocument::plain_lines`] directly.
pub fn semantic_document(content: &str) -> SemanticDocument {
    let mut blocks = Vec::new();
    let mut in_code = false;
    for raw in content.split('\n') {
        if raw.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            blocks.push(SemanticBlock::Code(raw.to_string()));
            continue;
        }
        let trimmed = raw.trim_start();
        if let Some(rest) = heading_text(trimmed) {
            blocks.push(SemanticBlock::Heading(strip_inline_markers(rest)));
        } else if let Some(rest) = raw.strip_prefix('+') {
            blocks.push(SemanticBlock::Diff {
                kind: DiffKind::Addition,
                text: rest.to_string(),
            });
        } else if let Some(rest) = raw.strip_prefix('-').filter(|_| !raw.starts_with("- ")) {
            blocks.push(SemanticBlock::Diff {
                kind: DiffKind::Deletion,
                text: rest.to_string(),
            });
        } else if let Some((label, target)) = whole_link(raw) {
            blocks.push(SemanticBlock::Link { label, target });
        } else {
            blocks.push(SemanticBlock::Paragraph(raw.to_string()));
        }
    }
    SemanticDocument::new(blocks)
}

fn whole_link(line: &str) -> Option<(String, String)> {
    let (label, target) = line.strip_prefix('[')?.split_once("](")?;
    let target = target.strip_suffix(')')?;
    Some((label.to_string(), target.to_string()))
}

/// Parse `content` into logical lines. Each input `\n`-separated line becomes
/// zero or one output line (a code fence marker consumes its line and emits
/// nothing); wrapping to a column width happens separately, in `wrap`.
pub fn parse(content: &str) -> Vec<MarkdownLine> {
    semantic_document(content)
        .blocks()
        .iter()
        .map(|block| match block {
            SemanticBlock::Heading(text) => MarkdownLine {
                text: text.clone(),
                role: Role::Attention,
                bold: true,
            },
            SemanticBlock::Code(text) => MarkdownLine {
                text: text.clone(),
                role: Role::Dim,
                bold: false,
            },
            SemanticBlock::Link { label, target } => MarkdownLine {
                text: format!("{label} ({target})"),
                role: Role::Text,
                bold: false,
            },
            SemanticBlock::Diff { kind, text } => MarkdownLine {
                text: format!(
                    "{}{}",
                    if *kind == DiffKind::Addition {
                        "+"
                    } else {
                        "-"
                    },
                    text
                ),
                role: Role::Text,
                bold: false,
            },
            SemanticBlock::Paragraph(text) => MarkdownLine {
                text: if let Some(rest) =
                    text.strip_prefix("- ").or_else(|| text.strip_prefix("* "))
                {
                    format!("• {}", strip_inline_markers(rest))
                } else {
                    strip_inline_markers(text)
                },
                role: Role::Text,
                bold: false,
            },
        })
        .collect()
}

/// `"# Title"` through `"###### Title"` -> the text after the marker.
fn heading_text(line: &str) -> Option<&str> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    line.get(hashes..)?.strip_prefix(' ')
}

/// Drop emphasis and inline-code delimiters, keeping the content they wrap.
///
/// Deliberately not per-span: a paragraph mixing plain and `code` text
/// renders as one role for the whole line. Readable, and a fraction of the
/// wrapping complexity a real inline-span model would add.
fn strip_inline_markers(s: &str) -> String {
    s.replace("**", "")
        .replace("__", "")
        .replace(['*', '_', '`'], "")
}

#[cfg(test)]
mod semantic_tests {
    use super::*;
    use crate::render::{DiffKind, SemanticBlock};

    #[test]
    fn semantic_projection_preserves_cross_frontend_structure() {
        let document = semantic_document(
            "# Title\n```rust\nlet x = 1;\n```\n+added\n-removed\n[docs](https://example.test)",
        );
        assert!(matches!(document.blocks()[0], SemanticBlock::Heading(_)));
        assert!(matches!(document.blocks()[1], SemanticBlock::Code(_)));
        assert!(matches!(
            document.blocks()[2],
            SemanticBlock::Diff {
                kind: DiffKind::Addition,
                ..
            }
        ));
        assert!(matches!(
            document.blocks()[3],
            SemanticBlock::Diff {
                kind: DiffKind::Deletion,
                ..
            }
        ));
        assert!(matches!(document.blocks()[4], SemanticBlock::Link { .. }));
        assert_eq!(document.plain_lines()[0], "Title");
    }
}

// ── REN-003: safe streaming-markdown boundary ──────────────────────────────
//
// Canon donor pin: `aaif-goose/goose@dbf57c81fd33e765509eb3ae383108968b9576c2`,
// `crates/goose-cli/src/session/streaming_buffer.rs`. `StreamingMarkdownBuffer`
// below is adapted from that file (tokenizer regex, `ParseState`, and the
// line-start/inline-token scan in `find_safe_end`/`process_line_start`/
// `check_code_fence`/`process_inline_token` are carried over near-verbatim);
// see `NOTICE` for attribution. Renamed to fit this module (`push`/`flush`
// keep the donor's names and semantics) and adapted to slot in front of
// [`parse`] above rather than Goose's `bat`-backed CLI renderer — this file
// does not import any of Goose's CLI/session/table architecture, and
// the host app's native scrollback is untouched: a caller streams chunks
// through this buffer and feeds only what it releases into [`parse`].
//
// Invariant this exists to guarantee: whatever a caller does with partial
// releases, once the stream ends and `flush` is called, the concatenation of
// every released piece is byte-identical to the whole message, so
// `parse(&concatenation)` is `parse(&whole_message)` — incremental and
// whole-message render semantics can never diverge. `find_safe_end` is what
// keeps that possible: it never releases text with an open code fence,
// inline-code span, emphasis/strikethrough marker, link/image bracket, or a
// pending ATX heading, and it only ever cuts at a token boundary the
// tokenizer regex agrees is a full UTF-8 codepoint (or at a bare `\n`), so a
// release can never split a multibyte character or emit invalid UTF-8.
use regex::Regex;
use std::sync::LazyLock;

static INLINE_TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(",
        r"\\.",                 // Escaped char (highest priority)
        r"|`+",                 // Inline code (variable length backticks)
        r"|\*\*\*",             // Bold+italic
        r"|\*\*",               // Bold
        r"|\*",                 // Italic
        r"|___",                // Bold+italic (underscore)
        r"|__",                 // Bold (underscore)
        r"|_",                  // Italic (underscore)
        r"|~~",                 // Strikethrough
        r"|\!\[",               // Image start
        r"|\]\(",               // Link URL start
        r"|\[",                 // Link text start
        r"|\]",                 // Bracket close (without following paren)
        r"|\)",                 // Link URL end
        r"|[^\\\*_`~\[\]!()]+", // Plain text (no special chars)
        r"|.",                  // Any other single char
        r")"
    ))
    .expect("streaming-markdown tokenizer regex is a fixed, compile-time-checked pattern")
});

/// A streaming-safe markdown buffer.
///
/// Accumulates chunks as they arrive and, on each [`push`](Self::push),
/// releases the longest prefix whose markdown constructs are all closed
/// (no open code fence, inline code span, emphasis, strikethrough, link, or
/// image). The unclosed tail stays buffered until either a later chunk
/// closes it or the stream ends and [`flush`](Self::flush) forces it out.
#[derive(Debug, Default)]
pub struct StreamingMarkdownBuffer {
    buffer: String,
}

/// Which markdown constructs are open at the current scan position.
#[derive(Default, Debug, Clone, PartialEq)]
struct ParseState {
    in_code_block: bool,
    code_fence_char: char,
    code_fence_len: usize,
    in_table: bool,
    pending_heading: bool,
    in_inline_code: bool,
    inline_code_len: usize,
    in_bold: bool,
    in_italic: bool,
    in_strikethrough: bool,
    in_link_text: bool,
    in_link_url: bool,
    in_image_alt: bool,
}

impl ParseState {
    fn is_clean(&self) -> bool {
        !self.in_code_block
            && !self.in_table
            && !self.pending_heading
            && !self.in_inline_code
            && !self.in_bold
            && !self.in_italic
            && !self.in_strikethrough
            && !self.in_link_text
            && !self.in_link_url
            && !self.in_image_alt
    }
}

// SAFETY: every slice below cuts at a byte offset that is either the start
// of a full regex token match (the tokenizer's alternatives each consume
// whole UTF-8 sequences: `.` in the `regex` crate matches one Unicode
// scalar value, not one byte) or immediately after an ASCII `\n`. Neither
// case can land inside a multibyte codepoint, so every `&str` slice here is
// a valid UTF-8 boundary and every released `String` is valid UTF-8.
#[allow(clippy::string_slice)]
impl StreamingMarkdownBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a chunk to the buffer and return the prefix that is now safe to
    /// render, if any grew safe as a result of this chunk.
    pub fn push(&mut self, chunk: &str) -> Option<String> {
        self.buffer.push_str(chunk);
        let safe_end = self.find_safe_end();

        if safe_end > 0 {
            let to_render = self.buffer[..safe_end].to_string();
            self.buffer = self.buffer[safe_end..].to_string();
            Some(to_render)
        } else {
            None
        }
    }

    /// Force out whatever remains buffered, closed or not. Call this once,
    /// at the end of the stream.
    pub fn flush(&mut self) -> String {
        std::mem::take(&mut self.buffer)
    }

    fn find_safe_end(&self) -> usize {
        let mut state = ParseState::default();
        let mut last_safe: usize = 0;
        let bytes = self.buffer.as_bytes();
        let len = bytes.len();
        let mut pos: usize = 0;

        while pos < len {
            let at_line_start = pos == 0 || bytes[pos - 1] == b'\n';

            if at_line_start && let Some(new_pos) = self.process_line_start(&mut state, pos) {
                pos = new_pos;
                if state.is_clean() {
                    last_safe = pos;
                }
                continue;
            }

            if state.in_code_block {
                while pos < len && bytes[pos] != b'\n' {
                    pos += 1;
                }
                if pos < len {
                    pos += 1;
                }
                continue;
            }

            let remaining = &self.buffer[pos..];
            let line_end = remaining.find('\n').map(|i| pos + i + 1).unwrap_or(len);
            let line_content = &self.buffer[pos..line_end];

            for cap in INLINE_TOKEN_RE.find_iter(line_content) {
                let token = cap.as_str();
                let token_end = pos + cap.end();

                self.process_inline_token(&mut state, token);

                if state.is_clean() {
                    last_safe = token_end;
                }
            }

            if line_end <= len && line_end > pos && bytes[line_end - 1] == b'\n' {
                state.pending_heading = false;
                if state.is_clean() {
                    last_safe = line_end;
                }
            }

            pos = line_end;
        }

        last_safe
    }

    fn process_line_start(&self, state: &mut ParseState, pos: usize) -> Option<usize> {
        let remaining = &self.buffer[pos..];

        if state.pending_heading {
            state.pending_heading = false;
        }

        if let Some(fence_result) = self.check_code_fence(remaining, state) {
            return Some(pos + fence_result);
        }

        if state.in_code_block {
            return None;
        }

        if remaining.starts_with('#') {
            let hashes = remaining.chars().take_while(|&c| c == '#').count();
            if hashes <= 6 {
                let after_hashes = &remaining[hashes..];
                if after_hashes.is_empty()
                    || after_hashes.starts_with(' ')
                    || after_hashes.starts_with('\n')
                {
                    state.pending_heading = true;
                    return None;
                }
            }
        }

        if remaining.starts_with('|') {
            state.in_table = true;
            return None;
        }

        if (remaining.starts_with('\n') || remaining.is_empty()) && state.in_table {
            state.in_table = false;
            return Some(pos + 1);
        }

        if state.in_table && !remaining.starts_with('|') {
            state.in_table = false;
        }

        None
    }

    fn check_code_fence(&self, line: &str, state: &mut ParseState) -> Option<usize> {
        let trimmed = line.trim_start();

        let fence_char = trimmed.chars().next()?;
        if fence_char != '`' && fence_char != '~' {
            return None;
        }

        let fence_len = trimmed.chars().take_while(|&c| c == fence_char).count();
        if fence_len < 3 {
            return None;
        }

        let after_fence = &trimmed[fence_len..];

        if state.in_code_block {
            if fence_char == state.code_fence_char
                && fence_len >= state.code_fence_len
                && (after_fence.is_empty()
                    || after_fence.starts_with('\n')
                    || after_fence.trim().is_empty())
            {
                state.in_code_block = false;
                state.code_fence_char = '\0';
                state.code_fence_len = 0;

                if let Some(newline_pos) = line.find('\n') {
                    return Some(newline_pos + 1);
                } else {
                    return Some(line.len());
                }
            }
        } else {
            state.in_code_block = true;
            state.code_fence_char = fence_char;
            state.code_fence_len = fence_len;

            if let Some(newline_pos) = line.find('\n') {
                return Some(newline_pos + 1);
            } else {
                return Some(line.len());
            }
        }

        None
    }

    fn process_inline_token(&self, state: &mut ParseState, token: &str) {
        if token.starts_with('\\') && token.len() == 2 {
            return;
        }

        if token.starts_with('`') {
            let tick_count = token.len();
            if state.in_inline_code {
                if tick_count == state.inline_code_len {
                    state.in_inline_code = false;
                    state.inline_code_len = 0;
                }
            } else {
                state.in_inline_code = true;
                state.inline_code_len = tick_count;
            }
            return;
        }

        if state.in_inline_code {
            return;
        }

        match token {
            "***" | "___" => {
                if state.in_bold && state.in_italic {
                    state.in_bold = false;
                    state.in_italic = false;
                } else if state.in_bold {
                    state.in_italic = !state.in_italic;
                } else if state.in_italic {
                    state.in_bold = !state.in_bold;
                } else {
                    state.in_bold = true;
                    state.in_italic = true;
                }
            }
            "**" | "__" => {
                state.in_bold = !state.in_bold;
            }
            "*" | "_" => {
                state.in_italic = !state.in_italic;
            }
            "~~" => {
                state.in_strikethrough = !state.in_strikethrough;
            }
            "![" => {
                state.in_image_alt = true;
            }
            "[" => {
                if !state.in_link_text && !state.in_image_alt {
                    state.in_link_text = true;
                }
            }
            "](" => {
                if state.in_link_text {
                    state.in_link_text = false;
                    state.in_link_url = true;
                } else if state.in_image_alt {
                    state.in_image_alt = false;
                    state.in_link_url = true;
                }
            }
            "]" => {}
            ")" if state.in_link_url => {
                state.in_link_url = false;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod legacy_semantic_tests {
    use super::*;

    #[test]
    fn a_heading_gets_the_attention_role_and_bold_weight() {
        let lines = parse("# Title");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "Title");
        assert_eq!(lines[0].role, Role::Attention);
        assert!(lines[0].bold);
    }

    #[test]
    fn headings_up_to_level_six_are_recognised() {
        for level in 1..=6 {
            let src = format!("{} heading", "#".repeat(level));
            let lines = parse(&src);
            assert_eq!(lines[0].text, "heading", "level {level}");
            assert_eq!(lines[0].role, Role::Attention);
        }
    }

    #[test]
    fn a_seven_hash_line_is_not_a_heading() {
        let lines = parse("####### not a heading");
        assert_eq!(lines[0].role, Role::Text);
    }

    #[test]
    fn dash_and_star_bullets_both_get_a_bullet_marker() {
        assert_eq!(parse("- one")[0].text, "• one");
        assert_eq!(parse("* two")[0].text, "• two");
    }

    #[test]
    fn a_fenced_code_block_keeps_its_content_verbatim_with_the_dim_role() {
        let lines = parse("```\nlet x = *y;\n```");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "let x = *y;");
        assert_eq!(lines[0].role, Role::Dim);
    }

    #[test]
    fn inline_emphasis_and_code_markers_are_stripped_from_paragraph_text() {
        let lines = parse("plain **bold** and `code` and _em_");
        assert_eq!(lines[0].text, "plain bold and code and em");
        assert_eq!(lines[0].role, Role::Text);
    }

    #[test]
    fn a_blank_line_survives_as_an_empty_line_not_disappears() {
        let lines = parse("first\n\nsecond");
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[1].text, "");
    }

    // ── REN-003 differential corpus ────────────────────────────────────────
    //
    // For each sample: split at every byte boundary, stream it through
    // `StreamingMarkdownBuffer` one byte-slice at a time, and assert (a) the
    // concatenation of every released piece is byte-identical to the whole
    // input (so `parse` of the reassembly is `parse` of the whole message),
    // and (b) every released piece — and the reassembly itself — is valid
    // UTF-8, i.e. no split ever cuts a multibyte codepoint.

    /// Stream `input` through a fresh buffer, splitting at byte offset `cut`:
    /// one `push` with `input[..cut]`, one with `input[cut..]`, then `flush`.
    /// Returns the concatenation of everything released.
    fn stream_split_at(input: &str, cut: usize) -> String {
        let mut buf = StreamingMarkdownBuffer::new();
        let mut out = String::new();
        if let Some(piece) = buf.push(&input[..cut]) {
            out.push_str(&piece);
        }
        if let Some(piece) = buf.push(&input[cut..]) {
            out.push_str(&piece);
        }
        out.push_str(&buf.flush());
        out
    }

    /// For one sample, split at every valid UTF-8 boundary (`cut` ranges over
    /// `0..=input.len()`, but only where `input.is_char_boundary(cut)` holds —
    /// a `cut` that is not on a char boundary is not a byte offset any real
    /// streaming transport would ever hand us, since chunk boundaries there
    /// would already be invalid UTF-8 on the wire) and assert the incremental
    /// reassembly equals the whole-message input exactly.
    fn assert_every_split_reassembles(input: &str) {
        for cut in 0..=input.len() {
            if !input.is_char_boundary(cut) {
                continue;
            }
            let reassembled = stream_split_at(input, cut);
            assert_eq!(
                reassembled, input,
                "split at byte {cut} of {input:?} did not reassemble to the original"
            );
            // `stream_split_at` only ever builds `reassembled` out of pieces
            // returned as `String`, which the compiler already guarantees
            // are valid UTF-8 — the assertion above is itself the strongest
            // available proof no split corrupted a codepoint: any corruption
            // would show up as a mismatch (or `push`/`flush` would have had
            // to fail to produce a `String` at all, which is not possible
            // through this buffer's public API).
            assert_eq!(
                parse(&reassembled),
                parse(input),
                "incremental render diverged from whole-message render for split at byte {cut} of {input:?}"
            );
        }
    }

    #[test]
    fn fenced_code_block_reassembles_at_every_split() {
        assert_every_split_reassembles(
            "before\n```rust\nfn main() {\n    println!(\"hi\");\n}\n```\nafter\n",
        );
    }

    #[test]
    fn nested_backticks_inside_fence_reassemble_at_every_split() {
        assert_every_split_reassembles(
            "example:\n\n```python\nprint(\"```nested```\")\n```\n\ndone!",
        );
    }

    #[test]
    fn unclosed_fence_reassembles_at_every_split() {
        assert_every_split_reassembles("```\ncode without a closing fence");
    }

    #[test]
    fn inline_code_reassembles_at_every_split() {
        assert_every_split_reassembles("Use the `println!` macro to print.");
    }

    #[test]
    fn unclosed_inline_code_reassembles_at_every_split() {
        assert_every_split_reassembles("Start of `code that never closes");
    }

    #[test]
    fn links_and_images_reassemble_at_every_split() {
        assert_every_split_reassembles(
            "See [the docs](https://docs.rs) and ![a screenshot](./img/screen.png) for more.",
        );
    }

    #[test]
    fn unclosed_link_reassembles_at_every_split() {
        assert_every_split_reassembles("Check [broken link](http://");
    }

    #[test]
    fn emphasis_variants_reassemble_at_every_split() {
        assert_every_split_reassembles(
            "plain **bold** and *italic* and ***both*** and __also bold__ and ~~struck~~ text",
        );
    }

    #[test]
    fn nested_and_incomplete_emphasis_reassembles_at_every_split() {
        assert_every_split_reassembles("**bold *and italic* together** but this **never closes");
    }

    #[test]
    fn escapes_reassemble_at_every_split() {
        assert_every_split_reassembles(r"Use \* for bullets, not \`code\`, and \_ for italics.");
    }

    #[test]
    fn table_reassembles_at_every_split() {
        assert_every_split_reassembles(
            "| Name | Value |\n|------|-------|\n| foo  | 42    |\n\nMore text after the table.",
        );
    }

    #[test]
    fn heading_reassembles_at_every_split() {
        assert_every_split_reassembles("## Getting Started\n\nFirst, install the tool.");
    }

    #[test]
    fn utf8_multibyte_content_reassembles_at_every_split() {
        // Multibyte in plain text, inside emphasis, inside a code fence, and
        // inside a table cell — every construct this buffer tracks, with
        // codepoints of varying byte width (2, 3, and 4 bytes).
        assert_every_split_reassembles(
            "héllo 世界! **太字** and `código` 🎉\n\n```rust\nlet 名前 = \"🦀\";\n```\n\n| A | 值 |\n|---|---|\n| 1 | 二 |\n",
        );
    }

    #[test]
    fn plain_sentence_with_no_markdown_reassembles_at_every_split() {
        assert_every_split_reassembles("A perfectly ordinary sentence with no markup at all.");
    }
}

// ---- donor codex-rs/tui/src/markdown.rs (444da310, Apache-2.0) ----
// Adapted seams: crate::inline_visualization -> transcript::inline_viz (narrow,
// context-free), git_action_directives::strip_line_directives -> the local
// strip_git_line_directives below (visible-text half of the donor algorithm).

use pulldown_cmark::CodeBlockKind;
use pulldown_cmark::Event;
use pulldown_cmark::Tag;
use pulldown_cmark::TagEnd;
#[cfg(test)]
use ratatui::text::Line;
use std::borrow::Cow;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use crate::terminal::hyperlinks::HyperlinkLine;
use crate::transcript::inline_viz::InlineVisualizationContext;
use crate::transcript::inline_viz::rewrite_inline_visualizations;
use crate::transcript::markdown_render::ListSpacing;
use crate::transcript::table_detect;

/// Render markdown source to styled ratatui lines and append them to `lines`.
///
/// Callers that already know the session working directory should pass it here so streamed and
/// non-streamed rendering show the same relative path text even if the process cwd differs.
#[cfg(test)]
pub(crate) fn append_markdown(
    markdown_source: &str,
    width: Option<usize>,
    cwd: Option<&Path>,
    lines: &mut Vec<Line<'static>>,
) {
    let rendered = crate::transcript::markdown_render::render_markdown_text_with_width_and_cwd(
        markdown_source,
        width,
        cwd,
    );
    crate::terminal::line_utils::push_owned_lines(&rendered.lines, lines);
}

/// Render an agent message to styled ratatui lines.
///
/// Before rendering, the source is passed through [`unwrap_markdown_fences`] so that tables
/// wrapped in `` ```md `` fences are rendered as native tables rather than code blocks.
/// Non-markdown fences (e.g. `rust`, `sh`) are left
/// intact.
#[cfg(test)]
pub(crate) fn append_markdown_agent(
    markdown_source: &str,
    width: Option<usize>,
    lines: &mut Vec<Line<'static>>,
) {
    let normalized = normalize_markdown_for_rendering(markdown_source);
    let rendered = crate::transcript::markdown_render::render_markdown_text_with_width_and_cwd(
        &normalized,
        width,
        /*cwd*/ None,
    );
    crate::terminal::line_utils::push_owned_lines(&rendered.lines, lines);
}

#[cfg(test)]
pub(crate) fn render_markdown_agent_with_links_and_cwd(
    markdown_source: &str,
    width: Option<usize>,
    cwd: Option<&Path>,
) -> Vec<HyperlinkLine> {
    render_markdown_agent_with_list_spacing(
        markdown_source,
        width,
        cwd,
        /*inline_visualization_context*/ None,
        ListSpacing::AfterMultiline,
    )
}

pub(crate) fn render_markdown_agent_with_list_spacing(
    markdown_source: &str,
    width: Option<usize>,
    cwd: Option<&Path>,
    inline_visualization_context: Option<&InlineVisualizationContext>,
    list_spacing: ListSpacing,
) -> Vec<HyperlinkLine> {
    let rewritten = rewrite_inline_visualizations(markdown_source, inline_visualization_context);
    let normalized = normalize_markdown_for_rendering(&rewritten.markdown);
    let is_hidden_link_destination = |destination: &str| {
        rewritten.trusted_file_links.contains_key(destination)
            || crate::transcript::markdown_render::hide_web_link_destination(destination)
    };
    let mut lines =
        crate::transcript::markdown_render::render_streaming_markdown_lines_with_width_and_cwd(
            &normalized,
            width,
            cwd,
            &is_hidden_link_destination,
            list_spacing,
        )
        .lines;
    for hyperlink in lines.iter_mut().flat_map(|line| &mut line.hyperlinks) {
        if let Some(link) = rewritten.trusted_file_links.get(&hyperlink.destination) {
            hyperlink.retarget_to_trusted_file(&link.destination);
        }
    }
    lines
}

/// Render an agent message and collect the block metadata needed for incremental rendering.
///
/// Block offsets are mapped back to `markdown_source` after Markdown table fences are unwrapped.
/// If a normalized boundary cannot be expressed as a raw-source suffix, it is discarded so the
/// transformed block remains mutable.
#[cfg(test)]
pub(crate) fn render_streaming_markdown_agent_with_links_and_cwd(
    markdown_source: &str,
    width: Option<usize>,
    cwd: Option<&Path>,
    list_spacing: ListSpacing,
) -> crate::transcript::markdown_render::StreamingMarkdownRender {
    let normalized = normalize_markdown_for_rendering(markdown_source);
    let mut rendered =
        crate::transcript::markdown_render::render_streaming_markdown_lines_with_width_and_cwd(
            &normalized,
            width,
            cwd,
            &crate::transcript::markdown_render::hide_web_link_destination,
            list_spacing,
        );
    if normalized != markdown_source {
        // Fence unwrapping removes opening/closing lines. A normalized tail that is still a raw
        // suffix necessarily begins after those removed lines, so its boundary can safely be
        // mapped back to the raw source; otherwise leave the transformed block mutable.
        rendered.pending_math_start = rendered.pending_math_start.map(|boundary| {
            markdown_source
                .strip_suffix(&normalized[boundary..])
                .map_or(0, str::len)
        });
        rendered.last_top_level_block_start = rendered
            .last_top_level_block_start
            .and_then(|boundary| markdown_source.strip_suffix(&normalized[boundary..]))
            .map(str::len);
        rendered.mutable_fence_start = rendered.mutable_fence_start.map(|boundary| {
            markdown_source
                .strip_suffix(&normalized[boundary..])
                .map_or(0, str::len)
        });
    }
    rendered
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CopyTarget {
    Code {
        language: Option<String>,
        content: Arc<str>,
    },
    Quote(Arc<str>),
}

pub(crate) fn extract_copy_targets(markdown_source: &str) -> Vec<CopyTarget> {
    let mut targets = Vec::new();
    let mut quote: Option<(Range<usize>, usize, bool)> = None;
    let mut quote_depth = 0usize;
    let mut in_code_block = false;
    let mut code = None;

    for (event, range) in pulldown_cmark::Parser::new(markdown_source).into_offset_iter() {
        match event {
            Event::Start(Tag::BlockQuote(_)) => {
                if quote_depth == 0 {
                    quote = Some((range, targets.len(), false));
                }
                quote_depth += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                quote_depth = quote_depth.saturating_sub(1);
                if quote_depth == 0 {
                    quote = None;
                }
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                in_code_block = true;
                if let CodeBlockKind::Fenced(info) = kind {
                    let language = info
                        .split([',', ' ', '\t'])
                        .next()
                        .filter(|language| !language.is_empty())
                        .map(str::to_owned);
                    code = Some((language, String::new()));
                }
            }
            Event::Text(text) | Event::Code(text) => {
                if in_code_block {
                    if let Some((_, content)) = &mut code {
                        if text.starts_with('\n')
                            && range.start > 0
                            && markdown_source.as_bytes()[range.start - 1] == b'\r'
                        {
                            content.push('\r');
                        }
                        content.push_str(&text);
                    }
                } else if let Some((span, position, emitted)) = &mut quote
                    && !*emitted
                    && !strip_git_line_directives(&text).trim().is_empty()
                {
                    let content = markdown_source[span.clone()]
                        .split_inclusive('\n')
                        .map(|line| {
                            line.trim_start_matches(' ')
                                .strip_prefix('>')
                                .map_or(line, |line| line.strip_prefix(' ').unwrap_or(line))
                        })
                        .collect::<String>();
                    targets.insert(*position, CopyTarget::Quote(content.into()));
                    *emitted = true;
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code_block = false;
                if let Some((language, content)) = code.take() {
                    targets.push(CopyTarget::Code {
                        language,
                        content: content.into(),
                    });
                }
            }
            _ => {}
        }
    }

    targets
}

/// Apply table-fence normalization only when terminal table layouts are enabled.
pub(crate) fn normalize_markdown_for_rendering(markdown_source: &str) -> Cow<'_, str> {
    if crate::transcript::markdown_render::preferences::current().tables {
        unwrap_markdown_fences(markdown_source)
    } else {
        Cow::Borrowed(markdown_source)
    }
}

/// Strip `` ```md ``/`` ```markdown `` fences that contain tables, emitting their content as bare
/// markdown so `pulldown-cmark` parses the tables natively.
///
/// Fences whose info string is not `md` or `markdown` are passed through unchanged.  Markdown
/// fences that do *not* contain a table (detected by checking for a header row + delimiter row)
/// are also passed through so that non-table markdown inside a fence still renders as a code
/// block.
///
/// The fence unwrapping is intentionally conservative: it buffers the entire fence body before
/// deciding, and an unclosed fence at end-of-input is re-emitted with its opening line so partial
/// streams degrade to code display.
pub(crate) fn unwrap_markdown_fences<'a>(markdown_source: &'a str) -> Cow<'a, str> {
    // Zero-copy fast path: most messages contain no fences at all.
    if !markdown_source.contains("```") && !markdown_source.contains("~~~") {
        return Cow::Borrowed(markdown_source);
    }

    #[derive(Clone, Copy)]
    struct Fence {
        marker: u8,
        len: usize,
        is_blockquoted: bool,
    }

    // Strip a trailing newline and up to 3 leading spaces, returning the
    // trimmed slice.  Returns `None` when the line has 4+ leading spaces
    // (which makes it an indented code line per CommonMark).
    fn strip_line_indent(line: &str) -> Option<&str> {
        let without_newline = line.strip_suffix('\n').unwrap_or(line);
        let mut byte_idx = 0usize;
        let mut column = 0usize;
        for b in without_newline.as_bytes() {
            match b {
                b' ' => {
                    byte_idx += 1;
                    column += 1;
                }
                b'\t' => {
                    byte_idx += 1;
                    column += 4;
                }
                _ => break,
            }
            if column >= 4 {
                return None;
            }
        }
        Some(&without_newline[byte_idx..])
    }

    // Parse an opening fence line, returning the fence metadata and whether
    // the fence info string indicates markdown content.
    fn parse_open_fence(line: &str) -> Option<(Fence, bool)> {
        let trimmed = strip_line_indent(line)?;
        let is_blockquoted = trimmed.trim_start().starts_with('>');
        let fence_scan_text = table_detect::strip_blockquote_prefix(trimmed);
        let (marker, len) = table_detect::parse_fence_marker(fence_scan_text)?;
        let is_markdown = table_detect::is_markdown_fence_info(fence_scan_text, len);
        Some((
            Fence {
                marker: marker as u8,
                len,
                is_blockquoted,
            },
            is_markdown,
        ))
    }

    fn is_close_fence(line: &str, fence: Fence) -> bool {
        let Some(trimmed) = strip_line_indent(line) else {
            return false;
        };
        let fence_scan_text = if fence.is_blockquoted {
            if !trimmed.trim_start().starts_with('>') {
                return false;
            }
            table_detect::strip_blockquote_prefix(trimmed)
        } else {
            trimmed
        };
        if let Some((marker, len)) = table_detect::parse_fence_marker(fence_scan_text) {
            marker as u8 == fence.marker
                && len >= fence.len
                && fence_scan_text[len..].trim().is_empty()
        } else {
            false
        }
    }

    fn markdown_fence_contains_table(content: &str, is_blockquoted_fence: bool) -> bool {
        let mut previous_line: Option<&str> = None;
        for line in content.lines() {
            let text = if is_blockquoted_fence {
                table_detect::strip_blockquote_prefix(line)
            } else {
                line
            };
            let trimmed = text.trim();
            if trimmed.is_empty() {
                previous_line = None;
                continue;
            }

            if let Some(previous) = previous_line
                && table_detect::is_table_header_line(previous)
                && !table_detect::is_table_delimiter_line(previous)
                && table_detect::is_table_delimiter_line(trimmed)
            {
                return true;
            }

            previous_line = Some(trimmed);
        }
        false
    }

    fn content_from_ranges(source: &str, ranges: &[Range<usize>]) -> String {
        let total_len: usize = ranges.iter().map(ExactSizeIterator::len).sum();
        let mut content = String::with_capacity(total_len);
        for range in ranges {
            content.push_str(&source[range.start..range.end]);
        }
        content
    }

    struct MarkdownCandidateData {
        fence: Fence,
        opening_range: Range<usize>,
        content_ranges: Vec<Range<usize>>,
    }

    // Box the large variant to keep ActiveFence small (~pointer-sized).
    enum ActiveFence {
        Passthrough(Fence),
        MarkdownCandidate(Box<MarkdownCandidateData>),
    }

    let mut out = String::with_capacity(markdown_source.len());
    let mut active_fence: Option<ActiveFence> = None;
    let mut source_offset = 0usize;

    let mut push_source_range = |range: Range<usize>| {
        if !range.is_empty() {
            out.push_str(&markdown_source[range]);
        }
    };

    for line in markdown_source.split_inclusive('\n') {
        let line_start = source_offset;
        source_offset += line.len();
        let line_range = line_start..source_offset;

        if let Some(active) = active_fence.take() {
            match active {
                ActiveFence::Passthrough(fence) => {
                    push_source_range(line_range);
                    if !is_close_fence(line, fence) {
                        active_fence = Some(ActiveFence::Passthrough(fence));
                    }
                }
                ActiveFence::MarkdownCandidate(mut data) => {
                    if is_close_fence(line, data.fence) {
                        if markdown_fence_contains_table(
                            &content_from_ranges(markdown_source, &data.content_ranges),
                            data.fence.is_blockquoted,
                        ) {
                            for range in data.content_ranges {
                                push_source_range(range);
                            }
                        } else {
                            push_source_range(data.opening_range);
                            for range in data.content_ranges {
                                push_source_range(range);
                            }
                            push_source_range(line_range);
                        }
                    } else {
                        data.content_ranges.push(line_range);
                        active_fence = Some(ActiveFence::MarkdownCandidate(data));
                    }
                }
            }
            continue;
        }

        if let Some((fence, is_markdown)) = parse_open_fence(line) {
            if is_markdown {
                active_fence = Some(ActiveFence::MarkdownCandidate(Box::new(
                    MarkdownCandidateData {
                        fence,
                        opening_range: line_range,
                        content_ranges: Vec::new(),
                    },
                )));
            } else {
                push_source_range(line_range);
                active_fence = Some(ActiveFence::Passthrough(fence));
            }
            continue;
        }

        push_source_range(line_range);
    }

    if let Some(active) = active_fence {
        match active {
            ActiveFence::Passthrough(_) => {}
            ActiveFence::MarkdownCandidate(data) => {
                push_source_range(data.opening_range);
                for range in data.content_ranges {
                    push_source_range(range);
                }
            }
        }
    }

    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use ratatui::text::Line;

    #[test]
    fn copy_targets_preserve_fenced_code_language_and_exact_content() {
        let markdown =
            "```python title=example\nprint('hi')\n```\n\n    ignored()\n\n~~~\nplain()\n~~~";
        assert_eq!(
            extract_copy_targets(markdown),
            vec![
                CopyTarget::Code {
                    language: Some("python".to_string()),
                    content: Arc::from("print('hi')\n"),
                },
                CopyTarget::Code {
                    language: None,
                    content: Arc::from("plain()\n"),
                },
            ]
        );
        assert_eq!(
            extract_copy_targets("```powershell\r\nGet-Item .\r\nWrite-Output ok\r\n```\r\n"),
            vec![CopyTarget::Code {
                language: Some("powershell".to_string()),
                content: Arc::from("Get-Item .\r\nWrite-Output ok\r\n"),
            }]
        );
    }

    #[test]
    fn copy_targets_preserve_nested_quote_markdown_and_source_order() {
        let markdown = "> outer **bold**\n> > inner *quote*\n> ```sh\n> nested()\n> ```\n";
        assert_eq!(
            extract_copy_targets(markdown),
            vec![
                CopyTarget::Quote(Arc::from(
                    "outer **bold**\n> inner *quote*\n```sh\nnested()\n```\n"
                )),
                CopyTarget::Code {
                    language: Some("sh".to_string()),
                    content: Arc::from("nested()\n"),
                },
            ]
        );
    }

    #[test]
    fn copy_targets_exclude_blockquotes_without_prose() {
        let markdown = concat!(
            "> > ::git-stage{cwd=\"/repo\"}\n\n",
            "> ::git-stage{cwd=\"/repo\"}\n> ```sh\n> code()\n> ```\n\n",
            "> quoted prose\n"
        );
        assert_eq!(
            extract_copy_targets(markdown),
            vec![
                CopyTarget::Code {
                    language: Some("sh".to_string()),
                    content: Arc::from("code()\n"),
                },
                CopyTarget::Quote(Arc::from("quoted prose\n")),
            ]
        );
    }

    fn lines_to_strings(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.clone())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn citations_render_as_plain_text() {
        let src = "Before 【F:/x.rs†L1】\nAfter 【F:/x.rs†L3】\n";
        let mut out = Vec::new();
        append_markdown(src, /*width*/ None, /*cwd*/ None, &mut out);
        let rendered = lines_to_strings(&out);
        assert_eq!(
            rendered,
            vec![
                "Before 【F:/x.rs†L1】".to_string(),
                "After 【F:/x.rs†L3】".to_string()
            ]
        );
    }

    #[test]
    fn indented_code_blocks_preserve_leading_whitespace() {
        // Basic sanity: indented code with surrounding blank lines should produce the indented line.
        let src = "Before\n\n    code 1\n\nAfter\n";
        let mut out = Vec::new();
        append_markdown(src, /*width*/ None, /*cwd*/ None, &mut out);
        let lines = lines_to_strings(&out);
        assert_eq!(lines, vec!["Before", "", "    code 1", "", "After"]);
    }

    #[test]
    fn append_markdown_preserves_full_text_line() {
        let src = "Hi! How can I help with codex-rs today? Want me to explore the repo, run tests, or work on a specific change?\n";
        let mut out = Vec::new();
        append_markdown(src, /*width*/ None, /*cwd*/ None, &mut out);
        assert_eq!(
            out.len(),
            1,
            "expected a single rendered line for plain text"
        );
        let rendered: String = out
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.clone())
            .collect::<Vec<_>>()
            .join("");
        assert_eq!(
            rendered,
            "Hi! How can I help with codex-rs today? Want me to explore the repo, run tests, or work on a specific change?"
        );
    }

    #[test]
    fn append_markdown_matches_tui_markdown_for_ordered_item() {
        let mut out = Vec::new();
        append_markdown(
            "1. Tight item\n",
            /*width*/ None,
            /*cwd*/ None,
            &mut out,
        );
        let lines = lines_to_strings(&out);
        assert_eq!(lines, vec!["1. Tight item".to_string()]);
    }

    #[test]
    fn append_markdown_keeps_ordered_list_line_unsplit_in_context() {
        let src = "Loose vs. tight list items:\n1. Tight item\n";
        let mut out = Vec::new();
        append_markdown(src, /*width*/ None, /*cwd*/ None, &mut out);

        let lines = lines_to_strings(&out);

        // Expect to find the ordered list line rendered as a single line,
        // not split into a marker-only line followed by the text.
        assert!(
            lines.iter().any(|s| s == "1. Tight item"),
            "expected '1. Tight item' rendered as a single line; got: {lines:?}"
        );
        assert!(
            !lines
                .windows(2)
                .any(|w| w[0].trim_end() == "1." && w[1] == "Tight item"),
            "did not expect a split into ['1.', 'Tight item']; got: {lines:?}"
        );
    }

    #[test]
    fn append_markdown_agent_unwraps_markdown_fences_for_table_rendering() {
        let src = "```markdown\n| A | B |\n|---|---|\n| 1 | 2 |\n```\n";
        let mut out = Vec::new();
        append_markdown_agent(src, /*width*/ None, &mut out);
        let rendered = lines_to_strings(&out);
        assert!(rendered.iter().any(|line| line.contains('━')));
        assert!(rendered.iter().any(|line| line.contains(" 1      2")));
    }

    #[test]
    fn append_markdown_agent_unwraps_markdown_fences_for_no_outer_table_rendering() {
        let src = "```md\nCol A | Col B | Col C\n--- | --- | ---\nx | y | z\n10 | 20 | 30\n```\n";
        let mut out = Vec::new();
        append_markdown_agent(src, /*width*/ None, &mut out);
        let rendered = lines_to_strings(&out);
        assert!(rendered.iter().any(|line| line.contains('━')));
        assert!(
            rendered
                .iter()
                .any(|line| line.contains(" Col A    Col B    Col C"))
        );
        assert!(
            !rendered
                .iter()
                .any(|line| line.trim() == "Col A | Col B | Col C")
        );
    }

    #[test]
    fn append_markdown_agent_unwraps_markdown_fences_for_two_column_no_outer_table() {
        let src = "```md\nA | B\n--- | ---\nleft | right\n```\n";
        let mut out = Vec::new();
        append_markdown_agent(src, /*width*/ None, &mut out);
        let rendered = lines_to_strings(&out);
        assert!(rendered.iter().any(|line| line.contains('━')));
        assert!(rendered.iter().any(|line| line.contains(" left    right")));
        assert!(!rendered.iter().any(|line| line.trim() == "A | B"));
    }

    #[test]
    fn append_markdown_agent_unwraps_markdown_fences_for_single_column_table() {
        let src = "```md\n| Only |\n|---|\n| value |\n```\n";
        let mut out = Vec::new();
        append_markdown_agent(src, /*width*/ None, &mut out);
        let rendered = lines_to_strings(&out);
        assert!(rendered.iter().any(|line| line.contains('━')));
        assert!(!rendered.iter().any(|line| line.trim() == "| Only |"));
    }

    #[test]
    fn append_markdown_agent_keeps_non_markdown_fences_as_code() {
        let src = "```rust\n| A | B |\n|---|---|\n| 1 | 2 |\n```\n";
        let mut out = Vec::new();
        append_markdown_agent(src, /*width*/ None, &mut out);
        let rendered = lines_to_strings(&out);
        assert_eq!(
            rendered,
            vec![
                "| A | B |".to_string(),
                "|---|---|".to_string(),
                "| 1 | 2 |".to_string(),
            ]
        );
    }

    #[test]
    fn append_markdown_agent_unwraps_blockquoted_markdown_fence_table() {
        let src = "> ```markdown\n> | A | B |\n> |---|---|\n> | 1 | 2 |\n> ```\n";
        let rendered = unwrap_markdown_fences(src);
        assert!(
            !rendered.contains("```"),
            "expected markdown fence markers to be removed: {rendered:?}"
        );
    }

    #[test]
    fn append_markdown_agent_keeps_non_blockquoted_markdown_fence_with_blockquote_table_example() {
        let src = "```markdown\n> | A | B |\n> |---|---|\n> | 1 | 2 |\n```\n";
        let normalized = unwrap_markdown_fences(src);
        assert_eq!(normalized, src);
    }

    #[test]
    fn append_markdown_agent_keeps_markdown_fence_when_content_is_not_table() {
        let src = "```markdown\n**bold**\n```\n";
        let mut out = Vec::new();
        append_markdown_agent(src, /*width*/ None, &mut out);
        let rendered = lines_to_strings(&out);
        assert_eq!(rendered, vec!["**bold**".to_string()]);
    }

    #[test]
    fn unwrap_markdown_fences_repro_keeps_fence_without_header_delimiter_pair() {
        let src = "```markdown\n| A | B |\nnot a delimiter row\n| --- | --- |\n# Heading\n```\n";
        let normalized = unwrap_markdown_fences(src);
        assert_eq!(normalized, src);
    }

    #[test]
    fn append_markdown_agent_keeps_markdown_fence_with_blank_line_between_header_and_delimiter() {
        let src = "```markdown\n| A | B |\n\n|---|---|\n| 1 | 2 |\n```\n";
        let rendered = unwrap_markdown_fences(src);
        assert_eq!(rendered, src);
    }
}

/// Visible text of `line` with `::git-*{...}` directives removed.
///
/// Narrow seam for the donor's
/// `git_action_directives::strip_line_directives` (same donor revision):
/// the host app does not absorb the git-action side effects, only the
/// directive-stripping scan the copy-target extractor needs so quoted prose
/// made only of directives is excluded.
fn strip_git_line_directives(line: &str) -> String {
    use super::assistant_directives::{QuoteEscaping, parse_assistant_directive};

    let mut visible = String::new();
    let mut remaining = line;
    while let Some(start) = remaining.find("::git-") {
        visible.push_str(&remaining[..start]);
        let source = &remaining[start..];
        if let Some(directive) = parse_assistant_directive(source, QuoteEscaping::Literal) {
            remaining = &source[directive.raw.len()..];
        } else if let Some((_, attributes)) = source.split_once('{')
            && let Some((_, suffix)) = attributes.split_once('}')
        {
            remaining = suffix;
        } else {
            visible.push_str(source);
            return visible;
        }
    }
    visible.push_str(remaining);
    visible
}
