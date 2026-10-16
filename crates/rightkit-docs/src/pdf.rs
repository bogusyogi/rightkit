//! PDF: a one-shot facade over `rightkit-pdf`. Text extraction is its `extract`, replace and add
//! text are its `compose`, page selection is its `merge`. This module owns selection, caps, the
//! encrypted refusal and the per-island loss report; it parses no PDF itself.

use std::panic::{catch_unwind, AssertUnwindSafe};

use lopdf::Document;
use rightkit_pdf::compose::compose_candidate;
use rightkit_pdf::font::FontCatalog;
use rightkit_pdf::inspect::walk_page_graph;
use rightkit_pdf::model::{
    document_sha256, PdfColor, PdfEditCommand, PdfEditError, PdfPoint, PdfRect, PdfTextAppearance,
    PdfTextEditState,
};

use crate::docx::EditOutcome;
use crate::error::{DocsError, Result};
use crate::limits::Limits;
use crate::model::{Loss, PdfContent, PdfPageText};

/// Stream decode bound while the document loads: same figure `rightkit-pdf` inspection uses.
const MAX_DECOMPRESSED_BYTES: usize = 64 * 1024 * 1024;

fn load(bytes: &[u8], limits: &Limits) -> Result<Document> {
    if bytes.len() as u64 > limits.max_input_bytes {
        return Err(DocsError::TooLarge {
            limit: limits.max_input_bytes,
            actual: bytes.len() as u64,
        });
    }
    let doc = catch_unwind(AssertUnwindSafe(|| {
        Document::load_mem_with_options(
            bytes,
            lopdf::LoadOptions::with_max_decompressed_size(MAX_DECOMPRESSED_BYTES),
        )
    }))
    .map_err(|_| DocsError::Malformed("the PDF parser panicked on this file".into()))?
    .map_err(|e| {
        let m = e.to_string();
        let l = m.to_ascii_lowercase();
        if l.contains("encrypt") || l.contains("decrypt") || l.contains("password") {
            DocsError::Encrypted
        } else {
            DocsError::Malformed(format!("pdf: {m}"))
        }
    })?;
    if doc.is_encrypted() {
        return Err(DocsError::Encrypted);
    }
    let pages = doc.get_pages().len();
    if pages > limits.max_pdf_pages {
        return Err(DocsError::ArchiveLimit(format!(
            "{pages} pages, limit {}",
            limits.max_pdf_pages
        )));
    }
    Ok(doc)
}

pub(crate) struct PdfRead {
    pub content: PdfContent,
    pub text: String,
    pub markdown: String,
    pub truncated: bool,
    pub omitted: Vec<String>,
}

