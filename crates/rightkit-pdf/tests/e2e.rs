#![cfg(feature = "edit")]

//! End-to-end PDF engine tests on real files: Chrome-produced fixtures (Type0 subset fonts),
//! the bundled Source Serif 4 embed path, atomic commit to disk, and (macOS) CoreGraphics
//! rasterization via the OS `sips` tool for visual verification.

use std::path::{Path, PathBuf};

use lopdf::{dictionary, Document, Object, Stream};
use rightkit_pdf::commit::{commit_pdf_save, CommitError};
use rightkit_pdf::compose::compose_candidate;
use rightkit_pdf::font::FontCatalog;
use rightkit_pdf::inspect::walk_page_graph;
use rightkit_pdf::model::{
    document_sha256, PdfColor, PdfEditCommand, PdfEditError, PdfPoint, PdfRect, PdfTextAppearance,
    PdfTextEditState,
};
use rightkit_pdf::save::{expected_text_checks, stage_prepared_save, PdfSaveTokenStore};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    )
    .unwrap()
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rightkit-pdf-e2e-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn page_text(bytes: &[u8], page: u32) -> String {
    let doc = Document::load_mem(bytes).unwrap();
    let graph = walk_page_graph(&doc, &document_sha256(bytes), page).unwrap();
    graph
        .islands
        .iter()
        .map(|i| i.text.clone())
        .collect::<Vec<_>>()
        .join(" ")
}

fn page_count(bytes: &[u8]) -> usize {
    Document::load_mem(bytes).unwrap().get_pages().len()
}

/// A hand-built single-page PDF with a base-14 Helvetica line (editable in place).
fn helvetica_pdf(text: &str) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let font = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    });
    let content = doc.add_object(Stream::new(
        dictionary! {},
        format!("BT /F1 18 Tf 72 700 Td ({text}) Tj ET").into_bytes(),
    ));
    let page = doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
        "Contents" => content,
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1,
        }),
    );
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog);
    let mut out = Vec::new();
    doc.save_to(&mut out).unwrap();
    out
}

fn appearance_for(family_id: &str) -> PdfTextAppearance {
    let entry = FontCatalog::shared()
        .list_entries()
        .into_iter()
        .find(|e| e.id == family_id)
        .expect("bundled font present");
    PdfTextAppearance {
        font_id: entry.id,
        family: entry.family,
        size_pt: 16.0,
        color: PdfColor {
            r: 0.8,
            g: 0.0,
            b: 0.0,
            alpha: 1.0,
        },
    }
}

#[test]
fn replace_source_text_in_place_round_trips_through_reopen() {
    let original = helvetica_pdf("Hello World");
    let doc = Document::load_mem(&original).unwrap();
    let sha = document_sha256(&original);
    let graph = walk_page_graph(&doc, &sha, 1).unwrap();
    let island = graph
        .islands
        .iter()
        .find(|i| i.text == "Hello World")
        .expect("island");
    assert!(island.editable, "{:?}", island.rejection);
    let q = island.quads[0];
    let bounds = PdfRect {
        x: q[0] - 1.0,
        y: q[1] - 1.0,
        width: 400.0,
        height: 30.0,
    };
    let command = PdfEditCommand::ReplaceSourceText {
        id: "r1".into(),
        locator: island.locator.clone(),
        before: PdfTextEditState {
            text: island.text.clone(),
            appearance: island.appearance.clone(),
        },
        after: PdfTextEditState {
            text: "Hello Rustaceans".into(),
            appearance: island.appearance.clone(),
        },
        available_bounds: bounds,
    };
    let out = compose_candidate(
        &original,
        std::slice::from_ref(&command),
        FontCatalog::shared(),
    )
    .unwrap();
    assert_eq!(out.affected_pages, vec![1]);
    let text = page_text(&out.bytes, 1);
    assert!(text.contains("Hello Rustaceans"), "{text}");
    assert!(!text.contains("World"), "{text}");
    assert_ne!(document_sha256(&out.bytes), sha);

    // The same command against the edited bytes is stale (locator digest bound to the original).
    let stale = compose_candidate(&out.bytes, &[command], FontCatalog::shared());
    assert!(matches!(stale, Err(PdfEditError::StaleSource)), "{stale:?}");
}

