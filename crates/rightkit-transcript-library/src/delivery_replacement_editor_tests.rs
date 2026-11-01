use super::replacement_editor_may_receive as ok;

#[test]
fn only_the_same_place_with_the_same_text_may_replace_the_original() {
    assert!(ok(true, Some(7), Some(7), Some(false)));
    assert!(!ok(false, Some(7), Some(7), Some(false))); // other app or window
    assert!(!ok(true, Some(7), Some(8), Some(false))); // different text
    assert!(!ok(true, Some(7), None, Some(false))); // unreadable candidate
    assert!(!ok(true, None, None, Some(false))); // nothing captured at start
    assert!(!ok(true, Some(7), Some(7), Some(true))); // secure field
    assert!(!ok(true, Some(7), Some(7), None)); // secure state unknown
}

#[test]
fn bound_editor_read_failure_is_not_content_change_or_permission_to_paste() {
    use super::{editor_content_status as status, EditorContentStatus::*};
    assert_eq!(status(Some(7), None, false), Unreadable);
    assert_eq!(status(None, None, false), MissingBaseline);
    assert_eq!(status(Some(7), Some(8), false), Changed);
    assert_eq!(status(Some(7), Some(7), false), Unchanged);
    // Destruction always wins, including a stale successful text read.
    assert_eq!(status(Some(7), Some(7), true), Destroyed);
    assert_eq!(status(Some(7), None, true), Destroyed);
}
