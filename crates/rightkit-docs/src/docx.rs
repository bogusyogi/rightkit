//! DOCX: read (paragraphs with style names, tables, headers, footers), create from blocks, append
//! blocks, and replace text, all as byte-range edits of the original parts.
//!
//! Reading is a streaming pass over `word/document.xml` (no model is built, nothing is
//! re-serialised). Writing never rebuilds a document: an edit patches exact byte ranges of the
//! parts it touches and copies every other part raw, so comments, tracked changes, footnotes,
//! charts, custom XML and anything else this crate does not model survive byte for byte.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use quick_xml::events::Event;

use crate::detect::Ooxml;
use crate::error::{DocsError, Result};
use crate::model::{Block, DocxBlock, DocxContent, Loss};
use crate::package::{self, Package};
use crate::xmlutil::{
    attr, dir_of, esc_text, local, parse_rels, rels_name_for, resolve_target, Walk,
};

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

// ---------------------------------------------------------------------------------------------
// Read
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct Flags {
    tracked: bool,
    drawings: usize,
    footnote_refs: usize,
    comment_refs: usize,
}

#[derive(Default)]
struct Para {
    style_id: Option<String>,
    list: bool,
    text: String,
}

#[derive(Default)]
struct Table {
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    cell: Option<String>,
}

fn styles_map(bytes: &[u8]) -> Result<HashMap<String, String>> {
    let mut walk = Walk::new(bytes, "word/styles.xml");
    let mut map = HashMap::new();
    let mut cur: Option<String> = None;
    loop {
        let (ev, _, _) = walk.next()?;
        match ev {
            Event::Eof => break,
            Event::Start(e) if local(e.name().as_ref()) == b"style" => cur = attr(&e, "styleId"),
            Event::Start(e) | Event::Empty(e) if local(e.name().as_ref()) == b"name" => {
                if let (Some(id), Some(name)) = (&cur, attr(&e, "val")) {
                    map.insert(id.clone(), name);
                }
            }
            Event::End(e) if local(e.name().as_ref()) == b"style" => cur = None,
            _ => {}
        }
    }
    Ok(map)
}

fn resolve_ref(r: &quick_xml::events::BytesRef<'_>) -> Option<char> {
    let name: &str = r;
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        n if n.starts_with('#') => r.resolve_char_ref().ok().flatten(),
        _ => None,
    }
}

