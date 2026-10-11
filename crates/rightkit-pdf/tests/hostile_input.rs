//! Hostile-input corpus for inspection (findings 1 and 9 of the 2026-10-11 Astra read; findings 1
//! and 8 of its second read, and the graphics-state stack bound that came with finding 3).
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
use rightkit_pdf::model::PdfEditBlocker;

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
            let panic = handle
                .join()
                .expect_err("worker dropped its channel by panicking");
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
    let mut parent =
        doc.add_object(dictionary! { "Type" => "Pages", "Kids" => Vec::<Object>::new() });
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
// Second read, finding 1: TJ gap provenance
// -------------------------------------------------------------------------------------------

#[test]
fn tj_array_of_50_000_empty_strings_between_numbers_retains_linear_state() {
    // `[(A) 0 () 0 () ... (B)] TJ`: every empty operand used to clone the numbers accumulated so
    // far into the spacing map - about 1.25 billion entries (20 GB) for 50 000 of them.
    let mut content = String::from("BT /F1 12 Tf 72 700 Td [(A)");
    for _ in 0..50_000 {
        content.push_str(" 0 ()");
    }
    content.push_str(" (B)] TJ ET");
    let pdf = one_page(
        |d| simple_font(d, 32, 126, vec![500.into(); 95]),
        content.as_bytes(),
        false,
        |_, _, _| {},
    );
    let texts = guarded("tj_empty_strings", move || {
        page_graph_from_bytes(&pdf, 1)
            .map(|graph| {
                graph
                    .islands
                    .into_iter()
                    .map(|island| island.text)
                    .collect::<Vec<_>>()
            })
            .map_err(|error| error.to_string())
    })
    .expect("a valid page inspects");
    assert!(texts.iter().any(|text| text == "AB"), "{texts:?}");
}

#[test]
fn tj_array_of_empty_strings_with_real_adjustments_is_linear_too() {
    // Non-zero adjustments are the gap provenance the composer rewrites; they must survive the
    // empty operands (one gap, many numbers) without being copied once per operand.
    let mut content = String::from("BT /F1 12 Tf 72 700 Td [(A)");
    for _ in 0..50_000 {
        content.push_str(" -1 ()");
    }
    content.push_str(" (B)] TJ ET");
    let pdf = one_page(
        |d| simple_font(d, 32, 126, vec![500.into(); 95]),
        content.as_bytes(),
        false,
        |_, _, _| {},
    );
    inspect_guarded("tj_empty_strings_adjusted", pdf);
}

// -------------------------------------------------------------------------------------------
// Second read, finding 8: total /W work across ranges
// -------------------------------------------------------------------------------------------

#[test]
fn many_repetitions_of_a_full_width_w_range_hit_a_total_work_budget() {
    // `0 65535 500` x 40 000 is 2.6 billion insertions if each range is only clamped. The font
    // is still read (what fit the budget is a valid table) but is not editable.
    let mut w: Vec<Object> = Vec::with_capacity(120_000);
    for _ in 0..40_000 {
        w.push(0.into());
        w.push(65535.into());
        w.push(500.into());
    }
    let pdf = one_page(
        |d| {
            identity_h(
                d,
                w,
                &cmap("2 beginbfchar\n<0001> <0041>\n<0002> <0042>\nendbfchar"),
            )
        },
        CID_TEXT,
        false,
        |_, _, _| {},
    );
    let islands = guarded("w_range_repetition", move || {
        page_graph_from_bytes(&pdf, 1)
            .map(|graph| {
                graph
                    .islands
                    .into_iter()
                    .map(|island| (island.text, island.editable, island.edit_blocker))
                    .collect::<Vec<_>>()
            })
            .map_err(|error| error.to_string())
    })
    .expect("a valid page inspects");
    assert_eq!(
        islands,
        vec![(
            "AB".to_string(),
            false,
            Some(PdfEditBlocker::MissingMetrics)
        )]
    );
}

