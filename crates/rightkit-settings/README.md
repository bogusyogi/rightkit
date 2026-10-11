# rightkit-settings

Versioned settings persistence for Right Suite apps. It owns one JSON document per app (or one explicit file), typed with the app's own `serde` struct (defaults come from `Default` or `#[serde(default)]`), with migrations, atomic writes, corruption recovery, cross-process transactions, and change events.

**Use it** for an app's settings file. **Do not use it** to store secret values (use `rightkit-secrets`; settings hold only a `SecretRef`).

Behavior:

- **Migrations**: ordered `Migration { to, apply }` transforms over the raw JSON object; a legacy flat file with no envelope is adopted as version 0 (envelope mode).
- **Atomic writes**: a temp file in the destination directory is flushed, renamed over the settings file (`rename(2)`, or `std::fs::rename` on Windows), and on Unix and macOS the directory is flushed. This is `rightkit_fs::atomic_write_with_options` with its default `ReplaceMode::Rename`. Windows has no directory flush. The previous good file is kept as `settings.json.bak`; if that copy fails, the write stops before it touches the settings file (`Error::PreserveFailed`, see below).
- **Cross-process transactions**: writes take an advisory lock on `settings.json.lock`, reload the committed value, apply the edit, and save. Concurrent updates to different keys from other stores or processes are not lost. `update_at` / `revision` give compare-and-swap.
- **Corruption recovery**: a main file that is present but does not parse is quarantined as `settings.json.corrupt-<unix>`, then `.bak` is tried, then defaults. If the damaged file cannot be moved aside, the operation fails with `Error::PreserveFailed` and the damaged file is left exactly where it is: nothing is written over a damaged file that could not be preserved. Single invalid fields are dropped back to their defaults rather than resetting all. A file that exists but cannot be read (permission denied, I/O error) is an `Error::Io`: nothing is quarantined or overwritten.
- **Cached reads**: `get()` compares the backend's stamp (for files: inode, mtime, ctime, length) with the one the cache was read under, and re-parses only when it changed.
- **Newer file** (downgrade): opening fails with `Error::NewerSchema`; nothing is overwritten.
- **Change events**: `subscribe` callbacks receive the changed top-level keys after a successful save. Failed saves leave the in-memory value unchanged. The one exception is `Error::Replaced`: the new file is already in place, so do not retry the write; `get()` reads it and no event fires for that write.
- **Secrets by reference only**: `SecretRef` (service + account) may live in settings; `SecretProbe` reports presence. Values are never stored here.

## Opening a store

```rust,ignore
use rightkit_settings::{FileFormat, Schema, Store, StoreOptions};

// Default: <root>/<namespace>/settings.json, versioned envelope.
let store: Store<Settings> = Store::open(&root, Schema::new("my-app", 2))?;

// Explicit file path, any directory; same envelope.
let store = Store::<Settings>::open_path("/data/prefs.json", StoreOptions::new(schema))?;

// Flat file: the settings object itself, readable by builds that parse it directly.
let store = Store::<Settings>::open_path(
    &path,
    StoreOptions::new(schema).format(FileFormat::Flat),
)?;

// Read-only for a second process: creates nothing and never waits for the writer's lock.
let view = Store::<Settings>::open_read_only(&path, StoreOptions::new(schema))?;

// Defaults while the file does not exist: nothing is written until the first real change.
let opts = StoreOptions::new(schema).defaults_provider(|| serde_json::json!({ "theme": "dark" }));

// What `null` means in a patch (default: reset to the default), and a check beyond type checks.
let opts = StoreOptions::new(schema)
    .null_means(NullMeans::SetNull)
    .validate_values(|values| match values.get("font_px").and_then(|v| v.as_u64()) {
        Some(8..=72) | None => Ok(()),
        Some(px) => Err(format!("font_px {px} outside 8..=72")),
    });
```

Migrations are `Migration { to, apply }` with `apply: fn(&mut Map<String, Value>) -> Result<(), Error>`; an error aborts the open and nothing is written.

### File formats

