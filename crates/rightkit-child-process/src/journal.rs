//! A persisted journal of the workers a program owns, so a program that crashed can find them
//! again (rightkit#1141).
//!
//! The owned-child list ([`owned_count`](crate::owned_count)) lives in memory and dies with the
//! process. [`WorkerJournal`] writes the identity of each worker (pid plus start time, see
//! [`AdoptedIdentity`]) and of its owner to an append-only file as the worker is spawned, and a
//! later run reads it back with [`recover`]: every worker that is still the process it was
//! recorded as, and whose owner is gone, is returned to adopt or terminate. A pid that was
//! recycled is never returned, because the start time no longer matches.
//!
//! # File format
//!
//! Text, one record per line, each ended by `\n`, fields separated by single spaces:
//!
//! ```text
//! RKJ1 S <pid> <start_major> <start_minor> <owner_pid> <owner_major> <owner_minor> <exe_hex|->
//! RKJ1 E <pid> <start_major> <start_minor>
//! ```
//!
//! `S` records a worker as started, `E` that it ended (or was handed on). A record counts only
//! once its `\n` is on disk, so a crash in the middle of a write leaves a torn final line that
//! [`recover`] drops and reports; a line that does not parse is skipped and counted. Every
//! record is flushed with `sync_data` before the call returns. Each record is one `write` to an
//! append-mode file, so several processes can share one journal.
//!
//! # Limits
//!
//! * A worker spawned and killed between two records leaves no trace: record before the worker
//!   runs. [`OwnedCommand::spawn_journaled`] does (on Windows the child is created suspended and
//!   released after its record is durable; Unix has no suspended creation, so the window is the
//!   few microseconds between `fork` and the record).
//! * The journal never signals anything itself. [`JournalRecovery::terminate_orphans`] goes
//!   through the identity-checked [`sweep`](crate::sweep).
//! * The executable path is informational (it is never compared), stored as lossy UTF-8.

use crate::{AdoptedIdentity, IdentityCheck, OwnedChild, OwnedCommand, StartToken, SweepReport};
use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    num::NonZeroU32,
    path::{Path, PathBuf},
    time::Duration,
};

const MAGIC: &str = "RKJ1";

/// An open journal that records the workers this process starts. Cheap to keep for the life of
/// the program; each record is one small append.
#[derive(Debug)]
pub struct WorkerJournal {
    path: PathBuf,
    file: File,
    owner: AdoptedIdentity,
}

impl WorkerJournal {
    /// Opens (creating it, and its parent directory, if needed) the journal at `path` for
    /// appending. The owner recorded with every worker is this process, with its start time.
    pub fn open(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let owner = own_identity()?;
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self { path, file, owner })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Records `worker` as started under this process, durably.
    pub fn record(&mut self, worker: &AdoptedIdentity) -> io::Result<()> {
        let start = worker.start_token();
        let owner = self.owner.start_token();
        let exe = worker.exe().map_or_else(
            || "-".to_owned(),
            |exe| hex(exe.to_string_lossy().as_bytes()),
        );
        self.append(&format!(
            "{MAGIC} S {} {} {} {} {} {} {exe}\n",
            worker.pid(),
            start.major,
            start.minor,
            self.owner.pid(),
            owner.major,
            owner.minor,
        ))
    }

    /// Records the identity of a freshly spawned child, read while the child is proven
    /// unreaped so the pid still names it. Fails when the child has already been reaped or
    /// cannot be described.
    pub fn record_child(&mut self, child: &OwnedChild) -> io::Result<AdoptedIdentity> {
        let captured = child.while_unreaped(|pid| {
            NonZeroU32::new(pid).map_or(Ok(None), AdoptedIdentity::capture)
        })?;
        let identity = captured.transpose()?.flatten().ok_or_else(|| {
            io::Error::other("the child could not be identified for the worker journal")
        })?;
        self.record(&identity)?;
        Ok(identity)
    }

    /// Records that `worker` ended or is no longer this program's to clean up, so recovery
    /// skips it.
    pub fn finish(&mut self, worker: &AdoptedIdentity) -> io::Result<()> {
        let start = worker.start_token();
        self.append(&format!(
            "{MAGIC} E {} {} {}\n",
            worker.pid(),
            start.major,
            start.minor
        ))
    }

    /// Rewrites the journal to hold exactly `keep` as active workers of this process (and
    /// nothing else), so a journal that has been through [`recover`] does not grow without
    /// bound. The new content is written to a sibling file, flushed, and renamed over the
    /// journal; a crash leaves either the old or the new journal.
    pub fn reset(&mut self, keep: &[AdoptedIdentity]) -> io::Result<()> {
        let mut temporary = self.path.clone().into_os_string();
        temporary.push(".new");
        let temporary = PathBuf::from(temporary);
        {
            let mut file = File::create(&temporary)?;
            let owner = self.owner.start_token();
            for worker in keep {
                let start = worker.start_token();
                let exe = worker.exe().map_or_else(
                    || "-".to_owned(),
                    |exe| hex(exe.to_string_lossy().as_bytes()),
                );
                writeln!(
                    file,
                    "{MAGIC} S {} {} {} {} {} {} {exe}",
                    worker.pid(),
                    start.major,
                    start.minor,
                    self.owner.pid(),
                    owner.major,
                    owner.minor,
                )?;
            }
            file.sync_all()?;
        }
        std::fs::rename(&temporary, &self.path)?;
        self.file = OpenOptions::new().append(true).open(&self.path)?;
        Ok(())
    }

    fn append(&mut self, line: &str) -> io::Result<()> {
        self.file.write_all(line.as_bytes())?;
        self.file.sync_data()
    }
}

