use crate::store::{Error, Schema};
use rightkit_fs::PublicationDurability;
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

/// The order of object keys in the file a write produces. Set it with [`StoreOptions::key_order`];
/// the default is [`KeyOrder::Preserve`].
///
/// Only the order of keys differs. Number formatting, string escaping and whitespace are the same
/// in both modes, and the document a store reads back is the same: JSON objects are unordered.
///
/// | | `Preserve` (default) | `Sorted` |
/// |---|---|---|
/// | key the file already holds | stays where it is, at every nesting level | alphabetical (byte order) |
/// | key the file does not hold yet | appended after the held keys, in the order the settings type serialises its fields; open keys (see [`StoreOptions::allow_unknown_keys`]) follow | alphabetical |
/// | key removed | gone, the others do not move | gone |
/// | new file | the settings type's field order | alphabetical |
///
/// A file written sorted (every envelope this crate wrote before this option existed) stays
/// sorted under `Preserve`, but a key it did not hold is appended after the sorted ones instead of
/// being inserted in place. Two edits that change different values touch different bytes only.
///
/// - **Envelope.** Under `Preserve` the envelope's own four keys keep the order the file has them
///   in; a new file gets `schemaVersion`, `namespace`, `values`, `revision`. A legacy flat file
///   adopted as version 0 keeps the order of its keys inside `values`. Under `Sorted` the whole
///   document is sorted, as before.
/// - **Flat.** A flat file keeps the order of the keys it holds in both modes (that is its
///   contract, together with the bytes of unchanged values). `Sorted` differs only in how a
///   value that changed, and a key the file did not hold, are written: alphabetically, as before.
/// - **Arrays.** An object inside an array keeps the key order of the element at the same index.
/// - **Open keys.** A patch is a `serde_json::Value`, which cannot carry the order the caller
///   wrote it in: with serde_json's default map several new open keys of one patch are appended
///   alphabetically (in the patch's own order if serde_json's `preserve_order` feature is on in
///   the build). Keys added by separate patches are appended in call order.
/// - **Maps in the settings type.** A `HashMap` field serialises in an order that changes between
///   runs, so several new keys inside one are appended in no stable order. Use a struct or a
///   `BTreeMap`.
///
/// [`StoreOptions::pretty_sorted_newline`] always sorts, whatever this says.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum KeyOrder {
    /// Keep the order of the keys the file holds; append new keys as the table above says. The
    /// default.
    #[default]
    Preserve,
    /// Alphabetical order, as the envelope was written before [`StoreOptions::key_order`] existed.
    Sorted,
}

/// What [`crate::Store`] does when the settings file is one this build must not read or change: its
/// schema version is newer than the code's, or it is larger than [`StoreOptions::max_bytes`].
/// Set it with [`StoreOptions::on_refused`].
///
/// The file is never moved, quarantined, backed up over or rewritten in either mode; the modes
/// differ in what the caller gets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OnRefused {
    /// `open` fails: [`crate::Error::NewerSchema`] for a newer version, [`crate::Error::Invalid`]
    /// for an oversized file. The default (what every earlier version did).
    #[default]
    Fail,
    /// `open` succeeds with read-only defaults. `get` returns the defaults (the provider's, else
    /// the type's own), `update`, `patch` and `remove_keys` fail with the same errors `Fail`
    /// opens with and touch nothing (not even the `<path>.lock` sidecar), and
    /// [`crate::Store::writable`] is `false` while the file stays refused,
    /// [`crate::Store::refusal`] says why. The file is checked again on every `get` and write:
    /// once it is replaced by one this build accepts (the user reinstalled the matching version,
    /// or deleted the file), the store reads and writes it normally.
    ReadOnlyDefaults,
}