- **Envelope** (default): `{ "schemaVersion", "namespace", "values", "revision" }`. Migrations run on version change. The declared fields are rewritten on each save, so undeclared keys are dropped.
- **Flat** (`FileFormat::Flat`): the file is the settings object with no envelope and no version. Migrations do not run. Undeclared top-level keys are kept, and a save changes only the values that changed. Unchanged values keep their exact bytes, the original key order, the indent unit, and the trailing newline. Revisions are a fingerprint of the file bytes.
- **Opt-in to the envelope**: open the same path with the envelope format. The flat object is adopted as version 0 and migrated, and the next write produces the envelope. Undeclared keys are dropped on that first write. Flat readers then refuse the file with `Error::Invalid`.

Builds that read a flat file directly keep working with `Flat`. A build of this crate that opens the file in envelope mode adopts it as version 0 and rewrites it as an envelope. Run only one mode per file.

### Dynamic keys and dry runs

```rust,ignore
let opts = StoreOptions::new(schema)
    .allow_unknown_keys(|k| k.strip_prefix("signature:").is_some_and(|a| !a.is_empty()));
let store = Store::<Settings>::open_path(&path, opts)?;

store.patch(&serde_json::json!({ "signature:acct-1": "Thanks" }), None)?;   // stored, retained across saves
let preview = store.patch_dry_run(&serde_json::json!({ "theme": "dark" }), None)?; // writes nothing
```

`patch` rejects undeclared keys unless `allow_unknown_keys` accepts them. An allow-list (`Some(&[...])`) refuses undeclared keys.

### Repair, optional fields, remove, and sidecars

- One resolver serves `Store::open`, `get()`, and `read_with_defaults` / `read_with_options`. It starts from the caller's defaults and overlays the file's keys one at a time; a key is dropped only if it stops the settings type parsing. Legacy `#[serde(alias)]` spellings in the file win over the default's canonical key, and a null-valued key that collides with another spelling is the one dropped. `T::default()` is never substituted; unusable defaults are `Error::Invalid`.
- `fill_optional(bool)` (`StoreOptions`, `ReadOptions`; default `false`). A key is optional when the type parses without it (`#[serde(default)]`, `Option`). By default a field the file omits keeps plain serde behaviour; with `true` it is filled from the defaults. `ReadOptions::fill_optional_when_repaired()` fills them only when the same read repaired another field (a dropped or defaulted field). Required fields the file omits, and fields the file holds with an invalid value, always take the defaults. A missing file uses the defaults whole.
- `Store::remove_keys(&["k"])` (and `remove_keys_dry_run`) deletes a key if present and does nothing, with no write, if absent, in either `NullMeans` mode. A declared key returns to the value the type yields when the key is absent.
- Sidecar read rule: with `sidecars(false)` the crate never reads `<path>.bak` (a missing file with a stale `.bak` reads as defaults, also under `OnCorrupt::Refuse`). Use `ReadOptions::sidecars(false)` to match on `read_with_options`.

### Durability, damaged files, formatting, flat migration

- **Replace mode** (`replace_mode`, `durable`, `retry_sharing_violation`). Default `Rename` with `durable(true)`: on Unix and macOS the temp file is flushed, renamed, and the directory flushed. On Windows the rename is not write-through, so a power loss can bring back the previous (complete) file. Only `ReplaceMode::MoveWriteThrough` is write-through on Windows. It can hit a sharing violation when another process holds the file open, so pair it with a retry:

  ```rust,ignore
  let opts = StoreOptions::new(schema)
      .replace_mode(ReplaceMode::MoveWriteThrough)
      .retry_sharing_violation(5, std::time::Duration::from_millis(50));

  // What a write with these options gets on this platform, given whether the file exists:
  let guarantee = opts.achieved_durability(path.exists()); // PublicationDurability
  ```

  No power-loss test has been run for any Windows mode.
