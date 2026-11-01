//! Recovery for a history database the current credential cannot unlock —
//! the ordinary machine-migration case: the encrypted DB travels through
//! Time Machine / Migration Assistant while the Keychain / Credential
//! Manager item holding its key does not (incident 2026-10-01: Air → Mini
//! left a migrated DB that no local key could open, and recovery needed
//! manual database surgery).
//!
//! `open_and_key` (sqlcipher_conn.rs) classifies the failure before deciding:
//! an absent credential means no key exists for this install; a stored key
//! that fails SQLITE_NOTADB / SQLITE_CORRUPT cannot decipher THIS file (wrong
//! key for it, or on-disk corruption — corruption is not proof the original
//! key is permanently gone); every other credential-store error (locked,
//! denied, unavailable) is transient and surfaces unchanged with the file
//! untouched.
//!
//! Quarantine is a recorded transaction, not a bare rename sequence: the
//! durable marker lists every source→destination pair BEFORE the first move,
//! and the moves themselves are non-clobbering (hard_link + unlink — never
//! `rename`, which silently overwrites on Unix). A crash or partial failure
//! leaves a completable record — every subsequent open runs
//! `complete_pending_quarantine` first, which finishes the recorded set or
//! rolls it back to the live names, so a missing main DB can never meet a
//! still-live predecessor WAL.
//!
//! On macOS, a consented pull whose validated scope digests to the marker's
//! bound scope deletes the scoped zone-change token (full replay), and
//! `finish_recovery_after_replay` — reached only after merge + cursor commit
//! — moves the pending quarantine set into the marker's `retained` ledger.
//! That is where it ends: zone coverage proves the zone contained items but
//! cannot prove EVERY unsynced predecessor row was in it, so the quarantined
//! ciphertext is retained internally, never deleted by this code path. There
//! is no customer-facing cleanup and no claim of restoration completeness.
//!
//! Boundaries kept deliberately conservative:
//! - a marker with no recorded scope (never consented / consent state didn't
//!   migrate) never authorizes cursor resets or completion;
//! - a marker that begins unbound STAYS unbound — a scope authorized later
//!   is a different consent epoch and must not adopt an older unknown-owner
//!   quarantine;
//! - if sync consent is off or unreachable, marker + files remain (inert
//!   ciphertext) until a matching-scope replay completes.

use crate::history::HistoryError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Durable record of a quarantined history store. Consumed by the macOS
/// CloudKit pull path (`cloud_sync.rs`): a pending marker bound to the pull's
/// authorized scope makes that pull reset the scoped zone token (full
/// replay), then complete only after the cursor commits.
const RECOVERY_MARKER_FILE: &str = "history-recovery.json";

/// Marker states. `pending` = a bound replay is still owed; `retained` = a
/// bound replay completed, so the quarantined set moved to the `retained`
/// ledger — files kept permanently, no more cursor resets.
const STATE_PENDING: &str = "pending";
const STATE_RETAINED: &str = "retained";

/// Serializes marker reads/writes and the recorded-move execution. Never
/// held across `open_and_key` itself (that is `OPEN_TRANSITION_LOCK`'s job).
static RECOVERY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Serializes the whole classify → complete-pending → mint → quarantine →
/// open transition in `open_and_key`. Without it, a background-sync open
/// could observe "no key" while the foreground's mint is in flight
/// (competing keys), or quarantine a replacement store another opener just
/// created. Store opens are rare, so one process-wide lock is enough and
/// strictly simpler than a per-path map. Lock order: TRANSITION → RECOVERY
/// (quarantine), never the reverse — `finish_recovery_after_replay` only
/// takes RECOVERY.
static OPEN_TRANSITION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Held for the entire open transition; see `OPEN_TRANSITION_LOCK`.
pub(super) fn transition_guard() -> Result<std::sync::MutexGuard<'static, ()>, HistoryError> {
    OPEN_TRANSITION_LOCK
        .lock()
        .map_err(|_| HistoryError::new("history open transition lock poisoned"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RecoveryReason {
    /// No credential exists on this machine (errSecItemNotFound /
    /// ERROR_NOT_FOUND) while a real database does: the classic migrated-Mac
    /// case.
    KeychainKeyAbsent,
    /// A credential exists but cannot decipher this file (SQLITE_NOTADB /
    /// SQLITE_CORRUPT): DB from an install with a different key, or on-disk
    /// corruption. Says nothing about whether the original key is lost for
    /// good — the file is preserved either way.
    StoredKeyRejected,
}

impl RecoveryReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::KeychainKeyAbsent => "keychain_key_absent",
            Self::StoredKeyRejected => "stored_key_rejected",
        }
    }
}

/// One recorded source→destination move in the quarantine transaction.
/// Pairs are written to the marker BEFORE the first move executes, so a
/// crash mid-move leaves durable intent the next open can finish or roll
/// back. Completion is derivable (destination exists && source gone) and
/// needs no flag.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct QuarantineMove {
    #[serde(default)]
    source: String,
    #[serde(default)]
    destination: String,
}