/// What opening or writing does when the settings file is unreadable as JSON.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OnCorrupt {
    /// Move the damaged file aside as `<path>.corrupt-<unix seconds>`, then use `<path>.bak` if
    /// present. If the file cannot be moved, the operation fails with
    /// [`crate::Error::PreserveFailed`] and the damaged file stays where it is: nothing is
    /// written over a damaged file that could not be preserved. The default.
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
/// before the replace; with `durable(false)` nothing is flushed on any platform). Ask
/// [`StoreOptions::achieved_durability`] for the answer that applies to a given call:
///
/// | mode | Unix / macOS | Windows |
/// |---|---|---|
/// | `Rename` | `rename(2)`, then the directory is flushed (`DirectoryFlushed`); a failed directory flush is [`crate::Error::Replaced`] | `std::fs::rename` (POSIX replace semantics: an open reader never blocks the writer). Not write-through and no directory flush (`FileFlushedOnly`): after a power loss the previous content may reappear (never a torn file) |
/// | `MoveWriteThrough` | same as `Rename` | `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING \| MOVEFILE_WRITE_THROUGH`, existing or not (`WriteThroughMove`). Not POSIX: a reader holding the file open without share-delete can make it fail with a sharing violation (see [`StoreOptions::retry_sharing_violation`]). Use this mode where the replacement must be on stable storage; no power-loss test has been run |
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReplaceMode {
    #[default]
    Rename,
    MoveWriteThrough,
}

