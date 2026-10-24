#![cfg(feature = "edit")]

//! Real-world PDF corpus QA harness (test-only, `#[ignore]` — run explicitly):
//!
//! ```sh
//! rustright cargo test corpus_qa_matrix -- --ignored --nocapture
//! ```
//!
//! Iterates every `*.pdf` under `RK_PDF_CORPUS_DIR` (default `../../../.qa-corpus`, i.e. the repo root) and exercises the real
//! backend pipeline end to end per file: lopdf load → `walk_page_graph` on up to the first 12
//! pages → one `ReplaceSourceText` compose on the first editable island → one `AddText` compose
//! through the discovered `FontCatalog` → reopen each candidate and re-walk the affected page.
//! Panics are caught per file so one wild PDF cannot hide results for the rest; the test fails if
//! any file panics or a candidate fails to reopen. Typed rejections (encrypted, unsupported
//! transform, …) are recorded as acceptable degradation, not failures — matching §3.2's
//! "unsupported content remains viewable & unchanged".

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;

use lopdf::Document;

use rightkit_pdf::compose::compose_candidate;
use rightkit_pdf::font::FontCatalog;
use rightkit_pdf::inspect::walk_page_graph;
use rightkit_pdf::model::{
    document_sha256, PdfEditCommand, PdfPoint, PdfRect, PdfTextEditState, PdfTextIsland,
};

const MAX_PAGES_WALKED: u32 = 12;

#[derive(Default)]
struct FileReport {
    name: String,
    pages: u32,
    pages_walked: u32,
    islands: usize,
    editable: usize,
    walk_errors: Vec<String>,
    replace: String,
    add_text: String,
    panicked: bool,
    fatal: Option<String>,
}

fn quads_union(island: &PdfTextIsland) -> Option<PdfRect> {
    let mut min_x = f64::MAX;
    let mut min_y = f64::MAX;
    let mut max_x = f64::MIN;
    let mut max_y = f64::MIN;
    for q in &island.quads {
        for i in 0..4 {
            min_x = min_x.min(q[i * 2]);
            max_x = max_x.max(q[i * 2]);
            min_y = min_y.min(q[i * 2 + 1]);
            max_y = max_y.max(q[i * 2 + 1]);
        }
    }
    if min_x > max_x {
        return None;
    }
    Some(PdfRect {
        x: min_x - 1.0,
        y: min_y - 1.0,
        width: (max_x - min_x) + 2.0,
        height: (max_y - min_y) + 2.0,
    })
}