impl OwnedCommand {
    /// [`spawn_suspended`](Self::spawn_suspended) with the child recorded in `journal` before it
    /// is released: the record is durable before the child executes a single instruction on
    /// Windows. A failed record or release stops the child and returns the error, so no
    /// unrecorded worker is handed out.
    pub fn spawn_journaled(self, journal: &mut WorkerJournal) -> io::Result<OwnedChild> {
        let mut child = self.spawn_suspended()?;
        let released = journal
            .record_child(&child)
            .map(|_| ())
            .and_then(|()| child.resume());
        match released {
            Ok(()) => Ok(child),
            Err(error) => {
                let _ = child.terminate_tree_bounded(Duration::from_secs(2));
                Err(error)
            }
        }
    }
}

/// What [`recover`] found in a journal.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct JournalRecovery {
    /// Workers recorded active that are still the process they were recorded as, whose owner is
    /// no longer running: adopt them or [`terminate_orphans`](Self::terminate_orphans).
    pub orphans: Vec<AdoptedIdentity>,
    /// Workers still running whose recorded owner is still running too (another live instance,
    /// or this process): not orphans, left alone.
    pub held: Vec<AdoptedIdentity>,
    /// Workers recorded active that no longer exist, or whose pid now names another process.
    pub gone: usize,
    /// Lines that did not parse and were skipped.
    pub corrupt_lines: usize,
    /// The file ended in an unterminated line (a write cut short by a crash), which was dropped.
    pub torn_tail: bool,
}

impl JournalRecovery {
    /// Stops the [`orphans`](Self::orphans) with the identity-checked [`sweep`](crate::sweep):
    /// a graceful signal each, up to `grace`, then a forced one. A pid that changed hands since
    /// [`recover`] ran is refused, never signalled.
    pub fn terminate_orphans(&self, grace: Duration) -> io::Result<SweepReport> {
        crate::sweep(&self.orphans, grace)
    }
}

/// Reads the journal at `path` and returns the workers that survived the program that recorded
/// them. A journal that does not exist yields an empty recovery. Nothing is signalled and the
/// file is not changed (see [`WorkerJournal::reset`]).
pub fn recover(path: impl AsRef<Path>) -> io::Result<JournalRecovery> {
    let bytes = match std::fs::read(path.as_ref()) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(JournalRecovery::default())
        }
        Err(error) => return Err(error),
    };
    let mut recovery = JournalRecovery::default();
    let text = String::from_utf8_lossy(&bytes);
    let complete = match text.rfind('\n') {
        Some(end) => {
            recovery.torn_tail = end + 1 != text.len();
            &text[..=end]
        }
        None => {
            recovery.torn_tail = !text.is_empty();
            ""
        }
    };
    // Active workers in file order, keyed by (pid, start): a later `E` removes the earlier `S`.
    let mut active: Vec<Active> = Vec::new();
    for line in complete.lines() {
        match parse(line) {
            Some(Record::Start(entry)) => {
                active.retain(|known| (known.pid, known.start) != (entry.pid, entry.start));
                active.push(entry);
            }
            Some(Record::End { pid, start }) => {
                active.retain(|known| (known.pid, known.start) != (pid, start));
            }
            None if line.trim().is_empty() => {}
            None => recovery.corrupt_lines += 1,
        }
    }
    for entry in active {
        let worker = AdoptedIdentity::from_recorded(entry.pid, entry.start, entry.exe);
        if worker.verify()? != IdentityCheck::Same || !worker.is_running()? {
            recovery.gone += 1;
            continue;
        }
        let owner = AdoptedIdentity::from_recorded(entry.owner_pid, entry.owner_start, None);
        if owner.verify()? == IdentityCheck::Same && owner.is_running()? {
            recovery.held.push(worker);
        } else {
            recovery.orphans.push(worker);
        }
    }
    Ok(recovery)
}

#[derive(Debug)]
struct Active {
    pid: NonZeroU32,
    start: StartToken,
    owner_pid: NonZeroU32,
    owner_start: StartToken,
    exe: Option<PathBuf>,
}

enum Record {
    Start(Active),
    End { pid: NonZeroU32, start: StartToken },
}

fn parse(line: &str) -> Option<Record> {
    let mut fields = line.split(' ');
    if fields.next()? != MAGIC {
        return None;
    }
    let kind = fields.next()?;
    let pid = NonZeroU32::new(fields.next()?.parse().ok()?)?;
    let start = StartToken {
        major: fields.next()?.parse().ok()?,
        minor: fields.next()?.parse().ok()?,
    };
    match kind {
        "E" => fields
            .next()
            .is_none()
            .then_some(Record::End { pid, start }),
        "S" => {
            let owner_pid = NonZeroU32::new(fields.next()?.parse().ok()?)?;
            let owner_start = StartToken {
                major: fields.next()?.parse().ok()?,
                minor: fields.next()?.parse().ok()?,
            };
            let exe = match fields.next()? {
                "-" => None,
                encoded => Some(PathBuf::from(unhex(encoded)?)),
            };
            fields.next().is_none().then_some(Record::Start(Active {
                pid,
                start,
                owner_pid,
                owner_start,
                exe,
            }))
        }
        _ => None,
    }
}

fn own_identity() -> io::Result<AdoptedIdentity> {
    let pid = NonZeroU32::new(std::process::id())
        .ok_or_else(|| io::Error::other("this process has pid 0"))?;
    AdoptedIdentity::capture(pid)?
        .ok_or_else(|| io::Error::other("this process could not be described"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Option<String> {
    if text.len() % 2 != 0 {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
        .collect();
    String::from_utf8(bytes?).ok()
}
