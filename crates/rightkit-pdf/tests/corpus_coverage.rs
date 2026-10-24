#![cfg(feature = "edit")]

//! Re-encoding coverage, measured on a real corpus (test-only, `#[ignore]` - run explicitly):
//!
//! ```sh
//! RK_PDF_CORPUS_DIR=/Volumes/D/claude/viewright/.qa-corpus \
//!   rustright cargo test -p rightkit-pdf --test corpus_coverage -- --ignored --nocapture
//! ```
//!
//! For page 1 of every `*.pdf` in the corpus it runs the replacement ViewRight's QA runs: take an
//! island, rotate its first 5+ letter word (`Alpha` -> `lphaA`, so every needed glyph is one the
//! island already shows), and compose `ReplaceSourceText` with unchanged appearance and page-wide
//! bounds. It prints, per file, how many islands the graph reports, how many it calls editable,
//! how many replacements were tried (editable islands with a rotatable word, capped at 25 per
//! file like the app-side QA), how many composed AND read back, and the typed refusals. It also
//! histograms the `editBlocker` reasons of the islands the graph refuses, which is the ranked
//! list of encoding cases left to unlock.
//!
//! `ok` means the candidate composed, reopened, and the new text was read back from page 1 with
//! the same decoder. Read-back proves the write is structurally sound; it cannot prove a glyph is
//! the one the author sees (the decoder and encoder share a table), so coverage numbers here are
//! an upper bound on glyph correctness - see the README's limits.
//!
//! The final assertion is the contract of the shared decision: an island the graph calls
//! editable must never be refused with `unsupported_encoding` (only `missing_glyph` or
//! `text_overflow` are legitimate refusals of an editable island).

use std::collections::BTreeMap;
use std::path::PathBuf;

use rightkit_pdf::compose::compose_candidate;
use rightkit_pdf::inspect::page_graph_from_bytes;
use rightkit_pdf::model::{
    GlyphId, PdfEditCommand, PdfEditError, PdfFontResolver, PdfRect, PdfTextEditState,
    PdfTextIsland, ResolvedPdfFont,
};

const MAX_TRIED_PER_FILE: usize = 25;

/// Replacement attempts never reach the catalog (appearance is unchanged), so no system-font scan.
struct NoFonts;

impl PdfFontResolver for NoFonts {
    fn resolve(&self, _id: &str) -> Result<ResolvedPdfFont, PdfEditError> {
        Err(PdfEditError::UnsupportedEncoding)
    }
    fn glyphs_for_text(
        &self,
        _font: &ResolvedPdfFont,
        _text: &str,
    ) -> Result<Vec<GlyphId>, PdfEditError> {
        Err(PdfEditError::UnsupportedEncoding)
    }
}

/// `islands / editable / tried / ok / unsupported` reported by ViewRight against 0.2.0, for the
/// delta column. Source: gap ledger row 93.
const BASELINE: &[(&str, [usize; 5])] = &[
    ("bitcoin.pdf", [40, 25, 25, 0, 25]),
    ("gpt3.pdf", [105, 30, 25, 0, 25]),
    ("irs-p17.pdf", [91, 91, 17, 0, 17]),
    ("bert.pdf", [183, 31, 25, 1, 24]),
    ("attention.pdf", [105, 32, 25, 6, 19]),
    ("irs-1040.pdf", [620, 48, 25, 17, 8]),
    ("gsa-sf1094.pdf", [40, 26, 25, 23, 2]),
];

/// First run of five or more ASCII letters, as `(byte_start, word)`.
fn first_long_word(text: &str) -> Option<(usize, &str)> {
    let mut start: Option<usize> = None;
    for (index, ch) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        match (ch.is_ascii_alphabetic(), start) {
            (true, None) => start = Some(index),
            (false, Some(from)) => {
                if index - from >= 5 {
                    return Some((from, &text[from..index]));
                }
                start = None;
            }
            _ => {}
        }
    }
    None
}

/// The rotated text and the rotated word's character range inside `island.text`.
fn rotated_text(island: &PdfTextIsland) -> Option<(String, std::ops::Range<usize>)> {
    let (at, word) = first_long_word(&island.text)?;
    let mut chars = word.chars();
    let first = chars.next()?;
    let rotated: String = chars.chain(std::iter::once(first)).collect();
    if rotated == word {
        return None;
    }
    let first_char = island.text[..at].chars().count();
    let range = first_char..first_char + word.chars().count();
    Some((
        format!(
            "{}{}{}",
            &island.text[..at],
            rotated,
            &island.text[at + word.len()..]
        ),
        range,
    ))
}

