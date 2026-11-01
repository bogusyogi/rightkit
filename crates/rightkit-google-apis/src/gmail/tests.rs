use super::*;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

#[test]
fn sync_covers_unread_label_for_old_mail() {
    // Regression: a full sync must list the UNREAD label, not rely on the
    // INBOX window. Without it, unread mail older than the 50 newest inbox
    // messages is never mirrored — undercounting the unread badge and
    // emptying the Unread view (the 3-vs-8 bug).
    assert!(
        SYNC_FOLDERS.iter().any(|(label, _)| *label == "UNREAD"),
        "UNREAD must be a synced system folder"
    );
}

#[test]
fn parse_from_name_and_addr() {
    let (n, a) = parse_from("Jane Doe <jane@x.com>");
    assert_eq!(n, "Jane Doe");
    assert_eq!(a, "jane@x.com");
}

#[test]
fn parse_from_quoted_name() {
    let (n, a) = parse_from("\"Doe, Jane\" <jane@x.com>");
    assert_eq!(n, "Doe, Jane");
    assert_eq!(a, "jane@x.com");
}

#[test]
fn parse_from_bare_addr() {
    let (n, a) = parse_from("jane@x.com");
    assert_eq!(n, "jane@x.com");
    assert_eq!(a, "jane@x.com");
}

#[test]
fn b64url_decodes_padded_and_unpadded() {
    assert_eq!(decode_b64url("SGVsbG8"), b"Hello");
    assert_eq!(decode_b64url("SGVsbG8="), b"Hello");
}

#[test]
fn b64url_handles_urlsafe_chars() {
    assert_eq!(decode_b64url("-_8"), vec![0xFB, 0xFF]);
}

#[test]
fn walk_extracts_plain_and_html() {
    let plain = B64.encode("hello text").replace('+', "-").replace('/', "_");
    let html = B64.encode("<p>hi</p>").replace('+', "-").replace('/', "_");
    let root = Part {
        mime_type: "multipart/alternative".into(),
        filename: String::new(),
        headers: vec![],
        body: None,
        parts: vec![
            Part {
                mime_type: "text/plain".into(),
                filename: String::new(),
                headers: vec![],
                body: Some(PartBody {
                    attachment_id: None,
                    size: None,
                    data: plain,
                }),
                parts: vec![],
            },
            Part {
                mime_type: "text/html".into(),
                filename: String::new(),
                headers: vec![],
                body: Some(PartBody {
                    attachment_id: None,
                    size: None,
                    data: html,
                }),
                parts: vec![],
            },
        ],
    };
    let (mut p, mut h, mut att) = (None, None, Vec::new());
    walk_parts(&root, &mut p, &mut h, &mut att);
    assert_eq!(p.as_deref(), Some("hello text"));
    assert_eq!(h.as_deref(), Some("<p>hi</p>"));
    assert!(att.is_empty());
}

#[test]
fn walk_collects_attachment_names() {
    let root = Part {
        mime_type: "multipart/mixed".into(),
        filename: String::new(),
        headers: vec![],
        body: None,
        parts: vec![Part {
            mime_type: "application/pdf".into(),
            filename: "doc.pdf".into(),
            headers: vec![],
            body: Some(PartBody {
                attachment_id: Some("att1".into()),
                size: Some(42),
                data: String::new(),
            }),
            parts: vec![],
        }],
    };
    let (mut p, mut h, mut att) = (None, None, Vec::new());
    walk_parts(&root, &mut p, &mut h, &mut att);
    assert_eq!(att.len(), 1);
    assert_eq!(att[0].filename, "doc.pdf");
    assert_eq!(att[0].attachment_id, "att1");
}

