use crate::store::{Error, Schema};
use serde_json::{Map, Value};
use std::sync::Arc;
use std::time::Duration;

/// Supplies the settings used while no file exists. Called on every open and read until the
/// first explicit save; it must return a JSON object.
pub type DefaultsProvider = Arc<dyn Fn() -> Value + Send + Sync>;

/// Decides whether an undeclared top-level key may be patched. See
/// [`StoreOptions::allow_unknown_keys`].
pub type KeyValidator = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Checks the complete candidate values before an edit that changes something is committed.
/// See [`StoreOptions::validate_values`].
pub type Validator = Arc<dyn Fn(&Map<String, Value>) -> Result<(), String> + Send + Sync>;

/// On-disk layout of the default [`crate::FileBackend`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FileFormat {
    /// `{ "schemaVersion", "namespace", "values", "revision" }`. The default.
    #[default]
    Envelope,
    /// The settings object itself: no envelope, no version, no migrations. Top-level keys the
    /// settings type does not declare are kept as written, and untouched values keep their bytes.
    /// Older builds that read the flat object directly keep working.
    Flat,
}

/// What a `null` in a patch means. The default is [`NullMeans::Reset`].
///
/// | patch `null` on...        | `Reset` (default)                  | `SetNull`                    |
/// |---------------------------|------------------------------------|------------------------------|
/// | declared key, held        | resets to the type's default       | stores `null`                |
/// | declared key, absent      | resets; nothing created, no write  | stores `null`                |
/// | open key, held            | removes the key                    | stores `null`                |
/// | open key, absent          | nothing created, no write          | stores `null`                |
///
/// `SetNull` is idempotent: applying the same patch twice leaves the document unchanged and the
/// second apply writes nothing. Only `SetNull` creates a key from a `null`. Removal is a separate
/// operation, [`crate::Store::remove_keys`], which deletes a key if present and does nothing if
/// absent, in either mode (under `SetNull` it is the only way to delete an open key).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum NullMeans {
    /// `null` resets the key to its default. The default.
    #[default]
    Reset,
    /// `null` stores JSON `null`. The settings type must accept it (for example `Option<T>`), or
    /// the patch fails. Open keys store `null` too, held or absent.
    SetNull,
}

/// What opening or writing does when the settings file is unreadable as JSON.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OnCorrupt {
    /// Move the damaged file aside as `<path>.corrupt-<ms>`, then use `<path>.bak` if present.
    /// The default.
    #[default]
    Quarantine,
    /// Return [`crate::Error::Corrupt`] and touch nothing: no rename, no write, no `.bak` read.
    Refuse,
    /// Treat the damaged file as absent and never rename or move it. The old bytes stay at the
    /// path until an atomic replace succeeds, so a failed write leaves them in place. Reads fall
    /// back to `<path>.bak` as for `Quarantine`. A damaged file is never copied over a good
    /// `<path>.bak`. No copy of the damaged bytes is kept unless
    /// [`StoreOptions::keep_corrupt_copy`] asks for one (made by copy before the replace).
    Overwrite,
}

/// Supplies a migration for a [`FileFormat::Flat`] file, which carries no version. It receives
/// the parsed object exactly as stored and returns the migrated object and whether it changed
/// anything. See [`StoreOptions::flat_migration`].
pub type FlatMigration =
    Arc<dyn Fn(Map<String, Value>) -> Result<(Map<String, Value>, bool), Error> + Send + Sync>;