/// Outcome of one edit, judged on the re-read page.
enum EditCheck {
    /// An island's text equals the request and every unchanged glyph stayed where it was.
    Ok(Vec<u8>),
    /// No island's text equals the request exactly.
    TextMismatch(String),
    /// Text matches but an unchanged glyph moved: `(glyph index, before x, after x)`.
    Moved(usize, f64, f64),
}

/// Real-glyph index of character `char_index`: gaps are characters but not glyphs.
fn glyph_index(island: &PdfTextIsland, char_index: usize) -> usize {
    char_index
        - island
            .gaps
            .iter()
            .filter(|g| g.char_index < char_index)
            .count()
}

/// Composes the rotation and verifies it exactly: the request must equal some island's text
/// (synthesised spaces included) and every glyph outside the rotated word must keep its x within
/// 0.01 pt (the rotation preserves the word's advance, so nothing on the line may move).
fn edit_and_check(
    bytes: &[u8],
    island: &PdfTextIsland,
    after: &str,
    word: std::ops::Range<usize>,
    bounds: PdfRect,
) -> Result<EditCheck, PdfEditError> {
    let command = PdfEditCommand::ReplaceSourceText {
        id: "coverage".into(),
        locator: island.locator.clone(),
        before: PdfTextEditState {
            text: island.text.clone(),
            appearance: island.appearance.clone(),
        },
        after: PdfTextEditState {
            text: after.to_string(),
            appearance: island.appearance.clone(),
        },
        available_bounds: bounds,
    };
    let candidate = compose_candidate(bytes, &[command], &NoFonts)?;
    let Ok(graph) = page_graph_from_bytes(&candidate.bytes, island.locator.page) else {
        return Ok(EditCheck::TextMismatch(
            "candidate page did not re-read".into(),
        ));
    };
    let Some(read) = graph.islands.iter().find(|i| i.text == after) else {
        let near: Vec<&str> = graph
            .islands
            .iter()
            .map(|i| i.text.as_str())
            .filter(|t| t.contains(after.chars().take(8).collect::<String>().as_str()))
            .collect();
        return Ok(EditCheck::TextMismatch(format!(
            "no island equals {after:?}; same-prefix islands: {near:?}"
        )));
    };
    let real = island.text.chars().count() - island.gaps.len();
    if read.quads.len() != island.quads.len() || island.quads.len() != real {
        return Ok(EditCheck::TextMismatch(format!(
            "glyph count {} -> {} (text has {} real glyphs)",
            island.quads.len(),
            read.quads.len(),
            real
        )));
    }
    let (g0, g1) = (
        glyph_index(island, word.start),
        glyph_index(island, word.end),
    );
    for (index, (b, a)) in island.quads.iter().zip(&read.quads).enumerate() {
        if (g0..g1).contains(&index) {
            continue;
        }
        if (b[0] - a[0]).abs() > 0.01 {
            return Ok(EditCheck::Moved(index, b[0], a[0]));
        }
    }
    Ok(EditCheck::Ok(candidate.bytes))
}

#[derive(Default)]
struct Row {
    graph_error: Option<String>,
    islands: usize,
    editable: usize,
    tried: usize,
    ok: usize,
    readback_miss: usize,
    moved: usize,
    non_editable_positioned: usize,
    refusals: BTreeMap<&'static str, usize>,
    blockers: BTreeMap<String, usize>,
}

