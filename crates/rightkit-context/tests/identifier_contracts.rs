use rightkit_context_runtime::{CheckpointSourceRefV1, CheckpointV1};

fn checkpoint_with_source(source_ref: &str, expected_content_hash: &str) -> CheckpointV1 {
    CheckpointV1 {
        checkpoint_id: "checkpoint/identifier-contract/100".into(),
        installation_id: "550e8400-e29b-41d4-a716-446655440000".into(),
        client: "codex".into(),
        session_id: "identifier-contract".into(),
        repository_id: "repo-1".into(),
        worktree_rev: "rev-1".into(),
        scope_id: "repo-1".into(),
        summary: "freeze identifier compatibility".into(),
        goal_snapshot: None,
        task_snapshot: None,
        created_at_ms: 100,
        expires_at_ms: 200,
        source_refs: vec![CheckpointSourceRefV1 {
            source_ref: source_ref.into(),
            expected_content_hash: expected_content_hash.into(),
            anchor_id: "sec:ledger:1".into(),
            label: "ledger".into(),
        }],
    }
}

fn source_status(root: &std::path::Path, source_ref: &str, expected_hash: &str) -> String {
    rightkit_context_runtime::checkpoint::resolve_source_refs(
        &checkpoint_with_source(source_ref, expected_hash),
        root,
    )[0]
    .status
    .clone()
}

#[test]
fn worktree_document_identifier_compatibility_corpus() {
    let root = tempfile::tempdir().unwrap();
    let content = "# Ledger\nbody";
    std::fs::create_dir_all(root.path().join("docs")).unwrap();
    std::fs::write(root.path().join("docs/ledger.md"), content).unwrap();
    std::fs::write(root.path().join("docs/guía.md"), content).unwrap();
    let expected_hash = rightkit_context_runtime::ledger::outline::build_outline(
        "doc://repo/worktree/docs/ledger.md",
        content,
        "comrak-0.54.0",
    )
    .content_hash;

    for source_ref in [
        "doc://repo/worktree/docs/ledger.md",
        "doc://repo/worktree/docs/guía.md",
    ] {
        assert_eq!(source_status(root.path(), source_ref, &expected_hash), "ok");
    }
    for source_ref in [
        "docs/ledger.md",
        "Doc://repo/worktree/docs/ledger.md",
        "doc://other/worktree/docs/ledger.md",
        "doc://repo/other/docs/ledger.md",
        "doc://repo/worktree/../ledger.md",
    ] {
        assert_eq!(
            source_status(root.path(), source_ref, &expected_hash),
            "deny"
        );
    }
}

#[test]
fn worktree_document_rejections_are_typed_before_io() {
    use rightkit_context_runtime::ledger::identifier::{WorktreeDocRef, WorktreeDocRefError};

    for (source_ref, expected) in [
        ("doc://repo/worktree/", WorktreeDocRefError::EmptyPath),
        (
            "doc://repo/worktree/docs//ledger.md",
            WorktreeDocRefError::AliasCollision,
        ),
        (
            "doc://repo/worktree/docs/%2e%2e/ledger.md",
            WorktreeDocRefError::AliasCollision,
        ),
        (
            "doc://repo/worktree/C:/repo/ledger.md",
            WorktreeDocRefError::WindowsPath,
        ),
        (
            "doc://repo/worktree/docs\\ledger.md",
            WorktreeDocRefError::WindowsPath,
        ),
        (
            "doc://repo/worktree/docs/∕ledger.md",
            WorktreeDocRefError::UnicodeAmbiguity,
        ),
    ] {
        assert_eq!(WorktreeDocRef::parse(source_ref).unwrap_err(), expected);
    }

    assert_eq!(
        WorktreeDocRef::parse("doc://other/worktree/docs/ledger.md").unwrap_err(),
        WorktreeDocRefError::CrossRepository
    );
    assert_eq!(
        WorktreeDocRef::parse("Doc://repo/worktree/docs/ledger.md").unwrap_err(),
        WorktreeDocRefError::PrefixMismatch
    );
}

#[test]
fn anchor_rejections_are_typed_before_io() {
    use rightkit_context_runtime::ledger::identifier::{AnchorRef, AnchorRefError};

    assert_eq!(
        AnchorRef::parse(&format!("mr://anchor/{}", "A".repeat(64))).unwrap_err(),
        AnchorRefError::AliasCollision
    );
    assert_eq!(
        AnchorRef::parse(&format!("mr://anchor/{}", "g".repeat(64))).unwrap_err(),
        AnchorRefError::InvalidDigest
    );
    assert_eq!(
        AnchorRef::parse(&format!("mr://other/{}", "0".repeat(64))).unwrap_err(),
        AnchorRefError::CrossNamespace
    );
    assert_eq!(
        AnchorRef::parse(&format!("MR://anchor/{}", "0".repeat(64))).unwrap_err(),
        AnchorRefError::PrefixMismatch
    );
}
