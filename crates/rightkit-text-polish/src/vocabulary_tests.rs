use super::*;

#[test]
fn validate_rejects_empty_too_long_and_newlines() {
    assert!(validate("").is_err());
    assert!(validate("   ").is_err());
    assert!(validate(&"a".repeat(201)).is_err());
    assert!(validate("a\nb").is_err());
    assert!(validate("hello world").is_ok());
}

#[test]
fn normalize_trims() {
    assert_eq!(normalize("  hello  "), "hello");
}

#[test]
fn add_assigns_ids_and_rejects_case_insensitive_dupes() {
    let mut s = VocabularyStore::default();
    let a = s.add("Parakeet", 100).unwrap();
    assert_eq!(a.id, 1);
    assert_eq!(s.next_id, 2);
    // case-insensitive duplicate rejected
    assert!(s.add("parakeet", 200).is_err());
    let b = s.add("Cloudflare", 300).unwrap();
    assert_eq!(b.id, 2);
    assert_eq!(s.entries.len(), 2);
}

#[test]
fn remove_reports_whether_it_removed() {
    let mut s = VocabularyStore::default();
    s.add("term", 1).unwrap();
    let id = s.entries[0].id;
    assert!(s.remove(id));
    assert!(!s.remove(id));
    assert!(s.entries.is_empty());
}

#[test]
fn import_skips_existing_invalid_and_empty() {
    let mut s = VocabularyStore::default();
    s.add("existing", 1).unwrap();
    let added = s.import(
        vec![
            "existing".into(), // dup -> skip
            "  ".into(),       // empty -> skip
            "a".repeat(201),   // too long -> skip
            "fresh".into(),    // added
        ],
        500,
    );
    assert_eq!(added.len(), 1);
    assert_eq!(added[0].term, "fresh");
    assert_eq!(added[0].created_at_ms, 500);
}

#[test]
fn import_dedupes_within_one_batch() {
    // The same term repeated in a single import must be added once, not once
    // per occurrence (case-insensitive).
    let mut store = VocabularyStore::default();
    let added = store.import(
        vec![
            "Kubernetes".into(),
            "kubernetes".into(),
            "KUBERNETES".into(),
        ],
        10,
    );
    assert_eq!(added.len(), 1, "in-batch duplicates must collapse to one");
    assert_eq!(store.entries.len(), 1);
}

#[test]
fn add_with_sounds_like_roundtrips_and_reads_legacy_json() {
    let mut store = VocabularyStore::default();
    let entry = store
        .add_with_aliases("Wispr Flow", vec!["whisper flow".into()], 1)
        .unwrap();
    assert_eq!(entry.sounds_like, vec!["whisper flow"]);

    let legacy: VocabularyEntry =
        serde_json::from_str(r#"{"id":1,"term":"HeardRight","created_at_ms":0}"#).unwrap();
    assert!(legacy.sounds_like.is_empty());
}

#[test]
fn single_common_word_is_not_a_safe_sound_alike() {
    let mut store = VocabularyStore::default();
    let entry = store
        .add_with_aliases("Slack", vec!["slack".into()], 1)
        .unwrap();
    assert!(entry.sounds_like.is_empty());
}

#[test]
fn suggest_all_dedupes_and_increments_seen() {
    use crate::vocab_learn::Suggestion;
    let mut store = VocabularyStore::default();
    let a = store.suggest_all(
        vec![Suggestion {
            term: "HeardRight".into(),
            heard_as: "heardright".into(),
        }],
        100,
    );
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].seen, 1);
    // Same term again bumps `seen`, doesn't add a row.
    let b = store.suggest_all(
        vec![Suggestion {
            term: "HeardRight".into(),
            heard_as: "heardright".into(),
        }],
        200,
    );
    assert_eq!(b.len(), 1);
    assert_eq!(b[0].seen, 2);
    assert_eq!(store.suggestions().len(), 1);
}

#[test]
fn accept_suggestion_merges_alias_into_existing_term() {
    use crate::vocab_learn::Suggestion;
    let mut store = VocabularyStore::default();
    store.add("HeardRight", 1).unwrap();
    store.suggest_all(
        vec![Suggestion {
            term: "heardright".into(),
            heard_as: "heard right".into(),
        }],
        2,
    );
    let entry = store.accept_suggestion("heardright", 3).unwrap();
    assert_eq!(entry.term, "HeardRight");
    assert_eq!(entry.sounds_like, vec!["heard right".to_string()]);
    assert!(store.suggestions().is_empty());
}

#[test]
fn dismiss_suggestion_removes_only_the_named_term() {
    use crate::vocab_learn::Suggestion;
    let mut store = VocabularyStore::default();
    store.suggest_all(
        vec![
            Suggestion {
                term: "A".into(),
                heard_as: "a".into(),
            },
            Suggestion {
                term: "B".into(),
                heard_as: "b".into(),
            },
        ],
        1,
    );
    assert!(store.dismiss_suggestion("a"));
    assert_eq!(store.suggestions().len(), 1);
    assert_eq!(store.suggestions()[0].term, "B");
}