pub(crate) fn read(
    bytes: &[u8],
    limits: &Limits,
    pages: &Option<Vec<u32>>,
    max_chars: usize,
) -> Result<PdfRead> {
    let doc = load(bytes, limits)?;
    let page_count = doc.get_pages().len() as u32;
    let wanted: Vec<u32> = match pages {
        None => (1..=page_count).collect(),
        Some(list) => {
            for p in list {
                if *p == 0 || *p > page_count {
                    return Err(DocsError::NotFound {
                        kind: "page",
                        name: p.to_string(),
                    });
                }
            }
            let mut sorted = list.clone();
            sorted.sort_unstable();
            sorted.dedup();
            sorted
        }
    };
    let by_page = rightkit_pdf::extract::extract_text_by_pages(bytes)
        .map_err(|e| DocsError::PdfExtract(e.to_string()))?;
    let mut out_pages = Vec::new();
    let mut text = String::new();
    let mut markdown = String::new();
    let mut truncated = false;
    let mut empty_pages = Vec::new();
    for p in wanted {
        let t = by_page.get(p as usize - 1).cloned().unwrap_or_default();
        if t.trim().is_empty() {
            empty_pages.push(p);
        }
        if !truncated {
            let have = text.chars().count();
            if have + t.chars().count() > max_chars {
                truncated = true;
            } else {
                if !text.is_empty() {
                    text.push_str("\n\n");
                    markdown.push_str("\n\n");
                }
                text.push_str(&t);
                markdown.push_str(&format!("## Page {p}\n\n{t}"));
                out_pages.push(PdfPageText { page: p, text: t });
                continue;
            }
        }
    }
    let mut omitted = vec!["images, vector graphics, annotations and form fields".to_string()];
    if !empty_pages.is_empty() {
        omitted.push(format!(
            "pages with no extractable text (scanned or image-only?): {}",
            empty_pages
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(PdfRead {
        content: PdfContent {
            page_count,
            pages: out_pages,
        },
        text,
        markdown,
        truncated,
        omitted,
    })
}

fn font_appearance(size_pt: f64) -> Result<PdfTextAppearance> {
    let entry = FontCatalog::shared()
        .list_entries()
        .into_iter()
        .find(|e| e.id == "source-serif-4")
        .ok_or(DocsError::Pdf(PdfEditError::UnsupportedEncoding))?;
    Ok(PdfTextAppearance {
        font_id: entry.id,
        family: entry.family,
        size_pt,
        color: PdfColor {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            alpha: 1.0,
        },
    })
}

pub(crate) fn replace_text(
    bytes: &[u8],
    limits: &Limits,
    find: &str,
    replace: &str,
) -> Result<EditOutcome> {
    if find.is_empty() {
        return Err(DocsError::InvalidOp("empty find string".into()));
    }
    let doc = load(bytes, limits)?;
    let sha = document_sha256(bytes);
    let mut commands = Vec::new();
    let mut count = 0usize;
    let mut losses = Vec::new();
    let mut blocked = 0usize;
    for page in 1..=doc.get_pages().len() as u32 {
        let graph = match walk_page_graph(&doc, &sha, page) {
            Ok(g) => g,
            Err(PdfEditError::EncryptedDocument) => return Err(DocsError::Encrypted),
            Err(e) => {
                losses.push(Loss {
                    part: format!("page {page}"),
                    kind: "page_not_searched".into(),
                    detail: e.to_string(),
                });
                continue;
            }
        };
        for island in graph.islands {
            let n = island.text.matches(find).count();
            if n == 0 {
                continue;
            }
            if !island.editable {
                blocked += n;
                losses.push(Loss {
                    part: format!("page {page}"),
                    kind: "not_replaced".into(),
                    detail: format!(
                        "{n} occurrence(s) in text the font cannot re-encode ({:?})",
                        island.edit_blocker
                    ),
                });
                continue;
            }
            let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
            for q in &island.quads {
                for i in 0..4 {
                    x0 = x0.min(q[i * 2]);
                    x1 = x1.max(q[i * 2]);
                    y0 = y0.min(q[i * 2 + 1]);
                    y1 = y1.max(q[i * 2 + 1]);
                }
            }
            if x0 > x1 {
                continue;
            }
            let page_w = (graph.media_box[2] - graph.media_box[0]).abs();
            let bounds = PdfRect {
                x: x0 - 1.0,
                y: y0 - 1.0,
                width: (x1 - x0).max(page_w - x0) + 2.0,
                height: (y1 - y0) + 2.0,
            };
            commands.push(PdfEditCommand::ReplaceSourceText {
                id: format!("r{}", commands.len()),
                locator: island.locator.clone(),
                before: PdfTextEditState {
                    text: island.text.clone(),
                    appearance: island.appearance.clone(),
                },
                after: PdfTextEditState {
                    text: island.text.replace(find, replace),
                    appearance: island.appearance.clone(),
                },
                available_bounds: bounds,
            });
            count += n;
        }
    }
    if commands.is_empty() {
        if blocked > 0 {
            return Err(DocsError::Pdf(PdfEditError::UnsupportedEncoding));
        }
        return Err(DocsError::NotFound {
            kind: "text",
            name: find.to_string(),
        });
    }
    let composed = compose_candidate(bytes, &commands, FontCatalog::shared())?;
    Ok(EditOutcome {
        bytes: composed.bytes,
        touched: composed
            .affected_pages
            .iter()
            .map(|p| format!("page {p}"))
            .collect(),
        count,
        warnings: Vec::new(),
        losses,
    })
}

pub(crate) fn add_text(
    bytes: &[u8],
    limits: &Limits,
    page: u32,
    x: f64,
    y: f64,
    text: &str,
    size_pt: Option<f64>,
) -> Result<EditOutcome> {
    let doc = load(bytes, limits)?;
    if page == 0 || page as usize > doc.get_pages().len() {
        return Err(DocsError::NotFound {
            kind: "page",
            name: page.to_string(),
        });
    }
    if !x.is_finite() || !y.is_finite() {
        return Err(DocsError::InvalidOp("non-finite position".into()));
    }
    let size = size_pt.unwrap_or(12.0);
    if !(1.0..=200.0).contains(&size) {
        return Err(DocsError::InvalidOp("size_pt must be 1 to 200".into()));
    }
    let text = crate::xmlutil::clean(text);
    if text.trim().is_empty() {
        return Err(DocsError::InvalidOp("empty text".into()));
    }
    let cmd = PdfEditCommand::AddText {
        id: "a0".into(),
        page,
        anchor: PdfPoint { x, y },
        text,
        appearance: font_appearance(size)?,
    };
    let composed = compose_candidate(bytes, std::slice::from_ref(&cmd), FontCatalog::shared())?;
    Ok(EditOutcome {
        bytes: composed.bytes,
        touched: vec![format!("page {page}")],
        count: 1,
        warnings: Vec::new(),
        losses: Vec::new(),
    })
}

pub(crate) fn select_pages(bytes: &[u8], limits: &Limits, pages: &[u32]) -> Result<EditOutcome> {
    let doc = load(bytes, limits)?;
    let n = doc.get_pages().len();
    for p in pages {
        if *p == 0 || *p as usize > n {
            return Err(DocsError::NotFound {
                kind: "page",
                name: p.to_string(),
            });
        }
    }
    let out = rightkit_pdf::merge::select_pages(bytes, pages)
        .map_err(|e| DocsError::InvalidOp(e.to_string()))?;
    let dropped = n.saturating_sub(
        pages
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
    );
    let mut losses = Vec::new();
    if dropped > 0 {
        losses.push(Loss {
            part: "pages".into(),
            kind: "pages_dropped".into(),
            detail: format!("{dropped} page(s) not selected were removed, with their annotations"),
        });
    }
    Ok(EditOutcome {
        bytes: out,
        touched: vec!["page tree".into()],
        count: pages.len(),
        warnings: vec![
            "document-level outlines, named destinations and form fields are not carried over by page selection"
                .into(),
        ],
        losses,
    })
}
