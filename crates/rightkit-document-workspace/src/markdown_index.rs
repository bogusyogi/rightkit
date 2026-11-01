//! Markdown folder index: the Markdown files beside a document, with titles, for a link picker
//! or completion provider. Hard caps: 1 000 files, 20 MiB total, 2 MiB per file, depth 8.

use crate::WorkspaceError;
use serde::Serialize;
use std::path::Path;
use walkdir::WalkDir;

pub const MAX_FILES: usize = 1_000;
pub const MAX_TOTAL_BYTES: u64 = 20 * 1024 * 1024;
pub const MAX_PER_FILE_BYTES: u64 = 2 * 1024 * 1024;
pub const MAX_DEPTH: usize = 8;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedMarkdownFile {
    pub absolute_path: String,
    pub relative_path: String,
    pub byte_length: u64,
    pub title: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedMarkdownFolderResult {
    pub files: Vec<IndexedMarkdownFile>,
    pub truncated: bool,
}

/// Walk the document's parent directory and return metadata for every
/// Markdown file under it (subject to the documented caps).
///
/// The directory walk (walkdir + per-file `metadata` + `read_to_string` title extraction) runs on a
/// Index the Markdown files beside `document` (an already authorized, existing file): its parent
/// folder, recursively to depth 8 when `recursive`, skipping hidden and build-output folders.
/// Refused over 1 000 files, 20 MiB in total or 2 MiB for one file. Folder enumeration must be
/// an explicit user choice; the caller checks consent and path grants before calling.
pub fn index_markdown_folder(
    document: &Path,
    recursive: bool,
) -> Result<IndexedMarkdownFolderResult, WorkspaceError> {
    let doc_path = document.to_path_buf();
    let parent = doc_path
        .parent()
        .ok_or_else(|| WorkspaceError::Other("no-document-parent".to_string()))?
        .to_path_buf();
    let parent = std::fs::canonicalize(parent)
        .map_err(|error| WorkspaceError::Io(format!("canonicalize markdown folder: {error}")))?;

    let mut files = Vec::new();
    let mut total: u64 = 0;
    let walker = WalkDir::new(&parent)
        .max_depth(if recursive { MAX_DEPTH } else { 1 })
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !is_skipped_dir(e.path()));

    for entry in walker {
        let entry =
            entry.map_err(|error| WorkspaceError::Io(format!("walk markdown folder: {error}")))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if !is_markdown_file(path) {
            continue;
        }
        let metadata = std::fs::metadata(path)
            .map_err(|error| WorkspaceError::Io(format!("inspect markdown file: {error}")))?;
        let len = metadata.len();
        if len > MAX_PER_FILE_BYTES {
            return Err(WorkspaceError::DocumentTooLarge(format!(
                "markdown index file exceeds {MAX_PER_FILE_BYTES} bytes"
            )));
        }
        if total + len > MAX_TOTAL_BYTES {
            return Err(WorkspaceError::DocumentTooLarge(format!(
                "markdown index exceeds {MAX_TOTAL_BYTES} total bytes"
            )));
        }
        if files.len() >= MAX_FILES {
            return Err(WorkspaceError::DocumentTooLarge(format!(
                "markdown index exceeds {MAX_FILES} files"
            )));
        }
        total += len;
        files.push(IndexedMarkdownFile {
            absolute_path: path.to_string_lossy().to_string(),
            relative_path: relative_to(&parent, path),
            byte_length: len,
            title: extract_title(path),
        });
    }

    Ok(IndexedMarkdownFolderResult {
        files,
        truncated: false,
    })
}

fn is_skipped_dir(path: &Path) -> bool {
    if !path.is_dir() {
        return false;
    }
    let name = match path.file_name().and_then(|n| n.to_str()) {
        Some(n) => n,
        None => return false,
    };
    name.starts_with('.')
        || matches!(
            name,
            "node_modules"
                | "dist"
                | "build"
                | "target"
                | ".next"
                | ".cache"
                | ".turbo"
                | ".parcel-cache"
                | ".DS_Store"
        )
}

fn is_markdown_file(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    matches!(ext.as_deref(), Some("md") | Some("markdown") | Some("mdx"))
}

fn relative_to(base: &Path, target: &Path) -> String {
    target
        .strip_prefix(base)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| target.to_string_lossy().replace('\\', "/"))
}

/// Extract the first H1 (or H2 fallback) from the file as a
/// best-effort title. Returns `None` when the file cannot be read.
fn extract_title(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let trimmed = line.trim_start_matches('\u{feff}');
        if let Some(rest) = trimmed.strip_prefix("# ") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_markdown_beside_the_document_and_skips_build_folders() {
        let dir =
            std::env::temp_dir().join(format!("rightkit-docws-mdindex-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("notes")).unwrap();
        std::fs::create_dir_all(dir.join("node_modules")).unwrap();
        std::fs::write(dir.join("a.md"), "# Alpha\nbody").unwrap();
        std::fs::write(dir.join("notes").join("b.markdown"), "text").unwrap();
        std::fs::write(dir.join("node_modules").join("c.md"), "# skip").unwrap();
        std::fs::write(dir.join("d.txt"), "not markdown").unwrap();
        let flat = index_markdown_folder(&dir.join("a.md"), false).unwrap();
        assert_eq!(flat.files.len(), 1);
        assert_eq!(flat.files[0].title.as_deref(), Some("Alpha"));
        let deep = index_markdown_folder(&dir.join("a.md"), true).unwrap();
        let mut rel: Vec<_> = deep.files.iter().map(|f| f.relative_path.clone()).collect();
        rel.sort();
        assert_eq!(rel, ["a.md", "notes/b.markdown"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
