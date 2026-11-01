use super::*;

/// The regression this function exists for: the same phrase dictated
/// twice into one field. Presence-based confirmation returned true here
/// before the paste had been processed at all.
#[test]
fn repeated_phrase_is_not_confirmed_until_the_count_actually_increases() {
    let transcript = "thanks";
    // Second dictation, paste not yet processed: the field still holds
    // only the first "thanks". Presence would say yes; attribution says no.
    assert!(!insertion_attributable("thanks", "thanks", transcript));
    // Paste processed: one more occurrence than before.
    assert!(insertion_attributable(
        "thanks",
        "thanks thanks",
        transcript
    ));
    // A third identical paste is still attributable on its own delta.
    assert!(insertion_attributable(
        "thanks thanks",
        "thanks thanks thanks",
        transcript
    ));
}

#[test]
fn text_already_present_in_an_untouched_field_is_never_confirmed() {
    assert!(!insertion_attributable(
        "please send the report",
        "please send the report",
        "send the report"
    ));
}

#[test]
fn insertion_into_an_empty_field_is_confirmed() {
    assert!(insertion_attributable("", "hello world", "hello world"));
}

#[test]
fn a_change_that_did_not_add_the_transcript_is_not_confirmed() {
    // The user typed while we waited; the field changed but our text
    // never arrived. Failing closed keeps the transcript recoverable.
    assert!(!insertion_attributable("abc", "abcd", "hello"));
}

#[test]
fn a_mangled_insertion_is_not_confirmed() {
    // Autocorrect ate part of it: the count did not rise by one, so we
    // refuse to call it consumed rather than press Enter on the mangling.
    assert!(!insertion_attributable("", "helo world", "hello world"));
}

#[test]
fn an_empty_or_whitespace_transcript_is_never_confirmed() {
    assert!(!insertion_attributable("a", "ab", ""));
    assert!(!insertion_attributable("a", "ab", "   "));
}

#[test]
fn generated_delivery_ids_share_a_process_global_monotonic_sequence() {
    let first = next_delivery_id(0);
    let second = next_delivery_id(0);
    assert_ne!(first, second);
    assert!(first.starts_with(&format!("delivery-{}-", std::process::id())));
    let first_sequence = first.rsplit_once('-').unwrap().1.parse::<u64>().unwrap();
    let second_sequence = second.rsplit_once('-').unwrap().1.parse::<u64>().unwrap();
    assert_eq!(second_sequence, first_sequence + 1);
}

#[test]
fn method_tracks_outcome() {
    assert_eq!(
        method_for_outcome(&DeliveryOutcome::Pasted),
        DeliveryMethod::ClipboardThenPaste
    );
    assert_eq!(
        method_for_outcome(&DeliveryOutcome::PasteSentUnconfirmed {
            reason: "readback_unavailable".into(),
        }),
        DeliveryMethod::ClipboardThenPasteUnconfirmed
    );
    assert_eq!(
        method_for_outcome(&DeliveryOutcome::CopiedFallback {
            reason: CopyFallbackReason::PasteFailed
        }),
        DeliveryMethod::ClipboardOnly
    );
    assert_eq!(
        method_for_outcome(&DeliveryOutcome::Error {
            code: "E".into(),
            message: "m".into()
        }),
        DeliveryMethod::None
    );
    assert_eq!(
        method_for_outcome(&DeliveryOutcome::Cancelled),
        DeliveryMethod::None
    );
    assert_eq!(
        method_for_outcome(&DeliveryOutcome::Draft),
        DeliveryMethod::None
    );
}

#[test]
fn draft_delivery_id_is_deterministic_per_session() {
    assert_eq!(
        draft_delivery_id("session-1"),
        draft_delivery_id("session-1")
    );
    assert_ne!(
        draft_delivery_id("session-1"),
        draft_delivery_id("session-2")
    );
    assert_eq!(draft_delivery_id("session-1"), "draft-session-1");
}