/// Proof a CloudKit pull actually replayed into the rebuilt store.
/// `cloud_sync_pull` constructs it only after `CommitPull` returns success,
/// so a receipt whose `cursor_committed` is false can never exist on a
/// legitimate path — the field remains as a defensive re-check.
#[derive(Debug, Clone)]
pub struct ReplayReceipt {
    /// The scope `authorize_sync_scope` validated for this pull.
    pub scope: String,
    /// `cloud_sync_scheduler::consent_generation()` captured at pull start.
    pub consent_generation: u64,
    /// True only after `CommitPull` durably persisted the new cursor.
    pub cursor_committed: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct HistoryRecoveryMarker {
    /// Marker schema version; unknown versions are treated as corrupt.
    v: u8,
    /// `RecoveryReason::as_str` of the most recent quarantine.
    #[serde(default)]
    reason: String,
    #[serde(default)]
    created_at_ms: u64,
    /// SHA-256 hex of the consent-authorized sync scope
    /// (`cloud-sync-state.json`'s `authorized_scope`) captured when the
    /// marker was FIRST created — the durable record of which iCloud
    /// account's zone this DB's rows belonged to. Empty = unknown (consent
    /// never granted or the state file didn't migrate): never authorizes
    /// cursor resets or completion, and is never filled in later — a scope
    /// authorized after the fact is a different consent epoch.
    #[serde(default)]
    bound_scope_digest: String,
    /// `cloud_sync_scheduler::consent_generation()` captured with the scope
    /// binding — the consent epoch the quarantine was recorded under. A
    /// sync enable/disable between quarantine and replay means the replay
    /// no longer runs under the consent this DB was quarantined with.
    #[serde(default)]
    consent_generation: u64,
    /// `pending` while a bound replay is still owed; `retained` once the
    /// bound committed replay has completed.
    #[serde(default = "default_state")]
    state: String,
    /// Completed moves still awaiting the replay verdict — inert ciphertext
    /// the bound pull has not yet accounted for.
    #[serde(default)]
    quarantined: Vec<String>,
    /// Recorded-but-unfinished moves — the open transaction. Written durable
    /// before any move runs; drained by the next open before it touches the
    /// database set.
    #[serde(default)]
    pairs: Vec<QuarantineMove>,
    /// Paths the bound replay already accounted for — kept permanently,
    /// never deleted by this code path, and never re-entering `quarantined`.
    #[serde(default)]
    retained: Vec<String>,
}

fn default_state() -> String {
    STATE_PENDING.to_string()
}

fn marker_path() -> PathBuf {
    crate::host::host().data_file(RECOVERY_MARKER_FILE)
}

/// Fsync a directory's entry table to stable storage (unix). On platforms
/// where directories cannot be fsynced through `std` this is a no-op —
/// durability there is still enforced at the file level.
fn sync_dir(dir: &Path) -> Result<(), HistoryError> {
    #[cfg(unix)]
    {
        std::fs::File::open(dir)
            .and_then(|d| d.sync_all())
            .map_err(|err| HistoryError::new(format!("sync dir {}: {err}", dir.display())))?;
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
    Ok(())
}

/// Fsync the directory containing `path` — the barrier that makes a
/// directory entry (create/unlink/rename) durable.
fn sync_parent_dir(path: &Path) -> Result<(), HistoryError> {
    match path.parent() {
        Some(parent) => sync_dir(parent),
        None => Ok(()),
    }
}

/// Durability barrier for a directory entry a quarantine move creates or
/// destroys: the destination's entry must be stable BEFORE the source name
/// is removed, and the removal must be stable before the move is done.
/// `RIGHTKIT_DEV_MOVE_SYNC_FAIL` injects a failure for partial-move tests;
/// compiled out of release.
fn flush_move_entry(path: &Path) -> Result<(), HistoryError> {
    #[cfg(feature = "test-mode")]
    if dev_move_sync_fail() {
        return Err(HistoryError::new(format!(
            "sync move dir {}: injected failure (RIGHTKIT_DEV_MOVE_SYNC_FAIL)",
            path.display()
        )));
    }
    sync_parent_dir(path)
}

/// Test-only fault injection for `flush_move_entry`: `RIGHTKIT_DEV_MOVE_SYNC_FAIL=1`
/// fails the pre-unlink (and post-unlink) directory sync so tests can prove
/// a sync failure preserves the original and keeps the transaction recorded.
#[cfg(feature = "test-mode")]
fn dev_move_sync_fail() -> bool {
    matches!(
        std::env::var("RIGHTKIT_DEV_MOVE_SYNC_FAIL").ok().as_deref(),
        Some("1") | Some("true") | Some("on")
    )
}

fn load_marker(path: &Path) -> Option<HistoryRecoveryMarker> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice::<HistoryRecoveryMarker>(&bytes)
        .ok()
        .filter(|marker| marker.v == 1)
}

/// Durable marker write: temp file → fsync → rename → parent-directory fsync
/// (unix), matching `save_ledger`'s crash-safety contract. The marker IS the
/// transaction record — without directory-entry durability a crash between a
/// move and the rename landing could strand an unrecorded quarantine, so an
/// unsyncable marker aborts the quarantine: no durable marker, no moves.
fn write_marker(marker: &HistoryRecoveryMarker) -> Result<(), HistoryError> {
    #[cfg(feature = "test-mode")]
    if dev_marker_fail() {
        return Err(HistoryError::new(
            "write recovery marker: injected failure (RIGHTKIT_DEV_MARKER_FAIL)",
        ));
    }
    let path = marker_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| HistoryError::new(format!("create recovery marker dir: {err}")))?;
    }
    let bytes = serde_json::to_vec(marker)
        .map_err(|err| HistoryError::new(format!("serialize recovery marker: {err}")))?;
    // rightkit-fs: temp file, fsync, rename over, directory fsync; the old
    // marker survives a failure and no temp file is left behind.
    super::atomic::replace_file_atomically(&path, &bytes)
        .map_err(|err| HistoryError::new(format!("commit recovery marker: {err}")))
}

