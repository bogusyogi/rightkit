//! Privacy scan of a JSONL log before it is shared (issue #1278).
//!
//! Flags absolute paths, user names, IPv4 addresses and e-mail addresses in the string values of
//! every record. A finding names the file, line, JSON key path and kind, never the matched text,
//! so the report is safe to show. It uses the same detectors as [`crate::scrub_text`].

use crate::scrub::{scrub_text_report, ScrubOptions};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io;
use std::path::{Path, PathBuf};

/// What a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyKind {
    AbsolutePath,
    /// A home directory, or the user-name segment of a path under `/Users`, `/home` or `C:\Users`.
    UserName,
    Ipv4,
    Email,
}

/// One place a log holds something private: `count` values of `kind` at `key_path` of `line`
/// (1-based) in `file`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivacyFinding {
    pub file: PathBuf,
    pub line: usize,
    /// Dotted key path of the string value (`ctx.path`, `items[2].host`).
    pub key_path: String,
    pub kind: PrivacyKind,
    pub count: u64,
}

/// Result of [`scan_privacy`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivacyReport {
    pub files_scanned: usize,
    pub lines_scanned: u64,
    pub findings: Vec<PrivacyFinding>,
}

impl PrivacyReport {
    /// True when nothing private was found.
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }

    /// Total values found of `kind`.
    pub fn count(&self, kind: PrivacyKind) -> u64 {
        self.findings
            .iter()
            .filter(|finding| finding.kind == kind)
            .map(|finding| finding.count)
            .sum()
    }
}

fn kinds(text: &str) -> [(PrivacyKind, u64); 4] {
    let (_, base) = scrub_text_report(text, &ScrubOptions::default());
    let (_, paths) = scrub_text_report(
        text,
        &ScrubOptions {
            redact_paths: true,
            ..ScrubOptions::default()
        },
    );
    [
        (PrivacyKind::AbsolutePath, paths.path),
        (PrivacyKind::UserName, base.home_dir + base.user_segment),
        (PrivacyKind::Ipv4, base.ip),
        (PrivacyKind::Email, base.email),
    ]
}

fn walk(value: &Value, key_path: &str, file: &Path, line: usize, out: &mut Vec<PrivacyFinding>) {
    match value {
        Value::String(text) => {
            for (kind, count) in kinds(text) {
                if count > 0 {
                    out.push(PrivacyFinding {
                        file: file.to_path_buf(),
                        line,
                        key_path: key_path.to_string(),
                        kind,
                        count,
                    });
                }
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                walk(item, &format!("{key_path}[{index}]"), file, line, out);
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                let child = if key_path.is_empty() {
                    key.clone()
                } else {
                    format!("{key_path}.{key}")
                };
                walk(item, &child, file, line, out);
            }
        }
        _ => {}
    }
}

/// Scan the JSONL files at `paths` (a missing file is skipped; a line that is not JSON is scanned
/// as one string value under the key path `<line>`).
pub fn scan_privacy(paths: &[PathBuf]) -> io::Result<PrivacyReport> {
    let mut report = PrivacyReport::default();
    for path in paths {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        report.files_scanned += 1;
        for (index, line) in text
            .lines()
            .enumerate()
            .filter(|(_, line)| !line.trim().is_empty())
        {
            report.lines_scanned += 1;
            match serde_json::from_str::<Value>(line) {
                Ok(value) => walk(&value, "", path, index + 1, &mut report.findings),
                Err(_) => walk(
                    &Value::String(line.to_string()),
                    "<line>",
                    path,
                    index + 1,
                    &mut report.findings,
                ),
            }
        }
    }
    Ok(report)
}

/// Scan a store's current file and its `generations` rotated predecessors.
pub fn scan_store_privacy(
    store: &crate::JsonlStore,
    generations: u32,
) -> io::Result<PrivacyReport> {
    let mut paths = vec![store.path().to_path_buf()];
    for n in 1..=generations {
        let mut name = store
            .path()
            .file_name()
            .map(|name| name.to_os_string())
            .unwrap_or_default();
        name.push(format!(".{n}"));
        paths.push(store.path().with_file_name(name));
    }
    scan_privacy(&paths)
}
