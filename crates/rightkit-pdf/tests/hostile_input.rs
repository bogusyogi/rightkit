//! Hostile-input corpus for inspection (findings 1 and 9 of the 2026-10-11 Astra read).
//!
//! Every case is a tiny PDF built in memory whose numbers lie about the file (CID widths up to
//! `u32::MAX`, `/LastChar` of `i64::MAX`, a page tree that loops, a `bfrange` that overflows).
//! Each runs on a worker thread under a wall-clock guard, so a regression fails the test
//! (panic, or no answer within the guard) instead of hanging the suite or exhausting memory.
//! A typed error or a degraded graph are both acceptable answers; a panic or a hang is not.
//!
//! Run in the default (debug) profile on purpose: overflow checks turn silent wraparound into a
//! panic here.

use std::sync::mpsc;
use std::time::Duration;

use lopdf::{dictionary, Dictionary, Document, Object, ObjectId, Stream};
use rightkit_pdf::inspect::page_graph_from_bytes;

/// Wall-clock guard for one case. Healthy cases finish in milliseconds.
const GUARD: Duration = Duration::from_secs(20);

/// Runs `work` on a worker thread; fails on a panic or when it does not finish within [`GUARD`].
fn guarded<T: Send + 'static>(label: &str, work: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::Builder::new()
        .name(label.to_string())
        // Small stack: a recursion bug must fail, not hide behind a large stack.
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let _ = tx.send(work());
        })
        .expect("spawn worker");
    match rx.recv_timeout(GUARD) {
        Ok(value) => {
            handle.join().expect("worker finished");
            value
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            let panic = handle.join().expect_err("worker dropped its channel by panicking");
            let message = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            panic!("{label}: inspection panicked: {message}");
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("{label}: no answer within {GUARD:?} (unbounded loop or allocation)")
        }
    }
}

fn save(mut doc: Document) -> Vec<u8> {
    let mut out = Vec::new();
    doc.save_to(&mut out).expect("fixture saves");
    out
}

/// One page, resources `/Font << /F1 font >>`, one content stream. `page_extra` lets a case add
/// or override page-dictionary entries (and `omit_media_box` drops the box).
fn one_page(
    font: impl FnOnce(&mut Document) -> ObjectId,
    content: &[u8],
    omit_media_box: bool,
    page_extra: impl FnOnce(&mut Document, ObjectId, &mut Dictionary),
) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let font_id = font(&mut doc);
    let resources = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font_id } });
    let contents = doc.add_object(Stream::new(dictionary! {}, content.to_vec()));
    let page_id = doc.new_object_id();
    let mut page = dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => contents,
        "Resources" => resources,
    };
    if !omit_media_box {
        page.set(
            "MediaBox",
            Object::Array(vec![0.into(), 0.into(), 612.into(), 792.into()]),
        );
    }
    page_extra(&mut doc, pages_id, &mut page);
    doc.objects.insert(page_id, Object::Dictionary(page));
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
        }),
    );
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog);
    save(doc)
}

/// An `Identity-H` Type0 font: descendant `/W` array as given, `ToUnicode` source as given.
fn identity_h(doc: &mut Document, w: Vec<Object>, to_unicode: &str) -> ObjectId {
    let descendant = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "CIDFontType2",
        "BaseFont" => "ABCDEF+Cid",
        "DW" => 1000,
        "W" => Object::Array(w),
        "CIDToGIDMap" => "Identity",
    });
    let cmap = doc.add_object(Stream::new(dictionary! {}, to_unicode.as_bytes().to_vec()));
    doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => "ABCDEF+Cid",
        "Encoding" => "Identity-H",
        "DescendantFonts" => vec![descendant.into()],
        "ToUnicode" => cmap,
    })
}

fn cmap(body: &str) -> String {
    format!(
        "/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
         1 begincodespacerange <0000> <FFFF> endcodespacerange\n{body}\nendcmap end end\n"
    )
}

const CID_TEXT: &[u8] = b"BT /F1 12 Tf 72 700 Td <00010002> Tj ET";

fn inspect_guarded(label: &str, pdf: Vec<u8>) {
    guarded(label, move || {
        // Either answer is acceptable; the graph for such a file is allowed to degrade.
        let _ = page_graph_from_bytes(&pdf, 1);
    });
}

// -------------------------------------------------------------------------------------------
// Finding 1: CID /W ranges and arrays
// -------------------------------------------------------------------------------------------