/// Test-only fault injection: `RIGHTKIT_DEV_MARKER_FAIL=1` makes `write_marker`
/// fail before touching disk, proving no file moves without durable intent.
/// Compiled out of release.
#[cfg(feature = "test-mode")]
fn dev_marker_fail() -> bool {
    matches!(
        std::env::var("RIGHTKIT_DEV_MARKER_FAIL").ok().as_deref(),
        Some("1") | Some("true") | Some("on")
    )
}

/// The database file plus every sidecar that can carry committed rows
/// (WAL holds writes not yet checkpointed; -journal covers a rollback-mode
/// remnant; SHM is moved for completeness so a stale index can never pair
/// with a different DB file).
fn db_and_sidecar_paths(db_path: &Path) -> [PathBuf; 4] {
    let sidecar = |suffix: &str| {
        let mut name = db_path.as_os_str().to_os_string();
        name.push(suffix);
        PathBuf::from(name)
    };
    [
        db_path.to_path_buf(),
        sidecar("-wal"),
        sidecar("-shm"),
        sidecar("-journal"),
    ]
}

/// Whether any database or sidecar file with content occupies `db_path`'s
/// name — i.e. a real store predates this open. A 0-byte/absent main file is
/// treated as "no store" (SQLCipher creates it lazily), but a non-empty WAL
/// without a main file still counts: it can hold committed rows.
pub(super) fn db_files_have_content(db_path: &Path) -> Result<bool, HistoryError> {
    for candidate in db_and_sidecar_paths(db_path) {
        match std::fs::metadata(&candidate) {
            Ok(meta) if meta.is_file() && meta.len() > 0 => return Ok(true),
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(HistoryError::new(format!(
                    "stat {}: {err}",
                    candidate.display()
                )))
            }
        }
    }
    Ok(false)
}

/// Test-only fault injection: `RIGHTKIT_DEV_QUARANTINE_FAIL_AFTER=<n>` makes the
/// nth (1-based) `move_no_clobber` in this quarantine/completion call fail,
/// so tests can cover partial moves and rollbacks. Compiled out of release.
#[cfg(feature = "test-mode")]
fn dev_quarantine_fail_after() -> Option<usize> {
    std::env::var("RIGHTKIT_DEV_QUARANTINE_FAIL_AFTER")
        .ok()?
        .parse()
        .ok()
        .filter(|n| *n > 0)
}

/// Move `source` to `destination` without ever overwriting an existing
/// file. `hard_link` creates the destination iff the name is absent —
/// atomic, no exists-then-rename window — and the source name is unlinked
/// afterwards. A crash between the two leaves a hardlinked duplicate; the
/// recorded pair completes on retry (both names present → same-inode proof
/// → unlink source). On filesystems without hardlink support this fails
/// rather than falling back to a clobbering rename: the quarantine aborts
/// and the originals stay live.
///
/// Directory-entry durability: the destination's entry is fsynced BEFORE
/// the source name is destroyed (a crash after an unsynced link could lose
/// BOTH names), and the source's removal is fsynced after unlink so a crash
/// cannot resurrect the live name against an already-quarantined copy.
/// Flush failures propagate: the source name is never destroyed before the
/// destination entry is proven stable.
fn move_no_clobber(source: &Path, destination: &Path) -> Result<(), HistoryError> {
    std::fs::hard_link(source, destination).map_err(|err| {
        HistoryError::new(format!(
            "quarantine link {} -> {}: {err}",
            source.display(),
            destination.display()
        ))
    })?;
    flush_move_entry(destination)?;
    std::fs::remove_file(source).map_err(|err| {
        HistoryError::new(format!("quarantine unlink {}: {err}", source.display()))
    })?;
    flush_move_entry(source)
}

