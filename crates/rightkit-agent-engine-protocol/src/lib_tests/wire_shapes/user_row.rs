use crate::*;

#[test]
fn current_user_row_serializes_without_trust_when_none() {
    let msg = TranscriptMessage::User {
        id: "u-current-1".into(),
        created_at: "2026-07-20T00:00:00Z".into(),
        content: "current prompt".into(),
        trust: None,
        pinned: false,
        attachments: Vec::new(),
    };

    let json = serde_json::to_string(&msg).unwrap();
    // `skip_serializing_if = "Option::is_none"` for trust: the serialized
    // form must NOT contain the trust key when None. The keys that ARE
    // present must remain camelCase.
    assert!(json.contains(r#""$kind":"user""#), "got: {json}");
    assert!(
        json.contains(r#""createdAt":"2026-07-20T00:00:00Z""#),
        "got: {json}"
    );
    assert!(
        json.contains(r#""content":"current prompt""#),
        "got: {json}"
    );
    assert!(
        !json.contains("trust"),
        "trust=None must be skipped: {json}"
    );
}

#[test]
fn current_user_row_with_trust_round_trips() {
    let msg = TranscriptMessage::User {
        id: "u-current-2".into(),
        created_at: "2026-07-20T00:00:00Z".into(),
        content: "trusted current prompt".into(),
        trust: Some(TrustEnvelope {
            trust: ContentTrust::User,
            source: "desktop".into(),
            instructions_are_data: false,
            content_hash: "deadbeef".into(),
        }),
        pinned: true,
        attachments: Vec::new(),
    };

    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains(r#""trust":"user""#), "got: {json}");
    assert!(json.contains(r#""contentHash":"deadbeef""#), "got: {json}");
    assert!(
        json.contains(r#""instructionsAreData":false"#),
        "got: {json}"
    );

    let parsed: TranscriptMessage = serde_json::from_str(&json).unwrap();
    match parsed {
        TranscriptMessage::User {
            id,
            content,
            trust,
            pinned,
            ..
        } => {
            assert_eq!(id, "u-current-2");
            assert_eq!(content, "trusted current prompt");
            assert!(pinned);
            assert!(trust.is_some());
        }
        other => panic!("expected TranscriptMessage::User, got: {other:?}"),
    }
}

#[test]
fn reference_meta_round_trips_with_camel_case_fields() {
    use crate::references::{ReferenceMeta, ReferenceProvenance};

    let meta = ReferenceMeta {
        id: "ref-1".into(),
        kind: "workspace_file".into(),
        destination: "/tmp/foo.txt".into(),
        mime: Some("text/plain".into()),
        filename: Some("foo.txt".into()),
        size_bytes: Some(42),
        provenance: ReferenceProvenance {
            source: "test".into(),
            created_at: "2026-07-20T00:00:00Z".into(),
            trust: "user".into(),
            content_hash: Some("hash".into()),
        },
        actions: vec!["open".into(), "preview".into()],
    };

    let json = serde_json::to_string(&meta).unwrap();
    assert!(json.contains(r#""sizeBytes":42"#), "got: {json}");
    assert!(json.contains(r#""contentHash":"hash""#), "got: {json}");
    assert!(
        json.contains(r#""createdAt":"2026-07-20T00:00:00Z""#),
        "got: {json}"
    );
    assert!(json.contains(r#""mime":"text/plain""#), "got: {json}");

    let parsed: ReferenceMeta = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.id, "ref-1");
    assert_eq!(parsed.size_bytes, Some(42));
    assert_eq!(parsed.provenance.content_hash.as_deref(), Some("hash"));
}
