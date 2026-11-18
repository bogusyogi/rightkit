//! Vocabulary store: disk and singleton glue around the pure [`crate::vocabulary::VocabularyStore`].
//!
//! The pure store logic (validate, normalize, add, remove, import, serde shapes) lives in
//! [`crate::vocabulary`]. This layer owns the lazy-loaded in-memory store, the JSON persistence and
//! the write ordering. Saves reach the file in mutation order with no store lock held across the
//! I/O ([`rightkit_fs::ordered_write::OrderedWriteQueue`]), and a vocabulary file that existed but
//! could not be read or parsed at load is never overwritten.
//!
//! The host supplies where the file lives and how a whole file is replaced; this module names no
//! product, path or directory.
//!
//! ```ignore
//! static VOCABULARY: PersistedVocabulary = PersistedVocabulary::new(VocabularyHost {
//!     path: || my_app_data_dir().join("vocabulary.json"),
//!     write: |path, bytes| my_atomic_replace(path, bytes),
//! });
//! let entry = VOCABULARY.add("Kubernetes")?;
//! ```

use crate::vocab_learn::Suggestion;
use crate::vocabulary::{SuggestedEntry, VocabularyEntry, VocabularyStore};
use rightkit_fs::ordered_write::OrderedWriteQueue;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// What the host decides: the file location (asked on every load and save, so a host whose data
/// directory can change keeps working) and the whole-file replace used to save.
/// Host callbacks must not reenter operations on this store: path resolution can run
/// during lazy initialization or under the state lock; replacement owns a write turn.
#[derive(Clone, Copy)]
pub struct VocabularyHost {
    pub path: fn() -> PathBuf,
    /// Atomic whole-file replace: the file holds the old bytes or the new bytes, never a mix.
    pub write: fn(&Path, &[u8]) -> std::io::Result<()>,
}

pub struct PersistedVocabulary {
    host: VocabularyHost,
    store: OnceLock<Mutex<VocabularyStore>>,
    queue: OrderedWriteQueue,
}

impl PersistedVocabulary {
    pub const fn new(host: VocabularyHost) -> Self {
        Self {
            host,
            store: OnceLock::new(),
            queue: OrderedWriteQueue::new(),
        }
    }

    fn load_from_disk(&self) -> VocabularyStore {
        match rightkit_fs::replaced::read_replaced(&(self.host.path)()) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(store) => store,
                Err(_) => {
                    self.queue.refuse_persist();
                    VocabularyStore::default()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                VocabularyStore::default()
            }
            Err(_) => {
                self.queue.refuse_persist();
                VocabularyStore::default()
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VocabularyStore> {
        self.store
            .get_or_init(|| Mutex::new(self.load_from_disk()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Atomic whole-file persist through the host's replace. A plain truncating write would leave
    /// a truncated file that parses as empty after a crash mid-write, silently losing every entry.
    fn persist_atomic(&self, destination: &Path, bytes: &[u8]) -> Result<(), String> {
        (self.host.write)(destination, bytes).map_err(|error| {
            format!(
                "atomically replace vocab {}: {error}",
                destination.display()
            )
        })
    }

    /// The disk-write step of `mutate`.
    fn persist(&self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        if self.queue.persist_refused() {
            return Err(
                "vocabulary file could not be read at load; not overwriting it".to_string(),
            );
        }
        self.persist_atomic(path, bytes)
    }

    /// Mutate the store under its lock, persist the serialized snapshot on disk after releasing
    /// it. The closure returns (result, dirty); dirty=false skips the write entirely. The write
    /// ticket is taken while the store lock is held (ticket order == mutation order), then the
    /// lock is dropped before waiting on earlier writes: disk I/O never blocks readers or other
    /// mutations. Save failures still surface to the caller; the mutation itself stays applied.
    fn mutate<R>(
        &self,
        f: impl FnOnce(&mut VocabularyStore) -> Result<(R, bool), String>,
    ) -> Result<R, String> {
        let mut store = self.lock();
        let (result, dirty) = f(&mut store)?;
        if !dirty {
            return Ok(result);
        }
        let bytes = serde_json::to_vec_pretty(&*store).map_err(|e| format!("serialize: {e}"))?;
        let path = (self.host.path)();
        // Ticket order follows mutation order. Unwinding before the write cancels its turn.
        let ticket = self.queue.take_ticket();
        drop(store);
        self.queue
            .write_in_turn(ticket, || self.persist(&path, &bytes))?;
        Ok(result)
    }

    pub fn list(&self) -> Vec<VocabularyEntry> {
        self.lock().entries.clone()
    }

    pub fn add(&self, term: &str) -> Result<VocabularyEntry, String> {
        self.mutate(|store| store.add(term, now_ms()).map(|entry| (entry, true)))
    }

    pub fn add_with_aliases(
        &self,
        term: &str,
        sounds_like: Vec<String>,
    ) -> Result<VocabularyEntry, String> {
        self.mutate(|store| {
            store
                .add_with_aliases(term, sounds_like, now_ms())
                .map(|entry| (entry, true))
        })
    }

    pub fn remove(&self, id: u64) -> Result<bool, String> {
        self.mutate(|store| {
            let removed = store.remove(id);
            Ok((removed, removed))
        })
    }

    pub fn import_terms(&self, terms: Vec<String>) -> Result<Vec<VocabularyEntry>, String> {
        self.mutate(|store| {
            let added = store.import(terms, now_ms());
            Ok((added, true))
        })
    }

    pub fn list_suggestions(&self) -> Vec<SuggestedEntry> {
        self.lock().suggestions().to_vec()
    }

    pub fn suggest_all(&self, candidates: Vec<Suggestion>) -> Result<(), String> {
        self.mutate(|store| {
            store.suggest_all(candidates, now_ms());
            Ok(((), true))
        })
    }

    pub fn accept_suggestion(&self, term: &str) -> Result<Option<VocabularyEntry>, String> {
        self.mutate(|store| {
            let result = store.accept_suggestion(term, now_ms());
            Ok((result, true))
        })
    }

    pub fn dismiss_suggestion(&self, term: &str) -> Result<bool, String> {
        self.mutate(|store| {
            let changed = store.dismiss_suggestion(term);
            Ok((changed, changed))
        })
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