#[test]
fn new_draft_uses_the_deterministic_id_and_empty_target() {
    let record = DeliveryRecord::new_draft("session-1", "partial text so far");
    assert_eq!(record.delivery_id, draft_delivery_id("session-1"));
    assert_eq!(record.outcome, DeliveryOutcome::Draft);
    assert_eq!(record.transcript, "partial text so far");
    assert_eq!(record.target, TargetSnapshot::empty());

    // Rebuilding for the same session reuses the same id (upsert key).
    let second = DeliveryRecord::new_draft("session-1", "more text");
    assert_eq!(record.delivery_id, second.delivery_id);
}

#[test]
fn insertion_proof_is_unconstructible_without_a_confirmed_delta() {
    assert!(InsertionProof::from_confirmed_delta(false).is_none());
    assert!(InsertionProof::from_confirmed_delta(true).is_some());
}

#[test]
fn pasted_delivery_record_requires_a_proof_and_reports_pasted() {
    let proof = InsertionProof::from_confirmed_delta(true).expect("confirmed delta");
    let record = pasted_delivery_record(
        "delivery-1",
        "hello world",
        TargetSnapshot::test_target(),
        12,
        proof,
    );
    assert_eq!(record.outcome, DeliveryOutcome::Pasted);
    assert_eq!(record.method, DeliveryMethod::ClipboardThenPaste);
    assert_eq!(record.elapsed_ms, 12);
}

#[test]
fn fallback_reason_display_is_snake_case() {
    assert_eq!(
        CopyFallbackReason::ClipboardUnavailable.to_string(),
        "clipboard_unavailable"
    );
    assert_eq!(CopyFallbackReason::Other("x".into()).to_string(), "x");
}

#[test]
fn test_target_carries_explicit_foreground_identity() {
    assert_eq!(
        TargetSnapshot::test_target().foreground_target,
        Some(ForegroundTarget::WindowHandle { handle: 1 })
    );
}

#[test]
fn stable_target_identity_requires_matching_field_or_foreground_target() {
    let target = TargetSnapshot::test_target();
    assert!(target.same_stable_target(&target));

    let mut other_field = target.clone();
    other_field.focused_control_handle = Some(3);
    assert!(!target.same_stable_target(&other_field));

    let mut unknown = target.clone();
    unknown.focused_control_handle = None;
    assert!(!target.same_stable_target(&unknown));

    let mut no_identity = target.clone();
    no_identity.focused_control_handle = None;
    no_identity.foreground_target = None;
    assert!(!no_identity.same_stable_target(&no_identity));

    let mut window_only = target.clone();
    window_only.focused_control_handle = None;
    window_only.foreground_target = Some(ForegroundTarget::WindowHandle { handle: 1 });
    assert!(window_only.same_stable_target(&window_only));

    let mut other_window = window_only.clone();
    other_window.foreground_target = Some(ForegroundTarget::WindowHandle { handle: 2 });
    assert!(!window_only.same_stable_target(&other_window));
}

#[test]
fn target_snapshot_deserializes_without_foreground_identity() {
    let snapshot: TargetSnapshot = serde_json::from_str(
        r#"{
                "process_id": 42,
                "process_name": "LegacyApp",
                "window_title": "Legacy Window",
                "window_handle": 1001,
                "is_elevated": false
            }"#,
    )
    .expect("legacy target snapshot should remain readable");

    assert_eq!(snapshot.process_id, Some(42));
    assert_eq!(snapshot.window_handle, Some(1001));
    assert_eq!(snapshot.focused_control_handle, None);
    assert_eq!(snapshot.foreground_target, None);
    assert_eq!(snapshot.focused_text_input, None);
}

