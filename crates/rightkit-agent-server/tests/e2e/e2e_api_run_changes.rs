//! Run-changes (review/undo) HTTP family of a REAL `coderight serve` daemon:
//! capture -> summary -> inventory -> diff-body -> revise -> undo -> reset, input
//! validation, fingerprint-guarded undo, and durability across a hard restart.
//!
//! Mirrors `api_run_changes.rs` (`RunChangeStore`, its serde field names are
//! snake_case, and the route handlers). Routes come from
//! `ops004_route_coverage.toml` (`/run-changes/*`).
//!
//! Retires `api_run_changes/tests/mod.rs`.

use crate::support::daemon_process;
use crate::support::e2e_api;
use crate::support::scripted_provider;

use e2e_api::*;
use serde_json::{json, Value};

const RUN: &str = "run-e2e-1";

fn url_encode(raw: &str) -> String {
    raw.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

fn entry(
    rig: &Rig,
    mutation_id: &str,
    path: &str,
    classification: &str,
    diff: &str,
    undo_body: &str,
) -> Value {
    json!({
        "mutation_id": mutation_id,
        "workspace_root": rig.ws().to_string_lossy(),
        "path": path,
        "original_path": null,
        "classification": classification,
        "outcome": "complete",
        "stage": "unstaged",
        "additions": 1,
        "deletions": 0,
        "diff": diff,
        "truncated": false,
        "anchor_status": "live",
        "anchor_offset": 0,
        "captured_at": "2026-10-06T00:00:00Z",
        "fingerprint": "",
        "size_bytes": 0,
        "modified_at": null,
        "undo": {"fingerprint": "", "body": undo_body, "note": "initial state at run start"},
        "last_error": null
    })
}

fn ws_query(rig: &Rig, run: &str, extra: &str) -> String {
    format!(
        "run_id={run}&workspace_root={}{extra}",
        url_encode(&rig.ws().to_string_lossy())
    )
}

/// Two on-disk changes in the rig's git workspace: a tracked edit and a new file.
fn seed_changes(rig: &Rig) -> Value {
    std::fs::write(rig.ws().join("README.md"), "seed\nextra\n").unwrap();
    std::fs::write(rig.ws().join("notes.txt"), "line one\nline two\n").unwrap();
    let readme_diff = "diff --git a/README.md b/README.md\n@@ -1 +1,2 @@\n seed\n+extra\n";
    let notes_diff = "@@ -0,0 +1,2 @@\n+line one\n+line two\n";
    json!({
        "run_id": RUN,
        "workspace_root": rig.ws().to_string_lossy(),
        "entries": [
            entry(rig, "m-notes", "notes.txt", "untracked", notes_diff, "__RIGHTKIT_DELETE__"),
            entry(rig, "m-readme", "README.md", "tracked", readme_diff, "seed\n"),
        ],
        "expected_index_fingerprint": null
    })
}

#[test]
fn capture_summary_inventory_and_reset_describe_one_run_and_survive_a_restart() {
    let mut rig = Rig::start(vec![]);
    let (status, captured) = rig.post("/run-changes/capture", seed_changes(&rig));
    assert_eq!(status, 200, "{captured}");
    assert_eq!(captured["run_id"], RUN);
    assert_eq!(captured["stored"], 2);
    assert_eq!(captured["truncated"], false);
    assert_eq!(
        captured["git"]["present"], true,
        "the rig workspace is a git repo"
    );
    assert_eq!(captured["git"]["branch"], "main");
    assert!(captured["git"]["head_sha"]
        .as_str()
        .is_some_and(|s| s.len() >= 40));
    assert!(
        captured["git"]["untracked_count"].as_u64().unwrap() >= 1,
        "notes.txt is untracked: {captured}"
    );

    let (status, summary) = rig.get(&format!("/run-changes/summary?{}", ws_query(&rig, RUN, "")));
    assert_eq!(status, 200, "{summary}");
    assert_eq!(summary["run_id"], RUN);
    assert_eq!(summary["entry_count"], 2);
    assert_eq!(summary["truncated"], false);
    assert_eq!(
        summary["counts_by_classification"],
        json!({"tracked": 1, "untracked": 1})
    );
    assert_eq!(
        summary["first_paths"],
        json!(["README.md", "notes.txt"]),
        "paths are sorted"
    );
    assert_eq!(summary["git"]["branch"], "main");
    assert!(summary["last_capture_at"]
        .as_str()
        .is_some_and(|t| !t.is_empty()));

    let (_, inventory) = rig.get(&format!(
        "/run-changes/inventory?{}",
        ws_query(&rig, RUN, "")
    ));
    let entries = inventory["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    let notes = entries.iter().find(|e| e["path"] == "notes.txt").unwrap();
    assert_eq!(notes["mutation_id"], "m-notes");
    assert_eq!(notes["classification"], "untracked");
    assert_eq!(
        notes["size_bytes"], 18,
        "size is measured from disk, not trusted from the client"
    );
    let fingerprint = notes["fingerprint"].as_str().unwrap();
    assert_eq!(
        fingerprint.len(),
        64,
        "the fingerprint is a sha256 computed by the daemon: {fingerprint}"
    );
    assert!(notes["stage"] == "unstaged" && notes["anchor_status"] == "live");

    // A path query narrows to one entry and --max_diff_bytes clips its diff.
    let (_, one) = rig.get(&format!(
        "/run-changes/inventory?{}",
        ws_query(&rig, RUN, "&path=notes.txt&max_diff_bytes=4")
    ));
    let only = one["entries"].as_array().unwrap();
    assert_eq!(only.len(), 1);
    assert_eq!(
        only[0]["diff"], "@@ -",
        "diff is clipped to the requested byte budget"
    );
    assert_eq!(only[0]["truncated"], true);

    // Durable: a kill -9 of the daemon loses nothing.
    rig.restart();
    let (status, after) = rig.get(&format!("/run-changes/summary?{}", ws_query(&rig, RUN, "")));
    assert_eq!(status, 200, "{after}");
    assert_eq!(after["entry_count"], 2);
    assert_eq!(after["first_paths"], json!(["README.md", "notes.txt"]));

    // Reset forgets the run; a second reset reports that nothing was removed.
    let body = json!({"run_id": RUN, "workspace_root": rig.ws().to_string_lossy()});
    assert_eq!(
        rig.post("/run-changes/reset", body.clone()).1,
        json!({"removed": true})
    );
    assert_eq!(
        rig.post("/run-changes/reset", body).1,
        json!({"removed": false})
    );
    let (status, gone) = rig.get(&format!("/run-changes/summary?{}", ws_query(&rig, RUN, "")));
    assert_eq!(status, 404);
    assert_eq!(gone, json!({"error": "run_not_found"}));
    assert_eq!(
        rig.get(&format!(
            "/run-changes/inventory?{}",
            ws_query(&rig, RUN, "")
        ))
        .0,
        404
    );
}

#[test]
fn capture_rejects_unsafe_input_with_typed_400s_and_ignores_a_drifted_index_fingerprint() {
    let rig = Rig::start(vec![]);
    let good = seed_changes(&rig);

    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "relative workspace",
            {
                let mut b = good.clone();
                b["workspace_root"] = json!("relative/path");
                b
            },
            "workspace root must be absolute",
        ),
        (
            "unsafe run id",
            {
                let mut b = good.clone();
                b["run_id"] = json!("bad/run");
                b
            },
            "run id contains unsafe filename characters",
        ),
        (
            "path escape",
            {
                let mut b = good.clone();
                b["entries"][0]["path"] = json!("../outside.txt");
                b
            },
            "unsafe workspace-relative path",
        ),
        (
            "empty mutation id",
            {
                let mut b = good.clone();
                b["entries"][0]["mutation_id"] = json!("");
                b
            },
            "mutation id is empty or too long",
        ),
    ];
    for (label, body, needle) in cases {
        let (status, resp) = rig.post("/run-changes/capture", body);
        assert_eq!(status, 400, "{label}: {resp}");
        assert_eq!(
            resp["error"], "invalid_run_change_capture",
            "{label}: {resp}"
        );
        assert!(
            resp["message"].as_str().unwrap().contains(needle),
            "{label}: {resp}"
        );
    }
    assert_eq!(
        rig.get(&format!("/run-changes/summary?{}", ws_query(&rig, RUN, "")))
            .0,
        404,
        "nothing was stored by rejected captures"
    );

    // A caller-asserted index fingerprint that no longer matches stores nothing.
    let mut drifted = good;
    drifted["expected_index_fingerprint"] = json!("0000000000000000000000000000000000000000");
    let (status, resp) = rig.post("/run-changes/capture", drifted);
    assert_eq!(status, 200, "{resp}");
    assert_eq!(resp["stored"], 0, "{resp}");
    assert_eq!(
        rig.get(&format!("/run-changes/summary?{}", ws_query(&rig, RUN, "")))
            .0,
        404
    );
}

