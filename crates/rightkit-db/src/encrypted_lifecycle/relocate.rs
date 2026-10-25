//! Phased encrypted relocation over FileTransaction. Copy leaves old root selected;
//! caller durably selects new root before `reopen`. No daemon or credentials dependency.

use super::error::LifecycleError;
use super::keystore::{Key, ProtectedKeySession, ProtectedKeyStore};
use super::rotate::make_primary;
use super::store::{
    db_unavailable, establish_relocation, key_store_error, probe, EncryptedStore,
    EncryptedStoreConfig, Probe,
};
use super::transition::{
    access_denied_retry, current_key, next_id, read_journal, Journal, JournalState,
    RelocationJournal, RootIdentity, StoreTransition, TransitionKind,
};
use crate::backup::online_backup_sized;
use crate::IntegrityCheck;
use rightkit_fs::transaction::{
    recover_transactions_with, FileTransaction, TransactionEntry, TransactionOptions,
    TransactionReport,
};
use rightkit_fs::transient::{retry_transient, retry_transient_fs};
use rusqlite::OpenFlags;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Database destination & caller-authorized filesystem transaction root.
pub struct RelocationPlan {
    pub new_path: PathBuf,
    pub new_key_store: Option<Arc<dyn ProtectedKeyStore>>,
    /// Required by phased relocation & recovery. Recovery covers this entire explicit root.
    pub transaction_root: Option<PathBuf>,
}
impl RelocationPlan {
    pub fn new(new_path: impl Into<PathBuf>) -> Self {
        Self {
            new_path: new_path.into(),
            new_key_store: None,
            transaction_root: None,
        }
    }
    pub fn with_new_key_store(mut self, store: Arc<dyn ProtectedKeyStore>) -> Self {
        self.new_key_store = Some(store);
        self
    }
    pub fn with_transaction_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.transaction_root = Some(root.into());
        self
    }
}

#[derive(Debug)]
#[non_exhaustive]
pub enum RelocationReport {
    Relocated {
        old_root: PathBuf,
        new_root: PathBuf,
        key_store_switched: bool,
    },
    /// Publication is final; retained FS cleanup never causes rollback.
    RelocatedCleanupPending {
        old_root: PathBuf,
        new_root: PathBuf,
        cleanup_retained: Vec<PathBuf>,
        /// Why each path in `cleanup_retained` was kept: the last error of the removal that was
        /// given up (it carries the OS error, for example `Access is denied. (os error 5)`), or the
        /// identity check that stopped it. One entry per retained path, in the same order.
        cleanup_errors: Vec<(PathBuf, String)>,
    },
    NotRelocated {
        error: LifecycleError,
        copy_retained: Option<PathBuf>,
        key_store_residue: bool,
    },
}

/// Caller supplies authoritative root selection; DB never infers it from filesystem presence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelocationSettlement {
    Committed,
    Aborted,
}

#[derive(Debug)]
pub struct RelocationSettlementReport {
    pub copy_retained: Option<PathBuf>,
    pub key_store_residue: bool,
    pub cleanup_retained: Vec<PathBuf>,
    /// Why each path in `cleanup_retained` was kept (see
    /// [`RelocationReport::RelocatedCleanupPending`]): one entry per retained path, in order.
    pub cleanup_errors: Vec<(PathBuf, String)>,
}

/// Explicit recovery can open selected destination without ever opening old root.
pub struct RelocationRecovery {
    pub store: Option<EncryptedStore>,
    pub report: RelocationSettlementReport,
}

type Session = Box<dyn ProtectedKeySession + Send + 'static>;