#[test]
fn cid_w_range_ending_at_u32_max_does_not_overflow_or_fill_the_map() {
    // `c += 1` past u32::MAX panicked (checked build) or wrapped to 0 and refilled 4 billion
    // entries (release).
    let pdf = one_page(
        |d| {
            identity_h(
                d,
                vec![4294967295i64.into(), 4294967295i64.into(), 500.into()],
                &cmap("1 beginbfchar\n<0001> <0041>\nendbfchar"),
            )
        },
        CID_TEXT,
        false,
        |_, _, _| {},
    );
    inspect_guarded("w_range_u32_max", pdf);
}

#[test]
fn cid_w_array_starting_at_u32_max_does_not_overflow() {
    let pdf = one_page(
        |d| {
            identity_h(
                d,
                vec![
                    4294967295i64.into(),
                    Object::Array(vec![500.into(), 500.into()]),
                ],
                &cmap("1 beginbfchar\n<0001> <0041>\nendbfchar"),
            )
        },
        CID_TEXT,
        false,
        |_, _, _| {},
    );
    inspect_guarded("w_array_u32_max", pdf);
}

#[test]
fn cid_w_range_covering_the_whole_u32_space_is_bounded() {
    // A 4-billion-entry range must not allocate 4 billion map entries.
    let pdf = one_page(
        |d| {
            identity_h(
                d,
                vec![0.into(), 4294967294i64.into(), 500.into()],
                &cmap("1 beginbfchar\n<0001> <0041>\n<0002> <0042>\nendbfchar"),
            )
        },
        CID_TEXT,
        false,
        |_, _, _| {},
    );
    inspect_guarded("w_range_whole_space", pdf);
}

#[test]
fn cid_w_with_negative_and_fractional_starts_is_skipped_not_wrapped() {
    let pdf = one_page(
        |d| {
            identity_h(
                d,
                vec![
                    (-5i64).into(),
                    10.into(),
                    500.into(),
                    Object::Real(1e30),
                    Object::Real(f32::NAN),
                    500.into(),
                ],
                &cmap("1 beginbfchar\n<0001> <0041>\nendbfchar"),
            )
        },
        CID_TEXT,
        false,
        |_, _, _| {},
    );
    inspect_guarded("w_negative_fractional", pdf);
}

// -------------------------------------------------------------------------------------------
// Finding 1: ToUnicode ranges
// -------------------------------------------------------------------------------------------

#[test]
fn bfrange_destination_overflowing_base_plus_offset_is_skipped() {
    let pdf = one_page(
        |d| {
            identity_h(
                d,
                vec![1.into(), Object::Array(vec![500.into(), 500.into()])],
                &cmap("1 beginbfrange\n<0001> <0002> <FFFFFFFF>\nendbfrange"),
            )
        },
        CID_TEXT,
        false,
        |_, _, _| {},
    );
    inspect_guarded("bfrange_dest_overflow", pdf);
}

#[test]
fn bfrange_source_near_u32_max_does_not_overflow() {
    let pdf = one_page(
        |d| {
            identity_h(
                d,
                vec![1.into(), Object::Array(vec![500.into(), 500.into()])],
                &cmap("1 beginbfrange\n<FFFFFFF0> <FFFFFFFF> <0041>\nendbfrange"),
            )
        },
        CID_TEXT,
        false,
        |_, _, _| {},
    );
    inspect_guarded("bfrange_src_u32_max", pdf);
}

#[test]
fn many_full_width_bfranges_are_capped_not_multiplied() {
    // 40 000 ranges of 65 536 codes each would be 2.6 billion map insertions from a 1 MB stream.
    let mut body = String::from("40000 beginbfrange\n");
    for _ in 0..40_000 {
        body.push_str("<0000> <FFFF> <0000>\n");
    }
    body.push_str("endbfrange");
    let pdf = one_page(
        |d| {
            identity_h(
                d,
                vec![1.into(), Object::Array(vec![500.into(), 500.into()])],
                &cmap(&body),
            )
        },
        CID_TEXT,
        false,
        |_, _, _| {},
    );
    inspect_guarded("bfrange_flood", pdf);
}

#[test]
fn bfrange_array_form_with_unbalanced_brackets_does_not_hang() {
    let pdf = one_page(
        |d| {
            identity_h(
                d,
                vec![1.into(), Object::Array(vec![500.into(), 500.into()])],
                &cmap("1 beginbfrange\n<0001> <0003> [<0041> <0042>\nendbfrange"),
            )
        },
        CID_TEXT,
        false,
        |_, _, _| {},
    );
    inspect_guarded("bfrange_unbalanced", pdf);
}

// -------------------------------------------------------------------------------------------
// Finding 9: /FirstChar../LastChar and inherited page attributes
// -------------------------------------------------------------------------------------------

