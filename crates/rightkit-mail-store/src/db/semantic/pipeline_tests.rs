//! Pipeline-key reindexing (schema v20): a bundle with the same model id but a
//! different pipeline must re-embed, and search must not read vectors made by
//! another pipeline. Uses the fake embedder seam; no model files, no ONNX.

use rightkit_db::rusqlite::{params, Connection};

use super::embedder::{index_key_for, Embedder, STORED_DIM};
use super::index::test_support::{conn_with_schema, seed_messages, FakeEmbedder};
use super::index::{prepare_step, run_index_step, StepPlan};
use super::query::{embed_query_vec, hybrid_search_with_vec};
use super::storage::{get_index_state, needs_reindex};

const MODEL: &str = "fake-v1";

/// Same model id every time; only the pipeline id varies.
struct PipeEmbedder {
    inner: FakeEmbedder,
    pipeline: &'static str,
}

fn embedder(pipeline: &'static str) -> PipeEmbedder {
    PipeEmbedder {
        inner: FakeEmbedder {
            dim: STORED_DIM,
            model_id: MODEL,
        },
        pipeline,
    }
}

impl Embedder for PipeEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
        self.inner.embed(text)
    }
    fn embed_query(&self, text: &str) -> Result<Vec<f32>, String> {
        self.inner.embed_query(text)
    }
    fn model_id(&self) -> &'static str {
        self.inner.model_id()
    }
    fn pipeline_id(&self) -> String {
        self.pipeline.to_string()
    }
}

fn vec_count(conn: &Connection) -> i64 {
    conn.query_row("SELECT count(*) FROM message_vec", [], |r| r.get(0))
        .unwrap()
}

fn indexed(pipeline: &'static str, n: usize) -> Connection {
    let conn = conn_with_schema();
    seed_messages(&conn, "a1", n);
    run_index_step(&conn, &conn, "a1", &embedder(pipeline)).unwrap();
    conn
}

#[test]
fn key_covers_pipeline_and_our_truncation() {
    let key = embedder("rkpipe1-abc").index_key();
    assert_eq!(key, index_key_for("rkpipe1-abc"));
    assert!(key.starts_with("rkpipe1-abc|"), "{key}");
    assert!(key.ends_with(&format!("mr-trunc{STORED_DIM}-l2")), "{key}");
    assert_ne!(key, embedder("rkpipe1-abd").index_key());
}

#[test]
fn fresh_account_is_not_stale_and_records_the_key() {
    let conn = conn_with_schema();
    seed_messages(&conn, "a1", 3);
    let e = embedder("p1");
    let fresh = get_index_state(&conn, "a1").unwrap();
    assert!(!needs_reindex(&fresh, MODEL, STORED_DIM, &e.index_key()));
    assert_eq!(run_index_step(&conn, &conn, "a1", &e).unwrap(), 3);
    let st = get_index_state(&conn, "a1").unwrap();
    assert_eq!(st.pipeline_key.as_deref(), Some(e.index_key().as_str()));
}

#[test]
fn same_pipeline_does_not_reindex() {
    let conn = indexed("p1", 3);
    let e = embedder("p1");
    let st = get_index_state(&conn, "a1").unwrap();
    assert!(!needs_reindex(&st, MODEL, STORED_DIM, &e.index_key()));
    assert_eq!(run_index_step(&conn, &conn, "a1", &e).unwrap(), 0);
    assert_eq!(vec_count(&conn), 3);
}

#[test]
fn changed_pipeline_with_same_model_id_deletes_vectors_and_reembeds() {
    let conn = indexed("p1", 3);
    let e2 = embedder("p2");
    let st = get_index_state(&conn, "a1").unwrap();
    assert!(needs_reindex(&st, MODEL, STORED_DIM, &e2.index_key()));

    let plan = prepare_step(&conn, &conn, "a1", MODEL, &e2.index_key()).unwrap();
    assert!(matches!(plan, StepPlan::Embed(_)));
    assert_eq!(vec_count(&conn), 0, "old-pipeline vectors are deleted");
    assert_eq!(get_index_state(&conn, "a1").unwrap().pipeline_key, None);

    assert_eq!(run_index_step(&conn, &conn, "a1", &e2).unwrap(), 3);
    let st = get_index_state(&conn, "a1").unwrap();
    assert_eq!(st.pipeline_key.as_deref(), Some(e2.index_key().as_str()));
    assert!(st.done);
    assert_eq!(vec_count(&conn), 3);
}

#[test]
fn legacy_row_with_null_pipeline_key_reindexes() {
    let conn = indexed("p1", 3);
    conn.execute("UPDATE semantic_index_state SET pipeline_key = NULL", [])
        .unwrap();
    let e = embedder("p1");
    let st = get_index_state(&conn, "a1").unwrap();
    assert_eq!(st.pipeline_key, None);
    assert!(needs_reindex(&st, MODEL, STORED_DIM, &e.index_key()));
    assert_eq!(run_index_step(&conn, &conn, "a1", &e).unwrap(), 3);
    assert_eq!(
        get_index_state(&conn, "a1")
            .unwrap()
            .pipeline_key
            .as_deref(),
        Some(e.index_key().as_str())
    );
}

#[test]
fn search_is_refused_while_the_stored_pipeline_differs() {
    let conn = indexed("p1", 3);
    for i in 0..3 {
        conn.execute(
            "INSERT INTO threads(account_id, id, subject, snippet, last_message_at, message_count)
             VALUES ('a1', ?1, 'subject', '', 0, 1)",
            params![format!("t{i}")],
        )
        .unwrap();
    }
    let (e1, e2) = (embedder("p1"), embedder("p2"));
    let search = |e: &PipeEmbedder, account: Option<&str>| {
        let q = embed_query_vec("zzzz unmatched", e).unwrap().unwrap();
        hybrid_search_with_vec(
            &conn,
            "zzzz unmatched",
            account,
            None,
            None,
            &q,
            &e.index_key(),
        )
        .unwrap()
    };
    assert!(
        !search(&e1, None).is_empty(),
        "matching pipeline serves results"
    );
    assert!(
        search(&e2, None).is_empty(),
        "mismatched pipeline is refused"
    );
    assert!(search(&e2, Some("a1")).is_empty());

    run_index_step(&conn, &conn, "a1", &e2).unwrap();
    assert!(
        !search(&e2, None).is_empty(),
        "served again once re-embedded"
    );
}
