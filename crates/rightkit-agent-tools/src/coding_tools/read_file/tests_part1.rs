use super::*;
use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
use tempfile::tempdir;

fn ctx(root: &std::path::Path) -> ToolContext {
    ToolContext::new(
        "sess1",
        root,
        PermissionMode::Bypass,
        WorkspaceTrust::Trusted,
    )
}

#[test]
fn hashline_anchor_stale_error_includes_fresh_anchors() {
    let stale = format_hashline(2, "beta");
    let err = resolve_hashline_line(&stale, "alpha\nchanged\ngamma\nbeta\n")
        .expect_err("stale anchor must error");

    assert!(err.contains("Stale hashline anchor"), "{err}");
    assert!(err.contains("Fresh anchors"), "{err}");
    assert!(err.contains("changed"), "{err}");
    assert!(
        err.contains("Nearest-content suggestion: lines 4-4 (similarity "),
        "{err}"
    );
}

/// S:A1a: a stale hashline anchor whose content bears no resemblance to
/// anything left in the file must NOT carry a misleading nearest-content
/// hint — the diagnostic is only appended when a genuine near-miss (>= 0.6
/// similarity) exists.
#[test]
fn hashline_anchor_stale_error_omits_hint_when_nothing_similar() {
    let stale = format_hashline(2, "completely unrelated anchor text");
    let err =
        resolve_hashline_line(&stale, "alpha\nbeta\ngamma\n").expect_err("stale anchor must error");

    assert!(err.contains("Stale hashline anchor"), "{err}");
    assert!(
        !err.contains("Nearest-content suggestion"),
        "unrelated content must not produce a misleading hint: {err}"
    );
}

#[test]
fn resolve_in_workspace_rejects_empty_root() {
    // An empty workspace root must fail closed — otherwise a relative path
    // resolves against the daemon's cwd and the escape check is bypassed.
    let err = resolve_in_workspace("sample.rs", Path::new(""))
        .expect_err("empty workspace root must be rejected");
    assert!(err.contains("no workspace root"), "{err}");

    // A real root still resolves a relative path inside it.
    let dir = tempdir().unwrap();
    let ok = resolve_in_workspace("sample.rs", dir.path()).expect("real root resolves");
    assert!(ok.starts_with(dir.path()));
}

#[tokio::test]
async fn read_file_returns_content() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("hello.txt");
    std::fs::write(&file, "Hello, World!\nLine 2\n").unwrap();

    let tool = ReadFileTool;
    let result = tool
        .execute(json!({"path": file.to_str().unwrap()}), &ctx(dir.path()))
        .await;

    assert!(!result.is_error);
    assert!(result.content.contains("Hello, World!"));
    assert!(result.content.contains("Line 2"));
}

#[tokio::test]
async fn read_file_missing_returns_error() {
    let dir = tempdir().unwrap();
    let tool = ReadFileTool;
    let result = tool
        .execute(
            json!({"path": dir.path().join("no_such_file.txt").to_str().unwrap()}),
            &ctx(dir.path()),
        )
        .await;

    assert!(result.is_error);
    assert!(result.content.contains("Failed to read file"));
}

#[tokio::test]
async fn read_file_rejects_workspace_escape() {
    let dir = tempdir().unwrap();
    let tool = ReadFileTool;
    let result = tool
        .execute(json!({"path": "../../../../etc/passwd"}), &ctx(dir.path()))
        .await;

    assert!(result.is_error);
    assert!(result.content.contains("escapes the workspace root"));
}

#[tokio::test]
async fn read_file_rejects_absolute_outside_workspace() {
    let dir = tempdir().unwrap();
    let tool = ReadFileTool;
    let result = tool
        .execute(json!({"path": "/etc/hostname"}), &ctx(dir.path()))
        .await;

    assert!(result.is_error);
    assert!(result.content.contains("escapes the workspace root"));
}

#[tokio::test]
async fn read_file_allows_workspace_inside_default_denied_parent_dir() {
    let dir = tempdir().unwrap();
    let denied_parent = dir.path().join("AppData").join("Local");
    std::fs::create_dir_all(&denied_parent).unwrap();
    let workspace = denied_parent.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let file = workspace.join("safe.txt");
    std::fs::write(&file, "workspace local data\n").unwrap();

    let tool = ReadFileTool;
    let result = tool
        .execute(json!({"path": file.to_str().unwrap()}), &ctx(&workspace))
        .await;

    assert!(!result.is_error, "{}", result.content);
    assert!(result.content.contains("workspace local data"));
}

#[tokio::test]
async fn read_file_on_directory_points_to_list_files() {
    let dir = tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let r = ReadFileTool
        .execute(json!({"path": sub.to_str().unwrap()}), &ctx(dir.path()))
        .await;
    assert!(r.is_error);
    assert!(r.content.contains("list_files"));
}

#[tokio::test]
async fn read_file_caps_huge_file() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("big.txt");
    let body: String = (1..=DEFAULT_LINE_CAP + 500)
        .map(|i| format!("line{i}\n"))
        .collect();
    std::fs::write(&file, body).unwrap();

    let tool = ReadFileTool;
    let result = tool
        .execute(json!({"path": file.to_str().unwrap()}), &ctx(dir.path()))
        .await;

    assert!(!result.is_error);
    assert!(result.content.contains("line1"));
    assert!(result.content.contains(&format!("line{DEFAULT_LINE_CAP}")));
    assert!(!result
        .content
        .contains(&format!("line{}", DEFAULT_LINE_CAP + 1)));
    assert!(result.content.contains("showing first"));
}

