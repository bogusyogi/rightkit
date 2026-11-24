//! The durable record of a local store that was moved aside, bound to the sync consent that was
//! current when it happened (`rightkit_sync::recovery_marker`).
//!
//! The ordinary case: an encrypted database travels to a new machine (backup restore, migration
//! assistant) while the credential holding its key does not. The owner of the database rebuilds it
//! empty and moves the unreadable files aside, never deleting them. This module is the part that
//! belongs to sync: the marker that binds that event to the consent-authorized sync scope, so the
//! rebuilt store is refilled by a full replay, and so the quarantined ciphertext is only released
//! from "pending" by a replay that provably ran under the same account and consent epoch.
//!
//! A consented pull whose validated scope digests to the marker's bound scope asks the engine for a
//! full replay; [`RecoveryLedger::finish_after_replay`], reached only after merge and cursor commit,
//! moves the pending quarantine set into the marker's `retained` ledger. That is where it ends: zone
//! coverage proves the zone contained items but cannot prove EVERY unsynced predecessor row was in
//! it, so the quarantined ciphertext is retained, never deleted by this code path. There is no
//! cleanup offered to a user and no claim of restoration completeness.
//!
//! Boundaries kept deliberately conservative:
//! - a marker with no recorded scope (never consented, or the consent state did not migrate) never
//!   authorizes a replay or a completion;
//! - a marker that begins unbound STAYS unbound: a scope authorized later is a different consent
//!   epoch and must not adopt an older unknown-owner quarantine;
//! - while sync consent is off or unreachable, marker and files remain (inert ciphertext) until a
//!   matching-scope replay completes.
//!
//! The marker's `pairs` are the recorded moves of a quarantine an EARLIER build made with its own
//! move code; [`RecoveryLedger::pending_pairs`] and [`RecoveryLedger::settle_pairs`] let the
//! database owner finish or roll them back before the database opens. This module never writes
//! `pairs`.
//!
//! Nothing here names an application, a path, a database or an event: the marker path, the clock,
//! the consent scope and epoch, the replacement-store check and the event sink come from the host
//! through [`RecoveryHost`]. The marker JSON field names and state words are a persisted format and
//! must not change.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Marker states. `pending` = a bound replay is still owed; `retained` = a bound replay completed,
/// so the quarantined set moved to the `retained` ledger: files kept permanently, no more replays.
const STATE_PENDING: &str = "pending";
const STATE_RETAINED: &str = "retained";

/// What the host supplies, including its consent commit boundary.
pub trait RecoveryHost: Send + Sync {
    /// Where the marker file lives (the host's app-data folder plus its marker file name).
    fn marker_path(&self) -> PathBuf;
    /// Wall clock in unix milliseconds, for the marker's `created_at_ms`.
    fn now_ms(&self) -> u64;
    /// The consent-authorized sync scope right now: `None` when consent was never granted or its
    /// state file did not migrate. This is the "who owns the quarantined zone" evidence.
    fn authorized_scope(&self) -> Option<String>;
    /// The live runtime consent generation. Re-checked against the replay receipt at finalize.
    fn consent_epoch(&self) -> u64;
    /// One trusted durable consent snapshot. Its epoch survives process restarts; the runtime
    /// generation returned by `consent_epoch` must never be substituted for it.
    fn recovery_consent(&self) -> Option<RecoveryConsent> {
        None
    }
    /// Reopen the replacement store and answer a real query: the number of rows it reports. Proves
    /// only that the replacement is usable, never that predecessor rows were recovered.
    fn verify_replacement(&self) -> Result<u32, String>;
    /// Admit `commit` exactly once while `receipt` remains authorized, excluding durable consent
    /// revocation and account transitions until the durable commit finishes. In-memory
    /// cancellation stays immediate; already-admitted marker I/O orders before durable revoke.
    /// The host must use
    /// the SAME boundary that linearizes those transitions; separate epoch/scope reads do not
    /// satisfy this contract. Replacement-store verification has already finished, so this
    /// callback performs marker work only and must not reopen the store.
    ///
    /// `Ok(false)` means no callback ran and recovery remains pending. Hosts without an atomic
    /// consent boundary fail closed by default; implementing only the live getters never permits
    /// a terminal marker write.
    fn commit_recovery_if_authorized(
        &self,
        _receipt: &ReplayReceipt,
        _consent: &RecoveryConsent,
        _commit: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<bool, String> {
        Ok(false)
    }
    /// Telemetry sink for [`LedgerEvent`]s. `detail` carries no key material or history content.
    fn event(&self, event: LedgerEvent, detail: &str);
}

/// Durable account authorization captured at quarantine, independent of the scheduler's runtime
/// generation. Hosts obtain it from their managed consent owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryConsent {
    pub scope: String,
    pub epoch: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DurableConsentBinding {
    v: u8,
    epoch: u64,
}

/// What the ledger reports to the host's sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LedgerEvent {
    /// A quarantine was recorded in the marker.
    Quarantined,
    /// The marker exists but could not be read (pending lookup).
    MarkerReadFailed,
    /// The marker exists but is unreadable or of an unknown version; left in place.
    MarkerUnreadable,
    /// A finalize was refused; `detail` is `reason=<why>`.
    FinalizeRejected,
    /// A finalize completed; `detail` is `retained_files=<n> verified_total=<n>`.
    Completed,
}