/// Stream one WordprocessingML part into blocks. `budget` is the character allowance left; the
/// pass stops (and reports truncation) once it is spent.
fn parse_blocks(
    xml: &[u8],
    part: &str,
    styles: &HashMap<String, String>,
    budget: &mut usize,
    flags: &mut Flags,
) -> Result<(Vec<DocxBlock>, bool)> {
    let mut walk = Walk::new(xml, part);
    let mut blocks: Vec<DocxBlock> = Vec::new();
    let mut paras: Vec<Para> = Vec::new();
    let mut tables: Vec<Table> = Vec::new();
    let mut in_t = false;
    let mut in_ppr = false;
    let mut depth = 0usize;
    let mut fallback_at: Option<usize> = None;
    let mut truncated = false;

    macro_rules! spend {
        ($n:expr) => {{
            let n: usize = $n;
            if n > *budget {
                *budget = 0;
                truncated = true;
            } else {
                *budget -= n;
            }
        }};
    }

    loop {
        let (ev, _, _) = walk.next()?;
        // Skip the legacy fallback copy of marked-up content (text boxes appear twice).
        if let Some(at) = fallback_at {
            match &ev {
                Event::Start(_) => depth += 1,
                Event::End(_) => {
                    if depth == at {
                        fallback_at = None;
                    }
                    depth -= 1;
                }
                Event::Eof => break,
                _ => {}
            }
            continue;
        }
        match ev {
            Event::Eof => break,
            Event::Start(e) => {
                depth += 1;
                match local(e.name().as_ref()) {
                    b"Fallback" => fallback_at = Some(depth),
                    b"p" => paras.push(Para::default()),
                    b"pPr" => in_ppr = true,
                    b"pStyle" => {
                        if let Some(p) = paras.last_mut() {
                            p.style_id = attr(&e, "val");
                        }
                    }
                    b"numPr" => {
                        if let Some(p) = paras.last_mut() {
                            p.list = true;
                        }
                    }
                    b"numId" => {
                        if attr(&e, "val").as_deref() == Some("0") {
                            if let Some(p) = paras.last_mut() {
                                p.list = false;
                            }
                        }
                    }
                    b"t" => in_t = true,
                    b"tbl" => tables.push(Table::default()),
                    b"tr" => {
                        if let Some(t) = tables.last_mut() {
                            t.row = Vec::new();
                        }
                    }
                    b"tc" => {
                        if let Some(t) = tables.last_mut() {
                            t.cell = Some(String::new());
                        }
                    }
                    b"ins" | b"del" | b"moveFrom" | b"moveTo" | b"rPrChange" | b"pPrChange"
                    | b"tblPrChange" | b"sectPrChange" => flags.tracked = true,
                    b"drawing" | b"pict" | b"object" => flags.drawings += 1,
                    b"tab" | b"br" | b"cr" | b"noBreakHyphen" => push_ctl(&mut paras, in_ppr, &e),
                    b"footnoteReference" | b"endnoteReference" => flags.footnote_refs += 1,
                    b"commentReference" => flags.comment_refs += 1,
                    _ => {}
                }
            }
            Event::Empty(e) => match local(e.name().as_ref()) {
                b"pStyle" => {
                    if let Some(p) = paras.last_mut() {
                        p.style_id = attr(&e, "val");
                    }
                }
                b"numId" => {
                    if attr(&e, "val").as_deref() == Some("0") {
                        if let Some(p) = paras.last_mut() {
                            p.list = false;
                        }
                    }
                }
                b"ins" | b"del" | b"moveFrom" | b"moveTo" | b"rPrChange" | b"pPrChange"
                | b"tblPrChange" | b"sectPrChange" => flags.tracked = true,
                b"drawing" | b"pict" | b"object" => flags.drawings += 1,
                b"tab" | b"br" | b"cr" | b"noBreakHyphen" => push_ctl(&mut paras, in_ppr, &e),
                b"footnoteReference" | b"endnoteReference" => flags.footnote_refs += 1,
                b"commentReference" => flags.comment_refs += 1,
                _ => {}
            },
            Event::Text(t) if in_t => {
                if let Some(p) = paras.last_mut() {
                    p.text.push_str(&t);
                }
            }
            Event::CData(t) if in_t => {
                if let Some(p) = paras.last_mut() {
                    p.text.push_str(&t);
                }
            }
            Event::GeneralRef(r) if in_t => {
                if let (Some(p), Some(c)) = (paras.last_mut(), resolve_ref(&r)) {
                    p.text.push(c);
                }
            }
            Event::End(e) => {
                depth = depth.saturating_sub(1);
                match local(e.name().as_ref()) {
                    b"t" => in_t = false,
                    b"pPr" => in_ppr = false,
                    b"p" => {
                        if let Some(p) = paras.pop() {
                            if !p.text.is_empty() {
                                spend!(p.text.chars().count());
                                if let Some(cell) = tables.last_mut().and_then(|t| t.cell.as_mut())
                                {
                                    if !cell.is_empty() {
                                        cell.push('\n');
                                    }
                                    cell.push_str(&p.text);
                                } else {
                                    let style = p.style_id.as_ref().map(|id| {
                                        styles.get(id).cloned().unwrap_or_else(|| id.clone())
                                    });
                                    blocks.push(DocxBlock::Paragraph {
                                        style,
                                        list: p.list,
                                        text: p.text,
                                    });
                                }
                            }
                        }
                    }
                    b"tc" => {
                        if let Some(t) = tables.last_mut() {
                            if let Some(cell) = t.cell.take() {
                                t.row.push(cell);
                            }
                        }
                    }
                    b"tr" => {
                        if let Some(t) = tables.last_mut() {
                            let row = std::mem::take(&mut t.row);
                            t.rows.push(row);
                        }
                    }
                    b"tbl" => {
                        if let Some(t) = tables.pop() {
                            if let Some(parent) = tables.last_mut().and_then(|p| p.cell.as_mut()) {
                                let flat = t
                                    .rows
                                    .iter()
                                    .map(|r| r.join("\t"))
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                if !parent.is_empty() && !flat.is_empty() {
                                    parent.push('\n');
                                }
                                parent.push_str(&flat);
                            } else if !t.rows.is_empty() {
                                blocks.push(DocxBlock::Table { rows: t.rows });
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        if truncated {
            break;
        }
    }
    Ok((blocks, truncated))
}

fn push_ctl(paras: &mut [Para], in_ppr: bool, e: &quick_xml::events::BytesStart<'_>) {
    if in_ppr {
        return; // `w:tabs/w:tab` in paragraph properties are stops, not content
    }
    if let Some(p) = paras.last_mut() {
        match local(e.name().as_ref()) {
            b"tab" => p.text.push('\t'),
            b"br" | b"cr" => p.text.push('\n'),
            b"noBreakHyphen" => p.text.push('-'),
            _ => {}
        }
    }
}

fn count_elems(pkg: &Package<'_>, part: &str, name: &[u8], skip_typed: bool) -> Result<usize> {
    let Some(bytes) = pkg.read_opt(part)? else {
        return Ok(0);
    };
    let mut walk = Walk::new(&bytes, part);
    let mut n = 0;
    loop {
        let (ev, _, _) = walk.next()?;
        match ev {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) if local(e.name().as_ref()) == name => {
                if skip_typed && attr(&e, "type").is_some() {
                    continue;
                }
                n += 1;
            }
            _ => {}
        }
    }
    Ok(n)
}

pub(crate) struct DocxRead {
    pub content: DocxContent,
    pub truncated: bool,
    pub omitted: Vec<String>,
}

pub(crate) fn read(o: &Ooxml<'_>, max_chars: usize) -> Result<DocxRead> {
    let pkg = &o.pkg;
    let rels_part = rels_name_for(&o.main);
    let rels = match pkg.read_opt(&rels_part)? {
        Some(b) => parse_rels(&b, &rels_part)?,
        None => Vec::new(),
    };
    let base = dir_of(&o.main).to_string();
    let styles_part = rels
        .iter()
        .find(|r| r.ty.ends_with("/styles") && !r.external)
        .map(|r| resolve_target(&base, &r.target))
        .unwrap_or_else(|| "word/styles.xml".to_string());
    let styles = match pkg.read_opt(&styles_part)? {
        Some(b) => styles_map(&b)?,
        None => HashMap::new(),
    };

    let mut flags = Flags::default();
    let mut budget = max_chars;
    let main_xml = pkg.read(&o.main)?;
    let (blocks, truncated) = parse_blocks(&main_xml, &o.main, &styles, &mut budget, &mut flags)?;

    let mut headers = Vec::new();
    let mut footers = Vec::new();
    for rel in rels.iter().filter(|r| !r.external) {
        let kind = if rel.ty.ends_with("/header") {
            Some(true)
        } else if rel.ty.ends_with("/footer") {
            Some(false)
        } else {
            None
        };
        let Some(is_header) = kind else { continue };
        let part = resolve_target(&base, &rel.target);
        let Some(xml) = pkg.read_opt(&part)? else {
            continue;
        };
        let mut hb = max_chars;
        let mut hf = Flags::default();
        let (hblocks, _) = parse_blocks(&xml, &part, &styles, &mut hb, &mut hf)?;
        let text = plain_blocks(&hblocks);
        if !text.trim().is_empty() {
            if is_header {
                headers.push(text);
            } else {
                footers.push(text);
            }
        }
    }

    let mut omitted = Vec::new();
    let comments = count_elems(pkg, "word/comments.xml", b"comment", false)?;
    if comments > 0 {
        omitted.push(format!("comments: {comments}"));
    }
    if flags.tracked {
        omitted.push("tracked changes: inserted text is included, deleted text is not".to_string());
    }
    let footnotes = count_elems(pkg, "word/footnotes.xml", b"footnote", true)?;
    if footnotes > 0 {
        omitted.push(format!("footnotes: {footnotes}"));
    }
    let endnotes = count_elems(pkg, "word/endnotes.xml", b"endnote", true)?;
    if endnotes > 0 {
        omitted.push(format!("endnotes: {endnotes}"));
    }
    let images = rels.iter().filter(|r| r.ty.ends_with("/image")).count();
    if images > 0 {
        omitted.push(format!("images: {images}"));
    }
    let charts = rels.iter().filter(|r| r.ty.ends_with("/chart")).count();
    if charts > 0 {
        omitted.push(format!("charts: {charts}"));
    }
    let external = rels.iter().filter(|r| r.external).count();
    if external > 0 {
        omitted.push(format!("external relationships not followed: {external}"));
    }
    if flags.drawings > 0 {
        omitted.push(format!("drawings and embedded objects: {}", flags.drawings));
    }
    if blocks
        .iter()
        .any(|b| matches!(b, DocxBlock::Paragraph { list: true, .. }))
    {
        omitted.push("list numbering: list items are flagged, numbers are not rendered".into());
    }
    Ok(DocxRead {
        content: DocxContent {
            blocks,
            headers,
            footers,
        },
        truncated,
        omitted,
    })
}

pub(crate) fn plain_blocks(blocks: &[DocxBlock]) -> String {
    let mut out: Vec<String> = Vec::new();
    for b in blocks {
        match b {
            DocxBlock::Paragraph { text, .. } => out.push(text.clone()),
            DocxBlock::Table { rows } => {
                for r in rows {
                    out.push(r.join("\t"));
                }
            }
        }
    }
    out.join("\n")
}

pub(crate) fn markdown_blocks(blocks: &[DocxBlock]) -> String {
    let mut out = String::new();
    let mut prev_list = false;
    for b in blocks {
        match b {
            DocxBlock::Paragraph { style, list, text } => {
                let lvl = style.as_deref().and_then(heading_level);
                let line = if let Some(n) = lvl {
                    format!("{} {}", "#".repeat(n as usize), text.replace('\n', " "))
                } else if *list {
                    format!("- {}", text.replace('\n', " "))
                } else {
                    text.clone()
                };
                if !out.is_empty() {
                    out.push_str(if *list && prev_list { "\n" } else { "\n\n" });
                }
                out.push_str(&line);
                prev_list = *list && lvl.is_none();
            }
            DocxBlock::Table { rows } => {
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
                for (i, r) in rows.iter().enumerate() {
                    let cells: Vec<String> = (0..cols)
                        .map(|c| {
                            r.get(c)
                                .map(|s| s.replace('|', "\\|").replace('\n', "<br>"))
                                .unwrap_or_default()
                        })
                        .collect();
                    out.push_str(&format!("| {} |\n", cells.join(" | ")));
                    if i == 0 {
                        out.push_str(&format!("| {} |\n", vec!["---"; cols].join(" | ")));
                    }
                }
                while out.ends_with('\n') {
                    out.pop();
                }
                prev_list = false;
            }
        }
    }
    out
}

fn heading_level(style: &str) -> Option<u8> {
    let s = style.trim().to_ascii_lowercase();
    if s == "title" {
        return Some(1);
    }
    let n = s.strip_prefix("heading")?.trim();
    n.parse::<u8>().ok().filter(|n| (1..=9).contains(n))
}

// ---------------------------------------------------------------------------------------------
// Create / append
// ---------------------------------------------------------------------------------------------

fn runs(text: &str, bold: bool) -> String {
    let rpr = if bold { "<w:rPr><w:b/></w:rPr>" } else { "" };
    let mut out = String::new();
    let mut buf = String::new();
    let flush = |buf: &mut String, out: &mut String| {
        if !buf.is_empty() {
            out.push_str(&format!(
                "<w:r>{rpr}<w:t xml:space=\"preserve\">{}</w:t></w:r>",
                esc_text(buf)
            ));
            buf.clear();
        }
    };
    for c in text.chars() {
        match c {
            '\n' => {
                flush(&mut buf, &mut out);
                out.push_str(&format!("<w:r>{rpr}<w:br/></w:r>"));
            }
            '\t' => {
                flush(&mut buf, &mut out);
                out.push_str(&format!("<w:r>{rpr}<w:tab/></w:r>"));
            }
            '\r' => {}
            c => buf.push(c),
        }
    }
    flush(&mut buf, &mut out);
    out
}

/// What the target document offers the blocks being written.
pub(crate) struct Ctx {
    /// Style ids the document defines; `None` means every style this crate writes is available
    /// (a document this crate just created).
    pub styles: Option<BTreeSet<String>>,
    pub numbering: bool,
}

impl Ctx {
    fn has_style(&self, id: &str) -> bool {
        self.styles.as_ref().map(|s| s.contains(id)).unwrap_or(true)
    }
}

/// Block XML plus the number of numbered-list runs (each run restarts at 1).
fn blocks_xml(blocks: &[Block], ctx: &Ctx, warnings: &mut Vec<String>) -> Result<(String, usize)> {
    let mut out = String::new();
    let mut num_runs = 0usize;
    let mut prev_numbered = false;
    let mut counter = 0usize;
    let mut warned_style: BTreeSet<String> = BTreeSet::new();
    let mut warned_list = false;
    for b in blocks {
        let is_numbered = matches!(b, Block::Numbered { .. });
        if is_numbered && !prev_numbered {
            num_runs += 1;
            counter = 0;
        }
        prev_numbered = is_numbered;
        match b {
            Block::Heading { level, text } => {
                if !(1..=6).contains(level) {
                    return Err(DocsError::InvalidOp(format!(
                        "heading level {level} (1 to 6)"
                    )));
                }
                let id = format!("Heading{level}");
                if ctx.has_style(&id) {
                    out.push_str(&format!(
                        "<w:p><w:pPr><w:pStyle w:val=\"{id}\"/></w:pPr>{}</w:p>",
                        runs(text, false)
                    ));
                } else {
                    if warned_style.insert(id.clone()) {
                        warnings.push(format!(
                            "style {id} is not defined in the document; headings use direct bold formatting"
                        ));
                    }
                    let half_points = match level {
                        1 => 32,
                        2 => 28,
                        3 => 26,
                        _ => 24,
                    };
                    let body = runs(text, true).replace(
                        "<w:rPr><w:b/></w:rPr>",
                        &format!("<w:rPr><w:b/><w:sz w:val=\"{half_points}\"/></w:rPr>"),
                    );
                    out.push_str(&format!("<w:p><w:pPr><w:keepNext/></w:pPr>{body}</w:p>"));
                }
            }
            Block::Paragraph { text } => {
                out.push_str(&format!("<w:p>{}</w:p>", runs(text, false)));
            }
            Block::Bullet { text } | Block::Numbered { text } => {
                counter += 1;
                if ctx.numbering {
                    let num_id = if is_numbered { 1 + num_runs } else { 1 };
                    out.push_str(&format!(
                        "<w:p><w:pPr><w:pStyle w:val=\"ListParagraph\"/><w:numPr><w:ilvl w:val=\"0\"/><w:numId w:val=\"{num_id}\"/></w:numPr></w:pPr>{}</w:p>",
                        runs(text, false)
                    ));
                } else {
                    if !warned_list {
                        warned_list = true;
                        warnings.push(
                            "the document has no numbering definitions; list items are written as paragraphs with a text marker"
                                .into(),
                        );
                    }
                    let marker = if is_numbered {
                        format!("{counter}. ")
                    } else {
                        "\u{2022} ".to_string()
                    };
                    out.push_str(&format!(
                        "<w:p>{}</w:p>",
                        runs(&format!("{marker}{text}"), false)
                    ));
                }
            }
            Block::Table { rows, header } => {
                if rows.is_empty() {
                    return Err(DocsError::InvalidOp("table with no rows".into()));
                }
                let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
                if cols == 0 {
                    return Err(DocsError::InvalidOp("table with no columns".into()));
                }
                let w = 9000 / cols;
                out.push_str("<w:tbl><w:tblPr><w:tblW w:w=\"0\" w:type=\"auto\"/><w:tblBorders>");
                for side in ["top", "left", "bottom", "right", "insideH", "insideV"] {
                    out.push_str(&format!(
                        "<w:{side} w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"808080\"/>"
                    ));
                }
                out.push_str("</w:tblBorders></w:tblPr><w:tblGrid>");
                for _ in 0..cols {
                    out.push_str(&format!("<w:gridCol w:w=\"{w}\"/>"));
                }
                out.push_str("</w:tblGrid>");
                for (i, row) in rows.iter().enumerate() {
                    out.push_str("<w:tr>");
                    if *header && i == 0 {
                        out.push_str("<w:trPr><w:tblHeader/></w:trPr>");
                    }
                    for c in 0..cols {
                        let text = row.get(c).map(String::as_str).unwrap_or("");
                        out.push_str(&format!(
                            "<w:tc><w:tcPr><w:tcW w:w=\"{w}\" w:type=\"dxa\"/></w:tcPr><w:p>{}</w:p></w:tc>",
                            runs(text, *header && i == 0)
                        ));
                    }
                    out.push_str("</w:tr>");
                }
                out.push_str("</w:tbl><w:p/>");
            }
            Block::PageBreak => {
                out.push_str("<w:p><w:r><w:br w:type=\"page\"/></w:r></w:p>");
            }
        }
    }
    Ok((out, num_runs))
}

const XML_DECL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n";

fn styles_xml() -> String {
    let mut s = String::from(XML_DECL);
    s.push_str(&format!("<w:styles xmlns:w=\"{W_NS}\">"));
    s.push_str("<w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii=\"Calibri\" w:hAnsi=\"Calibri\" w:eastAsia=\"Calibri\" w:cs=\"Calibri\"/><w:sz w:val=\"22\"/><w:szCs w:val=\"22\"/><w:lang w:val=\"en-US\"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after=\"160\" w:line=\"259\" w:lineRule=\"auto\"/></w:pPr></w:pPrDefault></w:docDefaults>");
    s.push_str("<w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/><w:qFormat/></w:style>");
    s.push_str("<w:style w:type=\"character\" w:default=\"1\" w:styleId=\"DefaultParagraphFont\"><w:name w:val=\"Default Paragraph Font\"/><w:uiPriority w:val=\"1\"/><w:semiHidden/></w:style>");
    s.push_str("<w:style w:type=\"table\" w:default=\"1\" w:styleId=\"TableNormal\"><w:name w:val=\"Normal Table\"/><w:uiPriority w:val=\"99\"/><w:semiHidden/><w:tblPr><w:tblInd w:w=\"0\" w:type=\"dxa\"/><w:tblCellMar><w:top w:w=\"0\" w:type=\"dxa\"/><w:left w:w=\"108\" w:type=\"dxa\"/><w:bottom w:w=\"0\" w:type=\"dxa\"/><w:right w:w=\"108\" w:type=\"dxa\"/></w:tblCellMar></w:tblPr></w:style>");
    s.push_str("<w:style w:type=\"paragraph\" w:styleId=\"Title\"><w:name w:val=\"Title\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/><w:pPr><w:spacing w:after=\"80\"/></w:pPr><w:rPr><w:sz w:val=\"56\"/><w:szCs w:val=\"56\"/></w:rPr></w:style>");
    let sizes = [32, 28, 26, 24, 22, 22];
    for (i, sz) in sizes.iter().enumerate() {
        let n = i + 1;
        s.push_str(&format!(
            "<w:style w:type=\"paragraph\" w:styleId=\"Heading{n}\"><w:name w:val=\"heading {n}\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/><w:pPr><w:keepNext/><w:keepLines/><w:spacing w:before=\"240\" w:after=\"80\"/><w:outlineLvl w:val=\"{i}\"/></w:pPr><w:rPr><w:b/><w:sz w:val=\"{sz}\"/><w:szCs w:val=\"{sz}\"/></w:rPr></w:style>"
        ));
    }
    s.push_str("<w:style w:type=\"paragraph\" w:styleId=\"ListParagraph\"><w:name w:val=\"List Paragraph\"/><w:basedOn w:val=\"Normal\"/><w:qFormat/><w:pPr><w:ind w:left=\"720\"/><w:contextualSpacing/></w:pPr></w:style>");
    s.push_str("<w:style w:type=\"table\" w:styleId=\"TableGrid\"><w:name w:val=\"Table Grid\"/><w:basedOn w:val=\"TableNormal\"/><w:uiPriority w:val=\"39\"/><w:tblPr><w:tblBorders><w:top w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/><w:left w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/><w:bottom w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/><w:right w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/><w:insideH w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/><w:insideV w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/></w:tblBorders></w:tblPr></w:style>");
    s.push_str("</w:styles>");
    s
}

fn numbering_xml(num_runs: usize) -> String {
    let mut s = String::from(XML_DECL);
    s.push_str(&format!("<w:numbering xmlns:w=\"{W_NS}\">"));
    s.push_str("<w:abstractNum w:abstractNumId=\"0\"><w:multiLevelType w:val=\"hybridMultilevel\"/><w:lvl w:ilvl=\"0\"><w:start w:val=\"1\"/><w:numFmt w:val=\"bullet\"/><w:lvlText w:val=\"\u{2022}\"/><w:lvlJc w:val=\"left\"/><w:pPr><w:ind w:left=\"720\" w:hanging=\"360\"/></w:pPr></w:lvl></w:abstractNum>");
    s.push_str("<w:abstractNum w:abstractNumId=\"1\"><w:multiLevelType w:val=\"hybridMultilevel\"/><w:lvl w:ilvl=\"0\"><w:start w:val=\"1\"/><w:numFmt w:val=\"decimal\"/><w:lvlText w:val=\"%1.\"/><w:lvlJc w:val=\"left\"/><w:pPr><w:ind w:left=\"720\" w:hanging=\"360\"/></w:pPr></w:lvl></w:abstractNum>");
    s.push_str("<w:num w:numId=\"1\"><w:abstractNumId w:val=\"0\"/></w:num>");
    for k in 0..num_runs {
        s.push_str(&format!(
            "<w:num w:numId=\"{}\"><w:abstractNumId w:val=\"1\"/><w:lvlOverride w:ilvl=\"0\"><w:startOverride w:val=\"1\"/></w:lvlOverride></w:num>",
            2 + k
        ));
    }
    s.push_str("</w:numbering>");
    s
}

/// Build a new DOCX from blocks.
pub(crate) fn create(blocks: &[Block]) -> Result<Vec<u8>> {
    let ctx = Ctx {
        styles: None,
        numbering: true,
    };
    let mut warnings = Vec::new();
    let (body, num_runs) = blocks_xml(blocks, &ctx, &mut warnings)?;
    let document = format!(
        "{XML_DECL}<w:document xmlns:w=\"{W_NS}\" xmlns:r=\"{R_NS}\"><w:body>{body}<w:sectPr><w:pgSz w:w=\"12240\" w:h=\"15840\"/><w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\" w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/></w:sectPr></w:body></w:document>"
    );
    let content_types = format!(
        "{XML_DECL}<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/><Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/><Override PartName=\"/word/numbering.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml\"/></Types>"
    );
    let root_rels = format!(
        "{XML_DECL}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"{R_NS}/officeDocument\" Target=\"word/document.xml\"/></Relationships>"
    );
    let doc_rels = format!(
        "{XML_DECL}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"{R_NS}/styles\" Target=\"styles.xml\"/><Relationship Id=\"rId2\" Type=\"{R_NS}/numbering\" Target=\"numbering.xml\"/></Relationships>"
    );
    package::build(&[
        ("[Content_Types].xml".into(), content_types.into_bytes()),
        ("_rels/.rels".into(), root_rels.into_bytes()),
        ("word/document.xml".into(), document.into_bytes()),
        ("word/_rels/document.xml.rels".into(), doc_rels.into_bytes()),
        ("word/styles.xml".into(), styles_xml().into_bytes()),
        (
            "word/numbering.xml".into(),
            numbering_xml(num_runs).into_bytes(),
        ),
    ])
}

pub(crate) struct EditOutcome {
    pub bytes: Vec<u8>,
    pub touched: Vec<String>,
    pub count: usize,
    pub warnings: Vec<String>,
    pub losses: Vec<Loss>,
}

/// Append blocks at the end of the body, before the final section properties.
pub(crate) fn append_blocks(o: &Ooxml<'_>, blocks: &[Block]) -> Result<EditOutcome> {
    let pkg = &o.pkg;
    let xml = pkg.read(&o.main)?;

    let rels_part = rels_name_for(&o.main);
    let base = dir_of(&o.main).to_string();
    let rels = match pkg.read_opt(&rels_part)? {
        Some(b) => parse_rels(&b, &rels_part)?,
        None => Vec::new(),
    };
    let styles_part = rels
        .iter()
        .find(|r| r.ty.ends_with("/styles") && !r.external)
        .map(|r| resolve_target(&base, &r.target))
        .unwrap_or_else(|| "word/styles.xml".to_string());
    let style_ids = match pkg.read_opt(&styles_part)? {
        Some(b) => Some(styles_map(&b)?.into_keys().collect::<BTreeSet<_>>()),
        None => Some(BTreeSet::new()),
    };
    let ctx = Ctx {
        styles: style_ids,
        // Existing numbering definitions are the document's own and this crate does not add to
        // them, so appended list items carry a text marker instead of a numbering reference.
        numbering: false,
    };

    let mut warnings = Vec::new();
    let (block_xml, _) = blocks_xml(blocks, &ctx, &mut warnings)?;

    // Find the insertion point: before the body's own sectPr, else before `</w:body>`.
    let mut walk = Walk::new(&xml, &o.main);
    let mut depth = 0usize;
    let mut body_depth: Option<usize> = None;
    let mut at: Option<usize> = None;
    let mut end_body: Option<usize> = None;
    loop {
        let (ev, start, _) = walk.next()?;
        match ev {
            Event::Eof => break,
            Event::Start(e) => {
                depth += 1;
                let name = e.name();
                if local(name.as_ref()) == b"body" && body_depth.is_none() {
                    if name.0 != "w:body" {
                        return Err(DocsError::InvalidOp(
                            "document body is not in the conventional w: namespace prefix".into(),
                        ));
                    }
                    body_depth = Some(depth);
                } else if local(name.as_ref()) == b"sectPr"
                    && Some(depth) == body_depth.map(|d| d + 1)
                {
                    at = Some(start);
                }
            }
            Event::Empty(e) => {
                if local(e.name().as_ref()) == b"sectPr" && Some(depth) == body_depth {
                    at = Some(start);
                }
            }
            Event::End(e) => {
                if local(e.name().as_ref()) == b"body" && Some(depth) == body_depth {
                    end_body = Some(start);
                }
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
    }
    let insert_at = at
        .or(end_body)
        .ok_or_else(|| DocsError::Malformed(format!("{}: no w:body element", o.main)))?;
    let mut out = Vec::with_capacity(xml.len() + block_xml.len());
    out.extend_from_slice(&xml[..insert_at]);
    out.extend_from_slice(block_xml.as_bytes());
    out.extend_from_slice(&xml[insert_at..]);

    let mut replace = BTreeMap::new();
    replace.insert(o.main.clone(), out);
    let bytes = pkg.rewrite(&replace, &BTreeSet::new(), &[])?;
    Ok(EditOutcome {
        bytes,
        touched: vec![o.main.clone()],
        count: blocks.len(),
        warnings,
        losses: Vec::new(),
    })
}

// ---------------------------------------------------------------------------------------------
// Replace text
// ---------------------------------------------------------------------------------------------

const BARRIER: char = '\u{FFFF}';

struct Node {
    /// `None` for a barrier (tab, break, drawing, field boundary...).
    tag: Option<(usize, usize)>,
    inner: (usize, usize),
    text: String,
}

fn is_barrier(name: &[u8]) -> bool {
    matches!(
        name,
        b"tab"
            | b"br"
            | b"cr"
            | b"ptab"
            | b"drawing"
            | b"pict"
            | b"object"
            | b"sym"
            | b"footnoteReference"
            | b"endnoteReference"
            | b"fldChar"
            | b"fldSimple"
    )
}

/// A node-local edit: byte range inside the node's text and the text that replaces it, if any.
type LocalEdit<'a> = (usize, usize, Option<&'a str>);

fn patch_paragraph(
    nodes: &[Node],
    xml: &[u8],
    find: &str,
    replace: &str,
    patches: &mut Vec<(usize, usize, Vec<u8>)>,
) -> usize {
    let mut s = String::new();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for n in nodes {
        let a = s.len();
        if n.tag.is_some() {
            s.push_str(&n.text);
        } else {
            s.push(BARRIER);
        }
        spans.push((a, s.len()));
    }
    let matches: Vec<usize> = s.match_indices(find).map(|(i, _)| i).collect();
    if matches.is_empty() {
        return 0;
    }
    // Per node: edits as (local_from, local_to, Option<replacement>).
    let mut edits: BTreeMap<usize, Vec<LocalEdit<'_>>> = BTreeMap::new();
    for &m in &matches {
        let me = m + find.len();
        let mut first = true;
        for (k, &(a, b)) in spans.iter().enumerate() {
            if nodes[k].tag.is_none() || b <= m || a >= me {
                continue;
            }
            let from = m.max(a) - a;
            let to = me.min(b) - a;
            let rep = if first { Some(replace) } else { None };
            first = false;
            edits.entry(k).or_default().push((from, to, rep));
        }
    }
    for (k, list) in edits {
        let node = &nodes[k];
        let mut text = String::new();
        let mut at = 0;
        for (from, to, rep) in list {
            text.push_str(&node.text[at..from]);
            if let Some(r) = rep {
                text.push_str(r);
            }
            at = to;
        }
        text.push_str(&node.text[at..]);
        if let Some((ts, te)) = node.tag {
            let tag = String::from_utf8_lossy(&xml[ts..te]).into_owned();
            if !tag.contains("xml:space=\"preserve\"") {
                let new_tag = if let Some(i) = tag.find("xml:space=\"") {
                    // Rewrite an existing xml:space value.
                    let rest = &tag[i + 11..];
                    let close = rest.find('"').unwrap_or(0);
                    format!("{}xml:space=\"preserve{}", &tag[..i], &rest[close..])
                } else {
                    let end = tag.rfind('>').unwrap_or(tag.len());
                    format!("{} xml:space=\"preserve\"{}", &tag[..end], &tag[end..])
                };
                patches.push((ts, te, new_tag.into_bytes()));
            }
        }
        patches.push((node.inner.0, node.inner.1, esc_text(&text).into_bytes()));
    }
    matches.len()
}

/// Replace `find` with `replace` across run boundaries in one WordprocessingML part. Returns the
/// patched bytes and the number of replacements, or `None` when `find` does not occur.
fn replace_in_part(
    xml: &[u8],
    part: &str,
    find: &str,
    replace: &str,
) -> Result<Option<(Vec<u8>, usize)>> {
    let mut walk = Walk::new(xml, part);
    let mut paras: Vec<Vec<Node>> = Vec::new();
    let mut open: Option<Node> = None;
    let mut patches: Vec<(usize, usize, Vec<u8>)> = Vec::new();
    let mut count = 0usize;
    loop {
        let (ev, start, end) = walk.next()?;
        match ev {
            Event::Eof => break,
            Event::Start(e) => {
                let name = e.name();
                let l = local(name.as_ref());
                if l == b"p" {
                    paras.push(Vec::new());
                } else if l == b"t" && !paras.is_empty() {
                    open = Some(Node {
                        tag: Some((start, end)),
                        inner: (end, end),
                        text: String::new(),
                    });
                } else if is_barrier(l) {
                    if let Some(p) = paras.last_mut() {
                        p.push(Node {
                            tag: None,
                            inner: (0, 0),
                            text: String::new(),
                        });
                    }
                }
            }
            Event::Empty(e) => {
                if is_barrier(local(e.name().as_ref())) {
                    if let Some(p) = paras.last_mut() {
                        p.push(Node {
                            tag: None,
                            inner: (0, 0),
                            text: String::new(),
                        });
                    }
                }
            }
            Event::Text(t) => {
                if let Some(n) = open.as_mut() {
                    n.text.push_str(&t);
                }
            }
            Event::CData(t) => {
                if let Some(n) = open.as_mut() {
                    n.text.push_str(&t);
                }
            }
            Event::GeneralRef(r) => {
                if let (Some(n), Some(c)) = (open.as_mut(), resolve_ref(&r)) {
                    n.text.push(c);
                }
            }
            Event::End(e) => {
                let name = e.name();
                let l = local(name.as_ref());
                if l == b"t" {
                    if let Some(mut n) = open.take() {
                        n.inner.1 = start;
                        if let Some(p) = paras.last_mut() {
                            p.push(n);
                        }
                    }
                } else if l == b"p" {
                    if let Some(nodes) = paras.pop() {
                        count += patch_paragraph(&nodes, xml, find, replace, &mut patches);
                    }
                }
            }
            _ => {}
        }
    }
    if count == 0 {
        return Ok(None);
    }
    patches.sort_by_key(|p| p.0);
    let mut out = Vec::with_capacity(xml.len());
    let mut at = 0;
    for (s, e, bytes) in patches {
        if s < at {
            return Err(DocsError::Malformed(format!(
                "{part}: overlapping text edits"
            )));
        }
        out.extend_from_slice(&xml[at..s]);
        out.extend_from_slice(&bytes);
        at = e;
    }
    out.extend_from_slice(&xml[at..]);
    Ok(Some((out, count)))
}

pub(crate) fn replace_text(o: &Ooxml<'_>, find: &str, replace: &str) -> Result<EditOutcome> {
    if find.is_empty() {
        return Err(DocsError::InvalidOp("empty find string".into()));
    }
    if find.contains(BARRIER) || replace.contains(BARRIER) {
        return Err(DocsError::InvalidOp(
            "text contains a reserved noncharacter".into(),
        ));
    }
    let replace = crate::xmlutil::clean(replace);
    let pkg = &o.pkg;
    let rels_part = rels_name_for(&o.main);
    let base = dir_of(&o.main).to_string();
    let rels = match pkg.read_opt(&rels_part)? {
        Some(b) => parse_rels(&b, &rels_part)?,
        None => Vec::new(),
    };
    let mut targets: Vec<String> = vec![o.main.clone()];
    for r in rels.iter().filter(|r| !r.external) {
        if r.ty.ends_with("/header") || r.ty.ends_with("/footer") {
            let p = resolve_target(&base, &r.target);
            if pkg.has(&p) && !targets.contains(&p) {
                targets.push(p);
            }
        }
    }
    let mut replacements = BTreeMap::new();
    let mut touched = Vec::new();
    let mut count = 0;
    for part in &targets {
        let xml = pkg.read(part)?;
        if let Some((out, n)) = replace_in_part(&xml, part, find, &replace)? {
            replacements.insert(part.clone(), out);
            touched.push(part.clone());
            count += n;
        }
    }
    if count == 0 {
        return Err(DocsError::NotFound {
            kind: "text",
            name: find.to_string(),
        });
    }
    let mut warnings = Vec::new();
    let mut tracked = false;
    // Occurrences this edit did not reach.
    for part in [
        "word/footnotes.xml",
        "word/endnotes.xml",
        "word/comments.xml",
    ] {
        if let Some(xml) = pkg.read_opt(part)? {
            let mut b = usize::MAX;
            let mut f = Flags::default();
            let (blocks, _) = parse_blocks(&xml, part, &HashMap::new(), &mut b, &mut f)?;
            let n = plain_blocks(&blocks).matches(find).count();
            if n > 0 {
                warnings.push(format!(
                    "{n} occurrence(s) of the text remain in {part}, which this edit does not change"
                ));
            }
        }
    }
    if let Some(xml) = pkg.read_opt(&o.main)? {
        let mut b = usize::MAX;
        let mut f = Flags::default();
        let _ = parse_blocks(&xml, &o.main, &HashMap::new(), &mut b, &mut f)?;
        tracked = f.tracked;
    }
    if tracked {
        warnings.push(
            "the document contains tracked changes; these replacements are not themselves tracked"
                .into(),
        );
    }
    let bytes = pkg.rewrite(&replacements, &BTreeSet::new(), &[])?;
    Ok(EditOutcome {
        bytes,
        touched,
        count,
        warnings,
        losses: Vec::new(),
    })
}
