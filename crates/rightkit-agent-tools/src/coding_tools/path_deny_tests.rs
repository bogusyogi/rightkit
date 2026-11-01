//! Issue #222 (B-8): configured `deny_read` / `deny_write` globs must be
//! enforced inside every file-touching tool, on the canonical path, before IO.

use crate::coding_tools::apply_patch::ApplyPatchTool;
use crate::coding_tools::code_mode::{
    execute_program, ContextToolDispatcher, NestedToolDispatcher, NestedToolFuture, ProgramToolSpec,
};
use crate::coding_tools::context::ToolContext;
use crate::coding_tools::edit_file::EditFileTool;
use crate::coding_tools::list_files::ListFilesTool;
use crate::coding_tools::multi_edit::MultiEditTool;
use crate::coding_tools::read_file::ReadFileTool;
use crate::coding_tools::search::SearchTool;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use crate::coding_tools::undo_edit::UndoEditTool;
use crate::coding_tools::write_file::WriteFileTool;
use rightkit_agent_policy::{DenyGlobSet, PermissionMode, WorkspaceTrust};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use tempfile::tempdir;

fn ctx(root: &Path) -> ToolContext {
    let deny = DenyGlobSet::with_additional_patterns(
        ["**/*.vault".to_string()],
        ["frozen/**".to_string()],
    )
    .unwrap();
    // Bypass: a configured deny must beat even the most permissive mode.
    ToolContext::new(
        "deny-s",
        root,
        PermissionMode::Bypass,
        WorkspaceTrust::Trusted,
    )
    .with_path_deny(Some(Arc::new(deny)))
}

fn assert_denied(result: &ToolResult, kind: &str) {
    assert!(result.is_error, "expected deny, got: {}", result.content);
    assert!(
        result.content.contains(&format!("deny-glob:{kind}")),
        "expected deny-glob:{kind}, got: {}",
        result.content
    );
}

fn seed_read(root: &Path) {
    std::fs::write(root.join("keys.vault"), "TOPSECRET\n").unwrap();
    std::fs::write(root.join("ok.txt"), "needle ok\n").unwrap();
}

fn seed_write(root: &Path) {
    std::fs::create_dir_all(root.join("frozen")).unwrap();
    std::fs::write(root.join("frozen/a.txt"), "alpha\n").unwrap();
}

#[tokio::test]
async fn read_file_enforces_configured_read_deny() {
    let dir = tempdir().unwrap();
    seed_read(dir.path());
    let c = ctx(dir.path());
    let denied = ReadFileTool
        .execute(json!({"path": "keys.vault"}), &c)
        .await;
    assert_denied(&denied, "read");
    assert!(!denied.content.contains("TOPSECRET"));
    // Lexical dodge: dot-dot spelling hits the same canonical path.
    let dodge = ReadFileTool
        .execute(json!({"path": "sub/../keys.vault"}), &c)
        .await;
    assert_denied(&dodge, "read");
    let ok = ReadFileTool.execute(json!({"path": "ok.txt"}), &c).await;
    assert!(!ok.is_error, "{}", ok.content);
}

#[cfg(unix)]
#[tokio::test]
async fn read_file_denies_through_in_workspace_symlink() {
    let dir = tempdir().unwrap();
    seed_read(dir.path());
    std::os::unix::fs::symlink(
        dir.path().join("keys.vault"),
        dir.path().join("innocent.txt"),
    )
    .unwrap();
    let r = ReadFileTool
        .execute(json!({"path": "innocent.txt"}), &ctx(dir.path()))
        .await;
    assert_denied(&r, "read");
    assert!(!r.content.contains("TOPSECRET"));
}

#[tokio::test]
async fn write_file_enforces_configured_write_deny() {
    let dir = tempdir().unwrap();
    seed_write(dir.path());
    let c = ctx(dir.path());
    for path in ["frozen/a.txt", "frozen/new.txt", "x/../frozen/a.txt"] {
        let r = WriteFileTool
            .execute(json!({"path": path, "content": "PWNED"}), &c)
            .await;
        assert_denied(&r, "write");
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join("frozen/a.txt")).unwrap(),
        "alpha\n"
    );
    assert!(!dir.path().join("frozen/new.txt").exists());
}

#[tokio::test]
async fn edit_file_enforces_write_and_read_deny() {
    let dir = tempdir().unwrap();
    seed_write(dir.path());
    seed_read(dir.path());
    let c = ctx(dir.path());
    let w = EditFileTool
        .execute(
            json!({"path": "frozen/a.txt", "old_string": "alpha", "new_string": "PWNED"}),
            &c,
        )
        .await;
    assert_denied(&w, "write");
    let r = EditFileTool
        .execute(
            json!({"path": "keys.vault", "old_string": "TOPSECRET", "new_string": "x"}),
            &c,
        )
        .await;
    assert_denied(&r, "read");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("frozen/a.txt")).unwrap(),
        "alpha\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("keys.vault")).unwrap(),
        "TOPSECRET\n"
    );
}

