//! Storage behind [`crate::Store`]: the [`SettingsBackend`] trait and the file-backed default.

use crate::atomic::{atomic_write_mode, WriteMode};
use crate::options::{
    FileFormat, KeyOrder, OnCorrupt, QuarantineName, ReplaceMode, SharingRetry, StoreOptions,
};
use crate::store::{Error, PreserveStep, Refusal};
use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeMap, SerializeSeq};
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

/// The edit [`SettingsBackend::patch_ordered`] runs, returning the layout of what it commits.
type OrderedEdit<'a> = dyn FnMut(Option<&Stored>) -> Result<(Edit, KeyLayout), Error> + 'a;

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
/// ```text
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

    /// [`SettingsBackend::patch`], where the edit also returns the [`KeyLayout`] of the document
    /// it commits. [`crate::Store`] calls this one. A backend that writes ordered text (the
    /// default [`FileBackend`] does) uses the layout to place keys the stored document does not
    /// hold yet; the default implementation drops it and calls `patch`, which is right for any
    /// backend that does not keep key order, so existing implementations need no change.
    fn patch_ordered(&self, edit: &mut OrderedEdit<'_>) -> Result<Option<u64>, Error> {
        self.patch(&mut |current| edit(current).map(|(decision, _)| decision))
    }

    /// Whether the stored document is over a size limit this backend enforces, without reading it.
    /// [`crate::Store`] asks before `load` so an oversized document can be refused read-only (see
    /// [`crate::StoreOptions::on_refused`]). The default: no limit.
    fn size_refusal(&self) -> Result<Option<Refusal>, Error> {
        Ok(None)
    }

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

/// The key order of the typed settings value a commit was made from. A backend that writes
/// ordered text uses it to place keys the stored document does not hold yet, in the order the
/// settings type serialises them; a backend that does not care ignores it. It carries no values
/// and cannot be built from outside the crate: `KeyLayout::default()` says "no information", and
/// then new keys follow the document's own map order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct KeyLayout(Option<Shape>);

impl KeyLayout {
    /// The order `value` serialises its keys in, at every nesting level. Empty when it cannot be
    /// serialised: the layout is only a hint, and a missing one changes where new keys go, not
    /// what is written (the store serialises the same value to build the document, so a real
    /// failure surfaces there).
    pub(crate) fn of<T: Serialize>(value: &T) -> Self {
        Self(
            serde_json::to_vec(value)
                .ok()
                .and_then(|bytes| Shape::parse(&bytes)),
        )
    }

    fn root(&self) -> Option<&Shape> {
        self.0.as_ref()
    }
}

/// The default backend: one JSON file, written atomically under a cross-process lock. The previous
/// good file is kept as `<path>.bak`; if that copy fails, the write stops before it changes the
/// file ([`Error::PreserveFailed`]).
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
    key_order: KeyOrder,
    max_bytes: Option<u64>,
    utf8_bom: bool,
}

/// The UTF-8 byte order mark.
const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