#[test]
fn classification_is_recomputed_from_the_files_on_disk() {
    let rig = Rig::start(vec![]);
    std::fs::write(rig.ws().join(".env"), "TOKEN=abc\n").unwrap();
    std::fs::write(rig.ws().join("blob.bin"), [0u8, 1, 2, 3, 0, 5]).unwrap();
    std::fs::write(rig.ws().join("plain.txt"), "plain\n").unwrap();
    let body = json!({
        "run_id": "run-classify",
        "workspace_root": rig.ws().to_string_lossy(),
        "entries": [
            entry(&rig, "m-env", ".env", "untracked", "@@ -0,0 +1 @@\n+TOKEN=abc\n", "__RIGHTKIT_DELETE__"),
            entry(&rig, "m-bin", "blob.bin", "untracked", "binary", "__RIGHTKIT_DELETE__"),
            entry(&rig, "m-plain", "plain.txt", "untracked", "@@ -0,0 +1 @@\n+plain\n", "__RIGHTKIT_DELETE__"),
            // Claimed tracked, but the file does not exist: it must be reported as deleted.
            entry(&rig, "m-gone", "README.md.gone", "tracked", "", "x"),
        ],
        "expected_index_fingerprint": null
    });
    let (status, resp) = rig.post("/run-changes/capture", body);
    assert_eq!(status, 200, "{resp}");
    assert_eq!(resp["stored"], 4);

    let (_, inv) = rig.get(&format!(
        "/run-changes/inventory?{}",
        ws_query(&rig, "run-classify", "")
    ));
    let by = |m: &str| {
        inv["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["mutation_id"] == m)
            .cloned()
            .unwrap()
    };
    let env = by("m-env");
    assert_eq!(
        env["classification"], "secret",
        "secret-looking files are masked"
    );
    assert!(
        env["undo"].is_null(),
        "a secret's pre-image is never retained"
    );
    let bin = by("m-bin");
    assert_eq!(bin["classification"], "binary");
    assert_eq!(bin["diff"], "", "binary diffs are dropped");
    assert_eq!(by("m-plain")["classification"], "untracked");
    let gone = by("m-gone");
    assert_eq!(gone["classification"], "deleted");
    assert_eq!(gone["anchor_status"], "vanished");
}

#[test]
fn diff_body_pages_rows_searches_and_derives_hunk_decisions() {
    let rig = Rig::start(vec![]);
    assert_eq!(rig.post("/run-changes/capture", seed_changes(&rig)).0, 200);
    let req = |path: &str, extra: Value| {
        let mut body = json!({"run_id": RUN, "workspace_root": rig.ws().to_string_lossy(), "path": path, "split": true});
        for (k, v) in extra.as_object().unwrap() {
            body[k] = v.clone();
        }
        body
    };

    let (status, page1) = rig.post(
        "/run-changes/diff-body",
        req("notes.txt", json!({"window": 2})),
    );
    assert_eq!(status, 200, "{page1}");
    assert_eq!(page1["path"], "notes.txt");
    let kinds: Vec<&str> = page1["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, vec!["hunk", "add"]);
    assert_eq!(page1["rows"][1]["text"], "line one");
    assert_eq!(
        page1["next_cursor"], "2",
        "more rows remain after the window"
    );
    let (_, page2) = rig.post(
        "/run-changes/diff-body",
        req("notes.txt", json!({"window": 2, "cursor": "2"})),
    );
    assert_eq!(page2["rows"].as_array().unwrap().len(), 1);
    assert_eq!(page2["rows"][0]["text"], "line two");
    assert!(
        page2["next_cursor"].is_null(),
        "the last page has no cursor"
    );

    let (_, searched) = rig.post(
        "/run-changes/diff-body",
        req("notes.txt", json!({"search": "LINE"})),
    );
    assert_eq!(searched["search"]["term"], "LINE");
    assert_eq!(
        searched["search"]["matches"], 2,
        "search is case-insensitive over the returned rows"
    );

    let (_, readme) = rig.post("/run-changes/diff-body", req("README.md", json!({})));
    let kinds: Vec<&str> = readme["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, vec!["file", "hunk", "ctx", "add"]);
    assert_eq!(
        readme["hunk_decisions"],
        json!([{"hunk_id": "README.md#0", "file_path": "README.md", "hunk_index": 0, "anchor_offset": 1, "decision": "pending"}])
    );

    // An unknown path or run is an empty body, not an error.
    let (status, none) = rig.post("/run-changes/diff-body", req("never.txt", json!({})));
    assert_eq!(status, 200);
    assert_eq!(none["rows"], json!([]));
    assert_eq!(none["truncated"], false);
}

#[test]
fn revision_comments_track_whether_the_anchor_is_still_live() {
    let rig = Rig::start(vec![]);
    assert_eq!(rig.post("/run-changes/capture", seed_changes(&rig)).0, 200);
    let revise = |body: &str, offset: u32| {
        rig.post(
            "/run-changes/revise",
            json!({
                "run_id": RUN, "workspace_root": rig.ws().to_string_lossy(), "mutation_id": "m-readme",
                "path": "README.md", "anchor_offset": offset, "kind": "revision", "body": body, "author": "qa"
            }),
        )
    };

    let (status, live) = revise("please reword", 0);
    assert_eq!(status, 200, "{live}");
    assert_eq!(
        live,
        json!({"comment_id": "cmt-please r", "anchor_status": "live", "stale": false})
    );

    // An anchor past the end of the file is stale.
    let (_, beyond) = revise("beyond the end", 50);
    assert_eq!(beyond["anchor_status"], "stale");
    assert_eq!(beyond["stale"], true);

    // The file drifting from the captured fingerprint also makes the anchor stale.
    std::fs::write(rig.ws().join("README.md"), "seed\nextra\nmore\n").unwrap();
    let (_, drifted) = revise("after drift", 0);
    assert_eq!(drifted["anchor_status"], "stale");

    std::fs::remove_file(rig.ws().join("README.md")).unwrap();
    let (_, vanished) = revise("file removed", 0);
    assert_eq!(vanished["anchor_status"], "vanished");
    assert_eq!(vanished["stale"], true);
}

#[test]
fn undo_is_guarded_by_fingerprints_and_restores_only_when_nothing_newer_landed() {
    let rig = Rig::start(vec![]);
    // A live session binds the daemon's FileService, which undo mutates through.
    rig.session("undo-host", json!({}));
    assert_eq!(rig.post("/run-changes/capture", seed_changes(&rig)).0, 200);
    let (_, inv) = rig.get(&format!(
        "/run-changes/inventory?{}",
        ws_query(&rig, RUN, "")
    ));
    let notes_fp = inv["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["mutation_id"] == "m-notes")
        .unwrap()["fingerprint"]
        .as_str()
        .unwrap()
        .to_string();
    let undo = |mutation: &str, observed: &str| {
        rig.post(
            "/run-changes/undo",
            json!({"run_id": RUN, "workspace_root": rig.ws().to_string_lossy(), "mutation_id": mutation, "observed_fingerprint": observed}),
        )
    };

    let (status, unknown) = undo("m-nope", &notes_fp);
    assert_eq!(status, 200, "undo reports conflicts in-band: {unknown}");
    assert_eq!(unknown["applied"], false);
    assert_eq!(unknown["conflict_reason"], "unknown mutation id");

    let (_, stale) = undo("m-notes", &"0".repeat(64));
    assert_eq!(stale["applied"], false);
    assert!(
        stale["conflict_reason"]
            .as_str()
            .unwrap()
            .starts_with("fingerprint mismatch: client observed 00000000 but live file is "),
        "{stale}"
    );
    assert!(
        rig.ws().join("notes.txt").is_file(),
        "a refused undo leaves the file alone"
    );

    // The matching fingerprint reverses the create: the agent-added file is removed.
    let (_, applied) = undo("m-notes", &notes_fp);
    assert_eq!(applied["mutation_id"], "m-notes");
    assert_eq!(applied["path"], "notes.txt");
    assert_eq!(applied["applied"], true, "{applied}");
    assert!(applied["conflict_reason"].is_null());
    assert!(
        !rig.ws().join("notes.txt").exists(),
        "the created file was removed"
    );
    let (_, inv) = rig.get(&format!(
        "/run-changes/inventory?{}",
        ws_query(&rig, RUN, "&path=notes.txt")
    ));
    assert_eq!(
        inv["entries"][0]["stage"], "reverted",
        "the entry is marked reverted"
    );

    // Restoring a modified file: newer bytes written after capture must not be overwritten.
    let readme_fp = rig
        .get(&format!(
            "/run-changes/inventory?{}",
            ws_query(&rig, RUN, "&path=README.md")
        ))
        .1["entries"][0]["fingerprint"]
        .as_str()
        .unwrap()
        .to_string();
    std::fs::write(rig.ws().join("README.md"), "someone else edited this\n").unwrap();
    let (_, clobber) = undo("m-readme", &readme_fp);
    assert_eq!(clobber["applied"], false);
    assert!(
        clobber["conflict_reason"]
            .as_str()
            .unwrap()
            .starts_with("fingerprint mismatch"),
        "{clobber}"
    );
    assert_eq!(
        std::fs::read_to_string(rig.ws().join("README.md")).unwrap(),
        "someone else edited this\n"
    );
}
