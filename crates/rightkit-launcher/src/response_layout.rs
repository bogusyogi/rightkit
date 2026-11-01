//! Structured display of an AI answer for the native Windows launcher and notch
//! (the response document model of the in-launcher assistant).
//!
//! A local equivalent of the shared display model: no JSON bridge, no Swift. It has three parts.
//!
//! * `parse` turns answer text into blocks with a deliberately small grammar (paragraphs, headings,
//!   bullet and numbered lists, block quotes, fenced code, simple tables, emphasis, strong, inline
//!   code, links). HTML and anything unsupported stay literal text. Over 128 KiB or 500 nodes the
//!   original text is shown as one paragraph; the stored answer is never shortened. The block
//!   grammar follows `swift/RightKitHub/Sources/RightKitPresentation/TextDocument.swift`: single
//!   newlines are kept, a run of two or more blank lines adds line breaks, a fence's language is
//!   the first word of its info string, a list item is parsed once from its whole source text
//!   (continuation lines joined by a newline), and nested items are an indent plus their own marker.
//!   The inline grammar (code spans, strong, emphasis, links) is this file's own, not a port.
//! * `layout` turns a document into wrapped visual lines the launcher draws one per row. Every
//!   character of the source is kept; tables become stacked records with each cell's header
//!   repeated; code keeps its whitespace and wraps by character.
//! * `excerpt` is the one line preview for the notch card.
//!
//! Accepted simpler treatment (W9): the launcher draws one face and size per line, so emphasis,
//! strong and inline code keep their text but not their styling here, because styling them would
//! need a new mixed-face line renderer; a link shows as `label (address)` and is never activated.
//! A heading's level 1 and levels 2 to 6 are two distinct sizes (`Role::Title`, `Role::Heading`).
//! Fixture note: `docs/fixtures/response-ui/markdown-cases.json` (inputs and expected blocks, the
//! Swift parser as reference) is read by no Rust code and this crate adds no unit test for it; a
//! journey or the root integration owner compares `parse` against it (cases: `name`, `input`,
//! `expect`, text of a block being the plain text of its runs, a list item's nested marker being
//! this file's layout prefix rather than text).

use std::collections::HashMap;

/// Largest source text, in bytes, that is parsed into structure.
pub const MAX_BYTES: usize = 128 * 1024;
/// Most structural nodes (blocks, list items, table rows) one document may hold.
pub const MAX_NODES: usize = 500;

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_bytes: usize,
    pub max_nodes: usize,
}

impl Limits {
    pub const DEFAULT: Limits = Limits {
        max_bytes: MAX_BYTES,
        max_nodes: MAX_NODES,
    };
}

