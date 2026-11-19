//! The transcript sync host: the consent switch and the account gate in front of the engine, the
//! engine's lifetime, the durable deletion outbox, the history adapter and the pass.
//!
//! Everything product-specific (names, file names, wording, timings) comes in through
//! [`TranscriptSyncConfig`]; the app's history store, clock and activity gate come in through
//! [`TranscriptHost`]; the backend comes in through [`SyncTransport`].

use std::cell::Cell;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeSet, VecDeque};
use std::ffi::OsStr;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, TryLockError};
use std::time::{Duration, Instant};

use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Serialize};

use super::history::{scan_keys, HistoryLocalStore, LOCAL_WINDOW_ROWS};
use super::ledger_guard::{scope_of_head, Disk, LedgerGuard, ScopeState};
use super::outbox::{Outbox, MAX_OUTBOX_ENTRIES};
use super::remote_window::{RemoteWindow, WindowedBackend};
use super::scheduler::{Scheduler, SchedulerPolicy, PULL, PUSH};
use super::traits::{NativeObserver, Spawner, SyncTransport, TranscriptHost};
use crate::transcripts::{
    self as sync_core, ConsentScopeBinding, LegacyFiles, LocalVersions, StateFileNames,
    TranscriptMapper, TranscriptSyncProfile,
};
use crate::{
    AuthProvider, Backend, FileStorage, LedgerStorage, PassOutcome, PassReport, SyncConfig,
    SyncEngine, SyncEngineBuilder, SyncError, SyncObserver, SyncResult,
};

/// The consent file is a fixed-field struct of a few dozen bytes; a larger file is not ours.
const MAX_CONSENT_FILE_BYTES: u64 = 1024 * 1024;
/// The sync fence file is `{"v":2,"state":"revoked"|"enabled","epoch":<n>}`, a few dozen bytes
/// (a v1 file `{"v":1,"epoch":<n>}` reads as state revoked); one no-follow read capped at
/// 4 KiB. A larger file is not ours and reads as untrusted.
const MAX_FENCE_FILE_BYTES: u64 = 4096;
/// Largest ledger file read or written. The ledger holds one remote version and one alias per
/// record (about 124 and 80 bytes), the tombstones (about 53 bytes) and the stashed records whose
/// local apply was deferred. 1,000,000 records are about 204 MB, so the cap leaves room for the
/// largest supported history and its tombstones. The engine holds the ledger in memory and writes
/// it whole on every change (see "Unproven" in HANDOFF.md for what that costs).
pub(super) const MAX_LEDGER_FILE_BYTES: u64 = 384 * 1024 * 1024;
/// Most elements across the ledger's collections (tombstones, pending tombstones, remote
/// versions, aliases, pending uploads, stashed records, flags, withheld ids, extras). Every
/// element the engine writes is at least 38 bytes (a quoted 36-character id), so a ledger the
/// engine wrote within the byte cap is never over this count (384 MiB / 38 B is about 10.59
/// million); the element cap only stops a foreign file made of very small elements.
const MAX_LEDGER_ENTRIES: usize = 10_600_000;
/// Largest legacy acknowledgement file read, only to learn its scope.
const MAX_LEGACY_ACKS_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// Upper bound `TranscriptSyncConfig::max_history_rows` may have: the largest history the earlier
/// HeardRight code supported. History is not loaded whole: it is walked in windows of keys and
/// read by id in windows of rows (`history.rs`); this bound is the size of the key index one pass
/// keeps (about 150 bytes per row, estimated, not measured), so a history above the host's limit
/// fails the operation that walks it and nothing is deleted.
const MAX_HISTORY_ROWS_LIMIT: usize = 1_000_000;
/// Largest local version book read (about 146 bytes per entry; 1,100,000 entries are about
/// 160 MB).
const MAX_VERSIONS_FILE_BYTES: u64 = 256 * 1024 * 1024;
/// Most entries the version book may hold when it is read. The book keeps an entry per history
/// row, and rows are capped at `MAX_HISTORY_ROWS_LIMIT`; the rest is slack for ids of rows
/// deleted since the book was last pruned.
const MAX_VERSIONS_ENTRIES: usize = 1_100_000;
/// Most deletions one pass hands to the engine (one ledger write for all of them). The rest stay
/// in the outbox and the pass asks for another one. It also bounds the tombstones the engine
/// holds to write: a pass hands over no more than this many minus what is still pending.
const MAX_DRAIN_PER_PASS: usize = 20_000;
/// Most items one pass keeps in memory to upload (`SyncEngineBuilder::max_uploads_per_pass`,
/// crate patch 0006); the rest are left for the next pass.
const MAX_UPLOADS_PER_PASS: usize = 5_000;
/// How long `TranscriptSync::status` waits for the account check before it reports
/// `NotConfirmed("check_timed_out")`. The blocking call itself ends when the helper answers or
/// its request timeout passes; it holds no lock the pass needs.
const STATUS_CHECK_TIMEOUT: Duration = Duration::from_secs(15);
/// Most consecutive passes that each left work over (feed cut, uploads left, or deletions
/// continuing) and were rescheduled immediately, whatever cursors they ended on and whatever they
/// applied. At this count the immediate reschedule stops and one `FEED_BACKOFF_BASE_SECS` wait is
/// taken (`StatusReport::Paused`), so a cycle longer than `FEED_RECENT_CURSORS` with positive
/// counts cannot spin. Independent of the cursor record and of `halt_streak`.
const FEED_MAX_IMMEDIATE_PARTIAL_PASSES: u32 = 16;
/// Pass-ending cursors remembered per process (as 64-bit digests) to spot a cycle across passes.
const FEED_RECENT_CURSORS: usize = 64;
/// Consecutive cut passes without real progress (a never-seen cursor AND at least one record
/// applied) after which the immediate reschedule stops.
const FEED_MAX_CUT_PASSES_WITHOUT_PROGRESS: u32 = 4;
/// Backoff after a halted pass: 60 s doubling per consecutive halt, clamped to 3,600 s.
const FEED_BACKOFF_BASE_SECS: u64 = 60;
const FEED_BACKOFF_MAX_SECS: u64 = 3_600;

/// What the host remembers across passes about the remote feed, in memory only (nothing survives
/// a restart; each process is bounded by the same rule from zero).
#[derive(Default)]
struct FeedWatch {
    /// Digests of the cursors recent cut passes ended on, oldest first, at most
    /// `FEED_RECENT_CURSORS`.
    recent: VecDeque<u64>,
    /// Consecutive cut passes without real progress.
    cut_streak: u32,
    /// Consecutive halted passes; drives the backoff. Reset only by real progress or a pass that
    /// reached the end of the feed.
    halt_streak: u32,
    /// Consecutive passes that left work over and were rescheduled immediately; counted whatever
    /// cursors or records they had. At `FEED_MAX_IMMEDIATE_PARTIAL_PASSES` the limit is taken
    /// (`note_immediate`) and the count restarts; a pass that leaves nothing over, a halt, or an
    /// explicit user action also restarts it.
    immediate_streak: u32,
}

/// What the window and the report say about one Synced pass.
struct FeedFacts<'a> {
    stalled: bool,
    cut: bool,
    end_token: Option<&'a [u8]>,
    /// At least one record or tombstone was applied from the feed.
    applied: bool,
}

enum FeedVerdict {
    Continue,
    /// The feed is not trustworthy as complete: reason tag (`stalled`, `cycle`, `no_progress`).
    Halt(&'static str),
}

fn cursor_digest(token: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    token.hash(&mut hasher);
    hasher.finish()
}

impl FeedWatch {
    fn judge(&mut self, facts: &FeedFacts<'_>) -> FeedVerdict {
        if facts.stalled {
            self.halt_streak = self.halt_streak.saturating_add(1);
            return FeedVerdict::Halt("stalled");
        }
        if !facts.cut {
            // The pass reached the end of the feed: nothing is left to loop on. The immediate
            // count is kept: uploads or deletions may still be left over (the caller restarts it
            // when nothing is).
            *self = Self {
                immediate_streak: self.immediate_streak,
                ..Self::default()
            };
            return FeedVerdict::Continue;
        }
        let digest = facts.end_token.map(cursor_digest);
        let fresh = match digest {
            Some(digest) if !self.recent.contains(&digest) => Some(digest),
            _ => None,
        };
        // No cursor, or one a recent pass already ended on: the feed went round (A, B, A ...).
        let Some(digest) = fresh else {
            self.halt_streak = self.halt_streak.saturating_add(1);
            return FeedVerdict::Halt("cycle");
        };
        if self.recent.len() >= FEED_RECENT_CURSORS {
            self.recent.pop_front();
        }
        self.recent.push_back(digest);
        if facts.applied {
            self.cut_streak = 0;
            self.halt_streak = 0;
            return FeedVerdict::Continue;
        }
        self.cut_streak = self.cut_streak.saturating_add(1);
        if self.cut_streak >= FEED_MAX_CUT_PASSES_WITHOUT_PROGRESS {
            self.halt_streak = self.halt_streak.saturating_add(1);
            return FeedVerdict::Halt("no_progress");
        }
        FeedVerdict::Continue
    }

    /// A pass that left work over is about to be rescheduled immediately: count it. `true` when
    /// the limit is reached; the count then restarts (the next immediate pass can only run after
    /// the backoff wait the caller imposes), and the caller must not reschedule immediately.
    fn note_immediate(&mut self) -> bool {
        self.immediate_streak = self.immediate_streak.saturating_add(1);
        if self.immediate_streak >= FEED_MAX_IMMEDIATE_PARTIAL_PASSES {
            self.immediate_streak = 0;
            true
        } else {
            false
        }
    }

    /// 60, 120, 240, 480, 960, 1,920, then 3,600 seconds (checked arithmetic, clamped).
    fn backoff(&self) -> Duration {
        let exponent = self.halt_streak.saturating_sub(1).min(6);
        let factor = 1u64.checked_shl(exponent).unwrap_or(64);
        let secs = FEED_BACKOFF_BASE_SECS
            .checked_mul(factor)
            .unwrap_or(FEED_BACKOFF_MAX_SECS)
            .clamp(FEED_BACKOFF_BASE_SECS, FEED_BACKOFF_MAX_SECS);
        Duration::from_secs(secs)
    }
}

/// The account and consent epoch one `delete_everywhere` operation acts under. Captured once at
/// entry from a positive observation of the live account made for that operation, and the only
/// source of the account for every later step: nothing in the operation re-reads
/// `authorized_scope` to decide whose ids these are.
#[derive(Debug, Clone)]
struct DeleteToken {
    /// The live account observed at entry, equal to the consent-authorized account.
    scope: String,
    /// The consent epoch at entry.
    generation: u64,
    /// The durable consent epoch (consent file), read BEFORE the observation that names the
    /// account. The outbox stamp is valid only while it is still the durable epoch.
    epoch: u64,
}

/// Returned (as the error text) by a pass that was started for a `delete_everywhere` token and
/// found at its own account observation that the token no longer holds; mapped to
/// `DeleteOutcome::Incomplete("scope_changed")`.
const PASS_TOKEN_MISMATCH: &str = "delete_everywhere_token_mismatch";

/// Everything the host chooses. There is no default for any field.
pub struct TranscriptSyncConfig {
    /// Record type, hash domains, default source, helper bundle names and state file names.
    /// Existing installs must pass the exact values they have always used (see
    /// `TranscriptSyncProfile`).
    pub profile: TranscriptSyncProfile,
    /// File name of the consent state under the data root (not environment namespaced).
    pub consent_file: String,
    /// `source` label written on records recorded on this device (a wire value shared with the
    /// other apps that read the same zone).
    pub local_source: String,
    /// Error returned when the live account differs from the one consent was given for.
    pub account_changed_message: String,
    /// Error returned when the transport reports no account scope.
    pub scope_unavailable_message: String,
    /// Most history rows one sync operation will walk. Must be 1..=1_000_000; a value outside that
    /// range makes `TranscriptSync::new` return an error. It bounds the key index a pass keeps
    /// (about 150 bytes per row); row text is read in windows and never held whole. A history over
    /// the limit fails the operation that walked it (nothing is deleted and nothing is read as
    /// "no rows").
    pub max_history_rows: usize,
    pub scheduler: SchedulerPolicy,
    /// Diagnostics sink: `(event name, detail)`. Details are sanitized before they reach it
    /// (path-like tokens replaced, length bounded) and never contain record text.
    pub event_sink: fn(&str, &str),
    pub spawn: Spawner,
}

fn check_text(what: &str, value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
        return Err(format!(
            "sync config: {what} is empty, longer than 512 bytes or has control characters"
        ));
    }
    Ok(())
}

fn check_file_name(what: &str, value: &str) -> Result<(), String> {
    check_text(what, value)?;
    let mut parts = Path::new(value).components();
    match (parts.next(), parts.next()) {
        (Some(Component::Normal(name)), None)
            if name == OsStr::new(value) && value.len() <= 128 =>
        {
            Ok(())
        }
        _ => Err(format!("sync config: {what} must be one plain file name")),
    }
}

impl TranscriptSyncConfig {
    fn validate(&self) -> Result<(), String> {
        let p = &self.profile;
        for (what, value) in [
            ("record_type", p.record_type.as_str()),
            ("id_domain", p.id_domain.as_str()),
            ("scope_domain", p.scope_domain.as_str()),
            ("default_source", p.default_source.as_str()),
            (
                "helper_bundle_production",
                p.helper_bundle_production.as_str(),
            ),
            (
                "helper_bundle_development",
                p.helper_bundle_development.as_str(),
            ),
            ("local_source", self.local_source.as_str()),
            (
                "account_changed_message",
                self.account_changed_message.as_str(),
            ),
            (
                "scope_unavailable_message",
                self.scope_unavailable_message.as_str(),
            ),
        ] {
            check_text(what, value)?;
        }
        for (what, value) in [
            ("files.ledger", p.files.ledger.as_str()),
            ("files.versions", p.files.versions.as_str()),
            ("files.outbox", p.files.outbox.as_str()),
            (
                "files.legacy_tombstones",
                p.files.legacy_tombstones.as_str(),
            ),
            ("files.legacy_pending", p.files.legacy_pending.as_str()),
            ("files.legacy_aliases", p.files.legacy_aliases.as_str()),
            ("files.legacy_acks", p.files.legacy_acks.as_str()),
            ("consent_file", self.consent_file.as_str()),
        ] {
            check_file_name(what, value)?;
        }
        if p.files.ledger.len() > 100 {
            return Err(
                "sync config: files.ledger must be at most 100 bytes (the ledger lock file name adds a suffix)"
                    .to_string(),
            );
        }
        if !(1..=MAX_HISTORY_ROWS_LIMIT).contains(&self.max_history_rows) {
            return Err(format!(
                "sync config: max_history_rows must be 1..={MAX_HISTORY_ROWS_LIMIT}"
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Bounded file helpers
// ---------------------------------------------------------------------------

pub(super) fn too_large() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "state file larger than its cap")
}

/// Open a state file for reading without following a link at its name (`O_NOFOLLOW` on Unix,
/// the reparse point itself on Windows). Only a regular file opens. The data root belongs to the
/// host, so the parent directories and the hard-link count are not checked. Errors carry no
/// path.
pub(super) fn open_state_file(path: &Path) -> io::Result<std::fs::File> {
    rightkit_fs::open_read_nofollow(path, &rightkit_fs::ReadOptions::new()).map_err(|error| {
        match error {
            rightkit_fs::FsError::Io { source, .. } => source,
            _ => io::Error::new(
                io::ErrorKind::InvalidData,
                "state file is a link or not a regular file",
            ),
        }
    })
}

/// One no-follow handle, a byte cap checked on that handle before any byte is read and again
/// while reading (a file that grows meanwhile), no second open. A file over the cap is an error,
/// never a truncated success. A missing file is an error of kind `NotFound`.
pub(super) fn read_capped(path: &Path, cap: u64) -> io::Result<Vec<u8>> {
    let file = open_state_file(path)?;
    if file.metadata()?.len() > cap {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    file.take(cap.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > cap {
        return Err(too_large());
    }
    Ok(bytes)
}

/// Decode one JSON document by streaming it through one no-follow handle and a byte cap, keeping
/// only what `T` keeps. `Ok(None)`: no such file. A file over the cap, one that does not decode
/// as `T` and a read error are `Err` (the error text never quotes the file).
fn stream_json<T: serde::de::DeserializeOwned>(path: &Path, cap: u64) -> io::Result<Option<T>> {
    let file = match open_state_file(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if file.metadata()?.len() > cap {
        return Err(too_large());
    }
    let mut limited = file.take(cap.saturating_add(1));
    let parsed = serde_json::from_reader::<_, T>(BufReader::new(&mut limited));
    if limited.limit() == 0 {
        return Err(too_large());
    }
    parsed.map(Some).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("state file does not decode ({:?})", error.classify()),
        )
    })
}

/// Whole-file atomic replacement through `rightkit_fs::atomic`: parent directories created, temp
/// file fsynced and renamed over the old one. One attempt; the error is reported. This is the
/// same function as the private one in `transcripts`; delete this copy when that one is made
/// `pub(crate)`.
pub(super) fn replace_file_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let options = rightkit_fs::atomic::AtomicWriteOptions {
        create_parents: true,
        ..Default::default()
    };
    rightkit_fs::atomic::atomic_write_with_options(path, options, |file| file.write_all(bytes))
        .map_err(|error| match error {
            rightkit_fs::FsError::Io { source, .. }
            | rightkit_fs::FsError::ReplacedButDirSyncFailed { source, .. } => source,
            _ => io::Error::other("atomic write refused"),
        })
}

/// The account scope a state file is bound to (`Ok(None)`: no file, or a file without a scope).
/// One no-follow handle. The scope is the first field the engine writes, so the first 4,096 bytes
/// answer it; a file that does not show it there is streamed through the same handle from the
/// start (byte cap checked on the handle and while reading).
fn read_scope(path: &Path, cap: u64) -> io::Result<Option<String>> {
    #[derive(Deserialize)]
    struct ScopeOnly {
        #[serde(default)]
        scope: Option<String>,
    }
    let mut file = match open_state_file(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if file.metadata()?.len() > cap {
        return Err(too_large());
    }
    let mut head = Vec::with_capacity(4096);
    (&mut file).take(4096).read_to_end(&mut head)?;
    match scope_of_head(&head) {
        ScopeState::Bound(scope) => return Ok(Some(scope)),
        ScopeState::Unbound => return Ok(None),
        ScopeState::Unknown => {}
    }
    file.seek(SeekFrom::Start(0))?;
    let mut limited = file.take(cap.saturating_add(1));
    let parsed = serde_json::from_reader::<_, ScopeOnly>(BufReader::new(&mut limited));
    if limited.limit() == 0 {
        return Err(too_large());
    }
    parsed.map(|state| state.scope).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("state file does not decode ({:?})", error.classify()),
        )
    })
}