#[test]
fn delivery_record_serializes_named_step_timings_and_reads_legacy_records() {
    let timing = DeliveryTimings::new(42)
        .with_step("clipboard_write", 3)
        .with_step("paste_keystroke", 4)
        .with_step("clipboard_cleanup", 5);
    let record = DeliveryRecord::for_test(
        DeliveryId::new("delivery-with-timing"),
        "hello",
        DeliveryOutcome::Pasted,
    )
    .with_delivery_timing(timing);

    let serialized = serde_json::to_value(&record).expect("record serializes");
    assert_eq!(serialized["delivery_timing"]["total_ms"], 42);
    assert_eq!(
        serialized["delivery_timing"]["steps"][1]["name"],
        "paste_keystroke"
    );
    assert_eq!(serialized["delivery_timing"]["steps"][2]["elapsed_ms"], 5);

    let legacy: DeliveryRecord = serde_json::from_str(
        r#"{
                "delivery_id": "legacy",
                "transcript": "old",
                "outcome": { "kind": "pasted" },
                "method": "clipboard_then_paste",
                "elapsed_ms": 9,
                "target": {
                    "process_id": 42,
                    "process_name": "LegacyApp",
                    "window_title": "Legacy Window",
                    "window_handle": 1001,
                    "is_elevated": false
                },
                "delivered_at_ms": 100
            }"#,
    )
    .expect("legacy delivery record should still deserialize");
    assert_eq!(legacy.delivery_timing, None);
}

#[test]
fn start_capture_binds_only_to_the_editor_named_at_the_hotkey() {
    // A: the hotkey proof names editor 10 and the pin is editor 10.
    assert!(start_capture_may_bind(true, Some(10), 10));
    // A -> B inside the same window: the pin found later is editor 20.
    assert!(!start_capture_may_bind(true, Some(10), 20));
    // The hotkey proof had NO editor identity: whichever editor appears
    // later (A or B) must not become context or the bound destination.
    assert!(!start_capture_may_bind(true, None, 10));
    assert!(!start_capture_may_bind(true, None, 20));
    // A proof acquired after the hotkey never binds, even if it names the pin.
    assert!(!start_capture_may_bind(false, Some(20), 20));
}

#[test]
fn a_container_proof_binds_only_with_a_hotkey_proof_that_names_an_editor() {
    // Chromium: proof names the render-host container (7), pin is the inner editor (0).
    assert!(start_capture_may_bind_via_container(true, Some(7), 0, true));
    assert!(!start_capture_may_bind_via_container(
        true,
        Some(7),
        0,
        false
    ));
    // No editor in the proof, or a late proof: the container claim is moot.
    assert!(!start_capture_may_bind_via_container(true, None, 0, true));
    assert!(!start_capture_may_bind_via_container(
        false,
        Some(7),
        0,
        true
    ));
    // Direct match still works without a container claim.
    assert!(start_capture_may_bind_via_container(
        true,
        Some(7),
        7,
        false
    ));
}

#[test]
fn a_plain_page_document_never_binds_under_a_web_area() {
    assert!(web_area_editor_is_bindable(Some(false), true, true, false));
    assert!(web_area_editor_is_bindable(Some(false), true, false, true));
    // Page-level Document: focusable, readable, but no writable edit target.
    assert!(!web_area_editor_is_bindable(
        Some(false),
        true,
        false,
        false
    ));
    assert!(!web_area_editor_is_bindable(Some(false), false, true, true));
    assert!(!web_area_editor_is_bindable(Some(true), true, true, true));
    assert!(!web_area_editor_is_bindable(None, true, true, true));
}

#[test]
fn unbound_stop_recovery_needs_the_same_nonsecure_continuous_editor() {
    let ok = |place, before, after, elem, secure, edit| {
        unbound_stop_editor_is_safe(place, before, after, elem, secure, edit)
    };
    assert!(ok(true, true, true, false, Some(true), Some(true)));
    assert!(ok(true, true, true, true, Some(true), None));
    assert!(!ok(false, true, true, true, Some(true), Some(true)));
    assert!(!ok(true, false, true, true, Some(true), Some(true)));
    assert!(!ok(true, true, false, true, Some(true), Some(true)));
    assert!(!ok(true, true, true, true, Some(false), Some(true)));
    assert!(!ok(true, true, true, true, None, Some(true)));
    assert!(!ok(true, true, true, true, Some(true), Some(false)));
    assert!(!ok(true, true, true, false, Some(true), None));
}

