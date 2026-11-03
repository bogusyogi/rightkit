//! A line-oriented (JSONL) append journal that cuts a torn final record back before the next append.
//!
//! A crash in the middle of an append leaves a partial final record. [`crate::append_record`] leaves
//! it in place, so a later append would glue its record onto the fragment. [`JsonlJournal`] owns
//! that repair: it scans the file for the last complete, valid record, truncates a torn tail back
//! to it, and flushes.
//!
//! A *record* is one line: its bytes, then `\n`. The caller supplies the validity test (for example
//! "parses as JSON and has the checksum field"); this module knows nothing about the content.
//!
//! The repair rules, chosen so that a repair can only remove an unfinished append, never data that
//! was committed:
//!
//! * Records are read from the start. The valid prefix is kept.
//! * A final line that has no `\n` but passes the validity test is a complete record whose newline
//!   was lost: the newline is added.
//! * An invalid or unterminated tail with **no valid record after it** is a torn append: the file is
//!   truncated to the end of the last valid record and flushed.
//! * An invalid record **followed by a valid one** is damage in the middle, not a torn tail.
//!   Nothing is cut: [`JsonlJournal::open`] returns [`FsError::Corrupt`] and leaves the file as it is,
//!   so the app can quarantine it.
//! * A blank line is neither a record nor damage.
//! * A line longer than [`MAX_RECORD_BYTES`] is invalid.
//!
//! Concurrency: open and every append take the cross-process lock `<journal>.lock` (feature
//! `lock`), so a repair cannot cut a record another process is appending, and an append checks
//! that the file ends with a newline first and repairs it if not. The lock is held only for the
//! call. A symlink at the journal path is refused ([`FsError::Symlink`]) and so is a name shaped
//! like a temp name this crate generates.

use crate::append::{append_record, AppendOptions};
use crate::atomic::{durable_sync_with, SyncLevel, SyncPolicy};
use crate::error::{FsError, Result};
use crate::handle::{open_read_nofollow, ReadOptions};
use crate::lock::{FileLock, LockOptions};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The longest line, newline excluded, that counts as a record. Longer is invalid.
pub const MAX_RECORD_BYTES: usize = 16 * 1024 * 1024;

/// How long a journal call waits for the journal lock.
const LOCK_WAIT: Duration = Duration::from_secs(30);

type Validate = Box<dyn Fn(&[u8]) -> bool + Send + Sync>;

/// What [`JsonlJournal::open`] found and did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct OpenReport {
    /// Complete valid records in the journal after the repair.
    pub records: usize,
    /// Length of the journal in bytes after the repair.
    pub len: u64,
    /// Bytes of torn tail that were cut off (zero when the journal was whole).
    pub truncated_bytes: u64,
    /// True when a final valid record had lost its newline and the newline was added.
    pub newline_restored: bool,
}

/// An append journal over one file. See the module docs.
pub struct JsonlJournal {
    path: PathBuf,
    lock_path: PathBuf,
    options: AppendOptions,
    validate: Validate,
}