/// Who may read the settings file a write produces. Set it with [`StoreOptions::write_security`] or
/// `FileBackend::write_security`. This crate only names the profile: the permission and ACL work is
/// done by `rightkit_fs` (`Privacy`), and a write whose protection cannot be established fails with
/// [`crate::Error::Protection`] and publishes nothing; it never falls back to the default profile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WriteSecurity {
    /// Today's behaviour: no privacy step. The file takes the existing file's mode or the
    /// platform default (Unix `0666 & ~umask`) and the parent directory's inherited ACL
    /// (Windows). The default, kept for compatibility.
    #[default]
    Default,
    /// Owner-only on every platform: Unix mode `0600` from creation with `0700` parents (macOS
    /// refuses a parent whose extended ACL would grant others access), a single protected ACE
    /// for the current user on Windows. The `.bak` and corrupt copies are already owner-only
    /// whatever the profile.
    OwnerPrivate,
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
    pub(crate) write_security: WriteSecurity,
    pub(crate) keep_corrupt_copy: bool,
    pub(crate) quarantine_name: QuarantineName,
    pub(crate) trailing_newline: bool,
    pub(crate) flat_migration: Option<FlatMigration>,
    pub(crate) key_order: KeyOrder,
    pub(crate) max_bytes: Option<u64>,
    pub(crate) on_refused: OnRefused,
    pub(crate) flat_version_key: Option<String>,
    pub(crate) utf8_bom: bool,
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
            write_security: WriteSecurity::Default,
            keep_corrupt_copy: false,
            quarantine_name: QuarantineName::Timestamped,
            trailing_newline: false,
            flat_migration: None,
            key_order: KeyOrder::Preserve,
            max_bytes: None,
            on_refused: OnRefused::Fail,
            flat_version_key: None,
            utf8_bom: false,
        }
    }

    /// What `open` does with a file that is newer than this build (schema version above
    /// [`crate::Schema::version`]) or over [`StoreOptions::max_bytes`]. Default
    /// [`OnRefused::Fail`]. [`OnRefused::ReadOnlyDefaults`] opens with the defaults and a store
    /// that refuses every write, leaving the file exactly as it is. Does not cover a damaged file:
    /// that is [`StoreOptions::on_corrupt`] (`OnCorrupt::Refuse` leaves it untouched too).
    pub fn on_refused(mut self, policy: OnRefused) -> Self {
        self.on_refused = policy;
        self
    }

    /// For [`FileFormat::Flat`], which has no envelope: the top-level key that holds the file's
    /// schema version as a non-negative integer (for example `"schema_version"`). A value above
    /// [`crate::Schema::version`] makes the file "newer" (see [`StoreOptions::on_refused`]); a
    /// missing or non-integer value is not newer. The key stays an ordinary value: it is kept
    /// and written as any other, and no migration runs for a flat file. No effect on
    /// [`FileFormat::Envelope`], whose version is `schemaVersion`.
    pub fn flat_version_key(mut self, key: impl Into<String>) -> Self {
        self.flat_version_key = Some(key.into());
        self
    }

    /// The order of object keys in the file a write produces. Default [`KeyOrder::Preserve`]:
    /// keys the file holds keep their place, new keys are appended in the settings type's field
    /// order. [`KeyOrder::Sorted`] writes them alphabetically, as the envelope was written before
    /// this option existed. See [`KeyOrder`] for the exact rules per format.
    pub fn key_order(mut self, order: KeyOrder) -> Self {
        self.key_order = order;
        self
    }

    /// How a write replaces the file. Default [`ReplaceMode::Rename`] (unchanged). See the table on
    /// [`ReplaceMode`] for what each gives on Unix and Windows.
    pub fn replace_mode(mut self, mode: ReplaceMode) -> Self {
        self.replace = mode;
        self
    }

    /// `true` (default): the temp file is flushed before the replace; on Unix and macOS the
    /// directory is flushed after it. On Windows only `MoveWriteThrough` is write-through; `Rename`
    /// is not, and this flag cannot change that. `false`: no flush on any platform; only for
    /// rebuildable data.
    /// [`StoreOptions::achieved_durability`] states the result for a given call.
    pub fn durable(mut self, yes: bool) -> Self {
        self.durable = yes;
        self
    }

    /// What a successful write with these options publishes with on this platform, given whether
    /// the settings file already exists. Use it before relying on power-loss durability: on
    /// Windows, `ReplaceMode::Rename` over an existing file returns
    /// [`PublicationDurability::FileFlushedOnly`], so the new content is flushed but the replace
    /// itself is not. Answers the same as `rightkit_fs::publication_durability` for the write.
    pub fn achieved_durability(&self, destination_exists: bool) -> PublicationDurability {
        let mode = crate::atomic::WriteMode {
            replace: self.replace,
            durable: self.durable,
            retry: self.sharing_retry,
            security: self.write_security,
        };
        rightkit_fs::publication_durability(&crate::atomic::fs_options(&mode), destination_exists)
    }

    /// Who may read the file a write produces (see [`WriteSecurity`]). Default
    /// [`WriteSecurity::Default`], what every earlier version did. With
    /// [`WriteSecurity::OwnerPrivate`] a write that cannot be made owner-only fails with
    /// [`crate::Error::Protection`] before anything is published; the existing file is not
    /// changed. Reads are unaffected.
    pub fn write_security(mut self, profile: WriteSecurity) -> Self {
        self.write_security = profile;
        self
    }

    /// Retry the replace up to `attempts` more times, pausing `delay` before each, when Windows
    /// reports a sharing or lock violation (another process holds the file open). Default: no
    /// retry. Inert on Unix. Useful with `MoveWriteThrough`.
    pub fn retry_sharing_violation(mut self, attempts: u32, delay: Duration) -> Self {
        self.sharing_retry = Some(SharingRetry { attempts, delay });
        self
    }

    /// With [`OnCorrupt::Overwrite`]: keep the damaged bytes by COPYING the file to the quarantine
    /// name immediately before the replace (never a rename, so a failed copy or replace leaves the
    /// original in place). A failed copy aborts the write with
    /// [`crate::Error::PreserveFailed`]. Default `false`. No effect on the other policies.
    pub fn keep_corrupt_copy(mut self, yes: bool) -> Self {
        self.keep_corrupt_copy = yes;
        self
    }

    /// Naming of the quarantine file. Default [`QuarantineName::Timestamped`].
    pub fn quarantine_name(mut self, name: QuarantineName) -> Self {
        self.quarantine_name = name;
        self
    }

    /// `true`: every write ends with a `\n`. Keys are not sorted (unless [`StoreOptions::key_order`]
    /// says so) and values are not rewritten; a flat rewrite still keeps unchanged values' bytes
    /// and the key order. Nothing is written when nothing changed, so an existing file without the
    /// newline gets one at the next real write. Default `false`. Independent of
    /// [`StoreOptions::pretty_sorted_newline`], which also sorts keys.
    pub fn trailing_newline(mut self, yes: bool) -> Self {
        self.trailing_newline = yes;
        self
    }

    /// Refuse to read a settings file larger than `limit` bytes. The check is on the file length
    /// before it is read, so an oversized file is never loaded into memory. `open` and writes fail
    /// with [`crate::Error::Invalid`]; the file is left byte for byte as it was (not quarantined,
    /// not overwritten); `get` keeps returning the last known value. With
    /// [`OnRefused::ReadOnlyDefaults`] `open` succeeds instead, with the defaults and a store that
    /// refuses writes. Applies to the main file of a `Store`; the free read has its own cap,
    /// [`ReadOptions::max_bytes`]. Default: no limit.
    pub fn max_bytes(mut self, limit: u64) -> Self {
        self.max_bytes = Some(limit);
        self
    }

    /// `true`: the settings file is a UTF-8 file with a byte order mark (`EF BB BF`, as Windows
    /// editors and some app writers produce). A leading BOM is accepted and removed on read, and
    /// every write puts one back at the start of the file. Default `false`: a file with a BOM is
    /// damaged JSON, handled by [`StoreOptions::on_corrupt`], as before. Applies to reads and writes
    /// of the main file and `<path>.bak`, and to [`crate::read_with_options`] through
    /// [`ReadOptions::utf8_bom`].
    pub fn utf8_bom(mut self, yes: bool) -> Self {
        self.utf8_bom = yes;
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
    /// alone, so unchanged values produce unchanged files. It always sorts: [`StoreOptions::key_order`]
    /// has no effect while it is on. Default `false` keeps the existing output.
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

/// When an omitted optional key is filled from the caller's defaults on a read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum OptionalFill {
    /// Never: an omitted optional key takes its plain serde default. The default.
    #[default]
    Never,
    /// Always: every omitted optional key is filled from the defaults.
    Always,
    /// Only when the read repaired at least one other field (a dropped or defaulted field).
    WhenRepaired,
}

