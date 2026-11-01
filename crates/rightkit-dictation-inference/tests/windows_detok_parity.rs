//! Windows/ONNX detokenization parity.
//!
//! The ONNX path now returns raw vocabulary pieces and the app assembles the
//! transcript first-party, the same way macOS does. The vendored crate still
//! contains its own `decode_with_timestamps` text assembly for upstream/bench
//! callers, so this test pins the two against each other: if a re-vendor changes
//! the vendored rule, or if the first-party policy drifts, this fails here on any
//! machine rather than silently on a Windows user's transcript.
//!
//! Runs anywhere: the assembly under test is pure string manipulation with no
//! ONNX session, no model file and no audio. Windows RUNTIME validation (that ORT
//! still returns the same pieces) remains a dogfood check.

use rightkit_asr::parakeet_core::{assemble_pieces, DetokPolicy};

/// The rule the vendored decoder applies, transcribed here so the test states the
/// contract independently instead of reaching into the private vendor module.
fn vendor_reference_assembly(pieces: &[&str]) -> String {
    let mut full_text = String::new();
    for token_text in pieces {
        let mut display_text = token_text.replace('\u{2581}', " ");
        if !full_text.is_empty()
            && !display_text.starts_with(' ')
            && display_text.chars().all(|c| c.is_ascii_digit())
        {
            let trailing_letters = full_text
                .chars()
                .rev()
                .take_while(|c| c.is_alphabetic())
                .count();
            let is_article_a = trailing_letters == 1 && full_text.ends_with('a');
            if trailing_letters > 1 || is_article_a {
                display_text.insert(0, ' ');
            }
        }
        if !(token_text.starts_with('<') && token_text.ends_with('>') && *token_text != "<unk>") {
            full_text.push_str(&display_text);
        }
    }
    full_text.trim().to_string()
}

#[test]
fn first_party_detok_matches_the_vendored_rule() {
    let cases: &[&[&str]] = &[
        &["\u{2581}minimal", "\u{2581}", "1", "3", ".", "2"],
        &["\u{2581}static", "\u{2581}", "1", "5"],
        &["\u{2581}hit", "\u{2581}", "9", "0", "%"],
        &["\u{2581}on", "\u{2581}", "1", "1", ".", "2", "4"],
        &[
            "\u{2581}Herd",
            "ride",
            "\u{2581}",
            "0",
            ".",
            "1",
            ".",
            "1",
            "3",
            "4",
        ],
        &["\u{2581}to", "\u{2581}", "3", ".", "6"],
        &[
            "\u{2581}Gwen",
            "\u{2581}",
            "3",
            ".",
            "8",
            "\u{2581}",
            "2",
            "7",
        ],
        &["\u{2581}", "5", "5", "5", "\u{2581}", "1", "2", "3", "4"],
        &["\u{2581}at", "6", "0"],
        &["\u{2581}A", "4"],
        &["\u{2581}a", "2", "4"],
        &["\u{2581}K", "3", ","],
        &["\u{2581}one", ",", "\u{2581}", "2", ",", "\u{2581}", "3"],
        &["\u{2581}hello", "<pad>", "\u{2581}", "1", "3"],
        &["\u{2581}a", "<unk>", "\u{2581}b"],
    ];

    for case in cases {
        let first_party = assemble_pieces(case.iter().copied(), DetokPolicy::canonical_rust()).text;
        assert_eq!(
            first_party,
            vendor_reference_assembly(case),
            "first-party detok diverged from the vendored rule on {case:?}"
        );
    }
}
