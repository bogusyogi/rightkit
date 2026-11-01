use super::*;

// ── q-builder ────────────────────────────────────────────────────────────

#[test]
fn q_no_args_returns_root() {
    assert_eq!(
        build_q(None, None, None, None),
        "'root' in parents and trashed=false"
    );
}

#[test]
fn q_folder_id_uses_parent_filter() {
    assert_eq!(
        build_q(Some("abc123"), None, None, None),
        "'abc123' in parents and trashed=false"
    );
}

#[test]
fn q_query_overrides_folder_id() {
    let q = build_q(Some("folder"), Some("report"), None, None);
    assert_eq!(q, "name contains 'report' and trashed=false");
}

#[test]
fn q_escapes_single_quote_in_search() {
    let q = build_q(None, Some("it's here"), None, None);
    assert_eq!(q, "name contains 'it\\'s here' and trashed=false");
}

#[test]
fn q_escapes_single_quote_in_folder_id() {
    // Unusual but the escape path must still work.
    let q = build_q(Some("fold'er"), None, None, None);
    assert_eq!(q, "'fold\\'er' in parents and trashed=false");
}

#[test]
fn q_shelf_shared_and_starred() {
    assert_eq!(
        build_q(None, None, Some("shared"), None),
        "sharedWithMe=true and trashed=false"
    );
    assert_eq!(
        build_q(None, None, Some("starred"), None),
        "starred=true and trashed=false"
    );
}

#[test]
fn q_shelf_trash_lists_trashed() {
    assert_eq!(build_q(None, None, Some("trash"), None), "trashed=true");
}

#[test]
fn q_mime_filter_is_anded_on() {
    let q = build_q(None, None, Some("recent"), Some("image/"));
    assert_eq!(q, "trashed=false and mimeType contains 'image/'");
}

#[test]
fn order_for_recent_is_recency() {
    assert_eq!(order_for(Some("recent"), None), "modifiedTime desc");
    assert_eq!(order_for(None, None), "folder,modifiedTime desc");
}

// ── is_folder mapping ────────────────────────────────────────────────────

fn make_raw(mime: &str) -> RawFile {
    RawFile {
        id: "x".into(),
        name: "x".into(),
        mime_type: mime.into(),
        modified_time: String::new(),
        web_view_link: String::new(),
        size: String::new(),
        icon_link: String::new(),
        thumbnail_link: String::new(),
        parents: vec![],
    }
}

#[test]
fn is_folder_true_for_folder_mime() {
    let f = raw_to_drive_file(make_raw("application/vnd.google-apps.folder"));
    assert!(f.is_folder);
}

#[test]
fn is_folder_false_for_doc() {
    let f = raw_to_drive_file(make_raw("application/vnd.google-apps.document"));
    assert!(!f.is_folder);
}

#[test]
fn is_folder_false_for_binary() {
    let f = raw_to_drive_file(make_raw("application/pdf"));
    assert!(!f.is_folder);
}

// ── export_mime mapping ──────────────────────────────────────────────────

#[test]
fn export_mime_document_is_pdf() {
    assert_eq!(
        export_mime("application/vnd.google-apps.document"),
        Some("application/pdf")
    );
}

#[test]
fn attachment_download_export_name_and_mime_for_google_docs() {
    assert_eq!(
        attachment_name_and_mime("Budget", "application/vnd.google-apps.spreadsheet"),
        (
            "Budget.xlsx".to_string(),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".to_string()
        )
    );
    assert_eq!(
        attachment_name_and_mime("Deck.pptx", "application/vnd.google-apps.presentation"),
        ("Deck.pdf".to_string(), "application/pdf".to_string())
    );
    assert_eq!(
        attachment_name_and_mime("notes.txt", "text/plain"),
        ("notes.txt".to_string(), "text/plain".to_string())
    );
}

#[test]
fn export_mime_spreadsheet_is_xlsx() {
    assert_eq!(
        export_mime("application/vnd.google-apps.spreadsheet"),
        Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet")
    );
}

#[test]
fn export_mime_presentation_is_pdf() {
    assert_eq!(
        export_mime("application/vnd.google-apps.presentation"),
        Some("application/pdf")
    );
}

#[test]
fn export_mime_unknown_google_apps_falls_back_to_pdf() {
    // Any future google-apps type we haven't explicitly mapped gets PDF.
    assert_eq!(
        export_mime("application/vnd.google-apps.unknownfuturetype"),
        Some("application/pdf")
    );
}

#[test]
fn export_mime_non_google_not_called_but_returns_none_would_be_caught() {
    // export_mime is only called when mime starts with google-apps, but
    // the binary branch doesn't call it at all. Confirm the function
    // returns Some (fallback PDF) for any google-apps prefix.
    // (No assertion needed beyond the above tests — this is a note.)
    let _ = export_mime("application/vnd.google-apps.document");
}
