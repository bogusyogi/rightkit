//! Full-repository Blueprint retrieval benchmark, native engine path. Deliberately ignored in ordinary CI.
//!
//! Drives the shipped `membrane-blueprint` native operations (Build, Recall, Search) and the shipped
//! parsed-symbol BM25 index over a real repository snapshot copied to an isolated directory.
//! Inputs are a JSON config file (RightKit need not forward custom environment); the path comes from
//! `BLUEPRINT_FULLREPO_CONFIG` or the default below. Without that file the test is a no-op.
//! No production source, ranking, embedding store, or activation policy is changed.
//!
//! Config keys: corpus_dir, work_dir, queries_path, out_dir, reuse_graph (bool),
//! seeds_path (optional JSON `{arm: {query_id: [node_id,...]}}` produced by the dense stage; each arm is
//! replayed as native Recall with explicit `seedIds`).
use rightkit_repo_graph::api::{BlueprintRequest, Bounds};
use rightkit_repo_graph::bm25_index::{build_bm25_code_index, GenerationNodeView, SearchOptions};
use rightkit_repo_graph::{BlueprintOperation, Operation};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

const DEFAULT_CONFIG: &str = "D:/Claude/.model-bench/blueprint-fullrepo/run-config.json";
const DEPTH: usize = 32;

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            std::fs::copy(&from, &to).unwrap();
        }
    }
}

fn phase(log: &mut std::fs::File, started: &Instant, name: &str) {
    writeln!(log, "{} {}", started.elapsed().as_millis(), name).unwrap();
    log.flush().unwrap();
}

/// `params`: None = production defaults (task/query only); Some(k) = report parameters (maxSeeds/maxCandidates = k).
fn call(
    root: &Path,
    operation: Operation,
    text: &str,
    seed_ids: Option<&[String]>,
    k: Option<usize>,
) -> Result<Value, String> {
    let mut request = BlueprintRequest::new("fullrepo-bench", operation, root.to_string_lossy());
    request.deadline_ms = if operation == Operation::Build {
        300_000
    } else {
        30_000
    };
    request.input["task"] = json!(text);
    request.input["query"] = json!(text);
    if let Some(k) = k {
        request.input["maxSeeds"] = json!(k);
        request.input["maxCandidates"] = json!(k);
        request.input["maxBytes"] = json!(rightkit_repo_graph::model::MAX_RESPONSE_BYTES);
    }
    if let Some(ids) = seed_ids {
        request.input["seedIds"] = json!(ids);
    }
    let context = request
        .validate(Bounds {
            max_candidates: k.unwrap_or(Bounds::default().max_candidates),
            ..Bounds::default()
        })
        .map_err(|e| format!("validate: {e:?}"))?;
    rightkit_repo_graph::engine::NativeBlueprintOperation
        .execute(&request, &context)
        .map_err(|e| format!("execute: {e:?}"))
}

fn parsed_bm25(nodes: &[Value]) -> rightkit_repo_graph::bm25_index::Bm25CodeIndex {
    let views = nodes
        .iter()
        .map(|node| {
            let evidence = node["evidence"]
                .as_array()
                .and_then(|e| e.first())
                .cloned()
                .unwrap_or(Value::Null);
            GenerationNodeView {
                id: node["id"].as_str().unwrap().to_string(),
                kind: node["kind"].as_str().unwrap().to_string(),
                name: node["name"].as_str().map(str::to_string),
                path: node["path"].as_str().map(str::to_string),
                qualified_name: evidence["qualifiedName"].as_str().map(str::to_string),
                signature: evidence["signature"].as_str().map(str::to_string),
                labels: evidence["labels"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
                node: node.clone(),
                ..Default::default()
            }
        })
        .collect::<Vec<_>>();
    build_bm25_code_index(&views)
}

fn ids_of(value: &Value, pointer: &str) -> Vec<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|c| c["id"].as_str().map(str::to_string))
        .collect()
}

fn record(
    arms: &mut Map<String, Value>,
    arm: &str,
    qid: &str,
    ids: Vec<String>,
    state: &str,
    ms: f64,
    error: Option<String>,
) {
    let entry = arms.entry(arm.to_string()).or_insert_with(|| json!({}));
    entry[qid] = json!({"ids": ids, "state": state, "ms": ms, "error": error});
}

fn phase_shared(path: &Path, started: &Instant, name: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(path) {
        let _ = writeln!(f, "{} {}", started.elapsed().as_millis(), name);
    }
}

/// Optional `dump_raw_dir` from the run config: full responses per query.
static DUMP_RAW_DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

fn record_call(
    arms: &mut Map<String, Value>,
    arm: &str,
    qid: &str,
    result: Result<Value, String>,
    pointer: &str,
    started: Instant,
) {
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    if let (Ok(v), Some(dir)) = (&result, DUMP_RAW_DIR.get()) {
        let _ = std::fs::write(
            Path::new(&dir).join(format!("{arm}-{qid}.json")),
            serde_json::to_vec_pretty(v).unwrap_or_default(),
        );
    }
    match result {
        Ok(v) => record(
            arms,
            arm,
            qid,
            ids_of(&v, pointer),
            v["state"].as_str().unwrap_or("?"),
            ms,
            None,
        ),
        Err(e) => record(arms, arm, qid, vec![], "error", ms, Some(e)),
    }
}

