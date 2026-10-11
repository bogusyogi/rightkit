//! Storage behind [`crate::Store`]: the [`SettingsBackend`] trait and the file-backed default.

use crate::atomic::{atomic_write_mode, WriteMode};
use crate::options::{
    FileFormat, OnCorrupt, QuarantineName, ReplaceMode, SharingRetry, StoreOptions,
};
use crate::store::Error;
use serde::de::{MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::ser::PrettyFormatter;
use serde_json::value::RawValue;
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const ENVELOPE_VERSION: &str = "schemaVersion";
const ENVELOPE_NS: &str = "namespace";
const ENVELOPE_VALUES: &str = "values";
const ENVELOPE_REVISION: &str = "revision";
const LOCK_WAIT: Duration = Duration::from_secs(10);

/// A settings document as a backend holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct Stored {
    /// Schema version of a versioned document. `None` for an unversioned (flat) document, which
    /// runs no migrations.
    pub version: Option<u32>,
    /// Namespace of a versioned document.
    pub namespace: Option<String>,
    /// Top-level values, including keys the settings type does not declare.
    pub values: Map<String, Value>,
    /// Committed revision. Ignored on commit: the backend assigns the next one.
    pub revision: u64,
    /// Set when a backend served the document from its backup. The store rewrites it on open.
    pub needs_rewrite: bool,
}

/// What an edit decided to do with the committed document.
#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    /// Leave storage untouched.
    Keep,
    /// Replace the committed document with this one.
    Commit(Stored),
}

/// Storage for one settings document. The default is [`FileBackend`]. An app can plug in its own,
/// for example an encrypted database.
///
/// Implementations own cross-process exclusion: `patch` must run its edit while no other writer
/// can commit. A SQLCipher-backed implementation would look like this (illustrative, not compiled):
///
/// ```rust,ignore
/// impl SettingsBackend for SqlCipherSettings {
///     fn load(&self) -> Result<Option<Stored>, Error> {
///         // SELECT key, value FROM settings; map rows into `values`; version/namespace from a meta row
///     }
///     fn patch(&self, edit: &mut dyn FnMut(Option<&Stored>) -> Result<Edit, Error>)
///         -> Result<Option<u64>, Error>
///     {
///         // BEGIN IMMEDIATE; load as in `load`; match edit(...)? { Keep => ROLLBACK,
///         // Commit(doc) => write changed rows, bump revision, COMMIT }
///     }
/// }
/// ```
pub trait SettingsBackend: Send + Sync {
    /// The committed document, or `None` when nothing is stored. Must not change storage.
    fn load(&self) -> Result<Option<Stored>, Error>;

    /// A cheap identity of the committed document that changes whenever it is committed. `None`
    /// means unknown, and the store re-reads on every `get`. Stamp first, then load: a commit
    /// between the two only makes the next check re-read.
    fn stamp(&self) -> Result<Option<String>, Error> {
        Ok(None)
    }

    /// Runs `edit` exactly once on the committed document, under the backend's exclusion, and
    /// commits what it returns. The edit receives `None` when nothing is stored. Returns the new
    /// revision, or `None` when the edit kept storage.
    fn patch(
        &self,
        edit: &mut dyn FnMut(Option<&Stored>) -> Result<Edit, Error>,
    ) -> Result<Option<u64>, Error>;

    /// The committed revision, or 0 when nothing is stored.
    fn revision(&self) -> Result<u64, Error> {
        Ok(self.load()?.map_or(0, |s| s.revision))
    }

    /// Replaces the committed document without a revision check. Store updates check revisions;
    /// prefer them.
    fn save(&self, doc: &Stored) -> Result<u64, Error> {
        let committed = self.patch(&mut |_| Ok(Edit::Commit(doc.clone())))?;
        Ok(committed.unwrap_or(doc.revision))
    }
}

/// The default backend: one JSON file, written atomically under a cross-process lock. The previous
/// good file is kept as `<path>.bak`.
#[derive(Debug, Clone)]
pub struct FileBackend {
    path: PathBuf,
    format: FileFormat,
    on_corrupt: OnCorrupt,
    sidecars: bool,
    pretty: bool,
    mode: WriteMode,
    keep_corrupt_copy: bool,
    quarantine_name: QuarantineName,
    trailing_newline: bool,
}

