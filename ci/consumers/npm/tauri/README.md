# @rightkit/tauri npm consumer (draft, npm.@rightkit/tauri)

Packed-archive consumer for `@rightkit/tauri` 0.4.3 (workspace version). The unit is installed only from the sealed
`file:` archive. Its direct `@rightkit/*` dependencies are pinned to workspace versions: `@rightkit/logs` 0.1.6
(runtime import of the command constants) and `@rightkit/app-update` 0.4.0 (non-optional peer, type-only at runtime).
`@tauri-apps/plugin-dialog` 2.8.3 is installed explicitly for dialog import portability. React 19.3.1 is used for
the optional hook export.

Reviewed generic portability imports all JS exports despite sealed legacy `skipImport` metadata. The root
and window closures pin logs and app-update. Dialog also pins plugin-dialog. The hook imports React.

Surface (exact `exports` keys): `.`, `./dialog`, `./react`, `./window`.

Scenarios (8): standard command contract; logs write with the named argument; collect and clear routing; device
identity refused until a native command is configured; invalid config refused and updates passed through; the
one constructor with `logsOnlyRightKitTauriCommands` matches the deprecated per-app wrappers it replaces; dialog
subpath bound to the archive without loading the Tauri plugin in this named consumer; typed event payload delivery
and the hook export. Separate generic portability loads dialog with its reviewed plugin dependency.

No network, no Tauri runtime, no user data. The invoke function is an in-memory fixture that records calls.