/// `bytes` without a leading UTF-8 BOM when `bom` is set; unchanged otherwise.
fn strip_bom(bytes: &[u8], bom: bool) -> &[u8] {
    match bytes.strip_prefix(UTF8_BOM) {
        Some(rest) if bom => rest,
        _ => bytes,
    }
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
            key_order: KeyOrder::Preserve,
            max_bytes: None,
            utf8_bom: false,
        }
    }

    /// Accept a leading UTF-8 byte order mark on read and write one on every write. See
    /// [`StoreOptions::utf8_bom`].
    pub fn utf8_bom(mut self, yes: bool) -> Self {
        self.utf8_bom = yes;
        self
    }

    /// Refuse to read the file when it is larger than `limit` bytes. See
    /// [`StoreOptions::max_bytes`].
    pub fn max_bytes(mut self, limit: u64) -> Self {
        self.max_bytes = Some(limit);
        self
    }

    fn check_size(&self) -> Result<(), Error> {
        let Some(limit) = self.max_bytes else {
            return Ok(());
        };
        match fs::metadata(&self.path) {
            Ok(meta) if meta.len() > limit => Err(Error::Invalid(format!(
                "{} is {} bytes, over the {limit} byte limit",
                self.path.display(),
                meta.len()
            ))),
            _ => Ok(()),
        }
    }

    /// The order of object keys in what a write produces. Default [`KeyOrder::Preserve`].
    pub fn key_order(mut self, order: KeyOrder) -> Self {
        self.key_order = order;
        self
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

    /// Who may read the written file. See [`StoreOptions::write_security`].
    pub fn write_security(mut self, profile: crate::options::WriteSecurity) -> Self {
        self.mode.security = profile;
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
    /// sidecars, replace mode, naming, newline, key order).
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
                security: options.write_security,
            },
            keep_corrupt_copy: options.keep_corrupt_copy,
            quarantine_name: options.quarantine_name.clone(),
            trailing_newline: options.trailing_newline,
            key_order: options.key_order,
            max_bytes: options.max_bytes,
            utf8_bom: options.utf8_bom,
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

    /// Writes `next` as the committed document. `reference` is the stored text the document was
    /// read from (the file, or its `.bak` when the file was missing or damaged): it supplies the
    /// key order to keep and, for a flat file, the bytes of unchanged values and the layout.
    /// `main_good` says the file at the path is a good document, which `<path>.bak` is refreshed
    /// from.
    fn write(
        &self,
        next: &Stored,
        prev_revision: u64,
        layout: &KeyLayout,
        reference: Option<&[u8]>,
        main_good: bool,
    ) -> Result<u64, Error> {
        // `Sorted` writes what it always did: only a good file at the path informs a rewrite. A
        // `.bak` stands in for the key order and layout under `Preserve` alone.
        let reference = if main_good || self.key_order == KeyOrder::Preserve {
            reference
        } else {
            None
        };
        let (mut bytes, envelope_revision) = match self.format {
            FileFormat::Envelope => {
                let revision = prev_revision + 1;
                let bytes = envelope_bytes(
                    next,
                    revision,
                    self.pretty,
                    self.key_order,
                    reference,
                    layout,
                )?;
                (bytes, Some(revision))
            }
            FileFormat::Flat => {
                let bytes = if self.pretty {
                    pretty_sorted(&Value::Object(next.values.clone()))?
                } else {
                    encode_flat(reference, &next.values, self.key_order, layout)?
                };
                (bytes, None)
            }
        };
        if self.trailing_newline && !bytes.ends_with(b"\n") {
            bytes.push(b'\n');
        }
        let revision = envelope_revision.unwrap_or_else(|| fingerprint(&bytes));
        if self.utf8_bom {
            let mut framed = Vec::with_capacity(UTF8_BOM.len() + bytes.len());
            framed.extend_from_slice(UTF8_BOM);
            framed.extend_from_slice(&bytes);
            bytes = framed;
        }
        // Keep the last good file. A damaged file is never copied over a good backup. A failed copy
        // stops the write: replacing the file now would destroy the only copy of its content, and
        // leave an older `.bak` to be restored from later. The backup is staged in a sibling temp
        // file and renamed over `.bak`, so an interrupted copy cannot truncate the good backup.
        if self.sidecars && main_good {
            preserve_backup(&self.path)?;
        }
        atomic_write_mode(&self.path, &bytes, &self.mode)?;
        Ok(revision)
    }

    /// The transaction behind both `patch` and `patch_ordered`.
    fn commit(&self, edit: &mut OrderedEdit<'_>) -> Result<Option<u64>, Error> {
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
        // The stored text the edit's document came from, for the key order to keep.
        let mut reference: Option<Vec<u8>> = None;
        let mut main_good = false;
        self.check_size()?;
        let main = read_bytes_capped(&self.path, self.max_bytes)?;
        let doc = match &main {
            Some(bytes) => parse_doc(&self.path, self.format, bytes, self.utf8_bom)?,
            None => Doc::Missing,
        };
        let current = match doc {
            Doc::Stored(s) => {
                main_good = true;
                reference = main.map(|bytes| strip_bom(&bytes, self.utf8_bom).to_vec());
                Some(s)
            }
            Doc::Missing => self.restore(&mut reference)?,
            Doc::Corrupt => {
                self.refuse_if_strict()?;
                match self.on_corrupt {
                    // Overwrite never moves the damaged file; the replace below decides.
                    OnCorrupt::Overwrite => corrupt_dest = true,
                    // A damaged file that cannot be set aside stops the operation here, before
                    // anything is written over it.
                    _ => quarantine(&self.path, &self.quarantine_name)?,
                }
                self.restore(&mut reference)?
            }
        };
        match edit(current.as_ref())? {
            (Edit::Keep, _) => Ok(None),
            (Edit::Commit(next), layout) => {
                if corrupt_dest && self.keep_corrupt_copy {
                    // A copy, made before the replace: the original stays until the replace wins.
                    corrupt_copy(&self.path, &self.quarantine_name)?;
                }
                let prev = current.as_ref().map_or(0, |s| s.revision);
                self.write(&next, prev, &layout, reference.as_deref(), main_good)
                    .map(Some)
            }
        }
    }
}

impl SettingsBackend for FileBackend {
    fn size_refusal(&self) -> Result<Option<Refusal>, Error> {
        let Some(limit) = self.max_bytes else {
            return Ok(None);
        };
        match fs::metadata(&self.path) {
            Ok(meta) if meta.len() > limit => Ok(Some(Refusal::Oversized {
                len: meta.len(),
                limit,
            })),
            _ => Ok(None),
        }
    }

    fn load(&self) -> Result<Option<Stored>, Error> {
        self.check_size()?;
        match read_file(&self.path, self.format, self.utf8_bom, self.max_bytes)? {
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
        self.commit(&mut |current| edit(current).map(|decision| (decision, KeyLayout::default())))
    }

    fn patch_ordered(&self, edit: &mut OrderedEdit<'_>) -> Result<Option<u64>, Error> {
        self.commit(edit)
    }
}

impl FileBackend {
    /// The `.bak` sidecar's values, or `None`. Never read when sidecars are off.
    fn backup(&self) -> Result<Option<Stored>, Error> {
        Ok(self.backup_with_text()?.map(|(stored, _)| stored))
    }

