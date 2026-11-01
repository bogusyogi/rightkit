//! rightkit-docs: write a DOCX, an XLSX and a PDF, read each back, edit it under a source-hash guard, refuse a stale
//! edit and hostile input, and prove the format is detected from content (every file has a lying extension).
use rightkit_docs::{
    read, write, Block, CellInput, CellSet, CellValue, Content, DocsError, DocxBlock, Format, Op, ReadOptions, SheetSpec,
    Source, WriteOptions,
};
use serde_json::{json, Value};

use super::Ctx;

fn text(s: &str) -> CellInput {
    CellInput::Text(s.into())
}

fn code(e: DocsError) -> String {
    format!("{e} ({})", e.code())
}

fn squash(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

fn read_path(path: &std::path::Path) -> Result<rightkit_docs::DocumentRead, String> {
    read(Source::Path(path), &ReadOptions::default()).map_err(|e| format!("read {}: {}", path.display(), code(e)))
}

fn docx_probe(dir: &std::path::Path) -> Result<Value, String> {
    // The extension lies on purpose: the format comes from the bytes.
    let path = dir.join("report-but-named.xlsx");
    let created = write(
        &path,
        &[Op::CreateDocx {
            blocks: vec![
                Block::Heading { level: 1, text: "Q3 Report".into() },
                Block::Paragraph { text: "Revenue grew & margins held <steady>.".into() },
                Block::Bullet { text: "first point".into() },
                Block::Table {
                    rows: vec![vec!["Region".into(), "Sales".into()], vec!["EMEA".into(), "120".into()]],
                    header: true,
                },
            ],
        }],
        &WriteOptions::default(),
    )
    .map_err(|e| format!("create docx: {}", code(e)))?;
    if !created.created || created.format != Format::Docx {
        return Err(format!("create receipt says created={} format={:?}", created.created, created.format));
    }
    let doc = read_path(&path)?;
    if doc.format != Format::Docx {
        return Err(format!("a DOCX named .xlsx was detected as {:?}", doc.format));
    }
    if doc.sha256 != created.sha256 {
        return Err("the read hash differs from the receipt hash".into());
    }
    let Content::Docx(content) = &doc.content else { return Err("DOCX read returned another content kind".into()) };
    let has_heading = content.blocks.iter().any(|b| {
        matches!(b, DocxBlock::Paragraph { style: Some(s), text, .. } if s.to_lowercase().contains("heading") && text == "Q3 Report")
    });
    let has_text = content
        .blocks
        .iter()
        .any(|b| matches!(b, DocxBlock::Paragraph { text, .. } if text == "Revenue grew & margins held <steady>."));
    let has_table = content.blocks.iter().any(|b| {
        matches!(b, DocxBlock::Table { rows } if rows == &vec![vec!["Region".to_string(), "Sales".to_string()], vec!["EMEA".to_string(), "120".to_string()]])
    });
    if !(has_heading && has_text && has_table) {
        return Err(format!("DOCX content lost something (heading {has_heading}, text {has_text}, table {has_table})"));
    }

    // Edit against exactly what was read.
    let edited = write(
        &path,
        &[Op::ReplaceText { find: "Q3".into(), replace: "Q4".into() }],
        &WriteOptions { expect_sha256: Some(doc.sha256.clone()), ..Default::default() },
    )
    .map_err(|e| format!("replace text: {}", code(e)))?;
    if edited.replacements != 1 || edited.source_sha256.as_deref() != Some(doc.sha256.as_str()) {
        return Err(format!("replace receipt: {} replacements, source {:?}", edited.replacements, edited.source_sha256));
    }
    let after = read_path(&path)?;
    if !after.text.contains("Q4 Report") || after.text.contains("Q3") {
        return Err(format!("the edit did not land: {:?}", after.text));
    }
    // The old hash is now stale: the edit is refused and the file keeps the newer bytes.
    match write(
        &path,
        &[Op::ReplaceText { find: "Q4".into(), replace: "Q5".into() }],
        &WriteOptions { expect_sha256: Some(doc.sha256.clone()), ..Default::default() },
    ) {
        Err(DocsError::SourceChanged) => {}
        other => return Err(format!("a stale edit: expected source_changed, got {other:?}")),
    }
    if read_path(&path)?.sha256 != after.sha256 {
        return Err("a refused edit still changed the file".into());
    }
    write(&path, &[Op::AppendBlocks { blocks: vec![Block::Paragraph { text: "Appended by the probe.".into() }] }], &WriteOptions::default())
        .map_err(|e| format!("append: {}", code(e)))?;
    if !read_path(&path)?.text.contains("Appended by the probe.") {
        return Err("appended paragraph is missing".into());
    }
    // A create never clobbers an existing file.
    match write(&path, &[Op::CreateDocx { blocks: vec![Block::Paragraph { text: "x".into() }] }], &WriteOptions::default()) {
        Err(DocsError::TargetExists) => {}
        other => return Err(format!("create over an existing file: expected target_exists, got {other:?}")),
    }
    Ok(json!({ "bytes": after.size_bytes, "replacements": edited.replacements, "touchedParts": edited.touched_parts }))
}

fn xlsx_probe(dir: &std::path::Path) -> Result<Value, String> {
    let path = dir.join("stock-but-named.docx");
    write(
        &path,
        &[Op::CreateXlsx {
            sheets: vec![SheetSpec {
                name: "Stock".into(),
                rows: vec![
                    vec![text("item"), text("qty"), text("price")],
                    vec![text("apple"), CellInput::Number(3.0), CellInput::Number(0.5)],
                    vec![text("pear"), CellInput::Number(4.0), CellInput::Number(0.25)],
                    vec![text("total"), CellInput::Formula("SUM(B2:B3)".into()), CellInput::Blank],
                ],
            }],
        }],
        &WriteOptions::default(),
    )
    .map_err(|e| format!("create xlsx: {}", code(e)))?;
    let doc = read_path(&path)?;
    if doc.format != Format::Xlsx {
        return Err(format!("an XLSX named .docx was detected as {:?}", doc.format));
    }
    let Content::Xlsx(content) = &doc.content else { return Err("XLSX read returned another content kind".into()) };
    let sheet = content.sheets.iter().find(|s| s.name == "Stock").ok_or("sheet Stock is missing")?;
    let cell = |a1: &str| sheet.cells.iter().find(|c| c.cell == a1).ok_or(format!("cell {a1} is missing"));
    if cell("B2")?.value != CellValue::Number(3.0) || cell("C3")?.value != CellValue::Number(0.25) {
        return Err("numeric cells did not round trip".into());
    }
    if cell("A4")?.value != CellValue::Text("total".into()) {
        return Err("text cell did not round trip".into());
    }
    if cell("B4")?.formula.as_deref() != Some("SUM(B2:B3)") {
        return Err(format!("formula lost: {:?}", cell("B4")?.formula));
    }
    if sheet.used_range.as_deref() != Some("A1:C4") {
        return Err(format!("used range {:?}", sheet.used_range));
    }

    let edited = write(
        &path,
        &[
            Op::SetCells { sheet: "Stock".into(), cells: vec![CellSet { cell: "B2".into(), value: CellInput::Number(10.0) }] },
            Op::AppendRows { sheet: "Stock".into(), rows: vec![vec![text("fig"), CellInput::Number(7.0), CellInput::Number(1.5)]] },
        ],
        &WriteOptions { expect_sha256: Some(doc.sha256.clone()), ..Default::default() },
    )
    .map_err(|e| format!("edit xlsx: {}", code(e)))?;
    if edited.cells_set != 1 || edited.rows_appended != 1 {
        return Err(format!("edit receipt: {} cells, {} rows", edited.cells_set, edited.rows_appended));
    }
    let again = read_path(&path)?;
    let Content::Xlsx(content) = &again.content else { return Err("re-read returned another content kind".into()) };
    let sheet = content.sheets.iter().find(|s| s.name == "Stock").ok_or("sheet Stock vanished")?;
    let cell = |a1: &str| sheet.cells.iter().find(|c| c.cell == a1).ok_or(format!("cell {a1} is missing after the edit"));
    if cell("B2")?.value != CellValue::Number(10.0) || cell("A5")?.value != CellValue::Text("fig".into()) {
        return Err("the cell edit or the appended row did not land".into());
    }
    if cell("B4")?.formula.as_deref() != Some("SUM(B2:B3)") {
        return Err("an untouched formula was lost by the edit".into());
    }
    // A formula that fetches from the network is refused, and the file is unchanged.
    match write(
        &path,
        &[Op::SetCells {
            sheet: "Stock".into(),
            cells: vec![CellSet { cell: "D1".into(), value: CellInput::Formula("WEBSERVICE(\"http://127.0.0.1/\")".into()) }],
        }],
        &WriteOptions::default(),
    ) {
        Err(DocsError::UnsafeFormula(_)) => {}
        other => return Err(format!("a network formula: expected unsafe_formula, got {other:?}")),
    }
    if read_path(&path)?.sha256 != again.sha256 {
        return Err("a refused formula still changed the file".into());
    }
    Ok(json!({ "cellsSet": edited.cells_set, "rowsAppended": edited.rows_appended, "usedRange": "A1:C4" }))
}

fn pdf_probe(dir: &std::path::Path) -> Result<Value, String> {
    let path = dir.join("memo-but-named.docx");
    std::fs::write(&path, super::pdf::sample_pdf(&["Probe alpha beta gamma", "Second page delta"])?)
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    let doc = read_path(&path)?;
    if doc.format != Format::Pdf {
        return Err(format!("a PDF named .docx was detected as {:?}", doc.format));
    }
    let Content::Pdf(content) = &doc.content else { return Err("PDF read returned another content kind".into()) };
    if content.page_count != 2 || !squash(&doc.text).contains("Probealphabetagamma") || !squash(&doc.text).contains("Secondpagedelta") {
        return Err(format!("PDF read: {} pages, text {:?}", content.page_count, doc.text));
    }
    // Replace text through the island's own font (only glyphs the subset already holds), then add a line, then keep page 2.
    let replaced = write(
        &path,
        &[Op::ReplaceText { find: "beta".into(), replace: "mama".into() }],
        &WriteOptions { expect_sha256: Some(doc.sha256.clone()), ..Default::default() },
    )
    .map_err(|e| format!("pdf replace: {}", code(e)))?;
    if replaced.replacements != 1 {
        return Err(format!("pdf replace made {} replacements", replaced.replacements));
    }
    if !squash(&read_path(&path)?.text).contains("alphamamagamma") {
        return Err("the PDF text replacement did not land".into());
    }
    write(
        &path,
        &[Op::AddPdfText { page: 2, x: 72.0, y: 600.0, text: "Added by the e2e probe".into(), size_pt: Some(14.0) }],
        &WriteOptions { expect_sha256: Some(replaced.sha256.clone()), ..Default::default() },
    )
    .map_err(|e| format!("pdf add text: {}", code(e)))?;
    let kept = write(&path, &[Op::SelectPdfPages { pages: vec![2] }], &WriteOptions::default())
        .map_err(|e| format!("pdf select: {}", code(e)))?;
    let after = read_path(&path)?;
    let Content::Pdf(content) = &after.content else { return Err("PDF re-read returned another content kind".into()) };
    let t = squash(&after.text);
    if content.page_count != 1 || !t.contains("Secondpagedelta") || !t.contains("Addedbythee2eprobe") || t.contains("alphamama") {
        return Err(format!("PDF after select: {} pages, text {:?}", content.page_count, after.text));
    }
    Ok(json!({ "pages": content.page_count, "replacements": replaced.replacements, "bytes": kept.bytes_written }))
}

pub fn probe(ctx: &Ctx) -> Result<Value, String> {
    let dir = ctx.dir("docs")?;
    let docx = docx_probe(&dir)?;
    let xlsx = xlsx_probe(&dir)?;
    let pdf = pdf_probe(&dir)?;

    // Hostile and unsupported input is a typed refusal, never a guess.
    match read(Source::Bytes(b"plain text, not a document"), &ReadOptions::default()) {
        Err(DocsError::UnsupportedFormat(_)) => {}
        other => return Err(format!("plain text: expected unsupported_format, got {other:?}")),
    }
    match read(Source::Bytes(b""), &ReadOptions::default()) {
        Err(DocsError::UnsupportedFormat(_)) => {}
        other => return Err(format!("empty input: expected unsupported_format, got {other:?}")),
    }
    match read(Source::Bytes(b"PK\x03\x04 truncated zip"), &ReadOptions::default()) {
        Err(_) => {}
        Ok(_) => return Err("a truncated zip was read as a document".into()),
    }
    match write(&dir.join("missing.docx"), &[Op::ReplaceText { find: "a".into(), replace: "b".into() }], &WriteOptions::default()) {
        Err(DocsError::TargetMissing) => {}
        other => return Err(format!("edit of a missing file: expected target_missing, got {other:?}")),
    }
    Ok(json!({ "docx": docx, "xlsx": xlsx, "pdf": pdf, "formatFromContent": ["docx", "xlsx", "pdf"] }))
}
