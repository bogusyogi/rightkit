//! A1 coverage without model files, downloads or the production embed worker.
use rightkit_db::rusqlite;
use rightkit_repo_graph::hybrid_task::{
    document_prompt, fuse_task_ranks, query_prompt, symbol_units, vector_cache_key,
    DenseOmissionReason, DenseTaskIndex, DenseTaskResult, TaskEmbedder,
};
use rightkit_repo_graph::{
    graph, query, store, BlueprintRequest, Bounds, CancellationToken, GraphGeneration, GraphNode,
    Operation,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

struct FakeEmbedder {
    calls: AtomicUsize,
    fail_after: usize,
}

impl FakeEmbedder {
    fn complete() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            fail_after: usize::MAX,
        }
    }
}

impl TaskEmbedder for FakeEmbedder {
    fn fingerprint(&self) -> String {
        "fake-eg2-v1".into()
    }
    fn embed_prompt(&self, prompt: &str) -> Result<Vec<f32>, String> {
        if self.calls.fetch_add(1, Ordering::SeqCst) >= self.fail_after {
            return Err("budget exhausted".into());
        }
        assert!(
            prompt.starts_with("title: src/lib.rs | text: ")
                || prompt.starts_with("task: search result | query: ")
        );
        Ok(if prompt.contains("payment") {
            vec![3.0, 0.0]
        } else {
            vec![0.0, 2.0]
        })
    }
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

fn fixture() -> (tempfile::TempDir, GraphGeneration, rusqlite::Connection) {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("src")).unwrap();
    let body = "pub fn payment_total() {\n    settle();\n}\npub fn inventory_count() {}\n";
    std::fs::write(root.path().join("src/lib.rs"), body).unwrap();
    let hash = rightkit_repo_graph::identity::content_digest(body.as_bytes());
    let node =
        |id: &str, name: &str, kind: &str, start: usize, end: usize, qualified: &str| GraphNode {
            id: id.into(),
            name: Some(name.into()),
            kind: kind.into(),
            path: Some("src/lib.rs".into()),
            generation_id: "generation-1".into(),
            evidence: vec![json!({"path":"src/lib.rs","startLine":start,"endLine":end,
            "qualifiedName":qualified,"contentHash":hash})],
        };
    let generation = GraphGeneration {
        schema_version: graph::GRAPH_SCHEMA_VERSION,
        provider: "native-rust".into(),
        provider_version: graph::PROVIDER_VERSION.into(),
        generation_id: "generation-1".into(),
        source_hash: hash.clone(),
        repo_root: root.path().to_string_lossy().into_owned(),
        complete: true,
        nodes: vec![
            node("file", "lib.rs", "file", 1, 5, "src/lib.rs"),
            node("bare", "payment_total", "symbol", 1, 3, "payment_total"),
            node(
                "payment",
                "payment_total",
                "symbol",
                1,
                3,
                "Shop.payment_total",
            ),
            node(
                "inventory",
                "inventory_count",
                "symbol",
                4,
                4,
                "inventory_count",
            ),
            node("domain", "domain", "domain", 1, 5, "domain"),
            node("bridge", "bridge", "bridge", 1, 5, "bridge"),
        ],
        edges: Vec::new(),
        files: Vec::new(),
        truncation_reasons: Vec::new(),
    };
    let mut conn = store::open_store(None).unwrap();
    persist(&mut conn, &generation);
    (root, generation, conn)
}

fn persist(conn: &mut rusqlite::Connection, generation: &GraphGeneration) {
    let stored = store::Generation {
        schema_version: Some(graph::GRAPH_SCHEMA_VERSION),
        provider: Some(json!({"id":"native-rust"})),
        manifest: Some(json!({"generationId":generation.generation_id})),
        repo_root: Some(json!(generation.repo_root)),
        nodes: generation.nodes.iter().map(|node| json!(node)).collect(),
        ..Default::default()
    };
    store::save_generation(conn, &stored).unwrap();
}

fn has_omission(result: &DenseTaskResult, reason: DenseOmissionReason) -> bool {
    result
        .omissions
        .iter()
        .any(|omission| omission.reason == reason)
}

fn recall(generation: &GraphGeneration, dense: DenseTaskResult) -> Value {
    let mut request =
        BlueprintRequest::new("fake-recall", Operation::Recall, &generation.repo_root);
    request.input["task"] = json!("calculate payment amount");
    request.input["maxSeeds"] = json!(2);
    let context = request.validate(Bounds::default()).unwrap();
    query::execute_query_with_dense(generation, &request, &context, Some(dense)).unwrap()
}