    /// As [`FileBackend::backup`], with the text the values were read from.
    fn backup_with_text(&self) -> Result<Option<(Stored, Vec<u8>)>, Error> {
        if !self.sidecars {
            return Ok(None);
        }
        from_backup(&self.path, self.format, self.utf8_bom, self.max_bytes)
    }

    /// The `.bak` sidecar's values; its text becomes the reference for key order.
    fn restore(&self, reference: &mut Option<Vec<u8>>) -> Result<Option<Stored>, Error> {
        Ok(self.backup_with_text()?.map(|(stored, text)| {
            *reference = Some(text);
            stored
        }))
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

/// The bytes of `path`, or `None` when it does not exist. Any other error is an error: a file
/// that cannot be read is never treated as missing.
fn read_bytes(path: &Path) -> Result<Option<Vec<u8>>, Error> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// The bytes of `path` like [`read_bytes`], but never more than `limit` bytes. The file is opened
/// once and at most `limit + 1` bytes are read, so a file that grows or is replaced after a size
/// check cannot make the read allocate past the cap. More than `limit` bytes is refused before
/// anything is parsed. `None` as the limit reads the whole file.
fn read_bytes_capped(path: &Path, limit: Option<u64>) -> Result<Option<Vec<u8>>, Error> {
    use std::io::Read;
    let Some(limit) = limit else {
        return read_bytes(path);
    };
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(Error::Invalid(format!(
            "{} is over the {limit} byte limit",
            path.display()
        )));
    }
    Ok(Some(bytes))
}

fn read_file(path: &Path, format: FileFormat, bom: bool, limit: Option<u64>) -> Result<Doc, Error> {
    match read_bytes_capped(path, limit)? {
        Some(bytes) => parse_doc(path, format, &bytes, bom),
        None => Ok(Doc::Missing),
    }
}

/// `bytes`, read from `path`, as a document of `format`. A leading BOM is removed first when `bom`.
fn parse_doc(path: &Path, format: FileFormat, bytes: &[u8], bom: bool) -> Result<Doc, Error> {
    let bytes = strip_bom(bytes, bom);
    let Ok(Value::Object(mut doc)) = serde_json::from_slice::<Value>(bytes) else {
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
                revision: fingerprint(bytes),
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
    bom: bool,
    max_bytes: Option<u64>,
) -> Result<Option<Map<String, Value>>, Error> {
    let candidates = if sidecars {
        vec![path.to_path_buf(), bak_path(path)]
    } else {
        vec![path.to_path_buf()]
    };
    for candidate in candidates {
        // The length is checked before the file is read; an oversized file is refused, not parsed.
        let oversized = max_bytes.and_then(|limit| {
            fs::metadata(&candidate)
                .ok()
                .filter(|meta| meta.len() > limit)
                .map(|meta| (limit, meta.len()))
        });
        if let Some((limit, len)) = oversized {
            return Err(Error::Invalid(format!(
                "{} is {len} bytes, over the {limit} byte limit",
                candidate.display()
            )));
        }
        if let Doc::Stored(stored) = read_file(&candidate, format, bom, max_bytes)? {
            return Ok(Some(stored.values));
        }
    }
    Ok(None)
}

/// The `.bak` document of `path` and the text it was read from.
fn from_backup(
    path: &Path,
    format: FileFormat,
    bom: bool,
    limit: Option<u64>,
) -> Result<Option<(Stored, Vec<u8>)>, Error> {
    let bak = bak_path(path);
    let Some(text) = read_bytes_capped(&bak, limit)? else {
        return Ok(None);
    };
    match parse_doc(&bak, format, &text, bom)? {
        Doc::Stored(mut s) => {
            s.needs_rewrite = true;
            Ok(Some((s, strip_bom(&text, bom).to_vec())))
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

/// The envelope document for `next`. Keys are written as `order` says; see [`KeyOrder`]. `reference`
/// is the stored text the document came from, whose key order `Preserve` keeps.
fn envelope_bytes(
    next: &Stored,
    revision: u64,
    pretty: bool,
    order: KeyOrder,
    reference: Option<&[u8]>,
    layout: &KeyLayout,
) -> Result<Vec<u8>, Error> {
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
    // The same bytes `serde_json::to_vec_pretty` gives for a value with these keys in this order:
    // `Ordered` serialises leaves, arrays and objects exactly as `Value` does.
    let root = Value::Object(doc);
    let mut bytes = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut bytes, PrettyFormatter::new());
    match order {
        KeyOrder::Sorted => Ordered::sorted(&root).serialize(&mut ser)?,
        KeyOrder::Preserve => {
            let old = reference.and_then(Shape::parse).map(Shape::into_envelope);
            let typed = Shape::envelope(layout.root());
            Ordered {
                value: &root,
                old: old.as_ref(),
                typed: Some(&typed),
                sorted: false,
            }
            .serialize(&mut ser)?;
        }
    }
    Ok(bytes)
}

/// Rewrites a flat object, keeping every value whose content is unchanged byte for byte, the
/// original key order, and the file's indentation and trailing newline. A value that changed is
/// written with its keys in the order `order` asks for, and so are the keys the file did not
/// hold: appended after the held ones, in the settings type's field order (`Preserve`) or
/// alphabetically (`Sorted`).
fn encode_flat(
    existing: Option<&[u8]>,
    values: &Map<String, Value>,
    order: KeyOrder,
    layout: &KeyLayout,
) -> Result<Vec<u8>, Error> {
    let style = Style::detect(existing.unwrap_or(&[]));
    let original: Vec<(String, Box<RawValue>)> = existing
        .and_then(|bytes| serde_json::from_slice::<RawEntries>(bytes).ok())
        .map(|entries| entries.0)
        .unwrap_or_default();
    let old = existing.and_then(Shape::parse);
    let typed = layout.root();
    let sorted = order == KeyOrder::Sorted;

    let mut written: BTreeSet<&str> = BTreeSet::new();
    let mut out: Vec<(&str, Entry<'_>)> = Vec::new();
    for (key, raw) in &original {
        if !written.insert(key.as_str()) {
            continue;
        }
        if let Some(value) = values.get(key) {
            let unchanged =
                serde_json::from_str::<Value>(raw.get()).is_ok_and(|previous| &previous == value);
            let entry = if unchanged {
                Entry::Raw(raw)
            } else {
                rewritten(key, value, old.as_ref(), typed, sorted)
            };
            out.push((key.as_str(), entry));
        }
    }
    let fresh: Vec<(&str, &Value)> = if sorted {
        let mut fresh: Vec<(&str, &Value)> = values
            .iter()
            .filter(|(key, _)| !written.contains(key.as_str()))
            .map(|(key, value)| (key.as_str(), value))
            .collect();
        fresh.sort_unstable_by_key(|(key, _)| *key);
        fresh
    } else {
        preserve_order(values, None, typed.and_then(Shape::entries), &written)
    };
    for (key, value) in fresh {
        out.push((key, rewritten(key, value, old.as_ref(), typed, sorted)));
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

/// A top-level value of a flat file that is written afresh: its keys follow `old` (the file's
/// own order) and `typed` (the settings type's), or are sorted.
fn rewritten<'a>(
    key: &str,
    value: &'a Value,
    old: Option<&'a Shape>,
    typed: Option<&'a Shape>,
    sorted: bool,
) -> Entry<'a> {
    if sorted {
        return Entry::Val(Ordered::sorted(value));
    }
    Entry::Val(Ordered {
        value,
        old: old.and_then(|shape| shape.child(key)),
        typed: typed.and_then(|shape| shape.child(key)),
        sorted: false,
    })
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
    Val(Ordered<'a>),
}

impl Serialize for Entry<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Entry::Raw(raw) => raw.serialize(serializer),
            Entry::Val(value) => value.serialize(serializer),
        }
    }
}

struct Out<'a>(Vec<(&'a str, Entry<'a>)>);

impl Serialize for Out<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, entry) in &self.0 {
            map.serialize_entry(key, entry)?;
        }
        map.end()
    }
}

/// The key order of a JSON document, at every nesting level, with the values left out. Parsed in
/// document order by a visitor, because `serde_json::Map` is a sorted map without the
/// `preserve_order` feature, and that feature is not enabled here: it would change `Map` for
/// every crate in the build.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Shape {
    /// An object's keys in document order, each with its value's shape. A key that occurs twice
    /// in the text occurs twice here; the last one is the one `serde_json` keeps.
    Object(Vec<(String, Shape)>),
    Array(Vec<Shape>),
    /// A string, number, boolean or null.
    Leaf,
}

