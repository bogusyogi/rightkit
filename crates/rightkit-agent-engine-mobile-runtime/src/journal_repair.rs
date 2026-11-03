//! Crash repair & format migration for the local session journal (ENG-35).
//!
//! A phone can be killed mid-write — background tasks killed by the OS,
//! battery death, force-quit — so the journal body (`journal.rs`) may end
//! with a torn line: a partially-written JSON record with no trailing
//! newline, or a newline followed by bytes that never got a matching close.
//! This module detects that condition, repairs the file by truncating to the
//! last complete, valid record (the "safe prefix"), and separately owns a
//! small on-disk metadata header (format version + session id) that is
//! migrated transactionally rather than edited in place.

use crate::journal::{JournalEvent, JOURNAL_FORMAT_VERSION};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::path::{Path, PathBuf};

/// Small sidecar metadata header, stored as `<journal>.meta.json` next to the
/// journal body. Kept separate from the JSONL body so migrating it never
/// touches (and risks corrupting) the append-only event stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalMetadata {
    pub format_version: u32,
    pub session_id: String,
}

#[derive(Debug)]
pub enum RepairError {
    Io(std::io::Error),
    Serialize(serde_json::Error),
    /// The journal (or its metadata) names a format version newer than this
    /// build understands. Refusing to open, rather than guessing at the
    /// schema, is the point of ENG-35's "refuse unknown format" clause.
    UnknownFormatVersion(u32),
}

impl std::fmt::Display for RepairError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "journal repair io error: {error}"),
            Self::Serialize(error) => write!(f, "journal repair serialize error: {error}"),
            Self::UnknownFormatVersion(version) => {
                write!(f, "journal format version {version} is not supported")
            }
        }
    }
}

impl std::error::Error for RepairError {}

impl From<std::io::Error> for RepairError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for RepairError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

/// Outcome of a [`repair_tail`] pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairReport {
    /// Number of valid records kept.
    pub valid_records: u64,
    /// Bytes discarded from a torn or corrupt tail. Zero means the file was
    /// already clean.
    pub bytes_truncated: u64,
}

/// Scan `path` line by line, keeping every line that parses as a valid
/// [`JournalEvent`] with strictly increasing sequence numbers, and truncate
/// the file to that safe prefix. A line that fails to parse (torn write) or
/// that goes backwards/repeats a sequence (a corrupt tail from a partial
/// fsync) ends the safe prefix at the line before it — every following byte,
/// valid-looking or not, is discarded, because a gap in the middle of a
/// journal is exactly the kind of silent data loss this exists to catch: it
/// is safer to lose confirmed-torn tail records than to risk accepting a
/// journal with a hole.
///
/// No-op (returns `bytes_truncated: 0`) when the file does not exist or is
/// already fully valid.
pub fn repair_tail(path: &Path) -> Result<RepairReport, RepairError> {
    if !path.exists() {
        return Ok(RepairReport {
            valid_records: 0,
            bytes_truncated: 0,
        });
    }

    let raw = fs::read(path)?;

    let mut safe_len: u64 = 0;
    let mut valid_records: u64 = 0;
    let mut last_sequence: u64 = 0;
    let mut cursor: u64 = 0;

    // Walk the file by its own bytes: `consumed` is each line's exact length including its
    // terminator, so a final line with no trailing newline, or CRLF endings, can neither
    // push `safe_len` past `raw.len()` nor under-count the safe prefix.
    for chunk in raw.split_inclusive(|byte| *byte == b'\n') {
        let Ok(line) = std::str::from_utf8(chunk) else {
            // Not even valid UTF-8 for this line: stop before it.
            break;
        };
        let consumed = chunk.len() as u64;
        let line = line.trim_end_matches(['\n', '\r']);
        if line.trim().is_empty() {
            cursor += consumed;
            safe_len = cursor;
            continue;
        }
        match serde_json::from_str::<JournalEvent>(line) {
            Ok(event) if event.sequence > last_sequence => {
                last_sequence = event.sequence;
                valid_records += 1;
                cursor += consumed;
                safe_len = cursor;
            }
            // Either unparsable (torn write) or a sequence that does not
            // move forward (corrupt/out-of-order tail): the safe prefix
            // stops here.
            _ => break,
        }
    }

    let bytes_truncated = raw.len() as u64 - safe_len;
    if bytes_truncated > 0 {
        let file = File::options().write(true).open(path)?;
        file.set_len(safe_len)?;
        file.sync_all()?;
    }

    Ok(RepairReport {
        valid_records,
        bytes_truncated,
    })
}

fn metadata_path(journal_path: &Path) -> PathBuf {
    let mut path = journal_path.as_os_str().to_owned();
    path.push(".meta.json");
    PathBuf::from(path)
}

/// Read the metadata header for `journal_path`, if any, refusing a future
/// format version rather than guessing at its shape.
pub fn read_metadata(journal_path: &Path) -> Result<Option<JournalMetadata>, RepairError> {
    let meta_path = metadata_path(journal_path);
    if !meta_path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&meta_path)?;
    let metadata: JournalMetadata = serde_json::from_str(&raw)?;
    if metadata.format_version > JOURNAL_FORMAT_VERSION {
        return Err(RepairError::UnknownFormatVersion(metadata.format_version));
    }
    Ok(Some(metadata))
}

/// Write (or migrate) the metadata header transactionally: the new content
/// is written to a temp file in the same directory, fsynced, then renamed
/// over the real path. `rename` is atomic on the local filesystems this
/// module targets (APFS on iOS, the desktop dev filesystems used in CI), so a
/// reader never observes a half-written header — either the old metadata or
/// the new one, never a torn mix (ENG-35's "migrate metadata transactionally"
/// clause).
pub fn write_metadata_atomic(
    journal_path: &Path,
    metadata: &JournalMetadata,
) -> Result<(), RepairError> {
    if metadata.format_version > JOURNAL_FORMAT_VERSION {
        return Err(RepairError::UnknownFormatVersion(metadata.format_version));
    }
    let meta_path = metadata_path(journal_path);
    let body = serde_json::to_vec_pretty(metadata)?;
    rightkit_fs::atomic_write(&meta_path, &body)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(())
}