/// A stretch of inline text with one style.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InlineRun {
    pub text: String,
    pub emphasis: bool,
    pub strong: bool,
    pub code: bool,
    pub link: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ListItem {
    /// Nesting level, 0 for the outermost.
    pub depth: usize,
    pub ordered: bool,
    /// The number as written, for a nested ordered item (a top-level item is numbered by position).
    pub number: u64,
    pub runs: Vec<InlineRun>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TextElement {
    Heading(u8, Vec<InlineRun>),
    Paragraph(Vec<InlineRun>),
    List {
        ordered: bool,
        start: u64,
        items: Vec<ListItem>,
    },
    Quote(Vec<InlineRun>),
    Code {
        language: String,
        text: String,
    },
    Table {
        headers: Vec<Vec<InlineRun>>,
        rows: Vec<Vec<Vec<InlineRun>>>,
    },
    LineBreak,
}

/// One answer, ready to lay out: its blocks in source order.
#[derive(Clone, Debug, PartialEq)]
pub struct ResponseDocument {
    pub blocks: Vec<TextElement>,
}

impl ResponseDocument {
    /// Parses `text` once.
    pub fn from_text(text: &str, limits: &Limits) -> Self {
        ResponseDocument {
            blocks: parse(text, limits),
        }
    }

    /// `text` as plain wrapped paragraphs with no structure (progress and error lines).
    pub fn plain(text: &str) -> Self {
        ResponseDocument {
            blocks: fallback(text),
        }
    }
}

fn literal(text: &str) -> InlineRun {
    InlineRun {
        text: text.to_string(),
        ..InlineRun::default()
    }
}

fn fallback(text: &str) -> Vec<TextElement> {
    vec![TextElement::Paragraph(vec![literal(text)])]
}

// ------------------------------------------------------------------ parsing

/// Parse state that stops the whole document: a limit was passed, so the original text is shown.
struct Overflow;

/// A block, the source lines it used and the nodes it counts.
type Parsed = (TextElement, usize, usize);

/// Spaces and tabs only, as the Swift side trims.
fn trim(text: &str) -> &str {
    text.trim_matches(|c: char| c == '\t' || (c.is_whitespace() && c != '\n' && c != '\r'))
}

/// Parses `text` with the small grammar. Over a limit the original text comes back as one
/// paragraph; nothing about the text itself changes.
pub fn parse(text: &str, limits: &Limits) -> Vec<TextElement> {
    if text.len() > limits.max_bytes {
        return fallback(text);
    }
    let normal = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normal.split('\n').collect();
    let mut blocks: Vec<TextElement> = Vec::new();
    let mut nodes = 0usize;
    let mut blanks = 0usize;
    let mut i = 0usize;
    while i < lines.len() {
        if trim(lines[i]).is_empty() {
            blanks += 1;
            i += 1;
            continue;
        }
        // Two or more blank lines in a row add line breaks (at most three), as on the Swift side.
        if blanks >= 2 && !blocks.is_empty() {
            for _ in 0..(blanks - 1).min(3) {
                blocks.push(TextElement::LineBreak);
                nodes += 1;
            }
        }
        blanks = 0;
        let Ok((block, used, count)) = block_at(&lines, i, limits) else {
            return fallback(text);
        };
        blocks.push(block);
        i += used.max(1);
        nodes += count;
        if nodes > limits.max_nodes {
            return fallback(text);
        }
    }
    blocks
}

/// The block that starts at `lines[i]`, the lines it used and the nodes it counts.
fn block_at(lines: &[&str], i: usize, limits: &Limits) -> Result<Parsed, Overflow> {
    let line = lines[i];
    let trimmed = trim(line);
    if let Some(fence) = fence_open(line) {
        return fenced(lines, i, &fence);
    }
    if let Some((level, rest)) = heading(trimmed) {
        return Ok((TextElement::Heading(level, inline(rest)), 1, 1));
    }
    if let Some(table) = table_at(lines, i, limits)? {
        return Ok(table);
    }
    if trimmed.starts_with('>') {
        let mut j = i;
        let mut body: Vec<&str> = Vec::new();
        while j < lines.len() && trim(lines[j]).starts_with('>') {
            let rest = &trim(lines[j])[1..];
            body.push(rest.strip_prefix(' ').unwrap_or(rest));
            j += 1;
        }
        return Ok((TextElement::Quote(inline(&body.join("\n"))), j - i, 1));
    }
    if marker(line).is_some() {
        return Ok(list_at(lines, i));
    }
    // A paragraph: lines up to a blank line or the start of another block, joined by newlines.
    let mut parts: Vec<&str> = vec![trimmed];
    let mut j = i + 1;
    while j < lines.len() {
        let next = trim(lines[j]);
        if next.is_empty()
            || fence_open(lines[j]).is_some()
            || heading(next).is_some()
            || next.starts_with('>')
            || marker(lines[j]).is_some()
            || table_starts(lines, j)
        {
            break;
        }
        parts.push(next);
        j += 1;
    }
    Ok((TextElement::Paragraph(inline(&parts.join("\n"))), j - i, 1))
}

struct Fence {
    mark: char,
    count: usize,
    indent: usize,
    language: String,
}

fn fence_open(line: &str) -> Option<Fence> {
    let indent = line.chars().take_while(|c| *c == ' ').count();
    if indent >= 4 {
        return None;
    }
    let rest = &line[indent..];
    let mark = rest.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let count = rest.chars().take_while(|c| *c == mark).count();
    if count < 3 {
        return None;
    }
    let info = trim(&rest[count..]);
    if mark == '`' && info.contains('`') {
        return None;
    }
    // The language is the first word of the info string.
    let language = info
        .split([' ', '\t'])
        .find(|word| !word.is_empty())
        .unwrap_or("")
        .to_string();
    Some(Fence {
        mark,
        count,
        indent,
        language,
    })
}

/// A fenced block. An unclosed fence runs to the end of the text, so nothing is lost.
fn fenced(lines: &[&str], i: usize, fence: &Fence) -> Result<Parsed, Overflow> {
    let mut body: Vec<&str> = Vec::new();
    let mut j = i + 1;
    while j < lines.len() {
        let stripped = trim(lines[j]);
        if stripped.chars().count() >= fence.count && stripped.chars().all(|c| c == fence.mark) {
            j += 1;
            break;
        }
        let mut line = lines[j];
        for _ in 0..fence.indent {
            match line.strip_prefix(' ') {
                Some(rest) => line = rest,
                None => break,
            }
        }
        body.push(line);
        j += 1;
        if body.len() > 100_000 {
            return Err(Overflow);
        }
    }
    Ok((
        TextElement::Code {
            language: fence.language.clone(),
            text: body.join("\n"),
        },
        j - i,
        1,
    ))
}

/// A heading line (already trimmed): its level and text, without an optional closing run of `#`.
fn heading(trimmed: &str) -> Option<(u8, &str)> {
    let level = trimmed.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &trimmed[level..];
    if !(rest.is_empty() || rest.starts_with([' ', '\t'])) {
        return None;
    }
    let mut content = trim(rest);
    let stripped = content.trim_end_matches('#');
    if stripped.len() < content.len() {
        if stripped.is_empty() {
            content = "";
        } else if stripped.ends_with(char::is_whitespace) {
            content = stripped.trim_end();
        }
    }
    Some((level as u8, content))
}

struct Marker<'a> {
    indent: usize,
    ordered: bool,
    number: u64,
    content: &'a str,
}

