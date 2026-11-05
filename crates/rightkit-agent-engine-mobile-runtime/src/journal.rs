//! Durable local session journal (ENG-33, ENG-34).
//!
//! Append-only JSONL, one record per line, under an app-owned storage path
//! supplied by the host through [`crate::RuntimeConfig`]-adjacent
//! configuration. Each record carries a contiguous monotonic sequence number
//! starting at 1, an idempotency `nonce`, and a redacted payload.
//!
//! # Credential safety
//!
//! [`JournalEvent`] is the ONLY type this module ever serializes into a
//! journal line. It intentionally has no field wide enough to carry a raw
//! secret — no `serde_json::Value`, no free-form `HashMap<String, String>`.
//! Callers construct a [`JournalEventKind`] variant, never hand this module a
//! blob to pass through. This is the enforcement boundary: a reviewer only
//! has to look at [`JournalEventKind`] to know the journal cannot contain
//! provider API keys, OAuth tokens, or Cloudflare/GitHub credentials, because
//! there is nowhere on the type for them to live. Do not add a variant or
//! field that stores a credential, token, or secret string — redact it to a
//! boolean/marker before it reaches this module.
//!
//! # Durability
//!
//! [`Journal::append`] writes one line and flushes it (`rightkit_fs::append_record`) before
//! returning, so a record is not considered committed until it has an
//! explicit flush boundary behind it (ENG-33). [`Journal::replay`] reads back
//! from a persisted cursor and deduplicates by nonce so a client that resumes
//! after a crash or relaunch never reprocesses a mutation it already
//! committed (ENG-34).

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// On-disk format version for the journal body. Bumped only for a
/// backward-incompatible record change; `journal_repair` refuses to open a
/// journal whose header names a version newer than this crate understands.
pub const JOURNAL_FORMAT_VERSION: u32 = 1;

/// Redacted event kinds. Deliberately closed (no catch-all payload variant)
/// so nothing wider than these fields can ever reach the journal — see the
/// module doc comment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum JournalEventKind {
    /// A user message was submitted. `preview` is truncated by the caller
    /// before it ever reaches this module; this module does not truncate on
    /// the caller's behalf.
    UserMessage { preview: String },
    /// A protocol event was published to the conduit at the given upstream
    /// sequence number.
    ProtocolEvent { conduit_seq: i64 },
    /// A tool call ran. `tool_name` only — never arguments, which may embed
    /// user-supplied secrets (e.g. a pasted token in a shell command).
    ToolCall { tool_name: String, ok: bool },
    /// A credential was refreshed. Marker only: never the credential value.
    CredentialRefreshed { provider: String },
    /// An effect (a committed mutation) was applied. `effect_id` is an
    /// opaque idempotency key the caller mints; it is not a credential.
    EffectApplied { effect_id: String },
}

/// One durable journal record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalEvent {
    /// Contiguous, monotonically increasing starting at 1. Never reused.
    pub sequence: u64,
    /// Idempotency key. Replaying a nonce already seen is a no-op from the
    /// caller's point of view (ENG-34): [`Journal::replay`] still
    /// contains a record's first occurrence, and its nonce dedup collapses
    /// repeats to that first occurrence.
    pub nonce: String,
    pub kind: JournalEventKind,
}

#[derive(Debug)]
pub enum JournalError {
    Io(std::io::Error),
    Serialize(serde_json::Error),
    /// `sequence` in the appended event does not match `last_sequence + 1`.
    NonContiguousSequence {
        expected: u64,
        got: u64,
    },
}

impl std::fmt::Display for JournalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "journal io error: {error}"),
            Self::Serialize(error) => write!(f, "journal serialize error: {error}"),
            Self::NonContiguousSequence { expected, got } => write!(
                f,
                "non-contiguous journal sequence: expected {expected}, got {got}"
            ),
        }
    }
}

impl std::error::Error for JournalError {}

impl From<std::io::Error> for JournalError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for JournalError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

/// Append-only per-session journal writer/reader.
///
/// One journal is one file: `<storage_dir>/<session_id>.journal.jsonl`. The
/// file is app-owned storage, named by the host's existing session id, so no
/// separate journal-id concept is introduced.
pub struct Journal {
    path: PathBuf,
    last_sequence: u64,
}

