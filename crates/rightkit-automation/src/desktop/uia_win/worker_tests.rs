use super::protocol::TextProof;

fn rows(scope: i32, text: &str) -> TextProof {
    TextProof {
        value: None,
        document: None,
        terminal_rows: Some(text.to_owned()),
        terminal_scope_runtime_id: Some(vec![scope]),
        terminal_scope_native_window_handle: Some(42),
    }
}

#[test]
fn terminal_proof_requires_same_scope_identity() {
    assert!(rows(1, "before").confirms(&rows(1, "before pasted"), "pasted"));
    assert!(!rows(1, "before").confirms(&rows(2, "before pasted"), "pasted"));
    assert!(!rows(1, "before pasted").confirms(&rows(1, "before pasted again"), "pasted"));
}

#[test]
fn static_terminal_label_is_not_text_proof() {
    assert!(super::is_static_terminal_label(
        "Terminal 1 Toggle Screen Reader Accessibility Mode"
    ));
    assert!(!super::is_static_terminal_label("Terminal 1 draft"));
}

#[test]
fn readback_uses_canonical_line_breaks() {
    let before = rows(1, "first\nsecond");
    let after = rows(1, "first\n\nsecond\npasted");
    assert!(before.confirms(&after, "pasted"));
}