#[test]
fn many_repetitions_of_a_full_width_w_array_hit_the_same_budget() {
    // 18 x `0 [500 x 60000]` is 1 080 000 assignments: just over the 2^20 budget, which is what
    // the test needs to prove. The cost of this test is the file itself: lopdf parses every
    // width before the reader can charge it, so fewer entries than the old 40 repetitions keep
    // the case well inside the guard.
    let mut w: Vec<Object> = Vec::with_capacity(36);
    for _ in 0..18 {
        w.push(0.into());
        w.push(Object::Array(vec![500.into(); 60_000]));
    }
    let pdf = one_page(
        |d| identity_h(d, w, &cmap("1 beginbfchar\n<0001> <0041>\nendbfchar")),
        CID_TEXT,
        false,
        |_, _, _| {},
    );
    inspect_guarded("w_array_repetition", pdf);
}

// -------------------------------------------------------------------------------------------
// Graphics-state stack bound (the `q` stack now carries the whole text state)
// -------------------------------------------------------------------------------------------

#[test]
fn hundreds_of_thousands_of_unbalanced_q_operators_are_refused_not_stacked() {
    let content = "q ".repeat(300_000);
    let pdf = one_page(
        |d| simple_font(d, 32, 126, vec![500.into(); 95]),
        content.as_bytes(),
        false,
        |_, _, _| {},
    );
    let outcome = guarded("q_flood", move || {
        page_graph_from_bytes(&pdf, 1)
            .map(|graph| graph.islands.len())
            .map_err(|error| error.to_string())
    });
    assert!(outcome.is_err(), "{outcome:?}");
}

// -------------------------------------------------------------------------------------------
// Decoded Unicode is bounded (third Astra read, finding 1)
// -------------------------------------------------------------------------------------------

/// Hex for `chars` repetitions of 'A' as UTF-16BE.
fn a_run_hex(chars: usize) -> String {
    "0041".repeat(chars)
}

fn cid_one_shown(times: usize, destination_chars: usize) -> Vec<u8> {
    let map = cmap(&format!(
        "1 beginbfchar\n<0001> <{}>\nendbfchar",
        a_run_hex(destination_chars)
    ));
    let content = format!("BT /F1 12 Tf 72 700 Td <{}> Tj ET", "0001".repeat(times));
    one_page(
        |d| identity_h(d, vec![], &map),
        content.as_bytes(),
        false,
        |_, _, _| {},
    )
}

#[test]
fn a_megabyte_cmap_destination_shown_fifty_thousand_times_does_not_allocate_gigabytes() {
    // 1 MiB of 'A' behind one code, shown 50 000 times: 48 GiB if every glyph clones it.
    let pdf = cid_one_shown(50_000, 1 << 20);
    let outcome = guarded("giant_destination", move || {
        page_graph_from_bytes(&pdf, 1).map(|graph| {
            graph
                .islands
                .iter()
                .map(|island| island.text.len())
                .sum::<usize>()
        })
    });
    // The over-long destination is not read, so nothing large is ever decoded.
    if let Ok(total) = outcome {
        assert!(total < 1 << 20, "{total}");
    }
}

#[test]
fn many_glyphs_each_with_a_legal_long_destination_hit_the_aggregate_decode_budget() {
    // 60 characters is a legal destination; 200 000 glyphs of it is 12 MB, past the page budget.
    let pdf = cid_one_shown(200_000, 60);
    let outcome = guarded("aggregate_budget", move || {
        page_graph_from_bytes(&pdf, 1)
            .map(|graph| graph.islands.len())
            .map_err(|error| error.code())
    });
    assert_eq!(outcome, Err("text_overflow"), "{outcome:?}");
}

#[test]
fn ordinary_text_is_unaffected_by_the_decode_budget() {
    let pdf = cid_one_shown(1_000, 1);
    let outcome = guarded("ordinary_text", move || {
        page_graph_from_bytes(&pdf, 1).map(|graph| graph.islands.len())
    });
    assert!(outcome.is_ok(), "{outcome:?}");
}