#[test]
fn add_text_on_chrome_pdf_stages_and_commits_atomically_leaving_other_pages_byte_identical() {
    let original = fixture("report.pdf");
    assert_eq!(page_count(&original), 3);
    let sha = document_sha256(&original);
    let command = PdfEditCommand::AddText {
        id: "a1".into(),
        page: 2,
        anchor: PdfPoint { x: 72.0, y: 400.0 },
        text: "Reviewed by RightKit".into(),
        appearance: appearance_for("source-serif-4"),
    };
    let composed = compose_candidate(
        &original,
        std::slice::from_ref(&command),
        FontCatalog::shared(),
    )
    .unwrap();
    assert_eq!(composed.affected_pages, vec![2]);

    let dir = scratch("commit");
    let target = dir.join("report.pdf");
    std::fs::write(&target, &original).unwrap();
    let store = PdfSaveTokenStore::default();
    let meta = stage_prepared_save(
        &store,
        target.to_str().unwrap(),
        &sha,
        &document_sha256(&std::fs::read(&target).unwrap()),
        composed.bytes.clone(),
        composed.affected_pages.clone(),
        expected_text_checks(&[command]),
    )
    .unwrap();

    let committed = commit_pdf_save(&store, &meta.token, None).unwrap();
    let saved = std::fs::read(&target).unwrap();
    assert_eq!(
        saved, composed.bytes,
        "on-disk bytes equal the verified candidate"
    );
    assert_eq!(committed.document_sha256, document_sha256(&saved));
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "temp file cleaned up");

    // Content: new text on page 2; pages 1 and 3 keep their exact text.
    assert!(page_text(&saved, 2).contains("Reviewed by RightKit"));
    for p in [1_u32, 3] {
        assert_eq!(
            page_text(&saved, p),
            page_text(&original, p),
            "page {p} text unchanged"
        );
    }
    // Bytes: untouched pages keep byte-identical content streams.
    let (a, b) = (
        Document::load_mem(&original).unwrap(),
        Document::load_mem(&saved).unwrap(),
    );
    for p in [1_u32, 3] {
        let ca = a.get_page_content(a.get_pages()[&p]);
        let cb = b.get_page_content(b.get_pages()[&p]);
        assert_eq!(ca, cb, "page {p} content stream byte-identical");
    }

    // Single-use: replaying the token is rejected and the file is untouched.
    let replay = commit_pdf_save(&store, &meta.token, None);
    assert!(
        matches!(replay, Err(CommitError::Edit(PdfEditError::WriteConflict))),
        "{replay:?}"
    );
    assert_eq!(std::fs::read(&target).unwrap(), saved);
}

#[test]
fn same_path_commit_refuses_when_source_changed_after_staging() {
    let original = fixture("memo.pdf");
    let sha = document_sha256(&original);
    let command = PdfEditCommand::AddText {
        id: "a1".into(),
        page: 1,
        anchor: PdfPoint { x: 72.0, y: 300.0 },
        text: "Approved".into(),
        appearance: appearance_for("source-serif-4"),
    };
    let composed = compose_candidate(
        &original,
        std::slice::from_ref(&command),
        FontCatalog::shared(),
    )
    .unwrap();
    let dir = scratch("conflict");
    let target = dir.join("memo.pdf");
    std::fs::write(&target, &original).unwrap();
    let store = PdfSaveTokenStore::default();
    let meta = stage_prepared_save(
        &store,
        target.to_str().unwrap(),
        &sha,
        &sha,
        composed.bytes,
        vec![1],
        expected_text_checks(&[command]),
    )
    .unwrap();
    // Another writer replaces the file before commit.
    let theirs = fixture("report.pdf");
    std::fs::write(&target, &theirs).unwrap();
    let r = commit_pdf_save(&store, &meta.token, None);
    assert!(
        matches!(r, Err(CommitError::Edit(PdfEditError::WriteConflict))),
        "{r:?}"
    );
    assert_eq!(
        std::fs::read(&target).unwrap(),
        theirs,
        "their file was not clobbered"
    );
}

/// An Add Text edit of `memo.pdf`: the command and the composed candidate bytes.
fn memo_edit(text: &str) -> (PdfEditCommand, Vec<u8>) {
    let command = PdfEditCommand::AddText {
        id: format!("add-{text}"),
        page: 1,
        anchor: PdfPoint { x: 72.0, y: 300.0 },
        text: text.into(),
        appearance: appearance_for("source-serif-4"),
    };
    let composed = compose_candidate(
        &fixture("memo.pdf"),
        std::slice::from_ref(&command),
        FontCatalog::shared(),
    )
    .unwrap();
    (command, composed.bytes)
}