- **Post-publication failures**: `Error::Replaced { path, source }` means the new file is in place and a later step failed (on Unix and macOS, the directory flush). The write is not undone. Do not repeat it as if it had failed; an `update` re-run applies its edit twice.
- **Damaged files**: `OnCorrupt::Quarantine` (default) renames it aside; `Refuse` errors; `Overwrite` leaves it in place until the atomic replace succeeds (add `keep_corrupt_copy(true)` for a copy made before the replace). Name the quarantine file with `quarantine_name(QuarantineName::Fixed(".corrupt".into()))` to overwrite one fixed file, or keep the timestamped default.
- **A step that keeps the previous content fails**: the operation fails and nothing is written. `Error::PreserveFailed { step, path, kept_as, source }` names the file, where it was to go and the I/O cause; `step` is `PreserveStep::Quarantine` (a damaged file could not be moved aside; it stays in place byte for byte), `CorruptCopy` (the `keep_corrupt_copy` copy), or `Backup` (`<path>.bak` could not be refreshed from the current good file). The in-memory value is unchanged and no change event fires. A retry repeats the step, so remove the cause (permissions, a full disk, a directory or locked file at `kept_as`) first. `sidecars(false)` has no `.bak` to refresh.
- **Formatting**: `trailing_newline(true)` ends every write with a newline without rewriting unchanged values. `key_order` is below.

### Key order

`StoreOptions::key_order(KeyOrder::{Preserve, Sorted})` decides the order of object keys in what a write produces. Number formatting, string escaping and whitespace are the same in both.

- **`Preserve`** (default). A key the file already holds stays where it is, at every nesting level (an object inside an array follows the element at the same index). A key the file does not hold yet is appended after them, in the order the settings type serialises its fields; open keys (`allow_unknown_keys`) follow. A removed key disappears and nothing else moves. A file written by another build, or by the old app, keeps its order, so diffs stay small: an unchanged document rewrites to the same bytes, and a patch changes the bytes of the value it changes (an envelope also advances `revision`). A **new** file is written in the settings type's field order, with the envelope's own keys as `schemaVersion`, `namespace`, `values`, `revision`.
- **`Sorted`**. Alphabetical (byte order) at every level: the envelope exactly as it was written before this option existed. A flat file still keeps the order of the keys it holds, which is its contract; `Sorted` applies to the keys a write adds and the values it rewrites.
- A file written sorted by an earlier version stays sorted under `Preserve`. A key it did not hold is appended after the sorted ones, not inserted in its sorted place; use `Sorted` to keep inserting in place.
- A legacy flat file adopted as an envelope keeps its key order inside `values`.
- A patch is a `serde_json::Value`, which cannot carry the order it was written in: several new open keys in one patch are appended alphabetically (in the patch's own order when serde_json's `preserve_order` feature is on somewhere in the build; this crate does not enable it, because that would change `serde_json::Map` for every crate in the build). Keys added by separate patches are appended in call order.
- A `HashMap` field has no stable serialisation order, so several new keys inside one are appended in no stable order. Use a struct or a `BTreeMap`.
- `pretty_sorted_newline(true)` always sorts, whatever `key_order` says.
- **Flat migration**: `flat_migration(|map| ...)` returns `(migrated, changed)`; it runs once on open, before defaults and repair, never read-only.

### Custom backends

`SettingsBackend` is the storage seam. The default is `FileBackend`. An app can plug in its own, for example an encrypted database. Implement `load` and `patch` (`patch` runs the edit under the backend's exclusion), then open with `Store::with_backend(backend, options)`. A backend that writes ordered text can also implement `patch_ordered`, whose edit returns the `KeyLayout` (the settings type's key order) beside its decision; the default drops the layout and calls `patch`, so existing backends need no change. The trait documentation carries an illustrative SQLCipher example. No SQLCipher implementation ships in this crate.

## Platform support

`src/backend.rs` has Unix and non-Unix branches (for example `file_stamp`, the cache stamp from inode and ctime). Atomic replacement goes through `atomic_write_mode` in `src/atomic.rs`, which calls `rightkit_fs::atomic_write_with_options`; `rightkit-fs` owns the per-platform replace and directory sync.

Published version: see `INDEX.md` at the repository root. See `CHANGELOG.md`.

## Features

None. The crate has no Cargo features. Dependencies: `serde`, `serde_json` (with `raw_value`), `thiserror`, and `rightkit-fs` (`fs` feature only).
