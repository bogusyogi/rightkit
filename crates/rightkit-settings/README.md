# rightkit-settings

Versioned settings persistence for Right Suite apps. One JSON document per app
namespace, typed with your own `serde` struct (defaults come from `Default` /
`#[serde(default)]`).

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
See `API.md` in the extraction lane for the adapter interface.