/// A quarantine destination that can never collide with a file on disk or
/// with a destination this marker already claims: `name.unreadable-<ms>`,
/// then `-2`, `-3`, …. Exhaustion fails the quarantine rather than risk an
/// overwrite.
fn quarantine_destination(
    source: &Path,
    stamp: u64,
    claimed: &mut Vec<PathBuf>,
) -> Result<PathBuf, HistoryError> {
    let base = source
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "history-db".to_string());
    for attempt in 0..64u32 {
        let suffix = if attempt == 0 {
            format!("{base}.unreadable-{stamp}")
        } else {
            format!("{base}.unreadable-{stamp}-{}", attempt + 1)
        };
        let candidate = source.with_file_name(suffix);
        if !candidate.exists() && !claimed.contains(&candidate) {
            claimed.push(candidate.clone());
            return Ok(candidate);
        }
    }
    Err(HistoryError::new(format!(
        "quarantine destination exhausted for {}",
        source.display()
    )))
}

/// The scope digest persisted by the Swift helper's token filename —
/// `cloud-sync-zone-token.<sha256(scope-utf8) lowercase hex>.data`. The same
/// digest binds the marker to the consent-authorized scope.
fn scope_digest(scope: &str) -> String {
    rightkit_fs::digest::sha256_bytes(scope.as_bytes())
}

/// The consent-authorized scope's digest at quarantine time — `None` when
/// consent was never granted or the state file did not migrate. This is the
/// "who owns the quarantined zone" evidence: only a pull authorized for the
/// same account can satisfy it.
fn authorized_scope_digest() -> Option<String> {
    crate::host::host()
        .authorized_sync_scope()
        .map(|scope| scope_digest(&scope))
}

/// The live consent epoch (`cloud_sync_scheduler::consent_generation()`).
/// Captured into the marker at quarantine and re-checked at finalize: a
/// consent change anywhere in between means the replay did not run under
/// the consent the quarantine was bound to, so completion is refused.
fn current_consent_generation() -> u64 {
    crate::host::host().consent_generation()
}

