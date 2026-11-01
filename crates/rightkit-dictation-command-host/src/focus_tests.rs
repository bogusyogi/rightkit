#[cfg(target_os = "macos")]
#[test]
fn macos_target_snapshot_is_identity_only() {
    let target = super::macos_target_snapshot(
        rightkit_automation::desktop::macos::FrontApp {
            pid: 42,
            name: Some("Target".to_string()),
            bundle: Some("app.target".to_string()),
            bundle_path: None,
        },
        false,
    );
    assert_eq!(target.process_id, Some(42));
    assert_eq!(target.process_name.as_deref(), Some("Target"));
    assert_eq!(target.focused_text_input, None);
    assert!(matches!(
        target.foreground_target,
        Some(rightkit_transcript_library::delivery::ForegroundTarget::WindowHandle { .. }) | None
    ));
}

#[test]
fn focused_field_context_is_available_for_an_empty_editable_field() {
    let context = super::focused_field_context_from_values(false, None, Some("  "), None, 8)
        .expect("an accessible non-password field is still a valid capture");
    assert_eq!(context.window_title, None);
    assert_eq!(context.field_text, None);
    assert_eq!(context.selected_text, None);
}

#[test]
fn focused_field_context_rejects_passwords_and_caps_unicode_safely() {
    assert!(super::focused_field_context_from_values(
        true,
        Some("Login"),
        Some("secret"),
        Some("secret"),
        8,
    )
    .is_none());

    let context = super::focused_field_context_from_values(
        false,
        Some("  Compose  "),
        Some("abc😀defghij"),
        Some("  Priya  "),
        8,
    )
    .expect("normal field context");
    assert_eq!(context.window_title.as_deref(), Some("Compose"));
    assert_eq!(context.field_text.as_deref(), Some("abc😀d […] hij"));
    assert_eq!(context.selected_text.as_deref(), Some("Priya"));
}

#[test]
fn selection_fingerprint_requires_nonempty_selection_and_tracks_field_changes() {
    let selected = super::FocusedFieldContext {
        field_text: Some("before selected after".to_string()),
        selected_text: Some("selected".to_string()),
        ..Default::default()
    };
    assert!(super::selection_fingerprint(&selected).is_some());
    assert_ne!(
        super::selection_fingerprint(&selected),
        super::selection_fingerprint(&super::FocusedFieldContext {
            field_text: Some("different selected after".to_string()),
            ..selected.clone()
        })
    );
    assert_eq!(
        super::selection_fingerprint(&super::FocusedFieldContext {
            selected_text: Some("  \n\t".to_string()),
            ..Default::default()
        }),
        None
    );
}

#[test]
fn selected_transform_context_excludes_the_source_selection() {
    assert_eq!(
        super::field_text_without_selection(
            Some("Before. Selected source. After."),
            Some("Selected source."),
        )
        .as_deref(),
        Some("Before. After.")
    );
    assert_eq!(
        super::field_text_without_selection(Some("Selected source."), Some("Selected source.")),
        None
    );
    assert_eq!(
        super::field_text_without_selection(Some("Surrounding reference"), Some("Other"))
            .as_deref(),
        Some("Surrounding reference")
    );
}

#[test]
fn uia_probe_target_and_required_counts_are_explicit() {
    assert!(super::uia_probe_target_matches(
        Some("msedge.exe"),
        Some("Inbox - Gmail - Microsoft Edge"),
        Some("msedge"),
        Some("gmail"),
    ));
    assert!(!super::uia_probe_target_matches(
        Some("pwsh.exe"),
        Some("Administrator: PowerShell"),
        Some("msedge"),
        Some("gmail"),
    ));
    assert!(super::uia_probe_counts_pass(120, 0, false));
    assert!(super::uia_probe_counts_pass(120, 8, true));
    assert!(!super::uia_probe_counts_pass(120, 0, true));
    assert!(!super::uia_probe_counts_pass(0, 0, false));
}

#[test]
fn selection_present_for_revalidation_rejects_missing_probe_and_whitespace() {
    // Probe unavailable (focus lost, AX tree unavailable) → stale.
    assert!(!super::selection_present_for_revalidation(None));
    // Empty probe value → stale.
    assert!(!super::selection_present_for_revalidation(Some("")));
    // Whitespace-only probe value → stale (treat as empty).
    assert!(!super::selection_present_for_revalidation(Some("   \n\t")));
    // Real selection → fresh.
    assert!(super::selection_present_for_revalidation(Some(
        "hello world"
    )));
}

#[cfg(target_os = "windows")]
#[test]
fn windows_uia_probe_timeout_saturates_lane_and_recovers() {
    let _test_guard = crate::test_support::test_env_guard();
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    };

    // RightKit's `win::with_deadline` owns the lane: a stuck call times the
    // caller out, later calls return immediately without running, and the lane
    // recovers once the stuck call exits.
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    let timed_out_call = std::thread::spawn(move || {
        let result = super::bounded_uia_probe("timeout_test", move || {
            entered_tx
                .send(())
                .expect("timeout probe caller remains alive");
            release_rx
                .recv()
                .expect("test releases blocked detached probe");
            Some(())
        });
        result_tx
            .send(result.is_none())
            .expect("timeout result receiver remains alive");
    });
    entered_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("probe started on the RightKit worker");
    // A blocked context/proof provider must not consume password capacity.
    assert_eq!(
        super::bounded_password_uia_probe("password_lane_while_context_hung", || Some(())),
        Some(()),
        "password security probe must have an independent bounded lane"
    );
    assert!(
        result_rx
            .recv_timeout(super::UIA_PROBE_TIMEOUT + std::time::Duration::from_millis(200))
            .expect("bounded caller returns at its deadline"),
        "blocked UIA probe must time out"
    );
    timed_out_call.join().expect("bounded caller thread joined");

    let closure_ran = Arc::new(AtomicBool::new(false));
    let closure_ran_in_probe = Arc::clone(&closure_ran);
    let started = std::time::Instant::now();
    assert_eq!(
        super::bounded_uia_probe("saturation_test", move || {
            closure_ran_in_probe.store(true, Ordering::Release);
            Some(())
        }),
        None
    );
    assert!(
        started.elapsed() < std::time::Duration::from_millis(100),
        "saturated UIA calls must return without waiting for the occupied lane"
    );
    assert!(
        !closure_ran.load(Ordering::Acquire),
        "saturated UIA calls must not run behind the stuck call"
    );

    release_tx.send(()).expect("release blocked probe");
    let recovered = (0..1000).find_map(|_| {
        let result = super::bounded_uia_probe("recovery_test", || Some(()));
        if result.is_none() {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        result
    });
    assert_eq!(
        recovered,
        Some(()),
        "lane recovers after the stuck call exits"
    );
}