/// A list marker: its indent, whether it is numbered, the number and the item text.
fn marker(line: &str) -> Option<Marker<'_>> {
    let mut indent = 0usize;
    let mut at = 0usize;
    for (b, c) in line.char_indices() {
        match c {
            ' ' => indent += 1,
            '\t' => indent += 4,
            _ => {
                at = b;
                break;
            }
        }
        at = b + c.len_utf8();
    }
    if indent >= 8 || at >= line.len() {
        return None;
    }
    let rest = &line[at..];
    let first = rest.chars().next()?;
    let blank_or_end = |text: &str| text.is_empty() || text.starts_with([' ', '\t']);
    if matches!(first, '-' | '*' | '+') {
        let after = &rest[1..];
        if !blank_or_end(after) {
            return None;
        }
        let content = trim(after);
        // A line of only repeated markers ("***", "- - -") is a rule, not an item.
        if content.is_empty() && rest.chars().count() > 1 {
            return None;
        }
        return Some(Marker {
            indent,
            ordered: false,
            number: 0,
            content,
        });
    }
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if !(1..=9).contains(&digits) {
        return None;
    }
    let after = &rest[digits..];
    if !matches!(after.chars().next(), Some('.' | ')')) {
        return None;
    }
    let tail = &after[1..];
    if !blank_or_end(tail) {
        return None;
    }
    Some(Marker {
        indent,
        ordered: true,
        number: rest[..digits].parse().unwrap_or(1),
        content: trim(tail),
    })
}

/// One list item's source before it is parsed: continuation lines are joined to it by a newline.
struct RawItem {
    depth: usize,
    ordered: bool,
    number: u64,
    text: String,
}

