use super::*;
fn binding() -> WezTermPaneBinding {
    WezTermPaneBinding {
        hwnd: 1,
        pid: 2,
        window_id: 3,
        pane_id: 4,
        socket: PathBuf::from("C:/sock"),
        cli_path: PathBuf::from("C:/wezterm.exe"),
        process_creation_time: 5,
    }
}
#[test]
fn parser_requires_unique_active_exact_pane() {
    let b = binding();
    assert!(parse_active_pane(r#"[{"window_id":3,"pane_id":4,"is_active":true}]"#, &b).is_ok());
    assert_eq!(
        parse_active_pane(r#"[{"window_id":3,"pane_id":4,"is_active":false}]"#, &b),
        Err(WezTermError::TargetChanged)
    );
    assert_eq!(
        parse_active_pane(
            r#"[{"window_id":3,"pane_id":4,"is_active":true},{"window_id":3,"pane_id":4,"is_active":true}]"#,
            &b
        ),
        Err(WezTermError::AmbiguousPane)
    );
    // WezTerm can report active panes from multiple tabs in one GUI window;
    // list-clients supplies focused pane identity used to disambiguate.
    assert!(parse_active_pane(
        r#"[{"window_id":3,"pane_id":4,"is_active":true},{"window_id":3,"pane_id":5,"is_active":true}]"#,
        &b
    ).is_ok());
    let zero = WezTermPaneBinding { window_id: 0, ..b };
    assert!(parse_active_pane(r#"[{"window_id":0,"pane_id":4,"is_active":true}]"#, &zero).is_ok());
}
#[test]
fn parser_rejects_changed_or_ambiguous_topology() {
    let b = binding();
    for json in [
        "[]",
        "invalid",
        r#"[{"window_id":3,"pane_id":5,"is_active":true}]"#,
        r#"[{"window_id":4,"pane_id":4,"is_active":true}]"#,
        r#"[{"window_id":3,"pane_id":5,"is_active":true}]"#,
    ] {
        assert_eq!(
            parse_active_pane(json, &b),
            Err(WezTermError::AmbiguousPane)
        );
    }
}

#[test]
fn focused_client_resolves_two_active_tabs_and_rejects_focus_changes() {
    let clients = r#"[
        {"pid":2,"focused_pane_id":4,"workspace":"default"}
    ]"#;
    assert_eq!(parse_focused_client(clients, 2, None), Ok(4));
    assert_eq!(parse_focused_client(clients, 2, Some(4)), Ok(4));
    assert_eq!(
        parse_focused_client(clients, 2, Some(5)),
        Err(WezTermError::TargetChanged)
    );
    let two_tabs = r#"[
        {"window_id":3,"pane_id":4,"is_active":true},
        {"window_id":3,"pane_id":5,"is_active":true}
    ]"#;
    assert!(parse_active_pane(two_tabs, &binding()).is_ok());
}

#[test]
fn focused_client_rejects_missing_or_duplicate_gui_pid() {
    let missing = r#"[{"pid":99,"focused_pane_id":4}]"#;
    assert_eq!(
        parse_focused_client(missing, 2, None),
        Err(WezTermError::AmbiguousPane)
    );
    let duplicate = r#"[
        {"pid":2,"focused_pane_id":4},
        {"pid":2,"focused_pane_id":4}
    ]"#;
    assert_eq!(
        parse_focused_client(duplicate, 2, None),
        Err(WezTermError::AmbiguousPane)
    );
    let missing_focus = r#"[{"pid":2,"workspace":"default"}]"#;
    assert_eq!(
        parse_focused_client(missing_focus, 2, None),
        Err(WezTermError::AmbiguousPane)
    );
}

#[test]
fn focused_pane_row_requires_same_window_and_active_focus() {
    let b = binding();
    assert_eq!(
        parse_active_pane(r#"[{"window_id":9,"pane_id":4,"is_active":true}]"#, &b),
        Err(WezTermError::AmbiguousPane)
    );
    assert_eq!(
        parse_active_pane(r#"[{"window_id":3,"pane_id":4,"is_active":false}]"#, &b),
        Err(WezTermError::TargetChanged)
    );
}
#[test]
fn confirmation_requires_new_single_nonce() {
    use rightkit_transcript_library::delivery::insertion_attributable as confirms_insertion;
    assert!(confirms_insertion("old", "old HR-1", "HR-1"));
    assert!(!confirms_insertion("old HR-1", "old HR-1", "HR-1"));
    assert!(!confirms_insertion("old", "HR-1 HR-1", "HR-1"));
}
