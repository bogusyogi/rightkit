//! Count-bounded private JSONL histories that retain raw lines.

use crate::store::{acquire_rotation_lock, tail_bytes};
use serde_json::Value;
use std::fs;
use std::io;
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::Path;

/// One private JSONL history, ordered oldest to newest.
///
/// Use the same path for all writers so they take the same sidecar lock.
/// Reading never rewrites the file or creates the lock.
#[derive(Debug)]
pub struct RecentJsonl {
    records: NonZeroUsize,
    max_bytes: NonZeroU64,
}

impl RecentJsonl {
    /// Limits both the persisted history and the portion of a legacy file read.
    /// The next append trims legacy files over either limit.
    pub const fn new(records: NonZeroUsize, max_bytes: NonZeroU64) -> Self {
        Self { records, max_bytes }
    }

    /// Reads the newest nonblank raw lines. Missing files read as empty; other read
    /// errors propagate. Malformed JSON counts toward retention; invalid UTF-8 is
    /// decoded lossily. A legacy file over the byte limit starts at its next full line.
    pub fn read(&self, path: &Path) -> io::Result<Vec<String>> {
        let tail = self.read_tail(path)?;
        let mut lines = tail.lines();
        let keep = lines.len().saturating_sub(self.records.get());
        Ok(lines.split_off(keep))
    }

    /// Appends one value under the per-log sidecar lock. Oversized records and failed
    /// reads leave the existing file untouched. A full or oversized log is replaced
    /// atomically; the rewrite keeps the newest consecutive lines within both limits.
    pub fn append(&self, path: &Path, value: &Value) -> io::Result<()> {
        let line = serde_json::to_string(value)?;
        let size = line.len() as u64 + 1;
        if size > self.max_bytes.get() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "record exceeds JSONL byte limit",
            ));
        }
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let _lock = acquire_rotation_lock(path)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::WouldBlock, "log lock timed out"))?;
        let tail = self.read_tail(path)?;
        let mut lines = tail.lines();
        let needs_separator = tail.bytes.last().is_some_and(|byte| *byte != b'\n');
        let new_size = (tail.bytes.len() as u64)
            .saturating_add(size)
            .saturating_add(u64::from(needs_separator));
        if !tail.truncated && lines.len() < self.records.get() && new_size <= self.max_bytes.get() {
            let record = if needs_separator {
                format!("\n{line}\n")
            } else {
                format!("{line}\n")
            };
            return rightkit_fs::append_record(
                path,
                record.as_bytes(),
                rightkit_fs::AppendOptions::private(),
            )
            .map(|_| ())
            .map_err(crate::atomic::into_io);
        }
        lines.push(line);
        let mut text = lines[self.first_retained(&lines)..].join("\n");
        text.push('\n');
        rightkit_fs::atomic_write_private(path, text.as_bytes()).map_err(crate::atomic::into_io)
    }

    fn read_tail(&self, path: &Path) -> io::Result<Tail> {
        // Read one lookbehind byte to recognize an exact line boundary at the cutoff.
        let mut bytes = tail_bytes(path, self.max_bytes.get().saturating_add(1))?;
        let truncated = bytes.len() as u64 > self.max_bytes.get();
        let offset = if truncated && bytes[0] != b'\n' {
            bytes
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |at| at + 1)
        } else {
            usize::from(truncated)
        };
        if offset > 0 {
            bytes.drain(..offset);
        }
        Ok(Tail { bytes, truncated })
    }

    fn first_retained(&self, lines: &[String]) -> usize {
        let mut remaining = self.max_bytes.get();
        let mut start = lines.len();
        for line in lines.iter().rev().take(self.records.get()) {
            let bytes = line.len() as u64 + 1;
            if bytes > remaining {
                break;
            }
            remaining -= bytes;
            start -= 1;
        }
        start
    }
}

struct Tail {
    bytes: Vec<u8>,
    truncated: bool,
}

impl Tail {
    fn lines(&self) -> Vec<String> {
        String::from_utf8_lossy(&self.bytes)
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_owned)
            .collect()
    }
}
