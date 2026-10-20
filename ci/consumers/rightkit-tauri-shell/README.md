# rightkit-tauri-shell 0.5.0 registry consumer (PK08)

Standalone consumer, outside the workspace, pinning `rightkit-tauri-shell =0.5.0` exactly with
default features. It exercises the pure navigation scheme rules: `external_scheme_allowed` and
`validate_internal_scheme`.

State: pure string checks. No network, no files, no process spawn, no user data. Run only by the CI
consumer lane. The crate compiles Tauri on the native host, but this recipe starts no webview.

Not covered: webview creation, IPC, windows, menus, the updater, and any Tauri runtime.
