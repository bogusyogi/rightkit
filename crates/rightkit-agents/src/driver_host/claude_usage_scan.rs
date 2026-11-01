//! Per-account usage scan of Claude transcripts (issue #342).
//!
//! Each managed Claude account has its own config home (`CLAUDE_CONFIG_DIR`). The CLI writes one
//! JSONL transcript per session under `<home>/projects/<project>/`, and every assistant line
//! carries the API `usage` block. Summing those lines gives usage per account without asking the
//! vendor anything.
//!
//! Two rules keep the totals honest:
//!
//! - **Real-path dedup.** A transcript reached twice (a symlinked project alias, or two accounts
//!   whose homes are the same directory) is read once. Files are keyed by their canonical path,
//!   and an account whose home canonicalizes to an earlier account's home is reported as an alias
//!   of it with zero totals, so there is one bar per account.
//! - **Transcripts only.** The scan walks `<home>/projects` and opens `.jsonl` files there, and
//!   nothing else: never a credentials file, settings, or any file outside `projects`. It reads no
//!   credential bytes (the same rule as the rest of this crate).
//!
//! A message that repeats across resumed or forked sessions (same `message.id` and `requestId`)
//! is counted once per account.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// One account to scan: its stable id and its config home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeAccountHome {
    pub account_id: String,
    pub home: PathBuf,
}

/// Token totals for one account.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaudeAccountUsage {
    pub account_id: String,
    /// Set when this account's home is the same directory as an earlier account's: the earlier
    /// account carries the totals and this row is zero.
    pub alias_of: Option<String>,
    pub transcript_files: u64,
    pub messages: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    /// Lines that were not valid JSON (a transcript being written, a truncated tail).
    pub skipped_lines: u64,
}

/// Scan every account's transcripts. The result has one row per input account, in input order.
pub fn scan_claude_account_usage(accounts: &[ClaudeAccountHome]) -> Vec<ClaudeAccountUsage> {
    let mut seen_homes: Vec<(PathBuf, String)> = Vec::new();
    let mut seen_files: BTreeSet<PathBuf> = BTreeSet::new();
    let mut rows = Vec::with_capacity(accounts.len());
    for account in accounts {
        let mut row = ClaudeAccountUsage {
            account_id: account.account_id.clone(),
            ..ClaudeAccountUsage::default()
        };
        let real_home = std::fs::canonicalize(&account.home).ok();
        if let Some(real_home) = &real_home {
            if let Some((_, first)) = seen_homes.iter().find(|(home, _)| home == real_home) {
                row.alias_of = Some(first.clone());
                rows.push(row);
                continue;
            }
            seen_homes.push((real_home.clone(), account.account_id.clone()));
        }
        let projects = account.home.join("projects");
        let mut message_keys: BTreeSet<(String, String)> = BTreeSet::new();
        for file in transcript_files(&projects) {
            let Ok(real) = std::fs::canonicalize(&file) else {
                continue;
            };
            if !seen_files.insert(real.clone()) {
                continue;
            }
            row.transcript_files += 1;
            let Ok(text) = std::fs::read_to_string(&real) else {
                continue;
            };
            for line in text.lines().filter(|line| !line.trim().is_empty()) {
                match serde_json::from_str::<Value>(line) {
                    Ok(value) => add_line(&mut row, &mut message_keys, &value),
                    Err(_) => row.skipped_lines += 1,
                }
            }
        }
        rows.push(row);
    }
    rows
}

/// Every regular `.jsonl` file under `projects`, following nothing outside it by name (a symlink
/// is resolved when read, and deduplicated by its real path).
fn transcript_files(projects: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![projects.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            // `metadata` follows symlinks, so an alias directory is walked and an alias file is
            // read; their real paths dedupe them.
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() && path.extension().is_some_and(|ext| ext == "jsonl") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

fn add_line(
    row: &mut ClaudeAccountUsage,
    message_keys: &mut BTreeSet<(String, String)>,
    value: &Value,
) {
    if value.get("type").and_then(Value::as_str) != Some("assistant") {
        return;
    }
    let Some(message) = value.get("message") else {
        return;
    };
    let Some(usage) = message.get("usage") else {
        return;
    };
    // A message repeated by a resume or fork carries the same ids; count it once.
    let id = message.get("id").and_then(Value::as_str).unwrap_or("");
    let request = value.get("requestId").and_then(Value::as_str).unwrap_or("");
    if !id.is_empty() && !message_keys.insert((id.to_string(), request.to_string())) {
        return;
    }
    let count = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    row.messages += 1;
    row.input_tokens += count("input_tokens");
    row.output_tokens += count("output_tokens");
    row.cache_read_input_tokens += count("cache_read_input_tokens");
    row.cache_creation_input_tokens += count("cache_creation_input_tokens");
}