#[test]
fn rrf_shared_support_beats_single_lane_first_place() {
    let bm25 = vec!["lexical".into(), "shared".into(), "third".into()];
    let dense = vec!["semantic".into(), "shared".into(), "third".into()];
    assert_eq!(
        fuse_task_ranks(&bm25, &dense, 4),
        vec!["shared", "third", "lexical", "semantic"]
    );
    assert_eq!(fuse_task_ranks(&bm25, &[], 2), vec!["lexical", "shared"]);
    assert!(fuse_task_ranks(&bm25, &dense, 0).is_empty());
}

#[test]
fn prompt_cache_key_is_stable_and_binds_model_title_and_body() {
    let prompt = document_prompt("src/lib.rs", "function payment_total\nsettle();");
    assert_eq!(
        prompt,
        "title: src/lib.rs | text: function payment_total\nsettle();"
    );
    assert_eq!(
        query_prompt("payment amount"),
        "task: search result | query: payment amount"
    );
    let key = vector_cache_key("model-1", &prompt);
    assert_eq!(key, vector_cache_key("model-1", &prompt));
    assert_eq!(key.1.len(), 64);
    assert_ne!(key, vector_cache_key("model-2", &prompt));
    assert_ne!(
        key,
        vector_cache_key(
            "model-1",
            &document_prompt("other.rs", "function payment_total\nsettle();")
        )
    );
    assert_ne!(key, vector_cache_key("model-1", &(prompt + "changed")));
}

#[test]
fn symbol_units_deduplicate_spans_prefer_qualified_names_and_skip_domains() {
    let (_root, generation, _conn) = fixture();
    let units = symbol_units(&generation, deadline(), &CancellationToken::new()).unwrap();
    assert_eq!(units.len(), 3);
    assert_eq!(units[1].id, "payment");
    assert_eq!(
        units[1].text,
        "symbol Shop.payment_total\npub fn payment_total() {\n    settle();\n}"
    );
    assert!(units[0].text.starts_with("file src/lib.rs\n"));
    assert!(units
        .iter()
        .all(|unit| unit.id != "bare" && unit.id != "domain" && unit.id != "bridge"));
}