/// Options for [`crate::read_with_options`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadOptions {
    pub(crate) format: FileFormat,
    pub(crate) fill: OptionalFill,
    pub(crate) sidecars: bool,
    pub(crate) utf8_bom: bool,
    pub(crate) max_bytes: Option<u64>,
}

impl ReadOptions {
    /// The given layout; optional fields the file omits are not filled.
    pub fn new(format: FileFormat) -> Self {
        Self {
            format,
            fill: OptionalFill::Never,
            sidecars: true,
            utf8_bom: false,
            max_bytes: None,
        }
    }

    /// Refuse to read a file (the main file, and `<path>.bak` when it is used) longer than `limit`
    /// bytes. The length is checked before the file is read, so an oversized file is never loaded.
    /// The read fails with [`crate::Error::Invalid`] and touches nothing: no quarantine, no write.
    /// Default: no limit.
    pub fn max_bytes(mut self, limit: u64) -> Self {
        self.max_bytes = Some(limit);
        self
    }

    /// Accept and remove a leading UTF-8 byte order mark, as [`StoreOptions::utf8_bom`] does.
    /// Default `false`.
    pub fn utf8_bom(mut self, yes: bool) -> Self {
        self.utf8_bom = yes;
        self
    }

    /// `false`: never read `<path>.bak`; a missing or unreadable main file reads as the defaults.
    /// Default `true`. Match the store's `sidecars` option.
    pub fn sidecars(mut self, yes: bool) -> Self {
        self.sidecars = yes;
        self
    }

    /// Same meaning as [`StoreOptions::fill_optional`]. Default `false`. Replaces any earlier
    /// [`ReadOptions::fill_optional_when_repaired`] on this value.
    pub fn fill_optional(mut self, yes: bool) -> Self {
        self.fill = if yes {
            OptionalFill::Always
        } else {
            OptionalFill::Never
        };
        self
    }

    /// Fill omitted optional keys from the defaults only when the same read repaired at least one
    /// other field (a dropped or defaulted field). A file that needs no repair keeps plain serde
    /// defaults for its omitted optional keys, as with the default `false`. Replaces any earlier
    /// [`ReadOptions::fill_optional`] on this value.
    pub fn fill_optional_when_repaired(mut self) -> Self {
        self.fill = OptionalFill::WhenRepaired;
        self
    }
}

impl Default for ReadOptions {
    fn default() -> Self {
        Self::new(FileFormat::Envelope)
    }
}