#[tokio::test]
async fn read_file_with_offset_and_limit() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("lines.txt");
    std::fs::write(&file, "line1\nline2\nline3\nline4\nline5\n").unwrap();

    let tool = ReadFileTool;
    let result = tool
        .execute(
            json!({
                "path": file.to_str().unwrap(),
                "offset": 2,
                "limit": 2
            }),
            &ctx(dir.path()),
        )
        .await;

    assert!(!result.is_error);
    assert!(result.content.contains("line2"));
    assert!(result.content.contains("line3"));
    assert!(!result.content.contains("line4"));
    assert!(!result.content.contains("line1"));
}

#[tokio::test]
async fn read_file_with_offset_without_limit_still_caps_output() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("large-offset.txt");
    let body: String = (1..=DEFAULT_LINE_CAP + 25)
        .map(|i| format!("line{i}\n"))
        .collect();
    std::fs::write(&file, body).unwrap();

    let result = ReadFileTool
        .execute(
            json!({
                "path": file.to_str().unwrap(),
                "offset": 2
            }),
            &ctx(dir.path()),
        )
        .await;

    assert!(!result.is_error);
    assert!(result.content.contains("line2"));
    assert!(result
        .content
        .contains(&format!("line{}", DEFAULT_LINE_CAP + 1)));
    assert!(!result
        .content
        .contains(&format!("line{}", DEFAULT_LINE_CAP + 2)));
    assert!(result.content.contains("Use offset/limit to read more"));
}

#[tokio::test]
async fn read_file_returns_image_metadata_and_data_url() {
    let _guard = unconfined_rich_worker();
    let dir = tempdir().unwrap();
    let file = dir.path().join("pixel.png");
    let png_1x1: &[u8] = &[
        0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, b'I', b'H', b'D',
        b'R', 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89,
    ];
    std::fs::write(&file, png_1x1).unwrap();

    let result = ReadFileTool
        .execute(json!({"path": file.to_str().unwrap()}), &ctx(dir.path()))
        .await;

    assert!(!result.is_error);
    assert!(result.content.contains("Image file:"));
    assert!(result.content.contains("mime: image/png"));
    assert!(result.content.contains("dimensions: 1x1"));
    assert!(result.content.contains("data:image/png;base64,"));
}

#[tokio::test]
async fn read_file_rejects_image_extension_signature_mismatch() {
    let _guard = unconfined_rich_worker();
    let dir = tempdir().unwrap();
    let file = dir.path().join("disguised.jpg");
    let png_header: &[u8] = &[
        0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, b'I', b'H', b'D', b'R', 0,
        0, 0, 1, 0, 0, 0, 1,
    ];
    std::fs::write(&file, png_header).unwrap();

    let result = ReadFileTool
        .execute(json!({"path": file.to_str().unwrap()}), &ctx(dir.path()))
        .await;

    assert!(result.is_error);
    assert!(result.content.contains("image/png content"));
}

#[tokio::test]
async fn read_file_rejects_oversized_image_dimensions_before_encoding() {
    let _guard = unconfined_rich_worker();
    let dir = tempdir().unwrap();
    let file = dir.path().join("huge.png");
    let mut png_header = vec![
        0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, b'I', b'H', b'D', b'R', 0,
        0, 0, 1, 0, 0, 0, 1,
    ];
    png_header[16..20].copy_from_slice(&10_000u32.to_be_bytes());
    png_header[20..24].copy_from_slice(&10_000u32.to_be_bytes());
    std::fs::write(&file, png_header).unwrap();

    let result = ReadFileTool
        .execute(json!({"path": file.to_str().unwrap()}), &ctx(dir.path()))
        .await;

    assert!(result.is_error);
    assert!(result.content.contains("maximum supported size"));
}

#[tokio::test]
async fn read_file_summarizes_notebook_cells() {
    let _guard = unconfined_rich_worker();
    let dir = tempdir().unwrap();
    let file = dir.path().join("analysis.ipynb");
    std::fs::write(
        &file,
        json!({
            "nbformat": 4,
            "nbformat_minor": 5,
            "cells": [
                {"cell_type": "markdown", "source": ["# Title\n", "notes"]},
                {"cell_type": "code", "source": ["x = 1\n", "print(x)"], "outputs": []}
            ]
        })
        .to_string(),
    )
    .unwrap();

    let result = ReadFileTool
        .execute(json!({"path": file.to_str().unwrap()}), &ctx(dir.path()))
        .await;

    assert!(!result.is_error);
    assert!(result.content.contains("Notebook:"));
    assert!(result.content.contains("nbformat: 4.5"));
    assert!(result.content.contains("cell 0 [markdown]"));
    assert!(result.content.contains("# Title"));
    assert!(result.content.contains("cell 1 [code]"));
    assert!(result.content.contains("x = 1"));
}

#[tokio::test]
async fn read_file_rejects_oversized_notebook_before_json_parse() {
    use std::io::Write;

    let _guard = unconfined_rich_worker();
    let dir = tempdir().unwrap();
    let file = dir.path().join("huge.ipynb");
    let mut handle = std::fs::File::create(&file).unwrap();
    handle.write_all(b"{").unwrap();
    handle.set_len((MAX_NOTEBOOK_BYTES + 1) as u64).unwrap();

    let result = ReadFileTool
        .execute(json!({"path": file.to_str().unwrap()}), &ctx(dir.path()))
        .await;

    assert!(result.is_error);
    assert!(result.content.contains("maximum supported size"));
}