/// How the settings file replaces the previous one. Mirrors `rightkit_fs::ReplaceMode`; the default
/// is [`ReplaceMode::Rename`], what every earlier version did.
///
/// What each gives, with `durable` left at its default `true` (the temp file is always flushed
/// before the replace; with `durable(false)` nothing is flushed on any platform):
///
/// | mode | Unix / macOS | Windows |
/// |---|---|---|
/// | `Rename` | `rename(2)`; the directory is `fsync`ed after it, so name and content survive power loss | `std::fs::rename` (POSIX replace semantics: an open reader never blocks the writer). The rename is **not** write-through and there is no directory flush, so after a power loss the previous content may reappear (never a torn file) |
/// | `ReplaceFile` | same as `Rename` | `ReplaceFileW` with `REPLACEFILE_WRITE_THROUGH` when the file exists, and `MoveFileExW` with `MOVEFILE_WRITE_THROUGH` for the first creation: on stable storage when the call returns. Not POSIX: a reader holding the file open without share-delete can make it fail with a sharing violation (see [`StoreOptions::retry_sharing_violation`]) |
/// | `MoveWriteThrough` | same as `Rename` | `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING \| MOVEFILE_WRITE_THROUGH`, existing or not. Same sharing caveat as `ReplaceFile` |
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReplaceMode {
    #[default]
    Rename,
    ReplaceFile,
    MoveWriteThrough,
}

/// Retry for a publish step that fails because another process holds the file open. Only Windows
/// sharing and lock violations are retried; the policy is inert on other platforms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SharingRetry {
    /// Retries after the first attempt. Zero means one attempt.
    pub attempts: u32,
    /// Pause before each retry.
    pub delay: Duration,
}

/// Where a quarantined (or, with [`StoreOptions::keep_corrupt_copy`], copied-aside) damaged file
/// goes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum QuarantineName {
    /// `<path>.corrupt-<unix seconds>`. Each quarantine keeps its own file. The default.
    #[default]
    Timestamped,
    /// `<path><suffix>` (for example `".corrupt"`). A later quarantine overwrites the earlier one.
    /// The suffix must be non-empty and contain no path separator.
    Fixed(String),
}

impl QuarantineName {
    pub(crate) fn validate(&self) -> Result<(), Error> {
        match self {
            QuarantineName::Fixed(s) if s.is_empty() || s.contains(['/', '\\']) => Err(
                Error::BadSchema("quarantine suffix must be non-empty with no path separator"),
            ),
            _ => Ok(()),
        }
    }
}

/// Options for opening a [`crate::Store`]. Built from the schema, then refined.
#[derive(Clone)]
pub struct StoreOptions {
    pub(crate) schema: Schema,
    pub(crate) format: FileFormat,
    pub(crate) defaults: Option<DefaultsProvider>,
    pub(crate) open_keys: Option<KeyValidator>,
    pub(crate) null_means: NullMeans,
    pub(crate) validator: Option<Validator>,
    pub(crate) persist_defaults: bool,
    pub(crate) fill_optional: bool,
    pub(crate) on_corrupt: OnCorrupt,
    pub(crate) sidecars: bool,
    pub(crate) pretty_sorted_newline: bool,
    pub(crate) replace: ReplaceMode,
    pub(crate) durable: bool,
    pub(crate) sharing_retry: Option<SharingRetry>,
    pub(crate) keep_corrupt_copy: bool,
    pub(crate) quarantine_name: QuarantineName,
    pub(crate) trailing_newline: bool,
    pub(crate) flat_migration: Option<FlatMigration>,
}

impl StoreOptions {
    /// Envelope format, no defaults provider, no open keys, `null` resets, no value validator,
    /// and no forced first write of defaults.
    pub fn new(schema: Schema) -> Self {
        Self {
            schema,
            format: FileFormat::Envelope,
            defaults: None,
            open_keys: None,
            null_means: NullMeans::Reset,
            validator: None,
            persist_defaults: false,
            fill_optional: false,
            on_corrupt: OnCorrupt::Quarantine,
            sidecars: true,
            pretty_sorted_newline: false,
            replace: ReplaceMode::Rename,
            durable: true,
            sharing_retry: None,
            keep_corrupt_copy: false,
            quarantine_name: QuarantineName::Timestamped,
            trailing_newline: false,
            flat_migration: None,
        }
    }