fn pending(detail: impl Into<String>) -> LifecycleError {
    LifecycleError::RecoveryRequired {
        detail: detail.into(),
    }
}
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}
fn check_destination(path: &Path) -> Result<(), LifecycleError> {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let candidate = with_suffix(path, suffix);
        match std::fs::symlink_metadata(&candidate) {
            Ok(_) => return Err(LifecycleError::DestinationExists { path: candidate }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(rightkit_fs::FsError::io(&candidate, error).into()),
        }
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        return Err(LifecycleError::Precondition(format!(
            "destination parent {} does not exist",
            parent.display()
        )));
    }
    Ok(())
}
/// Whether `recorded` still names the file and directory it recorded. `Ok(false)` is a real
/// mismatch: the root is gone or another file took its place. An `Err` means the identity could
/// not be read at all (after the bounded retry of a transient Windows refusal), which is not
/// evidence that anything changed and is never reported as a mismatch.
fn same_identity(recorded: &RootIdentity) -> Result<bool, LifecycleError> {
    if recorded.file.is_none() {
        return Ok(false);
    }
    let now = RootIdentity::try_capture(&recorded.path)?;
    Ok(recorded.file == now.file && recorded.dir == now.dir)
}
/// [`same_identity`] for an identity that may not have been recorded: none recorded is no match.
fn same_recorded(recorded: Option<&RootIdentity>) -> Result<bool, LifecycleError> {
    recorded.map_or(Ok(false), same_identity)
}
fn remove_file_at(path: &Path) -> std::io::Result<()> {
    std::fs::remove_file(path)
}
fn remove_dir_at(path: &Path) -> std::io::Result<()> {
    std::fs::remove_dir(path)
}
/// Delete `path` with `remove`, retrying a Windows refusal that clears by itself (a scanner or
/// indexer holding the file) within rightkit-fs's bound. Identity is proven again before every
/// retry, so a name that changed hands during a pause is never deleted. A path that is gone is
/// `Ok`; after the bound the last refusal is returned as it is.
fn remove_identified(
    path: &Path,
    expected: Option<rightkit_fs::FileKey>,
    remove: impl Fn(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let mut retrying = false;
    retry_transient(|| {
        if std::mem::replace(&mut retrying, true) {
            match rightkit_fs::FileKey::of_path(path) {
                Ok(now) if Some(now) == expected => {}
                Ok(_) => {
                    return Err(std::io::Error::other(
                        "identity changed while a removal was retried",
                    ))
                }
                Err(rightkit_fs::FsError::Io { source, .. }) => {
                    return if source.kind() == std::io::ErrorKind::NotFound {
                        Ok(())
                    } else {
                        Err(source)
                    }
                }
                Err(error) => return Err(std::io::Error::other(error.to_string())),
            }
        }
        remove(path)
    })
}
/// `Some((path, why))` when `path` is there, or when whether it is there cannot be told; `None`
/// only when it is proven absent.
fn still_there(path: &Path, why: &str) -> Option<(PathBuf, String)> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Some((path.to_path_buf(), why.to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => Some((
            path.to_path_buf(),
            format!("{why}; whether it is there could not be read: {error}"),
        )),
    }
}
/// Remove the candidate's workspace directory once the candidate is gone, only while it is still
/// the directory the journal recorded; whatever stops that is added to `kept` with its reason.
fn remove_workspace(candidate: &RootIdentity, kept: &mut Vec<(PathBuf, String)>) {
    let Some(parent) = candidate.path.parent() else {
        return;
    };
    if candidate.dir.is_none() {
        return;
    }
    match current_key(parent) {
        Ok(now) if now == candidate.dir => {
            match remove_identified(parent, candidate.dir, remove_dir_at) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => kept.push((parent.to_path_buf(), error.to_string())),
            }
        }
        // Another directory, or none: not this relocation's to remove.
        Ok(_) => {}
        Err(error) => kept.push((
            parent.to_path_buf(),
            format!("the workspace could not be identified: {error}"),
        )),
    }
}
fn absolute(path: &Path) -> Result<PathBuf, LifecycleError> {
    std::path::absolute(path).map_err(|e| rightkit_fs::FsError::io(path, e).into())
}
fn checked_plan(
    cfg: &EncryptedStoreConfig,
    mut plan: RelocationPlan,
) -> Result<RelocationPlan, LifecycleError> {
    let requested_destination = absolute(&plan.new_path)?;
    let root = plan.transaction_root.as_ref().ok_or_else(|| {
        LifecycleError::InvalidConfig("phased relocation requires explicit transaction_root".into())
    })?;
    let root = rightkit_fs::canonicalize_plain(root)
        .map_err(|error| rightkit_fs::FsError::io(root, error))?;
    let parent = plan
        .new_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = rightkit_fs::canonicalize_plain(parent)
        .map_err(|error| rightkit_fs::FsError::io(parent, error))?;
    let leaf = plan
        .new_path
        .file_name()
        .ok_or_else(|| LifecycleError::InvalidConfig("destination must name a database".into()))?;
    let normalized_destination = parent.join(leaf);
    if plan.new_path.to_str().is_none() || root.to_str().is_none() || !root.is_dir() {
        return Err(LifecycleError::InvalidConfig(
            "transaction root & destination must be existing UTF-8 directories".into(),
        ));
    }
    let relative = normalized_destination.strip_prefix(&root).map_err(|_| {
        LifecycleError::InvalidConfig("destination lies outside explicit transaction root".into())
    })?;
    if relative.as_os_str().is_empty() || requested_destination == absolute(&cfg.db.path)? {
        return Err(LifecycleError::InvalidConfig(
            "destination must differ from current database".into(),
        ));
    }
    plan.transaction_root = Some(root);
    plan.new_path = requested_destination;
    Ok(plan)
}

impl Drop for RelocationCore {
    fn drop(&mut self) {
        self.release();
    }
}

struct RelocationCore {
    cfg: EncryptedStoreConfig,
    plan: RelocationPlan,
    key: Key,
    transition: Option<StoreTransition>,
    source: Option<Session>,
    destination: Option<Session>,
    journal: Option<Journal>,
    quiesced: bool,
    copied: bool,
    switched: bool,
    reopened: bool,
    settled: bool,
    /// Paths a finished relocation could not clean up, and (parallel, by path) why.
    cleanup: Vec<PathBuf>,
    cleanup_errors: Vec<(PathBuf, String)>,
}

