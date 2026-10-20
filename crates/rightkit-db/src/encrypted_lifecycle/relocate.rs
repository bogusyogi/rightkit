//! `relocate`: the db-owned primitive that moves a database and its key to a new root.
//!
//! rightkit-daemon's `RootCutover` composes filesystem moves with opaque lifecycle participants the
//! app registers; this is the database participant. It takes no daemon dependency: the app wraps
//! `EncryptedStore::relocate` in whatever participant interface PK07 defines.
//!
//! Procedure, all under the transition guard of **both** roots (taken in path order, bounded) and
//! the key store's rotation lock:
//!
//! 1. Before anything is touched: the new path and its sidecars must not exist
//!    (`DestinationExists`), and a destination key store must not already hold a different primary
//!    key (`WrongDestinationKey`).
//! 2. Drain the host, the writer and the pools; take both guards; journal old and new root
//!    identities beside both roots.
//! 3. Verified online backup of the old database to the new path (staged privately, checked with
//!    `quick_check`, synced, published by rename), then reopen of the copy with the store key and
//!    the config's validation callback.
//! 4. If a destination key store is given, stage the key there, promote it, and read it back.
//! 5. Release the guards and reopen the store at the new root through the destination key store,
//!    from what it reads back.
//!
//! The old root is never changed or deleted: it stays a complete, openable store until the caller's
//! cutover decides otherwise, and removing it is a separate, identity-checked step that belongs to
//! the root cutover, not to this primitive. If the new root cannot be reopened, the store reopens at
//! the old root and the copy this call published is removed only while it still has the identity it
//! was published with.
//!
//! TODO(PK01-6): the companion files of the root (settings, logs, caches) move through rightkit-fs
//! `FileTransaction` / `recover_transactions` once PK01-6 lands; the daemon's `RootCutover`
//! composes that with this participant. Until then this primitive moves the database and its key
//! only, using the existing fs primitives (the SQLite backup API, atomic publish and file identity).

use super::error::LifecycleError;
use super::keystore::{Key, ProtectedKeyStore};
use super::rotate::{make_primary, NoFaults};
use super::store::{
    db_unavailable, establish, key_store_error, probe, remove_owned_copy, EncryptedStore,
    EncryptedStoreConfig, Probe,
};
use super::transition::{
    next_id, Journal, JournalState, RootIdentity, StoreTransition, TransitionKind,
};
use crate::{online_backup_to, IntegrityCheck};
use rusqlite::OpenFlags;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Where to move the database and, optionally, the key store its key moves to.
pub struct RelocationPlan {
    /// The database file's new path. It and its sidecars must not exist.
    pub new_path: PathBuf,
    /// The key store that serves the new root, when the root's key account changes with it. It
    /// must be empty or already hold the store's key as primary. `None` keeps the current key
    /// store (the key does not depend on the root).
    pub new_key_store: Option<Arc<dyn ProtectedKeyStore>>,
}

impl RelocationPlan {
    pub fn new(new_path: impl Into<PathBuf>) -> Self {
        Self {
            new_path: new_path.into(),
            new_key_store: None,
        }
    }

    pub fn with_new_key_store(mut self, key_store: Arc<dyn ProtectedKeyStore>) -> Self {
        self.new_key_store = Some(key_store);
        self
    }
}

/// How a relocation ended.
#[derive(Debug)]
#[non_exhaustive]
pub enum RelocationReport {
    /// The store now serves `new_root`, reopened with the key read back from key storage. The old
    /// root is intact and unchanged; removing it is the caller's separate step.
    Relocated {
        old_root: PathBuf,
        new_root: PathBuf,
        key_store_switched: bool,
    },
    /// The store still serves the old root, which was never changed.
    NotRelocated {
        error: LifecycleError,
        /// The copy at the new root, when it could not be removed safely and is retained.
        copy_retained: Option<PathBuf>,
        /// The destination key store received the key before the failure and still holds it.
        key_store_residue: bool,
    },
}

fn not_relocated(error: LifecycleError) -> RelocationReport {
    RelocationReport::NotRelocated {
        error,
        copy_retained: None,
        key_store_residue: false,
    }
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// The destination and its sidecars must all be absent.
fn check_destination(new_path: &Path) -> Result<(), LifecycleError> {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let candidate = with_suffix(new_path, suffix);
        if std::fs::symlink_metadata(&candidate).is_ok() {
            return Err(LifecycleError::DestinationExists { path: candidate });
        }
    }
    match new_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        Some(parent) if !parent.is_dir() => Err(LifecycleError::Precondition(format!(
            "the new root's directory {} does not exist",
            parent.display()
        ))),
        _ => Ok(()),
    }
}