fn measure(bytes: &[u8], save_as: Option<&std::path::Path>) -> Row {
    let mut saved = false;
    let mut row = Row { ..Row::default() };
    let graph = match page_graph_from_bytes(bytes, 1) {
        Ok(graph) => graph,
        Err(error) => {
            row.graph_error = Some(error.code().to_string());
            return row;
        }
    };
    row.islands = graph.islands.len();
    row.editable = graph.islands.iter().filter(|i| i.editable).count();
    row.non_editable_positioned = graph
        .islands
        .iter()
        .filter(|i| i.edit_blocker == Some(rightkit_pdf::model::PdfEditBlocker::PositionedTextRun))
        .count();
    for island in graph.islands.iter().filter(|i| !i.editable) {
        let key = island
            .edit_blocker
            .map(|b| format!("{b:?}"))
            .unwrap_or_else(|| "none (rejected before grouping)".to_string());
        *row.blockers.entry(key).or_default() += 1;
    }

    let page_wide = PdfRect {
        x: graph.media_box[0],
        y: graph.media_box[1],
        width: graph.media_box[2] - graph.media_box[0],
        height: graph.media_box[3] - graph.media_box[1],
    };
    for island in graph.islands.iter().filter(|i| i.editable) {
        if row.tried >= MAX_TRIED_PER_FILE {
            break;
        }
        let Some((after, word)) = rotated_text(island) else {
            continue;
        };
        row.tried += 1;
        match edit_and_check(bytes, island, &after, word, page_wide) {
            Ok(EditCheck::Ok(edited)) => {
                row.ok += 1;
                if let (Some(path), false) = (save_as, saved) {
                    std::fs::write(path, &edited).expect("write edited pdf");
                    saved = true;
                }
            }
            Ok(EditCheck::TextMismatch(why)) => {
                println!("    MISMATCH {why}");
                row.readback_miss += 1;
            }
            Ok(EditCheck::Moved(index, before, after_x)) => {
                println!(
                    "    MOVED glyph {index}: {before} -> {after_x} in {:?}",
                    island.text
                );
                row.moved += 1;
            }
            Err(error) => {
                let code: &'static str = match error.code() {
                    "unsupported_encoding" => "unsupported_encoding",
                    "missing_glyph" => "missing_glyph",
                    "text_overflow" => "text_overflow",
                    "malformed_content" => "malformed_content",
                    _ => "other",
                };
                *row.refusals.entry(code).or_default() += 1;
            }
        }
    }
    row
}

fn corpus_files() -> Vec<PathBuf> {
    let dir = std::env::var("RK_PDF_CORPUS_DIR")
        .unwrap_or_else(|_| "/Volumes/D/claude/viewright/.qa-corpus".into());
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("corpus dir {dir}: {e}"))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "pdf"))
        .collect();
    files.sort();
    files
}

#[test]
#[ignore]
fn corpus_replace_source_text_coverage() {
    let files = corpus_files();
    assert!(!files.is_empty(), "corpus has no PDFs");

    let mut totals = Row::default();
    let mut blockers_total: BTreeMap<String, usize> = BTreeMap::new();
    let mut disagreements = Vec::new();

    println!("\nfile | islands | editable | tried | ok | readback_miss | refusals | (baseline i/e/t/ok/unsup)");
    for path in &files {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Ok(bytes) = std::fs::read(path) else {
            println!("{name} | unreadable");
            continue;
        };
        let save_as = Some(std::env::var("RK_PDF_EDITED_DIR").unwrap_or_else(|_| {
            // The wrapper may not pass the environment through; ViewRight looks here.
            "/Volumes/D/claude/tasks/handoffs/2026-10-11/pdf-0.2.1-edited".into()
        }))
        .and_then(|dir| {
            let stem = name.trim_end_matches(".pdf");
            ["gpt3", "bert", "attention", "bitcoin", "irs-1040"]
                .contains(&stem)
                .then(|| {
                    std::fs::create_dir_all(&dir).expect("edited dir");
                    PathBuf::from(dir).join(format!("{stem}-edited.pdf"))
                })
        });
        let row = measure(&bytes, save_as.as_deref());
        let baseline = BASELINE
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, b)| format!(" | baseline {}/{}/{}/{}/{}", b[0], b[1], b[2], b[3], b[4]))
            .unwrap_or_default();
        match &row.graph_error {
            Some(code) => println!("{name} | GRAPH ERROR {code}{baseline}"),
            None => println!(
                "{name} | {} | {} | {} | {} | {} | moved {} | positioned {} | {:?}{baseline}",
                row.islands,
                row.editable,
                row.tried,
                row.ok,
                row.readback_miss,
                row.moved,
                row.non_editable_positioned,
                row.refusals
            ),
        }
        if row.refusals.contains_key("unsupported_encoding") {
            disagreements.push(name.clone());
        }
        for (blocker, count) in &row.blockers {
            *blockers_total.entry(blocker.clone()).or_default() += count;
        }
        totals.islands += row.islands;
        totals.editable += row.editable;
        totals.tried += row.tried;
        totals.ok += row.ok;
        totals.readback_miss += row.readback_miss;
        totals.moved += row.moved;
        totals.non_editable_positioned += row.non_editable_positioned;
        for (code, count) in row.refusals {
            *totals.refusals.entry(code).or_default() += count;
        }
    }

    println!(
        "TOTAL | {} | {} | {} | {} | {} | moved {} | positioned {} | {:?}",
        totals.islands,
        totals.editable,
        totals.tried,
        totals.ok,
        totals.readback_miss,
        totals.moved,
        totals.non_editable_positioned,
        totals.refusals
    );
    println!("islands the graph refuses, by edit_blocker (page 1, all files):");
    let mut ranked: Vec<_> = blockers_total.into_iter().collect();
    ranked.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    for (blocker, count) in ranked {
        println!("  {count:>6}  {blocker}");
    }

    assert_eq!(totals.moved, 0, "an edit moved an unchanged glyph");
    assert!(
        disagreements.is_empty(),
        "graph said editable but compose refused unsupported_encoding in: {disagreements:?}"
    );
}