impl Shape {
    /// The shape of the JSON text `bytes`, or `None` when it is not JSON. Only a missing hint:
    /// callers use the shape to order keys, never to decide what is written.
    fn parse(bytes: &[u8]) -> Option<Shape> {
        serde_json::from_slice(bytes).ok()
    }

    /// The envelope's own keys in the order a new file gets them, with `values` as given.
    fn envelope(values: Option<&Shape>) -> Shape {
        Shape::Object(vec![
            (ENVELOPE_VERSION.to_owned(), Shape::Leaf),
            (ENVELOPE_NS.to_owned(), Shape::Leaf),
            (
                ENVELOPE_VALUES.to_owned(),
                values.cloned().unwrap_or(Shape::Leaf),
            ),
            (ENVELOPE_REVISION.to_owned(), Shape::Leaf),
        ])
    }

    /// This shape read as an envelope. A legacy flat file (no `schemaVersion`) is the `values`
    /// of the envelope that adopts it: its keys keep their order there, and the envelope's own
    /// keys, which it lacks, take the order a new file gets.
    fn into_envelope(self) -> Shape {
        let flat = matches!(
            &self,
            Shape::Object(entries) if !entries.iter().any(|(key, _)| key == ENVELOPE_VERSION)
        );
        if flat {
            Shape::envelope(Some(&self))
        } else {
            self
        }
    }