impl EncryptedStore {
    /// Move the database (and its key, when `plan.new_key_store` is given) to a new root. See the
    /// module documentation for the procedure and what is guaranteed on each outcome.
    pub fn relocate(&self, plan: RelocationPlan) -> RelocationReport {
        let cfg = self.config();
        let old_path = cfg.db.path.clone();
        let new_path = plan.new_path.clone();
        if new_path.to_str().is_none() || new_path.file_name().is_none() {
            return not_relocated(LifecycleError::InvalidConfig(
                "the new path must be valid UTF-8 and name a file".into(),
            ));
        }
        if new_path == old_path {
            return not_relocated(LifecycleError::Precondition(
                "the new path is the current path".into(),
            ));
        }
        if let Err(error) = check_destination(&new_path) {
            return not_relocated(error);
        }
        let key = match self.current_key() {
            Ok(key) => key,
            Err(error) => return not_relocated(error),
        };
        if let Some(destination) = &plan.new_key_store {
            match destination.read_primary() {
                Ok(None) => {}
                Ok(Some(existing)) if existing.as_str() == key.as_str() => {}
                Ok(Some(_)) => {
                    return not_relocated(LifecycleError::WrongDestinationKey {
                        path: new_path,
                        detail: "the destination key store already holds a different primary key"
                            .into(),
                    })
                }
                Err(error) => return not_relocated(key_store_error("destination primary", error)),
            }
        }

        if let Err(error) = self.drain(TransitionKind::Relocate, false) {
            return not_relocated(error);
        }
        let outcome = self.relocate_guarded(&cfg, &key, &plan);
        match outcome {
            Ok(done) => done,
            Err(failure) => {
                // The old root was never changed: reopen it.
                let _ = self.resume();
                RelocationReport::NotRelocated {
                    error: failure.error,
                    copy_retained: failure.copy_retained,
                    key_store_residue: failure.key_store_residue,
                }
            }
        }
    }

