use super::session_tool::{MAX_ARTIFACT_SCAN_FILES, MAX_REPL_ARTIFACTS};
use std::collections::HashMap;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FileStamp {
    len: u64,
    modified_nanos: Option<u128>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ReplArtifact {
    path: String,
    len: u64,
    status: &'static str,
    kind: Option<String>,
}

pub(super) fn snapshot_workspace_artifacts(root: &Path) -> HashMap<String, FileStamp> {
    let mut out = HashMap::new();
    collect_workspace_files(root, root, &mut out);
    out
}

fn collect_workspace_files(root: &Path, dir: &Path, out: &mut HashMap<String, FileStamp>) {
    if out.len() >= MAX_ARTIFACT_SCAN_FILES || should_skip_artifact_dir(dir) {
        return;
    }

    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        if out.len() >= MAX_ARTIFACT_SCAN_FILES {
            return;
        }
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_dir() {
            collect_workspace_files(root, &path, out);
        } else if metadata.is_file() {
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            out.insert(
                rel.to_string_lossy().replace('\\', "/"),
                FileStamp {
                    len: metadata.len(),
                    modified_nanos: metadata
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_nanos()),
                },
            );
        }
    }
}

fn should_skip_artifact_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| {
            matches!(
                name,
                ".git" | "node_modules" | "target" | ".venv" | "__pycache__"
            )
        })
        .unwrap_or(false)
}

pub(super) fn changed_artifacts(
    root: &Path,
    before: &HashMap<String, FileStamp>,
    after: &HashMap<String, FileStamp>,
) -> Vec<ReplArtifact> {
    let mut artifacts = after
        .iter()
        .filter_map(|(path, stamp)| match before.get(path) {
            None => Some(ReplArtifact {
                path: path.clone(),
                len: stamp.len,
                status: "new",
                kind: artifact_kind(root, path),
            }),
            Some(old) if old != stamp => Some(ReplArtifact {
                path: path.clone(),
                len: stamp.len,
                status: "modified",
                kind: artifact_kind(root, path),
            }),
            _ => None,
        })
        .collect::<Vec<_>>();
    artifacts.sort_by(|a, b| a.path.cmp(&b.path));
    artifacts.truncate(MAX_REPL_ARTIFACTS);
    artifacts
}

pub(super) fn format_repl_artifacts(artifacts: &[ReplArtifact]) -> String {
    let rows = artifacts
        .iter()
        .map(|artifact| {
            let kind = artifact
                .kind
                .as_deref()
                .map(|kind| format!("; {kind}"))
                .unwrap_or_default();
            format!(
                "- {} ({}; {} bytes{})",
                artifact.path, artifact.status, artifact.len, kind
            )
        })
        .collect::<Vec<_>>();
    format!("artifacts:\n{}", rows.join("\n"))
}

fn artifact_kind(root: &Path, rel_path: &str) -> Option<String> {
    let path = root.join(rel_path);
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" => {
            let mime = match ext.as_str() {
                "jpg" | "jpeg" => "image/jpeg",
                "gif" => "image/gif",
                "webp" => "image/webp",
                _ => "image/png",
            };
            let bytes = std::fs::read(&path).ok()?;
            let dimensions = image_dimensions(&ext, &bytes)
                .map(|(w, h)| format!(" {w}x{h}"))
                .unwrap_or_default();
            Some(format!("{mime}{dimensions}"))
        }
        "pdf" => Some("application/pdf".to_string()),
        "ipynb" => Some("notebook".to_string()),
        "csv" => Some("text/csv".to_string()),
        "json" => Some("application/json".to_string()),
        "html" | "htm" => Some("text/html".to_string()),
        "svg" => Some("image/svg+xml".to_string()),
        _ => None,
    }
}

fn image_dimensions(ext: &str, bytes: &[u8]) -> Option<(u32, u32)> {
    match ext {
        "png" if bytes.len() >= 24 && bytes.starts_with(b"\x89PNG\r\n\x1a\n") => Some((
            u32::from_be_bytes(bytes[16..20].try_into().ok()?),
            u32::from_be_bytes(bytes[20..24].try_into().ok()?),
        )),
        "gif"
            if bytes.len() >= 10
                && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) =>
        {
            Some((
                u16::from_le_bytes(bytes[6..8].try_into().ok()?) as u32,
                u16::from_le_bytes(bytes[8..10].try_into().ok()?) as u32,
            ))
        }
        "jpg" | "jpeg" => jpeg_dimensions(bytes),
        _ => None,
    }
}

fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 4 || bytes[0] != 0xff || bytes[1] != 0xd8 {
        return None;
    }

    let mut i = 2usize;
    while i + 9 < bytes.len() {
        if bytes[i] != 0xff {
            i += 1;
            continue;
        }
        let marker = bytes[i + 1];
        i += 2;
        if marker == 0xd9 || marker == 0xda {
            break;
        }
        if i + 2 > bytes.len() {
            break;
        }
        let len = u16::from_be_bytes([bytes[i], bytes[i + 1]]) as usize;
        if len < 2 || i + len > bytes.len() {
            break;
        }
        if matches!(
            marker,
            0xc0 | 0xc1
                | 0xc2
                | 0xc3
                | 0xc5
                | 0xc6
                | 0xc7
                | 0xc9
                | 0xca
                | 0xcb
                | 0xcd
                | 0xce
                | 0xcf
        ) && len >= 7
        {
            let h = u16::from_be_bytes([bytes[i + 3], bytes[i + 4]]) as u32;
            let w = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
            return Some((w, h));
        }
        i += len;
    }
    None
}