/// Serialises writers of sidecar-free backends within this process.
static PROCESS_WRITERS: Mutex<()> = Mutex::new(());

impl FileBackend {
    /// A backend for the file at `path`, in the given layout, with the default policies:
    /// quarantine corrupt files, keep sidecars, and write `to_vec_pretty` output.
    pub fn new(path: impl Into<PathBuf>, format: FileFormat) -> Self {
        Self {
            path: path.into(),
            format,
            on_corrupt: OnCorrupt::Quarantine,
            sidecars: true,
            pretty: false,
            mode: WriteMode::default(),
            keep_corrupt_copy: false,
            quarantine_name: QuarantineName::Timestamped,
            trailing_newline: false,
        }
    }

    /// How a write replaces the file. Default [`ReplaceMode::Rename`].
    pub fn replace_mode(mut self, mode: ReplaceMode) -> Self {
        self.mode.replace = mode;
        self
    }

    /// Flush before the replace (default `true`). See [`StoreOptions::durable`].
    pub fn durable(mut self, yes: bool) -> Self {
        self.mode.durable = yes;
        self
    }

    /// Retry the replace on Windows sharing violations. See
    /// [`StoreOptions::retry_sharing_violation`].
    pub fn retry_sharing_violation(mut self, attempts: u32, delay: std::time::Duration) -> Self {
        self.mode.retry = Some(SharingRetry { attempts, delay });
        self
    }

    /// What to do with an unreadable file. Default [`OnCorrupt::Quarantine`].
    pub fn on_corrupt(mut self, policy: OnCorrupt) -> Self {
        self.on_corrupt = policy;
        self
    }

    /// A backend configured by the same options a [`crate::Store`] takes (format, policies,
    /// sidecars, replace mode, naming, newline).
    pub fn with_options(path: impl Into<PathBuf>, options: &StoreOptions) -> Self {
        Self::from_options(path, options)
    }

    /// A backend configured by the same options that [`crate::Store`] was opened with.
    pub(crate) fn from_options(path: impl Into<PathBuf>, options: &StoreOptions) -> Self {
        Self {
            path: path.into(),
            format: options.format,
            on_corrupt: options.on_corrupt,
            sidecars: options.sidecars,
            pretty: options.pretty_sorted_newline,
            mode: WriteMode {
                replace: options.replace,
                durable: options.durable,
                retry: options.sharing_retry,
            },
            keep_corrupt_copy: options.keep_corrupt_copy,
            quarantine_name: options.quarantine_name.clone(),
            trailing_newline: options.trailing_newline,
        }
    }

    /// The settings file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The on-disk layout.
    pub fn format(&self) -> FileFormat {
        self.format
    }

    fn write(&self, next: &Stored, prev_revision: u64) -> Result<u64, Error> {
        let (mut bytes, envelope_revision) = match self.format {
            FileFormat::Envelope => {
                let revision = prev_revision + 1;
                (envelope_bytes(next, revision, self.pretty)?, Some(revision))
            }
            FileFormat::Flat => {
                let bytes = if self.pretty {
                    pretty_sorted(&Value::Object(next.values.clone()))?
                } else {
                    let existing = fs::read(&self.path).ok();
                    encode_flat(existing.as_deref(), &next.values)?
                };
                (bytes, None)
            }
        };
        if self.trailing_newline && !bytes.ends_with(b"\n") {
            bytes.push(b'\n');
        }
        let revision = envelope_revision.unwrap_or_else(|| fingerprint(&bytes));
        // Keep the last good file; best-effort. A damaged file is never copied over a good backup.
        if self.sidecars && matches!(read_file(&self.path, self.format), Ok(Doc::Stored(_))) {
            let _ = fs::copy(&self.path, bak_path(&self.path));
        }
        atomic_write_mode(&self.path, &bytes, &self.mode)?;
        Ok(revision)
    }
}

impl SettingsBackend for FileBackend {
    fn load(&self) -> Result<Option<Stored>, Error> {
        match read_file(&self.path, self.format)? {
            Doc::Stored(s) => Ok(Some(s)),
            Doc::Corrupt => self.refuse_if_strict().and_then(|()| self.backup()),
            Doc::Missing => self.backup(),
        }
    }