impl RelocationCore {
    fn new(
        cfg: EncryptedStoreConfig,
        plan: RelocationPlan,
        key: Key,
    ) -> Result<Self, LifecycleError> {
        if cfg.db.effective_path() != cfg.db.path {
            return Err(LifecycleError::Precondition(
                "resolve per-host database path before relocation".into(),
            ));
        }
        let plan = checked_plan(&cfg, plan)?;
        check_destination(&plan.new_path)?;
        if read_journal(&cfg.db.path)?.is_some() || read_journal(&plan.new_path)?.is_some() {
            return Err(pending("an existing transition requires explicit recovery"));
        }
        Ok(Self {
            cfg,
            plan,
            key,
            transition: None,
            source: None,
            destination: None,
            journal: None,
            quiesced: false,
            copied: false,
            switched: false,
            reopened: false,
            settled: false,
            cleanup: Vec::new(),
            cleanup_errors: Vec::new(),
        })
    }
    /// Keep `path` and say why it is kept: the removal that was given up, or the check that
    /// stopped it, so a retained path always carries its reason.
    fn retain_for_cleanup(&mut self, path: PathBuf, why: impl std::fmt::Display) {
        self.cleanup_errors.push((path.clone(), why.to_string()));
        self.cleanup.push(path);
    }
    fn destination(&mut self) -> &mut dyn ProtectedKeySession {
        match self.destination.as_mut() {
            Some(destination) => &mut **destination,
            None => &mut **self.source.as_mut().expect("quiesced source authority"),
        }
    }
    fn release(&mut self) {
        drop(self.destination.take());
        drop(self.source.take());
        drop(self.transition.take());
    }
    fn persist(&self) -> Result<(), LifecycleError> {
        self.transition
            .as_ref()
            .ok_or_else(|| pending("relocation guard is not held"))?
            .write_journal(
                self.journal
                    .as_ref()
                    .ok_or_else(|| pending("relocation journal missing"))?,
            )
    }
    fn quiesce(&mut self, store: &EncryptedStore) -> Result<(), LifecycleError> {
        if self.settled {
            return Err(pending("relocation already settled"));
        }
        if self.quiesced {
            return Ok(());
        }
        if store.config().db.path != self.cfg.db.path
            || store.current_key()?.as_str() != self.key.as_str()
        {
            return Err(pending("store changed after relocation preparation"));
        }
        store.drain(TransitionKind::Relocate, false)?;
        let result = (|| {
            if store.config().db.path != self.cfg.db.path {
                return Err(pending("store root changed before quiescence"));
            }
            self.transition = Some(StoreTransition::begin(
                TransitionKind::Relocate,
                &[self.cfg.db.path.as_path(), self.plan.new_path.as_path()],
                &self.cfg.transition,
            )?);
            self.source = Some(
                self.cfg
                    .key_store
                    .lock_rotation(self.cfg.transition.acquire_timeout)
                    .map_err(|e| key_store_error("source rotation lock", e))?,
            );
            if let Some(destination) = &self.plan.new_key_store {
                if !Arc::ptr_eq(destination, &self.cfg.key_store) {
                    self.destination = Some(
                        destination
                            .lock_rotation(self.cfg.transition.acquire_timeout)
                            .map_err(|e| key_store_error("destination rotation lock", e))?,
                    );
                }
            }
            check_destination(&self.plan.new_path)?;
            if read_journal(&self.cfg.db.path)?.is_some()
                || read_journal(&self.plan.new_path)?.is_some()
            {
                return Err(pending("another unfinished transition exists"));
            }
            let source = self
                .source
                .as_mut()
                .expect("source session")
                .read_primary()
                .map_err(|e| key_store_error("source primary", e))?
                .ok_or(LifecycleError::KeyMissing)?;
            if source.as_str() != self.key.as_str() {
                return Err(LifecycleError::KeyStoreMismatch {
                    detail: "source primary differs from open key".into(),
                });
            }
            self.check_destination_keys()?;
            let root = self.plan.transaction_root.as_ref().expect("checked root");
            let candidate = root
                .join(format!(
                    ".rkdb-relocate-{}-{}",
                    std::process::id(),
                    next_id()
                ))
                .join("candidate.db");
            self.journal = Some(Journal {
                kind: TransitionKind::Relocate,
                state: JournalState::Active,
                phase: "quiesced".into(),
                retain_previous: false,
                pending: None,
                id: next_id(),
                old_root: RootIdentity::capture(&self.cfg.db.path),
                new_root: Some(RootIdentity::capture(&self.plan.new_path)),
                relocation: Some(RelocationJournal {
                    candidate: RootIdentity::capture(&candidate),
                    transaction_root: RootIdentity::capture(root),
                    transaction_id: None,
                    published: None,
                    fs_committed: false,
                    switched: false,
                    key_store_residue: false,
                }),
            });
            self.persist()?;
            Ok(())
        })();
        if result.is_err() {
            // Preparation failure cannot clear a journal possibly written to only one root.
            if self.journal.is_none() {
                self.release();
                let _ = store.resume();
            } else {
                self.quiesced = true;
                store.block("relocation quiescence requires explicit abort/recovery".into());
            }
        } else {
            self.quiesced = true;
        }
        result
    }
    fn check_destination_keys(&mut self) -> Result<(), LifecycleError> {
        let key = self.key.clone();
        let path = self.plan.new_path.clone();
        let destination = self.destination();
        match destination
            .read_primary()
            .map_err(|e| key_store_error("destination primary", e))?
        {
            None => {}
            Some(existing) if existing.as_str() == key.as_str() => {}
            Some(_) => {
                return Err(LifecycleError::WrongDestinationKey {
                    path,
                    detail: "destination already stores another primary".into(),
                })
            }
        }
        if destination
            .read_pending()
            .map_err(|e| key_store_error("destination pending", e))?
            .is_some()
        {
            return Err(pending("destination contains unfinished pending key"));
        }
        Ok(())
    }
    /// Advance the journal to `phase` and persist it. The in-memory phase changes only if the
    /// write succeeds, so a failed write cannot make later settlement act on a phase that was
    /// never recorded.
    fn record_phase(&mut self, phase: &str) -> Result<(), LifecycleError> {
        let journal = self
            .journal
            .as_mut()
            .ok_or_else(|| pending("relocation journal missing"))?;
        let previous = std::mem::replace(&mut journal.phase, phase.to_string());
        if let Err(error) = self.persist() {
            if let Some(journal) = self.journal.as_mut() {
                journal.phase = previous;
            }
            return Err(error);
        }
        Ok(())
    }
    fn metadata(&self) -> &RelocationJournal {
        self.journal
            .as_ref()
            .expect("relocation journal")
            .relocation
            .as_ref()
            .expect("relocation metadata")
    }
    fn metadata_mut(&mut self) -> &mut RelocationJournal {
        self.journal
            .as_mut()
            .expect("relocation journal")
            .relocation
            .as_mut()
            .expect("relocation metadata")
    }
    fn copy(&mut self) -> Result<(), LifecycleError> {
        if self.settled || self.switched || self.source.is_none() {
            return Err(pending("copy authority is no longer held"));
        }
        if self.copied {
            return self.promote_destination();
        }
        if !self.quiesced || self.settled {
            return Err(pending("relocation must quiesce before copying"));
        }
        if self.metadata().fs_committed {
            return Err(pending(
                "publication committed; recover key settlement instead of recopying",
            ));
        }
        if self.journal.as_ref().expect("journal").phase != "quiesced" {
            return Err(pending("interrupted copy requires explicit recovery"));
        }
        let candidate = self.metadata().candidate.path.clone();
        let workspace = candidate.parent().expect("candidate parent");
        self.journal.as_mut().expect("journal").phase = "candidate-intent".into();
        self.persist()?;
        std::fs::create_dir(workspace).map_err(|e| rightkit_fs::FsError::io(workspace, e))?;
        rightkit_fs::ensure_private_dir(workspace)?;
        self.metadata_mut().candidate = RootIdentity::capture(&candidate);
        self.persist()?;
        let db = self.cfg.keyed(&self.key);
        let conn = db.open_existing_at(&db.effective_path(), OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        // The whole copy is bounded by the size-derived budget every copy of this crate uses;
        // `busy_timeout` only bounds how long a busy source is waited for.
        online_backup_sized(&db, &conn, &candidate, None)?;
        drop(conn);
        let recorded = RootIdentity::capture(&candidate);
        if recorded.file.is_none() {
            return Err(pending("cannot capture candidate identity"));
        }
        self.metadata_mut().candidate = recorded;
        self.journal.as_mut().expect("journal").phase = "candidate-created".into();
        self.persist()?;
        validate_copy(&self.cfg, &self.key, &candidate)?;
        if !same_identity(&self.metadata().candidate)? {
            return Err(pending("candidate changed during validation"));
        }
        // Nothing is published yet: if this write fails the phase stays "candidate-created", so an
        // abort cleans up instead of retaining the workspace as an unresolved FS publication.
        self.record_phase("publishing")?;
        let root = self.plan.transaction_root.as_ref().expect("root");
        let destination_parent_path = self.plan.new_path.parent().expect("checked destination");
        let destination_parent = rightkit_fs::canonicalize_plain(destination_parent_path)
            .map_err(|error| rightkit_fs::FsError::io(destination_parent_path, error))?;
        let normalized_destination =
            destination_parent.join(self.plan.new_path.file_name().expect("checked file name"));
        let relative = normalized_destination
            .strip_prefix(root)
            .map_err(|_| pending("destination escaped root"))?;
        let prepared = match FileTransaction::prepare(
            root,
            vec![TransactionEntry::create_new(relative, &candidate).private()],
            TransactionOptions::default()
                .lock_timeout(self.cfg.transition.acquire_timeout)
                .access_denied_retry(access_denied_retry()),
        ) {
            Ok(prepared) => prepared,
            Err(error) => {
                self.journal.as_mut().expect("journal").phase = "not-published".into();
                self.persist()?;
                return Err(error.into());
            }
        };
        if !same_identity(&self.metadata().candidate)? {
            return Err(pending("candidate changed during FS preparation"));
        }
        let report = match prepared.commit() {
            Ok(report) => report,
            Err(error) => {
                self.journal.as_mut().expect("journal").phase = "not-published".into();
                self.persist()?;
                return Err(error.into());
            }
        };
        self.metadata_mut().transaction_id = Some(report.id().to_string());
        match report {
            TransactionReport::Committed(report) => {
                for item in report.retained {
                    self.retain_for_cleanup(item.path, item.reason);
                }
                self.metadata_mut().fs_committed = true;
                self.metadata_mut().published = Some(RootIdentity::capture(&self.plan.new_path));
                self.journal.as_mut().expect("journal").phase = "published".into();
                self.persist()?;
            }
            TransactionReport::RolledBack(report) => {
                self.journal.as_mut().expect("journal").phase = "rolled-back".into();
                self.persist()?;
                return Err(report.cause.into());
            }
            TransactionReport::RecoveryRequired(report) => {
                for item in report.retained {
                    self.retain_for_cleanup(item.path, item.reason);
                }
                self.journal.as_mut().expect("journal").phase = "fs-recovery-required".into();
                self.persist()?;
                return Err(pending(format!(
                    "FS transaction {} requires recovery at {}",
                    report.id,
                    report.journal.display()
                )));
            }
            _ => {
                self.persist()?;
                return Err(pending("unknown FS transaction outcome"));
            }
        }
        if let Err(error) = self.promote_destination() {
            self.journal.as_mut().expect("journal").state = JournalState::Indeterminate;
            let _ = self.persist();
            return Err(error);
        }
        self.copied = true;
        self.journal.as_mut().expect("journal").phase = "copied".into();
        self.persist()
    }
    fn promote_destination(&mut self) -> Result<(), LifecycleError> {
        if !same_recorded(self.metadata().published.as_ref())? {
            return Err(pending("published destination identity changed"));
        }
        validate_copy(&self.cfg, &self.key, &self.plan.new_path)?;
        let key = self.key.clone();
        let primary = self
            .destination()
            .read_primary()
            .map_err(|e| key_store_error("destination primary", e))?;
        if !primary.as_ref().is_some_and(|p| p.as_str() == key.as_str()) {
            if primary.is_some() {
                return Err(pending("destination primary changed after publication"));
            }
            self.metadata_mut().key_store_residue = true;
            self.journal.as_mut().expect("journal").phase = "promoting".into();
            self.persist()?;
            let staged = self
                .destination()
                .read_pending()
                .map_err(|e| key_store_error("destination pending", e))?;
            if staged
                .as_ref()
                .is_some_and(|pending| pending.key.as_str() != key.as_str())
            {
                return Err(pending("destination pending key differs from copied key"));
            }
            make_primary(
                self.destination(),
                &key,
                staged.as_ref().map(|pending| &pending.revision),
            )
            .map_err(|e| key_store_error("destination promotion", e))?;
        }
        let primary = self
            .destination()
            .read_primary()
            .map_err(|e| key_store_error("destination readback", e))?
            .ok_or(LifecycleError::KeyMissing)?;
        if primary.as_str() != key.as_str() {
            return Err(pending("destination key readback differs from copied key"));
        }
        Ok(())
    }
    fn new_config(&self) -> EncryptedStoreConfig {
        let mut cfg = self.cfg.clone();
        cfg.db.path = self.plan.new_path.clone();
        cfg.db.per_host_fallback = false;
        if let Some(store) = &self.plan.new_key_store {
            cfg.key_store = store.clone();
        }
        cfg
    }
    fn reopen(&mut self, store: &EncryptedStore) -> Result<(), LifecycleError> {
        if self.reopened {
            return Ok(());
        }
        if !self.copied || self.settled {
            return Err(pending(
                "verified copy required before selected-root reopen",
            ));
        }
        // Calling reopen is explicit authority that caller durably selected destination.
        self.switched = true;
        self.metadata_mut().switched = true;
        self.journal.as_mut().expect("journal").phase = "switched".into();
        self.journal.as_mut().expect("journal").state = JournalState::CommittedCleanup;
        let result = (|| {
            if self.transition.is_some() {
                self.persist()?;
            }
            let published = self
                .metadata()
                .published
                .clone()
                .ok_or_else(|| pending("missing published identity"))?;
            if !same_identity(&published)? {
                return Err(pending("selected destination identity changed"));
            }
            let cfg = self.new_config();
            self.release();
            let established = establish_relocation(&cfg, &published, &self.key)?;
            *crate::lock_recover(&store.cfg) = cfg.clone();
            store.install(established, &cfg);
            self.reopened = true;
            Ok(())
        })();
        if let Err(error) = &result {
            store.block(format!(
                "selected relocation destination remains pending: {error}"
            ));
            self.release();
        }
        result
    }
    fn report(&self) -> RelocationSettlementReport {
        RelocationSettlementReport {
            copy_retained: if self
                .journal
                .as_ref()
                .and_then(|j| j.relocation.as_ref())
                .is_some_and(|m| m.fs_committed)
            {
                Some(self.plan.new_path.clone())
            } else {
                None
            },
            key_store_residue: self
                .journal
                .as_ref()
                .and_then(|j| j.relocation.as_ref())
                .is_some_and(|m| m.key_store_residue),
            cleanup_retained: self.cleanup.clone(),
            cleanup_errors: self.cleanup_errors.clone(),
        }
    }
    fn settle(
        &mut self,
        store: &EncryptedStore,
        settlement: RelocationSettlement,
    ) -> Result<RelocationSettlementReport, LifecycleError> {
        if self.settled {
            return Ok(self.report());
        }
        match settlement {
            RelocationSettlement::Committed => {
                if !self.reopened {
                    return Err(pending(
                        "committed settlement requires selected destination reopen",
                    ));
                }
                // Reopen released exclusive guards before acquiring ordinary shared guard.
                // Only cleanup remains; failures never revert cfg or delete final copy.
                self.cleanup_candidate();
                if self.cleanup.is_empty() {
                    self.clear_own_journals();
                }
            }
            RelocationSettlement::Aborted => {
                if self.switched {
                    return Err(pending(
                        "destination was selected; explicit committed recovery required",
                    ));
                }
                if self.journal.is_some() {
                    // Ordinary settlement never sweeps unrelated transactions under a derived
                    // standalone parent. Unknown FS outcomes require explicit recovery authority.
                    let unresolved = matches!(
                        self.journal.as_ref().expect("journal").phase.as_str(),
                        "publishing" | "fs-recovery-required"
                    );
                    if unresolved {
                        let old = self.journal.as_ref().expect("journal").old_root.clone();
                        let key = self.key.clone();
                        self.release();
                        let established = establish_relocation(&self.cfg, &old, &key)?;
                        store.install(established, &self.cfg);
                        return Err(pending("old root resumed; unresolved FS publication retained for explicit recovery"));
                    }
                    if self.metadata().fs_committed && !self.copied {
                        // Explicit abort selects old root, but unavailable/indeterminate key
                        // readback cannot erase destination recovery evidence.
                        const WHY: &str = "the destination was published and its key state is not \
                                           proven, so the journal is kept for recovery";
                        let old = super::transition::journal_path(&self.cfg.db.path);
                        let new = super::transition::journal_path(&self.plan.new_path);
                        self.retain_for_cleanup(old, WHY);
                        self.retain_for_cleanup(new, WHY);
                    }
                    self.cleanup_candidate();
                    if self.cleanup.is_empty() {
                        self.clear_own_journals();
                    }
                }
                self.release();
                if self.quiesced || self.journal.is_some() {
                    if self.cleanup.is_empty() {
                        store.resume()?;
                    } else {
                        let identity = self
                            .journal
                            .as_ref()
                            .map(|j| j.old_root.clone())
                            .ok_or_else(|| pending("source identity missing"))?;
                        let established = establish_relocation(&self.cfg, &identity, &self.key)?;
                        store.install(established, &self.cfg);
                    }
                }
            }
        }
        self.release();
        self.settled = true;
        Ok(self.report())
    }
    fn clear_own_journals(&mut self) {
        let Some(journal) = &self.journal else {
            return;
        };
        let id = journal.id;
        let roots: Vec<RootIdentity> = std::iter::once(&journal.old_root)
            .chain(journal.new_root.iter())
            .cloned()
            .collect();
        let mut kept: Vec<(PathBuf, String)> = Vec::new();
        for root in &roots {
            let path = super::transition::journal_path(&root.path);
            let parent = root
                .path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            match current_key(parent) {
                Ok(now) if now == root.dir => {}
                Ok(_) => {
                    kept.extend(still_there(
                        &path,
                        "the directory beside the journal is not the one recorded",
                    ));
                    continue;
                }
                Err(error) => {
                    kept.extend(still_there(
                        &path,
                        &format!(
                            "the directory beside the journal could not be identified: {error}"
                        ),
                    ));
                    continue;
                }
            }
            match read_journal(&root.path) {
                Ok(None) => {}
                Ok(Some(record)) if record.kind == TransitionKind::Relocate && record.id == id => {
                    if let Err(error) = super::transition::remove_journal(&root.path) {
                        kept.push((path, error.to_string()));
                    }
                }
                Ok(Some(_)) => {
                    kept.push((path, "the journal belongs to another transition".into()))
                }
                Err(error) => kept.push((path, format!("the journal could not be read: {error}"))),
            }
        }
        for (path, why) in kept {
            self.retain_for_cleanup(path, why);
        }
    }
    fn cleanup_candidate(&mut self) {
        let Some(candidate) = self
            .journal
            .as_ref()
            .and_then(|j| j.relocation.as_ref())
            .map(|metadata| metadata.candidate.clone())
        else {
            return;
        };
        let mut kept: Vec<(PathBuf, String)> = Vec::new();
        match same_identity(&candidate) {
            Err(error) => kept.push((
                candidate.path.clone(),
                format!("the candidate could not be identified: {error}"),
            )),
            Ok(true) => match remove_identified(&candidate.path, candidate.file, remove_file_at) {
                Ok(()) => remove_workspace(&candidate, &mut kept),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => kept.push((candidate.path.clone(), error.to_string())),
            },
            Ok(false) => match still_there(
                &candidate.path,
                "the candidate no longer has the identity the journal recorded",
            ) {
                Some(residue) => kept.push(residue),
                None => remove_workspace(&candidate, &mut kept),
            },
        }
        for (path, why) in kept {
            self.retain_for_cleanup(path, why);
        }
    }
    fn resolve_fs(&mut self) -> Result<(), LifecycleError> {
        let root = self.metadata().transaction_root.clone();
        if !same_identity(&root)? {
            return Err(pending("explicit transaction root identity changed"));
        }
        if self.metadata().fs_committed && !same_recorded(self.metadata().published.as_ref())? {
            return Err(pending("recorded committed copy changed"));
        }
        let report = recover_transactions_with(
            &root.path,
            &TransactionOptions::default()
                .lock_timeout(self.cfg.transition.acquire_timeout)
                .access_denied_retry(access_denied_retry()),
        )?;
        if !report.is_clean() {
            return Err(pending(
                "FS recovery retained unresolved transaction material",
            ));
        }
        for transaction in report.transactions {
            if let rightkit_fs::transaction::RecoveryOutcome::CompletedSetAside { items } =
                transaction.outcome
            {
                for item in items {
                    self.retain_for_cleanup(item.path, item.reason);
                }
            }
        }
        if !self.metadata().fs_committed && self.plan.new_path.exists() {
            let candidate = self.metadata().candidate.clone();
            if !same_identity(&candidate)? || !same_bytes(&candidate.path, &self.plan.new_path)? {
                return Err(pending(
                    "unrecorded publication does not match verified candidate",
                ));
            }
            validate_copy(&self.cfg, &self.key, &self.plan.new_path)?;
            self.metadata_mut().fs_committed = true;
            self.metadata_mut().published = Some(RootIdentity::capture(&self.plan.new_path));
        }
        if self.metadata().fs_committed && !same_recorded(self.metadata().published.as_ref())? {
            return Err(pending("recorded committed copy changed"));
        }
        Ok(())
    }
}

/// Owned participant state. Dropping leaves recovery journal intact; it never resumes old root.
pub struct RelocationSession {
    store: Arc<EncryptedStore>,
    core: RelocationCore,
}
impl RelocationSession {
    /// Store selected by this participant. Its ordinary writers remain subject to lifecycle state.
    pub fn store(&self) -> Arc<EncryptedStore> {
        self.store.clone()
    }
    pub fn quiesce(&mut self) -> Result<(), LifecycleError> {
        self.core.quiesce(&self.store)
    }
    pub fn copy(&mut self) -> Result<(), LifecycleError> {
        self.core.copy()
    }
    /// Caller must durably select new root first. Failure leaves selected destination pending.
    pub fn reopen(&mut self) -> Result<(), LifecycleError> {
        self.core.reopen(&self.store)
    }
    pub fn settle(
        &mut self,
        settlement: RelocationSettlement,
    ) -> Result<RelocationSettlementReport, LifecycleError> {
        self.core.settle(&self.store, settlement)
    }
}
impl EncryptedStore {
    pub fn prepare_relocation(
        self: &Arc<Self>,
        plan: RelocationPlan,
    ) -> Result<RelocationSession, LifecycleError> {
        let core = RelocationCore::new(self.config(), plan, self.current_key()?)?;
        Ok(RelocationSession {
            store: self.clone(),
            core,
        })
    }
    /// Standalone convenience makes its own local selection after verified copy. For daemon
    /// cutover use phased session & durable external root selection instead.
    pub fn relocate(&self, mut plan: RelocationPlan) -> RelocationReport {
        let cfg = self.config();
        if plan.transaction_root.is_none() {
            plan.transaction_root = Some(
                plan.new_path
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or_else(|| Path::new("."))
                    .to_path_buf(),
            );
        }
        let key = match self.current_key() {
            Ok(key) => key,
            Err(error) => {
                return RelocationReport::NotRelocated {
                    error,
                    copy_retained: None,
                    key_store_residue: false,
                }
            }
        };
        let mut core = match RelocationCore::new(cfg.clone(), plan, key) {
            Ok(core) => core,
            Err(error) => {
                return RelocationReport::NotRelocated {
                    error,
                    copy_retained: None,
                    key_store_residue: false,
                }
            }
        };
        let result = core
            .quiesce(self)
            .and_then(|()| core.copy())
            .and_then(|()| core.reopen(self));
        match result {
            Ok(()) => match core.settle(self, RelocationSettlement::Committed) {
                Ok(report) if report.cleanup_retained.is_empty() => RelocationReport::Relocated {
                    old_root: cfg.db.path,
                    new_root: core.plan.new_path.clone(),
                    key_store_switched: core.plan.new_key_store.is_some(),
                },
                Ok(report) => RelocationReport::RelocatedCleanupPending {
                    old_root: cfg.db.path,
                    new_root: core.plan.new_path.clone(),
                    cleanup_retained: report.cleanup_retained,
                    cleanup_errors: report.cleanup_errors,
                },
                Err(error) => {
                    let report = core.report();
                    RelocationReport::NotRelocated {
                        error,
                        copy_retained: report.copy_retained,
                        key_store_residue: report.key_store_residue,
                    }
                }
            },
            Err(error) => {
                if !core.switched {
                    let _ = core.settle(self, RelocationSettlement::Aborted);
                }
                let report = core.report();
                RelocationReport::NotRelocated {
                    error,
                    copy_retained: report.copy_retained,
                    key_store_residue: report.key_store_residue,
                }
            }
        }
    }
}

fn validate_copy(cfg: &EncryptedStoreConfig, key: &Key, path: &Path) -> Result<(), LifecycleError> {
    let db = cfg.keyed(key);
    match probe(
        &db,
        path,
        key,
        IntegrityCheck::Quick,
        OpenFlags::SQLITE_OPEN_READ_WRITE,
    ) {
        Probe::Opens => {}
        Probe::KeyRejected => {
            return Err(LifecycleError::WrongDestinationKey {
                path: path.to_path_buf(),
                detail: "copy does not open with stored key".into(),
            })
        }
        Probe::Other(error) => return Err(db_unavailable(path, error)),
    }
    if let Some(validate) = &cfg.validate {
        let mut options = db;
        options.path = path.to_path_buf();
        validate(&options).map_err(LifecycleError::ValidationFailed)?;
    }
    Ok(())
}
fn same_bytes(left: &Path, right: &Path) -> Result<bool, LifecycleError> {
    let mut left_file = retry_transient(|| std::fs::File::open(left))
        .map_err(|e| rightkit_fs::FsError::io(left, e))?;
    let mut right_file = retry_transient(|| std::fs::File::open(right))
        .map_err(|e| rightkit_fs::FsError::io(right, e))?;
    let left_id = rightkit_fs::FileKey::of_file(&left_file, left)?;
    let right_id = rightkit_fs::FileKey::of_file(&right_file, right)?;
    let mut a = [0u8; 16384];
    let mut b = [0u8; 16384];
    loop {
        let an = left_file
            .read(&mut a)
            .map_err(|e| rightkit_fs::FsError::io(left, e))?;
        let bn = right_file
            .read(&mut b)
            .map_err(|e| rightkit_fs::FsError::io(right, e))?;
        if an != bn || a[..an] != b[..bn] {
            return Ok(false);
        }
        if an == 0 {
            break;
        }
    }
    Ok(
        retry_transient_fs(|| rightkit_fs::FileKey::of_path(left))? == left_id
            && retry_transient_fs(|| rightkit_fs::FileKey::of_path(right))? == right_id,
    )
}

/// Resume a destination selected durably by the caller, without opening the old database.
///
/// Keeps relocation journals & candidate material until `settle(Committed)`. A root coordinator
/// must retain this session through its durable Completed record, then settle it. Dropping before
/// settlement preserves replay authority for another process. Aborted settlement is forbidden.
pub fn recover_relocation_session(
    cfg: EncryptedStoreConfig,
    plan: RelocationPlan,
) -> Result<RelocationSession, LifecycleError> {
    let (store, mut core) = recover_relocation_core(cfg, plan, RelocationSettlement::Committed)?;
    core.copied = true;
    core.switched = true;
    core.reopened = true;
    Ok(RelocationSession {
        store: Arc::new(store),
        core,
    })
}

/// Terminal standalone recovery under explicit transaction-root authority & durable root decision.
/// This convenience settles cleanup before returning. A phased root coordinator must instead use
/// [`recover_relocation_session`] & settle only after its own Completed record is durable.
pub fn recover_relocation(
    cfg: EncryptedStoreConfig,
    plan: RelocationPlan,
    settlement: RelocationSettlement,
) -> Result<RelocationRecovery, LifecycleError> {
    let (store, core) = recover_relocation_core(cfg, plan, settlement)?;
    Ok(finish_relocation_recovery(store, core))
}

/// Recover an interrupted relocation under the caller's explicit old-root decision.
///
/// Returns `None` only when both relocation journals are absent under transition locks.
/// Otherwise this performs the same terminal settlement as `recover_relocation(..., Aborted)`:
/// the returned store serves the verified old database; any published copy remains retained
/// and must never be treated as retry authority. A selected destination or mismatched journal
/// is refused. The caller must honor `cleanup_retained`, using the returned store or reporting
/// pending recovery instead of dropping it and attempting an ordinary open through a journal.
/// This does not resume copying or authorize deletion of a retained destination.
pub fn recover_aborted_relocation(
    cfg: EncryptedStoreConfig,
    plan: RelocationPlan,
) -> Result<Option<RelocationRecovery>, LifecycleError> {
    Ok(
        recover_relocation_core_optional(cfg, plan, RelocationSettlement::Aborted, true)?
            .map(|(store, core)| finish_relocation_recovery(store, core)),
    )
}

fn finish_relocation_recovery(
    store: EncryptedStore,
    mut core: RelocationCore,
) -> RelocationRecovery {
    core.cleanup_candidate();
    if core.cleanup.is_empty() {
        core.clear_own_journals();
    }
    RelocationRecovery {
        store: Some(store),
        report: core.report(),
    }
}

/// Retry cleanup after a coordinator's durable Completed decision. Both journals are checked
/// under transition locks. Already-settled state returns `None` without opening any database;
/// retained, mismatched or unreadable authority is never mistaken for absence.
pub fn recover_completed_relocation(
    cfg: EncryptedStoreConfig,
    plan: RelocationPlan,
) -> Result<Option<RelocationSettlementReport>, LifecycleError> {
    let Some((store, core)) =
        recover_relocation_core_optional(cfg, plan, RelocationSettlement::Committed, true)?
    else {
        return Ok(None);
    };
    let recovered = finish_relocation_recovery(store, core);
    drop(recovered.store);
    Ok(Some(recovered.report))
}

fn recover_relocation_core(
    cfg: EncryptedStoreConfig,
    plan: RelocationPlan,
    settlement: RelocationSettlement,
) -> Result<(EncryptedStore, RelocationCore), LifecycleError> {
    recover_relocation_core_optional(cfg, plan, settlement, false)?
        .ok_or_else(|| pending("no relocation journal exists"))
}

fn recover_relocation_core_optional(
    cfg: EncryptedStoreConfig,
    plan: RelocationPlan,
    settlement: RelocationSettlement,
    allow_settled: bool,
) -> Result<Option<(EncryptedStore, RelocationCore)>, LifecycleError> {
    let cfg = cfg.checked()?;
    let plan = checked_plan(&cfg, plan)?;
    let mut roots = vec![plan.new_path.as_path()];
    // Read authority only after acquiring locks: another recovery may have settled while waiting.
    // Committed recovery can proceed after old directory disappeared; it never opens old DB.
    if settlement == RelocationSettlement::Aborted || cfg.db.path.parent().is_some_and(Path::is_dir)
    {
        roots.push(cfg.db.path.as_path());
    }
    let transition = StoreTransition::begin(TransitionKind::Relocate, &roots, &cfg.transition)?;
    let old_journal = read_journal(&cfg.db.path)?;
    let new_journal = read_journal(&plan.new_path)?;
    let journal = match (old_journal, new_journal) {
        (Some(old), Some(new)) => {
            if old.id != new.id
                || old.kind != new.kind
                || old.old_root != new.old_root
                || old.new_root != new.new_root
            {
                return Err(pending(
                    "relocation journals disagree about transaction or root identities",
                ));
            }
            let a = old
                .relocation
                .as_ref()
                .ok_or_else(|| pending("old journal lacks relocation metadata"))?;
            let b = new
                .relocation
                .as_ref()
                .ok_or_else(|| pending("new journal lacks relocation metadata"))?;
            if a.candidate.path != b.candidate.path || a.transaction_root != b.transaction_root {
                return Err(pending(
                    "relocation journals disagree about candidate or FS root",
                ));
            }
            if a.candidate
                .file
                .zip(b.candidate.file)
                .is_some_and(|(left, right)| left != right)
                || a.candidate
                    .dir
                    .zip(b.candidate.dir)
                    .is_some_and(|(left, right)| left != right)
                || a.published
                    .as_ref()
                    .zip(b.published.as_ref())
                    .is_some_and(|(left, right)| left != right)
                || a.transaction_id
                    .as_ref()
                    .zip(b.transaction_id.as_ref())
                    .is_some_and(|(left, right)| left != right)
            {
                return Err(pending(
                    "relocation journals contain conflicting recorded publication identities",
                ));
            }
            // Journal writes can stop between roots. Prefer strongest recorded irreversible
            // boundary; never turn durable destination selection back into an abortable copy.
            let rank = |m: &RelocationJournal| {
                (
                    m.switched,
                    m.fs_committed,
                    m.published.is_some(),
                    m.candidate.file.is_some(),
                    m.transaction_id.is_some(),
                    m.key_store_residue,
                )
            };
            let key_store_residue = a.key_store_residue || b.key_store_residue;
            let mut selected = if rank(b) > rank(a) { new } else { old };
            selected
                .relocation
                .as_mut()
                .expect("reconciled relocation metadata")
                .key_store_residue = key_store_residue;
            selected
        }
        (Some(journal), None) | (None, Some(journal)) => journal,
        (None, None) if allow_settled => return Ok(None),
        (None, None) => return Err(pending("no relocation journal exists")),
    };
    if journal.kind != TransitionKind::Relocate
        || journal.old_root.path != cfg.db.path
        || journal.new_root.as_ref().map(|r| &r.path) != Some(&plan.new_path)
    {
        return Err(pending("relocation journal does not match supplied roots"));
    }
    let metadata = journal
        .relocation
        .as_ref()
        .ok_or_else(|| pending("legacy relocation lacks transaction recovery metadata"))?;
    if metadata.transaction_root.path != *plan.transaction_root.as_ref().expect("root") {
        return Err(pending("supplied transaction root differs from journal"));
    }
    if settlement == RelocationSettlement::Aborted && metadata.switched {
        return Err(pending(
            "journal records selected destination; abort is forbidden",
        ));
    }
    let mut source = cfg
        .key_store
        .lock_rotation(cfg.transition.acquire_timeout)
        .map_err(|e| key_store_error("source rotation lock", e))?;
    let key = source
        .read_primary()
        .map_err(|e| key_store_error("source primary", e))?
        .ok_or(LifecycleError::KeyMissing)?;
    let destination = match &plan.new_key_store {
        Some(store) if !Arc::ptr_eq(store, &cfg.key_store) => Some(
            store
                .lock_rotation(cfg.transition.acquire_timeout)
                .map_err(|e| key_store_error("destination rotation lock", e))?,
        ),
        _ => None,
    };
    let mut core = RelocationCore {
        cfg,
        plan,
        key,
        transition: Some(transition),
        source: Some(source),
        destination,
        journal: Some(journal),
        quiesced: true,
        copied: false,
        switched: false,
        reopened: false,
        settled: false,
        cleanup: Vec::new(),
        cleanup_errors: Vec::new(),
    };
    core.resolve_fs()?;
    if settlement == RelocationSettlement::Committed {
        if !core.metadata().fs_committed {
            return Err(pending(
                "durable destination selection has no committed verified database",
            ));
        }
        core.promote_destination()?;
        core.metadata_mut().switched = true;
        core.journal.as_mut().expect("journal").phase = "switched".into();
        core.journal.as_mut().expect("journal").state = JournalState::CommittedCleanup;
        core.persist()?;
    } else if !same_identity(&core.journal.as_ref().expect("journal").old_root)? {
        return Err(pending("old root identity changed before abort"));
    }
    let selected = if settlement == RelocationSettlement::Committed {
        core.new_config()
    } else {
        core.cfg.clone()
    };
    let identity = if settlement == RelocationSettlement::Committed {
        core.metadata().published.clone()
    } else {
        core.journal.as_ref().map(|j| j.old_root.clone())
    }
    .ok_or_else(|| pending("selected identity missing"))?;
    core.release();
    let established = establish_relocation(&selected, &identity, &core.key)?;
    let store = EncryptedStore::from_relocation(selected, established);
    Ok(Some((store, core)))
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    fn written(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, name.as_bytes()).unwrap();
        path
    }

