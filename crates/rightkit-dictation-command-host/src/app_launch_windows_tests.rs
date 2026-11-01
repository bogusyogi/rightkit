use super::*;

#[test]
fn streaming_fires_only_on_certain_matches() {
    let apps = vec![
        ("cross device experience host".into(), "cross-target".into()),
        ("figma".into(), "figma-target".into()),
    ];
    // A mere whole-word hit ("cross") is NOT certain — streaming withholds it.
    assert_eq!(certain_windows_match("cross", &apps), None);
    // Exact name is certain — fires mid-hold.
    assert_eq!(
        certain_windows_match("figma", &apps).as_deref(),
        Some("figma-target")
    );
}

#[test]
fn fuzzy_resolves_single_word_app_names_the_asr_split() {
    let apps = vec![
        ("viewright".into(), "target-vr".into()),
        ("google chrome".into(), "target-chrome".into()),
    ];
    // "open ViewRight" is transcribed "open view, right?"; the query reaching
    // the matcher is "view right", which must resolve to ViewRight (the whole
    // reason app launch felt broken). squash("view right") == "viewright".
    assert_eq!(
        best_windows_match("view right", &apps).as_deref(),
        Some("target-vr")
    );
    assert_eq!(
        best_windows_match("viewright", &apps).as_deref(),
        Some("target-vr")
    );
    // A real whole-word name still resolves, and stray dictation still doesn't.
    assert_eq!(
        best_windows_match("chrome", &apps).as_deref(),
        Some("target-chrome")
    );
    assert_eq!(best_windows_match("the door", &apps), None);
}