fn list_at(lines: &[&str], i: usize) -> Parsed {
    let head = marker(lines[i]);
    let first_indent = head.as_ref().map_or(0, |m| m.indent);
    let ordered = head.as_ref().is_some_and(|m| m.ordered);
    let start = head
        .as_ref()
        .map_or(1, |m| if m.ordered { m.number } else { 1 });
    let mut raw: Vec<RawItem> = Vec::new();
    let mut j = i;
    while j < lines.len() {
        let line = lines[j];
        let text = trim(line);
        if text.is_empty() {
            // A blank line keeps the list going only when the next line is another item of it.
            let mut k = j + 1;
            while k < lines.len() && trim(lines[k]).is_empty() {
                k += 1;
            }
            match lines.get(k).and_then(|l| marker(l)) {
                Some(next) if next.indent >= first_indent + 2 || next.ordered == ordered => {
                    j = k;
                    continue;
                }
                _ => break,
            }
        }
        if let Some(m) = marker(line) {
            let nested = m.indent >= first_indent + 2;
            if !nested && m.ordered != ordered {
                break;
            }
            let depth = if nested {
                ((m.indent - first_indent) / 2).clamp(1, 4)
            } else {
                0
            };
            raw.push(RawItem {
                depth,
                ordered: m.ordered,
                number: m.number,
                text: m.content.to_string(),
            });
            j += 1;
            continue;
        }
        // A continuation of the last item: any line indented deeper than the list.
        let indent = line.chars().take_while(|c| *c == ' ' || *c == '\t').count();
        if indent > first_indent
            && let Some(last) = raw.last_mut()
        {
            last.text.push('\n');
            last.text.push_str(text);
            j += 1;
            continue;
        }
        break;
    }
    let count = 1 + raw.len();
    // Each item is parsed once, from its whole source text.
    let items = raw
        .into_iter()
        .map(|item| ListItem {
            depth: item.depth,
            ordered: item.ordered,
            number: item.number,
            runs: inline(&item.text),
        })
        .collect();
    (
        TextElement::List {
            ordered,
            start,
            items,
        },
        j - i,
        count,
    )
}

/// The cells of a table line; `\|` is a literal bar and any other backslash stays as written.
fn table_cells(line: &str) -> Vec<String> {
    let mut text = trim(line);
    if let Some(rest) = text.strip_prefix('|') {
        text = rest;
    }
    if text.ends_with('|')
        && !text.ends_with("\\|")
        && let Some(rest) = text.strip_suffix('|')
    {
        text = rest;
    }
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut escaped = false;
    for ch in text.chars() {
        if escaped {
            if ch != '|' {
                current.push('\\');
            }
            current.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '|' {
            out.push(trim(&current).to_string());
            current.clear();
        } else {
            current.push(ch);
        }
    }
    if escaped {
        current.push('\\');
    }
    out.push(trim(&current).to_string());
    out
}

fn separator_row(line: &str) -> bool {
    let cells = table_cells(line);
    !cells.is_empty()
        && cells.iter().all(|cell| {
            let body = cell.strip_prefix(':').unwrap_or(cell);
            let body = body.strip_suffix(':').unwrap_or(body);
            !body.is_empty() && body.chars().all(|c| c == '-')
        })
}

/// Whether a table's header line starts at `lines[i]` (ends a paragraph).
fn table_starts(lines: &[&str], i: usize) -> bool {
    lines[i].contains('|')
        && i + 1 < lines.len()
        && lines[i + 1].contains('-')
        && separator_row(lines[i + 1])
}

/// A table at `lines[i]`, `Ok(None)` when there is none, `Err` when it is past a size limit.
fn table_at(lines: &[&str], i: usize, limits: &Limits) -> Result<Option<Parsed>, Overflow> {
    if !table_starts(lines, i) {
        return Ok(None);
    }
    let header_cells = table_cells(lines[i]);
    if header_cells.len() != table_cells(lines[i + 1]).len() {
        return Ok(None);
    }
    // Checked before padding: bytes and row counts alone do not bound width x height.
    const MAX_COLUMNS: usize = 32;
    const MAX_CELLS: usize = 4096;
    if header_cells.len() > MAX_COLUMNS {
        return Err(Overflow);
    }
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut j = i + 2;
    while j < lines.len() && !trim(lines[j]).is_empty() && lines[j].contains('|') {
        let cells = table_cells(lines[j]);
        if cells.len() > MAX_COLUMNS {
            return Err(Overflow);
        }
        rows.push(cells);
        j += 1;
        if rows.len() > limits.max_nodes {
            return Err(Overflow);
        }
    }
    // No column is dropped: a longer row widens the table, a shorter one is padded.
    let width = rows
        .iter()
        .map(Vec::len)
        .chain([header_cells.len()])
        .max()
        .unwrap_or(0);
    if width > MAX_COLUMNS || rows.len() + 1 > MAX_CELLS / width.max(1) {
        return Err(Overflow);
    }
    let pad = |mut cells: Vec<String>| -> Vec<Vec<InlineRun>> {
        cells.resize(width, String::new());
        cells.iter().map(|c| inline(c)).collect()
    };
    let count = 1 + rows.len();
    let headers = pad(header_cells);
    let rows = rows.into_iter().map(pad).collect();
    Ok(Some((TextElement::Table { headers, rows }, j - i, count)))
}

// ------------------------------------------------------------------ inline

/// Inline runs of `text`: code spans, strong, emphasis and links. Unclosed or unsupported
/// markers stay as the characters that were typed.
pub fn inline(text: &str) -> Vec<InlineRun> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    inline_into(&chars, false, false, None, &mut out);
    merge(out)
}