    fn stamp(&self) -> Result<Option<String>, Error> {
        match fs::metadata(&self.path) {
            Ok(meta) => Ok(Some(file_stamp(&meta))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Some("missing".into())),
            Err(e) => Err(e.into()),
        }
    }

    fn patch(
        &self,
        edit: &mut dyn FnMut(Option<&Stored>) -> Result<Edit, Error>,
    ) -> Result<Option<u64>, Error> {
        // Exclusion: the cross-process sidecar lock, or the in-process lock when sidecars are off.
        let _file_lock = if self.sidecars {
            Some(lock_file(&self.path)?)
        } else {
            None
        };
        let _process_lock = if self.sidecars {
            None
        } else {
            Some(
                PROCESS_WRITERS
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
            )
        };
        let mut corrupt_dest = false;
        let current = match read_file(&self.path, self.format)? {
            Doc::Stored(s) => Some(s),
            Doc::Missing => self.backup()?,
            Doc::Corrupt => {
                self.refuse_if_strict()?;
                match self.on_corrupt {
                    // Overwrite never moves the damaged file; the replace below decides.
                    OnCorrupt::Overwrite => corrupt_dest = true,
                    _ => quarantine(&self.path, &self.quarantine_name),
                }
                self.backup()?
            }
        };
        match edit(current.as_ref())? {
            Edit::Keep => Ok(None),
            Edit::Commit(next) => {
                if corrupt_dest && self.keep_corrupt_copy {
                    // A copy, made before the replace: the original stays until the replace wins.
                    fs::copy(
                        &self.path,
                        quarantine_path(&self.path, &self.quarantine_name),
                    )?;
                }
                let prev = current.as_ref().map_or(0, |s| s.revision);
                self.write(&next, prev).map(Some)
            }
        }
    }
}

impl FileBackend {
    /// The `.bak` sidecar's values, or `None`. Never read when sidecars are off.
    fn backup(&self) -> Result<Option<Stored>, Error> {
        if !self.sidecars {
            return Ok(None);
        }
        from_backup(&self.path, self.format)
    }

    fn refuse_if_strict(&self) -> Result<(), Error> {
        match self.on_corrupt {
            OnCorrupt::Quarantine => Ok(()),
            OnCorrupt::Refuse => Err(Error::Corrupt(self.path.display().to_string())),
            OnCorrupt::Overwrite => Ok(()),
        }
    }
}

enum Doc {
    Stored(Stored),
    Missing,
    Corrupt,
}

fn read_file(path: &Path, format: FileFormat) -> Result<Doc, Error> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Doc::Missing),
        Err(e) => return Err(e.into()),
    };
    let Ok(Value::Object(mut doc)) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(Doc::Corrupt);
    };
    match format {
        FileFormat::Flat => {
            if doc.contains_key(ENVELOPE_VERSION)
                && doc.get(ENVELOPE_VALUES).is_some_and(Value::is_object)
            {
                return Err(Error::Invalid(format!(
                    "{} is a versioned envelope; open it with FileFormat::Envelope",
                    path.display()
                )));
            }
            Ok(Doc::Stored(Stored {
                version: None,
                namespace: None,
                values: doc,
                revision: fingerprint(&bytes),
                needs_rewrite: false,
            }))
        }
        FileFormat::Envelope => {
            let Some(version) = doc.get(ENVELOPE_VERSION).and_then(Value::as_u64) else {
                // Legacy flat file: the whole document is version-0 values.
                return Ok(Doc::Stored(Stored {
                    version: Some(0),
                    namespace: None,
                    values: doc,
                    revision: 0,
                    needs_rewrite: false,
                }));
            };
            let revision = doc
                .get(ENVELOPE_REVISION)
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let namespace = doc
                .get(ENVELOPE_NS)
                .and_then(Value::as_str)
                .map(str::to_owned);
            match doc.remove(ENVELOPE_VALUES) {
                Some(Value::Object(values)) => Ok(Doc::Stored(Stored {
                    version: Some(u32::try_from(version).unwrap_or(u32::MAX)),
                    namespace,
                    values,
                    revision,
                    needs_rewrite: false,
                })),
                _ => Ok(Doc::Corrupt),
            }
        }
    }
}

