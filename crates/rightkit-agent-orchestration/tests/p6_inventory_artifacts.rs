//! Tests for the P6 packet port: `rightkit_agent_orchestration::p6_inventory::artifacts`.
//!
//! Mirrors the behaviour exercised (directly or transitively) by the
//! legacy JS in `src/lib/artifacts/{digests,paths}.mjs` and
//! `src/lib/naming/{registry,migrations}.mjs`.

use std::path::Path;

use rightkit_agent_orchestration::p6_inventory::artifacts::{
    digest_bytes, safe_artifact_path, ArtifactPathError,
};

// ---- artifacts::digest_bytes ----

#[test]
fn digest_bytes_matches_known_sha256() {
    // sha256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
    assert_eq!(
        digest_bytes(b""),
        "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    // sha256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
    assert_eq!(
        digest_bytes(b"abc"),
        "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

// ---- artifacts::safe_artifact_path ----

#[test]
fn safe_artifact_path_allows_nested_relative_child() {
    let root = Path::new("/runs/current");
    let resolved = safe_artifact_path(root, "logs/output.txt").unwrap();
    assert_eq!(resolved, Path::new("/runs/current/logs/output.txt"));
}

#[test]
fn safe_artifact_path_rejects_absolute_child() {
    let root = Path::new("/runs/current");
    assert_eq!(
        safe_artifact_path(root, "/etc/passwd").unwrap_err(),
        ArtifactPathError::AbsoluteChild
    );
}

#[test]
fn safe_artifact_path_rejects_windows_drive_absolute_child() {
    let root = Path::new("/runs/current");
    assert_eq!(
        safe_artifact_path(root, "C:\\secrets.txt").unwrap_err(),
        ArtifactPathError::AbsoluteChild
    );
}

#[test]
fn safe_artifact_path_rejects_traversal_segment() {
    let root = Path::new("/runs/current");
    assert_eq!(
        safe_artifact_path(root, "../outside.txt").unwrap_err(),
        ArtifactPathError::Traversal
    );
    assert_eq!(
        safe_artifact_path(root, "nested/../../outside.txt").unwrap_err(),
        ArtifactPathError::Traversal
    );
}

#[test]
fn safe_artifact_path_rejects_empty_child() {
    let root = Path::new("/runs/current");
    assert_eq!(
        safe_artifact_path(root, "").unwrap_err(),
        ArtifactPathError::AbsoluteChild
    );
}
