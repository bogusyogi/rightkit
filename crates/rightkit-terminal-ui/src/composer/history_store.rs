//! Cross-session prompt history file.
//!
//! One JSON string per line (`"text"`), oldest first, kept under the the host app
//! home (`<state root>/history.jsonl`) --
//! never inside a repository. Appends are single `write` calls on an
//! `O_APPEND` handle, so concurrent sessions interleave whole lines. The file
//! is compacted to the newest [`MAX_ENTRIES`] once it grows past twice that.
//!
//! Every failure degrades to "no persistent history": prompt entry must never
//! depend on the disk.

use std::path::PathBuf;

/// Entries offered to Up/Ctrl+R and kept after compaction.
pub(crate) const MAX_ENTRIES: usize = 1000;
/// Entries longer than this (large pastes) are session-local only.
const MAX_ENTRY_BYTES: usize = 16 * 1024;

/// The default history file under the host's per-user state root.
pub fn default_history_path(state_root: &std::path::Path) -> PathBuf {
    state_root.join("history.jsonl")
}

#[derive(Debug, Clone)]
pub(crate) struct HistoryStore {
    path: PathBuf,
}

impl HistoryStore {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Newest [`MAX_ENTRIES`] entries, oldest first. Corrupt lines are skipped.
    pub(crate) fn load(&self) -> Vec<String> {
        let Ok(raw) = std::fs::read_to_string(&self.path) else {
            return Vec::new();
        };
        let mut entries: Vec<String> = raw
            .lines()
            .filter_map(|line| serde_json::from_str::<String>(line).ok())
            .filter(|text| !text.trim().is_empty())
            .collect();
        if entries.len() > MAX_ENTRIES {
            entries.drain(..entries.len() - MAX_ENTRIES);
        }
        entries
    }

    /// Append one submission; `last` is the newest stored entry, used to skip
    /// consecutive duplicates.
    pub(crate) fn append(&self, text: &str, last: Option<&str>) {
        if text.trim().is_empty() || text.len() > MAX_ENTRY_BYTES || last == Some(text) {
            return;
        }
        let Ok(mut line) = serde_json::to_string(text) else {
            return;
        };
        line.push('\n');
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if rightkit_fs::append_record(
            &self.path,
            line.as_bytes(),
            rightkit_fs::AppendOptions::default().durable(false),
        )
        .is_err()
        {
            return;
        }
        self.compact_if_needed();
    }

    fn compact_if_needed(&self) {
        let too_big = std::fs::metadata(&self.path)
            .map(|meta| meta.len() > (MAX_ENTRIES * 2 * 256) as u64)
            .unwrap_or(false);
        if !too_big {
            return;
        }
        let kept = self.load();
        if kept.len() < MAX_ENTRIES {
            return;
        }
        let mut body = String::new();
        for entry in &kept {
            if let Ok(line) = serde_json::to_string(entry) {
                body.push_str(&line);
                body.push('\n');
            }
        }
        // Best effort: history never blocks prompt entry.
        let _ = rightkit_fs::atomic_write(&self.path, body.as_bytes());
    }
}