#[cfg(target_os = "windows")]
#[test]
fn windows_password_uia_lane_is_independent_of_the_context_lane() {
    let _test_guard = crate::test_support::test_env_guard();
    use std::sync::mpsc;

    // Hold the context lane inside a probe; the password lane still answers.
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let holder = std::thread::spawn(move || {
        super::bounded_uia_probe("context_lane_holder", move || {
            entered_tx.send(()).expect("holder receiver alive");
            release_rx.recv().expect("test releases holder");
            Some(())
        })
    });
    entered_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("context lane occupied");
    assert_eq!(
        super::bounded_password_uia_probe("password_lane_independent", || Some(())),
        Some(())
    );
    release_tx.send(()).expect("release holder");
    let _ = holder.join();
    assert_eq!(
        super::bounded_password_uia_probe("password_lane_recovery", || Some(())),
        Some(())
    );
}

#[cfg(target_os = "windows")]
#[test]
#[ignore = "reads the currently focused Windows UIA element; metadata only"]
fn windows_uia_focused_field_probe_reports_metadata_only() {
    let delay_ms = std::env::var("RIGHTKIT_UIA_PROBE_DELAY_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    if delay_ms > 0 {
        println!("uia_probe_waiting_ms={delay_ms}");
        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
    }
    let process_contains = std::env::var("RIGHTKIT_UIA_PROBE_PROCESS_CONTAINS").ok();
    let title_contains = std::env::var("RIGHTKIT_UIA_PROBE_TITLE_CONTAINS").ok();
    let timeout_ms = std::env::var("RIGHTKIT_UIA_PROBE_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let require_selection = std::env::var("RIGHTKIT_UIA_PROBE_REQUIRE_SELECTION")
        .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
    let started = std::time::Instant::now();
    let target = loop {
        let target = super::snapshot_current_target();
        if super::uia_probe_target_matches(
            target.process_name.as_deref(),
            target.window_title.as_deref(),
            process_contains.as_deref(),
            title_contains.as_deref(),
        ) {
            break target;
        }
        assert!(
            started.elapsed().as_millis() < u128::from(timeout_ms),
            "uia probe timed out before expected foreground target"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    let process = target.process_name.as_deref().unwrap_or("unknown");
    let title_chars = target
        .window_title
        .as_deref()
        .map(|title| title.chars().count())
        .unwrap_or(0);
    match super::focused_field_context(super::FIELD_CONTEXT_MAX_CHARS) {
        Some(context) => {
            let field_chars = context
                .field_text
                .as_deref()
                .map(|text| text.chars().count())
                .unwrap_or(0);
            let selected_chars = context
                .selected_text
                .as_deref()
                .map(|text| text.chars().count())
                .unwrap_or(0);
            println!(
                "uia_context_available=true process={process} title_chars={title_chars} field_chars={field_chars} selected_chars={selected_chars} require_selection={require_selection}"
            );
            if process_contains.is_some() || title_contains.is_some() {
                assert!(
                    super::uia_probe_counts_pass(field_chars, selected_chars, require_selection),
                    "uia probe reached expected target but required text context was absent"
                );
            }
        }
        None => {
            println!(
                "uia_context_available=false process={process} title_chars={title_chars} field_chars=0 selected_chars=0"
            );
            assert!(
                process_contains.is_none() && title_contains.is_none(),
                "uia probe reached expected target but focused field context was unavailable"
            );
        }
    }
}

#[cfg(target_os = "windows")]
#[test]
fn windows_terminal_targets_are_editable_paste_targets() {
    assert!(super::windows_terminal_target(
        Some("WindowsTerminal.exe"),
        Some("Administrator: PowerShell"),
        Some("CASCADIA_HOSTING_WINDOW_CLASS"),
    ));
    assert!(super::windows_terminal_target(
        Some("pwsh.exe"),
        None,
        Some("ConsoleWindowClass"),
    ));
    assert!(!super::windows_terminal_target(
        Some("explorer.exe"),
        Some("Downloads"),
        Some("CabinetWClass"),
    ));
}

#[test]
fn unknown_password_state_is_neither_a_paste_veto_nor_submit_proof() {
    // Production decision functions used by `insert_transcript`.
    assert!(super::password_state_vetoes_paste(Some(true)));
    assert!(!super::password_state_vetoes_paste(Some(false)));
    assert!(!super::password_state_vetoes_paste(None));
    assert!(super::password_state_allows_submit(Some(false)));
    assert!(!super::password_state_allows_submit(Some(true)));
    assert!(!super::password_state_allows_submit(None));
}
