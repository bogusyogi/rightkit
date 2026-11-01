//! rightkit-pdf: compose a real text PDF (an embedded, subset Type0 font drawn page by page), inspect it back (page
//! count, extracted text, rotation), then run the structural operations (select, delete, rotate, merge) and commit to
//! disk. The font is the crate's own bundled Source Serif 4, resolved through `FontCatalog` (no host font needed).
use rightkit_pdf::font::FontCatalog;
use rightkit_pdf::inspect::page_graph_from_bytes;
use rightkit_pdf::merge::{merge_pdfs, select_pages};
use rightkit_pdf::model::PdfFontResolver;
use rightkit_pdf::organize::{delete_pages, rotate_pages, verify_structure};
use rightkit_pdf::write::{Document, WriteError};
use serde_json::{json, Value};

use super::Ctx;

/// A PDF with one US-Letter page per entry of `pages`, each drawn with the bundled Source Serif 4 at 18 pt.
/// Shared with the `docs` probe, which reads, edits and re-reads the same file.
pub(super) fn sample_pdf(pages: &[&str]) -> Result<Vec<u8>, String> {
    let font = FontCatalog::shared()
        .resolve("source-serif-4")
        .map_err(|e| format!("needs the bundled Source Serif 4 (rightkit-pdf assets/SourceSerif4-Variable.woff2, resolved by FontCatalog as `source-serif-4`): {e:?}"))?;
    let mut doc = Document::new("1.7").map_err(|e| format!("new document: {e}"))?;
    let font_id = doc.add_font(font.bytes, font.face_index).map_err(|e| format!("add font: {e}"))?;
    for text in pages {
        let page = doc.add_page(612.0, 792.0).map_err(|e| format!("add page: {e}"))?;
        doc.draw_text(page, font_id, 18.0, 72.0, 700.0, None, text).map_err(|e| format!("draw {text:?}: {e}"))?;
    }
    doc.try_finish().map_err(|e| format!("finish: {e}"))
}

fn page_text(bytes: &[u8], page: u32) -> Result<String, String> {
    let graph = page_graph_from_bytes(bytes, page).map_err(|e| format!("inspect page {page}: {e:?}"))?;
    Ok(graph.islands.iter().map(|i| i.text.as_str()).collect::<Vec<_>>().join(" "))
}

fn squash(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

fn expect_text(bytes: &[u8], page: u32, want: &str) -> Result<(), String> {
    let got = page_text(bytes, page)?;
    if squash(&got).contains(&squash(want)) {
        Ok(())
    } else {
        Err(format!("page {page} text {got:?} does not contain {want:?}"))
    }
}

pub fn probe(ctx: &Ctx) -> Result<Value, String> {
    let dir = ctx.dir("pdf")?;
    let first = "RightKit E2E page one";
    let second = "RightKit E2E page two";
    let pdf = sample_pdf(&[first, second])?;
    if !pdf.starts_with(b"%PDF-1.7") {
        return Err("the composed bytes are not a PDF 1.7".into());
    }
    if sample_pdf(&[first, second])? != pdf {
        return Err("composition is not deterministic: the same calls gave different bytes".into());
    }

    // Inspect: structure, page count, per-page text.
    verify_structure(&pdf, 2).map_err(|e| format!("two-page structure: {e:?}"))?;
    if verify_structure(&pdf, 3).is_ok() {
        return Err("structure verification accepted the wrong page count".into());
    }
    expect_text(&pdf, 1, first)?;
    expect_text(&pdf, 2, second)?;
    if squash(&page_text(&pdf, 1)?).contains("pagetwo") {
        return Err("page 1 carries page 2's text".into());
    }
    let graph = page_graph_from_bytes(&pdf, 1).map_err(|e| format!("inspect: {e:?}"))?;
    if graph.media_box != [0.0, 0.0, 612.0, 792.0] || graph.rotation != 0 {
        return Err(format!("unexpected page geometry {:?} rotation {}", graph.media_box, graph.rotation));
    }

    // Structural operations, each read back.
    let only_second = select_pages(&pdf, &[2]).map_err(|e| format!("select: {e:?}"))?;
    verify_structure(&only_second, 1).map_err(|e| format!("select structure: {e:?}"))?;
    expect_text(&only_second, 1, second)?;
    let without_first = delete_pages(&pdf, &[1]).map_err(|e| format!("delete: {e:?}"))?;
    verify_structure(&without_first, 1).map_err(|e| format!("delete structure: {e:?}"))?;
    expect_text(&without_first, 1, second)?;
    let turned = rotate_pages(&pdf, &[1], 90).map_err(|e| format!("rotate: {e:?}"))?;
    let turned_graph = page_graph_from_bytes(&turned, 1).map_err(|e| format!("inspect rotated: {e:?}"))?;
    if turned_graph.rotation != 90 {
        return Err(format!("rotation is {} after a 90 degree turn", turned_graph.rotation));
    }
    let merged = merge_pdfs(&[&pdf, &only_second]).map_err(|e| format!("merge: {e:?}"))?;
    verify_structure(&merged, 3).map_err(|e| format!("merge structure: {e:?}"))?;
    expect_text(&merged, 3, second)?;

    // Refusals: a glyph the font lacks is never substituted; garbage is not a PDF.
    let font = FontCatalog::shared().resolve("source-serif-4").map_err(|e| format!("{e:?}"))?;
    let mut doc = Document::new("1.7").map_err(|e| e.to_string())?;
    let font_id = doc.add_font(font.bytes, font.face_index).map_err(|e| e.to_string())?;
    let page = doc.add_page(612.0, 792.0).map_err(|e| e.to_string())?;
    match doc.draw_text(page, font_id, 18.0, 72.0, 700.0, None, "\u{6f22}") {
        Err(WriteError::MissingGlyph('\u{6f22}')) => {}
        other => return Err(format!("a CJK glyph the font lacks: expected MissingGlyph, got {other:?}")),
    }
    if verify_structure(b"this is not a pdf", 1).is_ok() || page_graph_from_bytes(b"this is not a pdf", 1).is_ok() {
        return Err("garbage bytes were accepted as a PDF".into());
    }

    // Commit to disk through the crate's atomic writer and read the file back.
    let path = dir.join("composed.pdf");
    let mut on_disk = Document::new("1.7").map_err(|e| e.to_string())?;
    let font = FontCatalog::shared().resolve("source-serif-4").map_err(|e| format!("{e:?}"))?;
    let fid = on_disk.add_font(font.bytes, font.face_index).map_err(|e| e.to_string())?;
    let pid = on_disk.add_page(612.0, 792.0).map_err(|e| e.to_string())?;
    on_disk.draw_text(pid, fid, 18.0, 72.0, 700.0, None, first).map_err(|e| e.to_string())?;
    on_disk.write_to(&path).map_err(|e| format!("write_to {}: {e}", path.display()))?;
    let read_back = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    if read_back != on_disk.try_finish().map_err(|e| e.to_string())? {
        return Err("the file on disk differs from the composed bytes".into());
    }
    expect_text(&read_back, 1, first)?;

    Ok(json!({
        "bytes": pdf.len(),
        "pages": 2,
        "selected": 1,
        "merged": 3,
        "rotation": turned_graph.rotation,
        "committed": path.file_name().and_then(|n| n.to_str()),
        "font": "source-serif-4 (bundled)",
    }))
}