/// Stages `memo_edit(text)` against `path` (which must hold the original `memo.pdf`); returns the
/// token and the candidate bytes it will write.
fn stage_memo(store: &PdfSaveTokenStore, path: &Path, text: &str) -> (String, Vec<u8>) {
    let sha = document_sha256(&fixture("memo.pdf"));
    let (command, bytes) = memo_edit(text);
    let meta = stage_prepared_save(
        store,
        path.to_str().unwrap(),
        &sha,
        &sha,
        bytes.clone(),
        vec![1],
        expected_text_checks(&[command]),
    )
    .unwrap();
    (meta.token, bytes)
}

fn assert_write_conflict(result: Result<rightkit_pdf::save::PdfCommitResult, CommitError>) {
    assert!(
        matches!(result, Err(CommitError::Edit(PdfEditError::WriteConflict))),
        "{result:?}"
    );
}

#[test]
fn commit_to_a_dot_segment_alias_of_the_source_still_rechecks_the_source() {
    // `dir/./memo.pdf` is `dir/memo.pdf`: string inequality used to skip the SHA recheck and
    // overwrite the newer bytes.
    let dir = scratch("alias");
    let source = dir.join("memo.pdf");
    std::fs::write(&source, fixture("memo.pdf")).unwrap();
    let store = PdfSaveTokenStore::default();
    let (token, _) = stage_memo(&store, &source, "Alias");

    let theirs = fixture("report.pdf");
    std::fs::write(&source, &theirs).unwrap();
    let alias = dir.join(".").join("memo.pdf");
    assert_write_conflict(commit_pdf_save(&store, &token, Some(alias.as_path())));
    assert_eq!(
        std::fs::read(&source).unwrap(),
        theirs,
        "their file survives"
    );
}

#[test]
fn commit_to_a_parent_segment_alias_of_the_source_still_rechecks_the_source() {
    let dir = scratch("alias-parent");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let source = dir.join("memo.pdf");
    std::fs::write(&source, fixture("memo.pdf")).unwrap();
    let store = PdfSaveTokenStore::default();
    let (token, _) = stage_memo(&store, &source, "Parent");

    let theirs = fixture("report.pdf");
    std::fs::write(&source, &theirs).unwrap();
    let alias = dir.join("sub").join("..").join("memo.pdf");
    assert_write_conflict(commit_pdf_save(&store, &token, Some(alias.as_path())));
    assert_eq!(std::fs::read(&source).unwrap(), theirs);
}

#[cfg(unix)]
#[test]
fn commit_to_a_hard_link_of_the_source_still_rechecks_the_source() {
    let dir = scratch("hardlink");
    let source = dir.join("memo.pdf");
    std::fs::write(&source, fixture("memo.pdf")).unwrap();
    let link = dir.join("linked.pdf");
    std::fs::hard_link(&source, &link).unwrap();
    let store = PdfSaveTokenStore::default();
    let (token, _) = stage_memo(&store, &source, "Link");

    // Rewritten in place, so the link sees the other writer's bytes too.
    let theirs = fixture("report.pdf");
    std::fs::write(&source, &theirs).unwrap();
    assert_write_conflict(commit_pdf_save(&store, &token, Some(link.as_path())));
    assert_eq!(std::fs::read(&link).unwrap(), theirs);
}

#[test]
fn commit_to_a_case_variant_of_the_source_still_rechecks_the_source() {
    let dir = scratch("case");
    let source = dir.join("memo.pdf");
    std::fs::write(&source, fixture("memo.pdf")).unwrap();
    let variant = dir.join("MEMO.PDF");
    if !variant.exists() {
        return; // a case-sensitive volume: the variant names a different (absent) file
    }
    let store = PdfSaveTokenStore::default();
    let (token, _) = stage_memo(&store, &source, "Case");

    let theirs = fixture("report.pdf");
    std::fs::write(&source, &theirs).unwrap();
    assert_write_conflict(commit_pdf_save(&store, &token, Some(variant.as_path())));
    assert_eq!(std::fs::read(&source).unwrap(), theirs);
}

#[test]
fn save_as_to_a_different_file_is_still_not_rechecked() {
    let dir = scratch("save-as");
    let source = dir.join("memo.pdf");
    std::fs::write(&source, fixture("memo.pdf")).unwrap();
    let store = PdfSaveTokenStore::default();
    let (token, bytes) = stage_memo(&store, &source, "Copy");

    // The source changed, but a copy elsewhere never depended on it.
    std::fs::write(&source, fixture("report.pdf")).unwrap();
    let copy = dir.join("copy.pdf");
    commit_pdf_save(&store, &token, Some(copy.as_path())).unwrap();
    assert_eq!(std::fs::read(&copy).unwrap(), bytes);
}