/// Whether `source` and `destination` name the SAME filesystem object — the
/// only proof that a recorded pair's `hard_link` half actually landed (the
/// crash-after-link case). Existence alone is not identity: a foreign file
/// can occupy the claimed destination name (collision, reuse), and deleting
/// the source then would destroy the original while keeping unrelated
/// bytes. Proven by inode/device identity; where the platform cannot prove
/// it, fail closed.
fn same_file_identity(source: &Path, destination: &Path) -> Result<bool, HistoryError> {
    #[cfg(not(windows))]
    let a = std::fs::metadata(source)
        .map_err(|err| HistoryError::new(format!("stat {}: {err}", source.display())))?;
    #[cfg(not(windows))]
    let b = std::fs::metadata(destination)
        .map_err(|err| HistoryError::new(format!("stat {}: {err}", destination.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(a.dev() == b.dev() && a.ino() == b.ino())
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            FileIdInfo, GetFileInformationByHandleEx, FILE_ID_INFO,
        };

        fn identity(path: &Path) -> Result<(u64, [u8; 16]), HistoryError> {
            let file = std::fs::File::open(path)
                .map_err(|err| HistoryError::new(format!("open {}: {err}", path.display())))?;
            let mut info = FILE_ID_INFO::default();
            // File owns handle for the call; buffer has exact FILE_ID_INFO size.
            let ok = unsafe {
                GetFileInformationByHandleEx(
                    file.as_raw_handle(),
                    FileIdInfo,
                    (&mut info as *mut FILE_ID_INFO).cast(),
                    std::mem::size_of::<FILE_ID_INFO>() as u32,
                )
            };
            if ok == 0 {
                return Err(HistoryError::new(format!(
                    "file identity {}: {}",
                    path.display(),
                    std::io::Error::last_os_error()
                )));
            }
            Ok((info.VolumeSerialNumber, info.FileId.Identifier))
        }
        let a = identity(source)?;
        let b = identity(destination)?;
        Ok(a.1 != [0; 16] && a == b)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (a, b);
        Ok(false)
    }
}

/// Execute the recorded-but-unfinished pairs for `db_path`'s file set —
/// pairs belonging to other stores are left alone. Idempotent: a pair is
/// done when its destination exists and its source is gone; a pair whose
/// link landed but whose unlink failed (both names present, SAME inode —
/// proven by `same_file_identity`) is finished by removing the source. If
/// both names exist but are DIFFERENT objects the destination was claimed
/// by unrelated bytes before the link landed: fail closed, preserve both,
/// keep the pair recorded — never delete the original on an unproven move.
/// `moves` counts executed `move_no_clobber` calls so
/// `RIGHTKIT_DEV_QUARANTINE_FAIL_AFTER` can inject a mid-sequence failure.
///
/// On ANY failure the whole recorded set is rolled back — every landed
/// destination whose source name is free moves back to its source — so the
/// live directory holds either the full original set or the full
/// quarantined set, never a split that could pair a fresh replacement DB
/// with a stale predecessor WAL.
///
/// On success, destinations that actually landed join the marker's
/// `quarantined` ledger (or `retained` when the marker already completed a
/// replay) and the consumed pairs are dropped. On failure the pairs are
/// restored to the marker unchanged — the on-disk record already lists
/// them, so the caller does not rewrite on error.
fn drain_pending_moves(
    marker: &mut HistoryRecoveryMarker,
    db_path: &Path,
    moves: &mut usize,
) -> Result<(), HistoryError> {
    let set = db_and_sidecar_paths(db_path);
    let (ours, others): (Vec<QuarantineMove>, Vec<QuarantineMove>) =
        std::mem::take(&mut marker.pairs)
            .into_iter()
            .partition(|pair| {
                set.iter()
                    .any(|member| member == &PathBuf::from(&pair.source))
            });
    let result = (|| -> Result<(), HistoryError> {
        for pair in &ours {
            let source = Path::new(&pair.source);
            let destination = Path::new(&pair.destination);
            match (source.exists(), destination.exists()) {
                // Already moved, or externally vanished — nothing to do.
                (false, _) => {}
                // Both names present. If they are the same object the link
                // landed but the unlink never ran (crash between the halves
                // of move_no_clobber): finish by dropping the source name.
                // If they are DIFFERENT objects the destination name was
                // claimed by unrelated bytes — the original link failed
                // EEXIST and this move never executed. Fail closed: keep the
                // source live and keep the pair recorded for the next
                // attempt, rather than deleting the original while the
                // recorded "destination" holds foreign content.
                (true, true) => {
                    if !same_file_identity(source, destination)? {
                        return Err(HistoryError::new(format!(
                            "quarantine destination {} holds unrelated bytes; {} left live",
                            destination.display(),
                            source.display()
                        )));
                    }
                    // Same object: the link half of the recorded move
                    // provably landed. Establish destination-entry
                    // durability, drop the source name, commit the removal.
                    flush_move_entry(destination)?;
                    std::fs::remove_file(source).map_err(|err| {
                        HistoryError::new(format!("quarantine unlink {}: {err}", source.display()))
                    })?;
                    flush_move_entry(source)?;
                }
                (true, false) => {
                    #[cfg(feature = "test-mode")]
                    if dev_quarantine_fail_after().is_some_and(|n| *moves + 1 >= n) {
                        return Err(HistoryError::new(format!(
                            "quarantine {}: injected failure (RIGHTKIT_DEV_QUARANTINE_FAIL_AFTER)",
                            source.display()
                        )));
                    }
                    move_no_clobber(source, destination)?;
                    *moves += 1;
                }
            }
        }
        Ok(())
    })();
    if let Err(err) = result {
        // Roll the recorded set back to its live names: each landed
        // destination whose source is absent becomes the source again.
        // Best-effort — a rollback move that fails leaves a hardlinked
        // duplicate, which the next drain resolves (both names present →
        // unlink source).
        for pair in &ours {
            let source = Path::new(&pair.source);
            let destination = Path::new(&pair.destination);
            if destination.exists() && !source.exists() {
                let _ = move_no_clobber(destination, source);
            }
        }
        marker.pairs = others.into_iter().chain(ours).collect();
        return Err(err);
    }
    // Destinations that actually landed join the pending ledger (or the
    // retained ledger when the marker already completed a replay).
    let landed: Vec<String> = ours
        .iter()
        .map(|pair| pair.destination.clone())
        .filter(|destination| Path::new(destination).exists())
        .collect();
    let ledger = if marker.state == STATE_RETAINED {
        &mut marker.retained
    } else {
        &mut marker.quarantined
    };
    for destination in landed {
        if !ledger.contains(&destination) {
            ledger.push(destination);
        }
    }
    marker.pairs = others;
    Ok(())
}

/// Finish (or roll back) any recorded-but-incomplete quarantine moves for
/// `db_path`'s set BEFORE the caller decides what to open. This is what
/// makes a retried open after a partial quarantine safe: a missing main DB
/// must never meet a still-live predecessor WAL.
///
/// An ABSENT marker is an ordinary first run. An EXISTING marker that
/// cannot be read or carries an unknown schema version may hold transaction
/// intent this open cannot see — a missing DB beside a live WAL could be an
/// incomplete recorded move — so the open fails closed: the error is
/// surfaced, and no DB, sidecar, or marker bytes are mutated.
pub(super) fn complete_pending_quarantine(db_path: &Path) -> Result<(), HistoryError> {
    let _guard = RECOVERY_LOCK
        .lock()
        .map_err(|_| HistoryError::new("history recovery lock poisoned"))?;
    let path = marker_path();
    // Read under the lock: a concurrent finalize or quarantine may mutate
    // the marker between an unlocked read and the drain/write below, and
    // stale bytes would overwrite newer transaction intent.
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        // Marker absent — ordinary path.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(HistoryError::new(format!(
                "read recovery marker {}: {err}",
                path.display()
            )))
        }
    };
    let mut marker = match serde_json::from_slice::<HistoryRecoveryMarker>(&bytes) {
        Ok(marker) if marker.v == 1 => marker,
        _ => {
            return Err(HistoryError::new(format!(
                "unreadable recovery marker {}: preserving all database files until the recorded transaction is resolved",
                path.display()
            )))
        }
    };
    if marker.pairs.is_empty() {
        return Ok(());
    }
    let mut moves = 0usize;
    drain_pending_moves(&mut marker, db_path, &mut moves)?;
    write_marker(&marker)
}