fn push_run(
    out: &mut Vec<InlineRun>,
    text: &str,
    emphasis: bool,
    strong: bool,
    code: bool,
    link: Option<&str>,
) {
    if text.is_empty() {
        return;
    }
    out.push(InlineRun {
        text: text.to_string(),
        emphasis,
        strong,
        code,
        link: link.map(str::to_string),
    });
}

fn merge(runs: Vec<InlineRun>) -> Vec<InlineRun> {
    let mut out: Vec<InlineRun> = Vec::new();
    for run in runs {
        if let Some(last) = out.last_mut()
            && last.emphasis == run.emphasis
            && last.strong == run.strong
            && last.code == run.code
            && last.link == run.link
        {
            last.text.push_str(&run.text);
            continue;
        }
        out.push(run);
    }
    out
}

fn inline_into(c: &[char], em: bool, st: bool, link: Option<&str>, out: &mut Vec<InlineRun>) {
    let mut buf = String::new();
    let mut i = 0usize;
    while i < c.len() {
        let ch = c[i];
        match ch {
            '\\' if i + 1 < c.len() && c[i + 1].is_ascii_punctuation() => {
                buf.push(c[i + 1]);
                i += 2;
            }
            '`' => {
                let n = c[i..].iter().take_while(|x| **x == '`').count();
                match find_ticks(c, i + n, n) {
                    Some(end) => {
                        push_run(out, &buf, em, st, false, link);
                        buf.clear();
                        let mut inner: String = c[i + n..end].iter().collect();
                        if inner.len() >= 2
                            && inner.starts_with(' ')
                            && inner.ends_with(' ')
                            && !inner.trim().is_empty()
                        {
                            inner = inner[1..inner.len() - 1].to_string();
                        }
                        push_run(out, &inner, em, st, true, link);
                        i = end + n;
                    }
                    None => {
                        buf.extend(std::iter::repeat_n('`', n));
                        i += n;
                    }
                }
            }
            '*' | '_' => {
                let run = c[i..].iter().take_while(|x| **x == ch).count();
                let d = run.min(3);
                if let Some(end) = find_closer(c, i, ch, d) {
                    push_run(out, &buf, em, st, false, link);
                    buf.clear();
                    inline_into(&c[i + d..end], em || d != 2, st || d >= 2, link, out);
                    i = end + d;
                } else {
                    buf.extend(std::iter::repeat_n(ch, run));
                    i += run;
                }
            }
            '!' if c.get(i + 1) == Some(&'[') => {
                // An image is never fetched; its alt text and reference stay as written.
                if let Some((_, _, after)) = link_at(c, i + 1) {
                    buf.extend(c[i..after].iter());
                    i = after;
                } else {
                    buf.push('!');
                    i += 1;
                }
            }
            '[' => {
                if let Some((label, url, after)) = link_at(c, i) {
                    push_run(out, &buf, em, st, false, link);
                    buf.clear();
                    inline_into(label, em, st, Some(url.as_str()), out);
                    i = after;
                } else {
                    buf.push('[');
                    i += 1;
                }
            }
            _ => {
                buf.push(ch);
                i += 1;
            }
        }
    }
    push_run(out, &buf, em, st, false, link);
}

fn find_ticks(c: &[char], from: usize, n: usize) -> Option<usize> {
    let mut j = from;
    while j < c.len() {
        if c[j] == '`' {
            let run = c[j..].iter().take_while(|x| **x == '`').count();
            if run == n {
                return Some(j);
            }
            j += run;
        } else {
            j += 1;
        }
    }
    None
}

