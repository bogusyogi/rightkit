//! A temp file for an external writer, with ownership evidence a later sweep can verify.
//!
//! An encoder or another child process writes its output to a temp path beside the final file; the
//! caller then publishes it. If the process that owns the work is killed mid-write the temp stays.
//! [`crate::sweep_stale_temps`] cannot remove it, because a name shaped like a temp proves no
//! ownership (it may be an imported record). [`OwnedTemp`] makes the ownership provable:
//!
//! * [`OwnedTemp::create`] reserves the temp path `.<leaf>.rkowned-<pid>-<n>` beside `target` with an
//!   exclusive empty file, and writes the evidence next to it in `<temp>.rkowner`, created
//!   exclusively: a magic line, the caller's `owner` label, the pid of the process that made the
//!   reservation, and the temp's own file name.
//! * [`OwnedTemp::path`] is handed to the child process (it may rewrite or replace the file).
//! * [`OwnedTemp::publish`] publishes it with [`crate::publish_file_with`] and removes the evidence.
//!   Dropping an unpublished `OwnedTemp` removes the temp and its evidence.
//! * [`sweep_owned_temps`] removes a temp only when all of this holds: there is a `.rkowner` record
//!   in the directory that this module wrote (magic line) for the same `owner` label; the temp name
//!   the record names is the record's own file name without the suffix and is a plain file name;
//!   the recorded pid is **confirmed dead** (a pid that is alive, ours, or whose liveness cannot be
//!   established keeps the temp); and, immediately before the unlink, the temp is a regular file and
//!   not a link (checked on an open handle on Unix). A name without a record is never touched.
//!   A record whose temp is already gone is removed.
//!
//! A pid can be reused by an unrelated process after the owner dies. That only keeps a temp that
//! could have been removed (the recorded pid then looks alive); it never removes one that is in use.

use crate::atomic::{publish_file_with, AtomicWriteOptions, SweepReport};
use crate::error::{FsError, Result};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const MAGIC: &str = "rightkit-owned-temp 1";
const RECORD_SUFFIX: &str = ".rkowner";
const MARKER: &str = ".rkowned-";
const MAX_RECORD_BYTES: u64 = 4096;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A reserved temp path for an external writer. See the module docs.
#[derive(Debug)]
#[must_use = "an unpublished OwnedTemp removes its temp when dropped"]
pub struct OwnedTemp {
    target: PathBuf,
    temp: PathBuf,
    record: PathBuf,
    done: bool,
}

impl OwnedTemp {
    /// Reserve a temp beside `target` and record the evidence a later [`sweep_owned_temps`] with the
    /// same `owner` label can verify. `owner` must be non-empty, one line, without control
    /// characters. `target`'s final name must not be shaped like a temp name this crate generates
    /// ([`FsError::BadName`]); its directory must exist.
    pub fn create(target: &Path, owner: &str) -> Result<OwnedTemp> {
        if owner.is_empty() || owner.chars().any(char::is_control) {
            return Err(FsError::BadName(owner.to_string()));
        }
        let leaf = target
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| FsError::BadName(target.display().to_string()))?;
        crate::atomic::reject_reserved_name(leaf)?;
        let dir = match target.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        let short: String = leaf.chars().take(100).collect();
        for _ in 0..16 {
            let name = format!(
                ".{short}{MARKER}{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            );
            let temp = dir.join(&name);
            let record = dir.join(format!("{name}{RECORD_SUFFIX}"));
            // The record first: a temp without a record is never swept, so a crash between the two
            // creates leaves only a record whose temp is absent, which the sweep removes.
            let mut record_file = match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&record)
            {
                Ok(f) => f,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(FsError::io(&record, e)),
            };
            let body = format!(
                "{MAGIC}\nowner={owner}\npid={}\ntemp={name}\n",
                std::process::id()
            );
            if let Err(e) = record_file
                .write_all(body.as_bytes())
                .and_then(|()| record_file.sync_all())
            {
                drop(record_file);
                let _ = fs::remove_file(&record);
                return Err(FsError::io(&record, e));
            }
            drop(record_file);
            match OpenOptions::new().write(true).create_new(true).open(&temp) {
                Ok(_) => {
                    return Ok(OwnedTemp {
                        target: target.to_path_buf(),
                        temp,
                        record,
                        done: false,
                    })
                }
                Err(e) => {
                    let _ = fs::remove_file(&record);
                    if e.kind() == io::ErrorKind::AlreadyExists {
                        continue;
                    }
                    return Err(FsError::io(&temp, e));
                }
            }
        }
        Err(FsError::AlreadyExists(target.to_path_buf()))
    }

    /// The temp path to hand to the writer. It exists (empty) until the writer replaces or fills it.
    pub fn path(&self) -> &Path {
        &self.temp
    }

    /// The final path this temp will be published at.
    pub fn target(&self) -> &Path {
        &self.target
    }

    /// Publish the finished temp at the target, as [`publish_file_with`]; the writer must have
    /// closed it. On success the evidence record is removed. On failure nothing is removed here
    /// (the temp and its record remain, and dropping `self` cleans them up).
    pub fn publish(mut self, options: AtomicWriteOptions) -> Result<()> {
        let result = publish_file_with(&self.temp, &self.target, options);
        // `ReplacedBut...` errors mean the target holds the new content: the temp is consumed.
        if result.is_ok() || result.as_ref().is_err_and(FsError::target_replaced) {
            self.done = true;
            let _ = fs::remove_file(&self.record);
        }
        result
    }
}