/// Move the unreadable database set aside (never delete) and record the
/// recovery marker. Transactional ordering: any previously recorded pairs
/// for this set drain first; then the new source→destination pairs are
/// written durable BEFORE their first move; then the moves execute; then
/// the completed destinations join `quarantined` and the marker is
/// rewritten. A failure anywhere either rolls the recorded set back to the
/// live names or leaves a completable transaction — never unmarked
/// ciphertext and never a split DB/WAL pair.
pub(super) fn quarantine_db_files(
    db_path: &Path,
    reason: RecoveryReason,
) -> Result<(), HistoryError> {
    let _guard = RECOVERY_LOCK
        .lock()
        .map_err(|_| HistoryError::new("history recovery lock poisoned"))?;
    let set = db_and_sidecar_paths(db_path);
    let mut marker = load_marker(&marker_path()).unwrap_or_default();
    let mut moves = 0usize;
    // Finish any recorded-but-incomplete moves for this set first — the
    // marker is the transaction record and this open continues it.
    let had_pending_pairs = marker
        .pairs
        .iter()
        .any(|pair| set.contains(&PathBuf::from(&pair.source)));
    drain_pending_moves(&mut marker, db_path, &mut moves)?;
    // Claim fresh destinations for files still live, skipping names this
    // marker already claims so a retried quarantine can never overwrite an
    // earlier one.
    let mut claimed: Vec<PathBuf> = marker
        .pairs
        .iter()
        .map(|pair| PathBuf::from(&pair.destination))
        .chain(marker.quarantined.iter().map(PathBuf::from))
        .chain(marker.retained.iter().map(PathBuf::from))
        .collect();
    let stamp = crate::delivery::now_ms();
    let mut new_pairs = Vec::new();
    for candidate in &set {
        if !candidate.exists()
            || marker
                .pairs
                .iter()
                .any(|pair| Path::new(&pair.source) == candidate.as_path())
        {
            continue;
        }
        new_pairs.push(QuarantineMove {
            source: candidate.to_string_lossy().into_owned(),
            destination: quarantine_destination(candidate, stamp, &mut claimed)?
                .to_string_lossy()
                .into_owned(),
        });
    }
    if new_pairs.is_empty() {
        // Nothing live left to move for this set. Persist only if the drain
        // above merged completed pairs into the ledger.
        if had_pending_pairs {
            write_marker(&marker)?;
        }
        return Ok(());
    }
    marker.v = 1;
    marker.reason = reason.as_str().to_string();
    marker.state = STATE_PENDING.to_string();
    // Bind the retained consent state (scope + epoch) to THIS pending set.
    // While files still await their replay verdict (`quarantined`
    // non-empty after the drain), the marker keeps — and keeps protecting —
    // its original binding: a scope authorized later can never adopt an
    // unknown-owner quarantine. Once the pending set is exhausted (all
    // retained, or a fresh marker), this quarantine is a new recovery event
    // and binds the consent state that is current NOW.
    if marker.quarantined.is_empty() {
        marker.created_at_ms = stamp;
        marker.consent_generation = current_consent_generation();
        marker.bound_scope_digest = authorized_scope_digest().unwrap_or_default();
    }
    marker.pairs.extend(new_pairs);
    // Durable intent: the marker names every source/destination BEFORE the
    // first move, so a crash mid-move leaves a completable transaction.
    write_marker(&marker)?;
    drain_pending_moves(&mut marker, db_path, &mut moves)?;
    // Durable result: completed destinations are now in `quarantined`.
    write_marker(&marker)?;
    crate::host::host().log(
        "history_db_quarantined",
        &format!(
            "reason={} moved={} listed={}",
            reason.as_str(),
            moves,
            marker.quarantined.len()
        ),
    );
    // Accelerate the restoring pull when the scheduler is already running.
    // Durable pending work: a not-yet-started scheduler still requests a pull
    // at startup, and a disabled one drops the bit at its loop top.
    crate::host::host().notify_remote_change();
    Ok(())
}

