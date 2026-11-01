use super::*;

#[test]
fn terminal_display_cannot_bind_transcript_to_scrollback() {
    for bundle in [
        "com.mitchellh.ghostty",
        "com.apple.Terminal",
        "com.googlecode.iterm2",
    ] {
        assert!(!supports_editor_content_binding(bundle, false));
        // Native Find/search controls inside the same app are still editors.
        assert!(supports_editor_content_binding(bundle, true));
    }
}

#[test]
fn nonterminal_editor_does_not_require_settable_ax_value() {
    // Rich browser editors often expose readable but non-settable AXValue.
    // They must retain existing-text protection.
    assert!(supports_editor_content_binding("com.google.Chrome", false));
    assert!(supports_editor_content_binding("com.apple.TextEdit", true));
}

#[test]
fn ax_seen_hashes_then_confirms_logical_equality() {
    let first = CFString::new("same AX element").as_CFType();
    let second = CFString::new("same AX element").as_CFType();
    let mut seen = AxSeen::new();
    assert!(!ax_element_seen_or_insert(&mut seen, &first));
    assert!(ax_element_seen_or_insert(&mut seen, &second));
    assert_eq!(seen.values().map(Vec::len).sum::<usize>(), 1);
}

#[test]
fn focused_empty_text_field_beats_unfocused_nonempty_field() {
    let focused =
        editable_candidate_score("AXTextArea", "text area", true, true, true, false).unwrap();
    let stale =
        editable_candidate_score("AXTextArea", "text area", false, true, true, true).unwrap();
    assert!(focused > stale);
}

#[test]
fn broad_ax_containers_are_never_text_candidates() {
    assert_eq!(
        editable_candidate_score("AXGroup", "group", true, true, true, true),
        None
    );
    assert_eq!(
        editable_candidate_score("AXWebArea", "web area", true, true, true, true),
        None
    );
}

#[test]
fn focused_contenteditable_semantics_can_use_marker_text() {
    assert!(
        editable_candidate_score("AXUnknown", "editable text", true, true, false, true,).is_some()
    );
}

#[test]
fn placeholder_value_is_empty_but_text_containing_it_is_content() {
    let advertised = vec!["Do anything".to_string()];
    assert!(value_is_placeholder("\nDo anything", &advertised));
    assert!(!value_is_placeholder("Do anything else", &advertised));
    assert!(!value_is_placeholder("hello", &advertised));
    assert!(!value_is_placeholder("", &advertised));
    assert!(!value_is_placeholder("Do anything", &[]));
    // Slate: marker + overlay in one paragraph is an empty editor.
    let overlay = slate_placeholder(&["\u{FEFF}".to_string(), "Tip: drag".to_string()]);
    assert_eq!(overlay.as_deref(), Some("Tip: drag"));
    assert!(value_is_placeholder(
        "\u{FEFF}\nTip: drag",
        &[overlay.unwrap()]
    ));
    // Real text never shares a paragraph with the empty-node marker.
    assert_eq!(slate_placeholder(&["real text".to_string()]), None);
    assert_eq!(slate_placeholder(&["\u{FEFF}".to_string()]), None);
}

#[test]
fn secure_probe_never_declares_unknown_focused_element_safe() {
    assert_eq!(secure_state_for_roles(None, None), None);
    assert_eq!(secure_state_for_roles(Some("AXUnknown"), None), None);
    assert_eq!(secure_state_for_roles(Some("AXCustomRole"), None), None);
    assert_eq!(
        secure_state_for_roles(Some("AXTextField"), None),
        Some(false)
    );
    assert_eq!(
        secure_state_for_roles(Some("AXTextField"), Some("AXSecureTextField")),
        Some(true)
    );
}