fn write_results(out_dir: &Path, tag: &str, build_info: &Value, arms: &Map<String, Value>) {
    let result = json!({"build": build_info, "arms": arms});
    std::fs::write(
        out_dir.join(format!("native-results-{tag}.json")),
        serde_json::to_vec(&result).unwrap(),
    )
    .unwrap();
}

#[test]
#[ignore = "requires the frozen external snapshot + queries; writes benchmark JSON"]
fn blueprint_fullrepo_native_arms() {
    let config_path =
        std::env::var("BLUEPRINT_FULLREPO_CONFIG").unwrap_or_else(|_| DEFAULT_CONFIG.to_string());
    let Ok(config_bytes) = std::fs::read(&config_path) else {
        eprintln!("no benchmark config at {config_path}; skipping");
        return;
    };
    let cfg: Value = serde_json::from_slice(&config_bytes).unwrap();
    if let Some(dir) = cfg["dump_raw_dir"].as_str() {
        let _ = DUMP_RAW_DIR.set(std::path::PathBuf::from(dir));
    }
    let corpus = PathBuf::from(cfg["corpus_dir"].as_str().unwrap());
    let work = PathBuf::from(cfg["work_dir"].as_str().unwrap());
    let out_dir = PathBuf::from(cfg["out_dir"].as_str().unwrap());
    let reuse = cfg["reuse_graph"].as_bool().unwrap_or(false);
    let repo = work.join("repo");
    std::fs::create_dir_all(&out_dir).unwrap();
    let started = Instant::now();
    let log_path = out_dir.join("phases.log");
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(out_dir.join("phases.log"))
        .unwrap();
    let queries: Value =
        serde_json::from_slice(&std::fs::read(cfg["queries_path"].as_str().unwrap()).unwrap())
            .unwrap();
    let queries: Vec<(String, String)> = queries["queries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|q| {
            (
                q["id"].as_str().unwrap().to_string(),
                q["text"].as_str().unwrap().to_string(),
            )
        })
        .collect();

    let mut build_info = json!({"reused": reuse});
    if !reuse {
        assert!(
            !repo.exists() || std::fs::read_dir(&repo).unwrap().next().is_none(),
            "work repo must start empty: {}",
            repo.display()
        );
        phase(&mut log, &started, "copy_start");
        let t = Instant::now();
        copy_tree(&corpus, &repo);
        build_info["copy_ms"] = json!(t.elapsed().as_secs_f64() * 1000.0);
        phase(&mut log, &started, "build_start");
        let t = Instant::now();
        let response = call(&repo, Operation::Build, "", None, None);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        phase(&mut log, &started, "build_end");
        build_info["build_ms"] = json!(ms);
        match &response {
            Ok(v) => build_info["build_response"] = v.clone(),
            Err(e) => build_info["build_error"] = json!(e),
        }
        assert!(response.is_ok(), "build failed: {response:?}");
    }
    let repo = repo.canonicalize().unwrap();
    let db_path = repo.join(".agent/graph/graph.db");
    let db = rightkit_repo_graph::store::open_store_read_only(&db_path).unwrap();
    let generation = rightkit_repo_graph::store::load_generation(&db)
        .unwrap()
        .unwrap();
    build_info["graph_db_bytes"] = json!(std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0));

    // Node inventory for scoring/aligning non-native arms (no judgments consulted).
    let nodes: Vec<Value> = generation
        .nodes
        .iter()
        .map(|n| {
            let ev = n["evidence"].as_array().and_then(|e| e.first()).cloned().unwrap_or(Value::Null);
            json!({
                "id": n["id"], "kind": n["kind"], "name": n["name"], "path": n["path"],
                "start": ev["startLine"], "end": ev["endLine"],
                "qualifiedName": ev["qualifiedName"], "signature": ev["signature"], "labels": ev["labels"],
            })
        })
        .collect();
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for n in &generation.nodes {
        *kinds
            .entry(n["kind"].as_str().unwrap_or("?").to_string())
            .or_default() += 1;
    }
    let mut edge_kinds: BTreeMap<String, usize> = BTreeMap::new();
    for e in &generation.edges {
        *edge_kinds
            .entry(e["kind"].as_str().unwrap_or("?").to_string())
            .or_default() += 1;
    }
    std::fs::write(
        out_dir.join("graph_nodes.json"),
        serde_json::to_vec(&nodes).unwrap(),
    )
    .unwrap();
    let edge_sample: Vec<&Value> = generation.edges.iter().take(3).collect();
    build_info["graph"] = json!({"nodes": generation.nodes.len(), "edges": generation.edges.len(), "node_kinds": kinds, "edge_kinds": edge_kinds, "edge_sample": edge_sample});

    let tag = cfg["tag"].as_str().unwrap_or("native").to_string();
    let deadline = cfg["max_minutes"]
        .as_f64()
        .map(|m| std::time::Duration::from_secs_f64(m * 60.0));
    let only: Option<Vec<String>> = cfg["native_arms_only"].as_array().map(|a| {
        a.iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    });
    let want = |arm: &str| only.as_ref().is_none_or(|o| o.iter().any(|x| x == arm));
    let mut arms = Map::new();
    if cfg["native_arms"].as_bool().unwrap_or(true) {
        phase(&mut log, &started, "bm25_build_start");
        let t = Instant::now();
        let bm25 = parsed_bm25(&generation.nodes);
        build_info["parsed_bm25_build_ms"] = json!(t.elapsed().as_secs_f64() * 1000.0);
        phase(&mut log, &started, "native_queries_start");
        // One untimed warm-up so lazy caches do not land on query 1.
        let _ = call(&repo, Operation::Recall, &queries[0].1, None, None);
        for (qi, (qid, text)) in queries.iter().enumerate() {
            if deadline.is_some_and(|d| started.elapsed() > d) {
                build_info["native_truncated_at_query"] = json!(qi);
                break;
            }
            if want("native-recall") {
                let t = Instant::now();
                let r = call(&repo, Operation::Recall, text, None, None);
                record_call(
                    &mut arms,
                    "native-recall",
                    qid,
                    r,
                    "/candidateSet/candidates",
                    t,
                );
            }
            if want("native-recall-k32") {
                let t = Instant::now();
                let r = call(&repo, Operation::Recall, text, None, Some(DEPTH));
                record_call(
                    &mut arms,
                    "native-recall-k32",
                    qid,
                    r,
                    "/candidateSet/candidates",
                    t,
                );
            }
            if want("native-search") {
                let t = Instant::now();
                let r = call(&repo, Operation::Search, text, None, None);
                record_call(&mut arms, "native-search", qid, r, "/candidates", t);
            }
            if want("parsed-symbol-bm25") {
                let t = Instant::now();
                let hits: Vec<String> = bm25
                    .search(
                        text,
                        SearchOptions {
                            limit: DEPTH as i64,
                        },
                    )
                    .into_iter()
                    .filter_map(|h| h.document.node["id"].as_str().map(str::to_string))
                    .collect();
                record(
                    &mut arms,
                    "parsed-symbol-bm25",
                    qid,
                    hits,
                    "component",
                    t.elapsed().as_secs_f64() * 1000.0,
                    None,
                );
            }
            phase(&mut log, &started, &format!("q{qi} {qid} done"));
            write_results(&out_dir, &tag, &build_info, &arms);
        }
        phase(&mut log, &started, "native_queries_end");
    }
    if let Some(seeds_path) = cfg["seeds_path"].as_str() {
        let seeds: Value = serde_json::from_slice(&std::fs::read(seeds_path).unwrap()).unwrap();
        phase(&mut log, &started, "seed_queries_start");
        let text_of: BTreeMap<_, _> = queries.iter().cloned().collect();
        let mut jobs: Vec<(String, String, Vec<String>)> = Vec::new();
        for (arm, per_query) in seeds.as_object().unwrap() {
            for (qid, ids) in per_query.as_object().unwrap() {
                let ids: Vec<String> = ids
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect();
                jobs.push((arm.clone(), qid.clone(), ids));
            }
        }
        // Independent read-only Recall calls; run on a bounded worker pool (each call re-reads the generation, so memory scales with workers).
        let workers = cfg["seed_workers"].as_u64().unwrap_or(6) as usize;
        let next = std::sync::atomic::AtomicUsize::new(0);
        type SeedCall = (String, String, Result<Value, String>, f64);
        let done: std::sync::Mutex<Vec<SeedCall>> = std::sync::Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if i >= jobs.len() || deadline.is_some_and(|d| started.elapsed() > d) {
                        break;
                    }
                    let (arm, qid, ids) = &jobs[i];
                    let t = Instant::now();
                    let r = call(&repo, Operation::Recall, &text_of[qid], Some(ids), None);
                    let ms = t.elapsed().as_secs_f64() * 1000.0;
                    done.lock().unwrap().push((arm.clone(), qid.clone(), r, ms));
                    phase_shared(&log_path, &started, &format!("seed{i} {arm} {qid} done"));
                });
            }
        });
        build_info["seed_workers"] = json!(workers);
        for (arm, qid, r, ms) in done.into_inner().unwrap() {
            let name = format!("seeded:{arm}");
            match r {
                Ok(v) => record(
                    &mut arms,
                    &name,
                    &qid,
                    ids_of(&v, "/candidateSet/candidates"),
                    v["state"].as_str().unwrap_or("?"),
                    ms,
                    None,
                ),
                Err(e) => record(&mut arms, &name, &qid, vec![], "error", ms, Some(e)),
            }
        }
        write_results(&out_dir, &tag, &build_info, &arms);
        phase(&mut log, &started, "seed_queries_end");
    }
    write_results(&out_dir, &tag, &build_info, &arms);
    phase(&mut log, &started, "done");
}
