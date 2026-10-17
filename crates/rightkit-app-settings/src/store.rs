use crate::backend::{read_values_only, Edit, FileBackend, KeyLayout, SettingsBackend, Stored};
use crate::options::{
    DefaultsProvider, FileFormat, FlatMigration, KeyValidator, NullMeans, OnRefused, OptionalFill,
    ReadOptions, StoreOptions, Validator,
};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("settings io: {0}")]
    Io(#[from] io::Error),
    /// The new settings file IS in place at `path`, but a step after the replace failed: on Unix
    /// and macOS the directory sync, on Windows nothing (there is no directory flush). The write is
    /// not undone and must not be repeated as if it had failed: an `update` re-run would apply its
    /// edit a second time. [`Store::get`] reads the new file, and no change event is emitted for
    /// this write. `source` says which step failed. What the new file is guaranteed to survive is
    /// given by [`StoreOptions::achieved_durability`].
    #[error("settings file {} was replaced, but a later step failed: {source}", path.display())]
    Replaced {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("settings serialization: {0}")]
    Json(#[from] serde_json::Error),
    #[error("settings file is schema v{found}, this build supports up to v{supported}")]
    NewerSchema { found: u32, supported: u32 },
    #[error("invalid settings value: {0}")]
    Invalid(String),
    #[error("invalid schema: {0}")]
    BadSchema(&'static str),
    #[error("settings changed since revision {expected}; now at revision {actual}")]
    Conflict { expected: u64, actual: u64 },
    #[error("timed out waiting for the settings lock at {0}")]
    LockTimeout(String),
    /// The file is unreadable as JSON and the store was opened with [`OnCorrupt::Refuse`].
    /// Nothing was moved or written.
    #[error("settings file is corrupt: {0}")]
    Corrupt(String),
    /// A step that keeps the previous content failed, so the operation stopped before it changed
    /// the settings file: nothing was written, moved or replaced. `step` says which step,
    /// `path` is the file being kept, `kept_as` is where it was to go (the quarantine name, the
    /// copy, or `<path>.bak`), and `source` is the I/O error.
    ///
    /// With [`PreserveStep::Quarantine`] the file at `path` is damaged and is still there, byte
    /// for byte: it could not be moved aside, so it is not overwritten. A retry repeats the step,
    /// so remove the cause first (permissions, a full disk, a directory or a locked file at
    /// `kept_as`). The in-memory value is unchanged and no change event fires.
    #[error(
        "settings file {} could not be {step} to {}: {source}; nothing was written",
        path.display(),
        kept_as.display()
    )]
    PreserveFailed {
        step: PreserveStep,
        path: PathBuf,
        kept_as: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Which step of keeping the previous content failed. See [`Error::PreserveFailed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PreserveStep {
    /// Moving a damaged file aside ([`crate::OnCorrupt::Quarantine`]).
    Quarantine,
    /// Copying a damaged file aside before it is overwritten
    /// ([`crate::StoreOptions::keep_corrupt_copy`]).
    CorruptCopy,
    /// Refreshing `<path>.bak` from the current good file.
    Backup,
}

impl std::fmt::Display for PreserveStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PreserveStep::Quarantine => "moved aside",
            PreserveStep::CorruptCopy => "copied aside",
            PreserveStep::Backup => "backed up",
        })
    }
}

/// Why a settings file is refused: this build must neither read nor change it. See
/// [`StoreOptions::on_refused`] and [`Store::refusal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Refusal {
    /// The file's schema version is newer than the code's (a downgrade).
    NewerSchema { found: u32, supported: u32 },
    /// The file is longer than [`StoreOptions::max_bytes`].
    Oversized { len: u64, limit: u64 },
}

impl Refusal {
    /// The error `open` returns under [`OnRefused::Fail`] and a write returns under
    /// [`OnRefused::ReadOnlyDefaults`].
    fn to_error(self) -> Error {
        match self {
            Refusal::NewerSchema { found, supported } => Error::NewerSchema { found, supported },
            Refusal::Oversized { len, limit } => Error::Invalid(format!(
                "settings file is {len} bytes, over the {limit} byte limit"
            )),
        }
    }
}

/// The schema version a stored document declares. A flat document has none unless
/// [`StoreOptions::flat_version_key`] names the key that holds it.
fn declared_version(stored: &Stored, format: FileFormat, flat_key: Option<&str>) -> Option<u32> {
    match (format, flat_key) {
        (FileFormat::Flat, Some(key)) => stored
            .values
            .get(key)
            .and_then(Value::as_u64)
            .map(|v| u32::try_from(v).unwrap_or(u32::MAX)),
        _ => stored.version,
    }
}

/// One forward transform over the raw settings object, bringing it to `to`.
#[derive(Clone, Copy)]
pub struct Migration {
    pub to: u32,
    /// Transforms the raw values. An error aborts the open; nothing is written.
    pub apply: fn(&mut Map<String, Value>) -> Result<(), Error>,
}

/// Per-app settings identity: namespace, current version, ordered migrations.
#[derive(Clone)]
pub struct Schema {
    pub namespace: String,
    pub version: u32,
    pub migrations: Vec<Migration>,
}

impl Schema {
    pub fn new(namespace: impl Into<String>, version: u32) -> Self {
        Self {
            namespace: namespace.into(),
            version,
            migrations: Vec::new(),
        }
    }
    pub fn migration(
        mut self,
        to: u32,
        apply: fn(&mut Map<String, Value>) -> Result<(), Error>,
    ) -> Self {
        self.migrations.push(Migration { to, apply });
        self
    }
    pub(crate) fn validate(&self) -> Result<(), Error> {
        let ok_ns = !self.namespace.is_empty()
            && self
                .namespace
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
        if !ok_ns {
            return Err(Error::BadSchema("namespace must be [A-Za-z0-9._-]+"));
        }
        let mut last = 0;
        for m in &self.migrations {
            if m.to <= last || m.to > self.version {
                return Err(Error::BadSchema(
                    "migrations must ascend and not exceed version",
                ));
            }
            last = m.to;
        }
        Ok(())
    }
}

/// Top-level keys whose value changed in one committed update.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub keys: Vec<String>,
}

/// What [`Store::patch_dry_run`] would produce. Nothing is written.
#[derive(Debug, Clone, PartialEq)]
pub struct PatchPreview<T> {
    /// The settings after the patch.
    pub value: T,
    /// Top-level keys the patch would change, including open keys.
    pub change: Change,
    /// The exact document a committed patch would store: declared values plus retained open
    /// keys, as the `values` of the envelope (or the flat object).
    pub document: Map<String, Value>,
}

type Listener = Arc<dyn Fn(&Change) + Send + Sync>;
type Listeners = Arc<Mutex<Vec<(u64, Listener)>>>;

/// Unsubscribes on drop.
pub struct Subscription {
    id: u64,
    listeners: Listeners,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        lock(&self.listeners).retain(|(id, _)| *id != self.id);
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// A flat document the migration hook changed on open, waiting to be committed once.
struct Pending {
    original: Map<String, Value>,
    migrated: Map<String, Value>,
}

struct Inner<T> {
    value: T,
    revision: u64,
    /// Backend stamp of the document `value` was read from. `None` forces a re-read.
    stamp: Option<String>,
}

/// A typed view of one settings document, with change events and revisions.
pub struct Store<T> {
    /// The settings file for file-backed stores. Empty for [`Store::with_backend`].
    path: PathBuf,
    schema: Schema,
    backend: Box<dyn SettingsBackend>,
    defaults: Option<DefaultsProvider>,
    open_keys: Option<KeyValidator>,
    null_means: NullMeans,
    validator: Option<Validator>,
    /// Keeps top-level keys the settings type does not declare (flat files and open keys).
    retain_unknown: bool,
    /// Commit the first save of a missing file even when nothing changed.
    persist_defaults: bool,
    /// Fill optional fields the file omits from the defaults (see [`resolve`]).
    fill_optional: bool,
    read_only: bool,
    format: FileFormat,
    flat_version_key: Option<String>,
    on_refused: OnRefused,
    /// App-supplied migration for flat files; see [`StoreOptions::flat_migration`].
    flat_migration: Option<FlatMigration>,
    /// The flat document as loaded and as migrated, until the open-time commit has stored it.
    pending: Mutex<Option<Pending>>,
    inner: Mutex<Inner<T>>,
    listeners: Listeners,
    next_id: Mutex<u64>,
}

impl<T: Serialize + DeserializeOwned + Default + Clone> Store<T> {
    /// Open `<root>/<namespace>/settings.json`, migrating, repairing, or
    /// recovering as needed. Persists once if migration or recovery changed it.
    pub fn open(root: &Path, schema: Schema) -> Result<Self, Error> {
        schema.validate()?;
        let path = root.join(&schema.namespace).join("settings.json");
        Self::open_path(&path, StoreOptions::new(schema))
    }

    /// Open the settings file at `path`, in any directory. The path is not tied to the namespace.
    /// The schema is still validated, because the envelope records its namespace.
    pub fn open_path(path: impl AsRef<Path>, options: StoreOptions) -> Result<Self, Error> {
        let path = path.as_ref().to_path_buf();
        let backend = FileBackend::from_options(path.clone(), &options);
        Self::assemble(path, Box::new(backend), options, false)
    }

    /// Open the file at `path` for reading only. Creates nothing (no directory, lock, or backup),
    /// never waits for a writer's lock, and refuses `update` and `patch`. Migrations and defaults
    /// apply in memory only.
    pub fn open_read_only(path: impl AsRef<Path>, options: StoreOptions) -> Result<Self, Error> {
        let path = path.as_ref().to_path_buf();
        let backend = FileBackend::from_options(path.clone(), &options);
        Self::assemble(path, Box::new(backend), options, true)
    }

    /// Open over a custom [`SettingsBackend`]. `options.format` decides only whether undeclared
    /// keys are retained: [`FileFormat::Flat`] or open keys retain them.
    pub fn with_backend(
        backend: impl SettingsBackend + 'static,
        options: StoreOptions,
    ) -> Result<Self, Error> {
        Self::assemble(PathBuf::new(), Box::new(backend), options, false)
    }

    fn assemble(
        path: PathBuf,
        backend: Box<dyn SettingsBackend>,
        options: StoreOptions,
        read_only: bool,
    ) -> Result<Self, Error> {
        let StoreOptions {
            schema,
            format,
            defaults,
            open_keys,
            null_means,
            validator,
            persist_defaults,
            fill_optional,
            flat_migration,
            quarantine_name,
            on_refused,
            flat_version_key,
            ..
        } = options;
        schema.validate()?;
        quarantine_name.validate()?;
        let retain_unknown = format == FileFormat::Flat || open_keys.is_some();
        // A refused file (newer than this build, or oversized) is never read into the value, moved
        // or written. Under `ReadOnlyDefaults` the store opens on the defaults instead of failing.
        let guard = on_refused == OnRefused::ReadOnlyDefaults;
        let mut refused = false;
        let mut snapshot = match backend.size_refusal()? {
            Some(_) if guard => {
                refused = true;
                None
            }
            Some(r) => return Err(r.to_error()),
            None => backend.load()?,
        };
        let newer = snapshot
            .as_ref()
            .and_then(|s| declared_version(s, format, flat_version_key.as_deref()))
            .filter(|found| *found > schema.version);
        if let Some(found) = newer {
            if !guard {
                return Err(Error::NewerSchema {
                    found,
                    supported: schema.version,
                });
            }
            refused = true;
            snapshot = None;
        }
        // Flat files have no version, so the app's hook runs here, once, before defaults and
        // repair. Never in read-only mode: that must not diverge from, or write, the file.
        let mut pending = None;
        if let (Some(migrate), false, FileFormat::Flat, Some(stored)) =
            (&flat_migration, read_only, format, snapshot.as_mut())
        {
            if stored.version.is_none() {
                let original = stored.values.clone();
                let (migrated, changed) = migrate(sorted_keys(&original))?;
                if changed {
                    stored.values = migrated.clone();
                    stored.needs_rewrite = true;
                    pending = Some(Pending { original, migrated });
                }
            }
        }
        let loaded = decode::<T>(snapshot.as_ref(), &schema, defaults.as_ref(), fill_optional)?;
        let store = Store {
            path,
            schema,
            backend,
            defaults,
            open_keys,
            null_means,
            validator,
            retain_unknown,
            persist_defaults,
            fill_optional,
            read_only,
            format,
            flat_version_key,
            on_refused,
            flat_migration,
            pending: Mutex::new(pending),
            inner: Mutex::new(Inner {
                value: loaded.value.clone(),
                revision: loaded.revision,
                stamp: None,
            }),
            listeners: Arc::default(),
            next_id: Mutex::new(0),
        };
        if loaded.dirty && !read_only && !refused {
            // Migration, repair, recovery, or first write of a missing file: commit once. Never
            // for a refused file: the defaults it opened on are not written over it.
            store.transact(None, &[], |_, _| Ok(()))?;
        }
        Ok(store)
    }

    /// Why this build must not read or change the settings file right now, or `None`. Checked
    /// against the file on every call, so it changes when the file does. With the default
    /// [`OnRefused::Fail`] a store can only be refused after it was opened, when another process
    /// replaced the file.
    pub fn refusal(&self) -> Option<Refusal> {
        if let Ok(Some(r)) = self.backend.size_refusal() {
            return Some(r);
        }
        match self.backend.load() {
            Ok(Some(s)) => self.newer(&s).map(|found| Refusal::NewerSchema {
                found,
                supported: self.schema.version,
            }),
            _ => None,
        }
    }

    /// Whether a write could go ahead: the store is not read-only and the file is not refused (see
    /// [`Store::refusal`]). `false` is what [`OnRefused::ReadOnlyDefaults`] reports for a newer or
    /// oversized file. A write can still fail for other reasons (I/O, a lock timeout, validation).
    pub fn writable(&self) -> bool {
        !self.read_only && self.refusal().is_none()
    }

    /// The version the stored document declares when it is newer than this build's schema.
    fn newer(&self, stored: &Stored) -> Option<u32> {
        declared_version(stored, self.format, self.flat_version_key.as_deref())
            .filter(|found| *found > self.schema.version)
    }

    /// The settings file for file-backed stores; empty for custom backends.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Current settings. Changes committed by another store or process on the
    /// same file are visible: the backend's stamp (for files: inode, mtime,
    /// ctime, length) is compared with the one the cache was read under, and the
    /// document is re-read only when it differs. A backend without a stamp is
    /// re-read on every call. Falls back to the last known value if the document
    /// cannot be read right now.
    pub fn get(&self) -> T {
        let mut g = lock(&self.inner);
        // Stamp before reading: a commit that races the read leaves a stale stamp, and the
        // next call re-reads.
        let stamp = self.backend.stamp().ok().flatten();
        let fresh = stamp.is_some() && stamp == g.stamp;
        if !fresh {
            match self.peek() {
                Some(l) => {
                    g.value = l.value;
                    g.revision = l.revision;
                    g.stamp = stamp;
                }
                None => g.stamp = None,
            }
        }
        g.value.clone()
    }

    /// Monotonic revision of the committed settings (bumped by every committed
    /// write from any process). Use with [`Store::update_at`]. For flat files the revision is a
    /// fingerprint of the file bytes.
    pub fn revision(&self) -> u64 {
        let _ = self.get();
        lock(&self.inner).revision
    }

    /// Lock-free read of the committed document. `None` when the document is missing, is a
    /// backup fallback (the last known value is kept), or is not this schema's version.
    fn peek(&self) -> Option<Loaded<T>> {
        let snapshot = self.backend.load().ok().flatten()?;
        if snapshot.needs_rewrite {
            return None;
        }
        if snapshot.version.is_some_and(|v| v != self.schema.version) {
            return None;
        }
        if self.newer(&snapshot).is_some() {
            return None;
        }
        decode::<T>(
            Some(&snapshot),
            &self.schema,
            self.defaults.as_ref(),
            self.fill_optional,
        )
        .ok()
    }

    /// Checks for a change committed by another store or process and tells this store's
    /// subscribers. Cross-process change has no push signal: the backend's stamp is compared with
    /// the one the cache was read under (as in [`Store::get`]); when it differs, the document is
    /// re-read, the cache is replaced, and one [`Change`] naming the top-level keys whose value
    /// differs is delivered to every subscriber, outside the lock. Call it on a timer or from a
    /// file watcher of the app's choosing; the crate starts no thread. Returns the change (empty
    /// when nothing differs, no event fires). Unlike `get`, a read failure is an `Err` and leaves
    /// the cache and stamp as they were, so the next call retries. Writes made through this store
    /// refresh the cache themselves and are not reported again.
    pub fn poll_external(&self) -> Result<Change, Error> {
        let change = {
            let mut g = lock(&self.inner);
            let stamp = self.backend.stamp()?;
            if stamp.is_some() && stamp == g.stamp {
                return Ok(Change { keys: Vec::new() });
            }
            if self.on_refused == OnRefused::ReadOnlyDefaults
                && self.backend.size_refusal()?.is_some()
            {
                // Refused: the store keeps its value, as for a newer file below.
                g.stamp = stamp;
                return Ok(Change { keys: Vec::new() });
            }
            let snapshot = self.backend.load()?;
            let Some(snapshot) = snapshot else {
                g.stamp = stamp;
                return Ok(Change { keys: Vec::new() });
            };
            if snapshot.needs_rewrite
                || snapshot.version.is_some_and(|v| v != self.schema.version)
                || self.newer(&snapshot).is_some()
            {
                // A document this build must migrate or repair: `get` keeps the last value too.
                g.stamp = stamp;
                return Ok(Change { keys: Vec::new() });
            }
            let loaded = decode::<T>(
                Some(&snapshot),
                &self.schema,
                self.defaults.as_ref(),
                self.fill_optional,
            )?;
            let change = diff(&to_object(&g.value)?, &to_object(&loaded.value)?);
            g.value = loaded.value;
            g.revision = loaded.revision;
            g.stamp = stamp;
            change
        };
        self.emit(change.clone());
        Ok(change)
    }

    /// Mutate through a closure. This is a cross-process transaction: take the
    /// backend's exclusion, reload the committed settings, apply `f` to that fresh value,
    /// save atomically, release. Concurrent updates to different keys from
    /// other stores or processes are therefore never lost. On any failure the
    /// in-memory value is unchanged. Subscribers fire after the save, outside the lock.
    pub fn update<R>(&self, f: impl FnOnce(&mut T) -> Result<R, Error>) -> Result<R, Error> {
        self.transact(None, &[], |t, _| f(t))
    }

    /// Compare-and-swap form of [`Store::update`]: fails with
    /// [`Error::Conflict`] unless the committed revision still equals `expected`
    /// (from [`Store::revision`]); nothing is written on conflict.
    pub fn update_at<R>(
        &self,
        expected: u64,
        f: impl FnOnce(&mut T) -> Result<R, Error>,
    ) -> Result<R, Error> {
        self.transact(Some(expected), &[], |t, _| f(t))
    }

    /// `nulls`: keys the edit explicitly set to `null`; under `SetNull` one the document does not
    /// hold yet is stored even when the value does not change.
    fn transact<R>(
        &self,
        expected: Option<u64>,
        nulls: &[String],
        f: impl FnOnce(&mut T, &mut Map<String, Value>) -> Result<R, Error>,
    ) -> Result<R, Error> {
        if self.read_only {
            return Err(Error::Invalid("settings store is read-only".into()));
        }
        if self.on_refused == OnRefused::ReadOnlyDefaults {
            // Before the backend takes its lock: a refused file gets no `.lock` sidecar either.
            if let Some(refusal) = self.refusal() {
                return Err(refusal.to_error());
            }
        }
        let mut g = lock(&self.inner);
        let mut f = Some(f);
        let mut done: Option<(Plan<T, R>, u64)> = None;
        let committed = self.backend.patch_ordered(&mut |cur| {
            // A flat file declares its version in a value, which `decode` does not look at.
            if let Some(found) = cur.and_then(|s| self.newer(s)) {
                return Err(Error::NewerSchema {
                    found,
                    supported: self.schema.version,
                });
            }
            let migrated = self.migrate_pending(cur)?;
            let cur = migrated.as_ref().or(cur);
            let l = decode::<T>(
                cur,
                &self.schema,
                self.defaults.as_ref(),
                self.fill_optional,
            )?;
            if let Some(e) = expected {
                if l.revision != e {
                    return Err(Error::Conflict {
                        expected: e,
                        actual: l.revision,
                    });
                }
            }
            let op = f
                .take()
                .ok_or(Error::Invalid("settings transaction ran twice".into()))?;
            let p = plan(self.retain_unknown, self.validator.as_ref(), &l, op)?;
            let first_save = self.persist_defaults && l.missing;
            // Under SetNull, a null-valued key the file does not hold yet is stored even when the
            // in-memory value already equals it (no change to report).
            let null_absent = self.null_means == NullMeans::SetNull
                && nulls
                    .iter()
                    .any(|k| p.after.get(k).is_some_and(Value::is_null) && !l.raw.contains_key(k));
            let edit = if l.dirty || !p.change.keys.is_empty() || first_save || null_absent {
                Edit::Commit(Stored {
                    version: Some(self.schema.version),
                    namespace: Some(self.schema.namespace.clone()),
                    values: p.after.clone(),
                    revision: l.revision,
                    needs_rewrite: false,
                })
            } else {
                Edit::Keep
            };
            // The order the settings type serialises its keys in, for a backend that writes
            // ordered text. `to_object` above cannot carry it: a map has already sorted it.
            let layout = if matches!(edit, Edit::Commit(_)) {
                KeyLayout::of(&p.next)
            } else {
                KeyLayout::default()
            };
            done = Some((p, l.revision));
            Ok((edit, layout))
        })?;
        *lock(&self.pending) = None;
        let (p, base) = done.ok_or(Error::Invalid(
            "settings transaction produced no plan".into(),
        ))?;
        let Plan {
            out, next, change, ..
        } = p;
        g.value = next;
        g.revision = committed.unwrap_or(base);
        // The stamp of our own write is not known here; the next get() re-reads once.
        g.stamp = None;
        drop(g);
        self.emit(change);
        Ok(out)
    }

    /// The document to commit for the open-time flat migration: the one `assemble` migrated, or,
    /// when the file changed between that load and this transaction, the hook run again on the
    /// fresh file. `None` when there is nothing pending.
    fn migrate_pending(&self, cur: Option<&Stored>) -> Result<Option<Stored>, Error> {
        let (Some(pending), Some(migrate)) = (
            lock(&self.pending)
                .as_ref()
                .map(|p| (p.original.clone(), p.migrated.clone())),
            self.flat_migration.as_ref(),
        ) else {
            return Ok(None);
        };
        let Some(stored) = cur else {
            return Ok(None);
        };
        let mut next = stored.clone();
        if stored.values == pending.0 {
            next.values = pending.1;
        } else {
            let (migrated, changed) = migrate(sorted_keys(&stored.values))?;
            if !changed {
                return Ok(None);
            }
            next.values = migrated;
        }
        next.needs_rewrite = true;
        Ok(Some(next))
    }

    /// Patch top-level keys. `null` resets a declared key to its default and removes an open key.
    /// Unknown keys are rejected unless [`StoreOptions::allow_unknown_keys`] accepts them. With
    /// `allow`, only the listed declared keys may change, and open keys are refused.
    pub fn patch(&self, patch: &Value, allow: Option<&[&str]>) -> Result<T, Error> {
        let open = self.open_keys.as_ref();
        let nulls: Vec<String> = patch
            .as_object()
            .map(|o| {
                o.iter()
                    .filter(|(_, v)| v.is_null())
                    .map(|(k, _)| k.clone())
                    .collect()
            })
            .unwrap_or_default();
        self.transact(None, &nulls, |t, extras| {
            apply_patch(t, extras, patch, allow, open, self.null_means)
        })?;
        Ok(self.get())
    }

    /// Explicit remove, independent of [`NullMeans`]. Deletes each named key if present; a key that
    /// is absent changes nothing, creates nothing, and causes no write.
    ///
    /// - An open key (accepted by [`StoreOptions::allow_unknown_keys`]) is deleted from the
    ///   document.
    /// - A declared key returns to what the settings type yields when the key is absent (its
    ///   serde default, or `None`); the type's own serialisation writes it back, so a declared
    ///   key can be reset this way but never leaves the file. A declared key with no serde
    ///   default is an `Error::Invalid`.
    /// - Any other key is `Error::Invalid`.
    pub fn remove_keys(&self, keys: &[&str]) -> Result<T, Error> {
        let open = self.open_keys.as_ref();
        self.transact(None, &[], |t, extras| apply_remove(t, extras, keys, open))?;
        Ok(self.get())
    }

    /// What [`Store::remove_keys`] would produce, with the same validation; nothing is written.
    pub fn remove_keys_dry_run(&self, keys: &[&str]) -> Result<PatchPreview<T>, Error> {
        let snapshot = self.backend.load()?;
        let l = decode::<T>(
            snapshot.as_ref(),
            &self.schema,
            self.defaults.as_ref(),
            self.fill_optional,
        )?;
        let open = self.open_keys.as_ref();
        let p = plan(
            self.retain_unknown,
            self.validator.as_ref(),
            &l,
            |t, extras| apply_remove(t, extras, keys, open),
        )?;
        Ok(PatchPreview {
            value: p.next,
            change: p.change,
            document: p.after,
        })
    }

    /// The result [`Store::patch`] would produce, with the same validation, but nothing written.
    /// It reads the committed settings without taking the lock.
    pub fn patch_dry_run(
        &self,
        patch: &Value,
        allow: Option<&[&str]>,
    ) -> Result<PatchPreview<T>, Error> {
        let snapshot = self.backend.load()?;
        let l = decode::<T>(
            snapshot.as_ref(),
            &self.schema,
            self.defaults.as_ref(),
            self.fill_optional,
        )?;
        let open = self.open_keys.as_ref();
        let p = plan(
            self.retain_unknown,
            self.validator.as_ref(),
            &l,
            |t, extras| apply_patch(t, extras, patch, allow, open, self.null_means),
        )?;
        Ok(PatchPreview {
            value: p.next,
            change: p.change,
            document: p.after,
        })
    }

    pub fn subscribe(&self, f: impl Fn(&Change) + Send + Sync + 'static) -> Subscription {
        let mut n = lock(&self.next_id);
        *n += 1;
        lock(&self.listeners).push((*n, Arc::new(f)));
        Subscription {
            id: *n,
            listeners: self.listeners.clone(),
        }
    }

    fn emit(&self, change: Change) {
        if change.keys.is_empty() {
            return;
        }
        let ls: Vec<Listener> = lock(&self.listeners)
            .iter()
            .map(|(_, l)| l.clone())
            .collect();
        for l in ls {
            l(&change);
        }
    }
}

/// A decoded committed document.
struct Loaded<T> {
    value: T,
    /// Migrated or repaired, so it must be rewritten.
    dirty: bool,
    /// No document was found on disk; the values come from the defaults.
    missing: bool,
    revision: u64,
    /// Migrated raw values, including keys the settings type does not declare.
    raw: Map<String, Value>,
}

/// Decodes a backend document: applies migrations, then repairs typed fields. A missing document
/// decodes to the defaults provider's object, or to `T::default()` (dirty, so it is written).
fn decode<T: Serialize + DeserializeOwned + Default>(
    snapshot: Option<&Stored>,
    schema: &Schema,
    defaults: Option<&DefaultsProvider>,
    fill_optional: bool,
) -> Result<Loaded<T>, Error> {
    let (mut values, version, revision, mut dirty) = match snapshot {
        Some(s) => (s.values.clone(), s.version, s.revision, s.needs_rewrite),
        None => match defaults {
            Some(provide) => (provided_object(provide)?, Some(schema.version), 0, false),
            None => (Map::new(), Some(schema.version), 0, true),
        },
    };
    if let Some(v) = version {
        if v > schema.version {
            return Err(Error::NewerSchema {
                found: v,
                supported: schema.version,
            });
        }
        if v < schema.version {
            for m in schema.migrations.iter().filter(|m| m.to > v) {
                (m.apply)(&mut values)?;
            }
            dirty = true;
        }
    }
    let raw = values.clone();
    let base = match defaults {
        Some(provide) => provided_object(provide)?,
        None => to_object(&T::default())?,
    };
    let (value, repaired) = resolve::<T>(values, base, snapshot.is_none(), fill_optional)?;
    Ok(Loaded {
        value,
        dirty: dirty || repaired,
        missing: snapshot.is_none(),
        revision,
        raw,
    })
}

/// Reads the settings at `path` without writing anything: no lock, backup, quarantine, or
/// directory. The main file is used, then `<path>.bak`, then `defaults`. A missing or invalid
/// field takes its value from `defaults`, not from `T::default()`. Optional fields the file merely
/// omits are not filled; see [`read_with_options`] and [`ReadOptions::fill_optional`]. Migrations
/// do not run, so this reads the current document format only. `defaults` must be a JSON object.
/// An I/O error other than `NotFound` (on the main file) is an `Err`; it is never replaced by
/// defaults.
pub fn read_with_defaults<T: Serialize + DeserializeOwned + Default>(
    path: impl AsRef<Path>,
    format: FileFormat,
    defaults: &Value,
) -> Result<T, Error> {
    read_with_options(path, defaults, ReadOptions::new(format))
}

/// [`read_with_defaults`] with options.
pub fn read_with_options<T: Serialize + DeserializeOwned + Default>(
    path: impl AsRef<Path>,
    defaults: &Value,
    options: ReadOptions,
) -> Result<T, Error> {
    let base = match defaults {
        Value::Object(object) => object.clone(),
        _ => return Err(Error::BadSchema("defaults must be a JSON object")),
    };
    let found = read_values_only(
        path.as_ref(),
        options.format,
        options.sidecars,
        options.utf8_bom,
        options.max_bytes,
    )?;
    let absent = found.is_none();
    let file = found.unwrap_or_default();
    let value = match options.fill {
        OptionalFill::WhenRepaired => {
            // Resolve without filling first; only a read that repaired another field re-resolves
            // with the omitted optional keys filled from the defaults.
            let (plain, repaired) = resolve::<T>(file.clone(), base.clone(), absent, false)?;
            if repaired {
                resolve::<T>(file, base, absent, true)?.0
            } else {
                plain
            }
        }
        OptionalFill::Always => resolve::<T>(file, base, absent, true)?.0,
        OptionalFill::Never => resolve::<T>(file, base, absent, false)?.0,
    };
    Ok(value)
}

fn provided_object(provide: &DefaultsProvider) -> Result<Map<String, Value>, Error> {
    match provide() {
        Value::Object(m) => Ok(m),
        _ => Err(Error::BadSchema(
            "defaults provider must return a JSON object",
        )),
    }
}

/// Result of running an edit against a decoded document.
struct Plan<T, R> {
    out: R,
    next: T,
    /// Full logical document to store: declared values plus retained keys.
    after: Map<String, Value>,
    change: Change,
}

/// Runs `f` on a copy of the decoded document and computes what would be stored and changed.
/// Undeclared keys travel in `extras` and are kept only when `retain` is set.
fn plan<T, R>(
    retain: bool,
    validator: Option<&Validator>,
    l: &Loaded<T>,
    f: impl FnOnce(&mut T, &mut Map<String, Value>) -> Result<R, Error>,
) -> Result<Plan<T, R>, Error>
where
    T: Serialize + DeserializeOwned + Default + Clone,
{
    let typed_before = to_object(&l.value)?;
    let extras_before = if retain {
        let known = known_keys::<T>(&typed_before)?;
        l.raw
            .iter()
            .filter(|(k, _)| !known.contains(k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect::<Map<String, Value>>()
    } else {
        Map::new()
    };
    let mut next = l.value.clone();
    let mut extras = extras_before.clone();
    let out = f(&mut next, &mut extras)?;
    if !retain {
        extras.clear();
    }
    let before = overlay(typed_before, extras_before);
    let after = overlay(to_object(&next)?, extras);
    let change = diff(&before, &after);
    if !change.keys.is_empty() {
        if let Some(check) = validator {
            check(&after).map_err(Error::Invalid)?;
        }
    }
    Ok(Plan {
        out,
        next,
        after,
        change,
    })
}

/// Declared keys: the defaults' keys plus the keys the current value serializes.
fn known_keys<T: Serialize + Default>(
    typed: &Map<String, Value>,
) -> Result<BTreeSet<String>, Error> {
    let defaults = to_object(&T::default())?;
    let mut keys: BTreeSet<String> = defaults.keys().cloned().collect();
    keys.extend(typed.keys().cloned());
    Ok(keys)
}

/// `values` with its keys in sorted order. `serde_json::Map` is sorted only without the
/// `preserve_order` feature, which the workspace enables through other crates, so the migration
/// hook gets the same key order either way.
fn sorted_keys(values: &Map<String, Value>) -> Map<String, Value> {
    let mut keys: Vec<&String> = values.keys().collect();
    keys.sort();
    keys.into_iter()
        .map(|key| (key.clone(), values[key].clone()))
        .collect()
}

/// `base`, plus the entries of `extra` whose key `base` lacks.
fn overlay(mut base: Map<String, Value>, extra: Map<String, Value>) -> Map<String, Value> {
    for (k, v) in extra {
        base.entry(k).or_insert(v);
    }
    base
}

fn apply_patch<T: Serialize + DeserializeOwned + Default>(
    current: &mut T,
    extras: &mut Map<String, Value>,
    patch: &Value,
    allow: Option<&[&str]>,
    open_keys: Option<&KeyValidator>,
    null_means: NullMeans,
) -> Result<(), Error> {
    let obj = patch
        .as_object()
        .ok_or_else(|| Error::Invalid("patch must be an object".into()))?;
    let defaults = to_object(&T::default())?;
    let mut typed = to_object(current)?;
    let mut next_extras = extras.clone();
    for (k, nv) in obj {
        if defaults.contains_key(k) || typed.contains_key(k) {
            if allow.is_some_and(|a| !a.contains(&k.as_str())) {
                return Err(Error::Invalid(format!("key `{k}` is not patchable")));
            }
            if nv.is_null() {
                match null_means {
                    NullMeans::Reset => match defaults.get(k) {
                        Some(d) => typed.insert(k.clone(), d.clone()),
                        None => typed.remove(k),
                    },
                    NullMeans::SetNull => typed.insert(k.clone(), Value::Null),
                };
            } else {
                typed.insert(k.clone(), nv.clone());
            }
        } else if open_keys.is_some_and(|valid| allow.is_none() && valid(k.as_str())) {
            if nv.is_null() {
                // SetNull always stores `null` for an open key, held or absent, so applying the
                // patch twice changes nothing. Reset removes a held key and ignores an absent one.
                // Removal under SetNull is `remove_keys` only.
                if null_means == NullMeans::SetNull {
                    next_extras.insert(k.clone(), Value::Null);
                } else {
                    next_extras.remove(k);
                }
            } else {
                next_extras.insert(k.clone(), nv.clone());
            }
        } else {
            return Err(Error::Invalid(format!("unknown key `{k}`")));
        }
    }
    *current =
        serde_json::from_value(Value::Object(typed)).map_err(|e| Error::Invalid(e.to_string()))?;
    *extras = next_extras;
    Ok(())
}

fn apply_remove<T: Serialize + DeserializeOwned + Default>(
    current: &mut T,
    extras: &mut Map<String, Value>,
    keys: &[&str],
    open_keys: Option<&KeyValidator>,
) -> Result<(), Error> {
    let declared = to_object(&T::default())?;
    let mut typed = to_object(current)?;
    let mut next_extras = extras.clone();
    for k in keys {
        if declared.contains_key(*k) || typed.contains_key(*k) {
            typed.remove(*k);
        } else if open_keys.is_some_and(|valid| valid(k)) {
            next_extras.remove(*k);
        } else {
            return Err(Error::Invalid(format!("unknown key `{k}`")));
        }
    }
    *current =
        serde_json::from_value(Value::Object(typed)).map_err(|e| Error::Invalid(e.to_string()))?;
    *extras = next_extras;
    Ok(())
}

/// The settings value as a JSON object. `null` is the empty object: `serde_json::Value::default()`
/// is `Null`, so a `Store<Value>` with no file yet starts from `{}`, as 0.1.1 did. Any other
/// non-object value (array, string, number, bool) is refused.
fn to_object<T: Serialize>(v: &T) -> Result<Map<String, Value>, Error> {
    match serde_json::to_value(v)? {
        Value::Object(m) => Ok(m),
        Value::Null => Ok(Map::new()),
        _ => Err(Error::BadSchema(
            "settings type must serialize to an object",
        )),
    }
}

fn diff(old: &Map<String, Value>, new: &Map<String, Value>) -> Change {
    let mut keys: Vec<String> = new
        .iter()
        .filter(|(k, v)| old.get(*k) != Some(*v))
        .map(|(k, _)| k.clone())
        .collect();
    keys.extend(old.keys().filter(|k| !new.contains_key(*k)).cloned());
    keys.sort();
    Change { keys }
}

/// The one resolver behind `Store::open`, `Store::get` and [`read_with_options`].
///
/// Inputs: the file's values (`file`), the caller's complete defaults (`defaults`), whether there
/// was no usable file at all (`absent`), and `fill_optional`. Output: the settings value and
/// whether the file itself needs repair (a file key was dropped, or a required field was missing
/// from the file and came from `defaults`).
///
/// 1. Wholesale: `defaults` overlaid with every file key. If that parses, every file key is
///    accepted. (Fast path; also the only path that works when `defaults` alone is incomplete.)
/// 2. Otherwise `defaults` alone must parse (else `Error::Invalid`). Start from `defaults` and
///    overlay the file keys one at a time in sorted order, so the outcome depends on the content
///    only, never on key order. A key is accepted if the struct still parses with it. If it does
///    not, a default-supplied key may be shadowing it as a different spelling (a serde `alias`:
///    the two together are a duplicate field): drop one default-supplied key and retry, and
///    accept when the parse then succeeds. If nothing makes it parse, the file key is dropped and
///    its default stays. Cost is at most `n` retries per key, `O(n^2)` parses for `n` keys.
///    A file holding both spellings of one field keeps the alphabetically first one.
///    A null-valued file key that collides with another file key is the one dropped (so a null
///    legacy alias beside its canonical key reads as the canonical value, and the mirror reads as
///    the alias value). Two valued spellings still keep the alphabetically first.
/// 3. Optional fields. A default-supplied key is optional when the struct parses without it. Unless
///    `fill_optional` is set (or `absent`: there is no file to omit anything), optional
///    default-supplied keys that the file does not contain are removed, so plain serde defaults
///    apply. A file key that was present but invalid keeps its default. If the removal makes the
///    struct unparseable (fields that are only jointly required), it is undone.
///
/// `T::default()` is never consulted: when nothing parses the result is an error, not a silent
/// reset.
fn resolve<T: DeserializeOwned>(
    file: Map<String, Value>,
    defaults: Map<String, Value>,
    absent: bool,
    fill_optional: bool,
) -> Result<(T, bool), Error> {
    let parse = |m: &Map<String, Value>| serde_json::from_value::<T>(Value::Object(m.clone()));
    let mut repaired = false;
    let mut accepted: BTreeSet<String> = BTreeSet::new();
    let mut acc = defaults.clone();
    for (k, v) in &file {
        acc.insert(k.clone(), v.clone());
    }
    if parse(&acc).is_ok() {
        accepted.extend(file.keys().cloned());
    } else {
        acc = defaults.clone();
        if let Err(e) = parse(&acc) {
            return Err(Error::Invalid(format!(
                "the settings file and the caller's defaults do not form a valid value: {e}"
            )));
        }
        let mut keys: Vec<&String> = file.keys().collect();
        keys.sort();
        for k in keys {
            let mut cand = acc.clone();
            cand.insert(k.clone(), file[k].clone());
            if parse(&cand).is_ok() {
                acc = cand;
                accepted.insert(k.clone());
                continue;
            }
            // A null-valued file key that collides is the one dropped: a null legacy alias beside
            // its canonical key, or a null canonical key beside a valued alias. If the key being
            // added is itself null it simply stays rejected (below).
            if !file[k].is_null() {
                let nulls: Vec<String> = accepted
                    .iter()
                    .filter(|a| file[*a].is_null())
                    .cloned()
                    .collect();
                let mut swapped = false;
                for a in nulls {
                    let mut retry = cand.clone();
                    retry.remove(&a);
                    if parse(&retry).is_ok() {
                        acc = retry;
                        accepted.remove(&a);
                        accepted.insert(k.clone());
                        swapped = true;
                        break;
                    }
                }
                if swapped {
                    repaired = true;
                    continue;
                }
            }
            // A default-supplied spelling of the same field is a duplicate: drop it, keep the file.
            let shadows: Vec<String> = acc
                .keys()
                .filter(|d| *d != k && !accepted.contains(*d))
                .cloned()
                .collect();
            let mut taken = false;
            for d in shadows {
                let mut retry = cand.clone();
                retry.remove(&d);
                if parse(&retry).is_ok() {
                    acc = retry;
                    accepted.insert(k.clone());
                    taken = true;
                    break;
                }
            }
            if !taken {
                repaired = true;
            }
        }
    }
    // Default-supplied keys: in `acc` but not from the file.
    let supplied: Vec<String> = acc
        .keys()
        .filter(|k| !accepted.contains(*k))
        .cloned()
        .collect();
    let full = acc.clone();
    for d in supplied {
        let mut without = acc.clone();
        without.remove(&d);
        if parse(&without).is_ok() {
            // Optional. Keep it for a missing file, fill_optional, or a present-but-invalid key.
            if !(absent || fill_optional || file.contains_key(&d)) {
                acc = without;
            }
        } else if !absent && !file.contains_key(&d) {
            // Required, and the file omitted it.
            repaired = true;
        }
    }
    let value = match parse(&acc) {
        Ok(v) => v,
        Err(_) => parse(&full)
            .map_err(|e| Error::Invalid(format!("settings do not form a valid value: {e}")))?,
    };
    Ok((value, repaired))
}
