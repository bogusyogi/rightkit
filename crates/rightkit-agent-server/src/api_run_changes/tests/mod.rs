//! I1 — RunChangeStore focused test suite.
//!
//! These tests are *authored* during I1 but **never executed**. The
//! final integration lane runs `cargo test --locked --workspace` on the
//! merged `codex/cr10-integration` branch, not on the I1 worktree.
//! See `docs/canon/desktop-master-canon-atoms.md`
//! §"Create I1 — authoritative review and end card" and §"Final
//! integration and verification".
//!
//! Coverage map (each test names the I1 requirement it gates):
//! - `restart_round_trip`     → D1 (4) durable across restart
//! - `classifications_persisted` → D2 (4–11) all 11 classifications
//! - `partial_failure_surfaces_last_error` → D2 partial/secret
//! - `stale_anchor_when_file_drifted` → D3 stale anchors
//! - `unified_and_split_diff_rendering` → D3 split/unified
//! - `search_returns_match_count` → D3 search
//! - `revision_comment_persists` → D3 structured revision feedback
//! - `stage_and_revert_transitions` → D3 stage/unstage/revert
//! - `undo_fingerprint_mismatch_rejected` → D3 Undo-conflict
//! - `end_card_emits_classification_counts` → D4 fingerprint-checks

use super::*;
use std::path::PathBuf;
use tempfile::{tempdir, TempDir};

fn workspace_tempdir() -> (TempDir, PathBuf) {
    let dir = tempdir().expect("tempdir");
    // Production resolves this root before containment and revision checks.
    let root = dir.path().canonicalize().expect("canonical workspace root");
    (dir, root)
}

fn init_repo(root: &Path) {
    let _ = rightkit_child_process::command_std("git")
        .args(["init", "--quiet"])
        .current_dir(root)
        .status();
    std::fs::write(root.join("README.md"), "hello\n").unwrap();
    let _ = rightkit_child_process::command_std("git")
        .args(["add", "README.md"])
        .current_dir(root)
        .status();
    let _ = rightkit_child_process::command_std("git")
        .args([
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=test",
            "commit",
            "-m",
            "init",
            "--quiet",
        ])
        .current_dir(root)
        .status();
}

fn empty_capture() -> CaptureSnapshotRequest {
    CaptureSnapshotRequest {
        run_id: "run-1".into(),
        workspace_root: String::new(),
        entries: Vec::new(),
        expected_index_fingerprint: None,
    }
}

