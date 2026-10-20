# @rightkit/tauri npm consumer (draft, npm.@rightkit/tauri)

Packed-archive consumer for `@rightkit/tauri` 0.3.0 (workspace version). The unit is installed only from the sealed
`file:` archive. Its direct `@rightkit/*` dependencies are pinned to workspace versions: `@rightkit/logs` 0.1.6
(runtime import of the command constants) and `@rightkit/app-update` 0.4.0 (non-optional peer, type-only at runtime).
`@tauri-apps/plugin-dialog` 2.8.3 is installed explicitly for dialog import portability.

Reviewed generic portability imports both JS exports despite sealed legacy `skipImport` metadata. Root
closure pins logs & app-update; dialog closure also pins plugin-dialog. Each closure uses an isolated install.

Surface (exact `exports` keys): `.`, `./dialog`.

Scenarios (6): standard command contract; logs write with the named argument; collect and clear routing; device
identity refused until a native command is configured; invalid config refused and updates passed through; dialog
subpath bound to the archive without loading the Tauri plugin in this named consumer. Separate generic portability
loads dialog with its reviewed plugin dependency.

No network, no Tauri runtime, no user data. The invoke function is an in-memory fixture that records calls.