#[tokio::test]
async fn multi_edit_enforces_write_and_read_deny() {
    let dir = tempdir().unwrap();
    seed_write(dir.path());
    seed_read(dir.path());
    let c = ctx(dir.path());
    let edits = json!([{"old_string": "alpha", "new_string": "PWNED"}]);
    let w = MultiEditTool
        .execute(json!({"path": "frozen/a.txt", "edits": edits}), &c)
        .await;
    assert_denied(&w, "write");
    let edits = json!([{"old_string": "TOPSECRET", "new_string": "x"}]);
    let r = MultiEditTool
        .execute(json!({"path": "keys.vault", "edits": edits}), &c)
        .await;
    assert_denied(&r, "read");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("frozen/a.txt")).unwrap(),
        "alpha\n"
    );
}

#[tokio::test]
async fn undo_edit_enforces_write_deny() {
    let dir = tempdir().unwrap();
    seed_write(dir.path());
    let r = UndoEditTool
        .execute(json!({"path": "frozen/a.txt"}), &ctx(dir.path()))
        .await;
    assert_denied(&r, "write");
}

#[tokio::test]
async fn apply_patch_enforces_deny_before_any_hunk_is_written() {
    let dir = tempdir().unwrap();
    seed_write(dir.path());
    std::fs::write(dir.path().join("plain.txt"), "one\n").unwrap();
    let c = ctx(dir.path());
    // A permitted file first, a denied one second: nothing may be applied.
    let patch = "*** plain.txt\n--- one\n+++ ONE\n*** frozen/a.txt\n--- alpha\n+++ PWNED";
    let r = ApplyPatchTool.execute(json!({"patch": patch}), &c).await;
    assert_denied(&r, "write");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("plain.txt")).unwrap(),
        "one\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("frozen/a.txt")).unwrap(),
        "alpha\n"
    );
}

#[tokio::test]
async fn list_files_hides_read_denied_paths() {
    let dir = tempdir().unwrap();
    seed_read(dir.path());
    let c = ctx(dir.path());
    let r = ListFilesTool.execute(json!({}), &c).await;
    assert!(!r.is_error, "{}", r.content);
    assert!(r.content.contains("ok.txt"));
    assert!(!r.content.contains("keys.vault"), "{}", r.content);
    // Listing a denied path directly is refused.
    std::fs::create_dir_all(dir.path().join("dir.vault")).unwrap();
    std::fs::write(dir.path().join("dir.vault/inner.txt"), "x").unwrap();
    let direct = ListFilesTool
        .execute(json!({"path": "dir.vault"}), &c)
        .await;
    assert_denied(&direct, "read");
}

#[tokio::test]
async fn search_never_opens_or_reports_read_denied_files() {
    let dir = tempdir().unwrap();
    seed_read(dir.path());
    std::fs::write(dir.path().join("keys.vault"), "needle TOPSECRET\n").unwrap();
    let c = ctx(dir.path());
    let r = SearchTool.execute(json!({"pattern": "needle"}), &c).await;
    assert!(!r.is_error, "{}", r.content);
    assert!(r.content.contains("ok.txt"), "{}", r.content);
    assert!(!r.content.contains("TOPSECRET"), "{}", r.content);
    assert!(!r.content.contains("keys.vault"), "{}", r.content);
    let direct = SearchTool
        .execute(json!({"pattern": "needle", "path": "keys.vault"}), &c)
        .await;
    assert_denied(&direct, "read");
}

struct RealToolDispatcher;

impl NestedToolDispatcher for RealToolDispatcher {
    fn invoke<'a>(
        &'a self,
        tool: &'a str,
        args: Value,
        c: &'a ToolContext,
    ) -> NestedToolFuture<'a> {
        Box::pin(async move {
            match tool {
                "read_file" => ReadFileTool.execute(args, c).await,
                other => ToolResult::error(format!("unexpected tool {other}")),
            }
        })
    }
}

/// Code mode fails closed without a Pull dispatcher; this stub returns the
/// program output unchanged so the deny-glob behaviour is what is tested.
struct Passthrough;

impl ContextToolDispatcher for Passthrough {
    fn compress<'a>(
        &'a self,
        output: String,
        _c: &'a ToolContext,
        _d: &'a dyn NestedToolDispatcher,
        _p: &'a str,
    ) -> NestedToolFuture<'a> {
        Box::pin(async move { ToolResult::ok(output) })
    }
}

#[tokio::test]
async fn code_mode_bridge_nested_read_is_denied() {
    let dir = tempdir().unwrap();
    seed_read(dir.path());
    let c = ctx(dir.path());
    let specs = vec![ProgramToolSpec {
        name: "read_file".into(),
        description: "d".into(),
        schema: json!({"type": "object"}),
    }];
    let denied = execute_program(
        r#"return await tools.read_file({path:"keys.vault"});"#,
        &specs,
        &c,
        &RealToolDispatcher,
        Some(&Passthrough),
        "parent",
    )
    .await;
    assert!(!denied.content.contains("TOPSECRET"), "{}", denied.content);
    assert!(
        denied.content.contains("deny-glob") || denied.is_error,
        "{}",
        denied.content
    );
    let ok = execute_program(
        r#"return await tools.read_file({path:"ok.txt"});"#,
        &specs,
        &c,
        &RealToolDispatcher,
        Some(&Passthrough),
        "parent",
    )
    .await;
    assert!(ok.content.contains("needle ok"), "{}", ok.content);
}
