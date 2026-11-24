//! Host-side steps around an opened [`EncryptedStore`] that every app with an encrypted store
//! repeats: the check for a missing main file before the first open, the read-back of the one
//! pragma the owner applies but does not verify (`secure_delete`), and a key rotation that settles
//! a store which has been open since launch.
//!
//! It also holds the pieces a host's quarantine record needs: the suffix a quarantined file name
//! takes, the words and destinations a durable quarantine record stores, and the adapter from the
//! owner's move record to a host ledger that keeps `(source, destination)` pairs.
//!
//! Nothing here names an application, an event, a path or a credential: the key-store probes come
//! in as closures and the telemetry names come in as inputs.

use super::{
    EncryptedStore, KeySource, LegacyQuarantineJournal, LifecycleError, QuarantineKind,
    QuarantineNotice, RebuildReason, RecordedMove, RotationReport,
};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Whether the main database file exists and is not empty. `Ok(false)` only when it is absent
/// (`NotFound`) or zero bytes; any other stat failure is an error.
pub fn main_has_content(db_path: &Path) -> Result<bool, String> {
    match std::fs::metadata(db_path) {
        Ok(meta) => Ok(meta.len() > 0),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(format!("stat {}: {err}", db_path.display())),
    }
}

/// The reason to record for sidecars left beside a MISSING main file, decided before the first
/// open. With no main file the key is minted here (the owner never mints a key for a store that is
/// not there), and when none was stored the sidecars with content are committed rows under a lost
/// key: `Some(RebuildReason::KeyAbsent)`, so the owner keeps them aside before it creates the new
/// database over them. `None` when the main file has content, or a key was stored.
///
/// - `key_is_stored`: whether the credential store holds a key now (`Err` is a store failure,
///   surfaced unchanged and nothing is touched).
/// - `ensure_key`: get or create the key.
///
/// Errors are `credential store: <text>` (or `stat <path>: <text>`).
pub fn orphan_reason_before_open(
    db_path: &Path,
    key_is_stored: impl FnOnce() -> Result<bool, String>,
    ensure_key: impl FnOnce() -> Result<(), String>,
) -> Result<Option<RebuildReason>, String> {
    let mut orphan_reason = None;
    if !main_has_content(db_path)? {
        let key_was_stored = key_is_stored().map_err(|err| format!("credential store: {err}"))?;
        ensure_key().map_err(|err| format!("credential store: {err}"))?;
        if !key_was_stored {
            orphan_reason = Some(RebuildReason::KeyAbsent);
        }
    }
    Ok(orphan_reason)
}

/// Why [`verify_secure_delete`] failed.
#[derive(Debug)]
pub enum SecureDeleteError {
    /// The store could not run the read (blocked, key store failure, ...).
    Store(LifecycleError),
    /// `PRAGMA secure_delete` failed.
    Read(String),
    /// The pragma is not `1`: a configuration violation, not a lost key, so it never triggers a
    /// rebuild.
    Mismatch(i64),
}

impl std::fmt::Display for SecureDeleteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(err) => write!(f, "{err}"),
            Self::Read(err) => write!(f, "read secure_delete: {err}"),
            Self::Mismatch(value) => {
                write!(f, "sqlite pragma contract mismatch: secure_delete={value}")
            }
        }
    }
}

impl std::error::Error for SecureDeleteError {}

/// The owner applies WAL, busy timeout, synchronous, journal size limit, temp store, optimize and
/// `secure_delete` on every connection it opens, but does not read `secure_delete` back. An app
/// that promises deleted rows are overwritten on disk checks it here, on the store's writer.
pub fn verify_secure_delete(store: &EncryptedStore) -> Result<(), SecureDeleteError> {
    let secure_delete: i64 = store
        .with_writer(|conn| conn.query_row("PRAGMA secure_delete", [], |row| row.get(0)))
        .map_err(SecureDeleteError::Store)?
        .map_err(|err| SecureDeleteError::Read(err.to_string()))?;
    if secure_delete != 1 {
        return Err(SecureDeleteError::Mismatch(secure_delete));
    }
    Ok(())
}

/// Telemetry names the host wants [`rotate_settling`] to report under.
#[derive(Debug, Clone, Copy)]
pub struct RotationEvents<'a> {
    /// A recovery pass ran; the detail is the report's `Debug`.
    pub recovery: &'a str,
    /// The new key is stored and opens the file; the cleanup the owner finishes on the next open.
    pub cleanup_pending: &'a str,
}