/// Identity of a settings file for cache checks. Every commit replaces the file with a new inode
/// and a new mtime, so a changed file gives a different stamp.
#[cfg(unix)]
fn file_stamp(meta: &fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;
    format!(
        "{}:{}:{}.{}:{}.{}:{}",
        meta.dev(),
        meta.ino(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
        meta.len()
    )
}

#[cfg(not(unix))]
fn file_stamp(meta: &fs::Metadata) -> String {
    format!("{:?}:{}", meta.modified().ok(), meta.len())
}

/// The stored values of `path`, or of `<path>.bak` when the main file is missing or unreadable as
/// JSON. Nothing is written, locked, moved or created. `None` when neither is usable.
pub(crate) fn read_values_only(
    path: &Path,
    format: FileFormat,
    sidecars: bool,
) -> Result<Option<Map<String, Value>>, Error> {
    let candidates = if sidecars {
        vec![path.to_path_buf(), bak_path(path)]
    } else {
        vec![path.to_path_buf()]
    };
    for candidate in candidates {
        if let Doc::Stored(stored) = read_file(&candidate, format)? {
            return Ok(Some(stored.values));
        }
    }
    Ok(None)
}

fn from_backup(path: &Path, format: FileFormat) -> Result<Option<Stored>, Error> {
    match read_file(&bak_path(path), format)? {
        Doc::Stored(mut s) => {
            s.needs_rewrite = true;
            Ok(Some(s))
        }
        Doc::Missing | Doc::Corrupt => Ok(None),
    }
}

/// Pretty JSON with object keys sorted at every level and a trailing newline. Sorting is done
/// explicitly so the bytes do not depend on the map type's iteration order.
fn pretty_sorted(value: &Value) -> Result<Vec<u8>, Error> {
    let mut bytes = serde_json::to_vec_pretty(&sorted_value(value))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn sorted_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            Value::Object(
                keys.into_iter()
                    .map(|key| (key.clone(), sorted_value(&map[key])))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(sorted_value).collect()),
        other => other.clone(),
    }
}

fn envelope_bytes(next: &Stored, revision: u64, pretty: bool) -> Result<Vec<u8>, Error> {
    let version = next
        .version
        .ok_or(Error::BadSchema("envelope commit needs a schema version"))?;
    let namespace = next
        .namespace
        .clone()
        .ok_or(Error::BadSchema("envelope commit needs a namespace"))?;
    let mut doc = Map::new();
    doc.insert(ENVELOPE_VERSION.into(), version.into());
    doc.insert(ENVELOPE_NS.into(), namespace.into());
    doc.insert(ENVELOPE_VALUES.into(), Value::Object(next.values.clone()));
    doc.insert(ENVELOPE_REVISION.into(), revision.into());
    if pretty {
        return pretty_sorted(&Value::Object(doc));
    }
    Ok(serde_json::to_vec_pretty(&Value::Object(doc))?)
}