/// The start of the closing run of `d` copies of `ch` for an opener at `open`.
fn find_closer(c: &[char], open: usize, ch: char, d: usize) -> Option<usize> {
    let first = open + d;
    let next = *c.get(first)?;
    if next.is_whitespace() {
        return None;
    }
    if ch == '_' && open > 0 && c[open - 1].is_alphanumeric() {
        return None;
    }
    let mut j = first + 1;
    while j + d <= c.len() {
        if c[j] == '`' {
            // Skip a code span so its contents cannot close the emphasis.
            let n = c[j..].iter().take_while(|x| **x == '`').count();
            j = find_ticks(c, j + n, n).map_or(j + n, |end| end + n);
            continue;
        }
        if c[j] == ch {
            let run = c[j..].iter().take_while(|x| **x == ch).count();
            let closes = run >= d
                && !c[j - 1].is_whitespace()
                && (ch != '_' || c.get(j + d).is_none_or(|x| !x.is_alphanumeric()));
            if closes && (d != 1 || run == 1) {
                return Some(j);
            }
            j += run;
        } else {
            j += 1;
        }
    }
    None
}

/// `[label](address)` at `open`: the label, the address and the index after it.
fn link_at(c: &[char], open: usize) -> Option<(&[char], String, usize)> {
    let close = c[open + 1..].iter().position(|x| *x == ']')? + open + 1;
    if c[open + 1..close].contains(&'[') || c.get(close + 1) != Some(&'(') {
        return None;
    }
    let end = c[close + 2..].iter().position(|x| *x == ')')? + close + 2;
    let url: String = c[close + 2..end].iter().collect();
    if url.is_empty() || url.chars().any(char::is_whitespace) || !link_target_ok(&url) {
        return None;
    }
    Some((&c[open + 1..close], url, end + 1))
}

/// Only http, https (with a host) and mailto (with an address) targets are links, as on the
/// Swift side; any other `[label](target)` stays the characters that were typed.
fn link_target_ok(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    if let Some(rest) = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
    {
        return rest
            .split(['/', '?', '#'])
            .next()
            .is_some_and(|host| !host.is_empty());
    }
    lower
        .strip_prefix("mailto:")
        .is_some_and(|rest| !rest.is_empty())
}

/// The text of `runs` as it is drawn: a link also shows its address.
pub fn runs_text(runs: &[InlineRun]) -> String {
    let mut out = String::new();
    for run in runs {
        out.push_str(&run.text);
        if let Some(url) = &run.link
            && *url != run.text
        {
            out.push_str(" (");
            out.push_str(url);
            out.push(')');
        }
    }
    out
}

// ------------------------------------------------------------------ layout

/// How a visual line is drawn.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum Role {
    /// A level 1 heading, the larger of the two heading sizes.
    Title,
    Heading,
    Body,
    Metadata,
    Code,
}

/// One wrapped row. `indent` is in the same unit as the measured widths; `block` is the index of
/// the source block, which stays the same when the width changes.
#[derive(Clone, Debug, PartialEq)]
pub struct VisualLine {
    pub text: String,
    pub role: Role,
    pub indent: f32,
    pub block: usize,
}

/// The source block shown at row `top`.
pub fn block_at_row(lines: &[VisualLine], top: usize) -> usize {
    lines
        .get(top.min(lines.len().saturating_sub(1)))
        .map_or(0, |line| line.block)
}

/// The first row of `block`: where the scroll position goes after the width changed.
pub fn row_of_block(lines: &[VisualLine], block: usize) -> usize {
    lines
        .iter()
        .position(|line| line.block >= block)
        .unwrap_or(lines.len().saturating_sub(1))
}

struct Measurer<'a> {
    measure: &'a mut dyn FnMut(&str, Role) -> f32,
    cache: HashMap<(Role, String), f32>,
}

impl Measurer<'_> {
    fn width(&mut self, text: &str, role: Role) -> f32 {
        if let Some(width) = self.cache.get(&(role, text.to_string())) {
            return *width;
        }
        let width = (self.measure)(text, role);
        self.cache.insert((role, text.to_string()), width);
        width
    }

    fn space(&mut self, role: Role) -> f32 {
        let space = self.width(" ", role);
        if space > 0.0 {
            space
        } else {
            self.width("n", role) * 0.4
        }
    }
}

struct Layout<'a> {
    m: Measurer<'a>,
    width: f32,
    unit: f32,
    lines: Vec<VisualLine>,
    block: usize,
}