    fn relocate_guarded(
        &self,
        cfg: &EncryptedStoreConfig,
        key: &Key,
        plan: &RelocationPlan,
    ) -> Result<RelocationReport, Box<Failure>> {
        let old_path = cfg.db.path.clone();
        let new_path = plan.new_path.clone();
        let transition = StoreTransition::begin(
            TransitionKind::Relocate,
            &[old_path.as_path(), new_path.as_path()],
            &cfg.transition,
        )
        .map_err(Failure::plain)?;
        let _rotation = cfg
            .key_store
            .lock_rotation(cfg.transition.acquire_timeout)
            .map_err(|error| Failure::plain(key_store_error("rotation lock", error)))?;
        // The destination may have appeared between the first check and the guard.
        check_destination(&new_path).map_err(Failure::plain)?;
        match cfg.key_store.read_primary() {
            Ok(Some(primary)) if primary.as_str() == key.as_str() => {}
            Ok(_) => {
                return Err(Failure::plain(LifecycleError::KeyStoreMismatch {
                    detail: "the primary key in protected storage differs from the open key".into(),
                }))
            }
            Err(error) => return Err(Failure::plain(key_store_error("primary", error))),
        }

        let journal = Journal {
            kind: TransitionKind::Relocate,
            state: JournalState::Active,
            phase: "copying".to_string(),
            retain_previous: false,
            pending: None,
            id: next_id(),
            old_root: RootIdentity::capture(&old_path),
            new_root: Some(RootIdentity::capture(&new_path)),
        };
        transition.write_journal(&journal).map_err(Failure::plain)?;

        // A failure after the copy was published removes the copy only while it still has the
        // identity it was published with; a failure before publication removes nothing.
        let fail = |error: LifecycleError, copy: rightkit_fs::FileKey| -> Box<Failure> {
            let removed = remove_owned_copy(&new_path, Some(copy));
            let mut failure = Failure::plain(error);
            if !removed && std::fs::symlink_metadata(&new_path).is_ok() {
                failure.copy_retained = Some(new_path.clone());
            }
            let _ = transition.clear_journal();
            failure
        };

        // Verified online backup of the old store to the new root.
        let db = cfg.keyed(key);
        let source_path = cfg.db.effective_path();
        let published = (|| -> Result<(), LifecycleError> {
            let conn = db.open_existing_at(&source_path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
            online_backup_to(&db, &conn, &new_path, None)?;
            drop(conn);
            Ok(())
        })();
        if let Err(error) = published {
            // `online_backup_to` publishes last and cleans up its own staging on failure.
            let _ = transition.clear_journal();
            return Err(Failure::plain(error));
        }
        let copy_key = match rightkit_fs::FileKey::of_path(&new_path) {
            Ok(copy_key) => copy_key,
            Err(error) => {
                let mut failure = Failure::plain(LifecycleError::Fs(error));
                failure.copy_retained = Some(new_path.clone());
                let _ = transition.clear_journal();
                return Err(failure);
            }
        };

        // Reopen the copy with the store key and the caller's validation.
        match probe(
            &db,
            &new_path,
            key,
            IntegrityCheck::Quick,
            OpenFlags::SQLITE_OPEN_READ_WRITE,
        ) {
            Probe::Opens => {}
            Probe::KeyRejected => {
                return Err(fail(
                    LifecycleError::WrongDestinationKey {
                        path: new_path.clone(),
                        detail: "the copy does not reopen with the store's key".into(),
                    },
                    copy_key,
                ))
            }
            Probe::Other(error) => return Err(fail(db_unavailable(&new_path, error), copy_key)),
        }
        if let Some(validate) = &cfg.validate {
            let mut options = db.clone();
            options.path = new_path.clone();
            if let Err(reason) = validate(&options) {
                return Err(fail(LifecycleError::ValidationFailed(reason), copy_key));
            }
        }

        // Move the key to the destination key store and read it back.
        let mut key_store_residue = false;
        if let Some(destination) = &plan.new_key_store {
            let already =
                matches!(destination.read_primary(), Ok(Some(ref k)) if k.as_str() == key.as_str());
            if !already {
                key_store_residue = true;
                if let Err(reason) = make_primary(&**destination, key, None) {
                    // Whatever was staged is ours: the destination's pending slot was not in use.
                    if let Ok(Some(pending)) = destination.read_pending() {
                        let _ = destination.discard_pending(&pending.revision);
                    }
                    let mut failure = fail(
                        LifecycleError::KeyStoreMismatch {
                            detail: format!(
                                "the destination key store did not take the key: {reason}"
                            ),
                        },
                        copy_key,
                    );
                    failure.key_store_residue =
                        destination.read_primary().is_ok_and(|slot| slot.is_some());
                    return Err(failure);
                }
            }
        }

        let mut activated = journal.clone();
        activated.phase = "copied".to_string();
        let _ = transition.write_journal(&activated);

        // Release the guards, then reopen at the new root from what key storage reads back.
        drop(_rotation);
        transition.settle();
        let mut new_cfg = cfg.clone();
        new_cfg.db.path = new_path.clone();
        if let Some(destination) = &plan.new_key_store {
            new_cfg.key_store = destination.clone();
        }
        // The guards are released and the copy is verified: the journals have served their purpose.
        // Removing them first keeps the reopen below from mistaking them for an interrupted
        // relocation.
        let _ = super::transition::remove_journal(&new_path);
        let _ = super::transition::remove_journal(&old_path);
        match establish(&new_cfg, &NoFaults) {
            Ok(established) => {
                *crate::lock_recover(&self.cfg) = new_cfg.clone();
                self.install(established, &new_cfg);
                Ok(RelocationReport::Relocated {
                    old_root: old_path,
                    new_root: new_path,
                    key_store_switched: plan.new_key_store.is_some(),
                })
            }
            Err(error) => {
                let removed = remove_owned_copy(&new_path, Some(copy_key));
                Err(Box::new(Failure {
                    error,
                    copy_retained: if removed || !new_path.exists() {
                        None
                    } else {
                        Some(new_path)
                    },
                    key_store_residue,
                }))
            }
        }
    }
}

struct Failure {
    error: LifecycleError,
    copy_retained: Option<PathBuf>,
    key_store_residue: bool,
}

impl Failure {
    fn plain(error: LifecycleError) -> Box<Self> {
        Box::new(Self {
            error,
            copy_retained: None,
            key_store_residue: false,
        })
    }
}