/// Rewrites a flat object, keeping every value whose content is unchanged byte for byte, the
/// original key order, and the file's indentation and trailing newline.
fn encode_flat(existing: Option<&[u8]>, values: &Map<String, Value>) -> Result<Vec<u8>, Error> {
    let style = Style::detect(existing.unwrap_or(&[]));
    let original: Vec<(String, Box<RawValue>)> = existing
        .and_then(|bytes| serde_json::from_slice::<RawEntries>(bytes).ok())
        .map(|entries| entries.0)
        .unwrap_or_default();

    let mut written: BTreeSet<&str> = BTreeSet::new();
    let mut out: Vec<(String, Entry<'_>)> = Vec::new();
    for (key, raw) in &original {
        if !written.insert(key.as_str()) {
            continue;
        }
        if let Some(value) = values.get(key) {
            let unchanged = serde_json::from_str::<Value>(raw.get()).is_ok_and(|old| &old == value);
            let entry = if unchanged {
                Entry::Raw(raw)
            } else {
                Entry::Val(value.clone())
            };
            out.push((key.clone(), entry));
        }
    }
    for (key, value) in values {
        if !written.contains(key.as_str()) {
            out.push((key.clone(), Entry::Val(value.clone())));
        }
    }

    let out = Out(out);
    let mut bytes = Vec::new();
    match &style.indent {
        None => serde_json::to_writer(&mut bytes, &out)?,
        Some(indent) => {
            let mut ser = serde_json::Serializer::with_formatter(
                &mut bytes,
                PrettyFormatter::with_indent(indent),
            );
            out.serialize(&mut ser)?;
        }
    }
    if style.trailing_newline {
        bytes.push(b'\n');
    }
    Ok(bytes)
}

/// Layout of an existing flat file, so a rewrite keeps it.
struct Style {
    /// `None` for compact output; otherwise the indent unit.
    indent: Option<Vec<u8>>,
    trailing_newline: bool,
}

impl Style {
    /// Empty files and files with no indented line get two spaces.
    fn detect(bytes: &[u8]) -> Self {
        let trailing_newline = bytes.ends_with(b"\n");
        if !bytes.contains(&b'\n') {
            return Self {
                indent: bytes.is_empty().then(|| b"  ".to_vec()),
                trailing_newline,
            };
        }
        let indent = bytes
            .split(|b| *b == b'\n')
            .skip(1)
            .find_map(|line| match line.first().copied() {
                Some(b'\t') => Some(b"\t".to_vec()),
                Some(b' ') => Some(vec![b' '; line.iter().take_while(|b| **b == b' ').count()]),
                _ => None,
            })
            .unwrap_or_else(|| b"  ".to_vec());
        Self {
            indent: Some(indent),
            trailing_newline,
        }
    }
}

/// Top-level entries of a JSON object, values kept as their original text.
struct RawEntries(Vec<(String, Box<RawValue>)>);

impl<'de> Deserialize<'de> for RawEntries {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;
        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = RawEntries;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<RawEntries, A::Error> {
                let mut out = Vec::new();
                while let Some((key, raw)) = map.next_entry::<String, Box<RawValue>>()? {
                    out.push((key, raw));
                }
                Ok(RawEntries(out))
            }
        }
        deserializer.deserialize_map(ObjectVisitor)
    }
}

/// One output value: original text when unchanged, otherwise a fresh value.
enum Entry<'a> {
    Raw(&'a RawValue),
    Val(Value),
}

impl Serialize for Entry<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Entry::Raw(raw) => raw.serialize(serializer),
            Entry::Val(value) => value.serialize(serializer),
        }
    }
}

struct Out<'a>(Vec<(String, Entry<'a>)>);

impl Serialize for Out<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, entry) in &self.0 {
            map.serialize_entry(key, entry)?;
        }
        map.end()
    }
}

/// FNV-1a, 64-bit. Stable across builds, unlike `DefaultHasher`. Used as the revision of a flat
/// file, which has no counter.
fn fingerprint(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn bak_path(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".bak");
    PathBuf::from(s)
}

fn quarantine_path(path: &Path, name: &QuarantineName) -> PathBuf {
    let suffix = match name {
        QuarantineName::Timestamped => {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            format!(".corrupt-{ts}")
        }
        QuarantineName::Fixed(s) if s.is_empty() => ".corrupt".to_string(),
        QuarantineName::Fixed(s) => s.replace(['/', '\\'], "_"),
    };
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

fn quarantine(path: &Path, name: &QuarantineName) {
    let _ = fs::rename(path, quarantine_path(path, name));
}

/// Exclusive cross-process transaction guard (OS advisory lock on a sidecar file; released when
/// dropped or when the process dies).
struct FileGuard(File);

impl Drop for FileGuard {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn lock_path(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".lock");
    PathBuf::from(s)
}

fn lock_file(settings: &Path) -> Result<FileGuard, Error> {
    let path = lock_path(settings);
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        // Durable, like the settings file written below it: a plain `create_dir_all` leaves the
        // new directories' links unsynced.
        rightkit_fs::atomic::create_dir_all_durable(parent, false)
            .map_err(|e| Error::Io(std::io::Error::from(e)))?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;
    let start = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(FileGuard(file)),
            Err(TryLockError::WouldBlock) => {
                if start.elapsed() >= LOCK_WAIT {
                    return Err(Error::LockTimeout(path.display().to_string()));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(TryLockError::Error(e)) => return Err(e.into()),
        }
    }
}