impl Layout<'_> {
    fn push(&mut self, text: String, role: Role, indent: f32) {
        self.lines.push(VisualLine {
            text,
            role,
            indent,
            block: self.block,
        });
    }

    fn room(&self, indent: f32) -> f32 {
        (self.width - indent).max(self.unit.max(1.0) * 2.0)
    }

    /// Wraps prose at word breaks (a word longer than the line breaks by character). `prefix` is
    /// a list or quote mark on the first row; later rows hang under the text.
    fn flow(&mut self, text: &str, role: Role, indent: f32, prefix: &str) {
        let pw = if prefix.is_empty() {
            0.0
        } else {
            self.m.width(prefix.trim_end(), role) + self.m.space(role)
        };
        let avail = self.room(indent + pw);
        let rows = self.wrap_prose(text, role, avail);
        if rows.is_empty() {
            self.push(prefix.to_string(), role, indent);
            return;
        }
        for (k, row) in rows.into_iter().enumerate() {
            if k == 0 {
                self.push(format!("{prefix}{row}"), role, indent);
            } else {
                self.push(row, role, indent + pw);
            }
        }
    }

    fn wrap_prose(&mut self, text: &str, role: Role, avail: f32) -> Vec<String> {
        let mut rows: Vec<String> = Vec::new();
        let space = self.m.space(role);
        for hard in text.split('\n') {
            let mut current = String::new();
            let mut used = 0.0f32;
            let mut any = false;
            for word in hard.split(' ').filter(|w| !w.is_empty()) {
                any = true;
                let w = self.m.width(word, role);
                if !current.is_empty() && used + space + w <= avail {
                    current.push(' ');
                    current.push_str(word);
                    used += space + w;
                    continue;
                }
                if !current.is_empty() {
                    rows.push(std::mem::take(&mut current));
                }
                if w <= avail {
                    current.push_str(word);
                    used = w;
                } else {
                    let (done, rest, rest_w) = self.break_chars(word, role, avail);
                    rows.extend(done);
                    current = rest;
                    used = rest_w;
                }
            }
            if !current.is_empty() || !any {
                rows.push(current);
            }
        }
        rows
    }

    /// Splits `text` by character into full rows; the last, partial row is returned apart.
    fn break_chars(&mut self, text: &str, role: Role, avail: f32) -> (Vec<String>, String, f32) {
        let mut done = Vec::new();
        let mut current = String::new();
        let mut used = 0.0f32;
        let mut buf = [0u8; 4];
        for ch in text.chars() {
            let w = self.m.width(ch.encode_utf8(&mut buf), role);
            if !current.is_empty() && used + w > avail {
                done.push(std::mem::take(&mut current));
                used = 0.0;
            }
            current.push(ch);
            used += w;
        }
        (done, current, used)
    }

    /// Code keeps every character and space; a long line wraps by character.
    fn code(&mut self, text: &str, indent: f32) {
        let avail = self.room(indent);
        for source in text.split('\n') {
            if source.is_empty() {
                self.push(String::new(), Role::Code, indent);
                continue;
            }
            let (mut done, rest, _) = self.break_chars(source, Role::Code, avail);
            if !rest.is_empty() {
                done.push(rest);
            }
            for row in done {
                self.push(row, Role::Code, indent);
            }
        }
    }

    fn blank(&mut self) {
        self.push(String::new(), Role::Body, 0.0);
    }

    fn element(&mut self, element: &TextElement) {
        let unit = self.unit;
        match element {
            TextElement::Heading(level, runs) => {
                let text = runs_text(runs);
                let role = if *level == 1 {
                    Role::Title
                } else {
                    Role::Heading
                };
                self.flow(&text, role, 0.0, "");
            }
            TextElement::Paragraph(runs) => {
                let text = runs_text(runs);
                self.flow(&text, Role::Body, 0.0, "");
            }
            TextElement::List {
                ordered,
                start,
                items,
            } => {
                // A top-level item is numbered from the list's start by its position among the
                // top-level items; a nested item shows its own marker with an en dash for a bullet.
                let mut position = 0u64;
                for item in items {
                    let prefix = if item.depth > 0 {
                        if item.ordered {
                            format!("{}. ", item.number)
                        } else {
                            "\u{2013} ".to_string()
                        }
                    } else if *ordered {
                        position += 1;
                        format!("{}. ", start + position - 1)
                    } else {
                        "\u{2022} ".to_string()
                    };
                    let text = runs_text(&item.runs);
                    self.flow(&text, Role::Body, item.depth as f32 * unit, &prefix);
                }
            }
            TextElement::Quote(runs) => {
                let text = runs_text(runs);
                self.flow(&text, Role::Body, unit * 0.5, "| ");
            }
            TextElement::Code { language, text } => {
                if !language.is_empty() {
                    self.flow(language, Role::Metadata, 0.0, "");
                }
                self.code(text, unit * 0.5);
            }
            TextElement::Table { headers, rows } => self.table(headers, rows),
            TextElement::LineBreak => self.blank(),
        }
    }

    /// Stacked records: each cell under its header, a blank row between records. A table with no
    /// rows shows its header cells on one row so no column is lost.
    fn table(&mut self, headers: &[Vec<InlineRun>], rows: &[Vec<Vec<InlineRun>>]) {
        if rows.is_empty() {
            let joined: Vec<String> = headers.iter().map(|h| runs_text(h)).collect();
            self.flow(&joined.join(" | "), Role::Metadata, 0.0, "");
            return;
        }
        for (r, row) in rows.iter().enumerate() {
            if r > 0 {
                self.blank();
            }
            for (c, cell) in row.iter().enumerate() {
                let header = headers.get(c).map(|h| runs_text(h)).unwrap_or_default();
                let value = runs_text(cell);
                let value = if value.trim().is_empty() {
                    "\u{2014}".to_string()
                } else {
                    value
                };
                if !header.trim().is_empty() {
                    self.flow(&header, Role::Metadata, 0.0, "");
                }
                self.flow(&value, Role::Body, self.unit * 0.75, "");
            }
        }
    }
}