#[test]
fn history_changes_cover_all_four_event_types() {
    let rec = HistoryRecord {
        messages_added: vec![HistMsgWrap {
            message: HistMsgRef { id: "a".into() },
        }],
        labels_added: vec![HistMsgWrap {
            message: HistMsgRef { id: "b".into() },
        }],
        labels_removed: vec![HistMsgWrap {
            message: HistMsgRef { id: "c".into() },
        }],
        messages_deleted: vec![HistMsgWrap {
            message: HistMsgRef { id: "d".into() },
        }],
    };
    let (changed, deleted) = collect_history_changes(&[rec]);
    // added + labels-added + labels-removed are all re-fetched (label events
    // matter — archive-on-phone is a labelsRemoved, and must be honored).
    assert_eq!(changed.len(), 3);
    assert!(changed.contains("a") && changed.contains("b") && changed.contains("c"));
    assert!(deleted.contains("d"));
}

#[test]
fn build_raw_mime_has_headers_and_body() {
    let m = build_raw_mime(
        "Me <me@x.com>",
        "you@x.com",
        "",
        "",
        "Hello",
        "body text",
        false,
        None,
        &[],
    );
    assert!(m.contains("From: Me <me@x.com>\r\n"));
    assert!(m.contains("To: you@x.com\r\n"));
    assert!(m.contains("Subject: Hello\r\n"));
    assert!(m.contains("Content-Type: text/plain; charset=\"UTF-8\"\r\n"));
    assert!(!m.contains("Cc:"), "no Cc when empty");
    assert!(m.ends_with("body text"));
}

#[test]
fn build_raw_mime_encodes_unicode_subject_and_threads() {
    let m = build_raw_mime(
        "me@x.com",
        "you@x.com",
        "c@x.com",
        "b@x.com",
        "café ☕",
        "hi",
        false,
        Some("<orig@mail>"),
        &[],
    );
    assert!(
        m.contains("Subject: =?UTF-8?B?"),
        "non-ASCII subject is encoded"
    );
    assert!(m.contains("Cc: c@x.com\r\n"));
    assert!(m.contains("Bcc: b@x.com\r\n"));
    assert!(m.contains("In-Reply-To: <orig@mail>\r\n"));
    assert!(m.contains("References: <orig@mail>\r\n"));
}

#[test]
fn build_raw_mime_html_sets_content_type() {
    let m = build_raw_mime(
        "me@x.com",
        "you@x.com",
        "",
        "",
        "Hi",
        "<p>hello <b>there</b></p>",
        true,
        None,
        &[],
    );
    assert!(m.contains("Content-Type: text/html; charset=\"UTF-8\"\r\n"));
    assert!(m.ends_with("<p>hello <b>there</b></p>"));
}

#[test]
fn build_raw_mime_multipart_with_attachment() {
    let att = OutgoingAttachment {
        filename: "report.pdf".into(),
        mime_type: "application/pdf".into(),
        data_b64: "SGVsbG8gd29ybGQ=".into(),
    };
    let m = build_raw_mime(
        "me@x.com",
        "you@x.com",
        "",
        "",
        "Files",
        "see attached",
        false,
        None,
        &[att],
    );
    assert!(m.contains("Content-Type: multipart/mixed; boundary=\""));
    assert!(m.contains("Content-Type: text/plain; charset=\"UTF-8\"\r\n"));
    assert!(m.contains("see attached"));
    assert!(m.contains("Content-Type: application/pdf; name=\"report.pdf\"\r\n"));
    assert!(m.contains("Content-Disposition: attachment; filename=\"report.pdf\"\r\n"));
    assert!(m.contains("Content-Transfer-Encoding: base64\r\n"));
    assert!(m.contains("SGVsbG8gd29ybGQ="));
    assert!(m.trim_end().ends_with("--"));
}

#[test]
fn deleted_message_is_not_refetched() {
    let rec = HistoryRecord {
        messages_added: vec![HistMsgWrap {
            message: HistMsgRef { id: "x".into() },
        }],
        messages_deleted: vec![HistMsgWrap {
            message: HistMsgRef { id: "x".into() },
        }],
        ..Default::default()
    };
    let (changed, deleted) = collect_history_changes(&[rec]);
    assert!(
        !changed.contains("x"),
        "deleted id removed from changed set"
    );
    assert!(deleted.contains("x"));
}
