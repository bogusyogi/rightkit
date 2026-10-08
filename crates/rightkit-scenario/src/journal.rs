//! Append-only, fsynced step journal. One JSON object per line.
//!
//! A record is acknowledged only after `sync_all` returns. On open, bytes after
//! the last newline are a torn write that was never acknowledged, so they are
//! truncated. A failed write poisons the handle.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Record {
    /// Contiguous from 1 within the journal.
    pub seq: u64,
    pub run: String,
    pub at_ms: u64,
    pub step: String,
    pub take: u32,
    /// Fencing token of the lease that wrote this record. Zero for coordinator
    /// records that need no lease.
    pub fence: u64,
    pub event: Event,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    LeaseGranted {
        worker: String,
        expires_ms: u64,
    },
    LeaseRenewed {
        expires_ms: u64,
    },
    /// Written before `perform`. From here on the action may have happened.
    Dispatched,
    /// `perform` returned an error. The outcome is unknown, so the step still
    /// resumes into observe.
    PerformErrored {
        reason: String,
    },
    Performed,
    Completed {
        value: Value,
    },
    Failed {
        reason: String,
    },
    /// The postcondition was never observed before the deadline. This is not
    /// a completion.
    Unresolved {
        reason: String,
    },
    Cancelled,
    /// Coordinator comparison of a retaken step with its original take.
    RetakeCompared {
        original_take: u32,
        matches: bool,
    },
}

pub struct Journal {
    file: File,
    run: String,
    next_seq: u64,
    poisoned: bool,
}

impl Journal {
    /// Opens or creates the journal and returns its replayed records. A torn
    /// final line is truncated and reported as absent.
    pub fn open(path: &Path, run: &str) -> Result<(Self, Vec<Record>)> {
        let existed = path.exists();
        let mut bytes = Vec::new();
        if existed {
            File::open(path)?.read_to_end(&mut bytes)?;
        }
        let keep = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);

        let mut records = Vec::new();
        for (index, line) in bytes[..keep].split(|b| *b == b'\n').enumerate() {
            if line.is_empty() {
                continue;
            }
            let record: Record = serde_json::from_slice(line).map_err(|e| Error::Corrupt {
                line: index + 1,
                reason: e.to_string(),
            })?;
            if record.run != run {
                return Err(Error::WrongRun {
                    expected: run.to_string(),
                    found: record.run,
                });
            }
            let expected_seq = records.len() as u64 + 1;
            if record.seq != expected_seq {
                return Err(Error::Corrupt {
                    line: index + 1,
                    reason: format!("seq {} where {expected_seq} expected", record.seq),
                });
            }
            records.push(record);
        }

        let file = OpenOptions::new().create(true).append(true).open(path)?;
        if (keep as u64) < file.metadata()?.len() {
            file.set_len(keep as u64)?;
            file.sync_all()?;
        }
        if !existed {
            sync_parent(path)?;
        }

        let next_seq = records.len() as u64 + 1;
        Ok((
            Self {
                file,
                run: run.to_string(),
                next_seq,
                poisoned: false,
            },
            records,
        ))
    }

    /// Appends one record and fsyncs before returning it.
    pub fn append(
        &mut self,
        step: &str,
        take: u32,
        fence: u64,
        at_ms: u64,
        event: Event,
    ) -> Result<Record> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        let record = Record {
            seq: self.next_seq,
            run: self.run.clone(),
            at_ms,
            step: step.to_string(),
            take,
            fence,
            event,
        };
        let mut line = serde_json::to_vec(&record)
            .map_err(|e| Error::Invalid(format!("journal encode: {e}")))?;
        line.push(b'\n');
        let written = self
            .file
            .write_all(&line)
            .and_then(|()| self.file.sync_all());
        if let Err(e) = written {
            self.poisoned = true;
            return Err(Error::Io(e));
        }
        self.next_seq += 1;
        Ok(record)
    }
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<()> {
    if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        File::open(dir)?.sync_all()?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> Result<()> {
    Ok(())
}
