mod common;
use common::*;
use rightkit_timeline::*;

fn edited() -> TimelineStore {
    let mut s = store_with(vec![
        video_source("a", 20.0, true),
        video_source("b", 20.0, false),
        audio_source("voice", 30.0),
        insert("c1", "a", 0, 0.0, 2.0),
        insert("c2", "b", 1, 0.0, 3.0),
        narration("n", "voice", 2.0, 0.5, 4.0, Some("c2")),
    ]);
    s.apply(Action::SetUnder {
        audio_id: "n".into(),
        under: UnderAudio::Duck {
            amount: MilliDb::from_db(12.0),
        },
    })
    .unwrap();
    s.apply(Action::MoveClip {
        clip_id: "c2".into(),
        to_index: 0,
    })
    .unwrap();
    s
}

#[test]
fn json_round_trip_is_byte_identical() {
    let s = edited();
    let bytes = s.to_canonical_json();
    assert!(bytes.ends_with(b"\n"));
    let reopened = TimelineStore::from_canonical_json(&bytes).unwrap();
    assert_eq!(reopened.to_canonical_json(), bytes);
    assert_eq!(reopened.timeline(), s.timeline());
    assert_eq!(reopened.revisions(), s.revisions());
    assert_eq!(reopened.cursor(), s.cursor());
}

#[test]
fn reopen_restores_history_and_cursor() {
    let mut s = edited();
    let head = s.head_hash();
    s.undo().unwrap();
    s.undo().unwrap();
    let mid = s.head_hash();
    let bytes = s.to_canonical_json();
    let mut r = TimelineStore::from_canonical_json(&bytes).unwrap();
    assert_eq!(r.head_hash(), mid);
    assert!(r.can_redo());
    assert_eq!(r.to_canonical_json(), bytes);
    r.redo().unwrap();
    r.redo().unwrap();
    assert_eq!(r.head_hash(), head);
    // and undo still works after a reopen
    let mut r2 = TimelineStore::from_canonical_json(&r.to_canonical_json()).unwrap();
    r2.undo().unwrap();
    r2.undo().unwrap();
    assert_eq!(r2.head_hash(), mid);
}

#[test]
fn canonical_document_shape() {
    let s = store_with(vec![video_source("a", 1.0, false)]);
    let text = String::from_utf8(s.to_canonical_json()).unwrap();
    assert!(text.starts_with(
        "{\n  \"schema_version\": 1,\n  \"kind\": \"cutright.timeline\",\n  \"fps\": {"
    ));
    let keys: Vec<usize> = ["schema_version", "kind", "fps", "cursor", "revisions"]
        .iter()
        .map(|k| text.find(&format!("\"{k}\"")).unwrap())
        .collect();
    assert!(keys.windows(2).all(|w| w[0] < w[1]), "stable key order");
}

#[test]
fn tampering_is_detected() {
    let text = String::from_utf8(edited().to_canonical_json()).unwrap();
    // change an action's payload without fixing the recorded hash
    let tampered = text.replacen("\"to_index\": 0", "\"to_index\": 1", 1);
    assert_ne!(tampered, text);
    assert!(matches!(
        TimelineStore::from_canonical_json(tampered.as_bytes()),
        Err(TimelineError::Integrity { .. })
    ));
    let wrong_version = text.replacen("\"schema_version\": 1", "\"schema_version\": 2", 1);
    assert_eq!(
        TimelineStore::from_canonical_json(wrong_version.as_bytes()).err(),
        Some(TimelineError::SchemaVersion(2))
    );
    let bad_source = text.replacen(&sid("a").to_string(), "NOTHEX", 1);
    assert!(TimelineStore::from_canonical_json(bad_source.as_bytes()).is_err());
    assert!(TimelineStore::from_canonical_json(b"{").is_err());
}
