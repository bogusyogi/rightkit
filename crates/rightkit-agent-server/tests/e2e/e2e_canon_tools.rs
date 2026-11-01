//! Canon closure journeys for tool-surface atoms, driven through the REAL `coderight run`
//! binary against a scripted provider (same rig as `e2e_tools_*`).
//!
//! Evidence map (ledger: docs/pending/canon-closure-ledger.md):
//! - `S:A1a`   stale-anchor edit rejection carries a nearest-content hint and writes nothing.
//! - `VG-006`  cell-level `.ipynb` edit tool: replace / insert / delete + typed refusals.
//! - `SRCH-002` search continuation resumes the same query and refuses a different one.
//! - `KRN-008` strict profile: a process-backed tool fails typed before any child exists.

use crate::support::e2e_tools as t;
use t::*;

const NEAR_FILE: &str =
    "fn main() {\n    let value = compute(alpha, beta);\n    println!(\"{value}\");\n}\n";

#[test]
fn stale_anchor_edit_is_refused_with_a_nearest_content_hint_and_writes_nothing() {
    let p = sequence(
        vec![
            call("read_file", json!({"path": "near.txt"})),
            call(
                "edit_file",
                json!({
                    "path": "near.txt",
                    "old_string": "    let value = compute(alpha, betta);",
                    "new_string": "    let value = 0;"
                }),
            ),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[("near.txt", NEAR_FILE)]);
    let out = rig.run("go");
    let r = expect_results(&out, 2);

    assert!(r[1].error, "a stale anchor must be refused: {}", r[1].text);
    assert!(
        r[1].text.contains("Nearest-content suggestion: lines "),
        "the refusal names the nearest real lines: {}",
        r[1].text
    );
    assert!(
        r[1].text.contains("similarity "),
        "the hint reports its similarity: {}",
        r[1].text
    );
    assert_eq!(
        rig.read("near.txt"),
        NEAR_FILE,
        "a refused edit must not touch the file"
    );
}

const NOTEBOOK: &str = r##"{
  "cells": [
    {"cell_type": "code", "id": "c1", "metadata": {}, "source": "before",
     "execution_count": 3, "outputs": [{"output_type": "stream", "name": "stdout", "text": "stale"}]}
  ],
  "metadata": {},
  "nbformat": 4,
  "nbformat_minor": 5
}"##;

#[test]
fn notebook_tool_edits_cells_by_id_and_refuses_bad_targets() {
    let p = sequence(
        vec![
            call(
                "notebook",
                json!({"notebook_path": "nb.ipynb", "cell_id": "c1",
                       "new_source": "after", "edit_mode": "replace"}),
            ),
            call(
                "notebook",
                json!({"notebook_path": "nb.ipynb", "cell_id": "c1",
                       "new_source": "# note", "cell_type": "markdown", "edit_mode": "insert"}),
            ),
            call(
                "notebook",
                json!({"notebook_path": "nb.ipynb", "cell_id": "no-such-cell",
                       "new_source": "", "edit_mode": "delete"}),
            ),
            call(
                "notebook",
                json!({"notebook_path": "README.md", "cell_id": "c1", "new_source": "x"}),
            ),
        ],
        "done",
    );
    let rig = Rig::new(&p, &[("nb.ipynb", NOTEBOOK)]);
    let out = rig.run("go");
    let r = expect_results(&out, 4);

    assert!(!r[0].error, "replace succeeds: {}", r[0].text);
    assert!(!r[1].error, "insert succeeds: {}", r[1].text);
    assert!(
        r[2].error && r[2].text.contains("Cell ID not found"),
        "{}",
        r[2].text
    );
    assert!(
        r[3].error && r[3].text.contains("Jupyter notebook"),
        "{}",
        r[3].text
    );

    let nb: Value = serde_json::from_str(&rig.read("nb.ipynb")).expect("notebook stays valid JSON");
    let cells = nb["cells"].as_array().expect("cells array");
    assert_eq!(cells.len(), 2, "one replaced + one inserted: {nb}");
    assert_eq!(cells[0]["id"], "c1");
    let src = |c: &Value| match &c["source"] {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().filter_map(|x| x.as_str()).collect::<String>(),
        _ => String::new(),
    };
    assert_eq!(src(&cells[0]), "after");
    assert!(
        cells[0]["execution_count"].is_null(),
        "a replaced code cell resets its execution count: {nb}"
    );
    assert_eq!(
        cells[0]["outputs"],
        json!([]),
        "a replaced code cell drops its stale outputs: {nb}"
    );
    assert_eq!(cells[1]["cell_type"], "markdown");
    assert_eq!(src(&cells[1]), "# note");
    assert_eq!(
        nb["nbformat"], 4,
        "unrelated notebook fields survive the edit"
    );
}

fn cursor_of(text: &str) -> String {
    // The model may see the footer wrapped in an untrusted-context envelope, so look
    // for the status line rather than assuming it is the last line.
    let footer = text
        .lines()
        .rev()
        .find(|l| l.contains("[search status=") && l.contains("cursor="))
        .expect("a search footer carries a cursor field");
    let start = footer.find("cursor=").unwrap() + "cursor=".len();
    // The cursor is itself a JSON document (it contains `]`); the footer's own closing
    // bracket is the last character on the line.
    let rest = footer[start..].trim_end();
    rest.strip_suffix(']').unwrap_or(rest).trim().to_string()
}

#[test]
fn truncated_search_resumes_from_its_cursor_and_refuses_a_different_query() {
    let files: &[(&str, &str)] = &[
        ("src/a.rs", "needle one\nneedle two\n"),
        ("src/b.rs", "needle three\nneedle four\n"),
    ];
    let p = sequence(
        vec![
            call("search", json!({"pattern": "needle", "max_results": 1})),
            call_with(
                "search",
                |seen| json!({"pattern": "needle", "max_results": 1, "cursor": cursor_of(&seen[0])}),
            ),
            call_with(
                "search",
                |seen| json!({"pattern": "different", "cursor": cursor_of(&seen[0])}),
            ),
        ],
        "done",
    );
    let rig = Rig::new(&p, files);
    let out = rig.run("go");
    let r = expect_results(&out, 3);

    let first = &r[0].text;
    assert!(
        first.contains("status=Truncated"),
        "partial results are never described as exhaustive: {first}"
    );
    assert!(!r[1].error, "same query + cursor continues: {}", r[1].text);
    let hit = |text: &str| {
        text.lines()
            .find(|l| l.contains("needle"))
            .unwrap_or_default()
            .to_string()
    };
    assert_ne!(
        hit(first),
        hit(&r[1].text),
        "the continuation returns the NEXT hit, not a duplicate: first={first} second={}",
        r[1].text
    );
    assert!(
        r[2].error,
        "a cursor from another query must be refused, not silently applied: {}",
        r[2].text
    );
}

#[test]
fn strict_profile_denies_a_process_backed_tool_before_any_child_is_spawned() {
    let p = sequence(
        vec![call(
            "shell",
            json!({"command": "echo strict-child-ran > strict-marker.txt"}),
        )],
        "done",
    );
    let rig = Rig::new(&p, &[]);
    let out = rig.run_args_with_resource_profile(
        &["--stream-json", "--bypass-approvals"],
        "go",
        Some("strict"),
    );
    let r = results(&out);
    assert!(
        !r.is_empty() && r.iter().all(|x| x.error),
        "strict profile must fail the process-backed call as a typed error: {r:#?}\n{}",
        out.diag()
    );
    assert!(
        !rig.exists("strict-marker.txt"),
        "no CodeRight child may have run under the strict profile"
    );
}
