use super::*;

#[test]
fn deliver_text_records_paste_step_timings_in_test_mode() {
    let _guard = crate::test_support::test_env_guard();
    std::env::set_var("RIGHTKIT_ENGINE_TEST_MODE", "1");
    let started = std::time::Instant::now();
    let record = deliver_text("paste timing smoke").expect("delivery succeeds in test mode");
    // Plain paste restoration is deferred so delivery does not block the
    // caller holding `EngineRuntime`'s worker-event-pump mutex.
    // `deliver_text` must therefore return well under PASTE_SETTLE_MS, not
    // after it.
    assert!(
        started.elapsed().as_millis() < 300,
        "deliver_text must not block on the paste-settle sleep"
    );
    let timing = record
        .delivery_timing
        .expect("pasted delivery should carry step timings");
    let step_names: Vec<&str> = timing.steps.iter().map(|step| step.name.as_str()).collect();

    assert!(step_names.contains(&"clipboard_write"));
    assert!(step_names.contains(&"paste_keystroke"));
    assert!(step_names.contains(&"clipboard_snapshot"));
    assert!(!step_names.contains(&"clipboard_cleanup"));
    assert!(!step_names.contains(&"paste_settle_deferred"));
    assert!(timing.total_ms < 300);
    assert!(timing
        .steps
        .iter()
        .all(|step| step.elapsed_ms <= timing.total_ms));
}

#[test]
fn deliver_text_with_enter_false_matches_plain_deliver_text() {
    // `deliver_text` must keep working exactly as before: it delegates to
    // `deliver_text_with_enter(transcript, false)`.
    let _guard = crate::test_support::test_env_guard();
    std::env::set_var("RIGHTKIT_ENGINE_TEST_MODE", "1");
    let record = deliver_text("plain delivery, no send").expect("delivery succeeds in test mode");
    assert!(matches!(record.outcome, DeliveryOutcome::Pasted));
}

#[test]
fn deliver_text_with_enter_records_completed_submit() {
    let _guard = crate::test_support::test_env_guard();
    std::env::set_var("RIGHTKIT_ENGINE_TEST_MODE", "1");
    let record =
        deliver_text_with_enter("zephyr send smoke", true).expect("delivery succeeds in test mode");
    assert!(matches!(record.outcome, DeliveryOutcome::Pasted));
    let timing = record
        .delivery_timing
        .expect("pasted delivery should carry step timings");
    let step_names: Vec<&str> = timing.steps.iter().map(|step| step.name.as_str()).collect();
    #[cfg(target_os = "windows")]
    assert!(step_names.contains(&"paste_settle_and_submit"));
    #[cfg(target_os = "macos")]
    assert!(step_names.contains(&"paste_settle_and_submit"));
}

#[test]
fn clipboard_transaction_failure_never_gets_destructive_fallback() {
    let error = DeliveryError::new("E_CLIPBOARD", "clipboard changed during staging");
    assert!(super::deliver::should_leave_clipboard_untouched(&error));

    let paste_error = DeliveryError::new("E_CGEVENT", "paste dispatch failed");
    assert!(!super::deliver::should_leave_clipboard_untouched(
        &paste_error
    ));
}

#[cfg(target_os = "windows")]
#[test]
fn terminal_targets_get_longer_clipboard_settle() {
    use super::insert::{paste_settle_ms_for_target, PASTE_SETTLE_MS, TERMINAL_PASTE_SETTLE_MS};

    let mut terminal = TargetSnapshot::test_target();
    terminal.process_name = Some("WindowsTerminal.exe".to_string());
    terminal.window_title = Some("Claude - PowerShell".to_string());

    let mut gui = TargetSnapshot::test_target();
    gui.process_name = Some("notepad.exe".to_string());
    gui.window_title = Some("Untitled - Notepad".to_string());

    assert_eq!(
        paste_settle_ms_for_target(&terminal),
        TERMINAL_PASTE_SETTLE_MS
    );
    assert_eq!(paste_settle_ms_for_target(&gui), PASTE_SETTLE_MS);
}
