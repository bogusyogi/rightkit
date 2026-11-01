//! End-to-end offline compiler-fact journeys. No language server or indexer is spawned: a
//! committed portable SCIP JSON index sitting in a temp repository is the only input, and the
//! public provider boundary (`providers::scip::collect`) is the outermost entry point that
//! accepts it. Covers exact fact ingestion, noise filtering, and every fail-soft degradation
//! state (absent / unreadable / incompatible / partial) so a bad index can never fabricate edges.

use rightkit_repo_graph::providers::scip::{collect, CollectResult, ScipContext};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use tempfile::tempdir;

const PKG: &str = "scip-python python demo 1";

fn sym(tail: &str) -> String {
    format!("{PKG} {tail}")
}

fn occ(symbol: &str, role: &str, range: [u32; 4]) -> Value {
    json!({"symbol": symbol, "roles": [role], "range": range})
}

fn write_index(root: &Path, name: &str, index: &Value) {
    fs::write(root.join(name), serde_json::to_vec_pretty(index).unwrap()).unwrap();
}

fn collect_from(root: &Path, explicit: &str) -> CollectResult {
    collect(&ScipContext {
        repo_root: root.to_path_buf(),
        scip_index_path: Some(explicit.to_owned()),
    })
}

fn node_by_symbol<'a>(result: &'a CollectResult, symbol: &str) -> Option<&'a Value> {
    result.nodes.iter().find(|n| n["symbol"] == symbol)
}

fn healthy_index() -> Value {
    let item = sym("pkg/models Item#");
    let total = sym("pkg/models Item#total().");
    let line_total = sym("pkg/service line_total().");
    json!({
        "metadata": {"version": "0.6.6", "indexer": "scip-python"},
        "documents": [
            {"relativePath": "pkg/models.py", "occurrences": [
                occ(&item, "definition", [0, 6, 0, 10]),
                occ(&total, "definition", [4, 8, 4, 13]),
            ]},
            {"relativePath": "pkg/service.py", "occurrences": [
                occ(&item, "reference", [0, 15, 0, 19]),
                occ(&line_total, "definition", [4, 4, 4, 14]),
                // Function parameter and a file-local symbol: compiler noise that must be dropped.
                occ(&sym("pkg/service line_total().(item)"), "definition", [4, 15, 4, 19]),
                occ("local 3", "definition", [5, 4, 5, 9]),
                occ(&total, "reference", [5, 15, 5, 20]),
            ]},
            {"relativePath": "main.py", "occurrences": [
                occ(&line_total, "reference", [2, 4, 2, 14]),
                // A symbol with no definition anywhere in the index.
                occ(&sym("pkg/ghost Ghost#"), "reference", [3, 0, 3, 5]),
            ]},
        ]
    })
}

#[test]
fn offline_scip_index_yields_exact_definitions_and_cross_file_references() {
    let repo = tempdir().unwrap();
    write_index(repo.path(), "index.scip.json", &healthy_index());
    let result = collect_from(repo.path(), "index.scip.json");

    assert_eq!(result.index["state"], "ok", "{}", result.index);
    assert_eq!(result.index["provider"], "scip-python");
    assert_eq!(result.index["documentCount"], 3);
    assert_eq!(
        result.index["definitionCount"], 3,
        "parameter and local definitions must not count"
    );
    assert_eq!(result.index["referenceCount"], 4);
    assert!(
        result.reports.is_empty(),
        "a healthy index reports no degradation: {:?}",
        result.reports
    );

    // 3 file nodes + 3 symbol nodes (Item, total, line_total); no parameter/local nodes.
    assert_eq!(
        result.nodes.len(),
        6,
        "{:#?}",
        result
            .nodes
            .iter()
            .map(|n| n["id"].clone())
            .collect::<Vec<_>>()
    );
    let item = node_by_symbol(&result, &sym("pkg/models Item#")).expect("class node");
    assert_eq!(item["labels"], json!(["Class"]));
    assert_eq!(item["name"], "Item");
    assert_eq!(item["path"], "pkg/models.py");
    assert_eq!(item["precisionTier"], "COMPILER");
    let total = node_by_symbol(&result, &sym("pkg/models Item#total().")).expect("method node");
    assert_eq!(total["labels"], json!(["Method"]));
    assert_eq!(total["name"], "total");
    assert_eq!(total["qualifiedName"], "Item.total");
    let line_total =
        node_by_symbol(&result, &sym("pkg/service line_total().")).expect("function node");
    assert_eq!(line_total["labels"], json!(["Function"]));
    assert!(node_by_symbol(&result, &sym("pkg/service line_total().(item)")).is_none());
    assert!(node_by_symbol(&result, "local 3").is_none());

    // Cross-document references resolve exactly to the defining node.
    let reference = |from: &str, target_symbol: &str| {
        let target_id =
            node_by_symbol(&result, target_symbol).map(|n| n["id"].as_str().unwrap().to_owned());
        result.edges.iter().find(|e| {
            e["kind"] == "REFERENCES"
                && e["source"] == format!("file:{from}")
                && e["target"].as_str().map(str::to_owned) == target_id
        })
    };
    let edge = reference("pkg/service.py", &sym("pkg/models Item#total()."))
        .expect("service -> total() edge");
    assert_eq!(edge["confidenceTier"], "EXACT_RESOLUTION");
    assert_eq!(edge["resolved"], true);
    assert!(
        reference("main.py", &sym("pkg/service line_total().")).is_some(),
        "main -> line_total() edge"
    );

    // A reference to a class additionally yields a TYPED edge; nothing else does.
    let typed: Vec<&Value> = result
        .edges
        .iter()
        .filter(|e| e["kind"] == "TYPED")
        .collect();
    assert_eq!(typed.len(), 1);
    assert_eq!(typed[0]["source"], "file:pkg/service.py");
    assert_eq!(typed[0]["target"], item["id"]);

    // A symbol with no definition stays unresolved: no name-match fallback invents a target.
    let ghost = result
        .edges
        .iter()
        .find(|e| e["source"] == "file:main.py" && e["resolved"] == false)
        .expect("unresolved edge");
    assert!(ghost["target"].is_null(), "{ghost}");
    assert_eq!(ghost["confidenceTier"], "UNRESOLVED");
    assert!(ghost["reason"]
        .as_str()
        .unwrap()
        .contains("no name-match fallback"));
    assert_eq!(result.edges.len(), 5, "4 REFERENCES + 1 TYPED");

    // Deterministic: the same index collects to the same graph.
    let again = collect_from(repo.path(), "index.scip.json");
    assert_eq!(
        serde_json::to_string(&result.nodes).unwrap(),
        serde_json::to_string(&again.nodes).unwrap()
    );
    assert_eq!(
        serde_json::to_string(&result.edges).unwrap(),
        serde_json::to_string(&again.edges).unwrap()
    );
}