/// One recorded source to destination move of a quarantine an earlier build made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedPair {
    pub source: PathBuf,
    pub destination: PathBuf,
}

impl RecordedPair {
    /// `(source, destination)`, the neutral shape a database owner's move record converts to and
    /// from, so no crate needs the other's type.
    pub fn into_paths(self) -> (PathBuf, PathBuf) {
        (self.source, self.destination)
    }
}

impl From<(PathBuf, PathBuf)> for RecordedPair {
    fn from((source, destination): (PathBuf, PathBuf)) -> Self {
        Self {
            source,
            destination,
        }
    }
}

/// The replacement-store check a host's [`RecoveryHost::verify_replacement`] needs: open the
/// rebuilt store, run a real query, report the rows it holds. `open` and `total` are the host's
/// store calls; this owns the order (a failed open never queries) and the error texts
/// (`post-replay store verification: <open error>`, `post-replay store query: <query error>`).
pub fn verify_replacement_total<S, OpenError, QueryError>(
    open: impl FnOnce() -> Result<S, OpenError>,
    total: impl FnOnce(&S) -> Result<u32, QueryError>,
) -> Result<u32, String>
where
    OpenError: std::fmt::Display,
    QueryError: std::fmt::Display,
{
    let store = open().map_err(|err| format!("post-replay store verification: {err}"))?;
    total(&store).map_err(|err| format!("post-replay store query: {err}"))
}

/// Proof a pull actually replayed into the rebuilt store. The host constructs it only after the
/// cursor commit returned success, so `cursor_committed == false` can never exist on a legitimate
/// path; the field remains as a defensive re-check.
#[derive(Debug, Clone)]
pub struct ReplayReceipt {
    /// The scope the host validated for this pull.
    pub scope: String,
    /// The consent epoch captured at pull start.
    pub consent_generation: u64,
    /// True only after the new cursor was durably persisted.
    pub cursor_committed: bool,
}

/// One recorded source to destination move in the quarantine transaction. An earlier build wrote its
/// pairs to the marker BEFORE the first move executed, so a crash mid-move left durable intent the
/// next open finishes or rolls back. Completion is derivable (destination exists and source gone).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct QuarantineMove {
    #[serde(default)]
    source: String,
    #[serde(default)]
    destination: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RecoveryMarker {
    /// Marker schema version; unknown versions are treated as corrupt.
    v: u8,
    /// Why the most recent quarantine happened (the host's reason words).
    #[serde(default)]
    reason: String,
    #[serde(default)]
    created_at_ms: u64,
    /// SHA-256 hex of the consent-authorized sync scope captured when the marker was FIRST created:
    /// the durable record of which account's zone this store's rows belonged to. Empty = unknown
    /// (consent never granted or the state file did not migrate): never authorizes a replay or a
    /// completion, and is never filled in later; a scope authorized after the fact is a different
    /// consent epoch.
    #[serde(default)]
    bound_scope_digest: String,
    /// Legacy runtime provenance, retained for compatibility. Never compared across restarts or
    /// substituted for the independently versioned durable binding below.
    #[serde(default)]
    consent_generation: u64,
    /// Independently versioned durable provenance. Legacy markers lack this field: their
    /// `consent_generation` is runtime-only and cannot authorize completion after restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    durable_consent: Option<DurableConsentBinding>,
    /// `pending` while a bound replay is still owed; `retained` once it completed.
    #[serde(default = "default_state")]
    state: String,
    /// Completed moves still awaiting the replay verdict: inert ciphertext the bound pull has not
    /// yet accounted for.
    #[serde(default)]
    quarantined: Vec<String>,
    /// Recorded-but-unfinished moves of a quarantine an earlier build made. Drained by the next open
    /// before it touches the database set; empty in every marker this build writes.
    #[serde(default)]
    pairs: Vec<QuarantineMove>,
    /// Paths the bound replay already accounted for: kept permanently, never deleted by this code
    /// path, and never re-entering `quarantined`.
    #[serde(default)]
    retained: Vec<String>,
}

fn default_state() -> String {
    STATE_PENDING.to_string()
}

