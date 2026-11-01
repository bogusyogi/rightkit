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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{Journal, JournalEventKind};
    use std::io::Write;

    #[test]
    fn clean_journal_is_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let mut journal = Journal::open(dir.path(), "s1").unwrap();
        journal
            .append(
                "n1".into(),
                JournalEventKind::UserMessage {
                    preview: "hi".into(),
                },
            )
            .unwrap();
        let report = repair_tail(journal.path()).unwrap();
        assert_eq!(report.valid_records, 1);
        assert_eq!(report.bytes_truncated, 0);
    }

    #[test]
    fn torn_tail_is_truncated_to_last_valid_record() {
        let dir = tempfile::tempdir().unwrap();
        let mut journal = Journal::open(dir.path(), "s1").unwrap();
        journal
            .append(
                "n1".into(),
                JournalEventKind::UserMessage {
                    preview: "a".into(),
                },
            )
            .unwrap();
        journal
            .append(
                "n2".into(),
                JournalEventKind::UserMessage {
                    preview: "b".into(),
                },
            )
            .unwrap();

        // Simulate a crash mid-write: append a truncated, unparsable line
        // with no trailing newline.
        {
            let mut file = File::options().append(true).open(journal.path()).unwrap();
            file.write_all(b"{\"sequence\":3,\"nonce\":\"n3\",\"kind\":{\"kind\":")
                .unwrap();
        }

        let before_len = fs::metadata(journal.path()).unwrap().len();
        let report = repair_tail(journal.path()).unwrap();
        assert_eq!(report.valid_records, 2);
        assert!(report.bytes_truncated > 0);
        let after_len = fs::metadata(journal.path()).unwrap().len();
        assert!(after_len < before_len);

        // The repaired journal must reopen cleanly at the last good record.
        let reopened = Journal::open(dir.path(), "s1").unwrap();
        assert_eq!(reopened.last_sequence(), 2);
    }

    #[test]
    fn unknown_future_format_version_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("s1.journal.jsonl");
        fs::write(&journal_path, "").unwrap();
        write_metadata_atomic(
            &journal_path,
            &JournalMetadata {
                format_version: JOURNAL_FORMAT_VERSION + 1,
                session_id: "s1".into(),
            },
        )
        .expect_err("writing a future version must itself be refused");

        // Also cover reading a header some other/future writer left behind.
        let meta_path = {
            let mut p = journal_path.as_os_str().to_owned();
            p.push(".meta.json");
            PathBuf::from(p)
        };
        fs::write(
            &meta_path,
            serde_json::to_string(&JournalMetadata {
                format_version: JOURNAL_FORMAT_VERSION + 1,
                session_id: "s1".into(),
            })
            .unwrap(),
        )
        .unwrap();
        match read_metadata(&journal_path) {
            Err(RepairError::UnknownFormatVersion(version)) => {
                assert_eq!(version, JOURNAL_FORMAT_VERSION + 1)
            }
            other => panic!("expected UnknownFormatVersion, got {other:?}"),
        }
    }

    #[test]
    fn metadata_migration_is_atomic_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("s1.journal.jsonl");
        fs::write(&journal_path, "").unwrap();

        let metadata = JournalMetadata {
            format_version: JOURNAL_FORMAT_VERSION,
            session_id: "s1".into(),
        };
        write_metadata_atomic(&journal_path, &metadata).unwrap();
        let read_back = read_metadata(&journal_path).unwrap();
        assert_eq!(read_back, Some(metadata));

        // No leftover temp file after a successful migration.
        let tmp_path = {
            let mut p = journal_path.as_os_str().to_owned();
            p.push(".meta.json.tmp");
            PathBuf::from(p)
        };
        assert!(!tmp_path.exists());
    }

    #[test]
    fn missing_metadata_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let journal_path = dir.path().join("s1.journal.jsonl");
        assert_eq!(read_metadata(&journal_path).unwrap(), None);
    }
}