    /// How a write replaces the file. Default [`ReplaceMode::Rename`] (unchanged). See the table on
    /// [`ReplaceMode`] for what each gives on Unix and Windows.
    pub fn replace_mode(mut self, mode: ReplaceMode) -> Self {
        self.replace = mode;
        self
    }

    /// `true` (default): the temp file is flushed before the replace (and, on Unix, the directory
    /// after it; with `ReplaceFile` / `MoveWriteThrough` on Windows the replace is write-through).
    /// `false`: no flush on any platform; only for rebuildable data.
    pub fn durable(mut self, yes: bool) -> Self {
        self.durable = yes;
        self
    }

    /// Retry the replace up to `attempts` more times, pausing `delay` before each, when Windows
    /// reports a sharing or lock violation (another process holds the file open). Default: no
    /// retry. Inert on Unix. Useful with `ReplaceFile` and `MoveWriteThrough`.
    pub fn retry_sharing_violation(mut self, attempts: u32, delay: Duration) -> Self {
        self.sharing_retry = Some(SharingRetry { attempts, delay });
        self
    }

    /// With [`OnCorrupt::Overwrite`]: keep the damaged bytes by COPYING the file to the quarantine
    /// name immediately before the replace (never a rename, so a failed copy or replace leaves the
    /// original in place). A failed copy aborts the write. Default `false`. No effect on the
    /// other policies.
    pub fn keep_corrupt_copy(mut self, yes: bool) -> Self {
        self.keep_corrupt_copy = yes;
        self
    }

    /// Naming of the quarantine file. Default [`QuarantineName::Timestamped`].
    pub fn quarantine_name(mut self, name: QuarantineName) -> Self {
        self.quarantine_name = name;
        self
    }

    /// `true`: every write ends with a `\n`. Keys are not sorted and values are not rewritten;
    /// a flat rewrite still keeps unchanged values' bytes and the key order. Nothing is written
    /// when nothing changed, so an existing file without the newline gets one at the next real
    /// write. Default `false`. Independent of [`StoreOptions::pretty_sorted_newline`], which also
    /// sorts keys.
    pub fn trailing_newline(mut self, yes: bool) -> Self {
        self.trailing_newline = yes;
        self
    }

    /// A migration for [`FileFormat::Flat`] files, which have no version. `migrate` gets the parsed
    /// object as stored and returns the migrated object and whether it changed anything.
    ///
    /// It runs once on open, before defaults are applied and before repair (so it sees the
    /// file's keys only, and repair then fills or drops fields of the migrated object). If it
    /// reports a change, the store commits the migrated document once during open. An `Err`
    /// aborts the open and nothing is written. It never runs for [`crate::Store::open_read_only`]
    /// (the value is then the unmigrated file), for a missing file, or for envelope files (which
    /// use [`crate::Schema::migration`]). It must be idempotent: a crash after the file write
    /// and before your own bookkeeping runs it again on the migrated object.
    pub fn flat_migration(
        mut self,
        migrate: impl Fn(Map<String, Value>) -> Result<(Map<String, Value>, bool), Error>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        let migrate: FlatMigration = Arc::new(migrate);
        self.flat_migration = Some(migrate);
        self
    }

    /// Chooses what happens to an unreadable file. Default [`OnCorrupt::Quarantine`].
    pub fn on_corrupt(mut self, policy: OnCorrupt) -> Self {
        self.on_corrupt = policy;
        self
    }

    /// `true` (default): writes keep a `<path>.bak` of the previous file and take a
    /// `<path>.lock` sidecar for cross-process exclusion. `false`: the file is the only file a
    /// write creates (the atomic-write temporary is renamed away), and writers in this process
    /// are serialised by an in-process lock. Other processes are not excluded in this mode.
    pub fn sidecars(mut self, yes: bool) -> Self {
        self.sidecars = yes;
        self
    }