/// User-facing view of the recovery marker: what happened to the store and where the preserved
/// (never deleted) encrypted backup lives. Contains no key material and no content.
#[derive(Debug, Clone, Serialize)]
pub struct RecoveryStatus {
    /// The reason words the host recorded (empty if unknown).
    pub reason: String,
    pub created_at_ms: u64,
    /// `pending` (replay still owed) or `retained`.
    pub state: String,
    /// Encrypted backup files kept on disk, with their sizes in bytes (`None` when the file is no
    /// longer present).
    pub backups: Vec<RecoveryBackup>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecoveryBackup {
    pub path: String,
    pub bytes: Option<u64>,
}

fn load_marker(path: &Path) -> Option<RecoveryMarker> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice::<RecoveryMarker>(&bytes)
        .ok()
        .filter(|marker| marker.v == 1)
}

/// The digest that binds the marker to a scope: SHA-256 hex of the scope's UTF-8 bytes.
fn scope_digest(scope: &str) -> String {
    rightkit_fs::digest::sha256_bytes(scope.as_bytes())
}

/// The recovery marker of one store: `new` takes the host, every operation takes the lock that
/// serializes marker reads, writes and the recorded-move execution.
pub struct RecoveryLedger<H: RecoveryHost> {
    host: H,
    lock: std::sync::Mutex<()>,
    recorded: AtomicBool,
}

impl<H: RecoveryHost> RecoveryLedger<H> {
    pub const fn new(host: H) -> Self {
        Self {
            host,
            lock: std::sync::Mutex::new(()),
            recorded: AtomicBool::new(false),
        }
    }

    pub fn host(&self) -> &H {
        &self.host
    }