impl Drop for OwnedTemp {
    fn drop(&mut self) {
        if !self.done {
            let _ = fs::remove_file(&self.temp);
            let _ = fs::remove_file(&self.record);
        }
    }
}

/// What a record file said, if it was one of ours.
struct Record {
    owner: String,
    pid: u32,
    temp: String,
}

fn read_record(path: &Path) -> Option<Record> {
    let file = fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_RECORD_BYTES).read_to_string(&mut text).ok()?;
    let mut lines = text.lines();
    if lines.next()? != MAGIC {
        return None;
    }
    let (mut owner, mut pid, mut temp) = (None, None, None);
    for line in lines {
        if let Some(v) = line.strip_prefix("owner=") {
            owner = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("pid=") {
            pid = v.parse::<u32>().ok();
        } else if let Some(v) = line.strip_prefix("temp=") {
            temp = Some(v.to_string());
        }
    }
    Some(Record {
        owner: owner?,
        pid: pid?,
        temp: temp?,
    })
}

/// Remove the temps in `dir` that [`OwnedTemp::create`] reserved with this `owner` label and whose
/// recorded process is confirmed dead. See the module docs for every condition. The report's
/// `removed` counts temps removed; `retained_live` counts temps kept because the recorded process is
/// this one or alive, `retained_unknown` those whose liveness could not be established.
/// `retained_other` counts temps kept for any other reason (a record that does not match its name, a
/// temp that is not a regular file). A missing `dir` is an empty report.
pub fn sweep_owned_temps(dir: &Path, owner: &str) -> Result<SweepReport> {
    let mut report = SweepReport {
        retained_counted: true,
        ..SweepReport::default()
    };
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(report),
        Err(e) => return Err(FsError::io(dir, e)),
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = file_name.strip_suffix(RECORD_SUFFIX) else {
            continue;
        };
        let record_path = entry.path();
        let Some(record) = read_record(&record_path) else {
            continue;
        };
        if record.owner != owner {
            continue;
        }
        // The record may only name the temp that carries its own name.
        if record.temp != stem || !stem.contains(MARKER) || stem.contains(['/', '\\']) {
            report.retained_other += 1;
            continue;
        }
        if record.pid == std::process::id() {
            report.retained_live += 1;
            continue;
        }
        match crate::process_liveness(record.pid) {
            crate::ProcessLiveness::Alive => {
                report.retained_live += 1;
                continue;
            }
            crate::ProcessLiveness::Unknown => {
                report.retained_unknown += 1;
                continue;
            }
            crate::ProcessLiveness::Dead => {}
        }
        let temp_path = dir.join(stem);
        match remove_regular_file(&temp_path) {
            Ok(Removal::Removed) => {
                report.removed += 1;
                let _ = fs::remove_file(&record_path);
            }
            Ok(Removal::Absent) => {
                let _ = fs::remove_file(&record_path);
            }
            Ok(Removal::NotRegular) | Err(_) => report.retained_other += 1,
        }
    }
    Ok(report)
}

enum Removal {
    Removed,
    Absent,
    NotRegular,
}

/// Unlink `path` only if it is a regular file and not a link, checked immediately before the
/// unlink. On Unix the check is on a handle opened without following links, and the unlink happens
/// only when the name still refers to that same file.
fn remove_regular_file(path: &Path) -> Result<Removal> {
    let before = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Removal::Absent),
        Err(e) => return Err(FsError::io(path, e)),
    };
    if before.file_type().is_symlink() || !before.is_file() {
        return Ok(Removal::NotRegular);
    }
    let file = match crate::handle::open_read_nofollow(path, &crate::handle::ReadOptions::new()) {
        Ok(f) => f,
        Err(FsError::Symlink(_) | FsError::NotAFile(_)) => return Ok(Removal::NotRegular),
        Err(e) if e.is_not_found() => return Ok(Removal::Absent),
        Err(e) => return Err(e),
    };
    let opened = crate::file_key::FileKey::of_file(&file, path)?;
    // The name must still be the file we opened.
    match crate::file_key::FileKey::of_path(path) {
        Ok(now) if now == opened => {}
        Ok(_) => return Ok(Removal::NotRegular),
        Err(e) if e.is_not_found() => return Ok(Removal::Absent),
        Err(e) => return Err(e),
    }
    drop(file);
    crate::transient::retry_transient(|| fs::remove_file(path))
        .map_err(|e| FsError::io(path, e))?;
    Ok(Removal::Removed)
}