    /// `true`: envelope (and flat, rewritten as a whole) output is pretty JSON with object keys
    /// sorted at every level and a trailing newline. The bytes are a function of the values
    /// alone, so unchanged values produce unchanged files. Default `false` keeps the existing
    /// output.
    pub fn pretty_sorted_newline(mut self, yes: bool) -> Self {
        self.pretty_sorted_newline = yes;
        self
    }

    /// When `true`, the first committed save against a missing file writes it even if the edit
    /// changes nothing, so the defaults are stored on disk. Default `false`: a no-change edit
    /// leaves the file missing. Only the first save of a missing file is affected.
    pub fn persist_defaults_on_first_save(mut self, yes: bool) -> Self {
        self.persist_defaults = yes;
        self
    }

    /// When `true`, optional fields the file merely omits are filled from the defaults. A field
    /// is optional when the settings type parses without its key (a `#[serde(default)]` field or
    /// an `Option`). Default `false`: an omitted optional field stays at plain serde behaviour
    /// (its serde default or `None`). Required fields the file omits, and fields the file holds
    /// with an invalid value, always take the defaults. A missing file uses the defaults whole.
    pub fn fill_optional(mut self, yes: bool) -> Self {
        self.fill_optional = yes;
        self
    }

    /// Selects the on-disk layout. [`FileFormat::Flat`] implies retaining undeclared keys.
    pub fn format(mut self, format: FileFormat) -> Self {
        self.format = format;
        self
    }

    /// Settings to use while the file is missing. Opening or reading a missing file with a
    /// provider writes nothing and creates no directory or lock file; the first explicit
    /// `update` or `patch` that changes a value writes the file.
    pub fn defaults_provider(
        mut self,
        provide: impl Fn() -> Value + Send + Sync + 'static,
    ) -> Self {
        let provide: DefaultsProvider = Arc::new(provide);
        self.defaults = Some(provide);
        self
    }

    /// Lets [`crate::Store::patch`] accept top-level keys the settings type does not declare
    /// (for example `signature:<account>`), when `valid` returns true for the key. Such keys are
    /// stored and retained across later saves. Patches that pass an allow-list reject them.
    pub fn allow_unknown_keys(
        mut self,
        valid: impl Fn(&str) -> bool + Send + Sync + 'static,
    ) -> Self {
        let valid: KeyValidator = Arc::new(valid);
        self.open_keys = Some(valid);
        self
    }

    /// Chooses what `null` means in a patch for declared keys. Default [`NullMeans::Reset`].
    pub fn null_means(mut self, meaning: NullMeans) -> Self {
        self.null_means = meaning;
        self
    }

    /// Validation beyond type checks. It sees the complete candidate values (declared and
    /// retained keys) and returns `Err(message)` to reject the edit. It runs only for edits that
    /// change something, in `update`, `update_at`, `patch`, and `patch_dry_run`. Nothing is
    /// written when it fails. Repairs and migrations of an existing file are not validated.
    pub fn validate_values(
        mut self,
        check: impl Fn(&Map<String, Value>) -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        let check: Validator = Arc::new(check);
        self.validator = Some(check);
        self
    }
}

/// Options for [`crate::read_with_options`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadOptions {
    pub(crate) format: FileFormat,
    pub(crate) fill_optional: bool,
    pub(crate) sidecars: bool,
}

impl ReadOptions {
    /// The given layout; optional fields the file omits are not filled.
    pub fn new(format: FileFormat) -> Self {
        Self {
            format,
            fill_optional: false,
            sidecars: true,
        }
    }

    /// `false`: never read `<path>.bak`; a missing or unreadable main file reads as the defaults.
    /// Default `true`. Match the store's `sidecars` option.
    pub fn sidecars(mut self, yes: bool) -> Self {
        self.sidecars = yes;
        self
    }

    /// Same meaning as [`StoreOptions::fill_optional`]. Default `false`.
    pub fn fill_optional(mut self, yes: bool) -> Self {
        self.fill_optional = yes;
        self
    }
}

impl Default for ReadOptions {
    fn default() -> Self {
        Self::new(FileFormat::Envelope)
    }
}
