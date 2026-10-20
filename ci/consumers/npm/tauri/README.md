# @rightkit/tauri npm consumer (draft, npm.@rightkit/tauri)

Packed-archive consumer for `@rightkit/tauri` 0.3.0 (workspace version). The unit is installed only from the sealed
`file:` archive. Its direct `@rightkit/*` dependencies are pinned to workspace versions: `@rightkit/logs` 0.1.6
(runtime import of the command constants) and `@rightkit/app-update` 0.4.1 (non-optional peer, type-only at runtime).
`@tauri-apps/plugin-dialog` is an optional peer and is declared only, never installed.

Surface (exact `exports` keys): `.`, `./dialog`.

Scenarios (6): standard command contract; logs write with the named argument; collect and clear routing; device
identity refused until a native command is configured; invalid config refused and updates passed through; dialog
subpath bound to the archive without loading the Tauri plugin.

No network, no Tauri runtime, no user data. The invoke function is an in-memory fixture that records calls.