// -------------------------------------------------------------------------------------------
// Resources are charged before they are allocated (fourth Astra read, findings 10 and 11)
// -------------------------------------------------------------------------------------------

/// A page whose `/Contents` array lists one compressed stream of `decoded_bytes` spaces `times`
/// times. The file is tiny; the content it expands to is `decoded_bytes * times`.
fn repeated_content_reference(decoded_bytes: usize, times: usize) -> Vec<u8> {
    one_page(
        |d| simple_font(d, 32, 126, vec![500.into(); 95]),
        b"",
        false,
        move |doc, _, page| {
            let mut stream = Stream::new(dictionary! {}, vec![b' '; decoded_bytes]);
            let _ = stream.compress();
            let id = doc.add_object(stream);
            page.set(
                "Contents",
                Object::Array((0..times).map(|_| Object::Reference(id)).collect()),
            );
        },
    )
}

#[test]
fn one_compressed_stream_listed_a_thousand_times_is_refused_not_decoded_a_thousand_times() {
    // 36 MiB listed 1 024 times is 36 GiB if every listing is decoded and retained. The page
    // budget lets the first listing through and refuses the second, so memory stays near one
    // stream however many times it is listed.
    let pdf = repeated_content_reference(36 << 20, 1_024);
    let outcome = guarded("repeated_content_reference", move || {
        page_graph_from_bytes(&pdf, 1)
            .map(|graph| graph.islands.len())
            .map_err(|error| error.code())
    });
    assert!(outcome.is_err(), "{outcome:?}");
}

#[test]
fn a_stream_listed_a_few_times_within_the_budget_is_still_read() {
    let pdf = repeated_content_reference(1 << 20, 4);
    let outcome = guarded("repeated_within_budget", move || {
        page_graph_from_bytes(&pdf, 1).map(|graph| graph.islands.len())
    });
    assert_eq!(outcome.ok(), Some(0));
}

#[test]
fn a_single_show_operand_past_the_glyph_cap_is_refused_before_its_glyphs_are_reserved() {
    // 3 million single-byte codes in one `Tj`: past the two-million-glyph page cap, which used to
    // be checked only after `bytes.len() / width` complete glyph records had been reserved.
    let content = format!("BT /F1 12 Tf 72 700 Td ({}) Tj ET", "A".repeat(3_000_000));
    let pdf = one_page(
        |d| simple_font(d, 32, 126, vec![500.into(); 95]),
        content.as_bytes(),
        false,
        |_, _, _| {},
    );
    let outcome = guarded("giant_operand", move || {
        page_graph_from_bytes(&pdf, 1)
            .map(|graph| graph.islands.len())
            .map_err(|error| error.code())
    });
    assert_eq!(outcome, Err("text_overflow"), "{outcome:?}");
}

#[test]
fn nine_million_empty_strings_in_one_array_are_refused_before_they_are_parsed() {
    // `[()()()...] TJ`: no regular characters, so a byte count of operators misses it; every `()`
    // is still an object the parser would build.
    let content = format!("BT /F1 12 Tf 72 700 Td [{}] TJ ET", "()".repeat(9_000_000));
    let pdf = one_page(
        |d| simple_font(d, 32, 126, vec![500.into(); 95]),
        content.as_bytes(),
        false,
        |_, _, _| {},
    );
    let outcome = guarded("string_flood", move || {
        page_graph_from_bytes(&pdf, 1)
            .map(|graph| graph.islands.len())
            .map_err(|error| error.code())
    });
    assert!(outcome.is_err(), "{outcome:?}");
}

