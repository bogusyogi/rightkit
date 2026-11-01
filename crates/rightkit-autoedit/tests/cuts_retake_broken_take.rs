//! Broken-take scenarios (#96): a later take that stops mid-phrase is suggested for
//! review and is never cut automatically. A take that ends a sentence stays auto-cuttable.

use rightkit_autoedit::cuts::{is_broken_take, take_tier, Tier};
use rightkit_transcript::{EndSource, Word};

fn word(text: &str, start_ms: u64) -> Word {
    Word {
        text: text.into(),
        start_ms,
        end_ms: start_ms + 300,
        confidence: None,
        end_source: EndSource::Model,
    }
}

/// Earlier complete attempt ("we ship on friday.") then a later take that stops
/// mid-phrase ("we ship on monday").
fn transcript() -> Vec<Word> {
    ["we", "ship", "on", "friday.", "we", "ship", "on", "monday"]
        .iter()
        .enumerate()
        .map(|(i, t)| word(t, i as u64 * 300))
        .collect()
}

#[test]
fn complete_earlier_take_is_auto_cuttable() {
    let words = transcript();
    assert!(!is_broken_take(&words, 0..4));
    assert_eq!(take_tier(&words, 0..4), Tier::Auto);
}

#[test]
fn broken_later_take_is_suggested_not_auto_cut() {
    let words = transcript();
    assert!(is_broken_take(&words, 4..8));
    assert_eq!(take_tier(&words, 4..8), Tier::Suggest);
}

#[test]
fn empty_or_out_of_range_take_is_suggested() {
    let words = transcript();
    assert_eq!(take_tier(&words, 0..0), Tier::Suggest);
    assert_eq!(take_tier(&words, 4..20), Tier::Suggest);
}