    fn entries(&self) -> Option<&[(String, Shape)]> {
        match self {
            Shape::Object(entries) => Some(entries.as_slice()),
            _ => None,
        }
    }

    fn child(&self, key: &str) -> Option<&Shape> {
        match self {
            Shape::Object(entries) => entries
                .iter()
                .rev()
                .find(|(name, _)| name == key)
                .map(|(_, shape)| shape),
            _ => None,
        }
    }

    fn element(&self, index: usize) -> Option<&Shape> {
        match self {
            Shape::Array(items) => items.get(index),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for Shape {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ShapeVisitor;
        impl<'de> Visitor<'de> for ShapeVisitor {
            type Value = Shape;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Shape, A::Error> {
                let mut entries = Vec::new();
                while let Some((key, shape)) = map.next_entry::<String, Shape>()? {
                    entries.push((key, shape));
                }
                Ok(Shape::Object(entries))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Shape, A::Error> {
                let mut items = Vec::new();
                while let Some(shape) = seq.next_element::<Shape>()? {
                    items.push(shape);
                }
                Ok(Shape::Array(items))
            }
            fn visit_bool<E>(self, _: bool) -> Result<Shape, E> {
                Ok(Shape::Leaf)
            }
            fn visit_i64<E>(self, _: i64) -> Result<Shape, E> {
                Ok(Shape::Leaf)
            }
            fn visit_u64<E>(self, _: u64) -> Result<Shape, E> {
                Ok(Shape::Leaf)
            }
            fn visit_f64<E>(self, _: f64) -> Result<Shape, E> {
                Ok(Shape::Leaf)
            }
            fn visit_str<E>(self, _: &str) -> Result<Shape, E> {
                Ok(Shape::Leaf)
            }
            fn visit_unit<E>(self) -> Result<Shape, E> {
                Ok(Shape::Leaf)
            }
        }
        deserializer.deserialize_any(ShapeVisitor)
    }
}

/// A [`Value`] written with its object keys in a chosen order. Leaves are serialised by `Value`
/// itself, so numbers, escapes and whitespace are what `serde_json` gives for the same value; only
/// the order of keys differs. `old` is the shape of the stored text this value replaces (its keys
/// stay where they were) and `typed` the shape of the settings type's serialisation (keys the old
/// text lacks follow it); keys in neither follow the map's own order. `sorted` ignores both and
/// sorts every object, explicitly, so the bytes do not depend on the map type.
#[derive(Clone, Copy)]
struct Ordered<'a> {
    value: &'a Value,
    old: Option<&'a Shape>,
    typed: Option<&'a Shape>,
    sorted: bool,
}

impl<'a> Ordered<'a> {
    fn sorted(value: &'a Value) -> Self {
        Self {
            value,
            old: None,
            typed: None,
            sorted: true,
        }
    }
}

impl Serialize for Ordered<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.value {
            Value::Object(map) => {
                let entries: Vec<(&str, &Value)> = if self.sorted {
                    let mut all: Vec<(&str, &Value)> = map
                        .iter()
                        .map(|(key, value)| (key.as_str(), value))
                        .collect();
                    all.sort_unstable_by_key(|(key, _)| *key);
                    all
                } else {
                    preserve_order(
                        map,
                        self.old.and_then(Shape::entries),
                        self.typed.and_then(Shape::entries),
                        &BTreeSet::new(),
                    )
                };
                let mut out = serializer.serialize_map(Some(entries.len()))?;
                for (key, value) in entries {
                    let child = Ordered {
                        value,
                        old: self.old.and_then(|shape| shape.child(key)),
                        typed: self.typed.and_then(|shape| shape.child(key)),
                        sorted: self.sorted,
                    };
                    out.serialize_entry(key, &child)?;
                }
                out.end()
            }
            Value::Array(items) => {
                let mut seq = serializer.serialize_seq(Some(items.len()))?;
                for (index, item) in items.iter().enumerate() {
                    let element = Ordered {
                        value: item,
                        old: self.old.and_then(|shape| shape.element(index)),
                        typed: self.typed.and_then(|shape| shape.element(index)),
                        sorted: self.sorted,
                    };
                    seq.serialize_element(&element)?;
                }
                seq.end()
            }
            leaf => leaf.serialize(serializer),
        }
    }
}