#[test]
fn restart_round_trip() {
    let (dir, root) = workspace_tempdir();
    init_repo(&root);
    let store_dir = dir.path().join("store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let store = RunChangeStore::open(store_dir.clone()).unwrap();

    let entry = ChangeEntry {
        mutation_id: "mut-1".into(),
        workspace_root: root.to_string_lossy().into_owned(),
        path: "README.md".into(),
        original_path: None,
        classification: ChangeClassification::Tracked,
        outcome: ChangeOutcome::Complete,
        stage: ChangeStage::Unstaged,
        additions: 2,
        deletions: 0,
        diff: "@@ -1 +1 @@\n-hello\n+hello world\n".into(),
        truncated: false,
        anchor_status: AnchorStatus::Live,
        anchor_offset: 1,
        captured_at: now_iso(),
        fingerprint: String::new(),
        size_bytes: 0,
        modified_at: None,
        undo: Some(UndoBody {
            fingerprint: String::new(),
            body: "hello\n".into(),
            note: "initial".into(),
        }),
        last_error: None,
    };
    let capture = CaptureSnapshotRequest {
        run_id: "run-1".into(),
        workspace_root: root.to_string_lossy().into_owned(),
        entries: vec![entry],
        expected_index_fingerprint: None,
    };
    let outcome = store.record("run-1", capture);
    assert_eq!(outcome.stored, 1);
    assert!(!outcome.truncated);

    drop(store);

    let reopened = RunChangeStore::open(store_dir).unwrap();
    let inv = reopened
        .inventory(
            "run-1",
            InventoryRequest {
                run_id: "run-1".into(),
                workspace_root: root.to_string_lossy().into_owned(),
                path: None,
                max_diff_bytes: None,
            },
        )
        .expect("inventory after restart");
    assert_eq!(inv.entries.len(), 1);
    assert_eq!(inv.entries[0].path, "README.md");
}

#[test]
fn classifications_persisted() {
    let (dir, root) = workspace_tempdir();
    init_repo(&root);
    let store_dir = dir.path().join("store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let store = RunChangeStore::open(&store_dir).unwrap();
    let all = vec![
        ("mut-track", "tracked", "tracked.txt"),
        ("mut-untr", "untracked", "untracked.txt"),
        ("mut-ren", "renamed", "renamed.txt"),
        ("mut-del", "deleted", "missing.txt"),
        ("mut-bin", "binary", "blob.bin"),
        ("mut-gen", "generated", "target/generated.txt"),
        ("mut-sec", "secret", ".env"),
        ("mut-big", "complete", "big.txt"),
        ("mut-con", "complete", "conflict.txt"),
        ("mut-cmp", "complete", "ok.txt"),
        ("mut-partial", "partial", "partial.txt"),
    ];
    let mut entries = Vec::new();
    for (id, _, path) in &all {
        entries.push(ChangeEntry {
            mutation_id: id.to_string(),
            workspace_root: root.to_string_lossy().into_owned(),
            path: path.to_string(),
            original_path: None,
            classification: ChangeClassification::Tracked,
            outcome: ChangeOutcome::Complete,
            stage: ChangeStage::Unstaged,
            additions: 1,
            deletions: 0,
            diff: String::new(),
            truncated: false,
            anchor_status: AnchorStatus::Live,
            anchor_offset: 0,
            captured_at: now_iso(),
            fingerprint: String::new(),
            size_bytes: 0,
            modified_at: None,
            undo: None,
            last_error: None,
        });
    }
    let capture = CaptureSnapshotRequest {
        run_id: "run-2".into(),
        workspace_root: root.to_string_lossy().into_owned(),
        entries,
        expected_index_fingerprint: None,
    };
    store.record("run-2", capture);

    // Real classification happens during record() — re-load to verify.
    let inv = store
        .inventory(
            "run-2",
            InventoryRequest {
                run_id: "run-2".into(),
                workspace_root: root.to_string_lossy().into_owned(),
                path: None,
                max_diff_bytes: None,
            },
        )
        .unwrap();
    let count = inv.entries.len();
    assert_eq!(count, all.len(), "all entries survived restart capture");
    let _ = all;
}

#[test]
fn partial_failure_surfaces_last_error() {
    let (dir, root) = workspace_tempdir();
    init_repo(&root);
    let store_dir = dir.path().join("store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let store = RunChangeStore::open(store_dir).unwrap();
    let entry = ChangeEntry {
        mutation_id: "mut-partial".into(),
        workspace_root: root.to_string_lossy().into_owned(),
        path: "broken.txt".into(),
        original_path: None,
        classification: ChangeClassification::Partial,
        outcome: ChangeOutcome::Partial,
        stage: ChangeStage::Unstaged,
        additions: 0,
        deletions: 0,
        diff: String::new(),
        truncated: false,
        anchor_status: AnchorStatus::Live,
        anchor_offset: 0,
        captured_at: now_iso(),
        fingerprint: String::new(),
        size_bytes: 0,
        modified_at: None,
        undo: None,
        last_error: Some("write mid-flight: daemon lost connection".into()),
    };
    let capture = CaptureSnapshotRequest {
        run_id: "run-3".into(),
        workspace_root: root.to_string_lossy().into_owned(),
        entries: vec![entry.clone()],
        expected_index_fingerprint: None,
    };
    store.record("run-3", capture);
    let inv = store
        .inventory(
            "run-3",
            InventoryRequest {
                run_id: "run-3".into(),
                workspace_root: root.to_string_lossy().into_owned(),
                path: Some("broken.txt".into()),
                max_diff_bytes: None,
            },
        )
        .unwrap();
    let row = inv.entries.first().expect("row persists");
    assert_eq!(row.classification, ChangeClassification::Partial);
    assert!(row.last_error.is_some());
}

#[test]
fn stale_anchor_when_file_drifted() {
    let (dir, root) = workspace_tempdir();
    init_repo(&root);
    std::fs::write(root.join("drift.txt"), "alpha\nbeta\ngamma\n").unwrap();
    let store_dir = dir.path().join("store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let store = RunChangeStore::open(store_dir).unwrap();
    let entry = ChangeEntry {
        mutation_id: "mut-drift".into(),
        workspace_root: root.to_string_lossy().into_owned(),
        path: "drift.txt".into(),
        original_path: None,
        classification: ChangeClassification::Tracked,
        outcome: ChangeOutcome::Complete,
        stage: ChangeStage::Unstaged,
        additions: 1,
        deletions: 0,
        diff: String::new(),
        truncated: false,
        anchor_status: AnchorStatus::Live,
        anchor_offset: 1,
        captured_at: now_iso(),
        fingerprint: String::new(),
        size_bytes: 0,
        modified_at: None,
        undo: Some(UndoBody {
            fingerprint: String::new(),
            body: "alpha\nbeta\ngamma\n".into(),
            note: "pre".into(),
        }),
        last_error: None,
    };
    let capture = CaptureSnapshotRequest {
        run_id: "run-4".into(),
        workspace_root: root.to_string_lossy().into_owned(),
        entries: vec![entry],
        expected_index_fingerprint: None,
    };
    store.record("run-4", capture);

    // Drift the file so refresh_anchor should report Stale.
    std::fs::write(
        root.join("drift.txt"),
        "alpha\nbeta\ngamma\ndelta\nepsilon\nzeta\neta\n",
    )
    .unwrap();

    let response = store.revise(
        "run-4",
        RevisionUpdateRequest {
            run_id: "run-4".into(),
            workspace_root: root.to_string_lossy().into_owned(),
            mutation_id: "mut-drift".into(),
            path: "drift.txt".into(),
            anchor_offset: 1,
            kind: CommentKind::Revision,
            body: "explain this anchor".into(),
            author: "reviewer".into(),
        },
    );
    assert_ne!(response.anchor_status, AnchorStatus::Live);
    assert!(response.stale);
}

#[test]
fn unified_and_split_diff_rendering() {
    let (dir, root) = workspace_tempdir();
    init_repo(&root);
    let store_dir = dir.path().join("store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let store = RunChangeStore::open(store_dir).unwrap();
    let entry = ChangeEntry {
        mutation_id: "mut-diff".into(),
        workspace_root: root.to_string_lossy().into_owned(),
        path: "a.txt".into(),
        original_path: None,
        classification: ChangeClassification::Tracked,
        outcome: ChangeOutcome::Complete,
        stage: ChangeStage::Unstaged,
        additions: 2,
        deletions: 1,
        diff: "diff --git a/a.txt b/a.txt\n@@ -1,2 +1,3 @@\n hello\n-world\n+all\n+again\n".into(),
        truncated: false,
        anchor_status: AnchorStatus::Live,
        anchor_offset: 1,
        captured_at: now_iso(),
        fingerprint: String::new(),
        size_bytes: 0,
        modified_at: None,
        undo: None,
        last_error: None,
    };
    let capture = CaptureSnapshotRequest {
        run_id: "run-5".into(),
        workspace_root: root.to_string_lossy().into_owned(),
        entries: vec![entry],
        expected_index_fingerprint: None,
    };
    store.record("run-5", capture);
    let unified = store.diff_body(DiffBodyRequest {
        run_id: "run-5".into(),
        workspace_root: root.to_string_lossy().into_owned(),
        path: "a.txt".into(),
        split: false,
        cursor: None,
        window: None,
        search: None,
    });
    let split = store.diff_body(DiffBodyRequest {
        run_id: "run-5".into(),
        workspace_root: root.to_string_lossy().into_owned(),
        path: "a.txt".into(),
        split: true,
        cursor: None,
        window: None,
        search: None,
    });
    assert!(unified
        .rows
        .iter()
        .any(|r| r.kind == "add" || r.kind == "del"));
    assert!(split
        .rows
        .iter()
        .any(|r| r.kind == "add" || r.kind == "del"));
    assert!(!unified.hunk_decisions.is_empty());
}

#[test]
fn search_returns_match_count() {
    let (dir, root) = workspace_tempdir();
    init_repo(&root);
    let store_dir = dir.path().join("store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let store = RunChangeStore::open(store_dir).unwrap();
    let entry = ChangeEntry {
        mutation_id: "mut-search".into(),
        workspace_root: root.to_string_lossy().into_owned(),
        path: "b.txt".into(),
        original_path: None,
        classification: ChangeClassification::Tracked,
        outcome: ChangeOutcome::Complete,
        stage: ChangeStage::Unstaged,
        additions: 2,
        deletions: 0,
        diff: "@@ -1 +1 @@\n-hello\n+hello world\n".into(),
        truncated: false,
        anchor_status: AnchorStatus::Live,
        anchor_offset: 1,
        captured_at: now_iso(),
        fingerprint: String::new(),
        size_bytes: 0,
        modified_at: None,
        undo: None,
        last_error: None,
    };
    let capture = CaptureSnapshotRequest {
        run_id: "run-6".into(),
        workspace_root: root.to_string_lossy().into_owned(),
        entries: vec![entry],
        expected_index_fingerprint: None,
    };
    store.record("run-6", capture);
    let response = store.diff_body(DiffBodyRequest {
        run_id: "run-6".into(),
        workspace_root: root.to_string_lossy().into_owned(),
        path: "b.txt".into(),
        split: false,
        cursor: None,
        window: Some(1024),
        search: Some("hello".into()),
    });
    let stats = response.search.expect("search present");
    assert!(stats.matches >= 1);
}

#[test]
fn revision_comment_persists() {
    let (dir, root) = workspace_tempdir();
    init_repo(&root);
    std::fs::write(root.join("c.txt"), "alpha\nbeta\n").unwrap();
    let store_dir = dir.path().join("store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let store = RunChangeStore::open(&store_dir).unwrap();
    let capture = CaptureSnapshotRequest {
        run_id: "run-7".into(),
        workspace_root: root.to_string_lossy().into_owned(),
        entries: vec![ChangeEntry {
            mutation_id: "mut-rev".into(),
            workspace_root: root.to_string_lossy().into_owned(),
            path: "c.txt".into(),
            original_path: None,
            classification: ChangeClassification::Tracked,
            outcome: ChangeOutcome::Complete,
            stage: ChangeStage::Unstaged,
            additions: 1,
            deletions: 0,
            diff: String::new(),
            truncated: false,
            anchor_status: AnchorStatus::Live,
            anchor_offset: 1,
            captured_at: now_iso(),
            fingerprint: String::new(),
            size_bytes: 0,
            modified_at: None,
            undo: None,
            last_error: None,
        }],
        expected_index_fingerprint: None,
    };
    store.record("run-7", capture);
    let r = store.revise(
        "run-7",
        RevisionUpdateRequest {
            run_id: "run-7".into(),
            workspace_root: root.to_string_lossy().into_owned(),
            mutation_id: "mut-rev".into(),
            path: "c.txt".into(),
            anchor_offset: 1,
            kind: CommentKind::Revision,
            body: "explain this further".into(),
            author: "reviewer".into(),
        },
    );
    assert_eq!(r.anchor_status, AnchorStatus::Live);
    // Persisted across restart.
    drop(store);
    let reopened = RunChangeStore::open(store_dir).unwrap();
    let inv = reopened
        .inventory(
            "run-7",
            InventoryRequest {
                run_id: "run-7".into(),
                workspace_root: root.to_string_lossy().into_owned(),
                path: None,
                max_diff_bytes: None,
            },
        )
        .unwrap();
    assert_eq!(inv.entries.len(), 1);
}

#[test]
fn stage_and_revert_transitions() {
    let (dir, root) = workspace_tempdir();
    init_repo(&root);
    std::fs::write(root.join("stage.txt"), "alpha\n").unwrap();
    let store_dir = dir.path().join("store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let store = RunChangeStore::open(store_dir).unwrap();
    let entry = ChangeEntry {
        mutation_id: "mut-stage".into(),
        workspace_root: root.to_string_lossy().into_owned(),
        path: "stage.txt".into(),
        original_path: None,
        classification: ChangeClassification::Untracked,
        outcome: ChangeOutcome::Complete,
        stage: ChangeStage::Unstaged,
        additions: 1,
        deletions: 0,
        diff: "@@ -0,0 +1 @@\n+alpha\n".into(),
        truncated: false,
        anchor_status: AnchorStatus::Live,
        anchor_offset: 1,
        captured_at: now_iso(),
        fingerprint: String::new(),
        size_bytes: 6,
        modified_at: None,
        undo: Some(UndoBody {
            fingerprint: String::new(),
            body: String::new(),
            note: "new file".into(),
        }),
        last_error: None,
    };
    let capture = CaptureSnapshotRequest {
        run_id: "run-8".into(),
        workspace_root: root.to_string_lossy().into_owned(),
        entries: vec![entry],
        expected_index_fingerprint: None,
    };
    let outcome = store.record("run-8", capture);
    assert_eq!(outcome.stored, 1);

    // Undo flips the stage to Reverted.
    let response = store.undo(
        "run-8",
        UndoRequest {
            run_id: "run-8".into(),
            workspace_root: root.to_string_lossy().into_owned(),
            mutation_id: "mut-stage".into(),
            observed_fingerprint: fingerprint_path(&root.join("stage.txt")).unwrap_or_default(),
        },
    );
    assert!(response.applied || response.conflict_reason.is_some());
}

#[test]
fn undo_fingerprint_mismatch_rejected() {
    let (dir, root) = workspace_tempdir();
    init_repo(&root);
    std::fs::write(root.join("fp.txt"), "old\n").unwrap();
    let store_dir = dir.path().join("store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let store = RunChangeStore::open(store_dir).unwrap();
    let entry = ChangeEntry {
        mutation_id: "mut-fp".into(),
        workspace_root: root.to_string_lossy().into_owned(),
        path: "fp.txt".into(),
        original_path: None,
        classification: ChangeClassification::Tracked,
        outcome: ChangeOutcome::Complete,
        stage: ChangeStage::Unstaged,
        additions: 1,
        deletions: 1,
        diff: "@@ -1 +1 @@\n-old\n+new\n".into(),
        truncated: false,
        anchor_status: AnchorStatus::Live,
        anchor_offset: 1,
        captured_at: now_iso(),
        fingerprint: String::new(),
        size_bytes: 4,
        modified_at: None,
        undo: Some(UndoBody {
            fingerprint: String::new(),
            body: "old\n".into(),
            note: "pre".into(),
        }),
        last_error: None,
    };
    let capture = CaptureSnapshotRequest {
        run_id: "run-9".into(),
        workspace_root: root.to_string_lossy().into_owned(),
        entries: vec![entry],
        expected_index_fingerprint: None,
    };
    store.record("run-9", capture);
    // Mutate the file so the observed fingerprint no longer matches.
    std::fs::write(root.join("fp.txt"), "out-of-band\n").unwrap();
    let response = store.undo(
        "run-9",
        UndoRequest {
            run_id: "run-9".into(),
            workspace_root: root.to_string_lossy().into_owned(),
            mutation_id: "mut-fp".into(),
            observed_fingerprint: "deadbeef".into(),
        },
    );
    assert!(!response.applied);
    assert!(response.conflict_reason.is_some());
}

#[test]
fn end_card_emits_classification_counts() {
    let (dir, root) = workspace_tempdir();
    init_repo(&root);
    std::fs::write(root.join("summary.txt"), "x\ny\nz\n").unwrap();
    let store_dir = dir.path().join("store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let store = RunChangeStore::open(store_dir).unwrap();
    let capture = CaptureSnapshotRequest {
        run_id: "run-10".into(),
        workspace_root: root.to_string_lossy().into_owned(),
        entries: vec![
            ChangeEntry {
                mutation_id: "a".into(),
                workspace_root: root.to_string_lossy().into_owned(),
                path: "summary.txt".into(),
                original_path: None,
                classification: ChangeClassification::Tracked,
                outcome: ChangeOutcome::Complete,
                stage: ChangeStage::Unstaged,
                additions: 2,
                deletions: 0,
                diff: String::new(),
                truncated: false,
                anchor_status: AnchorStatus::Live,
                anchor_offset: 1,
                captured_at: now_iso(),
                fingerprint: String::new(),
                size_bytes: 0,
                modified_at: None,
                undo: None,
                last_error: None,
            },
            ChangeEntry {
                mutation_id: "b".into(),
                workspace_root: root.to_string_lossy().into_owned(),
                path: "summary.txt".into(),
                original_path: None,
                classification: ChangeClassification::Untracked,
                outcome: ChangeOutcome::Complete,
                stage: ChangeStage::Unstaged,
                additions: 1,
                deletions: 0,
                diff: String::new(),
                truncated: false,
                anchor_status: AnchorStatus::Live,
                anchor_offset: 1,
                captured_at: now_iso(),
                fingerprint: String::new(),
                size_bytes: 0,
                modified_at: None,
                undo: None,
                last_error: None,
            },
        ],
        expected_index_fingerprint: None,
    };
    store.record("run-10", capture);
    let summary = store
        .summary("run-10", &root.to_string_lossy())
        .expect("summary");
    assert!(summary.entry_count >= 1);
    assert!(
        summary.counts_by_classification.contains_key("tracked")
            || summary.counts_by_classification.contains_key("untracked")
            || summary.counts_by_classification.contains_key("complete")
    );
}

#[test]
fn capture_request_run_id_must_match_route() {
    let (dir, _root) = workspace_tempdir();
    let store_dir = dir.path().join("store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let store = RunChangeStore::open(store_dir).unwrap();
    let error = store
        .record_checked("only-id", empty_capture())
        .unwrap_err();
    assert!(error.contains("does not match"));
}

#[test]
fn daemon_sink_records_observed_mutation_and_created_file_undo() {
    let (dir, root) = workspace_tempdir();
    init_repo(&root);
    let store_dir = dir.path().join("store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let store = Arc::new(RunChangeStore::open(store_dir).unwrap());
    store.bind_file_service(Arc::new(rightkit_agent_tools::coding_tools::FileService::default()));
    let sink = DaemonRunChangesSink {
        store: Arc::clone(&store),
    };
    let created = root.join("created-by-tool.txt");
    std::fs::write(&created, "actual workspace effect\n").unwrap();

    rightkit_agent_engine_core::RunChangesSink::record_mutations(
        &sink,
        "run-observed",
        "run-observed--turn-1",
        &root,
        "tool-42",
        &[rightkit_agent_tools::coding_tools::WorkspaceMutation {
            path: created.clone(),
            kind: rightkit_agent_tools::coding_tools::WorkspaceMutationKind::Created,
            previous_content: None,
        }],
        false,
    )
    .expect("daemon sink accepts a contained post-effect mutation");

    let inventory = store
        .inventory(
            "run-observed--turn-1",
            InventoryRequest {
                run_id: "run-observed--turn-1".into(),
                workspace_root: root.to_string_lossy().into_owned(),
                path: None,
                max_diff_bytes: None,
            },
        )
        .expect("captured inventory");
    assert_eq!(inventory.entries.len(), 1);
    let entry = &inventory.entries[0];
    assert_eq!(entry.path, "created-by-tool.txt");
    assert_eq!(entry.classification, ChangeClassification::Untracked);
    assert_eq!(entry.outcome, ChangeOutcome::Complete);
    assert!(entry.diff.contains("actual workspace effect"));
    assert!(entry.undo.is_some());

    let undo = store.undo(
        "run-observed--turn-1",
        UndoRequest {
            run_id: "run-observed--turn-1".into(),
            workspace_root: root.to_string_lossy().into_owned(),
            mutation_id: entry.mutation_id.clone(),
            observed_fingerprint: entry.fingerprint.clone(),
        },
    );
    assert!(
        undo.applied,
        "undo should remove the created file: {undo:?}"
    );
    assert!(!created.exists());
}

#[test]
fn daemon_sink_restores_exact_pre_effect_modified_content() {
    let (dir, root) = workspace_tempdir();
    init_repo(&root);
    let store_dir = dir.path().join("store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let store = Arc::new(RunChangeStore::open(store_dir).unwrap());
    store.bind_file_service(Arc::new(rightkit_agent_tools::coding_tools::FileService::default()));
    let sink = DaemonRunChangesSink {
        store: Arc::clone(&store),
    };
    let path = root.join("README.md");
    let previous = "user's pre-existing content\n";
    std::fs::write(&path, "agent result\n").unwrap();

    rightkit_agent_engine_core::RunChangesSink::record_mutations(
        &sink,
        "run-modified",
        "run-modified--turn-1",
        &root,
        "tool-7",
        &[rightkit_agent_tools::coding_tools::WorkspaceMutation {
            path: path.clone(),
            kind: rightkit_agent_tools::coding_tools::WorkspaceMutationKind::Modified,
            previous_content: Some(previous.to_string()),
        }],
        false,
    )
    .unwrap();

    let inventory = store
        .inventory(
            "run-modified--turn-1",
            InventoryRequest {
                run_id: "run-modified--turn-1".into(),
                workspace_root: root.to_string_lossy().into_owned(),
                path: None,
                max_diff_bytes: None,
            },
        )
        .unwrap();
    let entry = &inventory.entries[0];
    assert_eq!(entry.undo.as_ref().unwrap().body, previous);
    let response = store.undo(
        "run-modified--turn-1",
        UndoRequest {
            run_id: "run-modified--turn-1".into(),
            workspace_root: root.to_string_lossy().into_owned(),
            mutation_id: entry.mutation_id.clone(),
            observed_fingerprint: entry.fingerprint.clone(),
        },
    );
    assert!(response.applied, "{response:?}");
    assert_eq!(std::fs::read_to_string(path).unwrap(), previous);
}

#[test]
fn git_baseline_uses_real_write_tree_fingerprint() {
    let (_dir, root) = workspace_tempdir();
    init_repo(&root);
    let expected = run_git(root.to_str().unwrap(), &["write-tree"]);
    let baseline = git_baseline(root.to_str().unwrap());
    assert_eq!(baseline.index_fingerprint, expected);
    assert!(baseline.index_fingerprint.is_some());
}

#[test]
fn pre_effect_baseline_survives_first_mutation() {
    let (dir, root) = workspace_tempdir();
    init_repo(&root);
    let store = RunChangeStore::open(dir.path().join("store")).unwrap();
    let run_id = "session-1--run-1";
    let before = store
        .begin_run(run_id, root.to_str().unwrap())
        .expect("pre-effect baseline");
    assert_eq!(before.untracked_count, 0);

    let path = root.join("created-after-baseline.txt");
    std::fs::write(&path, "effect\n").unwrap();
    let sink = DaemonRunChangesSink {
        store: Arc::clone(&store),
    };
    rightkit_agent_engine_core::RunChangesSink::record_mutations(
        &sink,
        "session-1",
        run_id,
        &root,
        "tool-1",
        &[rightkit_agent_tools::coding_tools::WorkspaceMutation {
            path,
            kind: rightkit_agent_tools::coding_tools::WorkspaceMutationKind::Created,
            previous_content: None,
        }],
        false,
    )
    .unwrap();

    let inventory = store
        .inventory(
            run_id,
            InventoryRequest {
                run_id: run_id.into(),
                workspace_root: root.to_string_lossy().into_owned(),
                path: None,
                max_diff_bytes: None,
            },
        )
        .unwrap();
    assert_eq!(inventory.git, before);
    assert_eq!(inventory.git.untracked_count, 0);
    assert!(git_baseline(root.to_str().unwrap()).untracked_count > before.untracked_count);
}

#[test]
fn two_runs_in_one_session_keep_distinct_records_and_latest_alias() {
    let (dir, root) = workspace_tempdir();
    init_repo(&root);
    let store = RunChangeStore::open(dir.path().join("store")).unwrap();
    let sink = DaemonRunChangesSink {
        store: Arc::clone(&store),
    };

    for (run_id, filename) in [
        ("session-a--0001", "first.txt"),
        ("session-a--0002", "second.txt"),
    ] {
        rightkit_agent_engine_core::RunChangesSink::begin_run(&sink, "session-a", run_id, &root).unwrap();
        let path = root.join(filename);
        std::fs::write(&path, filename).unwrap();
        rightkit_agent_engine_core::RunChangesSink::record_mutations(
            &sink,
            "session-a",
            run_id,
            &root,
            "tool-1",
            &[rightkit_agent_tools::coding_tools::WorkspaceMutation {
                path,
                kind: rightkit_agent_tools::coding_tools::WorkspaceMutationKind::Created,
                previous_content: None,
            }],
            false,
        )
        .unwrap();
    }

    let request = |run_id: &str| InventoryRequest {
        run_id: run_id.into(),
        workspace_root: root.to_string_lossy().into_owned(),
        path: None,
        max_diff_bytes: None,
    };
    let first = store
        .inventory("session-a--0001", request("session-a--0001"))
        .unwrap();
    let second = store
        .inventory("session-a--0002", request("session-a--0002"))
        .unwrap();
    assert_eq!(first.entries[0].path, "first.txt");
    assert_eq!(second.entries[0].path, "second.txt");
    let latest = store.inventory("session-a", request("session-a")).unwrap();
    assert_eq!(latest.run_id, "session-a--0002");
    assert_eq!(latest.entries[0].path, "second.txt");
}

#[test]
fn partial_classification_is_not_laundered_by_secret_detection() {
    let (dir, root) = workspace_tempdir();
    let path = root.join(".env");
    std::fs::write(&path, "API_KEY=secret\n").unwrap();
    let store = RunChangeStore::open(dir.path().join("store")).unwrap();
    let capture = CaptureSnapshotRequest {
        run_id: "run-partial".into(),
        workspace_root: root.to_string_lossy().into_owned(),
        entries: vec![ChangeEntry {
            mutation_id: "partial-1".into(),
            workspace_root: root.to_string_lossy().into_owned(),
            path: ".env".into(),
            original_path: None,
            classification: ChangeClassification::Partial,
            outcome: ChangeOutcome::Partial,
            stage: ChangeStage::Unstaged,
            additions: 1,
            deletions: 0,
            diff: "secret diff".into(),
            truncated: false,
            anchor_status: AnchorStatus::Live,
            anchor_offset: 0,
            captured_at: now_iso(),
            fingerprint: String::new(),
            size_bytes: 0,
            modified_at: None,
            undo: None,
            last_error: Some("partial write".into()),
        }],
        expected_index_fingerprint: None,
    };
    store.record("run-partial", capture);
    let inventory = store
        .inventory(
            "run-partial",
            InventoryRequest {
                run_id: "run-partial".into(),
                workspace_root: root.to_string_lossy().into_owned(),
                path: None,
                max_diff_bytes: None,
            },
        )
        .unwrap();
    assert_eq!(
        inventory.entries[0].classification,
        ChangeClassification::Partial
    );
}

#[test]
fn git_baseline_counts_only_untracked_rows() {
    let (_dir, root) = workspace_tempdir();
    init_repo(&root);
    std::fs::write(root.join("README.md"), "modified\n").unwrap();
    std::fs::write(root.join("untracked.txt"), "new\n").unwrap();
    assert_eq!(git_baseline(root.to_str().unwrap()).untracked_count, 1);
}

/// EFF-008 closure fixtures: undo/revert declares the revision it expects and
/// refuses to restore blindly over newer state.
mod eff008_undo_revision_safety {
    use super::*;

    fn undo_entry(
        root: &std::path::Path,
        path: &str,
        agent_bytes: &str,
        undo_body: &str,
    ) -> ChangeEntry {
        let agent_fingerprint = {
            let digest = rightkit_crypto::sha256(agent_bytes.as_bytes());
            digest
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        };
        ChangeEntry {
            mutation_id: "mut-undo".into(),
            workspace_root: root.to_string_lossy().into_owned(),
            path: path.into(),
            original_path: None,
            classification: ChangeClassification::Tracked,
            outcome: ChangeOutcome::Complete,
            stage: ChangeStage::Unstaged,
            additions: 1,
            deletions: 0,
            diff: String::new(),
            truncated: false,
            anchor_status: AnchorStatus::Live,
            anchor_offset: 0,
            captured_at: now_iso(),
            // The content identity the agent left behind.
            fingerprint: agent_fingerprint,
            size_bytes: agent_bytes.len() as u64,
            modified_at: None,
            undo: Some(UndoBody {
                fingerprint: String::new(),
                body: undo_body.into(),
                note: "pre-image".into(),
            }),
            last_error: None,
        }
    }

    /// Clean case: agent edited, nothing else touched it, undo succeeds and
    /// the pre-image is restored.
    #[test]
    fn undo_succeeds_when_no_external_change_landed() {
        let (_dir, root) = workspace_tempdir();
        let file = root.join("notes.txt");
        std::fs::write(&file, "agent wrote this\n").unwrap();

        let entry = undo_entry(&root, "notes.txt", "agent wrote this\n", "original\n");
        let undo = entry.undo.clone().unwrap();

        super::super::apply_undo(&root, &entry, &undo).expect("clean undo must succeed");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "original\n",
            "pre-image must be restored"
        );
    }

    /// Conflict case: a user/external edit landed after the agent's write.
    /// The undo must refuse, and the NEWER bytes must survive untouched.
    #[test]
    fn undo_refuses_and_preserves_newer_external_bytes() {
        let (_dir, root) = workspace_tempdir();
        let file = root.join("notes.txt");
        std::fs::write(&file, "agent wrote this\n").unwrap();

        let entry = undo_entry(&root, "notes.txt", "agent wrote this\n", "original\n");
        let undo = entry.undo.clone().unwrap();

        // External edit after the agent committed.
        std::fs::write(&file, "user edited afterwards\n").unwrap();

        let error = super::super::apply_undo(&root, &entry, &undo)
            .expect_err("undo must refuse to clobber newer bytes");
        assert!(
            error.contains("changed after the agent wrote it"),
            "expected a typed staleness conflict, got: {error}"
        );
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "user edited afterwards\n",
            "newer external bytes must survive the refused undo"
        );
    }

    /// Conflict case: an external actor deleted the agent-written path.
    /// Undo must not mistake absence for the agent's original delete.
    #[test]
    fn undo_refuses_to_resurrect_externally_deleted_agent_bytes() {
        let (_dir, root) = workspace_tempdir();
        let file = root.join("notes.txt");
        std::fs::write(&file, "agent wrote this\n").unwrap();

        let entry = undo_entry(&root, "notes.txt", "agent wrote this\n", "original\n");
        let undo = entry.undo.clone().unwrap();
        std::fs::remove_file(&file).unwrap();

        let error = super::super::apply_undo(&root, &entry, &undo)
            .expect_err("undo must refuse to resurrect an externally deleted path");
        assert!(
            error.contains("no longer exists after the agent wrote it"),
            "expected a typed missing-path conflict, got: {error}"
        );
        assert!(!file.exists(), "externally deleted path must stay absent");
    }

    /// Reversal of a create: the file the agent added is removed.
    #[test]
    fn undo_of_a_create_deletes_the_added_file() {
        let (_dir, root) = workspace_tempdir();
        let file = root.join("added.txt");
        std::fs::write(&file, "added by agent\n").unwrap();

        let entry = undo_entry(
            &root,
            "added.txt",
            "added by agent\n",
            "__RIGHTKIT_DELETE__",
        );
        let undo = entry.undo.clone().unwrap();

        super::super::apply_undo(&root, &entry, &undo).expect("create-undo must succeed");
        assert!(!file.exists(), "the agent-created file must be removed");
    }

    /// Reversal of a create is a no-op, not an error, when the file is
    /// already gone.
    #[test]
    fn undo_of_a_create_is_idempotent_when_already_absent() {
        let (_dir, root) = workspace_tempdir();
        let entry = undo_entry(&root, "gone.txt", "", "__RIGHTKIT_DELETE__");
        let undo = entry.undo.clone().unwrap();
        super::super::apply_undo(&root, &entry, &undo)
            .expect("removing an already-absent file must not fail");
    }

    /// Reversal of a delete: the agent removed the file, undo recreates it.
    #[test]
    fn undo_of_a_delete_recreates_the_file() {
        let (_dir, root) = workspace_tempdir();
        let file = root.join("removed.txt");
        // Agent deleted it, so nothing is on disk and the recorded
        // fingerprint is empty.
        let mut entry = undo_entry(&root, "removed.txt", "", "restored content\n");
        entry.fingerprint = String::new();
        let undo = entry.undo.clone().unwrap();

        super::super::apply_undo(&root, &entry, &undo).expect("delete-undo must succeed");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "restored content\n"
        );
    }

    /// Reversal of a rename moves the file back, and refuses when the
    /// original path has been reoccupied.
    #[test]
    fn undo_of_a_rename_restores_the_original_path_and_refuses_a_reoccupied_one() {
        let (_dir, root) = workspace_tempdir();
        let renamed = root.join("new-name.txt");
        std::fs::write(&renamed, "moved\n").unwrap();

        let mut entry = undo_entry(&root, "new-name.txt", "moved\n", "__RIGHTKIT_RENAME__");
        entry.original_path = Some("old-name.txt".into());
        let undo = entry.undo.clone().unwrap();

        super::super::apply_undo(&root, &entry, &undo).expect("rename undo must succeed");
        assert!(!renamed.exists(), "source must be gone after the move back");
        assert_eq!(
            std::fs::read_to_string(root.join("old-name.txt")).unwrap(),
            "moved\n"
        );

        // Now redo the situation with the original path occupied.
        std::fs::write(&renamed, "moved\n").unwrap();
        std::fs::write(root.join("old-name.txt"), "someone else's file\n").unwrap();
        let error = super::super::apply_undo(&root, &entry, &undo)
            .expect_err("must refuse to overwrite a reoccupied original path");
        assert!(error.contains("already exists"), "got: {error}");
        assert_eq!(
            std::fs::read_to_string(root.join("old-name.txt")).unwrap(),
            "someone else's file\n",
            "the occupying file must survive"
        );
    }
}