fn run_one(path: &PathBuf, catalog: &FontCatalog) -> FileReport {
    let mut report = FileReport {
        name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        replace: "not-attempted".into(),
        add_text: "not-attempted".into(),
        ..FileReport::default()
    };

    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            report.fatal = Some(format!("read failed: {e}"));
            return report;
        }
    };
    let doc = match Document::load_mem(&bytes) {
        Ok(d) => d,
        Err(e) => {
            // Not a panic and not silent corruption — a typed load rejection is acceptable.
            report.fatal = Some(format!("lopdf load rejected: {e}"));
            return report;
        }
    };
    let sha = document_sha256(&bytes);
    let pages = doc.get_pages();
    report.pages = pages.len() as u32;

    let mut replace_candidate: Option<(u32, PdfTextIsland)> = None;

    for page in 1..=report.pages.min(MAX_PAGES_WALKED) {
        report.pages_walked = page;
        match walk_page_graph(&doc, &sha, page) {
            Ok(graph) => {
                report.islands += graph.islands.len();
                report.editable += graph.islands.iter().filter(|i| i.editable).count();
                if replace_candidate.is_none() {
                    if let Some(pos) = graph
                        .islands
                        .iter()
                        .position(|i| i.editable && i.text.trim().len() >= 4)
                    {
                        let mut islands = graph.islands;
                        replace_candidate = Some((page, islands.swap_remove(pos)));
                    }
                }
            }
            Err(e) => report.walk_errors.push(format!("p{page}: {e}")),
        }
    }

    // Replace-source-text round trip on the first editable island: shorten by one char so every
    // replacement glyph is guaranteed available in the original font.
    if let Some((page, island)) = replace_candidate {
        let mut after_text = island.text.clone();
        after_text.pop();
        let bounds = quads_union(&island);
        report.replace = match bounds {
            None => "skip: island has no quads".into(),
            Some(available_bounds) => {
                let command = PdfEditCommand::ReplaceSourceText {
                    id: "qa-replace".into(),
                    locator: island.locator.clone(),
                    before: PdfTextEditState {
                        text: island.text.clone(),
                        appearance: island.appearance.clone(),
                    },
                    after: PdfTextEditState {
                        text: after_text.clone(),
                        appearance: island.appearance.clone(),
                    },
                    available_bounds,
                };
                match compose_candidate(&bytes, &[command], catalog) {
                    Err(e) => format!("typed reject: {e}"),
                    Ok(candidate) => match Document::load_mem(&candidate.bytes) {
                        Err(e) => format!("FATAL: candidate did not reopen: {e}"),
                        Ok(reopened) => {
                            let new_sha = document_sha256(&candidate.bytes);
                            match walk_page_graph(&reopened, &new_sha, page) {
                                Err(e) => format!("FATAL: candidate page rewalk failed: {e}"),
                                Ok(graph) => {
                                    let found = graph
                                        .islands
                                        .iter()
                                        .any(|i| i.text.contains(after_text.trim()));
                                    if found {
                                        "ok".into()
                                    } else {
                                        "FATAL: replacement text missing after reopen".into()
                                    }
                                }
                            }
                        }
                    },
                }
            }
        };
    }

    // Add-text through the real discovered font catalog (exercises subset/embed on this host).
    // Skip private/system dot-prefixed families (e.g. macOS's `.AppleColorEmoji`, `.LastResort`) —
    // fontdb enumerates them and they legitimately lack ASCII glyphs, but no real user would pick
    // one from the toolbar's font dropdown; picking one here would test the wrong thing.
    if let Some(entry) = catalog
        .list_entries()
        .into_iter()
        .find(|e| e.embeddable && e.subsettable && !e.family.starts_with('.'))
    {
        let first_page_graph = walk_page_graph(&doc, &sha, 1);
        let media = first_page_graph
            .as_ref()
            .map(|g| g.media_box)
            .unwrap_or([0.0, 0.0, 612.0, 792.0]);
        let command = PdfEditCommand::AddText {
            id: "qa-add".into(),
            page: 1,
            anchor: PdfPoint {
                x: media[0] + (media[2] - media[0]) * 0.1,
                y: media[1] + (media[3] - media[1]) * 0.5,
            },
            text: "RightKit QA 123".into(),
            appearance: rightkit_pdf::model::PdfTextAppearance {
                font_id: entry.id.clone(),
                family: entry.family.clone(),
                size_pt: 12.0,
                color: rightkit_pdf::model::PdfColor {
                    r: 0.1,
                    g: 0.1,
                    b: 0.1,
                    alpha: 1.0,
                },
            },
        };
        report.add_text = match compose_candidate(&bytes, &[command], catalog) {
            Err(e) => format!("typed reject: {e}"),
            Ok(candidate) => match Document::load_mem(&candidate.bytes) {
                Err(e) => format!("FATAL: candidate did not reopen: {e}"),
                Ok(reopened) => {
                    let new_sha = document_sha256(&candidate.bytes);
                    match walk_page_graph(&reopened, &new_sha, 1) {
                        Err(e) => format!("typed reject on rewalk: {e}"),
                        Ok(graph) => {
                            if graph.islands.iter().any(|i| i.text.contains("RightKit QA")) {
                                "ok".into()
                            } else {
                                "FATAL: added text missing after reopen".into()
                            }
                        }
                    }
                }
            },
        };
    } else {
        report.add_text = "skip: no embeddable catalog font".into();
    }

    report
}