/// Keys of a JSON object, collected up to `MAX_OUTBOX_ENTRIES`; the values are skipped.
#[derive(Default)]
struct KeySet(Vec<String>);

impl<'de> Deserialize<'de> for KeySet {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct KeysVisitor;
        impl<'de> Visitor<'de> for KeysVisitor {
            type Value = KeySet;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<KeySet, A::Error> {
                let mut keys = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    if keys.len() >= MAX_OUTBOX_ENTRIES {
                        return Err(<A::Error as serde::de::Error>::custom(
                            "more ids than the cap",
                        ));
                    }
                    map.next_value::<IgnoredAny>()?;
                    keys.push(key);
                }
                Ok(KeySet(keys))
            }
        }
        deserializer.deserialize_map(KeysVisitor)
    }
}

/// The scope a ledger is bound to and the ids of the records it knows on the remote.
struct LedgerIds {
    scope: Option<String>,
    remote_ids: Vec<String>,
}

fn read_ledger_ids(path: &Path) -> io::Result<LedgerIds> {
    #[derive(Deserialize)]
    struct Shape {
        #[serde(default)]
        scope: Option<String>,
        #[serde(default)]
        remote_versions: KeySet,
    }
    Ok(match stream_json::<Shape>(path, MAX_LEDGER_FILE_BYTES)? {
        Some(shape) => LedgerIds {
            scope: shape.scope,
            remote_ids: shape.remote_versions.0,
        },
        None => LedgerIds {
            scope: None,
            remote_ids: Vec::new(),
        },
    })
}

/// Counts the elements of the ledger's collections while decoding, keeping none of them.
struct Tally {
    remaining: Cell<usize>,
    over: Cell<bool>,
}

impl Tally {
    fn take<E: serde::de::Error>(&self) -> Result<(), E> {
        match self.remaining.get().checked_sub(1) {
            Some(left) => {
                self.remaining.set(left);
                Ok(())
            }
            None => {
                self.over.set(true);
                Err(E::custom("ledger holds more entries than the cap"))
            }
        }
    }
}

struct CountSeed<'a>(&'a Tally);

impl<'de> DeserializeSeed<'de> for CountSeed<'_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(CountVisitor(self.0))
    }
}

struct CountVisitor<'a>(&'a Tally);

impl<'de> Visitor<'de> for CountVisitor<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a map or a list")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while map.next_key::<IgnoredAny>()?.is_some() {
            map.next_value::<IgnoredAny>()?;
            self.0.take::<A::Error>()?;
        }
        Ok(())
    }
    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {
            self.0.take::<A::Error>()?;
        }
        Ok(())
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<(), E> {
        Ok(())
    }
}

struct ShapeVisitor<'a>(&'a Tally);

impl<'de> Visitor<'de> for ShapeVisitor<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a ledger")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "tombstones" | "pending_tombstones" | "remote_versions" | "aliases"
                | "pending_uploads" | "pending_apply" | "flags" | "withheld" | "extras" => {
                    map.next_value_seed(CountSeed(self.0))?;
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(())
    }
}

/// Refuse a ledger with more than `MAX_LEDGER_ENTRIES` collection elements. Only the cap is
/// judged here: a ledger that does not decode is left to the engine, which treats it as corrupt
/// and starts from an empty ledger (a full replay).
fn check_ledger_entries(bytes: &[u8]) -> Result<(), String> {
    let tally = Tally {
        remaining: Cell::new(MAX_LEDGER_ENTRIES),
        over: Cell::new(false),
    };
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let _ = (&mut deserializer).deserialize_map(ShapeVisitor(&tally));
    if tally.over.get() {
        Err("cloud sync ledger holds more entries than the cap".to_string())
    } else {
        Ok(())
    }
}

/// The engine's ledger storage: bounded and no-follow on load, bounded on save, atomic and
/// owner-only on save through `FileStorage`, and every save goes through the [`LedgerGuard`]
/// (the disk must still equal what was loaded or last written, and the account may change only to
/// the one the host established). A ledger over a cap is an `Err` that fails the engine build; the
/// file is never truncated or replaced because of it.
struct BoundedLedgerStorage {
    path: PathBuf,
    writer: FileStorage,
    guard: Arc<LedgerGuard>,
}

impl BoundedLedgerStorage {
    fn new(path: PathBuf, guard: Arc<LedgerGuard>) -> Self {
        Self {
            writer: FileStorage::new(path.clone()),
            path,
            guard,
        }
    }
}

impl LedgerStorage for BoundedLedgerStorage {
    fn load(&self) -> Result<Option<Vec<u8>>, String> {
        let bytes = match read_capped(&self.path, MAX_LEDGER_FILE_BYTES) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.guard.record_loaded(None);
                return Ok(None);
            }
            Err(error) => return Err(format!("read cloud sync ledger: {error}")),
        };
        check_ledger_entries(&bytes)?;
        self.guard.record_loaded(Some(&bytes));
        Ok(Some(bytes))
    }

    fn save(&self, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() as u64 > MAX_LEDGER_FILE_BYTES {
            return Err("cloud sync ledger is larger than its cap".to_string());
        }
        self.guard
            .replace(bytes, |bytes| self.writer.save_locked(bytes))
    }
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

struct StatePaths {
    host: Arc<dyn TranscriptHost>,
    transport: Arc<dyn SyncTransport>,
    files: StateFileNames,
}

impl StatePaths {
    /// A state file of the transport's environment.
    fn env(&self, base: &str) -> PathBuf {
        self.host.data_file(&self.transport.state_file_name(base))
    }

    /// A file under the data root that is not environment namespaced.
    fn root(&self, base: &str) -> PathBuf {
        self.host.data_file(base)
    }

    /// The account a ledger already belongs to, so an account switch made before an upgrade is
    /// not silently adopted. The legacy acknowledgement file is consulted only when the
    /// transport says the legacy state belongs to its environment. `Ok(None)` means there is no
    /// such file or it has no scope; a file that cannot be read, is over its cap or does not
    /// decode is an `Err`, never `None`, so an unreadable anchor cannot make the live account
    /// look like the acknowledged one.
    fn acknowledged_scope(&self) -> Result<Option<String>, String> {
        let ledger = read_scope(&self.env(&self.files.ledger), MAX_LEDGER_FILE_BYTES)
            .map_err(|error| format!("read cloud sync ledger scope: {error}"))?;
        if ledger.is_some() || !self.transport.legacy_import() {
            return Ok(ledger);
        }
        read_scope(
            &self.root(&self.files.legacy_acks),
            MAX_LEGACY_ACKS_FILE_BYTES,
        )
        .map_err(|error| format!("read legacy acknowledgement scope: {error}"))
    }

    /// The acknowledged scope, read only when `authorize_scope` can use it: while the consent
    /// holds no authorized scope or no legacy scope yet. Once both are stored the ledger is not
    /// read here at all, so an unreadable ledger cannot block an account that is already
    /// authorized (the engine's own load reports it).
    fn anchor_for(&self, binding: &ConsentScopeBinding) -> Result<Option<String>, String> {
        if binding.authorized_scope.is_some() && binding.legacy_scope.is_some() {
            return Ok(None);
        }
        self.acknowledged_scope()
    }
}

// ---------------------------------------------------------------------------
// Consent: the one user-facing switch, persisted per device. Absent state is OFF.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct TranscriptSyncState {
    v: u8,
    #[serde(default)]
    sync_enabled: bool,
    /// The durable consent epoch. Increases by one, in the same atomic file write, on every
    /// enable, every disable and every change of the authorized account; never decreases. A file
    /// without the field (written before it existed) reads as 0 and is bumped by the first
    /// transition. Scoped outbox entries record the epoch they were stamped in and are sent only
    /// while it is still the current one. The counter lives in this file only as long as the file
    /// does: the outbox file keeps a high-water mark of every epoch it carried, a transition moves
    /// to one above the larger of the two, and an epoch below the high-water mark is not trusted
    /// (`TranscriptSync::trusted_state`).
    #[serde(default)]
    epoch: u64,
    #[serde(flatten)]
    scope: ConsentScopeBinding,
}

impl Default for TranscriptSyncState {
    fn default() -> Self {
        Self {
            v: 1,
            sync_enabled: false,
            epoch: 0,
            scope: ConsentScopeBinding::default(),
        }
    }
}

/// The fence file's name: the consent file's name with `.fence` before its extension
/// (`sync.json` -> `sync.fence.json`; no extension -> `.fence` appended), so it sits beside it.
fn fence_file_name(consent_file: &str) -> String {
    match consent_file.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.contains(['/', '\\']) => {
            format!("{stem}.fence.{ext}")
        }
        _ => format!("{consent_file}.fence"),
    }
}

/// What the consent file holds.
enum ConsentRead {
    /// No file: sync is off and there is no epoch to speak of.
    Missing,
    /// A file that does not decode: an error state, never read as epoch 0.
    Malformed,
    Valid(TranscriptSyncState),
}

/// The state the fence file records. `Revoked` overrides EVERY enabled consent file, whatever its
/// epoch (even one that was unreadable when the revocation was written and is readable later);
/// only `Enabled` with an epoch equal to the consent file's epoch lets an enabled consent count.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum FenceState {
    Revoked,
    Enabled,
}

/// The sync fence file: the revocation fence, independent of the outbox and of the consent file.
/// `{"v":2,"state":"revoked"|"enabled","epoch":<n>}`. A disable writes `revoked`; an enable
/// writes `revoked` with its new epoch first and `enabled` as its LAST durable step. A v1 file
/// (`{"v":1,"epoch":<n>}`, no state) reads as `revoked`: this code has never shipped, so the
/// strict rule applies (unknown state is revoked until an explicit enable rewrites the file).
#[derive(Serialize, Deserialize)]
struct SyncFence {
    v: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    state: Option<FenceState>,
    epoch: u64,
}

/// What the fence file holds.
enum FenceRead {
    /// No file. Untrusted (an enabled consent needs a fence that says `enabled`).
    Missing,
    /// A file that does not decode or has another version: untrusted.
    Malformed,
    Valid(FenceState, u64),
}

struct ConsentStore {
    host: Arc<dyn TranscriptHost>,
    file: String,
    /// The sync fence file name (beside the consent file): see `SyncFence`.
    fence_file: String,
    /// Sticky, in memory only: a disable began and its fence is not durable yet (or its fence
    /// write failed). While set no consent is trusted (`enabled`, every pass admission), the
    /// status is `RevocationIncomplete`, and only a successful write of the fence as `revoked`
    /// (a disable's, or the first step of an enable that no disable has overtaken) clears it.
    revocation_incomplete: AtomicBool,
    /// Sticky, in memory only (rev 68): a refused enable could not confirm that the transport is
    /// stopped (the existing helper's state was unknown). Sync itself stays off (consent is not
    /// trusted); the status says `NotConfirmed("stop_unverified")` while it is set. Cleared by a
    /// confirmed stop and by an enable that took effect. A missing helper is unknown, and is
    /// reported as unknown, never as a verified stop.
    stop_unverified: AtomicBool,
    /// Counts every disable request, bumped by the disable's in-memory front half (under
    /// `stop_gate`). An enable captures it when it is called and gives up, stopping the
    /// transport, if it has moved by the time the enable would take effect: a disable that
    /// arrives during an enable wins.
    disable_requests: AtomicU64,
    /// Held briefly (no I/O under it) by the disable's front half (count, sticky flag, scheduler
    /// stop) and by the enable's final check-and-start, so the two cannot interleave: either the
    /// enable starts before the disable's stop (which then stops it) or it sees the disable and
    /// does not start.
    stop_gate: Mutex<()>,
    /// The highest epoch this process has read from or written to the fence, consent or outbox.
    seen_epoch: AtomicU64,
    /// The deletion outbox file: every consent transition raises its epoch high-water mark and
    /// saves it before the consent file is written (`commit_transition`).
    outbox_file: PathBuf,
    /// The outbox lock, shared with `TranscriptSync`: a transition holds it from the outbox load
    /// to the consent write.
    outbox_lock: Arc<Mutex<()>>,
    account_changed_message: String,
    lock: Mutex<()>,
}

impl ConsentStore {
    fn path(&self) -> PathBuf {
        self.host.data_file(&self.file)
    }

    fn fence_path(&self) -> PathBuf {
        self.host.data_file(&self.fence_file)
    }