/// User-facing view of the recovery marker: what happened to the history
/// database and where the preserved (never deleted) encrypted backup lives.
/// Contains no key material and no history content.
#[derive(Debug, Clone, Serialize)]
pub struct HistoryRecoveryStatus {
    /// `keychain_key_absent` or `stored_key_rejected` (empty if unknown).
    pub reason: String,
    pub created_at_ms: u64,
    /// `pending` (replay still owed) or `retained`.
    pub state: String,
    /// Encrypted backup files kept on disk, with their sizes in bytes
    /// (`None` when the file is no longer present).
    pub backups: Vec<HistoryRecoveryBackup>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HistoryRecoveryBackup {
    pub path: String,
    pub bytes: Option<u64>,
}

/// Read-only status of the last history-database recovery, or `None` when no
/// recovery marker exists. Never mutates the marker or the backups: the
/// choices offered to the user are keep the backup (default, automatic),
/// start fresh (already done by the quarantine), and export diagnostics.
pub fn history_recovery_status() -> Option<HistoryRecoveryStatus> {
    let marker = load_marker(&marker_path())?;
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
        .map(|path| HistoryRecoveryBackup {
            bytes: std::fs::metadata(&path).ok().map(|meta| meta.len()),
            path,
        })
        .collect();
    Some(HistoryRecoveryStatus {
        reason: marker.reason,
        created_at_ms: marker.created_at_ms,
        state: marker.state,
        backups,
    })
}

/// Plain-text diagnostics a user can attach to a support request. Key and
/// history content are never included.
pub fn history_recovery_diagnostics() -> String {
    let Some(status) = history_recovery_status() else {
        return "History recovery: no recovery event recorded.\n".to_string();
    };
    let mut out = String::from("History recovery diagnostics\n");
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

/// Pending recovery, or `None`. A corrupt or unknown-version marker is
/// LEFT IN PLACE — `complete_pending_quarantine` owns the fail-closed
/// decision at open time, and moving the file would hide the evidence it
/// guards. `retained` markers are deliberately not pending: the bound
/// replay already completed for them.
fn pending_marker() -> Option<HistoryRecoveryMarker> {
    let path = marker_path();
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            crate::host::host().log("history_recovery_marker_read_failed", &format!("{err}"));
            return None;
        }
    };
    match serde_json::from_slice::<HistoryRecoveryMarker>(&bytes) {
        Ok(marker) if marker.v == 1 && marker.state == STATE_PENDING => Some(marker),
        Ok(_) => None,
        _ => {
            crate::host::host().log(
                "history_recovery_marker_unreadable",
                "unreadable marker left in place",
            );
            None
        }
    }
}

/// Whether a store rebuild is waiting on a CloudKit replay for `scope` —
/// i.e. a pending marker exists AND the pull's authorized scope digests to
/// the scope that was consent-authorized when the DB was quarantined. A
/// different account's pull, or a marker with no binding, returns false:
/// neither the cursor reset nor the completion may run for a scope that did
/// not own the quarantined data.
pub fn recovery_pending_for_scope(scope: &str) -> bool {
    let Some(marker) = pending_marker() else {
        return false;
    };
    !marker.bound_scope_digest.is_empty() && marker.bound_scope_digest == scope_digest(scope)
}

/// LEGACY (pre rightkit-sync): the engine now owns the cursor in its ledger and
/// recovery replays through `request_full_replay_once`; this path is kept only
/// for the old helper's leftover file and its tests.
/// Scoped CloudKit zone-change cursor written by the Swift
/// `HeardRightCloudSync` helper — `scopedChangeTokenURL` in
/// CloudSyncMain.swift stores it at
/// `HeardRightNext/cloud-sync-zone-token.<sha256(scope)>.data`, where the
/// digest is a plain SHA-256 of the scope string (no domain separator —
/// different from `cloud_sync_core::legacy_scoped_file_name`). Deleting it makes the
/// helper's next pull a full `recordZoneChanges(since: nil)` replay; the
/// commit step re-persists a fresh cursor.
pub fn zone_token_path_for_scope(scope: &str) -> PathBuf {
    crate::host::host().data_file(&format!(
        "cloud-sync-zone-token.{}.data",
        scope_digest(scope)
    ))
}