/// The keys of `map` in write order: those `old` names, in its order, then those `typed` names,
/// then the rest in the map's own order. Keys in `skip` are left out (the caller wrote them).
fn preserve_order<'a>(
    map: &'a Map<String, Value>,
    old: Option<&'a [(String, Shape)]>,
    typed: Option<&'a [(String, Shape)]>,
    skip: &BTreeSet<&str>,
) -> Vec<(&'a str, &'a Value)> {
    let mut placed: BTreeSet<&str> = skip.iter().copied().collect();
    let mut out = Vec::with_capacity(map.len());
    for source in [old, typed].into_iter().flatten() {
        for (key, _) in source {
            if let Some(value) = map.get(key.as_str()) {
                if placed.insert(key.as_str()) {
                    out.push((key.as_str(), value));
                }
            }
        }
    }
    for (key, value) in map {
        if placed.insert(key.as_str()) {
            out.push((key.as_str(), value));
        }
    }
    out
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

fn quarantine_candidate(base: &Path, attempt: u32) -> PathBuf {
    if attempt == 0 {
        return base.to_path_buf();
    }
    let mut s = base.as_os_str().to_owned();
    s.push(format!("-{attempt}"));
    PathBuf::from(s)
}

/// Moves the damaged file at `path` aside. A failed move is an error naming the file, where it was
/// to go and the cause: the caller must stop, because anything written now would go over a file
/// that was not preserved. A timestamped name has one-second resolution, so two quarantines in the
/// same second would collide: each retries with a numbered suffix and never replaces evidence an
/// earlier quarantine kept. A fixed name keeps its documented replace-the-previous behaviour.
fn quarantine(path: &Path, name: &QuarantineName) -> Result<(), Error> {
    if !matches!(name, QuarantineName::Timestamped) {
        return preserve(PreserveStep::Quarantine, path, &quarantine_path(path, name));
    }
    let base = quarantine_path(path, name);
    for attempt in 0.. {
        let kept_as = quarantine_candidate(&base, attempt);
        if fs::symlink_metadata(&kept_as).is_ok() {
            continue;
        }
        match rename_file(path, &kept_as) {
            Ok(()) => return Ok(()),
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(Error::PreserveFailed {
                    step: PreserveStep::Quarantine,
                    path: path.to_path_buf(),
                    kept_as,
                    source,
                })
            }
        }
    }
    unreachable!()
}

/// Copies the damaged file at `path` aside (the original stays until a later replace). Naming and
/// collision behaviour match [`quarantine`].
fn corrupt_copy(path: &Path, name: &QuarantineName) -> Result<(), Error> {
    if !matches!(name, QuarantineName::Timestamped) {
        return preserve(
            PreserveStep::CorruptCopy,
            path,
            &quarantine_path(path, name),
        );
    }
    let base = quarantine_path(path, name);
    for attempt in 0.. {
        let kept_as = quarantine_candidate(&base, attempt);
        match copy_file_new(path, &kept_as) {
            Ok(()) => return Ok(()),
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(Error::PreserveFailed {
                    step: PreserveStep::CorruptCopy,
                    path: path.to_path_buf(),
                    kept_as,
                    source,
                })
            }
        }
    }
    unreachable!()
}

/// Refreshes `<path>.bak` from `path` through a unique sibling temp file renamed over the backup,
/// so a copy that fails or is interrupted leaves the previous backup byte for byte intact.
fn preserve_backup(path: &Path) -> Result<(), Error> {
    let kept_as = bak_path(path);
    backup_atomic(path, &kept_as).map_err(|source| Error::PreserveFailed {
        step: PreserveStep::Backup,
        path: path.to_path_buf(),
        kept_as,
        source,
    })
}

// Private at exclusive creation, before any settings byte is written. Avoid a pathname
// chmod after creation, which could narrow a substituted file instead of our open handle.
fn create_preservation_file(path: &Path) -> io::Result<File> {
    rightkit_fs::create_private_new(path).map_err(io::Error::from)
}