fn simple_font(doc: &mut Document, first: i64, last: i64, widths: Vec<Object>) -> ObjectId {
    doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
        "FirstChar" => first,
        "LastChar" => last,
        "Widths" => Object::Array(widths),
    })
}

#[test]
fn lastchar_of_i64_max_with_one_width_is_bounded_by_the_widths_array() {
    // ~2^63 iterations for one width.
    let pdf = one_page(
        |d| simple_font(d, 0, i64::MAX, vec![500.into()]),
        b"BT /F1 12 Tf 72 700 Td (A) Tj ET",
        false,
        |_, _, _| {},
    );
    inspect_guarded("lastchar_i64_max", pdf);
}

#[test]
fn firstchar_near_i64_max_and_negative_ranges_do_not_overflow() {
    for (first, last) in [
        (i64::MAX - 1, i64::MAX),
        (i64::MIN, i64::MAX),
        (-3, 3),
        (300, 100_000),
    ] {
        let pdf = one_page(
            |d| simple_font(d, first, last, vec![500.into(), 500.into(), 500.into()]),
            b"BT /F1 12 Tf 72 700 Td (A) Tj ET",
            false,
            |_, _, _| {},
        );
        inspect_guarded(&format!("firstchar_{first}_{last}"), pdf);
    }
}

#[test]
fn page_whose_parent_is_itself_does_not_loop_looking_for_an_inherited_box() {
    // No /MediaBox or /CropBox anywhere: the inherited lookup must give up, not circle.
    let pdf = one_page(
        |d| simple_font(d, 32, 126, vec![500.into(); 95]),
        b"BT /F1 12 Tf 72 700 Td (A) Tj ET",
        true,
        |_, _, _| {},
    );
    // Re-parse and point the page's Parent at the page itself (the /Pages node stays reachable
    // through /Kids).
    let mut doc = Document::load_mem(&pdf).expect("fixture reloads");
    let page_id = *doc.get_pages().get(&1).expect("page 1");
    if let Ok(Object::Dictionary(page)) = doc.get_object_mut(page_id) {
        page.set("Parent", Object::Reference(page_id));
    }
    inspect_guarded("self_parent", save(doc));
}

#[test]
fn two_node_parent_cycle_does_not_loop() {
    let mut doc = Document::load_mem(&one_page(
        |d| simple_font(d, 32, 126, vec![500.into(); 95]),
        b"BT /F1 12 Tf 72 700 Td (A) Tj ET",
        true,
        |_, _, _| {},
    ))
    .expect("fixture reloads");
    let page_id = *doc.get_pages().get(&1).expect("page 1");
    let pages_id = match doc.get_object(page_id) {
        Ok(Object::Dictionary(page)) => page.get(b"Parent").and_then(|p| p.as_reference()).unwrap(),
        _ => panic!("page dict"),
    };
    // /Pages -> page -> /Pages ...
    if let Ok(Object::Dictionary(pages)) = doc.get_object_mut(pages_id) {
        pages.set("Parent", Object::Reference(page_id));
    }
    inspect_guarded("two_node_cycle", save(doc));
}

#[test]
fn very_deep_parent_chain_is_cut_off_by_the_depth_cap() {
    let mut doc = Document::load_mem(&one_page(
        |d| simple_font(d, 32, 126, vec![500.into(); 95]),
        b"BT /F1 12 Tf 72 700 Td (A) Tj ET",
        true,
        |_, _, _| {},
    ))
    .expect("fixture reloads");
    let page_id = *doc.get_pages().get(&1).expect("page 1");
    // A 20 000-deep straight chain of /Pages nodes above the page.
    let mut parent = doc.add_object(dictionary! { "Type" => "Pages", "Kids" => Vec::<Object>::new() });
    let top = parent;
    for _ in 0..20_000 {
        parent = doc.add_object(dictionary! { "Type" => "Pages", "Parent" => parent });
    }
    if let Ok(Object::Dictionary(page)) = doc.get_object_mut(page_id) {
        page.set("Parent", Object::Reference(parent));
    }
    let _ = top;
    inspect_guarded("deep_parent_chain", save(doc));
}

// -------------------------------------------------------------------------------------------
// Sanity: the guard itself fails loudly
// -------------------------------------------------------------------------------------------

#[test]
#[should_panic(expected = "inspection panicked")]
fn the_guard_reports_a_worker_panic() {
    guarded("selftest_panic", || {
        let _ = std::hint::black_box(u32::MAX) + std::hint::black_box(1u32);
    });
}