/// Remove the persisted zone cursor for the authorized scope so the next
/// helper pull replays every record. Idempotent — returns false when the
/// cursor was already absent.
pub fn reset_zone_change_token(scope: &str) -> Result<bool, String> {
    let path = zone_token_path_for_scope(scope);
    match std::fs::remove_file(&path) {
        Ok(()) => {
            crate::host::host().log("history_recovery_cursor_reset", "removed");
            Ok(true)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(format!("reset cloud sync change token: {err}")),
    }
}

/// Reopen the production store as the completion gate: the replacement
/// encrypted DB must demonstrably open, decipher, and answer a real query
/// before the pending marker can be consumed. This proves only that the
/// replacement is usable — never that all predecessor rows were recovered.
fn verify_replacement_store() -> Result<u32, String> {
    use crate::history::HistoryStore;
    let store = crate::history::SQLCipherHistoryStore::open_default()
        .map_err(|err| format!("post-replay store verification: {err}"))?;
    let page = store
        .query(&crate::history::HistoryQuery::default())
        .map_err(|err| format!("post-replay store query: {err}"))?;
    Ok(page.total as u32)
}

/// Called from `cloud_sync_pull` only after a pull whose merge AND cursor
/// commit succeeded — the `ReplayReceipt` is constructed after `CommitPull`
/// returns, so `cursor_committed` restates a fact, not a hope.
///
/// Completion requires all of:
/// - a `pending` marker,
/// - `receipt.cursor_committed`,
/// - `receipt.scope` digesting to the marker's `bound_scope_digest` (the
///   account that owned the quarantined DB's zone — unknown/mismatch keeps
///   everything pending),
/// - `receipt.consent_generation` matching BOTH the marker's bound
///   generation (consent unchanged since quarantine) and the live
///   generation (consent unchanged since the pull started),
/// - the replacement store reopening cleanly.
///
/// On success the pending `quarantined` set moves into the `retained`
/// ledger and the marker completes: no more cursor resets, normal
/// incremental pulls resume. NOTHING IS DELETED — zone coverage can prove
/// the zone contained items but can never prove every unsynced predecessor
/// row was in it, so the quarantined ciphertext is retained internally in
/// perpetuity. Every rejection keeps the marker pending so a later pull can
/// satisfy the proof — replays dedupe by delivery_id.
pub fn finish_recovery_after_replay(receipt: &ReplayReceipt) -> Result<(), String> {
    let Some(snapshot) = pending_marker() else {
        return Ok(());
    };
    if !receipt.cursor_committed {
        crate::host::host().log("history_recovery_finalize_rejected", "reason=uncommitted");
        return Ok(());
    }
    if snapshot.bound_scope_digest.is_empty()
        || snapshot.bound_scope_digest != scope_digest(&receipt.scope)
    {
        crate::host::host().log(
            "history_recovery_finalize_rejected",
            "reason=scope_unbound_or_mismatch",
        );
        return Ok(());
    }
    if receipt.consent_generation != snapshot.consent_generation
        || receipt.consent_generation != current_consent_generation()
    {
        crate::host::host().log(
            "history_recovery_finalize_rejected",
            "reason=consent_changed",
        );
        return Ok(());
    }
    // Verification reopens the live store, whose open path can itself
    // quarantine — and quarantine takes RECOVERY_LOCK. Verify BEFORE locking,
    // so this function can never self-deadlock on a nested recovery.
    let restored_total = verify_replacement_store()?;
    let _guard = RECOVERY_LOCK
        .lock()
        .map_err(|_| "history recovery lock poisoned".to_string())?;
    // Re-validate under the lock: the snapshot predates verification, so a
    // concurrent finalize may have retained the marker, or a new quarantine
    // may have replaced it. If the on-disk marker no longer carries the
    // binding this receipt proved against, do not consume it.
    let Some(mut current) = load_marker(&marker_path()) else {
        crate::host::host().log(
            "history_recovery_finalize_rejected",
            "reason=marker_vanished",
        );
        return Ok(());
    };
    if current.state != STATE_PENDING
        || current.bound_scope_digest != snapshot.bound_scope_digest
        || current.consent_generation != snapshot.consent_generation
    {
        crate::host::host().log(
            "history_recovery_finalize_rejected",
            "reason=marker_changed",
        );
        return Ok(());
    }
    // The snapshot's pending set moves to `retained` — the files are kept;
    // the marker completes so future pulls stop resetting the cursor. A
    // nested quarantine during verification may have appended NEWER paths:
    // only the snapshot's paths are proven consumed by this replay, so
    // leftovers stay pending (a replay is still owed for them). Recorded
    // pairs stay for the next open's `complete_pending_quarantine`, which
    // drains them straight into `retained` when the state is terminal.
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
    write_marker(&current).map_err(|err| err.to_string())?;
    crate::host::host().log(
        "history_recovery_completed",
        &format!("retained_files={retained_now} verified_total={restored_total}"),
    );
    Ok(())
}

/// Test seam: the consent generation `finish_recovery_after_replay`
/// compares receipts against. Scheduler internals are crate-private, so
/// integration tests read it through here.
#[doc(hidden)]
pub fn test_consent_generation() -> u64 {
    current_consent_generation()
}