    fn marker_path(&self) -> PathBuf {
        self.host.marker_path()
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, ()>, String> {
        self.lock
            .lock()
            .map_err(|_| "history recovery lock poisoned".to_string())
    }

    /// Durable marker write: temp file, fsync, rename, parent-directory fsync (unix). The marker IS
    /// the transaction record: without directory-entry durability a crash between a move and the
    /// rename landing could strand an unrecorded quarantine, so an unsyncable marker aborts the
    /// quarantine: no durable marker, no moves.
    fn write_marker(&self, marker: &RecoveryMarker) -> Result<(), String> {
        let path = self.marker_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("create recovery marker dir: {err}"))?;
        }
        let bytes = serde_json::to_vec(marker)
            .map_err(|err| format!("serialize recovery marker: {err}"))?;
        // rightkit-fs: temp file, fsync, rename over, directory fsync; the old marker survives a
        // failure and no temp file is left behind.
        let options = rightkit_fs::atomic::AtomicWriteOptions {
            create_parents: true,
            ..Default::default()
        };
        rightkit_fs::atomic::atomic_write_with_options(&path, options, |file| {
            std::io::Write::write_all(file, &bytes)
        })
        .map_err(|error| match error {
            // `ReplacedButDirSyncFailed` is deliberately an error: the directory-fsync failure
            // after a successful rename is returned.
            rightkit_fs::FsError::Io { source, .. }
            | rightkit_fs::FsError::ReplacedButDirSyncFailed { source, .. } => {
                format!("commit recovery marker: {source}")
            }
            other => format!("commit recovery marker: {other}"),
        })
    }

    /// The marker as written on disk, strictly: `Ok(None)` when there is none, `Err` when it exists
    /// and is unreadable or of an unknown version.
    fn read_marker_strict(&self) -> Result<Option<RecoveryMarker>, String> {
        let path = self.marker_path();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(format!("read recovery marker {}: {err}", path.display())),
        };
        match serde_json::from_slice::<RecoveryMarker>(&bytes) {
            Ok(marker) if marker.v == 1 => Ok(Some(marker)),
            _ => Err(format!(
                "unreadable recovery marker {}: preserving all database files until the recorded transaction is resolved",
                path.display()
            )),
        }
    }

    /// The marker's `pairs` (the recorded moves of a quarantine an EARLIER build made with its own
    /// move code). On every open, before it opens anything, the database owner asks for the pairs
    /// that belong to the database's file set and finishes or rolls them back, then reports what it
    /// consumed through [`Self::settle_pairs`]:
    ///
    /// - an ABSENT marker is an ordinary first run;
    /// - an EXISTING marker that cannot be read or carries an unknown schema version may hold
    ///   transaction intent this open cannot see, so this fails: the open fails closed, and no
    ///   database, sidecar or marker bytes are mutated.
    pub fn pending_pairs(&self) -> Result<Vec<RecordedPair>, String> {
        // Read under the lock: a concurrent quarantine may be writing the marker.
        let _guard = self.lock()?;
        Ok(self
            .read_marker_strict()?
            .map(|marker| {
                marker
                    .pairs
                    .into_iter()
                    .map(|pair| RecordedPair {
                        source: PathBuf::from(pair.source),
                        destination: PathBuf::from(pair.destination),
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// [`Self::pending_pairs`] as `(source, destination)` tuples (see [`RecordedPair::into_paths`]).
    pub fn pending_path_pairs(&self) -> Result<Vec<(PathBuf, PathBuf)>, String> {
        Ok(self
            .pending_pairs()?
            .into_iter()
            .map(RecordedPair::into_paths)
            .collect())
    }

    /// [`Self::settle_pairs`] taking `(source, destination)` tuples.
    pub fn settle_path_pairs(
        &self,
        consumed: &[(PathBuf, PathBuf)],
        landed: &[PathBuf],
    ) -> Result<(), String> {
        let consumed: Vec<RecordedPair> = consumed
            .iter()
            .map(|(source, destination)| RecordedPair {
                source: source.clone(),
                destination: destination.clone(),
            })
            .collect();
        self.settle_pairs(&consumed, landed)
    }

    /// The consumed pairs are dropped and the destinations that landed join the `quarantined`
    /// ledger (or `retained` when the marker already completed a replay).
    pub fn settle_pairs(
        &self,
        consumed: &[RecordedPair],
        landed: &[PathBuf],
    ) -> Result<(), String> {
        let _guard = self.lock()?;
        let Some(mut marker) = self.read_marker_strict()? else {
            return Ok(());
        };
        marker.pairs.retain(|pair| {
            !consumed.iter().any(|done| {
                done.source == PathBuf::from(&pair.source)
                    && done.destination == PathBuf::from(&pair.destination)
            })
        });
        let ledger = if marker.state == STATE_RETAINED {
            &mut marker.retained
        } else {
            &mut marker.quarantined
        };
        for destination in landed {
            let destination = destination.to_string_lossy().into_owned();
            if !ledger.contains(&destination) {
                ledger.push(destination);
            }
        }
        self.write_marker(&marker)
    }

    /// Record a quarantine durably. The database owner calls it inside its quarantine transaction,
    /// under its exclusive store guard, before the first file moves, with every destination the
    /// quarantine will take (also for an interrupted quarantine it is about to finish, and for
    /// orphan sidecars). An `Err` must stop the quarantine with every file where it was. Idempotent
    /// by destination: a destination the marker already lists (as `quarantined`, `retained` or a
    /// recorded pair) is not listed again.
    ///
    /// A new set binds the consent state that is current NOW (scope digest and epoch) unless files
    /// still await their replay verdict, in which case the marker keeps its original binding (a
    /// scope authorized later can never adopt an unknown-owner quarantine). `reason` is the host's
    /// words for why (`None` keeps the last one). Nothing is deleted.
    pub fn record_quarantine(
        &self,
        destinations: &[PathBuf],
        reason: Option<&str>,
    ) -> Result<(), String> {
        // Consent lock precedes marker lock at finalization. Capture before locking here to
        // avoid the reverse order; a later revocation makes this captured epoch inert.
        let consent = self.host.recovery_consent();
        let _guard = self.lock()?;
        // An unreadable marker may hold pending files or move intent. Never replace it with an
        // empty ledger: the caller must leave its database files in place on this error.
        let mut marker = self.read_marker_strict()?.unwrap_or_default();
        let fresh: Vec<String> = destinations
            .iter()
            .map(|destination| destination.to_string_lossy().into_owned())
            .filter(|path| {
                !marker.quarantined.contains(path)
                    && !marker.retained.contains(path)
                    && !marker.pairs.iter().any(|pair| &pair.destination == path)
            })
            .collect();
        if fresh.is_empty() {
            return Ok(());
        }
        let stamp = self.host.now_ms();
        marker.v = 1;
        if let Some(reason) = reason {
            marker.reason = reason.to_string();
        }
        marker.state = STATE_PENDING.to_string();
        if marker.quarantined.is_empty() && marker.pairs.is_empty() {
            marker.created_at_ms = stamp;
            marker.consent_generation = self.host.consent_epoch();
            marker.bound_scope_digest = consent
                .as_ref()
                .map(|consent| scope_digest(&consent.scope))
                .or_else(|| {
                    self.host
                        .authorized_scope()
                        .map(|scope| scope_digest(&scope))
                })
                .unwrap_or_default();
            marker.durable_consent = consent.map(|consent| DurableConsentBinding {
                v: 1,
                epoch: consent.epoch,
            });
        }
        let recorded = fresh.len();
        marker.quarantined.extend(fresh);
        self.write_marker(&marker)?;
        self.host.event(
            LedgerEvent::Quarantined,
            &format!(
                "reason={} recorded={} listed={}",
                marker.reason,
                recorded,
                marker.quarantined.len()
            ),
        );
        self.recorded.store(true, Ordering::Release);
        Ok(())
    }

    /// True once if a quarantine was recorded since the last call. The host calls it after the
    /// store is open, never inside the quarantine transaction, to request the restoring pull.
    pub fn take_recorded(&self) -> bool {
        self.recorded.swap(false, Ordering::AcqRel)
    }

    /// Read-only status of the last recovery, or `None` when no marker exists. Never mutates the
    /// marker or the backups.
    pub fn status(&self) -> Option<RecoveryStatus> {
        let marker = load_marker(&self.marker_path())?;
        let mut paths: Vec<String> = Vec::new();
        for path in marker
            .quarantined
            .iter()
            .chain(marker.retained.iter())
            .cloned()
            .chain(marker.pairs.iter().map(|pair| pair.destination.clone()))
        {
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
        let backups = paths
            .into_iter()
            .map(|path| RecoveryBackup {
                bytes: std::fs::metadata(&path).ok().map(|meta| meta.len()),
                path,
            })
            .collect();
        Some(RecoveryStatus {
            reason: marker.reason,
            created_at_ms: marker.created_at_ms,
            state: marker.state,
            backups,
        })
    }

    /// Plain-text diagnostics a user can attach to a support request. Key and content are never
    /// included. `label` names the store in the text (for example `"<Product> history recovery"`).
    pub fn diagnostics(&self, label: &str) -> String {
        let Some(status) = self.status() else {
            return format!("{label}: no recovery event recorded.\n");
        };
        let mut out = format!("{label} diagnostics\n");
        out.push_str(&format!("reason: {}\n", status.reason));
        out.push_str(&format!("created_at_ms: {}\n", status.created_at_ms));
        out.push_str(&format!("state: {}\n", status.state));
        for backup in &status.backups {
            match backup.bytes {
                Some(bytes) => out.push_str(&format!("backup: {} ({bytes} bytes)\n", backup.path)),
                None => out.push_str(&format!("backup: {} (missing)\n", backup.path)),
            }
        }
        out
    }

    /// Pending recovery, or `None`. A corrupt or unknown-version marker is LEFT IN PLACE:
    /// [`Self::pending_pairs`] owns the fail-closed decision at open time, and moving the file would
    /// hide the evidence it guards. `retained` markers are deliberately not pending.
    fn pending_marker(&self) -> Option<RecoveryMarker> {
        let path = self.marker_path();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
            Err(err) => {
                self.host
                    .event(LedgerEvent::MarkerReadFailed, &format!("{err}"));
                return None;
            }
        };
        match serde_json::from_slice::<RecoveryMarker>(&bytes) {
            Ok(marker) if marker.v == 1 && marker.state == STATE_PENDING => Some(marker),
            Ok(_) => None,
            _ => {
                self.host.event(
                    LedgerEvent::MarkerUnreadable,
                    "unreadable marker left in place",
                );
                None
            }
        }
    }

    /// Whether a store rebuild is waiting on a replay for `scope`: a pending marker exists AND the
    /// pull's authorized scope digests to the scope that was consent-authorized when the store was
    /// quarantined. A different account's pull, or a marker with no binding, returns false: neither
    /// the replay request nor the completion may run for a scope that did not own the data.
    pub fn pending_for_scope(&self, scope: &str) -> bool {
        let Some(marker) = self.pending_marker() else {
            return false;
        };
        !marker.bound_scope_digest.is_empty() && marker.bound_scope_digest == scope_digest(scope)
    }

    /// Called only after a pull whose merge AND cursor commit succeeded.
    ///
    /// Completion requires all of:
    /// - a `pending` marker,
    /// - `receipt.cursor_committed`,
    /// - `receipt.scope` digesting to the marker's `bound_scope_digest` (the account that owned the
    ///   quarantined store's zone; unknown or mismatch keeps everything pending),
    /// - independently versioned durable consent provenance matching the owner's current scope
    ///   and epoch, plus `receipt.consent_generation` matching the live runtime generation,
    /// - the owner's atomic commit admission ordering marker I/O before durable revoke,
    /// - the replacement store reopening cleanly (`RecoveryHost::verify_replacement`).
    ///
    /// On success the pending `quarantined` set moves into the `retained` ledger and the marker
    /// completes. NOTHING IS DELETED. Every rejection keeps the marker pending so a later pull can
    /// satisfy the proof; replays dedupe by delivery id.
    pub fn finish_after_replay(&self, receipt: &ReplayReceipt) -> Result<(), String> {
        let Some(snapshot) = self.pending_marker() else {
            return Ok(());
        };
        if !receipt.cursor_committed {
            self.host
                .event(LedgerEvent::FinalizeRejected, "reason=uncommitted");
            return Ok(());
        }
        if snapshot.bound_scope_digest.is_empty()
            || snapshot.bound_scope_digest != scope_digest(&receipt.scope)
        {
            self.host.event(
                LedgerEvent::FinalizeRejected,
                "reason=scope_unbound_or_mismatch",
            );
            return Ok(());
        }
        let Some(binding) = snapshot
            .durable_consent
            .as_ref()
            .filter(|binding| binding.v == 1)
        else {
            self.host.event(
                LedgerEvent::FinalizeRejected,
                "reason=durable_consent_unproven",
            );
            return Ok(());
        };
        if receipt.consent_generation != self.host.consent_epoch() {
            self.host
                .event(LedgerEvent::FinalizeRejected, "reason=consent_changed");
            return Ok(());
        }
        // Verification reopens the live store, whose open path can itself quarantine, and
        // quarantine takes the lock. Verify BEFORE locking, so this function can never self-deadlock
        // on a nested recovery.
        let restored_total = self.host.verify_replacement()?;
        let consent = RecoveryConsent {
            scope: receipt.scope.clone(),
            epoch: binding.epoch,
        };
        let mut commit = || self.commit_replay_snapshot(receipt, &snapshot, restored_total);
        if !self
            .host
            .commit_recovery_if_authorized(receipt, &consent, &mut commit)?
        {
            self.host.event(
                LedgerEvent::FinalizeRejected,
                "reason=consent_commit_unavailable_or_changed",
            );
        }
        Ok(())
    }

    fn commit_replay_snapshot(
        &self,
        receipt: &ReplayReceipt,
        snapshot: &RecoveryMarker,
        restored_total: u32,
    ) -> Result<(), String> {
        let _guard = self.lock()?;
        // Re-validate under the lock: the snapshot predates verification, so a concurrent finalize
        // may have retained the marker, or a new quarantine may have replaced it. If the on-disk
        // marker no longer carries the binding this receipt proved against, do not consume it.
        let Some(mut current) = self.read_marker_strict()? else {
            self.host
                .event(LedgerEvent::FinalizeRejected, "reason=marker_vanished");
            return Ok(());
        };
        if current.state != STATE_PENDING
            || current.bound_scope_digest != snapshot.bound_scope_digest
            || current.consent_generation != snapshot.consent_generation
            || current
                .durable_consent
                .as_ref()
                .map(|binding| (binding.v, binding.epoch))
                != snapshot
                    .durable_consent
                    .as_ref()
                    .map(|binding| (binding.v, binding.epoch))
        {
            self.host
                .event(LedgerEvent::FinalizeRejected, "reason=marker_changed");
            return Ok(());
        }
        // Opening/querying the replacement can yield to consent/account changes. The receipt's
        // authority must still be live after that call, before any pending paths are consumed.
        if self
            .host
            .authorized_scope()
            .map(|scope| scope_digest(&scope))
            .as_deref()
            != Some(current.bound_scope_digest.as_str())
        {
            self.host.event(
                LedgerEvent::FinalizeRejected,
                "reason=scope_unbound_or_mismatch",
            );
            return Ok(());
        }
        if receipt.consent_generation != self.host.consent_epoch() {
            self.host
                .event(LedgerEvent::FinalizeRejected, "reason=consent_changed");
            return Ok(());
        }
        // The snapshot's pending set moves to `retained`: the files are kept; the marker completes
        // so future pulls stop requesting a replay. A nested quarantine during verification may have
        // appended NEWER paths: only the snapshot's paths are proven consumed by this replay, so
        // leftovers stay pending (a replay is still owed for them). Recorded pairs stay for the
        // next open's settle, which drains them straight into `retained` when the state is
        // terminal.
        let mut remaining = Vec::new();
        let mut retained_now = 0usize;
        for path in current.quarantined.drain(..) {
            if snapshot.quarantined.contains(&path) {
                current.retained.push(path);
                retained_now += 1;
            } else {
                remaining.push(path);
            }
        }
        current.quarantined = remaining;
        current.state = if current.quarantined.is_empty() {
            STATE_RETAINED
        } else {
            STATE_PENDING
        }
        .to_string();
        self.write_marker(&current)?;
        self.host.event(
            LedgerEvent::Completed,
            &format!("retained_files={retained_now} verified_total={restored_total}"),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use std::sync::Mutex;

    static NEXT: AtomicU64 = AtomicU64::new(0);

    #[derive(Clone, Copy)]
    enum DuringVerification {
        Unchanged,
        Revoke,
        SwitchScope,
        CorruptMarker,
        Unsupported,
    }

    struct Host {
        root: PathBuf,
        epoch: AtomicU64,
        durable_epoch: AtomicU64,
        scope: Mutex<Option<String>>,
        consent_commit: Mutex<()>,
        during_verification: DuringVerification,
    }

    impl Host {
        fn new(during_verification: DuringVerification) -> Self {
            let root = std::env::temp_dir().join(format!(
                "rightkit-recovery-marker-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&root).unwrap();
            Self {
                root,
                epoch: AtomicU64::new(7),
                durable_epoch: AtomicU64::new(41),
                scope: Mutex::new(Some("owner".to_string())),
                consent_commit: Mutex::new(()),
                during_verification,
            }
        }
    }

    impl Drop for Host {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    impl RecoveryHost for Host {
        fn marker_path(&self) -> PathBuf {
            self.root.join("marker.json")
        }

        fn now_ms(&self) -> u64 {
            100
        }

        fn authorized_scope(&self) -> Option<String> {
            self.scope.lock().unwrap().clone()
        }

        fn consent_epoch(&self) -> u64 {
            self.epoch.load(Ordering::Acquire)
        }

        fn recovery_consent(&self) -> Option<RecoveryConsent> {
            Some(RecoveryConsent {
                scope: self.authorized_scope()?,
                epoch: self.durable_epoch.load(Ordering::Acquire),
            })
        }

        fn verify_replacement(&self) -> Result<u32, String> {
            match self.during_verification {
                DuringVerification::Unchanged | DuringVerification::Unsupported => {}
                DuringVerification::Revoke => {
                    self.epoch.fetch_add(1, Ordering::AcqRel);
                }
                DuringVerification::SwitchScope => {
                    *self.scope.lock().unwrap() = Some("other-owner".to_string());
                }
                DuringVerification::CorruptMarker => {
                    std::fs::write(self.marker_path(), b"broken marker").unwrap();
                }
            }
            Ok(3)
        }

        fn commit_recovery_if_authorized(
            &self,
            receipt: &ReplayReceipt,
            consent: &RecoveryConsent,
            commit: &mut dyn FnMut() -> Result<(), String>,
        ) -> Result<bool, String> {
            if matches!(self.during_verification, DuringVerification::Unsupported) {
                return Ok(false);
            }
            let _guard = self.consent_commit.lock().unwrap();
            if self.consent_epoch() != receipt.consent_generation
                || self.authorized_scope().as_deref() != Some(receipt.scope.as_str())
                || self.durable_epoch.load(Ordering::Acquire) != consent.epoch
            {
                return Ok(false);
            }
            commit()?;
            Ok(true)
        }

        fn event(&self, _event: LedgerEvent, _detail: &str) {}
    }

    fn receipt() -> ReplayReceipt {
        ReplayReceipt {
            scope: "owner".to_string(),
            consent_generation: 7,
            cursor_committed: true,
        }
    }

    #[test]
    fn quarantine_preserves_invalid_marker_bytes() {
        for bytes in [b"broken marker".as_slice(), b"{\"v\":2}"] {
            let ledger = RecoveryLedger::new(Host::new(DuringVerification::Unchanged));
            std::fs::write(ledger.marker_path(), bytes).unwrap();
            assert!(ledger
                .record_quarantine(&[ledger.host.root.join("backup")], None)
                .is_err());
            assert_eq!(std::fs::read(ledger.marker_path()).unwrap(), bytes);
            assert!(!ledger.take_recorded());
        }
    }

    #[test]
    fn quarantine_preserves_unreadable_marker() {
        let ledger = RecoveryLedger::new(Host::new(DuringVerification::Unchanged));
        std::fs::create_dir(ledger.marker_path()).unwrap();
        assert!(ledger
            .record_quarantine(&[ledger.host.root.join("backup")], None)
            .is_err());
        assert!(ledger.marker_path().is_dir());
        assert!(!ledger.take_recorded());
    }

    #[test]
    fn absent_marker_records_once_and_matching_replay_retains() {
        let ledger = RecoveryLedger::new(Host::new(DuringVerification::Unchanged));
        let backup = ledger.host.root.join("backup");
        ledger
            .record_quarantine(std::slice::from_ref(&backup), None)
            .unwrap();
        assert!(ledger.take_recorded());
        let before = std::fs::read(ledger.marker_path()).unwrap();
        ledger
            .record_quarantine(&[backup], Some("changed"))
            .unwrap();
        assert_eq!(std::fs::read(ledger.marker_path()).unwrap(), before);
        assert!(!ledger.take_recorded());
        ledger.finish_after_replay(&receipt()).unwrap();
        let marker = ledger.read_marker_strict().unwrap().unwrap();
        assert_eq!(marker.state, STATE_RETAINED);
        assert!(marker.quarantined.is_empty());
        assert_eq!(marker.retained.len(), 1);
    }

    #[test]
    fn queued_pairs_keep_original_unbound_consent() {
        let ledger = RecoveryLedger::new(Host::new(DuringVerification::Unchanged));
        let marker = RecoveryMarker {
            v: 1,
            state: STATE_PENDING.to_string(),
            pairs: vec![QuarantineMove {
                source: "old-store".to_string(),
                destination: "old-backup".to_string(),
            }],
            ..Default::default()
        };
        ledger.write_marker(&marker).unwrap();
        let before = std::fs::read(ledger.marker_path()).unwrap();
        ledger
            .record_quarantine(&[PathBuf::from("old-backup")], None)
            .unwrap();
        assert_eq!(std::fs::read(ledger.marker_path()).unwrap(), before);
        ledger
            .record_quarantine(&[ledger.host.root.join("new-backup")], None)
            .unwrap();
        let current = ledger.read_marker_strict().unwrap().unwrap();
        assert!(current.bound_scope_digest.is_empty());
        assert_eq!(current.consent_generation, marker.consent_generation);
        assert_eq!(current.pairs.len(), 1);
    }

    #[test]
    fn consent_changes_during_verification_leave_pending_bytes_unchanged() {
        for change in [DuringVerification::Revoke, DuringVerification::SwitchScope] {
            let ledger = RecoveryLedger::new(Host::new(change));
            ledger
                .record_quarantine(&[ledger.host.root.join("backup")], None)
                .unwrap();
            let before = std::fs::read(ledger.marker_path()).unwrap();
            ledger.finish_after_replay(&receipt()).unwrap();
            assert_eq!(std::fs::read(ledger.marker_path()).unwrap(), before);
        }
    }

    #[test]
    fn corruption_during_verification_is_reported_and_preserved() {
        let ledger = RecoveryLedger::new(Host::new(DuringVerification::CorruptMarker));
        ledger
            .record_quarantine(&[ledger.host.root.join("backup")], None)
            .unwrap();
        assert!(ledger.finish_after_replay(&receipt()).is_err());
        assert_eq!(
            std::fs::read(ledger.marker_path()).unwrap(),
            b"broken marker"
        );
    }

    #[test]
    fn unsupported_consent_commit_keeps_recovery_pending() {
        let ledger = RecoveryLedger::new(Host::new(DuringVerification::Unsupported));
        ledger
            .record_quarantine(&[ledger.host.root.join("backup")], None)
            .unwrap();
        let before = std::fs::read(ledger.marker_path()).unwrap();
        ledger.finish_after_replay(&receipt()).unwrap();
        assert_eq!(std::fs::read(ledger.marker_path()).unwrap(), before);
    }

    #[test]
    fn restart_uses_durable_epoch_instead_of_recorded_runtime_generation() {
        let ledger = RecoveryLedger::new(Host::new(DuringVerification::Unchanged));
        ledger
            .record_quarantine(&[ledger.host.root.join("backup")], None)
            .unwrap();
        let host = ledger.host;
        host.epoch.store(0, Ordering::Release);
        let restarted = RecoveryLedger::new(host);
        let mut replay = receipt();
        replay.consent_generation = 0;
        restarted.finish_after_replay(&replay).unwrap();
        assert_eq!(restarted.status().unwrap().state, STATE_RETAINED);
    }

    #[test]
    fn restart_cannot_adopt_another_durable_epoch_with_same_runtime_generation() {
        let ledger = RecoveryLedger::new(Host::new(DuringVerification::Unchanged));
        ledger
            .record_quarantine(&[ledger.host.root.join("backup")], None)
            .unwrap();
        let before = std::fs::read(ledger.marker_path()).unwrap();
        let host = ledger.host;
        host.durable_epoch.fetch_add(1, Ordering::AcqRel);
        let restarted = RecoveryLedger::new(host);
        restarted.finish_after_replay(&receipt()).unwrap();
        assert_eq!(std::fs::read(restarted.marker_path()).unwrap(), before);
    }

    #[test]
    fn legacy_runtime_provenance_stays_readable_without_becoming_durable() {
        let ledger = RecoveryLedger::new(Host::new(DuringVerification::Unchanged));
        let marker = RecoveryMarker {
            v: 1,
            state: STATE_PENDING.to_string(),
            bound_scope_digest: scope_digest("owner"),
            consent_generation: 7,
            quarantined: vec!["old-backup".to_string()],
            ..Default::default()
        };
        ledger.write_marker(&marker).unwrap();
        let before = std::fs::read(ledger.marker_path()).unwrap();
        assert_eq!(ledger.status().unwrap().state, STATE_PENDING);
        ledger.finish_after_replay(&receipt()).unwrap();
        assert_eq!(std::fs::read(ledger.marker_path()).unwrap(), before);
    }
}
