//! Cortex admission through the production store, with deterministic stored vectors.
//! Fixture embeddings isolate admission policy from model availability/inference.

use rightkit_context_runtime::cortex_lifecycle::{agent_memory_push, resolve_memory};
use rightkit_context_runtime::store::provenance_support_count;
use rightkit_context_runtime::{ApprovedProposalAdmissionV1, MemDb, MemoryStore};
use rightkit_db::rusqlite;
use rightkit_memory_core::{Embedder, HashEmbedder, MemoryTier};
use serde_json::{json, Value};
use std::sync::Arc;

const SCOPE: &str = "paraphrase-scope";
const ORIGINAL: &str = "Keep database backups on a daily schedule.";
const PARAPHRASE: &str = "Save a copy of stored records every day.";

// Same setup as e2e_embedder_swap: a supplied fixture backend, temporary SQLite,
// no process environment changes or model downloads.
struct FixtureEmbedder;

impl Embedder for FixtureEmbedder {
    fn embed(&self, _text: &str) -> Vec<f32> {
        let mut vector = vec![0.0; 8];
        vector[0] = 1.0;
        vector
    }

    fn dim(&self) -> usize {
        8
    }

    fn model_id(&self) -> &'static str {
        "paraphrase-admission-fixture"
    }
}

fn open(embedder: Arc<dyn Embedder>) -> (tempfile::TempDir, MemoryStore) {
    let dir = tempfile::tempdir().expect("temporary Cortex root");
    let store = MemoryStore::try_open_with_embedder(
        MemDb::open(dir.path().join("cortex.sqlite3")).expect("open Cortex SQLite"),
        embedder,
    )
    .expect("open MemoryStore");
    (dir, store)
}

fn fixture() -> (tempfile::TempDir, MemoryStore) {
    open(Arc::new(FixtureEmbedder))
}

fn put(store: &MemoryStore, name: &str, text: &str, scope: &str, refs: &[&str]) -> String {
    store
        .try_put_with_metadata(
            name,
            text,
            scope,
            MemoryTier::Semantic,
            "2026-10-08T00:00:00Z",
            &refs
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<Vec<_>>(),
        )
        .expect("admit memory")
}

fn count(store: &MemoryStore, table: &str) -> i64 {
    assert!(matches!(table, "memories" | "memory_quarantine"));
    store
        .db()
        .lock()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .expect("count durable rows")
}

fn disposition_event(store: &MemoryStore, kind: &str) -> Value {
    let encoded: String = store
        .db()
        .lock()
        .query_row(
            "SELECT meta FROM memory_event_log WHERE event_kind=?1 ORDER BY event_id DESC LIMIT 1",
            [kind],
            |row| row.get(0),
        )
        .expect("durable disposition event");
    serde_json::from_str(&encoded).expect("disposition JSON")
}