#[test]
#[ignore]
fn corpus_uscis_i9_every_page_graph_parses() {
    // Ledger #93: `/Contents` is an indirect reference to an array of streams.
    let dir = std::env::var("RK_PDF_CORPUS_DIR")
        .unwrap_or_else(|_| "/Volumes/D/claude/viewright/.qa-corpus".into());
    let bytes = std::fs::read(PathBuf::from(dir).join("uscis-i9.pdf")).expect("uscis-i9.pdf");
    for page in 1..=4 {
        let graph = page_graph_from_bytes(&bytes, page)
            .unwrap_or_else(|error| panic!("page {page}: {error}"));
        println!("uscis-i9 page {page}: {} islands", graph.islands.len());
        if page == 1 {
            assert!(!graph.islands.is_empty(), "page 1 has text");
        }
    }
}

/// Real LaTeX lines whose word gaps are `TJ` adjustments, not space glyphs. The island text must
/// carry the spaces, a one-word rotation must read back exactly, and no other glyph may move.
fn latex_line_case(file: &str, phrase: &str) {
    let dir = std::env::var("RK_PDF_CORPUS_DIR")
        .unwrap_or_else(|_| "/Volumes/D/claude/viewright/.qa-corpus".into());
    let bytes = std::fs::read(PathBuf::from(dir).join(file)).expect(file);
    let graph = page_graph_from_bytes(&bytes, 1).expect("graph");
    let Some(island) = graph.islands.iter().find(|i| i.text.contains(phrase)) else {
        let near: Vec<&str> = graph
            .islands
            .iter()
            .map(|i| i.text.as_str())
            .filter(|t| t.len() > 12)
            .take(8)
            .collect();
        panic!("{file}: no island contains {phrase:?}; sample islands: {near:?}");
    };
    println!("{file}: {:?} gaps={}", island.text, island.gaps.len());
    assert!(
        !island.gaps.is_empty(),
        "{file}: word gaps were not synthesised"
    );
    if !island.editable {
        println!(
            "{file}: not editable ({:?}) - nothing to damage",
            island.edit_blocker
        );
        return;
    }
    let (after, word) = rotated_text(island).expect("a 5+ letter word");
    let bounds = PdfRect {
        x: graph.media_box[0],
        y: graph.media_box[1],
        width: graph.media_box[2] - graph.media_box[0],
        height: graph.media_box[3] - graph.media_box[1],
    };
    match edit_and_check(&bytes, island, &after, word, bounds) {
        Ok(EditCheck::Ok(_)) => {}
        Ok(EditCheck::TextMismatch(why)) => panic!("{file}: {why}"),
        Ok(EditCheck::Moved(i, b, a)) => panic!("{file}: glyph {i} moved {b} -> {a}"),
        Err(error) => panic!("{file}: {error}"),
    }
}

#[test]
#[ignore]
fn corpus_gpt3_title_keeps_word_spaces() {
    latex_line_case("gpt3.pdf", "Language Models are");
}

#[test]
#[ignore]
fn corpus_bert_title_line_keeps_word_spaces() {
    latex_line_case("bert.pdf", "Pre-training of Deep");
}

#[test]
#[ignore]
fn corpus_attention_notice_line_keeps_word_spaces() {
    latex_line_case("attention.pdf", "Provided proper attribution");
}