#[test]
fn two_commits_staged_against_one_source_cannot_both_succeed() {
    // Two candidates staged against the same bytes, committed from two threads: whichever lands
    // first wins, the other gets the conflict error, and the file is exactly the winner's. The
    // deterministic interleaving is `commit::tests`; this is the public-API version.
    for round in 0..5 {
        let dir = scratch(&format!("race-{round}"));
        let source = dir.join("memo.pdf");
        std::fs::write(&source, fixture("memo.pdf")).unwrap();
        let store = PdfSaveTokenStore::default();
        let staged = [
            stage_memo(&store, &source, "First"),
            stage_memo(&store, &source, "Second"),
        ];
        let start = std::sync::Barrier::new(2);
        let results: Vec<_> = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for (token, _) in &staged {
                let (store, start) = (&store, &start);
                handles.push(scope.spawn(move || {
                    start.wait();
                    commit_pdf_save(store, token, None)
                }));
            }
            let mut finished = Vec::new();
            for handle in handles {
                finished.push(handle.join().unwrap());
            }
            finished
        });
        let winners: Vec<usize> = (0..2).filter(|i| results[*i].is_ok()).collect();
        assert_eq!(winners.len(), 1, "round {round}: {results:?}");
        assert_eq!(
            std::fs::read(&source).unwrap(),
            staged[winners[0]].1,
            "round {round}: the file is the winner's candidate"
        );
        let loser = results.into_iter().nth(1 - winners[0]).unwrap();
        assert_write_conflict(loser);
    }
}

#[test]
fn a_successful_commit_invalidates_the_sibling_token_for_that_file() {
    let dir = scratch("sibling");
    let source = dir.join("memo.pdf");
    std::fs::write(&source, fixture("memo.pdf")).unwrap();
    let store = PdfSaveTokenStore::default();
    let (first, first_bytes) = stage_memo(&store, &source, "One");
    let (second, _) = stage_memo(&store, &source, "Two");

    commit_pdf_save(&store, &first, None).unwrap();
    // Even saved to a different path (so no source recheck could object), the sibling is gone.
    let elsewhere = dir.join("elsewhere.pdf");
    assert_write_conflict(commit_pdf_save(&store, &second, Some(elsewhere.as_path())));
    assert!(!elsewhere.exists());
    assert_eq!(std::fs::read(&source).unwrap(), first_bytes);
}

#[test]
fn staging_rejects_candidate_missing_expected_text() {
    let original = fixture("memo.pdf");
    let sha = document_sha256(&original);
    let command = PdfEditCommand::AddText {
        id: "a1".into(),
        page: 1,
        anchor: PdfPoint { x: 72.0, y: 300.0 },
        text: "Present".into(),
        appearance: appearance_for("source-serif-4"),
    };
    let mut checks = expected_text_checks(&[command]);
    checks[0].text = "Never written".into();
    let r = stage_prepared_save(
        &PdfSaveTokenStore::default(),
        "p.pdf",
        &sha,
        &sha,
        original,
        vec![1],
        checks,
    );
    assert!(matches!(r, Err(PdfEditError::VerificationFailed)));
}

#[cfg(feature = "merge")]
#[test]
fn merge_and_select_preserve_page_content_and_order() {
    use rightkit_pdf::merge::{merge_pdfs, select_pages, MergeError};
    let (report, memo) = (fixture("report.pdf"), fixture("memo.pdf"));
    let merged = merge_pdfs(&[&report, &memo]).unwrap();
    assert_eq!(page_count(&merged), 4);
    assert!(page_text(&merged, 1).contains("Quarterly Report"));
    assert!(page_text(&merged, 3).contains("Outlook"));
    assert!(page_text(&merged, 4).contains("Delta memo"));

    let picked = select_pages(&merged, &[4, 2, 2]).unwrap();
    assert_eq!(page_count(&picked), 3);
    assert!(page_text(&picked, 1).contains("Delta memo"));
    assert!(page_text(&picked, 2).contains("Bravo"));
    assert!(page_text(&picked, 3).contains("Bravo"));
    assert!(matches!(
        select_pages(&merged, &[9]),
        Err(MergeError::PageOutOfRange(9))
    ));
    assert!(
        picked.len() < merged.len(),
        "dropped pages are pruned from the output"
    );
}