/// Rotate the key of a store that has been open since launch. `Ok` only when the new key is the
/// stored primary key and opens the file; a rotation that is not settled returns an error that
/// begins `key rotation pending` and leaves every viable key stored (the owner's recovery settles
/// it now, or the next open does).
///
/// The owner rotates only a store opened with the canonical primary key and with key storage fully
/// reconciled. A store that has been open since launch may be blocked, degraded or opened with a
/// non-primary key, so such leftovers are settled first. `rotate` performs the rotation itself
/// (for a managed store: `|| history.rotate_key(RotationRequest::new(new_key))`).
pub fn rotate_settling(
    store: &EncryptedStore,
    rotate: impl FnOnce() -> RotationReport,
    events: RotationEvents<'_>,
    sink: impl Fn(&str, &str),
) -> Result<(), String> {
    if store.is_blocked() || store.degraded().is_some() || store.key_source() != KeySource::Primary
    {
        let report = store.recover_rotation();
        sink(events.recovery, &format!("{report:?}"));
    }
    match rotate() {
        RotationReport::Committed => Ok(()),
        RotationReport::CommittedCleanupPending { pending } => {
            // The new key is stored and opens the file; the owner finishes the cleanup on the next open.
            sink(events.cleanup_pending, &pending.join("; "));
            Ok(())
        }
        RotationReport::NotCommitted { error, .. } => Err(format!("key rotation failed: {error}")),
        RotationReport::Indeterminate { detail } => {
            // The owner blocks the store until it knows which key won; settle it from the real state.
            let report = store.recover_rotation();
            sink(events.recovery, &format!("{report:?}"));
            Err(format!(
                "key rotation pending: {detail}; every viable key is kept and the rotation settles on the next open"
            ))
        }
    }
}

/// The whole suffix of a quarantined database file: `<mark><unix ms>`. `mark` is the host's
/// (for example `.unreadable-`); rightkit-db appends `-1`, `-2`, ... when the name is taken and
/// moves each sidecar beside it under the same suffix. A `fn(SystemTime) -> String` for
/// `QuarantineStamp::Custom` is a non-capturing closure over this: `|now| unix_millis_suffix(MARK, now)`.
pub fn unix_millis_suffix(mark: &str, now: SystemTime) -> String {
    let ms = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("{mark}{ms}")
}

/// The words a host's durable quarantine record stores for each [`RebuildReason`] (the host's
/// persisted vocabulary, so the values a ledger already holds stay readable).
#[derive(Debug, Clone, Copy)]
pub struct ReasonWords<'a> {
    pub key_absent: &'a str,
    pub key_rejected: &'a str,
}

impl<'a> ReasonWords<'a> {
    /// The word for `reason`; empty for a reason this crate adds later.
    pub fn text(&self, reason: RebuildReason) -> &'a str {
        match reason {
            RebuildReason::KeyAbsent => self.key_absent,
            RebuildReason::KeyRejected => self.key_rejected,
        }
    }
}

/// What a host records for a [`QuarantineNotice`] inside the owner's quarantine transaction: every
/// destination the quarantine takes, and the reason in the host's words. A rebuild carries its own
/// reason; an orphan-sidecar quarantine carries none, so it records `orphan_reason` (what
/// [`orphan_reason_before_open`] decided: `KeyAbsent` on a machine whose key is lost, nothing when
/// a key was stored).
pub fn quarantine_entry<'a>(
    notice: &QuarantineNotice,
    orphan_reason: Option<RebuildReason>,
    words: ReasonWords<'a>,
) -> (Vec<PathBuf>, Option<&'a str>) {
    let reason = if matches!(notice.kind, QuarantineKind::OrphanSidecars) {
        notice.reason.or(orphan_reason)
    } else {
        notice.reason
    };
    let destinations: Vec<PathBuf> = notice
        .moves
        .iter()
        .map(|recorded| recorded.destination.clone())
        .collect();
    (destinations, reason.map(|reason| words.text(reason)))
}

impl RecordedMove {
    /// `(source, destination)`, the neutral shape a host ledger converts to and from.
    pub fn into_paths(self) -> (PathBuf, PathBuf) {
        (self.source, self.destination)
    }
}