#[test]
#[ignore]
fn corpus_qa_diagnose_one() {
    let dir = std::env::var("RK_PDF_CORPUS_DIR").unwrap_or_else(|_| "../../../.qa-corpus".into());
    let name = std::env::var("RIGHTKIT_PDF_CORPUS_FILE").unwrap_or_else(|_| "irs-p17.pdf".into());
    let path = PathBuf::from(&dir).join(&name);
    let bytes = std::fs::read(&path).expect("read");
    let doc = Document::load_mem(&bytes).expect("load");
    let sha = document_sha256(&bytes);
    let catalog = FontCatalog::discover();

    for page in 1..=MAX_PAGES_WALKED {
        let graph = match walk_page_graph(&doc, &sha, page) {
            Ok(g) => g,
            Err(e) => {
                println!("page {page}: walk error {e}");
                continue;
            }
        };
        if let Some(island) = graph
            .islands
            .iter()
            .find(|i| i.editable && i.text.trim().len() >= 4)
        {
            let mut after_text = island.text.clone();
            after_text.pop();
            let bounds = quads_union(island).expect("bounds");
            println!(
                "page {page}: target island id={} text={:?} after={:?} locator_spans={} digest={}",
                island.id,
                island.text,
                after_text,
                island.locator.spans.len(),
                island.locator.island_digest
            );
            let command = PdfEditCommand::ReplaceSourceText {
                id: "diag".into(),
                locator: island.locator.clone(),
                before: PdfTextEditState {
                    text: island.text.clone(),
                    appearance: island.appearance.clone(),
                },
                after: PdfTextEditState {
                    text: after_text.clone(),
                    appearance: island.appearance.clone(),
                },
                available_bounds: bounds,
            };
            let candidate = compose_candidate(&bytes, &[command], &catalog).expect("compose");
            println!("affected_pages={:?}", candidate.affected_pages);
            let reopened = Document::load_mem(&candidate.bytes).expect("reopen");
            let new_sha = document_sha256(&candidate.bytes);
            let new_graph = walk_page_graph(&reopened, &new_sha, page).expect("rewalk");
            println!("post-reopen islands on page {page}:");
            for i in &new_graph.islands {
                if i.text
                    .to_lowercase()
                    .contains(&after_text.to_lowercase()[..after_text.len().min(6)])
                    || i.id == island.id
                {
                    println!(
                        "  id={} text={:?} editable={} rejection={:?}",
                        i.id, i.text, i.editable, i.rejection
                    );
                }
            }
            println!("total islands post-reopen: {}", new_graph.islands.len());
            return;
        }
    }
    panic!("no editable island found in first {MAX_PAGES_WALKED} pages");
}

#[test]
#[ignore]
fn corpus_qa_matrix() {
    let dir = std::env::var("RK_PDF_CORPUS_DIR").unwrap_or_else(|_| "../../../.qa-corpus".into());
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("corpus dir must exist")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "pdf"))
        .collect();
    files.sort();
    assert!(files.len() >= 20, "corpus must hold at least 20 PDFs");

    let catalog = FontCatalog::discover();
    let mut fatal_count = 0usize;

    println!("\n=== RightKit PDF corpus QA ({} files) ===", files.len());
    for path in &files {
        let result = catch_unwind(AssertUnwindSafe(|| run_one(path, &catalog)));
        let report = match result {
            Ok(r) => r,
            Err(_) => FileReport {
                name: path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                panicked: true,
                ..FileReport::default()
            },
        };
        let fatal = report.panicked
            || report.replace.starts_with("FATAL")
            || report.add_text.starts_with("FATAL");
        if fatal {
            fatal_count += 1;
        }
        println!(
            "{:<22} pages={:<4} walked={:<3} islands={:<5} editable={:<5} replace={:<40} addText={:<40} walkErrs={} {}{}",
            report.name,
            report.pages,
            report.pages_walked,
            report.islands,
            report.editable,
            report.replace,
            report.add_text,
            report.walk_errors.len(),
            if report.panicked { "PANIC " } else { "" },
            report.fatal.as_deref().unwrap_or(""),
        );
        for e in report.walk_errors.iter().take(3) {
            println!("    walk: {e}");
        }
    }
    assert_eq!(
        fatal_count, 0,
        "{fatal_count} file(s) hit panics or fatal round-trip failures"
    );
}