    /// One no-follow read capped at `MAX_FENCE_FILE_BYTES`. `Err` only when the file cannot be
    /// read now (an I/O error, a link, over its cap).
    fn load_fence(&self) -> Result<FenceRead, String> {
        match read_capped(&self.fence_path(), MAX_FENCE_FILE_BYTES) {
            Ok(bytes) => Ok(match serde_json::from_slice::<SyncFence>(&bytes) {
                Ok(fence) => {
                    let state = match (fence.v, fence.state) {
                        (1, None) => Some(FenceState::Revoked),
                        (2, Some(state)) => Some(state),
                        _ => None,
                    };
                    match state {
                        Some(state) => {
                            self.seen_epoch.fetch_max(fence.epoch, Ordering::AcqRel);
                            FenceRead::Valid(state, fence.epoch)
                        }
                        None => FenceRead::Malformed,
                    }
                }
                Err(_) => FenceRead::Malformed,
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(FenceRead::Missing),
            Err(error) => Err(format!("read cloud sync fence: {error}")),
        }
    }

    /// Atomic whole-file replacement of the fence.
    fn write_fence(&self, state: FenceState, epoch: u64) -> Result<(), String> {
        let bytes = serde_json::to_vec(&SyncFence {
            v: 2,
            state: Some(state),
            epoch,
        })
        .map_err(|err| format!("serialize cloud sync fence: {err}"))?;
        replace_file_atomically(&self.fence_path(), &bytes)
            .map_err(|err| format!("write cloud sync fence: {err}"))
    }

    /// The trust rule for a VALID consent file that says enabled: no revocation is incomplete in
    /// this process, and the fence file is present, readable, valid, says `enabled` and carries
    /// EXACTLY the consent epoch (not merely at or below it). A fence that says `revoked`, is
    /// missing (beside a consent file or an outbox), unreadable or malformed trusts nothing: an
    /// old enabled consent that was unreadable when a disable was written cannot become trusted
    /// by turning readable later, whatever its epoch. The last part of the rule,
    /// `consent.epoch >= outbox.high` while the outbox is readable, is applied where the outbox is
    /// at hand (`TranscriptSync::trusted_state`, `status_blocking`).
    fn consent_trusted(&self, state: &TranscriptSyncState) -> bool {
        if self.revocation_incomplete.load(Ordering::Acquire) {
            return false;
        }
        matches!(
            self.load_fence(),
            Ok(FenceRead::Valid(FenceState::Enabled, fence)) if state.epoch == fence
        )
    }

    /// `read()` with consent turned off when it is not trusted (`consent_trusted`).
    fn read_trusted(&self) -> TranscriptSyncState {
        let mut state = self.read();
        if state.sync_enabled && !self.consent_trusted(&state) {
            state.sync_enabled = false;
        }
        state
    }

    /// One bounded read. `Err` only when the file cannot be read now (an I/O error, a link, over
    /// its cap): the caller retries and never replaces it.
    fn load(&self) -> Result<ConsentRead, String> {
        match read_capped(&self.path(), MAX_CONSENT_FILE_BYTES) {
            Ok(bytes) => Ok(match serde_json::from_slice(&bytes) {
                Ok(state) => ConsentRead::Valid(state),
                Err(_) => ConsentRead::Malformed,
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(ConsentRead::Missing),
            Err(error) => Err(format!("read cloud sync state: {error}")),
        }
    }

    /// A missing file is the default (sync off, no scope, epoch 0). A file that cannot be read,
    /// is a link, is over its cap or does not decode is an `Err`: a malformed file is an error
    /// state, and its epoch is never taken to be 0.
    fn read_checked(&self) -> Result<TranscriptSyncState, String> {
        match self.load()? {
            ConsentRead::Valid(state) => Ok(state),
            ConsentRead::Missing => Ok(TranscriptSyncState::default()),
            ConsentRead::Malformed => Err("cloud sync state is not valid".to_string()),
        }
    }

    /// For decisions that only ever act on a positive answer: anything `read_checked` refuses
    /// reads as the default, sync off with no scope, so no pass runs and nothing is written.
    fn read(&self) -> TranscriptSyncState {
        self.read_checked().unwrap_or_default()
    }

    /// Take the transition lock. Every read-modify-write of the consent file (a transition) holds
    /// it from the read to the write, so two transitions cannot lose each other's epoch bump.
    /// Readers never take it: the file is replaced atomically. Lock order: this lock is taken
    /// first and the outbox lock (`commit_transition`) second; no code holds the outbox lock
    /// and then waits for this one (every outbox-lock holder only READS the consent file), and
    /// nothing that waits on the sync lock or the engine lock runs while either is held.
    fn transition_guard(&self) -> Result<std::sync::MutexGuard<'_, ()>, String> {
        self.lock
            .lock()
            .map_err(|_| "cloud sync scope state lock poisoned".to_string())
    }

    /// The next epoch: one above both the consent file's own epoch and `floor`, the outbox's
    /// high-water mark, so the number is never one an older stamp already used even when the
    /// consent file was lost, replaced or rolled back. A state whose epoch cannot grow refuses
    /// the transition.
    fn bump(state: &mut TranscriptSyncState, floor: u64) -> Result<(), String> {
        state.epoch = state
            .epoch
            .max(floor)
            .checked_add(1)
            .ok_or_else(|| "cloud sync consent epoch exhausted".to_string())?;
        Ok(())
    }

    /// The one way a consent transition becomes durable. The caller holds `transition_guard` and
    /// has changed `state` (not yet its epoch). Under the outbox lock the new epoch is one above
    /// every epoch seen (the consent file's own, the outbox's high-water mark, the fence's and
    /// this process's), and the three files are written in this order:
    ///
    /// 1. the SYNC FENCE file (its own file, independent of the outbox) with the new epoch;
    /// 2. the outbox's high-water mark raised to it and the outbox saved (nothing else in the
    ///    outbox is touched);
    /// 3. the consent file.
    ///
    /// Once the fence says `revoked`, no consent file is trusted as enabled, whatever its epoch
    /// (an old enabled consent that was unreadable at disable time and readable later included);
    /// only `finish_enable` (fence `enabled`, same epoch as the consent file) ends that.
    ///
    /// Enable and account change (`revoke == false`) stop at the first failure and return it; a
    /// failure after the fence leaves consent untrusted (fail closed) until the next transition.
    ///
    /// Disable (`revoke == true`) always attempts the independent fence, including after a
    /// poisoned outbox lock or exhausted epoch. A poisoned or unreadable outbox stays untouched;
    /// an exhausted epoch skips outbox & consent writes without issuing another consent epoch. `write_consent == false`
    /// (the consent file cannot be read, so it is never replaced) skips step 3. It returns
    /// `Err` only when the fence could not be written (the revocation is then incomplete and
    /// `revocation_incomplete` stays set); otherwise `Ok` with one line per failed secondary
    /// write (the fence already excludes the old consent, so none of them permits a replay).
    ///
    /// Both kinds write the fence with state `revoked`; the transition never writes `enabled`.
    /// The caller of an enable (`revoke == false`) finishes with `finish_enable`, the LAST durable
    /// step, only after its own final checks. A crash or failure before that leaves `revoked`:
    /// sync stays off. A successful `revoked` write clears `revocation_incomplete` for a disable
    /// (the revocation is durable); for an enable only when no disable was requested since
    /// `started` (the disable-request count the enable captured).
    fn commit_transition(
        &self,
        state: &mut TranscriptSyncState,
        revoke: bool,
        write_consent: bool,
        started: u64,
    ) -> Result<Vec<String>, String> {
        // Revocation must reach its independent fence even if the outbox mutex was poisoned.
        // Never recover or touch the potentially inconsistent outbox in that case. Enables
        // still refuse: they cannot safely issue an epoch without its high-water mark.
        let mut issues: Vec<String> = Vec::new();
        let outbox_guard = match self.outbox_lock.lock() {
            Ok(guard) => Some(guard),
            Err(_) if revoke => {
                issues.push("outbox left untouched: deletion outbox lock poisoned".to_string());
                None
            }
            Err(_) => return Err("deletion outbox lock poisoned".to_string()),
        };
        let mut outbox = if outbox_guard.is_some() {
            match Outbox::load(&self.outbox_file) {
                Ok(outbox) => Some(outbox),
                Err(error) if revoke => {
                    issues.push(format!("outbox left untouched: {error}"));
                    None
                }
                Err(error) => return Err(error),
            }
        } else {
            None
        };
        let fence_floor = match self.load_fence() {
            Ok(FenceRead::Valid(_, epoch)) => epoch,
            _ => 0,
        };
        let floor = outbox
            .as_ref()
            .map_or(0, Outbox::high_water)
            .max(fence_floor)
            .max(self.seen_epoch.load(Ordering::Acquire));
        let epoch_advanced = match Self::bump(state, floor) {
            Ok(()) => true,
            Err(error) if revoke => {
                // A revoked fence excludes EVERY enabled consent regardless of epoch. Reuse
                // only its revocation marker; never issue a consent epoch or rewrite outbox
                // state when the monotonic counter is exhausted. A later enable still refuses.
                state.epoch = state.epoch.max(floor);
                issues.push(error);
                false
            }
            Err(error) => return Err(error),
        };
        // 1. The fence, first, as `revoked` (even if the epoch could not advance). Normally
        // its epoch is one above the
        // consent file's (when readable), the fence's, the outbox's and this process's: when
        // none is readable the state alone carries the revocation.
        let fence_error = match self.write_fence(FenceState::Revoked, state.epoch) {
            Ok(()) => {
                self.seen_epoch.fetch_max(state.epoch, Ordering::AcqRel);
                if revoke {
                    self.revocation_incomplete.store(false, Ordering::Release);
                } else {
                    let _gate = self.stop_gate.lock().unwrap_or_else(|e| e.into_inner());
                    if self.disable_requests.load(Ordering::Acquire) == started {
                        self.revocation_incomplete.store(false, Ordering::Release);
                    }
                }
                None
            }
            Err(error) if revoke => Some(error),
            Err(error) => return Err(error),
        };
        // 2. The outbox's high-water mark (nothing else in the outbox changes).
        if let Some(outbox) = outbox.as_mut().filter(|_| epoch_advanced) {
            outbox.note_epoch(state.epoch);
            if let Err(error) = outbox.save(&self.outbox_file) {
                if revoke {
                    issues.push(error);
                } else {
                    return Err(error);
                }
            }
        }
        // 3. The consent file.
        if write_consent && epoch_advanced {
            if let Err(error) = self.write_locked(state) {
                if revoke {
                    issues.push(error);
                } else {
                    return Err(error);
                }
            }
        }
        match fence_error {
            Some(error) => {
                issues.insert(0, format!("durable revocation incomplete: {error}"));
                Err(issues.join("; "))
            }
            None => Ok(issues),
        }
    }

    /// The LAST durable step of an enable (or of an account change that keeps sync on): the fence
    /// says `enabled` with the epoch the transition wrote. The caller holds `transition_guard`.
    fn finish_enable(&self, epoch: u64) -> Result<(), String> {
        self.write_fence(FenceState::Enabled, epoch)?;
        self.seen_epoch.fetch_max(epoch, Ordering::AcqRel);
        Ok(())
    }

    /// A transition that keeps sync on (an account change): `commit_transition`, then
    /// `finish_enable` unless a disable was requested meanwhile (then the fence stays `revoked`).
    fn commit_enabled_transition(&self, state: &mut TranscriptSyncState) -> Result<(), String> {
        let started = self.disable_requests.load(Ordering::Acquire);
        self.commit_transition(state, false, true, started)?;
        if self.disable_requests.load(Ordering::Acquire) != started {
            return Err("cloud sync was turned off during the change".to_string());
        }
        self.finish_enable(state.epoch)
    }

    /// Write the consent file. The caller holds `transition_guard` (and, for a transition, the
    /// outbox lock: `commit_transition`).
    fn write_locked(&self, state: &TranscriptSyncState) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(state)
            .map_err(|err| format!("serialize cloud sync state: {err}"))?;
        replace_file_atomically(&self.path(), &bytes)
            .map_err(|err| format!("commit cloud sync state: {err}"))
    }

    fn enabled(&self) -> bool {
        self.read_trusted().sync_enabled
    }

    fn authorized_scope(&self) -> Option<String> {
        self.read().scope.authorized_scope
    }

    /// The engine binds its ledger to an account but carries no consent: on a scope change it
    /// would reset its ledger and publish local history into the NEW account's zone. This gate
    /// stays in front of the engine: a different live account is refused until the user turns
    /// sync on again (`explicit`), which re-authorizes the current account. (The engine's own
    /// `require_consent_on_scope_change` pause is not used.) `acknowledged` is evaluated only
    /// after the disabled check; an `Err` from it refuses the account.
    fn authorize_live_scope(
        &self,
        actual: &str,
        explicit: bool,
        acknowledged: &dyn Fn(&ConsentScopeBinding) -> Result<Option<String>, String>,
    ) -> Result<(), String> {
        let _guard = self.transition_guard()?;
        let mut state = self.read();
        if !explicit && !(state.sync_enabled && self.consent_trusted(&state)) {
            return Err("cloud sync is disabled".to_string());
        }
        let before = state.clone();
        let acknowledged = acknowledged(&state.scope)?;
        let outcome = sync_core::authorize_scope(
            &mut state.scope.authorized_scope,
            &mut state.scope.legacy_scope,
            acknowledged.as_deref(),
            actual,
            explicit,
            &self.account_changed_message,
        );
        if state != before {
            // A change of the authorized account is a consent transition: the outbox mark moves
            // first and the epoch in the same consent write (`commit_transition`).
            if state.scope.authorized_scope != before.scope.authorized_scope {
                self.commit_enabled_transition(&mut state)?;
            } else {
                self.write_locked(&state)?;
            }
        }
        outcome
    }
}

/// Engine auth: consent switch + transport account state + consent-authorized account. Its
/// generation also moves with the scheduler's consent epoch, so toggling consent cancels an
/// in-flight pass.
struct ConsentAuth {
    inner: Arc<dyn AuthProvider>,
    consent: Arc<ConsentStore>,
    paths: Arc<StatePaths>,
    scheduler: Arc<Scheduler>,
    scope_unavailable_message: String,
}

impl AuthProvider for ConsentAuth {
    fn enabled(&self) -> bool {
        self.consent.enabled() && self.inner.enabled()
    }

    fn availability(&self) -> SyncResult<()> {
        self.inner.availability()?;
        let live = self
            .inner
            .account_scope()
            .ok_or_else(|| SyncError::Unavailable(self.scope_unavailable_message.clone()))?;
        self.consent
            .authorize_live_scope(&live, false, &|binding| self.paths.anchor_for(binding))
            .map_err(SyncError::Unavailable)
    }

    fn generation(&self) -> u64 {
        self.inner
            .generation()
            .wrapping_add(self.scheduler.consent_generation())
    }

    fn account_scope(&self) -> Option<String> {
        self.inner.account_scope()
    }
}

// ---------------------------------------------------------------------------
// Pass helpers
// ---------------------------------------------------------------------------

/// How one automatic pass turn ended.
pub(crate) enum PassTurn {
    /// The turn ran (or was skipped: consent off, host busy, stale on completion): the same
    /// meaning as the `Option<PassReport>` of [`TranscriptSync::pass`].
    Done(Option<PassReport>),
    /// Admission was closed when the turn got `sync_lock` (a pass that ran while it waited
    /// installed `not_before`). Nothing ran, nothing was cleared and no engine work was done;
    /// the scheduler requeues the work and waits the remaining time.
    NotBefore(Duration),
}

/// What `prepare_pass_outbox` decided for one pass.
struct PassOutbox {
    /// Ids the ledger queues without current-epoch hand-over evidence: already retained in the
    /// outbox's unverified partition; the pass withdraws them from the ledger before the engine
    /// runs. Of these, `lift` are the ids the hand-over record proves had no tombstone before
    /// (the withdrawal lifts their tombstone) and `keep` all the others (ambiguous legacy
    /// evidence: the withdrawal keeps their tombstone; they are marked unproven in the outbox).
    stale: Vec<String>,
    lift: Vec<String>,
    keep: Vec<String>,
    /// The eligible ids marked unproven: their ledger tombstone is not confirmed.
    unproven: BTreeSet<String>,
    /// Deletions that may be handed to the engine now (the live account's partition in the
    /// durable epoch).
    eligible: BTreeSet<String>,
    /// The ids of that partition: pulled copies of them are held back.
    held: BTreeSet<String>,
    /// The durable consent epoch `eligible` was read in; `None` when nothing may be handed over.
    epoch: Option<u64>,
}

/// Reports the engine's `uploads_deferred` event to the instance (the engine had more uploads
/// than one pass keeps) and passes every event on to the transport's observer.
struct PassObserver {
    inner: Arc<dyn SyncObserver>,
    deferred_uploads: Arc<AtomicUsize>,
}

impl SyncObserver for PassObserver {
    fn event(&self, name: &'static str, detail: &str) {
        if name == "uploads_deferred" {
            if let Ok(count) = detail.trim().parse::<usize>() {
                self.deferred_uploads.store(count, Ordering::Release);
            }
        }
        self.inner.event(name, detail);
    }
}

/// Clears the account the ledger guard expects when the pass ends, however it ends.
struct ExpectedScope<'a>(&'a LedgerGuard);

impl Drop for ExpectedScope<'_> {
    fn drop(&mut self) {
        self.0.set_expected(None);
    }
}

/// What `settle_outbox` found after it changed the outbox.
struct Settled {
    /// Deletions waiting for the account.
    waiting: usize,
    /// Unverified deletions moved into the account's partition by proof.
    promoted: usize,
    /// Deletions still unverified.
    unverified: usize,
    /// Deletions that belong to other accounts.
    other_accounts: usize,
}

/// The deletions the outbox keeps, as counts. None of them is deleted silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeletionBacklog {
    /// Deletions whose account was never established (version 1 entries, deletions made while
    /// sync was off or while another account was live). They are never sent unless proof is found
    /// that the authorized account holds the record.
    pub unverified: usize,
    /// Deletions made under another account. They wait until that account is authorized again.
    pub other_accounts: usize,
    /// Deletions waiting for the authorized account.
    pub waiting: usize,
}

// ---------------------------------------------------------------------------
// Public result types
// ---------------------------------------------------------------------------

/// Account and sync state for the settings screen. `Ready` is the only success state, and
/// `TranscriptSync::status` reports it only when all of these hold at the time of the call:
/// consent is on and authorizes account A; the transport says the account is available; the live
/// account is A; the ledger file is bound to A; the last pass of this consent epoch ended
/// `Synced` with the ledger bound to A and had nothing left over for a following pass. Every other
/// answer names what is missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusReport {
    /// The user has not opted in on this device (the default).
    Disabled,
    /// Success: every condition above holds.
    Ready,
    /// Sync is stopped (no pass runs, the transport is off), but turning it off could not be made
    /// durable: the sync fence file could not be written. Nothing on disk records the
    /// revocation until a later write of the fence succeeds. Restarting can restore the earlier
    /// setting (sync on), and the host must say so. The user retries saving OFF by calling
    /// `set_enabled(false)` again when storage recovers. Not a success state.
    RevocationIncomplete,
    /// A pass is running (the engine lock is held). Not a success state: it says nothing about the
    /// account, because asking the helper would queue behind the pass.
    Syncing,
    /// The account and the ledger are confirmed and the last pass ended `Synced`, but it did not
    /// reach the end of the work: the remote change feed was cut at the size one pass holds, more
    /// local rows were left to upload than one pass holds, or both. Another pass follows at once;
    /// the history is not dropped, it is being processed across passes. Not a success state. The
    /// counts are of this run of passes since it began (it begins at the first pass that leaves
    /// work over and ends at the first pass that does not, or when consent or the account
    /// changes): `passes` ran, they downloaded `downloaded` and uploaded `uploaded` records.
    Partial {
        passes: u32,
        downloaded: u64,
        uploaded: u64,
    },
    /// Like `Partial`, but the immediate reschedule hit its own bound
    /// (`FEED_MAX_IMMEDIATE_PARTIAL_PASSES` immediate passes in a row, whatever the cursors and
    /// counts) and the host is waiting out one backoff (`FEED_BACKOFF_BASE_SECS`) before the next
    /// pass. `reason` is `pass_budget`. Work continues later: this is not a feed failure, the
    /// recovery stays unfinished, and it is never `Ready`. The counts are of the run so far, as
    /// for `Partial`. It clears when a later pass leaves nothing over (`Ready`), or by the next
    /// `Partial`/`Paused`/`FeedStalled` answer a later pass gives.
    Paused {
        reason: &'static str,
        passes: u32,
        downloaded: u64,
        uploaded: u64,
    },
    /// The account and the ledger are confirmed, but the remote change feed cannot be read to its
    /// end: the last pass met a page that says more is coming without a new cursor (`stalled`), the
    /// feed's cursors went round across passes (`cycle`), or `FEED_MAX_CUT_PASSES_WITHOUT_PROGRESS`
    /// cut passes in a row applied nothing new (`no_progress`). Failure state, never success:
    /// the unfinished state is kept (a recovery replay stays pending, the cursor is not moved past
    /// unapplied pages, `delete_everywhere` reports `Incomplete("feed_stalled")`), automatic retries
    /// run only with backoff (60 s doubling to 3,600 s). The counts are of the run of passes so
    /// far, as for `Partial`. It clears at the first pass that makes real progress or reaches the
    /// end of the feed, or when consent changes.
    FeedStalled {
        reason: &'static str,
        passes: u32,
        downloaded: u64,
        uploaded: u64,
    },
    /// The live account is not the one consent was given for. The text is
    /// `TranscriptSyncConfig::account_changed_message`. Sync stays stopped until the user turns it
    /// on again.
    AccountChanged(String),
    /// Sync is on, but the evidence for `Ready` is incomplete or could not be checked. The tag is
    /// one of:
    /// * `consent_unreadable`: the consent file cannot be read (over its cap, a link, an I/O
    ///   error).
    /// * `not_authorized`: consent is on but holds no authorized account.
    /// * `engine_unavailable`: an internal lock is poisoned.
    /// * `account_unknown`: the transport gave no account scope.
    /// * `ledger_unreadable`: the ledger file cannot be read, is over its cap or does not decode.
    /// * `ledger_unbound`: no ledger file yet, or it is bound to another account.
    /// * `no_pass_yet`: no pass has ended in this consent epoch.
    /// * `last_pass_not_synced`: the last pass failed, was incomplete, or ended with the ledger
    ///   bound to another account.
    /// * `check_timed_out`: the account check did not answer in time.
    NotConfirmed(&'static str),
    /// The transport's own detail (see [`SyncTransport`]), path-like tokens removed: the account
    /// is signed out, restricted or the helper is missing. No user content.
    Unavailable(String),
}

/// What [`TranscriptSync::delete_everywhere`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteOutcome {
    /// There was nothing to delete: no local row, and the ledger bound to the authorized account
    /// lists no remote record.
    Nothing,
    /// Every one of this many ids (local rows and the remote records the ledger of the authorized
    /// account lists) was tombstoned, the pass ended `Synced`, had nothing left over and the
    /// outbox holds none of them.
    Synced(u32),
    /// The pass ended in the state named by the tag (`pending`, `failed`, ...), or the tag is
    /// `queued`: the pass was the one that bound this account, or it handed over only as many
    /// deletions as one pass takes, so the deletions wait in the outbox for the next pass, which
    /// the scheduler runs; they stay in the outbox until the remote has acknowledged them. Or the
    /// tag is `partial`: the pass left work over and another follows. Or the tag is
    /// `zone_not_listed`: the ledger was not bound to the authorized account when the ids were
    /// listed, so records only the remote holds were not among them; run it again. Or the tag is
    /// `feed_stalled`: the remote change feed could not be read to its end (see
    /// `StatusReport::FeedStalled`), so nothing is claimed done. Or the tag is `paused`: the pass
    /// hit the immediate-reschedule bound (`StatusReport::Paused`); work continues after one
    /// backoff. Or the operation is bound to one captured account and consent epoch and aborted:
    /// `account_changed` (the live account at entry is not the consented one), `account_unconfirmed`
    /// (no positive observation of the live account), `scope_changed` (the account or the consent
    /// epoch changed after the capture, or the live account no longer matched at a re-check).
    /// None of these three performs a scoped write after the mismatch was seen.
    Incomplete(&'static str),
    /// A recording owns the history boundary, or consent changed during the pass. Queued.
    Deferred,
}

/// The ids `delete_everywhere` acts on.
struct KnownIds {
    ids: Vec<String>,
    /// The ledger was bound to the authorized account, so its remote ids are in `ids`.
    zone_listed: bool,
}