#[test]
fn nine_million_operands_are_refused_before_they_are_parsed() {
    let content = format!("{}Tj", "1 ".repeat(9_000_000));
    let pdf = one_page(
        |d| simple_font(d, 32, 126, vec![500.into(); 95]),
        content.as_bytes(),
        false,
        |_, _, _| {},
    );
    let outcome = guarded("operand_flood", move || {
        page_graph_from_bytes(&pdf, 1).map(|graph| graph.islands.len())
    });
    assert!(outcome.is_err(), "{outcome:?}");
}

#[test]
fn six_hundred_fonts_naming_one_large_cmap_share_one_page_budget() {
    // Each font names the same 4 MiB (decoded) CMap. Without a page-wide budget every font decodes
    // its own copy; with one, the first few take the budget and the rest are built inexact.
    let pdf = one_page(
        |d| identity_h(d, vec![], &cmap("")),
        b"BT /F1 12 Tf 72 700 Td <00010002> Tj ET",
        false,
        |doc, _, page| {
            let mut stream = Stream::new(
                dictionary! {},
                format!("{}{}", cmap(""), " ".repeat(4 << 20)).into_bytes(),
            );
            let _ = stream.compress();
            let shared = doc.add_object(stream);
            let mut fonts = Dictionary::new();
            for n in 0..600 {
                let descendant = doc.add_object(dictionary! {
                    "Type" => "Font",
                    "Subtype" => "CIDFontType2",
                    "BaseFont" => "X",
                    "DW" => 1000,
                    "CIDToGIDMap" => "Identity",
                });
                let font = doc.add_object(dictionary! {
                    "Type" => "Font",
                    "Subtype" => "Type0",
                    "BaseFont" => "X",
                    "Encoding" => "Identity-H",
                    "DescendantFonts" => vec![descendant.into()],
                    "ToUnicode" => shared,
                });
                fonts.set(format!("F{n}"), font);
            }
            page.set(
                "Resources",
                Object::Dictionary(dictionary! { "Font" => Object::Dictionary(fonts) }),
            );
        },
    );
    inspect_guarded("font_flood", pdf);
}

#[test]
fn adjacent_operands_and_operators_are_refused_before_they_are_parsed() {
    // `0q0q0q...` is an operand and an operator per two bytes with nothing between them, so a count
    // of whitespace-separated runs sees one token for the whole stream. The page budget charges the
    // bytes, which cannot undercount: each pattern is a little over the 2^23 a page may parse.
    for pattern in ["0q", "-1q", "[0]Q"] {
        let content = pattern.repeat((1 << 23) / pattern.len() + 1);
        let pdf = one_page(
            |d| simple_font(d, 32, 126, vec![500.into(); 95]),
            content.as_bytes(),
            false,
            |_, _, _| {},
        );
        let outcome = guarded("adjacent_flood", move || {
            page_graph_from_bytes(&pdf, 1)
                .map(|graph| graph.islands.len())
                .map_err(|error| error.code())
        });
        assert_eq!(outcome, Err("malformed_content"), "{pattern}: {outcome:?}");
    }
}

// -------------------------------------------------------------------------------------------
// Inherited page attributes are shared, not copied per page (fifth Astra read, finding 15)
// -------------------------------------------------------------------------------------------

/// `pages` pages under one `/Pages` node that holds a direct `/Resources` of `names` XObjects (each
/// a nested dictionary with an array), a direct `/MediaBox` and a `/Rotate`. The first page has a
/// `/Resources` of its own. Counted shallowly the node's `/Resources` is one entry; copied, it is
/// `pages * names * 3` of them.
#[cfg(feature = "merge")]
fn pages_inheriting_nested_resources(pages: usize, names: usize) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let mut xobjects = Dictionary::new();
    for n in 0..names {
        xobjects.set(
            format!("X{n}"),
            Object::Dictionary(dictionary! {
                "Subtype" => "Form",
                "BBox" => Object::Array(vec![
                    Object::Integer(0),
                    Object::Integer(0),
                    Object::Integer(1),
                    Object::Integer(1),
                ]),
            }),
        );
    }
    let mut kids = Vec::new();
    for n in 0..pages {
        let mut page = dictionary! { "Type" => "Page", "Parent" => pages_id };
        if n == 0 {
            page.set("Resources", Object::Dictionary(Dictionary::new()));
        }
        kids.push(Object::Reference(doc.add_object(page)));
    }
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => Object::Array(kids),
            "Count" => Object::Integer(pages as i64),
            "MediaBox" => Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(612),
                Object::Integer(792),
            ]),
            "Rotate" => Object::Integer(90),
            "Resources" => dictionary! { "XObject" => Object::Dictionary(xobjects) },
        }),
    );
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog);
    save(doc)
}