impl From<(PathBuf, PathBuf)> for RecordedMove {
    fn from((source, destination): (PathBuf, PathBuf)) -> Self {
        Self {
            source,
            destination,
        }
    }
}

/// A [`LegacyQuarantineJournal`] over a host ledger that keeps `(source, destination)` pairs: the
/// moves an EARLIER build recorded, finished or rolled back by the owner before the database opens.
/// `pending` lists the pairs not yet known to be finished (`Err` fails the open closed);
/// `settled` receives the pairs the owner consumed and the destinations that landed.
pub struct PathPairJournal<Pending, Settled> {
    pending: Pending,
    settled: Settled,
}

impl<Pending, Settled> PathPairJournal<Pending, Settled>
where
    Pending: Fn() -> Result<Vec<(PathBuf, PathBuf)>, String> + Send + Sync,
    Settled: Fn(&[(PathBuf, PathBuf)], &[PathBuf]) -> Result<(), String> + Send + Sync,
{
    pub const fn new(pending: Pending, settled: Settled) -> Self {
        Self { pending, settled }
    }
}

impl<Pending, Settled> LegacyQuarantineJournal for PathPairJournal<Pending, Settled>
where
    Pending: Fn() -> Result<Vec<(PathBuf, PathBuf)>, String> + Send + Sync,
    Settled: Fn(&[(PathBuf, PathBuf)], &[PathBuf]) -> Result<(), String> + Send + Sync,
{
    fn pending(&self, _database: &Path) -> Result<Vec<RecordedMove>, String> {
        Ok((self.pending)()?
            .into_iter()
            .map(RecordedMove::from)
            .collect())
    }

    fn settled(
        &self,
        _database: &Path,
        consumed: &[RecordedMove],
        landed: &[PathBuf],
    ) -> Result<(), String> {
        let consumed: Vec<(PathBuf, PathBuf)> = consumed
            .iter()
            .map(|done| (done.source.clone(), done.destination.clone()))
            .collect();
        (self.settled)(&consumed, landed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encrypted_lifecycle::{
        EncryptedStoreConfig, Key, KeyStoreError, PendingKey, PendingRevision, ProtectedKeySession,
        ProtectedKeyStore, RebuildHooks, RebuildPolicy,
    };
    use crate::{DbOptions, QuarantineStamp};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, UNIX_EPOCH};

    struct Files(PathBuf);

    impl Files {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "rightkit-db-support-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Files {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // These pre-open paths only read primary-slot authority. Any other credential access is an
    // unexpected widening of these filesystem tests, so fail instead of simulating rotation.
    struct EmptyAuthority;

    impl ProtectedKeyStore for EmptyAuthority {
        fn lock_rotation(
            &self,
            _: Duration,
        ) -> Result<Box<dyn ProtectedKeySession + Send + 'static>, KeyStoreError> {
            Ok(Box::new(Self))
        }
    }

    impl ProtectedKeySession for EmptyAuthority {
        fn read_primary(&mut self) -> Result<Option<Key>, KeyStoreError> {
            Ok(None)
        }
        fn read_pending(&mut self) -> Result<Option<PendingKey>, KeyStoreError> {
            panic!("unexpected credential access")
        }
        fn write_pending(&mut self, _: &Key) -> Result<PendingRevision, KeyStoreError> {
            panic!("unexpected credential mutation")
        }
        fn promote_pending(&mut self, _: &PendingRevision) -> Result<(), KeyStoreError> {
            panic!("unexpected credential mutation")
        }
        fn discard_pending(&mut self, _: &PendingRevision) -> Result<(), KeyStoreError> {
            panic!("unexpected credential mutation")
        }
        fn read_legacy_staging(&mut self, _: &str) -> Result<Option<Key>, KeyStoreError> {
            panic!("unexpected credential access")
        }
        fn delete_legacy_staging(&mut self, _: &str) -> Result<(), KeyStoreError> {
            panic!("unexpected credential mutation")
        }
        fn read_recovery(&mut self) -> Result<Option<Key>, KeyStoreError> {
            panic!("unexpected credential access")
        }
        fn write_recovery(&mut self, _: &Key) -> Result<(), KeyStoreError> {
            panic!("unexpected credential mutation")
        }
        fn clear_recovery(&mut self) -> Result<(), KeyStoreError> {
            panic!("unexpected credential mutation")
        }
    }

    fn config(path: &Path) -> EncryptedStoreConfig {
        EncryptedStoreConfig::new(DbOptions::new(path), Arc::new(EmptyAuthority))
    }

    fn settle(config: &EncryptedStoreConfig) -> Result<(), LifecycleError> {
        super::super::rebuild::settle_interrupted(config, &super::super::rotate::NoFaults)
            .map(|_| ())
    }

    fn legacy_suffix(_: SystemTime) -> String {
        unix_millis_suffix(".unreadable-", UNIX_EPOCH + Duration::from_millis(1234))
    }

    #[test]
    fn sidecar_only_quarantine_keeps_bytes_names_and_host_reason() {
        for empty_main in [false, true] {
            let files = Files::new();
            let db = files.path("history.db");
            if empty_main {
                std::fs::write(&db, []).unwrap();
            }
            for name in ["history.db-wal", "history.db-shm", "history.db-journal"] {
                std::fs::write(files.path(name), name.as_bytes()).unwrap();
            }
            // One occupied destination forces one shared collision suffix across the whole set.
            let occupied = files.path("history.db-wal.unreadable-1234");
            std::fs::write(&occupied, b"earlier quarantine").unwrap();
            let reason = orphan_reason_before_open(&db, || Ok(false), || Ok(())).unwrap();
            let entries = Arc::new(Mutex::new(Vec::new()));
            let capture = entries.clone();
            let cfg = config(&db)
                .with_rebuild(
                    RebuildPolicy::new(|| panic!("quarantine must not mint a key"))
                        .with_quarantine_stamp(QuarantineStamp::Custom(legacy_suffix)),
                )
                .with_rebuild_hooks(
                    RebuildHooks::new()
                        .with_orphan_sidecars(true)
                        .with_quarantine_notice(move |notice| {
                            assert_eq!(notice.kind, QuarantineKind::OrphanSidecars);
                            assert!(!notice.resumed);
                            for item in &notice.moves {
                                assert!(item.source.exists());
                                assert!(!item.destination.exists());
                            }
                            let (paths, reason) = quarantine_entry(
                                notice,
                                reason,
                                ReasonWords {
                                    key_absent: "missing-key",
                                    key_rejected: "rejected-key",
                                },
                            );
                            capture.lock().unwrap().push((paths, reason));
                            Ok(())
                        }),
                );
            settle(&cfg).unwrap();
            let entries = entries.lock().unwrap();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].1, Some("missing-key"));
            assert_eq!(entries[0].0.len(), 3);
            for name in ["history.db-wal", "history.db-shm", "history.db-journal"] {
                let destination = files.path(&format!("{name}.unreadable-1234-1"));
                assert!(entries[0].0.contains(&destination));
                assert_eq!(std::fs::read(destination).unwrap(), name.as_bytes());
                assert!(!files.path(name).exists());
            }
            assert_eq!(std::fs::read(occupied).unwrap(), b"earlier quarantine");
            assert_eq!(db.exists(), empty_main);
            assert!(!files.path("history.db.orphan-quarantine").exists());
        }
    }

    #[test]
    fn refused_sidecar_notice_keeps_every_source_byte() {
        let files = Files::new();
        let db = files.path("history.db");
        let wal = files.path("history.db-wal");
        std::fs::write(&wal, b"committed rows without main").unwrap();
        let cfg = config(&db).with_rebuild_hooks(
            RebuildHooks::new()
                .with_orphan_sidecars(true)
                .with_quarantine_notice(|_| Err("ledger unavailable".into())),
        );
        let error = settle(&cfg).unwrap_err();
        assert!(matches!(error, LifecycleError::RecoveryRequired { .. }));
        assert!(error.to_string().contains("ledger unavailable"));
        assert_eq!(std::fs::read(wal).unwrap(), b"committed rows without main");
        assert!(!db.exists());
        assert!(!files.path("history.db.orphan-quarantine").exists());
    }

    #[test]
    fn orphan_key_store_error_keeps_sidecars_and_does_not_mint() {
        let files = Files::new();
        let db = files.path("history.db");
        let wal = files.path("history.db-wal");
        std::fs::write(&wal, b"retained WAL").unwrap();
        let error = orphan_reason_before_open(
            &db,
            || Err("credential access denied".into()),
            || panic!("failed credential read must not mint a replacement"),
        )
        .unwrap_err();
        assert_eq!(error, "credential store: credential access denied");
        assert_eq!(std::fs::read(wal).unwrap(), b"retained WAL");
        assert!(!db.exists());
    }

    #[test]
    fn legacy_path_pairs_resume_partial_moves_with_exact_recovery_names() {
        let files = Files::new();
        let db = files.path("history.db");
        let destination = files.path("history.db.unreadable-1234");
        let wal = files.path("history.db-wal");
        let wal_destination = files.path("history.db.unreadable-1234-wal");
        // Earlier host moved main, then stopped before WAL. Preserve its destination spelling.
        std::fs::write(&destination, b"encrypted main retained").unwrap();
        std::fs::write(&wal, b"committed WAL retained").unwrap();
        let pairs = vec![
            (db.clone(), destination.clone()),
            (wal.clone(), wal_destination.clone()),
        ];
        let pending = pairs.clone();
        let reports = Arc::new(Mutex::new(Vec::new()));
        let capture = reports.clone();
        let journal = PathPairJournal::new(
            move || Ok(pending.clone()),
            move |consumed, landed| {
                capture
                    .lock()
                    .unwrap()
                    .push((consumed.to_vec(), landed.to_vec()));
                Ok(())
            },
        );
        let cfg = config(&db)
            .with_rebuild_hooks(RebuildHooks::new().with_legacy_journal(Arc::new(journal)));
        // Reporting the same retained host record twice must never replace or lose either file.
        settle(&cfg).unwrap();
        settle(&cfg).unwrap();
        let reports = reports.lock().unwrap();
        assert_eq!(reports.len(), 2);
        for (consumed, landed) in reports.iter() {
            assert_eq!(consumed, &pairs);
            assert_eq!(landed, &vec![destination.clone(), wal_destination.clone()]);
        }
        assert_eq!(
            std::fs::read(destination).unwrap(),
            b"encrypted main retained"
        );
        assert_eq!(
            std::fs::read(wal_destination).unwrap(),
            b"committed WAL retained"
        );
        assert!(!db.exists());
        assert!(!wal.exists());
    }

    #[test]
    fn legacy_path_pairs_refuse_outside_destination_without_changing_files() {
        let files = Files::new();
        let db = files.path("history.db");
        let outside = Files::new();
        let destination = outside.path("history.db.unreadable-1234");
        std::fs::write(&db, b"original encrypted bytes").unwrap();
        let source = db.clone();
        let target = destination.clone();
        let journal = PathPairJournal::new(
            move || Ok(vec![(source.clone(), target.clone())]),
            |_, _| panic!("refused moves must not be acknowledged"),
        );
        let cfg = config(&db)
            .with_rebuild_hooks(RebuildHooks::new().with_legacy_journal(Arc::new(journal)));
        assert!(matches!(
            settle(&cfg),
            Err(LifecycleError::RecoveryRequired { .. })
        ));
        assert_eq!(std::fs::read(db).unwrap(), b"original encrypted bytes");
        assert!(!destination.exists());
    }

    #[test]
    fn legacy_path_pairs_collision_keeps_both_distinct_files() {
        let files = Files::new();
        let db = files.path("history.db");
        let destination = files.path("history.db.unreadable-1234");
        std::fs::write(&db, b"live encrypted bytes").unwrap();
        std::fs::write(&destination, b"earlier retained bytes").unwrap();
        let source = db.clone();
        let target = destination.clone();
        let journal = PathPairJournal::new(
            move || Ok(vec![(source.clone(), target.clone())]),
            |_, _| panic!("conflicting files must not be acknowledged"),
        );
        let cfg = config(&db)
            .with_rebuild_hooks(RebuildHooks::new().with_legacy_journal(Arc::new(journal)));
        assert!(matches!(
            settle(&cfg),
            Err(LifecycleError::RecoveryRequired { .. })
        ));
        assert_eq!(std::fs::read(db).unwrap(), b"live encrypted bytes");
        assert_eq!(
            std::fs::read(destination).unwrap(),
            b"earlier retained bytes"
        );
    }
}