fn backup_atomic(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(test)]
    if let Some(error) = fault::copy() {
        return Err(error);
    }
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let bytes = fs::read(from)?;
    let parent = to.parent().filter(|p| !p.as_os_str().is_empty());
    let name = to
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "settings.bak".into());
    let (tmp, mut file) = loop {
        let file_name = format!(
            ".{name}.{}.{}.tmp",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        );
        let cand = match parent {
            Some(parent) => parent.join(file_name),
            None => PathBuf::from(file_name),
        };
        match create_preservation_file(&cand) {
            Ok(f) => break (cand, f),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    let result = (|| {
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, to)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// One step that keeps the content at `path` before something replaces or removes it: moves it to
/// `kept_as` ([`PreserveStep::Quarantine`]) or copies it there. A failure is returned, never
/// dropped. Every copy, under a fixed name or a timestamped one, is staged owner-only before any
/// byte is written and renamed over `kept_as` (`backup_atomic`), so a replaced destination is
/// never left with the wider permissions it had or that the parent directory would give a plain
/// `fs::copy`. A move keeps the permissions of the file it moves.
fn preserve(step: PreserveStep, path: &Path, kept_as: &Path) -> Result<(), Error> {
    let done = match step {
        PreserveStep::Quarantine => rename_file(path, kept_as),
        PreserveStep::CorruptCopy | PreserveStep::Backup => backup_atomic(path, kept_as),
    };
    done.map_err(|source| Error::PreserveFailed {
        step,
        path: path.to_path_buf(),
        kept_as: kept_as.to_path_buf(),
        source,
    })
}

// `rename_file`, `backup_atomic` and `copy_file_new` are the only places the preservation steps
// touch the file system, so a unit test can make them fail (`fault`), which no public seam of this
// crate or of rightkit-fs reaches.

fn rename_file(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(test)]
    if let Some(error) = fault::rename() {
        return Err(error);
    }
    fs::rename(from, to)
}

/// A copy, like [`backup_atomic`]'s, that refuses to replace: the destination is claimed with `create_new`, so an
/// existing file yields `AlreadyExists` and the caller retries with the next candidate. A copy
/// that fails after claiming removes its own partial file.
fn copy_file_new(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(test)]
    if let Some(error) = fault::copy() {
        return Err(error);
    }
    let mut src = File::open(from)?;
    let mut dst = create_preservation_file(to)?;
    let result = io::copy(&mut src, &mut dst).and_then(|_| dst.sync_all());
    if result.is_err() {
        drop(dst);
        let _ = fs::remove_file(to);
    }
    result.map(|_| ())
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

/// Failure injection for the unit tests below: `preserve` renames or copies through two calls
/// that consult this module first. Per thread, so parallel tests do not see each other's faults.
#[cfg(test)]
mod fault {
    use std::cell::Cell;
    use std::io;

    thread_local! {
        static RENAME: Cell<Option<io::ErrorKind>> = const { Cell::new(None) };
        static COPY: Cell<Option<io::ErrorKind>> = const { Cell::new(None) };
    }

    /// Clears the fault when dropped.
    pub(super) struct Guard(fn());

    impl Drop for Guard {
        fn drop(&mut self) {
            (self.0)();
        }
    }

    /// Makes every rename `preserve` makes on this thread fail with `kind`.
    pub(super) fn fail_rename(kind: io::ErrorKind) -> Guard {
        RENAME.with(|fault| fault.set(Some(kind)));
        Guard(|| RENAME.with(|fault| fault.set(None)))
    }

    /// Makes every copy `preserve` makes on this thread fail with `kind`.
    pub(super) fn fail_copy(kind: io::ErrorKind) -> Guard {
        COPY.with(|fault| fault.set(Some(kind)));
        Guard(|| COPY.with(|fault| fault.set(None)))
    }

    pub(super) fn rename() -> Option<io::Error> {
        RENAME
            .with(Cell::get)
            .map(|kind| io::Error::new(kind, "injected rename failure"))
    }

    pub(super) fn copy() -> Option<io::Error> {
        COPY.with(Cell::get)
            .map(|kind| io::Error::new(kind, "injected copy failure"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Schema, Store};

    #[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
    #[serde(default)]
    struct Cfg {
        n: u32,
    }

    fn schema() -> Schema {
        Schema::new("t", 1)
    }

    #[cfg(unix)]
    #[test]
    fn preservation_copies_are_owner_only_under_a_permissive_parent() {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let source = dir.path().join("settings.json");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&source)
            .unwrap();
        file.write_all(b"private settings").unwrap();
        drop(file);
        let staged = dir.path().join(".settings.json.bak.test.tmp");
        let empty_stage = create_preservation_file(&staged).unwrap();
        assert_eq!(
            empty_stage.metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&staged).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(empty_stage.metadata().unwrap().len(), 0);
        drop(empty_stage);
        fs::remove_file(staged).unwrap();
        let backup = dir.path().join("settings.json.bak");
        backup_atomic(&source, &backup).unwrap();
        let corrupt = dir.path().join("settings.json.corrupt");
        copy_file_new(&source, &corrupt).unwrap();
        for path in [&source, &backup, &corrupt] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(fs::read(path).unwrap(), b"private settings");
        }
        assert!(fs::read_dir(dir.path()).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")));
    }

    #[test]
    fn a_bounded_read_accepts_exactly_the_cap_and_refuses_one_more_byte() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.json");
        fs::write(&p, b"0123456789").unwrap();
        assert_eq!(
            read_bytes_capped(&p, Some(10)).unwrap().unwrap(),
            b"0123456789"
        );
        let over = read_bytes_capped(&p, Some(9));
        assert!(matches!(over, Err(Error::Invalid(m)) if m.contains("byte limit")));
        assert_eq!(read_bytes_capped(&p, None).unwrap().unwrap().len(), 10);
        let missing = dir.path().join("missing.json");
        assert!(read_bytes_capped(&missing, Some(9)).unwrap().is_none());
    }

    #[test]
    fn the_backup_read_is_bounded_by_the_cap_like_the_main_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.json");
        // `{"n":7}` is exactly 7 bytes; the main file is missing, so the backup is read.
        fs::write(bak_path(&p), br#"{"n":7}"#).unwrap();
        let at_cap = FileBackend::new(&p, FileFormat::Flat).max_bytes(7);
        let stored = at_cap.load().unwrap().unwrap();
        assert_eq!(stored.values.get("n").and_then(Value::as_u64), Some(7));
        let under = FileBackend::new(&p, FileFormat::Flat).max_bytes(6);
        assert!(under.load().is_err());
        let values = read_values_only(&p, FileFormat::Flat, true, false, Some(6));
        assert!(values.is_err());
        let values = read_values_only(&p, FileFormat::Flat, true, false, Some(7)).unwrap();
        assert_eq!(values.unwrap().get("n").and_then(Value::as_u64), Some(7));
    }

    /// File names in `dir` other than the lock sidecar.
    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| !name.ends_with(".lock"))
            .collect();
        names.sort();
        names
    }

    fn bump(store: &Store<Cfg>, n: u32) -> Result<(), Error> {
        store.update(|cfg| {
            cfg.n = n;
            Ok(())
        })
    }

    #[test]
    fn a_failed_quarantine_stops_the_open_and_leaves_the_damaged_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.json");
        fs::write(&p, b"{ damaged").unwrap();
        let _fault = fault::fail_rename(io::ErrorKind::PermissionDenied);
        let err = Store::<Cfg>::open_path(&p, StoreOptions::new(schema()))
            .err()
            .expect("open must fail");
        match &err {
            Error::PreserveFailed {
                step,
                path,
                kept_as,
                source,
            } => {
                assert_eq!(*step, PreserveStep::Quarantine);
                assert_eq!(path, &p);
                assert!(kept_as.to_string_lossy().contains("s.json.corrupt-"));
                assert_eq!(source.kind(), io::ErrorKind::PermissionDenied);
            }
            other => panic!("expected PreserveFailed, got {other:?}"),
        }
        let text = err.to_string();
        assert!(
            text.contains("s.json")
                && text.contains("moved aside")
                && text.contains("nothing was written"),
            "{text}"
        );
        assert_eq!(fs::read(&p).unwrap(), b"{ damaged");
        assert_eq!(
            names(d.path()),
            vec!["s.json"],
            "nothing moved, written or left over"
        );
    }

    #[test]
    fn a_failed_quarantine_stops_an_update_and_the_next_one_succeeds() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.json");
        let s = Store::<Cfg>::open_path(&p, StoreOptions::new(schema())).unwrap();
        bump(&s, 1).unwrap(); // the second write makes the .bak
        fs::write(&p, b"{ damaged").unwrap();
        {
            let _fault = fault::fail_rename(io::ErrorKind::PermissionDenied);
            let r = bump(&s, 2);
            assert!(
                matches!(
                    r,
                    Err(Error::PreserveFailed {
                        step: PreserveStep::Quarantine,
                        ..
                    })
                ),
                "{r:?}"
            );
        }
        assert_eq!(
            fs::read(&p).unwrap(),
            b"{ damaged",
            "nothing written over it"
        );
        assert_eq!(s.get().n, 1, "the in-memory value is unchanged");
        // The cause is gone: the same write now sets the file aside and succeeds.
        bump(&s, 3).unwrap();
        assert_eq!(s.get().n, 3);
        let kept: Vec<String> = names(d.path())
            .into_iter()
            .filter(|name| name.starts_with("s.json.corrupt-"))
            .collect();
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!(fs::read(d.path().join(&kept[0])).unwrap(), b"{ damaged");
    }

    #[test]
    fn a_failed_backup_refresh_stops_the_write_and_keeps_the_previous_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.json");
        let s = Store::<Cfg>::open_path(&p, StoreOptions::new(schema())).unwrap();
        bump(&s, 1).unwrap();
        let before = fs::read(&p).unwrap();
        {
            let _fault = fault::fail_copy(io::ErrorKind::StorageFull);
            match bump(&s, 2) {
                Err(Error::PreserveFailed {
                    step: PreserveStep::Backup,
                    path,
                    kept_as,
                    source,
                }) => {
                    assert_eq!(path, p);
                    assert_eq!(kept_as, d.path().join("s.json.bak"));
                    assert_eq!(source.kind(), io::ErrorKind::StorageFull);
                }
                other => panic!("expected PreserveFailed(Backup), got {other:?}"),
            }
        }
        assert_eq!(
            fs::read(&p).unwrap(),
            before,
            "the previous file is still there"
        );
        assert_eq!(s.get().n, 1);
        bump(&s, 2).unwrap();
        assert_eq!(s.get().n, 2);
    }

    #[test]
    fn a_failed_corrupt_copy_stops_an_overwrite() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.json");
        fs::write(&p, b"{ damaged").unwrap();
        let _fault = fault::fail_copy(io::ErrorKind::PermissionDenied);
        let opts = StoreOptions::new(schema())
            .on_corrupt(OnCorrupt::Overwrite)
            .keep_corrupt_copy(true);
        let err = Store::<Cfg>::open_path(&p, opts)
            .err()
            .expect("open must fail");
        assert!(
            matches!(
                err,
                Error::PreserveFailed {
                    step: PreserveStep::CorruptCopy,
                    ..
                }
            ),
            "{err:?}"
        );
        assert_eq!(fs::read(&p).unwrap(), b"{ damaged");
        assert_eq!(names(d.path()), vec!["s.json"]);
    }
}
