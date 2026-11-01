use super::*;

fn editor_fingerprint_fixture(description: Option<&str>, x: i64, y: i64) -> EditorFingerprint {
    EditorFingerprint {
        role: "AXTextArea".to_string(),
        subrole: None,
        identifier: None,
        description: description.map(str::to_string),
        x,
        y,
        width: 718,
    }
}

#[test]
fn logical_editor_replacement_requires_semantics_and_geometry() {
    let expected = editor_fingerprint_fixture(Some("Prompt"), 488, 754);
    assert!(expected.logically_matches(&editor_fingerprint_fixture(Some("Prompt"), 488, 700,)));
    assert!(!expected.logically_matches(&editor_fingerprint_fixture(
        Some("Other field"),
        488,
        754,
    )));
    assert!(!expected.logically_matches(&editor_fingerprint_fixture(None, 488, 754)));
    assert!(!expected.logically_matches(&editor_fingerprint_fixture(Some("Prompt"), 700, 754,)));
}

#[test]
fn empty_but_readable_field_is_not_blind() {
    // The common dictation target is an EMPTY composer: readable as "",
    // which must still support a first-insertion confirmation.
    assert!(FocusedFieldProof::default().is_blind());
    let proof = FocusedFieldProof {
        value: Some(String::new()),
        ..Default::default()
    };
    assert!(!proof.is_blind());
}

#[test]
fn confirmation_requires_an_attributable_delta_in_one_representation() {
    // First insertion into an empty field.
    assert!(confirm_insertion("", None, "hello", "hello"));
    // Measured in ChatGPT and Claude: a pasted paragraph break reads back
    // as one newline, into an empty box and after existing content.
    let needle = "one.\n\ntwo.";
    assert!(confirm_insertion("\n", Some((0, 0)), "one.\ntwo.", needle));
    assert!(confirm_insertion(
        "draft",
        Some((5, 0)),
        "draftone.\ntwo.",
        needle
    ));
    // Mirrored representation, multi-paragraph: whole value doubled...
    assert!(confirm_insertion("", None, "one.\ntwo.one.\ntwo.", needle));
    // ...and per-node doubling proven by the mirrored `before` snapshot.
    assert!(confirm_insertion(
        "draftdraft",
        None,
        "draftdraftone.\ntwo.one.\ntwo.",
        needle
    ));
    // Unchanged text never confirms, mirrored or not.
    assert!(!confirm_insertion("one.\ntwo.", None, "one.\ntwo.", needle));
    assert!(!confirm_insertion(
        "one.\ntwo.one.\ntwo.",
        None,
        "one.\ntwo.one.\ntwo.",
        needle
    ));
    // Incorrect text never confirms, mirrored or not.
    assert!(!confirm_insertion(
        "draft",
        None,
        "draftone.\nother.",
        needle
    ));
    assert!(!confirm_insertion(
        "",
        None,
        "one.\nother.one.\nother.",
        needle
    ));
    // A +2 delta without a proven mirror stays rejected.
    assert!(!confirm_insertion(
        "draft",
        None,
        "draftone.\ntwo.one.\ntwo.",
        needle
    ));
    // Unchanged, absent, removed, or already-present text is never proof.
    assert!(!confirm_insertion("hello", None, "hello", "hello"));
    assert!(!confirm_insertion("draft", None, "draft and more", "draft"));
    assert!(!confirm_insertion("gamma gamma", None, "gamma", "gamma"));
    // Exactly one NEW occurrence is proof.
    assert!(confirm_insertion(
        "alpha beta",
        None,
        "alpha beta gamma",
        "gamma"
    ));
    assert!(confirm_insertion(
        "",
        None,
        "hello worldhello world",
        "hello world"
    ));
    assert!(confirm_insertion(
        "draftdraft",
        None,
        "draft hellodraft hello",
        "hello"
    ));
    assert!(confirm_insertion(
        "draftdraft",
        None,
        "draftdrafthellohello",
        "hello"
    ));
    assert!(!confirm_insertion(
        "draft",
        None,
        "draft hellohello",
        "hello"
    ));
}

#[test]
fn confirmation_accepts_exact_selection_replacement_and_rejects_reused_text() {
    // Selection "world" replaced by "there".
    assert!(confirm_insertion(
        "hello world",
        Some((6, 5)),
        "hello there",
        "there"
    ));
    // A placeholder-like value with an unrelated range falls through to the
    // occurrence delta, which must still reject text that was already there.
    assert!(!confirm_insertion(
        "hello world",
        Some((0, 5)),
        "hello world!",
        "unrelated"
    ));
}