#[test]
fn paraphrase_merges_provenance_then_noops_without_rewriting_original() {
    let (_dir, store) = fixture();
    let id = put(&store, "original", ORIGINAL, SCOPE, &["source-one"]);
    assert_eq!(
        put(
            &store,
            "reworded",
            PARAPHRASE,
            SCOPE,
            &["source-two", "source-one"]
        ),
        id
    );
    let event = disposition_event(&store, "admission_update_metadata_only");
    assert_eq!(event["disposition"], "update_metadata_only");
    assert_eq!(event["exact"], false);
    assert!(event["similarity"].as_f64().unwrap() >= 0.92);

    assert_eq!(
        put(&store, "repeated", PARAPHRASE, SCOPE, &["source-two"]),
        id
    );
    assert_eq!(
        disposition_event(&store, "admission_no_op")["disposition"],
        "no_op"
    );
    assert_eq!(count(&store, "memories"), 1);
    assert_eq!(count(&store, "memory_quarantine"), 0);
    let (body, refs): (String, String) = store
        .db()
        .lock()
        .query_row(
            "SELECT content,source_ids FROM memories WHERE id=?1",
            [&id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(body.as_bytes(), ORIGINAL.as_bytes());
    let refs: Vec<String> = serde_json::from_str(&refs).unwrap();
    assert_eq!(refs, vec!["source-one", "source-two"]);
    assert_eq!(provenance_support_count(&refs), 2);
}

fn assert_quarantined(original: &str, incoming: &str) {
    let (_dir, store) = fixture();
    let id = put(&store, "original", original, SCOPE, &["source-one"]);
    assert!(store
        .try_put("changed", incoming, SCOPE, MemoryTier::Semantic)
        .is_err());
    assert_eq!(count(&store, "memories"), 1);
    assert_eq!(count(&store, "memory_quarantine"), 1);
    let event = disposition_event(&store, "admission_conflict_quarantined");
    assert_eq!(event["disposition"], "conflict_quarantined");
    assert_eq!(event["conflicts_with"], id);
    assert!(event["similarity"].as_f64().unwrap() >= 0.92);
    let quarantined: String = store
        .db()
        .lock()
        .query_row("SELECT content FROM memory_quarantine", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(quarantined.as_bytes(), incoming.as_bytes());
    let canonical: String = store
        .db()
        .lock()
        .query_row("SELECT content FROM memories WHERE id=?1", [&id], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(canonical.as_bytes(), original.as_bytes());
}

#[test]
fn number_difference_stays_out_of_active_truth() {
    assert_quarantined(
        "Keep database backups for 30 days.",
        "Retain stored record copies for 60 days.",
    );
}

#[test]
fn negation_difference_stays_out_of_active_truth() {
    assert_quarantined(ORIGINAL, "Never save a copy of stored records every day.");
}

#[test]
fn different_scope_never_merges() {
    let (_dir, store) = fixture();
    let first = put(&store, "original", ORIGINAL, SCOPE, &[]);
    let second = put(&store, "reworded", PARAPHRASE, "other-scope", &[]);
    assert_ne!(first, second);
    assert_eq!(count(&store, "memories"), 2);
    assert_eq!(count(&store, "memory_quarantine"), 0);
}

#[test]
fn semantic_embedder_absent_preserves_hash_backend_lexical_behavior() {
    let (_dir, store) = open(Arc::new(HashEmbedder::new()));
    let first = put(&store, "original", ORIGINAL, SCOPE, &[]);
    let second = put(&store, "reworded", PARAPHRASE, SCOPE, &[]);
    assert_ne!(first, second);
    assert_eq!(
        put(
            &store,
            "exact",
            "  KEEP database backups on a daily schedule.  ",
            SCOPE,
            &[]
        ),
        first
    );
    assert_eq!(count(&store, "memories"), 2);
    assert_eq!(count(&store, "memory_quarantine"), 0);
}

#[test]
fn missing_stored_embeddings_preserve_lexical_only_behavior() {
    let (_dir, store) = fixture();
    let first = put(&store, "original", ORIGINAL, SCOPE, &[]);
    store
        .db()
        .lock()
        .execute("UPDATE memories SET embedding_q=NULL", [])
        .unwrap();
    assert_ne!(put(&store, "reworded", PARAPHRASE, SCOPE, &[]), first);
    assert_eq!(put(&store, "exact", ORIGINAL, SCOPE, &[]), first);
    assert_eq!(count(&store, "memories"), 2);
}

#[test]
fn foreign_model_embeddings_are_not_semantic_candidates() {
    let (_dir, store) = fixture();
    let first = put(&store, "original", ORIGINAL, SCOPE, &[]);
    store.tamper_embed_model_for_tests(&first, "different-model");
    assert_ne!(put(&store, "reworded", PARAPHRASE, SCOPE, &[]), first);
    assert_eq!(count(&store, "memories"), 2);
}

#[test]
fn cosine_below_floor_is_not_a_candidate_even_with_material_difference() {
    let (_dir, store) = fixture();
    let first = put(&store, "original", ORIGINAL, SCOPE, &[]);
    // Incoming fixture vectors are [1, 0, ...], so this stored vector has cosine 0.90.
    let mut vector = [0.0_f32; 8];
    vector[0] = 0.90;
    vector[1] = (1.0_f32 - 0.90_f32 * 0.90_f32).sqrt();
    let blob = rightkit_memory_core::QuantizedVector::quantize(&vector).to_bytes();
    store
        .db()
        .lock()
        .execute(
            "UPDATE memories SET embedding_q=?2 WHERE id=?1",
            rusqlite::params![&first, blob],
        )
        .unwrap();
    let second = put(
        &store,
        "reworded",
        "Never save a copy of stored records every day.",
        SCOPE,
        &[],
    );
    assert_ne!(first, second);
    assert_eq!(count(&store, "memories"), 2);
    assert_eq!(count(&store, "memory_quarantine"), 0);
}

#[test]
fn inactive_memories_are_not_semantic_candidates() {
    let (_dir, store) = fixture();
    let first = put(&store, "original", ORIGINAL, SCOPE, &[]);
    store
        .db()
        .lock()
        .execute(
            "UPDATE memories SET lifecycle_state='retired' WHERE id=?1",
            [&first],
        )
        .unwrap();
    assert_ne!(put(&store, "reworded", PARAPHRASE, SCOPE, &[]), first);
    assert_eq!(count(&store, "memories"), 2);
    assert_eq!(count(&store, "memory_quarantine"), 0);
}

#[test]
fn approved_proposal_preserves_semantic_duplicate_and_conflict_dispositions() {
    let (_dir, store) = fixture();
    let id = put(&store, "original", ORIGINAL, SCOPE, &[]);
    let duplicate = store
        .admit_approved_proposal(
            "duplicate-proposal",
            &json!({"text":PARAPHRASE,"scopeId":SCOPE}).to_string(),
        )
        .unwrap();
    assert!(matches!(
        duplicate, ApprovedProposalAdmissionV1::Duplicate { existing_id } if existing_id == id
    ));
    let conflict = store
        .admit_approved_proposal(
            "conflict-proposal",
            &json!({"text":"Never save a copy of stored records every day.","scopeId":SCOPE})
                .to_string(),
        )
        .unwrap();
    assert!(matches!(
        conflict, ApprovedProposalAdmissionV1::Conflict { existing_id } if existing_id == id
    ));
    assert_eq!(count(&store, "memories"), 1);
    assert_eq!(count(&store, "memory_quarantine"), 1);
}

#[test]
fn merged_agent_pushes_recover_each_original_body_byte_for_byte() {
    let (_dir, store) = fixture();
    let original = format!("  {ORIGINAL}\n");
    let reworded = format!("{PARAPHRASE}\r\n");
    let first = agent_memory_push(
        &store,
        "fixture-repo",
        SCOPE,
        "first",
        "fixture-agent",
        &original,
    )
    .unwrap();
    let second = agent_memory_push(
        &store,
        "fixture-repo",
        SCOPE,
        "second",
        "fixture-agent",
        &reworded,
    )
    .unwrap();
    assert_eq!(first["memoryId"], second["memoryId"]);
    assert_eq!(count(&store, "memories"), 1);
    for (receipt, submitted) in [(&first, &original), (&second, &reworded)] {
        let resolved = resolve_memory(
            &store,
            SCOPE,
            receipt["memoryId"].as_str().unwrap(),
            receipt["contentHash"].as_str().unwrap(),
            0,
            12_000,
        )
        .unwrap();
        assert_eq!(
            resolved["content"].as_str().unwrap().as_bytes(),
            submitted.as_bytes()
        );
    }
    let sources: i64 = store
        .db()
        .lock()
        .query_row(
            "SELECT COUNT(*) FROM cortex_agent_memory_source_v1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(sources, 2);
}