#[test]
fn incomplete_coverage_never_ranks_dense_and_recall_emits_typed_omission() {
    let (_root, generation, conn) = fixture();
    let fake = FakeEmbedder {
        calls: AtomicUsize::new(0),
        fail_after: 1,
    };
    let result = DenseTaskIndex::default().run(
        &conn,
        &generation,
        Some(&fake),
        Some("payment amount"),
        8,
        deadline(),
        &CancellationToken::new(),
    );
    assert_eq!(result.units, 3);
    assert_eq!(result.embedded, 1);
    assert!(result.ranked_ids.is_empty());
    assert!(has_omission(
        &result,
        DenseOmissionReason::DenseIndexIncomplete
    ));
    let recalled = recall(&generation, result);
    assert!(recalled["omissions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o["reason"] == "dense_index_incomplete"));
    assert!(recalled["candidateSet"]["omissions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o["reason"] == "dense_index_incomplete"));
    assert_eq!(recalled["resolution"]["seeds"][0]["reason"], "bm25_task");
    let resumed = FakeEmbedder::complete();
    let complete = DenseTaskIndex::default().run(
        &conn,
        &generation,
        Some(&resumed),
        Some("payment amount"),
        8,
        deadline(),
        &CancellationToken::new(),
    );
    assert!(complete.omissions.is_empty());
    assert_eq!(complete.embedded, 3);
    assert_eq!(
        resumed.calls.load(Ordering::SeqCst),
        3,
        "two missing documents & one query"
    );
}

#[test]
fn missing_embedder_falls_back_without_loading_or_downloading_model() {
    let (_root, generation, conn) = fixture();
    let result = DenseTaskIndex::default().run(
        &conn,
        &generation,
        None,
        Some("payment amount"),
        8,
        deadline(),
        &CancellationToken::new(),
    );
    assert!(has_omission(&result, DenseOmissionReason::DenseUnavailable));
    assert!(has_omission(
        &result,
        DenseOmissionReason::DenseIndexIncomplete
    ));
    let recalled = recall(&generation, result);
    assert_eq!(recalled["resolution"]["seeds"][0]["reason"], "bm25_task");
    assert!(recalled["omissions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o["reason"] == "dense_unavailable"));
}

#[test]
fn vectors_survive_generation_replacement_and_in_memory_cache_reuse() {
    let (_root, mut generation, mut conn) = fixture();
    let fake = FakeEmbedder::complete();
    let mut index = DenseTaskIndex::default();
    let result = index.run(
        &conn,
        &generation,
        Some(&fake),
        None,
        8,
        deadline(),
        &CancellationToken::new(),
    );
    assert!(result.omissions.is_empty());
    assert_eq!(fake.calls.load(Ordering::SeqCst), 3);
    generation.generation_id = "generation-2".into();
    for node in &mut generation.nodes {
        node.generation_id = generation.generation_id.clone();
    }
    persist(&mut conn, &generation);
    let result = index.run(
        &conn,
        &generation,
        Some(&fake),
        Some("payment amount"),
        8,
        deadline(),
        &CancellationToken::new(),
    );
    assert!(result.omissions.is_empty());
    assert_eq!(
        fake.calls.load(Ordering::SeqCst),
        4,
        "only query re-embedded"
    );
    let result = index.run(
        &conn,
        &generation,
        Some(&fake),
        Some("payment amount"),
        8,
        deadline(),
        &CancellationToken::new(),
    );
    assert!(result.omissions.is_empty());
    assert_eq!(fake.calls.load(Ordering::SeqCst), 5);
    let recalled = recall(&generation, result);
    assert_eq!(recalled["resolution"]["seeds"][0]["reason"], "hybrid_task");
}

#[test]
fn generation_mismatch_and_expired_deadline_fail_closed_without_inference() {
    let (_root, mut generation, conn) = fixture();
    let fake = FakeEmbedder::complete();
    let expired = DenseTaskIndex::default().run(
        &conn,
        &generation,
        Some(&fake),
        Some("payment"),
        8,
        Instant::now(),
        &CancellationToken::new(),
    );
    assert!(has_omission(
        &expired,
        DenseOmissionReason::DenseIndexIncomplete
    ));
    generation.generation_id = "wrong".into();
    let mismatch = DenseTaskIndex::default().run(
        &conn,
        &generation,
        Some(&fake),
        Some("payment"),
        8,
        deadline(),
        &CancellationToken::new(),
    );
    assert!(has_omission(
        &mismatch,
        DenseOmissionReason::DenseIndexIncomplete
    ));
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn fresh_store_has_content_addressed_task_seed_vectors() {
    let conn = store::open_store(None).unwrap();
    conn.execute(
        "INSERT INTO task_seed_vectors VALUES(?1,?2,?3,?4)",
        rusqlite::params!["model", "prompt-hash", 2, vec![0_u8; 8]],
    )
    .unwrap();
    assert!(conn
        .execute(
            "INSERT INTO task_seed_vectors VALUES(?1,?2,?3,?4)",
            rusqlite::params!["model", "prompt-hash", 2, vec![0_u8; 8]]
        )
        .is_err());
}

#[test]
fn units_cap_body_lines_and_characters_using_benchmark_slices() {
    let (root, mut generation, _conn) = fixture();
    let body = (0..50)
        .map(|n| format!("line{n}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(root.path().join("src/lib.rs"), &body).unwrap();
    let hash = rightkit_repo_graph::identity::content_digest(body.as_bytes());
    for node in &mut generation.nodes {
        node.evidence[0]["contentHash"] = json!(hash);
        node.evidence[0]["startLine"] = json!(2);
        node.evidence[0]["endLine"] = json!(49);
    }
    let units = symbol_units(&generation, deadline(), &CancellationToken::new()).unwrap();
    assert_eq!(
        units[0].text.lines().count(),
        41,
        "heading & forty file lines"
    );
    assert!(units[0].text.ends_with("line39"));
    assert!(units[1].text.ends_with("line40"));
    let body = "é".repeat(5000);
    std::fs::write(root.path().join("src/lib.rs"), &body).unwrap();
    let hash = rightkit_repo_graph::identity::content_digest(body.as_bytes());
    for node in &mut generation.nodes {
        node.evidence[0]["contentHash"] = json!(hash);
        node.evidence[0]["startLine"] = json!(1);
        node.evidence[0]["endLine"] = json!(1);
    }
    let units = symbol_units(&generation, deadline(), &CancellationToken::new()).unwrap();
    assert_eq!(
        units[0].text.split_once('\n').unwrap().1.chars().count(),
        4000
    );
}