#[test]
fn unusable_indexes_degrade_to_ast_without_fabricating_facts() {
    let repo = tempdir().unwrap();
    let root = repo.path();

    // Absent: nothing found at the requested location.
    let absent = collect_from(root, "missing.scip.json");
    assert_eq!(absent.index["state"], "unavailable");
    assert_eq!(absent.index["code"], "scip_index_absent");
    assert_eq!(absent.reports[0]["kind"], "unavailable");
    assert_eq!(absent.reports[0]["degradesTo"], "AST");
    assert!(absent.nodes.is_empty() && absent.edges.is_empty());

    // Unreadable: not JSON at all.
    fs::write(root.join("garbage.scip.json"), "this is {not json").unwrap();
    let unreadable = collect_from(root, "garbage.scip.json");
    assert_eq!(unreadable.index["state"], "unavailable");
    assert_eq!(unreadable.index["code"], "scip_index_unreadable");
    assert!(unreadable.nodes.is_empty() && unreadable.edges.is_empty());

    // Wrong shape: JSON but no `documents` array.
    write_index(
        root,
        "shape.scip.json",
        &json!({"metadata": {"version": "0.6.6"}}),
    );
    let wrong_shape = collect_from(root, "shape.scip.json");
    assert_eq!(wrong_shape.index["code"], "scip_index_incompatible");
    assert!(wrong_shape.nodes.is_empty());

    // Incompatible indexer version: refused, not best-effort parsed.
    let mut future = healthy_index();
    future["metadata"]["version"] = json!("9.9.9");
    write_index(root, "future.scip.json", &future);
    let incompatible = collect_from(root, "future.scip.json");
    assert_eq!(incompatible.index["state"], "unavailable");
    assert_eq!(
        incompatible.index["code"],
        "scip_index_version_incompatible"
    );
    assert!(incompatible.nodes.is_empty() && incompatible.edges.is_empty());
    assert_eq!(incompatible.reports[0]["degradesTo"], "AST");

    // Partial: structurally broken entries are skipped and reported; the sound remainder is kept.
    let mut partial = healthy_index();
    partial["documents"]
        .as_array_mut()
        .unwrap()
        .push(json!({"occurrences": []}));
    partial["documents"][0]["occurrences"]
        .as_array_mut()
        .unwrap()
        .push(json!({"symbol": "", "roles": ["definition"], "range": [0, 0, 0, 1]}));
    write_index(root, "partial.scip.json", &partial);
    let degraded = collect_from(root, "partial.scip.json");
    assert_eq!(degraded.index["state"], "partial", "{}", degraded.index);
    assert_eq!(degraded.reports[0]["kind"], "partial");
    assert_eq!(degraded.reports[0]["code"], "scip_index_partial");
    assert_eq!(degraded.reports[0]["skippedDocuments"], 1);
    assert_eq!(degraded.reports[0]["skippedOccurrences"], 1);
    assert!(
        node_by_symbol(&degraded, &sym("pkg/models Item#")).is_some(),
        "sound documents still contribute"
    );
    assert_eq!(degraded.nodes.len(), 6);
}