#[cfg(feature = "merge")]
#[test]
fn merge_materializes_inherited_page_attributes() {
    // Resources/MediaBox only on the /Pages node: the donor recipe lost them.
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let font = doc.add_object(
        dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" },
    );
    let content = doc.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 12 Tf 50 50 Td (Inherited) Tj ET".to_vec(),
    ));
    let page = doc
        .add_object(dictionary! { "Type" => "Page", "Parent" => pages_id, "Contents" => content });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1,
            "MediaBox" => vec![0.into(), 0.into(), 300.into(), 200.into()],
            "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
        }),
    );
    let cat = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", cat);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();

    let merged = rightkit_pdf::merge::merge_pdfs(&[&bytes, &fixture("memo.pdf")]).unwrap();
    let m = Document::load_mem(&merged).unwrap();
    let first = m.get_pages()[&1];
    let dict = m.get_object(first).unwrap().as_dict().unwrap();
    assert!(dict.has(b"Resources") && dict.has(b"MediaBox"));
    assert!(page_text(&merged, 1).contains("Inherited"));
}

#[cfg(feature = "extract")]
#[test]
fn extract_reads_real_chrome_pdf_per_page() {
    let pages = rightkit_pdf::extract::extract_text_by_pages(&fixture("report.pdf")).unwrap();
    assert_eq!(pages.len(), 3);
    let first_page_text: String = pages[0].split_whitespace().collect();
    assert!(
        first_page_text.contains("Alphasection:revenuegrewbytwelvepercent"),
        "{:?}",
        pages[0]
    );
    assert!(pages[1]
        .split_whitespace()
        .collect::<String>()
        .contains("Bravosection"));
    assert!(pages[2]
        .split_whitespace()
        .collect::<String>()
        .contains("Charliesection"));
    let all = rightkit_pdf::extract::extract_text(&fixture("report.pdf")).unwrap();
    assert!(all.contains("Operations") && all.contains("Outlook"));
    assert!(rightkit_pdf::extract::extract_text(b"%PDF-1.4 not really").is_err());
}

#[cfg(target_os = "macos")]
mod visual {
    use super::*;
    use std::process::Command;

    fn rasterize(pdf: &Path, png: &Path) {
        let out = Command::new("/usr/bin/sips")
            .args([
                "-s",
                "format",
                "png",
                "-s",
                "dpiWidth",
                "72",
                "-s",
                "dpiHeight",
                "72",
            ])
            .arg(pdf)
            .arg("--out")
            .arg(png)
            .output()
            .expect("sips available on macOS");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn decode(png: &Path) -> (usize, usize, Vec<u8>) {
        let img = image::open(png).unwrap().to_rgba8();
        let (width, height) = (img.width() as usize, img.height() as usize);
        let pixels = img.into_raw();
        let gray = pixels
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| {
                let alpha = u16::from(pixel[3]);
                let luminance = (u16::from(pixel[0]) * 77
                    + u16::from(pixel[1]) * 150
                    + u16::from(pixel[2]) * 29)
                    / 256;
                ((luminance * alpha + 255 * (255 - alpha)) / 255) as u8
            })
            .collect();
        (width, height, gray)
    }

    #[test]
    fn added_text_changes_pixels_only_around_the_anchor() {
        let original = fixture("memo.pdf");
        let command = PdfEditCommand::AddText {
            id: "v1".into(),
            page: 1,
            anchor: PdfPoint { x: 72.0, y: 400.0 },
            text: "VISUAL CHECK".into(),
            appearance: appearance_for("source-serif-4"),
        };
        let edited = compose_candidate(&original, &[command], FontCatalog::shared())
            .unwrap()
            .bytes;
        let dir = scratch("visual");
        let (a_pdf, b_pdf) = (dir.join("a.pdf"), dir.join("b.pdf"));
        std::fs::write(&a_pdf, &original).unwrap();
        std::fs::write(&b_pdf, &edited).unwrap();
        let (a_png, b_png) = (dir.join("a.png"), dir.join("b.png"));
        rasterize(&a_pdf, &a_png);
        rasterize(&b_pdf, &b_png);
        let (w, h, a) = decode(&a_png);
        let (w2, h2, b) = decode(&b_png);
        assert_eq!((w, h), (w2, h2));
        // PDF y=400 on a 792pt page sits near the vertical middle; rasterization scale = h/792.
        let scale = h as f64 / 792.0;
        let band_top = ((792.0 - 400.0 - 40.0) * scale) as usize;
        let band_bottom = ((792.0 - 400.0 + 20.0) * scale) as usize;
        let (mut inside, mut outside) = (0_usize, 0_usize);
        for y in 0..h {
            for x in 0..w {
                if a[y * w + x].abs_diff(b[y * w + x]) > 40 {
                    if (band_top..band_bottom).contains(&y) {
                        inside += 1
                    } else {
                        outside += 1
                    }
                }
            }
        }
        assert!(
            inside > 150,
            "added text is visible: {inside} changed px in band; artifacts at {}",
            dir.display()
        );
        assert_eq!(outside, 0, "no pixels changed outside the text band");
    }
}