impl Journal {
    /// Open (creating if absent) the journal for `session_id` under
    /// `storage_dir`. Scans the existing file once, if any, to recover the
    /// last committed sequence number.
    pub fn open(storage_dir: &Path, session_id: &str) -> Result<Self, JournalError> {
        std::fs::create_dir_all(storage_dir)?;
        let path = journal_path(storage_dir, session_id);
        let last_sequence = if path.exists() {
            let file = File::open(&path)?;
            let reader = BufReader::new(file);
            let mut last = 0;
            for line in reader.lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(event) = serde_json::from_str::<JournalEvent>(&line) {
                    last = event.sequence;
                }
            }
            last
        } else {
            0
        };
        Ok(Self {
            path,
            last_sequence,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Sequence of the last durably appended record, or 0 for an empty
    /// journal.
    pub fn last_sequence(&self) -> u64 {
        self.last_sequence
    }

    /// Append one event, assigning it the next contiguous sequence number.
    /// `nonce` and `kind` are the caller's; the sequence is always
    /// `last_sequence() + 1` regardless of what the caller has in mind, so a
    /// caller cannot desynchronize the journal by minting its own numbers.
    ///
    /// Durable: this calls `sync_data` before returning, so a successful
    /// return means the record has crossed an explicit flush boundary
    /// (ENG-33), not merely landed in a libc/OS buffer.
    pub fn append(&mut self, nonce: String, kind: JournalEventKind) -> Result<u64, JournalError> {
        let sequence = self.last_sequence + 1;
        let event = JournalEvent {
            sequence,
            nonce,
            kind,
        };
        let mut line = serde_json::to_string(&event)?;
        line.push('\n');

        // Durable append (file flush, plus directory flush on first create).
        rightkit_fs::append_record(
            &self.path,
            line.as_bytes(),
            rightkit_fs::AppendOptions::default(),
        )
        .map_err(std::io::Error::from)?;

        self.last_sequence = sequence;
        Ok(sequence)
    }

    /// Replay every record with `sequence > cursor`, deduplicated by nonce:
    /// if the same nonce appears more than once (a caller retried an append
    /// after an ambiguous failure), only its first occurrence is returned.
    /// This is what makes it safe for a host to resume from a persisted
    /// cursor after a crash or relaunch without rerunning a committed
    /// mutation (ENG-34).
    pub fn replay(&self, cursor: u64) -> Result<Vec<JournalEvent>, JournalError> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = File::open(&self.path)?;
        let reader = BufReader::new(file);
        let mut seen_nonces = HashSet::new();
        let mut events = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let event: JournalEvent = serde_json::from_str(&line)?;
            if event.sequence <= cursor {
                continue;
            }
            if !seen_nonces.insert(event.nonce.clone()) {
                continue;
            }
            events.push(event);
        }
        Ok(events)
    }

    /// Replay at most `limit` records with `sequence > from`, deduplicated by
    /// nonce exactly as [`Journal::replay`] dedups (first occurrence of a
    /// repeated nonce wins).
    ///
    /// This is the bounded counterpart to `replay`: a host paginating history
    /// (e.g. `LocalSessionJournal.swift`) currently calls `replay` for the
    /// whole suffix and trims the result to a window in app memory, which
    /// means disk I/O is unbounded even though the returned slice is small.
    /// `replay_range` instead stops reading the file as soon as `limit`
    /// qualifying records have been collected, so a caller asking for a
    /// window near the start of a long-lived journal does not pay for
    /// scanning (or materializing) the rest of it. A window near the end of
    /// the journal still costs a scan up to that point — this format has no
    /// byte-offset index to seek by sequence number, so "bounded" here means
    /// "bounded by the window's own position," not O(1) regardless of
    /// journal length. `limit == 0` returns an empty vector without opening
    /// the file.
    ///
    /// Every invariant `replay` upholds is preserved unchanged: the journal
    /// is read-only from this call (append-only from the writer's side), only
    /// [`JournalEvent`]/[`JournalEventKind`] ever comes back (redacted by
    /// construction, see the module doc comment), and an unparsable or
    /// out-of-order line — the shape a torn/damaged tail leaves behind —
    /// surfaces as the same [`JournalError`] `replay` would return for that
    /// line rather than panicking, because both methods route through the
    /// same `serde_json::from_str::<JournalEvent>(&line)?`. If the damaged
    /// line lies outside the requested window (e.g. past where `limit` was
    /// already satisfied), `replay_range` does not reach it and returns
    /// successfully — a strictly better outcome than `replay`'s always-scan,
    /// never a worse one.
    pub fn replay_range(&self, from: u64, limit: usize) -> Result<Vec<JournalEvent>, JournalError> {
        if limit == 0 || !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = File::open(&self.path)?;
        let reader = BufReader::new(file);
        let mut seen_nonces = HashSet::new();
        let mut events = Vec::with_capacity(limit.min(1024));
        for line in reader.lines() {
            if events.len() >= limit {
                break;
            }
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let event: JournalEvent = serde_json::from_str(&line)?;
            if event.sequence <= from {
                continue;
            }
            if !seen_nonces.insert(event.nonce.clone()) {
                continue;
            }
            events.push(event);
        }
        Ok(events)
    }
}

fn journal_path(storage_dir: &Path, session_id: &str) -> PathBuf {
    storage_dir.join(format!("{session_id}.journal.jsonl"))
}

/// Read the full raw byte length of the journal file, used by
/// [`crate::journal_repair`] to detect a torn tail. Kept here (rather than
/// duplicated) since it is the one place that knows the journal's path
/// convention.
pub fn read_file_bytes(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let mut buf = Vec::new();
    file.seek(SeekFrom::Start(0))?;
    file.read_to_end(&mut buf)?;
    Ok(buf)
}