/// Wraps `doc` to `width` (the same unit `measure` returns, normally pixels) into visual lines.
/// `indent_unit` is one nesting step. A blank row separates blocks, except after a heading; a
/// line break block is one more blank row.
pub fn layout(
    doc: &ResponseDocument,
    width: f32,
    indent_unit: f32,
    measure: &mut dyn FnMut(&str, Role) -> f32,
) -> Vec<VisualLine> {
    let mut ctx = Layout {
        m: Measurer {
            measure,
            cache: HashMap::new(),
        },
        width,
        unit: indent_unit,
        lines: Vec::new(),
        block: 0,
    };
    for (index, element) in doc.blocks.iter().enumerate() {
        ctx.block = index;
        let after_heading = index > 0 && matches!(doc.blocks[index - 1], TextElement::Heading(..));
        if index > 0 && !after_heading && !matches!(element, TextElement::LineBreak) {
            ctx.blank();
        }
        ctx.element(element);
    }
    if ctx.lines.is_empty() {
        ctx.blank();
    }
    ctx.lines
}

// ------------------------------------------------------------------ excerpt

/// The preview of `text` for the notch: the first substantive text with display-only markers
/// (heading hashes, quote marks, fences, table rules, emphasis and code ticks) removed, blank
/// lines dropped and line breaks kept, cut to `limit` characters with an ellipsis. Code lines and
/// their punctuation are kept as written.
pub fn excerpt(text: &str, limit: usize) -> String {
    let normal = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut kept: Vec<String> = Vec::new();
    let mut in_code = false;
    let mut total = 0usize;
    for raw in normal.split('\n') {
        let t = raw.trim();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_code = !in_code;
            continue;
        }
        if t.is_empty() {
            continue;
        }
        let line = if in_code {
            raw.trim_end().to_string()
        } else if separator_row(t) {
            continue;
        } else {
            clean_line(t)
        };
        if line.trim().is_empty() {
            continue;
        }
        total += line.chars().count() + 1;
        kept.push(line);
        if total > limit + 1 {
            break;
        }
    }
    let joined = kept.join("\n");
    if joined.chars().count() <= limit {
        return joined;
    }
    let head: String = joined.chars().take(limit).collect();
    format!("{}\u{2026}", head.trim_end())
}

fn clean_line(t: &str) -> String {
    let mut line = t;
    if let Some((_, rest)) = heading(line) {
        line = rest;
    }
    while let Some(rest) = line.strip_prefix('>') {
        line = rest.trim_start();
    }
    inline(line).iter().map(|run| run.text.as_str()).collect()
}
