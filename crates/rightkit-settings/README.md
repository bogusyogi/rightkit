# rightkit-settings

Versioned settings persistence for Right Suite apps. It owns one JSON document per app (or one explicit file), typed with the app's own `serde` struct (defaults come from `Default` or `#[serde(default)]`), with migrations, atomic writes, corruption recovery, cross-process transactions, and change events.

**Use it** for an app's settings file. **Do not use it** to store secret values (use `rightkit-secrets`; settings hold only a `SecretRef`).

Behavior:

- **Migrations**: ordered `Migration { to, apply }` transforms over the raw JSON object; a legacy flat file with no envelope is adopted as version 0 (envelope mode).
- **Atomic writes**: a temp file in the destination directory is fsynced, renamed over the settings file (`rename(2)`, or `MoveFileEx` with replace on Windows), and the directory is fsynced. This is `rightkit_fs::atomic_write` with its default `ReplaceMode::Rename`. The previous good file is kept as `settings.json.bak`.
- **Cross-process transactions**: writes take an advisory lock on `settings.json.lock`, reload the committed value, apply the edit, and save. Concurrent updates to different keys from other stores or processes are not lost. `update_at` / `revision` give compare-and-swap.
- **Corruption recovery**: a main file that is present but does not parse is quarantined as `settings.json.corrupt-<unix>`, then `.bak` is tried, then defaults. Single invalid fields are dropped back to their defaults rather than resetting all. A file that exists but cannot be read (permission denied, I/O error) is an `Error::Io`: nothing is quarantined or overwritten.
- **Cached reads**: `get()` compares the backend's stamp (for files: inode, mtime, ctime, length) with the one the cache was read under, and re-parses only when it changed.
- **Newer file** (downgrade): opening fails with `Error::NewerSchema`; nothing is overwritten.
- **Change events**: `subscribe` callbacks receive the changed top-level keys after a successful save. Failed saves leave the in-memory value unchanged.
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
- `fill_optional(bool)` (`StoreOptions`, `ReadOptions`; default `false`). A key is optional when the type parses without it (`#[serde(default)]`, `Option`). By default a field the file omits keeps plain serde behaviour; with `true` it is filled from the defaults. Required fields the file omits, and fields the file holds with an invalid value, always take the defaults. A missing file uses the defaults whole.
- `Store::remove_keys(&["k"])` (and `remove_keys_dry_run`) deletes a key if present and does nothing, with no write, if absent, in either `NullMeans` mode. A declared key returns to the value the type yields when the key is absent.
- Sidecar read rule: with `sidecars(false)` the crate never reads `<path>.bak` (a missing file with a stale `.bak` reads as defaults, also under `OnCorrupt::Refuse`). Use `ReadOptions::sidecars(false)` to match on `read_with_options`.

### Durability, damaged files, formatting, flat migration

- **Replace mode** (`replace_mode`, `durable`, `retry_sharing_violation`). Default `Rename` with `durable(true)`: on Unix and macOS the temp file is flushed, renamed, and the directory flushed (power-loss safe). On Windows the rename is not write-through, so a power loss can bring back the previous (complete) file. Apps that need Windows write-through choose `ReplaceMode::ReplaceFile` (or `MoveWriteThrough`) and a retry, because those modes can hit a sharing violation when another process holds the file open:

  ```rust,ignore
  let opts = StoreOptions::new(schema)
      .replace_mode(ReplaceMode::ReplaceFile)
      .retry_sharing_violation(5, std::time::Duration::from_millis(50));
  ```
- **Damaged files**: `OnCorrupt::Quarantine` (default) renames it aside; `Refuse` errors; `Overwrite` leaves it in place until the atomic replace succeeds (add `keep_corrupt_copy(true)` for a copy made before the replace). Name the quarantine file with `quarantine_name(QuarantineName::Fixed(".corrupt".into()))` to overwrite one fixed file, or keep the timestamped default.
- **Formatting**: `trailing_newline(true)` ends every write with a newline without sorting keys or rewriting unchanged values.
- **Flat migration**: `flat_migration(|map| ...)` returns `(migrated, changed)`; it runs once on open, before defaults and repair, never read-only.

### Custom backends

`SettingsBackend` is the storage seam. The default is `FileBackend`. An app can plug in its own, for example an encrypted database. Implement `load` and `patch` (`patch` runs the edit under the backend's exclusion), then open with `Store::with_backend(backend, options)`. The trait documentation carries an illustrative SQLCipher example. No SQLCipher implementation ships in this crate.

## Platform support

The crate has no platform-specific code in `src/`. Atomic replacement goes through `rightkit_fs::atomic_write`, which owns the per-platform replace and directory sync.

Published version: see `INDEX.md` at the repository root. See `CHANGELOG.md`.

## Features

None. The crate has no Cargo features. Dependencies: `serde`, `serde_json` (with `raw_value`), `thiserror`, and `rightkit-fs` (`fs` feature only).