fn outcome_tag(outcome: &PassOutcome) -> &'static str {
    match outcome {
        PassOutcome::Synced => "synced",
        PassOutcome::Disabled => "disabled",
        PassOutcome::Pending(_) => "pending",
        PassOutcome::Deferred { .. } => "deferred",
        PassOutcome::Cancelled => "cancelled",
        PassOutcome::ConsentRequired(_) => "consent_required",
        PassOutcome::Unavailable(_) => "unavailable",
        PassOutcome::Failed(_) => "failed",
    }
}

fn pass_summary(report: &PassReport) -> String {
    let retry = match report.retry_after_secs {
        Some(secs) if secs.is_finite() => format!("{secs:.0}"),
        _ => "none".to_string(),
    };
    format!(
        "outcome={} uploaded={} downloaded={} tombstones_applied={} tombstones_written={} quarantined={} blocked_reuploads={} retry_after_secs={retry}",
        outcome_tag(&report.outcome),
        report.uploaded,
        report.downloaded,
        report.tombstones_applied,
        report.tombstones_written,
        report.quarantined,
        report.blocked_reuploads,
    )
}

// ---------------------------------------------------------------------------
// The host
// ---------------------------------------------------------------------------

/// One transcript sync instance. Build it once per process with [`TranscriptSync::new`]; every
/// piece of state (engine, locks, scheduler, consent epoch) belongs to the instance, so a late
/// callback holding an old instance can never act on a newer one.
pub struct TranscriptSync {
    profile: TranscriptSyncProfile,
    local_source: String,
    max_history_rows: usize,
    event_sink: fn(&str, &str),
    scope_unavailable_message: String,
    host: Arc<dyn TranscriptHost>,
    transport: Arc<dyn SyncTransport>,
    paths: Arc<StatePaths>,
    consent: Arc<ConsentStore>,
    pub(crate) scheduler: Arc<Scheduler>,
    engine: Mutex<Option<Hosted>>,
    /// The ledger guard: the one place the ledger file is compared with this process's view and
    /// written through (see `ledger_guard.rs`).
    guard: Arc<LedgerGuard>,
    /// How much of the remote change feed the pass in flight has taken (`remote_window.rs`).
    window: Arc<RemoteWindow>,
    /// Uploads the engine left for a later pass, from its `uploads_deferred` event.
    deferred_uploads: Arc<AtomicUsize>,
    sync_lock: tokio::sync::Mutex<()>,
    outbox_lock: Arc<Mutex<()>>,
    /// Cursors and streaks across passes (`FeedWatch`); in memory only.
    feed_watch: Mutex<FeedWatch>,
    /// The monotonic instant before which no AUTOMATIC pass is admitted (`admission_wait`). Set
    /// (never shortened) by any pass, foreground or scheduled, that ends with a feed halt or the
    /// pass-budget pause. A pass starting does not clear it. Cleared only when it has expired at
    /// an admission, or by an explicit user action (`reset_feed_watch`). In memory only.
    not_before: Mutex<Option<Instant>>,
    /// Shared with the history adapter; see `HistoryLocalStore::suppressed`.
    suppressed: Arc<Mutex<BTreeSet<String>>>,
    /// How the last pass ended; the evidence `status` needs for `Ready`. Cleared on every consent
    /// change.
    last_pass: Mutex<Option<PassRecord>>,
    /// The run of passes that each left work over; reset by the first pass that does not.
    run_progress: Mutex<RunProgress>,
    /// The name of the full replay a history recovery asked for, kept until that recovery has
    /// completed, so a replay that takes several passes continues instead of restarting.
    recovery_flag: Mutex<Option<String>>,
}

/// The engine and the history adapter it reads, kept together so the host can start and end the
/// adapter's per-pass state.
struct Hosted {
    engine: SyncEngine<TranscriptMapper>,
    local: Arc<HistoryLocalStore>,
}

/// Counts of a run of passes that each left work over (`StatusReport::Partial`).
#[derive(Debug, Clone, Default)]
struct RunProgress {
    passes: u32,
    downloaded: u64,
    uploaded: u64,
}

/// What the account read before a pass established.
enum LiveAccount {
    /// The live account is the one consent authorizes and the gate accepted it.
    Authorized(String),
    /// The live account is a different one than consent authorizes.
    Changed,
    /// The account could not be established: transport error, no scope, no consent, or an
    /// unreadable anchor.
    Unconfirmed,
}

impl LiveAccount {
    fn tag(&self) -> &'static str {
        match self {
            Self::Authorized(_) => "authorized",
            Self::Changed => "changed",
            Self::Unconfirmed => "unconfirmed",
        }
    }
}

/// How one engine pass ended.
#[derive(Debug, Clone)]
struct PassRecord {
    /// The account the ledger was bound to when the pass ended; `None` when the pass failed before
    /// the engine ran or the ledger was unbound.
    ledger_scope: Option<String>,
    /// The pass outcome was `Synced`.
    synced: bool,
    /// The pass ended `Synced` but left work over for the next one (the run so far).
    partial: Option<RunProgress>,
    /// The consent epoch the pass started in.
    consent_generation: u64,
    /// The pass ended `Synced` but the feed could not be read to its end (reason tag).
    halt: Option<&'static str>,
    /// The pass ended `Synced` and left work over, but the immediate-reschedule limit
    /// (`FEED_MAX_IMMEDIATE_PARTIAL_PASSES`) stopped the immediate next pass: work continues after
    /// the backoff wait (reason tag `pass_budget`).
    paused: Option<&'static str>,
}

/// What one engine pass returns to `run_engine_pass`.
struct PassResult {
    /// `Some("pass_budget")` when the immediate-reschedule limit was taken after this pass.
    paused: Option<&'static str>,
    /// The feed-halt reason (`stalled`, `cycle`, `no_progress`) of a Synced pass, else `None`.
    halt: Option<&'static str>,
    report: PassReport,
    /// The account the ledger was bound to when the pass ended.
    ledger_scope: Option<String>,
    /// The pass left work over: the remote feed was cut, or uploads were left, or deletions
    /// remain that a following pass hands over.
    partial: bool,
}

impl TranscriptSync {
    /// Validate `config` and build the instance. Nothing runs and no process starts until
    /// [`start`](Self::start) or the first pass.
    pub fn new(
        config: TranscriptSyncConfig,
        host: Arc<dyn TranscriptHost>,
        transport: Arc<dyn SyncTransport>,
    ) -> Result<Arc<Self>, String> {
        config.validate()?;
        let TranscriptSyncConfig {
            profile,
            consent_file,
            local_source,
            account_changed_message,
            scope_unavailable_message,
            max_history_rows,
            scheduler,
            event_sink,
            spawn,
        } = config;
        let paths = Arc::new(StatePaths {
            host: Arc::clone(&host),
            transport: Arc::clone(&transport),
            files: profile.files.clone(),
        });
        let outbox_lock = Arc::new(Mutex::new(()));
        let consent = Arc::new(ConsentStore {
            host: Arc::clone(&host),
            fence_file: fence_file_name(&consent_file),
            revocation_incomplete: AtomicBool::new(false),
            stop_unverified: AtomicBool::new(false),
            disable_requests: AtomicU64::new(0),
            stop_gate: Mutex::new(()),
            seen_epoch: AtomicU64::new(0),
            file: consent_file,
            outbox_file: paths.env(&profile.files.outbox),
            outbox_lock: Arc::clone(&outbox_lock),
            account_changed_message,
            lock: Mutex::new(()),
        });
        let guard = Arc::new(LedgerGuard::new(paths.env(&profile.files.ledger)));
        Ok(Arc::new(Self {
            profile,
            local_source,
            max_history_rows,
            event_sink,
            scope_unavailable_message,
            host,
            transport,
            paths,
            consent,
            scheduler: Arc::new(Scheduler::new(scheduler, spawn)),
            engine: Mutex::new(None),
            guard,
            window: Arc::new(RemoteWindow::new()),
            deferred_uploads: Arc::new(AtomicUsize::new(0)),
            sync_lock: tokio::sync::Mutex::new(()),
            outbox_lock,
            feed_watch: Mutex::new(FeedWatch::default()),
            not_before: Mutex::new(None),
            suppressed: Arc::new(Mutex::new(BTreeSet::new())),
            last_pass: Mutex::new(None),
            run_progress: Mutex::new(RunProgress::default()),
            recovery_flag: Mutex::new(None),
        }))
    }

    // ----- small accessors used by the scheduler and by the host -----

    pub(crate) fn emit(&self, name: &str, detail: &str) {
        (self.event_sink)(name, &super::safe_detail(detail));
    }

