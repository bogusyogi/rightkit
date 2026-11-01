//! Native transcript discovery plus JSONL source loading.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::{Result, TranscriptError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredTranscript {
    pub host: String,
    pub path: PathBuf,
}

#[derive(Debug)]
pub(crate) struct SourceRow {
    pub row_index: u64,
    pub byte_start: u64,
    pub byte_end: u64,
    pub value: Value,
}

#[derive(Debug)]
pub(crate) struct LoadedSource {
    pub source_len: u64,
    pub source_digest: String,
    pub rows: Vec<SourceRow>,
}

fn loaded(bytes: Vec<u8>, rows: Vec<SourceRow>) -> LoadedSource {
    LoadedSource {
        source_len: bytes.len() as u64,
        source_digest: rightkit_fs::digest::sha256_bytes(&bytes),
        rows,
    }
}

fn inaccessible(path: &Path, error: impl ToString) -> TranscriptError {
    TranscriptError::Inaccessible {
        path: path.to_path_buf(),
        detail: error.to_string(),
    }
}

fn malformed(
    path: &Path,
    row_index: u64,
    start: u64,
    end: u64,
    detail: impl ToString,
) -> TranscriptError {
    TranscriptError::MalformedRow {
        path: path.to_path_buf(),
        row_index,
        byte_start: start,
        byte_end: end,
        detail: detail.to_string(),
    }
}

fn load_jsonl(path: &Path, bytes: Vec<u8>) -> Result<LoadedSource> {
    let mut rows = Vec::new();
    let mut start = 0usize;
    for (index, chunk) in bytes.split_inclusive(|byte| *byte == b'\n').enumerate() {
        if chunk.is_empty() {
            continue;
        }
        let end = start + chunk.len();
        let text = std::str::from_utf8(chunk)
            .map_err(|error| malformed(path, index as u64 + 1, start as u64, end as u64, error))?;
        let value: Value = serde_json::from_str(text)
            .map_err(|error| malformed(path, index as u64 + 1, start as u64, end as u64, error))?;
        if !value.is_object() {
            return Err(malformed(
                path,
                index as u64 + 1,
                start as u64,
                end as u64,
                "expected a JSON object",
            ));
        }
        rows.push(SourceRow {
            row_index: index as u64 + 1,
            byte_start: start as u64,
            byte_end: end as u64,
            value,
        });
        start = end;
    }
    if rows.is_empty() {
        return Err(TranscriptError::NoCompleteRow {
            path: path.to_path_buf(),
        });
    }
    Ok(loaded(bytes, rows))
}

pub(crate) fn load(path: &Path, _host: &str) -> Result<LoadedSource> {
    load_jsonl(
        path,
        fs::read(path).map_err(|error| inaccessible(path, error))?,
    )
}

fn collect_files(root: &Path, accept: &dyn Fn(&Path) -> bool, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, accept, out);
        } else if accept(&path) {
            out.push(path);
        }
    }
}

fn add_tree(
    out: &mut Vec<DiscoveredTranscript>,
    host: &str,
    root: PathBuf,
    accept: impl Fn(&Path) -> bool,
) {
    let mut paths = Vec::new();
    collect_files(&root, &accept, &mut paths);
    out.extend(paths.into_iter().map(|path| DiscoveredTranscript {
        host: host.into(),
        path,
    }));
}

/// Discover native Claude Code and Codex transcript stores under `home`.
pub fn discover_open(home: &Path) -> Vec<DiscoveredTranscript> {
    let mut out = Vec::new();
    add_tree(
        &mut out,
        "claude_code",
        home.join(".claude/projects"),
        |p| p.extension().and_then(|x| x.to_str()) == Some("jsonl"),
    );
    add_tree(&mut out, "codex", home.join(".codex/sessions"), |p| {
        p.extension().and_then(|x| x.to_str()) == Some("jsonl")
    });
    out.sort_by(|a, b| a.host.cmp(&b.host).then(a.path.cmp(&b.path)));
    out.dedup();
    out
}