    #[test]
    fn an_unchanged_root_matches() {
        let dir = tempfile::tempdir().unwrap();
        let root = written(dir.path(), "root.db");
        let recorded = RootIdentity::capture(&root);
        assert!(same_identity(&recorded).unwrap());
        assert!(same_recorded(Some(&recorded)).unwrap());
    }

    #[test]
    fn a_missing_replaced_or_unrecorded_root_is_a_mismatch_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let root = written(dir.path(), "root.db");
        let recorded = RootIdentity::capture(&root);

        // Another file renamed over the name (both exist at once, so the inode is a different one).
        let other = written(dir.path(), "other.db");
        std::fs::rename(&other, &root).unwrap();
        assert!(!same_identity(&recorded).unwrap(), "replaced");

        std::fs::remove_file(&root).unwrap();
        assert!(!same_identity(&recorded).unwrap(), "missing");

        let never_there = RootIdentity::capture(&dir.path().join("never.db"));
        assert!(never_there.file.is_none());
        assert!(
            !same_identity(&never_there).unwrap(),
            "nothing was recorded"
        );
        assert!(!same_recorded(None).unwrap());
    }

    /// A root whose identity cannot be read is neither the same nor a changed one: it is the
    /// error. (A path below a regular file cannot be opened on Unix: not-a-directory, not
    /// not-found.)
    #[cfg(unix)]
    #[test]
    fn an_unreadable_root_is_an_error_never_a_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let file = written(dir.path(), "plain.db");
        let mut recorded = RootIdentity::capture(&file);
        assert!(recorded.file.is_some());
        recorded.path = file.join("below");
        let error = same_identity(&recorded).unwrap_err();
        assert!(matches!(error, LifecycleError::Fs(_)), "{error:?}");
        assert!(RootIdentity::try_capture(&recorded.path).is_err());
        // `capture` keeps its infallible shape: no identity.
        assert!(RootIdentity::capture(&recorded.path).file.is_none());
    }

    #[test]
    fn a_workspace_that_cannot_be_removed_is_kept_with_the_error() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let candidate = written(&workspace, "candidate.db");
        let recorded = RootIdentity::capture(&candidate);
        std::fs::remove_file(&candidate).unwrap();
        // The directory is not empty, so its removal fails with an OS error on every platform.
        written(&workspace, "stray");
        let mut kept = Vec::new();
        remove_workspace(&recorded, &mut kept);
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!(kept[0].0, workspace);
        assert!(kept[0].1.contains("os error"), "{}", kept[0].1);
        // Emptied, the same call removes it and keeps nothing.
        std::fs::remove_file(workspace.join("stray")).unwrap();
        let mut kept = Vec::new();
        remove_workspace(&recorded, &mut kept);
        assert!(kept.is_empty(), "{kept:?}");
        assert!(!workspace.exists());
    }

    #[test]
    fn a_path_that_is_still_there_is_reported_with_its_reason() {
        let dir = tempfile::tempdir().unwrap();
        let root = written(dir.path(), "root.db");
        let (path, why) = still_there(&root, "kept for a test").unwrap();
        assert_eq!(path, root);
        assert_eq!(why, "kept for a test");
        std::fs::remove_file(&root).unwrap();
        assert!(still_there(&root, "kept for a test").is_none());
    }
}