    pub(crate) fn busy_reason(&self) -> Option<&'static str> {
        self.host.busy_reason()
    }

    pub(crate) fn native_observer(&self) -> Option<Arc<dyn NativeObserver>> {
        self.transport.native_observer()
    }

    pub(crate) fn notify_downloaded(&self, count: u32) {
        self.host.downloaded(count);
    }

    /// Whether the user has opted in on this device. Reads the consent file (one bounded read).
    pub fn sync_enabled(&self) -> bool {
        self.consent.enabled()
    }

    /// The account scope the consent was authorized for.
    pub fn authorized_scope(&self) -> Option<String> {
        self.consent.authorized_scope()
    }

    /// Whether a consent file exists, i.e. the user has made a choice either way.
    pub fn has_local_choice(&self) -> bool {
        self.consent.path().exists()
    }

    /// The in-process consent epoch: moves on every enable, disable and stop. The DURABLE epoch
    /// (consent file) is separate and fences the outbox.
    pub fn consent_generation(&self) -> u64 {
        self.scheduler.consent_generation()
    }

    /// Durable local work exists. Any thread; does not touch the disk or any lock but a latch.
    pub fn notify_local_change(&self) {
        self.scheduler.request(PUSH);
    }

    /// Remote work may exist (native event, history recovery).
    pub fn notify_remote_change(&self) {
        self.scheduler.request(PULL);
    }

    pub(crate) fn notify_reconcile(&self) {
        self.scheduler.request(PUSH | PULL);
    }

    /// Wake a deferred cycle after the host's activity gate ended.
    pub fn wake(&self) {
        self.scheduler.wake();
    }

    /// Start the event-driven coordinator. Startup, local durable changes, native notifications
    /// and lifecycle recovery all wake the one coordinator. Calling it again requests a
    /// reconcile. Needs the host runtime to be running (see `Spawner`).
    pub fn start(self: &Arc<Self>) {
        self.scheduler.start(self);
    }

    /// Stop the coordinator and disable the transport so an in-flight pass cancels itself.
    /// Non-blocking: it does not wait for a pass (a pass can be inside a helper call for up to
    /// the transport's request timeout) and does not touch the consent file. Idempotent.
    pub fn shutdown(&self) {
        self.scheduler.stop();
        // Not verifiable here (no transition lock, non-blocking): a failed stop request is
        // recorded so the status never reads as a verified stop.
        if self.transport.set_enabled(false).is_err() {
            self.consent.stop_unverified.store(true, Ordering::Release);
        }
        if let Some(native) = self.transport.native_observer() {
            native.set_enabled(false);
        }
    }

    // ----- deletion outbox -----
    //
    // The outbox file (`outbox.rs`) is the durable owner of every local deletion until the remote
    // acknowledged it. The engine ledger only carries the active account's in-flight copy.
    //
    // Write order for one deletion, and what a crash leaves behind:
    //   1. `enqueue_deleted_ids`: outbox file replaced atomically with the entry, under the
    //      account the deletion provably belongs to, or in the unverified partition. Crash
    //      before: the delete command failed and the caller saw `Err`; after: the entry is
    //      durable.
    //   2a. pass start, before anything is handed over: every id the ledger already queues
    //      without current-epoch hand-over evidence is saved into the outbox's unverified
    //      partition (one save), then withdrawn from the ledger (`withdraw_pending_tombstones`).
    //      Crash between: the next pass finds the id still queued and unproven, and repeats.
    //   2. pass, only when the ledger on disk is bound to the live consented account: an id the
    //      ledger already holds as buried and confirmed is not queued (nothing to send; it is
    //      acknowledged in step 4); for the rest the hand-over record (account, epoch, ids, and
    //      which of them had no tombstone: `fresh`) is saved into the outbox file, THEN one
    //      `engine.note_deleted_many` writes the ledger (tombstones + pending) through the
    //      ledger guard. Crash between: a record without a queued id (dropped by the next pass);
    //      crash after: entry in both files and the record names it with the bit it had; the
    //      next pass repeats this step (idempotent).
    //   3. engine: tombstone record saved to the remote, then the ledger (bound to the same scope)
    //      drops the id from `pending_tombstones` and is written durably. This is the
    //      acknowledgement. Crash between the two: the next pass saves the same tombstone record
    //      again (an overwrite of the same record name, so harmless on the remote).
    //   4. `settle_outbox`: outbox file replaced atomically without the acknowledged entries.
    //      Crash before: the entry is still there, step 2 repeats and step 3 saves the tombstone
    //      once more. Nothing is ever removed before step 3 is durable.
    // A consent transition (`ConsentStore::commit_transition`) writes in this order, holding the
    // consent transition lock and then the outbox lock: (T0) the sync fence file as `revoked`
    // with the new epoch; (T1) the outbox file with its high-water mark raised to that epoch;
    // (T2) the consent file; an enable then adds (T3) the fence as `enabled`, its last durable
    // step. A disable first stops the scheduler and transport in memory and then attempts T0 to
    // T2 whatever any of them does; an enable stops at the first failure. Crash or failure
    // before T3: the fence says `revoked`, so no consent is trusted (no hand-over, no stamp,
    // status `consent_untrusted`) until the next enable. If T0 failed on a disable:
    // `revocation_incomplete` (sticky, status `RevocationIncomplete`). See
    // HANDOFF.md, item 12 of "Deletion outbox", for the full failure table and the residual.
    // An unverified entry is never handed to the engine; `settle_outbox` moves it into the
    // account's partition only with proof (the id is a remote version of the ledger bound to that
    // account), and step 2 then applies to it.

    fn outbox_path(&self) -> PathBuf {
        self.paths.env(&self.profile.files.outbox)
    }

    /// Whether sync is on, the consent-authorized account is exactly `scope` and the consent epoch
    /// is exactly `generation`. Reads the consent file and the scheduler's epoch now; the caller
    /// supplies the `scope` and `generation` it captured, so a changed account or epoch is a
    /// `false`, never silently adopted. The consent file's account alone never proves the live
    /// account: callers pair this with an observation (`observe_live_scope`) made for the
    /// operation.
    fn scope_holds(&self, scope: &str, generation: u64) -> bool {
        let state = self.consent.read_trusted();
        state.sync_enabled
            && state.scope.authorized_scope.as_deref() == Some(scope)
            && self.scheduler.consent_generation() == generation
    }

    /// Observe the live account NOW and check the captured `scope` and `generation` against it
    /// and against consent. `true` only when the observation is `Authorized(scope)` and the
    /// token holds before and after the observation. Blocking (one transport status read).
    fn reobserve_holds(&self, scope: &str, generation: u64) -> bool {
        self.scope_holds(scope, generation)
            && matches!(self.observe_live_scope(), LiveAccount::Authorized(live) if live == scope)
            && self.scope_holds(scope, generation)
    }

    /// The token for one `delete_everywhere`: the epoch is read BEFORE the observation, so an
    /// epoch change during it fails the final check. Only a positive observation of the live
    /// account made now, equal to the consent-authorized account, yields a token. Blocking.
    fn capture_delete_token(&self) -> Result<DeleteToken, DeleteOutcome> {
        let generation = self.scheduler.consent_generation();
        let epoch = self.consent.read().epoch;
        match self.observe_live_scope() {
            LiveAccount::Authorized(scope) if self.scope_holds(&scope, generation) => {
                Ok(DeleteToken {
                    scope,
                    generation,
                    epoch,
                })
            }
            LiveAccount::Authorized(_) => Err(DeleteOutcome::Incomplete("scope_changed")),
            LiveAccount::Changed => Err(DeleteOutcome::Incomplete("account_changed")),
            LiveAccount::Unconfirmed => Err(DeleteOutcome::Incomplete("account_unconfirmed")),
        }
    }

    /// Admission rule for every AUTOMATIC pass (the scheduler's: scheduled, retry, remote-event
    /// wake, local-change wake, status wake). `Some(remaining)` means do not run a pass now and
    /// re-arm for `remaining`; `None` means admitted. An expired instant is cleared here, at the
    /// admission that lets a pass run. Never cleared by a pass starting. Monotonic clock.
    pub(crate) fn admission_wait(&self) -> Option<Duration> {
        let mut slot = self
            .not_before
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let until = (*slot)?;
        match until.checked_duration_since(Instant::now()) {
            Some(remaining) if !remaining.is_zero() => Some(remaining),
            _ => {
                *slot = None;
                None
            }
        }
    }

    /// Close admission to automatic passes for `wait` from now. The later instant wins, so a
    /// pause is never shortened by a later one; an instant that cannot be represented takes the
    /// longest backoff.
    fn set_not_before(&self, wait: Duration) {
        let now = Instant::now();
        let target = now
            .checked_add(wait)
            .or_else(|| now.checked_add(Duration::from_secs(FEED_BACKOFF_MAX_SECS)))
            .unwrap_or(now);
        let mut slot = self
            .not_before
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if slot.map_or(true, |held| target > held) {
            *slot = Some(target);
        }
    }

    /// An explicit user action (consent change, delete everywhere) forgets the cursor record and
    /// the streaks, so the next pass starts with the full immediate-retry allowance.
    /// It also clears `not_before` (these are the explicit entry points) and wakes the scheduler
    /// so a waiting automatic pass re-checks admission now.
    fn reset_feed_watch(&self) {
        if let Ok(mut watch) = self.feed_watch.lock() {
            *watch = FeedWatch::default();
        }
        *self
            .not_before
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        self.scheduler.wake();
    }

    /// Queue local deletions for propagation under the account they provably belong to, or as
    /// unverified. Always persisted, even with sync off, so a later opt-in can still tombstone
    /// them. Never takes the engine lock (callers are history delete paths). `Err` means nothing
    /// was queued.
    pub fn enqueue_deleted_ids(&self, ids: &[String]) -> Result<(), String> {
        // The account stamp is bound to THIS deletion: with sync on, the live account is observed
        // now (one transport status read, bounded by the transport's own timeout; no engine
        // lock), and only an answer that names the consented account stamps the entries. An
        // earlier observation, however recent, is never used for the stamp: it proves the account
        // at its own time, not at the deletion. Anything else leaves the deletion unverified.
        // The stamp is the account value of that one observation (`Authorized` means the consent
        // gate accepted it and it equals the consent-authorized account at that moment); it is
        // passed on as is, and neither consent nor the observation is read again afterwards.
        // The durable consent epoch is read BEFORE the observation; `enqueue_scoped` compares it
        // again under the outbox lock, so a consent change anywhere between here and the save
        // (revoke, re-authorize) leaves the deletion unverified instead of scoped.
        let before = self.consent.read_trusted();
        let stamp = if !ids.is_empty() && before.sync_enabled {
            match self.observe_live_scope() {
                LiveAccount::Authorized(live) => Some((live, before.epoch)),
                LiveAccount::Changed | LiveAccount::Unconfirmed => None,
            }
        } else {
            None
        };
        self.enqueue_scoped(ids, stamp, false).map(|_| ())
    }

    /// Queue `ids`. `stamp` is the account and the durable consent epoch captured BEFORE the
    /// observation that named the account. The final check is made under the outbox lock, after
    /// waiting for it: sync on, the durable authorized account equals the stamp's account and
    /// the durable epoch equals the stamp's epoch. Only then are the entries added scoped (and
    /// recorded with that epoch). Otherwise `strict` (`delete_everywhere`) writes nothing and
    /// returns `Ok(false)`; a non-strict (ordinary) deletion is retained in the unverified
    /// partition and returns `Ok(false)`, so a deletion is never lost and never scoped to an
    /// account whose consent moved. Scoped partitions of any other epoch are fenced into the
    /// unverified partition in the same save. `Ok(true)` = queued scoped.
    ///
    /// Lock order: the outbox lock is taken first; the consent file is then only READ (it is
    /// replaced atomically; the consent transition lock is never taken here), so there is no
    /// path from this function into a lock a transition holds. A transition that lands after the
    /// check and before the save leaves entries recorded with the older epoch, which the fence
    /// refuses to send.
    fn enqueue_scoped(
        &self,
        ids: &[String],
        stamp: Option<(String, u64)>,
        strict: bool,
    ) -> Result<bool, String> {
        if ids.is_empty() {
            return Ok(true);
        }
        let guard = self
            .outbox_lock
            .lock()
            .map_err(|_| "deletion outbox lock poisoned".to_string())?;
        // Read only after the lock was acquired: this is the check the save is serialized with.
        // The consent epoch counts only while it is trusted (`trusted_state`): a consent file that
        // is missing, malformed or below the outbox's high-water mark stamps nothing, so the
        // deletion is retained unverified; an unreadable one is treated the same (the deletion is
        // never refused for it).
        let path = self.outbox_path();
        let mut outbox = Outbox::load(&path)?;
        let state = self.trusted_state(&outbox).unwrap_or(None);
        let holds = match (&state, &stamp) {
            (Some(state), Some((scope, epoch))) => {
                state.sync_enabled
                    && state.scope.authorized_scope.as_deref() == Some(scope.as_str())
                    && state.epoch == *epoch
            }
            _ => false,
        };
        if !holds && strict {
            return Ok(false);
        }
        if let Some(state) = &state {
            outbox.fence(state.epoch);
            outbox.note_epoch(state.epoch);
        }
        let now = self.host.now_ms();
        for raw in ids {
            let id = self.profile.logical_cloud_id(raw);
            match stamp.as_ref().filter(|_| holds) {
                Some((scope, epoch)) => outbox.add(Some(scope.as_str()), *epoch, &id, now)?,
                None => outbox.add(None, 0, &id, now)?,
            }
        }
        outbox.save(&path)?;
        drop(guard);
        if stamp.is_some() && !holds {
            self.emit(
                "cloud_sync_deletion_unverified",
                "reason=consent_epoch_moved",
            );
        }
        self.notify_local_change();
        Ok(holds)
    }

    /// The consent state the outbox may be judged against: a readable, well-formed consent file
    /// whose epoch is not below `outbox`'s high-water mark and not below the sync fence's epoch
    /// (`ConsentStore::consent_trusted`: the fence file must be present and valid, and no
    /// revocation may be incomplete). `Ok(None)`: the file is missing, is malformed, or carries
    /// an epoch lower than one the outbox or the fence already used (the counter was lost or
    /// rolled back, or an older enabled consent was restored), so its epoch proves nothing: nothing is fenced, handed over or
    /// held back on its word, and every ledger-queued deletion is unproven. The next consent
    /// transition moves to `max(consent epoch, high-water) + 1`. `Err`: the file cannot be read
    /// now.
    fn trusted_state(&self, outbox: &Outbox) -> Result<Option<TranscriptSyncState>, String> {
        Ok(match self.consent.load()? {
            ConsentRead::Valid(state)
                if state.epoch >= outbox.high_water() && self.consent.consent_trusted(&state) =>
            {
                Some(state)
            }
            _ => None,
        })
    }

    /// In-memory fence for counting only (nothing is saved).
    fn fence_in_memory(&self, outbox: &mut Outbox) {
        if let Ok(Some(state)) = self.trusted_state(outbox) {
            outbox.fence(state.epoch);
        }
    }

    /// Everything a pass needs from the outbox before the engine runs, read, decided and saved in
    /// ONE load and ONE save under the outbox lock:
    ///
    /// 1. `stale`: every id the ledger queues (`pending`, the engine's `pending_tombstones`)
    ///    that has no hand-over evidence for the ledger's account and the current durable epoch
    ///    (`Outbox::handed_at`). Older epoch, another account, no record at all (a ledger or an
    ///    outbox from before the record existed, a lost outbox) and an untrusted consent epoch
    ///    all make an id stale. Each is added to the unverified partition (retained, inert,
    ///    counted) in this save, split into `lift` (the record proves the hand-over created the
    ///    id's tombstone) and `keep` (no such proof: marked unproven). The caller withdraws them
    ///    from the ledger only after this returned, so a crash between the two repeats the
    ///    withdrawal and loses no intent.
    /// 2. The hand-over record keeps only ids that are still queued by the ledger (a stale id
    ///    stays, with its fresh bit, until the ledger no longer queues it).
    /// 3. Scoped partitions of any other epoch are fenced into the unverified partition.
    /// 4. `eligible` (at most the drain budget left after the ledger's queue) and `held`: the live
    ///    account's own partition in the durable epoch, only while consent is on for exactly that
    ///    account. The unverified partition is never returned.
    fn prepare_pass_outbox(
        &self,
        live: Option<&str>,
        ledger_scope: Option<&str>,
        pending: &BTreeSet<String>,
    ) -> Result<PassOutbox, String> {
        let _guard = self
            .outbox_lock
            .lock()
            .map_err(|_| "deletion outbox lock poisoned".to_string())?;
        let path = self.outbox_path();
        let mut outbox = Outbox::load(&path)?;
        let state = self.trusted_state(&outbox)?;
        let proof: Option<(&str, u64)> = match (&state, ledger_scope) {
            (Some(state), Some(scope))
                if state.sync_enabled && state.scope.authorized_scope.as_deref() == Some(scope) =>
            {
                Some((scope, state.epoch))
            }
            _ => None,
        };
        let stale: Vec<String> = pending
            .iter()
            .filter(|id| {
                !proof.is_some_and(|(scope, epoch)| outbox.handed_at(scope, epoch, id.as_str()))
            })
            .cloned()
            .collect();
        // The record proves a prior state only for an id it lists as fresh for the ledger's own
        // account (in any epoch); every other stale id has an unknown prior state.
        let (lift, keep): (Vec<String>, Vec<String>) = stale.iter().cloned().partition(|id| {
            ledger_scope.is_some_and(|scope| outbox.handed_fresh(scope, id.as_str()))
        });
        let mut changed = false;
        if !stale.is_empty() {
            let now = self.host.now_ms();
            for id in &stale {
                outbox.add(None, 0, id, now)?;
            }
            let keep_set: BTreeSet<String> = keep.iter().cloned().collect();
            // A mark is evidence about the ledger's own account: with no bound ledger account
            // there is nothing to bind it to, so none is made (an unbound ledger is reset on its
            // first bind, which drops its tombstones and queue anyway).
            if let Some(scope) = ledger_scope {
                let marked_in = state
                    .as_ref()
                    .map_or_else(|| outbox.high_water(), |state| state.epoch);
                outbox.mark_unproven(scope, marked_in, &keep_set);
            }
            changed = true;
        }
        // The record keeps a stale id (and its fresh bit) for as long as the ledger queues it, so
        // a crash between this save and the ledger withdrawal repeats the same decision.
        changed |= outbox.retain_handed(|id| pending.contains(id));
        let mut epoch = None;
        let mut eligible = BTreeSet::new();
        let mut held = BTreeSet::new();
        let mut unproven = BTreeSet::new();
        let mut fenced = 0;
        if let Some(state) = &state {
            fenced = outbox.fence(state.epoch);
            changed |= fenced > 0;
            changed |= outbox.note_epoch(state.epoch);
            if let Some(live) = live {
                if state.sync_enabled && state.scope.authorized_scope.as_deref() == Some(live) {
                    let queued = pending.len().saturating_sub(stale.len());
                    let budget = MAX_DRAIN_PER_PASS.saturating_sub(queued);
                    eligible = outbox.eligible(live, state.epoch, budget);
                    held = outbox.held_ids(live, state.epoch);
                    unproven = outbox.unproven_among(live, &eligible);
                    epoch = Some(state.epoch);
                }
            }
        }
        if changed {
            outbox.save(&path)?;
        }
        if fenced > 0 {
            self.emit(
                "cloud_sync_deletions_fenced",
                &format!("count={fenced} reason=consent_epoch_moved"),
            );
        }
        if !stale.is_empty() {
            self.emit(
                "cloud_sync_pending_quarantined",
                &format!(
                    "count={} kept_local={} reason={}",
                    stale.len(),
                    keep.len(),
                    if state.is_some() {
                        "no_current_epoch_evidence"
                    } else {
                        "consent_epoch_untrusted"
                    }
                ),
            );
        }
        Ok(PassOutbox {
            stale,
            lift,
            keep,
            unproven,
            eligible,
            held,
            epoch,
        })
    }

    /// Record, durably and BEFORE the ledger write, that `ids` are handed to the engine under
    /// `scope` in consent epoch `epoch`. The consent file is read again under the outbox lock:
    /// sync on, authorized account exactly `scope`, durable epoch exactly `epoch` and trusted.
    /// `Ok(false)`: it no longer holds; nothing was recorded and nothing may be handed over.
    fn record_handover(
        &self,
        scope: &str,
        epoch: u64,
        ids: &BTreeSet<String>,
        fresh: &BTreeSet<String>,
    ) -> Result<bool, String> {
        if ids.is_empty() {
            return Ok(true);
        }
        let _guard = self
            .outbox_lock
            .lock()
            .map_err(|_| "deletion outbox lock poisoned".to_string())?;
        let path = self.outbox_path();
        let mut outbox = Outbox::load(&path)?;
        let holds = self.trusted_state(&outbox)?.is_some_and(|state| {
            state.sync_enabled
                && state.scope.authorized_scope.as_deref() == Some(scope)
                && state.epoch == epoch
        });
        if !holds {
            self.emit(
                "cloud_sync_drain_deferred",
                &format!("count={} reason=consent_epoch_moved", ids.len()),
            );
            return Ok(false);
        }
        outbox.hand_over(scope, epoch, ids, fresh)?;
        outbox.save(&path)?;
        Ok(true)
    }

    /// After an engine pass whose ledger is bound to `scope`: remove the acknowledged ids.
    /// `proven` is always empty today (no unverified deletion is promoted; see the caller).
    fn settle_outbox(
        &self,
        scope: &str,
        acknowledged: &BTreeSet<String>,
        proven: &BTreeSet<String>,
    ) -> Result<Settled, String> {
        let _guard = self
            .outbox_lock
            .lock()
            .map_err(|_| "deletion outbox lock poisoned".to_string())?;
        let path = self.outbox_path();
        let mut outbox = Outbox::load(&path)?;
        let mut changed = outbox.remove(scope, acknowledged);
        let mut promoted = 0;
        if !proven.is_empty() {
            if let Ok(Some(state)) = self.trusted_state(&outbox) {
                promoted = outbox.promote(scope, state.epoch, proven);
            }
        }
        changed |= promoted > 0;
        if changed {
            outbox.save(&path)?;
        }
        // A partition of another epoch no longer waits (the next pass or enqueue saves the fence).
        self.fence_in_memory(&mut outbox);
        Ok(Settled {
            waiting: outbox.count_for(scope),
            promoted,
            unverified: outbox.count_unverified(),
            other_accounts: outbox.count_other_accounts(Some(scope)),
        })
    }

    /// Deletions still waiting for `scope`; an unreadable outbox counts as waiting.
    fn queued_deletions(&self, scope: &str) -> usize {
        let Ok(_guard) = self.outbox_lock.lock() else {
            return 1;
        };
        Outbox::load(&self.outbox_path()).map_or(1, |mut outbox| {
            self.fence_in_memory(&mut outbox);
            outbox.count_for(scope)
        })
    }

    /// How many deletions the outbox keeps without sending them, and how many wait for the
    /// authorized account. An unreadable outbox is an `Err`, never zero.
    pub fn deletion_backlog(&self) -> Result<DeletionBacklog, String> {
        let _guard = self
            .outbox_lock
            .lock()
            .map_err(|_| "deletion outbox lock poisoned".to_string())?;
        let mut outbox = Outbox::load(&self.outbox_path())?;
        // In memory only: what the next pass would fence is counted as unverified now (nothing
        // is fenced while the consent epoch is untrusted).
        self.fence_in_memory(&mut outbox);
        let authorized = self.consent.authorized_scope();
        Ok(DeletionBacklog {
            unverified: outbox.count_unverified(),
            other_accounts: outbox.count_other_accounts(authorized.as_deref()),
            waiting: authorized
                .as_deref()
                .map_or(0, |scope| outbox.count_for(scope)),
        })
    }

    /// The live account, read from the transport before the pass touches the engine, held against
    /// the account consent authorizes. Three answers, never merged: only `Authorized` lets queued
    /// deletions reach the engine; `Changed` and `Unconfirmed` hand it nothing (the engine pass
    /// itself reports why it cannot run).
    fn observe_live_scope(&self) -> LiveAccount {
        // No answer is kept: every caller that needs the account at a moment observes it then.
        let Ok(auth) = self.transport.auth() else {
            return LiveAccount::Unconfirmed;
        };
        if auth.availability().is_err() {
            return LiveAccount::Unconfirmed;
        }
        let Some(live) = auth.account_scope().filter(|scope| !scope.is_empty()) else {
            return LiveAccount::Unconfirmed;
        };
        let gate = self
            .consent
            .authorize_live_scope(&live, false, &|binding| self.paths.anchor_for(binding));
        let authorized = self.consent.authorized_scope();
        match (gate, authorized) {
            (Ok(()), Some(scope)) if scope == live => LiveAccount::Authorized(live),
            (_, Some(scope)) if scope != live => LiveAccount::Changed,
            _ => LiveAccount::Unconfirmed,
        }
    }

    // ----- engine hosting -----

    fn legacy_files(&self, binding: &ConsentScopeBinding) -> LegacyFiles {
        let scoped = |base: &str| match binding.authorized_scope.as_deref() {
            Some(scope) => self.paths.root(&self.profile.legacy_scoped_file_name(
                scope,
                binding.legacy_scope.as_deref(),
                base,
            )),
            None => self.paths.root(base),
        };
        let files = &self.profile.files;
        LegacyFiles {
            tombstones: scoped(&files.legacy_tombstones),
            pending: scoped(&files.legacy_pending),
            aliases: scoped(&files.legacy_aliases),
            acks: self.paths.root(&files.legacy_acks),
        }
    }

    /// One-time, non-destructive import of pre-engine state into the ledger. Runs only while
    /// the live account IS the consent-authorized account: legacy acknowledgements and
    /// tombstones describe one account's zone, and importing them under any other account would
    /// mark its records as already synced. The old files stay where they are as the backup; the
    /// ledger is written last, so its existence is the done marker. The ledger is created under
    /// the cross-process ledger lock and only if it still does not exist once the lock is held,
    /// so the migration never replaces a ledger another process created meanwhile. A failure
    /// aborts the engine build (the pass reports it and the scheduler retries), so a ledger is
    /// never created without the migration having been decided on proof of the account.
    fn migrate_legacy(&self, ledger_path: &Path, versions_path: &Path) -> Result<(), String> {
        if !self.transport.legacy_import() || ledger_path.exists() {
            return Ok(());
        }
        let auth = self.transport.auth()?;
        auth.availability().map_err(|err| err.to_string())?;
        let live = auth
            .account_scope()
            .filter(|scope| !scope.is_empty())
            .ok_or_else(|| self.scope_unavailable_message.clone())?;
        self.consent
            .authorize_live_scope(&live, false, &|binding| self.paths.anchor_for(binding))?;
        let binding = self.consent.read().scope;
        if binding.authorized_scope.as_deref() != Some(live.as_str()) {
            return Err(
                "cloud sync authorized account does not match the live account".to_string(),
            );
        }
        let migrated = self.profile.migrate_legacy_once(
            &self.legacy_files(&binding),
            Some(live.as_str()),
            ledger_path,
            versions_path,
        )?;
        if let Some(report) = migrated {
            self.emit(
                "cloud_sync_legacy_migrated",
                &format!(
                    "tombstones={} pending_tombstones={} alias_ids={} seeded_versions={} dropped_conflicts={}",
                    report.tombstones,
                    report.pending_tombstones,
                    report.alias_ids,
                    report.seeded_versions,
                    report.dropped_conflicts
                ),
            );
        }
        Ok(())
    }

    fn build_engine(&self) -> Result<Hosted, String> {
        let ledger_path = self.paths.env(&self.profile.files.ledger);
        let versions_path = self.paths.env(&self.profile.files.versions);
        // The existing book is validated against the effective limits BEFORE the migration may
        // touch it: a book over a cap, or one that cannot be read, fails the build here, so the
        // migration never rewrites or truncates it. A missing book starts empty.
        LocalVersions::load_capped(
            versions_path.clone(),
            MAX_VERSIONS_FILE_BYTES,
            MAX_VERSIONS_ENTRIES,
        )?;
        self.migrate_legacy(&ledger_path, &versions_path)?;
        // Loaded again after the migration, which may have seeded entries; the same limits apply,
        // and a book the migration pushed over a cap fails the build without another write.
        let versions = LocalVersions::load_capped(
            versions_path,
            MAX_VERSIONS_FILE_BYTES,
            MAX_VERSIONS_ENTRIES,
        )?;
        let local = Arc::new(HistoryLocalStore::new(
            Arc::clone(&self.host),
            self.profile.clone(),
            self.local_source.clone(),
            self.max_history_rows,
            self.event_sink,
            versions,
            Arc::clone(&self.suppressed),
        ));
        let backend: Arc<dyn Backend> = Arc::new(WindowedBackend::new(
            self.transport.backend()?,
            Arc::clone(&self.window),
        ));
        let observer: Arc<dyn SyncObserver> = Arc::new(PassObserver {
            inner: self.transport.observer(),
            deferred_uploads: Arc::clone(&self.deferred_uploads),
        });
        let engine = SyncEngineBuilder::new(
            Arc::new(TranscriptMapper::new(self.profile.clone())),
            backend,
            Arc::clone(&local),
            Box::new(BoundedLedgerStorage::new(
                ledger_path,
                Arc::clone(&self.guard),
            )),
        )
        .auth(Arc::new(ConsentAuth {
            inner: self.transport.auth()?,
            consent: Arc::clone(&self.consent),
            paths: Arc::clone(&self.paths),
            scheduler: Arc::clone(&self.scheduler),
            scope_unavailable_message: self.scope_unavailable_message.clone(),
        }))
        .observer(observer)
        .local_page_size(LOCAL_WINDOW_ROWS)
        .max_uploads_per_pass(MAX_UPLOADS_PER_PASS)
        .config(SyncConfig::default())
        .build()
        .map_err(|err| format!("build cloud sync engine: {err}"))?;
        Ok(Hosted { engine, local })
    }

    /// The name of the full replay a history recovery asked for. It stays the same across the
    /// passes of one recovery: the engine stores the name in the ledger when a pass ends, so a
    /// replay that takes several passes continues from its cursor instead of starting over.
    fn recovery_flag_name(&self) -> String {
        let fresh = || format!("history-recovery-{}", self.host.now_ms());
        match self.recovery_flag.lock() {
            Ok(mut flag) => flag.get_or_insert_with(fresh).clone(),
            Err(_) => fresh(),
        }
    }

    fn clear_recovery_flag(&self) {
        if let Ok(mut flag) = self.recovery_flag.lock() {
            *flag = None;
        }
    }

    /// One engine pass, and a record of how it ended for `status`: a pass that fails before or
    /// after the engine ran is recorded as not synced.
    fn run_engine_pass(
        &self,
        consent_generation: u64,
        expect: Option<&DeleteToken>,
    ) -> Result<PassReport, String> {
        let result = self.run_engine_pass_inner(consent_generation, expect);
        let record = match &result {
            Ok(done) => {
                let synced = done.report.outcome == PassOutcome::Synced;
                let halted = synced && done.halt.is_some();
                let run = match self.run_progress.lock() {
                    Ok(mut progress) => {
                        if synced && (done.partial || halted) {
                            progress.passes = progress.passes.saturating_add(1);
                            progress.downloaded = progress.downloaded.saturating_add(
                                u64::try_from(done.report.downloaded).unwrap_or(u64::MAX),
                            );
                            progress.uploaded = progress.uploaded.saturating_add(
                                u64::try_from(done.report.uploaded).unwrap_or(u64::MAX),
                            );
                            Some(progress.clone())
                        } else {
                            if synced {
                                *progress = RunProgress::default();
                            }
                            None
                        }
                    }
                    // Counts unavailable: still never Ready after a pass that left work over.
                    Err(_) => (synced && (done.partial || halted)).then(RunProgress::default),
                };
                PassRecord {
                    ledger_scope: done.ledger_scope.clone(),
                    synced,
                    partial: run,
                    consent_generation,
                    halt: if synced { done.halt } else { None },
                    paused: if synced { done.paused } else { None },
                }
            }
            Err(_) => PassRecord {
                ledger_scope: None,
                synced: false,
                partial: None,
                consent_generation,
                halt: None,
                paused: None,
            },
        };
        if let Ok(mut slot) = self.last_pass.lock() {
            *slot = Some(record);
        }
        result.map(|done| done.report)
    }

    /// The pass, in this order. Nothing is written into the ledger (cached or on disk) before
    /// step 3 has verified the DURABLE ledger's account against the live consented account.
    ///
    /// 1. Establish the live account (`observe_live_scope`): transport available, scope present,
    ///    consent gate accepted, authorized scope equals live. Writes only the consent file.
    /// 2. Tell the ledger guard which account this pass may bind the ledger to (that one, or
    ///    none), and make sure the engine's cached ledger still equals the file (else rebuild the
    ///    engine from the file).
    /// 3. Read the durable ledger's account through the guard (same locks as a write).
    /// 4a. Whatever the ledger already queues (`pending_tombstones`) must have hand-over evidence
    ///    for the ledger's account and the current durable epoch (`Outbox::handed_at`); every
    ///    other id is kept in the unverified partition and withdrawn from the ledger (queue; the
    ///    local tombstone too only when the hand-over record proves the hand-over created it; one
    ///    guarded write; crate patch 0007) before the engine runs. A
    ///    withdrawal that cannot be written ends the pass.
    /// 4b. Only if the ledger is bound to the live account in the cache AND on disk: record the
    ///    hand-over (outbox save), then hand over the deletions of that account in the current
    ///    epoch (one ledger write). Otherwise hand over nothing: the engine's
    ///    own pass binds (or resets) the ledger to the live account, the next pass hands over.
    /// 5. Engine pass; every ledger write goes through the guard.
    /// 6. Settle the outbox (remove acknowledged ids; unverified deletions are not promoted), and ask for another pass when
    ///    work was left over.
    ///
    /// `expect` is set only for a pass `delete_everywhere` drives: the pass then proceeds only
    /// when its own step-1 observation names the token's account in the token's consent epoch,
    /// and otherwise returns `PASS_TOKEN_MISMATCH` before the ledger guard is set, the engine is
    /// touched or anything is handed over (no scoped write).
    fn run_engine_pass_inner(
        &self,
        consent_generation: u64,
        expect: Option<&DeleteToken>,
    ) -> Result<PassResult, String> {
        self.transport.prepare()?;
        let mut slot = self
            .engine
            .lock()
            .map_err(|_| "cloud sync engine lock poisoned".to_string())?;

        // 1.
        let account = self.observe_live_scope();
        self.emit(
            "cloud_sync_live_account",
            &format!("state={}", account.tag()),
        );
        let live: Option<String> = match account {
            LiveAccount::Authorized(scope) => Some(scope),
            LiveAccount::Changed | LiveAccount::Unconfirmed => None,
        };
        if let Some(token) = expect {
            if live.as_deref() != Some(token.scope.as_str())
                || !self.scope_holds(&token.scope, token.generation)
            {
                return Err(PASS_TOKEN_MISMATCH.to_string());
            }
        }
        let live = live.as_deref();

        // 2.
        self.guard.set_expected(live.map(str::to_string));
        let _expected = ExpectedScope(&self.guard);
        let mut checked: Option<Disk> = if slot.is_some() {
            Some(self.guard.check()?)
        } else {
            None
        };
        let rebuild = match &checked {
            None => true,
            Some(Disk::Matches(_)) => false,
            Some(Disk::Differs) => {
                self.emit("cloud_sync_ledger_rebuilt", "reason=disk_changed");
                true
            }
        };
        if rebuild {
            *slot = None;
            *slot = Some(self.build_engine()?);
            checked = None;
        }
        let hosted = slot
            .as_mut()
            .ok_or_else(|| "cloud sync engine unavailable".to_string())?;
        hosted.local.begin_pass();
        self.window.begin_pass();
        self.deferred_uploads.store(0, Ordering::Release);

        // 3. A cached engine was compared with the file in step 2; an engine built just now is
        // compared here, so the answer always comes from the file read through the guard.
        let scope_before = hosted.engine.ledger().scope.clone();
        let disk = match checked {
            Some(disk) => disk,
            None => self.guard.check()?,
        };
        let durable = match disk {
            Disk::Matches(state) => Some(state),
            Disk::Differs => None,
        };
        let drain_ok = match (live, durable.as_ref()) {
            (Some(live), Some(ScopeState::Bound(on_disk))) => {
                on_disk.as_str() == live && scope_before.as_deref() == Some(live)
            }
            _ => false,
        };

        // 4a. Deletions the ledger already queues. The engine writes whatever its ledger queues
        // (`pending_tombstones`) to the remote in this pass, so nothing may stay there without
        // evidence that it was handed over under THIS account in THIS consent epoch
        // (`Outbox::handed_at`). `prepare_pass_outbox` decides this and, in one save, moves every
        // unproven id into the unverified partition; only then are they withdrawn from the
        // ledger (`SyncEngine::withdraw_pending_tombstones`, crate patch 0007): the id leaves
        // `pending_tombstones`; its local tombstone is lifted only when the hand-over record
        // proves the hand-over created it (`lift`) and is kept otherwise (`keep`), in one ledger
        // write through the guard. The engine is not running (it runs at step 5). A refused write, or a ledger on
        // disk that differs from the engine's view (the guard would refuse the write), ends the
        // pass here with nothing flushed.
        let ledger_pending = hosted.engine.ledger().pending_tombstones.clone();
        let prepared = self.prepare_pass_outbox(live, scope_before.as_deref(), &ledger_pending)?;
        if !prepared.stale.is_empty() {
            if durable.is_none() {
                return Err(
                    "cloud sync ledger changed; queued deletions are not verified".to_string(),
                );
            }
            hosted
                .engine
                .withdraw_pending_tombstones(&prepared.lift, &prepared.keep)
                .map_err(|err| format!("withdraw cloud tombstones: {err}"))?;
        }

        // 4b. Hand over the live account's deletions of the durable epoch. The hand-over record is
        // saved first; the ledger write follows.
        let mut batch: BTreeSet<String> = BTreeSet::new();
        if let Some(live) = live {
            if drain_ok {
                if let Some(epoch) = prepared.epoch {
                    // An id the ledger holds as buried and confirmed (a tombstone, not queued)
                    // is not queued again: the remote already holds the deletion, and the
                    // tombstone is left exactly as it is. That holds only for a tombstone this
                    // account's ledger confirmed: an id with an unproven mark for this account,
                    // or with a legacy unscoped mark (provenance unknown), is queued anyway and
                    // its remote write performed (`unconfirmed`), with the existing tombstone
                    // value preserved; it is acknowledged only after that write is confirmed. `fresh` are the ids with no
                    // tombstone yet: the hand-over creates it, and only those may lose it in a
                    // withdrawal. Read under the engine lock, before the record is saved.
                    let mut queue: BTreeSet<String> = BTreeSet::new();
                    let mut fresh: BTreeSet<String> = BTreeSet::new();
                    let mut unconfirmed: BTreeSet<String> = BTreeSet::new();
                    {
                        let ledger = hosted.engine.ledger();
                        for id in &prepared.eligible {
                            let buried = ledger.tombstones.contains_key(id);
                            let queued = ledger.pending_tombstones.contains(id);
                            let doubtful = prepared.unproven.contains(id);
                            if buried && !queued && !doubtful {
                                continue;
                            }
                            queue.insert(id.clone());
                            if doubtful {
                                unconfirmed.insert(id.clone());
                            }
                            if !buried {
                                fresh.insert(id.clone());
                            }
                        }
                    }
                    if self.record_handover(live, epoch, &queue, &fresh)? {
                        // Every eligible id counts as handed: a confirmed one is acknowledged
                        // below without having been queued.
                        batch = prepared.eligible;
                        let ids: Vec<String> = queue.iter().cloned().collect();
                        hosted
                            .engine
                            .note_deleted_many(&ids, &unconfirmed)
                            .map_err(|err| format!("queue cloud tombstones: {err}"))?;
                    }
                }
            } else if !prepared.eligible.is_empty() {
                let why = if scope_before.as_deref() == Some(live) {
                    "disk_unverified"
                } else {
                    "ledger_not_bound"
                };
                self.emit(
                    "cloud_sync_drain_deferred",
                    &format!("count={} reason={why}", prepared.eligible.len()),
                );
            }
            if let Ok(mut suppressed) = self.suppressed.lock() {
                *suppressed = prepared.held;
            }
        }
        // Store-rebuild recovery: a rebuilt history needs the full zone, not the cursor the old
        // install had. Runs only for the consent-authorized scope the host's marker is bound to.
        let scope = self.consent.authorized_scope();
        let recovery = scope
            .as_deref()
            .is_some_and(|scope| self.host.recovery_pending(scope));
        if recovery {
            let flag = self.recovery_flag_name();
            hosted.engine.request_full_replay_once(&flag);
        } else {
            self.clear_recovery_flag();
        }

        // Activation (rev 68). The helper starts DISABLED; the account was observed above without
        // enabling it. Only now, with trusted consent admitted by `pass_gated` (and no disable
        // since), is it turned on, under `stop_gate`, so a disable either stops it afterwards or
        // is seen here and nothing starts. On failure nothing was transferred: the pass fails and
        // the next one tries again (durable consent is untouched).
        self.activate_transport_for_pass(consent_generation)?;

        // 5.
        let report = hosted.engine.run_pass();
        let held = self
            .suppressed
            .lock()
            .map(|mut suppressed| std::mem::take(&mut *suppressed))
            .unwrap_or_default();

        // The acknowledgement evidence: the ledger was bound to this same account on disk and in
        // memory before the pass (so no bind or reset happened, which would clear
        // `pending_tombstones` without any remote write) and no longer queues the id while still
        // holding its tombstone. The engine writes the ledger only after the remote saved the
        // record. The proof of ownership for an unverified id: it is a remote version of a ledger
        // that is bound to the live account now, in memory and on disk. A remote version is
        // recorded only from a record fetched from, or saved to, the zone of the account the
        // ledger is bound to, and an account switch resets the ledger, so the record exists in
        // that account's zone.
        let (bound_now, acknowledged, proven) = {
            let ledger = hosted.engine.ledger();
            let bound_now = ledger.scope.clone();
            let acknowledged: BTreeSet<String> = if drain_ok {
                batch
                    .iter()
                    .filter(|id| {
                        ledger.tombstones.contains_key(*id)
                            && !ledger.pending_tombstones.contains(*id)
                    })
                    .cloned()
                    .collect()
            } else {
                BTreeSet::new()
            };
            // No unverified deletion is promoted here. That an id is a remote version of the
            // bound account's ledger proves that account's zone holds the record; it does not
            // prove an unscoped deletion of that id was made for that account. Promotion needs
            // independent provenance of the account, which nothing in this module has, so the
            // set is empty and unverified deletions stay retained and counted.
            let proven: BTreeSet<String> = BTreeSet::new();
            (bound_now, acknowledged, proven)
        };
        drop(held);
        let metrics = hosted.engine.metrics();
        self.emit(
            "cloud_sync_pass",
            &format!(
                "{} remote_versions={} tombstones={} pending_uploads={} pending_tombstones={} pending_apply={}",
                pass_summary(&report),
                metrics.remote_versions,
                metrics.tombstones,
                metrics.pending_uploads,
                metrics.pending_tombstones,
                metrics.pending_apply
            ),
        );
        hosted.local.end_pass();
        let window_totals = self.window.finish_pass();
        if window_totals.stalled {
            self.emit("cloud_sync_feed_stalled", "reason=no_progressing_cursor");
        }
        let feed_cut = window_totals.truncated;
        let uploads_left = self.deferred_uploads.swap(0, Ordering::AcqRel);
        drop(slot);

        // 6.
        let mut drain_continues = false;
        if let Some(live) = live {
            if bound_now.as_deref() == Some(live) {
                match self.settle_outbox(live, &acknowledged, &proven) {
                    Ok(settled) => {
                        if settled.promoted > 0 {
                            self.emit(
                                "cloud_sync_deletions_proven",
                                &format!("count={}", settled.promoted),
                            );
                        }
                        if settled.unverified > 0 || settled.other_accounts > 0 {
                            self.emit(
                                "cloud_sync_deletion_backlog",
                                &format!(
                                    "unverified={} other_accounts={}",
                                    settled.unverified, settled.other_accounts
                                ),
                            );
                        }
                        // Another pass can hand over what waits: it bound the ledger just now,
                        // or this pass made progress (acknowledged or proven some) and more
                        // remain. A pass that made none asks for nothing, so this cannot loop.
                        let just_bound = !drain_ok && scope_before.as_deref() != Some(live);
                        drain_continues = settled.waiting > 0
                            && (just_bound || !acknowledged.is_empty() || settled.promoted > 0);
                    }
                    // Nothing was lost: the entries stay and the next pass repeats the write.
                    Err(err) => self.emit("cloud_sync_outbox_settle_failed", &err),
                }
            }
        }
        let synced = report.outcome == PassOutcome::Synced;
        // The feed verdict. Only a Synced pass read the feed to a conclusion; any other outcome
        // leaves the cross-pass record as it was (the scheduler's own retry rules apply to it).
        let mut halt: Option<&'static str> = None;
        if synced {
            let facts = FeedFacts {
                stalled: window_totals.stalled,
                cut: feed_cut,
                end_token: window_totals.end_token.as_deref(),
                applied: report.downloaded > 0 || report.tombstones_applied > 0,
            };
            let (verdict, backoff) = match self.feed_watch.lock() {
                Ok(mut watch) => {
                    let verdict = watch.judge(&facts);
                    (verdict, watch.backoff())
                }
                // The record cannot be read: treated as a halt at the longest backoff.
                Err(_) => (
                    FeedVerdict::Halt("watch_unavailable"),
                    Duration::from_secs(FEED_BACKOFF_MAX_SECS),
                ),
            };
            if let FeedVerdict::Halt(reason) = verdict {
                halt = Some(reason);
                self.emit(
                    "cloud_sync_feed_halted",
                    &format!("reason={reason} backoff_secs={}", backoff.as_secs()),
                );
                // No automatic pass is admitted before this instant, whoever ran this pass (the
                // scheduler or `delete_everywhere`). The request guarantees a next pass exists;
                // the scheduler's admission check holds it back until the instant.
                self.set_not_before(backoff);
                self.scheduler.request(PULL);
            }
        }
        let partial = feed_cut || uploads_left > 0 || drain_continues;
        let mut paused: Option<&'static str> = None;
        if !partial || halt.is_some() {
            // Nothing left over (or a halt, which takes its own backoff): the immediate count
            // starts again.
            if let Ok(mut watch) = self.feed_watch.lock() {
                watch.immediate_streak = 0;
            }
        }
        if partial {
            self.emit(
                "cloud_sync_pass_partial",
                &format!(
                    "feed_cut={feed_cut} uploads_left={uploads_left} deletions_continue={drain_continues}"
                ),
            );
            // Only a pass that ended Synced and made progress asks for the next one at once (the
            // feed advanced, or something was uploaded, downloaded, tombstoned or handed over),
            // so this cannot spin; any other outcome keeps the scheduler's own retry and backoff.
            let progressed = feed_cut
                || drain_continues
                || report.uploaded > 0
                || report.downloaded > 0
                || report.tombstones_written > 0
                || report.tombstones_applied > 0;
            // A halted pass never asks for an immediate next one (the backoff above governs).
            if synced && progressed && halt.is_none() {
                // The immediate reschedule is bounded on its own: whatever the cursors and the
                // counts were, `FEED_MAX_IMMEDIATE_PARTIAL_PASSES` immediate passes in a row end
                // in one backoff wait. This is not a feed failure: work continues afterwards,
                // the recovery stays unfinished and the status says `Paused`, never `Ready`. An
                // unreadable record takes the limit (it cannot prove the count).
                let limit_taken = self
                    .feed_watch
                    .lock()
                    .map_or(true, |mut watch| watch.note_immediate());
                if limit_taken {
                    paused = Some("pass_budget");
                    let wait = Duration::from_secs(FEED_BACKOFF_BASE_SECS);
                    self.emit(
                        "cloud_sync_pass_budget_pause",
                        &format!(
                            "limit={FEED_MAX_IMMEDIATE_PARTIAL_PASSES} backoff_secs={}",
                            wait.as_secs()
                        ),
                    );
                    // A next pass is guaranteed to exist (also when this pass did not come from
                    // the scheduler); admission holds it back until the instant.
                    self.set_not_before(wait);
                    self.scheduler.request(PULL);
                } else {
                    self.scheduler.request(PULL | PUSH);
                }
            }
        }
        // Recovery completes only after a FULLY synced pass: the replay reached the end of the
        // feed (not stalled, not cycling), and nothing is left over, stashed or queued. A halted
        // pass leaves the recovery flag and the host's marker as they are.
        if recovery && synced && !partial && halt.is_none() && paused.is_none() {
            if let Some(scope) = scope {
                self.host.recovery_complete(&scope, consent_generation)?;
            }
            self.clear_recovery_flag();
        }
        Ok(PassResult {
            paused,
            halt,
            report,
            ledger_scope: bound_now,
            partial,
        })
    }

    /// Run one engine pass (pull, merge, push, tombstones). `Ok(None)` = skipped (consent off,
    /// the host is busy, or the result was stale on completion). Needs a running tokio runtime.
    /// This is an explicit pass: it is not held back by the admission rule (`admission_wait`).
    /// The scheduler's automatic passes go through [`pass_automatic`](Self::pass_automatic).
    pub async fn pass(self: &Arc<Self>) -> Result<Option<PassReport>, String> {
        self.pass_for(None).await
    }

    /// `pass`, optionally bound to a `delete_everywhere` token: the pass runs in the token's
    /// consent epoch (not a fresh read of it) and stops with `PASS_TOKEN_MISMATCH` before any
    /// scoped write when the live account or consent no longer equals the token. Explicit: never
    /// deferred by `not_before`.
    async fn pass_for(
        self: &Arc<Self>,
        expect: Option<DeleteToken>,
    ) -> Result<Option<PassReport>, String> {
        Ok(match self.pass_gated(expect, false).await? {
            PassTurn::Done(report) => report,
            // Only an automatic pass is deferred by the admission rule.
            PassTurn::NotBefore(_) => None,
        })
    }

    /// An AUTOMATIC pass (the scheduler's: scheduled, retry, remote-event wake, local-change
    /// wake, status wake). Admission is checked again here, under `sync_lock`, before any engine
    /// work; see [`PassTurn::NotBefore`].
    pub(crate) async fn pass_automatic(self: &Arc<Self>) -> Result<PassTurn, String> {
        self.pass_gated(None, true).await
    }

    /// The one pass entry. `automatic` passes re-check the admission rule after `sync_lock` is
    /// acquired: the scheduler checked it before it waited for the lock, and a pass that ran in
    /// between (a direct pass, `delete_everywhere`) may have set `not_before` meanwhile. The
    /// re-check and the engine work are in the same critical section, so no pass can install a
    /// pause between them.
    async fn pass_gated(
        self: &Arc<Self>,
        expect: Option<DeleteToken>,
        automatic: bool,
    ) -> Result<PassTurn, String> {
        if !self.consent.enabled() {
            return Ok(PassTurn::Done(None));
        }
        let consent_generation = match &expect {
            Some(token) => {
                if !self.scope_holds(&token.scope, token.generation) {
                    return Err(PASS_TOKEN_MISMATCH.to_string());
                }
                token.generation
            }
            None => self.scheduler.consent_generation(),
        };
        if let Some(reason) = self.host.busy_reason() {
            self.emit(
                "cloud_sync_deferred",
                &format!("phase=pass reason={reason}"),
            );
            self.notify_local_change();
            return Ok(PassTurn::Done(None));
        }
        let _sync_guard = self.sync_lock.lock().await;
        if let Some(reason) = self.host.busy_reason() {
            self.emit(
                "cloud_sync_deferred",
                &format!("phase=pass reason={reason}_after_lock"),
            );
            self.notify_local_change();
            return Ok(PassTurn::Done(None));
        }
        if automatic {
            if let Some(remaining) = self.admission_wait() {
                self.emit(
                    "cloud_sync_deferred",
                    "phase=pass reason=not_before_after_lock",
                );
                return Ok(PassTurn::NotBefore(remaining));
            }
        }
        let this = Arc::clone(self);
        let report = tokio::task::spawn_blocking(move || {
            this.run_engine_pass(consent_generation, expect.as_ref())
        })
        .await
        .map_err(|err| format!("cloud_sync_pass task join: {err}"))??;
        if !self.consent.enabled() || self.scheduler.consent_generation() != consent_generation {
            self.emit("cloud_sync_stale_completion_dropped", "phase=pass");
            return Ok(PassTurn::Done(None));
        }
        Ok(PassTurn::Done(Some(report)))
    }

    // ----- consent commands -----

    /// Toggle consent. Turning it OFF never deletes remote or local data (deletion has its own
    /// explicit tombstone path, `enqueue_deleted_ids`); it only stops future passes. Turning it
    /// ON authorizes the live account. The consent file and the scheduler change together, or
    /// neither does.
    pub async fn set_enabled(self: &Arc<Self>, enabled: bool) -> Result<(), String> {
        // An enable captures the disable-request count NOW, before it can wait for the transition
        // lock: a disable whose front half runs after this point is a disable that arrived during
        // the enable, and wins (`persist_consent`, the check-and-start below).
        let started = self.consent.disable_requests.load(Ordering::Acquire);
        // Result of the early cancel request (disable only). It is never discarded: it feeds the
        // stop verification below, under the transition lock.
        let mut early_stop: Result<(), String> = Ok(());
        if !enabled {
            // Stop in memory FIRST, before any file work and whatever any file does: admission
            // closes (no pass can start, the epoch moves so an in-flight pass's result is
            // dropped), consent is untrusted until the fence is durable, and the transport is
            // asked to cancel an in-flight pass. The count, the sticky flag and the scheduler stop
            // are one step under `stop_gate` (no I/O under it) so an enable's final
            // check-and-start cannot slip between them.
            {
                let _gate = self
                    .consent
                    .stop_gate
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                self.consent.disable_requests.fetch_add(1, Ordering::AcqRel);
                self.consent
                    .revocation_incomplete
                    .store(true, Ordering::Release);
                self.scheduler.disable();
            }
            early_stop = self.transport.set_enabled(false);
            if let Some(native) = self.transport.native_observer() {
                native.set_enabled(false);
            }
        }
        // The transition lock: this async lock is held by an enable and by a disable from here to
        // the end of each (it also excludes every pass), and the consent transition guard is held
        // inside `persist_consent` across the whole file work AND the account observation.
        let _sync_guard = self.sync_lock.lock().await;
        // Disable only: `Some(reason)` when the stop could not be proven (see
        // `stop_transport_and_verify`); the durable revocation below is attempted either way.
        let stop_unproven: Option<String> = if !enabled {
            // The disable's front half may have run while an enable held the transition lock and
            // switched the transport back on afterwards: stop it again now that the enable is
            // over, and verify on the existing helper (never constructing one).
            let note = self.stop_transport_and_verify(std::mem::replace(&mut early_stop, Ok(())));
            if let Some(native) = self.transport.native_observer() {
                native.set_enabled(false);
            }
            note
        } else {
            None
        };
        let this = Arc::clone(self);
        let joined = tokio::task::spawn_blocking(move || this.persist_consent(enabled, started))
            .await
            .map_err(|err| format!("join cloud_sync_set_enabled: {err}"));
        // A disable never returns before its cleanup below, and a lost join is a failed
        // revocation (`revocation_incomplete` stays set), not an early exit that hides the stop.
        let mut result = if enabled {
            joined?
        } else {
            joined.and_then(|r| r)
        };
        if enabled && result.is_ok() {
            // The enable takes effect only here, under `stop_gate`, and only if no disable was
            // requested since it was called; otherwise the disable wins and the enable ends
            // stopped (the fence is put back to `revoked`, the transport stopped).
            //
            // Activation (rev 68) is the LAST step, in the same critical section: the transport
            // is switched on only here, after the consent is durable and re-read as current
            // (`persist_consent`), and the scheduler starts only if that succeeded. The transport
            // was never enabled before this point, so a superseded enable has nothing to undo
            // in it (the disable that won stops it itself, front and back half).
            enum Outcome {
                Started,
                Superseded,
                ActivationFailed(String),
            }
            let outcome = {
                let _gate = self
                    .consent
                    .stop_gate
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if self.consent.disable_requests.load(Ordering::Acquire) != started {
                    Outcome::Superseded
                } else {
                    match self.transport.set_enabled(true) {
                        Ok(()) => {
                            self.scheduler.enable();
                            Outcome::Started
                        }
                        Err(error) => Outcome::ActivationFailed(error),
                    }
                }
            };
            match outcome {
                Outcome::Started => {
                    self.consent.stop_unverified.store(false, Ordering::Release);
                }
                Outcome::Superseded => {
                    self.revoke_fence_best_effort();
                    result =
                        Err("cloud sync was turned off while it was being turned on".to_string());
                }
                Outcome::ActivationFailed(error) => {
                    // Durable consent says on, nothing is running. Make them agree by the
                    // normal disable commit (fence `revoked` first, outbox mark, consent off),
                    // then report. Sync is already stopped: the scheduler was never enabled
                    // and the transport refused to start.
                    self.emit("cloud_sync_activation_failed", "transport=not_started");
                    let stop_note = self.stop_transport_and_verify(Ok(()));
                    let this = Arc::clone(self);
                    let rolled_back =
                        tokio::task::spawn_blocking(move || this.persist_revocation())
                            .await
                            .map_err(|err| format!("join cloud_sync_rollback: {err}"))
                            .and_then(|r| r);
                    // "Turned back off" is said only when BOTH the stop is verified and the
                    // durable revocation was saved; otherwise the text names what is not.
                    let detail = super::safe_detail(&error);
                    let message = match (stop_note, rolled_back.is_err()) {
                        (None, false) => format!(
                            "cloud sync could not be started and was turned back off: {detail}"
                        ),
                        (None, true) => format!(
                            "cloud sync could not be started and is stopped, but turning it off could not be saved: {detail}"
                        ),
                        (Some(note), false) => format!(
                            "cloud sync could not be started; it is off in settings but {note}: {detail}"
                        ),
                        (Some(note), true) => format!(
                            "cloud sync could not be started; {note}, and turning it off could not be saved: {detail}"
                        ),
                    };
                    result = Err(message);
                }
            }
        }
        // Disable: preserve BOTH failures. The status shows both (`status_blocking`); the text
        // names each one that is not established.
        if !enabled {
            result = match (stop_unproven, result) {
                (None, result) => result,
                (Some(note), Ok(())) => Err(format!(
                    "cloud sync is off in settings, but {note}; sync stays off"
                )),
                (Some(note), Err(saved)) => Err(format!(
                    "{note}, and turning sync off could not be saved: {saved}"
                )),
            };
        }
        // A disable always ends stopped, whatever its files did; an enable only when it was saved.
        if result.is_ok() || !enabled {
            if !enabled {
                self.scheduler.disable();
            }
            // A new consent epoch starts without evidence of any earlier pass.
            if let Ok(mut slot) = self.last_pass.lock() {
                *slot = None;
            }
            if let Ok(mut progress) = self.run_progress.lock() {
                *progress = RunProgress::default();
            }
            self.clear_recovery_flag();
            self.reset_feed_watch();
            if let Some(native) = self.transport.native_observer() {
                native.set_enabled(enabled);
            }
        }
        result
    }

    /// Turning sync OFF (`persist_revocation`) never fails for the state of a local file: sync is
    /// already stopped in memory, and its result only says whether the revocation is durable
    /// (`Err`: the fence could not be written; the status is `RevocationIncomplete` until a
    /// later `set_enabled(false)` writes it). Turning it ON: a consent file that cannot be read
    /// is never replaced and an outbox that cannot be read refuses the transition (before the
    /// account is even observed). The enable is wrapped: whatever refuses it, the
    /// transport is only ever switched OFF once the durable steps began
    /// (`stop_transport_after_refused_enable`); it is never switched on before the consent is
    /// durable (rev 68).
    ///
    /// A missing or malformed consent file does not block the transition (the user's explicit
    /// choice replaces it), and its epoch is never reused: the new epoch is one above every
    /// epoch seen (`ConsentStore::commit_transition`: consent file, outbox high-water mark,
    /// fence file, this process); the fence file is written FIRST as `revoked`, then the outbox
    /// mark, then the consent file, and last the fence as `enabled` (`commit_enable`).
    fn persist_consent(&self, enabled: bool, started: u64) -> Result<(), String> {
        if !enabled {
            return self.persist_revocation();
        }
        // The transition guard is held across the whole durable enable, observation included, so
        // no other transition interleaves (a disable's file work waits for it, and its in-memory
        // stop is seen through `disable_requests`).
        let _guard = self.consent.transition_guard()?;
        self.refuse_if_disable_requested(started)?;
        if let Err(error) = self.consent.load() {
            return Err(error);
        }
        // Turning sync on first refuses an outbox that cannot be read, before the account is
        // observed. The authoritative read is the one under the locks.
        Outbox::peek_high_water(&self.consent.outbox_file)?;
        // (a) Observe the live account WITHOUT enabling anything (rev 68): the helper starts
        // disabled, and `auth().availability()` and `account_scope()` neither read nor set the
        // enabled flag (`rightkit-sync/src/helper/mod.rs:135-158`). A failure here (offline, no
        // helper, no scope) leaves nothing changed and nothing enabled.
        let actual = self.observe_account_for_enable()?;
        // (b) the durable steps, then (c) the admission re-check. The caller switches the
        // transport on (d), under `stop_gate`, only if this returns `Ok`.
        match self.commit_enable(&actual, started) {
            Ok(epoch) => self
                .recheck_enable_admission(epoch, started)
                .map_err(|error| self.with_transport_detail(error)),
            Err(error) => Err(self.with_transport_detail(error)),
        }
    }

    /// Enable step (a): the live account, read through the existing `auth()` (which constructs
    /// the helper DISABLED if needed). Never calls `set_enabled`. An unavailable account or an
    /// empty scope is an error, and no durable state exists yet.
    fn observe_account_for_enable(&self) -> Result<String, String> {
        let auth = self.transport.auth()?;
        auth.availability().map_err(|err| err.to_string())?;
        auth.account_scope()
            .filter(|scope| !scope.is_empty())
            .ok_or_else(|| "cloud sync scope check returned no scope".to_string())
    }

    /// Enable step (c): under `transition_guard`, after the last durable write, confirm the
    /// transition is still the current one: no disable was requested, the fence reads `enabled`
    /// at `epoch` and the consent file reads enabled at `epoch`. Only then may the caller
    /// activate the transport.
    fn recheck_enable_admission(&self, epoch: u64, started: u64) -> Result<(), String> {
        self.refuse_if_disable_requested(started)?;
        let stale = || "cloud sync consent changed while it was being turned on".to_string();
        match self.consent.load_fence() {
            Ok(FenceRead::Valid(FenceState::Enabled, fence)) if fence == epoch => {}
            _ => return Err(stale()),
        }
        match self.consent.load() {
            Ok(ConsentRead::Valid(state)) if state.sync_enabled && state.epoch == epoch => Ok(()),
            _ => Err(stale()),
        }
    }

    /// Pass-time activation (start-up and every later pass, rev 68): the helper is turned on only
    /// when the pass was admitted on trusted consent (`pass_gated`) and no disable or consent
    /// change has happened since (the scheduler's consent epoch is unchanged), under `stop_gate`
    /// (no I/O: the helper already exists after `prepare`). Already enabled is a no-op.
    fn activate_transport_for_pass(&self, consent_generation: u64) -> Result<(), String> {
        if self.transport.confirmed_stopped() == Some(false) {
            return Ok(());
        }
        let _gate = self
            .consent
            .stop_gate
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if self.scheduler.consent_generation() != consent_generation
            || self.consent.revocation_incomplete.load(Ordering::Acquire)
        {
            return Err("cloud sync was turned off before the sync helper started".to_string());
        }
        self.transport
            .set_enabled(true)
            .map_err(|_| "the sync helper could not be started".to_string())
    }

    /// `error` plus, when the transport could not be confirmed stopped, a note saying so (sync
    /// itself stays off: consent is not trusted, and the engine's auth gate needs both).
    fn with_transport_detail(&self, error: String) -> String {
        match self.stop_transport_after_refused_enable() {
            Some(detail) => format!("{error}; {detail}"),
            None => error,
        }
    }

    fn refuse_if_disable_requested(&self, started: u64) -> Result<(), String> {
        if self.consent.disable_requests.load(Ordering::Acquire) != started {
            return Err("cloud sync was turned off while it was being turned on".to_string());
        }
        Ok(())
    }

    /// After an enable refused once its durable steps began: when the durable consent is no
    /// longer trusted-on, make sure the transport is stopped and verify it WITHOUT creating
    /// anything (`confirmed_stopped`: the existing helper only). A trusted-on old consent leaves
    /// the transport as it was (this enable never switched it on). An unknown answer (no helper,
    /// unreadable state) is not a stop: it sets `stop_unverified` (status
    /// `NotConfirmed("stop_unverified")`). `Some(note)` when a stop could not be confirmed.
    fn stop_transport_after_refused_enable(&self) -> Option<String> {
        if self.consent.enabled() {
            return None;
        }
        let set = self.transport.set_enabled(false);
        if set.is_ok() && self.transport.confirmed_stopped() == Some(true) {
            self.consent.stop_unverified.store(false, Ordering::Release);
            return None;
        }
        self.consent.stop_unverified.store(true, Ordering::Release);
        Some("the sync helper could not be confirmed stopped; sync stays off".to_string())
    }

    /// Stop the transport and PROVE it, under the transition lock. `earlier` is the result of any
    /// earlier stop call of the same transition (never discarded). The final `set_enabled(false)`
    /// result is captured, then `confirmed_stopped()` asks the EXISTING helper only (it never
    /// constructs one). Only `Some(true)` after every stop call succeeded is a verified stop
    /// (clears `stop_unverified`); an error from any stop call, `Some(false)`, `None` or a
    /// poisoned lock sets `stop_unverified` (status `NotConfirmed("stop_unverified")`) and returns
    /// the reason. It never re-enables anything, and the caller still attempts the durable
    /// revocation independently of this result.
    fn stop_transport_and_verify(&self, earlier: Result<(), String>) -> Option<String> {
        let last = self.transport.set_enabled(false);
        let proof = self.transport.confirmed_stopped();
        let note = if earlier.is_err() || last.is_err() {
            "a request to stop the sync helper failed, so the stop could not be confirmed"
        } else if proof == Some(false) {
            "the sync helper still reports running"
        } else if proof.is_none() {
            "the sync helper could not be confirmed stopped"
        } else {
            self.consent.stop_unverified.store(false, Ordering::Release);
            return None;
        };
        self.consent.stop_unverified.store(true, Ordering::Release);
        self.emit("cloud_sync_stop_unverified", "stop=not_proven");
        Some(note.to_string())
    }

    /// Put the fence back to `revoked` (best effort) after an enable lost to a disable.
    fn revoke_fence_best_effort(&self) {
        if let Ok(_guard) = self.consent.transition_guard() {
            if let Ok(FenceRead::Valid(_, epoch)) = self.consent.load_fence() {
                let _ = self.consent.write_fence(FenceState::Revoked, epoch);
            }
        }
    }

    /// The durable part of an enable, in this order: (1) fence `revoked` with the new epoch,
    /// (2) the outbox's high-water mark, (3) the consent file, (4) fence `enabled` with the same
    /// epoch, the LAST durable step. Between steps a requested disable aborts it. Holds
    /// `transition_guard` (the caller).
    fn commit_enable(&self, actual: &str, started: u64) -> Result<u64, String> {
        let mut state = match self.consent.load()? {
            ConsentRead::Valid(state) => state,
            ConsentRead::Missing | ConsentRead::Malformed => TranscriptSyncState::default(),
        };
        let acknowledged = self.paths.anchor_for(&state.scope)?;
        sync_core::authorize_scope(
            &mut state.scope.authorized_scope,
            &mut state.scope.legacy_scope,
            acknowledged.as_deref(),
            actual,
            true,
            &self.consent.account_changed_message,
        )?;
        state.sync_enabled = true;
        self.refuse_if_disable_requested(started)?;
        // Steps 1 to 3. A successful step 1 is also the retry of a failed revocation (the durable
        // fence now says `revoked`).
        self.consent
            .commit_transition(&mut state, false, true, started)?;
        // Step 4, the last durable step, only if no disable arrived meanwhile.
        self.refuse_if_disable_requested(started)?;
        self.consent.finish_enable(state.epoch)?;
        Ok(state.epoch)
    }

    /// The disable transition: every file write is attempted in the order fence, outbox mark,
    /// consent (unless the epoch is exhausted); none can fail the disable except by leaving
    /// the independent fence undone, which is reported
    /// as `Err` (durable revocation incomplete).
    fn persist_revocation(&self) -> Result<(), String> {
        // Fail closed first: set again here so a caller that skipped the async front half still
        // starts from "not durable"; only a successful fence write clears it.
        self.consent
            .revocation_incomplete
            .store(true, Ordering::Release);
        // The mutex protects only serialization, not mutable data: recover exclusion on
        // poison so a previous panic cannot suppress this independent revocation attempt.
        let _guard = self
            .consent
            .lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (mut state, write_consent) = match self.consent.load() {
            Ok(ConsentRead::Valid(state)) => (state, true),
            Ok(ConsentRead::Missing | ConsentRead::Malformed) => {
                (TranscriptSyncState::default(), true)
            }
            // An unreadable consent file is never replaced (it already reads as off); the fence
            // is still written.
            Err(_) => (TranscriptSyncState::default(), false),
        };
        state.sync_enabled = false;
        match self
            .consent
            .commit_transition(&mut state, true, write_consent, 0)
        {
            Ok(issues) => {
                if !write_consent || !issues.is_empty() {
                    self.emit(
                        "cloud_sync_revocation_partial",
                        &format!(
                            "fence=written other_failures={}",
                            issues.len() + usize::from(!write_consent)
                        ),
                    );
                }
                Ok(())
            }
            Err(error) => {
                self.emit("cloud_sync_revocation_incomplete", "fence=not_written");
                Err(error)
            }
        }
    }

    /// The sync state for the settings screen. `Ready` only when every condition on
    /// [`StatusReport`] holds; the answer is read from the consent file, the transport, the
    /// ledger file and the last pass record at call time, never from an earlier answer. Never
    /// queues behind a running pass (`Syncing`), writes no file (observing a different live
    /// account only forgets the in-memory record of the last pass), and gives up waiting for the
    /// account check after 15 seconds (`NotConfirmed("check_timed_out")`).
    pub async fn status(self: &Arc<Self>) -> Result<StatusReport, String> {
        let this = Arc::clone(self);
        let check = tokio::task::spawn_blocking(move || this.status_blocking());
        match tokio::time::timeout(STATUS_CHECK_TIMEOUT, check).await {
            Ok(Ok(report)) => Ok(report),
            Ok(Err(err)) => Err(format!("cloud_sync_status task join: {err}")),
            Err(_) => Ok(StatusReport::NotConfirmed("check_timed_out")),
        }
    }

    fn status_blocking(&self) -> StatusReport {
        // A disable whose fence is not durable (sync is stopped in memory) is its own state,
        // whatever the consent file says.
        // Both failures of one disable stay visible: the combined tag names both, and each flag
        // is still queryable alone once the other is cleared (a verified stop, a saved fence).
        let revocation_incomplete = self.consent.revocation_incomplete.load(Ordering::Acquire);
        let stop_unverified = self.consent.stop_unverified.load(Ordering::Acquire);
        if revocation_incomplete && stop_unverified {
            return StatusReport::NotConfirmed("stop_unverified_revocation_incomplete");
        }
        if revocation_incomplete {
            return StatusReport::RevocationIncomplete;
        }
        // A stop that could not be proven (a disable, an activation rollback or a refused
        // enable) is never `Disabled` and never `Ready`, whatever the consent file says.
        if stop_unverified {
            return StatusReport::NotConfirmed("stop_unverified");
        }
        let state = match self.consent.read_checked() {
            Ok(state) => state,
            Err(_) => return StatusReport::NotConfirmed("consent_unreadable"),
        };
        if !state.sync_enabled {
            return StatusReport::Disabled;
        }
        // The revocation fence: it must say `enabled` with exactly the consent epoch. A fence that
        // says `revoked` (a disable, or an enable that did not finish), has another epoch, or is
        // missing or unreadable, trusts nothing.
        match self.consent.load_fence() {
            Ok(FenceRead::Valid(FenceState::Enabled, fence)) if state.epoch == fence => {}
            Ok(_) => return StatusReport::NotConfirmed("consent_untrusted"),
            Err(_) => return StatusReport::NotConfirmed("fence_unreadable"),
        }
        // A consent epoch below one the outbox already used cannot be trusted (the counter was
        // lost or rolled back): nothing is handed over until the next consent transition.
        match Outbox::peek_high_water(&self.consent.outbox_file) {
            Ok(high) if state.epoch >= high => {}
            Ok(_) => return StatusReport::NotConfirmed("consent_epoch_invalid"),
            Err(_) => return StatusReport::NotConfirmed("outbox_unreadable"),
        }
        let Some(authorized) = state.scope.authorized_scope else {
            return StatusReport::NotConfirmed("not_authorized");
        };
        match self.engine.try_lock() {
            Err(TryLockError::WouldBlock) => return StatusReport::Syncing,
            Err(TryLockError::Poisoned(_)) => {
                return StatusReport::NotConfirmed("engine_unavailable");
            }
            Ok(guard) => drop(guard),
        }
        let auth = match self.transport.auth() {
            Ok(auth) => auth,
            Err(detail) => {
                return StatusReport::Unavailable(super::safe_detail(&detail));
            }
        };
        if let Err(error) = auth.availability() {
            return StatusReport::Unavailable(super::safe_detail(&error.to_string()));
        }
        let live = match auth.account_scope() {
            Some(scope) if !scope.is_empty() => scope,
            _ => {
                return StatusReport::NotConfirmed("account_unknown");
            }
        };
        if live != authorized {
            // A different account is signed in. The record of the last pass belongs to the
            // account consent was given for; forget it, so coming back to that account is not
            // Ready until a pass under it has ended `Synced` again.
            if let Ok(mut slot) = self.last_pass.lock() {
                *slot = None;
            }
            if let Ok(mut progress) = self.run_progress.lock() {
                *progress = RunProgress::default();
            }
            return StatusReport::AccountChanged(self.consent.account_changed_message.clone());
        }
        match read_scope(
            &self.paths.env(&self.profile.files.ledger),
            MAX_LEDGER_FILE_BYTES,
        ) {
            Err(_) => return StatusReport::NotConfirmed("ledger_unreadable"),
            Ok(scope) if scope.as_deref() != Some(authorized.as_str()) => {
                return StatusReport::NotConfirmed("ledger_unbound");
            }
            Ok(_) => {}
        }
        let Ok(record) = self.last_pass.lock() else {
            return StatusReport::NotConfirmed("engine_unavailable");
        };
        match record.as_ref() {
            Some(pass) if pass.consent_generation == self.scheduler.consent_generation() => {
                if pass.synced && pass.ledger_scope.as_deref() == Some(authorized.as_str()) {
                    // A halted feed outranks Partial and Ready: a stalled or cycling feed is
                    // never reported as synced or as work in progress.
                    if let Some(reason) = pass.halt {
                        let run = pass.partial.clone().unwrap_or_default();
                        return StatusReport::FeedStalled {
                            reason,
                            passes: run.passes,
                            downloaded: run.downloaded,
                            uploaded: run.uploaded,
                        };
                    }
                    // A pass that hit the immediate-reschedule bound is waiting out a backoff;
                    // work continues later. Not a failure, not Ready.
                    if let Some(reason) = pass.paused {
                        let run = pass.partial.clone().unwrap_or_default();
                        return StatusReport::Paused {
                            reason,
                            passes: run.passes,
                            downloaded: run.downloaded,
                            uploaded: run.uploaded,
                        };
                    }
                    match &pass.partial {
                        Some(run) => StatusReport::Partial {
                            passes: run.passes,
                            downloaded: run.downloaded,
                            uploaded: run.uploaded,
                        },
                        None => StatusReport::Ready,
                    }
                } else {
                    StatusReport::NotConfirmed("last_pass_not_synced")
                }
            }
            _ => {
                // The account is confirmed and the ledger is bound to it, but nothing proves the
                // last pass. Ask the scheduler for one (coalesced with any request already
                // pending). A pass that runs leaves a record, so the request stops once one ran;
                // while the host is busy the scheduler defers it until `wake`.
                self.scheduler.request(PULL);
                StatusReport::NotConfirmed("no_pass_yet")
            }
        }
    }

    /// Ids this device knows: every local history row, plus the ids the ledger lists on the
    /// remote when the ledger is bound to the token's account BOTH on disk and in the engine's
    /// memory (an engine not built yet, or bound elsewhere, is not bound). `zone_listed` says
    /// whether the remote ids were included. The account is `token.scope`; `authorized_scope` is
    /// not read here. A ledger that cannot be read, is over its cap or does not decode is an
    /// `Err`; it is never read as an empty zone.
    fn known_ids(&self, token: &DeleteToken) -> Result<KnownIds, String> {
        let mut history = self.host.open_history()?;
        let mut ids: BTreeSet<String> = BTreeSet::new();
        scan_keys(&mut *history, self.max_history_rows, |key| {
            ids.insert(self.profile.logical_cloud_id(&key.delivery_id));
        })?;
        // Only the ledger of the token's account: another account's remote ids are not this
        // account's to delete. Bound on disk (the file read here) and in memory (the engine's
        // cached ledger; `None` when no engine was built yet).
        let ledger = read_ledger_ids(&self.paths.env(&self.profile.files.ledger))
            .map_err(|error| format!("read cloud sync ledger: {error}"))?;
        let memory_scope: Option<String> = self
            .engine
            .lock()
            .map_err(|_| "cloud sync engine lock poisoned".to_string())?
            .as_ref()
            .and_then(|hosted| hosted.engine.ledger().scope.clone());
        let bound = Some(token.scope.clone());
        let zone_listed = ledger.scope == bound && memory_scope == bound;
        if zone_listed {
            ids.extend(ledger.remote_ids);
        }
        Ok(KnownIds {
            ids: ids.into_iter().collect(),
            zone_listed,
        })
    }

    /// Delete-everywhere for the TRANSCRIPT data class only: tombstone every transcript this
    /// device knows about (local history plus every id in the engine ledger) and run a pass
    /// immediately so the deletion reaches every device on this account. A separate, explicit,
    /// confirmed action; confirmation is the caller's. Requires sync to be enabled: with sync
    /// off there is no live channel, so there is nothing to propagate.
    ///
    /// The whole operation runs under one [`DeleteToken`] (account and consent epoch) captured at
    /// entry from a positive observation of the live account made for this operation. Listing
    /// the ids, the ledger check, the enqueue, the pass and the reported completion all use that
    /// token and nothing re-reads `authorized_scope`. Before the enqueue, inside the pass and
    /// before success is reported the token is checked again (and the live account re-observed
    /// before the enqueue and before success); a mismatch ends the operation with
    /// `Incomplete` and no further scoped write.
    pub async fn delete_everywhere(self: &Arc<Self>) -> Result<DeleteOutcome, String> {
        if !self.consent.enabled() {
            return Err("cloud sync is disabled".to_string());
        }
        // Capture: one positive observation of the live account, equal to the consented account.
        let this = Arc::clone(self);
        let token = match tokio::task::spawn_blocking(move || this.capture_delete_token())
            .await
            .map_err(|err| format!("cloud_sync_delete_everywhere task join: {err}"))?
        {
            Ok(token) => token,
            Err(outcome) => return Ok(outcome),
        };
        // List (reads only), bound to the token's account.
        let this = Arc::clone(self);
        let listing = token.clone();
        let KnownIds { ids, zone_listed } =
            tokio::task::spawn_blocking(move || this.known_ids(&listing))
                .await
                .map_err(|err| format!("cloud_sync_delete_everywhere task join: {err}"))??;
        if ids.is_empty() {
            // "Nothing" is a completion claim: the account is observed again first.
            let this = Arc::clone(self);
            let check = token.clone();
            let holds = tokio::task::spawn_blocking(move || {
                this.reobserve_holds(&check.scope, check.generation)
            })
            .await
            .map_err(|err| format!("cloud_sync_delete_everywhere task join: {err}"))?;
            return Ok(if !holds {
                DeleteOutcome::Incomplete("scope_changed")
            } else if zone_listed {
                DeleteOutcome::Nothing
            } else {
                DeleteOutcome::Incomplete("zone_not_listed")
            });
        }
        let count = u32::try_from(ids.len()).unwrap_or(u32::MAX);
        // First scoped write: the token is re-checked and the live account observed again, then
        // the ids are queued under the TOKEN's account (never a re-read one). Should the account
        // change right after this check the entries still sit in the token account's partition,
        // which no pass under another account hands to the engine.
        let this = Arc::clone(self);
        let writing = token.clone();
        let queued = tokio::task::spawn_blocking(move || {
            if !this.reobserve_holds(&writing.scope, writing.generation) {
                return Ok(false);
            }
            // Strict: the final check against the durable epoch and account is made under the
            // outbox lock; a mismatch writes nothing and ends the operation as `scope_changed`.
            this.enqueue_scoped(&ids, Some((writing.scope.clone(), writing.epoch)), true)
        })
        .await
        .map_err(|err| format!("cloud_sync_delete_everywhere task join: {err}"))??;
        if !queued {
            return Ok(DeleteOutcome::Incomplete("scope_changed"));
        }
        // An explicit user action: the cursor record and the streaks start again.
        self.reset_feed_watch();
        let report = match self.pass_for(Some(token.clone())).await {
            Ok(report) => report,
            Err(err) if err == PASS_TOKEN_MISMATCH => {
                return Ok(DeleteOutcome::Incomplete("scope_changed"));
            }
            Err(err) => return Err(err),
        };
        match report {
            Some(report) if report.outcome == PassOutcome::Synced => {
                // Synced is the pass outcome; the deletions are done only once the outbox no
                // longer holds them (the pass that binds an account cannot write them).
                // A pass that left work over (the feed cut, uploads left, deletions continuing)
                // has not finished; a record that cannot be read counts as not finished. A pass
                // whose feed stalled or cycled has not finished either, whatever the outbox says.
                // The record must be of a pass in the token's epoch with the ledger bound to the
                // token's account; anything else is a changed scope, not a completion.
                let (partial, halted, paused, same_scope) = self
                    .last_pass
                    .lock()
                    .ok()
                    .and_then(|slot| {
                        slot.as_ref().map(|record| {
                            (
                                record.partial.is_some(),
                                record.halt.is_some(),
                                record.paused.is_some(),
                                record.consent_generation == token.generation
                                    && record.ledger_scope.as_deref() == Some(token.scope.as_str()),
                            )
                        })
                    })
                    .unwrap_or((true, false, false, true));
                if !same_scope {
                    return Ok(DeleteOutcome::Incomplete("scope_changed"));
                }
                if halted {
                    return Ok(DeleteOutcome::Incomplete("feed_stalled"));
                }
                if paused {
                    return Ok(DeleteOutcome::Incomplete("paused"));
                }
                if self.queued_deletions(&token.scope) > 0 {
                    return Ok(DeleteOutcome::Incomplete("queued"));
                }
                if partial {
                    return Ok(DeleteOutcome::Incomplete("partial"));
                }
                // Reported completion: the live account is observed again and the token must
                // still hold, or nothing is claimed.
                let this = Arc::clone(self);
                let check = token.clone();
                let holds = tokio::task::spawn_blocking(move || {
                    this.reobserve_holds(&check.scope, check.generation)
                })
                .await
                .map_err(|err| format!("cloud_sync_delete_everywhere task join: {err}"))?;
                Ok(if !holds {
                    DeleteOutcome::Incomplete("scope_changed")
                } else if !zone_listed {
                    // The ledger was not bound to this account when the ids were listed, so
                    // records only the remote holds were not among them. The pass has bound the
                    // ledger now; running delete-everywhere again lists them.
                    DeleteOutcome::Incomplete("zone_not_listed")
                } else {
                    DeleteOutcome::Synced(count)
                })
            }
            Some(report) => Ok(DeleteOutcome::Incomplete(outcome_tag(&report.outcome))),
            None => Ok(DeleteOutcome::Deferred),
        }
    }
}
