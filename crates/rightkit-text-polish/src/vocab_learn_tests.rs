use super::*;

fn s(term: &str, heard_as: &str) -> Suggestion {
    Suggestion {
        term: term.into(),
        heard_as: heard_as.into(),
    }
}

#[test]
fn extracts_proper_noun_corrections_from_edits() {
    let cands = suggestion_candidates(
        "the whisper flow team shipped heardright integration",
        "the Wispr Flow team shipped HeardRight integration",
    );
    assert_eq!(
        cands,
        vec![
            s("Wispr Flow", "whisper flow"),
            s("HeardRight", "heardright")
        ]
    );
    // pure sentence-casing / punctuation edits produce nothing
    assert!(suggestion_candidates("hello there.", "Hello there!").is_empty());
    // common-word capitalization beside an insert is noise (stoplisted)
    assert!(suggestion_candidates("send it monday", "send it on Monday").is_empty());
    // mid-sentence TitleCase on a NON-common word is a name correction
    assert_eq!(
        suggestion_candidates("meet adrian at noon", "meet Adrian at noon"),
        vec![s("Adrian", "adrian")]
    );
    // real correction survives elsewhere in an insert-bearing edit
    assert_eq!(
        suggestion_candidates(
            "send the whisper flow doc monday",
            "send the Wispr Flow doc on Monday"
        ),
        vec![s("Wispr Flow", "whisper flow")]
    );
    // long rewrites (different content) produce nothing
    assert!(suggestion_candidates("send it monday", "totally different text").is_empty());
}

#[test]
fn boundary_and_adversarial_cases() {
    // insert adjacent to a MixedCase fix still surfaces the fix
    assert_eq!(
        suggestion_candidates("the heardright app", "the new HeardRight app"),
        vec![s("HeardRight", "heardright")]
    );
    // CaseFix-only run emits per token: "wispr" is a non-stoplisted TitleCase
    // brand (suggests), "flow" is stoplisted (does not) — so only "Wispr".
    assert_eq!(
        suggestion_candidates("the wispr flow rocks", "the Wispr Flow rocks"),
        vec![s("Wispr", "wispr")]
    );
    // trailing-edge (last token) correction
    assert_eq!(
        suggestion_candidates("we use scrapegraph", "we use ScrapeGraph"),
        vec![s("ScrapeGraph", "scrapegraph")]
    );
    // leading-edge (first token) correction
    assert_eq!(
        suggestion_candidates("mailright is fast", "MailRight is fast"),
        vec![s("MailRight", "mailright")]
    );
    // stoplisted brand word (single, lowercased common) never suggests
    assert!(suggestion_candidates("use whisper for asr", "use Whisper for asr").is_empty());
    assert!(suggestion_candidates("cut me some slack", "cut me some Slack").is_empty());
    // sentence-initial TitleCase is not a correction
    assert!(suggestion_candidates("monday works", "Monday works").is_empty());
    // punctuation-only difference is not a correction
    assert!(suggestion_candidates("ship it", "ship it.").is_empty());
    // deletion adjacent to a replace: replace still surfaces, deletion ignored
    assert_eq!(
        suggestion_candidates("ping the whisper flow please", "ping the Wispr Flow"),
        vec![s("Wispr Flow", "whisper flow")]
    );
    // pure insertion produces nothing (added words are not corrections)
    assert!(suggestion_candidates("ship it friday", "please ship it on friday").is_empty());
    // pure deletion produces nothing
    assert!(suggestion_candidates("please ship it now", "ship it now").is_empty());
    // empty inputs are safe
    assert!(suggestion_candidates("", "anything").is_empty());
    assert!(suggestion_candidates("anything", "").is_empty());
    // unicode words don't panic and casing fix still works
    assert_eq!(
        suggestion_candidates("café meeting with adrian", "café meeting with Adrian"),
        vec![s("Adrian", "adrian")]
    );
    // sound-alike phrase (edit distance within budget) surfaces
    assert_eq!(
        suggestion_candidates("book the squarespace site", "book the Squarespace site"),
        vec![s("Squarespace", "squarespace")]
    );
    // 3-token replace run cap: a 4-token contiguous replace is over budget → dropped,
    // but here it also trips the rewrite guard (all content changed)
    assert!(suggestion_candidates("aaa bbb ccc ddd", "Www Xxx Yyy Zzz").is_empty());
    // rewrite guard: >40% of tokens changed → nothing
    assert!(suggestion_candidates("one two three four five", "one two three Foo Bar").is_empty());
}
