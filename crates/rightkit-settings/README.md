# rightkit-settings

Versioned settings persistence for Right Suite apps. It owns one JSON document per app namespace, typed with the app's own `serde` struct (defaults come from `Default` or `#[serde(default)]`), with migrations, atomic writes, corruption recovery, and change events.

**Use it** for an app's settings file. **Do not use it** to store secret values (use `rightkit-secrets`; settings hold only a `SecretRef`) or for multi-process writes (no cross-process coordination).

Behavior:

- **Migrations**: ordered `Migration { to, apply }` transforms over the raw JSON
  object; a legacy flat file with no envelope is adopted as version 0.
- **Atomic writes**: temp file + fsync + replace (`ReplaceFileW` on Windows) +
  directory fsync; the previous good file is kept as `settings.json.bak`.
- **Corruption recovery**: unreadable main file is quarantined as
  `settings.json.corrupt-<unix>`, then `.bak` is tried, then defaults. Single
  invalid fields are dropped back to their defaults rather than resetting all.
- **Newer file** (downgrade): `open` fails with `Error::NewerSchema`; nothing is
  overwritten.
- **Change events**: `subscribe` callbacks receive the changed top-level keys
  after a successful save. Failed saves roll the in-memory value back.
- **Secrets by reference only**: `SecretRef` (service + account) may live in
  settings; `SecretProbe` reports presence. Values are never stored here.

Single-process writer: concurrent processes are not coordinated.

## Features

None. The crate has no Cargo features. Dependencies: `serde`, `serde_json`, `thiserror`.

## Platform support

Atomic replacement uses `ReplaceFileW` on Windows (`src/atomic.rs`, `cfg(windows)`). The `cfg(windows)` gate is the only platform branch in `src/`.

Published version: see `INDEX.md` at the repository root. This crate has no `CHANGELOG.md`.

## Example

Illustrative; not compiled as a doctest. For compiled usage, see `tests/settings.rs`.

```rust,ignore
use rightkit_settings::{Schema, Store};

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct Settings { theme: String }

let schema = Schema::new("my-app", 2)
    .migration(2, |obj| { /* transform the raw JSON object */ });
let store: Store<Settings> = Store::open(&root, schema)?;
store.update(|s| { s.theme = "dark".into(); Ok(()) })?;
```