#[cfg(feature = "merge")]
#[test]
fn a_large_inherited_resource_dictionary_is_shared_by_reference_not_cloned_per_page() {
    use std::collections::BTreeSet;

    use rightkit_pdf::merge::merge_pdfs;

    let input = pages_inheriting_nested_resources(400, 2_000);
    let merged = guarded("merge_inherited", {
        let input = input.clone();
        move || merge_pdfs(&[&input])
    })
    .expect("merges");
    // One copy of the 2 000 entries, not 399 more: copied per page the output would be about
    // 400 times the input.
    assert!(
        merged.len() < input.len() * 2,
        "{} bytes from {}",
        merged.len(),
        input.len()
    );

    let doc = Document::load_mem(&merged).expect("reloads");
    let pages = doc.get_pages();
    assert_eq!(pages.len(), 400);
    let (mut resources, mut boxes) = (BTreeSet::new(), BTreeSet::new());
    for (number, id) in &pages {
        let page = doc.get_object(*id).expect("page").as_dict().expect("dict");
        if *number == 1 {
            // A page's own value is left alone.
            assert!(
                matches!(page.get(b"Resources"), Ok(Object::Dictionary(own)) if own.is_empty()),
                "{:?}",
                page.get(b"Resources")
            );
        } else {
            match page.get(b"Resources").expect("resources") {
                Object::Reference(target) => {
                    resources.insert(*target);
                }
                other => panic!("page {number}: copied onto the page: {other:?}"),
            }
        }
        match page.get(b"MediaBox").expect("box") {
            Object::Reference(target) => {
                boxes.insert(*target);
            }
            other => panic!("page {number}: box copied onto the page: {other:?}"),
        }
        // A number costs nothing to copy and stays direct.
        assert!(matches!(page.get(b"Rotate"), Ok(Object::Integer(90))));
    }
    assert_eq!(resources.len(), 1, "one shared /Resources");
    assert_eq!(boxes.len(), 1, "one shared /MediaBox");

    // And it is the whole dictionary, nested content included.
    let shared = *resources.iter().next().expect("one");
    let shared = doc
        .get_object(shared)
        .expect("shared")
        .as_dict()
        .expect("dict");
    let xobjects = shared
        .get(b"XObject")
        .expect("xobject")
        .as_dict()
        .expect("dict");
    assert_eq!(xobjects.len(), 2_000);
    let first = xobjects
        .get(b"X0")
        .expect("X0")
        .as_dict()
        .expect("form")
        .get(b"BBox")
        .expect("bbox")
        .as_array()
        .expect("array");
    assert_eq!(first.len(), 4);
}

#[cfg(feature = "merge")]
#[test]
fn selecting_one_page_of_many_does_not_materialize_a_copy_per_page() {
    use rightkit_pdf::merge::select_pages;

    let input = pages_inheriting_nested_resources(400, 2_000);
    let picked = guarded("select_inherited", {
        let input = input.clone();
        move || select_pages(&input, &[2])
    })
    .expect("selects");
    assert!(
        picked.len() < input.len() * 2,
        "{} bytes from {}",
        picked.len(),
        input.len()
    );
    let doc = Document::load_mem(&picked).expect("reloads");
    assert_eq!(doc.get_pages().len(), 1);
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