impl std::fmt::Debug for JsonlJournal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsonlJournal")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl JsonlJournal {
    /// Open the journal at `path` (a missing file is an empty journal, created by the first
    /// append), repairing a torn tail first. `validate` decides whether a record's bytes (the line,
    /// newline excluded) are a complete, valid record. A file this call creates later is
    /// owner-only and appends are durable, as [`AppendOptions::private`].
    pub fn open(
        path: &Path,
        validate: impl Fn(&[u8]) -> bool + Send + Sync + 'static,
    ) -> Result<(JsonlJournal, OpenReport)> {
        Self::open_with(path, AppendOptions::private(), validate)
    }

    /// [`open`](Self::open) with explicit [`AppendOptions`] for the appends.
    pub fn open_with(
        path: &Path,
        options: AppendOptions,
        validate: impl Fn(&[u8]) -> bool + Send + Sync + 'static,
    ) -> Result<(JsonlJournal, OpenReport)> {
        let name = path
            .file_name()
            .ok_or_else(|| FsError::BadName(path.display().to_string()))?;
        crate::atomic::reject_reserved_name(&name.to_string_lossy())?;
        let mut lock_name = name.to_os_string();
        lock_name.push(".lock");
        let journal = JsonlJournal {
            path: path.to_path_buf(),
            lock_path: path.with_file_name(lock_name),
            options,
            validate: Box::new(validate),
        };
        let report = {
            let _lock = journal.lock()?;
            journal.repair_locked()?
        };
        Ok((journal, report))
    }

    /// The journal file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one record. `record` is one line without a trailing newline (a single trailing
    /// `\n` is tolerated and not doubled); it must pass the validity test and hold no other
    /// newline, so the scan would keep it. The file is repaired first when it does not end with a
    /// newline. Returns the [`SyncLevel`] the flush reached, `None` when appends are not durable.
    pub fn append(&self, record: &[u8]) -> Result<Option<SyncLevel>> {
        let body = record.strip_suffix(b"\n").unwrap_or(record);
        if body.contains(&b'\n') || body.len() > MAX_RECORD_BYTES || !(self.validate)(body) {
            return Err(FsError::io(
                &self.path,
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "the record is not one valid line",
                ),
            ));
        }
        let mut line = Vec::with_capacity(body.len() + 1);
        line.extend_from_slice(body);
        line.push(b'\n');
        let _lock = self.lock()?;
        if !self.ends_with_newline()? {
            self.repair_locked()?;
        }
        append_record(&self.path, &line, self.options)
    }

    fn lock(&self) -> Result<FileLock> {
        FileLock::acquire(
            &self.lock_path,
            &LockOptions::wait(LOCK_WAIT).labeled("jsonl-journal"),
        )
    }

    /// True for a missing or empty file and for one whose last byte is a newline.
    fn ends_with_newline(&self) -> Result<bool> {
        let mut file = match open_read_nofollow(&self.path, &ReadOptions::new()) {
            Ok(file) => file,
            Err(e) if e.is_not_found() => return Ok(true),
            Err(e) => return Err(e),
        };
        let len = file
            .metadata()
            .map_err(|e| FsError::io(&self.path, e))?
            .len();
        if len == 0 {
            return Ok(true);
        }
        file.seek(SeekFrom::Start(len - 1))
            .map_err(|e| FsError::io(&self.path, e))?;
        let mut last = [0u8; 1];
        file.read_exact(&mut last)
            .map_err(|e| FsError::io(&self.path, e))?;
        Ok(last[0] == b'\n')
    }

    /// Scan and repair. The caller holds the journal lock.
    fn repair_locked(&self) -> Result<OpenReport> {
        let file = match open_read_nofollow(&self.path, &ReadOptions::new()) {
            Ok(file) => file,
            Err(e) if e.is_not_found() => return Ok(OpenReport::default()),
            Err(e) => return Err(e),
        };
        let total = file
            .metadata()
            .map_err(|e| FsError::io(&self.path, e))?
            .len();
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        let mut offset: u64 = 0;
        let mut valid_end: u64 = 0;
        let mut records = 0usize;
        // Offset of the first record that failed, and whether a valid one followed it.
        let mut first_bad: Option<u64> = None;
        let mut valid_after_bad = false;
        let mut unterminated_valid = false;
        while offset < total {
            line.clear();
            // Two bytes past the cap tell a line that is too long from one of exactly the cap.
            let read = (&mut reader)
                .take(MAX_RECORD_BYTES as u64 + 2)
                .read_until(b'\n', &mut line)
                .map_err(|e| FsError::io(&self.path, e))?;
            if read == 0 {
                break;
            }
            let terminated = line.last() == Some(&b'\n');
            if !terminated && read >= MAX_RECORD_BYTES + 2 {
                // A line that never ended within the cap: one over-long fragment, bad, and the
                // rest of the file is not scanned as records.
                first_bad.get_or_insert(offset);
                break;
            }
            let body = &line[..line.len() - usize::from(terminated)];
            if body.is_empty() {
                // A blank line is neither a record nor damage.
                offset += read as u64;
                continue;
            }
            if body.len() <= MAX_RECORD_BYTES && (self.validate)(body) {
                if first_bad.is_some() {
                    valid_after_bad = true;
                } else {
                    records += 1;
                    valid_end = offset + read as u64;
                    unterminated_valid = !terminated;
                }
            } else {
                first_bad.get_or_insert(offset);
            }
            offset += read as u64;
        }
        drop(reader);
        if valid_after_bad {
            return Err(FsError::Corrupt {
                path: self.path.clone(),
                reason: format!(
                    "an invalid record at byte {} is followed by a valid one; not a torn tail",
                    first_bad.unwrap_or(0)
                ),
            });
        }
        let mut report = OpenReport {
            records,
            len: total,
            truncated_bytes: 0,
            newline_restored: false,
        };
        if first_bad.is_none() && !unterminated_valid {
            return Ok(report);
        }
        let mut file = OpenOptions::new()
            .write(true)
            .open(&self.path)
            .map_err(|e| FsError::io(&self.path, e))?;
        if first_bad.is_some() {
            file.set_len(valid_end)
                .map_err(|e| FsError::io(&self.path, e))?;
            report.truncated_bytes = total - valid_end;
            report.len = valid_end;
        } else {
            // A valid final record that lost its newline: add it.
            use std::io::Write;
            file.seek(SeekFrom::End(0))
                .map_err(|e| FsError::io(&self.path, e))?;
            file.write_all(b"\n")
                .map_err(|e| FsError::io(&self.path, e))?;
            report.newline_restored = true;
            report.len = total + 1;
        }
        durable_sync_with(&file, SyncPolicy::Fallback).map_err(|e| FsError::io(&self.path, e))?;
        Ok(report)
    }
}