#[test]
fn leading_space_rule_is_caret_aware_and_never_doubles() {
    assert!(leading_space_needed("This is a test.", None));
    assert!(!leading_space_needed("This is a test. ", None));
    assert!(!leading_space_needed("", None));
    assert!(!leading_space_needed("say (", None));
    assert!(!leading_space_needed("line one\n", None));
    // Caret mid-text: judged at the caret, not the end of the box.
    assert!(!leading_space_needed("hello world", Some(6)));
    assert!(leading_space_needed("hello world", Some(5)));
    assert!(!leading_space_needed("hello", Some(0)));
    // UTF-16 units: the emoji is two units; caret after it.
    assert!(leading_space_needed("ok \u{1F600}", Some(5)));
    assert_eq!(with_leading_space("Please add", true), " Please add");
    assert_eq!(with_leading_space("Please add", false), "Please add");
    assert_eq!(with_leading_space(", and more", true), ", and more");
    assert_eq!(with_leading_space(" spaced", true), " spaced");
    assert_eq!(with_leading_space("", true), "");
}

#[cfg(target_os = "windows")]
#[test]
fn windows_terminal_targets_are_recognized() {
    assert!(super::windows_terminal_target(
        Some("WindowsTerminal.exe"),
        None,
        None
    ));
    assert!(super::windows_terminal_target(
        None,
        Some("Windows PowerShell"),
        None
    ));
    assert!(super::windows_terminal_target(
        None,
        None,
        Some("CASCADIA_HOSTING_WINDOW_CLASS")
    ));
    assert!(!super::windows_terminal_target(
        Some("notepad.exe"),
        Some("Untitled - Notepad"),
        None
    ));
}

#[cfg(test)]
mod terminal_grid_attribution_tests {
    use super::super::terminal_grid_insertion_attributable as grid;

    const TRANSCRIPT: &str = "I need you to see what is wrong with delivery of paste in the terminal because it keeps failing and the popup still appears";

    /// Claude Code's input box: the transcript wraps across rows, each row starts with a border glyph.
    fn boxed(rows: &[&str]) -> String {
        let mut out = String::from("\u{256D}\u{2500}\u{2500}\u{2500}\u{256E}\n");
        for row in rows {
            out.push_str("\u{2502} ");
            out.push_str(row);
            out.push_str(" \u{2502}\n");
        }
        out.push_str("\u{2570}\u{2500}\u{2500}\u{2500}\u{256F}\n  ? for shortcuts\n");
        out
    }

    #[test]
    fn a_wrapped_bordered_paste_is_attributable() {
        let before = boxed(&[""]);
        let after = boxed(&[
            "I need you to see what is wrong with delivery of paste in the",
            "terminal because it keeps failing and the popup still",
            "appears",
        ]);
        assert!(grid(&before, &after, TRANSCRIPT));
    }

    #[test]
    fn a_wrap_inside_a_word_is_attributable() {
        let before = boxed(&[""]);
        let after = boxed(&[
            "I need you to see what is wrong with delivery of paste in the terminal because it keeps fai",
            "ling and the popup still appears",
        ]);
        assert!(grid(&before, &after, TRANSCRIPT));
    }

    #[test]
    fn an_unchanged_grid_is_not_attributable() {
        let screen = boxed(&["some earlier text"]);
        assert!(!grid(&screen, &screen, TRANSCRIPT));
    }

    #[test]
    fn a_grid_that_changed_without_the_transcript_is_not_attributable() {
        let before = boxed(&[""]);
        let after = boxed(&["a spinner redrew and nothing was pasted"]);
        assert!(!grid(&before, &after, TRANSCRIPT));
    }

    #[test]
    fn the_transcript_already_on_screen_does_not_count_twice() {
        let before = boxed(&[TRANSCRIPT]);
        let after = format!("{before}\nspinner tick");
        assert!(!grid(&before, &after, TRANSCRIPT));
    }

    #[test]
    fn a_second_copy_is_attributable() {
        let before = boxed(&[TRANSCRIPT]);
        let after = format!("{before}{}", boxed(&[TRANSCRIPT]));
        assert!(grid(&before, &after, TRANSCRIPT));
    }

    #[test]
    fn an_empty_needle_is_never_attributable() {
        assert!(!grid("a", "ab", "  \n "));
    }
}
