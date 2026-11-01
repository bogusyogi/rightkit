use super::{apply_brand_homophones, TEST_BRAND};

fn repair(text: &str) -> String {
    apply_brand_homophones(text, &TEST_BRAND)
}

#[test]
fn repairs_all_positive_forms() {
    // These are all DICTATION PROSE — the user writing *about* the app.
    // Deliberately not "open <app>" phrasing: spoken app-open is a
    // standalone command resolved by app_launch::resolve (live app scan +
    // fuzzy match), a different lane entirely. Mixing command-shaped
    // sentences in here muddies what this repair is responsible for.
    //
    // "her drive" is possessive-ambiguous, so it repairs only with brand
    // evidence in the sentence — "dictation" here.
    assert_eq!(
        repair("her drive handles dictation offline"),
        "HeardRight handles dictation offline"
    );
    assert_eq!(
        repair("switch to herd right please"),
        "switch to HeardRight please"
    );
    assert_eq!(
        repair("we shipped heard right for me"),
        "we shipped HeardRight for me"
    );
    assert_eq!(
        repair("launch heard write today"),
        "launch HeardRight today"
    );
    assert_eq!(
        repair("hurt right ships dictation next week"),
        "HeardRight ships dictation next week"
    );
    assert_eq!(
        repair("her right is the app I use"),
        "HeardRight is the app I use"
    );
    assert_eq!(repair("try heard rite instead"), "try HeardRight instead");
}

#[test]
fn leaves_literal_readings_untouched() {
    // Verb idiom, not the brand.
    assert_eq!(
        repair("I heard right about the meeting"),
        "I heard right about the meeting"
    );
    assert_eq!(repair("you heard right, we won"), "you heard right, we won");
    // Literal cattle sense: "right" as a direction/location adverb.
    assert_eq!(
        repair("the herd right there is grazing"),
        "the herd right there is grazing"
    );
    assert_eq!(repair("the herd right here now"), "the herd right here now");
    // Literal "heard, [now] write it down".
    assert_eq!(
        repair("I heard write it down please"),
        "I heard write it down please"
    );
    assert_eq!(repair("heard write that memo"), "heard write that memo");
}

/// Regression: an earlier draft replaced "her drive", "her right" and
/// "hurt right" unconditionally. Every sentence here is ordinary English
/// with no brand evidence, and every one of them would have been corrupted.
#[test]
fn possessive_ambiguous_forms_need_brand_evidence() {
    for text in [
        "I admired her drive to succeed",
        "her drive was full of photos",
        "she raised her right hand",
        "it is her right to refuse",
        "it hurt right there when I moved",
        "my knee hurt right after the run",
    ] {
        assert_eq!(repair(text), text, "must not rewrite: {text}");
    }
}

/// The real production failure, verbatim from the 2026-07-27 transcript.
/// "launching" and "apps" are the brand evidence that licenses the repair.
#[test]
fn repairs_the_observed_production_sentence() {
    assert_eq!(
        repair("How can we stop her drive from launching at the same time as all the other apps?"),
        "How can we stop HeardRight from launching at the same time as all the other apps?"
    );
}

/// Brand evidence must not leak across a sentence boundary.
#[test]
fn brand_evidence_does_not_cross_sentences() {
    assert_eq!(
        repair("I love this app. I admired her drive."),
        "I love this app. I admired her drive."
    );
}

#[test]
fn mixed_case_and_mid_sentence_placement() {
    assert_eq!(
        repair("Please install HER DRIVE for the demo"),
        "Please install HeardRight for the demo"
    );
    assert_eq!(
        repair("She said the notes on Herd Right were saved"),
        "She said the notes on HeardRight were saved"
    );
    assert_eq!(
        repair("Wait, Heard Write just crashed"),
        "Wait, HeardRight just crashed"
    );
}

#[test]
fn true_brand_and_literal_herd_in_same_sentence() {
    // The literal "herd right there" is grazing (untouched); the true brand
    // mention "her drive" later in the same sentence still repairs, licensed
    // by "dictation".
    assert_eq!(
        repair("the herd right there is grazing, and her drive does dictation"),
        "the herd right there is grazing, and HeardRight does dictation"
    );
}
